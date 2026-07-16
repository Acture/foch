use super::file_filter::FileFilter;
use super::{LoadedModSnapshot, WorkspaceScriptCache, cache::load_or_build_mod_snapshot};
use crate::base_data::{
	BaseSnapshotCurrentValidation, InstalledBaseSnapshot, InstalledBaseSnapshotIdentity,
	base_game_mod_id, detect_game_version, installed_base_snapshot_identity,
	load_installed_base_snapshot_from_identity, resolve_game_root, resolve_game_root_and_version,
};
use crate::cache::compute_mod_hash_for_files;
use crate::config::Config;
use crate::request::{CheckRequest, WorkspaceSource};
use foch_core::config::{FochConfig, WorkspaceConfig, WorkspaceImportKind, WorkspaceMod};
use foch_core::domain::ParseErrorKind;
use foch_core::domain::descriptor::load_descriptor;
use foch_core::domain::game::Game;
use foch_core::domain::playlist::{Playlist, PlaylistEntry};
use foch_core::model::{MergeUnitId, ModCandidate};
use foch_core::utils::steam::steam_workshop_mod_path;
use foch_language::analyzer::content_family::{
	ContentLoadPolicy, GameProfile, module_name_for_descriptor,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceResolveErrorKind {
	PlaylistFormat,
	Io,
}

#[derive(Clone, Debug)]
pub struct WorkspaceResolveError {
	pub kind: WorkspaceResolveErrorKind,
	pub path: PathBuf,
	pub message: String,
}

impl fmt::Display for WorkspaceResolveError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}: {}", self.path.display(), self.message)
	}
}

impl Error for WorkspaceResolveError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceTargetRole {
	Game,
	Mod,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceTarget {
	pub path: PathBuf,
	pub role: WorkspaceTargetRole,
	pub mod_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceResolvedMod {
	pub mod_id: String,
	pub display_name: Option<String>,
	pub steam_id: Option<String>,
	pub root_path: Option<PathBuf>,
	pub descriptor_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceResolveSummary {
	pub source_path: PathBuf,
	pub game: Game,
	pub game_root: Option<PathBuf>,
	pub mods: Vec<WorkspaceResolvedMod>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedFileContributor {
	pub mod_id: String,
	pub root_path: PathBuf,
	pub absolute_path: PathBuf,
	pub precedence: usize,
	pub is_base_game: bool,
	pub is_synthetic_base: bool,
	pub parse_ok_hint: Option<bool>,
	pub mod_hash: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedWorkspace {
	pub playlist_path: PathBuf,
	pub playlist: Playlist,
	pub mods: Vec<ModCandidate>,
	pub installed_base_snapshot: Option<InstalledBaseSnapshot>,
	pub cache_game_version: Option<String>,
	pub mod_snapshots: Vec<Option<LoadedModSnapshot>>,
	pub script_cache: WorkspaceScriptCache,
	pub file_inventory: BTreeMap<String, Vec<ResolvedFileContributor>>,
	pub requested_retained_paths: Option<BTreeSet<String>>,
	pub effective_retained_paths: Option<BTreeSet<String>>,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceInventory {
	pub playlist_path: PathBuf,
	pub playlist: Playlist,
	pub mods: Vec<ModCandidate>,
	pub base_game_root: Option<PathBuf>,
	pub mod_cache_game_version: Option<String>,
	pub base_snapshot_identity: Option<InstalledBaseSnapshotIdentity>,
	base_snapshot_current_validation: BaseSnapshotCurrentValidation,
	pub cache_game_version: Option<String>,
	pub snapshot_filter: FileFilter,
	pub mod_hashes: Vec<Option<String>>,
	pub requested_retained_paths: Option<BTreeSet<String>>,
	pub effective_retained_paths: Option<BTreeSet<String>>,
	pub retained_module_policy_versions: BTreeMap<MergeUnitId, u32>,
}

impl WorkspaceInventory {
	pub(crate) fn defer_base_snapshot_current_validation(&mut self) {
		self.base_snapshot_current_validation = BaseSnapshotCurrentValidation::Deferred;
	}
}

#[derive(Clone, Debug)]
struct LoadedWorkspaceSource {
	source_path: PathBuf,
	source_root: PathBuf,
	playlist: Playlist,
	config: Config,
}

fn load_workspace_source(
	request: &CheckRequest,
) -> Result<LoadedWorkspaceSource, WorkspaceResolveError> {
	match &request.source {
		WorkspaceSource::DlcLoad(path) => load_dlc_load_source(path, &request.config),
		WorkspaceSource::Manifest(path) => load_manifest_source(path, &request.config),
	}
}

fn load_dlc_load_source(
	path: &Path,
	config: &Config,
) -> Result<LoadedWorkspaceSource, WorkspaceResolveError> {
	let playlist = Playlist::from_dlc_load(path).map_err(workspace_error_from_parse)?;
	Ok(LoadedWorkspaceSource {
		source_path: path.to_path_buf(),
		source_root: source_root_for(path),
		playlist,
		config: config.clone(),
	})
}

fn load_manifest_source(
	path: &Path,
	config: &Config,
) -> Result<LoadedWorkspaceSource, WorkspaceResolveError> {
	let manifest = FochConfig::load_from_path(path).map_err(|err| WorkspaceResolveError {
		kind: WorkspaceResolveErrorKind::PlaylistFormat,
		path: path.to_path_buf(),
		message: err.to_string(),
	})?;
	let workspace = manifest
		.workspace
		.as_ref()
		.ok_or_else(|| WorkspaceResolveError {
			kind: WorkspaceResolveErrorKind::PlaylistFormat,
			path: path.to_path_buf(),
			message: "foch.toml does not contain a [workspace] table".to_string(),
		})?;
	let source_root = source_root_for(path);
	let mut effective_config = config.clone();
	apply_workspace_config_overrides(&mut effective_config, workspace, &source_root, path)?;
	let playlist = playlist_from_workspace_config(workspace, &source_root, path)?;
	Ok(LoadedWorkspaceSource {
		source_path: path.to_path_buf(),
		source_root,
		playlist,
		config: effective_config,
	})
}

fn workspace_error_from_parse(err: foch_core::domain::ParseError) -> WorkspaceResolveError {
	WorkspaceResolveError {
		kind: if matches!(err.kind, ParseErrorKind::Format) {
			WorkspaceResolveErrorKind::PlaylistFormat
		} else {
			WorkspaceResolveErrorKind::Io
		},
		path: err.path.clone(),
		message: match err.kind {
			ParseErrorKind::Format => err.message,
			ParseErrorKind::Io => format!("failed to read Playset: {err}"),
		},
	}
}

fn apply_workspace_config_overrides(
	config: &mut Config,
	workspace: &WorkspaceConfig,
	source_root: &Path,
	manifest_path: &Path,
) -> Result<(), WorkspaceResolveError> {
	if let Some(path) = workspace.paradox_data_path.as_ref() {
		config.paradox_data_path = Some(resolve_manifest_path(source_root, path));
	}
	if let Some(path) = workspace.game_path.as_ref() {
		let game = workspace
			.game
			.as_ref()
			.ok_or_else(|| WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::PlaylistFormat,
				path: manifest_path.to_path_buf(),
				message: "[workspace].game_path requires [workspace].game".to_string(),
			})?;
		config.game_path.insert(
			game.key().to_string(),
			resolve_manifest_path(source_root, path),
		);
	}
	Ok(())
}

fn playlist_from_workspace_config(
	workspace: &WorkspaceConfig,
	source_root: &Path,
	manifest_path: &Path,
) -> Result<Playlist, WorkspaceResolveError> {
	let mut game = workspace.game.clone();
	let mut mods = Vec::new();
	let mut next_position = 0usize;
	for import in &workspace.imports {
		match import.kind {
			WorkspaceImportKind::DlcLoad => {
				let import_path = resolve_manifest_path(source_root, &import.path);
				let imported =
					Playlist::from_dlc_load(&import_path).map_err(workspace_error_from_parse)?;
				let import_root = source_root_for(&import_path);
				if game.is_none() {
					game = Some(imported.game);
				}
				for mut entry in imported.mods {
					if entry.root_path.is_none()
						&& let Some(steam_id) = entry.steam_id.as_deref()
					{
						entry.root_path = resolve_mod_from_ugc_descriptor(&import_root, steam_id);
					}
					entry.position = Some(next_position);
					next_position += 1;
					mods.push(entry);
				}
			}
		}
	}
	for manifest_mod in &workspace.mods {
		if !manifest_mod.enabled {
			continue;
		}
		let entry = playlist_entry_from_workspace_mod(
			manifest_mod,
			source_root,
			next_position,
			manifest_path,
		)?;
		next_position += 1;
		remove_duplicate_entries(&mut mods, &entry);
		mods.push(entry);
	}
	let game = game.unwrap_or(Game::EuropaUniversalis4);
	Ok(Playlist {
		game,
		name: manifest_path
			.file_stem()
			.and_then(|stem| stem.to_str())
			.filter(|stem| !stem.is_empty())
			.unwrap_or("workspace")
			.to_string(),
		mods,
	})
}

fn playlist_entry_from_workspace_mod(
	manifest_mod: &WorkspaceMod,
	source_root: &Path,
	next_position: usize,
	manifest_path: &Path,
) -> Result<PlaylistEntry, WorkspaceResolveError> {
	let root_path = manifest_mod
		.path
		.as_ref()
		.map(|path| resolve_manifest_path(source_root, path));
	let steam_id = manifest_mod
		.steam_id
		.as_ref()
		.map(|steam_id| steam_id.trim().to_string())
		.filter(|steam_id| !steam_id.is_empty());
	let id = manifest_mod
		.id
		.as_ref()
		.map(|id| id.trim().to_string())
		.filter(|id| !id.is_empty());
	if root_path.is_none() && steam_id.is_none() {
		return Err(WorkspaceResolveError {
			kind: WorkspaceResolveErrorKind::PlaylistFormat,
			path: manifest_path.to_path_buf(),
			message: "[[workspace.mods]] entries require at least one of path or steam_id"
				.to_string(),
		});
	}
	let display_name = id
		.clone()
		.or_else(|| steam_id.as_ref().map(|steam_id| format!("ugc_{steam_id}")));
	Ok(PlaylistEntry {
		id,
		display_name,
		enabled: true,
		position: Some(manifest_mod.position.unwrap_or(next_position)),
		steam_id,
		root_path,
	})
}

fn remove_duplicate_entries(entries: &mut Vec<PlaylistEntry>, explicit: &PlaylistEntry) {
	let keys = playlist_entry_identity_keys(explicit);
	if keys.is_empty() {
		return;
	}
	entries.retain(|entry| {
		playlist_entry_identity_keys(entry)
			.iter()
			.all(|key| !keys.contains(key))
	});
}

fn playlist_entry_identity_keys(entry: &PlaylistEntry) -> HashSet<String> {
	let mut keys = HashSet::new();
	if let Some(steam_id) = entry
		.steam_id
		.as_ref()
		.filter(|value| !value.trim().is_empty())
	{
		keys.insert(format!("steam:{steam_id}"));
	}
	if let Some(id) = entry.id.as_ref().filter(|value| !value.trim().is_empty()) {
		keys.insert(format!("id:{id}"));
	}
	if let Some(root) = entry.root_path.as_ref() {
		keys.insert(format!("path:{}", normalize_relative_path(root)));
	}
	keys
}

fn source_root_for(path: &Path) -> PathBuf {
	path.parent()
		.map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn resolve_manifest_path(source_root: &Path, path: &Path) -> PathBuf {
	if path.is_absolute() {
		path.to_path_buf()
	} else {
		source_root.join(path)
	}
}

pub(crate) fn build_workspace_inventory(
	request: &CheckRequest,
	include_game_base: bool,
) -> Result<WorkspaceInventory, WorkspaceResolveError> {
	build_workspace_inventory_with_hash_cache(request, include_game_base, false, None)
}

pub fn resolve_workspace_summary(
	request: &CheckRequest,
) -> Result<WorkspaceResolveSummary, WorkspaceResolveError> {
	let loaded = load_workspace_source(request)?;
	let snapshot_filter = match FileFilter::new(
		loaded.playlist.game.clone(),
		&loaded.config.extra_ignore_patterns,
	) {
		Ok(filter) => filter,
		Err(message) => {
			tracing::warn!(target: "foch::workspace::resolve", message, "falling back to filter without extra ignore globs");
			FileFilter::for_game(loaded.playlist.game.clone())
		}
	};
	let mods = build_mod_candidates_with_filter(
		&loaded.source_root,
		&loaded.config,
		&loaded.playlist,
		&snapshot_filter,
		None,
	);
	let game_root = resolve_game_root(&loaded.config, &loaded.playlist.game);
	Ok(WorkspaceResolveSummary {
		source_path: loaded.source_path,
		game: loaded.playlist.game,
		game_root,
		mods: mods
			.into_iter()
			.map(|mod_item| WorkspaceResolvedMod {
				mod_id: mod_item.mod_id,
				display_name: mod_item.entry.display_name,
				steam_id: mod_item.entry.steam_id,
				root_path: mod_item.root_path,
				descriptor_error: mod_item.descriptor_error,
			})
			.collect(),
	})
}

pub fn resolve_workspace_targets(
	request: &CheckRequest,
	include_game_root: bool,
) -> Result<Vec<WorkspaceTarget>, WorkspaceResolveError> {
	let summary = resolve_workspace_summary(request)?;
	let mut targets = Vec::new();
	if include_game_root
		&& let Some(path) = summary.game_root
		&& path.is_dir()
	{
		targets.push(WorkspaceTarget {
			path,
			role: WorkspaceTargetRole::Game,
			mod_id: None,
		});
	}
	for mod_item in summary.mods {
		if let Some(path) = mod_item.root_path.filter(|path| path.is_dir()) {
			targets.push(WorkspaceTarget {
				path,
				role: WorkspaceTargetRole::Mod,
				mod_id: Some(mod_item.mod_id),
			});
		}
	}
	Ok(targets)
}

fn game_profile(game: &Game) -> Option<&'static dyn GameProfile> {
	match game {
		Game::EuropaUniversalis4 => Some(eu4_profile()),
		Game::CrusaderKings3
		| Game::Victoria3
		| Game::Stellaris
		| Game::HeartsOfIron4
		| Game::Unknown => None,
	}
}

fn retained_definition_modules(
	game: &Game,
	requested_paths: &BTreeSet<String>,
) -> BTreeMap<MergeUnitId, u32> {
	let Some(profile) = game_profile(game) else {
		return BTreeMap::new();
	};
	requested_paths
		.iter()
		.filter_map(|path| {
			let descriptor = profile.classify_content_family(Path::new(path))?;
			let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
				return None;
			};
			Some((
				MergeUnitId {
					family_id: descriptor.id.as_str().to_string(),
					module_name: module_name_for_descriptor(Path::new(path), descriptor),
				},
				policy.policy_version,
			))
		})
		.collect()
}

fn expand_retained_paths_for_game<'a>(
	game: &Game,
	requested_paths: Option<&BTreeSet<String>>,
	available_paths: impl IntoIterator<Item = &'a str>,
) -> Option<BTreeSet<String>> {
	let requested_paths = requested_paths?;
	let mut effective = requested_paths
		.iter()
		.map(|path| normalize_relative_path(Path::new(path)))
		.collect::<BTreeSet<_>>();
	let selected_modules = retained_definition_modules(game, &effective);
	if selected_modules.is_empty() {
		return Some(effective);
	}
	let Some(profile) = game_profile(game) else {
		return Some(effective);
	};
	for available_path in available_paths {
		let normalized = normalize_relative_path(Path::new(available_path));
		let Some(descriptor) = profile.classify_content_family(Path::new(&normalized)) else {
			continue;
		};
		if !matches!(
			descriptor.load_policy,
			ContentLoadPolicy::DefinitionModule(_)
		) {
			continue;
		}
		let module = MergeUnitId {
			family_id: descriptor.id.as_str().to_string(),
			module_name: module_name_for_descriptor(Path::new(&normalized), descriptor),
		};
		if selected_modules.contains_key(&module) {
			effective.insert(normalized);
		}
	}
	Some(effective)
}

pub(crate) fn build_workspace_inventory_with_hash_cache(
	request: &CheckRequest,
	include_game_base: bool,
	use_process_hash_cache: bool,
	retained_paths: Option<&BTreeSet<String>>,
) -> Result<WorkspaceInventory, WorkspaceResolveError> {
	let loaded = load_workspace_source(request)?;
	let LoadedWorkspaceSource {
		source_path,
		source_root,
		playlist,
		config,
	} = loaded;

	let snapshot_filter = match FileFilter::new(
		playlist.game.clone(),
		&config.extra_ignore_patterns,
	) {
		Ok(filter) => filter,
		Err(message) => {
			tracing::warn!(target: "foch::workspace::resolve", message, "falling back to filter without extra ignore globs");
			FileFilter::for_game(playlist.game.clone())
		}
	};
	let mut mods =
		build_mod_candidates_with_filter(&source_root, &config, &playlist, &snapshot_filter, None);
	let available_mod_paths = mods
		.iter()
		.flat_map(|mod_item| mod_item.files.iter())
		.map(|path| normalize_relative_path(path))
		.collect::<Vec<_>>();
	let effective_retained_paths = expand_retained_paths_for_game(
		&playlist.game,
		retained_paths,
		available_mod_paths.iter().map(String::as_str),
	);
	if let Some(effective_retained_paths) = effective_retained_paths.as_ref() {
		for mod_item in &mut mods {
			mod_item.files.retain(|relative| {
				effective_retained_paths.contains(&normalize_relative_path(relative))
			});
		}
	}
	let retained_module_policy_versions = effective_retained_paths
		.as_ref()
		.map(|paths| retained_definition_modules(&playlist.game, paths))
		.unwrap_or_default();
	let optional_game_root = resolve_game_root(&config, &playlist.game);
	let (base_game_root, mod_cache_game_version) = if include_game_base {
		let (game_root, game_version) = resolve_game_root_and_version(&config, &playlist.game)
			.map_err(|message| WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: source_path.clone(),
				message,
			})?;
		(Some(game_root), Some(game_version))
	} else {
		(
			None,
			optional_game_root
				.as_ref()
				.and_then(|game_root| detect_game_version(game_root)),
		)
	};
	let base_snapshot_identity = if let (Some(game_root), Some(game_version)) =
		(base_game_root.as_ref(), mod_cache_game_version.as_ref())
	{
		if let Some(lease) = request.base_snapshot_lease.as_ref() {
			Some(lease.clone())
		} else {
			Some(
				installed_base_snapshot_identity(playlist.game.key(), game_version)
					.map_err(|message| WorkspaceResolveError {
						kind: WorkspaceResolveErrorKind::Io,
						path: source_path.clone(),
						message,
					})?
					.ok_or_else(|| WorkspaceResolveError {
						kind: WorkspaceResolveErrorKind::Io,
						path: source_path.clone(),
						message: missing_base_data_message(&playlist.game, game_version, game_root),
					})?,
			)
		}
	} else {
		None
	};
	if request.base_snapshot_lease.is_some() && base_snapshot_identity.is_none() {
		return Err(WorkspaceResolveError {
			kind: WorkspaceResolveErrorKind::Io,
			path: source_path.clone(),
			message:
				"an exact base snapshot lease was supplied, but game-base resolution is disabled"
					.to_string(),
		});
	}
	if let Some(expected) = request.expected_base_snapshot_identity.as_deref() {
		let Some(actual) = base_snapshot_identity.as_ref() else {
			return Err(WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: source_path.clone(),
				message: format!(
					"expected base snapshot identity {expected}, but game-base resolution is disabled"
				),
			});
		};
		if actual.as_label() != expected {
			return Err(WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: source_path.clone(),
				message: format!(
					"installed base snapshot identity mismatch: expected {expected}, found {actual}"
				),
			});
		}
	}
	let cache_game_version = mod_cache_game_version.as_ref().map(|version| {
		let mut identity = format!("{} {version}", playlist.game.key());
		if let Some(base_snapshot_identity) = base_snapshot_identity.as_ref() {
			identity.push_str(" base=");
			identity.push_str(&base_snapshot_identity.as_label());
		}
		identity
	});
	let mod_hashes = mods
		.iter()
		.map(|mod_item| {
			if use_process_hash_cache {
				compute_candidate_hash_with_process_cache(mod_item)
			} else {
				compute_candidate_hash(mod_item)
			}
		})
		.collect();

	Ok(WorkspaceInventory {
		playlist_path: source_path,
		playlist,
		mods,
		base_game_root,
		mod_cache_game_version,
		base_snapshot_identity,
		base_snapshot_current_validation: if request.base_snapshot_lease.is_some() {
			BaseSnapshotCurrentValidation::Deferred
		} else {
			BaseSnapshotCurrentValidation::Immediate
		},
		cache_game_version,
		snapshot_filter,
		mod_hashes,
		requested_retained_paths: retained_paths.cloned(),
		effective_retained_paths,
		retained_module_policy_versions,
	})
}

pub(crate) fn resolve_workspace(
	request: &CheckRequest,
	include_game_base: bool,
) -> Result<ResolvedWorkspace, WorkspaceResolveError> {
	let inventory = build_workspace_inventory(request, include_game_base)?;
	resolve_workspace_from_inventory(inventory)
}

pub(crate) fn resolve_workspace_from_inventory(
	inventory: WorkspaceInventory,
) -> Result<ResolvedWorkspace, WorkspaceResolveError> {
	let WorkspaceInventory {
		playlist_path,
		playlist,
		mods,
		base_game_root,
		mod_cache_game_version,
		base_snapshot_identity,
		base_snapshot_current_validation,
		cache_game_version,
		snapshot_filter,
		mod_hashes,
		requested_retained_paths,
		effective_retained_paths,
		retained_module_policy_versions: _,
	} = inventory;
	let installed_base_snapshot = match (
		base_game_root.as_ref(),
		mod_cache_game_version.as_ref(),
		base_snapshot_identity.as_ref(),
	) {
		(Some(_game_root), Some(game_version), Some(identity)) => Some(
			load_installed_base_snapshot_from_identity(
				playlist.game.key(),
				game_version,
				identity,
				base_snapshot_current_validation,
			)
			.map_err(|message| WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: playlist_path.clone(),
				message,
			})?,
		),
		(Some(game_root), Some(game_version), None) => {
			return Err(WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: playlist_path.clone(),
				message: missing_base_data_message(&playlist.game, game_version, game_root),
			});
		}
		_ => None,
	};
	let mut available_paths = effective_retained_paths
		.as_ref()
		.into_iter()
		.flatten()
		.cloned()
		.collect::<Vec<_>>();
	if let Some(base_snapshot) = installed_base_snapshot.as_ref() {
		available_paths.extend(base_snapshot.snapshot.inventory_paths.iter().cloned());
	}
	let effective_retained_paths = expand_retained_paths_for_game(
		&playlist.game,
		requested_retained_paths.as_ref(),
		available_paths.iter().map(String::as_str),
	);
	let mod_snapshots: Vec<Option<LoadedModSnapshot>> = mods
		.iter()
		.enumerate()
		.map(|(idx, mod_item)| {
			load_or_build_mod_snapshot(
				playlist.game.key(),
				mod_cache_game_version.as_deref(),
				mod_item,
				&snapshot_filter,
				mod_hashes.get(idx).and_then(|hash| hash.as_deref()),
				requested_retained_paths.is_none(),
			)
		})
		.collect();
	let mut file_inventory = build_file_inventory(
		&playlist,
		&mods,
		&mod_snapshots,
		base_game_root.as_ref(),
		installed_base_snapshot.as_ref(),
		&mod_hashes,
		effective_retained_paths.as_ref(),
	);
	inject_synthetic_bases(&mut file_inventory);
	let script_cache = WorkspaceScriptCache::from_parts(
		&mods,
		&mod_snapshots,
		installed_base_snapshot.as_ref(),
		base_game_root.as_deref(),
	);

	Ok(ResolvedWorkspace {
		playlist_path,
		playlist,
		mods,
		installed_base_snapshot,
		cache_game_version,
		mod_snapshots,
		script_cache,
		file_inventory,
		requested_retained_paths,
		effective_retained_paths,
	})
}

fn missing_base_data_message(game: &Game, game_version: &str, game_root: &Path) -> String {
	format!(
		"missing installed base data for {} {}; run `foch data install {} --game-version auto` or `foch data build {} --from-game-path {} --game-version auto --install`, or use --no-game-base",
		game.key(),
		game_version,
		game.key(),
		game.key(),
		game_root.display()
	)
}

pub(crate) fn build_mod_candidates_with_filter(
	source_root: &Path,
	config: &Config,
	playlist: &Playlist,
	filter: &FileFilter,
	retained_paths: Option<&BTreeSet<String>>,
) -> Vec<ModCandidate> {
	let mut entries = playlist.mods.clone();
	entries.sort_by_key(|entry| entry.position.unwrap_or(usize::MAX));

	entries
		.into_iter()
		.map(|entry| {
			let mod_id = entry
				.steam_id
				.clone()
				.filter(|x| !x.trim().is_empty())
				.or_else(|| entry.id.clone().filter(|x| !x.trim().is_empty()))
				.unwrap_or_else(|| "<missing-steam-id>".to_string());

			let root_path = resolve_mod_root(source_root, config, playlist, &entry);
			let descriptor_path = root_path.as_ref().map(|path| path.join("descriptor.mod"));

			let (descriptor, descriptor_error) = match descriptor_path.as_ref() {
				Some(path) if path.exists() => match load_descriptor(path) {
					Ok(descriptor) => (Some(descriptor), None),
					Err(err) => (None, Some(err.to_string())),
				},
				Some(path) => (None, Some(format!("{} does not exist", path.display()))),
				None => (None, None),
			};

			let mut files = root_path
				.as_ref()
				.map_or_else(Vec::new, |root| collect_relative_files(root, filter));
			if let Some(retained_paths) = retained_paths {
				files
					.retain(|relative| retained_paths.contains(&normalize_relative_path(relative)));
			}

			ModCandidate {
				entry,
				mod_id,
				root_path,
				descriptor_path,
				descriptor,
				descriptor_error,
				files,
			}
		})
		.collect()
}

fn compute_candidate_hash(mod_item: &ModCandidate) -> Option<String> {
	let root = mod_item.root_path.as_ref()?;
	let files = candidate_hash_files(root, mod_item);
	compute_mod_hash_for_files(root, &files).ok()
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CandidateHashCacheKey {
	root: String,
	files: Vec<CandidateFileFingerprint>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CandidateFileFingerprint {
	path: String,
	len: u64,
	modified_ns: Option<u128>,
}

static CANDIDATE_HASH_CACHE: OnceLock<Mutex<HashMap<CandidateHashCacheKey, String>>> =
	OnceLock::new();

fn compute_candidate_hash_with_process_cache(mod_item: &ModCandidate) -> Option<String> {
	let root = mod_item.root_path.as_ref()?;
	let files = candidate_hash_files(root, mod_item);
	let Some(key) = candidate_hash_cache_key(root, &files) else {
		return compute_candidate_hash(mod_item);
	};
	let cache = CANDIDATE_HASH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
	if let Ok(guard) = cache.lock()
		&& let Some(hash) = guard.get(&key)
	{
		return Some(hash.clone());
	}
	let hash = compute_mod_hash_for_files(root, &files).ok()?;
	if let Ok(mut guard) = cache.lock() {
		guard.insert(key, hash.clone());
	}
	Some(hash)
}

fn candidate_hash_files(root: &Path, mod_item: &ModCandidate) -> Vec<PathBuf> {
	let mut files = mod_item.files.clone();
	if let Some(descriptor_path) = mod_item
		.descriptor_path
		.as_ref()
		.filter(|path| path.is_file())
		&& let Ok(relative) = descriptor_path.strip_prefix(root)
		&& !files.iter().any(|path| path == relative)
	{
		files.push(relative.to_path_buf());
	}
	files
}

fn candidate_hash_cache_key(root: &Path, files: &[PathBuf]) -> Option<CandidateHashCacheKey> {
	let mut fingerprints = Vec::with_capacity(files.len());
	for relative in files {
		let metadata = fs::metadata(root.join(relative)).ok()?;
		let modified_ns = metadata
			.modified()
			.ok()
			.and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
			.map(|duration| duration.as_nanos());
		fingerprints.push(CandidateFileFingerprint {
			path: normalize_relative_path(relative),
			len: metadata.len(),
			modified_ns,
		});
	}
	fingerprints.sort_by(|left, right| left.path.cmp(&right.path));
	Some(CandidateHashCacheKey {
		root: normalize_relative_path(root),
		files: fingerprints,
	})
}

fn resolve_mod_root(
	source_root: &Path,
	config: &Config,
	playlist: &Playlist,
	entry: &PlaylistEntry,
) -> Option<PathBuf> {
	if let Some(path) = entry.root_path.as_ref() {
		return Some(path.clone());
	}

	let mut candidates = Vec::new();

	if let Some(steam_id) = entry.steam_id.as_ref() {
		candidates.push(source_root.join(steam_id));
		candidates.push(source_root.join(format!("mod_{steam_id}")));

		// The dlc_load.json's parent directory IS the paradox game data
		// directory by construction, so the sibling `mod/ugc_<id>.mod`
		// descriptor is always a valid lookup root regardless of whether the
		// user has separately configured `paradox_data_path`. This makes
		// playset discovery work end-to-end without forcing every test fixture
		// to additionally pin a config field.
		if let Some(root) = resolve_mod_from_ugc_descriptor(source_root, steam_id) {
			candidates.push(root);
		}
		candidates.push(source_root.join("mod").join(steam_id));
		candidates.push(source_root.join("mod").join(format!("ugc_{steam_id}")));

		if let Some(path) = config.paradox_data_path.as_ref() {
			for game_data_dir in paradox_game_data_dirs(path, &playlist.game) {
				if let Some(root) = resolve_mod_from_ugc_descriptor(&game_data_dir, steam_id) {
					candidates.push(root);
				}
				candidates.push(game_data_dir.join("mod").join(steam_id));
				candidates.push(game_data_dir.join("mod").join(format!("ugc_{steam_id}")));
			}
		}

		if let Some(steam_root) = config.steam_root_path.as_ref() {
			for app_id in playlist.game.steam_app_ids() {
				if let Some(path) = steam_workshop_mod_path(steam_root, *app_id, steam_id) {
					candidates.push(path);
				}
				candidates.push(
					steam_root
						.join("steamapps")
						.join("workshop")
						.join("content")
						.join(app_id.to_string())
						.join(steam_id),
				);
			}
		}
	}

	if let Some(name) = entry.display_name.as_ref() {
		candidates.push(source_root.join(name));
		candidates.push(source_root.join(name.replace(' ', "_")));
	}

	dedup_candidates(candidates)
		.into_iter()
		.find(|candidate| candidate.is_dir())
}

fn dedup_candidates(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
	let mut seen = HashSet::new();
	let mut result = Vec::new();
	for candidate in candidates {
		let key = candidate.to_string_lossy().replace('\\', "/");
		if !seen.insert(key) {
			continue;
		}
		result.push(candidate);
	}
	result
}

fn paradox_game_data_dirs(base: &Path, game: &Game) -> Vec<PathBuf> {
	let mut dirs = vec![base.to_path_buf()];
	if let Some(game_dir_name) = game.paradox_data_dir_name() {
		dirs.push(base.join(game_dir_name));
	}
	dedup_candidates(dirs)
}

fn resolve_mod_from_ugc_descriptor(game_data_dir: &Path, steam_id: &str) -> Option<PathBuf> {
	let metadata = game_data_dir
		.join("mod")
		.join(format!("ugc_{steam_id}.mod"));
	if !metadata.is_file() {
		return None;
	}

	let descriptor = load_descriptor(&metadata).ok()?;
	let raw_path = descriptor.path?;
	descriptor_path_candidates(game_data_dir, &raw_path)
		.into_iter()
		.find(|candidate| candidate.is_dir())
}

fn descriptor_path_candidates(game_data_dir: &Path, raw: &str) -> Vec<PathBuf> {
	let mut fragments = vec![raw.to_string()];
	if raw.contains('\\') {
		fragments.push(raw.replace('\\', "/"));
	}
	if raw.contains('/') {
		fragments.push(raw.replace('/', "\\"));
	}

	let mut candidates = Vec::new();
	for fragment in fragments {
		let path = PathBuf::from(&fragment);
		if path.is_absolute() {
			candidates.push(path.clone());
		}
		candidates.push(game_data_dir.join(&path));
		candidates.push(game_data_dir.join("mod").join(&path));
	}

	dedup_candidates(candidates)
}

pub(crate) fn collect_relative_files(root: &Path, filter: &FileFilter) -> Vec<PathBuf> {
	let mut files = Vec::new();

	for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
		if !entry.file_type().is_file() {
			continue;
		}

		let path = entry.path();
		if path.file_name() == Some(OsStr::new("descriptor.mod")) {
			continue;
		}

		if let Ok(relative) = path.strip_prefix(root) {
			if !filter.accepts(relative) {
				continue;
			}
			files.push(relative.to_path_buf());
		}
	}

	files.sort();
	files
}

pub(crate) fn build_file_inventory(
	playlist: &Playlist,
	mods: &[ModCandidate],
	mod_snapshots: &[Option<LoadedModSnapshot>],
	base_game_root: Option<&PathBuf>,
	installed_base_snapshot: Option<&InstalledBaseSnapshot>,
	mod_hashes: &[Option<String>],
	retained_paths: Option<&BTreeSet<String>>,
) -> BTreeMap<String, Vec<ResolvedFileContributor>> {
	let mut inventory = BTreeMap::new();
	let mut precedence = 0;

	if let (Some(root), Some(snapshot)) = (base_game_root, installed_base_snapshot) {
		let mod_id = base_game_mod_id(playlist.game.key());
		let document_lookup = snapshot.snapshot.document_lookup();
		for relative in &snapshot.snapshot.inventory_paths {
			if retained_paths.is_some_and(|paths| !paths.contains(relative)) {
				continue;
			}
			let document = document_lookup.get(relative.as_str());
			inventory
				.entry(relative.clone())
				.or_insert_with(Vec::new)
				.push(ResolvedFileContributor {
					mod_id: mod_id.clone(),
					root_path: root.clone(),
					absolute_path: root.join(relative),
					precedence,
					is_base_game: true,
					is_synthetic_base: false,
					parse_ok_hint: document.map(|(_family, parse_ok)| *parse_ok),
					mod_hash: None,
				});
		}
		precedence += 1;
	}

	for (idx, mod_item) in mods.iter().enumerate() {
		let Some(root) = mod_item.root_path.as_ref() else {
			continue;
		};
		let parse_hints = mod_snapshots
			.get(idx)
			.and_then(|snapshot| snapshot.as_ref())
			.map(|snapshot| &snapshot.document_parse_hints);
		let mod_hash = mod_snapshots
			.get(idx)
			.and_then(|snapshot| snapshot.as_ref())
			.and_then(|snapshot| snapshot.mod_hash.clone())
			.or_else(|| mod_hashes.get(idx).cloned().flatten());
		for relative in &mod_item.files {
			let key = normalize_relative_path(relative);
			let parse_ok_hint = parse_hints.and_then(|hints| hints.get(&key).copied());
			inventory
				.entry(key)
				.or_insert_with(Vec::new)
				.push(ResolvedFileContributor {
					mod_id: mod_item.mod_id.clone(),
					root_path: root.clone(),
					absolute_path: root.join(relative),
					precedence,
					is_base_game: false,
					is_synthetic_base: false,
					parse_ok_hint,
					mod_hash: mod_hash.clone(),
				});
		}
		precedence += 1;
	}

	inventory
}

pub(crate) fn normalize_relative_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

/// 当 file_inventory 中某个文件没有 base game 贡献者，但有 ≥2 个 mod 贡献者时，
/// 选取 precedence 最小的 mod（tie 用 mod_id 字典序）clone 一份作为合成 base，
/// 插入到 contributors 最前面。这样下游的 patch 引擎可以把所有 mod 视为对该 base 的 patch。
///
/// 合成 base 的特征：`is_synthetic_base = true`，`is_base_game = false`，`precedence = 0`。
/// 原贡献者保持不动。
pub(crate) fn inject_synthetic_bases(
	file_inventory: &mut BTreeMap<String, Vec<ResolvedFileContributor>>,
) {
	for contributors in file_inventory.values_mut() {
		if contributors.iter().any(|c| c.is_base_game) {
			continue;
		}
		let non_base_count = contributors.iter().filter(|c| !c.is_base_game).count();
		if non_base_count < 2 {
			continue;
		}
		let Some(seed) = contributors
			.iter()
			.filter(|c| !c.is_base_game)
			.min_by(|a, b| {
				a.precedence
					.cmp(&b.precedence)
					.then_with(|| a.mod_id.cmp(&b.mod_id))
			})
		else {
			continue;
		};
		let mut synthetic = seed.clone();
		synthetic.is_synthetic_base = true;
		synthetic.is_base_game = false;
		synthetic.precedence = 0;
		contributors.insert(0, synthetic);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	fn descriptor_path_value(path: &Path) -> String {
		path.to_string_lossy()
			.replace('\\', "/")
			.replace('"', "\\\"")
	}

	fn write_descriptor(root: &Path, name: &str, steam_id: Option<&str>) {
		fs::create_dir_all(root).expect("create descriptor root");
		let mut body = format!(
			"name=\"{name}\"\npath=\"{}\"\n",
			descriptor_path_value(root)
		);
		if let Some(steam_id) = steam_id {
			body.push_str(&format!("remote_file_id=\"{steam_id}\"\n"));
		}
		fs::write(root.join("descriptor.mod"), body).expect("write descriptor");
	}

	fn write_dlc_load(paradox_dir: &Path, mods: &[(&str, &Path)]) {
		fs::create_dir_all(paradox_dir.join("mod")).expect("create launcher mod dir");
		let entries = mods
			.iter()
			.map(|(steam_id, _root)| format!("mod/ugc_{steam_id}.mod"))
			.collect::<Vec<_>>();
		let payload =
			serde_json::json!({ "enabled_mods": entries, "disabled_dlcs": Vec::<String>::new() });
		fs::write(
			paradox_dir.join("dlc_load.json"),
			serde_json::to_string_pretty(&payload).expect("serialize dlc_load"),
		)
		.expect("write dlc_load");
		for (steam_id, root) in mods {
			fs::write(
				paradox_dir.join("mod").join(format!("ugc_{steam_id}.mod")),
				format!(
					"name=\"ugc_{steam_id}\"\npath=\"{}\"\nremote_file_id=\"{steam_id}\"\n",
					descriptor_path_value(root)
				),
			)
			.expect("write launcher descriptor");
		}
	}

	fn request_for_manifest(path: &Path) -> CheckRequest {
		CheckRequest::from_manifest_path(path.to_path_buf(), Config::default())
	}

	#[test]
	fn manifest_summary_imports_dlc_load_and_appends_path_mod() {
		let temp = TempDir::new().expect("tempdir");
		let game_root = temp.path().join("game-root");
		let paradox_dir = temp.path().join("Europa Universalis IV");
		let imported_root = temp.path().join("imported_mod");
		let local_root = temp.path().join("local_patch");
		fs::create_dir_all(&game_root).expect("create game root");
		write_descriptor(&imported_root, "Imported Mod", Some("1001"));
		write_descriptor(&local_root, "Local Patch", None);
		write_dlc_load(&paradox_dir, &[("1001", imported_root.as_path())]);
		let manifest_path = temp.path().join("foch.toml");
		fs::write(
			&manifest_path,
			r#"
[workspace]
game = "eu4"
game_path = "game-root"

[[workspace.imports]]
kind = "dlc_load"
path = "Europa Universalis IV/dlc_load.json"

[[workspace.mods]]
id = "local_patch"
path = "local_patch"
"#,
		)
		.expect("write manifest");

		let request = request_for_manifest(&manifest_path);
		let summary = resolve_workspace_summary(&request).expect("resolve manifest");
		assert_eq!(summary.game, Game::EuropaUniversalis4);
		assert_eq!(summary.game_root.as_deref(), Some(game_root.as_path()));
		assert_eq!(summary.mods.len(), 2);
		assert_eq!(summary.mods[0].mod_id, "1001");
		assert_eq!(
			summary.mods[0].root_path.as_deref(),
			Some(imported_root.as_path())
		);
		assert_eq!(summary.mods[1].mod_id, "local_patch");
		assert_eq!(
			summary.mods[1].root_path.as_deref(),
			Some(local_root.as_path())
		);

		let targets = resolve_workspace_targets(&request, true).expect("resolve targets");
		assert_eq!(targets.len(), 3);
		assert!(
			targets
				.iter()
				.any(|target| target.role == WorkspaceTargetRole::Game)
		);
		assert!(targets.iter().any(|target| target.path == imported_root));
		assert!(targets.iter().any(|target| target.path == local_root));
	}

	#[test]
	fn manifest_explicit_mod_overrides_imported_duplicate() {
		let temp = TempDir::new().expect("tempdir");
		let paradox_dir = temp.path().join("Europa Universalis IV");
		let imported_root = temp.path().join("imported_mod");
		let override_root = temp.path().join("override_mod");
		write_descriptor(&imported_root, "Imported Mod", Some("1001"));
		write_descriptor(&override_root, "Override Mod", Some("1001"));
		write_dlc_load(&paradox_dir, &[("1001", imported_root.as_path())]);
		let manifest_path = temp.path().join("foch.toml");
		fs::write(
			&manifest_path,
			r#"
[workspace]
game = "eu4"

[[workspace.imports]]
kind = "dlc_load"
path = "Europa Universalis IV/dlc_load.json"

[[workspace.mods]]
id = "override"
steam_id = "1001"
path = "override_mod"
"#,
		)
		.expect("write manifest");

		let summary = resolve_workspace_summary(&request_for_manifest(&manifest_path))
			.expect("resolve manifest");
		assert_eq!(summary.mods.len(), 1);
		assert_eq!(summary.mods[0].mod_id, "1001");
		assert_eq!(
			summary.mods[0].root_path.as_deref(),
			Some(override_root.as_path())
		);
		assert_eq!(summary.mods[0].display_name.as_deref(), Some("override"));
	}

	#[test]
	fn manifest_rejects_mod_without_path_or_steam_id() {
		let temp = TempDir::new().expect("tempdir");
		let manifest_path = temp.path().join("foch.toml");
		fs::write(
			&manifest_path,
			r#"
[workspace]
game = "eu4"

[[workspace.mods]]
id = "broken"
"#,
		)
		.expect("write manifest");

		let err = resolve_workspace_summary(&request_for_manifest(&manifest_path))
			.expect_err("invalid manifest mod must fail");
		assert!(
			err.message.contains("at least one of path or steam_id"),
			"error was: {err}"
		);
	}

	#[test]
	fn manifest_explicit_missing_path_does_not_fall_back_to_id_directory() {
		let temp = TempDir::new().expect("tempdir");
		let fallback_root = temp.path().join("local_patch");
		write_descriptor(&fallback_root, "Fallback Root", None);
		let manifest_path = temp.path().join("foch.toml");
		fs::write(
			&manifest_path,
			r#"
[workspace]
game = "eu4"

[[workspace.mods]]
id = "local_patch"
path = "missing-local-patch"
"#,
		)
		.expect("write manifest");

		let summary = resolve_workspace_summary(&request_for_manifest(&manifest_path))
			.expect("resolve manifest");
		assert_eq!(summary.mods.len(), 1);
		let expected_root = temp.path().join("missing-local-patch");
		assert_eq!(
			summary.mods[0].root_path.as_deref(),
			Some(expected_root.as_path())
		);
		let targets = resolve_workspace_targets(&request_for_manifest(&manifest_path), false)
			.expect("targets");
		assert!(targets.is_empty());
	}

	#[test]
	fn retained_governments_path_expands_to_the_complete_definition_module() {
		let temp = TempDir::new().expect("tempdir");
		let mod_root = temp.path().join("governments_mod");
		write_descriptor(&mod_root, "Governments Mod", None);
		let governments = mod_root.join("common/governments");
		fs::create_dir_all(&governments).expect("create governments directory");
		fs::write(
			governments.join("00_governments.txt"),
			"early_government = { basic_reform = early_reform }\n",
		)
		.expect("write early government");
		fs::write(
			governments.join("zzz_governments.txt"),
			"late_government = { basic_reform = late_reform }\n",
		)
		.expect("write late government");
		let unrelated = mod_root.join("common/scripted_effects");
		fs::create_dir_all(&unrelated).expect("create unrelated directory");
		fs::write(unrelated.join("effect.txt"), "effect = { }\n").expect("write unrelated file");
		let manifest_path = temp.path().join("foch.toml");
		fs::write(
			&manifest_path,
			r#"
[workspace]
game = "eu4"

[[workspace.mods]]
id = "governments_mod"
path = "governments_mod"
"#,
		)
		.expect("write manifest");
		let requested = BTreeSet::from(["common/governments/00_governments.txt".to_string()]);

		let inventory = build_workspace_inventory_with_hash_cache(
			&request_for_manifest(&manifest_path),
			false,
			true,
			Some(&requested),
		)
		.expect("build retained workspace inventory");
		let retained_files = inventory.mods[0]
			.files
			.iter()
			.map(|path| normalize_relative_path(path))
			.collect::<BTreeSet<_>>();

		assert_eq!(inventory.requested_retained_paths, Some(requested));
		assert_eq!(
			inventory.effective_retained_paths,
			Some(BTreeSet::from([
				"common/governments/00_governments.txt".to_string(),
				"common/governments/zzz_governments.txt".to_string(),
			]))
		);
		assert_eq!(
			retained_files,
			BTreeSet::from([
				"common/governments/00_governments.txt".to_string(),
				"common/governments/zzz_governments.txt".to_string(),
			])
		);
	}

	#[test]
	fn retained_non_module_path_stays_exact() {
		let available = BTreeSet::from([
			"common/countries/France.txt".to_string(),
			"common/countries/England.txt".to_string(),
		]);
		let requested = BTreeSet::from(["common/countries/France.txt".to_string()]);

		let effective = expand_retained_paths_for_game(
			&Game::EuropaUniversalis4,
			Some(&requested),
			available.iter().map(String::as_str),
		);

		assert_eq!(effective, Some(requested));
	}

	#[test]
	fn retained_module_expansion_includes_available_basegame_siblings() {
		let requested = BTreeSet::from(["common/governments/mod_override.txt".to_string()]);
		let available = BTreeSet::from([
			"common/governments/00_vanilla.txt".to_string(),
			"common/governments/mod_override.txt".to_string(),
			"common/scripted_effects/unrelated.txt".to_string(),
		]);

		let effective = expand_retained_paths_for_game(
			&Game::EuropaUniversalis4,
			Some(&requested),
			available.iter().map(String::as_str),
		);

		assert_eq!(
			effective,
			Some(BTreeSet::from([
				"common/governments/00_vanilla.txt".to_string(),
				"common/governments/mod_override.txt".to_string(),
			]))
		);
	}

	fn make_contributor(
		mod_id: &str,
		precedence: usize,
		is_base_game: bool,
	) -> ResolvedFileContributor {
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(format!("/mods/{mod_id}")),
			absolute_path: PathBuf::from(format!("/mods/{mod_id}/file.txt")),
			precedence,
			is_base_game,
			is_synthetic_base: false,
			parse_ok_hint: None,
			mod_hash: Some(format!("hash-{mod_id}")),
		}
	}

	#[test]
	fn inject_synthetic_bases_no_base_two_mods_creates_synthetic() {
		let mut inventory: BTreeMap<String, Vec<ResolvedFileContributor>> = BTreeMap::new();
		inventory.insert(
			"common/file.txt".to_string(),
			vec![
				make_contributor("mod_b", 5, false),
				make_contributor("mod_a", 3, false),
			],
		);

		inject_synthetic_bases(&mut inventory);

		let contribs = &inventory["common/file.txt"];
		assert_eq!(contribs.len(), 3, "synthetic base should be added");
		let synth = &contribs[0];
		assert!(synth.is_synthetic_base);
		assert!(!synth.is_base_game);
		assert_eq!(synth.precedence, 0);
		assert_eq!(synth.mod_id, "mod_a", "lowest-precedence mod is mod_a (3)");
		// originals preserved
		assert!(!contribs[1].is_synthetic_base);
		assert!(!contribs[2].is_synthetic_base);
		assert_eq!(contribs[1].mod_id, "mod_b");
		assert_eq!(contribs[2].mod_id, "mod_a");
		assert_eq!(contribs[2].precedence, 3);
	}

	#[test]
	fn inject_synthetic_bases_with_real_base_skipped() {
		let mut inventory: BTreeMap<String, Vec<ResolvedFileContributor>> = BTreeMap::new();
		inventory.insert(
			"common/file.txt".to_string(),
			vec![
				make_contributor("base:eu4", 0, true),
				make_contributor("mod_a", 1, false),
				make_contributor("mod_b", 2, false),
			],
		);

		inject_synthetic_bases(&mut inventory);

		let contribs = &inventory["common/file.txt"];
		assert_eq!(contribs.len(), 3, "no synthetic base should be added");
		assert!(!contribs.iter().any(|c| c.is_synthetic_base));
	}

	#[test]
	fn inject_synthetic_bases_single_mod_skipped() {
		let mut inventory: BTreeMap<String, Vec<ResolvedFileContributor>> = BTreeMap::new();
		inventory.insert(
			"common/file.txt".to_string(),
			vec![make_contributor("mod_a", 1, false)],
		);

		inject_synthetic_bases(&mut inventory);

		let contribs = &inventory["common/file.txt"];
		assert_eq!(contribs.len(), 1, "no synthetic base for single mod");
		assert!(!contribs[0].is_synthetic_base);
	}

	#[test]
	fn inject_synthetic_bases_tie_breaks_on_mod_id() {
		let mut inventory: BTreeMap<String, Vec<ResolvedFileContributor>> = BTreeMap::new();
		inventory.insert(
			"common/file.txt".to_string(),
			vec![
				make_contributor("mod_z", 2, false),
				make_contributor("mod_a", 2, false),
			],
		);

		inject_synthetic_bases(&mut inventory);

		let contribs = &inventory["common/file.txt"];
		assert_eq!(contribs.len(), 3);
		assert!(contribs[0].is_synthetic_base);
		assert_eq!(
			contribs[0].mod_id, "mod_a",
			"tie on precedence resolved by mod_id lex"
		);
	}
}
