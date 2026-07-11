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

mod common;

use foch_merge_quality::corpus::Corpus;
use foch_merge_quality::orchestrate::score_case_with_cache;
use foch_merge_quality::score::ScoreCache;

/// Every committed fixture reproduces its baseline verdict tally. Data-driven:
/// add a case by running `extract-fixtures` + regenerating `expected.json` —
/// no test code change needed.
#[test]
fn committed_corpus_reproduces_baseline() {
	let expected = common::expected_verdicts();
	assert!(!expected.is_empty(), "baseline has at least one case");

	// Reuse the immutable committed corpus by archive hash. Re-inflating 100MB
	// of gzip and 7700+ files dominates repeated local scoring runs.
	let corpus = common::cached_corpus_root();
	let fixture_corpus_text =
		std::fs::read_to_string(corpus.join("corpus.json")).expect("read fixture corpus");
	let fixture_corpus = Corpus::from_json(&fixture_corpus_text).expect("parse fixture corpus");
	let workshop = corpus.join("workshop");
	let mut score_cache = ScoreCache::new();

	for (compatch_id, want) in &expected {
		let case = fixture_corpus
			.cases
			.iter()
			.find(|case| &case.compatch_id == compatch_id)
			.unwrap_or_else(|| panic!("fixture corpus contains case {compatch_id}"));
		let result = score_case_with_cache(case, &workshop, false, &mut score_cache)
			.expect("score full fixture case");
		assert_eq!(
			&result.multi_source_verdicts, want,
			"merge-quality verdicts drifted for compatch {compatch_id}"
		);
	}
}
