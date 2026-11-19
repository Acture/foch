//! Subject-type `copy_from` graph loaded from an ordered playset.
//!
//! Roots are given game-first, then mods in ascending precedence: a later
//! root overlays earlier ones. A mod's `descriptor.mod` `replace_path` drops
//! every earlier file under that directory before the mod's own
//! `common/subject_types/*.txt` files are added. Within one root, files are
//! read in normalized path order. A later definition of the same key replaces
//! the earlier one, including a definition that omits `copy_from`.
//!
//! Init does not receive playset roots yet, so the loader is reached from
//! tests and from `ancestry` on the hot path.

#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::RwLock;

/// The graph the hot path reads. Empty until a playset is installed.
pub static ACTIVE: RwLock<SubjectTypeGraph> = RwLock::new(SubjectTypeGraph::EMPTY);

/// `copy_from` parents for subject type keys. Missing keys are unknown types;
/// a present key whose parent is `None` is a root (no `copy_from`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SubjectTypeGraph {
	parents: BTreeMap<String, Option<String>>,
}

impl SubjectTypeGraph {
	pub const EMPTY: Self = Self {
		parents: BTreeMap::new(),
	};

	/// Parse one script file. Later assignments of the same key replace earlier
	/// ones, which is how the vanilla file's forward declarations work.
	pub fn from_script(text: &str) -> Result<Self, String> {
		Ok(Self {
			parents: parse_subject_types(text.as_bytes())?,
		})
	}

	/// Load `common/subject_types` from an ordered playset.
	///
	/// `roots[0]` is the game directory. Each later root is a mod content
	/// directory (the folder that contains `descriptor.mod` and `common/`).
	pub fn load_playset(roots: &[impl AsRef<Path>]) -> Result<Self, String> {
		let mut files: BTreeMap<String, (usize, Vec<u8>)> = BTreeMap::new();
		for (layer, root) in roots.iter().enumerate() {
			let root = root.as_ref();
			let replacements = descriptor_replace_paths(root)?;
			files.retain(|path, _| !replacements.iter().any(|prefix| path_covered(path, prefix)));
			for (path, bytes) in read_subject_files(root)? {
				files.insert(path, (layer, bytes));
			}
		}

		let mut ordered: Vec<(usize, String, Vec<u8>)> = files
			.into_iter()
			.map(|(path, (layer, bytes))| (layer, path, bytes))
			.collect();
		ordered.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

		let mut parents = BTreeMap::new();
		for (_, path, bytes) in ordered {
			let definitions =
				parse_subject_types(&bytes).map_err(|error| format!("{path}: {error}"))?;
			parents.extend(definitions);
		}
		Ok(Self { parents })
	}

	pub fn defines(&self, key: &str) -> bool {
		self.parents.contains_key(key)
	}

	/// Defined subject-type keys, in sorted order.
	pub fn keys(&self) -> impl Iterator<Item = &str> {
		self.parents.keys().map(String::as_str)
	}

	pub fn parent(&self, key: &str) -> Option<&str> {
		self.parents.get(key).and_then(Option::as_deref)
	}

	/// `key`, then each `copy_from` parent. A repeated key ends the walk, so a
	/// cycle cannot grow the chain.
	pub fn ancestry(&self, key: &str) -> Vec<String> {
		let mut chain = Vec::new();
		let mut seen = HashSet::new();
		let mut current = key.to_string();
		loop {
			if !seen.insert(current.clone()) {
				break;
			}
			let next = self.parents.get(&current).cloned().flatten();
			chain.push(current);
			match next {
				Some(parent) => current = parent,
				None => break,
			}
		}
		chain
	}
}

fn path_covered(path: &str, prefix: &str) -> bool {
	match path.strip_prefix(prefix) {
		Some("") => true,
		Some(rest) => rest.starts_with('/'),
		None => false,
	}
}

fn descriptor_replace_paths(root: &Path) -> Result<Vec<String>, String> {
	let path = root.join("descriptor.mod");
	if !path.is_file() {
		return Ok(Vec::new());
	}
	let bytes =
		std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
	parse_replace_paths(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

fn read_subject_files(root: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
	let dir = root.join("common").join("subject_types");
	if !dir.exists() {
		return Ok(Vec::new());
	}
	if !dir.is_dir() {
		return Err(format!("{} is not a directory", dir.display()));
	}
	let mut files = Vec::new();
	walk_subject_files(&dir, "", &mut files)?;
	Ok(files)
}

fn walk_subject_files(
	dir: &Path,
	relative: &str,
	out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
	let entries =
		std::fs::read_dir(dir).map_err(|error| format!("read {}: {error}", dir.display()))?;
	for entry in entries {
		let entry = entry.map_err(|error| format!("read {}: {error}", dir.display()))?;
		let name = entry.file_name();
		let Some(name) = name.to_str() else {
			continue;
		};
		let child_rel = if relative.is_empty() {
			name.to_string()
		} else {
			format!("{relative}/{name}")
		};
		let file_type = entry
			.file_type()
			.map_err(|error| format!("stat {}: {error}", entry.path().display()))?;
		if file_type.is_dir() {
			walk_subject_files(&entry.path(), &child_rel, out)?;
		} else if file_type.is_file() && is_txt(name) {
			let bytes = std::fs::read(entry.path())
				.map_err(|error| format!("read {}: {error}", entry.path().display()))?;
			out.push((format!("common/subject_types/{child_rel}"), bytes));
		}
	}
	Ok(())
}

fn is_txt(name: &str) -> bool {
	name.len() >= 4 && name.as_bytes()[name.len() - 4..].eq_ignore_ascii_case(b".txt")
}

struct Scan<'a> {
	bytes: &'a [u8],
	index: usize,
}

impl<'a> Scan<'a> {
	fn skip(&mut self) {
		loop {
			while self.index < self.bytes.len() && self.bytes[self.index].is_ascii_whitespace() {
				self.index += 1;
			}
			if self.index < self.bytes.len() && self.bytes[self.index] == b'#' {
				while self.index < self.bytes.len() && self.bytes[self.index] != b'\n' {
					self.index += 1;
				}
				continue;
			}
			break;
		}
	}

	fn peek(&mut self) -> Option<u8> {
		self.skip();
		self.bytes.get(self.index).copied()
	}

	fn ident(&mut self) -> Option<String> {
		self.skip();
		let start = self.index;
		let first = *self.bytes.get(start)?;
		if !first.is_ascii_alphabetic() && first != b'_' {
			return None;
		}
		self.index += 1;
		while self.index < self.bytes.len() {
			let byte = self.bytes[self.index];
			if byte.is_ascii_alphanumeric() || byte == b'_' {
				self.index += 1;
			} else {
				break;
			}
		}
		Some(String::from_utf8_lossy(&self.bytes[start..self.index]).into_owned())
	}

	fn string(&mut self) -> Option<String> {
		self.skip();
		if self.bytes.get(self.index) != Some(&b'"') {
			return None;
		}
		self.index += 1;
		let mut out = String::new();
		while self.index < self.bytes.len() {
			let byte = self.bytes[self.index];
			self.index += 1;
			match byte {
				b'"' => return Some(out),
				b'\\' => {
					if let Some(escaped) = self.bytes.get(self.index).copied() {
						self.index += 1;
						out.push(escaped as char);
					}
				}
				_ => out.push(byte as char),
			}
		}
		None
	}

	fn bare(&mut self) -> Option<String> {
		self.skip();
		let start = self.index;
		let byte = *self.bytes.get(start)?;
		if matches!(byte, b'{' | b'}' | b'"' | b'#' | b'=') {
			return None;
		}
		self.index += 1;
		while self.index < self.bytes.len() {
			let byte = self.bytes[self.index];
			if byte.is_ascii_whitespace() || matches!(byte, b'{' | b'}' | b'#' | b'=') {
				break;
			}
			self.index += 1;
		}
		Some(String::from_utf8_lossy(&self.bytes[start..self.index]).into_owned())
	}
}

fn parse_subject_types(bytes: &[u8]) -> Result<BTreeMap<String, Option<String>>, String> {
	let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
	let mut scan = Scan { bytes, index: 0 };
	let mut parents = BTreeMap::new();
	loop {
		if scan.peek().is_none() {
			break;
		}
		let mark = scan.index;
		let Some(key) = scan.ident() else {
			return Err(format!("expected a subject type name at byte {mark}"));
		};
		if scan.peek() != Some(b'=') {
			return Err(format!("expected '=' after {key}"));
		}
		scan.index += 1;
		match scan.peek() {
			Some(b'{') => {
				scan.index += 1;
				let parent = parse_type_block(&mut scan)?;
				parents.insert(key, parent);
			}
			Some(b'"') => {
				scan.string()
					.ok_or_else(|| format!("unterminated string after {key}"))?;
			}
			Some(_) => {
				scan.bare()
					.ok_or_else(|| format!("missing value after {key}"))?;
			}
			None => return Err(format!("missing value after {key}")),
		}
	}
	Ok(parents)
}

/// Fields directly inside one subject type. `copy_from` nested in a child
/// block is not a parent pointer.
fn parse_type_block(scan: &mut Scan<'_>) -> Result<Option<String>, String> {
	let mut parent = None;
	loop {
		if scan.peek().is_none() {
			return Err("unclosed subject type block".into());
		}
		if scan.bytes[scan.index] == b'}' {
			scan.index += 1;
			return Ok(parent);
		}
		let mark = scan.index;
		let Some(key) = scan.ident() else {
			if scan.peek() == Some(b'{') {
				scan.index += 1;
				skip_block(scan)?;
				continue;
			}
			if scan.peek() == Some(b'"') {
				scan.string()
					.ok_or_else(|| "unterminated string in subject type".to_string())?;
				continue;
			}
			if scan.bare().is_some() {
				continue;
			}
			return Err(format!("expected a field name at byte {mark}"));
		};
		if scan.peek() != Some(b'=') {
			continue;
		}
		scan.index += 1;
		match scan.peek() {
			Some(b'{') => {
				scan.index += 1;
				skip_block(scan)?;
			}
			Some(b'"') => {
				let value = scan
					.string()
					.ok_or_else(|| format!("unterminated string after {key}"))?;
				if key == "copy_from" {
					parent = Some(value);
				}
			}
			Some(_) => {
				let value = scan
					.bare()
					.ok_or_else(|| format!("missing value after {key}"))?;
				if key == "copy_from" {
					parent = Some(value);
				}
			}
			None => return Err(format!("missing value after {key}")),
		}
	}
}

fn skip_block(scan: &mut Scan<'_>) -> Result<(), String> {
	let mut depth = 1;
	while scan.index < scan.bytes.len() {
		let byte = scan.bytes[scan.index];
		if byte == b'"' {
			scan.index += 1;
			while scan.index < scan.bytes.len() {
				let inner = scan.bytes[scan.index];
				scan.index += 1;
				if inner == b'\\' {
					if scan.index < scan.bytes.len() {
						scan.index += 1;
					}
				} else if inner == b'"' {
					break;
				}
			}
			continue;
		}
		if byte == b'#' {
			while scan.index < scan.bytes.len() && scan.bytes[scan.index] != b'\n' {
				scan.index += 1;
			}
			continue;
		}
		scan.index += 1;
		match byte {
			b'{' => depth += 1,
			b'}' => {
				depth -= 1;
				if depth == 0 {
					return Ok(());
				}
			}
			_ => {}
		}
	}
	Err("unclosed block".into())
}

fn parse_replace_paths(bytes: &[u8]) -> Result<Vec<String>, String> {
	let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
	let mut scan = Scan { bytes, index: 0 };
	let mut paths = Vec::new();
	loop {
		if scan.peek().is_none() {
			break;
		}
		let mark = scan.index;
		let Some(key) = scan.ident() else {
			if scan.peek() == Some(b'{') {
				scan.index += 1;
				skip_block(&mut scan)?;
				continue;
			}
			if scan.bare().is_some() || scan.string().is_some() {
				continue;
			}
			return Err(format!("descriptor syntax at byte {mark}"));
		};
		if scan.peek() != Some(b'=') {
			continue;
		}
		scan.index += 1;
		let value = read_descriptor_value(&mut scan)?;
		if key == "replace_path" {
			paths.push(normalize_replace_path(&value)?);
		}
	}
	Ok(paths)
}

fn read_descriptor_value(scan: &mut Scan<'_>) -> Result<String, String> {
	match scan.peek() {
		Some(b'{') => {
			scan.index += 1;
			skip_block(scan)?;
			Ok(String::new())
		}
		Some(b'"') => scan
			.string()
			.ok_or_else(|| "unterminated descriptor string".to_string()),
		Some(_) => scan
			.bare()
			.ok_or_else(|| "missing descriptor value".to_string()),
		None => Err("missing descriptor value".into()),
	}
}

/// The directory a `replace_path` names under the game or mod root.
///
/// `/` and `\` separate components. A leading separator or a `.` component
/// adds nothing. `..`, a drive or device prefix, and an empty path are rejected
/// so a bad descriptor cannot wipe an unrelated directory.
fn normalize_replace_path(raw: &str) -> Result<String, String> {
	if raw.contains('\0') {
		return Err(format!("replace_path {raw:?} contains NUL"));
	}
	let mut parts = Vec::new();
	for component in raw.split(['/', '\\']) {
		if component.is_empty() || component == "." {
			continue;
		}
		if component == ".." {
			return Err(format!("replace_path {raw:?} contains .."));
		}
		if component.contains(':') {
			return Err(format!(
				"replace_path {raw:?} is not a directory under the game root"
			));
		}
		parts.push(component);
	}
	if parts.is_empty() {
		return Err(format!("replace_path {raw:?} is empty"));
	}
	Ok(parts.join("/"))
}

#[cfg(test)]
fn write_rel(root: &Path, relative: &str, body: &str) {
	let path = root.join(relative);
	std::fs::create_dir_all(path.parent().unwrap()).unwrap();
	std::fs::write(&path, body).unwrap();
}

#[cfg(test)]
mod tests {
	use super::*;

	const VANILLA_SHAPED: &str = r#"
# copy_from in a comment must not become a parent. "quotes { braces }" too.
vassal = {}
eyalet = {}
core_eyalet = {}

vassal = {
	copy_from = default
	pays_overlord = 1.0
	liberty_desire_same_dynasty = -5.0
	is_potential_overlord = { always = no } # copy_from = tributary_state
	modifier = {
		copy_from = march
		nested = { has_dlc = "Mandate of Heaven" }
	}
}
eyalet = {
	copy_from = vassal
}
core_eyalet = {
	copy_from = eyalet
	sprite = GFX_icon_core_eyalet
}
default = {
}
"#;

	#[test]
	fn forward_declarations_keep_the_later_copy_from() {
		let graph = SubjectTypeGraph::from_script(VANILLA_SHAPED).unwrap();
		assert_eq!(graph.parent("core_eyalet"), Some("eyalet"));
		assert_eq!(graph.parent("eyalet"), Some("vassal"));
		assert_eq!(graph.parent("vassal"), Some("default"));
		assert_eq!(graph.parent("default"), None);
		assert_eq!(
			graph.ancestry("core_eyalet"),
			vec!["core_eyalet", "eyalet", "vassal", "default"]
		);
	}

	#[test]
	fn a_later_definition_can_clear_copy_from() {
		let graph =
			SubjectTypeGraph::from_script("vassal = { copy_from = default }\nvassal = {}\n")
				.unwrap();
		assert!(graph.defines("vassal"));
		assert_eq!(graph.parent("vassal"), None);
	}

	#[test]
	fn quoted_copy_from_and_bom_are_read() {
		let graph =
			SubjectTypeGraph::from_script("\u{feff}custom = { copy_from = \"eyalet\" }\n").unwrap();
		assert_eq!(graph.parent("custom"), Some("eyalet"));
	}

	#[test]
	fn cycles_stop_at_the_repeated_key() {
		let graph = SubjectTypeGraph::from_script(
			"a = { copy_from = b }\nb = { copy_from = c }\nc = { copy_from = a }\nlooped = { copy_from = looped }\n",
		)
		.unwrap();
		assert_eq!(graph.ancestry("a"), vec!["a", "b", "c"]);
		assert_eq!(graph.ancestry("looped"), vec!["looped"]);
	}

	#[test]
	fn playset_chains_core_eyalet_through_vassal_and_a_mod_variant() {
		let game = tempfile::tempdir().unwrap();
		let reform = tempfile::tempdir().unwrap();
		write_rel(
			game.path(),
			"common/subject_types/00_subject_types.txt",
			VANILLA_SHAPED,
		);
		write_rel(
			reform.path(),
			"descriptor.mod",
			"name=\"Anatolia\"\nsupported_version=\"1.37\"\n",
		);
		// Filename sorts before the game file. Layer order still lets the mod win.
		write_rel(
			reform.path(),
			"common/subject_types/00_early.txt",
			r#"
beylerbey = {
	modifier = { copy_from = tributary_state }
	copy_from = core_eyalet
}
"#,
		);
		let graph = SubjectTypeGraph::load_playset(&[game.path(), reform.path()]).unwrap();
		assert_eq!(
			graph.ancestry("beylerbey"),
			vec!["beylerbey", "core_eyalet", "eyalet", "vassal", "default"]
		);
		assert_eq!(graph.parent("eyalet"), Some("vassal"));
		assert!(graph.defines("vassal"));
	}

	#[test]
	fn replace_path_drops_earlier_subject_types_and_keeps_the_mod() {
		let game = tempfile::tempdir().unwrap();
		let earlier = tempfile::tempdir().unwrap();
		let reset = tempfile::tempdir().unwrap();
		write_rel(
			game.path(),
			"common/subject_types/00_subject_types.txt",
			VANILLA_SHAPED,
		);
		write_rel(
			earlier.path(),
			"common/subject_types/01_extra.txt",
			"extra = { copy_from = vassal }\n",
		);
		write_rel(
			reset.path(),
			"descriptor.mod",
			"name=\"Reset\"\nreplace_path=\"common/subject_types\"\nreplace_path=\"common\\\\ideas\"\n",
		);
		write_rel(
			reset.path(),
			"common/subject_types/00_subject_types.txt",
			"default = {}\ncustom_vassal = { copy_from = default }\n",
		);
		// A sibling directory is not covered by the subject-type replace.
		write_rel(
			game.path(),
			"common/subject_types_extra/00_subject_types.txt",
			"should_not_load = { copy_from = vassal }\n",
		);
		let graph =
			SubjectTypeGraph::load_playset(&[game.path(), earlier.path(), reset.path()]).unwrap();
		assert!(!graph.defines("core_eyalet"));
		assert!(!graph.defines("eyalet"));
		assert!(!graph.defines("vassal"));
		assert!(!graph.defines("extra"));
		assert!(!graph.defines("should_not_load"));
		assert_eq!(graph.parent("custom_vassal"), Some("default"));
	}

	#[test]
	fn same_path_replacement_drops_keys_the_new_file_omits() {
		let game = tempfile::tempdir().unwrap();
		let overlay = tempfile::tempdir().unwrap();
		write_rel(
			game.path(),
			"common/subject_types/00_subject_types.txt",
			"vassal = { copy_from = default }\neyalet = { copy_from = vassal }\n",
		);
		write_rel(
			overlay.path(),
			"common/subject_types/00_subject_types.txt",
			"vassal = { copy_from = tributary_state }\n",
		);
		let graph = SubjectTypeGraph::load_playset(&[game.path(), overlay.path()]).unwrap();
		assert_eq!(graph.parent("vassal"), Some("tributary_state"));
		assert!(!graph.defines("eyalet"));
	}

	#[test]
	fn within_one_root_the_later_path_wins() {
		let game = tempfile::tempdir().unwrap();
		write_rel(
			game.path(),
			"common/subject_types/00_a.txt",
			"shared = { copy_from = default }\n",
		);
		write_rel(
			game.path(),
			"common/subject_types/01_b.txt",
			"shared = { copy_from = vassal }\n",
		);
		let graph = SubjectTypeGraph::load_playset(&[game.path()]).unwrap();
		assert_eq!(graph.parent("shared"), Some("vassal"));
	}

	#[test]
	fn a_replace_path_outside_subject_types_keeps_the_chain() {
		let game = tempfile::tempdir().unwrap();
		let ideas = tempfile::tempdir().unwrap();
		write_rel(
			game.path(),
			"common/subject_types/00_subject_types.txt",
			VANILLA_SHAPED,
		);
		write_rel(
			ideas.path(),
			"descriptor.mod",
			"replace_path=\"common/ideas\"\n",
		);
		let graph = SubjectTypeGraph::load_playset(&[game.path(), ideas.path()]).unwrap();
		assert_eq!(graph.parent("core_eyalet"), Some("eyalet"));
	}

	#[test]
	fn an_escaping_replace_path_is_rejected() {
		let game = tempfile::tempdir().unwrap();
		let bad = tempfile::tempdir().unwrap();
		write_rel(
			game.path(),
			"common/subject_types/00_subject_types.txt",
			"vassal = {}\n",
		);
		write_rel(
			bad.path(),
			"descriptor.mod",
			"replace_path=\"common/../subject_types\"\n",
		);
		let error = SubjectTypeGraph::load_playset(&[game.path(), bad.path()]).unwrap_err();
		assert!(error.contains(".."), "{error}");
	}

	#[test]
	fn parses_vanilla_subject_types_when_eu4_root_is_set() {
		let Ok(root) = std::env::var("EU4_ROOT") else {
			return;
		};
		let path = Path::new(&root).join("common/subject_types/00_subject_types.txt");
		let bytes =
			std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
		let graph = SubjectTypeGraph {
			parents: parse_subject_types(&bytes).unwrap_or_else(|error| panic!("{error}")),
		};
		assert_eq!(graph.parent("core_eyalet"), Some("eyalet"));
		assert_eq!(graph.parent("eyalet"), Some("default"));
		assert_eq!(graph.parent("vassal"), Some("default"));
		assert_eq!(graph.parent("march"), Some("vassal"));
	}

	#[test]
	fn replace_path_coverage_is_a_whole_component_prefix() {
		assert!(path_covered(
			"common/subject_types/00_subject_types.txt",
			"common/subject_types"
		));
		assert!(path_covered("common/subject_types/nested/a.txt", "common"));
		assert!(!path_covered(
			"common/subject_types_extra/a.txt",
			"common/subject_types"
		));
	}
}
