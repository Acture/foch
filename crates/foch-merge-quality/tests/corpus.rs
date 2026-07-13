//! The committed `corpus.json` must always parse into the [`Corpus`] model.

use std::path::Path;

use foch_merge_quality::corpus::{CORPUS_SCHEMA, Corpus, OracleExclusionReason, OracleStatus};

#[test]
fn committed_corpus_parses() {
	let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus.json");
	let text = std::fs::read_to_string(&path).expect("read corpus.json");
	let corpus = Corpus::from_json(&text).expect("corpus.json parses into Corpus");

	assert_eq!(corpus.schema, CORPUS_SCHEMA);
	assert!(!corpus.cases.is_empty(), "corpus has cases");
	assert!(
		corpus.cases.iter().all(|c| c.referenced_mods.len() >= 2),
		"every candidate references at least two mods"
	);

	let scored = corpus
		.cases
		.iter()
		.find(|c| c.compatch_id == "3630876155")
		.expect("the fixture's compatch is present in the corpus");
	assert_eq!(scored.referenced_mods, ["2164202838", "2185445645"]);
	assert_eq!(scored.referenced_mod_meta.len(), 2);
	assert_eq!(scored.oracle_assessment().status, OracleStatus::Proposed);

	let false_positive = corpus
		.cases
		.iter()
		.find(|case| case.compatch_id == "1449952810")
		.expect("the known broad-search false positive remains auditable");
	let assessment = false_positive.oracle_assessment();
	assert_eq!(assessment.status, OracleStatus::Excluded);
	assert_eq!(
		assessment.exclusion_reason,
		Some(OracleExclusionReason::MissingExplicitCompatibilityIntent)
	);
}
