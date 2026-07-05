//! Full-local symbol-conflict report.
//!
//! The committed scoring corpus intentionally stores only same-path overlap
//! files, because that slice has been verified to reproduce full-mod foch
//! verdicts. Cross-file symbol conflicts need the full local workshop context;
//! this module emits a small report over that context instead of adding
//! validation-sensitive files to the fixture archive.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::CmdResult;
use crate::corpus::{Case, Corpus};
use crate::score::{definition_index, ground_truth_files, read, top_level_keys};

#[derive(Clone, Debug, Default, Serialize)]
pub struct SymbolTotals {
	pub cases_seen: usize,
	pub cases_with_symbol_conflicts: usize,
	pub symbol_conflicts: usize,
	pub cross_file_symbol_conflicts: usize,
	pub same_path_symbol_conflicts: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SymbolProvider {
	pub mod_id: String,
	pub paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SymbolConflict {
	pub dir: String,
	pub key: String,
	pub compatch_files: Vec<String>,
	pub providers: Vec<SymbolProvider>,
	pub same_path: bool,
	pub cross_file: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SymbolCaseReport {
	pub compatch_id: String,
	pub title: String,
	pub patched: Vec<String>,
	pub symbol_conflicts: usize,
	pub cross_file_symbol_conflicts: usize,
	pub same_path_symbol_conflicts: usize,
	pub conflicts: Vec<SymbolConflict>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SymbolReport {
	pub totals: SymbolTotals,
	pub cases: Vec<SymbolCaseReport>,
}

/// Generate a symbol-conflict report over fully-local cases.
pub fn generate(
	corpus_path: &Path,
	workshop_dir: &Path,
	limit: usize,
) -> Result<SymbolReport, Box<dyn std::error::Error>> {
	let text = fs::read_to_string(corpus_path)?;
	let corpus = Corpus::from_json(&text)?;
	let local: Vec<&Case> = corpus
		.cases
		.iter()
		.filter(|c| c.patched.len() >= 2)
		.filter(|c| workshop_dir.join(&c.compatch_id).is_dir())
		.filter(|c| c.patched.iter().all(|m| workshop_dir.join(m).is_dir()))
		.collect();
	let to_scan: &[&Case] = if limit > 0 {
		&local[..limit.min(local.len())]
	} else {
		&local[..]
	};

	let mut cases = Vec::new();
	let mut totals = SymbolTotals {
		cases_seen: to_scan.len(),
		..Default::default()
	};
	let mut index_cache: BTreeMap<String, HashMap<(String, String), Vec<String>>> = BTreeMap::new();
	eprintln!("[symbols] scanning {} fully-local case(s)", to_scan.len());
	let all_started = Instant::now();
	for (idx, case) in to_scan.iter().enumerate() {
		eprintln!(
			"  [symbols] case {}/{} {}",
			idx + 1,
			to_scan.len(),
			case.compatch_id
		);
		let report = scan_case(case, workshop_dir, &mut index_cache);
		totals.symbol_conflicts += report.symbol_conflicts;
		totals.cross_file_symbol_conflicts += report.cross_file_symbol_conflicts;
		totals.same_path_symbol_conflicts += report.same_path_symbol_conflicts;
		if report.symbol_conflicts > 0 {
			totals.cases_with_symbol_conflicts += 1;
		}
		cases.push(report);
	}
	eprintln!(
		"[symbols] scanned {} case(s), indexed {} unique mod(s) in {:.1}s",
		to_scan.len(),
		index_cache.len(),
		all_started.elapsed().as_secs_f64()
	);

	Ok(SymbolReport { totals, cases })
}

/// Write `symbols.json` and `symbols.md` into `results_dir`.
pub fn run(corpus_path: &Path, workshop_dir: &Path, results_dir: &Path, limit: usize) -> CmdResult {
	let report = generate(corpus_path, workshop_dir, limit)?;
	fs::create_dir_all(results_dir)?;
	fs::write(
		results_dir.join("symbols.json"),
		serde_json::to_string_pretty(&report)?,
	)?;
	fs::write(results_dir.join("symbols.md"), render_markdown(&report))?;
	eprintln!(
		"[symbols] cases={} candidate_symbol_conflicts={} cross_file={}",
		report.totals.cases_seen,
		report.totals.symbol_conflicts,
		report.totals.cross_file_symbol_conflicts
	);
	Ok(())
}

fn scan_case(
	case: &Case,
	workshop_dir: &Path,
	index_cache: &mut BTreeMap<String, HashMap<(String, String), Vec<String>>>,
) -> SymbolCaseReport {
	let compatch_dir = workshop_dir.join(&case.compatch_id);
	let mods: Vec<(String, PathBuf)> = case
		.patched
		.iter()
		.map(|id| (id.clone(), workshop_dir.join(id)))
		.collect();
	for (mod_id, dir) in &mods {
		if !index_cache.contains_key(mod_id) {
			let started = Instant::now();
			let index = definition_index(dir);
			eprintln!(
				"    [symbols] indexed {}: {} definition key(s) in {:.1}s",
				mod_id,
				index.len(),
				started.elapsed().as_secs_f64()
			);
			index_cache.insert(mod_id.clone(), index);
		}
	}

	let mut compatch_defs: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
	for rel in ground_truth_files(&compatch_dir) {
		if !rel.ends_with(".txt") {
			continue;
		}
		let dir = Path::new(&rel)
			.parent()
			.map(|p| p.to_string_lossy().replace('\\', "/"))
			.unwrap_or_default();
		let Some(text) = read(&compatch_dir.join(&rel)) else {
			continue;
		};
		for key in top_level_keys(&text) {
			compatch_defs
				.entry((dir.clone(), key))
				.or_default()
				.insert(rel.clone());
		}
	}

	let mut conflicts = Vec::new();
	for ((dir, key), compatch_files) in compatch_defs {
		let lookup = (dir.clone(), key.clone());
		let mut providers = Vec::new();
		for (mod_id, _) in &mods {
			let Some(index) = index_cache.get(mod_id) else {
				continue;
			};
			let Some(paths) = index.get(&lookup) else {
				continue;
			};
			let paths = sorted_unique(paths);
			if !paths.is_empty() {
				providers.push(SymbolProvider {
					mod_id: mod_id.clone(),
					paths,
				});
			}
		}
		if providers.len() < 2 {
			continue;
		}

		let mut path_counts: BTreeMap<&str, usize> = BTreeMap::new();
		let mut all_paths: BTreeSet<&str> = BTreeSet::new();
		for provider in &providers {
			for path in &provider.paths {
				*path_counts.entry(path.as_str()).or_default() += 1;
				all_paths.insert(path.as_str());
			}
		}
		let same_path = path_counts.values().any(|&n| n >= 2);
		let cross_file = all_paths.len() > 1;

		conflicts.push(SymbolConflict {
			dir,
			key,
			compatch_files: compatch_files.into_iter().collect(),
			providers,
			same_path,
			cross_file,
		});
	}
	conflicts.sort_by(|a, b| a.dir.cmp(&b.dir).then(a.key.cmp(&b.key)));

	let symbol_conflicts = conflicts.len();
	let cross_file_symbol_conflicts = conflicts.iter().filter(|c| c.cross_file).count();
	let same_path_symbol_conflicts = conflicts.iter().filter(|c| c.same_path).count();

	SymbolCaseReport {
		compatch_id: case.compatch_id.clone(),
		title: case.title.clone(),
		patched: case.patched.clone(),
		symbol_conflicts,
		cross_file_symbol_conflicts,
		same_path_symbol_conflicts,
		conflicts,
	}
}

fn sorted_unique(paths: &[String]) -> Vec<String> {
	let set: BTreeSet<String> = paths.iter().cloned().collect();
	set.into_iter().collect()
}

fn render_markdown(report: &SymbolReport) -> String {
	let mut lines = vec![
		"# foch candidate symbol-conflict report".to_string(),
		String::new(),
		"This report scans full local Workshop mods. It is not derived from the committed scoring fixture archive.".to_string(),
		"It is schema-free: entries are compatch-anchored candidate symbol overlaps, not authoritative game-visibility facts. The complete uncapped data is in `symbols.json`.".to_string(),
		String::new(),
		format!(
			"Cases scanned: **{}**  ·  cases with candidate symbol conflicts: **{}**  ·  candidate symbol conflicts: **{}**  ·  cross-file: **{}**",
			report.totals.cases_seen,
			report.totals.cases_with_symbol_conflicts,
			report.totals.symbol_conflicts,
			report.totals.cross_file_symbol_conflicts
		),
		String::new(),
		"## Per-case".to_string(),
		String::new(),
		"| case | patched | symbol conflicts | cross-file | same-path |".to_string(),
		"|---|---:|---:|---:|---:|".to_string(),
	];
	for case in &report.cases {
		if case.symbol_conflicts == 0 {
			continue;
		}
		lines.push(format!(
			"| `{}` {} | {} | {} | {} | {} |",
			case.compatch_id,
			case.title,
			case.patched.len(),
			case.symbol_conflicts,
			case.cross_file_symbol_conflicts,
			case.same_path_symbol_conflicts
		));
	}
	lines.push(String::new());
	lines.push("## Conflicts".to_string());
	lines.push(String::new());
	const MAX_CONFLICTS_PER_CASE: usize = 12;
	const MAX_PATHS_PER_PROVIDER: usize = 6;
	for case in &report.cases {
		if case.conflicts.is_empty() {
			continue;
		}
		lines.push(format!("### {} `{}`", case.title, case.compatch_id));
		let mut shown: Vec<&SymbolConflict> =
			case.conflicts.iter().filter(|c| c.cross_file).collect();
		if shown.len() < MAX_CONFLICTS_PER_CASE {
			shown.extend(
				case.conflicts
					.iter()
					.filter(|c| !c.cross_file)
					.take(MAX_CONFLICTS_PER_CASE - shown.len()),
			);
		}
		shown.truncate(MAX_CONFLICTS_PER_CASE);
		for conflict in &shown {
			lines.push(format!(
				"- `{}/{}` same_path={} cross_file={} compatch={:?}",
				conflict.dir,
				conflict.key,
				conflict.same_path,
				conflict.cross_file,
				conflict.compatch_files
			));
			for provider in &conflict.providers {
				let shown_paths: Vec<_> = provider
					.paths
					.iter()
					.take(MAX_PATHS_PER_PROVIDER)
					.cloned()
					.collect();
				let suffix = if provider.paths.len() > shown_paths.len() {
					format!(" (+{} more)", provider.paths.len() - shown_paths.len())
				} else {
					String::new()
				};
				lines.push(format!(
					"  - `{}`: {} path(s) {:?}{}",
					provider.mod_id,
					provider.paths.len(),
					shown_paths,
					suffix
				));
			}
		}
		if case.conflicts.len() > shown.len() {
			lines.push(format!(
				"- _{} more conflict(s) omitted from markdown; see `symbols.json`._",
				case.conflicts.len() - shown.len()
			));
		}
		lines.push(String::new());
	}
	lines.join("\n")
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	fn write_file(base: &Path, rel: &str, content: &str) {
		let path = base.join(rel);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).unwrap();
		}
		fs::write(path, content).unwrap();
	}

	#[test]
	fn detects_compatch_anchored_cross_file_symbol_conflict() {
		let ws = TempDir::new().unwrap();
		let corpus_dir = TempDir::new().unwrap();
		let corpus_path = corpus_dir.path().join("corpus.json");
		let case = Case {
			compatch_id: "9000".to_string(),
			title: "Synthetic".to_string(),
			patched: vec!["1000".to_string(), "2000".to_string()],
			..Default::default()
		};
		let corpus = Corpus {
			cases: vec![case],
			..Default::default()
		};
		fs::write(&corpus_path, corpus.to_json_pretty().unwrap()).unwrap();

		write_file(
			&ws.path().join("9000"),
			"common/scripted_triggers/patch.txt",
			"declarewar = {\n\tmerged = yes\n}\n",
		);
		write_file(
			&ws.path().join("1000"),
			"common/scripted_triggers/a.txt",
			"declarewar = {\n\tfrom = a\n}\n",
		);
		write_file(
			&ws.path().join("2000"),
			"common/scripted_triggers/b.txt",
			"declarewar = {\n\tfrom = b\n}\n",
		);

		let report = generate(&corpus_path, ws.path(), 0).unwrap();
		assert_eq!(report.totals.cases_seen, 1);
		assert_eq!(report.totals.symbol_conflicts, 1);
		assert_eq!(report.totals.cross_file_symbol_conflicts, 1);
		assert_eq!(report.cases[0].conflicts[0].key, "declarewar");
	}
}
