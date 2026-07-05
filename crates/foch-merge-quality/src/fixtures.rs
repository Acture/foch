//! Extract full local Workshop cases into the committed corpus archive.
//!
//! The archive layout is deduplicated:
//! - `corpus.json` contains the selected cases.
//! - `workshop/<steam_id>/...` contains the full local compatch/mod directory.
//!
//! Full context matters because foch's merge strategy depends on workspace-wide
//! validation. Sliced fixtures can drift from the full-mod verdict.

use std::fs;
use std::path::Path;

use crate::CmdResult;

/// Copy `src` to `dst`, creating parent directories as needed.
fn copy_file(src: &Path, dst: &Path) -> std::io::Result<()> {
	if let Some(parent) = dst.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::copy(src, dst)?;
	Ok(())
}

/// Extract fixtures for the given compatch `ids` (empty = all fully-local cases
/// in the corpus) from `workshop_dir` into `out_dir`.
pub fn extract(corpus: &Path, workshop_dir: &Path, out_dir: &Path, ids: &[String]) -> CmdResult {
	let corpus_text = fs::read_to_string(corpus)?;
	let corpus = crate::corpus::Corpus::from_json(&corpus_text)?;

	let cases: Vec<&crate::corpus::Case> = if ids.is_empty() {
		corpus
			.cases
			.iter()
			.filter(|case| {
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
	let mut extracted_cases = Vec::new();
	for case in cases {
		if case.patched.len() < 2 {
			eprintln!(
				"  [extract] skip {}: fewer than 2 patched mods",
				case.compatch_id
			);
			skipped += 1;
			continue;
		}
		match extract_one(case, workshop_dir, out_dir) {
			Ok(0) => {
				eprintln!("  [extract] skip {}: no files copied", case.compatch_id);
				skipped += 1;
			}
			Ok(n) => {
				eprintln!("  [extract] {} — {n} files", case.compatch_id);
				extracted += 1;
				extracted_cases.push(case.clone());
			}
			Err(err) => {
				eprintln!("  [extract] skip {}: {err}", case.compatch_id);
				skipped += 1;
			}
		}
	}

	if !extracted_cases.is_empty() {
		let fixture_corpus = crate::corpus::Corpus {
			generated_at: corpus.generated_at,
			tool_commit: corpus.tool_commit.clone(),
			search_terms: corpus.search_terms.clone(),
			cases: extracted_cases,
		};
		fs::write(
			out_dir.join("corpus.json"),
			fixture_corpus.to_json_pretty()?,
		)?;
	}
	eprintln!("[extract] done: {extracted} extracted, {skipped} skipped");
	Ok(())
}

/// Extract one full case. Returns the number of files copied.
fn extract_one(
	case: &crate::corpus::Case,
	workshop_dir: &Path,
	out_dir: &Path,
) -> Result<usize, Box<dyn std::error::Error>> {
	let mut ids = Vec::with_capacity(case.patched.len() + 1);
	ids.push(case.compatch_id.clone());
	ids.extend(case.patched.iter().cloned());

	let mut copied = 0usize;
	for id in ids {
		let src = workshop_dir.join(&id);
		if !src.is_dir() {
			continue;
		}
		let dst = out_dir.join("workshop").join(&id);
		if dst.is_dir() {
			continue;
		}
		copied += copy_dir_all(&src, &dst)?;
	}

	Ok(copied)
}

/// Copy all files foch may read from a mod directory. VCS and platform metadata
/// are intentionally excluded; they are not part of the playable mod content and
/// have caused large accidental fixture bloat in real Workshop packages.
fn copy_dir_all(src: &Path, dst: &Path) -> Result<usize, Box<dyn std::error::Error>> {
	let mut copied = 0usize;
	for entry in walkdir::WalkDir::new(src)
		.into_iter()
		.filter_entry(|entry| {
			let name = entry.file_name().to_string_lossy();
			name != ".git" && name != ".DS_Store"
		}) {
		let entry = entry?;
		if !entry.file_type().is_file() {
			continue;
		}
		let rel = entry.path().strip_prefix(src)?;
		copy_file(entry.path(), &dst.join(rel))?;
		copied += 1;
	}
	Ok(copied)
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	fn write_file(base: &Path, rel: &str, content: &str) {
		let path = base.join(rel);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).unwrap();
		}
		fs::write(path, content).unwrap();
	}

	fn run_extract(ws: &Path, compatch_id: &str, mods: &[&str]) -> TempDir {
		let corpus = crate::corpus::Corpus {
			cases: vec![crate::corpus::Case {
				compatch_id: compatch_id.to_string(),
				patched: mods.iter().map(|s| s.to_string()).collect(),
				..Default::default()
			}],
			..Default::default()
		};
		let corpus_dir = TempDir::new().unwrap();
		let corpus_path = corpus_dir.path().join("corpus.json");
		fs::write(&corpus_path, corpus.to_json_pretty().unwrap()).unwrap();
		let out = TempDir::new().unwrap();
		extract(&corpus_path, ws, out.path(), &[compatch_id.to_string()])
			.expect("extract succeeds");
		out
	}

	#[test]
	fn extract_copies_full_case_context() {
		let ws = TempDir::new().unwrap();
		let (cid, a, b) = ("9999999991", "1111111111", "2222222222");

		write_file(&ws.path().join(cid), "common/x.txt", "x in compatch\n");
		write_file(&ws.path().join(cid), "interface/y.gui", "y in compatch\n");
		write_file(&ws.path().join(a), "common/x.txt", "x in mod a\n");
		write_file(&ws.path().join(a), "interface/y.gui", "y in mod a\n");
		write_file(&ws.path().join(a), "descriptor.mod", "name=\"mod_a\"\n");
		write_file(&ws.path().join(b), "common/x.txt", "x in mod b\n");

		let out = run_extract(ws.path(), cid, &[a, b]);
		let workshop = out.path().join("workshop");

		assert!(workshop.join(cid).join("common/x.txt").is_file());
		assert!(workshop.join(cid).join("interface/y.gui").is_file());
		assert!(workshop.join(a).join("common/x.txt").is_file());
		assert!(workshop.join(a).join("interface/y.gui").is_file());
		assert!(workshop.join(a).join("descriptor.mod").is_file());
		assert!(workshop.join(b).join("common/x.txt").is_file());
		assert!(!workshop.join(b).join("descriptor.mod").exists());
		assert_eq!(
			fs::read(workshop.join(a).join("common/x.txt")).unwrap(),
			b"x in mod a\n"
		);
		assert_eq!(
			fs::read(workshop.join(b).join("common/x.txt")).unwrap(),
			b"x in mod b\n"
		);

		let fixture_corpus =
			fs::read_to_string(out.path().join("corpus.json")).expect("fixture corpus written");
		let fixture_corpus =
			crate::corpus::Corpus::from_json(&fixture_corpus).expect("fixture corpus parses");
		assert_eq!(fixture_corpus.cases.len(), 1);
		assert_eq!(fixture_corpus.cases[0].compatch_id, cid);
	}

	#[test]
	fn extract_keeps_symbol_only_case() {
		let ws = TempDir::new().unwrap();
		let (cid, a, b) = ("9999999992", "1111111111", "2222222222");

		write_file(
			&ws.path().join(cid),
			"common/scripted_triggers/patch.txt",
			"trig_x = {\n\tmerged = yes\n}\n",
		);
		write_file(
			&ws.path().join(a),
			"common/scripted_triggers/a.txt",
			"trig_x = {\n\tfrom = a\n}\n",
		);
		write_file(
			&ws.path().join(b),
			"common/scripted_triggers/b.txt",
			"trig_x = {\n\tfrom = b\n}\n",
		);

		let out = run_extract(ws.path(), cid, &[a, b]);
		assert!(out.path().join("workshop").join(cid).is_dir());
		assert!(out.path().join("workshop").join(a).is_dir());
		assert!(out.path().join("workshop").join(b).is_dir());
	}

	#[test]
	fn extract_skips_vcs_metadata() {
		let ws = TempDir::new().unwrap();
		let (cid, a, b) = ("9999999993", "1111111111", "2222222222");

		write_file(&ws.path().join(cid), "common/x.txt", "x\n");
		write_file(&ws.path().join(a), "common/a.txt", "a\n");
		write_file(&ws.path().join(a), ".git/objects/blob", "not game data\n");
		write_file(&ws.path().join(b), "common/b.txt", "b\n");

		let out = run_extract(ws.path(), cid, &[a, b]);
		assert!(
			out.path()
				.join("workshop")
				.join(a)
				.join("common/a.txt")
				.is_file()
		);
		assert!(
			!out.path()
				.join("workshop")
				.join(a)
				.join(".git/objects/blob")
				.exists()
		);
	}
}
