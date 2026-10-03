use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) fn generation_dir(cache_dir: &Path, cache_version: &str) -> PathBuf {
	let namespace = crate::platform::cache_store::cache_version_namespace(cache_version)
		.expect("cache format version is valid SemVer");
	cache_dir.join(namespace)
}

pub(crate) fn ensure(cache_dir: &Path, cache_version: &str) -> io::Result<()> {
	let active_namespace = crate::platform::cache_store::cache_version_namespace(cache_version)?;
	fs::create_dir_all(cache_dir.join(active_namespace))
}
