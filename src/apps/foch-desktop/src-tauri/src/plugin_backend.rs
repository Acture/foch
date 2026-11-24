//! Desktop plugin install, configuration, and EU4 launch.
//!
//! Calls `foch` and `foch-runtime` directly. The desktop process does not
//! start the `foch` executable. `prepare_only` stages a runtime and returns
//! before any player process is created.

use crate::dto::{
	DesktopConfigField, DesktopError, DesktopLaunchResult, DesktopLaunchStatus, DesktopPluginBadge,
	DesktopPluginEntry, DesktopPluginInstall, DesktopPluginOverview, DesktopPluginPlan,
	DesktopPluginRuntimeState, DesktopPreferencesView, DesktopSettingsInput,
};
use foch::plugin::{
	LanguagePreference, MirrorPreference, TransportPreference, UiLanguage, UserPreferences,
	resolve_language, system_locale_hints,
};
use std::io;
use std::path::{Path, PathBuf};

pub(crate) struct PluginWorkspace {
	data_root: PathBuf,
	selections_path: PathBuf,
	preferences_path: PathBuf,
	host_dir: PathBuf,
	runtime_base: PathBuf,
	user_dir: Option<PathBuf>,
}

pub(crate) struct PlaysetContext {
	name: String,
	game_root: Option<PathBuf>,
	game_version: Option<String>,
	mod_roots: Vec<(String, PathBuf)>,
}

pub(crate) trait RunningPlayer {
	fn id(&self) -> u32;
	fn stop(&mut self);
}

pub(crate) trait PlayerStart {
	fn start(&mut self, runtime: &Path, user_dir: &Path) -> io::Result<Box<dyn RunningPlayer>>;
}

struct Eu4PlayerStart;

impl PlayerStart for Eu4PlayerStart {
	fn start(&mut self, runtime: &Path, user_dir: &Path) -> io::Result<Box<dyn RunningPlayer>> {
		let child = foch_runtime::runner::runtime::spawn_player(runtime, user_dir, &[])?;
		Ok(Box::new(LivePlayer { child }))
	}
}

struct LivePlayer {
	child: std::process::Child,
}

impl RunningPlayer for LivePlayer {
	fn id(&self) -> u32 {
		self.child.id()
	}

	fn stop(&mut self) {
		let _ = self.child.kill();
		let _ = self.child.wait();
	}
}

pub(crate) fn production_workspace() -> Result<PluginWorkspace, DesktopError> {
	let preferences_path = foch::plugin::preferences_path()
		.map_err(|error| DesktopError::new("settings", error.to_string()))?;
	let selections_path = foch::plugin::paths::selections_path()
		.map_err(|error| DesktopError::new("plugin_selection", error.to_string()))?;
	let host_dir = std::env::current_exe()
		.ok()
		.and_then(|path| path.parent().map(Path::to_path_buf))
		.unwrap_or_else(|| PathBuf::from("."));
	Ok(PluginWorkspace {
		data_root: foch::game::eu4::base::snapshot::data_root(),
		selections_path,
		preferences_path,
		host_dir,
		runtime_base: std::env::temp_dir().join("foch-rt"),
		user_dir: None,
	})
}

pub(crate) fn production_context(playset: Option<String>) -> Result<PlaysetContext, DesktopError> {
	let input = foch::input::inspect_current_eu4_input();
	let name = playset
		.filter(|name| !name.trim().is_empty())
		.or_else(|| input.playset.as_ref().map(|playset| playset.name.clone()))
		.unwrap_or_else(|| "default".to_string());
	let mut game_root = input.game.install_path.clone();
	if game_root.is_none() {
		game_root = foch_runtime::runner::locate_game(&foch::input::Config::default(), None).ok();
	}
	let game_version = input.game.version.clone().or_else(|| {
		game_root
			.as_deref()
			.and_then(foch::game::eu4::base::snapshot::detect_game_version)
	});
	let mod_roots = input
		.playset
		.as_ref()
		.map(|playset| {
			playset
				.mods
				.iter()
				.filter_map(|playset_mod| {
					let root = playset_mod
						.descriptor_path
						.as_ref()?
						.parent()?
						.to_path_buf();
					Some((playset_mod.name.clone(), root))
				})
				.collect()
		})
		.unwrap_or_default();
	Ok(PlaysetContext {
		name,
		game_root,
		game_version,
		mod_roots,
	})
}

pub(crate) fn ensure_player_platform(supported: bool) -> Result<(), DesktopError> {
	if supported {
		Ok(())
	} else {
		Err(DesktopError::new(
			"plugin_launch",
			"plugin launch requires Windows x64",
		))
	}
}

pub(crate) fn load_overview(
	workspace: &PluginWorkspace,
	context: &PlaysetContext,
) -> Result<DesktopPluginOverview, DesktopError> {
	let store = foch::plugin::paths::checked_store_root(&workspace.data_root)
		.map_err(|error| DesktopError::new("plugin_overview", error.to_string()))?;
	let (declared, mut warnings) = foch::plugin::discover_declared_plugins(&context.mod_roots);
	let game = context.game_version.as_deref().and_then(|version| {
		foch::plugin::parse_game_version(version).map(|version| foch::plugin::GameIdentity {
			game: "eu4".into(),
			version,
			platform: foch::plugin::WINDOWS_X64.into(),
		})
	});
	let overview = foch::plugin::build_overview(
		&context.name,
		&store,
		&workspace.selections_path,
		&declared,
		game.as_ref(),
	)
	.map_err(|error| DesktopError::new("plugin_overview", error.to_string()))?;
	for entry in &overview.plugins {
		if entry.declared_by_mods.is_empty() {
			continue;
		}
		if entry.installed_versions.is_empty() {
			warnings.push(format!(
				"{} declares {}, which is not installed",
				entry.declared_by_mods.join(", "),
				entry.id
			));
		} else if !entry.enabled && !entry.required_by_mods.is_empty() {
			warnings.push(format!(
				"{} requires {}, which is installed but turned off",
				entry.required_by_mods.join(", "),
				entry.id
			));
		}
	}
	let (host_dll_available, host_dll_detail) = host_status(&workspace.host_dir);
	let preferences = UserPreferences::load_or_default(&workspace.preferences_path);
	Ok(DesktopPluginOverview {
		playset: overview.playset,
		game_root: context
			.game_root
			.as_ref()
			.map(|path| path.to_string_lossy().into_owned()),
		game_version: context.game_version.clone(),
		host_dll_available,
		host_dll_detail,
		entries: overview.plugins.into_iter().map(entry_view).collect(),
		companion_warnings: warnings,
		plan: overview.plan.map(plan_view),
		preferences: preferences_view(&preferences),
		problems: overview.problems,
	})
}

pub(crate) fn install_plugin_spec(
	workspace: &PluginWorkspace,
	context: &PlaysetContext,
	spec: &str,
	sha256: Option<String>,
) -> Result<DesktopPluginInstall, DesktopError> {
	let options = saved_fetch_options(workspace, sha256);
	if let Ok(release) = foch::plugin::github::resolve(spec) {
		let mut options = options;
		if options.sha256.is_none() {
			options.sha256.clone_from(&release.sha256);
		}
		let source_label = format!("{}@{}", release.repo, release.tag);
		let fetched = foch::plugin::fetch_release_asset(
			&release.repo,
			&release.tag,
			&release.asset,
			&options,
		)
		.map_err(|error| DesktopError::new("plugin_install", error.to_string()))?;
		let package = foch::plugin::github::package_from_zip(&fetched.bytes, release)
			.map_err(|error| DesktopError::new("plugin_install", error.to_string()))?;
		place_package(
			workspace,
			context,
			package,
			format!("{source_label} {}", fetched.via),
			"plugin_install",
			None,
		)
	} else {
		let (repo, reference) = foch::plugin::github::split_install_spec(spec)
			.map_err(|error| DesktopError::new("plugin_install", error.to_string()))?;
		let dynamic = foch::plugin::github::resolve_dynamic_with_transport(
			&repo,
			reference.as_deref(),
			options.transport,
			&foch::plugin::ApiQuery::default(),
		)
		.map_err(|error| DesktopError::new("plugin_install", error.to_string()))?;
		let fetched = foch::plugin::fetch_release_asset(
			&dynamic.repo,
			&dynamic.tag,
			&dynamic.asset,
			&options,
		)
		.map_err(|error| DesktopError::new("plugin_install", error.to_string()))?;
		let package = foch::plugin::github::package_from_own_zip(&fetched.bytes)
			.map_err(|error| DesktopError::new("plugin_install", error.to_string()))?;
		let source = format!("{}@{} {}", dynamic.repo, dynamic.tag, fetched.via);
		place_package(workspace, context, package, source, "plugin_install", None)
	}
}

pub(crate) fn import_plugin_directory(
	workspace: &PluginWorkspace,
	context: &PlaysetContext,
	path: &Path,
) -> Result<DesktopPluginInstall, DesktopError> {
	if !path.is_dir() {
		return Err(DesktopError::new(
			"plugin_import",
			format!(
				"{} is not a directory; extract the plugin package first",
				path.display()
			),
		));
	}
	let entries = read_package_dir(path)
		.map_err(|error| DesktopError::new("plugin_import", error.to_string()))?;
	let package = foch::plugin::validate(entries).map_err(|problems| {
		let mut message = String::from("package failed validation:");
		for problem in problems {
			message.push_str(&format!("\n  - {problem}"));
		}
		DesktopError::new("plugin_import", message)
	})?;
	let source = format!("local directory {}", path.display());
	place_package(
		workspace,
		context,
		package,
		source,
		"plugin_import",
		Some(path),
	)
}

pub(crate) fn set_enabled(
	workspace: &PluginWorkspace,
	context: &PlaysetContext,
	id: &str,
	enabled: bool,
	version: Option<&str>,
) -> Result<(), DesktopError> {
	let store = foch::plugin::paths::checked_store_root(&workspace.data_root)
		.map_err(|error| DesktopError::new("plugin_selection", error.to_string()))?;
	let version = match version.map(str::trim).filter(|text| !text.is_empty()) {
		Some(text) => Some(semver::Version::parse(text).map_err(|error| {
			DesktopError::new(
				"plugin_selection",
				format!("invalid version {text}: {error}"),
			)
		})?),
		None => None,
	};
	foch::plugin::set_plugin_enabled(
		&workspace.selections_path,
		&store,
		&context.name,
		id,
		enabled,
		version,
	)
	.map_err(|error| DesktopError::new("plugin_selection", error.to_string()))
}

pub(crate) fn update_config(
	workspace: &PluginWorkspace,
	context: &PlaysetContext,
	id: &str,
	key: &str,
	value: serde_json::Value,
) -> Result<(), DesktopError> {
	let store = foch::plugin::paths::checked_store_root(&workspace.data_root)
		.map_err(|error| DesktopError::new("plugin_config", error.to_string()))?;
	let value = json_to_toml(value)?;
	foch::plugin::set_plugin_config_value(
		&workspace.selections_path,
		&store,
		&context.name,
		id,
		key,
		value,
	)
	.map_err(|error| DesktopError::new("plugin_config", error.to_string()))
}

pub(crate) fn launch_plugins(
	workspace: &PluginWorkspace,
	context: &PlaysetContext,
	prepare_only: bool,
	starter: &mut dyn PlayerStart,
) -> Result<DesktopLaunchResult, DesktopError> {
	ensure_player_platform(cfg!(all(windows, target_arch = "x86_64")))?;
	let game_root = context
		.game_root
		.clone()
		.ok_or_else(|| DesktopError::new("plugin_launch", "could not locate the EU4 install"))?;
	let raw_version = context
		.game_version
		.clone()
		.ok_or_else(|| DesktopError::new("plugin_launch", "could not detect the EU4 version"))?;
	let game = foch::plugin::GameIdentity {
		game: "eu4".into(),
		version: foch::plugin::parse_game_version(&raw_version).ok_or_else(|| {
			DesktopError::new(
				"plugin_launch",
				format!("invalid EU4 version '{raw_version}'"),
			)
		})?,
		platform: foch::plugin::WINDOWS_X64.into(),
	};
	let selections = foch::plugin::Selections::load(&workspace.selections_path)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let selected = selections.for_playset(&context.name).to_selections();
	let store = foch::plugin::paths::checked_store_root(&workspace.data_root)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let deployment = foch::plugin::deployment::resolve(&game, &store, &selected)
		.map_err(|error| DesktopError::new("plugin_launch", error))?;
	let host = foch_runtime::host::resolve(&workspace.host_dir, None)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let host_bytes = foch_runtime::host::read(&host)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let nanos = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|duration| duration.as_nanos())
		.unwrap_or(0);
	let run_id = format!("p{nanos:x}");
	let mut layer = foch_runtime::runner::runtime::RuntimeLayer::prepare(
		&game_root,
		&workspace.runtime_base,
		&run_id,
	)
	.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let directory = layer.directory.clone();
	foch::plugin::store::create_package_file(&directory.join("VERSION.dll"), &host_bytes)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	foch::plugin::deployment::ensure_outside_game(&game_root, &workspace.data_root)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let prepared = deployment
		.stage(&directory, &workspace.data_root, &run_id, &game_root)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	if let Some(cache) = deployment
		.prepare_font_cache(&workspace.data_root, &layer.fonts_sha256, &game_root)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?
	{
		layer
			.redirect_unicode_cache(&cache)
			.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	}
	let user_dir = player_user_dir(workspace)?;
	foch::plugin::deployment::ensure_outside_game(&game_root, &user_dir)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	std::fs::create_dir_all(&user_dir)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let user_dir = PathBuf::from(
		foch::plugin::deployment::path_text(
			&user_dir
				.canonicalize()
				.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?,
		)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?,
	);
	foch::plugin::store::replace_package_file(
		&directory.join("userdir.txt"),
		user_dir.to_string_lossy().as_bytes(),
	)
	.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let exe_bytes =
		foch::plugin::store::read_package_file(&directory.join("eu4.exe")).map_err(|error| {
			DesktopError::new(
				"plugin_launch",
				format!("could not read staged eu4.exe: {error}"),
			)
		})?;
	let plan_sha256 = prepared.plan_sha256.clone();
	let plugin_count = prepared.artifacts.len();
	let mut record = serde_json::json!({
		"format": 1,
		"run_id": run_id,
		"playset": context.name,
		"runtime": &directory,
		"user_dir": &user_dir,
		"game_version": raw_version,
		"game_exe_sha256": foch::plugin::deployment::hash(&exe_bytes),
		"host_sha256": foch::plugin::deployment::hash(&host_bytes),
		"plan_sha256": plan_sha256,
		"artifacts": prepared.artifacts,
		"excluded": layer.excluded,
		"events": prepared.plan.events,
		"pid": null,
		"prepared_only": prepare_only,
	});
	let record_bytes = serde_json::to_vec_pretty(&record)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	foch::plugin::store::create_package_file(&directory.join("foch-host/run.json"), &record_bytes)
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let pid = layer
		.retain_after(|runtime| {
			if prepare_only {
				return Ok(None);
			}
			let mut player = starter.start(runtime, &user_dir)?;
			record["pid"] = serde_json::json!(player.id());
			record["prepared_only"] = serde_json::Value::Bool(false);
			let bytes = serde_json::to_vec_pretty(&record).map_err(io::Error::other)?;
			if let Err(error) = foch::plugin::store::replace_package_file(
				&runtime.join("foch-host/run.json"),
				&bytes,
			) {
				player.stop();
				return Err(error);
			}
			Ok(Some(player.id()))
		})
		.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	let run_dir = foch::plugin::deployment::path_text(
		&directory
			.canonicalize()
			.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?,
	)
	.map_err(|error| DesktopError::new("plugin_launch", error.to_string()))?;
	Ok(DesktopLaunchResult {
		run_dir,
		plan_sha256,
		prepared_only: prepare_only && pid.is_none(),
		pid,
		plugin_count,
		version_dll: directory.join("VERSION.dll").is_file(),
	})
}

pub(crate) fn launch_plugins_for_player(
	workspace: &PluginWorkspace,
	context: &PlaysetContext,
	prepare_only: bool,
) -> Result<DesktopLaunchResult, DesktopError> {
	launch_plugins(workspace, context, prepare_only, &mut Eu4PlayerStart)
}

pub(crate) fn launch_status(run_dir: &Path) -> Result<DesktopLaunchStatus, DesktopError> {
	let plan = foch::plugin::deployment::read_plan(run_dir)
		.map_err(|error| DesktopError::new("plugin_status", error.to_string()))?;
	let states = foch::plugin::deployment::states(&plan, run_dir)
		.map_err(|error| DesktopError::new("plugin_status", error.to_string()))?;
	let plugins = states
		.into_iter()
		.map(|(id, value)| DesktopPluginRuntimeState {
			id,
			state: value
				.get("state")
				.and_then(serde_json::Value::as_str)
				.unwrap_or("unknown")
				.to_string(),
			reason: value
				.get("reason")
				.and_then(serde_json::Value::as_str)
				.map(str::to_string),
		})
		.collect();
	Ok(DesktopLaunchStatus {
		run_id: plan.run_id,
		plugins,
	})
}

pub(crate) fn load_production_settings() -> Result<DesktopPreferencesView, DesktopError> {
	let workspace = production_workspace()?;
	Ok(load_settings(&workspace.preferences_path))
}

pub(crate) fn save_production_settings(
	input: DesktopSettingsInput,
) -> Result<DesktopPreferencesView, DesktopError> {
	let workspace = production_workspace()?;
	save_settings(&workspace.preferences_path, input)
}

pub(crate) fn load_settings(path: &Path) -> DesktopPreferencesView {
	preferences_view(&UserPreferences::load_or_default(path))
}

pub(crate) fn save_settings(
	path: &Path,
	input: DesktopSettingsInput,
) -> Result<DesktopPreferencesView, DesktopError> {
	let preferences = UserPreferences {
		transport: parse_transport(&input.transport)?,
		mirror: parse_mirror(&input.mirror)?,
		custom_mirrors: input.custom_mirrors,
		language: parse_language(&input.language)?,
		..UserPreferences::default()
	};
	preferences
		.save(path)
		.map_err(|error| DesktopError::new("settings", error.to_string()))?;
	Ok(load_settings(path))
}

fn saved_fetch_options(
	workspace: &PluginWorkspace,
	sha256: Option<String>,
) -> foch::plugin::FetchOptions {
	let transport =
		UserPreferences::load_or_default(&workspace.preferences_path).to_transport_config();
	foch::plugin::FetchOptions {
		transport: transport.transport,
		mirror: transport.mirror,
		sha256,
		region: None,
		asset_origin: None,
	}
}

fn place_package(
	workspace: &PluginWorkspace,
	context: &PlaysetContext,
	package: foch::plugin::ValidatedPackage,
	source: String,
	code: &'static str,
	package_root: Option<&Path>,
) -> Result<DesktopPluginInstall, DesktopError> {
	let destination =
		foch::plugin::store::version_dir(&workspace.data_root.join("plugins/store"), &package);
	let mut guarded = Vec::new();
	if let Some(package_root) = package_root {
		guarded.push(package_root.to_path_buf());
	}
	if let Some(game_root) = &context.game_root {
		guarded.push(game_root.clone());
	}
	for root in guarded {
		foch::plugin::deployment::ensure_outside_game(&root, &workspace.data_root)
			.map_err(|error| DesktopError::new(code, error.to_string()))?;
		foch::plugin::deployment::ensure_outside_game(&root, &destination)
			.map_err(|error| DesktopError::new(code, error.to_string()))?;
	}
	let store = foch::plugin::paths::prepare_store_root(&workspace.data_root)
		.map_err(|error| DesktopError::new(code, error.to_string()))?;
	let already_present = match foch::plugin::install(&store, &package)
		.map_err(|error| DesktopError::new(code, error.to_string()))?
	{
		foch::plugin::Installed::New(_) => false,
		foch::plugin::Installed::AlreadyPresent(_) => true,
	};
	let conflicts = package
		.manifest
		.conflicts
		.iter()
		.map(|conflict| match &conflict.reason {
			Some(reason) => format!("{} ({reason})", conflict.id),
			None => conflict.id.clone(),
		})
		.collect();
	Ok(DesktopPluginInstall {
		id: package.manifest.plugin.id.clone(),
		version: package.manifest.plugin.version.to_string(),
		source,
		conflicts,
		already_present,
	})
}

fn read_package_dir(root: &Path) -> io::Result<Vec<foch::plugin::store::ArchiveEntry>> {
	let mut entries = Vec::new();
	walk_package(root, root, &mut entries)?;
	Ok(entries)
}

fn walk_package(
	root: &Path,
	directory: &Path,
	entries: &mut Vec<foch::plugin::store::ArchiveEntry>,
) -> io::Result<()> {
	for item in std::fs::read_dir(directory)? {
		let item = item?;
		let path = item.path();
		let metadata = std::fs::symlink_metadata(&path)?;
		let linked = metadata.file_type().is_symlink();
		#[cfg(windows)]
		let linked = {
			use std::os::windows::fs::MetadataExt;
			linked || metadata.file_attributes() & 0x400 != 0
		};
		if linked {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!("linked package entry is unsupported: {}", path.display()),
			));
		}
		if metadata.is_dir() {
			walk_package(root, &path, entries)?;
			continue;
		}
		if !metadata.is_file() {
			continue;
		}
		let relative = path.strip_prefix(root).map_err(io::Error::other)?;
		entries.push(foch::plugin::store::ArchiveEntry {
			path: relative.to_string_lossy().replace('\\', "/"),
			data: foch::plugin::store::read_package_file(&path)?,
		});
	}
	Ok(())
}

fn player_user_dir(workspace: &PluginWorkspace) -> Result<PathBuf, DesktopError> {
	if let Some(path) = &workspace.user_dir {
		return Ok(path.clone());
	}
	dirs::document_dir()
		.map(|path| {
			path.join("Paradox Interactive")
				.join("Europa Universalis IV")
		})
		.ok_or_else(|| {
			DesktopError::new("plugin_launch", "could not locate the EU4 user directory")
		})
}

fn host_status(directory: &Path) -> (bool, String) {
	for name in [
		foch_runtime::host::BUILT_DLL,
		foch_runtime::host::DISTRIBUTED_DLL,
	] {
		if directory.join(name).is_file() {
			return (true, format!("ready ({name})"));
		}
	}
	(
		false,
		format!(
			"missing {} / {}",
			foch_runtime::host::BUILT_DLL,
			foch_runtime::host::DISTRIBUTED_DLL
		),
	)
}

fn entry_view(entry: foch::plugin::PluginEntrySummary) -> DesktopPluginEntry {
	DesktopPluginEntry {
		id: entry.id,
		name: entry.name,
		description: entry.description,
		installed_versions: entry.installed_versions,
		selected_version: entry.selected_version,
		enabled: entry.enabled,
		install_spec: entry.install_spec,
		declared_by_mods: entry.declared_by_mods,
		required_by_mods: entry.required_by_mods,
		conflicts: entry.conflicts,
		config_fields: entry
			.config_fields
			.into_iter()
			.map(|field| DesktopConfigField {
				key: field.key,
				kind: field.kind,
				description: field.description,
				value: toml_to_json(&field.value),
				min: field.min,
				max: field.max,
			})
			.collect(),
		badge: match entry.badge {
			foch::plugin::PluginBadge::Enabled => DesktopPluginBadge::Enabled,
			foch::plugin::PluginBadge::Installed => DesktopPluginBadge::Installed,
			foch::plugin::PluginBadge::NotInstalled => DesktopPluginBadge::NotInstalled,
			foch::plugin::PluginBadge::Conflict => DesktopPluginBadge::Conflict,
		},
	}
}

fn plan_view(plan: foch::plugin::PluginPlanSummary) -> DesktopPluginPlan {
	DesktopPluginPlan {
		launchable: plan.launchable,
		order: plan.order,
		errors: plan.errors,
		warnings: plan.warnings,
	}
}

fn preferences_view(preferences: &UserPreferences) -> DesktopPreferencesView {
	DesktopPreferencesView {
		transport: transport_name(preferences.transport).to_string(),
		mirror: mirror_name(preferences.mirror).to_string(),
		custom_mirrors: preferences.custom_mirrors.clone(),
		language: language_name(preferences.language).to_string(),
		effective_language: effective_language_code(preferences).to_string(),
	}
}

fn effective_language_code(preferences: &UserPreferences) -> &'static str {
	let resolved = match preferences.language {
		LanguagePreference::Auto => {
			let hints = system_locale_hints();
			let mut resolved = resolve_language(LanguagePreference::Auto, None);
			for hint in &hints {
				let candidate = resolve_language(LanguagePreference::Auto, Some(hint));
				if candidate == UiLanguage::ZhCn {
					resolved = candidate;
					break;
				}
			}
			resolved
		}
		explicit => resolve_language(explicit, None),
	};
	match resolved {
		UiLanguage::ZhCn => "zh-CN",
		UiLanguage::En => "en",
	}
}

fn transport_name(value: TransportPreference) -> &'static str {
	match value {
		TransportPreference::Auto => "auto",
		TransportPreference::Http => "http",
		TransportPreference::Gh => "gh",
	}
}

fn mirror_name(value: MirrorPreference) -> &'static str {
	match value {
		MirrorPreference::Auto => "auto",
		MirrorPreference::Cn => "cn",
		MirrorPreference::Direct => "direct",
		MirrorPreference::Custom => "custom",
	}
}

fn language_name(value: LanguagePreference) -> &'static str {
	match value {
		LanguagePreference::Auto => "auto",
		LanguagePreference::ZhCn => "zh_cn",
		LanguagePreference::En => "en",
	}
}

fn parse_transport(value: &str) -> Result<TransportPreference, DesktopError> {
	match value {
		"auto" => Ok(TransportPreference::Auto),
		"http" => Ok(TransportPreference::Http),
		"gh" => Ok(TransportPreference::Gh),
		other => Err(DesktopError::new(
			"settings",
			format!("unknown transport {other}"),
		)),
	}
}

fn parse_mirror(value: &str) -> Result<MirrorPreference, DesktopError> {
	match value {
		"auto" => Ok(MirrorPreference::Auto),
		"cn" => Ok(MirrorPreference::Cn),
		"direct" => Ok(MirrorPreference::Direct),
		"custom" => Ok(MirrorPreference::Custom),
		other => Err(DesktopError::new(
			"settings",
			format!("unknown mirror {other}"),
		)),
	}
}

fn parse_language(value: &str) -> Result<LanguagePreference, DesktopError> {
	match value {
		"auto" => Ok(LanguagePreference::Auto),
		"zh_cn" => Ok(LanguagePreference::ZhCn),
		"en" => Ok(LanguagePreference::En),
		other => Err(DesktopError::new(
			"settings",
			format!("unknown language {other}"),
		)),
	}
}

fn json_to_toml(value: serde_json::Value) -> Result<toml::Value, DesktopError> {
	match value {
		serde_json::Value::Bool(flag) => Ok(toml::Value::Boolean(flag)),
		serde_json::Value::String(text) => Ok(toml::Value::String(text)),
		serde_json::Value::Number(number) => {
			number.as_i64().map(toml::Value::Integer).ok_or_else(|| {
				DesktopError::new("plugin_config", "configuration numbers must be integers")
			})
		}
		_ => Err(DesktopError::new(
			"plugin_config",
			"configuration value must be a boolean, integer, or string",
		)),
	}
}

fn toml_to_json(value: &toml::Value) -> serde_json::Value {
	match value {
		toml::Value::Boolean(flag) => serde_json::Value::Bool(*flag),
		toml::Value::Integer(number) => serde_json::Value::from(*number),
		toml::Value::String(text) => serde_json::Value::from(text.clone()),
		_ => serde_json::Value::Null,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	#[cfg(all(windows, target_arch = "x86_64"))]
	use std::sync::atomic::{AtomicU32, Ordering};

	fn workspace_in(root: &Path) -> (PluginWorkspace, PlaysetContext) {
		let game = root.join("game");
		fs::create_dir_all(&game).unwrap();
		let data = root.join("data");
		fs::create_dir_all(&data).unwrap();
		let workspace = PluginWorkspace {
			data_root: data,
			selections_path: root.join("selections.toml"),
			preferences_path: root.join("ui-settings.toml"),
			host_dir: root.join("host"),
			runtime_base: root.join("rt"),
			user_dir: Some(root.join("user")),
		};
		fs::create_dir_all(&workspace.host_dir).unwrap();
		fs::create_dir_all(workspace.user_dir.as_ref().unwrap()).unwrap();
		let context = PlaysetContext {
			name: "Campaign".into(),
			game_root: Some(game),
			game_version: Some("1.37.5".into()),
			mod_roots: Vec::new(),
		};
		(workspace, context)
	}

	fn settings(
		transport: &str,
		mirror: &str,
		mirrors: &[&str],
		language: &str,
	) -> DesktopSettingsInput {
		DesktopSettingsInput {
			transport: transport.into(),
			mirror: mirror.into(),
			custom_mirrors: mirrors.iter().map(|mirror| (*mirror).to_string()).collect(),
			language: language.into(),
		}
	}

	#[test]
	fn preferences_round_trip_and_reject_plain_http_mirrors() {
		let temp = tempfile::tempdir().unwrap();
		let (workspace, _) = workspace_in(temp.path());
		let saved = save_settings(
			&workspace.preferences_path,
			settings("http", "custom", &["https://gh-proxy.com"], "zh_cn"),
		)
		.unwrap();
		assert_eq!(saved.transport, "http");
		assert_eq!(saved.mirror, "custom");
		assert_eq!(saved.language, "zh_cn");
		assert_eq!(saved.effective_language, "zh-CN");
		assert_eq!(
			saved.custom_mirrors,
			vec!["https://gh-proxy.com/".to_string()]
		);
		assert_eq!(load_settings(&workspace.preferences_path), saved);
		let options = saved_fetch_options(&workspace, Some("abc".into()));
		assert_eq!(options.transport, foch::plugin::InstallTransport::Http);
		assert_eq!(
			options.mirror,
			foch::plugin::MirrorPolicy::Custom(vec!["https://gh-proxy.com/".to_string()])
		);
		assert_eq!(options.sha256.as_deref(), Some("abc"));
		let rejected = save_settings(
			&workspace.preferences_path,
			settings("http", "custom", &["http://127.0.0.1/"], "en"),
		);
		assert!(rejected.is_err());
	}

	#[test]
	fn explicit_english_stays_english() {
		let temp = tempfile::tempdir().unwrap();
		let (workspace, _) = workspace_in(temp.path());
		let saved = save_settings(
			&workspace.preferences_path,
			settings("auto", "direct", &[], "en"),
		)
		.unwrap();
		assert_eq!(saved.effective_language, "en");
		assert_eq!(saved.mirror, "direct");
	}

	#[test]
	fn companion_discovery_warns_when_a_declared_plugin_is_missing() {
		let temp = tempfile::tempdir().unwrap();
		let (workspace, mut context) = workspace_in(temp.path());
		let mod_root = temp.path().join("Alpha");
		fs::create_dir_all(&mod_root).unwrap();
		fs::write(
			mod_root.join("foch.toml"),
			"[[plugins]]\nid = \"dev.foch.sample\"\nrepo = \"Acture/sample\"\nrequired = true\n",
		)
		.unwrap();
		context.mod_roots = vec![("Alpha".into(), mod_root)];
		let overview = load_overview(&workspace, &context).unwrap();
		let value = serde_json::to_value(&overview).unwrap();
		assert!(value.get("gameRoot").is_some());
		assert!(value.get("hostDllAvailable").is_some());
		assert!(value.get("companionWarnings").is_some());
		assert!(value.get("entries").is_some());
		assert_eq!(value["preferences"]["language"], "auto");
		let entry = overview
			.entries
			.iter()
			.find(|entry| entry.id == "dev.foch.sample")
			.expect("declared plugin");
		assert_eq!(entry.declared_by_mods, vec!["Alpha".to_string()]);
		assert_eq!(entry.install_spec.as_deref(), Some("Acture/sample"));
		assert_eq!(entry.badge, DesktopPluginBadge::NotInstalled);
		assert!(overview.companion_warnings.iter().any(
			|warning| warning.contains("dev.foch.sample") && warning.contains("not installed")
		));
		assert!(!overview.host_dll_available);
	}

	#[test]
	fn schema_config_accepts_an_in_range_integer_and_rejects_the_rest() {
		let temp = tempfile::tempdir().unwrap();
		let (workspace, context) = workspace_in(temp.path());
		let package = temp.path().join("package");
		write_sample_package(&package);
		let installed = import_plugin_directory(&workspace, &context, &package).unwrap();
		assert_eq!(installed.id, "dev.foch.sample");
		assert_eq!(installed.version, "0.1.0");
		assert!(!installed.already_present);
		assert!(installed.source.contains("local directory"));
		set_enabled(&workspace, &context, "dev.foch.sample", true, Some("0.1.0")).unwrap();
		update_config(
			&workspace,
			&context,
			"dev.foch.sample",
			"limit",
			serde_json::json!(3),
		)
		.unwrap();
		let rejected = update_config(
			&workspace,
			&context,
			"dev.foch.sample",
			"limit",
			serde_json::json!(9),
		);
		assert!(rejected.unwrap_err().message.contains("limit"));
		let overview = load_overview(&workspace, &context).unwrap();
		let entry = overview
			.entries
			.iter()
			.find(|entry| entry.id == "dev.foch.sample")
			.unwrap();
		assert!(entry.enabled);
		assert_eq!(
			entry
				.config_fields
				.iter()
				.find(|field| field.key == "limit")
				.unwrap()
				.value,
			serde_json::json!(3)
		);
		let inside = context.game_root.as_ref().unwrap().join("data");
		let nested = PluginWorkspace {
			data_root: inside,
			..workspace
		};
		let forbidden = import_plugin_directory(&nested, &context, &package);
		assert!(forbidden.is_err());
	}

	#[test]
	fn an_unknown_install_spec_fails_before_any_download() {
		let temp = tempfile::tempdir().unwrap();
		let (workspace, context) = workspace_in(temp.path());
		let error = install_plugin_spec(&workspace, &context, "not a spec", None).unwrap_err();
		assert_eq!(error.code, "plugin_install");
	}

	#[test]
	fn non_windows_launch_is_refused_before_a_player_starts() {
		let error = ensure_player_platform(false).unwrap_err();
		assert!(error.message.contains("Windows x64"));
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	struct ExplodingStart;

	#[cfg(all(windows, target_arch = "x86_64"))]
	impl PlayerStart for ExplodingStart {
		fn start(
			&mut self,
			_runtime: &Path,
			_user_dir: &Path,
		) -> io::Result<Box<dyn RunningPlayer>> {
			panic!("prepare_only must not start eu4.exe");
		}
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	struct RecordingStart {
		calls: AtomicU32,
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	impl PlayerStart for RecordingStart {
		fn start(
			&mut self,
			_runtime: &Path,
			_user_dir: &Path,
		) -> io::Result<Box<dyn RunningPlayer>> {
			self.calls.fetch_add(1, Ordering::SeqCst);
			Ok(Box::new(FakePlayer { pid: 42 }))
		}
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	struct FakePlayer {
		pid: u32,
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	impl RunningPlayer for FakePlayer {
		fn id(&self) -> u32 {
			self.pid
		}

		fn stop(&mut self) {}
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	#[test]
	fn prepare_only_stages_a_runtime_and_a_real_launch_records_a_pid_without_the_test_starting_eu4()
	{
		let temp = tempfile::tempdir().unwrap();
		let (workspace, context) = workspace_in(temp.path());
		let game = context.game_root.clone().unwrap();
		fs::write(game.join("eu4.exe"), b"not a real game").unwrap();
		fs::write(game.join("version.txt"), "1.37.5\n").unwrap();
		fs::write(
			workspace.host_dir.join(foch_runtime::host::BUILT_DLL),
			host_dll(),
		)
		.unwrap();
		let package = temp.path().join("package");
		write_sample_package(&package);
		import_plugin_directory(&workspace, &context, &package).unwrap();
		set_enabled(&workspace, &context, "dev.foch.sample", true, None).unwrap();

		let prepared = launch_plugins(&workspace, &context, true, &mut ExplodingStart).unwrap();
		assert!(prepared.prepared_only);
		assert_eq!(prepared.pid, None);
		assert!(prepared.version_dll);
		assert_eq!(prepared.plugin_count, 1);
		assert_eq!(fs::read(game.join("eu4.exe")).unwrap(), b"not a real game");
		let run_dir = PathBuf::from(&prepared.run_dir);
		let record: serde_json::Value =
			serde_json::from_slice(&fs::read(run_dir.join("foch-host/run.json")).unwrap()).unwrap();
		assert_eq!(record["prepared_only"], true);
		assert!(record["pid"].is_null());
		let plan = foch::plugin::deployment::read_plan(&run_dir).unwrap();
		let events = run_dir.join("foch-host/events.jsonl");
		fs::write(
			&events,
			format!(
				"{}\n",
				serde_json::json!({
					"run_id": plan.run_id,
					"event": "state",
					"plugin_id": "dev.foch.sample",
					"state": "refused",
					"reason": "probe declined"
				})
			),
		)
		.unwrap();
		let status = launch_status(&run_dir).unwrap();
		assert_eq!(status.plugins.len(), 1);
		assert_eq!(status.plugins[0].state, "refused");
		assert_eq!(status.plugins[0].reason.as_deref(), Some("probe declined"));

		let mut starter = RecordingStart {
			calls: AtomicU32::new(0),
		};
		let launched = launch_plugins(&workspace, &context, false, &mut starter).unwrap();
		assert_eq!(starter.calls.load(Ordering::SeqCst), 1);
		assert_eq!(launched.pid, Some(42));
		assert!(!launched.prepared_only);
		assert_eq!(fs::read(game.join("eu4.exe")).unwrap(), b"not a real game");
	}

	fn write_sample_package(dir: &Path) {
		let dll = amd64_dll();
		let digest = foch::plugin::deployment::hash(&dll);
		let manifest = format!(
			r#"
schema = 1
[plugin]
id = "dev.foch.sample"
name = "Sample"
version = "0.1.0"
[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "*"
abi_major = 0
[entry]
kind = "native"
path = "sample.dll"
phase = "deferred"
[[files]]
path = "sample.dll"
sha256 = "{digest}"
[config.limit]
type = "int"
default = 1
min = 0
max = 4
"#
		);
		fs::create_dir_all(dir).unwrap();
		fs::write(dir.join("foch-plugin.toml"), manifest).unwrap();
		fs::write(dir.join("sample.dll"), dll).unwrap();
	}

	fn amd64_dll() -> Vec<u8> {
		let mut bytes = vec![0u8; 0x80];
		bytes[0] = b'M';
		bytes[1] = b'Z';
		bytes[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
		bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
		bytes[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
		bytes
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	fn put16(bytes: &mut [u8], offset: usize, value: u16) {
		bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	fn put32(bytes: &mut [u8], offset: usize, value: u32) {
		bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	fn host_dll() -> Vec<u8> {
		let names = foch_runtime::host::PROXY_EXPORTS;
		let mut bytes = vec![0; 0x1200];
		bytes[..2].copy_from_slice(b"MZ");
		put32(&mut bytes, 0x3c, 0x80);
		bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
		put16(&mut bytes, 0x84, 0x8664);
		put16(&mut bytes, 0x86, 1);
		put16(&mut bytes, 0x94, 240);
		put16(&mut bytes, 0x96, 0x2000);
		let optional = 0x98;
		put16(&mut bytes, optional, 0x20b);
		put32(&mut bytes, optional + 60, 0x200);
		put32(&mut bytes, optional + 108, 1);
		put32(&mut bytes, optional + 112, 0x1000);
		put32(&mut bytes, optional + 116, 0x800);
		let section = optional + 240;
		put32(&mut bytes, section + 8, 0x1000);
		put32(&mut bytes, section + 12, 0x1000);
		put32(&mut bytes, section + 16, 0x1000);
		put32(&mut bytes, section + 20, 0x200);
		put32(&mut bytes, section + 36, 0x60000020);
		let export = 0x200;
		put32(&mut bytes, export + 16, 1);
		put32(&mut bytes, export + 20, names.len() as u32);
		put32(&mut bytes, export + 24, names.len() as u32);
		put32(&mut bytes, export + 28, 0x1040);
		put32(&mut bytes, export + 32, 0x10c0);
		put32(&mut bytes, export + 36, 0x1140);
		let mut text = 0x400;
		for (index, name) in names.iter().enumerate() {
			put32(&mut bytes, 0x240 + index * 4, 0x1900 + index as u32);
			put32(
				&mut bytes,
				0x2c0 + index * 4,
				(text - 0x200 + 0x1000) as u32,
			);
			put16(&mut bytes, 0x340 + index * 2, index as u16);
			bytes[text..text + name.len()].copy_from_slice(name.as_bytes());
			text += name.len() + 1;
		}
		bytes
	}
}
