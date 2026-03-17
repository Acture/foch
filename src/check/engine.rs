use crate::check::analysis::{AnalyzeOptions, analyze_visibility};
use crate::check::base_data::{
	InstalledBaseSnapshot, base_game_mod_id, detect_game_version, load_installed_base_snapshot,
	resolve_game_root, resolve_game_root_and_version,
};
use crate::check::graph::export_graph;
use crate::check::mod_cache::{LoadedModSnapshot, load_or_build_mod_snapshot};
use crate::check::model::{
	AnalysisMeta, AnalysisMode, CheckContext, CheckRequest, CheckResult, FamilyParseStats, Finding,
	FindingChannel, ModCandidate, ParseFamilyStats, ParseIssueReportItem, RunOptions,
	SemanticIndex, Severity,
};
use crate::check::rules::{
	check_duplicate_mod_identity, check_duplicate_scripted_effect, check_file_conflict,
	check_missing_dependency, check_missing_descriptor, check_required_fields,
};
use crate::domain::descriptor::load_descriptor;
use crate::domain::game::Game;
use crate::domain::playlist::{Playlist, PlaylistEntry, load_playlist};
use crate::utils::steam::steam_workshop_mod_path;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

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
	pub parse_ok_hint: Option<bool>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedWorkspace {
	pub playlist_path: PathBuf,
	pub playlist: Playlist,
	pub mods: Vec<ModCandidate>,
	pub installed_base_snapshot: Option<InstalledBaseSnapshot>,
	pub mod_snapshots: Vec<Option<LoadedModSnapshot>>,
	pub file_inventory: BTreeMap<String, Vec<ResolvedFileContributor>>,
}

#[derive(Clone, Debug)]
struct GameBaseSemanticSnapshot {
	index: SemanticIndex,
	parsed_files: usize,
	parse_error_count: usize,
	parse_stats: ParseFamilyStats,
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

	let base_semantic =
		resolved
			.installed_base_snapshot
			.as_ref()
			.map(|installed| GameBaseSemanticSnapshot {
				index: installed.snapshot.to_semantic_index(),
				parsed_files: installed.snapshot.parsed_files,
				parse_error_count: installed.snapshot.parse_error_count,
				parse_stats: installed.snapshot.parse_stats.clone(),
			});
	let mod_parsed_files_count: usize = resolved
		.mod_snapshots
		.iter()
		.flatten()
		.map(|snapshot| snapshot.parsed_files)
		.sum();
	let mod_parse_error_count: usize = resolved
		.mod_snapshots
		.iter()
		.flatten()
		.map(|snapshot| snapshot.parse_error_count)
		.sum();
	let mod_parse_stats = resolved
		.mod_snapshots
		.iter()
		.flatten()
		.fold(ParseFamilyStats::default(), |acc, snapshot| {
			sum_parse_family_stats(acc, snapshot.parse_stats.clone())
		});
	let mod_semantic_index = merge_mod_snapshots(&resolved.mod_snapshots);
	let parsed_files_count = mod_parsed_files_count
		+ base_semantic
			.as_ref()
			.map_or(0, |snapshot| snapshot.parsed_files);
	let base_parse_error_count = base_semantic
		.as_ref()
		.map_or(0, |snapshot| snapshot.parse_error_count);
	let total_parse_stats = base_semantic
		.as_ref()
		.map_or(mod_parse_stats.clone(), |snapshot| {
			sum_parse_family_stats(snapshot.parse_stats.clone(), mod_parse_stats.clone())
		});
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
		parse_errors: mod_parse_error_count + base_parse_error_count,
		parse_stats: total_parse_stats,
		scopes: ctx.semantic_index.scopes.len(),
		symbol_definitions: ctx.semantic_index.definitions.len(),
		symbol_references: ctx.semantic_index.references.len(),
		alias_usages: ctx.semantic_index.alias_usages.len(),
	};
	result.parse_issue_report = build_parse_issue_report(&ctx.semantic_index);

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
	let optional_game_root = resolve_game_root(&request.config, &playlist.game);
	let (base_game_root, installed_base_snapshot, mod_cache_game_version) = if include_game_base {
		let (game_root, game_version) =
			resolve_game_root_and_version(&request.config, &playlist.game).map_err(|message| {
				WorkspaceResolveError {
					kind: WorkspaceResolveErrorKind::Io,
					path: request.playset_path.clone(),
					message,
				}
			})?;
		let installed = load_installed_base_snapshot(playlist.game.key(), &game_version)
			.map_err(|message| WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: request.playset_path.clone(),
				message,
			})?
			.ok_or_else(|| WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: request.playset_path.clone(),
				message: missing_base_data_message(&playlist.game, &game_version, &game_root),
			})?;
		(Some(game_root), Some(installed), Some(game_version))
	} else {
		(
			None,
			None,
			optional_game_root
				.as_ref()
				.and_then(|game_root| detect_game_version(game_root)),
		)
	};
	let mod_snapshots: Vec<Option<LoadedModSnapshot>> = mods
		.iter()
		.map(|mod_item| {
			load_or_build_mod_snapshot(
				playlist.game.key(),
				mod_cache_game_version.as_deref(),
				mod_item,
			)
		})
		.collect();
	let file_inventory = build_file_inventory(
		&playlist,
		&mods,
		&mod_snapshots,
		base_game_root.as_ref(),
		installed_base_snapshot.as_ref(),
	);

	Ok(ResolvedWorkspace {
		playlist_path: request.playset_path.clone(),
		playlist,
		mods,
		installed_base_snapshot,
		mod_snapshots,
		file_inventory,
	})
}

fn merge_mod_snapshots(snapshots: &[Option<LoadedModSnapshot>]) -> SemanticIndex {
	let mut merged = SemanticIndex::default();
	for snapshot in snapshots.iter().flatten() {
		merged = merge_semantic_indexes(merged, snapshot.semantic_index.clone());
	}
	merged
}

fn sum_parse_family_stats(lhs: ParseFamilyStats, rhs: ParseFamilyStats) -> ParseFamilyStats {
	ParseFamilyStats {
		clausewitz_mainline: sum_family_parse_stats(
			lhs.clausewitz_mainline,
			rhs.clausewitz_mainline,
		),
		localisation: sum_family_parse_stats(lhs.localisation, rhs.localisation),
		csv: sum_family_parse_stats(lhs.csv, rhs.csv),
		json: sum_family_parse_stats(lhs.json, rhs.json),
	}
}

fn sum_family_parse_stats(lhs: FamilyParseStats, rhs: FamilyParseStats) -> FamilyParseStats {
	FamilyParseStats {
		documents: lhs.documents + rhs.documents,
		parse_failed_documents: lhs.parse_failed_documents + rhs.parse_failed_documents,
		parse_issue_count: lhs.parse_issue_count + rhs.parse_issue_count,
	}
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

fn missing_base_data_message(game: &Game, game_version: &str, game_root: &Path) -> String {
	format!(
		"缺少 {} {} 的已安装基础数据；请运行 `foch data install {} --game-version auto` 或 `foch data build {} --from-game-path {} --game-version auto --install`，或使用 --no-game-base",
		game.key(),
		game_version,
		game.key(),
		game.key(),
		game_root.display()
	)
}

fn build_mod_candidates(request: &CheckRequest, playlist: &Playlist) -> Vec<ModCandidate> {
	let playset_dir = request
		.playset_path
		.parent()
		.map_or_else(|| PathBuf::from("."), PathBuf::from);

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
		let path = PathBuf::from(&fragment);
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

	files.sort();
	files
}

fn build_file_inventory(
	playlist: &Playlist,
	mods: &[ModCandidate],
	mod_snapshots: &[Option<LoadedModSnapshot>],
	base_game_root: Option<&PathBuf>,
	installed_base_snapshot: Option<&InstalledBaseSnapshot>,
) -> BTreeMap<String, Vec<ResolvedFileContributor>> {
	let mut inventory = BTreeMap::new();
	let mut precedence = 0;

	if let (Some(root), Some(snapshot)) = (base_game_root, installed_base_snapshot) {
		let mod_id = base_game_mod_id(playlist.game.key());
		let document_lookup = snapshot.snapshot.document_lookup();
		for relative in &snapshot.snapshot.inventory_paths {
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
					parse_ok_hint: document.map(|(_family, parse_ok)| *parse_ok),
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
					parse_ok_hint,
				});
		}
		precedence += 1;
	}

	inventory
}

fn normalize_relative_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

fn build_parse_issue_report(index: &SemanticIndex) -> Vec<ParseIssueReportItem> {
	let family_lookup = index
		.documents
		.iter()
		.map(|item| {
			(
				(item.mod_id.clone(), normalize_relative_path(&item.path)),
				item.family,
			)
		})
		.collect::<std::collections::HashMap<_, _>>();
	let mut items: Vec<ParseIssueReportItem> = index
		.parse_issues
		.iter()
		.map(|issue| ParseIssueReportItem {
			family: family_lookup
				.get(&(issue.mod_id.clone(), normalize_relative_path(&issue.path)))
				.copied()
				.unwrap_or(crate::check::model::DocumentFamily::Clausewitz),
			mod_id: issue.mod_id.clone(),
			path: issue.path.clone(),
			line: issue.line,
			column: issue.column,
			message: issue.message.clone(),
		})
		.collect();
	items.sort_by(|lhs, rhs| {
		(
			format!("{:?}", lhs.family),
			lhs.mod_id.as_str(),
			lhs.path.as_os_str(),
			lhs.line,
			lhs.column,
			lhs.message.as_str(),
		)
			.cmp(&(
				format!("{:?}", rhs.family),
				rhs.mod_id.as_str(),
				rhs.path.as_os_str(),
				rhs.line,
				rhs.column,
				rhs.message.as_str(),
			))
	});
	items
}
