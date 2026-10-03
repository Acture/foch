use semver::Version;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) mod generation;
mod layer;

pub use layer::{CacheLayerEntryInfo, EvictionStats, FileCacheLayer};

#[derive(Debug)]
pub enum CacheError {
	Io(io::Error),
	Encode(String),
}

impl CacheError {
	pub(crate) fn encode(error: impl fmt::Display) -> Self {
		Self::Encode(error.to_string())
	}
}

impl fmt::Display for CacheError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io(error) => write!(formatter, "{error}"),
			Self::Encode(error) => write!(formatter, "{error}"),
		}
	}
}

impl std::error::Error for CacheError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::Encode(_) => None,
		}
	}
}

impl From<io::Error> for CacheError {
	fn from(error: io::Error) -> Self {
		Self::Io(error)
	}
}

const DEFAULT_CACHE_CAP_BYTES: u64 = 1 << 30;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Publish `bytes` at `path` through a temporary sibling that no other writer
/// shares, then rename it into place. Merge workers store cache entries
/// concurrently, and two writers of the same entry must not truncate or rename
/// one temporary file under each other; the last complete rename wins.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
	let (temporary, mut file) = loop {
		let sequence: u64 = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
		let temporary: PathBuf = temporary_sibling(path, sequence);
		match OpenOptions::new()
			.write(true)
			.create_new(true)
			.open(&temporary)
		{
			Ok(file) => break (temporary, file),
			Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
			Err(error) => return Err(error),
		}
	};
	let written: io::Result<()> = file.write_all(bytes).and_then(|()| {
		drop(file);
		fs::rename(&temporary, path)
	});
	if written.is_err() {
		let _ = fs::remove_file(&temporary);
	}
	written
}

/// `path` with `.<pid>.<sequence>.tmp` appended to its extension. The
/// extension is extended as it is, never re-spelled as text.
fn temporary_sibling(path: &Path, sequence: u64) -> PathBuf {
	let mut extension: OsString = path.extension().unwrap_or_default().to_os_string();
	extension.push(format!(".{}.{sequence}.tmp", std::process::id()));
	path.with_extension(extension)
}

pub fn cache_cap_bytes() -> u64 {
	std::env::var("FOCH_CACHE_MAX_BYTES")
		.ok()
		.and_then(|value| value.trim().parse().ok())
		.unwrap_or(DEFAULT_CACHE_CAP_BYTES)
}

pub const CACHE_ROOT_ENV: &str = "FOCH_CACHE_ROOT";

pub fn cache_version_namespace(version: &str) -> io::Result<String> {
	Version::parse(version).map_err(|error| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			format!("cache format version must be SemVer, got {version:?}: {error}"),
		)
	})?;
	Ok(format!("v{version}"))
}

pub fn default_foch_cache_dir() -> PathBuf {
	if let Ok(override_dir) = std::env::var(CACHE_ROOT_ENV) {
		return PathBuf::from(override_dir);
	}
	if let Some(cache_dir) = dirs::cache_dir() {
		let candidate = cache_dir.join("foch");
		if ensure_writable_dir(&candidate) {
			return candidate;
		}
	}

	repo_fallback_cache_root_dir()
}

fn ensure_writable_dir(path: &Path) -> bool {
	// A shared probe name can be unavailable while another reader creates or
	// removes it, changing the cache root despite the directory being writable.
	fs::create_dir_all(path).is_ok() && tempfile::NamedTempFile::new_in(path).is_ok()
}

fn repo_fallback_cache_root_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("foch-cache")
}

#[cfg(test)]
mod tests {
	use super::{cache_version_namespace, ensure_writable_dir, repo_fallback_cache_root_dir};

	#[test]
	fn a_probe_name_collision_does_not_hide_a_writable_directory() {
		let root: tempfile::TempDir = tempfile::tempdir().expect("cache root");
		// An existing entry can make one fixed probe name unavailable even
		// though other files can still be created in the directory.
		let existing: std::path::PathBuf = root.path().join(".foch-write-test");
		std::fs::create_dir(&existing).expect("occupy the former probe name");
		assert!(ensure_writable_dir(root.path()));
		assert!(
			existing.is_dir(),
			"the probe must preserve existing entries"
		);
	}

	#[test]
	fn concurrent_writability_checks_preserve_one_cache_directory() {
		let root: tempfile::TempDir = tempfile::tempdir().expect("cache root");
		let barrier: std::sync::Barrier = std::sync::Barrier::new(8);
		std::thread::scope(|scope| {
			for _ in 0..8 {
				let directory: &std::path::Path = root.path();
				let barrier: &std::sync::Barrier = &barrier;
				scope.spawn(move || {
					barrier.wait();
					for _ in 0..32 {
						assert!(ensure_writable_dir(directory));
					}
				});
			}
		});
		assert_eq!(
			std::fs::read_dir(root.path())
				.expect("list cache root")
				.count(),
			0,
			"writability checks must clean up their temporary files"
		);
	}

	#[test]
	fn cache_namespaces_require_semver() {
		assert_eq!(
			cache_version_namespace("10.2.3").expect("valid SemVer"),
			"v10.2.3"
		);
		assert!(cache_version_namespace("10").is_err());
		assert!(cache_version_namespace("01.2.3").is_err());
		assert!(cache_version_namespace("1.2.3_bad").is_err());
		assert_eq!(
			cache_version_namespace("1.2.3-rc.1+build.7").expect("full SemVer"),
			"v1.2.3-rc.1+build.7"
		);
	}

	#[test]
	fn repository_fallback_lives_under_the_root_package_target() {
		assert_eq!(
			repo_fallback_cache_root_dir(),
			std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
				.join("target")
				.join("foch-cache")
		);
	}

	#[test]
	fn a_temporary_sibling_extends_the_extension_it_replaces() {
		let pid: u32 = std::process::id();
		for (path, expected) in [
			("cache/entry.bin", format!("cache/entry.bin.{pid}.7.tmp")),
			("cache/entry", format!("cache/entry..{pid}.7.tmp")),
		] {
			assert_eq!(
				super::temporary_sibling(std::path::Path::new(path), 7),
				std::path::PathBuf::from(expected),
				"{path}"
			);
		}
	}

	/// In memory: some filesystems refuse names that are not UTF-8.
	#[cfg(unix)]
	#[test]
	fn a_temporary_sibling_keeps_an_extension_that_is_not_utf8() {
		use std::ffi::OsStr;
		use std::os::unix::ffi::OsStrExt;

		let path = std::path::Path::new(OsStr::from_bytes(b"cache/entry.\xff"));
		let expected = format!(".{}.7.tmp", std::process::id());
		let mut bytes: Vec<u8> = b"cache/entry.\xff".to_vec();
		bytes.extend_from_slice(expected.as_bytes());
		assert_eq!(
			super::temporary_sibling(path, 7).as_os_str(),
			OsStr::from_bytes(&bytes)
		);
	}

	#[test]
	fn concurrent_writers_of_one_entry_each_publish_a_complete_file() {
		let root: tempfile::TempDir = tempfile::tempdir().expect("cache root");
		let entry: std::path::PathBuf = root.path().join("entry.bin");
		// Each payload is recognisable in full, so a torn or mixed file fails.
		let payloads: Vec<Vec<u8>> = (0..4u8).map(|writer| vec![writer; 256 * 1024]).collect();
		std::thread::scope(|scope| {
			for payload in &payloads {
				let entry: &std::path::Path = &entry;
				scope.spawn(move || {
					for _ in 0..25 {
						super::write_atomically(entry, payload).expect("write entry");
					}
				});
			}
		});
		let published: Vec<u8> = std::fs::read(&entry).expect("read entry");
		assert!(payloads.contains(&published), "the entry mixes writers");
		let leftovers: Vec<std::ffi::OsString> = std::fs::read_dir(root.path())
			.expect("list cache root")
			.map(|item| item.expect("cache entry").file_name())
			.filter(|name| name != "entry.bin")
			.collect();
		assert!(leftovers.is_empty(), "{leftovers:?}");
	}
}
