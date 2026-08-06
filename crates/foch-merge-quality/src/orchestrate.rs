//! Deterministic scoring over an already generated product output tree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::corpus::Case;
use crate::score::{
	Resolution, ScoreCache, ScoreFileRequest, SourceMod, classify_resolution, conflict_rel_paths,
	reference_output_files, score_file_with_cache_and_basegame, scoring_reference_units,
};
use foch_core::model::MergeReport;

// ------------------------------------------------------------------ data model

/// Per-file score record embedded in [`CaseResult`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
	pub rel: String,
	pub source_mod_ids: Vec<String>,
	pub source_count: usize,
	pub multi_source: bool,
	pub foch_emitted: bool,
	pub foch_conflict: bool,
	pub similarity: Option<f64>,
	pub keys_match: Option<bool>,
	pub ast_match: Option<bool>,
	pub dropped_keys: Vec<String>,
	pub verdict: String,
	pub accepted_ok: bool,
	pub acceptance_reason: Option<String>,
}

impl FileRecord {
	pub fn from_score(score: crate::score::FileScore) -> Self {
		Self {
			rel: score.rel,
			source_mod_ids: score.source_mod_ids,
			source_count: score.source_count,
			multi_source: score.multi_source,
			foch_emitted: score.foch_emitted,
			foch_conflict: score.foch_conflict,
			similarity: score.similarity,
			keys_match: score.keys_match,
			ast_match: score.ast_match,
			dropped_keys: score.dropped_keys,
			verdict: score.verdict.as_str().to_string(),
			accepted_ok: score.verdict.accepted_ok(),
			acceptance_reason: score.acceptance_reason,
		}
	}
}

/// Per-case wall-clock timings, in milliseconds.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CaseTimings {
	pub setup_ms: u64,
	pub merge_ms: u64,
	pub scoring_ms: u64,
	pub total_ms: u64,
}

/// Per-case scoring result for one already generated output tree.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaseResult {
	pub compatch_id: String,
	pub title: String,
	pub referenced_mods: Vec<String>,
	/// Snake-case `MergeReportStatus` (e.g. `"ready"`, `"blocked"`).
	pub merge_status: Option<String>,
	/// Full `MergeReportValidation` as a JSON value.
	pub validation: Option<serde_json::Value>,
	/// Number of script files in the compatch reference output.
	pub ground_truth_files: usize,
	/// Number of reference-output files attributable to at least two source mods.
	pub multi_source_files: usize,
	/// Verdict counts over every reference-output file.
	pub all_ground_truth_verdicts: BTreeMap<String, usize>,
	/// Verdict counts over files contributed by at least two source mods.
	pub multi_source_verdicts: BTreeMap<String, usize>,
	pub accepted_ground_truth_files: usize,
	pub accepted_multi_source_files: usize,
	/// Wall-clock timing breakdown for this case.
	#[serde(default)]
	pub timings: CaseTimings,
	/// Per-file scores for every reference-output file.
	pub files: Vec<FileRecord>,
}

/// Immutable inputs needed to score one already generated product output.
pub struct ScoreExistingOutputRequest<'a> {
	pub case: &'a Case,
	pub compatch_dir: &'a Path,
	pub source_dirs: &'a [PathBuf],
	pub output_dir: &'a Path,
	pub report: &'a MergeReport,
	pub basegame_root: Option<&'a Path>,
	pub merge_ms: u64,
}

/// Mechanical scores and derived human-resolution evidence for one case.
pub struct ExistingOutputScore {
	pub result: CaseResult,
	pub resolutions: BTreeMap<String, Resolution>,
}

// ------------------------------------------------------------------ public API

/// Score an already generated output tree against a human compatch.
///
/// This function performs no merge execution and selects no merge kernel. The
/// caller owns product execution and supplies its parsed [`MergeReport`] plus
/// measured merge duration.
pub fn score_existing_output_with_cache(
	request: &ScoreExistingOutputRequest<'_>,
	score_cache: &mut ScoreCache,
) -> Result<ExistingOutputScore, Box<dyn std::error::Error>> {
	if request.case.referenced_mods.len() != request.source_dirs.len() {
		return Err(format!(
			"case {} declares {} source mods but {} roots were provided",
			request.case.compatch_id,
			request.case.referenced_mods.len(),
			request.source_dirs.len()
		)
		.into());
	}
	let setup_started = Instant::now();
	let gt = reference_output_files(request.compatch_dir);
	let scoring_units = scoring_reference_units(&gt);
	let conflicts = conflict_rel_paths(request.report);
	let source_mods: Vec<SourceMod<'_>> = request
		.case
		.referenced_mods
		.iter()
		.zip(request.source_dirs)
		.map(|(id, root)| SourceMod { id, root })
		.collect();
	let setup_ms = elapsed_ms(setup_started.elapsed());

	let scoring_started = Instant::now();
	let files: Vec<FileRecord> = scoring_units
		.iter()
		.map(|rel| {
			let fs = score_file_with_cache_and_basegame(
				&ScoreFileRequest {
					rel,
					source_mods: &source_mods,
					compatch: request.compatch_dir,
					out_dir: request.output_dir,
					conflict_paths: &conflicts,
				},
				score_cache,
				request.basegame_root,
			);
			FileRecord::from_score(fs)
		})
		.collect();
	let scoring_ms = elapsed_ms(scoring_started.elapsed());

	let multi_source_files = files.iter().filter(|file| file.multi_source).count();
	let accepted_ground_truth_files = files.iter().filter(|file| file.accepted_ok).count();
	let accepted_multi_source_files = files
		.iter()
		.filter(|file| file.multi_source && file.accepted_ok)
		.count();
	let mut all_ground_truth_verdicts: BTreeMap<String, usize> = BTreeMap::new();
	let mut multi_source_verdicts: BTreeMap<String, usize> = BTreeMap::new();
	for file in &files {
		*all_ground_truth_verdicts
			.entry(file.verdict.clone())
			.or_default() += 1;
		if file.multi_source {
			*multi_source_verdicts
				.entry(file.verdict.clone())
				.or_default() += 1;
		}
	}

	// Serialise MergeReportStatus via serde → "ready" / "blocked" etc.
	let merge_status = serde_json::to_value(request.report.status)
		.ok()
		.and_then(|v| v.as_str().map(str::to_string));

	let validation = serde_json::to_value(&request.report.validation).ok();
	let total_ms = setup_ms
		.saturating_add(request.merge_ms)
		.saturating_add(scoring_ms);
	let resolutions = files
		.iter()
		.filter(|file| file.multi_source)
		.filter_map(|file| {
			classify_resolution(
				&file.rel,
				&source_mods,
				request.compatch_dir,
				request.basegame_root,
			)
			.map(|resolution| (file.rel.clone(), resolution))
		})
		.collect();

	let result = CaseResult {
		compatch_id: request.case.compatch_id.clone(),
		title: request.case.title.clone(),
		referenced_mods: request.case.referenced_mods.clone(),
		merge_status,
		validation,
		ground_truth_files: files.len(),
		multi_source_files,
		all_ground_truth_verdicts,
		multi_source_verdicts,
		accepted_ground_truth_files,
		accepted_multi_source_files,
		timings: CaseTimings {
			setup_ms,
			merge_ms: request.merge_ms,
			scoring_ms,
			total_ms,
		},
		files,
	};
	Ok(ExistingOutputScore {
		result,
		resolutions,
	})
}

fn elapsed_ms(duration: Duration) -> u64 {
	u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
