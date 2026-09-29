//! The files a CWT `type` or `complex_enum` declares it reads.
//!
//! A rule names a directory with `path = "game/<directory>"` and may narrow it
//! to one file with `path_file = "<file>"`. The text is parsed once, when the
//! schema is compiled; matching a file then compares game path components and
//! never re-reads rule text. Names compare ignoring ASCII case, as CWTools
//! matches them.

use crate::model::{GamePath, GamePathBuf, GamePathError};
use serde::{Deserialize, Serialize};

/// The directory a rule's `path` names, relative to the game root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SchemaDirectory {
	/// The game root itself (`path = "game"`). Every file lies below it, so
	/// as a directory to match against it matches nothing.
	GameRoot,
	Directory(GamePathBuf),
}

impl SchemaDirectory {
	/// Parses a rule path. CWTools spells the game root `game`, so exactly one
	/// leading `game/` is removed and the rest must be a game path; text
	/// without that prefix is read as a game path, as user schemas write it.
	pub fn parse(text: &str) -> Result<Self, GamePathError> {
		if matches!(text, "game" | "game/") {
			return Ok(Self::GameRoot);
		}
		let relative = text.strip_prefix("game/").unwrap_or(text);
		GamePathBuf::parse(relative).map(Self::Directory)
	}

	/// The directory's game path, or `None` for the game root.
	pub fn as_game_path(&self) -> Option<&GamePath> {
		match self {
			Self::GameRoot => None,
			Self::Directory(path) => Some(path),
		}
	}

	/// How specifically this rule matches `file`, counted in components, or
	/// `None` when it does not. Without `path_file` the rule matches the
	/// directory and everything below it; with one it matches exactly the file
	/// `<directory>/<path_file>`. The game root matches nothing in either
	/// form. A more specific rule matches more components.
	pub fn match_depth(&self, path_file: Option<&GamePath>, file: &GamePath) -> Option<usize> {
		let Self::Directory(directory) = self else {
			return None;
		};
		let mut components = file.iter();
		let mut depth = 0;
		for name in directory
			.iter()
			.chain(path_file.into_iter().flat_map(|path| path.iter()))
		{
			if !components
				.next()
				.is_some_and(|component| component.eq_ignore_ascii_case(name))
			{
				return None;
			}
			depth += 1;
		}
		(path_file.is_none() || components.next().is_none()).then_some(depth)
	}
}

#[cfg(test)]
mod tests {
	use super::SchemaDirectory;
	use crate::model::{GamePath, GamePathErrorKind};

	fn game_path(text: &str) -> &GamePath {
		GamePath::new(text).expect("valid game path")
	}

	fn directory(text: &str) -> SchemaDirectory {
		SchemaDirectory::parse(text).expect("valid rule path")
	}

	#[test]
	fn exactly_one_game_prefix_is_removed() {
		assert_eq!(
			directory("game/common/ideas").as_game_path(),
			Some(game_path("common/ideas"))
		);
		assert_eq!(
			directory("events").as_game_path(),
			Some(game_path("events"))
		);
		assert_eq!(
			directory("game/game/events").as_game_path(),
			Some(game_path("game/events"))
		);
		assert_eq!(directory("game"), SchemaDirectory::GameRoot);
		assert_eq!(directory("game/"), SchemaDirectory::GameRoot);
	}

	#[test]
	fn rule_paths_that_are_not_game_paths_are_rejected() {
		for (text, kind) in [
			(
				r"game/common\ideas",
				GamePathErrorKind::ReservedCharacter {
					component: r"common\ideas".to_string(),
					character: '\\',
				},
			),
			("game/common/../events", GamePathErrorKind::ParentComponent),
			("game/common/", GamePathErrorKind::EmptyComponent),
			("/events", GamePathErrorKind::NotRelative),
			("", GamePathErrorKind::Empty),
		] {
			assert_eq!(
				SchemaDirectory::parse(text).expect_err(text).kind,
				kind,
				"{text}"
			);
		}
	}

	#[test]
	fn a_directory_matches_itself_and_below_on_component_boundaries() {
		let common = directory("game/common");
		assert_eq!(common.match_depth(None, game_path("common/foo")), Some(1));
		assert_eq!(common.match_depth(None, game_path("common")), Some(1));
		assert_eq!(common.match_depth(None, game_path("commonplace/foo")), None);
		assert_eq!(
			directory("game/common/ideas").match_depth(None, game_path("common/ideas/x.txt")),
			Some(2)
		);
	}

	#[test]
	fn names_match_ignoring_ascii_case() {
		let ideas = directory("game/common/ideas");
		assert_eq!(
			ideas.match_depth(None, game_path("Common/IDEAS/00_Ideas.txt")),
			Some(2)
		);
		assert_eq!(
			directory("game/Common/Ideas").match_depth(None, game_path("common/ideas/x.txt")),
			Some(2)
		);
		assert_eq!(
			ideas.match_depth(
				Some(game_path("Achievements.TXT")),
				game_path("common/ideas/achievements.txt")
			),
			Some(3)
		);
	}

	#[test]
	fn the_game_root_matches_no_file() {
		let root = SchemaDirectory::GameRoot;
		for file in ["events/a.txt", "a.txt", "common/x/y.txt"] {
			assert_eq!(root.match_depth(None, game_path(file)), None, "{file}");
			assert_eq!(
				root.match_depth(Some(game_path("a.txt")), game_path(file)),
				None,
				"{file}"
			);
		}
	}

	#[test]
	fn path_file_matches_only_that_direct_child() {
		let common = directory("game/common");
		let file = Some(game_path("achievements.txt"));
		assert_eq!(
			common.match_depth(file, game_path("common/achievements.txt")),
			Some(2)
		);
		for other in [
			"common/sub/achievements.txt",
			"common/achievements.txt.bak",
			"common/other.txt",
			"common",
			"achievements.txt",
		] {
			assert_eq!(common.match_depth(file, game_path(other)), None, "{other}");
		}
	}
}
