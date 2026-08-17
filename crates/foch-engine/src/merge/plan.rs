use super::error::MergeError;
use super::normalize::normalize_defines_file;
use crate::request::{CheckRequest, MergePlanOptions};
use crate::workspace::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveError, WorkspaceResolveErrorKind,
	WorkspaceScriptCache, resolve_workspace,
};
use foch_core::model::{
	DocumentFamily, MergePlanContributor, MergePlanEntry, MergePlanResult, MergePlanStrategies,
	MergePlanStrategy, MergePlanTarget, MergeUnitId,
};
use foch_language::analyzer::content_family::GameProfile;
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, ContentLoadPolicy, DefinitionModuleOutput, DefinitionModulePolicy,
};
use foch_language::analyzer::documents::{classify_document_family, is_clausewitz_defines_path};
use foch_language::analyzer::eu4_profile::eu4_profile;
use std::collections::{BTreeMap, BTreeSet};
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
		if !is_structural_merge_path(path, Some(descriptor)) {
			regular.push(classify_entry(
				path,
				contributors,
				Some(descriptor),
				&workspace.script_cache,
			));
			continue;
		}
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
		let has_reset_participant = module_has_reset_participant(workspace, policy);
		if !module_has_non_base_contributor(&inputs) && !has_reset_participant {
			for (path, contributors) in inputs {
				let descriptor = profile.classify_content_family(Path::new(path));
				regular.push(classify_entry(
					path,
					contributors,
					descriptor,
					&workspace.script_cache,
				));
			}
			continue;
		}
		regular.push(classify_module_entry(
			merge_unit,
			policy,
			&inputs,
			has_reset_participant,
			&workspace.script_cache,
		));
	}
	regular.sort_by(|left, right| left.output_path().cmp(right.output_path()));
	regular
}

fn module_has_non_base_contributor(inputs: &ModuleInputs<'_>) -> bool {
	inputs.iter().any(|(_, contributors)| {
		contributors
			.iter()
			.any(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
	})
}

fn module_has_reset_participant(
	workspace: &ResolvedWorkspace,
	policy: DefinitionModulePolicy,
) -> bool {
	workspace.mods.iter().any(|mod_item| {
		mod_item.root_path.is_some()
			&& mod_item.descriptor.as_ref().is_some_and(|descriptor| {
				descriptor.replace_path.iter().any(|replace_path| {
					replace_path_covers_namespace(replace_path, policy.namespace_prefix)
				})
			})
	})
}

fn participating_module_namespaces(
	workspace: &ResolvedWorkspace,
	profile: &dyn GameProfile,
) -> BTreeSet<&'static str> {
	workspace
		.file_inventory
		.iter()
		.filter_map(|(path, contributors)| {
			let descriptor = profile.classify_content_family(Path::new(path))?;
			let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
				return None;
			};
			if !is_structural_merge_path(path, Some(descriptor)) {
				return None;
			}
			let has_non_base_contributor = contributors
				.iter()
				.any(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base);
			(has_non_base_contributor || module_has_reset_participant(workspace, policy))
				.then_some(policy.namespace_prefix)
		})
		.collect()
}

fn replace_path_covers_namespace(replace_path: &str, namespace_prefix: &str) -> bool {
	let replace_path = replace_path.trim_matches('/').replace('\\', "/");
	let namespace_prefix = namespace_prefix.trim_matches('/').replace('\\', "/");
	!replace_path.is_empty()
		&& (namespace_prefix == replace_path
			|| namespace_prefix
				.strip_prefix(&replace_path)
				.is_some_and(|suffix| suffix.starts_with('/')))
}

fn classify_module_entry(
	merge_unit: MergeUnitId,
	policy: DefinitionModulePolicy,
	inputs: &ModuleInputs<'_>,
	has_reset_participant: bool,
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
			replace_prefix: (policy.output_mode == DefinitionModuleOutput::ReplaceNamespace
				|| has_reset_participant)
				.then(|| policy.namespace_prefix.to_string()),
		},
		strategy,
		contributors,
		winner,
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
	let is_defines_path = is_clausewitz_defines_path(Path::new(path));

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
	if classify_document_family(Path::new(path)) != Some(DocumentFamily::Clausewitz) {
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
	let participating_modules = participating_module_namespaces(workspace, profile);
	for (path, contributors) in &workspace.file_inventory {
		let descriptor = profile.classify_content_family(Path::new(path));
		if !is_structural_merge_path(path, descriptor) {
			continue;
		}
		if descriptor.is_some_and(|descriptor| {
			matches!(
				descriptor.load_policy,
				ContentLoadPolicy::DefinitionModule(policy)
					if !participating_modules.contains(policy.namespace_prefix)
			)
		}) {
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
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::game::Game;
	use foch_core::domain::playlist::{Playlist, PlaylistEntry};
	use foch_core::model::{MergePlanStrategy, MergePlanTarget, ModCandidate};
	use std::collections::{BTreeMap, BTreeSet};
	use std::path::{Path, PathBuf};

	fn workspace_with_snapshot_gap(
		mod_id: &str,
		is_base_game: bool,
		parse_ok_hint: Option<bool>,
	) -> ResolvedWorkspace {
		workspace_with_snapshot_gap_at_path("events/test.txt", mod_id, is_base_game, parse_ok_hint)
	}

	fn workspace_with_snapshot_gap_at_path(
		relative_path: &str,
		mod_id: &str,
		is_base_game: bool,
		parse_ok_hint: Option<bool>,
	) -> ResolvedWorkspace {
		let root_path = PathBuf::from(mod_id);
		let mut file_inventory = BTreeMap::new();
		file_inventory.insert(
			relative_path.to_string(),
			vec![ResolvedFileContributor {
				mod_id: mod_id.to_string(),
				root_path: root_path.clone(),
				absolute_path: root_path.join(relative_path),
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
			verified_absent_base_paths: BTreeSet::new(),
			requested_retained_paths: None,
			effective_retained_paths: None,
		}
	}

	fn mod_contributor(
		mod_id: &str,
		relative_path: &str,
		precedence: usize,
	) -> ResolvedFileContributor {
		let root_path = PathBuf::from(mod_id);
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: root_path.clone(),
			absolute_path: root_path.join(relative_path),
			precedence,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: Some(true),
			mod_hash: Some(format!("hash-{mod_id}")),
		}
	}

	fn reset_only_mod(mod_id: &str, replace_path: &str) -> ModCandidate {
		let root_path = PathBuf::from(mod_id);
		ModCandidate {
			entry: PlaylistEntry {
				enabled: true,
				root_path: Some(root_path.clone()),
				..PlaylistEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: Some(root_path),
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: mod_id.to_string(),
				replace_path: vec![replace_path.to_string()],
				..ModDescriptor::default()
			}),
			workshop_identity: None,
			descriptor_error: None,
			files: Vec::new(),
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

	#[test]
	fn base_only_definition_module_input_is_planned_as_copy_through() {
		let workspace = workspace_with_snapshot_gap_at_path(
			"common/powerprojection/00_static.txt",
			"__game__eu4",
			true,
			None,
		);

		let result = build_merge_plan_from_workspace(&workspace, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.paths.len(), 1, "{:#?}", result.paths);
		assert_eq!(result.paths[0].strategy, MergePlanStrategy::CopyThrough);
		assert!(matches!(
			&result.paths[0].target,
			MergePlanTarget::File { path }
				if path == "common/powerprojection/00_static.txt"
		));
		assert!(
			result.paths[0]
				.contributors
				.iter()
				.all(|contributor| contributor.is_base_game)
		);
	}

	#[test]
	fn participating_definition_module_keeps_base_only_inputs() {
		let base_path = "common/powerprojection/00_static.txt";
		let mod_path = "common/powerprojection/modded.txt";
		let mut workspace =
			workspace_with_snapshot_gap_at_path(base_path, "__game__eu4", true, Some(true));
		workspace.file_inventory.insert(
			mod_path.to_string(),
			vec![mod_contributor("mod-a", mod_path, 1)],
		);

		let result = build_merge_plan_from_workspace(&workspace, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.paths.len(), 1, "{:#?}", result.paths);
		let MergePlanTarget::Module {
			input_paths,
			replace_prefix,
			..
		} = &result.paths[0].target
		else {
			panic!("expected definition module");
		};
		assert_eq!(input_paths, &[base_path.to_string(), mod_path.to_string()]);
		assert!(replace_prefix.is_none());
		assert!(
			result.paths[0]
				.contributors
				.iter()
				.any(|contributor| contributor.is_base_game)
		);
	}

	#[test]
	fn reset_only_mod_participates_in_base_backed_definition_module() {
		let mut workspace = workspace_with_snapshot_gap_at_path(
			"common/powerprojection/00_static.txt",
			"__game__eu4",
			true,
			Some(true),
		);
		workspace.mods.push(reset_only_mod("reset-mod", "common"));
		workspace.mod_snapshots.push(None);

		let result = build_merge_plan_from_workspace(&workspace, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.paths.len(), 1, "{:#?}", result.paths);
		assert!(matches!(
			&result.paths[0].target,
			MergePlanTarget::Module {
				replace_prefix: Some(prefix),
				..
			} if prefix == "common/powerprojection"
		));
	}

	#[test]
	fn non_clausewitz_lua_under_structural_family_does_not_require_parse_status() {
		let mut workspace = workspace_with_snapshot_gap_at_path(
			"gfx/shader_upgrade.lua",
			"__game__eu4",
			true,
			None,
		);
		workspace
			.file_inventory
			.get_mut("gfx/shader_upgrade.lua")
			.expect("shader inventory")
			.push(ResolvedFileContributor {
				mod_id: "mod-a".to_string(),
				root_path: PathBuf::from("mod-a"),
				absolute_path: PathBuf::from("mod-a/gfx/shader_upgrade.lua"),
				precedence: 1,
				is_base_game: false,
				is_synthetic_base: false,
				parse_ok_hint: None,
				mod_hash: Some("hash-mod-a".to_string()),
			});

		let result = build_merge_plan_from_workspace(&workspace, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.strategies.last_writer_overlay, 1);
		assert_eq!(
			result.paths[0]
				.winner
				.as_ref()
				.map(|winner| winner.mod_id.as_str()),
			Some("mod-a")
		);
	}

	#[test]
	fn clausewitz_paths_under_mixed_extension_families_require_parse_status() {
		for path in [
			"gfx/test.gfx",
			"common/defines.lua",
			"common/defines/test.lua",
		] {
			let workspace = workspace_with_snapshot_gap_at_path(path, "__game__eu4", true, None);

			let result = build_merge_plan_from_workspace(&workspace, true);

			assert!(result.has_fatal_errors(), "{path}");
			assert!(result.paths.is_empty(), "{path}");
		}
	}

	#[test]
	fn non_clausewitz_file_under_definition_module_is_a_regular_overlay() {
		let mut workspace = workspace_with_snapshot_gap_at_path(
			"common/governments/metadata.json",
			"__game__eu4",
			true,
			None,
		);
		workspace
			.file_inventory
			.get_mut("common/governments/metadata.json")
			.expect("governments metadata inventory")
			.push(ResolvedFileContributor {
				mod_id: "mod-a".to_string(),
				root_path: PathBuf::from("mod-a"),
				absolute_path: PathBuf::from("mod-a/common/governments/metadata.json"),
				precedence: 1,
				is_base_game: false,
				is_synthetic_base: false,
				parse_ok_hint: None,
				mod_hash: Some("hash-mod-a".to_string()),
			});

		let result = build_merge_plan_from_workspace(&workspace, true);

		assert!(!result.has_fatal_errors(), "{:#?}", result.fatal_errors);
		assert_eq!(result.strategies.last_writer_overlay, 1);
		assert!(matches!(
			&result.paths[0].target,
			foch_core::model::MergePlanTarget::File { path }
				if path == "common/governments/metadata.json"
		));
	}
}
