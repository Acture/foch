//! Scoring orchestration: `run` scores every locally-available compatch in the
//! corpus; `learn` classifies how humans resolved overlaps.
//!
//! Reuses [`crate::score`] primitives and writes artifacts via [`crate::report`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::CmdResult;
use crate::corpus::Case;
use crate::score::{
	classify_resolution, conflict_rel_paths, ground_truth_files, run_merge, score_file,
	write_playset,
};

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

/// One row in the per-file detail of `rules.md`: a classified overlap file.
pub struct ResolutionRow {
	pub title: String,
	pub rel: String,
	/// foch's verdict string for this file (e.g. `"conflict_withheld"`).
	pub foch_verdict: String,
	pub resolution: crate::score::Resolution,
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
	/// Isolate each merge in a `score-one` child process (CLI over the live
	/// corpus, where foch may crash). `false` scores in-process — for trusted
	/// inputs / tests, and required when `current_exe` is not the `foch-mq` bin.
	pub isolate: bool,
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

	// Isolate each case's merge in a child process. foch can stack-overflow
	// (unbounded recursion in build_merge_plan) on pathological community mods,
	// which aborts the process uncatchably — so a crash must take down only that
	// case, not the whole run. Each child is `foch-mq score-one --id <id>`.
	let exe = std::env::current_exe()?;
	let (mut ok, mut skipped) = (0usize, 0usize);
	let mut results: Vec<CaseResult> = Vec::with_capacity(to_score.len());
	for case in to_score {
		let scored: Option<CaseResult> = if opts.isolate {
			let output = std::process::Command::new(&exe)
				.arg("--corpus")
				.arg(opts.corpus)
				.arg("--workshop-dir")
				.arg(opts.workshop_dir)
				.arg("score-one")
				.arg("--id")
				.arg(&case.compatch_id)
				.output()?;
			if output.status.success() {
				serde_json::from_slice::<CaseResult>(&output.stdout).ok()
			} else {
				eprintln!(
					"  [run] skip {}: foch could not merge it (status {:?}) — likely a foch crash",
					case.compatch_id,
					output.status.code()
				);
				None
			}
		} else {
			score_case(case, opts.workshop_dir, opts.keep).ok()
		};
		match scored {
			Some(cr) => {
				results.push(cr);
				ok += 1;
			}
			None => skipped += 1,
		}
	}
	eprintln!("[run] scored {ok}, skipped {skipped}");

	crate::report::write_results_json(opts.results_dir, &results)?;
	crate::report::write_report_md(opts.results_dir, &results)?;

	Ok(())
}

/// Score a single case by id and print its [`CaseResult`] as JSON to stdout.
/// This is the per-case worker that the crash-isolating [`run`] spawns as a
/// child process; if foch aborts here, only this child dies.
pub fn score_one(corpus: &Path, workshop_dir: &Path, id: &str) -> CmdResult {
	let text = std::fs::read_to_string(corpus)?;
	let corpus = crate::corpus::Corpus::from_json(&text)?;
	let case = corpus
		.cases
		.iter()
		.find(|c| c.compatch_id == id)
		.ok_or_else(|| format!("compatch {id} not found in corpus"))?;
	let result = score_case(case, workshop_dir, false)?;
	// stdout = the JSON result (foch's [merge] logs go to stderr, kept separate).
	println!("{}", serde_json::to_string(&result)?);
	Ok(())
}

/// Read `results.json` from `results_dir`, classify how humans resolved each
/// overlap (using the mod + compatch files in `workshop_dir`), and write
/// `rules.md`.
///
/// Faithful port of the Python `cmd_learn`: for every overlap file in every
/// case, calls [`classify_resolution`] to determine the human resolution
/// strategy, then aggregates into relationship→verdict crosstab, overall
/// verdict distribution, and the subset where foch withheld a conflict.
pub fn learn(results_dir: &Path, workshop_dir: &Path) -> CmdResult {
	let text = std::fs::read_to_string(results_dir.join("results.json"))?;
	let results: Vec<CaseResult> = serde_json::from_str(&text)?;

	let mut rows: Vec<ResolutionRow> = Vec::new();
	for r in &results {
		if r.patched.len() < 2 {
			continue;
		}
		let base = workshop_dir.join(&r.patched[0]);
		let overlay = workshop_dir.join(&r.patched[1]);
		let compatch = workshop_dir.join(&r.compatch_id);

		for f in &r.files {
			if !f.overlap {
				continue;
			}
			if let Some(res) = classify_resolution(&f.rel, &base, &overlay, &compatch) {
				rows.push(ResolutionRow {
					title: r.title.clone(),
					rel: f.rel.clone(),
					foch_verdict: f.verdict.clone(),
					resolution: res,
				});
			}
		}
	}

	crate::report::write_rules_md(results_dir, &rows)?;
	Ok(())
}
