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
	///
	/// The panel binary is the caller. A cdylib build does not see that use.
	#[cfg_attr(not(test), allow(dead_code))]
	pub fn write_data_file(&self, data_dir: &Path) -> std::io::Result<()> {
		std::fs::create_dir_all(data_dir)?;
		let text = serde_json::to_string_pretty(self)?;
		let target: PathBuf = data_dir.join(DATA_FILE);
		let staging = data_dir.join(format!("{DATA_FILE}.tmp"));
		std::fs::write(&staging, text)?;
		std::fs::rename(&staging, &target)
	}
}

/// How long the hot path waits between `tribute-config.json` stat calls.
pub const RELOAD_DEBOUNCE_MS: u64 = 250;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
	modified_ns: u128,
	len: u64,
}

/// Manifest baseline plus the data-file watch. The hot path polls this at
/// most once per [`RELOAD_DEBOUNCE_MS`] so a slider save is picked up without
/// statting on every subject.
#[derive(Debug)]
pub struct ConfigWatch {
	data_dir: PathBuf,
	baseline: TributeConfig,
	debounce_ms: u64,
	last_poll_ms: u64,
	armed: bool,
	stamp: Option<FileStamp>,
}

impl ConfigWatch {
	pub fn new(data_dir: PathBuf, baseline: TributeConfig) -> Self {
		let stamp = file_stamp(&data_dir.join(DATA_FILE));
		Self {
			data_dir,
			baseline,
			debounce_ms: RELOAD_DEBOUNCE_MS,
			last_poll_ms: 0,
			armed: false,
			stamp,
		}
	}

	/// `Some` when the runtime config should be replaced. A corrupt file keeps
	/// the previous config (the stamp still advances, so a bad save is not
	/// retried until the file changes again). A missing file restores the
	/// manifest baseline.
	pub fn poll(&mut self, now_ms: u64) -> Option<TributeConfig> {
		if self.armed && now_ms.saturating_sub(self.last_poll_ms) < self.debounce_ms {
			return None;
		}
		self.armed = true;
		self.last_poll_ms = now_ms;
		let path = self.data_dir.join(DATA_FILE);
		let stamp = file_stamp(&path);
		if stamp == self.stamp {
			return None;
		}
		self.stamp = stamp;
		match read_overlay(&self.baseline, &path) {
			Overlay::Applied(config) => Some(config),
			Overlay::Missing => Some(self.baseline.clone()),
			Overlay::Corrupt => None,
		}
	}
}

enum Overlay {
	Applied(TributeConfig),
	Missing,
	Corrupt,
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
	let meta = std::fs::metadata(path).ok()?;
	let modified_ns = meta
		.modified()
		.ok()
		.and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
		.map(|duration| duration.as_nanos())
		.unwrap_or(0);
	Some(FileStamp {
		modified_ns,
		len: meta.len(),
	})
}

fn read_overlay(baseline: &TributeConfig, path: &Path) -> Overlay {
	let text = match std::fs::read_to_string(path) {
		Ok(text) => text,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Overlay::Missing,
		Err(_) => return Overlay::Corrupt,
	};
	let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
		return Overlay::Corrupt;
	};
	Overlay::Applied(apply_overlay(baseline.clone(), &value))
}

fn apply_overlay(mut config: TributeConfig, value: &serde_json::Value) -> TributeConfig {
	if let Some(enabled) = value.get("enabled").and_then(serde_json::Value::as_bool) {
		config.enabled = enabled;
	}
	if let Some(percent) = value
		.get("global_percent")
		.and_then(serde_json::Value::as_i64)
	{
		config.global_percent = clamp_percent(percent);
	}
	if let Some(kinds) = value.get("kind_percent") {
		config.kind_percent = percent_map(kinds, false);
	}
	if let Some(tags) = value.get("tag_percent") {
		config.tag_percent = percent_map(tags, true);
	}
	config
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

	#[test]
	fn watch_reloads_after_the_debounce_and_ignores_a_corrupt_save() {
		let dir = tempfile_dir();
		let baseline = TributeConfig::from_manifest_config(r#"{"global_percent":80}"#);
		write_stamped(
			&dir.join(DATA_FILE),
			r#"{"global_percent":40,"kind_percent":{"vassal":25}}"#,
			1,
		);
		let mut watch = ConfigWatch::new(dir.clone(), baseline.clone());
		assert!(
			watch.poll(1_000).is_none(),
			"init already applied this stamp"
		);

		write_stamped(
			&dir.join(DATA_FILE),
			r#"{"global_percent":40,"enabled":false}"#,
			2,
		);
		assert!(
			watch.poll(1_100).is_none(),
			"inside the debounce window the new file is not read"
		);
		let reloaded = watch.poll(1_000 + RELOAD_DEBOUNCE_MS).unwrap();
		assert!(!reloaded.enabled);
		assert_eq!(reloaded.global_percent, 40);

		write_stamped(&dir.join(DATA_FILE), "not json", 3);
		assert!(watch.poll(2_000).is_none(), "corrupt json does not replace");
		let _ = std::fs::remove_file(dir.join(DATA_FILE));
		let restored = watch.poll(3_000).unwrap();
		assert_eq!(restored, baseline);
		let _ = std::fs::remove_dir_all(&dir);
	}

	fn write_stamped(path: &Path, body: &str, seconds: u64) {
		std::fs::write(path, body).unwrap();
		let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
		file.set_modified(
			std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + seconds),
		)
		.unwrap();
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
