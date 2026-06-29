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

	for case in cases {
		if case.patched.len() < 2 {
			eprintln!(
				"  [extract] skip {}: fewer than 2 patched mods",
				case.compatch_id
			);
			continue;
		}

		let compatch_dir = workshop_dir.join(&case.compatch_id);
		let mod_a = workshop_dir.join(&case.patched[0]);
		let mod_b = workshop_dir.join(&case.patched[1]);

		let out_case = out_dir.join(&case.compatch_id);
		let out_compatch = out_case.join("compatch");
		let out_a = out_case.join("a");
		let out_b = out_case.join("b");

		let gt = crate::score::ground_truth_files(&compatch_dir);

		for rel in &gt {
			// Always copy the compatch's hand-merged version (source of truth)
			let src = compatch_dir.join(rel);
			if src.is_file() {
				copy_file(&src, &out_compatch.join(rel))?;
			}
			// Copy mod A's version of this file if it exists
			let a_src = mod_a.join(rel);
			if a_src.is_file() {
				copy_file(&a_src, &out_a.join(rel))?;
			}
			// Copy mod B's version of this file if it exists
			let b_src = mod_b.join(rel);
			if b_src.is_file() {
				copy_file(&b_src, &out_b.join(rel))?;
			}
		}

		// Each mod's descriptor is required by the scoring harness
		copy_file(&mod_a.join("descriptor.mod"), &out_a.join("descriptor.mod"))?;
		copy_file(&mod_b.join("descriptor.mod"), &out_b.join("descriptor.mod"))?;

		eprintln!("  [extract] {} — {} gt files", case.compatch_id, gt.len());
	}

	Ok(())
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

		// compatch/ must contain every gt file
		assert!(
			case_out.join("compatch/common/x.txt").is_file(),
			"compatch/common/x.txt must be extracted"
		);
		assert!(
			case_out.join("compatch/interface/y.gui").is_file(),
			"compatch/interface/y.gui must be extracted"
		);

		// a/ must contain mod A's version of both gt files + descriptor.mod
		assert!(
			case_out.join("a/common/x.txt").is_file(),
			"a/common/x.txt must be extracted"
		);
		assert!(
			case_out.join("a/interface/y.gui").is_file(),
			"a/interface/y.gui must be extracted"
		);
		assert!(
			case_out.join("a/descriptor.mod").is_file(),
			"a/descriptor.mod must be copied"
		);

		// b/ must contain mod B's version of x.txt but NOT y.gui + descriptor.mod
		assert!(
			case_out.join("b/common/x.txt").is_file(),
			"b/common/x.txt must be extracted"
		);
		assert!(
			!case_out.join("b/interface/y.gui").is_file(),
			"b/interface/y.gui must NOT be extracted (absent in mod b)"
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
}
