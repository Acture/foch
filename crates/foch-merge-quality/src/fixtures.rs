//! Extract scored-file slices from a local workshop into the committed test
//! fixture tree: for each selected compatch, copy the files the scorer compares
//! (in both patched mods + the compatch's hand-merged version), plus each mod's
//! `descriptor.mod`. Keeps the scoring test reproducible without shipping mods.

use std::fs;
use std::path::Path;

use crate::CmdResult;

/// Copy `src` to `dst`, creating parent directories as needed.
fn copy_file(src: &Path, dst: &Path) -> std::io::Result<()> {
	if let Some(p) = dst.parent() {
		fs::create_dir_all(p)?;
	}
	fs::copy(src, dst)?;
	Ok(())
}

/// Extract fixtures for the given compatch `ids` (empty = all fully-local cases
/// in the corpus) from `workshop_dir` into `out_dir`.
///
/// For each selected case the output layout is:
/// ```text
/// out_dir/<compatch_id>/compatch/<rel>   — the hand-merged ground truth
/// out_dir/<compatch_id>/a/<rel>          — mod A's version (if present)
/// out_dir/<compatch_id>/b/<rel>          — mod B's version (if present)
/// out_dir/<compatch_id>/a/descriptor.mod
/// out_dir/<compatch_id>/b/descriptor.mod
/// ```
/// where `<rel>` iterates over `ground_truth_files(compatch_dir)`.
pub fn extract(corpus: &Path, workshop_dir: &Path, out_dir: &Path, ids: &[String]) -> CmdResult {
	let corpus_text = fs::read_to_string(corpus)?;
	let corpus = crate::corpus::Corpus::from_json(&corpus_text)?;

	let cases: Vec<&crate::corpus::Case> = if ids.is_empty() {
		corpus
			.cases
			.iter()
			.filter(|case| {
				// "fully local": compatch dir AND all patched mod dirs are present
				workshop_dir.join(&case.compatch_id).is_dir()
					&& case.patched.iter().all(|p| workshop_dir.join(p).is_dir())
			})
			.collect()
	} else {
		corpus
			.cases
			.iter()
			.filter(|case| ids.contains(&case.compatch_id))
			.collect()
	};

	let (mut extracted, mut skipped) = (0usize, 0usize);
	for case in cases {
		if case.patched.len() < 2 {
			eprintln!(
				"  [extract] skip {}: fewer than 2 patched mods",
				case.compatch_id
			);
			skipped += 1;
			continue;
		}
		// One bad case (e.g. a mod missing its descriptor) must not abort the
		// whole corpus extraction — log it and carry on.
		match extract_one(case, workshop_dir, out_dir) {
			Ok(0) => {
				// No files modified by BOTH mods → no conflict to score. Such a
				// "compatch" resolves nothing here (often a discovery false
				// positive / standalone mod); don't persist an empty case.
				eprintln!(
					"  [extract] skip {}: 0 overlap files (not a 2-mod conflict set)",
					case.compatch_id
				);
				skipped += 1;
			}
			Ok(n) => {
				eprintln!("  [extract] {} — {n} overlap files", case.compatch_id);
				extracted += 1;
			}
			Err(err) => {
				eprintln!("  [extract] skip {}: {err}", case.compatch_id);
				skipped += 1;
			}
		}
	}
	eprintln!("[extract] done: {extracted} extracted, {skipped} skipped");
	Ok(())
}

/// Extract one case's slices. Returns the ground-truth file count.
fn extract_one(
	case: &crate::corpus::Case,
	workshop_dir: &Path,
	out_dir: &Path,
) -> std::io::Result<usize> {
	let compatch_dir = workshop_dir.join(&case.compatch_id);
	let mod_a = workshop_dir.join(&case.patched[0]);
	let mod_b = workshop_dir.join(&case.patched[1]);

	let out_case = out_dir.join(&case.compatch_id);
	let out_compatch = out_case.join("compatch");
	let out_a = out_case.join("a");
	let out_b = out_case.join("b");

	let gt = crate::score::ground_truth_files(&compatch_dir);

	let mut copied = 0usize;
	for rel in &gt {
		let a_src = mod_a.join(rel);
		let b_src = mod_b.join(rel);
		// Only OVERLAP files — present in BOTH patched mods — are scored conflicts.
		// A compatch can ship thousands of non-conflict glue/passthrough files;
		// persisting the full ground-truth set would bloat the repo. The verdict
		// tally is computed over overlaps only, so the overlap slice reproduces it.
		if !(a_src.is_file() && b_src.is_file()) {
			continue;
		}
		let src = compatch_dir.join(rel);
		if src.is_file() {
			copy_file(&src, &out_compatch.join(rel))?;
		}
		copy_file(&a_src, &out_a.join(rel))?;
		copy_file(&b_src, &out_b.join(rel))?;
		copied += 1;
	}

	// No overlaps → nothing scored → don't create an (effectively empty) case
	// dir holding only descriptors.
	if copied == 0 {
		return Ok(0);
	}

	// descriptor.mod is useful metadata but not present in every mod — copy it
	// when it exists, skip silently otherwise (the scorer tolerates its absence).
	for (mod_dir, out) in [(&mod_a, &out_a), (&mod_b, &out_b)] {
		let desc = mod_dir.join("descriptor.mod");
		if desc.is_file() {
			copy_file(&desc, &out.join("descriptor.mod"))?;
		}
	}

	Ok(copied)
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	fn write_file(base: &Path, rel: &str, content: &str) {
		let path = base.join(rel);
		if let Some(p) = path.parent() {
			fs::create_dir_all(p).unwrap();
		}
		fs::write(path, content).unwrap();
	}

	/// Hermetic: builds a synthetic temp workshop (no real Steam dir), runs
	/// `extract` for one case, and asserts the output fixture layout.
	#[test]
	fn extract_copies_gt_files_and_descriptors() {
		let ws = TempDir::new().unwrap();
		let ws_path = ws.path();

		let compatch_id = "9999999999";
		let mod_a_id = "1111111111";
		let mod_b_id = "2222222222";

		let compatch_dir = ws_path.join(compatch_id);
		let mod_a_dir = ws_path.join(mod_a_id);
		let mod_b_dir = ws_path.join(mod_b_id);

		// Ground-truth files in the compatch (extensions not in SKIP_EXTS)
		write_file(&compatch_dir, "common/x.txt", "x in compatch\n");
		write_file(&compatch_dir, "interface/y.gui", "y in compatch\n");

		// Mod A has both gt files + descriptor.mod
		write_file(&mod_a_dir, "common/x.txt", "x in mod a\n");
		write_file(&mod_a_dir, "interface/y.gui", "y in mod a\n");
		write_file(&mod_a_dir, "descriptor.mod", "name=\"mod_a\"\n");

		// Mod B has only x.txt (y.gui is absent) + descriptor.mod
		write_file(&mod_b_dir, "common/x.txt", "x in mod b\n");
		write_file(&mod_b_dir, "descriptor.mod", "name=\"mod_b\"\n");

		// Build corpus.json with one case
		let corpus = crate::corpus::Corpus {
			cases: vec![crate::corpus::Case {
				compatch_id: compatch_id.to_string(),
				patched: vec![mod_a_id.to_string(), mod_b_id.to_string()],
				..Default::default()
			}],
			..Default::default()
		};
		let corpus_dir = TempDir::new().unwrap();
		let corpus_path = corpus_dir.path().join("corpus.json");
		fs::write(&corpus_path, corpus.to_json_pretty().unwrap()).unwrap();

		// Output directory
		let out_tmp = TempDir::new().unwrap();
		let out_dir = out_tmp.path();

		// Run extract for the specific id
		let ids = vec![compatch_id.to_string()];
		extract(&corpus_path, ws_path, out_dir, &ids).expect("extract succeeds");

		let case_out = out_dir.join(compatch_id);

		// x.txt is an OVERLAP (in both mods) → extracted to all three slices.
		assert!(
			case_out.join("compatch/common/x.txt").is_file(),
			"compatch/common/x.txt (overlap) must be extracted"
		);
		assert!(
			case_out.join("a/common/x.txt").is_file(),
			"a/common/x.txt must be extracted"
		);
		assert!(
			case_out.join("a/descriptor.mod").is_file(),
			"a/descriptor.mod must be copied"
		);
		assert!(
			case_out.join("b/common/x.txt").is_file(),
			"b/common/x.txt must be extracted"
		);

		// y.gui is in the compatch + mod A but NOT mod B → NON-overlap → not a
		// scored conflict → must not be extracted to ANY slice.
		assert!(
			!case_out.join("compatch/interface/y.gui").is_file(),
			"non-overlap y.gui must NOT be extracted"
		);
		assert!(
			!case_out.join("a/interface/y.gui").is_file(),
			"non-overlap y.gui must NOT be extracted to a/"
		);
		assert!(
			!case_out.join("b/interface/y.gui").is_file(),
			"y.gui absent in mod b, and non-overlap → not extracted"
		);
		assert!(
			case_out.join("b/descriptor.mod").is_file(),
			"b/descriptor.mod must be copied"
		);

		// Bytes of one copied file must equal the source exactly
		let src = fs::read(compatch_dir.join("common/x.txt")).unwrap();
		let dst = fs::read(case_out.join("compatch/common/x.txt")).unwrap();
		assert_eq!(src, dst, "copied bytes must match source");

		// The mod versions must reflect mod-specific content (not just compatch)
		let a_bytes = fs::read(case_out.join("a/common/x.txt")).unwrap();
		assert_eq!(a_bytes, b"x in mod a\n", "a/x.txt content must be mod A's");

		let b_bytes = fs::read(case_out.join("b/common/x.txt")).unwrap();
		assert_eq!(b_bytes, b"x in mod b\n", "b/x.txt content must be mod B's");
	}

	/// A mod without a root `descriptor.mod` must not abort extraction — the
	/// descriptor copy is skipped and the case is still extracted. (Regression:
	/// real Workshop mods sometimes lack a top-level descriptor.mod.)
	#[test]
	fn extract_tolerates_missing_descriptor() {
		let ws = TempDir::new().unwrap();
		let (compatch_id, mod_a_id, mod_b_id) = ("9999999999", "1111111111", "2222222222");

		write_file(&ws.path().join(compatch_id), "common/x.txt", "x compatch\n");
		write_file(&ws.path().join(mod_a_id), "common/x.txt", "x a\n");
		write_file(&ws.path().join(mod_a_id), "descriptor.mod", "name=\"a\"\n");
		// Mod B has the gt file but NO descriptor.mod.
		write_file(&ws.path().join(mod_b_id), "common/x.txt", "x b\n");

		let corpus = crate::corpus::Corpus {
			cases: vec![crate::corpus::Case {
				compatch_id: compatch_id.to_string(),
				patched: vec![mod_a_id.to_string(), mod_b_id.to_string()],
				..Default::default()
			}],
			..Default::default()
		};
		let corpus_dir = TempDir::new().unwrap();
		let corpus_path = corpus_dir.path().join("corpus.json");
		fs::write(&corpus_path, corpus.to_json_pretty().unwrap()).unwrap();
		let out_tmp = TempDir::new().unwrap();

		extract(
			&corpus_path,
			ws.path(),
			out_tmp.path(),
			&[compatch_id.to_string()],
		)
		.expect("extract must not abort on a mod missing descriptor.mod");

		let case_out = out_tmp.path().join(compatch_id);
		assert!(
			case_out.join("b/common/x.txt").is_file(),
			"b gt file still extracted"
		);
		assert!(
			!case_out.join("b/descriptor.mod").exists(),
			"absent descriptor is skipped, not fabricated"
		);
		assert!(
			case_out.join("a/descriptor.mod").is_file(),
			"present descriptor still copied"
		);
	}
}
