//! The plugin version store: validate a package, then keep each version as an
//! immutable directory under the Foch data directory.
//!
//! Validation works on an in-memory list of package entries so it needs no
//! archive library and is fully testable; the CLI reads an extracted directory
//! into that list. Nothing is written until every check passes, and an installed
//! version directory is never overwritten, so a version a launch is using
//! cannot change under it.

use super::manifest::{FILE_NAME, Manifest};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `IMAGE_FILE_MACHINE_AMD64`: the only machine type supported now.
pub const MACHINE_AMD64: u16 = 0x8664;

/// Known installation proxies are excluded, not copied into a Foch layer.
pub fn is_proxy_name(name: &str) -> bool {
	matches!(
		name.to_ascii_lowercase().as_str(),
		"version.dll" | "d3d9.dll" | "dinput8.dll" | "winmm.dll" | "dxgi.dll"
	)
}

/// Bind an adapter to these exact extracted release bytes. Its upstream
/// VERSION proxy is omitted; the runtime deploys the Foch host instead.
pub fn adapt(
	mut manifest: Manifest,
	mut entries: Vec<ArchiveEntry>,
) -> Result<ValidatedPackage, Vec<ImportError>> {
	entries.retain(|entry| !is_proxy_name(&entry.path) && entry.path != FILE_NAME);
	manifest.files = entries
		.iter()
		.map(|entry| super::manifest::FileEntry {
			path: entry.path.clone(),
			sha256: sha256_hex(&entry.data),
		})
		.collect();
	let text = toml::to_string_pretty(&manifest)
		.map_err(|error| vec![ImportError::Manifest(error.to_string())])?;
	entries.push(ArchiveEntry {
		path: FILE_NAME.into(),
		data: text.into_bytes(),
	});
	validate(entries)
}

/// One file in a candidate package, its path relative to the package root.
#[derive(Clone, Debug)]
pub struct ArchiveEntry {
	pub path: String,
	pub data: Vec<u8>,
}

/// Why a package cannot be imported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportError {
	/// A path escapes the package root or is otherwise unsafe.
	UnsafePath(String),
	/// Two entries differ only by case; Windows would collapse them.
	CaseCollision(String, String),
	/// No `foch-plugin.toml` at the package root.
	MissingManifest,
	Manifest(String),
	/// A file the manifest lists is absent from the package.
	MissingFile(String),
	/// A listed file's bytes do not match its declared digest.
	DigestMismatch {
		path: String,
	},
	/// The entry DLL is missing or not an AMD64 PE image.
	BadEntryDll {
		path: String,
		reason: String,
	},
}

impl std::fmt::Display for ImportError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::UnsafePath(path) => write!(f, "unsafe path in package: {path}"),
			Self::CaseCollision(a, b) => {
				write!(f, "paths collide case-insensitively on Windows: {a} vs {b}")
			}
			Self::MissingManifest => write!(f, "package has no {FILE_NAME} at its root"),
			Self::Manifest(message) => write!(f, "{message}"),
			Self::MissingFile(path) => {
				write!(f, "manifest lists {path}, which is not in the package")
			}
			Self::DigestMismatch { path } => write!(f, "{path} does not match its declared sha256"),
			Self::BadEntryDll { path, reason } => write!(f, "entry DLL {path}: {reason}"),
		}
	}
}

impl std::error::Error for ImportError {}

/// A package that passed every check and is ready to install.
#[derive(Clone, Debug)]
pub struct ValidatedPackage {
	pub manifest: Manifest,
	entries: Vec<ArchiveEntry>,
	/// SHA-256 over the sorted (path, content-digest) pairs: a stable identity
	/// for this exact set of bytes, independent of entry order.
	pub digest: String,
}

impl ValidatedPackage {
	pub fn entries(&self) -> &[ArchiveEntry] {
		&self.entries
	}

	/// The directory name a version is stored under: `<version>+<digest12>`.
	pub fn version_dir_name(&self) -> String {
		format!("{}+{}", self.manifest.plugin.version, &self.digest[..12])
	}
}

fn sha256_hex(bytes: &[u8]) -> String {
	let mut hasher = Sha256::new();
	hasher.update(bytes);
	hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
	let mut out = String::with_capacity(bytes.len() * 2);
	for byte in bytes {
		out.push_str(&format!("{byte:02x}"));
	}
	out
}

/// A normalized, verified-safe relative path, or why it is unsafe. Accepts
/// `/` and `\` separators and rejects absolute paths, drive letters, `..`,
/// and `.` components. Package-relative names currently use ASCII so the
/// case-collision checks match supported Windows paths on every platform.
pub(super) fn safe_relative(path: &str) -> Result<String, ImportError> {
	let unsafe_path = || ImportError::UnsafePath(path.to_string());
	if path.is_empty() || !path.is_ascii() {
		return Err(unsafe_path());
	}
	let normalized = path.replace('\\', "/");
	// A leading slash, or a Windows drive like `C:`, is absolute.
	if normalized.starts_with('/') || normalized.as_bytes().get(1) == Some(&b':') {
		return Err(unsafe_path());
	}
	let mut parts = Vec::new();
	for part in normalized.split('/') {
		// Win32 strips trailing dots/spaces and interprets ':' as an alternate
		// stream. Reject aliases before writing either the store or a runtime.
		if part.ends_with(['.', ' '])
			|| part
				.chars()
				.any(|c| c.is_control() || "<>:\"|?*".contains(c))
		{
			return Err(unsafe_path());
		}
		let base = part
			.split('.')
			.next()
			.unwrap_or_default()
			.to_ascii_uppercase();
		if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
			|| base
				.strip_prefix("COM")
				.or_else(|| base.strip_prefix("LPT"))
				.is_some_and(|suffix| {
					matches!(
						suffix,
						"1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
					)
				}) {
			return Err(unsafe_path());
		}
		match part {
			"" | "." => return Err(unsafe_path()), // empty component or `.`
			".." => return Err(unsafe_path()),
			_ => parts.push(part),
		}
	}
	Ok(parts.join("/"))
}

/// The COFF machine type of a PE image, if the headers are well-formed.
pub fn pe_machine(bytes: &[u8]) -> Option<u16> {
	if bytes.len() < 0x40 || &bytes[0..2] != b"MZ" {
		return None;
	}
	let e_lfanew = u32::from_le_bytes(bytes[0x3c..0x40].try_into().ok()?) as usize;
	if bytes.len() < e_lfanew + 6 || &bytes[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
		return None;
	}
	Some(u16::from_le_bytes(
		bytes[e_lfanew + 4..e_lfanew + 6].try_into().ok()?,
	))
}

/// Validate a candidate package's entries. On success the manifest, the safe
/// entries and the package digest are returned; on failure every problem is.
pub fn validate(entries: Vec<ArchiveEntry>) -> Result<ValidatedPackage, Vec<ImportError>> {
	let mut errors = Vec::new();

	// Normalize paths and catch unsafe ones and case collisions.
	let mut safe_entries: Vec<ArchiveEntry> = Vec::new();
	let mut seen_lower: BTreeMap<String, String> = BTreeMap::new();
	for entry in entries {
		match safe_relative(&entry.path) {
			Ok(normalized) => {
				let lower = normalized.to_ascii_lowercase();
				if let Some(existing) = seen_lower.get(&lower) {
					errors.push(ImportError::CaseCollision(
						existing.clone(),
						normalized.clone(),
					));
				} else {
					seen_lower.insert(lower, normalized.clone());
				}
				safe_entries.push(ArchiveEntry {
					path: normalized,
					data: entry.data,
				});
			}
			Err(error) => errors.push(error),
		}
	}

	let by_path: BTreeMap<&str, &[u8]> = safe_entries
		.iter()
		.map(|entry| (entry.path.as_str(), entry.data.as_slice()))
		.collect();

	let Some(manifest_bytes) = by_path.get(FILE_NAME) else {
		errors.push(ImportError::MissingManifest);
		return Err(errors);
	};
	let manifest = match std::str::from_utf8(manifest_bytes)
		.map_err(|error| error.to_string())
		.and_then(|text| Manifest::parse(text).map_err(|error| error.to_string()))
	{
		Ok(manifest) => manifest,
		Err(message) => {
			errors.push(ImportError::Manifest(message));
			return Err(errors);
		}
	};

	// Every listed file must be present with the declared digest.
	for file in &manifest.files {
		match by_path.get(file.path.as_str()) {
			None => errors.push(ImportError::MissingFile(file.path.clone())),
			Some(bytes) => {
				if sha256_hex(bytes) != file.sha256.to_ascii_lowercase() {
					errors.push(ImportError::DigestMismatch {
						path: file.path.clone(),
					});
				}
			}
		}
	}

	// The entry DLL must exist and be an AMD64 PE image.
	match by_path.get(manifest.entry.path.as_str()) {
		None => errors.push(ImportError::BadEntryDll {
			path: manifest.entry.path.clone(),
			reason: "not found in the package".into(),
		}),
		Some(bytes) => match pe_machine(bytes) {
			Some(MACHINE_AMD64) => {}
			Some(machine) => errors.push(ImportError::BadEntryDll {
				path: manifest.entry.path.clone(),
				reason: format!("machine type {machine:#06x} is not AMD64"),
			}),
			None => errors.push(ImportError::BadEntryDll {
				path: manifest.entry.path.clone(),
				reason: "not a PE image".into(),
			}),
		},
	}

	if !errors.is_empty() {
		return Err(errors);
	}

	let digest = package_digest(&safe_entries);
	Ok(ValidatedPackage {
		manifest,
		entries: safe_entries,
		digest,
	})
}

/// A content identity over the whole package: SHA-256 of each entry's path and
/// content digest, sorted by path so entry order does not matter.
fn package_digest(entries: &[ArchiveEntry]) -> String {
	let mut pairs: Vec<(&str, String)> = entries
		.iter()
		.map(|entry| (entry.path.as_str(), sha256_hex(&entry.data)))
		.collect();
	pairs.sort();
	let mut hasher = Sha256::new();
	for (path, digest) in pairs {
		hasher.update(path.as_bytes());
		hasher.update(b"\0");
		hasher.update(digest.as_bytes());
		hasher.update(b"\n");
	}
	hex(&hasher.finalize())
}

/// Where a plugin's versions live under the store root.
pub fn plugin_dir(store_root: &Path, plugin_id: &str) -> PathBuf {
	store_root.join(plugin_id)
}

/// The directory a validated package installs to.
pub fn version_dir(store_root: &Path, package: &ValidatedPackage) -> PathBuf {
	plugin_dir(store_root, &package.manifest.plugin.id).join(package.version_dir_name())
}

/// The outcome of an install.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Installed {
	/// Written freshly to this path.
	New(PathBuf),
	/// This exact version and digest was already stored; nothing changed.
	AlreadyPresent(PathBuf),
}

/// Install a validated package under `store_root`. An existing version
/// directory with the same name is left untouched (so a running launch is
/// never disturbed); the write is staged in a sibling temp directory and moved
/// into place only once complete.
pub fn install(store_root: &Path, package: &ValidatedPackage) -> io::Result<Installed> {
	fs::create_dir_all(store_root)?;
	let root = store_root.canonicalize()?;
	let parent = plugin_dir(&root, &package.manifest.plugin.id);
	match fs::create_dir(&parent) {
		Ok(()) => {}
		Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
		Err(error) => return Err(error),
	}
	let parent = checked_directory(&parent, &root)?;
	let destination = parent.join(package.version_dir_name());
	match fs::symlink_metadata(&destination) {
		Ok(_) => {
			return checked_directory(&destination, &root).map(Installed::AlreadyPresent);
		}
		Err(error) if error.kind() == io::ErrorKind::NotFound => {}
		Err(error) => return Err(error),
	}

	// Stage under a unique sibling name on the same volume, then rename.
	let staging = tempfile::Builder::new()
		.prefix(&format!(".staging-{}-", package.version_dir_name()))
		.tempdir_in(&parent)?;
	// TempDir removes this importer's private staging directory on failures
	// and AlreadyPresent, including a concurrent winner of the final rename.
	write_entries(staging.path(), package).and_then(|()| {
		// A pre-existing destination between the check and the rename means a
		// concurrent writer won; keep theirs.
		match fs::rename(staging.path(), &destination) {
			Ok(()) => Ok(Installed::New(destination.clone())),
			Err(error) => match checked_directory(&destination, &root) {
				Ok(directory) => Ok(Installed::AlreadyPresent(directory)),
				Err(_) => Err(error),
			},
		}
	})
}

/// One installed version: where it lives and its manifest.
#[derive(Clone, Debug)]
pub struct InstalledVersion {
	pub dir: PathBuf,
	pub manifest: Manifest,
}

/// Read the store into the map the planner wants: plugin id → version →
/// manifest. Unreadable or malformed version directories are skipped so one
/// bad entry never hides the rest; `problems` collects why each was skipped.
pub fn catalog(
	store_root: &Path,
) -> (
	BTreeMap<String, BTreeMap<semver::Version, Manifest>>,
	Vec<String>,
) {
	let mut catalog: BTreeMap<String, BTreeMap<semver::Version, Manifest>> = BTreeMap::new();
	let (versions, problems) = installed_versions(store_root);
	for version in versions {
		let manifest = version.manifest;
		catalog
			.entry(manifest.plugin.id.clone())
			.or_default()
			.insert(manifest.plugin.version.clone(), manifest);
	}
	(catalog, problems)
}

/// Preserve installed directory identities for frozen runtime deployment.
pub fn installed_versions(store_root: &Path) -> (Vec<InstalledVersion>, Vec<String>) {
	let mut versions = Vec::new();
	let mut problems = Vec::new();
	let Ok(root) = store_root.canonicalize() else {
		return (versions, problems);
	};
	let Ok(plugin_dirs) = fs::read_dir(&root) else {
		return (versions, problems);
	};
	for plugin_entry in plugin_dirs.flatten() {
		if !plugin_entry
			.file_type()
			.is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
		{
			continue;
		}
		let plugin_dir = match checked_directory(&plugin_entry.path(), &root) {
			Ok(path) => path,
			Err(error) => {
				problems.push(format!("{}: {error}", plugin_entry.path().display()));
				continue;
			}
		};
		let version_dirs = match fs::read_dir(&plugin_dir) {
			Ok(entries) => entries,
			Err(error) => {
				problems.push(format!("{}: {error}", plugin_dir.display()));
				continue;
			}
		};
		for version_entry in version_dirs.flatten() {
			let dir = version_entry.path();
			let name = version_entry.file_name();
			let name = name.to_string_lossy();
			// A staging directory from an interrupted install is not a version.
			if name.starts_with('.')
				|| !version_entry
					.file_type()
					.is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
			{
				continue;
			}
			let dir = match checked_directory(&dir, &root) {
				Ok(path) => path,
				Err(error) => {
					problems.push(format!("{}: {error}", dir.display()));
					continue;
				}
			};
			let manifest_path = dir.join(FILE_NAME);
			match ordinary_metadata(&manifest_path)
				.and_then(|metadata| {
					if !metadata.is_file() {
						return Err(io::Error::other("manifest is not a regular file"));
					}
					fs::read_to_string(&manifest_path)
				})
				.map_err(|error| error.to_string())
				.and_then(|text| Manifest::parse(&text).map_err(|error| error.to_string()))
			{
				Ok(manifest) => {
					versions.push(InstalledVersion { dir, manifest });
				}
				Err(message) => problems.push(format!("{}: {message}", dir.display())),
			}
		}
	}
	versions.sort_by(|a, b| a.dir.cmp(&b.dir));
	(versions, problems)
}

pub(super) fn ordinary_metadata(path: &Path) -> io::Result<fs::Metadata> {
	let metadata = fs::symlink_metadata(path)?;
	if metadata.file_type().is_symlink() {
		return Err(io::Error::other(
			"installed artifact links are not supported",
		));
	}
	#[cfg(windows)]
	{
		use std::os::windows::fs::MetadataExt;
		if metadata.file_attributes() & 0x400 != 0 {
			return Err(io::Error::other(
				"installed artifact reparse points are not supported",
			));
		}
	}
	Ok(metadata)
}

pub(super) fn checked_directory(path: &Path, canonical_store: &Path) -> io::Result<PathBuf> {
	if !ordinary_metadata(path)?.is_dir() {
		return Err(io::Error::other("installed artifact is not a directory"));
	}
	let directory = path.canonicalize()?;
	if !directory.starts_with(canonical_store) {
		return Err(io::Error::other("installed artifact escapes the store"));
	}
	Ok(directory)
}

fn write_entries(root: &Path, package: &ValidatedPackage) -> io::Result<()> {
	for entry in &package.entries {
		let path = root.join(entry.path.replace('/', std::path::MAIN_SEPARATOR_STR));
		if let Some(dir) = path.parent() {
			fs::create_dir_all(dir)?;
		}
		fs::write(&path, &entry.data)?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	// A minimal AMD64 PE image: MZ header, e_lfanew -> "PE\0\0" + machine.
	fn fake_dll() -> Vec<u8> {
		let mut bytes = vec![0u8; 0x80];
		bytes[0] = b'M';
		bytes[1] = b'Z';
		let e_lfanew: u32 = 0x40;
		bytes[0x3c..0x40].copy_from_slice(&e_lfanew.to_le_bytes());
		bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
		bytes[0x44..0x46].copy_from_slice(&MACHINE_AMD64.to_le_bytes());
		bytes
	}

	fn package(dll: Vec<u8>) -> Vec<ArchiveEntry> {
		let dll_digest = sha256_hex(&dll);
		let manifest = format!(
			r#"
schema = 1
[plugin]
id = "dev.foch.sample"
name = "Sample"
version = "0.1.0"
[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "*"
abi_major = 1
[entry]
kind = "native"
path = "sample.dll"
phase = "deferred"
[[files]]
path = "sample.dll"
sha256 = "{dll_digest}"
"#
		);
		vec![
			ArchiveEntry {
				path: FILE_NAME.into(),
				data: manifest.into_bytes(),
			},
			ArchiveEntry {
				path: "sample.dll".into(),
				data: dll,
			},
		]
	}

	#[test]
	fn accepts_a_well_formed_package() {
		let validated = validate(package(fake_dll())).unwrap();
		assert_eq!(validated.manifest.plugin.id, "dev.foch.sample");
		assert!(validated.version_dir_name().starts_with("0.1.0+"));
		assert_eq!(validated.digest.len(), 64);
	}

	#[test]
	fn rejects_path_traversal() {
		let mut entries = package(fake_dll());
		entries.push(ArchiveEntry {
			path: "../evil.dll".into(),
			data: vec![0],
		});
		let errors = validate(entries).unwrap_err();
		assert!(
			errors
				.iter()
				.any(|e| matches!(e, ImportError::UnsafePath(_)))
		);
	}

	#[test]
	fn rejects_absolute_and_drive_paths() {
		assert!(safe_relative("/etc/passwd").is_err());
		assert!(safe_relative("C:/windows/system32").is_err());
		assert!(safe_relative("a/../b").is_err());
		assert_eq!(safe_relative("plugins\\a.dll").unwrap(), "plugins/a.dll");
		for path in [
			"plugins/.. /outside.dll",
			"plugins/file.dll:stream",
			"plugins/file.dll.",
			"CON.txt",
			"plugins/NUL",
			"LPT1.log",
			"COM¹.txt",
		] {
			let mut entries = package(fake_dll());
			entries.push(ArchiveEntry {
				path: path.into(),
				data: vec![0],
			});
			assert!(
				validate(entries)
					.unwrap_err()
					.iter()
					.any(|error| matches!(error, ImportError::UnsafePath(_))),
				"accepted {path}"
			);
		}
	}

	#[test]
	fn rejects_case_collisions() {
		let mut entries = package(fake_dll());
		entries.push(ArchiveEntry {
			path: "Sample.DLL".into(),
			data: vec![0],
		});
		let errors = validate(entries).unwrap_err();
		assert!(
			errors
				.iter()
				.any(|e| matches!(e, ImportError::CaseCollision(_, _)))
		);
	}

	#[test]
	fn rejects_non_ascii_package_paths() {
		let mut entries = package(fake_dll());
		for path in ["ä.dll", "Ä.dll"] {
			entries.push(ArchiveEntry {
				path: path.into(),
				data: fake_dll(),
			});
		}
		assert!(validate(entries).is_err());
	}

	#[test]
	fn rejects_digest_mismatch() {
		let mut entries = package(fake_dll());
		// Corrupt the DLL after the manifest recorded its digest.
		entries[1].data.push(0xff);
		let errors = validate(entries).unwrap_err();
		assert!(
			errors
				.iter()
				.any(|e| matches!(e, ImportError::DigestMismatch { .. }))
		);
	}

	#[test]
	fn rejects_non_amd64_entry_dll() {
		let mut dll = fake_dll();
		dll[0x44..0x46].copy_from_slice(&0x014cu16.to_le_bytes()); // i386
		let errors = validate(package(dll)).unwrap_err();
		assert!(
			errors
				.iter()
				.any(|e| matches!(e, ImportError::BadEntryDll { .. }))
		);
	}

	#[test]
	fn concurrent_imports_keep_one_complete_version_and_no_staging_directories() {
		let temp = tempfile::tempdir().unwrap();
		let mut entries = package(fake_dll());
		entries.push(ArchiveEntry {
			path: "resources/large.bin".into(),
			data: vec![42; 2 * 1024 * 1024],
		});
		let validated = validate(entries).unwrap();
		let barrier = std::sync::Barrier::new(12);
		std::thread::scope(|scope| {
			let handles: Vec<_> = (0..12)
				.map(|_| {
					scope.spawn(|| {
						barrier.wait();
						install(temp.path(), &validated)
					})
				})
				.collect();
			let results: Vec<_> = handles
				.into_iter()
				.map(|handle| handle.join().unwrap().unwrap())
				.collect();
			assert_eq!(
				results
					.iter()
					.filter(|result| matches!(result, Installed::New(_)))
					.count(),
				1
			);
		});
		let installed = version_dir(temp.path(), &validated);
		assert_eq!(
			fs::read(installed.join("resources/large.bin")).unwrap(),
			vec![42; 2 * 1024 * 1024]
		);
		assert_eq!(
			fs::read_dir(plugin_dir(temp.path(), &validated.manifest.plugin.id))
				.unwrap()
				.count(),
			1
		);
	}

	#[test]
	fn install_is_atomic_and_idempotent() {
		let temp = tempfile::tempdir().unwrap();
		let validated = validate(package(fake_dll())).unwrap();
		let first = install(temp.path(), &validated).unwrap();
		let path = match &first {
			Installed::New(path) => path.clone(),
			other => panic!("expected New, got {other:?}"),
		};
		assert!(path.join("sample.dll").is_file());
		assert!(path.join(FILE_NAME).is_file());
		// A second install of the same version/digest changes nothing.
		assert_eq!(
			install(temp.path(), &validated).unwrap(),
			Installed::AlreadyPresent(path)
		);
		// No staging directory is left behind.
		let leftover = fs::read_dir(plugin_dir(temp.path(), "dev.foch.sample"))
			.unwrap()
			.filter_map(Result::ok)
			.any(|entry| entry.file_name().to_string_lossy().starts_with(".staging"));
		assert!(!leftover);
	}
}
