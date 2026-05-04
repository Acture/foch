use globset::{Glob, GlobMatcher};
use regex::Regex;
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
	/// Pattern selector DSL: `<file_side>::<addr_side>?`. Each side defaults
	/// to a glob; prefix `re:` switches that side to a regex. The address
	/// side is optional — if absent (or empty after `::`), the rule matches
	/// on file path alone, applying to every leaf in matching files.
	///
	/// Examples:
	/// - `common/ideas/**` — every leaf inside any file under common/ideas/
	/// - `common/ideas/**::xx_idea_*` — only leaves whose address matches
	/// - `re:^events/.*\.txt$::re:^test\..*` — both sides regex
	#[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
	pub r#match: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub prefer_mod: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub use_file: Option<PathBuf>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub keep_existing: Option<bool>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub priority_boost: Option<i32>,
	/// Named handler from the merge handler registry (e.g. `last_writer`,
	/// `defer`, `keep_existing`). Only valid alongside the `match` selector.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub handler: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolutionMap {
	pub by_file: HashMap<PathBuf, ResolutionDecision>,
	pub by_conflict_id: HashMap<String, ResolutionDecision>,
	pub mod_priority_boost: HashMap<String, i32>,
	pub pattern_rules: Vec<PatternRule>,
}

impl PartialEq for ResolutionMap {
	fn eq(&self, other: &Self) -> bool {
		self.by_file == other.by_file
			&& self.by_conflict_id == other.by_conflict_id
			&& self.mod_priority_boost == other.mod_priority_boost
			&& self.pattern_rules.len() == other.pattern_rules.len()
			&& self
				.pattern_rules
				.iter()
				.zip(other.pattern_rules.iter())
				.all(|(a, b)| a.dsl == b.dsl && a.decision == b.decision)
	}
}

impl Eq for ResolutionMap {}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ResolutionDecision {
	PreferMod(String),
	UseFile(PathBuf),
	KeepExisting,
	/// Dispatch to a named handler in the merge handler registry.
	Handler(String),
}

/// Compiled pattern rule from a `[[resolutions]]` entry that uses the
/// `match` selector. Holds the original DSL string for diagnostics plus the
/// pre-compiled file/leaf matchers ready for lookup-time use.
#[derive(Clone, Debug)]
pub struct PatternRule {
	pub dsl: String,
	pub file_matcher: Matcher,
	pub leaf_matcher: Option<Matcher>,
	pub decision: ResolutionDecision,
}

impl PatternRule {
	/// Returns true when this rule covers the given (file, leaf_address).
	/// `leaf_address` may be empty when the caller has no per-leaf identity
	/// (e.g. file-only resolutions); rules with a leaf matcher then never
	/// match.
	pub fn matches(&self, file: &Path, leaf_address: &str) -> bool {
		let file_str = file.to_string_lossy().replace('\\', "/");
		if !self.file_matcher.is_match(&file_str) {
			return false;
		}
		match &self.leaf_matcher {
			None => true,
			Some(matcher) => !leaf_address.is_empty() && matcher.is_match(leaf_address),
		}
	}
}

/// Matches a single side of a pattern rule. Globs are compiled via
/// [`globset`]; the `re:` prefix switches to a [`regex::Regex`].
#[derive(Clone, Debug)]
pub enum Matcher {
	Glob(GlobMatcher),
	Regex(Regex),
}

impl Matcher {
	pub fn is_match(&self, value: &str) -> bool {
		match self {
			Self::Glob(matcher) => matcher.is_match(value),
			Self::Regex(regex) => regex.is_match(value),
		}
	}
}

/// Splits a DSL `<file_side>::<addr_side>?` into a file matcher plus an
/// optional leaf matcher. Each side independently honors the `re:` prefix.
/// An empty address side (`"events/**::"` or trailing `::`) is treated as
/// "no address constraint" — same as omitting `::` entirely.
pub fn parse_match_dsl(input: &str) -> Result<(Matcher, Option<Matcher>), ConfigError> {
	let trimmed = input.trim();
	if trimmed.is_empty() {
		return Err(ConfigError::new("match pattern cannot be empty"));
	}
	let (file_side, addr_side) = match trimmed.split_once("::") {
		Some((file, addr)) => {
			let addr = addr.trim();
			(file.trim(), if addr.is_empty() { None } else { Some(addr) })
		}
		None => (trimmed, None),
	};
	if file_side.is_empty() {
		return Err(ConfigError::new(
			"match pattern file side cannot be empty (use `**` to match everything)",
		));
	}
	let file_matcher = parse_pattern_side(file_side)?;
	let leaf_matcher = match addr_side {
		Some(side) => Some(parse_pattern_side(side)?),
		None => None,
	};
	Ok((file_matcher, leaf_matcher))
}

fn parse_pattern_side(side: &str) -> Result<Matcher, ConfigError> {
	if let Some(re) = side.strip_prefix("re:") {
		let re = re.trim();
		if re.is_empty() {
			return Err(ConfigError::new(
				"regex pattern side cannot be empty after `re:` prefix",
			));
		}
		Regex::new(re)
			.map(Matcher::Regex)
			.map_err(|err| ConfigError::new(format!("invalid regex `{re}`: {err}")))
	} else {
		Glob::new(side)
			.map(|glob| Matcher::Glob(glob.compile_matcher()))
			.map_err(|err| ConfigError::new(format!("invalid glob `{side}`: {err}")))
	}
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
			} else if let Some(dsl) = &entry.r#match {
				let (file_matcher, leaf_matcher) = parse_match_dsl(dsl)
					.map_err(|err| ConfigError::resolution_entry(index, err.message()))?;
				map.pattern_rules.push(PatternRule {
					dsl: dsl.clone(),
					file_matcher,
					leaf_matcher,
					decision,
				});
			}
		}
		Ok(map)
	}

	/// Look up a resolution for the given (file, conflict_id, leaf_address).
	///
	/// Precedence (first match wins):
	/// 1. exact `conflict_id` match (per-leaf hash from foch.toml)
	/// 2. exact `file` match (whole-file resolutions from foch.toml)
	/// 3. pattern rules in declaration order
	///
	/// `leaf_address` should follow the canonical `path/key` shape (e.g.
	/// `flavor_fra.3135/option/define_advisor/name`); pass an empty string
	/// when the caller has no leaf identity (file-only lookups still work
	/// against the first two layers and against pattern rules with no
	/// address side).
	pub fn lookup(
		&self,
		file: &Path,
		conflict_id: &str,
		leaf_address: &str,
	) -> Option<&ResolutionDecision> {
		if let Some(decision) = self.by_conflict_id.get(conflict_id) {
			return Some(decision);
		}
		if let Some(decision) = self.by_file.get(file) {
			return Some(decision);
		}
		self.pattern_rules
			.iter()
			.find(|rule| rule.matches(file, leaf_address))
			.map(|rule| &rule.decision)
	}
}

impl ResolutionEntry {
	fn validate(&self, index: usize) -> Result<(), ConfigError> {
		if self.selector_count() != 1 {
			return Err(ConfigError::resolution_entry(
				index,
				"exactly one selector (file, conflict_id, mod, match) must be set",
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
					"priority_boost cannot be combined with prefer_mod, use_file, keep_existing, or handler",
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
		if self.handler.is_some() && self.r#match.is_none() {
			return Err(ConfigError::resolution_entry(
				index,
				"handler action requires match selector",
			));
		}
		if self.all_action_count() != 1 {
			return Err(ConfigError::resolution_entry(
				index,
				"exactly one action (prefer_mod, use_file, keep_existing, handler) must be set",
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
		option_count(&self.file)
			+ option_count(&self.conflict_id)
			+ option_count(&self.mod_id)
			+ option_count(&self.r#match)
	}

	fn standard_action_count(&self) -> usize {
		option_count(&self.prefer_mod)
			+ option_count(&self.use_file)
			+ option_count(&self.keep_existing)
			+ option_count(&self.handler)
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
		} else if let Some(handler) = &self.handler {
			ResolutionDecision::Handler(handler.clone())
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
			map.lookup(Path::new("events/PirateEvents.txt"), "ab12cd34", ""),
			Some(&conflict_decision)
		);
		assert_eq!(
			map.lookup(Path::new("events/PirateEvents.txt"), "unknown", ""),
			Some(&file_decision)
		);
		assert_eq!(
			map.lookup(Path::new("events/OtherEvents.txt"), "unknown", ""),
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

	// ---------------------------------------------------------------
	// Pattern DSL + Handler decision tests (p-pattern + p-decision)
	// ---------------------------------------------------------------

	#[test]
	fn pattern_dsl_pure_glob_matches_file_only() {
		let (file_matcher, leaf_matcher) = parse_match_dsl("common/ideas/**").expect("parse glob");
		assert!(file_matcher.is_match("common/ideas/foo.txt"));
		assert!(file_matcher.is_match("common/ideas/sub/bar.txt"));
		assert!(!file_matcher.is_match("events/foo.txt"));
		assert!(leaf_matcher.is_none());
	}

	#[test]
	fn pattern_dsl_glob_address_side() {
		let (file_matcher, leaf_matcher) =
			parse_match_dsl("common/ideas/**::xx_idea_*").expect("parse mixed");
		assert!(file_matcher.is_match("common/ideas/national.txt"));
		let leaf = leaf_matcher.expect("address side present");
		assert!(leaf.is_match("xx_idea_pirates"));
		assert!(!leaf.is_match("yy_idea_pirates"));
	}

	#[test]
	fn pattern_dsl_pure_regex() {
		let (file_matcher, leaf_matcher) =
			parse_match_dsl("re:^events/.*\\.txt$").expect("parse regex");
		assert!(file_matcher.is_match("events/PirateEvents.txt"));
		assert!(!file_matcher.is_match("events/PirateEvents.json"));
		assert!(leaf_matcher.is_none());
	}

	#[test]
	fn pattern_dsl_mixed_glob_file_regex_address() {
		let (file_matcher, leaf_matcher) =
			parse_match_dsl("common/**::re:^test\\..*").expect("parse mixed");
		assert!(file_matcher.is_match("common/ideas/anything.txt"));
		let leaf = leaf_matcher.expect("address side present");
		assert!(leaf.is_match("test.123"));
		assert!(!leaf.is_match("other.123"));
	}

	#[test]
	fn pattern_dsl_empty_address_side_treated_as_no_constraint() {
		let (file_matcher, leaf_matcher) =
			parse_match_dsl("events/**::").expect("parse trailing colons");
		assert!(file_matcher.is_match("events/foo.txt"));
		assert!(leaf_matcher.is_none());
	}

	#[test]
	fn pattern_dsl_rejects_empty_input() {
		assert!(parse_match_dsl("").is_err());
		assert!(parse_match_dsl("   ").is_err());
	}

	#[test]
	fn pattern_dsl_rejects_empty_file_side() {
		let err = parse_match_dsl("::xx_idea_*").expect_err("empty file side rejected");
		assert!(err.to_string().contains("file side"));
	}

	#[test]
	fn pattern_dsl_rejects_empty_regex() {
		let err = parse_match_dsl("re:").expect_err("empty regex rejected");
		assert!(err.to_string().contains("regex"));
	}

	#[test]
	fn pattern_dsl_rejects_invalid_regex() {
		let err = parse_match_dsl("re:[unterminated").expect_err("bad regex rejected");
		assert!(err.to_string().contains("invalid regex"));
	}

	#[test]
	fn match_selector_with_handler_action_round_trips() {
		let config = FochConfig::from_toml_str(
			r#"
[[resolutions]]
match = "common/ideas/**::xx_idea_*"
handler = "last_writer"
"#,
		)
		.expect("parse match+handler");
		let map = ResolutionMap::from_entries(&config.resolutions).expect("build map");
		assert_eq!(map.pattern_rules.len(), 1);
		let rule = &map.pattern_rules[0];
		assert_eq!(rule.dsl, "common/ideas/**::xx_idea_*");
		assert_eq!(
			rule.decision,
			ResolutionDecision::Handler("last_writer".to_string())
		);
		assert!(rule.matches(Path::new("common/ideas/national.txt"), "xx_idea_pirates"));
		assert!(!rule.matches(Path::new("common/ideas/national.txt"), "yy_idea_pirates"));
		assert!(!rule.matches(Path::new("events/foo.txt"), "xx_idea_pirates"));
	}

	#[test]
	fn match_selector_supports_prefer_mod_action_too() {
		let config = FochConfig::from_toml_str(
			r#"
[[resolutions]]
match = "common/ideas/**"
prefer_mod = "ideas-mod"
"#,
		)
		.expect("parse match+prefer_mod");
		let map = ResolutionMap::from_entries(&config.resolutions).expect("build map");
		assert_eq!(map.pattern_rules.len(), 1);
		assert_eq!(
			map.pattern_rules[0].decision,
			ResolutionDecision::PreferMod("ideas-mod".to_string())
		);
	}

	#[test]
	fn rejects_match_combined_with_other_selectors() {
		let cases = [
			r#"
[[resolutions]]
match = "common/**"
file = "common/foo.txt"
prefer_mod = "x"
"#,
			r#"
[[resolutions]]
match = "common/**"
conflict_id = "ab12cd34"
prefer_mod = "x"
"#,
		];
		for content in cases {
			let err =
				FochConfig::from_toml_str(content).expect_err("multi-selector should be rejected");
			assert!(
				err.to_string().contains("exactly one selector"),
				"expected multi-selector error, got {err}"
			);
		}
	}

	#[test]
	fn rejects_handler_without_match_selector() {
		let err = FochConfig::from_toml_str(
			r#"
[[resolutions]]
file = "common/foo.txt"
handler = "last_writer"
"#,
		)
		.expect_err("handler+file should be rejected");
		assert!(
			err.to_string().contains("handler action requires match"),
			"got: {err}"
		);
	}

	#[test]
	fn rejects_handler_combined_with_other_actions() {
		let err = FochConfig::from_toml_str(
			r#"
[[resolutions]]
match = "common/**"
handler = "last_writer"
prefer_mod = "x"
"#,
		)
		.expect_err("handler+prefer_mod should be rejected");
		assert!(err.to_string().contains("exactly one action"), "got: {err}");
	}

	#[test]
	fn rejects_invalid_match_dsl() {
		let err = FochConfig::from_toml_str(
			r#"
[[resolutions]]
match = "re:[unterminated"
handler = "last_writer"
"#,
		)
		.expect_err("invalid regex must surface");
		assert!(err.to_string().contains("invalid regex"), "got: {err}");
	}

	#[test]
	fn lookup_precedence_conflict_id_beats_pattern() {
		let config = FochConfig::from_toml_str(
			r#"
[[resolutions]]
match = "**"
handler = "last_writer"

[[resolutions]]
conflict_id = "abc12345"
prefer_mod = "specific-mod"
"#,
		)
		.expect("parse");
		let map = ResolutionMap::from_entries(&config.resolutions).expect("build map");
		assert_eq!(
			map.lookup(Path::new("anything.txt"), "abc12345", "any/leaf"),
			Some(&ResolutionDecision::PreferMod("specific-mod".to_string()))
		);
	}

	#[test]
	fn lookup_precedence_file_beats_pattern() {
		let config = FochConfig::from_toml_str(
			r#"
[[resolutions]]
match = "**"
handler = "last_writer"

[[resolutions]]
file = "events/foo.txt"
prefer_mod = "file-mod"
"#,
		)
		.expect("parse");
		let map = ResolutionMap::from_entries(&config.resolutions).expect("build map");
		assert_eq!(
			map.lookup(Path::new("events/foo.txt"), "no-id", "any/leaf"),
			Some(&ResolutionDecision::PreferMod("file-mod".to_string()))
		);
		// pattern still applies to non-matching file
		assert_eq!(
			map.lookup(Path::new("events/bar.txt"), "no-id", "any/leaf"),
			Some(&ResolutionDecision::Handler("last_writer".to_string()))
		);
	}

	#[test]
	fn lookup_pattern_rules_evaluated_in_declaration_order() {
		let config = FochConfig::from_toml_str(
			r#"
[[resolutions]]
match = "common/ideas/**::xx_idea_*"
handler = "last_writer"

[[resolutions]]
match = "common/ideas/**"
handler = "defer"
"#,
		)
		.expect("parse");
		let map = ResolutionMap::from_entries(&config.resolutions).expect("build map");
		// xx_idea_* should win; first rule covers it
		assert_eq!(
			map.lookup(
				Path::new("common/ideas/national.txt"),
				"no-id",
				"xx_idea_pirates"
			),
			Some(&ResolutionDecision::Handler("last_writer".to_string()))
		);
		// non-xx_ leaf falls through to second rule
		assert_eq!(
			map.lookup(
				Path::new("common/ideas/national.txt"),
				"no-id",
				"yy_idea_pirates"
			),
			Some(&ResolutionDecision::Handler("defer".to_string()))
		);
	}

	#[test]
	fn lookup_pattern_with_leaf_matcher_skipped_when_address_empty() {
		let config = FochConfig::from_toml_str(
			r#"
[[resolutions]]
match = "common/**::xx_*"
handler = "last_writer"
"#,
		)
		.expect("parse");
		let map = ResolutionMap::from_entries(&config.resolutions).expect("build map");
		// empty leaf address — leaf matcher can't match
		assert_eq!(map.lookup(Path::new("common/foo.txt"), "no-id", ""), None);
	}
}
