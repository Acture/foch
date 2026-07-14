use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn generation_dir(cache_dir: &Path, cache_version: u32) -> PathBuf {
	cache_dir.join(format!("v{cache_version}"))
}

pub(super) fn prepare(cache_dir: &Path, cache_version: u32) -> io::Result<usize> {
	fs::create_dir_all(cache_dir)?;
	let active_namespace = format!("v{cache_version}");
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
			if !is_generation(&name) {
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

fn is_generation(name: &OsStr) -> bool {
	name.to_str()
		.and_then(|name| name.strip_prefix('v'))
		.is_some_and(|version| {
			!version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
		})
}
