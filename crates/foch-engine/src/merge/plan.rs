use super::error::MergeError;
use super::normalize::normalize_defines_file;
use crate::request::{CheckRequest, MergePlanOptions};
use crate::workspace::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveError, WorkspaceResolveErrorKind,
	WorkspaceScriptCache, resolve_workspace,
};
use foch_core::model::{
	MergePlanContributor, MergePlanEntry, MergePlanResult, MergePlanStrategies, MergePlanStrategy,
	MergePlanTarget, MergeUnitId,
};
use foch_language::analyzer::content_family::GameProfile;
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, ContentLoadPolicy, DefinitionModuleOutput, DefinitionModulePolicy,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run_merge_plan(request: CheckRequest) -> MergePlanResult {
	run_merge_plan_with_options(request, MergePlanOptions::default())
}

pub fn run_merge_plan_with_options(
	request: CheckRequest,
	options: MergePlanOptions,
) -> MergePlanResult {
	let mut workspace = match resolve_workspace(&request, options.include_game_base) {
		Ok(workspace) => workspace,
		Err(err) => return fatal_plan_from_workspace_error(&err, options.include_game_base),
	};
	prune_noop_script_contributors(&mut workspace, eu4_profile());

	build_merge_plan_from_workspace(&workspace, options.include_game_base)
}

pub(crate) fn fatal_plan_from_workspace_error(
	err: &WorkspaceResolveError,
	include_game_base: bool,
) -> MergePlanResult {
	let mut result = MergePlanResult {
		generated_at: current_generated_at(),
		include_game_base,
		..MergePlanResult::default()
	};
	if err.kind == WorkspaceResolveErrorKind::PlaylistFormat {
		result.push_fatal_error("failed to parse Playset JSON");
	} else {
		result.push_fatal_error(err.message.clone());
	}
	result
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
	if let Err(error) = validate_structural_snapshot(workspace, profile) {
		result.push_fatal_error(error);
		return result;
	}
	result.paths = build_merge_units(workspace, profile);
	result.strategies = summarize_paths(&result.paths);
	result
}

type ModuleInputs<'a> = Vec<(&'a str, &'a [ResolvedFileContributor])>;

fn build_merge_units(
	workspace: &ResolvedWorkspace,
	profile: &dyn GameProfile,
) -> Vec<MergePlanEntry> {
	let mut regular = Vec::new();
	let mut modules: BTreeMap<MergeUnitId, (DefinitionModulePolicy, ModuleInputs<'_>)> =
		BTreeMap::new();

	for (path, contributors) in &workspace.file_inventory {
		let Some(descriptor) = profile.classify_content_family(Path::new(path)) else {
			regular.push(classify_entry(
				path,
				contributors,
				None,
				&workspace.script_cache,
			));
			continue;
		};
		let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
			regular.push(classify_entry(
				path,
				contributors,
				Some(descriptor),
				&workspace.script_cache,
			));
			continue;
		};
		let merge_unit = MergeUnitId {
			family_id: descriptor.id.as_str().to_string(),
			module_name: policy
				.namespace_prefix
				.rsplit('/')
				.next()
				.unwrap_or(descriptor.id.as_str())
				.to_string(),
		};
		modules
			.entry(merge_unit)
			.or_insert_with(|| (policy, Vec::new()))
			.1
			.push((path.as_str(), contributors.as_slice()));
	}

	for (merge_unit, (policy, inputs)) in modules {
		regular.push(classify_module_entry(
			merge_unit,
			policy,
			&inputs,
			&workspace.script_cache,
		));
	}
	regular.sort_by(|left, right| left.output_path().cmp(right.output_path()));
	regular
}

fn classify_module_entry(
	merge_unit: MergeUnitId,
	policy: DefinitionModulePolicy,
	inputs: &ModuleInputs<'_>,
	script_cache: &WorkspaceScriptCache,
) -> MergePlanEntry {
	let input_paths = inputs
		.iter()
		.map(|(path, _)| (*path).to_string())
		.collect::<Vec<_>>();
	let mut contributors = inputs
		.iter()
		.flat_map(|(_, contributors)| contributors.iter())
		.map(to_merge_contributor)
		.collect::<Vec<_>>();
	contributors.sort_by(|left, right| {
		left.precedence
			.cmp(&right.precedence)
			.then_with(|| left.source_path.cmp(&right.source_path))
			.then_with(|| left.mod_id.cmp(&right.mod_id))
	});
	let mut notes = Vec::new();
	let strategy = inputs
		.iter()
		.find_map(|(input_path, contributors)| {
			validate_structural_merge_inputs(input_path, contributors, script_cache).err()
		})
		.map_or(MergePlanStrategy::StructuralMerge, |error| {
			notes.push(error.to_string());
			MergePlanStrategy::ManualConflict
		});
	let winner = (strategy != MergePlanStrategy::ManualConflict)
		.then(|| contributors.last().cloned())
		.flatten();

	MergePlanEntry {
		target: MergePlanTarget::Module {
			id: merge_unit,
			input_paths,
			output_path: policy.output_path.to_string(),
			replace_prefix: (policy.output_mode == DefinitionModuleOutput::ReplaceNamespace)
				.then(|| policy.namespace_prefix.to_string()),
		},
		strategy,
		contributors,
		winner,
		generated: false,
		notes,
	}
}

fn classify_entry(
	path: &str,
	contributors: &[ResolvedFileContributor],
	descriptor: Option<&ContentFamilyDescriptor>,
	script_cache: &WorkspaceScriptCache,
) -> MergePlanEntry {
	let contributors_out: Vec<MergePlanContributor> =
		contributors.iter().map(to_merge_contributor).collect();
	let mut winner = contributors_out.last().cloned();
	let mut notes = Vec::new();

	let strategy = if contributors.len() == 1 {
		MergePlanStrategy::CopyThrough
	} else if is_structural_merge_path(path, descriptor) {
		match validate_structural_merge_inputs(path, contributors, script_cache) {
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
		target: MergePlanTarget::File {
			path: path.to_string(),
		},
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
	script_cache: &WorkspaceScriptCache,
) -> Result<(), MergeError> {
	let mut failures = Vec::new();
	let is_defines_path = path.to_ascii_lowercase().starts_with("common/defines/");

	for contributor in contributors {
		let Some(parse_ok) = contributor.parse_ok_hint else {
			failures.push(format!(
				"missing cached parse status for {}",
				contributor.mod_id
			));
			continue;
		};
		if !parse_ok {
			if contributor.is_base_game {
				failures.push(format!("base game parse issues in {}", contributor.mod_id));
			} else {
				failures.push(format!("cached parse issues in {}", contributor.mod_id));
			}
			continue;
		}
		if !is_defines_path {
			continue;
		}
		let parsed = match script_cache.load(contributor) {
			Ok(parsed) => parsed,
			Err(error) => {
				failures.push(format!(
					"failed to load AST for {}: {error}",
					contributor.mod_id
				));
				continue;
			}
		};
		if let Err(err) = normalize_defines_file(&parsed) {
			failures.push(format!(
				"non-normalizable defines in {}: {}",
				contributor.mod_id, err
			));
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

fn is_structural_merge_path(path: &str, descriptor: Option<&ContentFamilyDescriptor>) -> bool {
	if !is_text_like_overlay_path(path) {
		return false;
	}
	descriptor
		.and_then(|descriptor| descriptor.merge_key_source)
		.is_some()
}

fn validate_structural_snapshot(
	workspace: &ResolvedWorkspace,
	profile: &dyn GameProfile,
) -> Result<(), String> {
	for (path, contributors) in &workspace.file_inventory {
		let descriptor = profile.classify_content_family(Path::new(path));
		if !is_structural_merge_path(path, descriptor) {
			continue;
		}
		for contributor in contributors {
			if contributor.parse_ok_hint.is_some() {
				continue;
			}
			let repair = if contributor.is_base_game {
				"foch data build eu4 --from-game-path <EU4_ROOT> --game-version auto --install"
			} else {
				"foch cache clear --layer mods --yes"
			};
			return Err(format!(
				"structural merge snapshot invariant violated for {path} from {}: missing parse status; run `{repair}` and retry",
				contributor.mod_id
			));
		}
	}
	Ok(())
}

pub(crate) fn prune_noop_script_contributors(
	workspace: &mut ResolvedWorkspace,
	profile: &dyn GameProfile,
) {
	let script_cache = &workspace.script_cache;
	workspace
		.file_inventory
		.retain(|relative_path, contributors| {
			let descriptor = profile.classify_content_family(Path::new(relative_path));
			if descriptor.is_some_and(|descriptor| {
				matches!(
					descriptor.load_policy,
					ContentLoadPolicy::DefinitionModule(_)
				)
			}) {
				return true;
			}
			if !is_structural_merge_path(relative_path, descriptor) {
				return true;
			}
			if contributors.len() < 2 {
				return true;
			}
			contributors.retain(|contributor| {
				contributor.is_base_game
					|| contributor.is_synthetic_base
					|| script_cache.is_noop_hint(&contributor.mod_id, Path::new(relative_path))
						!= Some(true)
			});
			!contributors.is_empty()
		});
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

#[cfg(test)]
mod tests {
	use super::build_merge_plan_from_workspace;
	use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace};
	use foch_core::domain::game::Game;
	use foch_core::domain::playlist::Playlist;
	use std::collections::BTreeMap;
	use std::path::{Path, PathBuf};

	fn workspace_with_snapshot_gap(
		mod_id: &str,
		is_base_game: bool,
		parse_ok_hint: Option<bool>,
	) -> ResolvedWorkspace {
		let root_path = PathBuf::from(mod_id);
		let mut file_inventory = BTreeMap::new();
		file_inventory.insert(
			"events/test.txt".to_string(),
			vec![ResolvedFileContributor {
				mod_id: mod_id.to_string(),
				root_path: root_path.clone(),
				absolute_path: root_path.join("events/test.txt"),
				precedence: usize::from(!is_base_game),
				is_base_game,
				is_synthetic_base: false,
				parse_ok_hint,
				mod_hash: (!is_base_game).then(|| format!("hash-{mod_id}")),
			}],
		);
		ResolvedWorkspace {
			playlist_path: PathBuf::from("playlist.json"),
			playlist: Playlist {
				game: Game::EuropaUniversalis4,
				name: "snapshot-gap".to_string(),
				mods: Vec::new(),
			},
			mods: Vec::new(),
			installed_base_snapshot: None,
			cache_game_version: None,
			mod_snapshots: Vec::new(),
			script_cache: Default::default(),
			file_inventory,
			requested_retained_paths: None,
			effective_retained_paths: None,
		}
	}

	#[test]
	fn missing_mod_parse_status_is_a_fatal_snapshot_invariant() {
		let workspace = workspace_with_snapshot_gap("mod-a", false, None);

		let result = build_merge_plan_from_workspace(&workspace, false);

		assert!(result.has_fatal_errors());
		assert!(result.paths.is_empty());
		assert!(
			result
				.fatal_errors
				.iter()
				.any(|error| error.contains("foch cache clear --layer mods --yes"))
		);
	}

	#[test]
	fn missing_cached_ast_is_not_a_snapshot_invariant() {
		let workspace = workspace_with_snapshot_gap("__game__", true, Some(true));

		let result = build_merge_plan_from_workspace(&workspace, true);

		assert!(!result.has_fatal_errors());
		assert_eq!(result.paths.len(), 1);
	}

	#[test]
	fn singleton_structural_contributor_does_not_load_an_ast_during_pruning() {
		let workspace = workspace_with_snapshot_gap("mod-a", false, Some(true));

		let result = build_merge_plan_from_workspace(&workspace, false);

		assert!(!result.has_fatal_errors());
		assert_eq!(result.paths.len(), 1);
		assert!(
			!workspace
				.script_cache
				.is_loaded("mod-a", Path::new("events/test.txt"))
		);
	}
}
