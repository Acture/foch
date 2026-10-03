use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::model::MERGE_PROVENANCE_ARTIFACT_PATH;

const RELATIVE: &str = "common/scripted_effects/test.txt";
const TEXT: &str = "shared = { add_prestige = 1 }\n";

fn fixture(relative: &str, text: &str, sources: &[&str]) -> (TempDir, PathBuf) {
	let root = TempDir::new().unwrap();
	let path = root.path().join(relative);
	fs::create_dir_all(path.parent().unwrap()).unwrap();
	fs::write(&path, text).unwrap();
	write_sidecar(root.path(), json!({relative: {"shared": sources}}));
	(root, path)
}

fn write_sidecar(root: &Path, value: serde_json::Value) {
	let path = root.join(MERGE_PROVENANCE_ARTIFACT_PATH);
	fs::create_dir_all(path.parent().unwrap()).unwrap();
	fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn write_verified_sidecar(root: &Path, bytes: &[u8]) {
	write_sidecar(
		root,
		json!({
			"version": 1,
			"files": {RELATIVE: {
				"content_hash": blake3::hash(bytes).to_hex().as_str(),
				"definitions": {"shared": ["940", "320"]}
			}},
			"mod_names": {"940": "模组 Z", "320": "Mod A"}
		}),
	);
}

#[test]
fn provenance_hover_verifies_emitted_bytes_and_displays_names_in_source_order() {
	let (root, path) = fixture(RELATIVE, TEXT, &["940"]);
	write_verified_sidecar(root.path(), TEXT.as_bytes());
	let result = hover(&path, TEXT).unwrap();
	assert!(
		result
			.markdown
			.contains("Merged from 模组 Z (940), Mod A (320)")
	);
	assert!(!result.markdown.contains("not verified"));
	// A same-length saved edit must fail even if the original timestamp survives.
	let timestamp = filetime::FileTime::from_last_modification_time(&fs::metadata(&path).unwrap());
	let changed = TEXT.replace("= 1", "= 2");
	fs::write(&path, &changed).unwrap();
	filetime::set_file_mtime(&path, timestamp).unwrap();
	assert!(hover(&path, &changed).is_none());
	assert!(hover(&path, TEXT).is_none());
	write_verified_sidecar(root.path(), changed.as_bytes());
	assert!(hover(&path, &changed).is_some());
}

#[test]
fn provenance_hover_verification_uses_bytes_before_script_decoding() {
	let text = "shared = { name = \"中文\" }\r\n";
	let (root, path) = fixture(RELATIVE, text, &["940"]);
	let bytes = [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat();
	fs::write(&path, &bytes).unwrap();
	write_verified_sidecar(root.path(), &bytes);
	assert!(hover(&path, text).unwrap().markdown.contains("Merged from"));
	write_verified_sidecar(root.path(), text.as_bytes());
	assert!(
		hover(&path, text).is_none(),
		"decoded equality is not byte identity"
	);
}

#[cfg(unix)]
#[test]
fn provenance_hover_does_not_alias_a_literal_backslash_filename() {
	let nested: &str = "common/scripted_effects/a/b.txt";
	let (root, nested_path): (TempDir, PathBuf) = fixture(nested, TEXT, &["nested"]);
	let literal_path: PathBuf = root.path().join(r"common/scripted_effects/a\b.txt");
	fs::write(&literal_path, TEXT).unwrap();
	write_sidecar(
		root.path(),
		json!({
			"version": 1,
			"files": {nested: {
				"content_hash": blake3::hash(TEXT.as_bytes()).to_hex().as_str(),
				"definitions": {"shared": ["nested"]}
			}},
			"mod_names": {}
		}),
	);
	assert!(
		hover(&nested_path, TEXT)
			.unwrap()
			.markdown
			.contains("nested")
	);
	assert!(hover(&literal_path, TEXT).is_none());
}

#[test]
fn provenance_hover_rejects_unknown_or_incomplete_verified_records() {
	let (root, path) = fixture(RELATIVE, TEXT, &["940"]);
	for value in [
		json!({"version": 2, "files": {}, "mod_names": {}}),
		json!({"version": 1, "files": {RELATIVE: {"definitions": {"shared": ["940"]}}}, "mod_names": {}}),
		json!({"version": 1, "files": {}, "mod_names": {}}),
	] {
		write_sidecar(root.path(), value);
		assert!(hover(&path, TEXT).is_none());
	}
}

fn hover(path: &Path, text: &str) -> Option<SchemaHover> {
	document_hover(path, None, text, EditorPosition::default(), None, None)
}

#[test]
fn provenance_hover_preserves_single_and_multiple_source_order() {
	for sources in [vec!["940"], vec!["940", "320"]] {
		let (_root, path) = fixture(RELATIVE, TEXT, &sources);
		let result = hover(&path, TEXT).expect("recorded merge sources");
		assert!(result.markdown.contains("Recorded merge sources"));
		assert!(result.markdown.contains(&sources.join(", ")));
		assert!(result.markdown.contains("last merge"));
		assert_eq!(result.range.start, EditorPosition::default());
		assert_eq!(
			result.range.end,
			EditorPosition {
				line: 0,
				character: 6
			}
		);
	}
}

#[test]
fn provenance_hover_does_not_credit_dirty_or_ambiguous_definitions() {
	let (_root, path) = fixture(RELATIVE, TEXT, &["mod_a"]);
	assert!(hover(&path, "shared = { add_prestige = 2 }\n").is_none());
	for (relative, text) in [
		(RELATIVE, "shared = {}\nshared = {}\n"),
		("events/test.txt", "shared = { id = example.1 }\n"),
		("decisions/test.txt", "shared = { decision = {} }\n"),
		("unknown/test.txt", TEXT),
		(RELATIVE, "shared = {"),
		(RELATIVE, "shared = { nested = {}"),
	] {
		let (_root, path) = fixture(relative, text, &["mod_a"]);
		assert!(
			hover(&path, text).is_none(),
			"must abstain: {relative} {text}"
		);
	}
}

#[test]
fn provenance_hover_is_scoped_to_output_root_and_exact_file() {
	let (first, first_path) = fixture(RELATIVE, TEXT, &["940"]);
	let (_second, second_path) = fixture(RELATIVE, TEXT, &["320"]);
	let other_relative = "common/scripted_effects/other.txt";
	let other_path = first.path().join(other_relative);
	fs::write(&other_path, TEXT).unwrap();
	write_sidecar(
		first.path(),
		json!({
			RELATIVE: {"shared": ["940"]},
			other_relative: {"shared": ["710"]},
		}),
	);
	for (path, expected) in [
		(first_path, "940"),
		(second_path, "320"),
		(other_path, "710"),
	] {
		let result = hover(&path, TEXT).unwrap();
		assert!(result.markdown.contains(&format!("Mod IDs: {expected}")));
	}
}

#[test]
fn provenance_hover_never_falls_through_an_inner_mod_boundary() {
	let outer = TempDir::new().unwrap();
	let inner = outer.path().join("common/scripted_effects/inner");
	let path = inner.join(RELATIVE);
	fs::create_dir_all(path.parent().unwrap()).unwrap();
	fs::write(&path, TEXT).unwrap();
	let outer_key = format!("common/scripted_effects/inner/{RELATIVE}");
	write_sidecar(outer.path(), json!({outer_key: {"shared": ["wrong"]}}));
	fs::write(inner.join("descriptor.mod"), "name = inner").unwrap();
	assert!(hover(&path, TEXT).is_none());
	write_sidecar(&inner, json!({RELATIVE: {"shared": ["correct"]}}));
	assert!(hover(&path, TEXT).unwrap().markdown.contains("correct"));
	fs::remove_file(inner.join("descriptor.mod")).unwrap();
	fs::write(inner.join(MERGE_PROVENANCE_ARTIFACT_PATH), "broken").unwrap();
	assert!(hover(&path, TEXT).is_none());
	fs::remove_file(inner.join(MERGE_PROVENANCE_ARTIFACT_PATH)).unwrap();
	assert!(hover(&path, TEXT).is_none());
}

#[test]
fn provenance_hover_refreshes_sources_and_rejects_observably_stale_scripts() {
	use filetime::{FileTime, set_file_mtime};
	let (root, path) = fixture(RELATIVE, TEXT, &["940"]);
	assert!(hover(&path, TEXT).unwrap().markdown.contains("940"));
	write_sidecar(root.path(), json!({RELATIVE: {"shared": ["320"]}}));
	let result = hover(&path, TEXT).unwrap();
	assert!(result.markdown.contains("320"));
	assert!(!result.markdown.contains("940"));
	let sidecar = root.path().join(MERGE_PROVENANCE_ARTIFACT_PATH);
	set_file_mtime(&sidecar, FileTime::from_unix_time(100, 0)).unwrap();
	set_file_mtime(&path, FileTime::from_unix_time(101, 0)).unwrap();
	assert!(hover(&path, TEXT).is_none());
	set_file_mtime(sidecar, FileTime::from_unix_time(102, 0)).unwrap();
	assert!(hover(&path, TEXT).is_some());
}

#[test]
fn provenance_hover_uses_utf16_ranges_and_does_not_credit_nested_keys() {
	let text = "# 合并😀\r\nother = { name = \"中文😀\" } shared = { nested = yes }\r\n";
	let (_root, path) = fixture(RELATIVE, text, &["940"]);
	let start = super::super::position::position_at(text, text.find("shared").unwrap()).unwrap();
	let query = |position| document_hover(&path, None, text, position, None, None);
	let result = query(start).unwrap();
	assert_eq!(result.range.start, start);
	assert_eq!(result.range.end.character, start.character + 6);
	assert!(query(result.range.end).is_none());
	let nested = super::super::position::position_at(text, text.find("nested").unwrap()).unwrap();
	assert!(query(nested).is_none());
}

#[test]
fn provenance_hover_escapes_ids_and_leaves_files_unchanged() {
	let (root, path) = fixture(
		RELATIVE,
		TEXT,
		&["[name](https://example.test)", "<b>mod</b>"],
	);
	let sidecar = root.path().join(MERGE_PROVENANCE_ARTIFACT_PATH);
	let before = fs::read(&sidecar).unwrap();
	let result = hover(&path, TEXT).unwrap();
	assert!(
		result
			.markdown
			.contains(r"\[name\]\(https\:\/\/example\.test\)")
	);
	assert!(result.markdown.contains(r"\<b\>mod\<\/b\>"));
	assert_eq!(fs::read(&path).unwrap(), TEXT.as_bytes());
	assert_eq!(fs::read(sidecar).unwrap(), before);
}

#[test]
fn provenance_hover_recovers_schema_path_from_the_output_root() {
	let (_root, path) = fixture(RELATIVE, TEXT, &["940"]);
	let schema_dir = TempDir::new().unwrap();
	fs::write(
		schema_dir.path().join("effects.cwt"),
		r#"
		types = { type[effect] = { path = "game/common/scripted_effects" } }
		effect = { shared = { add_prestige = scalar } }
	"#,
	)
	.unwrap();
	let schema = EditorSchema::load_from_directory_with_cache(schema_dir.path(), None).unwrap();
	let result = document_hover(
		&path,
		Some(GamePath::new("wrong/root/test.txt").unwrap()),
		TEXT,
		EditorPosition::default(),
		Some(&schema),
		None,
	)
	.unwrap();
	assert!(result.markdown.contains("Type: `Block`"));
	assert!(result.markdown.contains("940"));
}

#[test]
fn provenance_hover_keeps_schema_on_hits_and_broken_sidecars() {
	let (root, path) = fixture(RELATIVE, TEXT, &["mod_a"]);
	let schema_dir = TempDir::new().unwrap();
	fs::write(
		schema_dir.path().join("effects.cwt"),
		r#"
		types = { type[effect] = { path = "game/common/scripted_effects" } }
		effect = { shared = { add_prestige = scalar } }
	"#,
	)
	.unwrap();
	let schema = EditorSchema::load_from_directory_with_cache(schema_dir.path(), None).unwrap();
	let lookup = || {
		document_hover(
			&path,
			Some(GamePath::new(RELATIVE).unwrap()),
			TEXT,
			EditorPosition::default(),
			Some(&schema),
			None,
		)
		.unwrap()
	};
	let expected = schema
		.hover(
			GamePath::new(RELATIVE).unwrap(),
			TEXT,
			EditorPosition::default(),
			None,
		)
		.unwrap();
	let result = lookup();
	assert!(result.markdown.contains(&expected.markdown));
	assert!(result.markdown.contains("mod\\_a"));
	let sidecar = root.path().join(MERGE_PROVENANCE_ARTIFACT_PATH);
	for content in [
		"{broken",
		"{}",
		r#"{"common/scripted_effects/test.txt":{"shared":[]}}"#,
	] {
		fs::write(&sidecar, content).unwrap();
		assert_eq!(lookup(), expected);
	}
	fs::remove_file(sidecar).unwrap();
	assert_eq!(lookup(), expected);
}

#[test]
fn provenance_hover_keeps_caller_schema_beneath_unrelated_metadata() {
	let (outer, _) = fixture(RELATIVE, TEXT, &["outer"]);
	let path = outer.path().join("childmod").join(RELATIVE);
	fs::create_dir_all(path.parent().unwrap()).unwrap();
	fs::write(&path, TEXT).unwrap();
	let schema_dir = TempDir::new().unwrap();
	fs::write(
		schema_dir.path().join("effects.cwt"),
		r#"types = { type[effect] = { path = "game/common/scripted_effects" } }
		effect = { shared = { add_prestige = scalar } }"#,
	)
	.unwrap();
	let schema = EditorSchema::load_from_directory_with_cache(schema_dir.path(), None).unwrap();
	let expected = schema.hover(
		GamePath::new(RELATIVE).unwrap(),
		TEXT,
		EditorPosition::default(),
		None,
	);
	assert!(expected.is_some());
	assert_eq!(
		document_hover(
			&path,
			Some(GamePath::new(RELATIVE).unwrap()),
			TEXT,
			EditorPosition::default(),
			Some(&schema),
			None
		),
		expected,
	);
}

#[cfg(any(unix, windows))]
#[test]
fn provenance_hover_rejects_links_that_change_output_or_relative_identity() {
	for verified in [false, true] {
		let (root, path) = fixture(RELATIVE, TEXT, &["target_only"]);
		if verified {
			write_verified_sidecar(root.path(), TEXT.as_bytes());
		}
		assert!(hover(&path, TEXT).is_some());
		let other = TempDir::new().unwrap();
		fs::create_dir_all(other.path().join(".foch")).unwrap();
		for alias in [other.path().join("common"), root.path().join("alias")] {
			let link = DirectoryLink::new(&root.path().join("common"), &alias);
			assert!(hover(&alias.join("scripted_effects/test.txt"), TEXT).is_none());
			drop(link);
		}
	}
}

#[cfg(any(unix, windows))]
struct DirectoryLink(PathBuf);

#[cfg(any(unix, windows))]
impl DirectoryLink {
	fn new(target: &Path, link: &Path) -> Self {
		#[cfg(unix)]
		std::os::unix::fs::symlink(target, link).unwrap();
		#[cfg(windows)]
		{
			// Junctions do not require Windows developer mode or symlink privileges.
			let output = std::process::Command::new("powershell.exe")
				.args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference = 'Stop'; New-Item -ItemType Junction -Path $env:FOCH_TEST_LINK -Value $env:FOCH_TEST_TARGET | Out-Null"])
				.env("FOCH_TEST_LINK", link)
				.env("FOCH_TEST_TARGET", target)
				.output()
				.unwrap();
			assert!(
				output.status.success(),
				"{}",
				String::from_utf8_lossy(&output.stderr)
			);
		}
		Self(link.to_path_buf())
	}
}

#[cfg(any(unix, windows))]
impl Drop for DirectoryLink {
	fn drop(&mut self) {
		#[cfg(unix)]
		fs::remove_file(&self.0).unwrap();
		#[cfg(windows)]
		fs::remove_dir(&self.0).unwrap();
	}
}
