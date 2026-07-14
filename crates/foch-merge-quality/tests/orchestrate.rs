//! Integration tests for orchestrate::score_case / run / learn.
//!
//! The committed fixture archive already contains a temp "workshop directory"
//! (`workshop/<steam_id>/...`), which is exactly how a real Steam Workshop
//! download looks.

mod common;

use std::path::{Path, PathBuf};

use foch_merge_quality::corpus::{Case, Corpus};
use foch_merge_quality::orchestrate::{
	self, BaseGameMode, CaseResult, RunOptions, score_case_from_paths_with_cache,
};
use foch_merge_quality::score::ScoreCache;

// ------------------------------------------------------------------ helpers

/// Reuse the archive-hash-addressed committed corpus fixture.
fn workshop_3630876155() -> PathBuf {
	common::cached_corpus_root().join("workshop")
}

fn case_3630876155() -> Case {
	Case {
		compatch_id: "3630876155".to_string(),
		title: "Expanded Family Compatch".to_string(),
		referenced_mods: vec!["2164202838".to_string(), "2185445645".to_string()],
		..Default::default()
	}
}

fn write_file(root: &Path, relative: &str, content: &str) {
	let path = root.join(relative);
	std::fs::create_dir_all(path.parent().expect("fixture file parent"))
		.expect("create fixture file parent");
	std::fs::write(path, content).expect("write fixture file");
}

// ------------------------------------------------------------------ Test 1: score_case

/// Validated reference output: 7 overlapping files, verdicts exactly as below.
#[test]
fn score_case_verdict_tally() {
	let ws = workshop_3630876155();
	let case = case_3630876155();

	let result = orchestrate::score_case(&case, &ws, BaseGameMode::ExplicitlyDisabled, false)
		.expect("score_case succeeds");

	assert_eq!(result.ground_truth_files, result.files.len());
	assert_eq!(
		result.ground_truth_files,
		result.all_ground_truth_verdicts.values().sum::<usize>()
	);
	assert_eq!(result.multi_source_files, 7, "multi-source file count");

	let expected = common::expected_verdicts()
		.remove("3630876155")
		.expect("expected verdicts for fixture compatch");
	assert_eq!(result.multi_source_verdicts, expected, "verdict tally");
	assert_eq!(result.accepted_multi_source_files, 4, "accepted tally");
}

#[test]
fn definition_module_paths_count_as_one_ground_truth_unit() {
	let root = tempfile::tempdir().expect("fixture root");
	let mod_a = root.path().join("mod-a");
	let mod_b = root.path().join("mod-b");
	let compatch = root.path().join("compatch");
	let out = root.path().join("out");
	write_file(
		&mod_a,
		"common/governments/a.txt",
		"monarchy = { rank = 1 }\n",
	);
	write_file(
		&mod_b,
		"common/governments/b.txt",
		"republic = { rank = 1 }\n",
	);
	write_file(
		&compatch,
		"common/governments/00_human.txt",
		"monarchy = { rank = 1 }\n",
	);
	write_file(
		&compatch,
		"common/governments/10_human.txt",
		"republic = { rank = 1 }\n",
	);
	let case = Case {
		compatch_id: "human".to_string(),
		title: "two-file governments compatch".to_string(),
		referenced_mods: vec!["mod-a".to_string(), "mod-b".to_string()],
		..Default::default()
	};
	let mut cache = ScoreCache::new();

	let result = score_case_from_paths_with_cache(
		&case,
		&compatch,
		&[mod_a, mod_b],
		&out,
		BaseGameMode::ExplicitlyDisabled,
		None,
		&mut cache,
	)
	.expect("score definition module fixture");

	assert_eq!(result.files.len(), 1);
	assert_eq!(result.ground_truth_files, 1);
	assert_eq!(result.all_ground_truth_verdicts.values().sum::<usize>(), 1);
	assert_eq!(
		result.files[0].rel,
		"common/governments/zzz_foch_governments.txt"
	);
}

// ------------------------------------------------------------------ Test 2: run

/// `run` with a single-case corpus writes results.json (with the right tally)
/// and a non-empty report.md.
#[test]
fn run_writes_artifacts() {
	let ws = workshop_3630876155();

	// Build a minimal corpus.json
	let corpus = Corpus {
		cases: vec![case_3630876155()],
		..Default::default()
	};
	let corpus_json = corpus.to_json_pretty().unwrap();

	let run_tmp = tempfile::tempdir().expect("run tmp");
	let corpus_path = run_tmp.path().join("corpus.json");
	let results_dir = run_tmp.path().join("results");

	std::fs::write(&corpus_path, corpus_json).unwrap();

	orchestrate::run(&RunOptions {
		corpus: &corpus_path,
		workshop_dir: &ws,
		results_dir: &results_dir,
		base_game: BaseGameMode::ExplicitlyDisabled,
		limit: 0,
		keep: false,
		isolate: false, // in-process: the test binary is not `foch-mq`
	})
	.expect("run succeeds");

	// results.json must exist and decode to a vec with one record
	let results_text =
		std::fs::read_to_string(results_dir.join("results.json")).expect("results.json written");
	let records: Vec<CaseResult> =
		serde_json::from_str(&results_text).expect("results.json parses");

	assert_eq!(records.len(), 1);
	assert_eq!(records[0].compatch_id, "3630876155");
	assert_eq!(records[0].multi_source_files, 7);
	assert!(
		records[0].timings.total_ms >= records[0].timings.merge_ms,
		"total timing covers merge timing"
	);

	let expected = common::expected_verdicts()
		.remove("3630876155")
		.expect("expected verdicts for fixture compatch");
	assert_eq!(records[0].multi_source_verdicts, expected);
	assert_eq!(records[0].accepted_multi_source_files, 4);

	// report.md must be non-empty
	let report_md =
		std::fs::read_to_string(results_dir.join("report.md")).expect("report.md written");
	assert!(!report_md.trim().is_empty(), "report.md is non-empty");
	assert!(
		report_md.contains("Timing: total="),
		"report.md includes aggregate timing"
	);
	assert!(
		report_md.contains("- timings: total="),
		"report.md includes per-case timing"
	);
}

// ------------------------------------------------------------------ Test 3: learn

/// `learn` re-reads the mod + compatch files, classifies human resolutions, and
/// writes a rules.md with the four canonical sections.
#[test]
fn learn_writes_rules_md() {
	// Use the real fixture workshop so classify_resolution can read the files.
	let ws = workshop_3630876155();
	let case = case_3630876155();

	// Score first to obtain a CaseResult with real overlap file records.
	let result = orchestrate::score_case(&case, &ws, BaseGameMode::ExplicitlyDisabled, false)
		.expect("score_case");

	let results_tmp = tempfile::tempdir().expect("results tmp");
	let results_dir = results_tmp.path();

	// Write results.json with real file-level data.
	let json = serde_json::to_string_pretty(&[result]).unwrap();
	std::fs::write(results_dir.join("results.json"), json).unwrap();

	// learn now takes workshop_dir as well.
	orchestrate::learn(results_dir, &ws, None).expect("learn succeeds");

	let rules = std::fs::read_to_string(results_dir.join("rules.md")).expect("rules.md written");

	// Must contain all four Python-compat section headers.
	assert!(
		rules.contains("## Order-independent rule"),
		"rules.md must have crosstab section"
	);
	assert!(
		rules.contains("## How humans resolve overlaps (ALL overlapping files)"),
		"rules.md must have ALL section"
	);
	assert!(
		rules.contains("## How humans resolve the conflicts foch WITHHELD"),
		"rules.md must have conflict section"
	);
	assert!(
		rules.contains("## Per-file detail"),
		"rules.md must have per-file section"
	);

	// Per-file detail must have at least one classified row (7 overlap files).
	let has_resolution = rules.contains("union")
		|| rules.contains("took_base")
		|| rules.contains("took_overlay")
		|| rules.contains("hand_edit")
		|| rules.contains("identical");
	assert!(
		has_resolution,
		"rules.md must contain a human resolution verdict"
	);

	// The per-file table must mention at least one fixture file.
	let has_file_row = rules.contains("interface/") || rules.contains("common/");
	assert!(
		has_file_row,
		"rules.md per-file detail must list at least one overlap file"
	);
}
