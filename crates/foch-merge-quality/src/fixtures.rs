//! Extract full local Workshop cases into the committed corpus archive.
//!
//! The archive layout is deduplicated:
//! - `corpus.json` contains the selected cases.
//! - `workshop/<steam_id>/...` contains the full local compatch/mod directory.
//! - `basegame/...` contains every text file in the version-bound vanilla
//!   installation.
//! - `basegame-manifest.json` binds that snapshot to its game version and
//!   content hash.
//!
//! Full context matters because foch's merge strategy depends on workspace-wide
//! validation. Sliced fixtures can drift from the full-mod verdict.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

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
pub fn extract(
	corpus: &Path,
	workshop_dir: &Path,
	game_root: &Path,
	out_dir: &Path,
	ids: &[String],
) -> CmdResult {
	let corpus_text = fs::read_to_string(corpus)?;
	let corpus = crate::corpus::Corpus::from_json(&corpus_text)?;

	let cases: Vec<&crate::corpus::Case> = if ids.is_empty() {
		corpus
			.cases
			.iter()
			.filter(|case| case.oracle_assessment().is_scorable())
			.filter(|case| {
				workshop_dir.join(&case.compatch_id).is_dir()
					&& case
						.referenced_mods
						.iter()
						.all(|p| workshop_dir.join(p).is_dir())
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
		if case.referenced_mods.len() < 2 {
			eprintln!(
				"  [extract] skip {}: fewer than 2 referenced mods",
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
		extract_basegame_text(game_root, out_dir)?;
		let fixture_corpus = crate::corpus::Corpus {
			schema: corpus.schema.clone(),
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

#[derive(Serialize)]
struct BasegameFixtureManifest<'a> {
	schema: &'static str,
	game: &'static str,
	game_version: &'a str,
	selection: &'static str,
	file_count: usize,
	content_bytes: u64,
	content_hash: String,
}

fn extract_basegame_text(
	game_root: &Path,
	out_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
	let game_version = crate::config::detect_game_version(game_root).ok_or_else(|| {
		format!(
			"failed to detect game version under {}; fixture extraction requires a version-bound base game",
			game_root.display()
		)
	})?;
	let mut retained = Vec::new();
	for entry in walkdir::WalkDir::new(game_root)
		.into_iter()
		.filter_entry(|entry| entry.file_name() != ".DS_Store")
	{
		let entry = entry?;
		if !entry.file_type().is_file() || !is_probably_text_file(entry.path())? {
			continue;
		}
		let relative = entry.path().strip_prefix(game_root)?;
		let normalized = relative.to_string_lossy().replace('\\', "/");
		retained.push((normalized, entry.path().to_path_buf()));
	}
	retained.push(("version.txt".to_string(), game_root.join("version.txt")));
	retained.sort_by(|lhs, rhs| lhs.0.cmp(&rhs.0));
	retained.dedup_by(|lhs, rhs| lhs.0 == rhs.0);

	let basegame_out = out_dir.join("basegame");
	fs::create_dir_all(&basegame_out)?;
	let mut hasher = blake3::Hasher::new();
	let mut file_count = 0usize;
	let mut content_bytes = 0u64;
	let mut copied_version = false;
	for (relative, source) in retained {
		let relative_path = PathBuf::from(&relative);
		if relative_path.is_absolute()
			|| relative_path
				.components()
				.any(|component| matches!(component, std::path::Component::ParentDir))
		{
			return Err(format!("unsafe base-game fixture path: {relative}").into());
		}
		let bytes = if relative == "version.txt" {
			copied_version = true;
			format!("{game_version}\n").into_bytes()
		} else {
			fs::read(&source)?
		};
		let destination = basegame_out.join(&relative_path);
		if let Some(parent) = destination.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::write(destination, &bytes)?;
		hasher.update(&(relative.len() as u64).to_le_bytes());
		hasher.update(relative.as_bytes());
		hasher.update(&(bytes.len() as u64).to_le_bytes());
		hasher.update(&bytes);
		file_count += 1;
		content_bytes += bytes.len() as u64;
		if file_count.is_multiple_of(1_000) {
			eprintln!("  [extract] base game text - {file_count} files");
		}
	}
	debug_assert!(copied_version, "synthetic version.txt is always retained");
	let manifest = BasegameFixtureManifest {
		schema: "1.0.0",
		game: "eu4",
		game_version: &game_version,
		selection: "all-probable-text-files-v1",
		file_count,
		content_bytes,
		content_hash: hasher.finalize().to_hex().to_string(),
	};
	fs::write(
		out_dir.join("basegame-manifest.json"),
		serde_json::to_vec_pretty(&manifest)?,
	)?;
	eprintln!(
		"  [extract] base game {} - {} text files ({} bytes)",
		game_version, file_count, content_bytes
	);
	Ok(())
}

/// Git-style binary detection with a control-byte guard. EU4 text is not
/// uniformly UTF-8, so validating an encoding would incorrectly drop valid
/// Clausewitz and localisation inputs.
fn is_probably_text_file(path: &Path) -> std::io::Result<bool> {
	const SAMPLE_BYTES: usize = 8 * 1024;
	let mut sample = [0u8; SAMPLE_BYTES];
	let read = fs::File::open(path)?.read(&mut sample)?;
	let sample = &sample[..read];
	if sample.is_empty() {
		return Ok(true);
	}
	if sample.starts_with(&[0xff, 0xfe]) || sample.starts_with(&[0xfe, 0xff]) {
		return Ok(true);
	}
	if sample.contains(&0) {
		return Ok(false);
	}
	let control_bytes = sample
		.iter()
		.filter(|byte| matches!(byte, 0x01..=0x08 | 0x0b | 0x0e..=0x1f | 0x7f))
		.count();
	Ok(control_bytes * 100 <= sample.len())
}

/// Extract one full case. Returns the number of files copied.
fn extract_one(
	case: &crate::corpus::Case,
	workshop_dir: &Path,
	out_dir: &Path,
) -> Result<usize, Box<dyn std::error::Error>> {
	let mut ids = Vec::with_capacity(case.referenced_mods.len() + 1);
	ids.push(case.compatch_id.clone());
	ids.extend(case.referenced_mods.iter().cloned());

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
				referenced_mods: mods.iter().map(|s| s.to_string()).collect(),
				..Default::default()
			}],
			..Default::default()
		};
		let corpus_dir = TempDir::new().unwrap();
		let corpus_path = corpus_dir.path().join("corpus.json");
		fs::write(&corpus_path, corpus.to_json_pretty().unwrap()).unwrap();
		let game = TempDir::new().unwrap();
		write_file(game.path(), "version.txt", "1.37.5\n");
		write_file(game.path(), "common/x.txt", "x in vanilla\n");
		write_file(game.path(), "common/defines.lua", "NDefines = {}\n");
		write_file(game.path(), "interface/y.gui", "y in vanilla\n");
		write_file(game.path(), "README.md", "vanilla notes\n");
		let binary = game.path().join("gfx/not-text.dds");
		fs::create_dir_all(binary.parent().unwrap()).unwrap();
		fs::write(binary, [0x44, 0x44, 0x53, 0, 0x01, 0x02]).unwrap();
		let out = TempDir::new().unwrap();
		extract(
			&corpus_path,
			ws,
			game.path(),
			out.path(),
			&[compatch_id.to_string()],
		)
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
		assert_eq!(
			fs::read(out.path().join("basegame/common/x.txt")).unwrap(),
			b"x in vanilla\n"
		);
		assert_eq!(
			fs::read(out.path().join("basegame/common/defines.lua")).unwrap(),
			b"NDefines = {}\n"
		);
		assert_eq!(
			fs::read(out.path().join("basegame/README.md")).unwrap(),
			b"vanilla notes\n"
		);
		assert!(!out.path().join("basegame/gfx/not-text.dds").exists());
		assert_eq!(
			fs::read_to_string(out.path().join("basegame/version.txt")).unwrap(),
			"1.37.5\n"
		);
		let manifest: serde_json::Value =
			serde_json::from_slice(&fs::read(out.path().join("basegame-manifest.json")).unwrap())
				.unwrap();
		assert_eq!(manifest["game_version"], "1.37.5");
		assert_eq!(manifest["selection"], "all-probable-text-files-v1");
		assert_eq!(manifest["file_count"], 5);
		assert_eq!(manifest["content_bytes"], 61);
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
