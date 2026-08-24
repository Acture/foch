#![allow(dead_code)]

mod cross_file_dedup;
mod io;
mod output_transaction;
mod per_entry_noop;
mod provenance_tooltip;
mod stale_detect;
pub(crate) mod structural;

use super::super::conflict_handler::ConflictHandler;
use super::super::dag::{
	DagDiagnostic, DagDiagnosticKind, IgnoreReplacePath, ModDag, ModId, build_mod_dag,
};
use super::super::error::MergeError;
#[allow(unused_imports)]
use super::super::namespace::{
	FamilyKeyIndex, build_family_key_index, detect_key_conflicts, group_by_family,
};
use super::super::plan::{
	build_merge_plan_from_workspace, fatal_plan_from_workspace_error,
	prune_noop_script_contributors,
};
use super::super::planning::module_view::{
	CrossFileModuleViewError, build_cross_file_module_views,
};
use super::localisation_merge::{LocalisationMergeOutcome, merge_localisation_file};
use crate::emit::EmitOptions;
use crate::merge::backend::{BackendRequest, BackendUnit, GumtreePcsNwayBackend, MergeBackend};
use crate::merge::execute::{
	CancellationToken, MergeAnalysisStage, MergeProgress, ProgressObserver,
};
use crate::merge::model::ExternalFileResolution;
use crate::merge::model::VanillaBaseMode;
use crate::request::CheckRequest;
use crate::workspace::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveError, WorkspaceScriptCache,
	resolve_workspace,
};
use cross_file_dedup::{CrossFilePruneResult, prune_cross_file_noop_duplicates};
use foch::model::{
	CheckContext, ConflictKind, DeferredUnitReason, DepMisuseFinding, HandlerResolutionRecord,
	LeafConflictDetail, MERGED_MOD_DESCRIPTOR_PATH, MergePlanEntry, MergePlanResult,
	MergePlanStrategy, MergePlanTarget, MergeReport, MergeReportConflictResolution,
	MergeReportStatus, MergeTraceEntry, SemanticIndex, StaleVanillaTargetDescriptor,
};
use foch::project::{AppliedDepOverride, DepOverride, ResolutionMap};
use foch_cwt::RuleEngine;
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, ContentLoadPolicy, GameProfile, MergeKeySource,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::rules::{detect_dependency_misuse, detect_version_mismatch};
#[cfg(test)]
use io::StructuralOutputMaterialization;
use io::{
	copy_winner_file, is_text_placeholder_path, write_clean_metadata_only,
	write_conflict_placeholder, write_generated_descriptor, write_metadata_only,
	write_structural_merge_output,
};
pub(crate) use output_transaction::OutputTransaction;
use provenance_tooltip::write_surviving_provenance_localisation;
use stale_detect::apply_dep_misuse_remove_counts;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::IsTerminal;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) struct MergeMaterializeOptions {
	pub include_game_base: bool,
	pub include_base: bool,
	pub gui_scroll_merge: bool,
	pub force: bool,
	pub ignore_replace_path: bool,
	pub dep_overrides: Vec<AppliedDepOverride>,
	pub resolution_map: foch::project::ResolutionMap,
	pub emit_options: EmitOptions,
	/// External resolution payloads read during prepare. Configured `use_file`
	/// decisions consume these bytes so export cannot drift during review.
	pub frozen_external_files: BTreeMap<PathBuf, Vec<u8>>,
	pub interactive_conflict_handler: Option<Box<dyn ConflictHandler>>,
	pub interactive_resolution_config_path: Option<PathBuf>,
	/// When set, annotate merged definitions with their adopted source mods
	/// (inline `# foch: …` comments + `.foch/foch-provenance.json`).
	pub provenance: bool,
	pub backend: Box<dyn MergeBackend>,
	/// Optional relative-path retention set for callers that only need a subset
	/// of copy-through output.
	pub retained_paths: Option<BTreeSet<String>>,
	pub cancellation: CancellationToken,
}

pub(crate) struct MaterializeOutput<'a> {
	pub artifacts_dir: &'a Path,
	pub prior_dir: Option<&'a Path>,
	pub target_dir: &'a Path,
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
			resolution_map: foch::project::ResolutionMap::default(),
			emit_options: EmitOptions::default(),
			frozen_external_files: BTreeMap::new(),
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			provenance: false,
			backend: Box::new(GumtreePcsNwayBackend),
			retained_paths: None,
			cancellation: CancellationToken::new(),
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

/// Freeze the exact path-level plan that materialization will consume.
///
/// Merge-only priority changes and no-op pruning must happen before the user
/// reviews the plan. Keeping this preparation in one helper prevents the
/// preview and export paths from resolving or planning the workspace twice.
pub(crate) fn prepare_merge_plan(
	workspace_result: &mut Result<ResolvedWorkspace, WorkspaceResolveError>,
	include_game_base: bool,
	resolution_map: &ResolutionMap,
) -> MergePlanResult {
	if let Ok(workspace) = workspace_result {
		apply_mod_priority_boosts(workspace, &resolution_map.mod_priority_boost);
		prune_noop_script_contributors(workspace, eu4_profile());
	}

	stage_log_with("build_merge_plan", || {
		let plan = match workspace_result {
			Ok(workspace) => build_merge_plan_from_workspace(workspace, include_game_base),
			Err(err) => fatal_plan_from_workspace_error(err, include_game_base),
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
	})
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
	// Resolve once and reuse: planning and materialization consume the same
	// ResolvedWorkspace snapshot.
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
	transaction.commit()?;
	Ok(report)
}

pub(crate) fn materialize_merge_with_workspace_result(
	request: CheckRequest,
	out_dir: &Path,
	prior_out_dir: Option<&Path>,
	published_out_dir: &Path,
	options: MergeMaterializeOptions,
	mut workspace_result: Result<ResolvedWorkspace, WorkspaceResolveError>,
) -> Result<MergeReport, MergeError> {
	let plan = prepare_merge_plan(
		&mut workspace_result,
		options.include_game_base,
		&options.resolution_map,
	);
	materialize_prepared_merge_with_workspace_result(
		request,
		MaterializeOutput {
			artifacts_dir: out_dir,
			prior_dir: prior_out_dir,
			target_dir: published_out_dir,
		},
		options,
		workspace_result,
		plan,
		None,
	)
}

pub(crate) fn materialize_prepared_merge_with_workspace_result(
	request: CheckRequest,
	output: MaterializeOutput<'_>,
	mut options: MergeMaterializeOptions,
	workspace_result: Result<ResolvedWorkspace, WorkspaceResolveError>,
	plan: MergePlanResult,
	progress: Option<(&dyn ProgressObserver, Instant)>,
) -> Result<MergeReport, MergeError> {
	let MaterializeOutput {
		artifacts_dir: out_dir,
		prior_dir: prior_out_dir,
		target_dir: published_out_dir,
	} = output;
	let mut report = MergeReport::default();
	let mut generated_paths = BTreeSet::new();
	let profile = eu4_profile();
	report.unsupported_input_count = plan.strategies.manual_conflict;
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
		// Keep fatal snapshot and resolution failures actionable in the report;
		// merge-plan fatal errors are intentionally not serialized.
		report.fatal_reason = workspace_result
			.as_ref()
			.err()
			.map(|err| err.message.clone())
			.or_else(|| plan.fatal_errors.first().cloned());
		write_clean_metadata_only(out_dir, &plan, &report)?;
		return Ok(report);
	}
	if options.backend.profile().validate_semantic_units {
		validate_structured_plan_selection(&plan, options.retained_paths.as_ref())?;
	}
	record_plan_unsupported_inputs(&mut report, &plan);

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

	fs::create_dir_all(out_dir)?;
	let descriptor_root = descriptor_output_root(published_out_dir)?;

	let mod_versions = workspace_mod_versions(&workspace);
	let mod_display_names = workspace_mod_display_names(&workspace);
	let cache_game_version = workspace_cache_game_version(&workspace);
	let cache_game_version =
		cache_game_version_with_resolution_salt(&cache_game_version, &options.resolution_map);
	let emit_options = options.emit_options.clone();

	crate::cache::reset_mod_diff_cache_stats();
	crate::cache::reset_dag_base_cache_stats();
	let materialize_started = Instant::now();
	let total_paths = plan.paths.len();
	eprintln!("[merge] materialize: start (total_paths={total_paths})");
	let mut materialize_progress = MaterializeProgress::new(total_paths, progress);
	let mut pending_copy_through = Vec::new();
	let mut counted_generated_paths = BTreeSet::new();
	let mut provenance_localisation_by_script = BTreeMap::<String, BTreeMap<String, String>>::new();

	for entry in &plan.paths {
		options.cancellation.check()?;
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
								record_counted_generated_output(
									entry.output_path(),
									&mut generated_paths,
									&mut counted_generated_paths,
									&mut report,
								);
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
				let contributors = workspace.file_inventory.get(entry.output_path());
				let descriptor = profile.classify_content_family(Path::new(entry.output_path()));
				let vanilla_base_mode = effective_vanilla_base_mode(
					descriptor,
					contributors.map(Vec::as_slice),
					VanillaBaseMode::from_include_game_base(options.include_game_base),
					workspace
						.verified_absent_base_paths
						.contains(entry.output_path()),
				);
				if options.backend.profile().validate_semantic_units {
					validate_structured_merge_entry(
						entry,
						contributors.map(Vec::as_slice),
						vanilla_base_mode,
						profile,
					)?;
				}
				if matches!(&entry.target, MergePlanTarget::Module { .. }) {
					let module_started = Instant::now();
					let deferred_before = report.deferred_unit_count();
					materialize_cross_file_module(CrossFileModuleMaterializeContext {
						workspace: &workspace,
						entry,
						out_dir,
						prior_out_dir,
						options: &mut options,
						report: &mut report,
						generated_paths: &mut generated_paths,
						counted_generated_paths: &mut counted_generated_paths,
						profile,
						mod_dag: &mod_dag,
						ignore_replace_path: &ignore_replace_path,
						dep_overrides: &dep_overrides,
						mod_versions: &mod_versions,
						mod_display_names: &mod_display_names,
						cache_game_version: &cache_game_version,
						emit_options: &emit_options,
						provenance_localisation_by_script: &mut provenance_localisation_by_script,
					})?;
					report.definition_module_elapsed_ms = report
						.definition_module_elapsed_ms
						.saturating_add(module_started.elapsed().as_millis() as u64);
					if !generated_paths.contains(entry.output_path())
						&& report.deferred_unit_count() > deferred_before
					{
						report.definition_module_blocked_count += 1;
					}
					continue;
				}
				let can_run_semantic_merge = !vanilla_base_mode.requires_non_empty()
					|| contributors
						.map(|cs| cs.iter().any(|c| c.is_base_game))
						.unwrap_or(false);

				if can_run_semantic_merge && let Some(contributors) = contributors {
					// Only invoke the tree merge when 2+ non-base mods contribute
					// (single-mod overlap with base is just last-writer).
					let non_base_count = contributors
						.iter()
						.filter(|c| !c.is_base_game && !c.is_synthetic_base)
						.count();

					if non_base_count >= 2 {
						let merge_key_source = descriptor.and_then(|d| d.merge_key_source);

						if let (Some(descriptor), Some(merge_key_source)) =
							(descriptor, merge_key_source)
						{
							let target = entry.output_path().to_string();
							let contribs = contributors.clone();
							let desc = descriptor.clone();
							let cwt_rule_engine =
								crate::merge::cwt_suggestions::cwt_rule_engine_for_profile(profile);
							let dag = mod_dag.clone();
							let ignore = ignore_replace_path.clone();
							let dep_overrides = dep_overrides.clone();
							let dep_misuse = report.dep_misuse.clone();
							let resolution_map = options.resolution_map.clone();
							let interactive_config_path =
								options.interactive_resolution_config_path.clone();
							let backend = &*options.backend;
							let interactive_handler =
								options.interactive_conflict_handler.as_deref_mut();
							let result =
								std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
									let context = StructuralMergeContext {
										descriptor: &desc,
										cwt_rule_engine: cwt_rule_engine.clone(),
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
										vanilla_base_mode,
									};
									backend.analyze(BackendRequest {
										target_path: &target,
										unit: BackendUnit::File(&contribs),
										context,
										interactive_handler,
										interactive_config_path: interactive_config_path.as_deref(),
									})
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
									let materialization = write_structural_merge_output(
										entry.output_path(),
										&mut merge_output,
										out_dir,
										prior_out_dir,
										&options.resolution_map,
										&options.frozen_external_files,
										&mut report,
									)?;
									if materialization.uses_rendered_output() {
										report.per_entry_noop_skipped_count +=
											merge_output.per_entry_noop_skipped_count;
									}
									if materialization.publishes_output()
										&& materialization.uses_rendered_output()
									{
										let entries = std::mem::take(
											&mut merge_output.provenance_localisation,
										);
										if !entries.is_empty() {
											provenance_localisation_by_script
												.insert(entry.output_path().to_string(), entries);
										}
									}
									if materialization.counts_as_generated() {
										record_counted_generated_output(
											entry.output_path(),
											&mut generated_paths,
											&mut counted_generated_paths,
											&mut report,
										);
										if options.provenance
											&& materialization.uses_rendered_output()
										{
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
								Ok(Err(StructuralMergeFailure::Unresolved(conflict))) => {
									if resolve_structural_merge_failure(
										StructuralMergeFailureCtx {
											entry,
											out_dir,
											conflict,
											deferred_reason: DeferredUnitReason::NeedsUserChoice,
											options: &options,
											report: &mut report,
											generated_paths: &mut generated_paths,
											counted_generated_paths: &mut counted_generated_paths,
										},
									)? {
										continue;
									}
								}
								Ok(Err(StructuralMergeFailure::Merge(err))) => {
									let conflict = StructuralConflictReport::without_details(
										format!("structural merge failed: {err}"),
									);
									if resolve_structural_merge_failure(
										StructuralMergeFailureCtx {
											entry,
											out_dir,
											conflict,
											deferred_reason: DeferredUnitReason::EngineFailure,
											options: &options,
											report: &mut report,
											generated_paths: &mut generated_paths,
											counted_generated_paths: &mut counted_generated_paths,
										},
									)? {
										continue;
									}
								}
								Err(_) => {
									let conflict = StructuralConflictReport::without_details(
										"structural merge panicked".to_string(),
									);
									if resolve_structural_merge_failure(
										StructuralMergeFailureCtx {
											entry,
											out_dir,
											conflict,
											deferred_reason: DeferredUnitReason::EngineFailure,
											options: &options,
											report: &mut report,
											generated_paths: &mut generated_paths,
											counted_generated_paths: &mut counted_generated_paths,
										},
									)? {
										continue;
									}
								}
							}
						}
					}

					// Single non-base mod or structural merge failed: copy winner
					copy_winner_file(&workspace, entry, out_dir)?;
					record_counted_generated_output(
						entry.output_path(),
						&mut generated_paths,
						&mut counted_generated_paths,
						&mut report,
					);
				} else {
					// No observed vanilla base and the family does not permit a
					// known-empty semantic ancestor;
					// fall back to last-writer copy.
					copy_winner_file(&workspace, entry, out_dir)?;
					record_counted_generated_output(
						entry.output_path(),
						&mut generated_paths,
						&mut counted_generated_paths,
						&mut report,
					);
				}
			}
			MergePlanStrategy::ManualConflict => {
				let reason = if entry.notes.is_empty() {
					"input is not supported by a safe merge strategy".to_string()
				} else {
					entry.notes.join("; ")
				};
				if matches!(&entry.target, MergePlanTarget::Module { .. }) {
					discard_module_output(entry, out_dir, &mut generated_paths)?;
					let force_note = if options.force {
						"; --force applies only to genuine conflicts"
					} else {
						""
					};
					report.warnings.push(format!(
						"{} for {}; deferred complete module output{}",
						reason,
						entry.output_path(),
						force_note
					));
					continue;
				}
				let force_note = if options.force {
					"; --force applies only to genuine conflicts"
				} else {
					""
				};
				report.warnings.push(format!(
					"{} for {}; unsupported input deferred, skipping output{}",
					reason,
					entry.output_path(),
					force_note
				));
			}
		}
	}
	materialize_progress.finish();
	// Cross-file dedup must observe the same files that the game loader will:
	// pending one-writer paths are real mod outputs, not vanilla fallbacks.
	let prune_result = build_surviving_output_manifest(
		&workspace,
		out_dir,
		&pending_copy_through,
		generated_paths,
		profile,
		&mut report,
	)?;
	let published_module_replacements = reconcile_surviving_output_facts(
		&plan,
		&prune_result,
		&mut counted_generated_paths,
		&mut provenance_localisation_by_script,
		&mut report,
	);
	let mut generated_paths = prune_result.surviving_generated_paths;
	if options.provenance
		&& let Some(localisation_path) = write_surviving_provenance_localisation(
			out_dir,
			&provenance_localisation_by_script,
			&generated_paths,
		)? {
		record_counted_generated_output(
			&localisation_path,
			&mut generated_paths,
			&mut counted_generated_paths,
			&mut report,
		);
	}
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

	report.status = if report.deferred_unit_count() > 0 {
		MergeReportStatus::PartialSuccess
	} else {
		MergeReportStatus::Ready
	};

	write_generated_descriptor(
		&descriptor_root,
		request.source_path(),
		&plan.playset_name,
		&published_module_replacements,
		&out_dir.join(MERGED_MOD_DESCRIPTOR_PATH),
	)?;
	write_metadata_only(out_dir, &plan, &report)?;
	Ok(report)
}

fn record_counted_generated_output(
	path: &str,
	generated_paths: &mut BTreeSet<String>,
	counted_generated_paths: &mut BTreeSet<String>,
	report: &mut MergeReport,
) {
	generated_paths.insert(path.to_string());
	if counted_generated_paths.insert(path.to_string()) {
		report.generated_file_count += 1;
	}
}

fn reconcile_surviving_output_facts(
	plan: &MergePlanResult,
	prune_result: &CrossFilePruneResult,
	counted_generated_paths: &mut BTreeSet<String>,
	provenance_localisation_by_script: &mut BTreeMap<String, BTreeMap<String, String>>,
	report: &mut MergeReport,
) -> BTreeSet<String> {
	let surviving_paths = &prune_result.surviving_generated_paths;
	debug_assert!(prune_result.pruned_paths.is_disjoint(surviving_paths));

	counted_generated_paths.retain(|path| surviving_paths.contains(path));
	report.generated_file_count = counted_generated_paths.len();
	report.cross_file_noop_skipped_file_count = prune_result.pruned_paths.len();
	report
		.merge_trace
		.retain(|path, _| surviving_paths.contains(path));
	report
		.definition_provenance
		.retain(|path, _| surviving_paths.contains(path));
	provenance_localisation_by_script.retain(|path, _| surviving_paths.contains(path));

	let mut published_module_replacements = BTreeSet::new();
	report.definition_module_generated_count = 0;
	for entry in &plan.paths {
		if !matches!(&entry.target, MergePlanTarget::Module { .. })
			|| !surviving_paths.contains(entry.output_path())
		{
			continue;
		}
		report.definition_module_generated_count += 1;
		if let Some(prefix) = entry.target.replace_prefix() {
			published_module_replacements.insert(prefix.to_string());
		}
	}

	published_module_replacements
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
	counted_generated_paths: &'a mut BTreeSet<String>,
	profile: &'a dyn GameProfile,
	mod_dag: &'a ModDag,
	ignore_replace_path: &'a IgnoreReplacePath,
	dep_overrides: &'a [DepOverride],
	mod_versions: &'a HashMap<String, String>,
	mod_display_names: &'a HashMap<String, String>,
	cache_game_version: &'a str,
	emit_options: &'a EmitOptions,
	provenance_localisation_by_script: &'a mut BTreeMap<String, BTreeMap<String, String>>,
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
		counted_generated_paths,
		profile,
		mod_dag,
		ignore_replace_path,
		dep_overrides,
		mod_versions,
		mod_display_names,
		cache_game_version,
		emit_options,
		provenance_localisation_by_script,
	} = context;
	let Some(descriptor) = profile.classify_content_family(Path::new(entry.output_path())) else {
		return resolve_cross_file_module_failure(
			entry,
			out_dir,
			options,
			report,
			generated_paths,
			DeferredUnitReason::EngineFailure,
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
			DeferredUnitReason::EngineFailure,
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
		options.backend.profile().duplicate_definition_override,
	) {
		Ok(views) => views,
		Err(error) => {
			let (deferred_reason, reason) = match error {
				CrossFileModuleViewError::UnsupportedInput(reason) => {
					(DeferredUnitReason::UnsupportedInput, reason)
				}
				CrossFileModuleViewError::EngineFailure(reason) => {
					(DeferredUnitReason::EngineFailure, reason)
				}
			};
			return resolve_cross_file_module_failure(
				entry,
				out_dir,
				options,
				report,
				generated_paths,
				deferred_reason,
				reason,
			);
		}
	};
	let cwt_rule_engine = crate::merge::cwt_suggestions::cwt_rule_engine_for_profile(profile);
	let backend = &*options.backend;
	let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		let merge_context = StructuralMergeContext {
			descriptor,
			cwt_rule_engine,
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
			vanilla_base_mode: VanillaBaseMode::from_include_game_base(options.include_game_base),
		};
		backend.analyze(BackendRequest {
			target_path: entry.output_path(),
			unit: BackendUnit::DefinitionModule(&views),
			context: merge_context,
			interactive_handler: options.interactive_conflict_handler.as_deref_mut(),
			interactive_config_path: options.interactive_resolution_config_path.as_deref(),
		})
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
			let materialization = match write_structural_merge_output(
				entry.output_path(),
				&mut merge_output,
				&stage_dir,
				prior_out_dir,
				&options.resolution_map,
				&options.frozen_external_files,
				report,
			) {
				Ok(materialization) => materialization,
				Err(error) => {
					let _ = fs::remove_dir_all(&stage_dir);
					return Err(error);
				}
			};
			if materialization.uses_rendered_output() {
				report.per_entry_noop_skipped_count += merge_output.per_entry_noop_skipped_count;
			}
			if materialization.publishes_output() {
				publish_staged_module_output(&stage_dir, out_dir, entry.output_path())?;
				generated_paths.insert(entry.output_path().to_string());
				if materialization.uses_rendered_output() {
					let entries = std::mem::take(&mut merge_output.provenance_localisation);
					if !entries.is_empty() {
						provenance_localisation_by_script
							.insert(entry.output_path().to_string(), entries);
					}
				}
				if materialization.counts_as_generated() {
					record_counted_generated_output(
						entry.output_path(),
						generated_paths,
						counted_generated_paths,
						report,
					);
				}
				if materialization.counts_as_generated()
					&& materialization.uses_rendered_output()
					&& options.provenance
				{
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
					DeferredUnitReason::EngineFailure,
					"definition module did not produce its required staged output".to_string(),
				);
			}
			let _ = fs::remove_dir_all(&stage_dir);
			Ok(())
		}
		Ok(Err(StructuralMergeFailure::Unresolved(conflict))) => {
			resolve_cross_file_module_conflict(
				entry,
				out_dir,
				options,
				report,
				generated_paths,
				DeferredUnitReason::NeedsUserChoice,
				conflict,
			)?;
			Ok(())
		}
		Ok(Err(StructuralMergeFailure::Merge(error))) => resolve_cross_file_module_failure(
			entry,
			out_dir,
			options,
			report,
			generated_paths,
			DeferredUnitReason::EngineFailure,
			format!("cross-file module merge failed: {error}"),
		),
		Err(_) => resolve_cross_file_module_failure(
			entry,
			out_dir,
			options,
			report,
			generated_paths,
			DeferredUnitReason::EngineFailure,
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
	deferred_reason: DeferredUnitReason,
	reason: String,
) -> Result<(), MergeError> {
	resolve_cross_file_module_conflict(
		entry,
		out_dir,
		options,
		report,
		generated_paths,
		deferred_reason,
		StructuralConflictReport::without_details(reason),
	)
}

fn resolve_cross_file_module_conflict(
	entry: &MergePlanEntry,
	out_dir: &Path,
	options: &MergeMaterializeOptions,
	report: &mut MergeReport,
	generated_paths: &mut BTreeSet<String>,
	deferred_reason: DeferredUnitReason,
	conflict: StructuralConflictReport,
) -> Result<(), MergeError> {
	discard_module_output(entry, out_dir, generated_paths)?;
	let reason = conflict.reason;
	record_deferred_unit(report, deferred_reason);
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
			deferred_reason,
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
	let started = Instant::now();
	let mut last_progress = started;
	let mut cloned = 0_usize;
	let total = pending_copy_through.len();
	eprintln!("[merge] copy_through_flush: start files={total}");
	for (index, entry) in pending_copy_through.iter().enumerate() {
		cloned += usize::from(copy_winner_file(workspace, entry, out_dir)?);
		let processed = index + 1;
		if processed < total
			&& (processed.is_multiple_of(5_000)
				|| last_progress.elapsed() >= Duration::from_secs(2))
		{
			let elapsed = started.elapsed().as_secs_f64().max(f64::EPSILON);
			let rate = processed as f64 / elapsed;
			let eta_seconds = (total - processed) as f64 / rate;
			eprintln!(
				"[merge] copy_through_flush: progress processed={processed} total={total} cloned={cloned} copied={} elapsed_ms={} rate_files_per_second={rate:.1} eta_seconds={eta_seconds:.1}",
				processed.saturating_sub(cloned),
				started.elapsed().as_millis(),
			);
			last_progress = Instant::now();
		}
	}
	eprintln!(
		"[merge] copy_through_flush: done elapsed_ms={} files={} cloned={} copied={}",
		started.elapsed().as_millis(),
		total,
		cloned,
		total.saturating_sub(cloned),
	);
	Ok(())
}

fn build_surviving_output_manifest(
	workspace: &ResolvedWorkspace,
	out_dir: &Path,
	pending_copy_through: &[MergePlanEntry],
	generated_paths: BTreeSet<String>,
	profile: &dyn GameProfile,
	report: &mut MergeReport,
) -> Result<CrossFilePruneResult, MergeError> {
	flush_pending_copy_through(workspace, out_dir, pending_copy_through)?;
	prune_cross_file_noop_duplicates(out_dir, generated_paths, workspace, profile, report)
}

fn dependency_misuse_context(workspace: &ResolvedWorkspace) -> CheckContext {
	CheckContext {
		playlist_path: workspace.playlist_path.clone(),
		playlist: workspace.playlist.clone(),
		mods: workspace.mods.clone(),
		semantic_index: workspace_mod_semantic_index(workspace),
	}
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

fn validate_structured_merge_entry(
	entry: &MergePlanEntry,
	contributors: Option<&[ResolvedFileContributor]>,
	vanilla_base_mode: VanillaBaseMode,
	profile: &dyn GameProfile,
) -> Result<(), MergeError> {
	let descriptor = profile
		.classify_content_family(Path::new(entry.output_path()))
		.ok_or_else(|| {
			structured_merge_unsupported(entry, "the path has no ContentFamily descriptor")
		})?;
	if !descriptor.capabilities.merge_ready {
		return Err(structured_merge_unsupported(
			entry,
			"the ContentFamily is not marked merge-ready",
		));
	}

	match &entry.target {
		MergePlanTarget::File { .. } => {
			let contributors = contributors.ok_or_else(|| {
				structured_merge_unsupported(entry, "the merge unit has no file contributors")
			})?;
			let source_mods = contributors
				.iter()
				.filter(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
				.map(|contributor| contributor.mod_id.as_str())
				.collect::<BTreeSet<_>>();
			if source_mods.len() < 2 {
				return Ok(());
			}
			if vanilla_base_mode.requires_non_empty()
				&& !contributors
					.iter()
					.any(|contributor| contributor.is_base_game && !contributor.is_synthetic_base)
			{
				return Err(structured_merge_unsupported(
					entry,
					"a real vanilla file is required as the semantic merge base",
				));
			}
			if descriptor.merge_key_source.is_none() {
				return Err(structured_merge_unsupported(
					entry,
					"the ContentFamily has no merge-key contract",
				));
			}
		}
		MergePlanTarget::Module { .. } => {
			let source_mods = entry
				.contributors
				.iter()
				.filter(|contributor| !contributor.is_base_game)
				.map(|contributor| contributor.mod_id.as_str())
				.collect::<BTreeSet<_>>();
			if source_mods.len() < 2 {
				return Ok(());
			}
			if !matches!(
				descriptor.load_policy,
				ContentLoadPolicy::DefinitionModule(_)
			) {
				return Err(structured_merge_unsupported(
					entry,
					"the target is not a definition module",
				));
			}
			if descriptor.merge_key_source.is_none() {
				return Err(structured_merge_unsupported(
					entry,
					"the ContentFamily has no merge-key contract",
				));
			}
			if vanilla_base_mode.requires_non_empty()
				&& !entry
					.contributors
					.iter()
					.any(|contributor| contributor.is_base_game)
			{
				return Err(structured_merge_unsupported(
					entry,
					"a vanilla definition module is required as the semantic merge base",
				));
			}
		}
	}
	Ok(())
}

fn effective_vanilla_base_mode(
	descriptor: Option<&ContentFamilyDescriptor>,
	contributors: Option<&[ResolvedFileContributor]>,
	configured: VanillaBaseMode,
	base_path_verified_absent: bool,
) -> VanillaBaseMode {
	if configured != VanillaBaseMode::Required
		|| !base_path_verified_absent
		|| !descriptor.is_some_and(ContentFamilyDescriptor::supports_verified_empty_file_base)
	{
		return configured;
	}

	let has_real_vanilla = contributors.is_some_and(|contributors| {
		contributors
			.iter()
			.any(|contributor| contributor.is_base_game && !contributor.is_synthetic_base)
	});
	if has_real_vanilla {
		VanillaBaseMode::Required
	} else {
		VanillaBaseMode::KnownAbsent
	}
}

fn validate_structured_plan_selection(
	plan: &MergePlanResult,
	retained_paths: Option<&BTreeSet<String>>,
) -> Result<(), MergeError> {
	let Some(retained_paths) = retained_paths.filter(|paths| !paths.is_empty()) else {
		return Ok(());
	};
	for retained_path in retained_paths {
		let normalized = retained_path.replace('\\', "/");
		let entry = plan
			.paths
			.iter()
			.find(|entry| {
				entry.output_path() == normalized
					|| entry
						.target
						.input_paths()
						.iter()
						.any(|path| path == &normalized)
			})
			.ok_or_else(|| MergeError::Validation {
				path: Some(normalized.clone()),
				message: "structured merge unsupported: retained path has no merge-plan unit"
					.to_string(),
			})?;
		let is_base_only_runtime_fallback = entry.strategy == MergePlanStrategy::CopyThrough
			&& !entry.contributors.is_empty()
			&& entry
				.contributors
				.iter()
				.all(|contributor| contributor.is_base_game);
		if entry.strategy != MergePlanStrategy::StructuralMerge && !is_base_only_runtime_fallback {
			return Err(structured_merge_unsupported(
				entry,
				&format!(
					"retained path planned as {}; the candidate kernel was not invoked",
					merge_plan_strategy_name(entry.strategy)
				),
			));
		}
	}
	Ok(())
}

fn merge_plan_strategy_name(strategy: MergePlanStrategy) -> &'static str {
	match strategy {
		MergePlanStrategy::CopyThrough => "copy_through",
		MergePlanStrategy::LastWriterOverlay => "last_writer_overlay",
		MergePlanStrategy::StructuralMerge => "structural_merge",
		MergePlanStrategy::LocalisationMerge => "localisation_merge",
		MergePlanStrategy::ManualConflict => "manual_conflict",
	}
}

fn structured_merge_unsupported(entry: &MergePlanEntry, reason: &str) -> MergeError {
	MergeError::Validation {
		path: Some(entry.output_path().to_string()),
		message: format!("structured merge unsupported: {reason}"),
	}
}

fn record_plan_unsupported_inputs(report: &mut MergeReport, plan: &MergePlanResult) {
	for entry in &plan.paths {
		if entry.strategy != MergePlanStrategy::ManualConflict {
			continue;
		}
		let reason = if entry.notes.is_empty() {
			"input is not supported by a safe merge strategy".to_string()
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
	conflict: StructuralConflictReport,
	deferred_reason: DeferredUnitReason,
	options: &'a MergeMaterializeOptions,
	report: &'a mut MergeReport,
	generated_paths: &'a mut BTreeSet<String>,
	counted_generated_paths: &'a mut BTreeSet<String>,
}

fn resolve_structural_merge_failure(
	ctx: StructuralMergeFailureCtx<'_>,
) -> Result<bool, MergeError> {
	let StructuralMergeFailureCtx {
		entry,
		out_dir,
		conflict,
		deferred_reason,
		options,
		report,
		generated_paths,
		counted_generated_paths,
	} = ctx;
	let reason = conflict.reason;
	record_deferred_unit(report, deferred_reason);
	report
		.handler_resolutions
		.extend(conflict.handler_resolutions);
	if deferred_reason == DeferredUnitReason::NeedsUserChoice
		&& options.force
		&& is_text_placeholder_path(entry.output_path())
	{
		let mut marker_entry = entry.clone();
		marker_entry.notes.push(reason.clone());
		write_conflict_placeholder(&marker_entry, out_dir)?;
		record_counted_generated_output(
			entry.output_path(),
			generated_paths,
			counted_generated_paths,
			report,
		);
		report.warnings.push(format!(
			"{} for {}; wrote manual conflict marker",
			reason,
			entry.output_path()
		));
	} else {
		report.warnings.push(format!(
			"{} for {}; {} deferred, skipping output",
			reason,
			entry.output_path(),
			deferred_reason.as_str()
		));
	}
	report
		.conflict_resolutions
		.push(workspace_conflict_skipped_resolution(
			entry,
			&reason,
			deferred_reason,
			conflict.leaf_conflicts,
		));
	Ok(true)
}

fn workspace_conflict_skipped_resolution(
	entry: &MergePlanEntry,
	reason: &str,
	deferred_reason: DeferredUnitReason,
	leaf_conflicts: Vec<LeafConflictDetail>,
) -> MergeReportConflictResolution {
	MergeReportConflictResolution {
		path: entry.output_path().to_string(),
		reason: reason.to_string(),
		deferred_reason,
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
		deferred_reason: DeferredUnitReason::UnsupportedInput,
		kind: None,
		leaf_conflicts: Vec::new(),
	}
}

fn summarize_conflict_kind(leaf_conflicts: &[LeafConflictDetail]) -> Option<ConflictKind> {
	let mut kinds = leaf_conflicts.iter().filter_map(|leaf| leaf.kind);
	let first = kinds.next()?;
	kinds.all(|kind| kind == first).then_some(first)
}

fn record_deferred_unit(report: &mut MergeReport, reason: DeferredUnitReason) {
	match reason {
		DeferredUnitReason::NeedsUserChoice => report.manual_conflict_count += 1,
		DeferredUnitReason::UnsupportedInput => report.unsupported_input_count += 1,
		DeferredUnitReason::EngineFailure => report.engine_failure_count += 1,
	}
}

#[derive(Clone, Debug)]
pub(crate) struct StructuralMergeOutput {
	rendered: String,
	dep_remove_counts: Vec<DepMisuseRemoveCount>,
	stale_vanilla_targets: Vec<StaleVanillaTargetDescriptor>,
	handler_resolutions: Vec<HandlerResolutionRecord>,
	external_file_resolutions: HashMap<PathBuf, ExternalFileResolution>,
	keep_existing_paths: HashSet<PathBuf>,
	/// True when the merged statement list is AST-equal (modulo
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
	/// Eu4-loadable localisation entries for condition tooltip wrappers. These
	/// are published only when this exact rendered output survives materialization.
	provenance_localisation: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct StructuralConflictReport {
	reason: String,
	leaf_conflicts: Vec<LeafConflictDetail>,
	handler_resolutions: Vec<HandlerResolutionRecord>,
}

impl StructuralConflictReport {
	fn without_details(reason: String) -> Self {
		Self {
			reason,
			leaf_conflicts: Vec::new(),
			handler_resolutions: Vec::new(),
		}
	}
}

#[derive(Debug)]
pub(crate) enum StructuralMergeFailure {
	Merge(MergeError),
	Unresolved(StructuralConflictReport),
}

impl From<MergeError> for StructuralMergeFailure {
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
pub(crate) struct StructuralMergeContext<'a> {
	descriptor: &'a ContentFamilyDescriptor,
	cwt_rule_engine: Option<Arc<RuleEngine>>,
	merge_key_source: MergeKeySource,
	gui_scroll_merge: bool,
	mod_dag: &'a ModDag,
	ignore_replace_path: &'a IgnoreReplacePath,
	dep_overrides: &'a [DepOverride],
	dep_misuse_findings: &'a [DepMisuseFinding],
	resolution_map: &'a foch::project::ResolutionMap,
	mod_versions: &'a HashMap<String, String>,
	mod_display_names: &'a HashMap<String, String>,
	cache_game_version: &'a str,
	emit_options: &'a EmitOptions,
	provenance: bool,
	script_cache: &'a WorkspaceScriptCache,
	vanilla_base_mode: VanillaBaseMode,
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
struct MaterializeProgress<'a> {
	total: usize,
	current: usize,
	tty: bool,
	last_tick: Instant,
	progress: Option<(&'a dyn ProgressObserver, Instant)>,
}

impl<'a> MaterializeProgress<'a> {
	const TICK_EVERY: usize = 200;
	const TICK_INTERVAL_MS: u128 = 200;

	fn new(total: usize, progress: Option<(&'a dyn ProgressObserver, Instant)>) -> Self {
		Self {
			total,
			current: 0,
			tty: std::io::stderr().is_terminal(),
			last_tick: Instant::now(),
			progress,
		}
	}

	fn tick(&mut self) {
		self.current += 1;
		if let Some((observer, started)) = self.progress {
			observer.update(MergeProgress {
				stage: MergeAnalysisStage::SemanticMerge,
				completed: false,
				completed_units: Some(self.current as u64),
				total_units: Some(self.total as u64),
				elapsed: started.elapsed(),
			});
		}
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
	use super::{
		MaterializeOutput, MergeMaterializeOptions, materialize_merge_internal,
		materialize_merge_with_workspace_result, materialize_prepared_merge_with_workspace_result,
	};
	use crate::config::Config;
	use crate::emit::EmitOptions;
	use crate::merge::execute::CancellationToken;
	use crate::merge::model::{ExternalFileResolution, VanillaBaseMode};
	use crate::request::CheckRequest;
	use crate::workspace::{
		ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveError,
		WorkspaceResolveErrorKind,
	};
	use foch::game::eu4::Eu4;
	use foch::model::{
		DeferredUnitReason, HandlerResolutionRecord, MERGE_PLAN_ARTIFACT_PATH,
		MERGE_REPORT_ARTIFACT_PATH, MERGED_MOD_DESCRIPTOR_PATH, MergePlanContributor,
		MergePlanEntry, MergePlanResult, MergePlanStrategy, MergePlanTarget, MergeReport,
		MergeReportStatus, MergeTraceDecision, MergeTraceEntry, MergeTracePolicy, MergeUnitId,
	};
	use foch::playset::Playset;
	use foch::project::{ResolutionDecision, ResolutionMap};
	use foch_language::analyzer::content_family::{
		ContentFamilyDescriptor, GameProfile, MergeKeySource,
	};
	use foch_language::analyzer::parser::{AstStatement, parse_clausewitz_content};
	use serde_json::json;
	use std::cell::Cell;
	use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
	use std::fs;
	use std::path::{Path, PathBuf};
	use tempfile::TempDir;

	#[derive(Clone)]
	struct FixedStructuralBackend {
		output: super::StructuralMergeOutput,
	}

	impl crate::merge::backend::MergeBackend for FixedStructuralBackend {
		fn descriptor(&self) -> crate::merge::backend::MergeBackendDescriptor {
			crate::merge::backend::MergeBackendId::GumtreePcsNway.descriptor()
		}

		fn profile(&self) -> crate::merge::backend::BackendProfile {
			crate::merge::backend::BackendProfile {
				validate_semantic_units: false,
				duplicate_definition_override: None,
			}
		}

		fn analyze(
			&self,
			request: crate::merge::backend::BackendRequest<'_, '_>,
		) -> crate::merge::backend::BackendOutcome {
			match request.unit {
				crate::merge::backend::BackendUnit::File(_) => Ok(self.output.clone()),
				crate::merge::backend::BackendUnit::DefinitionModule(_) => {
					panic!("ordinary structural regression must not use the module branch")
				}
			}
		}
	}

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
	fn failed_workspace_is_not_resolved_again_while_building_plan() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("dlc_load.json");
		let mod_root = temp.path().join("9701");
		write_dlc_load(&playlist_path, &[("9701", "A")]);
		write_descriptor(&mod_root, "A");
		write_file(
			&mod_root,
			"events/test.txt",
			"namespace = test\ncountry_event = { id = test.1 }\n",
		);

		let report = materialize_merge_with_workspace_result(
			request_for(&playlist_path),
			&temp.path().join("staging"),
			None,
			&temp.path().join("published"),
			no_base_options(false),
			Err(WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: temp.path().join("sentinel"),
				message: "sentinel resolution failure".to_string(),
			}),
		)
		.expect("resolution failure should produce a fatal report");

		assert_eq!(report.status, MergeReportStatus::Fatal);
		assert_eq!(
			report.fatal_reason.as_deref(),
			Some("sentinel resolution failure")
		);
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

	#[test]
	fn structured_validation_accepts_an_explicit_definition_module() {
		let entry = structured_definition_module_entry(true);
		let plan = MergePlanResult {
			paths: vec![entry.clone()],
			..MergePlanResult::default()
		};
		let retained =
			BTreeSet::from(["common/institutions/zzz_foch_institutions.txt".to_string()]);

		super::validate_structured_plan_selection(&plan, Some(&retained))
			.expect("select definition module");
		super::validate_structured_merge_entry(
			&entry,
			None,
			VanillaBaseMode::Required,
			foch_language::analyzer::eu4_profile::eu4_profile(),
		)
		.expect("validate definition module");
	}

	#[test]
	fn structured_validation_allows_a_retained_base_only_runtime_fallback() {
		let base = test_contributor("base", 0, true, false);
		let entry = copy_through_entry("common/powerprojection/00_static.txt", &base);
		let plan = MergePlanResult {
			paths: vec![entry],
			..MergePlanResult::default()
		};
		let retained = BTreeSet::from(["common/powerprojection/00_static.txt".to_string()]);

		super::validate_structured_plan_selection(&plan, Some(&retained))
			.expect("base-only retained paths use the unchanged runtime base");
	}

	#[test]
	fn structured_validation_rejects_a_retained_mod_copy_through() {
		let contributor = test_contributor("mod", 1, false, false);
		let entry = copy_through_entry("common/powerprojection/00_static.txt", &contributor);
		let plan = MergePlanResult {
			paths: vec![entry],
			..MergePlanResult::default()
		};
		let retained = BTreeSet::from(["common/powerprojection/00_static.txt".to_string()]);

		let error = super::validate_structured_plan_selection(&plan, Some(&retained))
			.expect_err("mod input must exercise the candidate structural kernel");
		assert!(
			error
				.to_string()
				.contains("candidate kernel was not invoked")
		);
	}

	#[test]
	fn structured_definition_module_requires_a_vanilla_base() {
		let entry = structured_definition_module_entry(false);

		let error = super::validate_structured_merge_entry(
			&entry,
			None,
			VanillaBaseMode::Required,
			foch_language::analyzer::eu4_profile::eu4_profile(),
		)
		.expect_err("module without vanilla must be rejected");

		assert!(error.to_string().contains("vanilla definition module"));
	}

	#[test]
	fn structured_definition_module_allows_an_explicitly_disabled_vanilla_base() {
		let entry = structured_definition_module_entry(false);

		super::validate_structured_merge_entry(
			&entry,
			None,
			VanillaBaseMode::ExplicitlyDisabled,
			foch_language::analyzer::eu4_profile::eu4_profile(),
		)
		.expect("explicit empty base is allowed");
	}

	#[test]
	fn supported_file_families_treat_verified_missing_vanilla_as_known_absent() {
		let profile = foch_language::analyzer::eu4_profile::eu4_profile();
		let defines = profile.classify_content_family(Path::new("common/defines/es_defines.lua"));
		let events = profile.classify_content_family(Path::new("events/test.txt"));
		let gfx =
			profile.classify_content_family(Path::new("interface/000_expanded_mod_family.gfx"));
		let governments = profile.classify_content_family(Path::new("common/governments/test.txt"));
		let contributors = [
			test_contributor("left", 1, false, false),
			test_contributor("right", 2, false, false),
		];

		assert_eq!(
			super::effective_vanilla_base_mode(
				defines,
				Some(&contributors),
				VanillaBaseMode::Required,
				true,
			),
			VanillaBaseMode::KnownAbsent
		);
		assert_eq!(
			super::effective_vanilla_base_mode(
				gfx,
				Some(&contributors),
				VanillaBaseMode::Required,
				true,
			),
			VanillaBaseMode::KnownAbsent
		);
		assert_eq!(
			super::effective_vanilla_base_mode(
				events,
				Some(&contributors),
				VanillaBaseMode::Required,
				true,
			),
			VanillaBaseMode::KnownAbsent
		);
		assert_eq!(
			super::effective_vanilla_base_mode(
				governments,
				Some(&contributors),
				VanillaBaseMode::Required,
				true,
			),
			VanillaBaseMode::Required
		);
		assert_eq!(
			super::effective_vanilla_base_mode(
				defines,
				Some(&contributors),
				VanillaBaseMode::ExplicitlyDisabled,
				false,
			),
			VanillaBaseMode::ExplicitlyDisabled
		);
		assert_eq!(
			super::effective_vanilla_base_mode(
				defines,
				Some(&contributors),
				VanillaBaseMode::Required,
				false,
			),
			VanillaBaseMode::Required
		);
	}

	fn structured_definition_module_entry(include_base: bool) -> MergePlanEntry {
		let mut contributors = [
			("left", "common/institutions/left.txt", 1, false),
			("right", "common/institutions/right.txt", 2, false),
		]
		.into_iter()
		.map(
			|(mod_id, source_path, precedence, is_base_game)| MergePlanContributor {
				mod_id: mod_id.to_string(),
				source_path: source_path.to_string(),
				precedence,
				is_base_game,
			},
		)
		.collect::<Vec<_>>();
		if include_base {
			contributors.insert(
				0,
				MergePlanContributor {
					mod_id: "__game__".to_string(),
					source_path: "common/institutions/00_core.txt".to_string(),
					precedence: 0,
					is_base_game: true,
				},
			);
		}
		MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: "institutions".to_string(),
					module_name: "institutions".to_string(),
				},
				input_paths: vec![
					"common/institutions/00_core.txt".to_string(),
					"common/institutions/left.txt".to_string(),
					"common/institutions/right.txt".to_string(),
				],
				output_path: "common/institutions/zzz_foch_institutions.txt".to_string(),
				replace_prefix: None,
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors,
			winner: None,
			notes: Vec::new(),
		}
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
			playlist: Playset {
				game: Eu4,
				name: "test".to_string(),
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
			resolution_map: foch::project::ResolutionMap::default(),
			emit_options: EmitOptions::default(),
			frozen_external_files: BTreeMap::new(),
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			provenance: false,
			backend: Box::new(crate::merge::backend::AddressPatchBackend),
			retained_paths: None,
			cancellation: CancellationToken::new(),
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

	fn structural_merge_output(rendered: &str) -> super::StructuralMergeOutput {
		super::StructuralMergeOutput {
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
			provenance_localisation: BTreeMap::new(),
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
			playlist: Playset {
				game: Eu4,
				name: "cross-file-noop".to_string(),
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

	fn ordinary_structural_fixture(
		test_root: &Path,
		output: super::StructuralMergeOutput,
	) -> (MergeReport, PathBuf, &'static str) {
		const TARGET: &str = "common/diplomatic_actions/ordinary.txt";
		let workspace = cross_file_workspace(
			test_root,
			&[
				("mod_a", TARGET, "from a\n", 10, false),
				("mod_b", TARGET, "from b\n", 20, false),
			],
		);
		let contributors = workspace.file_inventory[TARGET]
			.iter()
			.map(|contributor| MergePlanContributor {
				mod_id: contributor.mod_id.clone(),
				source_path: contributor.absolute_path.to_string_lossy().into_owned(),
				precedence: contributor.precedence,
				is_base_game: contributor.is_base_game,
			})
			.collect::<Vec<_>>();
		let plan = MergePlanResult {
			playset_name: "ordinary-structural-provenance".to_string(),
			paths: vec![MergePlanEntry {
				target: MergePlanTarget::File {
					path: TARGET.to_string(),
				},
				strategy: MergePlanStrategy::StructuralMerge,
				contributors,
				winner: None,
				notes: Vec::new(),
			}],
			..MergePlanResult::default()
		};
		let out_dir = test_root.join("out");
		let mut options = no_base_options(false);
		options.provenance = true;
		options.backend = Box::new(FixedStructuralBackend { output });
		let report = materialize_prepared_merge_with_workspace_result(
			request_for(&workspace.playlist_path),
			MaterializeOutput {
				artifacts_dir: &out_dir,
				prior_dir: None,
				target_dir: &out_dir,
			},
			options,
			Ok(workspace),
			plan,
			None,
		)
		.expect("materialize ordinary structural fixture");
		(report, out_dir, TARGET)
	}

	fn structural_output_with_provenance(target: &str) -> super::StructuralMergeOutput {
		let mut output = structural_merge_output(
			"send_warning = { condition = { tooltip = FOCH_PROVENANCE_fixed } }\n",
		);
		output.provenance_localisation.insert(
			"FOCH_PROVENANCE_fixed".to_string(),
			"$ORIGINAL_TT$\\n\\nBase: Mod A".to_string(),
		);
		output
			.definition_provenance
			.insert("send_warning".to_string(), vec!["mod_a".to_string()]);
		output.merge_trace.insert(
			"send_warning".to_string(),
			MergeTraceEntry {
				contributors: Vec::new(),
				policy: MergeTracePolicy::Union,
				decision: MergeTraceDecision::Unioned,
			},
		);
		assert!(target.starts_with("common/diplomatic_actions/"));
		output
	}

	#[test]
	fn ordinary_structural_output_publishes_its_surviving_tooltip_localisation() {
		let temp = TempDir::new().expect("temp dir");
		let target = "common/diplomatic_actions/ordinary.txt";
		let output = structural_output_with_provenance(target);

		let (report, out_dir, target) = ordinary_structural_fixture(temp.path(), output);

		let script = fs::read_to_string(out_dir.join(target)).expect("read ordinary script");
		assert!(
			script.contains("tooltip = FOCH_PROVENANCE_fixed"),
			"{script}"
		);
		let localisation_paths = fs::read_dir(out_dir.join("localisation"))
			.expect("ordinary output must publish localisation")
			.map(|entry| entry.expect("localisation entry").path())
			.collect::<Vec<_>>();
		assert_eq!(localisation_paths.len(), 1, "{localisation_paths:?}");
		let localisation =
			fs::read_to_string(&localisation_paths[0]).expect("read provenance localisation");
		for language in ["l_english:", "l_german:", "l_french:", "l_spanish:"] {
			assert!(localisation.contains(language), "{localisation}");
		}
		assert_eq!(localisation.matches("FOCH_PROVENANCE_fixed").count(), 4);
		assert!(report.definition_provenance.contains_key(target));
		assert!(report.merge_trace.contains_key(target));
	}

	#[test]
	fn ordinary_external_write_publishes_no_stale_semantic_facts_or_tooltip_localisation() {
		let temp = TempDir::new().expect("temp dir");
		let target = "common/diplomatic_actions/ordinary.txt";
		let external_path = temp.path().join("external.txt");
		fs::write(&external_path, "external = yes\n").expect("write external payload");
		let mut output = structural_output_with_provenance(target);
		output.external_file_resolutions.insert(
			PathBuf::from(target),
			ExternalFileResolution::Live(external_path),
		);

		let (report, out_dir, target) = ordinary_structural_fixture(temp.path(), output);

		assert_eq!(
			fs::read_to_string(out_dir.join(target)).expect("read external output"),
			"external = yes\n",
		);
		assert!(!out_dir.join("localisation").exists());
		assert!(!report.definition_provenance.contains_key(target));
		assert!(!report.merge_trace.contains_key(target));
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
		let generated_paths = BTreeSet::from([generated_path.to_string()]);
		let mut counted_generated_paths = generated_paths.clone();
		let mut report = MergeReport {
			generated_file_count: 1,
			..MergeReport::default()
		};
		let prune_result = super::prune_cross_file_noop_duplicates(
			out_dir,
			generated_paths,
			workspace,
			foch_language::analyzer::eu4_profile::eu4_profile(),
			&mut report,
		)
		.expect("cross-file noop prune succeeds");
		let mut provenance_localisation = BTreeMap::new();
		super::reconcile_surviving_output_facts(
			&MergePlanResult::default(),
			&prune_result,
			&mut counted_generated_paths,
			&mut provenance_localisation,
			&mut report,
		);
		(prune_result.surviving_generated_paths, report)
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
	fn cross_file_noop_observes_pending_copy_through_mod_winner() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let copied_path = "common/scripted_effects/00_mod_winner.txt";
		let generated_path = "common/scripted_effects/zz_generated.txt";
		let content = "shared_effect = {\n\tadd_prestige = 1\n}\n";
		let workspace = cross_file_workspace(
			temp.path(),
			&[
				("mod_winner", copied_path, content, 1, false),
				("merged_input", generated_path, content, 2, false),
			],
		);
		let copied_entry =
			copy_through_entry(copied_path, &workspace.file_inventory[copied_path][0]);
		let mut pending_copy_through = Vec::new();
		let mut report = MergeReport {
			generated_file_count: 1,
			..MergeReport::default()
		};
		super::materialize_copy_through(
			&workspace,
			&copied_entry,
			&out_dir,
			false,
			&mut report,
			None,
			&mut pending_copy_through,
		)
		.expect("queue copy-through winner");
		write_file(&out_dir, generated_path, content);
		assert!(!out_dir.join(copied_path).exists());

		let prune_result = super::build_surviving_output_manifest(
			&workspace,
			&out_dir,
			&pending_copy_through,
			BTreeSet::from([generated_path.to_string()]),
			foch_language::analyzer::eu4_profile::eu4_profile(),
			&mut report,
		)
		.expect("flush pending winner before cross-file pruning");
		let mut counted_generated_paths = BTreeSet::from([generated_path.to_string()]);
		let mut provenance_localisation = BTreeMap::new();
		super::reconcile_surviving_output_facts(
			&MergePlanResult::default(),
			&prune_result,
			&mut counted_generated_paths,
			&mut provenance_localisation,
			&mut report,
		);

		assert_eq!(
			fs::read_to_string(out_dir.join(copied_path)).expect("read copied winner"),
			content
		);
		assert!(!out_dir.join(generated_path).exists());
		assert!(prune_result.surviving_generated_paths.is_empty());
		assert_eq!(
			prune_result.pruned_paths,
			BTreeSet::from([generated_path.to_string()])
		);
		assert_eq!(report.generated_file_count, 0);
		assert_eq!(report.copied_file_count, 1);
		assert_eq!(report.overlay_file_count, 0);
		assert_eq!(report.cross_file_noop_skipped_file_count, 1);
	}

	#[test]
	fn surviving_manifest_removes_pruned_module_publication_facts() {
		let temp = TempDir::new().expect("temp dir");
		let vanilla_path = "common/scripted_effects/00_vanilla.txt";
		let dropped_path = "common/scripted_effects/zzz_foch_dropped.txt";
		let kept_path = "common/scripted_triggers/zzz_foch_kept.txt";
		let duplicated_effect = "shared_effect = {\n\tadd_prestige = 1\n}\n";
		let kept_trigger = "kept_trigger = {\n\talways = yes\n}\n";
		let module_entry = |family: &str, path: &str, prefix: &str| MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: family.to_string(),
					module_name: family.to_string(),
				},
				input_paths: Vec::new(),
				output_path: path.to_string(),
				replace_prefix: Some(prefix.to_string()),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: Vec::new(),
			winner: None,
			notes: Vec::new(),
		};
		let plan = MergePlanResult {
			paths: vec![
				module_entry("scripted_effects", dropped_path, "common/scripted_effects"),
				module_entry("scripted_triggers", kept_path, "common/scripted_triggers"),
			],
			..MergePlanResult::default()
		};
		let trace = MergeTraceEntry {
			contributors: Vec::new(),
			policy: MergeTracePolicy::Union,
			decision: MergeTraceDecision::Unioned,
		};
		let mut report = MergeReport {
			generated_file_count: 2,
			copied_file_count: 3,
			overlay_file_count: 4,
			definition_module_generated_count: 2,
			definition_provenance: BTreeMap::from([
				(
					dropped_path.to_string(),
					BTreeMap::from([("dropped".to_string(), vec!["mod_a".to_string()])]),
				),
				(
					kept_path.to_string(),
					BTreeMap::from([("kept".to_string(), vec!["mod_b".to_string()])]),
				),
			]),
			merge_trace: BTreeMap::from([
				(
					dropped_path.to_string(),
					BTreeMap::from([("dropped".to_string(), trace.clone())]),
				),
				(
					kept_path.to_string(),
					BTreeMap::from([("kept".to_string(), trace)]),
				),
			]),
			..MergeReport::default()
		};
		let mut counted_generated_paths =
			BTreeSet::from([dropped_path.to_string(), kept_path.to_string()]);
		let mut localisation_by_script = BTreeMap::from([
			(
				dropped_path.to_string(),
				BTreeMap::from([("FOCH_DROPPED".to_string(), "dropped".to_string())]),
			),
			(
				kept_path.to_string(),
				BTreeMap::from([("FOCH_KEPT".to_string(), "kept".to_string())]),
			),
		]);
		let workspace = cross_file_workspace(
			temp.path(),
			&[
				("base_game", vanilla_path, duplicated_effect, 0, true),
				("merged_effects", dropped_path, duplicated_effect, 1, false),
				("merged_triggers", kept_path, kept_trigger, 2, false),
			],
		);
		write_file(temp.path(), dropped_path, duplicated_effect);
		write_file(temp.path(), kept_path, kept_trigger);
		let prune_result = super::prune_cross_file_noop_duplicates(
			temp.path(),
			BTreeSet::from([dropped_path.to_string(), kept_path.to_string()]),
			&workspace,
			foch_language::analyzer::eu4_profile::eu4_profile(),
			&mut report,
		)
		.expect("prune covered replacement module output");

		let replacement_prefixes = super::reconcile_surviving_output_facts(
			&plan,
			&prune_result,
			&mut counted_generated_paths,
			&mut localisation_by_script,
			&mut report,
		);
		let mut generated_paths = prune_result.surviving_generated_paths.clone();
		let localisation_path = super::write_surviving_provenance_localisation(
			temp.path(),
			&localisation_by_script,
			&generated_paths,
		)
		.expect("write surviving provenance localisation")
		.expect("kept script owns localisation");
		super::record_counted_generated_output(
			&localisation_path,
			&mut generated_paths,
			&mut counted_generated_paths,
			&mut report,
		);
		let descriptor_path = temp.path().join("descriptor.mod");
		super::io::write_generated_descriptor(
			temp.path(),
			&temp.path().join("playlist.json"),
			"test",
			&replacement_prefixes,
			&descriptor_path,
		)
		.expect("write descriptor from surviving module entries");
		let descriptor = fs::read_to_string(descriptor_path).expect("read descriptor");

		let localisation =
			fs::read_to_string(temp.path().join(&localisation_path)).expect("read localisation");
		assert!(localisation.contains("FOCH_KEPT"));
		assert!(!localisation.contains("FOCH_DROPPED"));
		assert!(!temp.path().join(dropped_path).exists());
		assert!(temp.path().join(kept_path).is_file());
		assert_eq!(report.generated_file_count, 2);
		assert_eq!(report.copied_file_count, 3);
		assert_eq!(report.overlay_file_count, 4);
		assert_eq!(report.cross_file_noop_skipped_file_count, 1);
		assert_eq!(report.definition_module_generated_count, 1);
		assert!(!report.merge_trace.contains_key(dropped_path));
		assert!(!report.definition_provenance.contains_key(dropped_path));
		assert!(!localisation_by_script.contains_key(dropped_path));
		assert!(report.merge_trace.contains_key(kept_path));
		assert!(report.definition_provenance.contains_key(kept_path));
		assert!(localisation_by_script.contains_key(kept_path));
		assert!(!descriptor.contains("replace_path=\"common/scripted_effects\""));
		assert!(descriptor.contains("replace_path=\"common/scripted_triggers\""));
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

		let mut merge_output = structural_merge_output("merged\n");
		merge_output
			.keep_existing_paths
			.insert(PathBuf::from(relative_path));
		let mut report = MergeReport::default();

		let materialization = super::write_structural_merge_output(
			relative_path,
			&mut merge_output,
			&staging_dir,
			Some(&prior_out_dir),
			&ResolutionMap::default(),
			&BTreeMap::new(),
			&mut report,
		)
		.expect("materialize keep existing");

		assert_eq!(
			materialization,
			super::StructuralOutputMaterialization::KeptExisting
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
	fn materialize_keep_existing_rejects_uncarried_provenance_localisation_dependencies() {
		let temp = TempDir::new().expect("temp dir");
		let prior_out_dir = temp.path().join("prior-out");
		let staging_dir = temp.path().join("staging");
		let relative_path = "common/diplomatic_actions/handler.txt";
		write_file(
			&prior_out_dir,
			relative_path,
			"send_warning = { condition = { tooltip = FOCH_PROVENANCE_deadbeef } }\n",
		);
		let mut merge_output = structural_merge_output("merged\n");
		merge_output
			.keep_existing_paths
			.insert(PathBuf::from(relative_path));
		let mut report = MergeReport::default();

		let error = super::write_structural_merge_output(
			relative_path,
			&mut merge_output,
			&staging_dir,
			Some(&prior_out_dir),
			&ResolutionMap::default(),
			&BTreeMap::new(),
			&mut report,
		)
		.expect_err("keep_existing must not orphan generated tooltip keys");

		assert!(
			error
				.to_string()
				.contains("without its exact generated localisation dependencies"),
			"{error}",
		);
		assert!(!staging_dir.join(relative_path).exists());
		assert!(report.handler_resolutions.is_empty());
	}

	#[test]
	fn materialize_file_level_keep_existing_resolution_skips_write_when_output_exists() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let relative_path = "common/ideas/file-level-handler.txt";
		write_file(&out_dir, relative_path, "existing\n");

		let mut merge_output = structural_merge_output("merged\n");
		let mut resolution_map = ResolutionMap::default();
		resolution_map.by_file.insert(
			PathBuf::from(relative_path),
			ResolutionDecision::KeepExisting,
		);
		let mut report = MergeReport::default();

		let materialization = super::write_structural_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			Some(&out_dir),
			&resolution_map,
			&BTreeMap::new(),
			&mut report,
		)
		.expect("materialize file-level keep existing");

		assert_eq!(
			materialization,
			super::StructuralOutputMaterialization::KeptExisting
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
		let mut merge_output = structural_merge_output("merged\n");
		merge_output
			.keep_existing_paths
			.insert(PathBuf::from(relative_path));
		let mut report = MergeReport::default();

		let materialization = super::write_structural_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			Some(&out_dir),
			&ResolutionMap::default(),
			&BTreeMap::new(),
			&mut report,
		)
		.expect("materialize normal write");

		assert_eq!(
			materialization,
			super::StructuralOutputMaterialization::NormalWrite
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
		let mut merge_output = structural_merge_output("merged\n");
		merge_output
			.handler_resolutions
			.push(HandlerResolutionRecord {
				path: relative_path.to_string(),
				action: "dep_implied".to_string(),
				source: Some("mod_a".to_string()),
				rationale: Some("mod mod_a declares dep on mod_b".to_string()),
			});
		let mut report = MergeReport::default();

		let materialization = super::write_structural_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			None,
			&ResolutionMap::default(),
			&BTreeMap::new(),
			&mut report,
		)
		.expect("materialize normal write");

		assert_eq!(
			materialization,
			super::StructuralOutputMaterialization::NormalWrite
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

		let mut merge_output = structural_merge_output("merged\n");
		merge_output.external_file_resolutions.insert(
			PathBuf::from(relative_path),
			ExternalFileResolution::Frozen(external_path.clone()),
		);
		let frozen_external_files =
			BTreeMap::from([(external_path.clone(), b"external\n".to_vec())]);
		fs::write(&external_path, "changed after prepare\n").expect("mutate external source");
		let mut report = MergeReport::default();

		let materialization = super::write_structural_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			None,
			&ResolutionMap::default(),
			&frozen_external_files,
			&mut report,
		)
		.expect("materialize external file");

		assert_eq!(
			materialization,
			super::StructuralOutputMaterialization::ExternalWrite
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
	fn materialize_live_external_file_ignores_frozen_payload_for_same_path() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let external_path = temp.path().join("external.txt");
		let relative_path = "common/ideas/external.txt";
		fs::write(&external_path, "live interactive bytes\n").expect("write live source");

		let mut merge_output = structural_merge_output("merged\n");
		merge_output.external_file_resolutions.insert(
			PathBuf::from(relative_path),
			ExternalFileResolution::Live(external_path.clone()),
		);
		let frozen_external_files =
			BTreeMap::from([(external_path, b"stale configured bytes\n".to_vec())]);
		let mut report = MergeReport::default();

		super::write_structural_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			None,
			&ResolutionMap::default(),
			&frozen_external_files,
			&mut report,
		)
		.expect("materialize live external file");

		assert_eq!(
			fs::read_to_string(out_dir.join(relative_path)).expect("read output"),
			"live interactive bytes\n"
		);
	}

	#[test]
	fn materialize_external_file_errors_when_external_missing() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let external_path = temp.path().join("missing-external.txt");
		let relative_path = "common/ideas/missing-external.txt";
		let mut merge_output = structural_merge_output("merged\n");
		merge_output.external_file_resolutions.insert(
			PathBuf::from(relative_path),
			ExternalFileResolution::Live(external_path.clone()),
		);
		let mut report = MergeReport::default();

		let err = super::write_structural_merge_output(
			relative_path,
			&mut merge_output,
			&out_dir,
			None,
			&ResolutionMap::default(),
			&BTreeMap::new(),
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
		assert_eq!(report.manual_conflict_count, 0);
		assert_eq!(report.unsupported_input_count, 1);
		assert_eq!(report.engine_failure_count, 0);
		assert_eq!(
			report.conflict_resolutions[0].deferred_reason,
			DeferredUnitReason::UnsupportedInput
		);
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

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
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

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
	#[test]
	fn reset_only_mod_publishes_overlay_module_replacement() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("powerprojection-a");
		let reset_mod = temp.path().join("powerprojection-reset");
		let mod_c = temp.path().join("powerprojection-c");
		let out_dir = temp.path().join("out");
		write_dlc_load(
			&playlist_path,
			&[
				("powerprojection-a", "A"),
				("powerprojection-reset", "Reset"),
				("powerprojection-c", "C"),
			],
		);
		write_descriptor(&mod_a, "powerprojection-a");
		write_descriptor(&reset_mod, "powerprojection-reset");
		fs::write(
			reset_mod.join("descriptor.mod"),
			"name=\"powerprojection-reset\"\nreplace_path=\"common/powerprojection\"\n",
		)
		.expect("write reset descriptor");
		write_descriptor(&mod_c, "powerprojection-c");
		write_file(
			&mod_a,
			"common/powerprojection/a.txt",
			"removed_by_reset = { yearly_decay = 1 }\n",
		);
		write_file(
			&mod_c,
			"common/powerprojection/c.txt",
			"survives_reset = { yearly_decay = 2 }\n",
		);

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize reset overlay module");

		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.definition_module_count, 1);
		assert_eq!(report.definition_module_generated_count, 1);
		let output =
			fs::read_to_string(out_dir.join("common/powerprojection/zzz_foch_powerprojection.txt"))
				.expect("read overlay module output");
		assert!(output.contains("survives_reset"), "output: {output}");
		assert!(!output.contains("removed_by_reset"), "output: {output}");
		let descriptor =
			fs::read_to_string(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH)).expect("read descriptor");
		assert!(
			descriptor.contains("replace_path=\"common/powerprojection\""),
			"descriptor: {descriptor}"
		);
		let plan = read_plan(&out_dir);
		assert!(matches!(
			&plan_entry_for(
				&plan,
				"common/powerprojection/zzz_foch_powerprojection.txt"
			)
			.target,
			MergePlanTarget::Module {
				replace_prefix: Some(prefix),
				..
			} if prefix == "common/powerprojection"
		));
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

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
	#[test]
	fn unresolved_structural_merge_defers_conflict_and_publishes_partial_output() {
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

		assert_eq!(report.status, MergeReportStatus::PartialSuccess);
		assert_eq!(report.manual_conflict_count, 1);
		assert!(!out_dir.join(DAG_CONFLICT_PATH).exists());
		assert!(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
		assert!(!out_dir.join("common/governments/stale-module.txt").exists());
		assert!(out_dir.join(MERGE_PLAN_ARTIFACT_PATH).is_file());
		assert!(out_dir.join(MERGE_REPORT_ARTIFACT_PATH).is_file());
		assert_eq!(report.conflict_resolutions.len(), 1);
		let resolution = &report.conflict_resolutions[0];
		assert!(resolution.reason.contains("unresolved conflict"));
		assert_eq!(resolution.leaf_conflicts.len(), 1);
		assert_eq!(resolution.leaf_conflicts[0].address_key, "group");
	}

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
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
