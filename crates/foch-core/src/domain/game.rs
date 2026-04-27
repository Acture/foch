use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Debug, Clone, Default, Eq, PartialEq)]
pub enum Game {
	#[serde(alias = "eu4")]
	EuropaUniversalis4,
	#[serde(alias = "ck3")]
	CrusaderKings3,
	#[serde(alias = "vic3")]
	Victoria3,
	Stellaris,
	#[serde(alias = "hoi4")]
	HeartsOfIron4,
	#[serde(other)]
	#[default]
	Unknown,
}

impl Game {
	pub fn from_key(value: &str) -> Option<Self> {
		match value.trim().to_ascii_lowercase().as_str() {
			"eu4" | "europauniversalis4" | "europa-universalis-4" => Some(Self::EuropaUniversalis4),
			"ck3" | "crusaderkings3" | "crusader-kings-3" => Some(Self::CrusaderKings3),
			"vic3" | "victoria3" | "victoria-3" => Some(Self::Victoria3),
			"stellaris" => Some(Self::Stellaris),
			"hoi4" | "heartsofiron4" | "hearts-of-iron-4" => Some(Self::HeartsOfIron4),
			_ => None,
		}
	}

	pub fn steam_app_ids(&self) -> &'static [u32] {
		match self {
			Self::EuropaUniversalis4 => &[236850],
			Self::CrusaderKings3 => &[1158310],
			Self::Victoria3 => &[529340],
			Self::Stellaris => &[281990],
			Self::HeartsOfIron4 => &[394360],
			Self::Unknown => &[],
		}
	}

	pub fn key(&self) -> &'static str {
		match self {
			Self::EuropaUniversalis4 => "eu4",
			Self::CrusaderKings3 => "ck3",
			Self::Victoria3 => "vic3",
			Self::Stellaris => "stellaris",
			Self::HeartsOfIron4 => "hoi4",
			Self::Unknown => "unknown",
		}
	}

	pub fn paradox_data_dir_name(&self) -> Option<&'static str> {
		match self {
			Self::EuropaUniversalis4 => Some("Europa Universalis IV"),
			Self::CrusaderKings3 => Some("Crusader Kings III"),
			Self::Victoria3 => Some("Victoria 3"),
			Self::Stellaris => Some("Stellaris"),
			Self::HeartsOfIron4 => Some("Hearts of Iron IV"),
			Self::Unknown => None,
		}
	}

	/// Returns the set of top-level directories the game engine actually loads
	/// from a mod root. Files outside these roots (e.g. `.git/`, `README.md`,
	/// IDE artifacts, top-level `thumbnail.png`) are not part of the runtime
	/// content namespace and therefore cannot collide across mods at load time.
	///
	/// Returns `None` for games where we have no authoritative list yet; in
	/// that case callers should treat every relative path as potentially
	/// loadable to avoid suppressing genuine conflicts.
	pub fn loadable_content_roots(&self) -> Option<&'static [&'static str]> {
		match self {
			Self::EuropaUniversalis4 => Some(&[
				"common",
				"customizable_localization",
				"decisions",
				"dlc",
				"dlc_metadata",
				"events",
				"fonts",
				"gfx",
				"hints",
				"history",
				"interface",
				"localisation",
				"map",
				"missions",
				"music",
				"music_async",
				"pdx_browser",
				"pdx_online_assets",
				"previewer_assets",
				"sfx",
				"sound",
				"tests",
				"tutorial",
				"tweakergui",
			]),
			Self::CrusaderKings3
			| Self::Victoria3
			| Self::Stellaris
			| Self::HeartsOfIron4
			| Self::Unknown => None,
		}
	}

	/// Returns `true` when the game engine would actually load the given
	/// relative path from a mod root. Used to filter cross-mod overlap
	/// detection so that VCS metadata, IDE artifacts and loose top-level
	/// documentation files (`.git/`, `.vscode/`, `README.md`, `description.txt`,
	/// `thumbnail.png`, …) don't surface as runtime "file conflicts".
	pub fn is_loadable_content_path(&self, relative: &Path) -> bool {
		let Some(roots) = self.loadable_content_roots() else {
			return true;
		};

		let normalized = relative.to_string_lossy().replace('\\', "/");
		let trimmed = normalized.trim_start_matches("./");
		if trimmed.is_empty() {
			return false;
		}

		let top = match trimmed.split_once('/') {
			Some((head, rest)) if !rest.is_empty() => head,
			_ => return false,
		};

		let top_lower = top.to_ascii_lowercase();
		roots.contains(&top_lower.as_str())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;

	#[test]
	fn eu4_loadable_content_path_filters_dev_artifacts() {
		let game = Game::EuropaUniversalis4;
		assert!(game.is_loadable_content_path(Path::new("common/countries/Yokotan.txt")));
		assert!(game.is_loadable_content_path(Path::new("events/ME_Tibet_Events.txt")));
		assert!(game.is_loadable_content_path(Path::new("history/provinces/4291 - Qazania.txt")));
		assert!(game.is_loadable_content_path(Path::new("gfx/interface/foo.dds")));
		assert!(game.is_loadable_content_path(Path::new("Common/Countries/X.txt")));

		assert!(!game.is_loadable_content_path(Path::new(".git/HEAD")));
		assert!(!game.is_loadable_content_path(Path::new(".gitattributes")));
		assert!(!game.is_loadable_content_path(Path::new(".vscode/settings.json")));
		assert!(!game.is_loadable_content_path(Path::new("README.md")));
		assert!(!game.is_loadable_content_path(Path::new("description.txt")));
		assert!(!game.is_loadable_content_path(Path::new("details_discussion.txt")));
		assert!(!game.is_loadable_content_path(Path::new("更新日志.txt")));
		assert!(!game.is_loadable_content_path(Path::new("thumbnail.png")));
		assert!(!game.is_loadable_content_path(Path::new("Thumbnail.png")));
		assert!(!game.is_loadable_content_path(Path::new("thumbnail.jpg")));
		assert!(
			!game.is_loadable_content_path(Path::new("scripted_triggers/00_scripted_triggers.txt"))
		);
	}

	#[test]
	fn unknown_game_does_not_filter_paths() {
		let game = Game::Unknown;
		assert!(game.is_loadable_content_path(Path::new(".git/HEAD")));
		assert!(game.is_loadable_content_path(Path::new("README.md")));
		assert!(game.is_loadable_content_path(Path::new("common/x.txt")));
	}
}
