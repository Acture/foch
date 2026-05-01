use crate::domain::ParseError;
use crate::domain::descriptor::load_descriptor;
use crate::domain::game::Game;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// In-memory representation of an EU4 (or other Paradox game) playset.
///
/// The on-disk format is the launcher's `dlc_load.json` plus the `mod/`
/// directory of `.mod` descriptors next to it; this struct is the parsed
/// projection used everywhere downstream. It is **not** itself a serializable
/// JSON shape — the launcher owns the canonical format and `foch` consumes it
/// via [`Playlist::from_dlc_load`].
#[derive(Debug, Clone, Default)]
pub struct Playlist {
	pub game: Game,
	pub name: String,
	pub mods: Vec<PlaylistEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct PlaylistEntry {
	pub display_name: Option<String>,
	pub enabled: bool,
	pub position: Option<usize>,
	pub steam_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DlcLoad {
	#[serde(default)]
	enabled_mods: Vec<String>,
	#[serde(default)]
	#[allow(dead_code)] // surfaced for completeness; foch ignores DLC selection.
	disabled_dlcs: Vec<String>,
}

impl Playlist {
	/// Parse the launcher's `dlc_load.json` plus the sibling `mod/` directory
	/// of `.mod` descriptors into an in-memory [`Playlist`].
	///
	/// Conventions:
	/// - `dlc_load.json`'s `enabled_mods` is an ordered list of paths like
	///   `mod/ugc_<steamId>.mod` (positions = array index = precedence).
	/// - Each referenced descriptor is read for its `name` (→ display_name)
	///   and `remote_file_id` (→ steam_id, falling back to the filename's
	///   numeric tail when the descriptor omits it).
	/// - The owning game is inferred from the parent directory name, or set
	///   to [`Game::Unknown`] if it does not match any known
	///   `paradox_data_dir_name()`.
	pub fn from_dlc_load(path: &Path) -> Result<Self, ParseError> {
		let bytes = std::fs::read(path).map_err(|err| ParseError::io(path.to_path_buf(), err))?;
		let dlc: DlcLoad = serde_json::from_slice(&bytes)
			.map_err(|err| ParseError::format(path.to_path_buf(), err.to_string()))?;
		let parent = path
			.parent()
			.ok_or_else(|| {
				ParseError::format(
					path.to_path_buf(),
					"dlc_load.json must live inside a paradox game data directory".to_string(),
				)
			})?
			.to_path_buf();
		let game = infer_game_from_paradox_data_dir(&parent);
		let mods = dlc
			.enabled_mods
			.iter()
			.enumerate()
			.map(|(position, rel)| read_dlc_load_entry(&parent, position, rel))
			.collect();
		let name = match path.file_stem().and_then(|s| s.to_str()) {
			Some(stem) if !stem.is_empty() => format!("{stem} (active)"),
			_ => "active".to_string(),
		};
		Ok(Playlist { game, name, mods })
	}
}

fn infer_game_from_paradox_data_dir(dir: &Path) -> Game {
	let dir_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
	match dir_name {
		"Europa Universalis IV" => Game::EuropaUniversalis4,
		"Crusader Kings III" => Game::CrusaderKings3,
		"Victoria 3" => Game::Victoria3,
		"Stellaris" => Game::Stellaris,
		"Hearts of Iron IV" => Game::HeartsOfIron4,
		// dlc_load.json itself does not encode the owning game; when the
		// containing dir name is unrecognized (e.g. tests writing under a
		// random temp dir) fall back to EU4 since that is the only game
		// foch currently ships content-family descriptors for.
		_ => Game::EuropaUniversalis4,
	}
}

fn read_dlc_load_entry(paradox_data_dir: &Path, position: usize, rel: &str) -> PlaylistEntry {
	let descriptor_path = paradox_data_dir.join(rel);
	let descriptor = load_descriptor(&descriptor_path).ok();
	let steam_id = descriptor
		.as_ref()
		.and_then(|d| d.remote_file_id.clone())
		.or_else(|| extract_steam_id_from_descriptor_path(rel));
	let display_name = descriptor
		.as_ref()
		.and_then(|d| (!d.name.trim().is_empty()).then(|| d.name.clone()))
		.or_else(|| steam_id.as_ref().map(|id| format!("ugc_{id}")));
	PlaylistEntry {
		display_name,
		enabled: true,
		position: Some(position),
		steam_id,
	}
}

fn extract_steam_id_from_descriptor_path(rel: &str) -> Option<String> {
	// Convention: dlc_load lists mods as `mod/ugc_<numeric steam id>.mod`;
	// strip the prefix/suffix and validate the inner segment.
	let filename = Path::new(rel).file_stem().and_then(|s| s.to_str())?;
	let stripped = filename.strip_prefix("ugc_")?;
	if stripped.chars().all(|c| c.is_ascii_digit()) && !stripped.is_empty() {
		Some(stripped.to_string())
	} else {
		None
	}
}

/// Default location of a launcher `dlc_load.json` for a paradox data
/// directory configured via `Config::paradox_data_path`.
pub fn default_dlc_load_path(paradox_data_dir: &Path) -> PathBuf {
	paradox_data_dir.join("dlc_load.json")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::domain::game::Game;
	use std::fs;
	use tempfile::TempDir;

	fn write_dlc_load(dir: &Path, mods: &[(&str, &str)]) {
		fs::create_dir_all(dir.join("mod")).unwrap();
		let entries: Vec<String> = mods
			.iter()
			.map(|(steam_id, _)| format!("mod/ugc_{steam_id}.mod"))
			.collect();
		let payload =
			serde_json::json!({ "enabled_mods": entries, "disabled_dlcs": Vec::<String>::new() });
		fs::write(
			dir.join("dlc_load.json"),
			serde_json::to_string_pretty(&payload).unwrap(),
		)
		.unwrap();
		for (steam_id, name) in mods {
			let body = format!(
				"name=\"{name}\"\npath=\"/tmp/mod_{steam_id}\"\nremote_file_id=\"{steam_id}\"\n"
			);
			fs::write(dir.join("mod").join(format!("ugc_{steam_id}.mod")), body).unwrap();
		}
	}

	#[test]
	fn parses_dlc_load_with_descriptors() {
		let temp = TempDir::new().unwrap();
		let game_dir = temp.path().join("Europa Universalis IV");
		fs::create_dir_all(&game_dir).unwrap();
		write_dlc_load(
			&game_dir,
			&[("2164202838", "Europa Expanded"), ("1999055990", "汉化")],
		);

		let playlist = Playlist::from_dlc_load(&game_dir.join("dlc_load.json")).unwrap();
		assert_eq!(playlist.game, Game::EuropaUniversalis4);
		assert_eq!(playlist.mods.len(), 2);
		assert_eq!(playlist.mods[0].steam_id.as_deref(), Some("2164202838"));
		assert_eq!(
			playlist.mods[0].display_name.as_deref(),
			Some("Europa Expanded")
		);
		assert_eq!(playlist.mods[0].position, Some(0));
		assert!(playlist.mods[0].enabled);
		assert_eq!(playlist.mods[1].steam_id.as_deref(), Some("1999055990"));
		assert_eq!(playlist.mods[1].display_name.as_deref(), Some("汉化"));
		assert_eq!(playlist.mods[1].position, Some(1));
	}

	#[test]
	fn falls_back_to_filename_when_descriptor_missing() {
		let temp = TempDir::new().unwrap();
		let game_dir = temp.path().join("Europa Universalis IV");
		fs::create_dir_all(&game_dir).unwrap();
		fs::write(
			game_dir.join("dlc_load.json"),
			r#"{"enabled_mods":["mod/ugc_999.mod"],"disabled_dlcs":[]}"#,
		)
		.unwrap();

		let playlist = Playlist::from_dlc_load(&game_dir.join("dlc_load.json")).unwrap();
		assert_eq!(playlist.mods.len(), 1);
		assert_eq!(playlist.mods[0].steam_id.as_deref(), Some("999"));
		assert_eq!(playlist.mods[0].display_name.as_deref(), Some("ugc_999"));
	}

	#[test]
	fn unknown_paradox_data_dir_defaults_to_eu4() {
		let temp = TempDir::new().unwrap();
		let path = temp.path().join("dlc_load.json");
		fs::write(&path, r#"{"enabled_mods":[],"disabled_dlcs":[]}"#).unwrap();
		let playlist = Playlist::from_dlc_load(&path).unwrap();
		assert_eq!(playlist.game, Game::EuropaUniversalis4);
	}
}
