//! `foch plugin`: install plugins, choose them per playset, and plan a launch.
//!
//! The CLI drives the shared management module in `foch::plugin`; the desktop
//! application uses the same module. Launch stages a frozen plan into an
//! isolated game layer; only the in-game host loads DLLs.

use super::HandlerResult;
use crate::cli::arg::{
	CheckOutputFormat, PluginArgs, PluginCommand, PluginImportArgs, PluginLaunchArgs,
	PluginListArgs, PluginPlanArgs, PluginStatusArgs, PluginToggleArgs,
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
		PluginCommand::Launch(launch) => launch_plugins(launch, &config),
		PluginCommand::Status(status) => show_status(status),
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
	let candidate = match &args.adapter {
		Some(id) => store::adapt(
			plugin::builtin::adapter(id).ok_or_else(|| format!("unknown built-in adapter {id}"))?,
			entries,
		),
		None => plugin::validate(entries),
	};
	let validated = match candidate {
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
		let metadata = std::fs::symlink_metadata(entry.path())?;
		let linked = metadata.file_type().is_symlink();
		#[cfg(windows)]
		let linked = {
			use std::os::windows::fs::MetadataExt;
			linked || metadata.file_attributes() & 0x400 != 0
		};
		if linked {
			return Err(std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				format!(
					"linked package entry is unsupported: {}",
					entry.path().display()
				),
			));
		}
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
	let selections = chosen.to_selections();
	let resolution = match plugin::deployment::resolve(&game, &store_root, &selections) {
		Ok(deployment) => deployment.resolution().clone(),
		Err(reason) => {
			let mut resolution = plugin::plan(&game, &catalog, &selections);
			resolution.errors.push(planner::Diagnostic::Incompatible {
				id: "deployment".into(),
				reason,
			});
			resolution
		}
	};

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

fn launch_plugins(args: &PluginLaunchArgs, config: &Config) -> HandlerResult {
	if !cfg!(all(windows, target_arch = "x86_64")) {
		return Err("plugin launch requires Windows x64".into());
	}
	let game_root = foch_runner::locate_game(config, args.plan.game_path.as_deref())?;
	let raw_version = detect_game_version(&game_root).ok_or("could not detect the EU4 version")?;
	let game = GameIdentity {
		game: "eu4".into(),
		version: planner::parse_game_version(&raw_version).ok_or("invalid EU4 version")?,
		platform: planner::WINDOWS_X64.into(),
	};
	let (_, selections) = load_selections()?;
	let selected = selections.for_playset(&args.plan.playset).to_selections();
	let deployment = plugin::deployment::resolve(&game, &plugin::paths::store_root(), &selected)?;
	let host = args.host_dll.clone().unwrap_or(
		std::env::current_exe()?
			.parent()
			.unwrap()
			.join("foch_eu4_host.dll"),
	);
	let host_bytes = std::fs::read(&host).map_err(|error| {
		format!(
			"cannot read host {}: {error}; build foch-eu4-host and pass --host-dll",
			host.display()
		)
	})?;
	if store::pe_machine(&host_bytes) != Some(store::MACHINE_AMD64) {
		return Err("host DLL is not an AMD64 PE image".into());
	}
	let runtime_base = args
		.runtime_base
		.clone()
		.unwrap_or(std::env::temp_dir().join("foch-rt"));
	let run_id = format!(
		"p{:x}",
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)?
			.as_nanos()
	);
	let mut layer =
		foch_runner::runtime::RuntimeLayer::prepare(&game_root, &runtime_base, &run_id)?;
	let directory = layer.directory.clone();
	std::fs::write(directory.join("VERSION.dll"), &host_bytes)?;
	let data_root = foch::game::eu4::base::snapshot::data_root();
	foch_runner::runtime::ensure_outside_game(&game_root, &data_root)?;
	let prepared = deployment.stage(&directory, &data_root, &run_id, &game_root)?;
	if let Some(cache) = deployment.font_cache(&data_root, &layer.fonts_sha256) {
		layer.redirect_unicode_cache(&cache)?;
	}
	let user_dir = args
		.user_dir
		.clone()
		.or_else(|| {
			dirs::document_dir().map(|path| path.join("Paradox Interactive/Europa Universalis IV"))
		})
		.ok_or("could not locate the EU4 user directory; pass --user-dir")?;
	foch_runner::runtime::ensure_outside_game(&game_root, &user_dir)?;
	std::fs::create_dir_all(&user_dir)?;
	let user_dir = PathBuf::from(plugin::deployment::path_text(&user_dir.canonicalize()?)?);
	std::fs::write(
		directory.join("userdir.txt"),
		user_dir.to_string_lossy().as_bytes(),
	)?;
	let mut record = json!({
		"format":1, "run_id":run_id, "playset":args.plan.playset, "runtime":directory,
		"user_dir":user_dir, "game_version":raw_version,
		"game_exe_sha256":plugin::deployment::hash(&std::fs::read(directory.join("eu4.exe"))?),
		"host_sha256":plugin::deployment::hash(&host_bytes), "plan_sha256":prepared.plan_sha256,
		"artifacts":prepared.artifacts, "excluded":layer.excluded,
		"events":prepared.plan.events, "pid":null, "prepared_only":args.prepare_only,
	});
	let mut child = if args.prepare_only {
		None
	} else {
		Some(foch_runner::runtime::spawn_player(
			&directory,
			&user_dir,
			&args.game_args,
		)?)
	};
	if let Some(child) = &child {
		record["pid"] = child.id().into();
	}
	let retain = std::fs::write(
		directory.join("foch-host/run.json"),
		serde_json::to_vec_pretty(&record)?,
	)
	.and_then(|()| layer.retain().map(|_| ()));
	if let Err(error) = retain {
		if let Some(child) = &mut child {
			let _ = child.kill();
			let _ = child.wait();
		}
		return Err(error.into());
	}
	match args.plan.format {
		CheckOutputFormat::Json => print_json(&record)?,
		CheckOutputFormat::Text => {
			println!(
				"{}: {}",
				if args.prepare_only {
					"Prepared runtime"
				} else {
					"Started EU4"
				},
				directory.display()
			);
			if let Some(child) = &child {
				println!("PID: {}", child.id());
			}
			println!(
				"Read states: foch plugin status --run-dir \"{}\"",
				directory.display()
			);
		}
	}
	Ok(0)
}

fn show_status(args: &PluginStatusArgs) -> HandlerResult {
	let plan: plugin::deployment::HostPlan =
		serde_json::from_slice(&std::fs::read(args.run_dir.join("foch-host/plan.json"))?)?;
	let states = plugin::deployment::states(&plan, &args.run_dir)?;
	match args.format {
		CheckOutputFormat::Json => print_json(&json!({"run_id":plan.run_id,"plugins":states}))?,
		CheckOutputFormat::Text => {
			for (id, state) in states {
				println!(
					"{id}: {}{}",
					state["state"].as_str().unwrap_or("unknown"),
					state["reason"]
						.as_str()
						.map(|reason| format!(" ({reason})"))
						.unwrap_or_default()
				);
			}
		}
	}
	Ok(0)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn package_links_are_rejected_instead_of_losing_optional_resources() {
		let scratch = tempfile::tempdir().unwrap();
		let package = scratch.path().join("package");
		std::fs::create_dir_all(package.join("fonts")).unwrap();
		std::fs::write(package.join("fonts/optional.ttf"), "font bytes").unwrap();
		assert_eq!(
			read_package_dir(&package).unwrap()[0].path,
			"fonts/optional.ttf"
		);
		// Preparing this empty fixture creates an ordinary directory link
		// (a native junction on Windows) without launching any executable.
		let layer = foch_runner::runtime::RuntimeLayer::prepare(
			&package,
			&scratch.path().join("runtimes"),
			"linked-package",
		)
		.unwrap();
		let error = read_package_dir(&layer.directory).unwrap_err();
		assert!(error.to_string().contains("linked package entry"));
		assert_eq!(
			std::fs::read(package.join("fonts/optional.ttf")).unwrap(),
			b"font bytes"
		);
	}
}
