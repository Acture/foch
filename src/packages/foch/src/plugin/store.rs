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
		name.rsplit(['/', '\\'])
			.next()
			.unwrap_or_default()
			.to_ascii_lowercase()
			.as_str(),
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
	/// Another proxy loader would conflict with Foch's host.
	ProxyLoader(String),
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
			Self::ProxyLoader(path) => write!(f, "package contains another proxy loader: {path}"),
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
				if is_proxy_name(&normalized) {
					errors.push(ImportError::ProxyLoader(normalized.clone()));
				}
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
/// bad entry never hides the rest. Ambiguous versions are excluded and every
/// conflicting directory is reported in `problems`.
pub fn catalog(
	store_root: &Path,
) -> (
	BTreeMap<String, BTreeMap<semver::Version, Manifest>>,
	Vec<String>,
) {
	catalog_with_defaults(store_root, std::iter::empty())
}

/// Add default manifests only for versions with no installed directory. An
/// ambiguous installed version remains excluded rather than gaining a fallback.
pub fn catalog_with_defaults(
	store_root: &Path,
	defaults: impl IntoIterator<Item = Manifest>,
) -> (
	BTreeMap<String, BTreeMap<semver::Version, Manifest>>,
	Vec<String>,
) {
	let mut catalog: BTreeMap<String, BTreeMap<semver::Version, Manifest>> = BTreeMap::new();
	let (versions, mut problems) = installed_versions(store_root);
	let mut groups: BTreeMap<_, Vec<InstalledVersion>> = BTreeMap::new();
	for version in versions {
		groups
			.entry((
				version.manifest.plugin.id.clone(),
				version.manifest.plugin.version.clone(),
			))
			.or_default()
			.push(version);
	}
	for manifest in defaults {
		if !groups.contains_key(&(manifest.plugin.id.clone(), manifest.plugin.version.clone())) {
			catalog
				.entry(manifest.plugin.id.clone())
				.or_default()
				.entry(manifest.plugin.version.clone())
				.or_insert(manifest);
		}
	}
	for ((id, version), mut installed) in groups {
		if installed.len() > 1 {
			installed.sort_by(|a, b| a.dir.cmp(&b.dir));
			problems.push(format!(
				"ambiguous installed version {id} {version}: {}",
				installed
					.iter()
					.map(|item| item.dir.display().to_string())
					.collect::<Vec<_>>()
					.join(", ")
			));
			continue;
		}
		let manifest = installed.pop().unwrap().manifest;
		catalog.entry(id).or_default().insert(version, manifest);
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
					read_package_file(&manifest_path).and_then(|bytes| {
						String::from_utf8(bytes)
							.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
					})
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

/// Read an ordinary package file without following a final link.
/// Input hard links are copied as bytes and remain supported.
pub fn read_package_file(path: &Path) -> io::Result<Vec<u8>> {
	use std::io::Read;
	let mut file = open_package_file(path)?;
	let mut bytes = Vec::new();
	file.read_to_end(&mut bytes)?;
	Ok(bytes)
}

fn open_package_file(path: &Path) -> io::Result<fs::File> {
	let mut options = fs::OpenOptions::new();
	options.read(true);
	#[cfg(windows)]
	{
		use std::os::windows::fs::OpenOptionsExt;
		use windows_sys::Win32::Storage::FileSystem::{
			FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
		};
		options
			.share_mode(FILE_SHARE_READ)
			.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
	}
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
	}
	let file = options.open(path)?;
	let metadata = file.metadata()?;
	let linked = metadata.file_type().is_symlink();
	#[cfg(windows)]
	let linked = {
		use std::os::windows::fs::MetadataExt;
		linked || metadata.file_attributes() & 0x400 != 0
	};
	if !metadata.is_file() || linked {
		return Err(io::Error::other("package entry must be an ordinary file"));
	}
	Ok(file)
}

/// Create a fresh output without overwriting an existing leaf or link target.
pub fn create_package_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
	use std::io::Write;
	fs::OpenOptions::new()
		.write(true)
		.create_new(true)
		.open(path)?
		.write_all(bytes)
}

/// Replace an owned output by renaming a unique sibling, never truncating the
/// previous inode. Failure removes only this writer's temporary file.
pub fn replace_package_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
	use std::io::Write;
	let parent = path
		.parent()
		.filter(|parent| !parent.as_os_str().is_empty())
		.unwrap_or_else(|| Path::new("."));
	// tempfile's Windows persist calls Win32 directly. Give both paths the
	// extended parent, since the random sibling can exceed MAX_PATH even when
	// the short destination fits. Resolve the parent, never a linked leaf.
	#[cfg(windows)]
	let parent = parent.canonicalize()?;
	#[cfg(windows)]
	let destination =
		parent.join(path.file_name().ok_or_else(|| {
			io::Error::new(io::ErrorKind::InvalidInput, "output needs a file name")
		})?);
	#[cfg(windows)]
	let path = destination.as_path();
	let mut temporary = tempfile::Builder::new()
		.prefix(".foch-output-")
		.tempfile_in(parent)?;
	temporary.write_all(bytes)?;
	// Close the written handle before the Windows replacement operation.
	temporary
		.into_temp_path()
		.persist(path)
		.map_err(|error| error.error)
}

/// Copy an ordinary input through its checked handle into a fresh output,
/// preserving the input's permissions without reopening either file path.
pub fn copy_package_file(source: &Path, destination: &Path) -> io::Result<()> {
	let mut source = open_package_file(source)?;
	let permissions = source.metadata()?.permissions();
	let mut destination = fs::OpenOptions::new()
		.write(true)
		.create_new(true)
		.open(destination)?;
	io::copy(&mut source, &mut destination)?;
	destination.set_permissions(permissions)
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

/// Preserve a selected data-root alias while refusing links in the directory
/// components leading to the managed root, before any further creation.
pub(super) fn prepare_data_directory(data_root: &Path, relative: &str) -> io::Result<PathBuf> {
	let relative = safe_relative(relative).map_err(io::Error::other)?;
	fs::create_dir_all(data_root)?;
	let root = data_root.canonicalize()?;
	let mut directory = root.clone();
	for component in relative.split('/') {
		directory.push(component);
		match fs::create_dir(&directory) {
			Ok(()) => {}
			Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
			Err(error) => return Err(error),
		}
		directory = checked_directory(&directory, &root)?;
	}
	Ok(directory)
}

fn write_entries(root: &Path, package: &ValidatedPackage) -> io::Result<()> {
	for entry in &package.entries {
		let path = root.join(entry.path.replace('/', std::path::MAIN_SEPARATOR_STR));
		if let Some(dir) = path.parent() {
			fs::create_dir_all(dir)?;
		}
		create_package_file(&path, &entry.data)?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[cfg(any(unix, windows))]
	#[test]
	fn package_reads_refuse_leaf_link_swaps_and_preserve_hard_link_inputs() {
		let scratch = tempfile::tempdir().unwrap();
		let package = scratch.path().join("package");
		fs::create_dir(&package).unwrap();
		let external = scratch.path().join("external");
		fs::write(&external, b"external bytes").unwrap();
		for name in [FILE_NAME, "entry.dll"] {
			let path = package.join(name);
			fs::write(&path, b"owned bytes").unwrap();
			assert!(ordinary_metadata(&path).unwrap().is_file());
			fs::remove_file(&path).unwrap();
			#[cfg(unix)]
			std::os::unix::fs::symlink(&external, &path).unwrap();
			#[cfg(windows)]
			std::os::windows::fs::symlink_file(&external, &path).unwrap();
			assert!(read_package_file(&path).is_err(), "followed swapped {name}");
			assert_eq!(fs::read(&external).unwrap(), b"external bytes");
			fs::remove_file(&path).unwrap();
			fs::write(&path, b"owned bytes").unwrap();
			assert_eq!(read_package_file(&path).unwrap(), b"owned bytes");
			fs::remove_file(&path).unwrap();
			fs::hard_link(&external, &path).unwrap();
			assert_eq!(read_package_file(&path).unwrap(), b"external bytes");
			assert_eq!(fs::read(&external).unwrap(), b"external bytes");
		}
	}

	#[cfg(any(unix, windows))]
	#[test]
	fn copying_and_replacing_outputs_preserve_external_link_targets() {
		let scratch = tempfile::tempdir().unwrap();
		let source = scratch.path().join("source");
		let output = scratch.path().join("output");
		let external = scratch.path().join("external");
		fs::write(&external, b"external bytes").unwrap();
		fs::write(&source, b"owned bytes").unwrap();
		assert!(ordinary_metadata(&source).unwrap().is_file());
		fs::remove_file(&source).unwrap();
		#[cfg(unix)]
		std::os::unix::fs::symlink(&external, &source).unwrap();
		#[cfg(windows)]
		std::os::windows::fs::symlink_file(&external, &source).unwrap();
		assert!(copy_package_file(&source, &output).is_err());
		assert!(!output.exists());
		fs::remove_file(&source).unwrap();
		fs::hard_link(&external, &source).unwrap();
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			fs::set_permissions(&external, fs::Permissions::from_mode(0o755)).unwrap();
		}
		copy_package_file(&source, &output).unwrap();
		assert_eq!(fs::read(&output).unwrap(), b"external bytes");
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			assert_eq!(
				fs::metadata(&output).unwrap().permissions().mode() & 0o777,
				0o755
			);
		}
		fs::remove_file(&output).unwrap();
		fs::hard_link(&external, &output).unwrap();
		assert_eq!(
			copy_package_file(&source, &output).unwrap_err().kind(),
			io::ErrorKind::AlreadyExists
		);
		assert_eq!(
			create_package_file(&output, b"new bytes")
				.unwrap_err()
				.kind(),
			io::ErrorKind::AlreadyExists
		);
		replace_package_file(&output, b"new bytes").unwrap();
		assert_eq!(fs::read(&output).unwrap(), b"new bytes");
		assert_eq!(fs::read(&external).unwrap(), b"external bytes");
		fs::remove_file(&output).unwrap();
		#[cfg(unix)]
		std::os::unix::fs::symlink(&external, &output).unwrap();
		#[cfg(windows)]
		std::os::windows::fs::symlink_file(&external, &output).unwrap();
		assert_eq!(
			create_package_file(&output, b"new bytes")
				.unwrap_err()
				.kind(),
			io::ErrorKind::AlreadyExists
		);
		replace_package_file(&output, b"replaced bytes").unwrap();
		assert!(
			!fs::symlink_metadata(&output)
				.unwrap()
				.file_type()
				.is_symlink()
		);
		assert_eq!(fs::read(&output).unwrap(), b"replaced bytes");
		assert_eq!(fs::read(&external).unwrap(), b"external bytes");
		let blocked = scratch.path().join("directory");
		fs::create_dir(&blocked).unwrap();
		assert!(replace_package_file(&blocked, b"new bytes").is_err());
		assert!(fs::read_dir(scratch.path()).unwrap().all(|entry| {
			!entry
				.unwrap()
				.file_name()
				.to_string_lossy()
				.starts_with(".foch-output-")
		}));
	}

	#[cfg(windows)]
	#[test]
	fn replacing_short_filenames_near_the_windows_path_limit_uses_extended_paths() {
		use std::os::windows::ffi::OsStrExt;
		let scratch = tempfile::tempdir().unwrap();
		let root = PathBuf::from(super::super::deployment::path_text(scratch.path()).unwrap());
		let padding = 243 - root.as_os_str().encode_wide().count() - 1;
		let parent = root.join("a".repeat(padding));
		fs::create_dir(&parent).unwrap();
		let output = parent.join("x.ini");
		assert_eq!(output.as_os_str().encode_wide().count(), 249);
		create_package_file(&output, b"original bytes").unwrap();
		for bytes in [b"first replacement".as_slice(), b"second replacement"] {
			replace_package_file(&output, bytes).unwrap();
			assert_eq!(fs::read(&output).unwrap(), bytes);
			assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
		}
		let blocked = parent.join("folder");
		fs::create_dir(&blocked).unwrap();
		assert!(replace_package_file(&blocked, b"blocked bytes").is_err());
		assert_eq!(fs::read_dir(&parent).unwrap().count(), 2);
		assert_eq!(fs::read(&output).unwrap(), b"second replacement");
	}

	#[test]
	fn staged_package_entries_never_truncate_existing_files() {
		let scratch = tempfile::tempdir().unwrap();
		let root = scratch.path().join("staging");
		fs::create_dir(&root).unwrap();
		let external = scratch.path().join("external.dll");
		fs::write(&external, b"external bytes").unwrap();
		fs::hard_link(&external, root.join("sample.dll")).unwrap();
		let validated = validate(package(fake_dll())).unwrap();
		let result = write_entries(&root, &validated);
		assert_eq!(fs::read(&external).unwrap(), b"external bytes");
		assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
		let fresh = scratch.path().join("fresh");
		fs::create_dir(&fresh).unwrap();
		write_entries(&fresh, &validated).unwrap();
		assert_eq!(fs::read(fresh.join("sample.dll")).unwrap(), fake_dll());
	}

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
	fn manifest_and_archive_paths_use_the_same_separator_normalization() {
		for archive_path in ["plugins/sample.dll", "plugins\\sample.dll"] {
			for declared_path in ["plugins/sample.dll", "plugins\\sample.dll"] {
				let mut entries = package(fake_dll());
				let mut manifest =
					Manifest::parse(std::str::from_utf8(&entries[0].data).unwrap()).unwrap();
				manifest.entry.path = declared_path.into();
				manifest.files[0].path = declared_path.into();
				entries[0].data = toml::to_string(&manifest).unwrap().into_bytes();
				entries[1].path = archive_path.into();
				let validated = validate(entries).unwrap();
				assert_eq!(validated.manifest.entry.path, "plugins/sample.dll");
				assert_eq!(validated.manifest.files[0].path, "plugins/sample.dll");
				let root = tempfile::tempdir().unwrap();
				install(root.path(), &validated).unwrap();
				let (installed, errors) = installed_versions(root.path());
				assert!(errors.is_empty(), "{errors:?}");
				assert_eq!(installed.len(), 1);
				assert_eq!(installed[0].manifest.entry.path, "plugins/sample.dll");
				assert_eq!(installed[0].manifest.files[0].path, "plugins/sample.dll");
				assert!(installed[0].dir.join("plugins/sample.dll").is_file());
			}
		}
	}

	#[test]
	fn catalog_reports_all_ambiguous_artifacts_and_refuses_default_fallbacks() {
		let root = tempfile::tempdir().unwrap();
		let mut directories = Vec::new();
		let fallback = validate(package(fake_dll())).unwrap().manifest;
		for value in 0..3 {
			let mut entries = package(fake_dll());
			entries.push(ArchiveEntry {
				path: "resource.bin".into(),
				data: vec![value],
			});
			let validated = validate(entries).unwrap();
			install(root.path(), &validated).unwrap();
			directories.push(version_dir(root.path(), &validated).canonicalize().unwrap());
		}
		let mut entries = package(fake_dll());
		let mut newer = fallback.clone();
		newer.plugin.version = semver::Version::new(0, 2, 0);
		entries[0].data = toml::to_string(&newer).unwrap().into_bytes();
		install(root.path(), &validate(entries).unwrap()).unwrap();
		let mut builtin = fallback.clone();
		builtin.plugin.id = "dev.foch.builtin".into();
		let (catalog, problems) = catalog_with_defaults(root.path(), [fallback, builtin]);
		assert_eq!(problems.len(), 1, "{problems:?}");
		for directory in directories {
			assert!(
				problems[0].contains(&directory.display().to_string()),
				"{problems:?}"
			);
		}
		assert_eq!(catalog["dev.foch.sample"].len(), 1);
		assert!(catalog["dev.foch.sample"].contains_key(&newer.plugin.version));
		assert!(catalog.contains_key("dev.foch.builtin"));
		let (plain, problems) = self::catalog(root.path());
		assert_eq!(problems.len(), 1);
		assert_eq!(plain["dev.foch.sample"].len(), 1);
	}

	#[test]
	fn accepts_a_well_formed_package() {
		let validated = validate(package(fake_dll())).unwrap();
		assert_eq!(validated.manifest.plugin.id, "dev.foch.sample");
		assert!(validated.version_dir_name().starts_with("0.1.0+"));
		assert_eq!(validated.digest.len(), 64);
	}

	#[test]
	fn ini_adapters_cannot_target_entry_or_auxiliary_dlls() {
		let base = validate(package(fake_dll())).unwrap();
		for (destination, rejected) in [
			("sample.dll", true),
			("SAMPLE.DLL", true),
			("private/helper.dll", true),
			("PRIVATE\\HELPER.DLL", true),
			("resources/unlisted.dll", true),
			("resources/new.DLL", true),
			("private/config.ini", false),
			("settings.cfg", false),
		] {
			let mut manifest = base.manifest.clone();
			manifest.files.push(super::super::manifest::FileEntry {
				path: "private/helper.dll".into(),
				sha256: sha256_hex(&fake_dll()),
			});
			manifest.config.insert(
				"enabled".into(),
				super::super::manifest::ConfigField::Bool {
					default: true,
					description: None,
					adapter: Some(super::super::manifest::ConfigAdapter {
						ini: destination.into(),
						section: "settings".into(),
						key: "enabled".into(),
					}),
				},
			);
			let mut entries = base.entries().to_vec();
			for path in ["private/helper.dll", "resources/unlisted.dll"] {
				entries.push(ArchiveEntry {
					path: path.into(),
					data: fake_dll(),
				});
			}
			entries[0].data = toml::to_string(&manifest).unwrap().into_bytes();
			let imported = validate(entries.clone());
			let adapted = adapt(manifest, entries);
			for result in [imported, adapted] {
				if rejected {
					let errors = result.expect_err(destination);
					assert!(
						errors.iter().any(|error| matches!(
							error, ImportError::Manifest(message) if message.contains("DLL")
						)),
						"{destination}: {errors:?}"
					);
				} else {
					let validated = result.unwrap();
					assert_eq!(
						validated
							.entries()
							.iter()
							.find(|entry| entry.path == "sample.dll")
							.unwrap()
							.data,
						fake_dll()
					);
				}
			}
		}
	}

	#[test]
	fn proxy_loaders_are_filtered_or_rejected_at_every_package_depth() {
		for path in ["VERSION.dll", "plugins/version.dll", "plugins\\DINPUT8.dll"] {
			assert!(is_proxy_name(path), "{path}");
			let mut entries = package(fake_dll());
			let manifest = Manifest::parse(std::str::from_utf8(&entries[0].data).unwrap()).unwrap();
			entries.push(ArchiveEntry {
				path: path.into(),
				data: fake_dll(),
			});
			assert!(validate(entries.clone()).is_err(), "{path}");
			let adapted = adapt(manifest, entries).unwrap();
			assert!(
				adapted
					.entries()
					.iter()
					.all(|entry| !is_proxy_name(&entry.path))
			);
		}
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
