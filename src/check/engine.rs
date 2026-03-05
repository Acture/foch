use crate::check::analysis::{AnalyzeOptions, analyze_visibility};
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
use crate::check::semantic_index::{build_semantic_index, parse_script_file};
use crate::domain::descriptor::load_descriptor;
use crate::domain::game::Game;
use crate::domain::playlist::{Playlist, PlaylistEntry, load_playlist};
use crate::utils::steam::{steam_game_install_path, steam_workshop_mod_path};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const BASE_GAME_MOD_ID_PREFIX: &str = "__game__";
const BASE_SEMANTIC_CACHE_VERSION: u32 = 2;

pub fn run_checks(request: CheckRequest) -> CheckResult {
	run_checks_with_options(request, RunOptions::default())
}

pub fn run_checks_with_options(request: CheckRequest, options: RunOptions) -> CheckResult {
	let mut result = CheckResult::default();

	let playlist = match load_playlist(&request.playset_path) {
		Ok(playlist) => playlist,
		Err(err) => {
			if matches!(err.kind, crate::domain::ParseErrorKind::Format) {
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
			result.push_fatal_error(format!("无法读取 Playset: {err}"));
			return result;
		}
	};

	let mods = build_mod_candidates(&request, &playlist);
	let base_semantic = if options.include_game_base {
		load_or_build_game_base_semantic_index(&request, &playlist)
	} else {
		None
	};
	let mod_parsed_files = collect_mod_parsed_script_files(&mods);
	let mod_semantic_index = build_semantic_index(&mod_parsed_files);
	let parsed_files_count = mod_parsed_files.len()
		+ base_semantic
			.as_ref()
			.map_or(0, |snapshot| snapshot.parsed_files);
	let semantic_index = match base_semantic {
		Some(snapshot) => merge_semantic_indexes(snapshot.index, mod_semantic_index),
		None => mod_semantic_index,
	};
	let ctx = CheckContext {
		playlist_path: request.playset_path.clone(),
		playlist,
		mods,
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

fn collect_mod_parsed_script_files(
	mods: &[ModCandidate],
) -> Vec<crate::check::semantic_index::ParsedScriptFile> {
	let mut parsed = Vec::new();

	for mod_item in mods {
		let Some(root) = mod_item.root_path.as_ref() else {
			continue;
		};
		for script_file in iter_script_files(root) {
			if let Some(file) = parse_script_file(&mod_item.mod_id, root, &script_file) {
				parsed.push(file);
			}
		}
	}
	parsed
}

#[derive(Clone, Debug)]
struct GameBaseSemanticSnapshot {
	index: SemanticIndex,
	parsed_files: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BaseSemanticCacheEntry {
	version: u32,
	manifest_hash: u64,
	parsed_files: usize,
	index: SemanticIndex,
}

fn load_or_build_game_base_semantic_index(
	request: &CheckRequest,
	playlist: &Playlist,
) -> Option<GameBaseSemanticSnapshot> {
	let game_root = resolve_game_root(request, playlist)?;
	let script_files = iter_script_files(&game_root);
	let parsed_files = script_files.len();
	if parsed_files == 0 {
		return Some(GameBaseSemanticSnapshot {
			index: SemanticIndex::default(),
			parsed_files: 0,
		});
	}

	let manifest_hash = semantic_manifest_hash(&game_root, &script_files);
	let cache_path = base_semantic_cache_file(playlist.game.key(), &game_root);
	if let Some(entry) = load_base_semantic_cache(&cache_path)
		&& entry.version == BASE_SEMANTIC_CACHE_VERSION
		&& entry.manifest_hash == manifest_hash
		&& entry.parsed_files == parsed_files
	{
		return Some(GameBaseSemanticSnapshot {
			index: entry.index,
			parsed_files,
		});
	}

	let mod_id = format!("{BASE_GAME_MOD_ID_PREFIX}{}", playlist.game.key());
	let mut parsed = Vec::with_capacity(parsed_files);
	for script_file in &script_files {
		if let Some(file) = parse_script_file(&mod_id, &game_root, script_file) {
			parsed.push(file);
		}
	}
	let index = build_semantic_index(&parsed);
	let snapshot = GameBaseSemanticSnapshot {
		index,
		parsed_files: parsed.len(),
	};
	let cache_entry = BaseSemanticCacheEntry {
		version: BASE_SEMANTIC_CACHE_VERSION,
		manifest_hash,
		parsed_files: snapshot.parsed_files,
		index: snapshot.index.clone(),
	};
	store_base_semantic_cache(&cache_path, &cache_entry);
	Some(snapshot)
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

	base.scopes.extend(overlay.scopes);
	base.definitions.extend(overlay.definitions);
	base.references.extend(overlay.references);
	base.alias_usages.extend(overlay.alias_usages);
	base.key_usages.extend(overlay.key_usages);
	base.parse_issues.extend(overlay.parse_issues);
	base
}

fn semantic_manifest_hash(root: &std::path::Path, files: &[std::path::PathBuf]) -> u64 {
	let mut entries: Vec<String> = Vec::new();
	for file in files {
		let relative = file
			.strip_prefix(root)
			.unwrap_or(file)
			.to_string_lossy()
			.replace('\\', "/");
		let mut entry = relative;
		if let Ok(metadata) = fs::metadata(file) {
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
	if let Ok(override_dir) = std::env::var("FOCH_SEMANTIC_CACHE_DIR") {
		return std::path::PathBuf::from(override_dir);
	}
	dirs::cache_dir()
		.unwrap_or_else(std::env::temp_dir)
		.join("foch")
		.join("semantic_base")
}

fn base_semantic_cache_file(game_key: &str, game_root: &std::path::Path) -> std::path::PathBuf {
	let mut hasher = DefaultHasher::new();
	game_key.hash(&mut hasher);
	game_root
		.to_string_lossy()
		.replace('\\', "/")
		.hash(&mut hasher);
	let key = format!("{:016x}", hasher.finish());
	base_semantic_cache_root().join(format!("{key}.json"))
}

fn load_base_semantic_cache(path: &std::path::Path) -> Option<BaseSemanticCacheEntry> {
	let raw = fs::read_to_string(path).ok()?;
	serde_json::from_str::<BaseSemanticCacheEntry>(&raw).ok()
}

fn store_base_semantic_cache(path: &std::path::Path, entry: &BaseSemanticCacheEntry) {
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

fn modified_nanos(metadata: &fs::Metadata) -> u128 {
	metadata
		.modified()
		.ok()
		.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
		.map_or(0, |duration| duration.as_nanos())
}

fn resolve_game_root(request: &CheckRequest, playlist: &Playlist) -> Option<std::path::PathBuf> {
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
	playset_dir: &std::path::Path,
	request: &CheckRequest,
	playlist: &Playlist,
	entry: &PlaylistEntry,
) -> Option<std::path::PathBuf> {
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

fn dedup_candidates(candidates: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
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

fn paradox_game_data_dirs(base: &std::path::Path, game: &Game) -> Vec<std::path::PathBuf> {
	let mut dirs = vec![base.to_path_buf()];
	if let Some(game_dir_name) = game.paradox_data_dir_name() {
		dirs.push(base.join(game_dir_name));
	}
	dedup_candidates(dirs)
}

fn resolve_mod_from_ugc_descriptor(
	game_data_dir: &std::path::Path,
	steam_id: &str,
) -> Option<std::path::PathBuf> {
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

fn descriptor_path_candidates(
	game_data_dir: &std::path::Path,
	raw: &str,
) -> Vec<std::path::PathBuf> {
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

fn collect_relative_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
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

fn iter_script_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
	let mut files = Vec::new();
	for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
		if !entry.file_type().is_file() {
			continue;
		}
		let path = entry.path();
		let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
			continue;
		};
		if !matches!(ext, "txt" | "lua") {
			continue;
		}
		if !is_semantic_target_path(root, path) {
			continue;
		}
		files.push(path.to_path_buf());
	}
	files.sort();
	files
}

fn is_semantic_target_path(root: &std::path::Path, path: &std::path::Path) -> bool {
	let Ok(relative) = path.strip_prefix(root) else {
		return false;
	};
	let normalized = relative.to_string_lossy().replace('\\', "/");
	normalized.starts_with("events/")
		|| normalized.starts_with("decisions/")
		|| normalized.starts_with("common/scripted_effects/")
		|| normalized.starts_with("common/diplomatic_actions/")
		|| normalized.starts_with("common/triggered_modifiers/")
		|| normalized.starts_with("common/defines/")
}
