//! Download curated compatch candidates and referenced mods via SteamCMD.
//!
//! Curates a prime set from the corpus (non-churn & ≥ min subscribers, top N by
//! subs) and downloads via batched `steamcmd +login <user> +workshop_download_item
//! <appid> <id> ... +quit`.

use std::collections::HashSet;
use std::path::Path;

use regex::Regex;

use crate::CmdResult;
use crate::config::EU4_APPID;
use crate::corpus::{Case, Corpus};

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
	prime.sort_by_key(|c| std::cmp::Reverse(c.subscriptions));
	prime.truncate(fetch_n);
	prime
}

/// Collect the candidate id and all referenced mod ids for the selected cases
/// (deduped, in encounter order), minus ids whose directory already exists
/// in `local`.
fn download_targets(selected: &[&Case], local: &HashSet<String>) -> Vec<String> {
	let mut seen: HashSet<&str> = HashSet::new();
	let mut needed: Vec<String> = Vec::new();
	for case in selected {
		for id in std::iter::once(case.compatch_id.as_str())
			.chain(case.referenced_mods.iter().map(String::as_str))
		{
			if seen.insert(id) && !local.contains(id) {
				needed.push(id.to_string());
			}
		}
	}
	needed
}

/// Split `ids` into chunks of at most `size` elements.
fn batches(ids: &[String], size: usize) -> Vec<&[String]> {
	ids.chunks(size).collect()
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

/// Curate the top `fetch_n` scorable candidates with at least `min_subs`
/// subscribers, then download them and their referenced mods.
pub fn fetch(corpus: &Path, workshop_dir: &Path, fetch_n: usize, min_subs: i64) -> CmdResult {
	let text = std::fs::read_to_string(corpus)?;
	let corpus_data = Corpus::from_json(&text)?;

	let user = crate::secrets::steam_username().ok_or("Steam username not configured")?;

	let selected = curate(&corpus_data, min_subs, fetch_n);

	// Build the set of ids already present locally.
	let local: HashSet<String> = if workshop_dir.is_dir() {
		std::fs::read_dir(workshop_dir)?
			.filter_map(|e| e.ok())
			.filter(|e| e.path().is_dir())
			.filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
			.collect()
	} else {
		HashSet::new()
	};

	let needed = download_targets(&selected, &local);
	let seen_count: usize = {
		let mut seen: HashSet<&str> = HashSet::new();
		for c in &selected {
			seen.insert(c.compatch_id.as_str());
			for m in &c.referenced_mods {
				seen.insert(m.as_str());
			}
		}
		seen.len()
	};
	eprintln!(
		"[fetch] {} compatches curated (non-churn, >={min_subs} subs); \
		 {} items to download ({} already local)",
		selected.len(),
		needed.len(),
		seen_count.saturating_sub(needed.len()),
	);

	if !needed.is_empty() {
		steamcmd_download(&user, &needed, 2, 20);
	}

	let full = selected
		.iter()
		.filter(|c| {
			workshop_dir.join(&c.compatch_id).is_dir()
				&& c.referenced_mods
					.iter()
					.all(|m| workshop_dir.join(m).is_dir())
		})
		.count();
	println!(
		"{full}/{} curated compatches now fully local and testable \
		 (workshop dir: {}). Next: `run`.",
		selected.len(),
		workshop_dir.display()
	);
	Ok(())
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use std::collections::{BTreeMap, HashSet};

	use crate::corpus::{Case, Corpus, ReferencedModMeta};

	use super::{batches, curate, download_targets, parse_downloaded};

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
		let targets = download_targets(&selected, &local);
		// Encounter order: cp1, m1(skip-local), m2, cp2, m2(skip-seen), m3
		assert_eq!(targets, vec!["cp1", "m2", "cp2", "m3"]);
	}

	#[test]
	fn test_download_targets_all_local() {
		let case_a = Case {
			compatch_id: "cp1".to_string(),
			referenced_mods: vec!["m1".to_string()],
			..Default::default()
		};
		let local: HashSet<String> = HashSet::from(["cp1".to_string(), "m1".to_string()]);
		let targets = download_targets(&[&case_a], &local);
		assert!(targets.is_empty());
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
}
