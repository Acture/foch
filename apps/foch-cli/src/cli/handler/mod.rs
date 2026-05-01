pub mod cache;
pub mod check;
pub mod config;
pub mod data;
pub mod graph;
pub mod merge;
pub mod merge_plan;
pub mod simplify;

pub type HandlerResult = Result<i32, Box<dyn std::error::Error>>;

use foch_engine::Config;
use std::path::{Path, PathBuf};

/// Resolve the `<PLAYSET_PATH>` argument shared by `check`, `merge-plan`,
/// `merge`, `graph`, and `simplify`. When the user supplies an explicit path
/// it is used verbatim; otherwise `<paradox_data_path>/dlc_load.json` (the
/// launcher's currently-active playset) is the implicit default. Returns a
/// human-readable error string when neither source produces a usable path.
pub fn resolve_playset_path(explicit: Option<&Path>, config: &Config) -> Result<PathBuf, String> {
	if let Some(path) = explicit {
		return Ok(path.to_path_buf());
	}
	let paradox = config.paradox_data_path.as_ref().ok_or_else(|| {
		"no <PLAYSET_PATH> given and `paradox_data_path` is not configured; either pass a path \
		 explicitly or run `foch config set --paradox-data-path <dir>` so foch can default to the \
		 launcher's `dlc_load.json` in that directory"
			.to_string()
	})?;
	let candidate = paradox.join("dlc_load.json");
	if !candidate.is_file() {
		return Err(format!(
			"no <PLAYSET_PATH> given and `{}` does not exist; pass an explicit playset path or \
			 launch the Paradox launcher once to write `dlc_load.json`",
			candidate.display()
		));
	}
	Ok(candidate)
}
