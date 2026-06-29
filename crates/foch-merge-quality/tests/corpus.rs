//! The committed `corpus.json` must always parse into the [`Corpus`] model.

use std::path::Path;

use foch_merge_quality::corpus::Corpus;

#[test]
fn committed_corpus_parses() {
	let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus.json");
	let text = std::fs::read_to_string(&path).expect("read corpus.json");
	let corpus = Corpus::from_json(&text).expect("corpus.json parses into Corpus");

	assert!(!corpus.cases.is_empty(), "corpus has cases");
	assert!(
		corpus.cases.iter().all(|c| c.patched.len() >= 2),
		"every case pairs at least two patched mods"
	);

	let scored = corpus
		.cases
		.iter()
		.find(|c| c.compatch_id == "3630876155")
		.expect("the fixture's compatch is present in the corpus");
	assert_eq!(scored.patched, ["2164202838", "2185445645"]);
	assert_eq!(scored.patched_meta.len(), 2);
}
