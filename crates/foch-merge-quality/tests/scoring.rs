//! Reproducible, network-free merge-quality scoring over committed fixtures.
//!
//! Each fixture under `tests/fixtures/<compatch_id>/` holds three slices —
//! `a/`, `b/` (the two patched mods) and `compatch/` (the human ground truth) —
//! containing only the scored files. Merging just those files reproduces the
//! full-mod verdicts (foch's per-file merge is local), so this gates merge
//! quality without shipping multi-GB mods. See `fixtures/CREDITS.md` for
//! provenance and the takedown policy.

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

/// Merge `a`+`b`, score every ground-truth file against `compatch`, return the
/// verdict tally keyed by verdict name.
fn score_case(compatch_id: &str, mod_a: &str, mod_b: &str) -> BTreeMap<String, usize> {
	let case_dir = fixtures_root().join(compatch_id);
	let a_dir = case_dir.join("a");
	let b_dir = case_dir.join("b");
	let compatch_dir = case_dir.join("compatch");
	assert!(a_dir.is_dir() && b_dir.is_dir() && compatch_dir.is_dir());

	let tmp = tempfile::tempdir().expect("scratch playset dir");
	let out_dir = tmp.path().join("out");
	let dlc = write_playset(
		tmp.path(),
		&[
			(mod_a.to_string(), a_dir.clone()),
			(mod_b.to_string(), b_dir.clone()),
		],
	)
	.expect("write playset");

	// force = false: foch withholds manual conflicts rather than picking a
	// winner, so `conflict_withheld` stays a distinct signal from auto-merges.
	let result = run_merge(&dlc, &out_dir, /*force=*/ false).expect("merge runs");
	let conflicts = conflict_rel_paths(&result.report);

	let mut tally: BTreeMap<String, usize> = BTreeMap::new();
	for rel in ground_truth_files(&compatch_dir) {
		let fs = score_file(&rel, &a_dir, &b_dir, &compatch_dir, &out_dir, &conflicts);
		*tally.entry(fs.verdict.as_str().to_string()).or_default() += 1;
	}
	tally
}

/// Expanded Mod Family + a sibling overhaul: 7 overlapping files. foch
/// auto-merges 3 (structurally diverging from the human) and withholds 4
/// interface conflicts that the human compatch resolved by hand.
#[test]
fn expanded_family_compatch_3630876155() {
	let tally = score_case("3630876155", "2164202838", "2185445645");

	let expected = BTreeMap::from([
		("conflict_withheld".to_string(), 4usize),
		("diverges_structure".to_string(), 2usize),
		("diverges_formatting".to_string(), 1usize),
	]);
	assert_eq!(
		tally, expected,
		"merge-quality verdicts drifted for compatch 3630876155"
	);
}
