//! Reproducible, network-free merge-quality scoring over the committed corpus.
//!
//! The corpus is committed as a single compressed archive
//! `tests/fixtures/corpus.tar.gz`. It contains a deduplicated full Workshop-like
//! layout (`workshop/<steam_id>/...`) plus a fixture-local `corpus.json`.
//! Full context is required because foch's merge strategy depends on
//! workspace-wide validation.
//!
//! `tests/fixtures/expected.json` is the committed baseline: `compatch_id ->
//! { verdict -> count }`. Regenerate both with `foch-mq run` + `extract-fixtures`
//! when the corpus grows. See `fixtures/CREDITS.md` for provenance + takedown.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use foch_merge_quality::corpus::Corpus;
use foch_merge_quality::orchestrate::score_case;

fn fixtures_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
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

	// Unpack the committed compressed corpus into a temp dir once.
	let corpus = tempfile::tempdir().expect("corpus unpack dir");
	foch_merge_quality::archive::unpack(&fixtures_root().join("corpus.tar.gz"), corpus.path())
		.expect("unpack corpus.tar.gz");
	let fixture_corpus_text =
		std::fs::read_to_string(corpus.path().join("corpus.json")).expect("read fixture corpus");
	let fixture_corpus = Corpus::from_json(&fixture_corpus_text).expect("parse fixture corpus");
	let workshop = corpus.path().join("workshop");

	for (compatch_id, want) in &expected {
		let case = fixture_corpus
			.cases
			.iter()
			.find(|case| &case.compatch_id == compatch_id)
			.unwrap_or_else(|| panic!("fixture corpus contains case {compatch_id}"));
		let result = score_case(case, &workshop, false).expect("score full fixture case");
		assert_eq!(
			&result.verdicts, want,
			"merge-quality verdicts drifted for compatch {compatch_id}"
		);
	}
}
