use std::fs;
use std::path::{Path, PathBuf};

pub const CACHE_ROOT_ENV: &str = "FOCH_CACHE_ROOT";
pub const LEGACY_CACHE_ROOT_ENV: &str = "FOCH_CACHE_DIR";

pub fn default_foch_cache_dir() -> PathBuf {
	if let Ok(override_dir) = std::env::var(CACHE_ROOT_ENV) {
		return PathBuf::from(override_dir);
	}
	if let Ok(override_dir) = std::env::var(LEGACY_CACHE_ROOT_ENV) {
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
		.parent()
		.and_then(Path::parent)
		.map(Path::to_path_buf)
		.unwrap_or_else(|| PathBuf::from("."))
		.join("target")
		.join("foch-cache")
}
