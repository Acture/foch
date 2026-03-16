use crate::check::analysis::{AnalyzeOptions, analyze_visibility};
use crate::check::documents::{
	build_semantic_index_from_documents, discover_text_documents, parse_text_documents,
};
use crate::check::graph::export_graph;
use crate::check::model::{
	AnalysisMeta, AnalysisMode, CheckContext, CheckRequest, CheckResult, Finding, FindingChannel,
	ModCandidate, RunOptions, SemanticIndex, Severity,
};
use crate::check::rules::{
	check_duplicate_mod_identity, check_duplicate_scripted_effect, check_file_conflict,
	check_missing_dependency, check_missing_descriptor, check_required_fields,
	check_unresolved_scripted_effect,
};
use crate::domain::descriptor::load_descriptor;
use crate::domain::game::Game;
use crate::domain::playlist::{Playlist, PlaylistEntry, load_playlist};
use crate::utils::steam::{steam_game_install_path, steam_workshop_mod_path};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const BASE_GAME_MOD_ID_PREFIX: &str = "__game__";
const BASE_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceResolveErrorKind {
	PlaylistFormat,
	Io,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceResolveError {
	pub kind: WorkspaceResolveErrorKind,
	pub path: PathBuf,
	pub message: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedFileContributor {
	pub mod_id: String,
	pub root_path: PathBuf,
	pub absolute_path: PathBuf,
	pub precedence: usize,
	pub is_base_game: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedWorkspace {
	pub playlist_path: PathBuf,
	pub playlist: Playlist,
	pub mods: Vec<ModCandidate>,
	pub base_game_root: Option<PathBuf>,
	pub file_inventory: BTreeMap<String, Vec<ResolvedFileContributor>>,
}

pub fn run_checks(request: CheckRequest) -> CheckResult {
	run_checks_with_options(request, RunOptions::default())
}

pub fn run_checks_with_options(request: CheckRequest, options: RunOptions) -> CheckResult {
	let mut result = CheckResult::default();

	let resolved = match resolve_workspace(&request, options.include_game_base) {
		Ok(workspace) => workspace,
		Err(err) => {
			if err.kind == WorkspaceResolveErrorKind::PlaylistFormat {
				result.findings.push(Finding {
					rule_id: "R001".to_string(),
					severity: Severity::Error,
					channel: FindingChannel::Strict,
					message: "Playset JSON 无法解析".to_string(),
					mod_id: None,
					path: Some(err.path),
					evidence: Some(err.message),
					line: None,
					column: None,
					confidence: Some(1.0),
				});
				result.recompute_channels();
				return result;
			}
			result.push_fatal_error(err.message);
			return result;
		}
	};

	let base_semantic = if options.include_game_base {
		resolved.base_game_root.as_ref().map(|game_root| {
			load_or_build_game_base_semantic_index(resolved.playlist.game.key(), game_root)
		})
	} else {
		None
	};
	let mod_documents = collect_mod_parsed_documents(&resolved.mods);
	let mod_semantic_index = build_semantic_index_from_documents(&mod_documents);
	let parsed_files_count = mod_documents.len()
		+ base_semantic
			.as_ref()
			.map_or(0, |snapshot| snapshot.parsed_files);
	let semantic_index = match base_semantic {
		Some(snapshot) => merge_semantic_indexes(snapshot.index, mod_semantic_index),
		None => mod_semantic_index,
	};
	let ctx = CheckContext {
		playlist_path: resolved.playlist_path.clone(),
		playlist: resolved.playlist,
		mods: resolved.mods,
		semantic_index,
	};

	result.findings.extend(check_required_fields(&ctx));
	result.findings.extend(check_duplicate_mod_identity(&ctx));
	result.findings.extend(check_missing_descriptor(&ctx));
	result.findings.extend(check_file_conflict(&ctx));
	result.findings.extend(check_missing_dependency(&ctx));
	result
		.findings
		.extend(check_duplicate_scripted_effect(&ctx));
	result
		.findings
		.extend(check_unresolved_scripted_effect(&ctx));

	if options.analysis_mode == AnalysisMode::Semantic {
		let diagnostics = analyze_visibility(
			&ctx.semantic_index,
			&AnalyzeOptions {
				mode: options.analysis_mode,
			},
		);
		result.findings.extend(diagnostics.strict);
		result.findings.extend(diagnostics.advisory);
	}

	result.analysis_meta = AnalysisMeta {
		text_documents: ctx.semantic_index.documents.len(),
		parsed_files: parsed_files_count,
		parse_errors: ctx.semantic_index.parse_issues.len(),
		scopes: ctx.semantic_index.scopes.len(),
		symbol_definitions: ctx.semantic_index.definitions.len(),
		symbol_references: ctx.semantic_index.references.len(),
		alias_usages: ctx.semantic_index.alias_usages.len(),
	};

	if let Some(format) = options.graph_format {
		result.graph_output = Some(export_graph(&ctx.semantic_index, format));
	}

	result.recompute_channels();
	result
}

pub(crate) fn resolve_workspace(
	request: &CheckRequest,
	include_game_base: bool,
) -> Result<ResolvedWorkspace, WorkspaceResolveError> {
	let playlist = load_playlist(&request.playset_path).map_err(|err| WorkspaceResolveError {
		kind: if matches!(err.kind, crate::domain::ParseErrorKind::Format) {
			WorkspaceResolveErrorKind::PlaylistFormat
		} else {
			WorkspaceResolveErrorKind::Io
		},
		path: err.path.clone(),
		message: match err.kind {
			crate::domain::ParseErrorKind::Format => err.message,
			crate::domain::ParseErrorKind::Io => format!("无法读取 Playset: {err}"),
		},
	})?;

	let mods = build_mod_candidates(request, &playlist);
	let base_game_root = if include_game_base {
		Some(
			resolve_game_root(request, &playlist).ok_or_else(|| WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: request.playset_path.clone(),
				message: format!(
					"无法定位 {} 基础游戏目录；请配置 game_path.{} 或 Steam 路径，或使用 --no-game-base",
					playlist.game.key(),
					playlist.game.key()
				),
			})?,
		)
	} else {
		None
	};
	let file_inventory = build_file_inventory(&playlist, &mods, base_game_root.as_ref());

	Ok(ResolvedWorkspace {
		playlist_path: request.playset_path.clone(),
		playlist,
		mods,
		base_game_root,
		file_inventory,
	})
}

fn collect_mod_parsed_documents(
	mods: &[ModCandidate],
) -> Vec<crate::check::documents::ParsedTextDocument> {
	let mut parsed = Vec::new();

	for mod_item in mods {
		let Some(root) = mod_item.root_path.as_ref() else {
			continue;
		};
		parsed.extend(parse_text_documents(&mod_item.mod_id, root));
	}
	parsed
}

#[derive(Clone, Debug)]
struct GameBaseSemanticSnapshot {
	detected_version: Option<String>,
	index: SemanticIndex,
	parsed_files: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BaseSemanticSnapshotEntry {
	schema_version: u32,
	game_key: String,
	detected_version: Option<String>,
	manifest_hash: u64,
	parsed_files: usize,
	index: SemanticIndex,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BuiltinBaseSnapshotEntry {
	game_key: String,
	detected_version: String,
	parsed_files: usize,
	index: SemanticIndex,
}

fn load_or_build_game_base_semantic_index(
	game_key: &str,
	game_root: &Path,
) -> GameBaseSemanticSnapshot {
	let documents = discover_text_documents(game_root);
	let parsed_files = documents.len();
	let detected_version = detect_game_version(game_root);
	if parsed_files == 0 {
		return GameBaseSemanticSnapshot {
			detected_version,
			index: SemanticIndex::default(),
			parsed_files: 0,
		};
	}

	let manifest_hash = semantic_manifest_hash(&documents);
	let cache_path = base_semantic_cache_file(game_key, detected_version.as_deref(), manifest_hash);
	if let Some(entry) = load_base_semantic_cache(&cache_path)
		&& entry.schema_version == BASE_SNAPSHOT_SCHEMA_VERSION
		&& entry.game_key == game_key
		&& entry.detected_version == detected_version
		&& entry.manifest_hash == manifest_hash
		&& entry.parsed_files == parsed_files
	{
		return GameBaseSemanticSnapshot {
			detected_version,
			index: entry.index,
			parsed_files,
		};
	}

	if let Some(snapshot) = load_builtin_base_snapshot(game_key, detected_version.as_deref()) {
		let cache_entry = BaseSemanticSnapshotEntry {
			schema_version: BASE_SNAPSHOT_SCHEMA_VERSION,
			game_key: game_key.to_string(),
			detected_version: snapshot.detected_version.clone(),
			manifest_hash,
			parsed_files: snapshot.parsed_files,
			index: snapshot.index.clone(),
		};
		store_base_semantic_cache(&cache_path, &cache_entry);
		return snapshot;
	}

	let mod_id = base_game_mod_id(game_key);
	let parsed = parse_text_documents(&mod_id, game_root);
	let index = build_semantic_index_from_documents(&parsed);
	let snapshot = GameBaseSemanticSnapshot {
		detected_version: detected_version.clone(),
		index,
		parsed_files: parsed.len(),
	};
	let cache_entry = BaseSemanticSnapshotEntry {
		schema_version: BASE_SNAPSHOT_SCHEMA_VERSION,
		game_key: game_key.to_string(),
		detected_version,
		manifest_hash,
		parsed_files: snapshot.parsed_files,
		index: snapshot.index.clone(),
	};
	store_base_semantic_cache(&cache_path, &cache_entry);
	snapshot
}

fn merge_semantic_indexes(mut base: SemanticIndex, mut overlay: SemanticIndex) -> SemanticIndex {
	let offset = base.scopes.len();
	for scope in &mut overlay.scopes {
		scope.id += offset;
		if let Some(parent) = scope.parent {
			scope.parent = Some(parent + offset);
		}
	}
	for definition in &mut overlay.definitions {
		definition.scope_id += offset;
	}
	for reference in &mut overlay.references {
		reference.scope_id += offset;
	}
	for alias in &mut overlay.alias_usages {
		alias.scope_id += offset;
	}
	for usage in &mut overlay.key_usages {
		usage.scope_id += offset;
	}
	for assignment in &mut overlay.scalar_assignments {
		assignment.scope_id += offset;
	}

	base.scopes.extend(overlay.scopes);
	base.definitions.extend(overlay.definitions);
	base.references.extend(overlay.references);
	base.alias_usages.extend(overlay.alias_usages);
	base.key_usages.extend(overlay.key_usages);
	base.scalar_assignments.extend(overlay.scalar_assignments);
	base.documents.extend(overlay.documents);
	base.localisation_definitions
		.extend(overlay.localisation_definitions);
	base.localisation_duplicates
		.extend(overlay.localisation_duplicates);
	base.ui_definitions.extend(overlay.ui_definitions);
	base.resource_references.extend(overlay.resource_references);
	base.csv_rows.extend(overlay.csv_rows);
	base.json_properties.extend(overlay.json_properties);
	base.parse_issues.extend(overlay.parse_issues);
	base
}

fn semantic_manifest_hash(files: &[crate::check::documents::DiscoveredTextDocument]) -> u64 {
	let mut entries: Vec<String> = Vec::new();
	for file in files {
		let relative = file.relative_path.to_string_lossy().replace('\\', "/");
		let mut entry = relative;
		if let Ok(metadata) = fs::metadata(&file.absolute_path) {
			entry.push('|');
			entry.push_str(&metadata.len().to_string());
			entry.push('|');
			entry.push_str(&modified_nanos(&metadata).to_string());
		}
		entries.push(entry);
	}
	entries.sort();

	let mut hasher = DefaultHasher::new();
	for entry in entries {
		entry.hash(&mut hasher);
	}
	hasher.finish()
}

fn base_semantic_cache_root() -> std::path::PathBuf {
	if let Ok(override_dir) = std::env::var("FOCH_BASE_SNAPSHOT_DIR") {
		return std::path::PathBuf::from(override_dir);
	}
	if let Ok(override_dir) = std::env::var("FOCH_SEMANTIC_CACHE_DIR") {
		return std::path::PathBuf::from(override_dir);
	}
	dirs::data_local_dir()
		.unwrap_or_else(std::env::temp_dir)
		.join("foch")
		.join("base_snapshots")
}

fn base_semantic_cache_file(
	game_key: &str,
	detected_version: Option<&str>,
	manifest_hash: u64,
) -> std::path::PathBuf {
	let version_key = sanitize_cache_component(detected_version.unwrap_or("unknown"));
	base_semantic_cache_root()
		.join(game_key)
		.join(version_key)
		.join(format!("{manifest_hash:016x}.json"))
}

fn load_base_semantic_cache(path: &std::path::Path) -> Option<BaseSemanticSnapshotEntry> {
	let raw = fs::read_to_string(path).ok()?;
	serde_json::from_str::<BaseSemanticSnapshotEntry>(&raw).ok()
}

fn store_base_semantic_cache(path: &std::path::Path, entry: &BaseSemanticSnapshotEntry) {
	let Some(parent) = path.parent() else {
		return;
	};
	if fs::create_dir_all(parent).is_err() {
		return;
	}
	let Ok(raw) = serde_json::to_string(entry) else {
		return;
	};
	let tmp = path.with_extension("json.tmp");
	if fs::write(&tmp, raw).is_err() {
		return;
	}
	let _ = fs::rename(tmp, path);
}

fn load_builtin_base_snapshot(
	game_key: &str,
	detected_version: Option<&str>,
) -> Option<GameBaseSemanticSnapshot> {
	let detected_version = detected_version?;
	let raw = match (game_key, detected_version) {
		("eu4", "builtin-test-1.0.0") => {
			include_str!("data/eu4_base_snapshot_builtin_test_1_0_0.json")
		}
		_ => return None,
	};
	let entry: BuiltinBaseSnapshotEntry = serde_json::from_str(raw).ok()?;
	Some(GameBaseSemanticSnapshot {
		detected_version: Some(entry.detected_version),
		index: entry.index,
		parsed_files: entry.parsed_files,
	})
}

fn detect_game_version(game_root: &Path) -> Option<String> {
	for candidate in [
		game_root.join("launcher-settings.json"),
		game_root.join("launcher").join("launcher-settings.json"),
		game_root.join("version.txt"),
	] {
		if !candidate.is_file() {
			continue;
		}
		if candidate.file_name().and_then(|value| value.to_str()) == Some("version.txt") {
			let version = fs::read_to_string(&candidate).ok()?;
			let version = version.lines().next()?.trim();
			if !version.is_empty() {
				return Some(version.to_string());
			}
			continue;
		}
		let raw = fs::read_to_string(&candidate).ok()?;
		let json = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
		for key in ["rawVersion", "version", "gameVersion"] {
			if let Some(value) = json.get(key).and_then(|value| value.as_str())
				&& !value.trim().is_empty()
			{
				return Some(value.trim().to_string());
			}
		}
	}
	None
}

fn sanitize_cache_component(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for ch in value.chars() {
		if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
			out.push(ch);
		} else {
			out.push('_');
		}
	}
	if out.is_empty() {
		"unknown".to_string()
	} else {
		out
	}
}

fn modified_nanos(metadata: &fs::Metadata) -> u128 {
	metadata
		.modified()
		.ok()
		.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
		.map_or(0, |duration| duration.as_nanos())
}

pub(crate) fn base_game_mod_id(game_key: &str) -> String {
	format!("{BASE_GAME_MOD_ID_PREFIX}{game_key}")
}

fn resolve_game_root(request: &CheckRequest, playlist: &Playlist) -> Option<PathBuf> {
	let mut candidates = Vec::new();
	if let Some(path) = request.config.game_path.get(playlist.game.key()) {
		candidates.push(path.clone());
	}
	if let Some(steam_root) = request.config.steam_root_path.as_ref() {
		for app_id in playlist.game.steam_app_ids() {
			if let Some(path) = steam_game_install_path(steam_root, *app_id) {
				candidates.push(path);
			}
		}
	}
	dedup_candidates(candidates)
		.into_iter()
		.find(|candidate| candidate.is_dir())
}

fn build_mod_candidates(request: &CheckRequest, playlist: &Playlist) -> Vec<ModCandidate> {
	let playset_dir = request
		.playset_path
		.parent()
		.map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);

	let mut entries = playlist.mods.clone();
	entries.sort_by_key(|entry| entry.position.unwrap_or(usize::MAX));

	entries
		.into_iter()
		.map(|entry| {
			let mod_id = entry
				.steam_id
				.clone()
				.filter(|x| !x.trim().is_empty())
				.unwrap_or_else(|| "<missing-steam-id>".to_string());

			let root_path = resolve_mod_root(&playset_dir, request, playlist, &entry);
			let descriptor_path = root_path.as_ref().map(|path| path.join("descriptor.mod"));

			let (descriptor, descriptor_error) = match descriptor_path.as_ref() {
				Some(path) if path.exists() => match load_descriptor(path) {
					Ok(descriptor) => (Some(descriptor), None),
					Err(err) => (None, Some(err.to_string())),
				},
				Some(path) => (None, Some(format!("{} 不存在", path.display()))),
				None => (None, None),
			};

			let files = root_path
				.as_ref()
				.map_or_else(Vec::new, |root| collect_relative_files(root));

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

fn resolve_mod_root(
	playset_dir: &Path,
	request: &CheckRequest,
	playlist: &Playlist,
	entry: &PlaylistEntry,
) -> Option<PathBuf> {
	let mut candidates = Vec::new();

	if let Some(steam_id) = entry.steam_id.as_ref() {
		candidates.push(playset_dir.join(steam_id));
		candidates.push(playset_dir.join(format!("mod_{steam_id}")));

		if let Some(path) = request.config.paradox_data_path.as_ref() {
			for game_data_dir in paradox_game_data_dirs(path, &playlist.game) {
				if let Some(root) = resolve_mod_from_ugc_descriptor(&game_data_dir, steam_id) {
					candidates.push(root);
				}
				candidates.push(game_data_dir.join("mod").join(steam_id));
				candidates.push(game_data_dir.join("mod").join(format!("ugc_{steam_id}")));
			}
		}

		if let Some(steam_root) = request.config.steam_root_path.as_ref() {
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
		candidates.push(playset_dir.join(name));
		candidates.push(playset_dir.join(name.replace(' ', "_")));
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
		let path = std::path::PathBuf::from(&fragment);
		if path.is_absolute() {
			candidates.push(path.clone());
		}
		candidates.push(game_data_dir.join(&path));
		candidates.push(game_data_dir.join("mod").join(&path));
	}

	dedup_candidates(candidates)
}

fn collect_relative_files(root: &Path) -> Vec<PathBuf> {
	let mut files = Vec::new();

	for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
		if !entry.file_type().is_file() {
			continue;
		}

		let path = entry.path();
		if path.file_name().and_then(|name| name.to_str()) == Some("descriptor.mod") {
			continue;
		}

		if let Ok(relative) = path.strip_prefix(root) {
			files.push(relative.to_path_buf());
		}
	}

	files
}

fn build_file_inventory(
	playlist: &Playlist,
	mods: &[ModCandidate],
	base_game_root: Option<&PathBuf>,
) -> BTreeMap<String, Vec<ResolvedFileContributor>> {
	let mut inventory = BTreeMap::new();
	let mut precedence = 0;

	if let Some(root) = base_game_root {
		let mod_id = base_game_mod_id(playlist.game.key());
		for relative in collect_relative_files(root) {
			let key = normalize_relative_path(&relative);
			inventory
				.entry(key)
				.or_insert_with(Vec::new)
				.push(ResolvedFileContributor {
					mod_id: mod_id.clone(),
					root_path: root.clone(),
					absolute_path: root.join(&relative),
					precedence,
					is_base_game: true,
				});
		}
		precedence += 1;
	}

	for mod_item in mods {
		let Some(root) = mod_item.root_path.as_ref() else {
			continue;
		};
		for relative in &mod_item.files {
			let key = normalize_relative_path(relative);
			inventory
				.entry(key)
				.or_insert_with(Vec::new)
				.push(ResolvedFileContributor {
					mod_id: mod_item.mod_id.clone(),
					root_path: root.clone(),
					absolute_path: root.join(relative),
					precedence,
					is_base_game: false,
				});
		}
		precedence += 1;
	}

	inventory
}

fn normalize_relative_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}
