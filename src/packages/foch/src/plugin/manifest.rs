//! `foch-plugin.toml`: what a plugin package declares about itself.
//!
//! A manifest is the plugin's identity, its compatibility range, its ordering
//! relative to other plugins, and how Foch reads its configuration and status.
//! Foch resolves order and shows the management UI from manifests alone,
//! without loading any DLL. The manifest never proves runtime behavior; a
//! plugin still checks the running game itself.

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The only manifest schema this build understands.
pub const SCHEMA: u32 = 1;

/// The canonical manifest file name inside a plugin package.
pub const FILE_NAME: &str = "foch-plugin.toml";

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
	/// An existing DLL Foch drives through a built-in adapter.
	Legacy,
	/// A DLL that implements the Foch plugin ABI itself.
	Native,
}

/// When in game startup a plugin must be loaded.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
	ProcessAttach,
	Entry,
	Deferred,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
	pub schema: u32,
	pub plugin: PluginMeta,
	pub target: Target,
	pub entry: Entry,
	#[serde(default)]
	pub files: Vec<FileEntry>,
	#[serde(default)]
	pub depends: Vec<Dependency>,
	#[serde(default)]
	pub conflicts: Vec<Conflict>,
	/// Plugin ids this one must load after / before, beyond hard dependencies.
	#[serde(default)]
	pub load_after: Vec<String>,
	#[serde(default)]
	pub load_before: Vec<String>,
	#[serde(default)]
	pub lifecycle: Lifecycle,
	#[serde(default)]
	pub config: BTreeMap<String, ConfigField>,
	/// Writable locations the plugin needs, so the runtime layer can provide
	/// them without writing through to the install.
	#[serde(default)]
	pub writes: Vec<WriteKind>,
	/// Package files the game or DLL reads from the executable directory.
	/// Staging copies them there after INI configuration is written.
	/// `plugin64.dll` reads `<exe>\plugins\plugin.ini`, not the DLL directory.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub exe_relative: Vec<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub status: Option<StatusRule>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct PluginMeta {
	/// Stable identity: the explicit `id` field, or `<owner>/<repo>`
	/// derived from `repository` when the manifest carries no id. Never
	/// changes across versions.
	pub id: String,
	pub name: String,
	pub version: Version,
	#[serde(default)]
	pub authors: Vec<String>,
	#[serde(default)]
	pub license: Option<String>,
	#[serde(default)]
	pub description: Option<String>,
	/// The plugin's source repository; the identity derives from it when
	/// `id` is absent. Accepts `https://host/owner/repo[.git]` and the
	/// ssh form `git@host:owner/repo[.git]`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub repository: Option<String>,
}

/// `<owner>/<repo>` from a repository URL, the identity a manifest
/// without an explicit `id` carries.
pub fn id_from_repository(repository: &str) -> Result<String, String> {
	let mut rest = repository.trim().trim_end_matches(".git");
	if let Some((_, after)) = rest.split_once("://") {
		rest = after;
	} else if let Some((_, after)) = rest.split_once(':') {
		// The ssh form, `git@host:owner/repo`.
		rest = after;
	}
	let mut segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
	// A full URL carries its host first (`github.com/owner/repo`); the
	// ssh remainder and bare paths are already `owner/repo`.
	if segments.first().is_some_and(|first| first.contains('.')) {
		if segments.len() < 3 {
			return Err(format!(
				"repository {repository:?} carries no <owner>/<repo> path to derive the id from"
			));
		}
		segments.remove(0);
	}
	if segments.len() != 2 {
		return Err(format!(
			"repository {repository:?} carries no <owner>/<repo> path to derive the id from"
		));
	}
	let (owner, repo) = (segments[0], segments[1]);
	if [owner, repo].iter().any(|part| {
		part.is_empty()
			|| part.starts_with('.')
			|| part.ends_with('.')
			|| !part
				.bytes()
				.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
	}) {
		return Err(format!("repository {repository:?} derives an invalid id"));
	}
	Ok(format!("{owner}/{repo}"))
}

impl<'de> Deserialize<'de> for PluginMeta {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		#[derive(Deserialize)]
		#[serde(deny_unknown_fields)]
		struct Raw {
			id: Option<String>,
			name: String,
			version: Version,
			#[serde(default)]
			authors: Vec<String>,
			#[serde(default)]
			license: Option<String>,
			#[serde(default)]
			description: Option<String>,
			#[serde(default)]
			repository: Option<String>,
		}
		let raw = Raw::deserialize(deserializer)?;
		let id = match raw.id {
			Some(id) => id,
			None => id_from_repository(raw.repository.as_deref().unwrap_or(""))
				.map_err(serde::de::Error::custom)?,
		};
		Ok(PluginMeta {
			id,
			name: raw.name,
			version: raw.version,
			authors: raw.authors,
			license: raw.license,
			description: raw.description,
			repository: raw.repository,
		})
	}
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Target {
	/// Platform tuple, e.g. `windows-x86_64`. Only that value is supported now.
	pub platform: String,
	/// Game key, e.g. `eu4`.
	pub game: String,
	/// Declared compatible game versions.
	pub game_versions: VersionReq,
	/// For native plugins, the ABI major version the plugin implements.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub abi_major: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
	pub kind: Kind,
	/// Entry DLL path, relative to the package root.
	pub path: String,
	pub phase: Phase,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
	pub path: String,
	/// Lowercase hex SHA-256 of the file's bytes.
	pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
	pub id: String,
	#[serde(default = "version_req_star")]
	pub version: VersionReq,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
	pub id: String,
	#[serde(default = "version_req_star")]
	pub version: VersionReq,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
	/// A changed enable state or config takes effect only on the next launch.
	#[serde(default = "default_true")]
	pub restart_required: bool,
	/// The plugin accepts config changes while running (ABI capability).
	#[serde(default)]
	pub runtime_config: bool,
	/// The plugin accepts enable/disable while running (ABI capability).
	#[serde(default)]
	pub runtime_toggle: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigField {
	Bool {
		#[serde(default)]
		default: bool,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		description: Option<String>,
		/// For a legacy plugin, where Foch writes this value.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		adapter: Option<ConfigAdapter>,
	},
	Int {
		#[serde(default)]
		default: i64,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		min: Option<i64>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		max: Option<i64>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		description: Option<String>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		adapter: Option<ConfigAdapter>,
	},
	String {
		#[serde(default)]
		default: String,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		description: Option<String>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		adapter: Option<ConfigAdapter>,
	},
}

/// How a configuration value reaches a legacy plugin: a key in an INI file,
/// relative to the plugin's own directory. DLL destinations are refused
/// during package validation, before any configuration is staged.
/// A bool writes `1`/`0` unless `yes` and `no` name the words the DLL compares.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigAdapter {
	pub ini: String,
	pub section: String,
	pub key: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub yes: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub no: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WriteKind {
	/// `<exe>\gfx\fonts\<name>` the plugin generates and the game reads back.
	FontCache,
	/// The plugin's own directory under `plugins\`.
	PluginDir,
}

/// How a legacy plugin's status export is read and interpreted.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StatusRule {
	pub export: String,
	pub mode: StatusMode,
	#[serde(default)]
	pub timeout_ms: u64,
	#[serde(default)]
	pub pending: Vec<i32>,
	pub values: Vec<StatusValue>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StatusMode {
	Sync,
	Poll,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StatusValue {
	pub value: i32,
	pub state: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reason: Option<String>,
}

fn default_true() -> bool {
	true
}

fn version_req_star() -> VersionReq {
	VersionReq::STAR
}

fn ini_token(text: &str) -> bool {
	!text.is_empty() && !text.chars().any(|c| matches!(c, '\r' | '\n' | '\0'))
}

/// Why a manifest is not usable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
	Parse(String),
	UnsupportedSchema(u32),
	Invalid(String),
}

impl std::fmt::Display for ManifestError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Parse(message) => write!(f, "could not parse manifest: {message}"),
			Self::UnsupportedSchema(found) => {
				write!(
					f,
					"manifest schema {found} is not supported (this build reads {SCHEMA})"
				)
			}
			Self::Invalid(message) => write!(f, "invalid manifest: {message}"),
		}
	}
}

impl std::error::Error for ManifestError {}

impl Manifest {
	/// Parse, validate and normalize package paths from TOML text.
	pub fn parse(text: &str) -> Result<Self, ManifestError> {
		let mut manifest: Manifest =
			toml::from_str(text).map_err(|error| ManifestError::Parse(error.to_string()))?;
		manifest.validate()?;
		manifest.entry.path = super::store::safe_relative(&manifest.entry.path)
			.map_err(|error| ManifestError::Invalid(error.to_string()))?;
		for file in &mut manifest.files {
			file.path = super::store::safe_relative(&file.path)
				.map_err(|error| ManifestError::Invalid(error.to_string()))?;
		}
		for relative in &mut manifest.exe_relative {
			*relative = super::store::safe_relative(relative)
				.map_err(|error| ManifestError::Invalid(error.to_string()))?;
		}
		Ok(manifest)
	}

	fn validate(&self) -> Result<(), ManifestError> {
		if self.schema != SCHEMA {
			return Err(ManifestError::UnsupportedSchema(self.schema));
		}
		let id = &self.plugin.id;
		// Ids are `owner/plugin` style (derived from a repository) or a
		// plain identifier; each `/`-separated segment must be a safe
		// identifier, so the id stays a safe relative store path.
		let valid_segment = |segment: &str| {
			!segment.is_empty()
				&& !segment.starts_with('.')
				&& !segment.ends_with('.')
				&& segment
					.bytes()
					.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
		};
		if id.is_empty()
			|| id.len() > 256
			|| id.starts_with('/')
			|| id.ends_with('/')
			|| !id.split('/').all(valid_segment)
			|| super::store::safe_relative(id).is_err()
		{
			return Err(ManifestError::Invalid(
				"plugin id must be `/`-separated safe identifiers of 1..256 ASCII bytes".into(),
			));
		}
		if !self.entry.path.to_ascii_lowercase().ends_with(".dll") {
			return Err(ManifestError::Invalid(format!(
				"{id}: entry must be a DLL path"
			)));
		}
		if self.depends.iter().any(|dependency| dependency.id == *id) {
			return Err(ManifestError::Invalid(format!("{id} depends on itself")));
		}
		if self.conflicts.iter().any(|conflict| conflict.id == *id) {
			return Err(ManifestError::Invalid(format!(
				"{id} conflicts with itself"
			)));
		}
		if let (Kind::Native, None) = (&self.entry.kind, self.target.abi_major) {
			return Err(ManifestError::Invalid(format!(
				"{id}: a native plugin must declare target.abi_major"
			)));
		}
		// A legacy DLL may omit [status]. plugin64.dll has no status export;
		// the host then records unknown after the DLL loads.
		if let Some(status) = &self.status {
			if self.entry.kind != Kind::Legacy
				|| status.export.is_empty()
				|| !status.export.is_ascii()
				|| status.export.contains('\0')
			{
				return Err(ManifestError::Invalid(format!(
					"{id}: invalid legacy status export"
				)));
			}
			if status.mode == StatusMode::Poll
				&& (status.timeout_ms == 0
					|| status.timeout_ms > 120_000
					|| status.pending.is_empty())
			{
				return Err(ManifestError::Invalid(format!(
					"{id}: polling requires a bounded timeout and pending values"
				)));
			}
			let mut values = std::collections::BTreeSet::new();
			for value in &status.values {
				if !values.insert(value.value)
					|| !matches!(
						value.state.as_str(),
						"active" | "inactive" | "refused" | "failed" | "unknown"
					) {
					return Err(ManifestError::Invalid(format!(
						"{id}: invalid legacy status mapping"
					)));
				}
			}
		}
		for (name, field) in &self.config {
			let adapter = match field {
				ConfigField::Bool { adapter, .. }
				| ConfigField::Int { adapter, .. }
				| ConfigField::String { adapter, .. } => adapter,
			};
			if let Some(adapter) = adapter {
				let ini = super::store::safe_relative(&adapter.ini).map_err(|_| {
					ManifestError::Invalid(format!("{id}: unsafe INI adapter path for {name}"))
				})?;
				if ini.to_ascii_lowercase().ends_with(".dll") {
					return Err(ManifestError::Invalid(format!(
						"{id}: INI adapter destination for {name} must not be a DLL"
					)));
				}
				match (&adapter.yes, &adapter.no) {
					(None, None) => {}
					(Some(yes), Some(no)) => {
						if !matches!(field, ConfigField::Bool { .. }) {
							return Err(ManifestError::Invalid(format!(
								"{id}: yes/no INI words are only valid on bool field {name}"
							)));
						}
						if !ini_token(yes) || !ini_token(no) {
							return Err(ManifestError::Invalid(format!(
								"{id}: yes/no words for {name} must be a single line"
							)));
						}
					}
					_ => {
						return Err(ManifestError::Invalid(format!(
							"{id}: bool INI adapter for {name} must set both yes and no"
						)));
					}
				}
			}
			if let ConfigField::Int {
				default, min, max, ..
			} = field && (min.is_some_and(|min| *default < min)
				|| max.is_some_and(|max| *default > max)
				|| min.zip(*max).is_some_and(|(min, max)| min > max))
			{
				return Err(ManifestError::Invalid(format!(
					"{id}: invalid default or bounds for {name}"
				)));
			}
		}
		let mut exe_relative = std::collections::BTreeSet::new();
		for relative in &self.exe_relative {
			let path = super::store::safe_relative(relative).map_err(|_| {
				ManifestError::Invalid(format!("{id}: unsafe exe-relative path {relative}"))
			})?;
			let lower = path.to_ascii_lowercase();
			if super::store::is_proxy_name(&path)
				|| lower.ends_with(".dll")
				|| lower.ends_with(".exe")
				|| lower.ends_with(".bat")
			{
				return Err(ManifestError::Invalid(format!(
					"{id}: exe-relative path {path} would publish another loader"
				)));
			}
			if !exe_relative.insert(lower) {
				return Err(ManifestError::Invalid(format!(
					"{id}: duplicate exe-relative path {path}"
				)));
			}
		}
		Ok(())
	}

	/// The default configuration values as a JSON object, from the schema.
	pub fn default_config(&self) -> serde_json::Value {
		let mut map = serde_json::Map::new();
		for (name, field) in &self.config {
			let value = match field {
				ConfigField::Bool { default, .. } => serde_json::Value::Bool(*default),
				ConfigField::Int { default, .. } => serde_json::Value::from(*default),
				ConfigField::String { default, .. } => serde_json::Value::from(default.clone()),
			};
			map.insert(name.clone(), value);
		}
		serde_json::Value::Object(map)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn repository_derives_the_identity_when_id_is_absent() {
		for (repository, id) in [
			(
				"https://github.com/Acture/eu4-dynamic-trade-blackhole",
				"Acture/eu4-dynamic-trade-blackhole",
			),
			(
				"https://github.com/Acture/eu4-dynamic-trade-blackhole.git",
				"Acture/eu4-dynamic-trade-blackhole",
			),
			(
				"git@github.com:Acture/eu4-dynamic-trade-blackhole.git",
				"Acture/eu4-dynamic-trade-blackhole",
			),
		] {
			assert_eq!(id_from_repository(repository).unwrap(), id);
		}
		assert!(id_from_repository("https://github.com/Acture").is_err());
		assert!(id_from_repository("not a url").is_err());
	}

	#[test]
	fn a_manifest_without_id_or_repository_is_rejected() {
		let text = UNICODE.replace(
			"id = \"io.github.yozoratempest.eu4-unicode-patch\"
",
			"",
		);
		let Err(error) = toml::from_str::<Manifest>(&text) else {
			panic!("identity-less manifest accepted");
		};
		assert!(error.to_string().contains("derive"));
	}

	#[test]
	fn owner_slash_ids_validate_and_bad_segments_do_not() {
		let mut manifest: Manifest = toml::from_str(UNICODE).unwrap();
		manifest.plugin.id = "Acture/eu4-dynamic-trade-blackhole".to_string();
		assert!(manifest.validate().is_ok());
		for bad in ["a//b", "/a", "a/", "a/./b", "a/../b", "a/ b"] {
			manifest.plugin.id = bad.to_string();
			assert!(manifest.validate().is_err(), "{bad:?} accepted");
		}
	}

	const UNICODE: &str = r#"
schema = 1

[plugin]
id = "io.github.yozoratempest.eu4-unicode-patch"
name = "EU4 Unicode Patch"
version = "0.1.14"
authors = ["YozoraTempest"]
license = "MIT"

[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "=1.37.5"

[entry]
kind = "legacy"
path = "plugins/eu4_unicode_patch.dll"
phase = "entry"

[[files]]
path = "plugins/eu4_unicode_patch.dll"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"

[lifecycle]
restart_required = true

[config.typo_tolerance]
type = "int"
default = 1
min = 0
max = 2
adapter = { ini = "plugins/eu4_unicode_patch/config.ini", section = "search", key = "typo_tolerance" }

[status]
export = "Eu4UnicodeProbeEnabled"
mode = "sync"
values = [
	{ value = 1, state = "active" },
	{ value = 0, state = "refused", reason = "see eu4_unicode_patch.log" },
]
"#;

	#[test]
	fn ini_adapters_reject_windows_path_aliases() {
		for path in [
			".. /outside.ini",
			"config.ini.",
			"NUL.ini",
			"config.ini:extra",
			"a\0.ini",
		] {
			let mut manifest = Manifest::parse(UNICODE).unwrap();
			let ConfigField::Int {
				adapter: Some(adapter),
				..
			} = manifest.config.get_mut("typo_tolerance").unwrap()
			else {
				panic!("missing adapter")
			};
			adapter.ini = path.into();
			assert!(
				Manifest::parse(&toml::to_string(&manifest).unwrap()).is_err(),
				"{path:?}"
			);
		}
	}

	#[test]
	fn parses_a_legacy_manifest() {
		let manifest = Manifest::parse(UNICODE).unwrap();
		assert_eq!(
			manifest.plugin.id,
			"io.github.yozoratempest.eu4-unicode-patch"
		);
		assert_eq!(manifest.entry.kind, Kind::Legacy);
		assert_eq!(manifest.entry.phase, Phase::Entry);
		let status = manifest.status.as_ref().unwrap();
		assert_eq!(status.values.len(), 2);
		assert_eq!(manifest.default_config()["typo_tolerance"], 1);
	}

	#[test]
	fn plugin_ids_reject_windows_device_names_and_preserve_safe_identifiers() {
		let mut manifest = Manifest::parse(UNICODE).unwrap();
		for id in [
			"CON",
			"con",
			"Con.plugin",
			"NUL",
			"nul.dll",
			"PRN",
			"AUX",
			"COM1",
			"com9.sample",
			"LPT1",
			"lpt9.sample",
			"name.",
		] {
			manifest.plugin.id = id.into();
			let error = Manifest::parse(&toml::to_string(&manifest).unwrap()).unwrap_err();
			assert!(matches!(error, ManifestError::Invalid(_)), "{id}: {error}");
			assert!(error.to_string().contains("plugin id"), "{id}: {error}");
		}
		for id in [
			"dev.foch.sample",
			"com.example.plugin",
			"CONSOLE",
			"NUL_plugin",
			"COM0",
			"COM10",
			"LPT0",
			"LPT10",
		] {
			manifest.plugin.id = id.into();
			assert_eq!(
				Manifest::parse(&toml::to_string(&manifest).unwrap())
					.unwrap()
					.plugin
					.id,
				id
			);
		}
	}

	#[test]
	fn a_legacy_plugin_may_omit_its_status_rule() {
		let text = UNICODE.split("[status]").next().unwrap();
		let manifest = Manifest::parse(text).unwrap();
		assert!(manifest.status.is_none());
		let unknown = UNICODE.replace("[status]", "[ignored]");
		assert!(Manifest::parse(&unknown).is_err());
	}

	#[test]
	fn a_native_plugin_needs_an_abi_major() {
		let text = r#"
schema = 1
[plugin]
id = "dev.foch.sample"
name = "Sample"
version = "0.1.0"
[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "*"
[entry]
kind = "native"
path = "sample.dll"
phase = "deferred"
"#;
		assert!(matches!(
			Manifest::parse(text).unwrap_err(),
			ManifestError::Invalid(_)
		));
		let ok = text.replace(
			"game_versions = \"*\"",
			"game_versions = \"*\"\nabi_major = 1",
		);
		assert_eq!(Manifest::parse(&ok).unwrap().target.abi_major, Some(1));
	}

	#[test]
	fn an_unsupported_schema_is_rejected() {
		let text = UNICODE.replace("schema = 1", "schema = 2");
		assert_eq!(
			Manifest::parse(&text).unwrap_err(),
			ManifestError::UnsupportedSchema(2)
		);
	}

	#[test]
	fn roundtrips_through_toml() {
		let manifest = Manifest::parse(UNICODE).unwrap();
		let text = toml::to_string(&manifest).unwrap();
		assert_eq!(Manifest::parse(&text).unwrap(), manifest);
	}

	#[test]
	fn refuses_status_rules_and_identities_the_host_cannot_execute() {
		for text in [
			UNICODE.replace("mode = \"sync\"", "mode = \"poll\""),
			UNICODE.replace("state = \"active\"", "state = \"healthy\""),
			UNICODE.replace("{ value = 0,", "{ value = 1,"),
			UNICODE.replace("eu4_unicode_patch.dll", "eu4_unicode_patch.bin"),
			UNICODE.replace("io.github.yozoratempest.eu4-unicode-patch", "../escape"),
			UNICODE.replace(
				"io.github.yozoratempest.eu4-unicode-patch",
				&"a".repeat(257),
			),
			UNICODE
				.replace("kind = \"legacy\"", "kind = \"native\"")
				.replace("game = \"eu4\"", "game = \"eu4\"\nabi_major = 1"),
		] {
			assert!(Manifest::parse(&text).is_err(), "accepted {text}");
		}
	}
}
