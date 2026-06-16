#![allow(dead_code)]

use super::conflict_handler::{
	ConflictHandler, DeferHandler, DepImpliesResolutionHandler, PriorityBoostResolutionHandler,
	PromptOutcomeKind, prompt_survivors_and_persist,
};
use super::conflict_view::build_conflict_view;
use super::dag::{
	DagDiagnostic, DagDiagnosticKind, IgnoreReplacePath, ModDag, ModId, build_mod_dag,
};
use super::error::MergeError;
use super::localisation_merge::{LocalisationMergeOutcome, merge_localisation_file};
#[allow(unused_imports)]
use super::namespace::{
	FamilyKeyIndex, build_family_key_index, detect_key_conflicts, group_by_family,
};
use super::normalize::normalize_defines_file;
use super::patch::ClausewitzPatch;
use super::patch_deps::{DagPatchRequest, compute_dag_patches_with_handler};
use super::patch_merge::{AttributedPatch, PatchAddress, PatchConflict, PatchResolution};
use super::plan::build_merge_plan_from_workspace;
use super::stale_vanilla::detect_stale_vanilla_targets;
use crate::emit::{EmitOptions, emit_clausewitz_statements_with_options};
use crate::request::{CheckRequest, MergePlanOptions};
use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace, resolve_workspace};
use foch_core::config::{
	AppliedDepOverride, DepOverride, FochConfig, ResolutionDecision, ResolutionMap,
	compute_conflict_id,
};
use foch_core::model::{
	CheckContext, DepMisuseFinding, HandlerResolutionRecord, LeafConflictDetail,
	MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH, MERGED_MOD_DESCRIPTOR_PATH,
	MergePlanContributor, MergePlanEntry, MergePlanResult, MergePlanStrategy, MergeReport,
	MergeReportConflictContributor, MergeReportConflictResolution, MergeReportStatus,
	SemanticIndex, StaleVanillaTargetDescriptor,
};
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, GameProfile, MergeKeySource,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::rules::{detect_dependency_misuse, detect_version_mismatch};
use foch_language::analyzer::semantic_index::{
	ParsedScriptFile, is_decision_container_key, parse_script_file, parse_script_file_with_profile,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::io::IsTerminal;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) struct MergeMaterializeOptions {
	pub include_game_base: bool,
	pub force: bool,
	pub ignore_replace_path: bool,
	pub dep_overrides: Vec<AppliedDepOverride>,
	pub resolution_map: foch_core::config::ResolutionMap,
	pub interactive_conflict_handler: Option<Box<dyn ConflictHandler>>,
	pub interactive_resolution_config_path: Option<PathBuf>,
}

impl Default for MergeMaterializeOptions {
	fn default() -> Self {
		Self {
			include_game_base: true,
			force: false,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_map: foch_core::config::ResolutionMap::default(),
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
		}
	}
}

fn apply_mod_priority_boosts(workspace: &mut ResolvedWorkspace, boosts: &BTreeMap<String, i32>) {
	if boosts.is_empty() {
		return;
	}

	for contributors in workspace.file_inventory.values_mut() {
		for contributor in contributors.iter_mut() {
			if contributor.is_base_game || contributor.is_synthetic_base {
				continue;
			}
			let Some(boost) = boosts.get(&contributor.mod_id) else {
				continue;
			};
			contributor.precedence = boosted_precedence(contributor.precedence, *boost);
		}
		contributors.sort_by(|left, right| {
			left.precedence
				.cmp(&right.precedence)
				.then_with(|| {
					contributor_priority_rank(left).cmp(&contributor_priority_rank(right))
				})
				.then_with(|| left.mod_id.cmp(&right.mod_id))
		});
	}
}

fn boosted_precedence(precedence: usize, boost: i32) -> usize {
	if boost >= 0 {
		precedence.saturating_add(boost as usize)
	} else {
		precedence.saturating_sub(boost.saturating_abs() as usize)
	}
}

fn contributor_priority_rank(contributor: &ResolvedFileContributor) -> u8 {
	if contributor.is_base_game || contributor.is_synthetic_base {
		0
	} else {
		1
	}
}

pub(crate) fn materialize_merge_internal(
	request: CheckRequest,
	out_dir: &Path,
	mut options: MergeMaterializeOptions,
) -> Result<MergeReport, MergeError> {
	let mut report = MergeReport::default();
	let mut generated_paths = BTreeSet::new();

	// Resolve once and reuse: build_merge_plan_from_workspace and the rest of
	// the pipeline both consume the same ResolvedWorkspace. The legacy
	// run_merge_plan_with_options recovery path is kept for the case where
	// resolution itself failed (it may still produce a fatal-only plan).
	let mut workspace_result = stage_log_with("resolve_workspace", || {
		let result = resolve_workspace(&request, options.include_game_base);
		let summary = result
			.as_ref()
			.ok()
			.map(|w| format!("mods={} files={}", w.mods.len(), w.file_inventory.len()));
		(result, summary)
	});
	if let Ok(workspace) = &mut workspace_result {
		// The merge resolution map is loaded after generic workspace resolution,
		// so priority_boost is a merge-only post-processing pass here.
		apply_mod_priority_boosts(workspace, &options.resolution_map.mod_priority_boost);
	}

	let plan = stage_log_with("build_merge_plan", || {
		let plan = match &workspace_result {
			Ok(workspace) => build_merge_plan_from_workspace(workspace, options.include_game_base),
			Err(_) => crate::run_merge_plan_with_options(
				request.clone(),
				MergePlanOptions {
					include_game_base: options.include_game_base,
				},
			),
		};
		let summary = format!(
			"total_paths={} copy_through={} last_writer_overlay={} structural_merge={} localisation_merge={} manual_conflict={}",
			plan.strategies.total_paths,
			plan.strategies.copy_through,
			plan.strategies.last_writer_overlay,
			plan.strategies.structural_merge,
			plan.strategies.localisation_merge,
			plan.strategies.manual_conflict,
		);
		(plan, Some(summary))
	});
	report.manual_conflict_count = plan.strategies.manual_conflict;

	if plan.has_fatal_errors() {
		report.status = MergeReportStatus::Fatal;
		write_metadata_only(out_dir, &plan, &report)?;
		return Ok(report);
	}

	let workspace = workspace_result?;
	let (mod_dag, dag_diagnostics) = stage_log_with("build_mod_dag", || {
		let (dag, diags) = build_mod_dag(&workspace.mods);
		let summary = format!("nodes={} diagnostics={}", dag.topo().len(), diags.len());
		((dag, diags), Some(summary))
	});
	record_dag_diagnostics(&mut report, &dag_diagnostics);
	let analyzer_context = dependency_misuse_context(&workspace);
	report.dep_misuse = stage_log_with("dependency_misuse_detection", || {
		let findings = detect_dependency_misuse(&analyzer_context);
		let summary = format!("findings={}", findings.len());
		(findings, Some(summary))
	});
	if let Some(game_version) = workspace_game_version(&workspace) {
		report.version_mismatch = detect_version_mismatch(&analyzer_context, game_version);
	}
	report.dep_overrides_applied = filter_applied_dep_overrides(&mod_dag, &options.dep_overrides);
	let dep_overrides: Vec<DepOverride> = report
		.dep_overrides_applied
		.iter()
		.map(DepOverride::from)
		.collect();
	let ignore_replace_path = if options.ignore_replace_path {
		IgnoreReplacePath::All
	} else {
		IgnoreReplacePath::None
	};

	if report.manual_conflict_count > 0 && !options.force {
		record_plan_manual_conflicts(&mut report, &plan);
		report.status = MergeReportStatus::Blocked;
		write_metadata_only(out_dir, &plan, &report)?;
		return Ok(report);
	}

	fs::create_dir_all(out_dir)?;
	let descriptor_root = out_dir
		.canonicalize()
		.unwrap_or_else(|_| out_dir.to_path_buf());

	let profile = eu4_profile();
	let mod_versions = workspace_mod_versions(&workspace);
	let mod_display_names = workspace_mod_display_names(&workspace);
	let cache_game_version = workspace_cache_game_version(&workspace);
	let cache_game_version =
		cache_game_version_with_resolution_salt(&cache_game_version, &options.resolution_map);
	let emit_options = load_emit_options(&request)?;

	crate::cache::reset_mod_diff_cache_stats();
	crate::cache::reset_dag_base_cache_stats();
	let materialize_started = Instant::now();
	let total_paths = plan.paths.len();
	eprintln!("[merge] materialize: start (total_paths={total_paths})");
	let mut materialize_progress = MaterializeProgress::new(total_paths);

	for entry in &plan.paths {
		materialize_progress.tick();
		match entry.strategy {
			MergePlanStrategy::CopyThrough => {
				copy_winner_file(&workspace, entry, out_dir)?;
				report.copied_file_count += 1;
			}
			MergePlanStrategy::LastWriterOverlay => {
				copy_winner_file(&workspace, entry, out_dir)?;
				report.overlay_file_count += 1;
			}
			MergePlanStrategy::LocalisationMerge => {
				let contributors = workspace.file_inventory.get(&entry.path);
				match contributors {
					Some(contributors) => {
						match merge_localisation_file(&entry.path, contributors) {
							Ok(LocalisationMergeOutcome::Merged(bytes)) => {
								let target = out_dir.join(&entry.path);
								if let Some(parent) = target.parent() {
									fs::create_dir_all(parent)?;
								}
								fs::write(target, bytes)?;
								generated_paths.insert(entry.path.clone());
								report.generated_file_count += 1;
							}
							Ok(LocalisationMergeOutcome::LanguageMismatch { warning }) => {
								report.warnings.push(warning);
								copy_winner_file(&workspace, entry, out_dir)?;
								report.overlay_file_count += 1;
							}
							Err(err) => {
								report.warnings.push(format!(
									"localisation merge overlay for {}: {err}",
									entry.path
								));
								copy_winner_file(&workspace, entry, out_dir)?;
								report.overlay_file_count += 1;
							}
						}
					}
					None => {
						copy_winner_file(&workspace, entry, out_dir)?;
						report.overlay_file_count += 1;
					}
				}
			}
			MergePlanStrategy::StructuralMerge => {
				let contributors = workspace.file_inventory.get(&entry.path);
				let has_base = contributors
					.map(|cs| cs.iter().any(|c| c.is_base_game || c.is_synthetic_base))
					.unwrap_or(false);

				if has_base && let Some(contributors) = contributors {
					// Only use patch engine when 2+ non-base mods contribute
					// (single-mod overlap with base is just last-writer).
					let non_base_count = contributors
						.iter()
						.filter(|c| !c.is_base_game && !c.is_synthetic_base)
						.count();

					if non_base_count >= 2 {
						let descriptor = profile.classify_content_family(Path::new(&entry.path));
						let merge_key_source = descriptor.and_then(|d| d.merge_key_source);

						if let (Some(descriptor), Some(merge_key_source)) =
							(descriptor, merge_key_source)
						{
							let target = entry.path.clone();
							let contribs = contributors.clone();
							let desc = *descriptor;
							let dag = mod_dag.clone();
							let ignore = ignore_replace_path.clone();
							let dep_overrides = dep_overrides.clone();
							let dep_misuse = report.dep_misuse.clone();
							let resolution_map = options.resolution_map.clone();
							let interactive_config_path =
								options.interactive_resolution_config_path.clone();
							let interactive_handler =
								options.interactive_conflict_handler.as_deref_mut();
							let result =
								std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
									let context = PatchBasedMergeContext {
										descriptor: &desc,
										merge_key_source,
										mod_dag: &dag,
										ignore_replace_path: &ignore,
										dep_overrides: &dep_overrides,
										dep_misuse_findings: &dep_misuse,
										resolution_map: &resolution_map,
										mod_versions: &mod_versions,
										mod_display_names: &mod_display_names,
										cache_game_version: &cache_game_version,
										emit_options: &emit_options,
									};
									patch_based_structural_merge(
										&target,
										&contribs,
										context,
										interactive_handler,
										interactive_config_path.as_deref(),
									)
								}));
							match result {
								Ok(Ok(mut merge_output)) => {
									report
										.stale_vanilla_targets
										.append(&mut merge_output.stale_vanilla_targets);
									apply_dep_misuse_remove_counts(
										&mut report.dep_misuse,
										std::mem::take(&mut merge_output.dep_remove_counts),
									);
									let materialization = write_patch_merge_output(
										&entry.path,
										&mut merge_output,
										out_dir,
										&options.resolution_map,
										&mut report,
									)?;
									if materialization.uses_patch_merge_rendered_output() {
										report.per_entry_noop_skipped_count +=
											merge_output.per_entry_noop_skipped_count;
									}
									if materialization.counts_as_generated() {
										generated_paths.insert(entry.path.clone());
										report.generated_file_count += 1;
									} else if materialization.counts_as_noop_skipped() {
										report.noop_skipped_file_count += 1;
									}
									continue;
								}
								Ok(Err(PatchBasedMergeFailure::Unresolved(conflict))) => {
									if resolve_structural_merge_failure(
										StructuralMergeFailureCtx {
											entry,
											out_dir,
											conflict,
											options: &options,
											report: &mut report,
											generated_paths: &mut generated_paths,
										},
									)? {
										continue;
									}
								}
								Ok(Err(PatchBasedMergeFailure::Merge(err))) => {
									let conflict = PatchConflictReport::without_details(format!(
										"patch merge failed: {err}"
									));
									if resolve_structural_merge_failure(
										StructuralMergeFailureCtx {
											entry,
											out_dir,
											conflict,
											options: &options,
											report: &mut report,
											generated_paths: &mut generated_paths,
										},
									)? {
										continue;
									}
								}
								Err(_) => {
									let conflict = PatchConflictReport::without_details(
										"patch merge panicked".to_string(),
									);
									if resolve_structural_merge_failure(
										StructuralMergeFailureCtx {
											entry,
											out_dir,
											conflict,
											options: &options,
											report: &mut report,
											generated_paths: &mut generated_paths,
										},
									)? {
										continue;
									}
								}
							}
						}
					}

					// Single non-base mod or patch engine failed: copy winner
					copy_winner_file(&workspace, entry, out_dir)?;
					generated_paths.insert(entry.path.clone());
					report.generated_file_count += 1;
				} else {
					// No base available at all (neither vanilla nor synthetic);
					// fall back to last-writer copy.
					copy_winner_file(&workspace, entry, out_dir)?;
					generated_paths.insert(entry.path.clone());
					report.generated_file_count += 1;
				}
			}
			MergePlanStrategy::ManualConflict => {
				if options.force {
					if is_text_placeholder_path(&entry.path) {
						write_conflict_placeholder(entry, out_dir)?;
						generated_paths.insert(entry.path.clone());
						report.generated_file_count += 1;
					} else if let Some(contributors) = workspace.file_inventory.get(&entry.path) {
						// Binary conflict: copy highest-precedence (last) mod's version
						if let Some(best) = contributors
							.iter()
							.filter(|c| !c.is_base_game)
							.max_by_key(|c| c.precedence)
						{
							let target = out_dir.join(&entry.path);
							if let Some(parent) = target.parent() {
								fs::create_dir_all(parent)?;
							}
							fs::copy(&best.absolute_path, target)?;
							generated_paths.insert(entry.path.clone());
							report.generated_file_count += 1;
						}
					}
				}
			}
		}
	}
	materialize_progress.finish();
	prune_cross_file_noop_duplicates(
		out_dir,
		&mut generated_paths,
		&workspace,
		profile,
		&mut report,
	)?;
	let mod_diff_cache_stats = crate::cache::mod_diff_cache_stats();
	let dag_base_cache_stats = crate::cache::dag_base_cache_stats();
	eprintln!(
		"[merge] materialize: done elapsed_ms={} generated={} copied={} overlay={} noop_skipped={} cross_file_noop_skipped={} per_entry_noop_skipped={} mod_diff_cache_hits={} mod_diff_cache_misses={} dag_base_cache_hits={} dag_base_cache_misses={}",
		materialize_started.elapsed().as_millis(),
		report.generated_file_count,
		report.copied_file_count,
		report.overlay_file_count,
		report.noop_skipped_file_count,
		report.cross_file_noop_skipped_file_count,
		report.per_entry_noop_skipped_count,
		mod_diff_cache_stats.hits,
		mod_diff_cache_stats.misses,
		dag_base_cache_stats.hits,
		dag_base_cache_stats.misses
	);

	// Namespace conflict warnings (skipped for large workspaces to avoid
	// excessive parsing; will be done incrementally by the LSP).
	// TODO: re-enable once parse_script_file uses iterative instead of
	// recursive parsing for deeply nested files.
	/*
	let grouped = group_by_family(&workspace.file_inventory, profile);
	for (family_id, paths_by_file) in &grouped {
		let descriptor = profile.descriptor_for_root_family(family_id);
		let merge_key_source = descriptor.and_then(|d| d.merge_key_source);
		if let (Some(_descriptor), Some(merge_key_source)) = (descriptor, merge_key_source) {
			let index =
				build_family_key_index(family_id, merge_key_source, paths_by_file, profile);
			let conflicts = detect_key_conflicts(&index);
			for conflict in &conflicts {
				let mod_ids: Vec<_> = conflict
					.contributors
					.iter()
					.filter(|c| !c.is_base_game)
					.map(|c| format!("{}({})", c.mod_id, c.file_path))
					.collect();
				report.warnings.push(format!(
					"namespace conflict: key '{}' in family '{}' defined by multiple mods: {}",
					conflict.key,
					conflict.family_id,
					mod_ids.join(", "),
				));
			}
		}
	}
	*/

	write_generated_descriptor(
		&descriptor_root,
		&request.playset_path,
		&plan.playset_name,
		&out_dir.join(MERGED_MOD_DESCRIPTOR_PATH),
	)?;

	let mut persisted_plan = plan.clone();
	for entry in &mut persisted_plan.paths {
		entry.generated = generated_paths.contains(&entry.path);
	}
	report.status = if report.manual_conflict_count > 0 && !options.force {
		MergeReportStatus::Blocked
	} else if report.manual_conflict_count > 0 {
		MergeReportStatus::PartialSuccess
	} else {
		MergeReportStatus::Ready
	};
	write_metadata_only(out_dir, &persisted_plan, &report)?;
	Ok(report)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CrossFileKeyValue {
	key: String,
	fingerprint: String,
}

#[derive(Default)]
struct FamilyValueFingerprintIndex {
	file_entries: HashMap<String, Vec<CrossFileKeyValue>>,
	path_key_fingerprints: HashMap<(String, String), Vec<String>>,
}

fn prune_cross_file_noop_duplicates(
	out_dir: &Path,
	generated_paths: &mut BTreeSet<String>,
	workspace: &ResolvedWorkspace,
	profile: &dyn GameProfile,
	report: &mut MergeReport,
) -> Result<(), MergeError> {
	if generated_paths.is_empty() {
		return Ok(());
	}

	let effective_inventory = build_effective_merged_inventory(out_dir, generated_paths, workspace);
	let grouped = group_by_family(&effective_inventory, profile);
	let mut dropped_paths = BTreeSet::new();

	for (family_id, paths_by_file) in &grouped {
		let Some(descriptor) = profile.descriptor_for_root_family(family_id) else {
			continue;
		};
		if !descriptor.capabilities.cross_file_dedup_safe {
			continue;
		}
		let Some(merge_key_source) = descriptor.merge_key_source else {
			continue;
		};

		let generated_paths_in_family = generated_paths
			.iter()
			.filter(|path| paths_by_file.contains_key(path.as_str()))
			.cloned()
			.collect::<BTreeSet<_>>();
		if generated_paths_in_family.is_empty() {
			continue;
		}

		let key_index = build_family_key_index(family_id, merge_key_source, paths_by_file, profile);
		let value_index =
			build_family_value_fingerprint_index(paths_by_file, merge_key_source, profile);

		for path in &generated_paths_in_family {
			let Some(entries) = value_index.file_entries.get(path) else {
				continue;
			};
			if entries.is_empty() {
				continue;
			}

			// Deterministic tie-break: a generated file may be covered by vanilla or
			// any non-generated kept output file, but among generated files only a
			// lexicographically earlier surviving path may cover a later one. This
			// keeps the first path when two generated files cross-cover each other.
			let fully_covered = entries.iter().all(|entry| {
				has_cross_file_identical_match(
					&key_index,
					&value_index,
					path,
					entry,
					&generated_paths_in_family,
					&dropped_paths,
				)
			});

			if fully_covered {
				drop_cross_file_noop_path(out_dir, path, family_id, generated_paths, report)?;
				dropped_paths.insert(path.clone());
			}
		}
	}

	Ok(())
}

fn build_effective_merged_inventory(
	out_dir: &Path,
	generated_paths: &BTreeSet<String>,
	workspace: &ResolvedWorkspace,
) -> BTreeMap<String, Vec<ResolvedFileContributor>> {
	let mut all_paths = workspace
		.file_inventory
		.keys()
		.cloned()
		.collect::<BTreeSet<_>>();
	all_paths.extend(generated_paths.iter().cloned());

	let mut inventory = BTreeMap::new();
	for path in all_paths {
		let output_path = out_dir.join(&path);
		if output_path.is_file() {
			inventory.insert(
				path.clone(),
				vec![ResolvedFileContributor {
					mod_id: "__foch_merged_output__".to_string(),
					root_path: out_dir.to_path_buf(),
					absolute_path: output_path,
					precedence: usize::MAX,
					is_base_game: false,
					is_synthetic_base: false,
					parse_ok_hint: None,
				}],
			);
			continue;
		}

		let Some(contributors) = workspace.file_inventory.get(&path) else {
			continue;
		};
		if let Some(base) = contributors
			.iter()
			.find(|contributor| contributor.is_base_game)
		{
			inventory.insert(path, vec![base.clone()]);
		}
	}

	inventory
}

fn build_family_value_fingerprint_index(
	paths_by_file: &BTreeMap<String, Vec<ResolvedFileContributor>>,
	merge_key_source: MergeKeySource,
	profile: &dyn GameProfile,
) -> FamilyValueFingerprintIndex {
	let mut index = FamilyValueFingerprintIndex::default();
	for (rel_path, contributors) in paths_by_file {
		for contributor in contributors {
			let Some(parsed) = parse_script_file_with_profile(
				&contributor.mod_id,
				&contributor.root_path,
				&contributor.absolute_path,
				profile,
			) else {
				continue;
			};
			let entries = extract_key_value_fingerprints(&parsed, merge_key_source);
			for entry in &entries {
				index
					.path_key_fingerprints
					.entry((rel_path.clone(), entry.key.clone()))
					.or_default()
					.push(entry.fingerprint.clone());
			}
			index
				.file_entries
				.entry(rel_path.clone())
				.or_default()
				.extend(entries);
		}
	}
	index
}

fn has_cross_file_identical_match(
	key_index: &FamilyKeyIndex,
	value_index: &FamilyValueFingerprintIndex,
	current_path: &str,
	entry: &CrossFileKeyValue,
	generated_paths_in_family: &BTreeSet<String>,
	dropped_paths: &BTreeSet<String>,
) -> bool {
	let Some(contributors) = key_index.entries.get(&entry.key) else {
		return false;
	};

	contributors.iter().any(|contributor| {
		let other_path = contributor.file_path.as_str();
		if other_path == current_path {
			return false;
		}
		if !covering_path_survives(
			current_path,
			other_path,
			generated_paths_in_family,
			dropped_paths,
		) {
			return false;
		}
		value_index
			.path_key_fingerprints
			.get(&(other_path.to_string(), entry.key.clone()))
			.is_some_and(|fingerprints| fingerprints.iter().any(|fp| fp == &entry.fingerprint))
	})
}

fn covering_path_survives(
	current_path: &str,
	other_path: &str,
	generated_paths_in_family: &BTreeSet<String>,
	dropped_paths: &BTreeSet<String>,
) -> bool {
	if !generated_paths_in_family.contains(other_path) {
		return true;
	}
	other_path < current_path && !dropped_paths.contains(other_path)
}

fn drop_cross_file_noop_path(
	out_dir: &Path,
	path: &str,
	family_id: &str,
	generated_paths: &mut BTreeSet<String>,
	report: &mut MergeReport,
) -> Result<(), MergeError> {
	let target = out_dir.join(path);
	match fs::remove_file(&target) {
		Ok(()) => {}
		Err(err) if err.kind() == io::ErrorKind::NotFound => {}
		Err(err) => return Err(MergeError::Io(err)),
	}
	generated_paths.remove(path);
	report.generated_file_count = report.generated_file_count.saturating_sub(1);
	report.cross_file_noop_skipped_file_count += 1;
	report.handler_resolutions.push(HandlerResolutionRecord {
        path: path.to_string(),
        action: "cross_file_noop_skipped".to_string(),
        source: None,
        rationale: Some(format!(
            "all merge keys are already defined identically in another kept file in the {family_id} namespace"
        )),
    });
	Ok(())
}

fn extract_key_value_fingerprints(
	parsed: &ParsedScriptFile,
	merge_key_source: MergeKeySource,
) -> Vec<CrossFileKeyValue> {
	match merge_key_source {
		MergeKeySource::AssignmentKey => extract_assignment_key_values(parsed),
		MergeKeySource::FieldValue(field) => extract_field_value_key_values(parsed, field),
		MergeKeySource::ContainerChildKey => extract_container_child_key_values(parsed),
		MergeKeySource::ContainerChildFieldValue {
			container,
			child_key_field,
			child_types,
		} => extract_container_child_field_value_key_values(
			parsed,
			container,
			child_key_field,
			child_types,
		),
		MergeKeySource::LeafPath => normalize_defines_file(parsed)
			.map(|fragments| {
				fragments
					.into_iter()
					.map(|fragment| CrossFileKeyValue {
						key: fragment.merge_key,
						fingerprint: statement_fingerprint(&fragment.statement),
					})
					.collect()
			})
			.unwrap_or_default(),
	}
}

fn extract_assignment_key_values(parsed: &ParsedScriptFile) -> Vec<CrossFileKeyValue> {
	parsed
		.ast
		.statements
		.iter()
		.filter_map(|stmt| match stmt {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { .. },
				..
			} => Some(CrossFileKeyValue {
				key: key.clone(),
				fingerprint: statement_fingerprint(stmt),
			}),
			_ => None,
		})
		.collect()
}

fn extract_field_value_key_values(
	parsed: &ParsedScriptFile,
	field: &str,
) -> Vec<CrossFileKeyValue> {
	parsed
		.ast
		.statements
		.iter()
		.filter_map(|stmt| {
			let AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} = stmt
			else {
				return None;
			};
			scalar_assignment_value(items, field).map(|key| CrossFileKeyValue {
				key,
				fingerprint: statement_fingerprint(stmt),
			})
		})
		.collect()
}

fn extract_container_child_key_values(parsed: &ParsedScriptFile) -> Vec<CrossFileKeyValue> {
	let mut entries = Vec::new();
	for stmt in &parsed.ast.statements {
		let AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} = stmt
		else {
			continue;
		};
		if !is_decision_container_key(key) {
			continue;
		}
		for item in items {
			if let AstStatement::Assignment {
				key: child_key,
				value: AstValue::Block { .. },
				..
			} = item
			{
				entries.push(CrossFileKeyValue {
					key: child_key.clone(),
					fingerprint: container_child_fingerprint(key, item),
				});
			}
		}
	}
	entries
}

fn extract_container_child_field_value_key_values(
	parsed: &ParsedScriptFile,
	container: &str,
	child_key_field: &str,
	child_types: &[&str],
) -> Vec<CrossFileKeyValue> {
	let mut entries = Vec::new();
	for stmt in &parsed.ast.statements {
		let AstStatement::Assignment { key, value, .. } = stmt else {
			continue;
		};
		if key != container {
			entries.push(CrossFileKeyValue {
				key: key.clone(),
				fingerprint: statement_fingerprint(stmt),
			});
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		for child in items {
			if let Some(child_key) =
				container_child_field_value_key(child, child_key_field, child_types)
			{
				entries.push(CrossFileKeyValue {
					key: child_key,
					fingerprint: container_child_fingerprint(key, child),
				});
			}
		}
	}
	entries
}

fn container_child_field_value_key(
	stmt: &AstStatement,
	child_key_field: &str,
	child_types: &[&str],
) -> Option<String> {
	let AstStatement::Assignment { key, value, .. } = stmt else {
		return None;
	};
	if (child_types.is_empty() || child_types.contains(&key.as_str()))
		&& let AstValue::Block { items, .. } = value
		&& let Some(field_value) = scalar_assignment_value(items, child_key_field)
	{
		return Some(format!("{key}:{field_value}"));
	}
	Some(key.clone())
}

fn scalar_assignment_value(items: &[AstStatement], expected_key: &str) -> Option<String> {
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != expected_key {
			continue;
		}
		if let AstValue::Scalar { value, .. } = value {
			return Some(value.as_text());
		}
	}
	None
}

fn container_child_fingerprint(container: &str, child: &AstStatement) -> String {
	let mut out = String::new();
	out.push_str("container:");
	out.push_str(container);
	out.push(';');
	fingerprint_statement_into(child, &mut out);
	out
}

fn statement_fingerprint(statement: &AstStatement) -> String {
	let mut out = String::new();
	fingerprint_statement_into(statement, &mut out);
	out
}

fn fingerprint_statement_into(statement: &AstStatement, out: &mut String) {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			out.push('a');
			out.push_str(key);
			out.push('=');
			fingerprint_value_into(value, out);
			out.push(';');
		}
		AstStatement::Item { value, .. } => {
			out.push('i');
			fingerprint_value_into(value, out);
			out.push(';');
		}
		AstStatement::Comment { .. } => {}
	}
}

fn fingerprint_value_into(value: &AstValue, out: &mut String) {
	match value {
		AstValue::Scalar { value: scalar, .. } => {
			out.push('s');
			out.push(':');
			out.push_str(&scalar.as_text());
		}
		AstValue::Block { items, .. } => {
			out.push('b');
			out.push('[');
			for item in items {
				fingerprint_statement_into(item, out);
			}
			out.push(']');
		}
	}
}

fn dependency_misuse_context(workspace: &ResolvedWorkspace) -> CheckContext {
	CheckContext {
		playlist_path: workspace.playlist_path.clone(),
		playlist: workspace.playlist.clone(),
		mods: workspace.mods.clone(),
		semantic_index: workspace_mod_semantic_index(workspace),
	}
}

fn load_emit_options(request: &CheckRequest) -> Result<EmitOptions, MergeError> {
	let playset_root = request
		.playset_path
		.parent()
		.unwrap_or_else(|| Path::new("."));
	let config = FochConfig::try_load(playset_root).map_err(|err| MergeError::Validation {
		path: Some(playset_root.display().to_string()),
		message: err.to_string(),
	})?;
	Ok(EmitOptions::with_indent(config.emit_indent()))
}

fn workspace_game_version(workspace: &ResolvedWorkspace) -> Option<&str> {
	workspace
		.installed_base_snapshot
		.as_ref()
		.map(|installed| installed.snapshot.game_version.as_str())
}

fn workspace_cache_game_version(workspace: &ResolvedWorkspace) -> String {
	workspace
		.cache_game_version
		.clone()
		.unwrap_or_else(|| format!("{} unknown", workspace.playlist.game.key()))
}

fn cache_game_version_with_resolution_salt(base: &str, resolution_map: &ResolutionMap) -> String {
	let Some(salt) = resolution_map_cache_salt(resolution_map) else {
		return base.to_string();
	};
	format!("{base} resolutions:{salt}")
}

fn resolution_map_cache_salt(resolution_map: &ResolutionMap) -> Option<String> {
	if resolution_map.by_file.is_empty()
		&& resolution_map.by_conflict_id.is_empty()
		&& resolution_map.mod_priority_boost.is_empty()
		&& resolution_map.pattern_rules.is_empty()
	{
		return None;
	}

	let pattern_rules = resolution_map
		.pattern_rules
		.iter()
		.map(|rule| (&rule.dsl, &rule.decision))
		.collect::<Vec<_>>();
	let raw = format!(
		"by_file={:?};by_conflict_id={:?};mod_priority_boost={:?};pattern_rules={:?}",
		resolution_map.by_file,
		resolution_map.by_conflict_id,
		resolution_map.mod_priority_boost,
		pattern_rules
	);
	Some(blake3::hash(raw.as_bytes()).to_hex().to_string())
}

fn workspace_mod_versions(workspace: &ResolvedWorkspace) -> HashMap<String, String> {
	workspace
		.mods
		.iter()
		.map(|candidate| {
			let version = candidate
				.descriptor
				.as_ref()
				.and_then(|descriptor| descriptor.version.as_deref())
				.map(str::trim)
				.filter(|version| !version.is_empty())
				.unwrap_or("unknown")
				.to_string();
			(candidate.mod_id.clone(), version)
		})
		.collect()
}

fn workspace_mod_display_names(workspace: &ResolvedWorkspace) -> HashMap<String, String> {
	workspace
		.mods
		.iter()
		.map(|candidate| {
			let display_name = candidate
				.descriptor
				.as_ref()
				.map(|descriptor| descriptor.name.trim())
				.filter(|name| !name.is_empty())
				.map(str::to_string)
				.or_else(|| {
					candidate
						.entry
						.display_name
						.as_deref()
						.map(str::trim)
						.filter(|name| !name.is_empty())
						.map(str::to_string)
				})
				.unwrap_or_else(|| candidate.mod_id.clone());
			(candidate.mod_id.clone(), display_name)
		})
		.collect()
}

fn workspace_mod_semantic_index(workspace: &ResolvedWorkspace) -> SemanticIndex {
	let mut merged = SemanticIndex::default();
	for snapshot in workspace.mod_snapshots.iter().flatten() {
		merged = merge_semantic_indexes(merged, snapshot.semantic_index.clone());
	}
	merged
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

fn record_dag_diagnostics(report: &mut MergeReport, diagnostics: &[DagDiagnostic]) {
	for diagnostic in diagnostics {
		if let Some(warning) = dag_diagnostic_warning(diagnostic) {
			report.warnings.push(warning);
		}
	}
}

fn filter_applied_dep_overrides(
	mod_dag: &ModDag,
	overrides: &[AppliedDepOverride],
) -> Vec<AppliedDepOverride> {
	let mut applied = Vec::new();
	for dep_override in overrides {
		let child = ModId(dep_override.mod_id.clone());
		let has_edge = mod_dag
			.parents_of(&child)
			.iter()
			.any(|parent| parent.as_str() == dep_override.dep_id);
		if has_edge && !applied.contains(dep_override) {
			applied.push(dep_override.clone());
		}
	}
	applied
}

fn dag_diagnostic_warning(diagnostic: &DagDiagnostic) -> Option<String> {
	match &diagnostic.kind {
		DagDiagnosticKind::MissingDependency { mod_id, dep_token } => Some(format!(
			"Mod {} declares dep on {} not in playset; treating as absent",
			mod_id.as_str(),
			dep_token
		)),
		DagDiagnosticKind::DependencyCycle { members } => {
			let mods = members
				.iter()
				.map(|mod_id| mod_id.as_str())
				.collect::<Vec<_>>()
				.join(", ");
			Some(format!(
				"Dependency cycle detected among mods {mods}; breaking deterministically by playlist position"
			))
		}
		DagDiagnosticKind::BrokenCycleEdge { .. } => None,
	}
}

fn record_plan_manual_conflicts(report: &mut MergeReport, plan: &MergePlanResult) {
	for entry in &plan.paths {
		if entry.strategy != MergePlanStrategy::ManualConflict {
			continue;
		}
		let reason = if entry.notes.is_empty() {
			"manual conflict".to_string()
		} else {
			entry.notes.join("; ")
		};
		report
			.conflict_resolutions
			.push(plan_conflict_skipped_resolution(entry, &reason));
	}
}

struct StructuralMergeFailureCtx<'a> {
	entry: &'a MergePlanEntry,
	out_dir: &'a Path,
	conflict: PatchConflictReport,
	options: &'a MergeMaterializeOptions,
	report: &'a mut MergeReport,
	generated_paths: &'a mut BTreeSet<String>,
}

fn resolve_structural_merge_failure(
	ctx: StructuralMergeFailureCtx<'_>,
) -> Result<bool, MergeError> {
	let StructuralMergeFailureCtx {
		entry,
		out_dir,
		conflict,
		options,
		report,
		generated_paths,
	} = ctx;
	let reason = conflict.reason;
	report.manual_conflict_count += 1;
	report
		.handler_resolutions
		.extend(conflict.handler_resolutions);
	if options.force && is_text_placeholder_path(&entry.path) {
		let mut marker_entry = entry.clone();
		marker_entry.notes.push(reason.clone());
		write_conflict_placeholder(&marker_entry, out_dir)?;
		report.generated_file_count += 1;
		generated_paths.insert(entry.path.clone());
		report.warnings.push(format!(
			"{} for {}; wrote manual conflict marker",
			reason, entry.path
		));
	} else {
		report.warnings.push(format!(
			"{} for {}; manual resolution required, skipping output",
			reason, entry.path
		));
	}
	report
		.conflict_resolutions
		.push(workspace_conflict_skipped_resolution(
			entry,
			&reason,
			conflict.leaf_conflicts,
		));
	Ok(true)
}

fn workspace_conflict_skipped_resolution(
	entry: &MergePlanEntry,
	reason: &str,
	leaf_conflicts: Vec<LeafConflictDetail>,
) -> MergeReportConflictResolution {
	MergeReportConflictResolution {
		path: entry.path.clone(),
		reason: reason.to_string(),
		leaf_conflicts,
	}
}

fn plan_conflict_skipped_resolution(
	entry: &MergePlanEntry,
	reason: &str,
) -> MergeReportConflictResolution {
	MergeReportConflictResolution {
		path: entry.path.clone(),
		reason: reason.to_string(),
		leaf_conflicts: Vec::new(),
	}
}

#[derive(Clone, Debug)]
struct PatchBasedMergeOutput {
	rendered: String,
	dep_remove_counts: Vec<DepMisuseRemoveCount>,
	stale_vanilla_targets: Vec<StaleVanillaTargetDescriptor>,
	handler_resolutions: Vec<HandlerResolutionRecord>,
	external_file_resolutions: HashMap<PathBuf, PathBuf>,
	keep_existing_paths: HashSet<PathBuf>,
	/// True when the patch-merged statement list is AST-equal (modulo
	/// span / comment trivia) to the vanilla base — shipping the file
	/// would just shadow the game's own copy with the same content.
	noop_vs_vanilla: bool,
	/// Entries removed because an opted-in family already has an identical
	/// vanilla definition at the same key in the same file.
	per_entry_noop_skipped_count: usize,
}

#[derive(Clone, Debug)]
struct PatchConflictReport {
	reason: String,
	leaf_conflicts: Vec<LeafConflictDetail>,
	handler_resolutions: Vec<HandlerResolutionRecord>,
}

impl PatchConflictReport {
	fn without_details(reason: String) -> Self {
		Self {
			reason,
			leaf_conflicts: Vec::new(),
			handler_resolutions: Vec::new(),
		}
	}
}

#[derive(Debug)]
enum PatchBasedMergeFailure {
	Merge(MergeError),
	Unresolved(PatchConflictReport),
}

impl From<MergeError> for PatchBasedMergeFailure {
	fn from(err: MergeError) -> Self {
		Self::Merge(err)
	}
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PerEntryNoopLookupKey {
	path: Vec<String>,
	key: String,
}

fn drop_per_entry_noop_duplicates(
	merged_statements: Vec<AstStatement>,
	vanilla_statements: &[AstStatement],
	descriptor: &ContentFamilyDescriptor,
) -> (Vec<AstStatement>, usize) {
	if !descriptor.capabilities.per_entry_dedup_safe {
		return (merged_statements, 0);
	}
	let Some(merge_key_source) = descriptor.merge_key_source else {
		return (merged_statements, 0);
	};
	if matches!(merge_key_source, MergeKeySource::LeafPath) {
		return (merged_statements, 0);
	}

	let vanilla_lookup = build_per_entry_noop_lookup(vanilla_statements, merge_key_source);
	if vanilla_lookup.is_empty() {
		return (merged_statements, 0);
	}

	filter_per_entry_noop_statements(merged_statements, merge_key_source, &vanilla_lookup)
}

fn build_per_entry_noop_lookup(
	statements: &[AstStatement],
	merge_key_source: MergeKeySource,
) -> HashMap<PerEntryNoopLookupKey, Vec<AstStatement>> {
	let mut lookup: HashMap<PerEntryNoopLookupKey, Vec<AstStatement>> = HashMap::new();
	for statement in statements {
		if let Some(key) = per_entry_noop_top_level_key(statement, merge_key_source) {
			lookup.entry(key).or_default().push(statement.clone());
		}
		for (key, child) in per_entry_noop_child_entries(statement, merge_key_source) {
			lookup.entry(key).or_default().push(child.clone());
		}
	}
	lookup
}

fn filter_per_entry_noop_statements(
	statements: Vec<AstStatement>,
	merge_key_source: MergeKeySource,
	vanilla_lookup: &HashMap<PerEntryNoopLookupKey, Vec<AstStatement>>,
) -> (Vec<AstStatement>, usize) {
	let mut filtered = Vec::with_capacity(statements.len());
	let mut dropped = 0usize;
	for statement in statements {
		if let Some(key) = per_entry_noop_top_level_key(&statement, merge_key_source)
			&& per_entry_noop_matches_vanilla(&key, &statement, vanilla_lookup)
		{
			dropped += 1;
			continue;
		}

		let (statement, child_dropped) =
			filter_per_entry_noop_child_statements(statement, merge_key_source, vanilla_lookup);
		dropped += child_dropped;
		filtered.push(statement);
	}
	(filtered, dropped)
}

fn filter_per_entry_noop_child_statements(
	statement: AstStatement,
	merge_key_source: MergeKeySource,
	vanilla_lookup: &HashMap<PerEntryNoopLookupKey, Vec<AstStatement>>,
) -> (AstStatement, usize) {
	match statement {
		AstStatement::Assignment {
			key,
			key_span,
			value: AstValue::Block {
				items,
				span: value_span,
			},
			span,
		} if per_entry_noop_container_is_filterable(&key, merge_key_source) => {
			let mut filtered_items = Vec::with_capacity(items.len());
			let mut dropped = 0usize;
			for item in items {
				if let Some(lookup_key) = per_entry_noop_child_key(&key, &item, merge_key_source)
					&& per_entry_noop_matches_vanilla(&lookup_key, &item, vanilla_lookup)
				{
					dropped += 1;
					continue;
				}
				filtered_items.push(item);
			}
			(
				AstStatement::Assignment {
					key,
					key_span,
					value: AstValue::Block {
						items: filtered_items,
						span: value_span,
					},
					span,
				},
				dropped,
			)
		}
		other => (other, 0),
	}
}

fn per_entry_noop_matches_vanilla(
	key: &PerEntryNoopLookupKey,
	statement: &AstStatement,
	vanilla_lookup: &HashMap<PerEntryNoopLookupKey, Vec<AstStatement>>,
) -> bool {
	vanilla_lookup.get(key).is_some_and(|vanilla_entries| {
		vanilla_entries
			.iter()
			.any(|vanilla| super::patch::ast_statements_semantically_equal(vanilla, statement))
	})
}

fn per_entry_noop_top_level_key(
	statement: &AstStatement,
	merge_key_source: MergeKeySource,
) -> Option<PerEntryNoopLookupKey> {
	match merge_key_source {
		MergeKeySource::AssignmentKey => match statement {
			AstStatement::Assignment { key, .. } => Some(PerEntryNoopLookupKey {
				path: Vec::new(),
				key: key.clone(),
			}),
			_ => None,
		},
		MergeKeySource::FieldValue(field) => {
			let AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} = statement
			else {
				return None;
			};
			scalar_assignment_value(items, field).map(|key| PerEntryNoopLookupKey {
				path: Vec::new(),
				key,
			})
		}
		MergeKeySource::ContainerChildFieldValue { container, .. } => {
			let AstStatement::Assignment { key, .. } = statement else {
				return None;
			};
			(key != container).then(|| PerEntryNoopLookupKey {
				path: Vec::new(),
				key: key.clone(),
			})
		}
		MergeKeySource::ContainerChildKey | MergeKeySource::LeafPath => None,
	}
}

fn per_entry_noop_child_entries(
	statement: &AstStatement,
	merge_key_source: MergeKeySource,
) -> Vec<(PerEntryNoopLookupKey, &AstStatement)> {
	let AstStatement::Assignment {
		key,
		value: AstValue::Block { items, .. },
		..
	} = statement
	else {
		return Vec::new();
	};
	if !per_entry_noop_container_is_filterable(key, merge_key_source) {
		return Vec::new();
	}
	items
		.iter()
		.filter_map(|item| {
			per_entry_noop_child_key(key, item, merge_key_source)
				.map(|lookup_key| (lookup_key, item))
		})
		.collect()
}

fn per_entry_noop_container_is_filterable(
	container: &str,
	merge_key_source: MergeKeySource,
) -> bool {
	match merge_key_source {
		MergeKeySource::ContainerChildKey => is_decision_container_key(container),
		MergeKeySource::ContainerChildFieldValue {
			container: expected,
			..
		} => container == expected,
		_ => false,
	}
}

fn per_entry_noop_child_key(
	container: &str,
	child: &AstStatement,
	merge_key_source: MergeKeySource,
) -> Option<PerEntryNoopLookupKey> {
	match merge_key_source {
		MergeKeySource::ContainerChildKey => {
			if !is_decision_container_key(container) {
				return None;
			}
			let AstStatement::Assignment { key, .. } = child else {
				return None;
			};
			Some(PerEntryNoopLookupKey {
				path: vec![container.to_string()],
				key: key.clone(),
			})
		}
		MergeKeySource::ContainerChildFieldValue {
			container: expected,
			child_key_field,
			child_types,
		} => {
			if container != expected {
				return None;
			}
			container_child_field_value_key(child, child_key_field, child_types).map(|key| {
				PerEntryNoopLookupKey {
					path: vec![container.to_string()],
					key,
				}
			})
		}
		_ => None,
	}
}

#[derive(Clone, Debug)]
struct DepMisuseRemoveCount {
	mod_id: String,
	dep_id: String,
	count: u32,
}

#[derive(Clone, Copy)]
struct PatchBasedMergeContext<'a> {
	descriptor: &'a ContentFamilyDescriptor,
	merge_key_source: MergeKeySource,
	mod_dag: &'a ModDag,
	ignore_replace_path: &'a IgnoreReplacePath,
	dep_overrides: &'a [DepOverride],
	dep_misuse_findings: &'a [DepMisuseFinding],
	resolution_map: &'a foch_core::config::ResolutionMap,
	mod_versions: &'a HashMap<String, String>,
	mod_display_names: &'a HashMap<String, String>,
	cache_game_version: &'a str,
	emit_options: &'a EmitOptions,
}

fn leaf_conflicts_for_unresolved(
	target_path: &str,
	conflicts: &[PatchResolution],
	mod_versions: &HashMap<String, String>,
) -> Vec<LeafConflictDetail> {
	conflicts
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Conflict {
				address, patches, ..
			} => {
				let address_path = address.path.join("/");
				Some(LeafConflictDetail {
					address_path: address_path.clone(),
					address_key: address.key.clone(),
					conflict_id: compute_conflict_id(
						Path::new(target_path),
						&address_path,
						&address.key,
					),
					contributors: leaf_conflict_contributors(patches, mod_versions),
				})
			}
			_ => None,
		})
		.collect()
}

fn leaf_conflict_contributors(
	patches: &[AttributedPatch],
	mod_versions: &HashMap<String, String>,
) -> Vec<MergeReportConflictContributor> {
	let mut contributors = patches
		.iter()
		.map(|patch| MergeReportConflictContributor {
			mod_id: patch.mod_id.clone(),
			mod_version: mod_versions
				.get(&patch.mod_id)
				.cloned()
				.unwrap_or_else(|| "unknown".to_string()),
			precedence: patch.precedence,
		})
		.collect::<Vec<_>>();
	contributors.sort_by(|left, right| {
		left.precedence
			.cmp(&right.precedence)
			.then_with(|| left.mod_id.cmp(&right.mod_id))
	});
	contributors
		.dedup_by(|left, right| left.mod_id == right.mod_id && left.precedence == right.precedence);
	contributors
}

/// Patch-based structural merge: walk the dependency DAG level by level, diff
/// every mod in a level against the same running base, sibling-merge that
/// level's patches, then apply the resolved level to advance the running state.
fn patch_based_structural_merge(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	context: PatchBasedMergeContext<'_>,
	mut interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
) -> Result<PatchBasedMergeOutput, PatchBasedMergeFailure> {
	// Hold an owned, mutable resolution map so that any post-pass interactive
	// resolutions can be folded back in before we re-run the merge engine
	// below. The merge engine itself never invokes interactive prompts — every
	// surviving conflict that reaches the user has already been pruned by the
	// downstream-override post-pass inside `compute_dag_patches_with_handler`.
	let mut effective_map = context.resolution_map.clone();
	let mut dag_patches =
		run_patch_merge_engine(target_path, contributors, &context, &effective_map)?;
	let vanilla = parse_vanilla_for_stale_detection(target_path, contributors)?;

	if !dag_patches.merge_result.conflicts.is_empty()
		&& let (Some(handler), Some(config_path)) =
			(interactive_handler.as_mut(), interactive_config_path)
	{
		let survivors: Vec<(PatchAddress, PatchConflict)> = dag_patches
			.merge_result
			.conflicts
			.iter()
			.filter_map(|resolution| match resolution {
				PatchResolution::Conflict {
					address,
					patches,
					reason,
				} => Some((
					address.clone(),
					PatchConflict {
						patches: patches.clone(),
						reason: reason.clone(),
					},
				)),
				_ => None,
			})
			.collect();
		if !survivors.is_empty() {
			let vanilla_lookup = |address: &PatchAddress| -> Option<String> {
				vanilla_snippet_for_address(vanilla.as_ref(), address, context.emit_options)
			};
			let survivor_views = survivors
				.iter()
				.map(|(address, conflict)| {
					let address_path = address.path.join("/");
					let conflict_id =
						compute_conflict_id(Path::new(target_path), &address_path, &address.key);
					let view = build_conflict_view(
						Path::new(target_path),
						address,
						conflict,
						conflict_id,
						context.mod_display_names,
						vanilla_lookup(address),
						context.emit_options,
					)?;
					Ok((address.clone(), view))
				})
				.collect::<Result<Vec<_>, MergeError>>()?;
			let prompt = prompt_survivors_and_persist(
				Path::new(target_path),
				&survivor_views,
				&mut **handler,
				config_path,
			);
			let mut new_picks = 0usize;
			for outcome in prompt.outcomes {
				if let PromptOutcomeKind::Picked(decision) = outcome.kind {
					effective_map
						.by_conflict_id
						.insert(outcome.conflict_id, decision);
					new_picks += 1;
				}
			}
			if prompt.aborted {
				return Err(PatchBasedMergeFailure::Merge(MergeError::Validation {
					path: Some(target_path.to_string()),
					message: "merge aborted by user".to_string(),
				}));
			}
			if new_picks > 0 {
				dag_patches =
					run_patch_merge_engine(target_path, contributors, &context, &effective_map)?;
			}
		}
	}

	let stale_vanilla_targets = collect_stale_vanilla_targets(
		target_path,
		&dag_patches.mod_patches,
		vanilla.as_ref(),
		context.merge_key_source,
		context.mod_versions,
	);
	let dep_remove_counts = collect_dep_misuse_remove_counts(
		context.dep_misuse_findings,
		contributors,
		&dag_patches.mod_patches,
	);
	let merge_result = dag_patches.merge_result;

	if !merge_result.conflicts.is_empty() {
		let conflict_keys: Vec<_> = merge_result
			.conflicts
			.iter()
			.filter_map(|r| match r {
				PatchResolution::Conflict {
					address, reason, ..
				} => Some(format!("{}: {}", address.key, reason)),
				_ => None,
			})
			.collect();
		let reason = format!(
			"patch merge has {} unresolved conflict(s): {}",
			conflict_keys.len(),
			conflict_keys.join("; "),
		);
		return Err(PatchBasedMergeFailure::Unresolved(PatchConflictReport {
			reason,
			leaf_conflicts: leaf_conflicts_for_unresolved(
				target_path,
				&merge_result.conflicts,
				context.mod_versions,
			),
			handler_resolutions: merge_result.handler_resolutions,
		}));
	}

	let noop_vs_vanilla = vanilla
		.as_ref()
		.map(|base| {
			super::patch::ast_statement_lists_semantically_equal(
				&base.ast.statements,
				&dag_patches.merged_statements,
			)
		})
		.unwrap_or(false);
	let merged_statements = dag_patches.merged_statements;
	let (merged_statements, per_entry_noop_skipped_count) = if let Some(base) = vanilla.as_ref() {
		drop_per_entry_noop_duplicates(merged_statements, &base.ast.statements, context.descriptor)
	} else {
		(merged_statements, 0)
	};
	let rendered =
		emit_clausewitz_statements_with_options(&merged_statements, context.emit_options)?;
	Ok(PatchBasedMergeOutput {
		rendered,
		dep_remove_counts,
		stale_vanilla_targets,
		handler_resolutions: merge_result.handler_resolutions,
		external_file_resolutions: merge_result.external_file_resolutions,
		keep_existing_paths: merge_result.keep_existing_paths,
		noop_vs_vanilla,
		per_entry_noop_skipped_count,
	})
}

fn run_patch_merge_engine(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	context: &PatchBasedMergeContext<'_>,
	resolution_map: &foch_core::config::ResolutionMap,
) -> Result<super::patch_deps::DagPatchComputation, MergeError> {
	let mut handler = super::conflict_handler::ChainHandler {
		first: super::conflict_handler::LookupHandler::with_display_names(
			resolution_map,
			PathBuf::from(target_path),
			(*context.mod_display_names).clone(),
		),
		second: super::conflict_handler::ChainHandler {
			first: PriorityBoostResolutionHandler::new(
				PathBuf::from(target_path),
				&resolution_map.mod_priority_boost,
			),
			second: super::conflict_handler::ChainHandler {
				first: DepImpliesResolutionHandler::from_mod_dag(
					PathBuf::from(target_path),
					context.mod_dag,
					context.dep_overrides,
				),
				second: DeferHandler,
			},
		},
	};
	compute_dag_patches_with_handler(
		DagPatchRequest {
			file_path: target_path,
			contributors,
			merge_key_source: context.merge_key_source,
			policies: &context.descriptor.merge_policies,
			mod_dag: context.mod_dag,
			ignore_replace_path: context.ignore_replace_path,
			dep_overrides: context.dep_overrides,
			game_version: context.cache_game_version,
		},
		&mut handler,
	)
	.map_err(|err| MergeError::Validation {
		path: Some(target_path.to_string()),
		message: format!("patch computation failed: {err}"),
	})
}

fn vanilla_snippet_for_address(
	vanilla: Option<&ParsedScriptFile>,
	address: &PatchAddress,
	emit_options: &EmitOptions,
) -> Option<String> {
	let vanilla = vanilla?;
	let statements = vanilla_statements_at_address(&vanilla.ast.statements, address);
	Some(match statements {
		Some(statements) if !statements.is_empty() => {
			emit_clausewitz_statements_with_options(&statements, emit_options)
				.unwrap_or_else(|err| format!("(failed to render vanilla snippet: {err})"))
		}
		_ => "(key not present in vanilla)".to_string(),
	})
}

fn vanilla_statements_at_address(
	statements: &[AstStatement],
	address: &PatchAddress,
) -> Option<Vec<AstStatement>> {
	let mut current = statements;
	for segment in &address.path {
		current = current.iter().find_map(|statement| match statement {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} if key == segment => Some(items.as_slice()),
			_ => None,
		})?;
	}

	let Some(key) = vanilla_address_lookup_key(&address.key) else {
		return Some(current.to_vec());
	};
	if key.is_empty() {
		return Some(current.to_vec());
	}

	let matches = current
		.iter()
		.filter(|statement| {
			matches!(statement, AstStatement::Assignment { key: statement_key, .. } if statement_key == key)
		})
		.cloned()
		.collect::<Vec<_>>();
	(!matches.is_empty()).then_some(matches)
}

fn vanilla_address_lookup_key(address_key: &str) -> Option<&str> {
	if let Some(rest) = address_key.strip_prefix("__node__::") {
		return rest.split("::").next();
	}
	if let Some(rest) = address_key.strip_prefix("__list_item__::") {
		return rest.split("::").next();
	}
	if let Some(rest) = address_key.strip_prefix("__rename__::") {
		return Some(rest);
	}
	if address_key.starts_with("__append_block_item__::")
		|| address_key.starts_with("__remove_block_item__::")
	{
		return None;
	}
	Some(address_key)
}

fn parse_vanilla_for_stale_detection(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
) -> Result<Option<ParsedScriptFile>, MergeError> {
	let Some(base) = contributors
		.iter()
		.find(|contributor| contributor.is_base_game)
	else {
		return Ok(None);
	};
	parse_script_file(&base.mod_id, &base.root_path, &base.absolute_path)
		.map(Some)
		.ok_or_else(|| MergeError::Validation {
			path: Some(file_path.to_string()),
			message: format!(
				"failed to parse vanilla file {} for stale target detection",
				base.absolute_path.display()
			),
		})
}

fn collect_stale_vanilla_targets(
	file_path: &str,
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
	vanilla: Option<&ParsedScriptFile>,
	merge_key_source: MergeKeySource,
	mod_versions: &HashMap<String, String>,
) -> Vec<StaleVanillaTargetDescriptor> {
	mod_patches
		.iter()
		.flat_map(|(mod_id, _, patches)| {
			let mod_version = mod_versions
				.get(mod_id)
				.map(String::as_str)
				.unwrap_or("unknown");
			detect_stale_vanilla_targets(
				patches,
				file_path,
				mod_id,
				mod_version,
				vanilla,
				merge_key_source,
			)
		})
		.collect()
}

fn collect_dep_misuse_remove_counts(
	findings: &[DepMisuseFinding],
	contributors: &[ResolvedFileContributor],
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
) -> Vec<DepMisuseRemoveCount> {
	if findings.is_empty() {
		return Vec::new();
	}

	let contributor_mods = contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
		.map(|contributor| contributor.mod_id.as_str())
		.collect::<HashSet<_>>();
	let mut counts = Vec::new();
	for finding in findings {
		if !contributor_mods.contains(finding.mod_id.as_str())
			|| !contributor_mods.contains(finding.suspicious_dep_id.as_str())
		{
			continue;
		}

		let count = mod_patches
			.iter()
			.filter(|(mod_id, _, _)| mod_id == &finding.mod_id)
			.flat_map(|(_, _, patches)| patches)
			.filter(|patch| is_remove_patch(patch))
			.count();
		if count == 0 {
			continue;
		}
		counts.push(DepMisuseRemoveCount {
			mod_id: finding.mod_id.clone(),
			dep_id: finding.suspicious_dep_id.clone(),
			count: count.min(u32::MAX as usize) as u32,
		});
	}
	counts
}

fn is_remove_patch(patch: &ClausewitzPatch) -> bool {
	matches!(
		patch,
		ClausewitzPatch::RemoveNode { .. }
			| ClausewitzPatch::RemoveListItem { .. }
			| ClausewitzPatch::RemoveBlockItem { .. }
	)
}

fn apply_dep_misuse_remove_counts(
	findings: &mut [DepMisuseFinding],
	counts: Vec<DepMisuseRemoveCount>,
) {
	for count in counts {
		if let Some(finding) = findings.iter_mut().find(|finding| {
			finding.mod_id == count.mod_id && finding.suspicious_dep_id == count.dep_id
		}) {
			finding.evidence.false_remove_count = finding
				.evidence
				.false_remove_count
				.saturating_add(count.count);
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PatchOutputMaterialization {
	NormalWrite,
	ExternalWrite,
	KeptExisting,
	NoopSkippedVsVanilla,
}

impl PatchOutputMaterialization {
	fn counts_as_generated(self) -> bool {
		matches!(self, Self::NormalWrite | Self::ExternalWrite)
	}

	fn counts_as_noop_skipped(self) -> bool {
		matches!(self, Self::NoopSkippedVsVanilla)
	}

	fn uses_patch_merge_rendered_output(self) -> bool {
		matches!(self, Self::NormalWrite | Self::NoopSkippedVsVanilla)
	}
}

fn write_patch_merge_output(
	target_path: &str,
	merge_output: &mut PatchBasedMergeOutput,
	out_dir: &Path,
	resolution_map: &ResolutionMap,
	report: &mut MergeReport,
) -> Result<PatchOutputMaterialization, MergeError> {
	let output_relative_path = PathBuf::from(target_path);
	let target = out_dir.join(target_path);

	if matches!(
		resolution_map.lookup(Path::new(target_path), "", ""),
		Some(ResolutionDecision::KeepExisting)
	) {
		merge_output
			.keep_existing_paths
			.insert(output_relative_path.clone());
	}

	if merge_output
		.keep_existing_paths
		.contains(&output_relative_path)
	{
		if target.exists() {
			report.handler_resolutions.push(HandlerResolutionRecord {
				path: target_path.to_string(),
				action: "kept_existing".to_string(),
				source: None,
				rationale: None,
			});
			return Ok(PatchOutputMaterialization::KeptExisting);
		}

		report.warnings.push(format!(
			"keep_existing_failed: file does not exist at output dir: {}",
			target.display()
		));
	}

	if let Some(source_path) = merge_output
		.external_file_resolutions
		.get(&output_relative_path)
	{
		let bytes = fs::read(source_path).map_err(|err| {
			MergeError::Io(io::Error::new(
				err.kind(),
				format!(
					"failed to read external resolution source {} for {}: {err}",
					source_path.display(),
					target_path
				),
			))
		})?;
		if let Some(parent) = target.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::write(&target, bytes)?;
		report.handler_resolutions.push(HandlerResolutionRecord {
			path: target_path.to_string(),
			action: "external".to_string(),
			source: Some(source_path.display().to_string()),
			rationale: None,
		});
		return Ok(PatchOutputMaterialization::ExternalWrite);
	}

	if merge_output.noop_vs_vanilla {
		// The patch-merged result is AST-equivalent to the vanilla base
		// (modulo whitespace and comments). Shipping it would just shadow
		// the game's own copy with byte-for-byte equivalent content, so
		// skip the write and record the skip in the report instead of
		// inflating `generated_file_count` with NoOp files.
		report.handler_resolutions.push(HandlerResolutionRecord {
			path: target_path.to_string(),
			action: "noop_skipped_vs_vanilla".to_string(),
			source: None,
			rationale: Some(
				"merged content is AST-equal to vanilla; not shipping a redundant copy".to_string(),
			),
		});
		return Ok(PatchOutputMaterialization::NoopSkippedVsVanilla);
	}

	write_rendered_output(target_path, &merge_output.rendered, out_dir)?;
	report
		.handler_resolutions
		.extend(merge_output.handler_resolutions.iter().cloned());
	Ok(PatchOutputMaterialization::NormalWrite)
}

fn write_rendered_output(
	target_path: &str,
	rendered: &str,
	out_dir: &Path,
) -> Result<(), MergeError> {
	let target = out_dir.join(target_path);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::write(target, rendered)?;
	Ok(())
}

fn write_metadata_only(
	out_dir: &Path,
	plan: &MergePlanResult,
	report: &MergeReport,
) -> Result<(), MergeError> {
	fs::create_dir_all(out_dir.join(".foch"))?;
	write_json_artifact(&out_dir.join(MERGE_PLAN_ARTIFACT_PATH), plan)?;
	write_json_artifact(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH), report)?;
	Ok(())
}

fn write_json_artifact<T: Serialize>(path: &Path, value: &T) -> Result<(), MergeError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let bytes = serde_json::to_vec_pretty(value).map_err(|err| {
		MergeError::Io(io::Error::other(format!(
			"failed to serialize {}: {err}",
			path.display()
		)))
	})?;
	fs::write(path, bytes)?;
	Ok(())
}

fn copy_winner_file(
	workspace: &ResolvedWorkspace,
	entry: &MergePlanEntry,
	out_dir: &Path,
) -> Result<(), MergeError> {
	let source = winner_source_path(workspace, entry)?;
	let target = out_dir.join(&entry.path);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::copy(source, target)?;
	Ok(())
}

fn write_conflict_placeholder(entry: &MergePlanEntry, out_dir: &Path) -> Result<(), MergeError> {
	let target = out_dir.join(&entry.path);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	let mut lines = vec![
		"FOCH_MERGE_CONFLICT".to_string(),
		format!("path = {}", entry.path),
	];
	if !entry.notes.is_empty() {
		lines.push(format!("notes = {}", entry.notes.join(" | ")));
	}
	lines.push("contributors =".to_string());
	for contributor in &entry.contributors {
		lines.push(format!(
			"- {} [{}] {}",
			contributor.mod_id, contributor.precedence, contributor.source_path
		));
	}
	lines.push(String::new());
	fs::write(target, lines.join("\n"))?;
	Ok(())
}

fn write_generated_descriptor(
	out_dir: &Path,
	playset_path: &Path,
	playset_name: &str,
	descriptor_path: &Path,
) -> Result<(), MergeError> {
	if let Some(parent) = descriptor_path.parent() {
		fs::create_dir_all(parent)?;
	}
	let normalized_out_dir = normalize_path_string(out_dir);
	let normalized_playset_path = normalize_path_string(playset_path);
	let escaped_name = escape_descriptor_value(&format!("{playset_name} (Merged)"));
	let escaped_path = escape_descriptor_value(&normalized_out_dir);
	let escaped_playset = escape_descriptor_value(&normalized_playset_path);
	let descriptor = format!(
		"# Source playset: {escaped_playset}\nname=\"{escaped_name}\"\npath=\"{escaped_path}\"\n"
	);
	fs::write(descriptor_path, descriptor)?;
	Ok(())
}

fn winner_source_path<'a>(
	workspace: &'a ResolvedWorkspace,
	entry: &MergePlanEntry,
) -> Result<&'a Path, MergeError> {
	let winner = entry
		.winner
		.as_ref()
		.ok_or_else(|| MergeError::Validation {
			path: Some(entry.path.clone()),
			message: format!("merge plan entry {} is missing a winner", entry.path),
		})?;
	let contributors =
		workspace
			.file_inventory
			.get(&entry.path)
			.ok_or_else(|| MergeError::Validation {
				path: Some(entry.path.clone()),
				message: format!(
					"workspace is missing contributor inventory for {}",
					entry.path
				),
			})?;
	find_contributor_path(contributors, winner)
		.map(|path| path.as_path())
		.ok_or_else(|| MergeError::Validation {
			path: Some(entry.path.clone()),
			message: format!(
				"winner source {} is missing from workspace inventory for {}",
				winner.source_path, entry.path
			),
		})
}

fn find_contributor_path<'a>(
	contributors: &'a [ResolvedFileContributor],
	winner: &MergePlanContributor,
) -> Option<&'a PathBuf> {
	contributors
		.iter()
		.find(|contributor| normalized_contributor_path(contributor) == winner.source_path)
		.map(|contributor| &contributor.absolute_path)
}

fn normalized_contributor_path(contributor: &ResolvedFileContributor) -> String {
	normalize_path_string(&contributor.absolute_path)
}

fn normalize_path_string(path: &Path) -> String {
	let raw = path.to_string_lossy();
	let stripped = strip_extended_length_prefix(&raw);
	stripped.replace('\\', "/")
}

/// Strip Windows `\\?\` / `\\?\UNC\` extended-length prefixes (and their
/// forward-slash twins) so the value embedded in a Paradox descriptor is
/// loadable by the launcher and the game.
fn strip_extended_length_prefix(path: &str) -> String {
	if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
		format!(r"\\{rest}")
	} else if let Some(rest) = path.strip_prefix(r"\\?\") {
		rest.to_string()
	} else if let Some(rest) = path.strip_prefix("//?/UNC/") {
		format!("//{rest}")
	} else if let Some(rest) = path.strip_prefix("//?/") {
		rest.to_string()
	} else {
		path.to_string()
	}
}

fn escape_descriptor_value(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn is_text_placeholder_path(path: &str) -> bool {
	let normalized = path.to_ascii_lowercase();
	let Some(ext) = normalized.rsplit('.').next() else {
		return false;
	};
	matches!(
		ext,
		"txt" | "lua" | "yml" | "yaml" | "csv" | "json" | "asset" | "gui" | "gfx" | "mod"
	)
}

/// Run `f`, framing it with `[merge] {name}: start` / `[merge] {name}: done` lines
/// on stderr. The closure can return `(value, Option<summary>)`; the summary, if
/// any, is appended to the `done` line as space-separated `kv=value` pairs.
fn stage_log_with<F, T>(name: &str, f: F) -> T
where
	F: FnOnce() -> (T, Option<String>),
{
	eprintln!("[merge] {name}: start");
	let started = Instant::now();
	let (value, summary) = f();
	let elapsed_ms = started.elapsed().as_millis();
	match summary {
		Some(s) => eprintln!("[merge] {name}: done elapsed_ms={elapsed_ms} {s}"),
		None => eprintln!("[merge] {name}: done elapsed_ms={elapsed_ms}"),
	}
	value
}

/// In-place per-file counter for the materialize loop. On a TTY, refreshes the
/// same line via `\r`; off a TTY, prints a fresh line every `TICK_EVERY` items
/// so piped logs stay readable.
struct MaterializeProgress {
	total: usize,
	current: usize,
	tty: bool,
	last_tick: Instant,
}

impl MaterializeProgress {
	const TICK_EVERY: usize = 200;
	const TICK_INTERVAL_MS: u128 = 200;

	fn new(total: usize) -> Self {
		Self {
			total,
			current: 0,
			tty: io::stderr().is_terminal(),
			last_tick: Instant::now(),
		}
	}

	fn tick(&mut self) {
		self.current += 1;
		let last_ms = self.last_tick.elapsed().as_millis();
		let due_by_count = self.current.is_multiple_of(Self::TICK_EVERY);
		let due_by_time = last_ms >= Self::TICK_INTERVAL_MS;
		if !(due_by_count || due_by_time) {
			return;
		}
		self.last_tick = Instant::now();
		let pct = (self.current * 100).checked_div(self.total).unwrap_or(100);
		let stderr = io::stderr();
		let mut handle = stderr.lock();
		if self.tty {
			let _ = write!(
				handle,
				"\r[merge] materialize: {}/{} files ({pct}%)        ",
				self.current, self.total
			);
		} else {
			let _ = writeln!(
				handle,
				"[merge] materialize: {}/{} files ({pct}%)",
				self.current, self.total
			);
		}
		let _ = handle.flush();
	}

	fn finish(&mut self) {
		if self.tty {
			// Clear the in-place line so the trailing `done` line starts fresh.
			let _ = writeln!(io::stderr());
		}
	}
}

#[cfg(test)]
mod tests {
	use super::{MergeMaterializeOptions, materialize_merge_internal};
	use crate::config::Config;
	use crate::request::CheckRequest;
	use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace};
	use foch_core::config::{ResolutionDecision, ResolutionMap};
	use foch_core::domain::game::Game;
	use foch_core::domain::playlist::Playlist;
	use foch_core::model::{
		HandlerResolutionRecord, MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH,
		MERGED_MOD_DESCRIPTOR_PATH, MergePlanEntry, MergePlanResult, MergeReport,
		MergeReportStatus,
	};
	use foch_language::analyzer::content_family::{ContentFamilyDescriptor, MergeKeySource};
	use foch_language::analyzer::parser::{AstStatement, parse_clausewitz_content};
	use serde_json::json;
	use std::cell::Cell;
	use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
	use std::fs;
	use std::path::{Path, PathBuf};
	use tempfile::TempDir;

	#[test]
	fn stage_log_with_invokes_closure_exactly_once_and_returns_value() {
		let calls = Cell::new(0u32);
		let value = super::stage_log_with("test_stage", || {
			calls.set(calls.get() + 1);
			(42i32, Some("k=v".to_string()))
		});
		assert_eq!(calls.get(), 1, "closure must run exactly once");
		assert_eq!(value, 42, "stage_log_with must return the closure's value");
	}

	fn per_entry_noop_descriptor(opted_in: bool) -> ContentFamilyDescriptor {
		let builder = ContentFamilyDescriptor::prefix("test", "test/")
			.merge_key(MergeKeySource::AssignmentKey);
		if opted_in {
			builder.per_entry_dedup_safe().build()
		} else {
			builder.build()
		}
	}

	fn parse_test_statements(content: &str) -> Vec<AstStatement> {
		let parsed = parse_clausewitz_content(PathBuf::from("test.txt"), content);
		assert!(
			parsed.diagnostics.is_empty(),
			"test content should parse without diagnostics: {:?}",
			parsed.diagnostics
		);
		parsed.ast.statements
	}

	fn assignment_keys(statements: &[AstStatement]) -> Vec<String> {
		statements
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect()
	}

	#[test]
	fn per_entry_noop_drops_entries_equal_to_vanilla_when_opted_in() {
		let descriptor = per_entry_noop_descriptor(true);
		let vanilla = parse_test_statements(
			"same = {\n\tadd_prestige = 1\n}\nchanged = {\n\tadd_legitimacy = 1\n}\n",
		);
		let merged = parse_test_statements(
			"same = {\n\tadd_prestige = 1\n}\nchanged = {\n\tadd_legitimacy = 2\n}\n",
		);

		let (filtered, count) =
			super::drop_per_entry_noop_duplicates(merged, &vanilla, &descriptor);

		assert_eq!(count, 1);
		assert_eq!(assignment_keys(&filtered), vec!["changed".to_string()]);
	}

	#[test]
	fn per_entry_noop_keeps_entries_with_different_value() {
		let descriptor = per_entry_noop_descriptor(true);
		let vanilla = parse_test_statements("same = {\n\tadd_prestige = 1\n}\n");
		let merged = parse_test_statements("same = {\n\tadd_prestige = 2\n}\n");

		let (filtered, count) =
			super::drop_per_entry_noop_duplicates(merged, &vanilla, &descriptor);

		assert_eq!(count, 0);
		assert_eq!(assignment_keys(&filtered), vec!["same".to_string()]);
	}

	#[test]
	fn per_entry_noop_keeps_entries_when_family_not_opted_in() {
		let descriptor = per_entry_noop_descriptor(false);
		let vanilla = parse_test_statements("same = {\n\tadd_prestige = 1\n}\n");
		let merged = parse_test_statements("same = {\n\tadd_prestige = 1\n}\n");

		let (filtered, count) =
			super::drop_per_entry_noop_duplicates(merged, &vanilla, &descriptor);

		assert_eq!(count, 0);
		assert_eq!(assignment_keys(&filtered), vec!["same".to_string()]);
	}

	#[test]
	fn per_entry_noop_keeps_entries_with_no_vanilla_counterpart() {
		let descriptor = per_entry_noop_descriptor(true);
		let vanilla = parse_test_statements("same = {\n\tadd_prestige = 1\n}\n");
		let merged = parse_test_statements("unique = {\n\tadd_legitimacy = 1\n}\n");

		let (filtered, count) =
			super::drop_per_entry_noop_duplicates(merged, &vanilla, &descriptor);

		assert_eq!(count, 0);
		assert_eq!(assignment_keys(&filtered), vec!["unique".to_string()]);
	}

	fn descriptor_path_value(path: &Path) -> String {
		path.to_string_lossy()
			.replace('\\', "/")
			.replace('"', "\\\"")
	}

	fn write_dlc_load(path: &Path, mods: &[(&str, &str)]) {
		let parent = path.parent().expect("playset path has parent");
		fs::create_dir_all(parent.join("mod")).expect("create mod metadata dir");
		let enabled_mods: Vec<String> = mods
			.iter()
			.map(|(steam_id, _)| format!("mod/ugc_{steam_id}.mod"))
			.collect();
		let dlc_load = json!({
			"enabled_mods": enabled_mods,
			"disabled_dlcs": Vec::<String>::new(),
		});
		fs::write(
			path,
			serde_json::to_string_pretty(&dlc_load).expect("serialize dlc_load"),
		)
		.expect("write dlc_load.json");
		for (steam_id, display_name) in mods {
			let mod_root = parent.join(steam_id);
			let body = format!(
				"name=\"{display_name}\"\npath=\"{}\"\nremote_file_id=\"{steam_id}\"\n",
				descriptor_path_value(&mod_root)
			);
			fs::write(parent.join("mod").join(format!("ugc_{steam_id}.mod")), body)
				.expect("write ugc descriptor");
		}
	}

	fn write_descriptor(mod_root: &Path, name: &str) {
		write_descriptor_with_dependencies(mod_root, name, &[]);
	}

	fn write_descriptor_with_dependencies(mod_root: &Path, name: &str, dependencies: &[&str]) {
		fs::create_dir_all(mod_root).expect("create mod root");
		let mut descriptor = format!("name=\"{name}\"\nversion=\"1.0.0\"\n");
		if !dependencies.is_empty() {
			descriptor.push_str("dependencies={\n");
			for dependency in dependencies {
				descriptor.push_str(&format!("\t\"{dependency}\"\n"));
			}
			descriptor.push_str("}\n");
		}
		fs::write(mod_root.join("descriptor.mod"), descriptor).expect("write descriptor");
	}

	fn write_file(mod_root: &Path, relative: &str, content: impl AsRef<[u8]>) {
		let path = mod_root.join(relative);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).expect("create parent");
		}
		fs::write(path, content).expect("write file");
	}

	fn request_for(playlist_path: &Path) -> CheckRequest {
		let game_root = playlist_path
			.parent()
			.expect("playlist parent")
			.join("eu4-game");
		fs::create_dir_all(&game_root).expect("create game root");
		let mut game_path = std::collections::HashMap::new();
		game_path.insert("eu4".to_string(), game_root);
		CheckRequest {
			playset_path: playlist_path.to_path_buf(),
			config: Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		}
	}

	fn no_base_options(force: bool) -> MergeMaterializeOptions {
		MergeMaterializeOptions {
			include_game_base: false,
			force,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_map: foch_core::config::ResolutionMap::default(),
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
		}
	}

	fn read_plan(out_dir: &Path) -> MergePlanResult {
		let bytes =
			fs::read(out_dir.join(MERGE_PLAN_ARTIFACT_PATH)).expect("read merge plan artifact");
		serde_json::from_slice(&bytes).expect("deserialize merge plan artifact")
	}

	fn read_report(out_dir: &Path) -> MergeReport {
		let bytes =
			fs::read(out_dir.join(MERGE_REPORT_ARTIFACT_PATH)).expect("read merge report artifact");
		serde_json::from_slice(&bytes).expect("deserialize merge report artifact")
	}

	fn plan_entry_for<'a>(plan: &'a MergePlanResult, path: &str) -> &'a MergePlanEntry {
		plan.paths
			.iter()
			.find(|entry| entry.path == path)
			.expect("merge plan entry exists")
	}

	fn patch_merge_output(rendered: &str) -> super::PatchBasedMergeOutput {
		super::PatchBasedMergeOutput {
			rendered: rendered.to_string(),
			dep_remove_counts: Vec::new(),
			stale_vanilla_targets: Vec::new(),
			handler_resolutions: Vec::new(),
			external_file_resolutions: HashMap::new(),
			keep_existing_paths: HashSet::new(),
			noop_vs_vanilla: false,
			per_entry_noop_skipped_count: 0,
		}
	}

	fn cross_file_workspace(
		test_root: &Path,
		contributors: &[(&str, &str, &str, usize, bool)],
	) -> ResolvedWorkspace {
		let mut file_inventory = BTreeMap::new();
		for (mod_id, relative_path, content, precedence, is_base_game) in contributors {
			let root = test_root.join(mod_id);
			write_file(&root, relative_path, content);
			file_inventory
				.entry((*relative_path).to_string())
				.or_insert_with(Vec::new)
				.push(ResolvedFileContributor {
					mod_id: (*mod_id).to_string(),
					root_path: root.clone(),
					absolute_path: root.join(relative_path),
					precedence: *precedence,
					is_base_game: *is_base_game,
					is_synthetic_base: false,
					parse_ok_hint: None,
				});
		}

		ResolvedWorkspace {
			playlist_path: test_root.join("playlist.json"),
			playlist: Playlist {
				game: Game::EuropaUniversalis4,
				name: "cross-file-noop".to_string(),
				mods: Vec::new(),
			},
			mods: Vec::new(),
			installed_base_snapshot: None,
			cache_game_version: None,
			mod_snapshots: Vec::new(),
			file_inventory,
		}
	}

	#[test]
	fn mod_priority_boost_reorders_workspace_contributors_by_effective_precedence() {
		let temp = TempDir::new().expect("temp dir");
		let mut workspace = cross_file_workspace(
			temp.path(),
			&[
				("mod_a", "events/test.txt", "a", 1, false),
				("mod_b", "events/test.txt", "b", 2, false),
			],
		);
		let mut boosts = BTreeMap::new();
		boosts.insert("mod_a".to_string(), 100);

		super::apply_mod_priority_boosts(&mut workspace, &boosts);

		let contributors = &workspace.file_inventory["events/test.txt"];
		assert_eq!(contributors[0].mod_id, "mod_b");
		assert_eq!(contributors[0].precedence, 2);
		assert_eq!(contributors[1].mod_id, "mod_a");
		assert_eq!(contributors[1].precedence, 101);
	}

	fn prune_single_generated_path(
		out_dir: &Path,
		workspace: &ResolvedWorkspace,
		generated_path: &str,
	) -> (BTreeSet<String>, MergeReport) {
		let mut generated_paths = BTreeSet::from([generated_path.to_string()]);
		let mut report = MergeReport {
			generated_file_count: 1,
			..MergeReport::default()
		};
		super::prune_cross_file_noop_duplicates(
			out_dir,
			&mut generated_paths,
			workspace,
			foch_language::analyzer::eu4_profile::eu4_profile(),
			&mut report,
		)
		.expect("cross-file noop prune succeeds");
		(generated_paths, report)
	}

	#[test]
	fn cross_file_noop_drops_fully_covered_file() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let vanilla_path = "common/scripted_effects/00_vanilla.txt";
		let generated_path = "common/scripted_effects/zz_generated.txt";
		let content = "shared_effect = {\n\tadd_prestige = 1\n}\n";
		let workspace = cross_file_workspace(
			temp.path(),
			&[
				("base_game", vanilla_path, content, 0, true),
				("mod_a", generated_path, content, 1, false),
			],
		);
		write_file(&out_dir, generated_path, content);

		let (generated_paths, report) =
			prune_single_generated_path(&out_dir, &workspace, generated_path);

		assert!(!out_dir.join(generated_path).exists());
		assert!(generated_paths.is_empty());
		assert_eq!(report.generated_file_count, 0);
		assert_eq!(report.cross_file_noop_skipped_file_count, 1);
		assert_eq!(
			report.handler_resolutions[0].action,
			"cross_file_noop_skipped"
		);
	}

	#[test]
	fn cross_file_noop_keeps_file_when_one_key_unique() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let vanilla_path = "common/scripted_effects/00_vanilla.txt";
		let generated_path = "common/scripted_effects/zz_generated.txt";
		let vanilla = "shared_effect = {\n\tadd_prestige = 1\n}\n";
		let generated = "shared_effect = {\n\tadd_prestige = 1\n}\nunique_effect = {\n\tadd_legitimacy = 1\n}\n";
		let workspace = cross_file_workspace(
			temp.path(),
			&[
				("base_game", vanilla_path, vanilla, 0, true),
				("mod_a", generated_path, generated, 1, false),
			],
		);
		write_file(&out_dir, generated_path, generated);

		let (generated_paths, report) =
			prune_single_generated_path(&out_dir, &workspace, generated_path);

		assert!(out_dir.join(generated_path).exists());
		assert!(generated_paths.contains(generated_path));
		assert_eq!(report.generated_file_count, 1);
		assert_eq!(report.cross_file_noop_skipped_file_count, 0);
	}

	#[test]
	fn cross_file_noop_keeps_file_when_value_differs() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let vanilla_path = "common/scripted_effects/00_vanilla.txt";
		let generated_path = "common/scripted_effects/zz_generated.txt";
		let vanilla = "shared_effect = {\n\tadd_prestige = 1\n}\n";
		let generated = "shared_effect = {\n\tadd_prestige = 2\n}\n";
		let workspace = cross_file_workspace(
			temp.path(),
			&[
				("base_game", vanilla_path, vanilla, 0, true),
				("mod_a", generated_path, generated, 1, false),
			],
		);
		write_file(&out_dir, generated_path, generated);

		let (generated_paths, report) =
			prune_single_generated_path(&out_dir, &workspace, generated_path);

		assert!(out_dir.join(generated_path).exists());
		assert!(generated_paths.contains(generated_path));
		assert_eq!(report.generated_file_count, 1);
		assert_eq!(report.cross_file_noop_skipped_file_count, 0);
	}

	#[test]
	fn cross_file_noop_only_runs_on_opted_in_families() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let vanilla_path = "common/ideas/00_vanilla.txt";
		let generated_path = "common/ideas/zz_generated.txt";
		let content = "idea_group = {\n\tstart = {\n\t\tinfantry_power = 0.1\n\t}\n}\n";
		let workspace = cross_file_workspace(
			temp.path(),
			&[
				("base_game", vanilla_path, content, 0, true),
				("mod_a", generated_path, content, 1, false),
			],
		);
		write_file(&out_dir, generated_path, content);

		let (generated_paths, report) =
			prune_single_generated_path(&out_dir, &workspace, generated_path);

		assert!(out_dir.join(generated_path).exists());
		assert!(generated_paths.contains(generated_path));
		assert_eq!(report.generated_file_count, 1);
		assert_eq!(report.cross_file_noop_skipped_file_count, 0);
	}

	const DAG_CONFLICT_PATH: &str = "common/ideas/conflict.txt";

	fn idea_file(cost: &str) -> String {
		format!("group = {{\n\tidea = {{\n\t\tcost = {cost}\n\t}}\n}}\n")
	}

	#[test]
	fn materialize_keep_existing_skips_write_when_output_exists() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let relative_path = "common/ideas/handler.txt";
		write_file(&out_dir, relative_path, "existing\n");

		let mut merge_output = patch_merge_output("merged\n");
		merge_output
			.keep_existing_paths
			.insert(PathBuf::from(relative_path));
		let mut report = MergeReport::default();

		let materialization = super::write_patch_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			&ResolutionMap::default(),
			&mut report,
		)
		.expect("materialize keep existing");

		assert_eq!(
			materialization,
			super::PatchOutputMaterialization::KeptExisting
		);
		assert_eq!(
			fs::read_to_string(out_dir.join(relative_path)).expect("read output"),
			"existing\n"
		);
		assert!(report.warnings.is_empty());
		assert_eq!(report.handler_resolutions.len(), 1);
		assert_eq!(report.handler_resolutions[0].path, relative_path);
		assert_eq!(report.handler_resolutions[0].action, "kept_existing");
		assert_eq!(report.handler_resolutions[0].source.as_deref(), None);
	}

	#[test]
	fn materialize_file_level_keep_existing_resolution_skips_write_when_output_exists() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let relative_path = "common/ideas/file-level-handler.txt";
		write_file(&out_dir, relative_path, "existing\n");

		let mut merge_output = patch_merge_output("merged\n");
		let mut resolution_map = ResolutionMap::default();
		resolution_map.by_file.insert(
			PathBuf::from(relative_path),
			ResolutionDecision::KeepExisting,
		);
		let mut report = MergeReport::default();

		let materialization = super::write_patch_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			&resolution_map,
			&mut report,
		)
		.expect("materialize file-level keep existing");

		assert_eq!(
			materialization,
			super::PatchOutputMaterialization::KeptExisting
		);
		assert_eq!(
			fs::read_to_string(out_dir.join(relative_path)).expect("read output"),
			"existing\n"
		);
		assert!(
			merge_output
				.keep_existing_paths
				.contains(&PathBuf::from(relative_path))
		);
		assert_eq!(report.handler_resolutions.len(), 1);
		assert_eq!(report.handler_resolutions[0].action, "kept_existing");
	}

	#[test]
	fn materialize_keep_existing_falls_through_when_output_missing() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let relative_path = "common/ideas/missing.txt";
		let mut merge_output = patch_merge_output("merged\n");
		merge_output
			.keep_existing_paths
			.insert(PathBuf::from(relative_path));
		let mut report = MergeReport::default();

		let materialization = super::write_patch_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			&ResolutionMap::default(),
			&mut report,
		)
		.expect("materialize normal write");

		assert_eq!(
			materialization,
			super::PatchOutputMaterialization::NormalWrite
		);
		assert_eq!(
			fs::read_to_string(out_dir.join(relative_path)).expect("read output"),
			"merged\n"
		);
		assert_eq!(report.handler_resolutions.len(), 0);
		assert_eq!(report.warnings.len(), 1);
		assert!(report.warnings[0].contains("keep_existing_failed"));
		assert!(report.warnings[0].contains(relative_path));
	}

	#[test]
	fn materialize_normal_write_records_handler_resolutions() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let relative_path = "common/ideas/dep.txt";
		let mut merge_output = patch_merge_output("merged\n");
		merge_output
			.handler_resolutions
			.push(HandlerResolutionRecord {
				path: relative_path.to_string(),
				action: "dep_implied".to_string(),
				source: Some("mod_a".to_string()),
				rationale: Some("mod mod_a declares dep on mod_b".to_string()),
			});
		let mut report = MergeReport::default();

		let materialization = super::write_patch_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			&ResolutionMap::default(),
			&mut report,
		)
		.expect("materialize normal write");

		assert_eq!(
			materialization,
			super::PatchOutputMaterialization::NormalWrite
		);
		assert_eq!(
			fs::read_to_string(out_dir.join(relative_path)).expect("read output"),
			"merged\n"
		);
		assert_eq!(report.handler_resolutions.len(), 1);
		assert_eq!(report.handler_resolutions[0].action, "dep_implied");
		assert_eq!(
			report.handler_resolutions[0].rationale.as_deref(),
			Some("mod mod_a declares dep on mod_b")
		);
	}

	#[test]
	fn materialize_external_file_writes_external_content() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let external_path = temp.path().join("external.txt");
		let relative_path = "common/ideas/external.txt";
		fs::write(&external_path, "external\n").expect("write external source");

		let mut merge_output = patch_merge_output("merged\n");
		merge_output
			.external_file_resolutions
			.insert(PathBuf::from(relative_path), external_path.clone());
		let mut report = MergeReport::default();

		let materialization = super::write_patch_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			&ResolutionMap::default(),
			&mut report,
		)
		.expect("materialize external file");

		assert_eq!(
			materialization,
			super::PatchOutputMaterialization::ExternalWrite
		);
		assert_eq!(
			fs::read_to_string(out_dir.join(relative_path)).expect("read output"),
			"external\n"
		);
		assert!(report.warnings.is_empty());
		assert_eq!(report.handler_resolutions.len(), 1);
		assert_eq!(report.handler_resolutions[0].path, relative_path);
		assert_eq!(report.handler_resolutions[0].action, "external");
		let external_source = external_path.display().to_string();
		assert_eq!(
			report.handler_resolutions[0].source.as_deref(),
			Some(external_source.as_str())
		);
	}

	#[test]
	fn materialize_external_file_errors_when_external_missing() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let external_path = temp.path().join("missing-external.txt");
		let relative_path = "common/ideas/missing-external.txt";
		let mut merge_output = patch_merge_output("merged\n");
		merge_output
			.external_file_resolutions
			.insert(PathBuf::from(relative_path), external_path.clone());
		let mut report = MergeReport::default();

		let err = super::write_patch_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			&ResolutionMap::default(),
			&mut report,
		)
		.expect_err("missing external source should error");

		assert!(
			err.to_string()
				.contains("failed to read external resolution source")
		);
		assert!(!out_dir.join(relative_path).exists());
		assert!(report.handler_resolutions.is_empty());
	}

	fn stage_dag_downstream_conflict(
		playlist_path: &Path,
		mod_base: &Path,
		mod_a: &Path,
		mod_b: &Path,
		mod_c: &Path,
	) {
		write_dlc_load(
			playlist_path,
			&[
				("9101", "Base"),
				("9102", "A"),
				("9103", "B"),
				("9104", "C"),
			],
		);
		write_descriptor(mod_base, "conflict-base");
		write_descriptor_with_dependencies(mod_a, "conflict-a", &["conflict-base"]);
		write_descriptor_with_dependencies(mod_b, "conflict-b", &["conflict-base"]);
		write_descriptor_with_dependencies(mod_c, "conflict-c", &["conflict-a", "conflict-b"]);
		write_file(mod_base, DAG_CONFLICT_PATH, idea_file("old"));
		write_file(mod_a, DAG_CONFLICT_PATH, idea_file("alpha"));
		write_file(mod_b, DAG_CONFLICT_PATH, idea_file("beta"));
		write_file(mod_c, DAG_CONFLICT_PATH, idea_file("gamma"));
	}

	/// Same as `stage_dag_downstream_conflict` but without the downstream resolver
	/// mod C. Yields a genuine sibling-overwrite conflict between mods A and B
	/// that the DAG topo walk cannot auto-resolve.
	fn stage_dag_genuine_conflict(
		playlist_path: &Path,
		mod_base: &Path,
		mod_a: &Path,
		mod_b: &Path,
	) {
		write_dlc_load(
			playlist_path,
			&[("9101", "Base"), ("9102", "A"), ("9103", "B")],
		);
		write_descriptor(mod_base, "conflict-base");
		write_descriptor_with_dependencies(mod_a, "conflict-a", &["conflict-base"]);
		write_descriptor_with_dependencies(mod_b, "conflict-b", &["conflict-base"]);
		write_file(mod_base, DAG_CONFLICT_PATH, idea_file("old"));
		write_file(mod_a, DAG_CONFLICT_PATH, idea_file("alpha"));
		write_file(mod_b, DAG_CONFLICT_PATH, idea_file("beta"));
	}

	#[test]
	fn copy_through_materialization_writes_descriptor_sidecars_and_source_file() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_root = temp.path().join("1001");
		let out_dir = temp.path().join("out");

		write_dlc_load(&playlist_path, &[("1001", "A")]);
		write_descriptor(&mod_root, "mod-a");
		write_file(&mod_root, "common/only.txt", "from-a\n");

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");
		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.manual_conflict_count, 0);
		assert_eq!(report.generated_file_count, 0);
		assert_eq!(report.copied_file_count, 1);
		assert_eq!(report.overlay_file_count, 0);

		let descriptor =
			fs::read_to_string(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH)).expect("read descriptor");
		assert!(descriptor.contains("name=\"playlist (active) (Merged)\""));
		assert!(descriptor.contains("# Source playset: "));
		assert!(!descriptor.contains("remote_file_id"));
		assert!(!descriptor.contains("supported_version"));
		assert_eq!(
			fs::read_to_string(out_dir.join("common/only.txt")).expect("read copied file"),
			"from-a\n"
		);

		let plan = read_plan(&out_dir);
		assert!(!plan_entry_for(&plan, "common/only.txt").generated);
		let persisted_report = read_report(&out_dir);
		assert_eq!(persisted_report.status, report.status);
		assert_eq!(persisted_report.copied_file_count, 1);
	}

	#[test]
	fn overlay_materialization_copies_only_the_highest_precedence_file() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("2001");
		let mod_b = temp.path().join("2002");
		let out_dir = temp.path().join("out");

		write_dlc_load(&playlist_path, &[("2001", "A"), ("2002", "B")]);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_file(&mod_a, "common/overlay.txt", "from-a\n");
		write_file(&mod_b, "common/overlay.txt", "from-b\n");

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");
		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.overlay_file_count, 1);
		assert_eq!(report.copied_file_count, 0);
		assert_eq!(report.generated_file_count, 0);
		assert_eq!(
			fs::read_to_string(out_dir.join("common/overlay.txt")).expect("read overlay output"),
			"from-b\n"
		);
	}

	#[test]
	fn binary_overlap_resolved_by_last_writer_overlay() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("4001");
		let mod_b = temp.path().join("4002");
		let out_dir = temp.path().join("out");

		write_dlc_load(&playlist_path, &[("4001", "A"), ("4002", "B")]);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		// Binary overlap → LastWriterOverlay (highest-precedence wins, mirroring
		// the game's runtime load order)
		write_file(&mod_a, "pdx_browser/overlap.bin", [0u8, 1, 2, 3]);
		write_file(&mod_b, "pdx_browser/overlap.bin", [4u8, 5, 6, 7]);

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");
		assert_eq!(report.manual_conflict_count, 0);
		assert_eq!(report.overlay_file_count, 1);
		assert!(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
		// Last-writer wins: mod B's bytes
		let copied = fs::read(out_dir.join("pdx_browser/overlap.bin")).expect("read overlay");
		assert_eq!(copied, vec![4u8, 5, 6, 7]);
		assert!(out_dir.join(MERGE_PLAN_ARTIFACT_PATH).exists());
		assert!(out_dir.join(MERGE_REPORT_ARTIFACT_PATH).exists());

		let plan = read_plan(&out_dir);
		let entry = plan_entry_for(&plan, "pdx_browser/overlap.bin");
		assert!(entry.winner.is_some());
	}

	#[test]
	fn unresolved_structural_merge_skips_by_default() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let out_dir = temp.path().join("out");
		stage_dag_genuine_conflict(
			&playlist_path,
			&temp.path().join("9101"),
			&temp.path().join("9102"),
			&temp.path().join("9103"),
		);

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");

		assert_eq!(report.status, MergeReportStatus::Blocked);
		assert_eq!(report.manual_conflict_count, 1);
		assert!(!out_dir.join(DAG_CONFLICT_PATH).exists());
		assert_eq!(report.conflict_resolutions.len(), 1);
		let resolution = &report.conflict_resolutions[0];
		assert!(resolution.reason.contains("unresolved conflict"));
		assert_eq!(resolution.leaf_conflicts.len(), 1);
		assert_eq!(resolution.leaf_conflicts[0].address_key, "group");
	}

	#[test]
	fn force_mode_writes_manual_marker_for_unresolved_structural_merge() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let out_dir = temp.path().join("out");
		stage_dag_genuine_conflict(
			&playlist_path,
			&temp.path().join("9101"),
			&temp.path().join("9102"),
			&temp.path().join("9103"),
		);

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(true),
		)
		.expect("materialize");

		assert_eq!(report.status, MergeReportStatus::PartialSuccess);
		assert_eq!(report.manual_conflict_count, 1);
		assert_eq!(report.generated_file_count, 1);
		let marker = fs::read_to_string(out_dir.join(DAG_CONFLICT_PATH)).expect("read marker");
		assert!(marker.starts_with("FOCH_MERGE_CONFLICT"));
		assert!(marker.contains("unresolved conflict"));
	}

	#[test]
	fn downstream_mod_resolves_upstream_sibling_conflict() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let out_dir = temp.path().join("out");
		// Mod C declares deps on both A and B and writes its own value at the
		// same address. The DAG topo walk should recognize C as a downstream
		// override of the A/B sibling-overwrite conflict and emit C's value
		// without invoking a manual marker.
		stage_dag_downstream_conflict(
			&playlist_path,
			&temp.path().join("9101"),
			&temp.path().join("9102"),
			&temp.path().join("9103"),
			&temp.path().join("9104"),
		);

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");

		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.manual_conflict_count, 0);
		assert_eq!(report.generated_file_count, 1);
		let output =
			fs::read_to_string(out_dir.join(DAG_CONFLICT_PATH)).expect("read merged output");
		// C's value wins via downstream override, no foch:conflict marker.
		assert!(
			output.contains("cost = gamma"),
			"expected mod C's gamma value to win, got:\n{output}"
		);
		assert!(!output.contains("# foch:conflict"));
		// One downstream-override resolution should be recorded.
		let downstream = report
			.handler_resolutions
			.iter()
			.find(|r| r.action == "downstream_override");
		assert!(
			downstream.is_some(),
			"expected downstream_override handler resolution, got {:?}",
			report.handler_resolutions
		);
	}

	#[test]
	fn force_mode_with_only_safe_overlaps_succeeds() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("5001");
		let mod_b = temp.path().join("5002");
		let out_dir = temp.path().join("out");

		write_dlc_load(&playlist_path, &[("5001", "A"), ("5002", "B")]);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		// Binary overlaps now resolve cleanly via LastWriterOverlay → no manual
		// conflicts left for --force to handle.
		write_file(&mod_a, "pdx_browser/overlap.bin", [0u8, 1, 2, 3]);
		write_file(&mod_b, "pdx_browser/overlap.bin", [4u8, 5, 6, 7]);
		write_file(&mod_a, "pdx_browser/icon.png", [8u8, 9, 10]);
		write_file(&mod_b, "pdx_browser/icon.png", [11u8, 12, 13]);
		write_file(&mod_b, "common/safe.txt", "safe\n");

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(true),
		)
		.expect("materialize");
		assert_eq!(report.manual_conflict_count, 0);
		assert_eq!(report.overlay_file_count, 2);
		assert!(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
		assert_eq!(
			fs::read_to_string(out_dir.join("common/safe.txt")).expect("read copied safe file"),
			"safe\n"
		);
		assert!(out_dir.join("pdx_browser/overlap.bin").exists());
		assert!(out_dir.join("pdx_browser/icon.png").exists());
	}
}
