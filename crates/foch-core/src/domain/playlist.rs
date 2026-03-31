use crate::domain::ParseError;
use crate::domain::game::Game;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Playlist {
	#[serde(default)]
	pub game: Game,
	#[serde(default)]
	pub name: String,
	#[serde(default)]
	pub mods: Vec<PlaylistEntry>,
}

impl Default for Playlist {
	fn default() -> Self {
		Self {
			game: Game::Unknown,
			name: String::new(),
			mods: Vec::new(),
		}
	}
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PlaylistEntry {
	#[serde(rename = "displayName", default)]
	pub display_name: Option<String>,
	#[serde(default)]
	pub enabled: bool,
	#[serde(default)]
	pub position: Option<usize>,
	#[serde(rename = "steamId", default)]
	pub steam_id: Option<String>,
}

pub fn load_playlist(path: &Path) -> Result<Playlist, ParseError> {
	let content =
		std::fs::read_to_string(path).map_err(|err| ParseError::io(path.to_path_buf(), err))?;
	serde_json::from_str::<Playlist>(&content)
		.map_err(|err| ParseError::format(path.to_path_buf(), err.to_string()))
}

#[cfg(test)]
mod tests {
	use super::load_playlist;
	use crate::domain::game::Game;
	use std::path::Path;

	#[test]
	fn parses_playlist_from_corpus() {
		let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
		let test_file = manifest_dir
			.join("..")
			.join("..")
			.join("tests")
			.join("corpus")
			.join("playlist.json");
		let playlist = load_playlist(&test_file).expect("failed to parse test playlist");
		assert_eq!(playlist.game, Game::EuropaUniversalis4);
		assert_eq!(playlist.name, "Initial playset");
		assert_eq!(playlist.mods.len(), 26);
	}
}
