use crate::check::analysis::{AnalyzeOptions, analyze_visibility};
use crate::check::graph::export_graph;
use crate::check::model::{
	AnalysisMeta, AnalysisMode, CheckContext, CheckRequest, CheckResult, Finding, FindingChannel,
	ModCandidate, RunOptions, Severity,
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
use crate::utils::steam::steam_workshop_mod_path;
use std::collections::HashSet;
use walkdir::WalkDir;

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
	let parsed_files = collect_parsed_script_files(&mods);
	let semantic_index = build_semantic_index(&parsed_files);
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
		parsed_files: parsed_files.len(),
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

fn collect_parsed_script_files(
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
