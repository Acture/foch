use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::Path;

/// The only verified game identity in Foch's product API.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Eu4;

impl Eu4 {
	pub const STEAM_APP_ID: u32 = 236_850;
	pub const KEY: &'static str = "eu4";
	pub const PARADOX_DATA_DIR_NAME: &'static str = "Europa Universalis IV";

	pub fn from_key(value: &str) -> Option<Self> {
		matches!(
			value.trim().to_ascii_lowercase().as_str(),
			"eu4" | "europauniversalis4" | "europa-universalis-4"
		)
		.then_some(Self)
	}

	pub const fn key(&self) -> &'static str {
		Self::KEY
	}

	pub const fn steam_app_ids(&self) -> &'static [u32] {
		&[Self::STEAM_APP_ID]
	}

	pub const fn paradox_data_dir_name(&self) -> Option<&'static str> {
		Some(Self::PARADOX_DATA_DIR_NAME)
	}

	pub const fn loadable_content_roots(&self) -> Option<&'static [&'static str]> {
		Some(&[
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
		])
	}

	pub fn is_loadable_content_path(&self, relative: &Path) -> bool {
		let normalized = relative.to_string_lossy().replace('\\', "/");
		let trimmed = normalized.trim_start_matches("./");
		if trimmed.is_empty() {
			return false;
		}
		let Some((top, rest)) = trimmed.split_once('/') else {
			return false;
		};
		if rest.is_empty() {
			return false;
		}
		let top = top.to_ascii_lowercase();
		self.loadable_content_roots()
			.expect("EU4 loadable roots are static")
			.contains(&top.as_str())
	}
}

impl Serialize for Eu4 {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.serialize_str(Self::KEY)
	}
}

impl<'de> Deserialize<'de> for Eu4 {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = String::deserialize(deserializer)?;
		Self::from_key(&value)
			.ok_or_else(|| serde::de::Error::custom(format!("unsupported game key {value:?}")))
	}
}
