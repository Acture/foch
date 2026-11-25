//! The plugin list, enablement, and configuration the CLI, TUI, and desktop share.
//!
//! The overview joins the on-disk store, the built-in registry, and plugins
//! declared by the active playset. Changing a choice rewrites `selections.toml`
//! through [`super::selection::Selections::update`].

use super::companion::DeclaredModPlugin;
use super::manifest::{ConfigField, Manifest};
use super::planner::{self, Diagnostic, GameIdentity};
use super::selection::{Choice, Selections};
use super::{builtin, registry, store};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

/// Why a plugin overview or edit cannot be applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceError {
	message: String,
}

impl ServiceError {
	pub fn new(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
		}
	}

	pub fn message(&self) -> &str {
		&self.message
	}
}

impl std::fmt::Display for ServiceError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.message)
	}
}

impl std::error::Error for ServiceError {}

impl From<io::Error> for ServiceError {
	fn from(error: io::Error) -> Self {
		Self::new(error.to_string())
	}
}

/// How the list should badge one plugin.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginBadge {
	Enabled,
	Installed,
	NotInstalled,
	Conflict,
}

/// One configuration field and the value the playset would use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigFieldSummary {
	pub key: String,
	pub kind: String,
	pub description: Option<String>,
	pub value: toml::Value,
	pub min: Option<i64>,
	pub max: Option<i64>,
}

/// One row of the unified plugin list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginEntrySummary {
	pub id: String,
	pub name: String,
	pub description: Option<String>,
	pub installed_versions: Vec<String>,
	pub selected_version: Option<String>,
	pub enabled: bool,
	pub install_spec: Option<String>,
	pub declared_by_mods: Vec<String>,
	pub required_by_mods: Vec<String>,
	pub conflicts: Vec<String>,
	pub config_fields: Vec<ConfigFieldSummary>,
	pub badge: PluginBadge,
}

/// The frozen load order for one game identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginPlanSummary {
	pub launchable: bool,
	pub order: Vec<String>,
	pub errors: Vec<String>,
	pub warnings: Vec<String>,
}

/// Everything the management UI shows for one playset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginOverview {
	pub playset: String,
	pub plugins: Vec<PluginEntrySummary>,
	pub problems: Vec<String>,
	pub plan: Option<PluginPlanSummary>,
}

/// Join installed packages, the built-in registry, and playset declarations.
pub fn build_overview(
	playset: &str,
	store_root: &Path,
	selections_path: &Path,
	declared_plugins: &[DeclaredModPlugin],
	game_identity: Option<&GameIdentity>,
) -> Result<PluginOverview, ServiceError> {
	let (installed, mut problems) = store::catalog(store_root);
	let (catalog, catalog_problems) = store::catalog_with_defaults(store_root, builtin::adapters());
	problems.extend(catalog_problems);
	let selections = Selections::load(selections_path)?;
	let chosen = selections.for_playset(playset);
	let mut ids = Vec::new();
	for plugin in registry::plugins() {
		push_id(&mut ids, &plugin.id);
	}
	for declared in declared_plugins {
		push_id(&mut ids, &declared.plugin_id);
	}
	let mut extra: Vec<&String> = installed
		.keys()
		.filter(|id| !ids.iter().any(|existing| existing == *id))
		.collect();
	extra.sort();
	for id in extra {
		ids.push(id.clone());
	}

	let mut plugins = Vec::with_capacity(ids.len());
	for id in &ids {
		plugins.push(summarize(
			id,
			&installed,
			&catalog,
			&chosen.plugins,
			declared_plugins,
		));
	}
	mark_enabled_conflicts(&mut plugins);

	let plan = game_identity.map(|game| {
		let resolution = planner::plan(game, &catalog, &chosen.to_selections());
		PluginPlanSummary {
			launchable: resolution.is_launchable(),
			order: resolution
				.order
				.iter()
				.map(|resolved| format!("{} {}", resolved.id, resolved.version))
				.collect(),
			errors: resolution.errors.iter().map(diagnostic_text).collect(),
			warnings: resolution.warnings.iter().map(diagnostic_text).collect(),
		}
	});
	Ok(PluginOverview {
		playset: playset.to_string(),
		plugins,
		problems,
		plan,
	})
}

/// Enable or disable one plugin for a playset.
///
/// `version` pins the choice. When it is absent, the current pin is kept, or
/// the newest installed or built-in version is used. A plugin that is not in
/// the catalog cannot be enabled.
pub fn set_plugin_enabled(
	selections_path: &Path,
	store_root: &Path,
	playset: &str,
	id: &str,
	enabled: bool,
	version: Option<Version>,
) -> Result<(), ServiceError> {
	let (catalog, _) = store::catalog_with_defaults(store_root, builtin::adapters());
	Selections::update(selections_path, |selections| {
		let choices = selections.playsets.entry(playset.to_string()).or_default();
		let existing = choices.plugins.get(id).cloned();
		if !enabled && existing.is_none() && version.is_none() {
			return Ok(());
		}
		let version = version
			.or_else(|| existing.as_ref().map(|choice| choice.version.clone()))
			.or_else(|| newest(catalog.get(id)))
			.ok_or_else(|| {
				io::Error::new(io::ErrorKind::NotFound, format!("unknown plugin {id}"))
			})?;
		let known = catalog
			.get(id)
			.is_some_and(|versions| versions.contains_key(&version));
		if !known {
			return Err(io::Error::new(
				io::ErrorKind::NotFound,
				format!("{id} {version} is not installed or built in"),
			));
		}
		let config = existing.map(|choice| choice.config).unwrap_or_default();
		choices.plugins.insert(
			id.to_string(),
			Choice {
				version,
				enabled,
				config,
			},
		);
		Ok(())
	})?;
	Ok(())
}

/// Validate `value` against the selected manifest field and store it.
pub fn set_plugin_config_value(
	selections_path: &Path,
	store_root: &Path,
	playset: &str,
	id: &str,
	key: &str,
	value: toml::Value,
) -> Result<(), ServiceError> {
	let (catalog, _) = store::catalog_with_defaults(store_root, builtin::adapters());
	Selections::update(selections_path, |selections| {
		let choices = selections.playsets.entry(playset.to_string()).or_default();
		let existing = choices.plugins.get(id).cloned();
		let version = existing
			.as_ref()
			.map(|choice| choice.version.clone())
			.or_else(|| newest(catalog.get(id)))
			.ok_or_else(|| {
				io::Error::new(io::ErrorKind::NotFound, format!("unknown plugin {id}"))
			})?;
		let manifest = catalog
			.get(id)
			.and_then(|versions| versions.get(&version))
			.ok_or_else(|| {
				io::Error::new(
					io::ErrorKind::NotFound,
					format!("{id} {version} is not installed or built in"),
				)
			})?;
		validate_config_value(manifest, key, &value).map_err(io::Error::other)?;
		let enabled = existing
			.as_ref()
			.map(|choice| choice.enabled)
			.unwrap_or(false);
		let mut config = existing.map(|choice| choice.config).unwrap_or_default();
		config.insert(key.to_string(), value.clone());
		choices.plugins.insert(
			id.to_string(),
			Choice {
				version,
				enabled,
				config,
			},
		);
		Ok(())
	})?;
	Ok(())
}

fn push_id(ids: &mut Vec<String>, id: &str) {
	if !ids.iter().any(|existing| existing == id) {
		ids.push(id.to_string());
	}
}

fn newest(versions: Option<&BTreeMap<Version, Manifest>>) -> Option<Version> {
	versions.and_then(|versions| versions.keys().next_back().cloned())
}

fn summarize(
	id: &str,
	installed: &BTreeMap<String, BTreeMap<Version, Manifest>>,
	catalog: &BTreeMap<String, BTreeMap<Version, Manifest>>,
	choices: &BTreeMap<String, Choice>,
	declared_plugins: &[DeclaredModPlugin],
) -> PluginEntrySummary {
	let choice = choices.get(id);
	let declared: Vec<&DeclaredModPlugin> = declared_plugins
		.iter()
		.filter(|plugin| plugin.plugin_id == id)
		.collect();
	let registered = registry::plugins().iter().find(|plugin| plugin.id == id);
	let installed_map = installed.get(id);
	let installed_versions: Vec<String> = installed_map
		.map(|versions| versions.keys().map(ToString::to_string).collect())
		.unwrap_or_default();
	let manifest = choice
		.and_then(|choice| {
			catalog
				.get(id)
				.and_then(|versions| versions.get(&choice.version))
		})
		.or_else(|| installed_map.and_then(|versions| versions.values().next_back()))
		.or_else(|| {
			catalog
				.get(id)
				.and_then(|versions| versions.values().next_back())
		});
	let mut name = registered
		.map(|plugin| plugin.name.clone())
		.or_else(|| manifest.map(|manifest| manifest.plugin.name.clone()))
		.unwrap_or_else(|| id.to_string());
	if name.is_empty() {
		name = id.to_string();
	}
	let description = manifest.and_then(|manifest| manifest.plugin.description.clone());
	let install_spec = registered
		.map(|plugin| plugin.repo.clone())
		.or_else(|| declared.iter().find_map(|plugin| plugin.repo.clone()));
	let mut declared_by_mods = Vec::new();
	let mut required_by_mods = Vec::new();
	for plugin in &declared {
		for source in &plugin.source_mods {
			if !declared_by_mods.contains(source) {
				declared_by_mods.push(source.clone());
			}
			if plugin.required && !required_by_mods.contains(source) {
				required_by_mods.push(source.clone());
			}
		}
	}
	let mut conflicts = Vec::new();
	let mut conflict_ids = BTreeSet::new();
	if let Some(plugin) = registered {
		for conflict in &plugin.conflicts_with {
			conflict_ids.insert(conflict.id.clone());
			conflicts.push(format!("{} ({})", conflict.id, conflict.because));
		}
	}
	if let Some(manifest) = manifest {
		for conflict in &manifest.conflicts {
			if conflict_ids.insert(conflict.id.clone()) {
				match &conflict.reason {
					Some(reason) => conflicts.push(format!("{} ({reason})", conflict.id)),
					None => conflicts.push(conflict.id.clone()),
				}
			}
		}
	}
	let enabled = choice.is_some_and(|choice| choice.enabled);
	let config_fields = manifest
		.map(|manifest| config_fields(manifest, choice))
		.unwrap_or_default();
	let badge = if enabled {
		PluginBadge::Enabled
	} else if !installed_versions.is_empty() {
		PluginBadge::Installed
	} else {
		PluginBadge::NotInstalled
	};
	PluginEntrySummary {
		id: id.to_string(),
		name,
		description,
		installed_versions,
		selected_version: choice.map(|choice| choice.version.to_string()),
		enabled,
		install_spec,
		declared_by_mods,
		required_by_mods,
		conflicts,
		config_fields,
		badge,
	}
}

fn config_fields(manifest: &Manifest, choice: Option<&Choice>) -> Vec<ConfigFieldSummary> {
	manifest
		.config
		.iter()
		.map(|(key, field)| {
			let (kind, min, max, description, default) = match field {
				ConfigField::Bool {
					default,
					description,
					..
				} => (
					"bool",
					None,
					None,
					description.clone(),
					toml::Value::Boolean(*default),
				),
				ConfigField::Int {
					default,
					min,
					max,
					description,
					..
				} => (
					"int",
					*min,
					*max,
					description.clone(),
					toml::Value::Integer(*default),
				),
				ConfigField::String {
					default,
					description,
					..
				} => (
					"string",
					None,
					None,
					description.clone(),
					toml::Value::String(default.clone()),
				),
			};
			let value = choice
				.and_then(|choice| choice.config.get(key))
				.filter(|value| value_matches(kind, min, max, value))
				.cloned()
				.unwrap_or(default);
			ConfigFieldSummary {
				key: key.clone(),
				kind: kind.to_string(),
				description,
				value,
				min,
				max,
			}
		})
		.collect()
}

fn value_matches(kind: &str, min: Option<i64>, max: Option<i64>, value: &toml::Value) -> bool {
	match kind {
		"bool" => value.is_bool(),
		"string" => value.is_str(),
		"int" => value.as_integer().is_some_and(|number| {
			min.is_none_or(|min| number >= min) && max.is_none_or(|max| number <= max)
		}),
		_ => false,
	}
}

fn validate_config_value(
	manifest: &Manifest,
	key: &str,
	value: &toml::Value,
) -> Result<(), String> {
	let Some(field) = manifest.config.get(key) else {
		return Err(format!("unknown configuration field {key}"));
	};
	match field {
		ConfigField::Bool { .. } => {
			if !value.is_bool() {
				return Err(format!("{key} must be a boolean"));
			}
		}
		ConfigField::Int { min, max, .. } => {
			let Some(number) = value.as_integer() else {
				return Err(format!("{key} must be an integer"));
			};
			if min.is_some_and(|min| number < min) || max.is_some_and(|max| number > max) {
				return Err(format!(
					"{key} must be within {}..={}",
					min.map(|min| min.to_string())
						.unwrap_or_else(|| "-inf".to_string()),
					max.map(|max| max.to_string())
						.unwrap_or_else(|| "inf".to_string())
				));
			}
		}
		ConfigField::String { .. } => {
			if !value.is_str() {
				return Err(format!("{key} must be a string"));
			}
		}
	}
	Ok(())
}

fn mark_enabled_conflicts(plugins: &mut [PluginEntrySummary]) {
	let enabled: BTreeSet<String> = plugins
		.iter()
		.filter(|plugin| plugin.enabled)
		.map(|plugin| plugin.id.clone())
		.collect();
	for plugin in plugins.iter_mut() {
		if !plugin.enabled {
			continue;
		}
		let id = plugin.id.clone();
		let conflicts = plugin.conflicts.iter().any(|conflict| {
			let other = conflict.split_whitespace().next().unwrap_or("");
			other != id && enabled.contains(other)
		});
		if conflicts {
			plugin.badge = PluginBadge::Conflict;
		}
	}
}

fn diagnostic_text(diagnostic: &Diagnostic) -> String {
	match diagnostic {
		Diagnostic::DuplicateSelection { id } => format!("duplicate selection {id}"),
		Diagnostic::Missing { id, version } => format!("{id} {version} is not in the store"),
		Diagnostic::Incompatible { id, reason } => format!("{id} is incompatible: {reason}"),
		Diagnostic::UnsupportedAbi { id, abi_major } => {
			format!("{id} ABI {abi_major} is not supported")
		}
		Diagnostic::Dependency {
			id,
			requires,
			reason,
		} => format!("{id} needs {requires}: {reason}"),
		Diagnostic::Conflict { id, with, reason } => match reason {
			Some(reason) => format!("{id} conflicts with {with}: {reason}"),
			None => format!("{id} conflicts with {with}"),
		},
		Diagnostic::Cycle { ids } => format!("load cycle: {}", ids.join(" -> ")),
		Diagnostic::FileCollision { path, ids } => {
			format!("{path} is shipped by {}", ids.join(", "))
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::plugin::planner::WINDOWS_X64;
	use std::fs;

	const DEMO: &str = r#"
schema = 1

[plugin]
id = "demo.plugin"
name = "Demo Plugin"
version = "0.1.0"
description = "A store fixture."

[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "*"

[entry]
kind = "legacy"
path = "plugins/demo.dll"
phase = "entry"

[config.flag]
type = "bool"
default = false

[config.limit]
type = "int"
default = 1
min = 0
max = 2

[config.label]
type = "string"
default = "alpha"
"#;

	fn install_demo(store: &Path) {
		let dir = store.join("demo.plugin").join("0.1.0+local");
		fs::create_dir_all(&dir).unwrap();
		fs::write(dir.join("foch-plugin.toml"), DEMO).unwrap();
	}

	#[test]
	fn overview_lists_registry_plugins_and_a_declared_companion() {
		let temp = tempfile::tempdir().unwrap();
		let store = temp.path().join("store");
		fs::create_dir(&store).unwrap();
		let selections = temp.path().join("selections.toml");
		let declared = vec![DeclaredModPlugin {
			plugin_id: "dev.foch.eu4-dynamic-trade-blackhole".into(),
			repo: Some("Acture/eu4-dynamic-trade-blackhole".into()),
			game: "eu4".into(),
			game_versions: Some("=1.37.5".into()),
			required: true,
			min_version: Some("0.1.0".into()),
			sha256: None,
			source_mods: vec!["Dynamic Trade".into()],
		}];
		let overview = build_overview("play", &store, &selections, &declared, None).unwrap();
		assert!(
			overview
				.plugins
				.iter()
				.any(|plugin| plugin.id == "io.github.yozoratempest.eu4-unicode-patch")
		);
		let companion = overview
			.plugins
			.iter()
			.find(|plugin| plugin.id == "dev.foch.eu4-dynamic-trade-blackhole")
			.expect("declared plugin");
		assert_eq!(companion.badge, PluginBadge::NotInstalled);
		assert_eq!(
			companion.declared_by_mods,
			vec!["Dynamic Trade".to_string()]
		);
		assert_eq!(
			companion.required_by_mods,
			vec!["Dynamic Trade".to_string()]
		);
		assert_eq!(
			companion.install_spec.as_deref(),
			Some("Acture/eu4-dynamic-trade-blackhole")
		);
		assert!(overview.plan.is_none());
	}

	#[test]
	fn enable_and_config_edits_persist_inside_the_schema() {
		let temp = tempfile::tempdir().unwrap();
		let store = temp.path().join("store");
		fs::create_dir(&store).unwrap();
		install_demo(&store);
		let selections = temp.path().join("selections.toml");
		set_plugin_enabled(
			&selections,
			&store,
			"play",
			"demo.plugin",
			true,
			Some(Version::parse("0.1.0").unwrap()),
		)
		.unwrap();
		set_plugin_config_value(
			&selections,
			&store,
			"play",
			"demo.plugin",
			"limit",
			toml::Value::Integer(2),
		)
		.unwrap();
		let error = set_plugin_config_value(
			&selections,
			&store,
			"play",
			"demo.plugin",
			"limit",
			toml::Value::Integer(3),
		)
		.unwrap_err();
		assert!(error.to_string().contains("within"), "{error}");
		let unknown = set_plugin_config_value(
			&selections,
			&store,
			"play",
			"demo.plugin",
			"missing",
			toml::Value::Boolean(true),
		)
		.unwrap_err();
		assert!(unknown.to_string().contains("unknown"), "{unknown}");

		let game = GameIdentity {
			game: "eu4".into(),
			version: Version::parse("1.37.5").unwrap(),
			platform: WINDOWS_X64.into(),
		};
		let overview = build_overview("play", &store, &selections, &[], Some(&game)).unwrap();
		let demo = overview
			.plugins
			.iter()
			.find(|plugin| plugin.id == "demo.plugin")
			.expect("demo");
		assert!(demo.enabled);
		assert_eq!(demo.installed_versions, vec!["0.1.0".to_string()]);
		assert_eq!(demo.badge, PluginBadge::Enabled);
		let limit = demo
			.config_fields
			.iter()
			.find(|field| field.key == "limit")
			.expect("limit");
		assert_eq!(limit.value, toml::Value::Integer(2));
		assert_eq!(limit.min, Some(0));
		assert_eq!(limit.max, Some(2));
		set_plugin_enabled(&selections, &store, "play", "demo.plugin", false, None).unwrap();
		let overview = build_overview("play", &store, &selections, &[], None).unwrap();
		let demo = overview
			.plugins
			.iter()
			.find(|plugin| plugin.id == "demo.plugin")
			.expect("demo");
		assert!(!demo.enabled);
		assert_eq!(demo.badge, PluginBadge::Installed);
		assert!(
			overview
				.plugins
				.iter()
				.any(|plugin| plugin.badge == PluginBadge::NotInstalled)
		);
	}
}
