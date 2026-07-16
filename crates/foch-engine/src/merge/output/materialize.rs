#![allow(dead_code)]

mod cross_file_dedup;
mod io;
mod output_transaction;
mod patch_structural;
mod per_entry_noop;
mod stale_detect;

use super::super::conflict_handler::ConflictHandler;
use super::super::dag::{
	DagDiagnostic, DagDiagnosticKind, IgnoreReplacePath, ModDag, ModId, build_mod_dag,
};
use super::super::error::MergeError;
#[allow(unused_imports)]
use super::super::namespace::{
	FamilyKeyIndex, build_family_key_index, detect_key_conflicts, group_by_family,
};
use super::super::plan::build_merge_plan_from_workspace;
use super::super::planning::module_view::build_cross_file_module_views;
use super::localisation_merge::{LocalisationMergeOutcome, merge_localisation_file};
use crate::emit::EmitOptions;
use crate::merge::patch::ast_statement_list_has_real_content;
use crate::request::{CheckRequest, MergePlanOptions};
use crate::workspace::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveError, WorkspaceScriptCache,
	resolve_workspace,
};
use cross_file_dedup::prune_cross_file_noop_duplicates;
use foch_core::config::{AppliedDepOverride, DepOverride, FochConfig, ResolutionMap};
use foch_core::model::{
	CheckContext, ConflictKind, DepMisuseFinding, HandlerResolutionRecord, LeafConflictDetail,
	MERGED_MOD_DESCRIPTOR_PATH, MergePlanEntry, MergePlanResult, MergePlanStrategy,
	MergePlanTarget, MergeReport, MergeReportConflictResolution, MergeReportStatus,
	MergeTraceEntry, SemanticIndex, StaleVanillaTargetDescriptor,
};
use foch_cwt::CwtSchemaGraph;
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, ContentLoadPolicy, GameProfile, MergeKeySource,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::parse_clausewitz_file;
use foch_language::analyzer::rules::{detect_dependency_misuse, detect_version_mismatch};
#[cfg(test)]
use io::PatchOutputMaterialization;
use io::{
	copy_winner_file, is_text_placeholder_path, write_clean_metadata_only,
	write_conflict_placeholder, write_generated_descriptor, write_metadata_only,
	write_patch_merge_output,
};
pub(crate) use output_transaction::OutputTransaction;
use patch_structural::{patch_based_cross_file_module_merge, patch_based_structural_merge};
use stale_detect::apply_dep_misuse_remove_counts;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::IsTerminal;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub(crate) struct MergeMaterializeOptions {
	pub include_game_base: bool,
	pub include_base: bool,
	pub gui_scroll_merge: bool,
	pub force: bool,
	pub ignore_replace_path: bool,
	pub dep_overrides: Vec<AppliedDepOverride>,
	pub resolution_map: foch_core::config::ResolutionMap,
	pub interactive_conflict_handler: Option<Box<dyn ConflictHandler>>,
	pub interactive_resolution_config_path: Option<PathBuf>,
	/// When set, annotate merged definitions with their adopted source mods
	/// (inline `# foch: …` comments + `.foch/foch-provenance.json`).
	pub provenance: bool,
	/// Optional relative-path retention set for callers that only need a subset
	/// of copy-through output.
	pub retained_paths: Option<BTreeSet<String>>,
}

impl Default for MergeMaterializeOptions {
	fn default() -> Self {
		Self {
			include_game_base: true,
			include_base: false,
			gui_scroll_merge: false,
			force: false,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_map: foch_core::config::ResolutionMap::default(),
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			provenance: false,
			retained_paths: None,
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

fn prune_noop_script_contributors(workspace: &mut ResolvedWorkspace, profile: &dyn GameProfile) {
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
			let is_structural_script = descriptor
				.and_then(|descriptor| descriptor.merge_key_source)
				.is_some();
			if !is_structural_script {
				return true;
			}
			contributors.retain(|contributor| {
				contributor.is_base_game
					|| contributor.is_synthetic_base
					|| !is_noop_script_contributor(contributor)
			});
			!contributors.is_empty()
		});
}

fn is_noop_script_contributor(contributor: &ResolvedFileContributor) -> bool {
	let parsed = parse_clausewitz_file(&contributor.absolute_path);
	parsed.diagnostics.is_empty() && !ast_statement_list_has_real_content(&parsed.ast.statements)
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
	options: MergeMaterializeOptions,
) -> Result<MergeReport, MergeError> {
	// Resolve once and reuse: build_merge_plan_from_workspace and the rest of
	// the pipeline both consume the same ResolvedWorkspace. The legacy
	// run_merge_plan_with_options recovery path is kept for the case where
	// resolution itself failed (it may still produce a fatal-only plan).
	let workspace_result = stage_log_with("resolve_workspace", || {
		let result = resolve_workspace(&request, options.include_game_base);
		let summary = result
			.as_ref()
			.ok()
			.map(|w| format!("mods={} files={}", w.mods.len(), w.file_inventory.len()));
		(result, summary)
	});
	let transaction = OutputTransaction::begin(out_dir)?;
	let staging_dir = transaction.staging_dir().to_path_buf();
	let prior_out_dir = transaction.prior_dir().map(Path::to_path_buf);
	let report = materialize_merge_with_workspace_result(
		request,
		&staging_dir,
		prior_out_dir.as_deref(),
		out_dir,
		options,
		workspace_result,
	)?;
	transaction.publish()?;
	Ok(report)
}

pub(crate) fn materialize_merge_with_workspace_result(
	request: CheckRequest,
	out_dir: &Path,
	prior_out_dir: Option<&Path>,
	published_out_dir: &Path,
	mut options: MergeMaterializeOptions,
	mut workspace_result: Result<ResolvedWorkspace, WorkspaceResolveError>,
) -> Result<MergeReport, MergeError> {
	let mut report = MergeReport::default();
	let mut generated_paths = BTreeSet::new();

	if let Ok(workspace) = &mut workspace_result {
		// The merge resolution map is loaded after generic workspace resolution,
		// so priority_boost is a merge-only post-processing pass here.
		apply_mod_priority_boosts(workspace, &options.resolution_map.mod_priority_boost);
	}
	let profile = eu4_profile();
	if let Ok(workspace) = &mut workspace_result {
		prune_noop_script_contributors(workspace, profile);
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
	report.definition_module_count = plan
		.paths
		.iter()
		.filter(|entry| matches!(&entry.target, MergePlanTarget::Module { .. }))
		.count();
	report.definition_module_blocked_count = plan
		.paths
		.iter()
		.filter(|entry| {
			matches!(&entry.target, MergePlanTarget::Module { .. })
				&& entry.strategy == MergePlanStrategy::ManualConflict
		})
		.count();

	if plan.has_fatal_errors() {
		report.status = MergeReportStatus::Fatal;
		// Surface *why* resolution failed (e.g. missing/stale base data with the
		// `foch data install` hint) so a fatal merge isn't an opaque `status:
		// FATAL`, mirroring `foch check`. Only the Err path produces a fatal
		// plan, so this stays `None` on success and the report is unchanged.
		report.fatal_reason = workspace_result
			.as_ref()
			.err()
			.map(|err| err.message.clone());
		write_clean_metadata_only(out_dir, &plan, &report)?;
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
		write_clean_metadata_only(out_dir, &plan, &report)?;
		return Ok(report);
	}

	fs::create_dir_all(out_dir)?;
	let descriptor_root = descriptor_output_root(published_out_dir)?;

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
	let mut pending_copy_through = Vec::new();
	let mut published_module_replacements = BTreeSet::new();

	for entry in &plan.paths {
		materialize_progress.tick();
		match entry.strategy {
			MergePlanStrategy::CopyThrough => {
				materialize_copy_through(
					&workspace,
					entry,
					out_dir,
					options.include_base,
					&mut report,
					options.retained_paths.as_ref(),
					&mut pending_copy_through,
				)?;
			}
			MergePlanStrategy::LastWriterOverlay => {
				copy_winner_file(&workspace, entry, out_dir)?;
				report.overlay_file_count += 1;
			}
			MergePlanStrategy::LocalisationMerge => {
				let contributors = workspace.file_inventory.get(entry.output_path());
				match contributors {
					Some(contributors) => {
						match merge_localisation_file(entry.output_path(), contributors) {
							Ok(LocalisationMergeOutcome::Merged(bytes)) => {
								let target = out_dir.join(entry.output_path());
								if let Some(parent) = target.parent() {
									fs::create_dir_all(parent)?;
								}
								fs::write(target, bytes)?;
								generated_paths.insert(entry.output_path().to_string());
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
									entry.output_path()
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
				if matches!(&entry.target, MergePlanTarget::Module { .. }) {
					let module_started = Instant::now();
					let conflicts_before = report.manual_conflict_count;
					materialize_cross_file_module(CrossFileModuleMaterializeContext {
						workspace: &workspace,
						entry,
						out_dir,
						prior_out_dir,
						options: &mut options,
						report: &mut report,
						generated_paths: &mut generated_paths,
						profile,
						mod_dag: &mod_dag,
						ignore_replace_path: &ignore_replace_path,
						dep_overrides: &dep_overrides,
						mod_versions: &mod_versions,
						mod_display_names: &mod_display_names,
						cache_game_version: &cache_game_version,
						emit_options: &emit_options,
					})?;
					report.definition_module_elapsed_ms = report
						.definition_module_elapsed_ms
						.saturating_add(module_started.elapsed().as_millis() as u64);
					if generated_paths.contains(entry.output_path()) {
						if let Some(prefix) = entry.target.replace_prefix() {
							published_module_replacements.insert(prefix.to_string());
						}
						report.definition_module_generated_count += 1;
					} else if report.manual_conflict_count > conflicts_before {
						report.definition_module_blocked_count += 1;
					}
					continue;
				}
				let contributors = workspace.file_inventory.get(entry.output_path());
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
						let descriptor =
							profile.classify_content_family(Path::new(entry.output_path()));
						let merge_key_source = descriptor.and_then(|d| d.merge_key_source);

						if let (Some(descriptor), Some(merge_key_source)) =
							(descriptor, merge_key_source)
						{
							let target = entry.output_path().to_string();
							let contribs = contributors.clone();
							let desc = descriptor.clone();
							let cwt_schema_graph =
								crate::merge::cwt_suggestions::cwt_schema_graph_for_profile(
									profile,
								);
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
										cwt_schema_graph: cwt_schema_graph.clone(),
										merge_key_source,
										gui_scroll_merge: options.gui_scroll_merge,
										mod_dag: &dag,
										ignore_replace_path: &ignore,
										dep_overrides: &dep_overrides,
										dep_misuse_findings: &dep_misuse,
										resolution_map: &resolution_map,
										mod_versions: &mod_versions,
										mod_display_names: &mod_display_names,
										cache_game_version: &cache_game_version,
										emit_options: &emit_options,
										provenance: options.provenance,
										script_cache: &workspace.script_cache,
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
										entry.output_path(),
										&mut merge_output,
										out_dir,
										prior_out_dir,
										&options.resolution_map,
										&mut report,
									)?;
									if materialization.uses_patch_merge_rendered_output() {
										report.per_entry_noop_skipped_count +=
											merge_output.per_entry_noop_skipped_count;
									}
									if materialization.counts_as_generated() {
										generated_paths.insert(entry.output_path().to_string());
										report.generated_file_count += 1;
										if options.provenance {
											let trace =
												std::mem::take(&mut merge_output.merge_trace);
											if !trace.is_empty() {
												report
													.merge_trace
													.insert(entry.output_path().to_string(), trace);
											}
											let prov = std::mem::take(
												&mut merge_output.definition_provenance,
											);
											if !prov.is_empty() {
												report
													.definition_provenance
													.insert(entry.output_path().to_string(), prov);
											}
										}
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
					generated_paths.insert(entry.output_path().to_string());
					report.generated_file_count += 1;
				} else {
					// No base available at all (neither vanilla nor synthetic);
					// fall back to last-writer copy.
					copy_winner_file(&workspace, entry, out_dir)?;
					generated_paths.insert(entry.output_path().to_string());
					report.generated_file_count += 1;
				}
			}
			MergePlanStrategy::ManualConflict => {
				if matches!(&entry.target, MergePlanTarget::Module { .. }) {
					discard_module_output(entry, out_dir, &mut generated_paths)?;
					let reason = if entry.notes.is_empty() {
						"definition module requires manual resolution".to_string()
					} else {
						entry.notes.join("; ")
					};
					report.warnings.push(format!(
						"{} for {}; skipped complete module output even with --force",
						reason,
						entry.output_path()
					));
					report
						.conflict_resolutions
						.push(plan_conflict_skipped_resolution(entry, &reason));
					continue;
				}
				if options.force {
					if is_text_placeholder_path(entry.output_path()) {
						write_conflict_placeholder(entry, out_dir)?;
						generated_paths.insert(entry.output_path().to_string());
						report.generated_file_count += 1;
					} else if let Some(contributors) =
						workspace.file_inventory.get(entry.output_path())
					{
						// Binary conflict: copy highest-precedence (last) mod's version
						if let Some(best) = contributors
							.iter()
							.filter(|c| !c.is_base_game)
							.max_by_key(|c| c.precedence)
						{
							let target = out_dir.join(entry.output_path());
							if let Some(parent) = target.parent() {
								fs::create_dir_all(parent)?;
							}
							fs::copy(&best.absolute_path, target)?;
							generated_paths.insert(entry.output_path().to_string());
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
	flush_pending_copy_through(&workspace, out_dir, &pending_copy_through)?;
	let mod_diff_cache_stats = crate::cache::mod_diff_cache_stats();
	let dag_base_cache_stats = crate::cache::dag_base_cache_stats();
	eprintln!(
		"[merge] materialize: done elapsed_ms={} generated={} copied={} overlay={} definition_modules={} definition_modules_generated={} definition_modules_blocked={} definition_module_elapsed_ms={} base_passthrough_skipped={} noop_skipped={} cross_file_noop_skipped={} per_entry_noop_skipped={} mod_diff_cache_hits={} mod_diff_cache_misses={} dag_base_cache_hits={} dag_base_cache_misses={}",
		materialize_started.elapsed().as_millis(),
		report.generated_file_count,
		report.copied_file_count,
		report.overlay_file_count,
		report.definition_module_count,
		report.definition_module_generated_count,
		report.definition_module_blocked_count,
		report.definition_module_elapsed_ms,
		report.base_passthrough_skipped_file_count,
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

	let mut persisted_plan = plan.clone();
	report.status = if report.manual_conflict_count > 0 && !options.force {
		MergeReportStatus::Blocked
	} else if report.manual_conflict_count > 0 {
		MergeReportStatus::PartialSuccess
	} else {
		MergeReportStatus::Ready
	};
	if report.status == MergeReportStatus::Blocked {
		write_clean_metadata_only(out_dir, &persisted_plan, &report)?;
		return Ok(report);
	}

	write_generated_descriptor(
		&descriptor_root,
		request.source_path(),
		&plan.playset_name,
		&published_module_replacements,
		&out_dir.join(MERGED_MOD_DESCRIPTOR_PATH),
	)?;
	for entry in &mut persisted_plan.paths {
		entry.generated = generated_paths.contains(entry.output_path());
	}
	write_metadata_only(out_dir, &persisted_plan, &report)?;
	Ok(report)
}

fn descriptor_output_root(published_out_dir: &Path) -> Result<PathBuf, MergeError> {
	if published_out_dir.is_absolute() {
		return Ok(published_out_dir.to_path_buf());
	}
	Ok(std::env::current_dir()?.join(published_out_dir))
}

struct CrossFileModuleMaterializeContext<'a> {
	workspace: &'a ResolvedWorkspace,
	entry: &'a MergePlanEntry,
	out_dir: &'a Path,
	prior_out_dir: Option<&'a Path>,
	options: &'a mut MergeMaterializeOptions,
	report: &'a mut MergeReport,
	generated_paths: &'a mut BTreeSet<String>,
	profile: &'a dyn GameProfile,
	mod_dag: &'a ModDag,
	ignore_replace_path: &'a IgnoreReplacePath,
	dep_overrides: &'a [DepOverride],
	mod_versions: &'a HashMap<String, String>,
	mod_display_names: &'a HashMap<String, String>,
	cache_game_version: &'a str,
	emit_options: &'a EmitOptions,
}

fn materialize_cross_file_module(
	context: CrossFileModuleMaterializeContext<'_>,
) -> Result<(), MergeError> {
	let CrossFileModuleMaterializeContext {
		workspace,
		entry,
		out_dir,
		prior_out_dir,
		options,
		report,
		generated_paths,
		profile,
		mod_dag,
		ignore_replace_path,
		dep_overrides,
		mod_versions,
		mod_display_names,
		cache_game_version,
		emit_options,
	} = context;
	let Some(descriptor) = profile.classify_content_family(Path::new(entry.output_path())) else {
		return resolve_cross_file_module_failure(
			entry,
			out_dir,
			options,
			report,
			generated_paths,
			format!(
				"missing content-family descriptor for {}",
				entry.output_path()
			),
		);
	};
	let Some(merge_key_source) = descriptor.merge_key_source else {
		return resolve_cross_file_module_failure(
			entry,
			out_dir,
			options,
			report,
			generated_paths,
			format!("missing merge-key policy for {}", entry.output_path()),
		);
	};
	let views = match build_cross_file_module_views(
		entry,
		workspace,
		descriptor,
		mod_dag,
		ignore_replace_path,
		dep_overrides,
	) {
		Ok(views) => views,
		Err(reason) => {
			return resolve_cross_file_module_failure(
				entry,
				out_dir,
				options,
				report,
				generated_paths,
				reason,
			);
		}
	};
	let cwt_schema_graph = crate::merge::cwt_suggestions::cwt_schema_graph_for_profile(profile);
	let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		let patch_context = PatchBasedMergeContext {
			descriptor,
			cwt_schema_graph,
			merge_key_source,
			gui_scroll_merge: options.gui_scroll_merge,
			mod_dag,
			ignore_replace_path,
			dep_overrides,
			dep_misuse_findings: &report.dep_misuse,
			resolution_map: &options.resolution_map,
			mod_versions,
			mod_display_names,
			cache_game_version,
			emit_options,
			provenance: options.provenance,
			script_cache: &workspace.script_cache,
		};
		patch_based_cross_file_module_merge(
			entry.output_path(),
			&views,
			patch_context,
			options.interactive_conflict_handler.as_deref_mut(),
			options.interactive_resolution_config_path.as_deref(),
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
			// A namespace replacement cannot skip output merely because it matches
			// vanilla: its descriptor will hide the original prefix. An overlay
			// module can safely remain absent when it is a semantic no-op.
			if entry.target.replace_prefix().is_some() {
				merge_output.noop_vs_vanilla = false;
			}
			let stage_dir = prepare_module_stage_dir(out_dir, entry.output_path())?;
			let materialization = match write_patch_merge_output(
				entry.output_path(),
				&mut merge_output,
				&stage_dir,
				prior_out_dir,
				&options.resolution_map,
				report,
			) {
				Ok(materialization) => materialization,
				Err(error) => {
					let _ = fs::remove_dir_all(&stage_dir);
					return Err(error);
				}
			};
			if materialization.uses_patch_merge_rendered_output() {
				report.per_entry_noop_skipped_count += merge_output.per_entry_noop_skipped_count;
			}
			if materialization.publishes_output() {
				publish_staged_module_output(&stage_dir, out_dir, entry.output_path())?;
				generated_paths.insert(entry.output_path().to_string());
				if materialization.counts_as_generated() {
					report.generated_file_count += 1;
				}
				if materialization.counts_as_generated() && options.provenance {
					let trace = std::mem::take(&mut merge_output.merge_trace);
					if !trace.is_empty() {
						report
							.merge_trace
							.insert(entry.output_path().to_string(), trace);
					}
					let provenance = std::mem::take(&mut merge_output.definition_provenance);
					if !provenance.is_empty() {
						report
							.definition_provenance
							.insert(entry.output_path().to_string(), provenance);
					}
				}
			} else if materialization.counts_as_noop_skipped()
				&& entry.target.replace_prefix().is_none()
			{
				report.noop_skipped_file_count += 1;
			} else {
				let _ = fs::remove_dir_all(&stage_dir);
				return resolve_cross_file_module_failure(
					entry,
					out_dir,
					options,
					report,
					generated_paths,
					"definition module did not produce its required staged output".to_string(),
				);
			}
			let _ = fs::remove_dir_all(&stage_dir);
			Ok(())
		}
		Ok(Err(PatchBasedMergeFailure::Unresolved(conflict))) => {
			resolve_cross_file_module_conflict(
				entry,
				out_dir,
				options,
				report,
				generated_paths,
				conflict,
			)?;
			Ok(())
		}
		Ok(Err(PatchBasedMergeFailure::Merge(error))) => resolve_cross_file_module_failure(
			entry,
			out_dir,
			options,
			report,
			generated_paths,
			format!("cross-file module merge failed: {error}"),
		),
		Err(_) => resolve_cross_file_module_failure(
			entry,
			out_dir,
			options,
			report,
			generated_paths,
			"cross-file module merge panicked".to_string(),
		),
	}
}

fn prepare_module_stage_dir(out_dir: &Path, output_path: &str) -> Result<PathBuf, MergeError> {
	let digest = blake3::hash(output_path.as_bytes()).to_hex();
	let stage_dir = out_dir
		.join(".foch")
		.join(format!("module-stage-{}", &digest[..16]));
	if stage_dir.exists() {
		fs::remove_dir_all(&stage_dir)?;
	}
	fs::create_dir_all(&stage_dir)?;
	Ok(stage_dir)
}

fn publish_staged_module_output(
	stage_dir: &Path,
	out_dir: &Path,
	output_path: &str,
) -> Result<(), MergeError> {
	let staged = stage_dir.join(output_path);
	if !staged.is_file() {
		return Err(MergeError::Validation {
			path: Some(output_path.to_string()),
			message: "definition module staging completed without an output file".to_string(),
		});
	}
	let target = out_dir.join(output_path);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	match fs::rename(&staged, &target) {
		Ok(()) => Ok(()),
		Err(first_error) if target.is_file() => {
			fs::remove_file(&target)?;
			fs::rename(&staged, &target).map_err(|second_error| {
				MergeError::Io(std::io::Error::new(
					second_error.kind(),
					format!(
						"failed to publish staged definition module after replacing {}: first rename: {first_error}; second rename: {second_error}",
						target.display()
					),
				))
			})
		}
		Err(error) => Err(MergeError::Io(error)),
	}
}

fn resolve_cross_file_module_failure(
	entry: &MergePlanEntry,
	out_dir: &Path,
	options: &MergeMaterializeOptions,
	report: &mut MergeReport,
	generated_paths: &mut BTreeSet<String>,
	reason: String,
) -> Result<(), MergeError> {
	resolve_cross_file_module_conflict(
		entry,
		out_dir,
		options,
		report,
		generated_paths,
		PatchConflictReport::without_details(reason),
	)
}

fn resolve_cross_file_module_conflict(
	entry: &MergePlanEntry,
	out_dir: &Path,
	options: &MergeMaterializeOptions,
	report: &mut MergeReport,
	generated_paths: &mut BTreeSet<String>,
	conflict: PatchConflictReport,
) -> Result<(), MergeError> {
	discard_module_output(entry, out_dir, generated_paths)?;
	let reason = conflict.reason;
	report.manual_conflict_count += 1;
	report
		.handler_resolutions
		.extend(conflict.handler_resolutions);
	let force_note = if options.force {
		"; --force cannot publish a malformed complete module"
	} else {
		""
	};
	report.warnings.push(format!(
		"{} for {}; skipped complete module output{}",
		reason,
		entry.output_path(),
		force_note
	));
	report
		.conflict_resolutions
		.push(workspace_conflict_skipped_resolution(
			entry,
			&reason,
			conflict.leaf_conflicts,
		));
	Ok(())
}

fn discard_module_output(
	entry: &MergePlanEntry,
	out_dir: &Path,
	generated_paths: &mut BTreeSet<String>,
) -> Result<(), MergeError> {
	generated_paths.remove(entry.output_path());
	let target = out_dir.join(entry.output_path());
	match fs::remove_file(target) {
		Ok(()) => Ok(()),
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(error) => Err(MergeError::Io(error)),
	}
}

fn should_skip_base_passthrough(
	contributors: Option<&[ResolvedFileContributor]>,
	include_base: bool,
) -> bool {
	if include_base {
		return false;
	}
	contributors
		.and_then(|contributors| contributors.last())
		.is_some_and(|winner| winner.is_base_game && !winner.is_synthetic_base)
}

fn materialize_copy_through(
	workspace: &ResolvedWorkspace,
	entry: &MergePlanEntry,
	_out_dir: &Path,
	include_base: bool,
	report: &mut MergeReport,
	retained_paths: Option<&BTreeSet<String>>,
	pending_copy_through: &mut Vec<MergePlanEntry>,
) -> Result<(), MergeError> {
	let contributors = workspace.file_inventory.get(entry.output_path());
	if should_skip_base_passthrough(contributors.map(Vec::as_slice), include_base) {
		report.base_passthrough_skipped_file_count += 1;
	} else if retained_paths.is_some_and(|paths| !paths.contains(entry.output_path())) {
		return Ok(());
	} else {
		pending_copy_through.push(entry.clone());
		report.copied_file_count += 1;
	}
	Ok(())
}

fn flush_pending_copy_through(
	workspace: &ResolvedWorkspace,
	out_dir: &Path,
	pending_copy_through: &[MergePlanEntry],
) -> Result<(), MergeError> {
	for entry in pending_copy_through {
		copy_winner_file(workspace, entry, out_dir)?;
	}
	Ok(())
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
		.source_path()
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
	if options.force && is_text_placeholder_path(entry.output_path()) {
		let mut marker_entry = entry.clone();
		marker_entry.notes.push(reason.clone());
		write_conflict_placeholder(&marker_entry, out_dir)?;
		report.generated_file_count += 1;
		generated_paths.insert(entry.output_path().to_string());
		report.warnings.push(format!(
			"{} for {}; wrote manual conflict marker",
			reason,
			entry.output_path()
		));
	} else {
		report.warnings.push(format!(
			"{} for {}; manual resolution required, skipping output",
			reason,
			entry.output_path()
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
		path: entry.output_path().to_string(),
		reason: reason.to_string(),
		kind: summarize_conflict_kind(&leaf_conflicts),
		leaf_conflicts,
	}
}

fn plan_conflict_skipped_resolution(
	entry: &MergePlanEntry,
	reason: &str,
) -> MergeReportConflictResolution {
	MergeReportConflictResolution {
		path: entry.output_path().to_string(),
		reason: reason.to_string(),
		kind: None,
		leaf_conflicts: Vec::new(),
	}
}

fn summarize_conflict_kind(leaf_conflicts: &[LeafConflictDetail]) -> Option<ConflictKind> {
	let mut kinds = leaf_conflicts.iter().filter_map(|leaf| leaf.kind);
	let first = kinds.next()?;
	kinds.all(|kind| kind == first).then_some(first)
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
	/// Per top-level definition key → adopted-contributor mods (precedence
	/// order). Always computed; surfaced only when `--provenance` is enabled.
	definition_provenance: BTreeMap<String, Vec<String>>,
	/// Per top-level definition key → merge audit trail.
	merge_trace: BTreeMap<String, MergeTraceEntry>,
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

#[derive(Clone, Debug)]
struct DepMisuseRemoveCount {
	mod_id: String,
	dep_id: String,
	count: u32,
}

#[derive(Clone)]
struct PatchBasedMergeContext<'a> {
	descriptor: &'a ContentFamilyDescriptor,
	cwt_schema_graph: Option<Arc<CwtSchemaGraph>>,
	merge_key_source: MergeKeySource,
	gui_scroll_merge: bool,
	mod_dag: &'a ModDag,
	ignore_replace_path: &'a IgnoreReplacePath,
	dep_overrides: &'a [DepOverride],
	dep_misuse_findings: &'a [DepMisuseFinding],
	resolution_map: &'a foch_core::config::ResolutionMap,
	mod_versions: &'a HashMap<String, String>,
	mod_display_names: &'a HashMap<String, String>,
	cache_game_version: &'a str,
	emit_options: &'a EmitOptions,
	provenance: bool,
	script_cache: &'a WorkspaceScriptCache,
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
			tty: std::io::stderr().is_terminal(),
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
		let stderr = std::io::stderr();
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
			let _ = writeln!(std::io::stderr());
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
		MERGED_MOD_DESCRIPTOR_PATH, MergePlanContributor, MergePlanEntry, MergePlanResult,
		MergePlanStrategy, MergePlanTarget, MergeReport, MergeReportStatus,
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

	#[test]
	fn base_passthrough_skip_only_applies_to_true_base_without_include_base() {
		let true_base = test_contributor("base", 0, true, false);
		let synthetic_base = test_contributor("synthetic", 0, false, true);
		let mod_winner = test_contributor("mod", 1, false, false);

		assert!(super::should_skip_base_passthrough(
			Some(&[true_base]),
			false
		));
		assert!(!super::should_skip_base_passthrough(
			Some(&[test_contributor("base", 0, true, false)]),
			true
		));
		assert!(!super::should_skip_base_passthrough(
			Some(&[synthetic_base]),
			false
		));
		assert!(!super::should_skip_base_passthrough(
			Some(&[test_contributor("base", 0, true, false), mod_winner]),
			false
		));
		assert!(!super::should_skip_base_passthrough(None, false));
	}

	#[test]
	fn copy_through_skips_true_base_by_default_but_writes_opted_in_or_synthetic_base() {
		let temp = TempDir::new().expect("temp dir");
		let true_base_source = temp.path().join("game").join("common").join("vanilla.txt");
		let synthetic_source = temp
			.path()
			.join("synthetic")
			.join("common")
			.join("vanilla.txt");
		fs::create_dir_all(true_base_source.parent().expect("true base parent"))
			.expect("create true base parent");
		fs::create_dir_all(synthetic_source.parent().expect("synthetic parent"))
			.expect("create synthetic parent");
		fs::write(&true_base_source, "vanilla\n").expect("write true base source");
		fs::write(&synthetic_source, "synthetic\n").expect("write synthetic source");

		let true_base = test_contributor_with_path("base", true_base_source, 0, true, false);
		let synthetic_base =
			test_contributor_with_path("synthetic", synthetic_source, 0, false, true);
		let path = "common/vanilla.txt";

		let mut skipped_report = MergeReport::default();
		let mut skipped_pending = Vec::new();
		super::materialize_copy_through(
			&workspace_with_contributor(path, true_base.clone()),
			&copy_through_entry(path, &true_base),
			&temp.path().join("skip"),
			false,
			&mut skipped_report,
			None,
			&mut skipped_pending,
		)
		.expect("skip true base");
		assert!(!temp.path().join("skip").join(path).exists());
		assert!(skipped_pending.is_empty());
		assert_eq!(skipped_report.base_passthrough_skipped_file_count, 1);
		assert_eq!(skipped_report.copied_file_count, 0);

		let mut included_report = MergeReport::default();
		let mut included_pending = Vec::new();
		let included_workspace = workspace_with_contributor(path, true_base.clone());
		let included_out = temp.path().join("include");
		super::materialize_copy_through(
			&included_workspace,
			&copy_through_entry(path, &true_base),
			&included_out,
			true,
			&mut included_report,
			None,
			&mut included_pending,
		)
		.expect("include true base");
		assert!(
			!included_out.join(path).exists(),
			"copy-through is deferred until flush"
		);
		super::flush_pending_copy_through(&included_workspace, &included_out, &included_pending)
			.expect("flush included copy-through");
		assert_eq!(
			fs::read_to_string(included_out.join(path)).expect("read included"),
			"vanilla\n"
		);
		assert_eq!(included_report.base_passthrough_skipped_file_count, 0);
		assert_eq!(included_report.copied_file_count, 1);

		let mut synthetic_report = MergeReport::default();
		let mut synthetic_pending = Vec::new();
		let synthetic_workspace = workspace_with_contributor(path, synthetic_base.clone());
		let synthetic_out = temp.path().join("synthetic-out");
		super::materialize_copy_through(
			&synthetic_workspace,
			&copy_through_entry(path, &synthetic_base),
			&synthetic_out,
			false,
			&mut synthetic_report,
			None,
			&mut synthetic_pending,
		)
		.expect("write synthetic base");
		super::flush_pending_copy_through(&synthetic_workspace, &synthetic_out, &synthetic_pending)
			.expect("flush synthetic copy-through");
		assert_eq!(
			fs::read_to_string(synthetic_out.join(path)).expect("read synthetic"),
			"synthetic\n"
		);
		assert_eq!(synthetic_report.base_passthrough_skipped_file_count, 0);
		assert_eq!(synthetic_report.copied_file_count, 1);
	}

	#[test]
	fn copy_through_retained_paths_filters_deferred_copies() {
		let temp = TempDir::new().expect("temp dir");
		let source = temp.path().join("mod").join("common").join("keep.txt");
		fs::create_dir_all(source.parent().expect("source parent")).expect("create source parent");
		fs::write(&source, "kept\n").expect("write source");
		let contributor = test_contributor_with_path("mod", source, 1, false, false);
		let workspace = workspace_with_contributor("common/keep.txt", contributor.clone());
		let mut report = MergeReport::default();
		let mut pending = Vec::new();
		let retained = BTreeSet::from(["common/other.txt".to_string()]);

		super::materialize_copy_through(
			&workspace,
			&copy_through_entry("common/keep.txt", &contributor),
			&temp.path().join("out"),
			false,
			&mut report,
			Some(&retained),
			&mut pending,
		)
		.expect("filter copy-through");

		assert!(pending.is_empty());
		assert_eq!(report.copied_file_count, 0);
	}

	fn test_contributor(
		mod_id: &str,
		precedence: usize,
		is_base_game: bool,
		is_synthetic_base: bool,
	) -> ResolvedFileContributor {
		test_contributor_with_path(
			mod_id,
			PathBuf::from(mod_id).join("common/test.txt"),
			precedence,
			is_base_game,
			is_synthetic_base,
		)
	}

	fn test_contributor_with_path(
		mod_id: &str,
		absolute_path: PathBuf,
		precedence: usize,
		is_base_game: bool,
		is_synthetic_base: bool,
	) -> ResolvedFileContributor {
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(mod_id),
			absolute_path,
			precedence,
			is_base_game,
			is_synthetic_base,
			parse_ok_hint: None,
			mod_hash: if is_base_game {
				None
			} else {
				Some(format!("hash-{mod_id}"))
			},
		}
	}

	fn copy_through_entry(path: &str, contributor: &ResolvedFileContributor) -> MergePlanEntry {
		let plan_contributor = MergePlanContributor {
			mod_id: contributor.mod_id.clone(),
			source_path: contributor
				.absolute_path
				.to_string_lossy()
				.replace('\\', "/"),
			precedence: contributor.precedence,
			is_base_game: contributor.is_base_game,
		};
		MergePlanEntry {
			target: MergePlanTarget::File {
				path: path.to_string(),
			},
			strategy: MergePlanStrategy::CopyThrough,
			contributors: vec![plan_contributor.clone()],
			winner: Some(plan_contributor),
			generated: false,
			notes: Vec::new(),
		}
	}

	fn workspace_with_contributor(
		path: &str,
		contributor: ResolvedFileContributor,
	) -> ResolvedWorkspace {
		let mut file_inventory = BTreeMap::new();
		file_inventory.insert(path.to_string(), vec![contributor]);
		ResolvedWorkspace {
			playlist_path: PathBuf::from("playlist.json"),
			playlist: Playlist {
				game: Game::EuropaUniversalis4,
				name: "test".to_string(),
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
			super::per_entry_noop::drop_per_entry_noop_duplicates(merged, &vanilla, &descriptor);

		assert_eq!(count, 1);
		assert_eq!(assignment_keys(&filtered), vec!["changed".to_string()]);
	}

	#[test]
	fn per_entry_noop_keeps_entries_with_different_value() {
		let descriptor = per_entry_noop_descriptor(true);
		let vanilla = parse_test_statements("same = {\n\tadd_prestige = 1\n}\n");
		let merged = parse_test_statements("same = {\n\tadd_prestige = 2\n}\n");

		let (filtered, count) =
			super::per_entry_noop::drop_per_entry_noop_duplicates(merged, &vanilla, &descriptor);

		assert_eq!(count, 0);
		assert_eq!(assignment_keys(&filtered), vec!["same".to_string()]);
	}

	#[test]
	fn per_entry_noop_keeps_entries_when_family_not_opted_in() {
		let descriptor = per_entry_noop_descriptor(false);
		let vanilla = parse_test_statements("same = {\n\tadd_prestige = 1\n}\n");
		let merged = parse_test_statements("same = {\n\tadd_prestige = 1\n}\n");

		let (filtered, count) =
			super::per_entry_noop::drop_per_entry_noop_duplicates(merged, &vanilla, &descriptor);

		assert_eq!(count, 0);
		assert_eq!(assignment_keys(&filtered), vec!["same".to_string()]);
	}

	#[test]
	fn per_entry_noop_keeps_entries_with_no_vanilla_counterpart() {
		let descriptor = per_entry_noop_descriptor(true);
		let vanilla = parse_test_statements("same = {\n\tadd_prestige = 1\n}\n");
		let merged = parse_test_statements("unique = {\n\tadd_legitimacy = 1\n}\n");

		let (filtered, count) =
			super::per_entry_noop::drop_per_entry_noop_duplicates(merged, &vanilla, &descriptor);

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
		CheckRequest::from_playset_path(
			playlist_path.to_path_buf(),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		)
	}

	fn no_base_options(force: bool) -> MergeMaterializeOptions {
		MergeMaterializeOptions {
			include_game_base: false,
			include_base: false,
			gui_scroll_merge: false,
			force,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_map: foch_core::config::ResolutionMap::default(),
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			provenance: false,
			retained_paths: None,
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
			.find(|entry| entry.output_path() == path)
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
			definition_provenance: BTreeMap::new(),
			merge_trace: BTreeMap::new(),
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
					mod_hash: None,
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
			script_cache: Default::default(),
			file_inventory,
			requested_retained_paths: None,
			effective_retained_paths: None,
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

	const DAG_CONFLICT_PATH: &str = "history/countries/conflict.txt";

	fn idea_file(cost: &str) -> String {
		format!("group = {{\n\tidea = {{\n\t\tcost = {cost}\n\t}}\n}}\n")
	}

	#[test]
	fn materialize_keep_existing_carries_only_the_explicit_prior_file_into_staging() {
		let temp = TempDir::new().expect("temp dir");
		let prior_out_dir = temp.path().join("prior-out");
		let staging_dir = temp.path().join("staging");
		let relative_path = "common/ideas/handler.txt";
		write_file(&prior_out_dir, relative_path, "existing\n");
		write_file(
			&prior_out_dir,
			"common/ideas/unrelated-stale.txt",
			"unrelated\n",
		);

		let mut merge_output = patch_merge_output("merged\n");
		merge_output
			.keep_existing_paths
			.insert(PathBuf::from(relative_path));
		let mut report = MergeReport::default();

		let materialization = super::write_patch_merge_output(
			relative_path,
			&mut merge_output,
			&staging_dir,
			Some(&prior_out_dir),
			&ResolutionMap::default(),
			&mut report,
		)
		.expect("materialize keep existing");

		assert_eq!(
			materialization,
			super::PatchOutputMaterialization::KeptExisting
		);
		assert_eq!(
			fs::read_to_string(staging_dir.join(relative_path)).expect("read output"),
			"existing\n"
		);
		assert!(
			!staging_dir
				.join("common/ideas/unrelated-stale.txt")
				.exists()
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
			Some(&out_dir),
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
			Some(&out_dir),
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
			None,
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
			None,
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
			None,
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
	fn generated_descriptor_emits_sorted_unique_module_replacement_prefixes() {
		let temp = TempDir::new().expect("temp dir");
		let descriptor_path = temp.path().join("descriptor.mod");
		let replace_prefixes = BTreeSet::from([
			"common/governments".to_string(),
			"common/advisortypes".to_string(),
		]);

		super::io::write_generated_descriptor(
			temp.path(),
			&temp.path().join("playlist.json"),
			"test",
			&replace_prefixes,
			&descriptor_path,
		)
		.expect("write generated descriptor");

		let descriptor = fs::read_to_string(descriptor_path).expect("read descriptor");
		let replace_lines = descriptor
			.lines()
			.filter(|line| line.starts_with("replace_path="))
			.collect::<Vec<_>>();
		assert_eq!(
			replace_lines,
			vec![
				"replace_path=\"common/advisortypes\"",
				"replace_path=\"common/governments\"",
			]
		);
	}

	#[test]
	fn force_never_publishes_a_malformed_definition_module_placeholder() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("government-a");
		let mod_b = temp.path().join("government-b");
		let out_dir = temp.path().join("out");
		write_dlc_load(
			&playlist_path,
			&[("government-a", "A"), ("government-b", "B")],
		);
		write_descriptor(&mod_a, "government-a");
		write_descriptor(&mod_b, "government-b");
		write_file(
			&mod_a,
			"common/governments/a.txt",
			"government_a = { basic_reform = reform_a }\n",
		);
		write_file(
			&mod_b,
			"common/governments/b.txt",
			"unexpected_item\ngovernment_b = { basic_reform = reform_b }\n",
		);

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(true),
		)
		.expect("materialize forced module conflict");

		assert_eq!(report.status, MergeReportStatus::PartialSuccess);
		assert_eq!(report.manual_conflict_count, 1);
		assert_eq!(report.definition_module_count, 1);
		assert_eq!(report.definition_module_generated_count, 0);
		assert_eq!(report.definition_module_blocked_count, 1);
		assert!(
			!out_dir
				.join("common/governments/zzz_foch_governments.txt")
				.exists()
		);
		let descriptor =
			fs::read_to_string(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH)).expect("read descriptor");
		assert!(!descriptor.contains("replace_path=\"common/governments\""));
	}

	#[test]
	fn reset_only_mod_participates_in_definition_module_merge() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("government-a");
		let reset_mod = temp.path().join("government-reset");
		let mod_c = temp.path().join("government-c");
		let out_dir = temp.path().join("out");
		write_dlc_load(
			&playlist_path,
			&[
				("government-a", "A"),
				("government-reset", "Reset"),
				("government-c", "C"),
			],
		);
		write_descriptor(&mod_a, "government-a");
		write_descriptor(&reset_mod, "government-reset");
		fs::write(
			reset_mod.join("descriptor.mod"),
			"name=\"government-reset\"\nreplace_path=\"common/governments\"\n",
		)
		.expect("write reset descriptor");
		write_descriptor(&mod_c, "government-c");
		write_file(
			&mod_a,
			"common/governments/a.txt",
			"removed_by_reset = { basic_reform = old_reform }\n",
		);
		write_file(
			&mod_c,
			"common/governments/c.txt",
			"survives_reset = { basic_reform = new_reform }\n",
		);
		write_file(
			&out_dir,
			"common/governments/stale-sibling.txt",
			"stale = yes\n",
		);
		fs::write(
			out_dir.join(MERGED_MOD_DESCRIPTOR_PATH),
			"stale descriptor\n",
		)
		.expect("write stale descriptor");

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize reset module");

		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.definition_module_count, 1);
		assert_eq!(report.definition_module_generated_count, 1);
		assert_eq!(report.definition_module_blocked_count, 0);
		let output =
			fs::read_to_string(out_dir.join("common/governments/zzz_foch_governments.txt"))
				.expect("read module output");
		assert!(output.contains("survives_reset"), "output: {output}");
		assert!(!output.contains("removed_by_reset"), "output: {output}");
		assert!(
			!out_dir
				.join("common/governments/stale-sibling.txt")
				.exists()
		);
		let descriptor =
			fs::read_to_string(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH)).expect("read descriptor");
		assert!(descriptor.contains("replace_path=\"common/governments\""));
		assert!(
			descriptor.contains(&format!("path=\"{}\"", descriptor_path_value(&out_dir))),
			"descriptor must identify the final output directory: {descriptor}"
		);
		assert!(!descriptor.contains("foch-staging"));
	}

	#[test]
	fn ignore_replace_path_keeps_pre_reset_definition_module_content() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("government-a");
		let reset_mod = temp.path().join("government-reset");
		let mod_c = temp.path().join("government-c");
		let out_dir = temp.path().join("out");
		write_dlc_load(
			&playlist_path,
			&[
				("government-a", "A"),
				("government-reset", "Reset"),
				("government-c", "C"),
			],
		);
		write_descriptor(&mod_a, "government-a");
		write_descriptor(&reset_mod, "government-reset");
		fs::write(
			reset_mod.join("descriptor.mod"),
			"name=\"government-reset\"\nreplace_path=\"common/governments\"\n",
		)
		.expect("write reset descriptor");
		write_descriptor(&mod_c, "government-c");
		write_file(
			&mod_a,
			"common/governments/a.txt",
			"kept_when_ignored = { basic_reform = old_reform }\n",
		);
		write_file(
			&mod_c,
			"common/governments/c.txt",
			"later_definition = { basic_reform = new_reform }\n",
		);
		let mut options = no_base_options(false);
		options.ignore_replace_path = true;

		let report = materialize_merge_internal(request_for(&playlist_path), &out_dir, options)
			.expect("materialize ignored reset module");

		assert_eq!(report.status, MergeReportStatus::Ready);
		let output =
			fs::read_to_string(out_dir.join("common/governments/zzz_foch_governments.txt"))
				.expect("read module output");
		assert!(output.contains("kept_when_ignored"), "output: {output}");
		assert!(output.contains("later_definition"), "output: {output}");
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
		write_file(
			&out_dir,
			"common/governments/stale-module.txt",
			"stale = yes\n",
		);
		fs::write(
			out_dir.join(MERGED_MOD_DESCRIPTOR_PATH),
			"stale descriptor\n",
		)
		.expect("write stale descriptor");

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");

		assert_eq!(report.status, MergeReportStatus::Blocked);
		assert_eq!(report.manual_conflict_count, 1);
		assert!(!out_dir.join(DAG_CONFLICT_PATH).exists());
		assert!(!out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
		assert!(!out_dir.join("common/governments/stale-module.txt").exists());
		assert!(out_dir.join(MERGE_PLAN_ARTIFACT_PATH).is_file());
		assert!(out_dir.join(MERGE_REPORT_ARTIFACT_PATH).is_file());
		assert_eq!(report.conflict_resolutions.len(), 1);
		let resolution = &report.conflict_resolutions[0];
		assert!(resolution.reason.contains("unresolved conflict"));
		assert_eq!(resolution.leaf_conflicts.len(), 1);
		assert_eq!(resolution.leaf_conflicts[0].address_key, "group");
	}

	#[test]
	fn fatal_materialization_replaces_old_output_with_metadata_only() {
		let temp = TempDir::new().expect("temp dir");
		let missing_playlist = temp.path().join("missing-playlist.json");
		let out_dir = temp.path().join("out");
		write_file(
			&out_dir,
			"common/governments/stale-module.txt",
			"stale = yes\n",
		);
		fs::write(
			out_dir.join(MERGED_MOD_DESCRIPTOR_PATH),
			"stale descriptor\n",
		)
		.expect("write stale descriptor");

		let report = materialize_merge_internal(
			request_for(&missing_playlist),
			&out_dir,
			no_base_options(false),
		)
		.expect("publish fatal metadata");

		assert_eq!(report.status, MergeReportStatus::Fatal);
		assert!(!out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
		assert!(!out_dir.join("common/governments/stale-module.txt").exists());
		assert!(out_dir.join(MERGE_PLAN_ARTIFACT_PATH).is_file());
		assert!(out_dir.join(MERGE_REPORT_ARTIFACT_PATH).is_file());
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
