use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_EMIT_INDENT: &str = "\t";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct FochConfig {
	#[serde(default)]
	pub overrides: Vec<DepOverride>,
	#[serde(default)]
	pub resolutions: Vec<ResolutionEntry>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub emit: Option<EmitConfig>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmitConfig {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub indent: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFochConfig {
	#[serde(default)]
	overrides: Vec<DepOverride>,
	#[serde(default)]
	resolutions: Vec<ResolutionEntry>,
	#[serde(default)]
	emit: Option<EmitConfig>,
}

impl<'de> Deserialize<'de> for FochConfig {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let raw = RawFochConfig::deserialize(deserializer)?;
		ResolutionMap::from_entries(&raw.resolutions).map_err(serde::de::Error::custom)?;
		Ok(Self {
			overrides: raw.overrides,
			resolutions: raw.resolutions,
			emit: raw.emit,
		})
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DepOverride {
	#[serde(rename = "mod", alias = "mod_id")]
	pub mod_id: String,
	#[serde(rename = "dep", alias = "dep_id")]
	pub dep_id: String,
	#[serde(default)]
	pub note: Option<String>,
}

impl DepOverride {
	pub fn new(mod_id: impl Into<String>, dep_id: impl Into<String>) -> Self {
		Self {
			mod_id: mod_id.into(),
			dep_id: dep_id.into(),
			note: None,
		}
	}

	pub fn matches(&self, mod_id: &str, dep_id: &str) -> bool {
		self.mod_id == mod_id && self.dep_id == dep_id
	}
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResolutionEntry {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub file: Option<PathBuf>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub conflict_id: Option<String>,
	#[serde(rename = "mod", default, skip_serializing_if = "Option::is_none")]
	pub mod_id: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub prefer_mod: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub use_file: Option<PathBuf>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub keep_existing: Option<bool>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub priority_boost: Option<i32>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct ResolutionMap {
	pub by_file: HashMap<PathBuf, ResolutionDecision>,
	pub by_conflict_id: HashMap<String, ResolutionDecision>,
	pub mod_priority_boost: HashMap<String, i32>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ResolutionDecision {
	PreferMod(String),
	UseFile(PathBuf),
	KeepExisting,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError {
	message: String,
}

impl ConfigError {
	pub fn new(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
		}
	}

	pub fn message(&self) -> &str {
		&self.message
	}

	fn resolution_entry(index: usize, message: impl Into<String>) -> Self {
		Self::new(format!(
			"invalid [[resolutions]] entry {}: {}",
			index + 1,
			message.into()
		))
	}
}

impl fmt::Display for ConfigError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.message)
	}
}

impl Error for ConfigError {}

impl ResolutionMap {
	pub fn from_entries(entries: &[ResolutionEntry]) -> Result<Self, ConfigError> {
		let mut map = Self::default();
		for (index, entry) in entries.iter().enumerate() {
			entry.validate(index)?;
			if let Some(mod_id) = &entry.mod_id {
				let priority_boost = entry
					.priority_boost
					.expect("validated mod resolution has priority_boost");
				map.mod_priority_boost
					.insert(mod_id.clone(), priority_boost);
				continue;
			}

			let decision = entry.decision();
			if let Some(file) = &entry.file {
				map.by_file.insert(file.clone(), decision);
			} else if let Some(conflict_id) = &entry.conflict_id {
				map.by_conflict_id.insert(conflict_id.clone(), decision);
			}
		}
		Ok(map)
	}

	pub fn lookup(&self, file: &Path, conflict_id: &str) -> Option<&ResolutionDecision> {
		self.by_conflict_id
			.get(conflict_id)
			.or_else(|| self.by_file.get(file))
	}
}

impl ResolutionEntry {
	fn validate(&self, index: usize) -> Result<(), ConfigError> {
		if self.selector_count() != 1 {
			return Err(ConfigError::resolution_entry(
				index,
				"exactly one selector (file, conflict_id, mod) must be set",
			));
		}
		if matches!(self.keep_existing, Some(false)) {
			return Err(ConfigError::resolution_entry(
				index,
				"keep_existing must be true when set",
			));
		}
		if self.mod_id.is_some() {
			if self.priority_boost.is_none() {
				return Err(ConfigError::resolution_entry(
					index,
					"mod selector requires priority_boost action",
				));
			}
			if self.all_action_count() != 1 {
				return Err(ConfigError::resolution_entry(
					index,
					"priority_boost cannot be combined with prefer_mod, use_file, or keep_existing",
				));
			}
			return Ok(());
		}
		if self.priority_boost.is_some() {
			return Err(ConfigError::resolution_entry(
				index,
				"priority_boost requires mod selector",
			));
		}
		if self.standard_action_count() != 1 {
			return Err(ConfigError::resolution_entry(
				index,
				"exactly one action (prefer_mod, use_file, keep_existing) must be set",
			));
		}
		if self.keep_existing.is_some() && self.file.is_none() {
			return Err(ConfigError::resolution_entry(
				index,
				"keep_existing action requires file selector",
			));
		}
		Ok(())
	}

	fn selector_count(&self) -> usize {
		option_count(&self.file) + option_count(&self.conflict_id) + option_count(&self.mod_id)
	}

	fn standard_action_count(&self) -> usize {
		option_count(&self.prefer_mod)
			+ option_count(&self.use_file)
			+ option_count(&self.keep_existing)
	}

	fn all_action_count(&self) -> usize {
		self.standard_action_count() + option_count(&self.priority_boost)
	}

	fn decision(&self) -> ResolutionDecision {
		if let Some(prefer_mod) = &self.prefer_mod {
			ResolutionDecision::PreferMod(prefer_mod.clone())
		} else if let Some(use_file) = &self.use_file {
			ResolutionDecision::UseFile(use_file.clone())
		} else if self.keep_existing.is_some() {
			ResolutionDecision::KeepExisting
		} else {
			unreachable!("validated non-mod resolution has a decision")
		}
	}
}

pub fn compute_conflict_id(file_path: &Path, addr_path: &str, addr_key: &str) -> String {
	let mut hasher = blake3::Hasher::new();
	let normalized_file_path = file_path.to_string_lossy().replace('\\', "/");
	hasher.update(normalized_file_path.as_bytes());
	hasher.update(b"\0");
	hasher.update(addr_path.as_bytes());
	hasher.update(b"\0");
	hasher.update(addr_key.as_bytes());
	let hash = hasher.finalize();
	let hex = hash.to_hex();
	hex.as_str()[..8].to_owned()
}

fn option_count<T>(option: &Option<T>) -> usize {
	if option.is_some() { 1 } else { 0 }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DepOverrideSource {
	Config,
	Cli,
}

impl fmt::Display for DepOverrideSource {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Config => f.write_str("config"),
			Self::Cli => f.write_str("cli"),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct AppliedDepOverride {
	pub mod_id: String,
	pub dep_id: String,
	pub source: DepOverrideSource,
}

impl AppliedDepOverride {
	pub fn config(dep_override: &DepOverride) -> Self {
		Self {
			mod_id: dep_override.mod_id.clone(),
			dep_id: dep_override.dep_id.clone(),
			source: DepOverrideSource::Config,
		}
	}

	pub fn cli(mod_id: impl Into<String>, dep_id: impl Into<String>) -> Self {
		Self {
			mod_id: mod_id.into(),
			dep_id: dep_id.into(),
			source: DepOverrideSource::Cli,
		}
	}
}

impl From<&AppliedDepOverride> for DepOverride {
	fn from(value: &AppliedDepOverride) -> Self {
		Self::new(value.mod_id.clone(), value.dep_id.clone())
	}
}

#[derive(Debug)]
pub enum FochConfigLoadError {
	Io {
		path: PathBuf,
		source: std::io::Error,
	},
	Parse {
		path: PathBuf,
		source: toml::de::Error,
	},
}

impl fmt::Display for FochConfigLoadError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io { path, source } => {
				write!(f, "failed to read foch config {}: {source}", path.display())
			}
			Self::Parse { path, source } => {
				write!(
					f,
					"failed to parse foch config {}: {source}",
					path.display()
				)
			}
		}
	}
}

impl Error for FochConfigLoadError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Io { source, .. } => Some(source),
			Self::Parse { source, .. } => Some(source),
		}
	}
}

impl FochConfig {
	pub fn from_toml_str(content: &str) -> Result<Self, toml::de::Error> {
		toml::from_str(content)
	}

	pub fn emit_indent(&self) -> &str {
		self.emit
			.as_ref()
			.and_then(|emit| emit.indent.as_deref())
			.unwrap_or(DEFAULT_EMIT_INDENT)
	}

	pub fn load(playset_root: &Path) -> Self {
		Self::try_load(playset_root).unwrap_or_default()
	}

	pub fn try_load(playset_root: &Path) -> Result<Self, FochConfigLoadError> {
		Self::try_load_from_paths(Self::search_paths(playset_root))
	}

	pub fn load_from_path(path: &Path) -> Result<Self, FochConfigLoadError> {
		Self::load_file(path)
	}

	fn try_load_from_paths(paths: Vec<PathBuf>) -> Result<Self, FochConfigLoadError> {
		let mut merged = Self::default();
		for path in paths {
			if path.is_file() {
				let config = Self::load_file(&path)?;
				merged.overrides.extend(config.overrides);
				merged.resolutions.extend(config.resolutions);
				if config.emit.is_some() {
					merged.emit = config.emit;
				}
			}
		}
		Ok(merged)
	}

	fn load_file(path: &Path) -> Result<Self, FochConfigLoadError> {
		let content = fs::read_to_string(path).map_err(|source| FochConfigLoadError::Io {
			path: path.to_path_buf(),
			source,
		})?;
		if content.trim().is_empty() {
			return Ok(Self::default());
		}
		Self::from_toml_str(&content).map_err(|source| FochConfigLoadError::Parse {
			path: path.to_path_buf(),
			source,
		})
	}

	fn search_paths(playset_root: &Path) -> Vec<PathBuf> {
		let mut paths = Vec::new();
		let mut seen = HashSet::new();
		if let Ok(cwd) = std::env::current_dir() {
			push_unique_path(&mut paths, &mut seen, cwd.join("foch.toml"));
		}
		push_unique_path(&mut paths, &mut seen, playset_root.join("foch.toml"));
		// 注意:不要扫 ~/.config/foch/config.toml — 那个文件归
		// foch_engine::config::Config(steam_root_path / game_path / extra_ignore_patterns),
		// 跟此处的本地 foch.toml(overrides / resolutions)是两个不同 schema。
		// 用户级 foch.toml 用 ~/.config/foch/foch.toml 隔离开。
		if let Some(home) = dirs::home_dir() {
			push_unique_path(
				&mut paths,
				&mut seen,
				home.join(".config").join("foch").join("foch.toml"),
			);
		}
		paths
	}
}

fn push_unique_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
	if seen.insert(path.clone()) {
		paths.push(path);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn parses_valid_toml_with_multiple_overrides() {
		let config = FochConfig::from_toml_str(
			r#"
[[overrides]]
mod = "3378403419"
dep = "1999055990"
note = "not a git parent"

[[overrides]]
mod = "abc"
dep = "def"
"#,
		)
		.expect("parse config");

		assert_eq!(config.overrides.len(), 2);
		assert_eq!(config.overrides[0].mod_id, "3378403419");
		assert_eq!(config.overrides[0].dep_id, "1999055990");
		assert_eq!(
			config.overrides[0].note.as_deref(),
			Some("not a git parent")
		);
		assert_eq!(config.overrides[1], DepOverride::new("abc", "def"));
	}

	#[test]
	fn parses_emit_indent_config() {
		let config = FochConfig::from_toml_str(
			r#"
[emit]
indent = "  "
"#,
		)
		.expect("parse config");

		assert_eq!(
			config.emit,
			Some(EmitConfig {
				indent: Some("  ".to_string()),
			})
		);
		assert_eq!(config.emit_indent(), "  ");
	}

	#[test]
	fn missing_emit_config_uses_default_indent() {
		let config = FochConfig::from_toml_str(
			r#"
[[overrides]]
mod = "abc"
dep = "def"
"#,
		)
		.expect("parse config");

		assert_eq!(config.emit, None);
		assert_eq!(config.emit_indent(), DEFAULT_EMIT_INDENT);
	}

	#[test]
	fn rejects_override_missing_dep_field() {
		let err = FochConfig::from_toml_str(
			r#"
[[overrides]]
mod = "3378403419"
"#,
		)
		.expect_err("missing dep must be invalid");

		assert!(err.to_string().contains("dep"), "error was: {err}");
	}

	#[test]
	fn parses_valid_toml_with_resolution_variants() {
		let config = FochConfig::from_toml_str(
			r#"
[[overrides]]
mod = "abc"
dep = "def"

[[resolutions]]
file = "events/PirateEvents.txt"
prefer_mod = "1234567890"

[[resolutions]]
file = "events/ManualEvents.txt"
use_file = "manual/PirateEvents.txt"

[[resolutions]]
file = "events/ExistingEvents.txt"
keep_existing = true

[[resolutions]]
conflict_id = "ab12cd34"
prefer_mod = "9876543210"

[[resolutions]]
mod = "1234567890"
priority_boost = 100
"#,
		)
		.expect("parse config");

		assert_eq!(config.overrides, vec![DepOverride::new("abc", "def")]);
		assert_eq!(config.resolutions.len(), 5);

		let map = ResolutionMap::from_entries(&config.resolutions).expect("build resolution map");
		assert_eq!(
			map.by_file.get(Path::new("events/PirateEvents.txt")),
			Some(&ResolutionDecision::PreferMod("1234567890".to_owned()))
		);
		assert_eq!(
			map.by_file.get(Path::new("events/ManualEvents.txt")),
			Some(&ResolutionDecision::UseFile(PathBuf::from(
				"manual/PirateEvents.txt"
			)))
		);
		assert_eq!(
			map.by_file.get(Path::new("events/ExistingEvents.txt")),
			Some(&ResolutionDecision::KeepExisting)
		);
		assert_eq!(
			map.by_conflict_id.get("ab12cd34"),
			Some(&ResolutionDecision::PreferMod("9876543210".to_owned()))
		);
		assert_eq!(map.mod_priority_boost.get("1234567890"), Some(&100));
	}

	#[test]
	fn rejects_invalid_resolution_entries() {
		let cases = [
			(
				"missing selector",
				r#"
[[resolutions]]
prefer_mod = "123"
"#,
				"exactly one selector",
			),
			(
				"missing action",
				r#"
[[resolutions]]
file = "events/PirateEvents.txt"
"#,
				"exactly one action",
			),
			(
				"multiple selectors",
				r#"
[[resolutions]]
file = "events/PirateEvents.txt"
conflict_id = "ab12cd34"
prefer_mod = "123"
"#,
				"exactly one selector",
			),
			(
				"multiple actions",
				r#"
[[resolutions]]
file = "events/PirateEvents.txt"
prefer_mod = "123"
use_file = "manual/PirateEvents.txt"
"#,
				"exactly one action",
			),
			(
				"mod without priority boost",
				r#"
[[resolutions]]
mod = "123"
prefer_mod = "123"
"#,
				"mod selector requires priority_boost",
			),
			(
				"keep existing without file",
				r#"
[[resolutions]]
conflict_id = "ab12cd34"
keep_existing = true
"#,
				"keep_existing action requires file selector",
			),
		];

		for (name, content, expected) in cases {
			let err = FochConfig::from_toml_str(content).expect_err(name);
			let message = err.to_string();
			assert!(
				message.contains(expected),
				"{name} error should contain {expected:?}, error was: {message}"
			);
		}
	}

	#[test]
	fn compute_conflict_id_is_stable_and_input_sensitive() {
		let base = compute_conflict_id(Path::new("events/PirateEvents.txt"), "root/event", "id");

		assert_eq!(base.len(), 8);
		assert!(base.chars().all(|c| c.is_ascii_hexdigit()));
		assert_eq!(
			base,
			compute_conflict_id(Path::new("events/PirateEvents.txt"), "root/event", "id")
		);
		assert_ne!(
			base,
			compute_conflict_id(Path::new("events/OtherEvents.txt"), "root/event", "id")
		);
		assert_ne!(
			base,
			compute_conflict_id(Path::new("events/PirateEvents.txt"), "root/other", "id")
		);
		assert_ne!(
			base,
			compute_conflict_id(Path::new("events/PirateEvents.txt"), "root/event", "other")
		);
	}

	#[test]
	fn resolution_map_lookup_prefers_conflict_id_over_file() {
		let config = FochConfig::from_toml_str(
			r#"
[[resolutions]]
file = "events/PirateEvents.txt"
prefer_mod = "file-mod"

[[resolutions]]
conflict_id = "ab12cd34"
prefer_mod = "conflict-mod"
"#,
		)
		.expect("parse config");
		let map = ResolutionMap::from_entries(&config.resolutions).expect("build resolution map");

		let conflict_decision = ResolutionDecision::PreferMod("conflict-mod".to_owned());
		let file_decision = ResolutionDecision::PreferMod("file-mod".to_owned());
		assert_eq!(
			map.lookup(Path::new("events/PirateEvents.txt"), "ab12cd34"),
			Some(&conflict_decision)
		);
		assert_eq!(
			map.lookup(Path::new("events/PirateEvents.txt"), "unknown"),
			Some(&file_decision)
		);
		assert_eq!(
			map.lookup(Path::new("events/OtherEvents.txt"), "unknown"),
			None
		);
	}

	#[test]
	fn load_merges_multiple_existing_sources() {
		let temp = TempDir::new().expect("temp dir");
		let first = temp.path().join("first.toml");
		let second = temp.path().join("second.toml");
		fs::write(&first, "[[overrides]]\nmod = \"a\"\ndep = \"b\"\n").expect("write first");
		fs::write(&second, "[[overrides]]\nmod = \"c\"\ndep = \"d\"\n").expect("write second");

		let config = FochConfig::try_load_from_paths(vec![first, second]).expect("load configs");

		assert_eq!(
			config.overrides,
			vec![DepOverride::new("a", "b"), DepOverride::new("c", "d")]
		);
	}

	#[test]
	fn search_paths_excludes_engine_config_toml() {
		let temp = TempDir::new().expect("temp dir");
		let paths = FochConfig::search_paths(temp.path());
		// FochConfig (overrides/resolutions) 不能撞到 foch_engine::Config
		// 占用的 ~/.config/foch/config.toml 文件,否则两个 schema 互相 deny_unknown_fields。
		for path in &paths {
			assert!(
				!path.ends_with("config.toml"),
				"FochConfig 不应扫描 config.toml 文件名(归 foch_engine::Config 所有);命中:{:?}",
				path
			);
		}
	}
}
