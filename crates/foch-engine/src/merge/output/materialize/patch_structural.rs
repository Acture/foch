use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use foch_core::config::compute_conflict_id;
use foch_core::model::{
	LeafConflictDetail, MergeReportConflictContributor, MergeTraceContributor, MergeTraceDecision,
	MergeTraceEntry, MergeTracePolicy,
};
use foch_language::analyzer::content_family::{
	BlockPatchPolicy, ContentFamilyDescriptor, ContentLoadPolicy, MergePolicies,
	NamedContainerPolicy,
};
use foch_language::analyzer::parser::{AstStatement, Span, SpanRange};

use super::per_entry_noop::drop_per_entry_noop_duplicates;
use super::stale_detect::{
	collect_dep_misuse_remove_counts, collect_stale_vanilla_targets,
	parse_vanilla_for_stale_detection, vanilla_snippet_for_address,
};
use super::{
	PatchBasedMergeContext, PatchBasedMergeFailure, PatchBasedMergeOutput, PatchConflictReport,
};
use crate::emit::{EmitOptions, EmitOrdering, emit_clausewitz_statements_with_options};
use crate::merge::MergeKernelMode;
use crate::merge::cwt_suggestions::classify_conflict_kind;
use crate::workspace::ResolvedFileContributor;
use foch_cwt::CwtSchemaGraph;

use super::super::super::conflict_handler::{
	ChainHandler, ConflictHandler, DeferHandler, DepImpliesResolutionHandler, LookupHandler,
	PriorityBoostResolutionHandler, PromptOutcomeKind, prompt_survivors_and_persist,
};
use super::super::super::conflict_view::build_conflict_view;
use super::super::super::error::MergeError;
use super::super::super::patch_deps::{
	DagPatchComputation, DagPatchRequest, compute_dag_patches_from_parsed_with_kernel,
	compute_dag_patches_with_handler_and_kernel,
};
use super::super::super::patch_merge::{
	AttributedPatch, PatchAddress, PatchConflict, PatchResolution,
};
use crate::merge::planning::module_view::CrossFileModuleViews;

fn leaf_conflicts_for_unresolved(
	target_path: &str,
	conflicts: &[PatchResolution],
	mod_versions: &HashMap<String, String>,
	cwt_schema_graph: Option<&CwtSchemaGraph>,
) -> Vec<LeafConflictDetail> {
	conflicts
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Conflict {
				address,
				patches,
				reason,
			} => {
				let address_path = address.path.join("/");
				let ast_path = address.path.iter().map(String::as_str).collect::<Vec<_>>();
				Some(LeafConflictDetail {
					address_path: address_path.clone(),
					address_key: address.key.clone(),
					conflict_id: compute_conflict_id(
						Path::new(target_path),
						&address_path,
						&address.key,
					),
					kind: cwt_schema_graph.and_then(|graph| {
						classify_conflict_kind(graph, Path::new(target_path), &ast_path, reason)
					}),
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
pub(super) fn patch_based_structural_merge(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	context: PatchBasedMergeContext<'_>,
	interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
) -> Result<PatchBasedMergeOutput, PatchBasedMergeFailure> {
	let vanilla =
		parse_vanilla_for_stale_detection(target_path, contributors, context.script_cache)?;
	finish_patch_based_merge(
		target_path,
		contributors,
		context,
		vanilla,
		interactive_handler,
		interactive_config_path,
		|resolution_map, context| {
			run_patch_merge_engine(target_path, contributors, context, resolution_map)
		},
	)
}

pub(super) fn patch_based_cross_file_module_merge(
	target_path: &str,
	views: &CrossFileModuleViews,
	context: PatchBasedMergeContext<'_>,
	interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
) -> Result<PatchBasedMergeOutput, PatchBasedMergeFailure> {
	finish_patch_based_merge(
		target_path,
		&views.aggregate_contributors,
		context,
		views.vanilla.clone(),
		interactive_handler,
		interactive_config_path,
		|resolution_map, context| {
			run_cross_file_module_patch_engine(target_path, views, context, resolution_map)
		},
	)
}

fn finish_patch_based_merge<F>(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	context: PatchBasedMergeContext<'_>,
	vanilla: Option<foch_language::analyzer::semantic_index::ParsedScriptFile>,
	mut interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
	mut run_engine: F,
) -> Result<PatchBasedMergeOutput, PatchBasedMergeFailure>
where
	F: FnMut(
		&foch_core::config::ResolutionMap,
		&PatchBasedMergeContext<'_>,
	) -> Result<DagPatchComputation, MergeError>,
{
	// Hold an owned, mutable resolution map so that any post-pass interactive
	// resolutions can be folded back in before we re-run the merge engine
	// below. The merge engine itself never invokes interactive prompts — every
	// surviving conflict that reaches the user has already been pruned by the
	// downstream-override post-pass inside `compute_dag_patches_with_handler`.
	let mut effective_map = context.resolution_map.clone();
	let mut dag_patches = run_engine(&effective_map, &context)?;

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
				dag_patches = run_engine(&effective_map, &context)?;
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
				context.cwt_schema_graph.as_deref(),
			),
			handler_resolutions: merge_result.handler_resolutions,
		}));
	}

	let noop_vs_vanilla = vanilla
		.as_ref()
		.map(|base| {
			crate::merge::patch::ast_statement_lists_semantically_equal(
				&base.ast.statements,
				&dag_patches.merged_statements,
			)
		})
		.unwrap_or(false);
	let merged_statements = dag_patches.merged_statements;
	let preserve_complete_module =
		preserves_complete_structured_module(context.merge_kernel, context.descriptor);
	let (merged_statements, per_entry_noop_skipped_count) = if preserve_complete_module {
		(merged_statements, 0)
	} else if let Some(base) = vanilla.as_ref() {
		drop_per_entry_noop_duplicates(merged_statements, &base.ast.statements, context.descriptor)
	} else {
		(merged_statements, 0)
	};
	let definition_participants = dag_patches.definition_participants;
	let definition_provenance = dag_patches.definition_provenance;
	let merge_trace = build_merge_trace(
		&definition_provenance,
		&definition_participants,
		context.descriptor,
	);
	let merged_statements = if context.provenance {
		inject_provenance_comments(
			merged_statements,
			&definition_provenance,
			context.mod_display_names,
		)
	} else {
		merged_statements
	};
	let emit_options = emit_options_for_descriptor(context.emit_options, context.descriptor);
	let rendered = emit_clausewitz_statements_with_options(&merged_statements, &emit_options)?;
	Ok(PatchBasedMergeOutput {
		rendered,
		dep_remove_counts,
		stale_vanilla_targets,
		handler_resolutions: merge_result.handler_resolutions,
		external_file_resolutions: merge_result.external_file_resolutions,
		keep_existing_paths: merge_result.keep_existing_paths,
		noop_vs_vanilla,
		per_entry_noop_skipped_count,
		definition_provenance,
		merge_trace,
	})
}

fn preserves_complete_structured_module(
	merge_kernel: MergeKernelMode,
	descriptor: &ContentFamilyDescriptor,
) -> bool {
	merge_kernel == MergeKernelMode::Structured
		&& matches!(
			descriptor.load_policy,
			ContentLoadPolicy::DefinitionModule(_)
		)
}

fn emit_options_for_descriptor(
	options: &EmitOptions,
	descriptor: &foch_language::analyzer::content_family::ContentFamilyDescriptor,
) -> EmitOptions {
	let ordering = if descriptor_preserves_sibling_order(descriptor) {
		EmitOrdering::Preserve
	} else {
		EmitOrdering::FixedTopLevel
	};
	options.clone().with_ordering(ordering)
}

fn descriptor_preserves_sibling_order(
	descriptor: &foch_language::analyzer::content_family::ContentFamilyDescriptor,
) -> bool {
	matches!(
		descriptor.id.as_str(),
		"interface" | "common/interface" | "gfx"
	)
}

fn build_merge_trace(
	provenance: &BTreeMap<String, Vec<String>>,
	participants: &BTreeMap<String, Vec<MergeTraceContributor>>,
	descriptor: &foch_language::analyzer::content_family::ContentFamilyDescriptor,
) -> BTreeMap<String, MergeTraceEntry> {
	let mut trace = BTreeMap::new();
	for (key, adopted_mods) in provenance {
		let policy = trace_policy_for_key(descriptor, key);
		let all_participants = participants.get(key).cloned().unwrap_or_default();
		let mut contributors = Vec::new();
		for mod_id in adopted_mods {
			if let Some(participant) = all_participants
				.iter()
				.find(|participant| participant.mod_id == *mod_id)
			{
				contributors.push(participant.clone());
			} else {
				contributors.push(MergeTraceContributor {
					mod_id: mod_id.clone(),
					precedence: usize::MAX,
					dag_level: usize::MAX,
				});
			}
		}
		contributors.sort_by(|left, right| {
			left.dag_level
				.cmp(&right.dag_level)
				.then_with(|| left.precedence.cmp(&right.precedence))
				.then_with(|| left.mod_id.cmp(&right.mod_id))
		});
		let decision = trace_decision(policy, &contributors, all_participants.len());
		trace.insert(
			key.clone(),
			MergeTraceEntry {
				contributors,
				policy,
				decision,
			},
		);
	}
	trace
}

fn trace_policy_for_key(
	descriptor: &foch_language::analyzer::content_family::ContentFamilyDescriptor,
	key: &str,
) -> MergeTracePolicy {
	match descriptor.merge_policies.block_patch_policy_for_key(key) {
		BlockPatchPolicy::Union => MergeTracePolicy::Union,
		BlockPatchPolicy::BooleanOr => MergeTracePolicy::BooleanOr,
		BlockPatchPolicy::LastWriter => MergeTracePolicy::Overlay,
		BlockPatchPolicy::Recurse => {
			if descriptor.merge_policies.named_container != NamedContainerPolicy::Conflict {
				MergeTracePolicy::NamedContainer
			} else {
				MergeTracePolicy::Conflict
			}
		}
	}
}

fn trace_decision(
	policy: MergeTracePolicy,
	contributors: &[MergeTraceContributor],
	participant_count: usize,
) -> MergeTraceDecision {
	if contributors.len() > 1
		&& matches!(
			policy,
			MergeTracePolicy::Union
				| MergeTracePolicy::BooleanOr
				| MergeTracePolicy::NamedContainer
		) {
		return MergeTraceDecision::Unioned;
	}
	if contributors.len() == 1 && participant_count > 1 {
		return MergeTraceDecision::Overridden;
	}
	MergeTraceDecision::Adopted
}

/// Build a zero-width span for synthesized statements (provenance comments) that
/// have no source location.
fn synthetic_span() -> SpanRange {
	let point = Span {
		line: 0,
		column: 0,
		offset: 0,
	};
	SpanRange {
		start: point.clone(),
		end: point,
	}
}

/// Insert a `# foch: <key> from <display names>` comment immediately before each
/// top-level definition that has an adopted-provenance entry. Definitions with
/// no entry (pure vanilla / unchanged) are left untouched.
fn inject_provenance_comments(
	statements: Vec<AstStatement>,
	provenance: &BTreeMap<String, Vec<String>>,
	display_names: &HashMap<String, String>,
) -> Vec<AstStatement> {
	if provenance.is_empty() {
		return statements;
	}
	let mut out: Vec<AstStatement> = Vec::with_capacity(statements.len());
	for stmt in statements {
		if let AstStatement::Assignment { key, .. } = &stmt
			&& let Some(mods) = provenance.get(key)
		{
			let names: Vec<String> = mods
				.iter()
				.map(|m| display_names.get(m).cloned().unwrap_or_else(|| m.clone()))
				.collect();
			out.push(AstStatement::Comment {
				text: format!("foch: {key} from {}", names.join(", ")),
				span: synthetic_span(),
			});
		}
		out.push(stmt);
	}
	out
}

fn run_patch_merge_engine(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	context: &PatchBasedMergeContext<'_>,
	resolution_map: &foch_core::config::ResolutionMap,
) -> Result<DagPatchComputation, MergeError> {
	let mut handler = ChainHandler {
		first: LookupHandler::with_display_names(
			resolution_map,
			PathBuf::from(target_path),
			(*context.mod_display_names).clone(),
			context.cwt_schema_graph.clone(),
		),
		second: ChainHandler {
			first: PriorityBoostResolutionHandler::new(
				PathBuf::from(target_path),
				&resolution_map.mod_priority_boost,
			),
			second: ChainHandler {
				first: DepImpliesResolutionHandler::from_mod_dag(
					PathBuf::from(target_path),
					context.mod_dag,
					context.dep_overrides,
				),
				second: DeferHandler,
			},
		},
	};
	let effective_policies = effective_merge_policies(context);
	compute_dag_patches_with_handler_and_kernel(
		DagPatchRequest {
			file_path: target_path,
			contributors,
			merge_key_source: context.merge_key_source,
			policies: &effective_policies,
			mod_dag: context.mod_dag,
			ignore_replace_path: context.ignore_replace_path,
			dep_overrides: context.dep_overrides,
			game_version: context.cache_game_version,
			script_cache: Some(context.script_cache),
		},
		&mut handler,
		context.merge_kernel,
	)
	.map_err(|err| MergeError::Validation {
		path: Some(target_path.to_string()),
		message: format!("patch computation failed: {err}"),
	})
}

fn run_cross_file_module_patch_engine(
	target_path: &str,
	views: &CrossFileModuleViews,
	context: &PatchBasedMergeContext<'_>,
	resolution_map: &foch_core::config::ResolutionMap,
) -> Result<DagPatchComputation, MergeError> {
	let mut handler = ChainHandler {
		first: LookupHandler::with_display_names(
			resolution_map,
			PathBuf::from(target_path),
			(*context.mod_display_names).clone(),
			context.cwt_schema_graph.clone(),
		),
		second: ChainHandler {
			first: PriorityBoostResolutionHandler::new(
				PathBuf::from(target_path),
				&resolution_map.mod_priority_boost,
			),
			second: ChainHandler {
				first: DepImpliesResolutionHandler::from_mod_dag(
					PathBuf::from(target_path),
					context.mod_dag,
					context.dep_overrides,
				),
				second: DeferHandler,
			},
		},
	};
	let effective_policies = effective_merge_policies(context);
	compute_dag_patches_from_parsed_with_kernel(
		&views.file_dag,
		views.vanilla.as_ref(),
		&views.contributors,
		context.merge_key_source,
		&effective_policies,
		&mut handler,
		context.merge_kernel,
	)
	.map_err(|err| MergeError::Validation {
		path: Some(target_path.to_string()),
		message: format!("cross-file module patch computation failed: {err}"),
	})
}

fn effective_merge_policies(context: &PatchBasedMergeContext<'_>) -> MergePolicies {
	let mut policies = context.descriptor.merge_policies;
	if context.gui_scroll_merge && is_gui_container_family(context) {
		policies.named_container = NamedContainerPolicy::ScrollStack;
	}
	policies
}

fn is_gui_container_family(context: &PatchBasedMergeContext<'_>) -> bool {
	matches!(
		context.descriptor.id.as_str(),
		"interface" | "common/interface"
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::content_family::{
		ContentFamilyDescriptor, GameProfile, MergeKeySource,
	};
	use foch_language::analyzer::eu4_profile::eu4_profile;

	fn participant(mod_id: &str, precedence: usize, dag_level: usize) -> MergeTraceContributor {
		MergeTraceContributor {
			mod_id: mod_id.to_string(),
			precedence,
			dag_level,
		}
	}

	#[test]
	fn trace_derivation_marks_union_of_two_mods() {
		let descriptor =
			ContentFamilyDescriptor::prefix("common/scripted_effects", "common/scripted_effects/")
				.merge_key(MergeKeySource::AssignmentKey)
				.block_patch_policy(BlockPatchPolicy::Union)
				.build();
		let provenance = BTreeMap::from([(
			"test_shared_effect".to_string(),
			vec!["mod_a".to_string(), "mod_b".to_string()],
		)]);
		let participants = BTreeMap::from([(
			"test_shared_effect".to_string(),
			vec![participant("mod_a", 1, 0), participant("mod_b", 2, 0)],
		)]);

		let trace = build_merge_trace(&provenance, &participants, &descriptor);
		let entry = trace.get("test_shared_effect").expect("trace entry");
		assert_eq!(entry.policy, MergeTracePolicy::Union);
		assert_eq!(entry.decision, MergeTraceDecision::Unioned);
		assert_eq!(
			entry
				.contributors
				.iter()
				.map(|contributor| contributor.mod_id.as_str())
				.collect::<Vec<_>>(),
			vec!["mod_a", "mod_b"]
		);
	}

	#[test]
	fn trace_derivation_marks_overlay_winner_as_overridden() {
		let descriptor = ContentFamilyDescriptor::prefix("common/test", "common/test/")
			.merge_key(MergeKeySource::AssignmentKey)
			.block_patch_policy(BlockPatchPolicy::LastWriter)
			.build();
		let provenance = BTreeMap::from([("shared_key".to_string(), vec!["mod_b".to_string()])]);
		let participants = BTreeMap::from([(
			"shared_key".to_string(),
			vec![participant("mod_a", 1, 0), participant("mod_b", 2, 1)],
		)]);

		let trace = build_merge_trace(&provenance, &participants, &descriptor);
		let entry = trace.get("shared_key").expect("trace entry");
		assert_eq!(entry.policy, MergeTracePolicy::Overlay);
		assert_eq!(entry.decision, MergeTraceDecision::Overridden);
		assert_eq!(entry.contributors[0].mod_id, "mod_b");
	}

	#[test]
	fn structured_definition_modules_keep_the_complete_resolved_output() {
		let module = eu4_profile()
			.classify_content_family(Path::new(
				"common/scripted_triggers/zzz_foch_scripted_triggers.txt",
			))
			.expect("scripted triggers descriptor");
		let event = eu4_profile()
			.classify_content_family(Path::new("events/test.txt"))
			.expect("events descriptor");

		assert!(preserves_complete_structured_module(
			MergeKernelMode::Structured,
			module,
		));
		assert!(!preserves_complete_structured_module(
			MergeKernelMode::Legacy,
			module,
		));
		assert!(!preserves_complete_structured_module(
			MergeKernelMode::Structured,
			event,
		));
	}
}
