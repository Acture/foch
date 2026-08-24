use semver::Version;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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

pub fn is_cache_version_namespace(name: &str) -> bool {
	name.strip_prefix('v').is_some_and(|version| {
		Version::parse(version).is_ok() || is_legacy_integer_version(version)
	})
}

fn is_legacy_integer_version(version: &str) -> bool {
	!version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
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
	use super::{
		cache_version_namespace, is_cache_version_namespace, repo_fallback_cache_root_dir,
	};

	#[test]
	fn cache_namespaces_require_semver_and_recognize_old_integer_generations() {
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
		assert!(is_cache_version_namespace("v10.2.3"));
		assert!(is_cache_version_namespace("v9"));
		assert!(!is_cache_version_namespace("metadata"));
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
}
