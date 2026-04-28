use super::error::MergeError;
use super::normalize::normalize_defines_file;
use crate::request::{CheckRequest, MergePlanOptions};
use crate::workspace::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveErrorKind, resolve_workspace,
};
use foch_core::model::{
	MergePlanContributor, MergePlanEntry, MergePlanResult, MergePlanStrategies, MergePlanStrategy,
};
use foch_language::analyzer::content_family::GameProfile;
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::semantic_index::parse_script_file;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run_merge_plan(request: CheckRequest) -> MergePlanResult {
	run_merge_plan_with_options(request, MergePlanOptions::default())
}

pub fn run_merge_plan_with_options(
	request: CheckRequest,
	options: MergePlanOptions,
) -> MergePlanResult {
	let workspace = match resolve_workspace(&request, options.include_game_base) {
		Ok(workspace) => workspace,
		Err(err) => {
			let mut result = MergePlanResult {
				generated_at: current_generated_at(),
				include_game_base: options.include_game_base,
				..MergePlanResult::default()
			};
			if err.kind == WorkspaceResolveErrorKind::PlaylistFormat {
				result.push_fatal_error("无法解析 Playset JSON");
			} else {
				result.push_fatal_error(err.message);
			}
			return result;
		}
	};

	build_merge_plan_from_workspace(&workspace, options.include_game_base)
}

pub(crate) fn build_merge_plan_from_workspace(
	workspace: &ResolvedWorkspace,
	include_game_base: bool,
) -> MergePlanResult {
	let mut result = MergePlanResult {
		generated_at: current_generated_at(),
		include_game_base,
		..MergePlanResult::default()
	};

	result.game = workspace.playlist.game.key().to_string();
	result.playset_name = workspace.playlist.name.clone();

	let profile = eu4_profile();
	result.paths = workspace
		.file_inventory
		.iter()
		.map(|(path, contributors)| classify_entry(path, contributors, profile))
		.collect();
	result.strategies = summarize_paths(&result.paths);
	result
}

fn classify_entry(
	path: &str,
	contributors: &[ResolvedFileContributor],
	profile: &dyn GameProfile,
) -> MergePlanEntry {
	let contributors_out: Vec<MergePlanContributor> =
		contributors.iter().map(to_merge_contributor).collect();
	let mut winner = contributors_out.last().cloned();
	let mut notes = Vec::new();

	let strategy = if contributors.len() == 1 {
		MergePlanStrategy::CopyThrough
	} else if is_structural_merge_path(path, profile) {
		match validate_structural_merge_inputs(path, contributors) {
			Ok(()) => MergePlanStrategy::StructuralMerge,
			Err(err) => {
				notes.push(err.to_string());
				MergePlanStrategy::ManualConflict
			}
		}
	} else if is_localisation_yml_path(path) {
		MergePlanStrategy::LocalisationMerge
	} else {
		// Text-like or binary content with no structural-merge handler:
		// last-writer-overlay matches what the game's load order would do
		// at runtime (later-precedence mod replaces earlier ones).
		if !is_text_like_overlay_path(path) {
			notes.push("binary overlap resolved by last-writer-overlay".to_string());
		}
		MergePlanStrategy::LastWriterOverlay
	};

	if strategy == MergePlanStrategy::ManualConflict {
		winner = None;
	}

	MergePlanEntry {
		path: path.to_string(),
		strategy,
		contributors: contributors_out,
		winner,
		generated: false,
		notes,
	}
}

fn to_merge_contributor(contributor: &ResolvedFileContributor) -> MergePlanContributor {
	MergePlanContributor {
		mod_id: contributor.mod_id.clone(),
		source_path: contributor
			.absolute_path
			.to_string_lossy()
			.replace('\\', "/"),
		precedence: contributor.precedence,
		is_base_game: contributor.is_base_game,
	}
}

fn summarize_paths(paths: &[MergePlanEntry]) -> MergePlanStrategies {
	let mut strategies = MergePlanStrategies {
		total_paths: paths.len(),
		..MergePlanStrategies::default()
	};

	for path in paths {
		match path.strategy {
			MergePlanStrategy::CopyThrough => strategies.copy_through += 1,
			MergePlanStrategy::LastWriterOverlay => strategies.last_writer_overlay += 1,
			MergePlanStrategy::StructuralMerge => strategies.structural_merge += 1,
			MergePlanStrategy::LocalisationMerge => strategies.localisation_merge += 1,
			MergePlanStrategy::ManualConflict => strategies.manual_conflict += 1,
		}
	}

	strategies
}

fn current_generated_at() -> String {
	let millis = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |duration| duration.as_millis());
	millis.to_string()
}

fn validate_structural_merge_inputs(
	path: &str,
	contributors: &[ResolvedFileContributor],
) -> Result<(), MergeError> {
	let mut failures = Vec::new();
	let is_defines_path = path.to_ascii_lowercase().starts_with("common/defines/");

	for contributor in contributors {
		if let Some(parse_ok) = contributor.parse_ok_hint {
			if parse_ok {
				if !is_defines_path {
					continue;
				}
			} else if contributor.is_base_game {
				failures.push(format!("base game parse issues in {}", contributor.mod_id));
				continue;
			} else {
				failures.push(format!("cached parse issues in {}", contributor.mod_id));
				continue;
			}
		}

		match parse_script_file(
			&contributor.mod_id,
			&contributor.root_path,
			&contributor.absolute_path,
		) {
			Some(parsed) if parsed.parse_issues.is_empty() => {
				if is_defines_path && let Err(err) = normalize_defines_file(&parsed) {
					failures.push(format!(
						"non-normalizable defines in {}: {}",
						contributor.mod_id, err
					));
				}
			}
			Some(parsed) => failures.push(format!(
				"{} parse issues in {}",
				parsed.parse_issues.len(),
				contributor.mod_id
			)),
			None => failures.push(format!("unable to parse {}", contributor.mod_id)),
		}
	}

	if failures.is_empty() {
		Ok(())
	} else {
		Err(MergeError::Validation {
			path: Some(path.to_string()),
			message: format!(
				"structural merge blocked by invalid contributors: {}",
				failures.join(", ")
			),
		})
	}
}

fn is_structural_merge_path(path: &str, profile: &dyn GameProfile) -> bool {
	if !is_text_like_overlay_path(path) {
		return false;
	}
	profile
		.classify_content_family(Path::new(path))
		.and_then(|d| d.merge_key_source)
		.is_some()
}

fn is_text_like_overlay_path(path: &str) -> bool {
	let normalized = path.to_ascii_lowercase();
	let Some(ext) = normalized.rsplit('.').next() else {
		return false;
	};

	matches!(
		ext,
		"txt" | "lua" | "yml" | "yaml" | "csv" | "json" | "asset" | "gui" | "gfx" | "mod"
	)
}

/// Localisation YAML files (`localisation/**.yml` and
/// `common/localisation/**.yml`) follow the EU4 paradox-yaml format and can be
/// merged at the key level: the union of all contributors' keys is preserved,
/// with the highest-precedence contributor winning on collision.
pub(crate) fn is_localisation_yml_path(path: &str) -> bool {
	let normalized = path.to_ascii_lowercase();
	let under_loc =
		normalized.starts_with("localisation/") || normalized.starts_with("common/localisation/");
	if !under_loc {
		return false;
	}
	let Some(ext) = normalized.rsplit('.').next() else {
		return false;
	};
	matches!(ext, "yml" | "yaml")
}
