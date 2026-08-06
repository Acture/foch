//! Download curated compatch candidates and referenced mods via SteamCMD.
//!
//! Curates a prime set from the corpus (non-churn & ≥ min subscribers, top N by
//! subs) and downloads via batched `steamcmd +login <user> +workshop_download_item
//! <appid> <id> ... +quit`.

use std::collections::HashSet;
use std::error::Error;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::EU4_APPID;
use crate::corpus::{Case, Corpus, ORACLE_POLICY_VERSION};
use crate::object_store::{TREE_DIGEST_FORMAT, TreeDigest, digest_tree};

pub const ACQUISITION_CORPUS_FILE: &str = "corpus.json";
pub const ACQUISITION_MANIFEST_FILE: &str = "manifest.json";
pub const ACQUISITION_CHECKSUMS_FILE: &str = "checksums.txt";
const ACQUISITION_MANIFEST_SCHEMA: &str = "1.0.0";

pub type AcquisitionResult<T> = Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionSelectionPolicy {
	ScorableNonChurnSubscriptionsDescCompatchIdAscV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionCorpusArtifact {
	pub relative_path: String,
	pub blake3: String,
	pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionSelection {
	pub steam_app_id: u32,
	pub policy: AcquisitionSelectionPolicy,
	pub oracle_policy_version: String,
	pub requested_case_limit: u64,
	pub minimum_subscriptions: i64,
	pub selected_case_ids: Vec<String>,
	pub selected_item_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionWorkshopItem {
	pub item_id: String,
	pub tree: TreeDigest,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionManifest {
	pub schema: String,
	pub tree_digest_format: String,
	pub corpus: AcquisitionCorpusArtifact,
	pub selection: AcquisitionSelection,
	pub workshop_items: Vec<AcquisitionWorkshopItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionPlan {
	plan_id: String,
	corpus: AcquisitionCorpusArtifact,
	selection: AcquisitionSelection,
}

impl AcquisitionPlan {
	fn selected_case_count(&self) -> usize {
		self.selection.selected_case_ids.len()
	}
}

/// Operational fetch evidence. Pre-existing directories are local inputs, not
/// claims that Steam served fresh content during this acquisition run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionFetchOutcome {
	plan_id: String,
	already_local_item_ids: Vec<String>,
	downloaded_item_ids: Vec<String>,
}

impl AcquisitionFetchOutcome {
	pub fn already_local_count(&self) -> usize {
		self.already_local_item_ids.len()
	}

	pub fn downloaded_count(&self) -> usize {
		self.downloaded_item_ids.len()
	}
}

// ─── pure helpers (unit-tested) ───────────────────────────────────────────────

/// Extract downloaded item ids from steamcmd stdout.
/// Matches every `Downloaded item <digits>` line; noise and failures are ignored.
fn parse_downloaded(stdout: &str) -> HashSet<String> {
	let re = Regex::new(r"Downloaded item (\d+)").expect("hardcoded regex is valid");
	re.captures_iter(stdout)
		.map(|cap| cap[1].to_string())
		.collect()
}

/// Return scorable, non-churned cases with at least `min_subs` subscribers,
/// sorted by subscriptions descending and capped to `fetch_n`.
fn curate(corpus: &Corpus, min_subs: i64, fetch_n: usize) -> Vec<&Case> {
	let mut prime: Vec<&Case> = corpus
		.cases
		.iter()
		.filter(|case| case.oracle_assessment().is_scorable())
		.filter(|c| !c.mod_churned() && c.subscriptions >= min_subs)
		.collect();
	prime.sort_by(|left, right| {
		right
			.subscriptions
			.cmp(&left.subscriptions)
			.then_with(|| left.compatch_id.cmp(&right.compatch_id))
	});
	prime.truncate(fetch_n);
	prime
}

/// Collect every Workshop item required by the selected cases, deduplicated and
/// sorted lexicographically by canonical decimal id. The same list drives
/// download and attestation.
fn selected_item_ids(selected: &[&Case]) -> Vec<String> {
	let mut seen: HashSet<&str> = HashSet::new();
	let mut ids = Vec::new();
	for case in selected {
		for id in std::iter::once(case.compatch_id.as_str())
			.chain(case.referenced_mods.iter().map(String::as_str))
		{
			if seen.insert(id) {
				ids.push(id.to_string());
			}
		}
	}
	ids.sort();
	ids
}

fn acquisition_selection(
	selected: &[&Case],
	fetch_n: usize,
	min_subs: i64,
) -> io::Result<AcquisitionSelection> {
	if selected.len() != fetch_n {
		return Err(invalid_data(format!(
			"acquisition selection contains {} of {fetch_n} required cases",
			selected.len()
		)));
	}
	let selected_case_ids = selected
		.iter()
		.map(|case| case.compatch_id.clone())
		.collect::<Vec<_>>();
	if selected_case_ids.iter().collect::<HashSet<_>>().len() != selected_case_ids.len() {
		return Err(invalid_data(
			"acquisition selection contains duplicate case ids",
		));
	}
	let selected_item_ids = selected_item_ids(selected);
	for item_id in &selected_item_ids {
		validate_workshop_item_id(item_id)?;
	}
	Ok(AcquisitionSelection {
		steam_app_id: EU4_APPID,
		policy: AcquisitionSelectionPolicy::ScorableNonChurnSubscriptionsDescCompatchIdAscV1,
		oracle_policy_version: ORACLE_POLICY_VERSION.to_string(),
		requested_case_limit: u64::try_from(fetch_n)
			.map_err(|_| io::Error::new(ErrorKind::InvalidInput, "case limit exceeds u64"))?,
		minimum_subscriptions: min_subs,
		selected_case_ids,
		selected_item_ids,
	})
}

/// Freeze the exact corpus identity and acquisition selection before any
/// SteamCMD downloads begin.
pub fn plan_acquisition(
	corpus_bytes: &[u8],
	fetch_n: usize,
	min_subs: i64,
) -> AcquisitionResult<AcquisitionPlan> {
	let corpus_text = std::str::from_utf8(corpus_bytes)?;
	let corpus = Corpus::from_json(corpus_text)?;
	let selected = curate(&corpus, min_subs, fetch_n);
	let selection = acquisition_selection(&selected, fetch_n, min_subs)?;
	let corpus = AcquisitionCorpusArtifact {
		relative_path: ACQUISITION_CORPUS_FILE.to_string(),
		blake3: blake3::hash(corpus_bytes).to_hex().to_string(),
		bytes: u64::try_from(corpus_bytes.len())
			.map_err(|_| invalid_data("corpus byte count exceeds u64"))?,
	};
	let plan_id = acquisition_plan_id(&corpus, &selection)?;
	Ok(AcquisitionPlan {
		plan_id,
		corpus,
		selection,
	})
}

fn acquisition_plan_id(
	corpus: &AcquisitionCorpusArtifact,
	selection: &AcquisitionSelection,
) -> serde_json::Result<String> {
	let encoded = serde_json::to_vec(&(corpus, selection))?;
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-acquisition-plan-v1");
	hasher.update(&encoded);
	Ok(hasher.finalize().to_hex().to_string())
}

/// Collect the candidate id and all referenced mod ids for the selected cases
/// (deduped and sorted), minus ids whose directory already exists in `local`.
fn download_targets(selected_item_ids: &[String], local: &HashSet<String>) -> Vec<String> {
	selected_item_ids
		.iter()
		.filter(|id| !local.contains(*id))
		.cloned()
		.collect()
}

fn already_local_items(plan: &AcquisitionPlan, workshop_dir: &Path) -> io::Result<HashSet<String>> {
	let mut local = HashSet::new();
	for item_id in &plan.selection.selected_item_ids {
		match fs::symlink_metadata(workshop_dir.join(item_id)) {
			Ok(metadata) if metadata.file_type().is_dir() => {
				local.insert(item_id.clone());
			}
			Ok(_) => {
				return Err(invalid_data(format!(
					"Workshop item {item_id} has a non-directory or symlink root"
				)));
			}
			Err(error) if error.kind() == ErrorKind::NotFound => {}
			Err(error) => return Err(error),
		}
	}
	Ok(local)
}

fn require_download_confirmations(
	needed: &[String],
	confirmed: &HashSet<String>,
) -> io::Result<Vec<String>> {
	let missing = needed
		.iter()
		.filter(|item_id| !confirmed.contains(*item_id))
		.collect::<Vec<_>>();
	if !missing.is_empty() {
		return Err(invalid_data(format!(
			"SteamCMD confirmed {} of {} required Workshop downloads",
			needed.len() - missing.len(),
			needed.len(),
		)));
	}
	Ok(needed.to_vec())
}

fn validate_fetch_outcome(
	plan: &AcquisitionPlan,
	outcome: &AcquisitionFetchOutcome,
) -> io::Result<()> {
	if outcome.plan_id != plan.plan_id {
		return Err(invalid_data(
			"fetch outcome belongs to a different acquisition plan",
		));
	}
	let already_local = outcome
		.already_local_item_ids
		.iter()
		.collect::<HashSet<_>>();
	let downloaded = outcome.downloaded_item_ids.iter().collect::<HashSet<_>>();
	if already_local.len() != outcome.already_local_item_ids.len()
		|| downloaded.len() != outcome.downloaded_item_ids.len()
		|| !already_local.is_disjoint(&downloaded)
		|| !outcome
			.already_local_item_ids
			.windows(2)
			.all(|pair| pair[0] < pair[1])
		|| !outcome
			.downloaded_item_ids
			.windows(2)
			.all(|pair| pair[0] < pair[1])
	{
		return Err(invalid_data(
			"fetch outcome contains unsorted, duplicate, or overlapping item ids",
		));
	}
	let mut covered = outcome.already_local_item_ids.clone();
	covered.extend(outcome.downloaded_item_ids.iter().cloned());
	covered.sort();
	if covered != plan.selection.selected_item_ids {
		return Err(invalid_data(
			"fetch outcome does not cover the acquisition plan exactly",
		));
	}
	Ok(())
}

/// Split `ids` into chunks of at most `size` elements.
fn batches(ids: &[String], size: usize) -> Vec<&[String]> {
	ids.chunks(size).collect()
}

// ─── deterministic acquisition integrity ────────────────────────────────────

fn validate_plan_corpus(plan: &AcquisitionPlan, corpus_path: &Path) -> AcquisitionResult<Vec<u8>> {
	let corpus_bytes = fs::read(corpus_path)?;
	let actual = AcquisitionCorpusArtifact {
		relative_path: ACQUISITION_CORPUS_FILE.to_string(),
		blake3: blake3::hash(&corpus_bytes).to_hex().to_string(),
		bytes: u64::try_from(corpus_bytes.len())
			.map_err(|_| invalid_data("corpus byte count exceeds u64"))?,
	};
	if actual != plan.corpus {
		return Err(
			invalid_data("corpus bytes changed after the acquisition plan was created").into(),
		);
	}
	Ok(corpus_bytes)
}

/// Build a path-free integrity manifest from one frozen acquisition plan.
fn build_acquisition_manifest(
	plan: &AcquisitionPlan,
	outcome: &AcquisitionFetchOutcome,
	workshop_dir: &Path,
) -> AcquisitionResult<AcquisitionManifest> {
	validate_fetch_outcome(plan, outcome)?;

	let started = Instant::now();
	let mut workshop_items = Vec::with_capacity(plan.selection.selected_item_ids.len());
	for (index, item_id) in plan.selection.selected_item_ids.iter().enumerate() {
		let item_path = workshop_dir.join(item_id);
		let metadata = fs::symlink_metadata(&item_path).map_err(|error| {
			io::Error::new(
				error.kind(),
				format!("cannot attest Workshop item {item_id}: item root is unavailable"),
			)
		})?;
		if !metadata.file_type().is_dir() {
			return Err(invalid_data(format!(
				"cannot attest Workshop item {item_id}: item root is not a directory"
			))
			.into());
		}
		let tree = digest_tree(&item_path).map_err(|error| {
			io::Error::new(
				error.kind(),
				format!("cannot attest Workshop item {item_id}: tree digest failed"),
			)
		})?;
		if tree.stats.files == 0 {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!("cannot attest Workshop item {item_id}: tree contains no files"),
			)
			.into());
		}
		workshop_items.push(AcquisitionWorkshopItem {
			item_id: item_id.clone(),
			tree,
		});
		eprintln!(
			"[acquisition] hashed {}/{} Workshop trees: item={} {}",
			index + 1,
			plan.selection.selected_item_ids.len(),
			item_id,
			progress(index + 1, plan.selection.selected_item_ids.len(), started),
		);
	}

	Ok(AcquisitionManifest {
		schema: ACQUISITION_MANIFEST_SCHEMA.to_string(),
		tree_digest_format: TREE_DIGEST_FORMAT.to_string(),
		corpus: plan.corpus.clone(),
		selection: plan.selection.clone(),
		workshop_items,
	})
}

/// Write `manifest.json` and `checksums.txt`, then read both back and verify
/// their canonical bytes. Workshop trees are hashed exactly once in this path.
pub fn write_acquisition_integrity(
	plan: &AcquisitionPlan,
	outcome: &AcquisitionFetchOutcome,
	corpus_path: &Path,
	workshop_dir: &Path,
	output_dir: &Path,
) -> AcquisitionResult<AcquisitionManifest> {
	validate_acquisition_layout(corpus_path, output_dir)?;
	validate_output_files(output_dir, &[ACQUISITION_CORPUS_FILE])?;
	let corpus_bytes = validate_plan_corpus(plan, corpus_path)?;
	validate_fetch_outcome(plan, outcome)?;
	let manifest = build_acquisition_manifest(plan, outcome, workshop_dir)?;
	let manifest_bytes = encode_manifest(&manifest)?;
	let checksums = checksum_document(&corpus_bytes, &manifest_bytes);
	write_new_file(&output_dir.join(ACQUISITION_MANIFEST_FILE), &manifest_bytes)?;
	write_new_file(
		&output_dir.join(ACQUISITION_CHECKSUMS_FILE),
		checksums.as_bytes(),
	)?;
	validate_output_files(
		output_dir,
		&[
			ACQUISITION_CORPUS_FILE,
			ACQUISITION_MANIFEST_FILE,
			ACQUISITION_CHECKSUMS_FILE,
		],
	)?;
	verify_written_artifacts(
		corpus_path,
		output_dir,
		&corpus_bytes,
		&manifest,
		&manifest_bytes,
		checksums.as_bytes(),
	)?;
	Ok(manifest)
}

/// Fully audit a previously written acquisition result against the current
/// corpus and Workshop trees. Unlike the write path, this intentionally
/// re-hashes every selected tree so later tampering is detected.
pub fn verify_acquisition_integrity(
	plan: &AcquisitionPlan,
	outcome: &AcquisitionFetchOutcome,
	corpus_path: &Path,
	workshop_dir: &Path,
	output_dir: &Path,
) -> AcquisitionResult<AcquisitionManifest> {
	validate_acquisition_layout(corpus_path, output_dir)?;
	validate_output_files(
		output_dir,
		&[
			ACQUISITION_CORPUS_FILE,
			ACQUISITION_MANIFEST_FILE,
			ACQUISITION_CHECKSUMS_FILE,
		],
	)?;
	let corpus_bytes = validate_plan_corpus(plan, corpus_path)?;
	validate_fetch_outcome(plan, outcome)?;
	let manifest_bytes = fs::read(output_dir.join(ACQUISITION_MANIFEST_FILE))?;
	let checksums = fs::read(output_dir.join(ACQUISITION_CHECKSUMS_FILE))?;
	let expected_checksums = checksum_document(&corpus_bytes, &manifest_bytes);
	if checksums != expected_checksums.as_bytes() {
		return Err(invalid_data(
			"acquisition checksums do not match corpus.json and manifest.json",
		)
		.into());
	}
	let manifest: AcquisitionManifest = serde_json::from_slice(&manifest_bytes)?;
	if encode_manifest(&manifest)? != manifest_bytes {
		return Err(invalid_data("acquisition manifest is not canonical JSON").into());
	}
	let expected = build_acquisition_manifest(plan, outcome, workshop_dir)?;
	if manifest != expected {
		return Err(invalid_data(
			"acquisition manifest does not match the corpus-derived selection and Workshop trees",
		)
		.into());
	}
	validate_plan_corpus(plan, corpus_path)?;
	Ok(manifest)
}

fn validate_acquisition_layout(corpus_path: &Path, output_dir: &Path) -> io::Result<()> {
	let output_metadata = fs::symlink_metadata(output_dir)?;
	if !output_metadata.file_type().is_dir() {
		return Err(invalid_data(
			"acquisition output root must be a direct directory, not a symlink",
		));
	}
	if corpus_path.file_name() != Some(OsStr::new(ACQUISITION_CORPUS_FILE))
		|| corpus_path.parent() != Some(output_dir)
	{
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			"acquisition corpus must be named corpus.json directly under the output directory",
		));
	}
	Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
	let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
	file.write_all(bytes)
}

fn validate_workshop_item_id(item_id: &str) -> io::Result<()> {
	let parsed = item_id.parse::<u64>().ok();
	if parsed.is_none_or(|id| id == 0 || id.to_string() != item_id) {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!("invalid Steam Workshop item id {item_id:?}"),
		));
	}
	Ok(())
}

fn validate_output_files(output_dir: &Path, expected: &[&str]) -> io::Result<()> {
	let mut actual = Vec::new();
	for entry in fs::read_dir(output_dir)? {
		let entry = entry?;
		let metadata = fs::symlink_metadata(entry.path())?;
		if !metadata.file_type().is_file() {
			return Err(invalid_data(
				"acquisition output contains a non-regular or nested entry",
			));
		}
		let name = entry
			.file_name()
			.into_string()
			.map_err(|_| invalid_data("acquisition output contains a non-UTF-8 file name"))?;
		actual.push(name);
	}
	actual.sort();
	let mut expected = expected
		.iter()
		.map(|name| (*name).to_string())
		.collect::<Vec<_>>();
	expected.sort();
	if actual != expected {
		return Err(invalid_data(
			"acquisition output does not contain exactly the required files",
		));
	}
	Ok(())
}

fn encode_manifest(manifest: &AcquisitionManifest) -> serde_json::Result<Vec<u8>> {
	let mut bytes = serde_json::to_vec_pretty(manifest)?;
	bytes.push(b'\n');
	Ok(bytes)
}

fn checksum_document(corpus_bytes: &[u8], manifest_bytes: &[u8]) -> String {
	format!(
		"{}  {ACQUISITION_CORPUS_FILE}\n{}  {ACQUISITION_MANIFEST_FILE}\n",
		blake3::hash(corpus_bytes).to_hex(),
		blake3::hash(manifest_bytes).to_hex(),
	)
}

fn verify_written_artifacts(
	corpus_path: &Path,
	output_dir: &Path,
	expected_corpus_bytes: &[u8],
	expected_manifest: &AcquisitionManifest,
	expected_manifest_bytes: &[u8],
	expected_checksums: &[u8],
) -> AcquisitionResult<()> {
	if fs::read(corpus_path)? != expected_corpus_bytes {
		return Err(invalid_data("corpus changed while acquisition integrity was written").into());
	}
	let manifest_bytes = fs::read(output_dir.join(ACQUISITION_MANIFEST_FILE))?;
	if manifest_bytes != expected_manifest_bytes {
		return Err(invalid_data("written acquisition manifest bytes drifted").into());
	}
	let manifest: AcquisitionManifest = serde_json::from_slice(&manifest_bytes)?;
	if &manifest != expected_manifest {
		return Err(invalid_data("written acquisition manifest content drifted").into());
	}
	if fs::read(output_dir.join(ACQUISITION_CHECKSUMS_FILE))? != expected_checksums {
		return Err(invalid_data("written acquisition checksum bytes drifted").into());
	}
	Ok(())
}

fn progress(completed: usize, total: usize, started: Instant) -> String {
	let elapsed = started.elapsed().as_secs_f64();
	let remaining = total.saturating_sub(completed);
	let eta = if completed == 0 {
		"unknown".to_string()
	} else {
		format!("{:.1}s", elapsed / completed as f64 * remaining as f64)
	};
	format!("elapsed={elapsed:.1}s eta={eta}")
}

fn invalid_data(message: impl Into<String>) -> io::Error {
	io::Error::new(ErrorKind::InvalidData, message.into())
}

// ─── subprocess shell (NOT unit-tested) ──────────────────────────────────────

/// Download `ids` via steamcmd, retrying up to `retries` times.
/// Returns the set of ids that steamcmd confirmed as downloaded.
fn steamcmd_download(
	user: &str,
	ids: &[String],
	retries: usize,
	batch_size: usize,
) -> HashSet<String> {
	let mut ok: HashSet<String> = HashSet::new();
	// Deduplicate while preserving encounter order.
	let mut pending: Vec<String> = {
		let mut seen: HashSet<&str> = HashSet::new();
		ids.iter()
			.filter(|id| seen.insert(id.as_str()))
			.cloned()
			.collect()
	};

	for attempt in 0..=retries {
		if pending.is_empty() {
			break;
		}
		if attempt > 0 {
			eprintln!(
				"  [fetch] retry {attempt}/{retries}: {} item(s) left",
				pending.len()
			);
		}
		let total = pending.len();
		for (i, chunk) in batches(&pending, batch_size).into_iter().enumerate() {
			let mut cmd = std::process::Command::new("steamcmd");
			cmd.arg("+login").arg(user);
			for id in chunk {
				cmd.args(["+workshop_download_item", &EU4_APPID.to_string(), id]);
			}
			cmd.arg("+quit");
			match cmd.output() {
				Ok(out) => {
					let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
					let downloaded = parse_downloaded(&stdout);
					for id in chunk {
						if downloaded.contains(id) {
							ok.insert(id.clone());
						}
					}
					eprintln!(
						"  [fetch] pass {attempt}: {}/{total} ({} ok)",
						std::cmp::min((i + 1) * batch_size, total),
						ok.len()
					);
				}
				Err(e) => {
					eprintln!("  [fetch] steamcmd error: {e}");
				}
			}
		}
		pending.retain(|id| !ok.contains(id));
	}
	if !pending.is_empty() {
		let head: Vec<&str> = pending.iter().take(8).map(|s| s.as_str()).collect();
		eprintln!(
			"  [fetch] {} unrecoverable after {retries} retries: {}",
			pending.len(),
			head.join(", ")
		);
	}
	ok
}

// ─── public entry point ───────────────────────────────────────────────────────

/// Download every non-local item from one frozen acquisition plan. Success
/// requires a SteamCMD confirmation for every requested download.
pub fn fetch(
	plan: &AcquisitionPlan,
	workshop_dir: &Path,
) -> AcquisitionResult<AcquisitionFetchOutcome> {
	let local = already_local_items(plan, workshop_dir)?;
	let needed = download_targets(&plan.selection.selected_item_ids, &local);
	eprintln!(
		"[fetch] {} compatches planned; \
		 {} items to download ({} already local)",
		plan.selected_case_count(),
		needed.len(),
		local.len(),
	);

	let confirmed = if needed.is_empty() {
		HashSet::new()
	} else {
		let user = crate::secrets::steam_username().ok_or("Steam username not configured")?;
		steamcmd_download(&user, &needed, 2, 20)
	};
	let downloaded_item_ids = require_download_confirmations(&needed, &confirmed)?;
	let already_local_item_ids = plan
		.selection
		.selected_item_ids
		.iter()
		.filter(|item_id| local.contains(*item_id))
		.cloned()
		.collect();
	let outcome = AcquisitionFetchOutcome {
		plan_id: plan.plan_id.clone(),
		already_local_item_ids,
		downloaded_item_ids,
	};
	validate_fetch_outcome(plan, &outcome)?;
	println!(
		"Acquisition fetch complete: {} existing-local inputs; {} SteamCMD-confirmed downloads.",
		outcome.already_local_item_ids.len(),
		outcome.downloaded_item_ids.len(),
	);
	Ok(outcome)
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use std::collections::{BTreeMap, HashSet};
	use std::fs;
	use std::path::{Path, PathBuf};

	use crate::corpus::{Case, Corpus, ReferencedModMeta};

	use super::{
		ACQUISITION_CHECKSUMS_FILE, ACQUISITION_MANIFEST_FILE, AcquisitionFetchOutcome,
		AcquisitionManifest, AcquisitionPlan, batches, checksum_document, curate, download_targets,
		encode_manifest, parse_downloaded, plan_acquisition, require_download_confirmations,
		selected_item_ids, validate_fetch_outcome, validate_workshop_item_id,
		verify_acquisition_integrity, write_acquisition_integrity,
	};

	// ── fixtures ──────────────────────────────────────────────────────────────

	/// Build a `Case` whose `mod_churned()` returns `churned`.
	///
	/// The compatch `time_updated` is always 500.  Non-churned mods have
	/// `referenced_mod_meta.time_updated = 50` (< 500); churned mods have 999 (> 500).
	fn make_case(id: &str, referenced_mods: &[&str], subs: i64, churned: bool) -> Case {
		let mut referenced_mod_meta = BTreeMap::new();
		for pid in referenced_mods {
			referenced_mod_meta.insert(
				pid.to_string(),
				ReferencedModMeta {
					title: pid.to_string(),
					time_created: 100,
					// churned ⟺ mod updated AFTER the compatch
					time_updated: if churned { 999 } else { 50 },
					workshop: Default::default(),
				},
			);
		}
		Case {
			compatch_id: id.to_string(),
			title: format!("{id} Compatch"),
			referenced_mods: referenced_mods.iter().map(|s| s.to_string()).collect(),
			time_created: 100,
			time_updated: 500,
			subscriptions: subs,
			referenced_mod_meta,
			workshop: Default::default(),
		}
	}

	fn acquisition_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
		let output = root.join("output");
		let workshop = root.join("workshop");
		fs::create_dir_all(&output).unwrap();
		fs::create_dir_all(&workshop).unwrap();
		let corpus = Corpus {
			generated_at: 123,
			search_terms: vec!["compatibility patch".to_string()],
			cases: vec![
				make_case("100", &["200", "300"], 500, false),
				make_case("101", &["300", "400"], 400, false),
			],
			..Default::default()
		};
		let corpus_path = output.join(super::ACQUISITION_CORPUS_FILE);
		fs::write(
			&corpus_path,
			format!("{}\n", corpus.to_json_pretty().unwrap()),
		)
		.unwrap();
		for item_id in ["100", "200", "300", "101", "400", "999"] {
			let tree = workshop.join(item_id).join("nested");
			fs::create_dir_all(&tree).unwrap();
			fs::write(tree.join("data.txt"), format!("Workshop item {item_id}\n")).unwrap();
		}
		(corpus_path, workshop, output)
	}

	fn fixture_plan(corpus_path: &Path) -> AcquisitionPlan {
		plan_acquisition(&fs::read(corpus_path).unwrap(), 2, 100).unwrap()
	}

	fn all_local_outcome(plan: &AcquisitionPlan) -> AcquisitionFetchOutcome {
		AcquisitionFetchOutcome {
			plan_id: plan.plan_id.clone(),
			already_local_item_ids: plan.selection.selected_item_ids.clone(),
			downloaded_item_ids: Vec::new(),
		}
	}

	// ── parse_downloaded ──────────────────────────────────────────────────────

	#[test]
	fn test_parse_downloaded_extracts_ids_ignores_noise() {
		let stdout = concat!(
			"Steam Console Client (c) Valve Corporation\n",
			"Connecting anonymously to Steam Public... Logged in OK\n",
			"[----] Downloading item 111222333...\n",
			" Update state (0x61) downloading, progress: 99.00 (1234 / 1234)\n",
			"Downloaded item 111222333.\n",
			"Downloading item 444555666...\n",
			" Update state (0x11) checking, progress: 0.00 (0 / 0)\n",
			"ERROR! Download item 999000999 failed (Timeout).\n",
			"Downloaded item 444555666.\n",
			"Quit\n",
		);
		let result = parse_downloaded(stdout);
		assert_eq!(
			result,
			HashSet::from(["111222333".to_string(), "444555666".to_string()])
		);
	}

	#[test]
	fn test_parse_downloaded_empty_stdout() {
		assert!(parse_downloaded("").is_empty());
	}

	// ── curate ────────────────────────────────────────────────────────────────

	#[test]
	fn test_curate_filters_and_sorts_and_caps() {
		let corpus = Corpus {
			cases: vec![
				make_case("c1", &["base", "m1"], 500, false), // passes
				make_case("c2", &["base", "m2"], 200, false), // cut by fetch_n
				make_case("c3", &["base", "m3"], 800, true),  // churned
				make_case("c4", &["base", "m4"], 50, false),  // below min_subs
				make_case("c5", &["base", "m5"], 600, false), // top-1
			],
			..Default::default()
		};
		// min_subs=100, fetch_n=2 → c5(600), c1(500)
		let result = curate(&corpus, 100, 2);
		assert_eq!(result.len(), 2);
		assert_eq!(result[0].compatch_id, "c5");
		assert_eq!(result[1].compatch_id, "c1");
	}

	#[test]
	fn test_curate_all_excluded_returns_empty() {
		let corpus = Corpus {
			cases: vec![
				make_case("c1", &["base", "m1"], 500, true), // churned
				make_case("c2", &["base", "m2"], 50, false), // below min_subs
			],
			..Default::default()
		};
		assert!(curate(&corpus, 100, 10).is_empty());
	}

	#[test]
	fn test_curate_fetch_n_larger_than_pool() {
		let corpus = Corpus {
			cases: vec![
				make_case("c1", &["base", "m1"], 300, false),
				make_case("c2", &["base", "m2"], 100, false),
			],
			..Default::default()
		};
		// fetch_n=5 but only 2 pass → return all 2
		let result = curate(&corpus, 50, 5);
		assert_eq!(result.len(), 2);
		assert_eq!(result[0].compatch_id, "c1");
	}

	#[test]
	fn test_curate_breaks_subscription_ties_by_compatch_id() {
		let corpus = Corpus {
			cases: vec![
				make_case("200", &["400", "500"], 300, false),
				make_case("100", &["600", "700"], 300, false),
			],
			..Default::default()
		};
		let result = curate(&corpus, 100, 2);
		assert_eq!(result[0].compatch_id, "100");
		assert_eq!(result[1].compatch_id, "200");
	}

	// ── download_targets ──────────────────────────────────────────────────────

	#[test]
	fn test_download_targets_dedupes_and_skips_local() {
		let case_a = Case {
			compatch_id: "cp1".to_string(),
			referenced_mods: vec!["m1".to_string(), "m2".to_string()],
			..Default::default()
		};
		let case_b = Case {
			compatch_id: "cp2".to_string(),
			referenced_mods: vec!["m2".to_string(), "m3".to_string()], // m2 shared
			..Default::default()
		};
		let selected: Vec<&Case> = vec![&case_a, &case_b];
		// m1 already exists locally
		let local: HashSet<String> = HashSet::from(["m1".to_string()]);
		let ids = selected_item_ids(&selected);
		let targets = download_targets(&ids, &local);
		assert_eq!(targets, vec!["cp1", "cp2", "m2", "m3"]);
	}

	#[test]
	fn test_download_targets_all_local() {
		let case_a = Case {
			compatch_id: "cp1".to_string(),
			referenced_mods: vec!["m1".to_string()],
			..Default::default()
		};
		let local: HashSet<String> = HashSet::from(["cp1".to_string(), "m1".to_string()]);
		let ids = selected_item_ids(&[&case_a]);
		let targets = download_targets(&ids, &local);
		assert!(targets.is_empty());
	}

	#[test]
	fn workshop_item_ids_must_be_canonical_nonzero_u64_values() {
		assert!(validate_workshop_item_id("1").is_ok());
		assert!(validate_workshop_item_id(&u64::MAX.to_string()).is_ok());
		for invalid in ["", "0", "01", "+1", " 1", "1/2", "18446744073709551616"] {
			assert!(validate_workshop_item_id(invalid).is_err(), "{invalid}");
		}
	}

	#[test]
	fn download_confirmation_requires_every_needed_item() {
		let needed = vec!["100".to_string(), "200".to_string()];
		let complete = HashSet::from(["100".to_string(), "200".to_string()]);
		assert_eq!(
			require_download_confirmations(&needed, &complete).unwrap(),
			needed
		);
		let partial = HashSet::from(["100".to_string()]);
		assert!(require_download_confirmations(&needed, &partial).is_err());
		assert!(require_download_confirmations(&needed, &HashSet::new()).is_err());
	}

	#[test]
	fn fetch_outcome_is_an_exact_partition_of_one_plan() {
		let temp = tempfile::tempdir().unwrap();
		let (corpus_path, _workshop, _output) = acquisition_fixture(temp.path());
		let plan = fixture_plan(&corpus_path);
		let outcome = AcquisitionFetchOutcome {
			plan_id: plan.plan_id.clone(),
			already_local_item_ids: vec!["100".to_string(), "101".to_string()],
			downloaded_item_ids: vec!["200".to_string(), "300".to_string(), "400".to_string()],
		};
		assert!(validate_fetch_outcome(&plan, &outcome).is_ok());

		let mut overlap = outcome.clone();
		overlap.downloaded_item_ids.push("100".to_string());
		assert!(validate_fetch_outcome(&plan, &overlap).is_err());

		let other_plan = plan_acquisition(&fs::read(&corpus_path).unwrap(), 2, 101).unwrap();
		assert!(validate_fetch_outcome(&other_plan, &outcome).is_err());
	}

	// ── batches ───────────────────────────────────────────────────────────────

	#[test]
	fn test_batches_45_ids_size_20() {
		let ids: Vec<String> = (0..45).map(|i| i.to_string()).collect();
		let chunks = batches(&ids, 20);
		assert_eq!(chunks.len(), 3);
		assert_eq!(chunks[0].len(), 20);
		assert_eq!(chunks[1].len(), 20);
		assert_eq!(chunks[2].len(), 5);
	}

	#[test]
	fn test_batches_exact_multiple() {
		let ids: Vec<String> = (0..40).map(|i| i.to_string()).collect();
		let chunks = batches(&ids, 20);
		assert_eq!(chunks.len(), 2);
		assert_eq!(chunks[0].len(), 20);
		assert_eq!(chunks[1].len(), 20);
	}

	#[test]
	fn test_batches_fewer_than_size() {
		let ids: Vec<String> = (0..5).map(|i| i.to_string()).collect();
		let chunks = batches(&ids, 20);
		assert_eq!(chunks.len(), 1);
		assert_eq!(chunks[0].len(), 5);
	}

	#[test]
	fn test_batches_empty() {
		let ids: Vec<String> = vec![];
		let chunks = batches(&ids, 20);
		assert!(chunks.is_empty());
	}

	// ── acquisition integrity ────────────────────────────────────────────────

	#[test]
	fn acquisition_integrity_is_deterministic_verified_and_path_free() {
		let temp = tempfile::tempdir().unwrap();
		let first_root = temp.path().join("first");
		let second_root = temp.path().join("second");
		let (first_corpus, first_workshop, first_output) = acquisition_fixture(&first_root);
		let (second_corpus, second_workshop, second_output) = acquisition_fixture(&second_root);
		let first_plan = fixture_plan(&first_corpus);
		let second_plan = fixture_plan(&second_corpus);
		let first_outcome = all_local_outcome(&first_plan);
		let second_outcome = all_local_outcome(&second_plan);

		let first = write_acquisition_integrity(
			&first_plan,
			&first_outcome,
			&first_corpus,
			&first_workshop,
			&first_output,
		)
		.unwrap();
		let second = write_acquisition_integrity(
			&second_plan,
			&second_outcome,
			&second_corpus,
			&second_workshop,
			&second_output,
		)
		.unwrap();

		assert_eq!(first, second);
		assert_eq!(
			first.selection.selected_case_ids,
			vec!["100".to_string(), "101".to_string()]
		);
		assert_eq!(
			first.selection.selected_item_ids,
			["100", "101", "200", "300", "400"]
				.into_iter()
				.map(str::to_string)
				.collect::<Vec<_>>()
		);
		assert!(
			first
				.workshop_items
				.iter()
				.all(|item| item.item_id != "999")
		);
		let first_manifest = fs::read(first_output.join(ACQUISITION_MANIFEST_FILE)).unwrap();
		let second_manifest = fs::read(second_output.join(ACQUISITION_MANIFEST_FILE)).unwrap();
		let first_checksums = fs::read(first_output.join(ACQUISITION_CHECKSUMS_FILE)).unwrap();
		let second_checksums = fs::read(second_output.join(ACQUISITION_CHECKSUMS_FILE)).unwrap();
		assert_eq!(first_manifest, second_manifest);
		assert_eq!(first_checksums, second_checksums);
		let rendered = format!(
			"{}{}",
			String::from_utf8(first_manifest).unwrap(),
			String::from_utf8(first_checksums).unwrap(),
		);
		assert!(!rendered.contains(&temp.path().to_string_lossy().to_string()));
		assert_eq!(
			verify_acquisition_integrity(
				&first_plan,
				&first_outcome,
				&first_corpus,
				&first_workshop,
				&first_output,
			)
			.unwrap(),
			first
		);
	}

	#[test]
	fn acquisition_verification_rejects_corpus_and_tree_tampering() {
		let temp = tempfile::tempdir().unwrap();
		let corpus_root = temp.path().join("corpus-tamper");
		let (corpus_path, workshop, output) = acquisition_fixture(&corpus_root);
		let plan = fixture_plan(&corpus_path);
		let outcome = all_local_outcome(&plan);
		write_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output).unwrap();
		let mut corpus_bytes = fs::read(&corpus_path).unwrap();
		corpus_bytes.push(b'\n');
		fs::write(&corpus_path, corpus_bytes).unwrap();
		assert!(
			verify_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output)
				.is_err()
		);

		let tree_root = temp.path().join("tree-tamper");
		let (corpus_path, workshop, output) = acquisition_fixture(&tree_root);
		let plan = fixture_plan(&corpus_path);
		let outcome = all_local_outcome(&plan);
		write_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output).unwrap();
		fs::write(
			workshop.join("100/nested/data.txt"),
			"tampered Workshop tree\n",
		)
		.unwrap();
		assert!(
			verify_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output)
				.is_err()
		);
	}

	#[test]
	fn acquisition_verification_rejects_missing_extra_and_checksum_drift() {
		let temp = tempfile::tempdir().unwrap();
		let underfilled_root = temp.path().join("underfilled");
		let (corpus_path, _workshop, output) = acquisition_fixture(&underfilled_root);
		let error = plan_acquisition(&fs::read(&corpus_path).unwrap(), 3, 100).unwrap_err();
		assert!(error.to_string().contains("2 of 3 required cases"));
		assert!(!output.join(ACQUISITION_MANIFEST_FILE).exists());

		let duplicate_root = temp.path().join("duplicate-case");
		let (corpus_path, _workshop, output) = acquisition_fixture(&duplicate_root);
		let mut corpus = Corpus::from_json(&fs::read_to_string(&corpus_path).unwrap()).unwrap();
		corpus.cases.push(corpus.cases[0].clone());
		fs::write(
			&corpus_path,
			format!("{}\n", corpus.to_json_pretty().unwrap()),
		)
		.unwrap();
		assert!(plan_acquisition(&fs::read(&corpus_path).unwrap(), 3, 100).is_err());
		assert!(!output.join(ACQUISITION_MANIFEST_FILE).exists());

		let drift_root = temp.path().join("plan-drift");
		let (corpus_path, workshop, output) = acquisition_fixture(&drift_root);
		let plan = fixture_plan(&corpus_path);
		let outcome = all_local_outcome(&plan);
		fs::write(&corpus_path, "{\"schema\":\"1.0.0\",\"cases\":[]}\n").unwrap();
		assert!(
			write_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output).is_err()
		);
		assert!(!output.join(ACQUISITION_MANIFEST_FILE).exists());
		assert!(!output.join(ACQUISITION_CHECKSUMS_FILE).exists());

		let missing_root = temp.path().join("missing");
		let (corpus_path, workshop, output) = acquisition_fixture(&missing_root);
		let plan = fixture_plan(&corpus_path);
		let outcome = all_local_outcome(&plan);
		fs::remove_dir_all(workshop.join("400")).unwrap();
		assert!(
			write_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output).is_err()
		);

		#[cfg(unix)]
		{
			let symlink_root = temp.path().join("symlink");
			let (corpus_path, workshop, output) = acquisition_fixture(&symlink_root);
			let plan = fixture_plan(&corpus_path);
			let outcome = all_local_outcome(&plan);
			fs::remove_dir_all(workshop.join("400")).unwrap();
			std::os::unix::fs::symlink(workshop.join("999"), workshop.join("400")).unwrap();
			assert!(
				write_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output)
					.is_err()
			);

			let output_symlink_root = temp.path().join("output-symlink");
			let (real_corpus, workshop, real_output) = acquisition_fixture(&output_symlink_root);
			let plan = fixture_plan(&real_corpus);
			let outcome = all_local_outcome(&plan);
			let linked_output = temp.path().join("linked-output");
			std::os::unix::fs::symlink(&real_output, &linked_output).unwrap();
			let linked_corpus = linked_output.join(super::ACQUISITION_CORPUS_FILE);
			assert!(
				write_acquisition_integrity(
					&plan,
					&outcome,
					&linked_corpus,
					&workshop,
					&linked_output,
				)
				.is_err()
			);
		}

		let unexpected_root = temp.path().join("unexpected-output");
		let (corpus_path, workshop, output) = acquisition_fixture(&unexpected_root);
		let plan = fixture_plan(&corpus_path);
		let outcome = all_local_outcome(&plan);
		fs::write(
			output.join("unexpected.txt"),
			"not part of the acquisition contract\n",
		)
		.unwrap();
		assert!(
			write_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output).is_err()
		);

		let extra_root = temp.path().join("extra");
		let (corpus_path, workshop, output) = acquisition_fixture(&extra_root);
		let plan = fixture_plan(&corpus_path);
		let outcome = all_local_outcome(&plan);
		write_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output).unwrap();
		let manifest_path = output.join(ACQUISITION_MANIFEST_FILE);
		let mut manifest: AcquisitionManifest =
			serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
		let mut extra = manifest.workshop_items[0].clone();
		extra.item_id = "999".to_string();
		manifest.selection.selected_item_ids.push("999".to_string());
		manifest.workshop_items.push(extra);
		let manifest_bytes = encode_manifest(&manifest).unwrap();
		fs::write(&manifest_path, &manifest_bytes).unwrap();
		let corpus_bytes = fs::read(&corpus_path).unwrap();
		fs::write(
			output.join(ACQUISITION_CHECKSUMS_FILE),
			checksum_document(&corpus_bytes, &manifest_bytes),
		)
		.unwrap();
		assert!(
			verify_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output)
				.is_err()
		);

		let checksum_root = temp.path().join("checksum");
		let (corpus_path, workshop, output) = acquisition_fixture(&checksum_root);
		let plan = fixture_plan(&corpus_path);
		let outcome = all_local_outcome(&plan);
		write_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output).unwrap();
		fs::write(
			output.join(ACQUISITION_CHECKSUMS_FILE),
			"drifted checksums\n",
		)
		.unwrap();
		assert!(
			verify_acquisition_integrity(&plan, &outcome, &corpus_path, &workshop, &output)
				.is_err()
		);
	}
}
