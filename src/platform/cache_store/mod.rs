use semver::Version;
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
	let extension: String = path
		.extension()
		.map(|extension| extension.to_string_lossy().into_owned())
		.unwrap_or_default();
	let (temporary, mut file) = loop {
		let sequence: u64 = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
		let temporary: PathBuf =
			path.with_extension(format!("{extension}.{}.{sequence}.tmp", std::process::id()));
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
	if fs::create_dir_all(path).is_err() {
		return false;
	}
	let probe = path.join(".foch-write-test");
	match fs::write(&probe, b"") {
		Ok(()) => {
			let _ = fs::remove_file(probe);
			true
		}
		Err(_) => false,
	}
}

fn repo_fallback_cache_root_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("foch-cache")
}

#[cfg(test)]
mod tests {
	use super::{cache_version_namespace, repo_fallback_cache_root_dir};

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
