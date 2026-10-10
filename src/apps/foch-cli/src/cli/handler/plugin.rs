//! `foch plugin`: install plugins, choose them per playset, and plan a launch.
//!
//! The CLI drives the shared management module in `foch::plugin`; the desktop
//! application uses the same module. Nothing here loads a DLL or launches the
//! game: it manages identity, versions, selection and the resolved load order.

use super::HandlerResult;
use crate::cli::arg::{
	CheckOutputFormat, PluginArgs, PluginCommand, PluginImportArgs, PluginListArgs, PluginPlanArgs,
	PluginToggleArgs,
};
use foch::game::eu4::Eu4;
use foch::game::eu4::base::snapshot::{detect_game_version, resolve_game_root};
use foch::input::Config;
use foch::plugin::manifest::Manifest;
use foch::plugin::{self, GameIdentity, Selections, planner, store};
use semver::Version;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn handle_plugin(args: &PluginArgs, config: Config) -> HandlerResult {
	match &args.command {
		PluginCommand::List(list) => list_plugins(list, &config),
		PluginCommand::Import(import) => import_plugin(import),
		PluginCommand::Enable(toggle) => set_enabled(toggle, true),
		PluginCommand::Disable(toggle) => set_enabled(toggle, false),
		PluginCommand::Plan(plan) => plan_launch(plan, &config),
	}
}

/// The store catalog, including Foch's built-in adapter manifests as available
/// versions so they can be selected without a separate import step.
fn catalog_with_builtins(
	store_root: &Path,
) -> (BTreeMap<String, BTreeMap<Version, Manifest>>, Vec<String>) {
	let (mut catalog, problems) = store::catalog(store_root);
	for adapter in plugin::builtin::adapters() {
		catalog
			.entry(adapter.plugin.id.clone())
			.or_default()
			.entry(adapter.plugin.version.clone())
			.or_insert(adapter);
	}
	(catalog, problems)
}

fn load_selections() -> Result<(PathBuf, Selections), Box<dyn std::error::Error>> {
	let path = plugin::paths::selections_path()?;
	let selections = Selections::load(&path)?;
	Ok((path, selections))
}

fn list_plugins(args: &PluginListArgs, _config: &Config) -> HandlerResult {
	let store_root = plugin::paths::store_root();
	let (catalog, problems) = catalog_with_builtins(&store_root);
	let (_, selections) = load_selections()?;
	let chosen = selections.for_playset(&args.playset);

	match args.format {
		CheckOutputFormat::Json => {
			let plugins: Vec<_> = catalog
				.iter()
				.map(|(id, versions)| {
					let choice = chosen.plugins.get(id);
					json!({
						"id": id,
						"versions": versions.keys().map(|v| v.to_string()).collect::<Vec<_>>(),
						"selected": choice.map(|c| json!({
							"version": c.version.to_string(),
							"enabled": c.enabled,
						})),
					})
				})
				.collect();
			print_json(
				&json!({ "playset": args.playset, "plugins": plugins, "problems": problems }),
			)?;
		}
		CheckOutputFormat::Text => {
			if catalog.is_empty() {
				println!("No plugins installed.");
			}
			for (id, versions) in &catalog {
				let versions: Vec<String> = versions.keys().map(|v| v.to_string()).collect();
				let mark = match chosen.plugins.get(id) {
					Some(choice) if choice.enabled => format!(" [enabled {}]", choice.version),
					Some(choice) => format!(" [disabled {}]", choice.version),
					None => String::new(),
				};
				println!("{id}  ({}){mark}", versions.join(", "));
			}
			for problem in &problems {
				eprintln!("warning: {problem}");
			}
		}
	}
	Ok(0)
}

fn import_plugin(args: &PluginImportArgs) -> HandlerResult {
	if !args.path.is_dir() {
		return Err(format!(
			"{} is not a directory; extract the plugin package first",
			args.path.display()
		)
		.into());
	}
	let entries = read_package_dir(&args.path)?;
	let validated = match plugin::validate(entries) {
		Ok(package) => package,
		Err(problems) => {
			let mut message = String::from("package failed validation:");
			for problem in problems {
				message.push_str(&format!("\n  - {problem}"));
			}
			return Err(message.into());
		}
	};
	let store_root = plugin::paths::store_root();
	match plugin::install(&store_root, &validated)? {
		plugin::Installed::New(path) => {
			println!(
				"Imported {} {} into {}",
				validated.manifest.plugin.id,
				validated.manifest.plugin.version,
				path.display()
			);
		}
		plugin::Installed::AlreadyPresent(path) => {
			println!(
				"{} {} is already in the store ({})",
				validated.manifest.plugin.id,
				validated.manifest.plugin.version,
				path.display()
			);
		}
	}
	Ok(0)
}

/// Read a package directory into the store's archive-entry list, with paths
/// relative to the directory root and normalized to forward slashes.
fn read_package_dir(root: &Path) -> std::io::Result<Vec<store::ArchiveEntry>> {
	let mut entries = Vec::new();
	for entry in walkdir::WalkDir::new(root).follow_links(false) {
		let entry = entry?;
		if !entry.file_type().is_file() {
			continue;
		}
		let relative = entry
			.path()
			.strip_prefix(root)
			.map_err(std::io::Error::other)?;
		let path = relative.to_string_lossy().replace('\\', "/");
		entries.push(store::ArchiveEntry {
			path,
			data: std::fs::read(entry.path())?,
		});
	}
	Ok(entries)
}

fn set_enabled(args: &PluginToggleArgs, enabled: bool) -> HandlerResult {
	let store_root = plugin::paths::store_root();
	let (catalog, _) = catalog_with_builtins(&store_root);
	let Some(versions) = catalog.get(&args.id) else {
		return Err(format!("no plugin with id {} is installed", args.id).into());
	};

	let (path, mut selections) = load_selections()?;
	let mut chosen = selections.for_playset(&args.playset);

	let version = match &args.version {
		Some(text) => Version::parse(text)?,
		None => match chosen.plugins.get(&args.id) {
			// Keep the already-pinned version when only toggling state.
			Some(choice) if versions.contains_key(&choice.version) => choice.version.clone(),
			// Otherwise pin the highest installed version.
			_ => versions
				.keys()
				.next_back()
				.cloned()
				.ok_or_else(|| format!("no installed version of {} to pin", args.id))?,
		},
	};
	if !versions.contains_key(&version) {
		return Err(format!("{} {version} is not installed", args.id).into());
	}

	let entry =
		chosen
			.plugins
			.entry(args.id.clone())
			.or_insert_with(|| plugin::selection::Choice {
				version: version.clone(),
				enabled,
				config: BTreeMap::new(),
			});
	entry.version = version.clone();
	entry.enabled = enabled;

	selections.set_playset(&args.playset, chosen);
	selections.save(&path)?;
	println!(
		"{} {} {version} for playset '{}'",
		if enabled { "Enabled" } else { "Disabled" },
		args.id,
		args.playset
	);
	Ok(0)
}

fn plan_launch(args: &PluginPlanArgs, config: &Config) -> HandlerResult {
	let game_root = args
		.game_path
		.clone()
		.or_else(|| resolve_game_root(config, &Eu4))
		.ok_or("could not locate the EU4 install; pass --game-path")?;
	let raw_version = detect_game_version(&game_root)
		.ok_or("could not detect the EU4 version from the install")?;
	let version = planner::parse_game_version(&raw_version)
		.ok_or_else(|| format!("could not parse game version '{raw_version}'"))?;
	let game = GameIdentity {
		game: "eu4".into(),
		version,
		platform: planner::WINDOWS_X64.into(),
	};

	let store_root = plugin::paths::store_root();
	let (catalog, _) = catalog_with_builtins(&store_root);
	let (_, selections) = load_selections()?;
	let chosen = selections.for_playset(&args.playset);
	let resolution = plugin::plan(&game, &catalog, &chosen.to_selections());

	match args.format {
		CheckOutputFormat::Json => {
			print_json(&json!({
				"playset": args.playset,
				"game_version": game.version.to_string(),
				"launchable": resolution.is_launchable(),
				"order": resolution.order.iter().map(|r| json!({
					"id": r.id,
					"version": r.version.to_string(),
					"phase": format!("{:?}", r.phase).to_lowercase(),
				})).collect::<Vec<_>>(),
				"errors": resolution.errors.iter().map(|e| format!("{e:?}")).collect::<Vec<_>>(),
				"warnings": resolution.warnings.iter().map(|w| format!("{w:?}")).collect::<Vec<_>>(),
			}))?;
		}
		CheckOutputFormat::Text => {
			println!(
				"Playset '{}' on EU4 {}: {}",
				args.playset,
				game.version,
				if resolution.is_launchable() {
					"launchable"
				} else {
					"not launchable"
				}
			);
			if !resolution.order.is_empty() {
				println!("Load order:");
				for (index, resolved) in resolution.order.iter().enumerate() {
					println!(
						"  {}. {} {} ({:?})",
						index + 1,
						resolved.id,
						resolved.version,
						resolved.phase
					);
				}
			}
			for error in &resolution.errors {
				println!("error: {error:?}");
			}
			for warning in &resolution.warnings {
				println!("warning: {warning:?}");
			}
		}
	}
	Ok(if resolution.is_launchable() { 0 } else { 1 })
}

fn print_json(value: &serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
	println!("{}", serde_json::to_string_pretty(value)?);
	Ok(())
}
