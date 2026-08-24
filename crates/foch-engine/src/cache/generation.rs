use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn generation_dir(cache_dir: &Path, cache_version: &str) -> PathBuf {
	let namespace = foch::platform::cache_store::cache_version_namespace(cache_version)
		.expect("cache format version is valid SemVer");
	cache_dir.join(namespace)
}

pub(super) fn prepare(cache_dir: &Path, cache_version: &str) -> io::Result<usize> {
	fs::create_dir_all(cache_dir)?;
	let active_namespace = foch::platform::cache_store::cache_version_namespace(cache_version)?;
	fs::create_dir_all(cache_dir.join(&active_namespace))?;

	let mut removed_items = 0;
	for entry in fs::read_dir(cache_dir)? {
		let entry = entry?;
		let name = entry.file_name();
		if name == active_namespace.as_str() {
			continue;
		}

		let file_type = entry.file_type()?;
		if file_type.is_dir() {
			if !foch::platform::cache_store::is_cache_version_namespace(&name.to_string_lossy()) {
				continue;
			}
			fs::remove_dir_all(entry.path())?;
		} else {
			fs::remove_file(entry.path())?;
		}
		removed_items += 1;
	}
	Ok(removed_items)
}
