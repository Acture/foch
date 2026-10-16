use std::borrow::Cow;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::compile::CwtSchemaGraph;
use super::error::CwtLoadError;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CwtSchemaId([u8; 32]);

impl CwtSchemaId {
	pub fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}

	pub fn to_hex(&self) -> String {
		self.0.iter().map(|byte| format!("{byte:02x}")).collect()
	}

	pub fn from_hex(hex: &str) -> Option<Self> {
		let mut bytes = [0; 32];
		if hex.len() != bytes.len() * 2 {
			return None;
		}
		for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
			*byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
		}
		Some(Self(bytes))
	}
}

impl Display for CwtSchemaId {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(&self.to_hex())
	}
}

#[derive(Clone, Debug)]
pub struct SchemaPack {
	pub id: CwtSchemaId,
	pub graph: Arc<CwtSchemaGraph>,
}

impl SchemaPack {
	pub(crate) fn load_from_dir(root: &Path) -> Result<Self, CwtLoadError> {
		Self::load_from_dirs(&[root])
	}

	/// One schema compiled from several directories, each read after the one
	/// before it, so a later directory adds to the rules of an earlier one.
	pub(crate) fn load_from_dirs(roots: &[&Path]) -> Result<Self, CwtLoadError> {
		let id = cwt_schema_id_from_dirs(roots)?;
		let graph = Arc::new(CwtSchemaGraph::from_directories(roots)?);
		Ok(Self { id, graph })
	}

	pub(crate) fn load_from_dir_with_id(
		root: &Path,
		id: CwtSchemaId,
	) -> Result<Self, CwtLoadError> {
		let graph = Arc::new(CwtSchemaGraph::from_directory(root)?);
		Ok(Self { id, graph })
	}
}

/// Separates the files of one directory from the next in a schema id. No
/// file name is this long, so it cannot be read as a name's length prefix.
const SCHEMA_DIRECTORY_BOUNDARY: u64 = u64::MAX;

/// The identity of the schema below `root`. Compilation reads the files in
/// name order and lets a later file override an earlier one, so the id binds
/// every file's name below `root` and its content, each length-prefixed so
/// neither a moved file nor bytes shifted across a file boundary keep the old
/// id. Names are joined with `/` in their exact case; content has its line
/// endings normalized.
pub fn cwt_schema_id_from_dir(root: &Path) -> Result<CwtSchemaId, CwtLoadError> {
	cwt_schema_id_from_dirs(&[root])
}

/// The identity of a schema compiled from `roots` in order: the files of
/// each directory as [`cwt_schema_id_from_dir`] binds them, with a boundary
/// between directories. A single directory keeps its own id.
pub fn cwt_schema_id_from_dirs(roots: &[&Path]) -> Result<CwtSchemaId, CwtLoadError> {
	let mut hasher = Sha256::new();
	hasher.update(b"foch-cwt-schema-id/v2\n");
	for (index, root) in roots.iter().enumerate() {
		if index > 0 {
			hasher.update(SCHEMA_DIRECTORY_BOUNDARY.to_le_bytes());
		}
		for path in cwt_files(root)? {
			let bytes = std::fs::read(&path).map_err(|source| CwtLoadError::Io {
				path: path.clone(),
				source,
			})?;
			let name = schema_file_name(root, &path);
			let content = normalize_line_endings(&bytes);
			hasher.update((name.len() as u64).to_le_bytes());
			hasher.update(&name);
			hasher.update((content.len() as u64).to_le_bytes());
			hasher.update(&content);
		}
	}
	Ok(CwtSchemaId(hasher.finalize().into()))
}

/// A schema file's name below `root`, its components joined with `/`.
fn schema_file_name(root: &Path, path: &Path) -> Vec<u8> {
	let relative = path.strip_prefix(root).unwrap_or(path);
	let mut name = Vec::new();
	for (index, component) in relative.iter().enumerate() {
		if index > 0 {
			name.push(b'/');
		}
		name.extend_from_slice(component.as_encoded_bytes());
	}
	name
}

pub(crate) fn normalize_line_endings(bytes: &[u8]) -> Cow<'_, [u8]> {
	if !bytes.contains(&b'\r') {
		return Cow::Borrowed(bytes);
	}

	let mut normalized = Vec::with_capacity(bytes.len());
	let mut index = 0;
	while index < bytes.len() {
		if bytes[index] == b'\r' {
			normalized.push(b'\n');
			index += usize::from(bytes.get(index + 1) == Some(&b'\n'));
		} else {
			normalized.push(bytes[index]);
		}
		index += 1;
	}
	Cow::Owned(normalized)
}

/// Every `.cwt` file below `root`, in schema file order. Both the schema id
/// and compilation follow that order, and compilation lets a later file
/// override an earlier one. A directory that cannot be read fails the load.
pub(crate) fn cwt_files(root: &Path) -> Result<Vec<PathBuf>, CwtLoadError> {
	let mut files = Vec::new();
	for entry in WalkDir::new(root) {
		let entry = entry.map_err(|error| {
			let path = error
				.path()
				.map_or_else(|| root.to_path_buf(), Path::to_path_buf);
			CwtLoadError::Io {
				path,
				source: error.into(),
			}
		})?;
		if entry.file_type().is_file()
			&& entry.path().extension().and_then(|ext| ext.to_str()) == Some("cwt")
		{
			files.push(entry.into_path());
		}
	}
	if files.is_empty() {
		return Err(CwtLoadError::NoRuleFiles {
			root: root.to_path_buf(),
		});
	}
	files.sort_by_cached_key(|path| (schema_file_order_key(root, path), path.clone()));
	Ok(files)
}

/// Schema files are ordered by their names below `root`, ASCII case folded
/// and joined with `/`, compared bytewise, so `a.cwt` precedes `a/b.cwt` and
/// `B.cwt` sorts as `b.cwt`. Names that differ only in case fall back to the
/// order of their exact paths, so the order is total. The key is built from
/// the names' bytes; it orders files and never names one.
fn schema_file_order_key(root: &Path, path: &Path) -> Vec<u8> {
	let relative = path.strip_prefix(root).unwrap_or(path);
	let mut key = Vec::new();
	for (index, name) in relative.iter().enumerate() {
		if index > 0 {
			key.push(b'/');
		}
		key.extend(name.as_encoded_bytes().iter().map(u8::to_ascii_lowercase));
	}
	key
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::path::Path;

	use super::{CwtSchemaId, cwt_files, cwt_schema_id_from_dir, cwt_schema_id_from_dirs};
	use crate::game::schema::error::CwtLoadError;

	fn relative_names(root: &Path) -> Vec<String> {
		cwt_files(root)
			.expect("walk schema files")
			.iter()
			.map(|path| {
				let relative = path.strip_prefix(root).expect("below the root");
				relative
					.iter()
					.map(|name| name.to_str().expect("UTF-8 fixture name"))
					.collect::<Vec<_>>()
					.join("/")
			})
			.collect()
	}

	/// The order compilation and the schema id follow: names below the root,
	/// ASCII case folded, compared as `/`-joined text. A file sorts before a
	/// directory sharing its stem (`.` is below `/`), and case does not move
	/// a name.
	#[test]
	fn schema_files_are_ordered_by_their_case_folded_names_below_the_root() {
		let root = tempfile::tempdir().unwrap();
		for relative in ["c.cwt", "B.cwt", "a/b.cwt", "a.cwt", "a-b.cwt", "a/x.txt"] {
			let path = root.path().join(relative);
			fs::create_dir_all(path.parent().unwrap()).unwrap();
			fs::write(path, b"types = { }\n").unwrap();
		}

		assert_eq!(
			relative_names(root.path()),
			["a-b.cwt", "a.cwt", "a/b.cwt", "B.cwt", "c.cwt"]
		);
	}

	#[cfg(unix)]
	#[test]
	fn an_unreadable_schema_directory_fails_the_walk() {
		use std::os::unix::fs::PermissionsExt;

		let root = tempfile::tempdir().unwrap();
		fs::write(root.path().join("rules.cwt"), b"types = { }\n").unwrap();
		let locked = root.path().join("locked");
		fs::create_dir(&locked).unwrap();
		fs::write(locked.join("hidden.cwt"), b"types = { }\n").unwrap();
		fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

		let result = cwt_files(root.path());
		fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

		let error = result.expect_err("a schema directory that cannot be read");
		assert!(
			matches!(&error, CwtLoadError::Io { path, .. } if path == &locked),
			"{error}"
		);
	}
	#[test]
	fn schema_id_round_trips_through_hex() {
		let dir = tempfile::tempdir().unwrap();
		fs::write(dir.path().join("rules.cwt"), b"types = { }\n").unwrap();
		let id = cwt_schema_id_from_dir(dir.path()).unwrap();

		assert_eq!(CwtSchemaId::from_hex(&id.to_hex()), Some(id));
		assert_eq!(CwtSchemaId::from_hex("abc"), None);
		assert_eq!(CwtSchemaId::from_hex(&"zz".repeat(32)), None);
	}

	/// A file's name decides when it is compiled, and so which file overrides
	/// which: renaming or moving a file must change the id. So must moving
	/// bytes from one file into the next, which keeps their concatenation but
	/// changes which file overrides which.
	#[test]
	fn schema_id_binds_file_names_and_boundaries() {
		let id_of = |files: &[(&str, &str)]| {
			let root = tempfile::tempdir().unwrap();
			for (relative, content) in files {
				let path = root.path().join(relative);
				fs::create_dir_all(path.parent().unwrap()).unwrap();
				fs::write(path, content).unwrap();
			}
			cwt_schema_id_from_dir(root.path()).unwrap()
		};
		let types = "types = { type[x] = { } }\n";

		let in_common = id_of(&[("common/types.cwt", types)]);
		assert_eq!(in_common, id_of(&[("common/types.cwt", types)]));
		assert_ne!(in_common, id_of(&[("events/types.cwt", types)]));
		assert_ne!(
			id_of(&[("a.cwt", "types = { }\n"), ("b.cwt", "enums = { }\n")]),
			id_of(&[("a.cwt", "types = { }\nenums = { }\n"), ("b.cwt", "")])
		);
	}

	/// Each directory is compiled after the one before it, so which directory
	/// holds a file, and their order, are part of the id. A single directory
	/// keeps the id it has on its own.
	#[test]
	fn schema_id_binds_each_directory_and_their_order() {
		let directory = |files: &[(&str, &str)]| {
			let root = tempfile::tempdir().unwrap();
			for (relative, content) in files {
				fs::write(root.path().join(relative), content).unwrap();
			}
			root
		};
		let vendor = directory(&[("a.cwt", "types = { }\n"), ("b.cwt", "enums = { }\n")]);
		let first = directory(&[("a.cwt", "types = { }\n")]);
		let second = directory(&[("b.cwt", "enums = { }\n")]);
		let id = |roots: &[&Path]| cwt_schema_id_from_dirs(roots).unwrap();

		assert_eq!(
			id(&[vendor.path()]),
			cwt_schema_id_from_dir(vendor.path()).unwrap()
		);
		assert_ne!(id(&[first.path(), second.path()]), id(&[vendor.path()]));
		assert_ne!(
			id(&[first.path(), second.path()]),
			id(&[second.path(), first.path()])
		);
	}

	#[test]
	fn a_directory_without_rule_files_has_no_schema_id() {
		let empty = tempfile::tempdir().unwrap();
		fs::write(empty.path().join("README.md"), b"not a rule file\n").unwrap();

		assert!(matches!(
			cwt_schema_id_from_dir(empty.path()),
			Err(CwtLoadError::NoRuleFiles { .. })
		));
	}

	#[test]
	fn schema_pack_id_ignores_text_line_endings() {
		let lf = tempfile::tempdir().unwrap();
		let crlf = tempfile::tempdir().unwrap();
		fs::write(
			lf.path().join("rules.cwt"),
			b"types = {\n  event = { }\n}\n",
		)
		.unwrap();
		fs::write(
			crlf.path().join("rules.cwt"),
			b"types = {\r\n  event = { }\r\n}\r\n",
		)
		.unwrap();

		assert_eq!(
			cwt_schema_id_from_dir(lf.path()).unwrap(),
			cwt_schema_id_from_dir(crlf.path()).unwrap()
		);
	}
}
