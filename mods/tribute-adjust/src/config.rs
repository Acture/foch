//! Runtime configuration: manifest config JSON in, `tribute-config.json` in
//! the plugin data directory on top, out to the hot path and the slider panel.

use crate::coefficient::TributeConfig;
use std::path::{Path, PathBuf};

pub const DATA_FILE: &str = "tribute-config.json";

/// Clamp every percent the panel or a hand-edited file can produce.
fn clamp_percent(value: i64) -> i64 {
	value.clamp(0, 200)
}

/// Extract a percent map from a JSON object of key -> number.
/// Non-numeric entries are skipped. Tag keys are uppercased when they are
/// three characters; subject-type keys keep the spelling from the script.
fn percent_map(
	value: &serde_json::Value,
	uppercase_tags: bool,
) -> std::collections::BTreeMap<String, i64> {
	value
		.as_object()
		.map(|entries| {
			entries
				.iter()
				.filter_map(|(key, value)| {
					let percent = value.as_i64()?;
					let key = if uppercase_tags && key.len() == 3 {
						key.to_ascii_uppercase()
					} else {
						key.clone()
					};
					Some((key, clamp_percent(percent)))
				})
				.collect()
		})
		.unwrap_or_default()
}

impl TributeConfig {
	/// Build the runtime configuration from the host-delivered manifest
	/// config JSON (`{"enabled":..,"global_percent":..,"tag_overrides":".."}`).
	pub fn from_manifest_config(config_json: &str) -> TributeConfig {
		let value: serde_json::Value = serde_json::from_str(config_json).unwrap_or_default();
		let mut config = TributeConfig {
			enabled: value
				.get("enabled")
				.and_then(serde_json::Value::as_bool)
				.unwrap_or(true),
			global_percent: value
				.get("global_percent")
				.and_then(serde_json::Value::as_i64)
				.unwrap_or(100),
			kind_percent: value
				.get("kind_percent")
				.map(|kinds| percent_map(kinds, false))
				.unwrap_or_default(),
			tag_percent: value
				.get("tag_overrides")
				.and_then(serde_json::Value::as_str)
				.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
				.map(|overrides| percent_map(&overrides, true))
				.unwrap_or_default(),
		};
		config.global_percent = clamp_percent(config.global_percent);
		if let Some(percent) = value
			.get("vassal_percent")
			.and_then(serde_json::Value::as_i64)
		{
			config
				.kind_percent
				.insert("vassal".into(), clamp_percent(percent));
		}
		config
	}

	/// Overlay `tribute-config.json` from the data directory; anything the
	/// file states wins, the rest of the manifest configuration stays.
	pub fn overlay_data_file(mut self, data_dir: &Path) -> TributeConfig {
		let Ok(text) = std::fs::read_to_string(data_dir.join(DATA_FILE)) else {
			return self;
		};
		let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
			return self;
		};
		if let Some(enabled) = value.get("enabled").and_then(serde_json::Value::as_bool) {
			self.enabled = enabled;
		}
		if let Some(percent) = value
			.get("global_percent")
			.and_then(serde_json::Value::as_i64)
		{
			self.global_percent = clamp_percent(percent);
		}
		if let Some(kinds) = value.get("kind_percent") {
			self.kind_percent = percent_map(kinds, false);
		}
		if let Some(tags) = value.get("tag_percent") {
			self.tag_percent = percent_map(tags, true);
		}
		self
	}

	/// Persist as `tribute-config.json`, atomically enough for a slider panel.
	#[allow(dead_code)] // until the slider panel lands
	pub fn write_data_file(&self, data_dir: &Path) -> std::io::Result<()> {
		std::fs::create_dir_all(data_dir)?;
		let text = serde_json::to_string_pretty(self)?;
		let target: PathBuf = data_dir.join(DATA_FILE);
		let staging = data_dir.join(format!("{DATA_FILE}.tmp"));
		std::fs::write(&staging, text)?;
		std::fs::rename(&staging, &target)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn manifest_config_maps_and_clamps() {
		let config = TributeConfig::from_manifest_config(
			r#"{"enabled":true,"global_percent":999,"tag_overrides":"{\"mug\":10,\"KOR\":-5}"}"#,
		);
		assert_eq!(config.global_percent, 200);
		assert_eq!(config.tag_percent.get("MUG"), Some(&10));
		assert_eq!(config.tag_percent.get("KOR"), Some(&0));
	}

	#[test]
	fn kind_keys_keep_script_spelling_while_tags_are_uppercased() {
		let config = TributeConfig::from_manifest_config(
			r#"{"kind_percent":{"eyalet":40,"abc":15,"Core_Eyalet":70},"tag_overrides":"{\"mug\":10}"}"#,
		);
		assert_eq!(config.kind_percent.get("eyalet"), Some(&40));
		assert_eq!(config.kind_percent.get("abc"), Some(&15));
		assert_eq!(config.kind_percent.get("Core_Eyalet"), Some(&70));
		assert_eq!(config.tag_percent.get("MUG"), Some(&10));
	}

	#[test]
	fn tag_overrides_that_are_not_json_are_ignored() {
		let config = TributeConfig::from_manifest_config(r#"{"tag_overrides":"not json"}"#);
		assert!(config.tag_percent.is_empty());
	}

	#[test]
	fn data_file_overlays_only_what_it_states() {
		let dir = tempfile_dir();
		let base = TributeConfig::from_manifest_config(r#"{"global_percent":80}"#);
		let _ = std::fs::write(
			dir.join(DATA_FILE),
			r#"{"enabled":false,"tag_percent":{"FRA":150}}"#,
		);
		let merged = base.clone().overlay_data_file(&dir);
		assert!(!merged.enabled);
		assert_eq!(merged.global_percent, 80);
		assert_eq!(merged.tag_percent.get("FRA"), Some(&150));
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn writing_then_overlaying_round_trips() {
		let dir = tempfile_dir();
		let mut config = TributeConfig::default();
		config.kind_percent.insert("tributary".into(), 50);
		config.write_data_file(&dir).unwrap();
		let reloaded = TributeConfig::default().overlay_data_file(&dir);
		assert_eq!(reloaded, config);
		let _ = std::fs::remove_dir_all(&dir);
	}

	fn tempfile_dir() -> PathBuf {
		let dir = std::env::temp_dir().join(format!(
			"foch-eu4-tribute-{}-{:x}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_nanos()
		));
		std::fs::create_dir_all(&dir).unwrap();
		dir
	}
}
