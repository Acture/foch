//! Scoring: run `foch merge` on a synthetic 2-mod playset and classify, for
//! every file the compatch hand-merged (ground truth), how foch's structural
//! merge compares — structurally and by line similarity.
//!
//! This is a faithful port of the Python harness's scoring so the verdicts are
//! identical, with one deliberate change: the merge runs **in-process** via
//! `foch_engine::run_merge_with_options` (no `foch` subprocess) with
//! `include_game_base = false`, which emits the full union of the input mods —
//! the comparable target for a self-contained compatch.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use foch_core::model::MergeReport;
use foch_engine::{
	CheckRequest, Config, MergeError, MergeExecuteOptions, MergeExecutionResult,
	run_merge_with_options,
};
use regex::Regex;

/// `^key = {` at a line start — a top-level Clausewitz definition.
static TOP_KEY_RE: LazyLock<Regex> =
	LazyLock::new(|| Regex::new(r"(?m)^([A-Za-z_][\w.\-]*)\s*=\s*\{").unwrap());
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
/// `for <path>;` inside a conflict warning string.
static WARN_PATH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"for ([\w./\-]+);").unwrap());

const SKIP_NAMES: &[&str] = &["descriptor.mod", "thumbnail.png"];
const SKIP_EXTS: &[&str] = &["bak", "jpg", "jpeg", "png", "dds", "tga", "mod"];

/// Read a file as UTF-8, replacing invalid sequences (mirrors Python's
/// `read_text(errors="replace")`). Returns `None` only on I/O error.
pub fn read(path: &Path) -> Option<String> {
	match fs::read(path) {
		Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
		Err(_) => None,
	}
}

/// Set of top-level definition keys in a Clausewitz file.
pub fn top_level_keys(text: &str) -> HashSet<String> {
	TOP_KEY_RE
		.captures_iter(text)
		.map(|c| c[1].to_string())
		.collect()
}

/// Whitespace/comment-insensitive line list for similarity scoring.
fn normalise(text: &str) -> Vec<String> {
	text.lines()
		.filter_map(|line| {
			let stripped = line.split('#').next().unwrap_or("").trim();
			let collapsed = WS_RE.replace_all(stripped, " ").into_owned();
			if collapsed.is_empty() {
				None
			} else {
				Some(collapsed)
			}
		})
		.collect()
}

/// `difflib.SequenceMatcher(None, normalise(a), normalise(b)).ratio()`.
pub fn similarity(a: &str, b: &str) -> f64 {
	let la = normalise(a);
	let lb = normalise(b);
	ratio(&la, &lb)
}

// --- faithful port of CPython difflib.SequenceMatcher.ratio() over lines ---
// autojunk is irrelevant for our line counts (it only triggers for sequences
// longer than 200 elements with elements appearing in >1% of positions).

fn ratio(a: &[String], b: &[String]) -> f64 {
	let total = a.len() + b.len();
	if total == 0 {
		return 1.0;
	}
	let mut b2j: HashMap<&str, Vec<usize>> = HashMap::new();
	for (j, s) in b.iter().enumerate() {
		b2j.entry(s.as_str()).or_default().push(j);
	}
	let matches = matching_total(a, &b2j, 0, a.len(), 0, b.len());
	2.0 * matches as f64 / total as f64
}

fn find_longest_match(
	a: &[String],
	b2j: &HashMap<&str, Vec<usize>>,
	alo: usize,
	ahi: usize,
	blo: usize,
	bhi: usize,
) -> (usize, usize, usize) {
	let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
	let mut j2len: HashMap<usize, usize> = HashMap::new();
	for (offset, item) in a[alo..ahi].iter().enumerate() {
		let i = alo + offset;
		let mut newj2len: HashMap<usize, usize> = HashMap::new();
		if let Some(js) = b2j.get(item.as_str()) {
			for &j in js {
				if j < blo {
					continue;
				}
				if j >= bhi {
					break;
				}
				let k = j2len.get(&j.wrapping_sub(1)).copied().unwrap_or(0) + 1;
				newj2len.insert(j, k);
				if k > bestsize {
					besti = i + 1 - k;
					bestj = j + 1 - k;
					bestsize = k;
				}
			}
		}
		j2len = newj2len;
	}
	(besti, bestj, bestsize)
}

fn matching_total(
	a: &[String],
	b2j: &HashMap<&str, Vec<usize>>,
	alo: usize,
	ahi: usize,
	blo: usize,
	bhi: usize,
) -> usize {
	let (i, j, k) = find_longest_match(a, b2j, alo, ahi, blo, bhi);
	if k == 0 {
		return 0;
	}
	k + matching_total(a, b2j, alo, i, blo, j) + matching_total(a, b2j, i + k, ahi, j + k, bhi)
}

/// Relative paths of every file the compatch hand-merged (the ground-truth set),
/// skipping descriptors and non-script binary assets.
pub fn ground_truth_files(compatch_dir: &Path) -> Vec<String> {
	let mut out = Vec::new();
	for entry in walkdir::WalkDir::new(compatch_dir)
		.into_iter()
		.filter_map(Result::ok)
	{
		if !entry.file_type().is_file() {
			continue;
		}
		let path = entry.path();
		let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
		if SKIP_NAMES.contains(&name) {
			continue;
		}
		let ext = path
			.extension()
			.and_then(|e| e.to_str())
			.map(str::to_ascii_lowercase)
			.unwrap_or_default();
		if SKIP_EXTS.contains(&ext.as_str()) {
			continue;
		}
		if let Ok(rel) = path.strip_prefix(compatch_dir) {
			out.push(rel.to_string_lossy().replace('\\', "/"));
		}
	}
	out.sort();
	out
}

/// Paths foch surfaced as conflicts (it declined to auto-merge; a human did).
pub fn conflict_rel_paths(report: &MergeReport) -> HashSet<String> {
	let mut out = HashSet::new();
	for c in &report.conflict_resolutions {
		if !c.path.is_empty() {
			out.insert(c.path.clone());
		}
	}
	for w in &report.warnings {
		if let Some(m) = WARN_PATH_RE.captures(w) {
			out.insert(m[1].to_string());
		}
	}
	out
}

/// Write `dlc_load.json` + `mod/ugc_<id>.mod` descriptors pointing at mod dirs.
pub fn write_playset(tmp: &Path, mods: &[(String, PathBuf)]) -> io::Result<PathBuf> {
	fs::create_dir_all(tmp.join("mod"))?;
	let mut enabled = Vec::new();
	for (steam_id, ws_dir) in mods {
		let rel = format!("mod/ugc_{steam_id}.mod");
		let path_val = ws_dir.to_string_lossy().replace('\\', "/");
		fs::write(
			tmp.join(&rel),
			format!("name=\"{steam_id}\"\npath=\"{path_val}\"\nremote_file_id=\"{steam_id}\"\n"),
		)?;
		enabled.push(rel);
	}
	let dlc = serde_json::json!({ "enabled_mods": enabled, "disabled_dlcs": [] });
	let dlc_path = tmp.join("dlc_load.json");
	fs::write(&dlc_path, serde_json::to_string(&dlc).unwrap())?;
	Ok(dlc_path)
}

/// Run a merge of `playset` into `out_dir`, in-process, with the game base
/// excluded (full-union output). `force` auto-resolves manual conflicts when
/// true; when false, conflicting files are withheld and surface in the report.
pub fn run_merge(
	playset: &Path,
	out_dir: &Path,
	force: bool,
) -> Result<MergeExecutionResult, MergeError> {
	run_merge_with_options(
		CheckRequest {
			playset_path: playset.to_path_buf(),
			config: Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path: HashMap::new(),
				extra_ignore_patterns: Vec::new(),
			},
		},
		MergeExecuteOptions {
			out_dir: out_dir.to_path_buf(),
			include_game_base: false,
			include_base: false,
			gui_scroll_merge: false,
			force,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_config_path: None,
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			playset_fingerprint: None,
			provenance: false,
		},
	)
}

/// Classification of foch's output for one ground-truth file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
	/// foch surfaced it as a conflict; the human resolved by hand.
	ConflictWithheld,
	/// foch emitted nothing for this path.
	NotEmitted,
	/// same definitions, line-similarity ≥ 0.92 to the human merge.
	MatchesHuman,
	/// foch dropped top-level definitions present in either input mod.
	DropsContent,
	/// same definitions as the human, different text.
	DivergesFormatting,
	/// different top-level definitions from the human merge.
	DivergesStructure,
}

impl Verdict {
	pub fn as_str(self) -> &'static str {
		match self {
			Verdict::ConflictWithheld => "conflict_withheld",
			Verdict::NotEmitted => "not_emitted",
			Verdict::MatchesHuman => "matches_human",
			Verdict::DropsContent => "drops_content",
			Verdict::DivergesFormatting => "diverges_formatting",
			Verdict::DivergesStructure => "diverges_structure",
		}
	}
}

#[derive(Clone, Debug)]
pub struct FileScore {
	pub rel: String,
	pub in_a: bool,
	pub in_b: bool,
	pub overlap: bool,
	pub foch_emitted: bool,
	pub foch_conflict: bool,
	pub similarity: Option<f64>,
	pub keys_match: Option<bool>,
	pub dropped_keys: Vec<String>,
	pub verdict: Verdict,
}

/// Classify foch's merged output for one ground-truth file against the compatch.
pub fn score_file(
	rel: &str,
	mod_a: &Path,
	mod_b: &Path,
	compatch: &Path,
	out_dir: &Path,
	conflict_paths: &HashSet<String>,
) -> FileScore {
	let in_a = mod_a.join(rel).is_file();
	let in_b = mod_b.join(rel).is_file();
	let overlap = in_a && in_b;
	let foch_path = out_dir.join(rel);
	let foch_emitted = foch_path.is_file();
	let foch_conflict = conflict_paths.contains(rel);

	let compatch_text = read(&compatch.join(rel)).unwrap_or_default();
	let foch_text = if foch_emitted { read(&foch_path) } else { None };

	let mut sim = None;
	let mut keys_match = None;
	let mut dropped: Vec<String> = Vec::new();
	if let Some(ft) = foch_text.as_deref() {
		sim = Some((similarity(ft, &compatch_text) * 1000.0).round() / 1000.0);
		let fk = top_level_keys(ft);
		let ck = top_level_keys(&compatch_text);
		keys_match = Some(fk == ck);
		let union_ab: HashSet<String> = top_level_keys(&read(&mod_a.join(rel)).unwrap_or_default())
			.union(&top_level_keys(&read(&mod_b.join(rel)).unwrap_or_default()))
			.cloned()
			.collect();
		dropped = union_ab.difference(&fk).cloned().collect();
		dropped.sort();
	}

	let verdict = if foch_conflict {
		Verdict::ConflictWithheld
	} else if !foch_emitted {
		Verdict::NotEmitted
	} else if keys_match == Some(true) && sim.is_some_and(|s| s >= 0.92) {
		Verdict::MatchesHuman
	} else if !dropped.is_empty() {
		Verdict::DropsContent
	} else if keys_match == Some(true) {
		Verdict::DivergesFormatting
	} else {
		Verdict::DivergesStructure
	};

	FileScore {
		rel: rel.to_string(),
		in_a,
		in_b,
		overlap,
		foch_emitted,
		foch_conflict,
		similarity: sim,
		keys_match,
		dropped_keys: dropped,
		verdict,
	}
}
