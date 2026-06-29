//! Integration tests for orchestrate::score_case / run / learn.
//!
//! The committed fixture at tests/fixtures/3630876155/{a,b,compatch} is
//! reorganised into a temp "workshop directory" (flat by mod-id), which is
//! exactly how a real Steam Workshop download looks.

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

/// Recursively copy src into dst (dst is created if absent).
fn copy_dir_all(src: &Path, dst: &Path) {
	std::fs::create_dir_all(dst).expect("create dst");
	for entry in walkdir::WalkDir::new(src)
		.into_iter()
		.filter_map(Result::ok)
	{
		let rel = entry.path().strip_prefix(src).unwrap();
		let dest = dst.join(rel);
		if entry.file_type().is_dir() {
			std::fs::create_dir_all(&dest).ok();
		} else {
			if let Some(p) = dest.parent() {
				std::fs::create_dir_all(p).ok();
			}
			std::fs::copy(entry.path(), &dest).expect("copy file");
		}
	}
}

/// Build a temp workshop dir from the fixture slice for compatch 3630876155.
///
/// Returns (TempDir, workshop_path) — hold the TempDir alive for the test.
fn build_workshop_3630876155() -> (tempfile::TempDir, PathBuf) {
	let fixture = fixtures_root().join("3630876155");
	let tmp = tempfile::tempdir().expect("temp dir");
	let ws = tmp.path().to_path_buf();

	copy_dir_all(&fixture.join("a"), &ws.join("2164202838"));
	copy_dir_all(&fixture.join("b"), &ws.join("2185445645"));
	copy_dir_all(&fixture.join("compatch"), &ws.join("3630876155"));

	(tmp, ws)
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
		("conflict_withheld".to_string(), 4),
		("diverges_structure".to_string(), 2),
		("diverges_formatting".to_string(), 1),
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
		("conflict_withheld".to_string(), 4),
		("diverges_structure".to_string(), 2),
		("diverges_formatting".to_string(), 1),
	]);
	assert_eq!(records[0].verdicts, expected);

	// report.md must be non-empty
	let report_md =
		std::fs::read_to_string(results_dir.join("report.md")).expect("report.md written");
	assert!(!report_md.trim().is_empty(), "report.md is non-empty");
}

// ------------------------------------------------------------------ Test 3: learn

/// `learn` reads results.json, writes rules.md containing verdict summaries.
#[test]
fn learn_writes_rules_md() {
	// Build a hand-crafted results.json with known verdict data
	let results: Vec<serde_json::Value> = vec![serde_json::json!({
		"compatch_id": "3630876155",
		"title": "Test Case",
		"patched": ["2164202838", "2185445645"],
		"merge_status": "blocked",
		"validation": null,
		"ground_truth_files": 7,
		"overlap_files": 7,
		"verdicts": {
			"conflict_withheld": 4,
			"diverges_structure": 2,
			"diverges_formatting": 1
		},
		"files": []
	})];

	let tmp = tempfile::tempdir().expect("learn tmp");
	let results_dir = tmp.path();
	std::fs::write(
		results_dir.join("results.json"),
		serde_json::to_string_pretty(&results).unwrap(),
	)
	.unwrap();

	orchestrate::learn(results_dir).expect("learn succeeds");

	let rules = std::fs::read_to_string(results_dir.join("rules.md")).expect("rules.md written");

	// Must mention the verdict names present in the data
	assert!(
		rules.contains("conflict_withheld"),
		"rules.md mentions conflict_withheld"
	);
	assert!(
		rules.contains("diverges_structure"),
		"rules.md mentions diverges_structure"
	);
}
