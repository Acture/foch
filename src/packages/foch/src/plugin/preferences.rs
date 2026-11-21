//! Player network and interface preferences shared by the CLI, TUI, and desktop.
//!
//! The file is `<foch config dir>/ui-settings.toml`. Loading a missing file
//! returns the defaults and does not create the directory. Saving replaces
//! the file through a sibling temporary file.

use super::transport::{
	self, InstallTransport, MirrorPolicy, TransportConfig, normalize_mirror_prefix,
};
use crate::input::config::get_config_dir_path;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const FORMAT: u32 = 1;
const FILE_NAME: &str = "ui-settings.toml";

/// How plugin downloads choose HTTPS and `gh`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportPreference {
	#[default]
	Auto,
	Http,
	Gh,
}

/// Which GitHub mirror policy the player saved.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MirrorPreference {
	#[default]
	Auto,
	Cn,
	Direct,
	Custom,
}

/// The language the interface should use.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguagePreference {
	#[default]
	Auto,
	ZhCn,
	En,
}

/// A concrete interface language.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLanguage {
	ZhCn,
	En,
}

/// Version-one UI settings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserPreferences {
	#[serde(default = "format_one")]
	pub format: u32,
	#[serde(default)]
	pub transport: TransportPreference,
	#[serde(default)]
	pub mirror: MirrorPreference,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub custom_mirrors: Vec<String>,
	#[serde(default)]
	pub language: LanguagePreference,
}

fn format_one() -> u32 {
	FORMAT
}

impl Default for UserPreferences {
	fn default() -> Self {
		Self {
			format: FORMAT,
			transport: TransportPreference::Auto,
			mirror: MirrorPreference::Auto,
			custom_mirrors: Vec::new(),
			language: LanguagePreference::Auto,
		}
	}
}

impl UserPreferences {
	/// `<config dir>/ui-settings.toml`. This does not create the directory.
	pub fn path() -> Result<PathBuf, Box<dyn std::error::Error>> {
		Ok(get_config_dir_path()?.join(FILE_NAME))
	}

	/// Load version-one settings. A missing or blank file is the default.
	pub fn load(path: &Path) -> io::Result<Self> {
		match fs::read_to_string(path) {
			Ok(text) if text.trim().is_empty() => Ok(Self::default()),
			Ok(text) => {
				let mut value: Self = toml::from_str(&text).map_err(io::Error::other)?;
				if value.format != FORMAT {
					return Err(io::Error::new(
						io::ErrorKind::InvalidData,
						format!(
							"unsupported ui settings format {} (expected {FORMAT})",
							value.format
						),
					));
				}
				value
					.validate()
					.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
				Ok(value)
			}
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
			Err(error) => Err(error),
		}
	}

	/// Missing, unreadable, and invalid files become the defaults.
	pub fn load_or_default(path: &Path) -> Self {
		Self::load(path).unwrap_or_default()
	}

	/// Write the snapshot by renaming a sibling temporary file over `path`.
	pub fn save(&self, path: &Path) -> io::Result<()> {
		let mut value = self.clone();
		value.format = FORMAT;
		value
			.validate()
			.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
		let text = toml::to_string_pretty(&value).map_err(io::Error::other)?;
		atomic_write(path, &text)
	}

	/// Reject a custom-mirror list that is not HTTPS, and require one when
	/// the mirror policy is custom.
	pub fn validate(&mut self) -> Result<(), String> {
		let mut mirrors = Vec::with_capacity(self.custom_mirrors.len());
		for mirror in &self.custom_mirrors {
			let normalized = normalize_mirror_prefix(mirror).map_err(|error| error.to_string())?;
			if !normalized.starts_with("https://") {
				return Err(format!("custom mirror must use https: {mirror}"));
			}
			mirrors.push(normalized);
		}
		self.custom_mirrors = mirrors;
		if self.mirror == MirrorPreference::Custom && self.custom_mirrors.is_empty() {
			return Err("custom mirror preference needs at least one https prefix".to_string());
		}
		Ok(())
	}

	/// The language this preference resolves to.
	pub fn resolved_language(&self) -> UiLanguage {
		match self.language {
			LanguagePreference::ZhCn => UiLanguage::ZhCn,
			LanguagePreference::En => UiLanguage::En,
			LanguagePreference::Auto => detect_system_language(),
		}
	}

	/// Transport and mirror for an install that has no CLI flag and no
	/// `FOCH_PLUGIN_TRANSPORT` / `FOCH_PLUGIN_MIRROR` override.
	pub fn to_transport_config(&self) -> TransportConfig {
		TransportConfig {
			transport: match self.transport {
				TransportPreference::Auto => InstallTransport::Auto,
				TransportPreference::Http => InstallTransport::Http,
				TransportPreference::Gh => InstallTransport::Gh,
			},
			mirror: match self.mirror {
				MirrorPreference::Auto => MirrorPolicy::Auto,
				MirrorPreference::Cn => MirrorPolicy::China,
				MirrorPreference::Direct => MirrorPolicy::Direct,
				MirrorPreference::Custom => MirrorPolicy::Custom(self.custom_mirrors.clone()),
			},
		}
	}
}

/// The settings file under the Foch config directory.
pub fn preferences_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
	UserPreferences::path()
}

/// `Auto` follows `locale_hint`. Anything else is that explicit language.
/// A missing hint, or a hint that is not simplified Chinese, is English.
pub fn resolve_language(pref: LanguagePreference, locale_hint: Option<&str>) -> UiLanguage {
	match pref {
		LanguagePreference::ZhCn => UiLanguage::ZhCn,
		LanguagePreference::En => UiLanguage::En,
		LanguagePreference::Auto => match locale_hint {
			Some(hint) if is_simplified_chinese(hint) => UiLanguage::ZhCn,
			_ => UiLanguage::En,
		},
	}
}

/// `LC_ALL`, `LANG`, and on Windows `GetUserDefaultLocaleName`, via the
/// locale hints the download transport already collects.
pub fn detect_system_language() -> UiLanguage {
	if transport::system_locale_hints()
		.iter()
		.any(|hint| is_simplified_chinese(hint))
	{
		UiLanguage::ZhCn
	} else {
		UiLanguage::En
	}
}

fn is_simplified_chinese(hint: &str) -> bool {
	let tag = hint.trim().to_ascii_lowercase().replace('_', "-");
	let primary = tag.split(['.', '@']).next().unwrap_or("");
	if primary == "zh" || primary.starts_with("zh-hans") || primary.starts_with("zh-cn") {
		return true;
	}
	if primary == "zh-sg" {
		return true;
	}
	if !primary.starts_with("zh-") {
		return false;
	}
	!(primary.contains("hant")
		|| primary.contains("-tw")
		|| primary.contains("-hk")
		|| primary.contains("-mo"))
}

fn atomic_write(path: &Path, text: &str) -> io::Result<()> {
	let parent = path
		.parent()
		.filter(|parent| !parent.as_os_str().is_empty())
		.unwrap_or_else(|| Path::new("."));
	fs::create_dir_all(parent)?;
	let mut temporary = tempfile::Builder::new()
		.prefix(".ui-settings-")
		.suffix(".tmp")
		.tempfile_in(parent)?;
	temporary.write_all(text.as_bytes())?;
	let mut temporary = temporary.into_temp_path();
	let deadline = Instant::now() + Duration::from_secs(2);
	let mut backoff = Duration::from_millis(1);
	loop {
		match temporary.persist(path) {
			Ok(()) => return Ok(()),
			Err(error)
				if cfg!(windows)
					&& error.error.kind() == io::ErrorKind::PermissionDenied
					&& Instant::now() < deadline =>
			{
				temporary = error.path;
				std::thread::sleep(backoff);
				backoff = (backoff * 2).min(Duration::from_millis(50));
			}
			Err(error) => return Err(error.error),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_missing_file_is_the_default_and_does_not_need_a_parent_directory() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("missing").join("ui-settings.toml");
		assert!(!path.parent().unwrap().exists());
		let loaded = UserPreferences::load(&path).unwrap();
		assert_eq!(loaded, UserPreferences::default());
		assert!(!path.parent().unwrap().exists());
	}

	#[test]
	fn save_round_trips_and_normalizes_https_mirrors() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("ui-settings.toml");
		let preferences = UserPreferences {
			format: 0,
			transport: TransportPreference::Http,
			mirror: MirrorPreference::Custom,
			custom_mirrors: vec!["https://gh-proxy.com".to_string()],
			language: LanguagePreference::ZhCn,
		};
		preferences.save(&path).unwrap();
		let loaded = UserPreferences::load(&path).unwrap();
		assert_eq!(loaded.format, FORMAT);
		assert_eq!(loaded.transport, TransportPreference::Http);
		assert_eq!(loaded.mirror, MirrorPreference::Custom);
		assert_eq!(
			loaded.custom_mirrors,
			vec!["https://gh-proxy.com/".to_string()]
		);
		assert_eq!(loaded.language, LanguagePreference::ZhCn);
		assert_eq!(
			loaded.to_transport_config(),
			TransportConfig {
				transport: InstallTransport::Http,
				mirror: MirrorPolicy::Custom(vec!["https://gh-proxy.com/".to_string()]),
			}
		);
	}

	#[test]
	fn custom_mirrors_must_be_https_and_non_empty_when_selected() {
		let mut preferences = UserPreferences {
			mirror: MirrorPreference::Custom,
			custom_mirrors: vec!["http://127.0.0.1:9/".to_string()],
			..UserPreferences::default()
		};
		assert!(preferences.validate().is_err());
		preferences.custom_mirrors.clear();
		preferences.mirror = MirrorPreference::Direct;
		assert!(preferences.validate().is_ok());
		preferences.mirror = MirrorPreference::Custom;
		assert!(preferences.validate().is_err());
	}

	#[test]
	fn resolve_language_follows_the_hint_only_for_auto() {
		assert_eq!(
			resolve_language(LanguagePreference::Auto, Some("zh_CN.UTF-8")),
			UiLanguage::ZhCn
		);
		assert_eq!(
			resolve_language(LanguagePreference::Auto, Some("zh-Hans")),
			UiLanguage::ZhCn
		);
		assert_eq!(
			resolve_language(LanguagePreference::Auto, Some("zh-TW")),
			UiLanguage::En
		);
		assert_eq!(
			resolve_language(LanguagePreference::Auto, Some("en_US")),
			UiLanguage::En
		);
		assert_eq!(
			resolve_language(LanguagePreference::Auto, None),
			UiLanguage::En
		);
		assert_eq!(
			resolve_language(LanguagePreference::En, Some("zh-CN")),
			UiLanguage::En
		);
		assert_eq!(
			resolve_language(LanguagePreference::ZhCn, Some("en")),
			UiLanguage::ZhCn
		);
	}

	#[test]
	fn detect_system_language_returns_one_of_the_two_languages() {
		assert!(matches!(
			detect_system_language(),
			UiLanguage::ZhCn | UiLanguage::En
		));
	}
}
