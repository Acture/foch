//! The plugin version store: validate a package, then keep each version as an
//! immutable directory under the Foch data directory.
//!
//! Validation works on an in-memory list of package entries so it needs no
//! archive library and is fully testable; the CLI decodes a ZIP into that
//! list. Nothing is written until every check passes, and an installed version
//! directory is never overwritten, so a version a launch is using cannot change
//! under it.

use super::manifest::{FILE_NAME, Manifest};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `IMAGE_FILE_MACHINE_AMD64`: the only machine type supported now.
pub const MACHINE_AMD64: u16 = 0x8664;

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
/// and `.` components.
fn safe_relative(path: &str) -> Result<String, ImportError> {
	let unsafe_path = || ImportError::UnsafePath(path.to_string());
	if path.is_empty() {
		return Err(unsafe_path());
	}
	let normalized = path.replace('\\', "/");
	// A leading slash, or a Windows drive like `C:`, is absolute.
	if normalized.starts_with('/') || normalized.as_bytes().get(1) == Some(&b':') {
		return Err(unsafe_path());
	}
	let mut parts = Vec::new();
	for part in normalized.split('/') {
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
	let destination = version_dir(store_root, package);
	if destination.is_dir() {
		return Ok(Installed::AlreadyPresent(destination));
	}
	let parent = plugin_dir(store_root, &package.manifest.plugin.id);
	fs::create_dir_all(&parent)?;

	// Stage under a unique sibling name on the same volume, then rename.
	let staging = parent.join(format!(".staging-{}", package.version_dir_name()));
	if staging.exists() {
		fs::remove_dir_all(&staging)?;
	}
	fs::create_dir_all(&staging)?;
	let staged = write_entries(&staging, package).and_then(|()| {
		// A pre-existing destination between the check and the rename means a
		// concurrent writer won; keep theirs.
		match fs::rename(&staging, &destination) {
			Ok(()) => Ok(Installed::New(destination.clone())),
			Err(_) if destination.is_dir() => Ok(Installed::AlreadyPresent(destination.clone())),
			Err(error) => Err(error),
		}
	});
	if staged.is_err() {
		let _ = fs::remove_dir_all(&staging);
	}
	staged
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
	let mut problems = Vec::new();
	let Ok(plugin_dirs) = fs::read_dir(store_root) else {
		return (catalog, problems);
	};
	for plugin_entry in plugin_dirs.flatten() {
		if !plugin_entry.path().is_dir() {
			continue;
		}
		let Ok(version_dirs) = fs::read_dir(plugin_entry.path()) else {
			continue;
		};
		for version_entry in version_dirs.flatten() {
			let dir = version_entry.path();
			let name = version_entry.file_name();
			let name = name.to_string_lossy();
			// A staging directory from an interrupted install is not a version.
			if name.starts_with('.') || !dir.is_dir() {
				continue;
			}
			let manifest_path = dir.join(FILE_NAME);
			match fs::read_to_string(&manifest_path)
				.map_err(|error| error.to_string())
				.and_then(|text| Manifest::parse(&text).map_err(|error| error.to_string()))
			{
				Ok(manifest) => {
					catalog
						.entry(manifest.plugin.id.clone())
						.or_default()
						.insert(manifest.plugin.version.clone(), manifest);
				}
				Err(message) => problems.push(format!("{}: {message}", dir.display())),
			}
		}
	}
	(catalog, problems)
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
