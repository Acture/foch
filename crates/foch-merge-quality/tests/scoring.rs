//! Reproducible, network-free merge-quality scoring over the committed corpus.
//!
//! Each fixture under `tests/fixtures/<compatch_id>/` holds three slices —
//! `a/`, `b/` (the two patched mods) and `compatch/` (the human ground truth) —
//! containing only the scored OVERLAP files (those present in both mods).
//! Merging just those reproduces the full-mod verdicts (foch's per-file merge is
//! local), so this gates merge quality without shipping multi-GB mods.
//!
//! `tests/fixtures/expected.json` is the committed baseline: `compatch_id ->
//! { verdict -> count }`. Regenerate it with `foch-mq run` + `extract-fixtures`
//! when the corpus grows. See `fixtures/CREDITS.md` for provenance + takedown.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use foch_merge_quality::score::{
	conflict_rel_paths, ground_truth_files, run_merge, score_file, write_playset,
};

fn fixtures_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
}

/// Merge `<case>/a` + `<case>/b` and tally foch's verdict over every scored file.
fn verdict_tally(case_dir: &Path) -> BTreeMap<String, usize> {
	let a_dir = case_dir.join("a");
	let b_dir = case_dir.join("b");
	let compatch_dir = case_dir.join("compatch");
	assert!(
		a_dir.is_dir() && b_dir.is_dir() && compatch_dir.is_dir(),
		"fixture {} must have a/ b/ compatch/",
		case_dir.display()
	);

	let tmp = tempfile::tempdir().expect("scratch playset dir");
	let out_dir = tmp.path().join("out");
	let dlc = write_playset(
		tmp.path(),
		&[
			("a".to_string(), a_dir.clone()),
			("b".to_string(), b_dir.clone()),
		],
	)
	.expect("write playset");
	// force = false: foch withholds manual conflicts (a distinct signal from auto-merges).
	let result = run_merge(&dlc, &out_dir, false).expect("merge runs");
	let conflicts = conflict_rel_paths(&result.report);

	let mut tally: BTreeMap<String, usize> = BTreeMap::new();
	for rel in ground_truth_files(&compatch_dir) {
		let fs = score_file(&rel, &a_dir, &b_dir, &compatch_dir, &out_dir, &conflicts);
		*tally.entry(fs.verdict.as_str().to_string()).or_default() += 1;
	}
	tally
}

/// Every committed fixture reproduces its baseline verdict tally. Data-driven:
/// add a case by running `extract-fixtures` + regenerating `expected.json` —
/// no test code change needed.
#[test]
fn committed_corpus_reproduces_baseline() {
	let expected_text =
		std::fs::read_to_string(fixtures_root().join("expected.json")).expect("read expected.json");
	let expected: BTreeMap<String, BTreeMap<String, usize>> =
		serde_json::from_str(&expected_text).expect("parse expected.json");
	assert!(!expected.is_empty(), "baseline has at least one case");

	for (compatch_id, want) in &expected {
		let got = verdict_tally(&fixtures_root().join(compatch_id));
		assert_eq!(
			&got, want,
			"merge-quality verdicts drifted for compatch {compatch_id}"
		);
	}
}
