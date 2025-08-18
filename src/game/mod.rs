mod eu4;

use crate::filesystem::{FS};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;
use std::path::PathBuf;
use typed_builder::TypedBuilder;

pub trait GameType: Debug + Clone + PartialEq + Eq + Serialize + 'static {
	const NAME: &'static str;
}

#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder, Serialize, Deserialize)]
pub struct GameEntry {
	#[builder(default, setter(into))]
	pub name: String,
	#[builder(default, setter(into))]
	pub path: PathBuf,
}

#[derive(Debug, TypedBuilder)]
pub struct Game<K: GameType> {
	pub name: String,
	pub fs: FS,
	_kind: PhantomData<K>,
}


fn normalize_game_name(input: &str) -> String {
	match input.to_ascii_lowercase().as_str() {
		"eu4" => "Europa Universalis IV",
		"ck3" => "Crusader Kings III",
		other => other,
	}
	.to_string()
}
impl<K: GameType> From<GameEntry> for Game<K> {
	fn from(entry: GameEntry) -> Self {
		let name = normalize_game_name(&entry.name);
		assert_eq!(name, K::NAME, "GameEntry name must match GameType name");
		let fs = FS::builder()
			.root(entry.path) // 使用 GameEntry 的 path
			.build();
		Game {
			name,
			fs,
			_kind: PhantomData,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_game_entry() {
		let entry = GameEntry {
			name: "Test Game".into(),
			path: Default::default(),
		};
		println!("{entry:?}");
	}
	#[test]
	fn test_game_fs() {
		let fs = FS::builder().root("/path/to/game").build();
		println!("{fs:?}");
	}

	#[test]
	fn test_game() {
		let entry = GameEntry {
			name: "eu4".into(),
			path: Default::default(),
		};
		let game: Game<eu4::EU4> = Game::from(entry);
		println!("{game:?}");
	}
}
