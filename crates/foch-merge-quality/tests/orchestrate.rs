//! Integration tests for orchestrate::score_case / run / learn.
//!
//! The committed fixture archive already contains a temp "workshop directory"
//! (`workshop/<steam_id>/...`), which is exactly how a real Steam Workshop
//! download looks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use foch_merge_quality::corpus::{Case, Corpus};
use foch_merge_quality::orchestrate::{self, CaseResult, RunOptions};

// ------------------------------------------------------------------ helpers

fn fixtures_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
}

/// Build a temp workshop dir from the fixture slice for compatch 3630876155,
/// unpacked from the committed compressed corpus archive.
///
/// Returns (TempDir, workshop_path) — hold the TempDir alive for the test.
fn build_workshop_3630876155() -> (tempfile::TempDir, PathBuf) {
	let unpacked = tempfile::tempdir().expect("unpack dir");
	foch_merge_quality::archive::unpack(&fixtures_root().join("corpus.tar.gz"), unpacked.path())
		.expect("unpack corpus.tar.gz");
	let ws = unpacked.path().join("workshop");
	(unpacked, ws)
}

fn case_3630876155() -> Case {
	Case {
		compatch_id: "3630876155".to_string(),
		title: "Expanded Family + Italian Patchwork".to_string(),
		patched: vec!["2164202838".to_string(), "2185445645".to_string()],
		..Default::default()
	}
}

// ------------------------------------------------------------------ Test 1: score_case

/// Validated ground truth: 7 overlapping files, verdicts exactly as below.
#[test]
fn score_case_verdict_tally() {
	let (_tmp, ws) = build_workshop_3630876155();
	let case = case_3630876155();

	let result = orchestrate::score_case(&case, &ws, false).expect("score_case succeeds");

	assert_eq!(result.overlap_files, 7, "overlap file count");

	let expected: BTreeMap<String, usize> = BTreeMap::from([
		("diverges_ast".to_string(), 5),
		("matches_human".to_string(), 2),
	]);
	assert_eq!(result.verdicts, expected, "verdict tally");
}

// ------------------------------------------------------------------ Test 2: run

/// `run` with a single-case corpus writes results.json (with the right tally)
/// and a non-empty report.md.
#[test]
fn run_writes_artifacts() {
	let (_tmp, ws) = build_workshop_3630876155();

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
	assert_eq!(records[0].overlap_files, 7);

	let expected: BTreeMap<String, usize> = BTreeMap::from([
		("diverges_ast".to_string(), 5),
		("matches_human".to_string(), 2),
	]);
	assert_eq!(records[0].verdicts, expected);

	// report.md must be non-empty
	let report_md =
		std::fs::read_to_string(results_dir.join("report.md")).expect("report.md written");
	assert!(!report_md.trim().is_empty(), "report.md is non-empty");
}

// ------------------------------------------------------------------ Test 3: learn

/// `learn` re-reads the mod + compatch files, classifies human resolutions, and
/// writes a rules.md with the four canonical sections.
#[test]
fn learn_writes_rules_md() {
	// Use the real fixture workshop so classify_resolution can read the files.
	let (_tmp, ws) = build_workshop_3630876155();
	let case = case_3630876155();

	// Score first to obtain a CaseResult with real overlap file records.
	let result = orchestrate::score_case(&case, &ws, false).expect("score_case");

	let results_tmp = tempfile::tempdir().expect("results tmp");
	let results_dir = results_tmp.path();

	// Write results.json with real file-level data.
	let json = serde_json::to_string_pretty(&[result]).unwrap();
	std::fs::write(results_dir.join("results.json"), json).unwrap();

	// learn now takes workshop_dir as well.
	orchestrate::learn(results_dir, &ws).expect("learn succeeds");

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
