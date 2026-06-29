//! Shared configuration helpers: app id, default paths, tool provenance.

use std::path::PathBuf;
use std::process::Command;

/// Europa Universalis IV Steam application id.
pub const EU4_APPID: u32 = 236850;

/// Best-guess EU4 Steam Workshop content dir per platform.
/// Override with the `STEAM_WORKSHOP_DIR` environment variable.
pub fn default_workshop_dir() -> PathBuf {
	if let Ok(dir) = std::env::var("STEAM_WORKSHOP_DIR") {
		return PathBuf::from(dir);
	}
	let home = std::env::var("HOME").unwrap_or_default();
	let suffix = format!("steamapps/workshop/content/{EU4_APPID}");
	if cfg!(target_os = "macos") {
		PathBuf::from(home)
			.join("Library/Application Support/Steam")
			.join(suffix)
	} else if cfg!(target_os = "windows") {
		PathBuf::from(r"G:\SteamLibrary").join(suffix.replace('/', r"\"))
	} else {
		PathBuf::from(home).join(".steam/steam").join(suffix)
	}
}

/// Short git SHA of the current checkout, recorded into `corpus.json` for
/// provenance. `None` if git is unavailable or this isn't a repo.
pub fn tool_commit() -> Option<String> {
	let out = Command::new("git")
		.args(["rev-parse", "--short", "HEAD"])
		.output()
		.ok()?;
	if !out.status.success() {
		return None;
	}
	let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
	if sha.is_empty() { None } else { Some(sha) }
}
