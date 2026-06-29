//! Scoring orchestration: `run` scores every locally-available compatch in the
//! corpus; `learn` classifies how humans resolved overlaps.
//!
//! Reuses [`crate::score`] primitives and writes artifacts via [`crate::report`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::CmdResult;
use crate::corpus::Case;
use crate::score::{conflict_rel_paths, ground_truth_files, run_merge, score_file, write_playset};

// ------------------------------------------------------------------ data model

/// Per-file score record embedded in [`CaseResult`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRecord {
	pub rel: String,
	pub in_a: bool,
	pub in_b: bool,
	pub overlap: bool,
	pub foch_emitted: bool,
	pub foch_conflict: bool,
	pub similarity: Option<f64>,
	pub keys_match: Option<bool>,
	pub dropped_keys: Vec<String>,
	pub verdict: String,
}

/// Per-case scoring result — the unit element of `results.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaseResult {
	pub compatch_id: String,
	pub title: String,
	pub patched: Vec<String>,
	/// Snake-case `MergeReportStatus` (e.g. `"ready"`, `"blocked"`).
	pub merge_status: Option<String>,
	/// Full `MergeReportValidation` as a JSON value.
	pub validation: Option<serde_json::Value>,
	/// Number of ground-truth files in the compatch.
	pub ground_truth_files: usize,
	/// Number of ground-truth files that appear in both patched mods (overlap).
	pub overlap_files: usize,
	/// Verdict counts over the overlap set (BTreeMap → deterministic JSON key order).
	pub verdicts: BTreeMap<String, usize>,
	/// Per-file scores for every ground-truth file.
	pub files: Vec<FileRecord>,
}

// ------------------------------------------------------------------ public API

/// Options for [`run`].
pub struct RunOptions<'a> {
	/// Path to `corpus.json`.
	pub corpus: &'a Path,
	/// Steam Workshop content dir holding the downloaded mods + compatches.
	pub workshop_dir: &'a Path,
	/// Directory to write `results.json` + `report.md` into.
	pub results_dir: &'a Path,
	/// Cap on number of cases scored (`0` = all).
	pub limit: usize,
	/// Preserve per-case temp merge directories.
	pub keep: bool,
}

/// Score one compatch case against a flat workshop directory.
///
/// The workshop directory must contain `<compatch_id>/` and every `<mod_id>/`
/// subdirectory listed in `case.patched`.
///
/// When `keep` is `true` the per-case temp merge directory is leaked (not
/// cleaned up); useful for post-hoc inspection.
pub fn score_case(
	case: &Case,
	workshop_dir: &Path,
	keep: bool,
) -> Result<CaseResult, Box<dyn std::error::Error>> {
	let compatch_dir = workshop_dir.join(&case.compatch_id);
	let mod_dirs: Vec<PathBuf> = case
		.patched
		.iter()
		.map(|id| workshop_dir.join(id))
		.collect();
	let gt = ground_truth_files(&compatch_dir);

	let tmp = tempfile::tempdir()?;
	let out_dir = tmp.path().join("out");

	let mods: Vec<(String, PathBuf)> = case
		.patched
		.iter()
		.cloned()
		.zip(mod_dirs.iter().cloned())
		.collect();
	let dlc = write_playset(tmp.path(), &mods)?;
	let result = run_merge(&dlc, &out_dir, /* force= */ false)?;
	let conflicts = conflict_rel_paths(&result.report);

	let mod_a = &mod_dirs[0];
	let mod_b = if mod_dirs.len() > 1 {
		&mod_dirs[1]
	} else {
		&mod_dirs[0]
	};

	let files: Vec<FileRecord> = gt
		.iter()
		.map(|rel| {
			let fs = score_file(rel, mod_a, mod_b, &compatch_dir, &out_dir, &conflicts);
			FileRecord {
				rel: fs.rel,
				in_a: fs.in_a,
				in_b: fs.in_b,
				overlap: fs.overlap,
				foch_emitted: fs.foch_emitted,
				foch_conflict: fs.foch_conflict,
				similarity: fs.similarity,
				keys_match: fs.keys_match,
				dropped_keys: fs.dropped_keys,
				verdict: fs.verdict.as_str().to_string(),
			}
		})
		.collect();

	let overlap_count = files.iter().filter(|f| f.overlap).count();
	let mut verdicts: BTreeMap<String, usize> = BTreeMap::new();
	for f in files.iter().filter(|f| f.overlap) {
		*verdicts.entry(f.verdict.clone()).or_default() += 1;
	}

	// Serialise MergeReportStatus via serde → "ready" / "blocked" etc.
	let merge_status = serde_json::to_value(result.report.status)
		.ok()
		.and_then(|v| v.as_str().map(str::to_string));

	let validation = serde_json::to_value(result.report.validation).ok();

	if keep {
		// Preserve the temp directory (don't clean up).
		let _ = tmp.keep();
	}
	// If !keep, `tmp` drops here and the directory is removed.

	Ok(CaseResult {
		compatch_id: case.compatch_id.clone(),
		title: case.title.clone(),
		patched: case.patched.clone(),
		merge_status,
		validation,
		ground_truth_files: gt.len(),
		overlap_files: overlap_count,
		verdicts,
		files,
	})
}

/// Filter the corpus to fully-local cases (compatch + all patched mods present
/// in `workshop_dir`), run foch merge on each, score against the compatch, and
/// write `results.json` + `report.md`.
pub fn run(opts: &RunOptions) -> CmdResult {
	let text = std::fs::read_to_string(opts.corpus)?;
	let corpus = crate::corpus::Corpus::from_json(&text)?;

	let local: Vec<&Case> = corpus
		.cases
		.iter()
		.filter(|c| c.patched.len() >= 2)
		.filter(|c| opts.workshop_dir.join(&c.compatch_id).is_dir())
		.filter(|c| c.patched.iter().all(|m| opts.workshop_dir.join(m).is_dir()))
		.collect();

	let to_score: &[&Case] = if opts.limit > 0 {
		&local[..opts.limit.min(local.len())]
	} else {
		&local[..]
	};

	let mut results: Vec<CaseResult> = Vec::with_capacity(to_score.len());
	for case in to_score {
		let cr = score_case(case, opts.workshop_dir, opts.keep)?;
		results.push(cr);
	}

	crate::report::write_results_json(opts.results_dir, &results)?;
	crate::report::write_report_md(opts.results_dir, &results)?;

	Ok(())
}

/// Read `results.json` from `results_dir`, aggregate foch verdict distribution,
/// and write `rules.md`.
///
/// Note: unlike the Python reference (which accesses the workshop directory to
/// call `classify_resolution`), the Rust implementation works purely from the
/// pre-computed `CaseResult` records in `results.json`. This is correct given
/// the fixed `learn(&Path)` signature.
pub fn learn(results_dir: &Path) -> CmdResult {
	let text = std::fs::read_to_string(results_dir.join("results.json"))?;
	let results: Vec<CaseResult> = serde_json::from_str(&text)?;
	crate::report::write_rules_md(results_dir, &results)?;
	Ok(())
}
