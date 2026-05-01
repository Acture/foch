#![allow(dead_code)]

use super::conflict_handler::{DeferHandler, DepImpliesResolutionHandler};
use super::dag::{
	DagDiagnostic, DagDiagnosticKind, IgnoreReplacePath, ModDag, ModId, build_mod_dag,
};
use super::emit::{EmitOptions, emit_clausewitz_statements_with_options};
use super::error::MergeError;
use super::localisation_merge::{LocalisationMergeOutcome, merge_localisation_file};
#[allow(unused_imports)]
use super::namespace::{build_family_key_index, detect_key_conflicts, group_by_family};
use super::patch::ClausewitzPatch;
use super::patch_deps::compute_dag_patches_with_handler;
use super::patch_merge::PatchResolution;
use super::plan::build_merge_plan_from_workspace;
use super::stale_vanilla::detect_stale_vanilla_targets;
use crate::request::{CheckRequest, MergePlanOptions};
use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace, resolve_workspace};
use foch_core::config::{AppliedDepOverride, DepOverride, FochConfig};
use foch_core::model::{
	CheckContext, DepMisuseFinding, HandlerResolutionRecord, MERGE_PLAN_ARTIFACT_PATH,
	MERGE_REPORT_ARTIFACT_PATH, MERGED_MOD_DESCRIPTOR_PATH, MergePlanContributor, MergePlanEntry,
	MergePlanResult, MergePlanStrategy, MergeReport, MergeReportConflictContributor,
	MergeReportConflictKind, MergeReportConflictResolution, MergeReportStatus, SemanticIndex,
	StaleVanillaTargetDescriptor,
};
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, GameProfile, MergeKeySource,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::rules::{detect_dependency_misuse, detect_version_mismatch};
use foch_language::analyzer::semantic_index::{ParsedScriptFile, parse_script_file};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct MergeMaterializeOptions {
	pub include_game_base: bool,
	pub force: bool,
	pub ignore_replace_path: bool,
	pub fallback: bool,
	pub dep_overrides: Vec<AppliedDepOverride>,
	pub resolution_map: foch_core::config::ResolutionMap,
}

impl Default for MergeMaterializeOptions {
	fn default() -> Self {
		Self {
			include_game_base: true,
			force: false,
			ignore_replace_path: false,
			fallback: false,
			dep_overrides: Vec::new(),
			resolution_map: foch_core::config::ResolutionMap::default(),
		}
	}
}

pub(crate) fn materialize_merge_internal(
	request: CheckRequest,
	out_dir: &Path,
	options: MergeMaterializeOptions,
) -> Result<MergeReport, MergeError> {
	let mut report = MergeReport::default();
	let mut generated_paths = BTreeSet::new();

	let plan = match resolve_workspace(&request, options.include_game_base) {
		Ok(workspace) => build_merge_plan_from_workspace(&workspace, options.include_game_base),
		Err(_) => crate::run_merge_plan_with_options(
			request.clone(),
			MergePlanOptions {
				include_game_base: options.include_game_base,
			},
		),
	};
	report.manual_conflict_count = plan.strategies.manual_conflict;

	if plan.has_fatal_errors() {
		report.status = MergeReportStatus::Fatal;
		write_metadata_only(out_dir, &plan, &report)?;
		return Ok(report);
	}

	let workspace = resolve_workspace(&request, options.include_game_base)?;
	let (mod_dag, dag_diagnostics) = build_mod_dag(&workspace.mods);
	record_dag_diagnostics(&mut report, &dag_diagnostics);
	let analyzer_context = dependency_misuse_context(&workspace);
	report.dep_misuse = detect_dependency_misuse(&analyzer_context);
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
	let emit_options = load_emit_options(&request)?;

	for entry in &plan.paths {
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
									"localisation merge fallback for {}: {err}",
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
							let result = std::panic::catch_unwind(|| {
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
									emit_options: &emit_options,
								};
								patch_based_structural_merge(&target, &contribs, context)
							});
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
										&merge_output,
										out_dir,
										&mut report,
									)?;
									if materialization.counts_as_generated() {
										generated_paths.insert(entry.path.clone());
										report.generated_file_count += 1;
									} else if materialization.counts_as_noop_skipped() {
										report.noop_skipped_file_count += 1;
									}
									continue;
								}
								Ok(Err(err)) => {
									let reason = format!("patch merge failed: {err}");
									if resolve_structural_merge_failure(
										&workspace,
										entry,
										out_dir,
										&reason,
										&options,
										&mut report,
										&mut generated_paths,
									)? {
										continue;
									}
								}
								Err(_) => {
									let reason = "patch merge panicked".to_string();
									if resolve_structural_merge_failure(
										&workspace,
										entry,
										out_dir,
										&reason,
										&options,
										&mut report,
										&mut generated_paths,
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

fn resolve_structural_merge_failure(
	workspace: &ResolvedWorkspace,
	entry: &MergePlanEntry,
	out_dir: &Path,
	reason: &str,
	options: &MergeMaterializeOptions,
	report: &mut MergeReport,
	generated_paths: &mut BTreeSet<String>,
) -> Result<bool, MergeError> {
	if options.fallback || options.force {
		let resolution = write_last_writer_fallback(workspace, entry, out_dir, reason)?;
		report.fallback_resolved_count += 1;
		report.generated_file_count += 1;
		generated_paths.insert(entry.path.clone());
		report.warnings.push(format!(
			"{}; using last-writer fallback for {}",
			reason, entry.path
		));
		report.conflict_resolutions.push(resolution);
		return Ok(true);
	}

	report.warnings.push(format!(
		"{} for {}; fallback disabled, skipping output",
		reason, entry.path
	));
	report.manual_conflict_count += 1;
	report
		.conflict_resolutions
		.push(workspace_conflict_skipped_resolution(
			workspace, entry, reason,
		));
	Ok(true)
}

fn write_last_writer_fallback(
	workspace: &ResolvedWorkspace,
	entry: &MergePlanEntry,
	out_dir: &Path,
	reason: &str,
) -> Result<MergeReportConflictResolution, MergeError> {
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
	let winner = last_writer_contributor(contributors).ok_or_else(|| MergeError::Validation {
		path: Some(entry.path.clone()),
		message: format!("no mod contributor available for {}", entry.path),
	})?;
	let winner_bytes = fs::read(&winner.absolute_path)?;
	let marker_prefix = conflict_comment_prefix_for_path(&entry.path);
	let marker_written = marker_prefix.is_some();

	let target = out_dir.join(&entry.path);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}

	if let Some(prefix) = marker_prefix {
		let marker = fallback_marker(workspace, contributors, winner, reason, prefix);
		let mut output = marker.into_bytes();
		output.extend_from_slice(&winner_bytes);
		fs::write(target, output)?;
	} else {
		fs::write(target, winner_bytes)?;
	}

	Ok(MergeReportConflictResolution {
		path: entry.path.clone(),
		kind: MergeReportConflictKind::LastWriterFallback,
		reason: reason.to_string(),
		winning_mod: contributor_label(workspace, winner),
		marker_written,
		contributors: report_contributors(workspace, contributors),
	})
}

fn workspace_conflict_skipped_resolution(
	workspace: &ResolvedWorkspace,
	entry: &MergePlanEntry,
	reason: &str,
) -> MergeReportConflictResolution {
	let contributors = workspace.file_inventory.get(&entry.path);
	let winner = contributors.and_then(|items| last_writer_contributor(items));
	MergeReportConflictResolution {
		path: entry.path.clone(),
		kind: MergeReportConflictKind::TrueConflictSkipped,
		reason: reason.to_string(),
		winning_mod: winner
			.map(|contributor| contributor_label(workspace, contributor))
			.unwrap_or_default(),
		marker_written: false,
		contributors: contributors
			.map(|items| report_contributors(workspace, items))
			.unwrap_or_default(),
	}
}

fn plan_conflict_skipped_resolution(
	entry: &MergePlanEntry,
	reason: &str,
) -> MergeReportConflictResolution {
	MergeReportConflictResolution {
		path: entry.path.clone(),
		kind: MergeReportConflictKind::TrueConflictSkipped,
		reason: reason.to_string(),
		winning_mod: entry
			.winner
			.as_ref()
			.map(|winner| format!("{}:unknown", winner.mod_id))
			.unwrap_or_default(),
		marker_written: false,
		contributors: entry
			.contributors
			.iter()
			.filter(|contributor| !contributor.is_base_game)
			.map(|contributor| MergeReportConflictContributor {
				mod_id: contributor.mod_id.clone(),
				mod_version: "unknown".to_string(),
				precedence: contributor.precedence,
			})
			.collect(),
	}
}

fn fallback_marker(
	workspace: &ResolvedWorkspace,
	contributors: &[ResolvedFileContributor],
	winner: &ResolvedFileContributor,
	reason: &str,
	prefix: &str,
) -> String {
	let contributors = active_mod_contributors(contributors)
		.into_iter()
		.map(|contributor| contributor_label(workspace, contributor))
		.collect::<Vec<_>>()
		.join(", ");
	format!(
		"{prefix} foch:conflict reason=\"{}\" resolved=\"last-writer:{}\"\n{prefix} foch:conflict contributors=[{}]\n",
		short_marker_reason(reason),
		contributor_label(workspace, winner),
		contributors
	)
}

fn conflict_comment_prefix_for_path(path: &str) -> Option<&'static str> {
	let normalized = path.to_ascii_lowercase();
	let ext = normalized.rsplit('.').next()?;
	match ext {
		"txt" | "gui" | "yml" | "yaml" | "gfx" | "asset" | "mod" => Some("#"),
		_ => None,
	}
}

fn short_marker_reason(reason: &str) -> String {
	let normalized = reason.split_whitespace().collect::<Vec<_>>().join(" ");
	let mut shortened = normalized.chars().take(160).collect::<String>();
	if normalized.chars().count() > 160 {
		shortened.push('…');
	}
	shortened.replace('"', "'")
}

fn last_writer_contributor(
	contributors: &[ResolvedFileContributor],
) -> Option<&ResolvedFileContributor> {
	contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
		.max_by(|a, b| {
			a.precedence
				.cmp(&b.precedence)
				.then_with(|| a.mod_id.cmp(&b.mod_id))
		})
}

fn active_mod_contributors(
	contributors: &[ResolvedFileContributor],
) -> Vec<&ResolvedFileContributor> {
	let mut active = contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
		.collect::<Vec<_>>();
	active.sort_by(|a, b| {
		a.precedence
			.cmp(&b.precedence)
			.then_with(|| a.mod_id.cmp(&b.mod_id))
	});
	active
}

fn report_contributors(
	workspace: &ResolvedWorkspace,
	contributors: &[ResolvedFileContributor],
) -> Vec<MergeReportConflictContributor> {
	active_mod_contributors(contributors)
		.into_iter()
		.map(|contributor| MergeReportConflictContributor {
			mod_id: contributor.mod_id.clone(),
			mod_version: contributor_version(workspace, &contributor.mod_id),
			precedence: contributor.precedence,
		})
		.collect()
}

fn contributor_label(
	workspace: &ResolvedWorkspace,
	contributor: &ResolvedFileContributor,
) -> String {
	format!(
		"{}:{}",
		contributor.mod_id,
		contributor_version(workspace, &contributor.mod_id)
	)
}

fn contributor_version(workspace: &ResolvedWorkspace, mod_id: &str) -> String {
	workspace
		.mods
		.iter()
		.find(|candidate| candidate.mod_id == mod_id)
		.and_then(|candidate| candidate.descriptor.as_ref())
		.and_then(|descriptor| descriptor.version.as_deref())
		.map(str::trim)
		.filter(|version| !version.is_empty())
		.unwrap_or("unknown")
		.to_string()
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
	emit_options: &'a EmitOptions,
}

/// Patch-based structural merge: walk the dependency DAG level by level, diff
/// every mod in a level against the same running base, sibling-merge that
/// level's patches, then apply the resolved level to advance the running state.
fn patch_based_structural_merge(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	context: PatchBasedMergeContext<'_>,
) -> Result<PatchBasedMergeOutput, MergeError> {
	let mut handler = super::conflict_handler::ChainHandler {
		first: super::conflict_handler::LookupHandler::with_display_names(
			context.resolution_map,
			PathBuf::from(target_path),
			(*context.mod_display_names).clone(),
		),
		second: super::conflict_handler::ChainHandler {
			first: DepImpliesResolutionHandler::from_mod_dag(
				PathBuf::from(target_path),
				context.mod_dag,
				context.dep_overrides,
			),
			second: DeferHandler,
		},
	};
	let dag_patches = compute_dag_patches_with_handler(
		target_path,
		contributors,
		context.merge_key_source,
		&context.descriptor.merge_policies,
		context.mod_dag,
		context.ignore_replace_path,
		context.dep_overrides,
		&mut handler,
	)
	.map_err(|err| MergeError::Validation {
		path: Some(target_path.to_string()),
		message: format!("patch computation failed: {err}"),
	})?;
	let vanilla = parse_vanilla_for_stale_detection(target_path, contributors)?;
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
		return Err(MergeError::Validation {
			path: Some(target_path.to_string()),
			message: format!(
				"patch merge has {} unresolved conflict(s): {}",
				conflict_keys.len(),
				conflict_keys.join("; "),
			),
		});
	}

	let rendered = emit_clausewitz_statements_with_options(
		&dag_patches.merged_statements,
		context.emit_options,
	)?;
	let noop_vs_vanilla = vanilla
		.as_ref()
		.map(|base| {
			super::patch::ast_statement_lists_semantically_equal(
				&base.ast.statements,
				&dag_patches.merged_statements,
			)
		})
		.unwrap_or(false);
	Ok(PatchBasedMergeOutput {
		rendered,
		dep_remove_counts,
		stale_vanilla_targets,
		handler_resolutions: merge_result.handler_resolutions,
		external_file_resolutions: merge_result.external_file_resolutions,
		keep_existing_paths: merge_result.keep_existing_paths,
		noop_vs_vanilla,
	})
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
}

fn write_patch_merge_output(
	target_path: &str,
	merge_output: &PatchBasedMergeOutput,
	out_dir: &Path,
	report: &mut MergeReport,
) -> Result<PatchOutputMaterialization, MergeError> {
	let output_relative_path = PathBuf::from(target_path);
	let target = out_dir.join(target_path);

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
	path.to_string_lossy().replace('\\', "/")
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

#[cfg(test)]
mod tests {
	use super::{MergeMaterializeOptions, materialize_merge_internal};
	use crate::config::Config;
	use crate::request::CheckRequest;
	use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace};
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::game::Game;
	use foch_core::domain::playlist::{Playlist, PlaylistEntry};
	use foch_core::model::{
		HandlerResolutionRecord, MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH,
		MERGED_MOD_DESCRIPTOR_PATH, MergePlanEntry, MergePlanResult, MergeReport,
		MergeReportConflictKind, MergeReportStatus, ModCandidate,
	};
	use serde_json::json;
	use std::collections::{BTreeMap, HashMap, HashSet};
	use std::fs;
	use std::path::{Path, PathBuf};
	use tempfile::TempDir;

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
				mod_root.display()
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
			fallback: false,
			dep_overrides: Vec::new(),
			resolution_map: foch_core::config::ResolutionMap::default(),
		}
	}

	fn no_base_options_with_fallback(force: bool, fallback: bool) -> MergeMaterializeOptions {
		MergeMaterializeOptions {
			include_game_base: false,
			force,
			ignore_replace_path: false,
			fallback,
			dep_overrides: Vec::new(),
			resolution_map: foch_core::config::ResolutionMap::default(),
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
		}
	}

	const DAG_FALLBACK_PATH: &str = "common/ideas/fallback.txt";

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

		let materialization =
			super::write_patch_merge_output(relative_path, &merge_output, &out_dir, &mut report)
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
	fn materialize_keep_existing_falls_through_when_output_missing() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let relative_path = "common/ideas/missing.txt";
		let mut merge_output = patch_merge_output("merged\n");
		merge_output
			.keep_existing_paths
			.insert(PathBuf::from(relative_path));
		let mut report = MergeReport::default();

		let materialization =
			super::write_patch_merge_output(relative_path, &merge_output, &out_dir, &mut report)
				.expect("materialize fallback write");

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

		let materialization =
			super::write_patch_merge_output(relative_path, &merge_output, &out_dir, &mut report)
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

		let materialization =
			super::write_patch_merge_output(relative_path, &merge_output, &out_dir, &mut report)
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

		let err =
			super::write_patch_merge_output(relative_path, &merge_output, &out_dir, &mut report)
				.expect_err("missing external source should error");

		assert!(
			err.to_string()
				.contains("failed to read external resolution source")
		);
		assert!(!out_dir.join(relative_path).exists());
		assert!(report.handler_resolutions.is_empty());
	}

	fn stage_dag_fallback_conflict(
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
		write_descriptor(mod_base, "fallback-base");
		write_descriptor_with_dependencies(mod_a, "fallback-a", &["fallback-base"]);
		write_descriptor_with_dependencies(mod_b, "fallback-b", &["fallback-base"]);
		write_descriptor_with_dependencies(mod_c, "fallback-c", &["fallback-a", "fallback-b"]);
		write_file(mod_base, DAG_FALLBACK_PATH, idea_file("old"));
		write_file(mod_a, DAG_FALLBACK_PATH, idea_file("alpha"));
		write_file(mod_b, DAG_FALLBACK_PATH, idea_file("beta"));
		write_file(mod_c, DAG_FALLBACK_PATH, idea_file("gamma"));
	}

	/// Same as `stage_dag_fallback_conflict` but without the downstream resolver
	/// mod C. Yields a genuine sibling-overwrite conflict between mods A and B
	/// that the DAG topo walk cannot auto-resolve, so the fallback path applies.
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
		write_descriptor(mod_base, "fallback-base");
		write_descriptor_with_dependencies(mod_a, "fallback-a", &["fallback-base"]);
		write_descriptor_with_dependencies(mod_b, "fallback-b", &["fallback-base"]);
		write_file(mod_base, DAG_FALLBACK_PATH, idea_file("old"));
		write_file(mod_a, DAG_FALLBACK_PATH, idea_file("alpha"));
		write_file(mod_b, DAG_FALLBACK_PATH, idea_file("beta"));
	}

	fn fallback_mod_candidate(mod_id: &str, name: &str, version: &str) -> ModCandidate {
		ModCandidate {
			entry: PlaylistEntry {
				steam_id: Some(mod_id.to_string()),
				..PlaylistEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: name.to_string(),
				version: Some(version.to_string()),
				..ModDescriptor::default()
			}),
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn fallback_workspace(
		test_root: &Path,
		relative_path: &str,
		mod_a_content: impl AsRef<[u8]>,
		mod_b_content: impl AsRef<[u8]>,
	) -> ResolvedWorkspace {
		let mod_a = test_root.join("9201");
		let mod_b = test_root.join("9202");
		write_file(&mod_a, relative_path, mod_a_content);
		write_file(&mod_b, relative_path, mod_b_content);

		let mut file_inventory = BTreeMap::new();
		file_inventory.insert(
			relative_path.to_string(),
			vec![
				ResolvedFileContributor {
					mod_id: "9201".to_string(),
					root_path: mod_a.clone(),
					absolute_path: mod_a.join(relative_path),
					precedence: 1,
					is_base_game: false,
					is_synthetic_base: false,
					parse_ok_hint: None,
				},
				ResolvedFileContributor {
					mod_id: "9202".to_string(),
					root_path: mod_b.clone(),
					absolute_path: mod_b.join(relative_path),
					precedence: 2,
					is_base_game: false,
					is_synthetic_base: false,
					parse_ok_hint: None,
				},
			],
		);

		ResolvedWorkspace {
			playlist_path: test_root.join("playlist.json"),
			playlist: Playlist {
				game: Game::EuropaUniversalis4,
				name: "fallback-workspace".to_string(),
				mods: Vec::new(),
			},
			mods: vec![
				fallback_mod_candidate("9201", "fallback-a", "1.0.0"),
				fallback_mod_candidate("9202", "fallback-b", "2.0.0"),
			],
			installed_base_snapshot: None,
			mod_snapshots: Vec::new(),
			file_inventory,
		}
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
	fn last_writer_fallback_writes_file_with_marker() {
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
			no_base_options_with_fallback(false, true),
		)
		.expect("materialize");

		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.manual_conflict_count, 0);
		assert_eq!(report.fallback_resolved_count, 1);
		assert_eq!(report.generated_file_count, 1);
		let output = fs::read_to_string(out_dir.join(DAG_FALLBACK_PATH)).expect("read fallback");
		assert!(output.starts_with("# foch:conflict reason=\"patch merge failed:"));
		assert!(output.contains("resolved=\"last-writer:9103:1.0.0\""));
		assert!(
			output.contains("# foch:conflict contributors=[9101:1.0.0, 9102:1.0.0, 9103:1.0.0]")
		);
		assert!(output.ends_with(&idea_file("beta")));
		assert_eq!(report.conflict_resolutions.len(), 1);
		let resolution = &report.conflict_resolutions[0];
		assert_eq!(resolution.kind, MergeReportConflictKind::LastWriterFallback);
		assert_eq!(resolution.path, DAG_FALLBACK_PATH);
		assert_eq!(resolution.winning_mod, "9103:1.0.0");
		assert!(resolution.marker_written);
	}

	#[test]
	fn last_writer_fallback_binary_file_no_marker() {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let relative_path = "gfx/interface/icon.dds";
		let workspace = fallback_workspace(temp.path(), relative_path, [0u8, 1, 2], [3u8, 4, 5]);
		let entry = MergePlanEntry {
			path: relative_path.to_string(),
			..MergePlanEntry::default()
		};

		let resolution = super::write_last_writer_fallback(
			&workspace,
			&entry,
			&out_dir,
			"synthetic binary conflict",
		)
		.expect("fallback");

		let output = fs::read(out_dir.join(relative_path)).expect("read binary fallback");
		assert_eq!(output, vec![3u8, 4, 5]);
		assert_eq!(resolution.kind, MergeReportConflictKind::LastWriterFallback);
		assert_eq!(resolution.winning_mod, "9202:2.0.0");
		assert!(!resolution.marker_written);
	}

	#[test]
	fn unresolved_structural_merge_skips_without_fallback_by_default() {
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
		assert_eq!(report.fallback_resolved_count, 0);
		assert!(!out_dir.join(DAG_FALLBACK_PATH).exists());
		assert_eq!(report.conflict_resolutions.len(), 1);
		assert_eq!(
			report.conflict_resolutions[0].kind,
			MergeReportConflictKind::TrueConflictSkipped
		);
	}

	#[test]
	fn force_mode_implies_last_writer_fallback() {
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

		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.manual_conflict_count, 0);
		assert_eq!(report.fallback_resolved_count, 1);
		assert!(out_dir.join(DAG_FALLBACK_PATH).exists());
	}

	#[test]
	fn downstream_mod_resolves_upstream_sibling_conflict() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let out_dir = temp.path().join("out");
		// Mod C declares deps on both A and B and writes its own value at the
		// same address. The DAG topo walk should recognize C as a downstream
		// override of the A/B sibling-overwrite conflict and emit C's value
		// without invoking fallback.
		stage_dag_fallback_conflict(
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
		assert_eq!(report.fallback_resolved_count, 0);
		assert_eq!(report.generated_file_count, 1);
		let output =
			fs::read_to_string(out_dir.join(DAG_FALLBACK_PATH)).expect("read merged output");
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
