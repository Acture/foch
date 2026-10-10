//! Where plugin data lives on disk.
//!
//! The version store is durable data under the Foch data root (so it moves
//! with `FOCH_DATA_DIR` like the base snapshot); the per-playset selections are
//! user configuration under the Foch config directory.

use crate::game::eu4::base::snapshot::data_root;
use crate::input::config::get_config_dir_path;
use std::path::PathBuf;

/// The immutable version store: `<data root>/plugins/store`.
pub fn store_root() -> PathBuf {
	data_root().join("plugins").join("store")
}

/// The per-playset selection file: `<config dir>/plugins/selections.toml`.
pub fn selections_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
	Ok(get_config_dir_path()?
		.join("plugins")
		.join("selections.toml"))
}
