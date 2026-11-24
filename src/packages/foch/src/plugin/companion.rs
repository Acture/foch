//! Plugins a playset's mods declare in their own `foch.toml` `[[plugins]]` tables.
//!
//! Discovery never fails the whole playset. A mod without `foch.toml` is
//! skipped, and a mod whose file does not parse contributes a warning.

use crate::project::{PluginDeclaration, Project};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One plugin id merged from every mod that declared it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeclaredModPlugin {
	pub plugin_id: String,
	pub repo: Option<String>,
	pub game: String,
	pub game_versions: Option<String>,
	pub required: bool,
	pub min_version: Option<String>,
	pub sha256: Option<String>,
	/// Mod names, in playset order, that declared this id.
	pub source_mods: Vec<String>,
}

/// Read `<root>/foch.toml` for each `(mod name, mod root)`.
///
/// The returned list keeps the first time each `plugin_id` appears. Later
/// declarations append their mod name and turn `required` on if any of them
/// asks for the plugin. A missing file is ignored. A file that does not parse
/// becomes a warning and does not discard the other mods.
pub fn discover_declared_plugins(
	mod_roots: &[(String, PathBuf)],
) -> (Vec<DeclaredModPlugin>, Vec<String>) {
	let mut declared = Vec::new();
	let mut warnings = Vec::new();
	for (name, root) in mod_roots {
		let path = root.join("foch.toml");
		if !path.is_file() {
			continue;
		}
		let project = match Project::load_from_path(&path) {
			Ok(project) => project,
			Err(error) => {
				warnings.push(format!("{name}: {error}"));
				continue;
			}
		};
		for plugin in project.plugins {
			merge_declaration(&mut declared, name, plugin);
		}
	}
	(declared, warnings)
}

fn merge_declaration(declared: &mut Vec<DeclaredModPlugin>, name: &str, plugin: PluginDeclaration) {
	if let Some(existing) = declared
		.iter_mut()
		.find(|entry| entry.plugin_id == plugin.id)
	{
		if !existing.source_mods.iter().any(|source| source == name) {
			existing.source_mods.push(name.to_string());
		}
		existing.required |= plugin.required;
		if existing.repo.is_none() {
			existing.repo = plugin.repo;
		}
		if existing.game_versions.is_none() {
			existing.game_versions = plugin.game_versions;
		}
		if existing.min_version.is_none() {
			existing.min_version = plugin.min_version;
		}
		if existing.sha256.is_none() {
			existing.sha256 = plugin.sha256;
		}
		return;
	}
	declared.push(DeclaredModPlugin {
		plugin_id: plugin.id,
		repo: plugin.repo,
		game: plugin.game,
		game_versions: plugin.game_versions,
		required: plugin.required,
		min_version: plugin.min_version,
		sha256: plugin.sha256,
		source_mods: vec![name.to_string()],
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;

	fn root(dir: &std::path::Path, name: &str) -> (String, PathBuf) {
		let path = dir.join(name);
		fs::create_dir_all(&path).unwrap();
		(name.to_string(), path)
	}

	#[test]
	fn missing_files_are_ignored_and_declarations_merge_in_order() {
		let temp = tempfile::tempdir().unwrap();
		let plain = root(temp.path(), "Plain");
		let first = root(temp.path(), "Alpha");
		let second = root(temp.path(), "Beta");
		let broken = root(temp.path(), "Broken");
		fs::write(
			first.1.join("foch.toml"),
			"[[plugins]]\nid = \"dev.foch.sample\"\nrepo = \"Acture/sample\"\nrequired = false\n[[plugins]]\nid = \"dev.foch.other\"\n",
		)
		.unwrap();
		fs::write(
			second.1.join("foch.toml"),
			"[[plugins]]\nid = \"dev.foch.sample\"\nrequired = true\nmin_version = \"0.2.0\"\n[[plugins]]\nid = \"dev.foch.third\"\nrepo = \"Acture/third\"\n",
		)
		.unwrap();
		fs::write(
			broken.1.join("foch.toml"),
			"[[plugins]]\nrepo = \"missing-id\"\n",
		)
		.unwrap();

		let (declared, warnings) =
			discover_declared_plugins(&[plain, first, second, broken.clone()]);
		assert_eq!(warnings.len(), 1);
		assert!(warnings[0].starts_with("Broken:"), "{warnings:?}");
		assert_eq!(
			declared
				.iter()
				.map(|plugin| plugin.plugin_id.as_str())
				.collect::<Vec<_>>(),
			vec!["dev.foch.sample", "dev.foch.other", "dev.foch.third"]
		);
		assert_eq!(
			declared[0].source_mods,
			vec!["Alpha".to_string(), "Beta".to_string()]
		);
		assert!(declared[0].required);
		assert_eq!(declared[0].repo.as_deref(), Some("Acture/sample"));
		assert_eq!(declared[0].min_version.as_deref(), Some("0.2.0"));
		assert_eq!(declared[2].repo.as_deref(), Some("Acture/third"));
		assert!(declared[1].required);
	}
}
