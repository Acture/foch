use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

const OBJECT_MARKER: &str = ".foch-object.json";
const HASH_FORMAT: &str = "foch-tree-v1";

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct TreeStats {
	pub files: u64,
	pub directories: u64,
	pub symlinks: u64,
	pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct TreeDigest {
	pub hash: String,
	pub stats: TreeStats,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct StoredObject {
	pub hash: String,
	pub tree: PathBuf,
	pub stats: TreeStats,
	pub newly_stored: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportProfile {
	Semantic,
	Full,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportedArchive {
	pub path: PathBuf,
	pub hash: String,
	pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct ObjectStore {
	root: PathBuf,
	work: PathBuf,
}

impl ObjectStore {
	pub fn new(root: impl Into<PathBuf>, work: impl Into<PathBuf>) -> Self {
		Self {
			root: root.into(),
			work: work.into(),
		}
	}

	pub fn object_dir(&self, hash: &str) -> io::Result<PathBuf> {
		validate_hash(hash)?;
		Ok(self.root.join(&hash[..2]).join(hash))
	}

	pub fn tree_path(&self, hash: &str) -> io::Result<PathBuf> {
		Ok(self.object_dir(hash)?.join("tree"))
	}

	pub fn verify_object(&self, hash: &str) -> io::Result<StoredObject> {
		let opened = self.open_object(hash)?;
		let actual = digest_tree(&opened.tree)?;
		if actual.hash != hash || actual.stats != opened.stats {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!("stored object {hash} is corrupt"),
			));
		}
		Ok(opened)
	}

	/// Open a previously verified object without re-hashing its payload. Callers
	/// must establish a run-level verification boundary before using this fast path.
	pub fn open_object(&self, hash: &str) -> io::Result<StoredObject> {
		let object_dir = self.object_dir(hash)?;
		let expected = read_marker(&object_dir)?;
		if expected.hash != hash {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!("object marker hash does not match path {hash}"),
			));
		}
		let tree = object_dir.join("tree");
		if !tree.is_dir() {
			return Err(io::Error::new(
				ErrorKind::NotFound,
				format!("object tree is missing: {}", tree.display()),
			));
		}
		Ok(StoredObject {
			hash: expected.hash,
			tree,
			stats: expected.stats,
			newly_stored: false,
		})
	}

	pub fn snapshot_tree(&self, source: &Path) -> io::Result<StoredObject> {
		if !source.is_dir() {
			return Err(io::Error::new(
				ErrorKind::InvalidInput,
				format!("snapshot source is not a directory: {}", source.display()),
			));
		}
		let source_digest = digest_tree(source)?;
		let object_dir = self.object_dir(&source_digest.hash)?;
		if object_dir.exists() {
			return self.verify_existing(&source_digest, &object_dir);
		}

		fs::create_dir_all(&self.root)?;
		fs::create_dir_all(&self.work)?;
		fs::create_dir_all(object_dir.parent().expect("object path has shard parent"))?;
		let staging = tempfile::Builder::new()
			.prefix("snapshot-")
			.tempdir_in(&self.work)?;
		let staging_tree = staging.path().join("tree");
		fs::create_dir(&staging_tree)?;
		clone_tree(source, &staging_tree)?;

		let staged_digest = digest_tree(&staging_tree)?;
		if staged_digest != source_digest {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!(
					"source changed while snapshotting {}: expected {}, got {}",
					source.display(),
					source_digest.hash,
					staged_digest.hash
				),
			));
		}
		write_marker(staging.path(), &source_digest)?;

		match fs::rename(staging.path(), &object_dir) {
			Ok(()) => Ok(StoredObject {
				hash: source_digest.hash,
				tree: object_dir.join("tree"),
				stats: source_digest.stats,
				newly_stored: true,
			}),
			Err(_err) if object_dir.exists() => self.verify_existing(&source_digest, &object_dir),
			Err(err) => Err(err),
		}
	}

	pub fn export_object(
		&self,
		hash: &str,
		output: &Path,
		profile: ExportProfile,
	) -> io::Result<ExportedArchive> {
		let tree = self.verify_object(hash)?.tree;
		if let Some(parent) = output.parent() {
			fs::create_dir_all(parent)?;
		}

		let encoder = zstd::Encoder::new(File::create(output)?, 9)?;
		let mut builder = tar::Builder::new(encoder);
		for entry in collect_entries(&tree)? {
			if profile == ExportProfile::Semantic && !is_semantic_entry(&entry.relative, entry.kind)
			{
				continue;
			}
			append_tar_entry(&mut builder, &entry)?;
		}
		let encoder = builder.into_inner()?;
		encoder.finish()?;

		let bytes = fs::metadata(output)?.len();
		Ok(ExportedArchive {
			path: output.to_path_buf(),
			hash: digest_file(output)?,
			bytes,
		})
	}

	fn verify_existing(
		&self,
		expected: &TreeDigest,
		object_dir: &Path,
	) -> io::Result<StoredObject> {
		let marker = read_marker(object_dir)?;
		if marker != *expected {
			return Err(io::Error::new(
				ErrorKind::AlreadyExists,
				format!(
					"object path {} exists with mismatched metadata",
					object_dir.display()
				),
			));
		}
		let tree = object_dir.join("tree");
		let actual = digest_tree(&tree)?;
		if actual != *expected {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!("stored object {} is corrupt", expected.hash),
			));
		}
		Ok(StoredObject {
			hash: expected.hash.clone(),
			tree,
			stats: expected.stats.clone(),
			newly_stored: false,
		})
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
	Directory,
	File,
	Symlink,
}

#[derive(Clone, Debug)]
struct TreeEntry {
	absolute: PathBuf,
	relative: PathBuf,
	key: Vec<u8>,
	kind: EntryKind,
}

pub fn digest_tree(root: &Path) -> io::Result<TreeDigest> {
	if !root.is_dir() {
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!("tree root is not a directory: {}", root.display()),
		));
	}
	let entries = collect_entries(root)?;
	let mut hasher = blake3::Hasher::new();
	hasher.update(HASH_FORMAT.as_bytes());
	let mut stats = TreeStats::default();
	let mut buffer = vec![0_u8; 1024 * 1024];

	for entry in entries {
		hasher.update(&(entry.key.len() as u64).to_le_bytes());
		hasher.update(&entry.key);
		match entry.kind {
			EntryKind::Directory => {
				hasher.update(b"D");
				stats.directories += 1;
			}
			EntryKind::File => {
				hasher.update(b"F");
				let metadata = fs::symlink_metadata(&entry.absolute)?;
				let size = metadata.len();
				hasher.update(&size.to_le_bytes());
				hasher.update(&[executable_bit(&metadata)]);
				let mut file = File::open(&entry.absolute)?;
				loop {
					let read = file.read(&mut buffer)?;
					if read == 0 {
						break;
					}
					hasher.update(&buffer[..read]);
				}
				stats.files += 1;
				stats.bytes = stats.bytes.checked_add(size).ok_or_else(|| {
					io::Error::new(ErrorKind::InvalidData, "tree byte count overflowed u64")
				})?;
			}
			EntryKind::Symlink => {
				hasher.update(b"L");
				let target = fs::read_link(&entry.absolute)?;
				let target_bytes = path_bytes(&target);
				hasher.update(&(target_bytes.len() as u64).to_le_bytes());
				hasher.update(&target_bytes);
				stats.symlinks += 1;
			}
		}
	}

	Ok(TreeDigest {
		hash: hasher.finalize().to_hex().to_string(),
		stats,
	})
}

fn collect_entries(root: &Path) -> io::Result<Vec<TreeEntry>> {
	let mut entries = Vec::new();
	for result in WalkDir::new(root).follow_links(false) {
		let entry = result.map_err(io::Error::other)?;
		if entry.path() == root {
			continue;
		}
		let relative = entry
			.path()
			.strip_prefix(root)
			.expect("walkdir entry remains under root")
			.to_path_buf();
		if is_excluded(&relative) {
			if entry.file_type().is_dir() {
				continue;
			}
			continue;
		}
		let kind = if entry.file_type().is_dir() {
			EntryKind::Directory
		} else if entry.file_type().is_file() {
			EntryKind::File
		} else if entry.file_type().is_symlink() {
			EntryKind::Symlink
		} else {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!("unsupported file type: {}", entry.path().display()),
			));
		};
		entries.push(TreeEntry {
			absolute: entry.path().to_path_buf(),
			key: path_bytes(&relative),
			relative,
			kind,
		});
	}
	entries.sort_by(|left, right| left.key.cmp(&right.key));
	Ok(entries)
}

fn is_excluded(relative: &Path) -> bool {
	relative.components().any(|component| match component {
		Component::Normal(name) => name == ".git" || name == ".DS_Store" || name == OBJECT_MARKER,
		_ => false,
	})
}

fn clone_tree(source: &Path, destination: &Path) -> io::Result<()> {
	for entry in collect_entries(source)? {
		let target = destination.join(&entry.relative);
		match entry.kind {
			EntryKind::Directory => fs::create_dir(&target)?,
			EntryKind::File => clone_file(&entry.absolute, &target)?,
			EntryKind::Symlink => clone_symlink(source, &entry.absolute, &entry.relative, &target)?,
		}
	}
	Ok(())
}

#[cfg(target_os = "macos")]
fn clone_file(source: &Path, destination: &Path) -> io::Result<()> {
	let source = path_c_string(source)?;
	let destination = path_c_string(destination)?;
	// SAFETY: both C strings are NUL-terminated, remain alive for the call, and
	// clonefile does not retain either pointer.
	let result = unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) };
	if result == 0 {
		Ok(())
	} else {
		Err(io::Error::last_os_error())
	}
}

#[cfg(not(target_os = "macos"))]
fn clone_file(source: &Path, _destination: &Path) -> io::Result<()> {
	Err(io::Error::new(
		ErrorKind::Unsupported,
		format!(
			"copy-on-write snapshots require macOS clonefile; cannot snapshot {}",
			source.display()
		),
	))
}

#[cfg(target_os = "macos")]
fn clone_symlink(
	root: &Path,
	source: &Path,
	relative: &Path,
	destination: &Path,
) -> io::Result<()> {
	use std::os::unix::fs::symlink;

	let target = fs::read_link(source)?;
	ensure_internal_symlink(root, relative, &target)?;
	symlink(target, destination)
}

#[cfg(not(target_os = "macos"))]
fn clone_symlink(
	root: &Path,
	source: &Path,
	relative: &Path,
	_destination: &Path,
) -> io::Result<()> {
	let target = fs::read_link(source)?;
	ensure_internal_symlink(root, relative, &target)?;
	Err(io::Error::new(
		ErrorKind::Unsupported,
		"copy-on-write snapshots require macOS",
	))
}

fn ensure_internal_symlink(root: &Path, relative: &Path, target: &Path) -> io::Result<()> {
	if target.is_absolute() {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!(
				"absolute symlink target is not archivable: {}",
				target.display()
			),
		));
	}
	let parent = relative.parent().unwrap_or_else(|| Path::new(""));
	let mut depth = 0_usize;
	for component in parent.components().chain(target.components()) {
		match component {
			Component::CurDir => {}
			Component::Normal(_) => depth += 1,
			Component::ParentDir if depth > 0 => depth -= 1,
			Component::ParentDir => {
				return Err(io::Error::new(
					ErrorKind::InvalidData,
					format!(
						"symlink {} escapes snapshot root {}",
						relative.display(),
						root.display()
					),
				));
			}
			Component::RootDir | Component::Prefix(_) => {
				return Err(io::Error::new(
					ErrorKind::InvalidData,
					format!("invalid symlink target: {}", target.display()),
				));
			}
		}
	}
	Ok(())
}

fn append_tar_entry(
	builder: &mut tar::Builder<zstd::Encoder<'static, File>>,
	entry: &TreeEntry,
) -> io::Result<()> {
	let mut header = tar::Header::new_gnu();
	header.set_mtime(0);
	header.set_uid(0);
	header.set_gid(0);
	header.set_username("")?;
	header.set_groupname("")?;
	match entry.kind {
		EntryKind::Directory => {
			header.set_entry_type(tar::EntryType::Directory);
			header.set_mode(0o755);
			header.set_size(0);
			header.set_cksum();
			builder.append_data(&mut header, &entry.relative, io::empty())?;
		}
		EntryKind::File => {
			let metadata = fs::symlink_metadata(&entry.absolute)?;
			header.set_entry_type(tar::EntryType::Regular);
			header.set_mode(if executable_bit(&metadata) == 1 {
				0o755
			} else {
				0o644
			});
			header.set_size(metadata.len());
			header.set_cksum();
			builder.append_data(&mut header, &entry.relative, File::open(&entry.absolute)?)?;
		}
		EntryKind::Symlink => {
			header.set_entry_type(tar::EntryType::Symlink);
			header.set_mode(0o777);
			header.set_size(0);
			header.set_link_name(fs::read_link(&entry.absolute)?)?;
			header.set_cksum();
			builder.append_data(&mut header, &entry.relative, io::empty())?;
		}
	}
	Ok(())
}

fn is_semantic_entry(relative: &Path, kind: EntryKind) -> bool {
	if kind == EntryKind::Directory {
		return true;
	}
	if relative
		.file_name()
		.is_some_and(|name| name == "descriptor.mod")
	{
		return true;
	}
	relative
		.extension()
		.and_then(|extension| extension.to_str())
		.is_some_and(|extension| {
			matches!(
				extension.to_ascii_lowercase().as_str(),
				"txt" | "yml" | "yaml" | "csv" | "json" | "gui" | "mod" | "asset"
			)
		})
}

fn write_marker(object_dir: &Path, digest: &TreeDigest) -> io::Result<()> {
	let bytes = serde_json::to_vec_pretty(digest).map_err(io::Error::other)?;
	fs::write(object_dir.join(OBJECT_MARKER), bytes)
}

fn read_marker(object_dir: &Path) -> io::Result<TreeDigest> {
	let path = object_dir.join(OBJECT_MARKER);
	serde_json::from_slice(&fs::read(&path)?).map_err(|err| {
		io::Error::new(
			ErrorKind::InvalidData,
			format!("invalid object marker {}: {err}", path.display()),
		)
	})
}

fn digest_file(path: &Path) -> io::Result<String> {
	let mut hasher = blake3::Hasher::new();
	let mut file = File::open(path)?;
	let mut buffer = vec![0_u8; 1024 * 1024];
	loop {
		let read = file.read(&mut buffer)?;
		if read == 0 {
			break;
		}
		hasher.update(&buffer[..read]);
	}
	Ok(hasher.finalize().to_hex().to_string())
}

fn validate_hash(hash: &str) -> io::Result<()> {
	if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
		Ok(())
	} else {
		Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!("invalid BLAKE3 hash: {hash}"),
		))
	}
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
	use std::os::unix::ffi::OsStrExt;
	path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
	path.to_string_lossy().replace('\\', "/").into_bytes()
}

#[cfg(target_os = "macos")]
fn path_c_string(path: &Path) -> io::Result<CString> {
	use std::os::unix::ffi::OsStrExt;
	CString::new(path.as_os_str().as_bytes()).map_err(|_| {
		io::Error::new(
			ErrorKind::InvalidInput,
			format!("path contains NUL byte: {}", path.display()),
		)
	})
}

#[cfg(not(target_os = "macos"))]
fn path_c_string(_path: &Path) -> io::Result<CString> {
	unreachable!("path_c_string is only used by macOS clonefile")
}

#[cfg(unix)]
fn executable_bit(metadata: &fs::Metadata) -> u8 {
	use std::os::unix::fs::PermissionsExt;
	u8::from(metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable_bit(_metadata: &fs::Metadata) -> u8 {
	0
}

#[cfg(test)]
mod tests {
	use super::*;

	fn write_fixture(root: &Path) {
		fs::create_dir_all(root.join("common/governments")).unwrap();
		fs::create_dir_all(root.join("empty")).unwrap();
		fs::write(root.join("descriptor.mod"), b"name=\"fixture\"\n").unwrap();
		fs::write(
			root.join("common/governments/example.txt"),
			b"government = { rank = 1 }\n",
		)
		.unwrap();
		fs::write(root.join("preview.jpg"), b"binary-image-placeholder").unwrap();
	}

	#[test]
	fn tree_digest_is_stable_and_tracks_content() {
		let temp = tempfile::tempdir().unwrap();
		write_fixture(temp.path());
		let first = digest_tree(temp.path()).unwrap();
		let second = digest_tree(temp.path()).unwrap();
		assert_eq!(first, second);
		assert_eq!(first.stats.files, 3);
		assert_eq!(first.stats.directories, 3);

		fs::write(temp.path().join(".DS_Store"), b"ignored").unwrap();
		assert_eq!(digest_tree(temp.path()).unwrap(), first);

		fs::write(
			temp.path().join("common/governments/example.txt"),
			b"government = { rank = 2 }\n",
		)
		.unwrap();
		assert_ne!(digest_tree(temp.path()).unwrap().hash, first.hash);
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn snapshot_is_verified_and_idempotent() {
		let source = tempfile::tempdir().unwrap();
		write_fixture(source.path());
		let dataset = tempfile::tempdir().unwrap();
		let store = ObjectStore::new(dataset.path().join("objects"), dataset.path().join("work"));

		let first = store.snapshot_tree(source.path()).unwrap();
		assert!(first.newly_stored);
		assert_eq!(
			fs::read(first.tree.join("preview.jpg")).unwrap(),
			b"binary-image-placeholder"
		);
		let second = store.snapshot_tree(source.path()).unwrap();
		assert!(!second.newly_stored);
		assert_eq!(second.hash, first.hash);
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn escaping_symlinks_are_rejected() {
		use std::os::unix::fs::symlink;

		let source = tempfile::tempdir().unwrap();
		fs::create_dir(source.path().join("nested")).unwrap();
		symlink("../../outside", source.path().join("nested/escape")).unwrap();
		let dataset = tempfile::tempdir().unwrap();
		let store = ObjectStore::new(dataset.path().join("objects"), dataset.path().join("work"));
		assert_eq!(
			store.snapshot_tree(source.path()).unwrap_err().kind(),
			ErrorKind::InvalidData
		);
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn exports_are_deterministic_and_semantic_profile_excludes_media() {
		let source = tempfile::tempdir().unwrap();
		write_fixture(source.path());
		let dataset = tempfile::tempdir().unwrap();
		let store = ObjectStore::new(dataset.path().join("objects"), dataset.path().join("work"));
		let object = store.snapshot_tree(source.path()).unwrap();
		let first = dataset.path().join("first.tar.zst");
		let second = dataset.path().join("second.tar.zst");

		let first_export = store
			.export_object(&object.hash, &first, ExportProfile::Semantic)
			.unwrap();
		let second_export = store
			.export_object(&object.hash, &second, ExportProfile::Semantic)
			.unwrap();
		assert_eq!(first_export.hash, second_export.hash);
		assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());

		let decoder = zstd::Decoder::new(File::open(first).unwrap()).unwrap();
		let mut archive = tar::Archive::new(decoder);
		let names: Vec<PathBuf> = archive
			.entries()
			.unwrap()
			.map(|entry| entry.unwrap().path().unwrap().into_owned())
			.collect();
		assert!(names.iter().any(|name| name == Path::new("descriptor.mod")));
		assert!(!names.iter().any(|name| name == Path::new("preview.jpg")));
	}
}
