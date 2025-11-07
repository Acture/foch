use jomini::JominiDeserialize;
use serde::{Deserialize, Serialize};
use crate::game::Game;

#[derive(Serialize, Deserialize, Debug)]
struct PlayList {
	game: Game,
	name: String,
	#[serde(default)]
	mods: Vec<PlayListEntry>,
}

#[derive(Serialize, Deserialize, Debug)]
struct PlayListEntry {
	#[serde(rename = "displayName")]
	display_name: String,
	enabled: bool,
	position: usize,
	#[serde(rename = "steamId")]
	steam_id: String,
}


#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;

	#[test]
	fn test_parse_playlist() {
		let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
		let corpus_dir = manifest_dir.join("tests").join("corpus");
		let test_file = corpus_dir.join("playlist.json");
		println!("{:#?}", test_file);
		let data = std::fs::read_to_string(test_file).unwrap();
		let playlist: PlayList = serde_json::from_str(&data).unwrap();
		println!("{:#?}", playlist);
		assert_eq!(playlist.game, Game::EuropaUniversalis4);
		assert_eq!(playlist.name, "Initial playset");
		assert_eq!(playlist.mods.len(), 26);
	}
}
