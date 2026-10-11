//! Where plugin data lives on disk.
//!
//! The version store is durable data under the Foch data root (so it moves
//! with `FOCH_DATA_DIR` like the base snapshot); the per-playset selections are
//! user configuration under the Foch config directory.

use crate::game::eu4::base::snapshot::data_root;
use crate::input::config::get_config_dir_path;
use std::{
	fs, io,
	path::{Path, PathBuf},
};

/// Prepare the version store below the explicitly selected data root.
pub fn prepare_store_root(data_root: &Path) -> io::Result<PathBuf> {
	super::store::prepare_data_directory(data_root, "plugins/store")
}

/// Read the derived store without creating its directories.
pub fn checked_store_root(data_root: &Path) -> io::Result<PathBuf> {
	let root = match data_root.canonicalize() {
		Ok(root) => root,
		Err(error) if error.kind() == io::ErrorKind::NotFound => {
			return Ok(data_root.join("plugins/store"));
		}
		Err(error) => return Err(error),
	};
	if !root.is_dir() {
		return Err(io::Error::other("plugin data root is not a directory"));
	}
	let mut directory = root.clone();
	for component in ["plugins", "store"] {
		directory.push(component);
		match fs::symlink_metadata(&directory) {
			Ok(_) => directory = super::store::checked_directory(&directory, &root)?,
			Err(error) if error.kind() == io::ErrorKind::NotFound => {
				return Ok(root.join("plugins/store"));
			}
			Err(error) => return Err(error),
		}
	}
	Ok(directory)
}

/// The store location; use [`checked_store_root`] before reading its contents.
pub fn store_root() -> PathBuf {
	data_root().join("plugins").join("store")
}

/// The per-playset selection file: `<config dir>/plugins/selections.toml`.
pub fn selections_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
	Ok(get_config_dir_path()?
		.join("plugins")
		.join("selections.toml"))
}
