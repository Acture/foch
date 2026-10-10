//! Per-playset plugin choices: which plugins a playset turns on, pinned to an
//! exact version, with their configuration.
//!
//! This is the user's intent, saved under the Foch config directory. It is
//! separate from the store (what is installed) and from a launch's actual
//! result. Turning intent into a launch order is [`super::planner::plan`]'s
//! job; this module only persists and converts the choices.

use super::planner::Selection;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

/// All playsets' plugin choices, as stored on disk.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selections {
	#[serde(default)]
	pub format: u32,
	/// Keyed by playset name.
	#[serde(default)]
	pub playsets: BTreeMap<String, PlaysetSelections>,
}

/// One playset's plugin choices, keyed by stable plugin id.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlaysetSelections {
	#[serde(default)]
	pub plugins: BTreeMap<String, Choice>,
}

/// A single plugin's choice for a playset.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
	pub version: Version,
	#[serde(default = "default_true")]
	pub enabled: bool,
	/// Configuration values as a TOML table; validated against the manifest
	/// schema when a launch is planned, not here.
	#[serde(default)]
	pub config: BTreeMap<String, toml::Value>,
}

fn default_true() -> bool {
	true
}

pub const FORMAT: u32 = 1;

impl Selections {
	/// Load selections from `path`; an absent file is an empty set.
	pub fn load(path: &Path) -> io::Result<Self> {
		match fs::read_to_string(path) {
			Ok(text) if text.trim().is_empty() => Ok(Self::default()),
			Ok(text) => toml::from_str(&text).map_err(io::Error::other),
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
			Err(error) => Err(error),
		}
	}

	/// Write selections to `path` atomically (temp file + rename on the same
	/// directory), creating parent directories as needed.
	pub fn save(&self, path: &Path) -> io::Result<()> {
		let mut value = self.clone();
		value.format = FORMAT;
		let text = toml::to_string_pretty(&value).map_err(io::Error::other)?;
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		let temp = path.with_extension("toml.tmp");
		fs::write(&temp, text.as_bytes())?;
		fs::rename(&temp, path)
	}

	/// The choices for one playset, or an empty set.
	pub fn for_playset(&self, playset: &str) -> PlaysetSelections {
		self.playsets.get(playset).cloned().unwrap_or_default()
	}

	/// Replace one playset's choices.
	pub fn set_playset(&mut self, playset: &str, choices: PlaysetSelections) {
		self.playsets.insert(playset.to_string(), choices);
	}
}

impl PlaysetSelections {
	/// The enabled and disabled choices as planner [`Selection`]s, in plugin-id
	/// order. Disabled plugins are included so the planner can report them;
	/// the planner ignores disabled entries for ordering.
	pub fn to_selections(&self) -> Vec<Selection> {
		self.plugins
			.iter()
			.map(|(id, choice)| Selection {
				id: id.clone(),
				version: choice.version.clone(),
				enabled: choice.enabled,
				config: config_to_json(&choice.config),
			})
			.collect()
	}
}

/// Convert the stored TOML config scalars to the JSON the planner and host
/// carry. Only the scalar kinds the manifest schema allows are represented;
/// unsupported kinds remain invalid (null), so even a string field rejects
/// arrays, tables, floats and datetimes instead of silently coercing them.
fn config_to_json(config: &BTreeMap<String, toml::Value>) -> serde_json::Value {
	let mut map = serde_json::Map::new();
	for (key, value) in config {
		let json = match value {
			toml::Value::Boolean(flag) => serde_json::Value::Bool(*flag),
			toml::Value::Integer(number) => serde_json::Value::from(*number),
			toml::Value::String(text) => serde_json::Value::from(text.clone()),
			_ => serde_json::Value::Null,
		};
		map.insert(key.clone(), json);
	}
	serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample() -> Selections {
		let mut selections = Selections::default();
		let mut choices = PlaysetSelections::default();
		choices.plugins.insert(
			"io.github.yozoratempest.eu4-unicode-patch".into(),
			Choice {
				version: Version::parse("0.1.14").unwrap(),
				enabled: true,
				config: BTreeMap::from([("typo_tolerance".into(), toml::Value::Integer(2))]),
			},
		);
		choices.plugins.insert(
			"io.github.yozoratempest.eu4-menu-patch".into(),
			Choice {
				version: Version::parse("0.1.4").unwrap(),
				enabled: false,
				config: BTreeMap::new(),
			},
		);
		selections.set_playset("My Playset", choices);
		selections
	}

	#[test]
	fn round_trips_through_a_file() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("selections.toml");
		sample().save(&path).unwrap();
		let loaded = Selections::load(&path).unwrap();
		let choices = loaded.for_playset("My Playset");
		assert_eq!(choices.plugins.len(), 2);
		let unicode = &choices.plugins["io.github.yozoratempest.eu4-unicode-patch"];
		assert_eq!(unicode.version, Version::parse("0.1.14").unwrap());
		assert!(unicode.enabled);
		assert_eq!(unicode.config["typo_tolerance"], toml::Value::Integer(2));
	}

	#[test]
	fn an_absent_file_is_empty() {
		let temp = tempfile::tempdir().unwrap();
		let loaded = Selections::load(&temp.path().join("none.toml")).unwrap();
		assert!(loaded.playsets.is_empty());
		assert!(loaded.for_playset("anything").plugins.is_empty());
	}

	#[test]
	fn converts_to_planner_selections_preserving_enabled() {
		let choices = sample().for_playset("My Playset");
		let selections = choices.to_selections();
		assert_eq!(selections.len(), 2);
		let unicode = selections
			.iter()
			.find(|s| s.id.ends_with("unicode-patch"))
			.unwrap();
		assert!(unicode.enabled);
		assert_eq!(unicode.config["typo_tolerance"], 2);
		let menu = selections
			.iter()
			.find(|s| s.id.ends_with("menu-patch"))
			.unwrap();
		assert!(!menu.enabled);
	}

	#[test]
	fn unsupported_toml_values_cannot_pass_a_string_schema() {
		let mut manifest = super::super::builtin::adapters().remove(0);
		manifest.config.clear();
		manifest.config.insert(
			"caption".into(),
			super::super::manifest::ConfigField::String {
				default: "hello".into(),
				description: None,
				adapter: None,
			},
		);
		for text in [
			"caption=[1,2]",
			"caption={x=1}",
			"caption=2026-10-11",
			"caption=1.25",
		] {
			let config: BTreeMap<String, toml::Value> = toml::from_str(text).unwrap();
			assert!(
				super::super::deployment::effective_config(&manifest, &config_to_json(&config))
					.is_err(),
				"{text}"
			);
		}
	}

	#[test]
	fn save_is_atomic_leaving_no_temp_file() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("selections.toml");
		let mut selections = sample();
		selections.save(&path).unwrap();
		selections.playsets.clear();
		selections.save(&path).unwrap();
		assert!(Selections::load(&path).unwrap().playsets.is_empty());
		let leftover = fs::read_dir(temp.path())
			.unwrap()
			.filter_map(Result::ok)
			.any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"));
		assert!(!leftover);
	}
}
