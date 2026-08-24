use std::collections::HashMap;
use std::path::Path;

use crate::game::eu4::cwt::merge::classify_conflict_kind;
use crate::game::eu4::script::ParsedScriptFile;
use crate::model::{LeafConflictDetail, MergeReportConflictContributor};
use crate::project::{ResolutionMap, compute_conflict_id};

use super::super::{
	StructuralConflictReport, StructuralMergeContext, StructuralMergeFailure,
	StructuralMergeOutput,
	per_entry_noop::drop_per_entry_noop_duplicates,
	stale_detect::{
		collect_dep_misuse_remove_counts, collect_stale_vanilla_targets,
		parse_vanilla_for_stale_detection, vanilla_snippet_for_address,
	},
};
use crate::game::eu4::script::emit::{EmitOptions, emit_clausewitz_statements_with_options};
use crate::input::ResolvedInputContributor;
use crate::merge::address_patch::conflict_view::build_conflict_view;
use crate::merge::address_patch::dag_merge::{
	ReferenceDagMergeComputation, ReferenceDagMergeRequest, ReferenceParsedDagMergeRequest,
	compute_reference_dag_merge, compute_reference_dag_merge_from_parsed,
};
use crate::merge::address_patch::patch_merge::{
	AttributedPatch, PatchConflict, PatchMergeResult, PatchResolution,
};
use crate::merge::error::MergeError;
use crate::merge::planning::dag_input::{
	DagMergeInputRequest, merge_ancestor_statements, template_for,
};
use crate::merge::planning::module_view::CrossFileModuleViews;
use crate::merge::resolution::conflict_handler::ConflictHandler;
use crate::merge::resolution::conflict_view::ConflictView;
use crate::merge::structured::observe_merge_trace;

pub(crate) fn merge_structural_file(
	target_path: &str,
	contributors: &[ResolvedInputContributor],
	context: StructuralMergeContext<'_>,
	interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
) -> Result<StructuralMergeOutput, StructuralMergeFailure> {
	let vanilla =
		parse_vanilla_for_stale_detection(target_path, contributors, context.script_cache)?;
	finish_reference_structural_merge(
		target_path,
		contributors,
		context,
		vanilla,
		interactive_handler,
		interactive_config_path,
		|resolution_map, context| {
			run_reference_structural_file_engine(target_path, contributors, context, resolution_map)
		},
	)
}

pub(crate) fn merge_definition_module(
	target_path: &str,
	views: &CrossFileModuleViews,
	context: StructuralMergeContext<'_>,
	interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
) -> Result<StructuralMergeOutput, StructuralMergeFailure> {
	finish_reference_structural_merge(
		target_path,
		&views.aggregate_contributors,
		context,
		views.vanilla.clone(),
		interactive_handler,
		interactive_config_path,
		|resolution_map, context| {
			run_reference_definition_module_engine(target_path, views, context, resolution_map)
		},
	)
}

fn finish_reference_structural_merge<F>(
	target_path: &str,
	contributors: &[ResolvedInputContributor],
	context: StructuralMergeContext<'_>,
	vanilla: Option<ParsedScriptFile>,
	mut interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
	mut run_engine: F,
) -> Result<StructuralMergeOutput, StructuralMergeFailure>
where
	F: FnMut(
		&ResolutionMap,
		&StructuralMergeContext<'_>,
	) -> Result<ReferenceDagMergeComputation, MergeError>,
{
	let mut effective_map = context.resolution_map.clone();
	let mut dag_merge = run_engine(&effective_map, &context)?;

	if !dag_merge.merge_result.conflicts.is_empty()
		&& let (Some(handler), Some(config_path)) =
			(interactive_handler.as_mut(), interactive_config_path)
	{
		let survivor_views = survivor_views(
			target_path,
			&dag_merge.merge_result,
			vanilla.as_ref(),
			context.mod_display_names,
			context.emit_options,
		)?;
		if super::prompt_conflict_views(
			target_path,
			&survivor_views,
			&mut effective_map,
			&mut **handler,
			config_path,
		)? {
			dag_merge = run_engine(&effective_map, &context)?;
		}
	}

	let stale_vanilla_targets = collect_stale_vanilla_targets(
		target_path,
		&dag_merge.mod_patches,
		vanilla.as_ref(),
		context.merge_key_source,
		context.mod_versions,
	);
	let dep_remove_counts = collect_dep_misuse_remove_counts(
		context.dep_misuse_findings,
		contributors,
		&dag_merge.mod_patches,
	);
	if !dag_merge.merge_result.conflicts.is_empty() {
		return Err(StructuralMergeFailure::Unresolved(unresolved_report(
			target_path,
			&dag_merge.merge_result,
			context.mod_versions,
		)));
	}

	let noop_vs_vanilla = vanilla.as_ref().is_some_and(|base| {
		crate::merge::address_patch::patch::ast_statement_lists_semantically_equal(
			&base.ast.statements,
			&dag_merge.merged_statements,
		)
	});
	let merged_statements = dag_merge.merged_statements;
	let (merged_statements, per_entry_noop_skipped_count) = if let Some(base) = vanilla.as_ref() {
		drop_per_entry_noop_duplicates(merged_statements, &base.ast.statements, context.descriptor)
	} else {
		(merged_statements, 0)
	};
	let definition_participants = dag_merge.definition_participants;
	let definition_provenance = dag_merge.definition_provenance;
	let merge_trace = observe_merge_trace(
		&definition_provenance,
		&definition_participants,
		context.descriptor,
		None,
	)
	.map_err(|message| {
		StructuralMergeFailure::Merge(MergeError::Validation {
			path: Some(target_path.to_string()),
			message,
		})
	})?;
	let merged_statements = if context.provenance {
		super::inject_provenance_comments(
			merged_statements,
			&definition_provenance,
			context.mod_display_names,
		)
	} else {
		merged_statements
	};
	let emit_options = super::emit_options_for_descriptor(context.emit_options, context.descriptor);
	let rendered = emit_clausewitz_statements_with_options(&merged_statements, &emit_options)?;
	let merge_result = dag_merge.merge_result;
	Ok(StructuralMergeOutput {
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
		// The address-patch reference backend does not retain node lineage. It is
		// test-only; product materialization uses the semantic backend above.
		provenance_localisation: Default::default(),
	})
}

fn run_reference_structural_file_engine(
	target_path: &str,
	contributors: &[ResolvedInputContributor],
	context: &StructuralMergeContext<'_>,
	resolution_map: &ResolutionMap,
) -> Result<ReferenceDagMergeComputation, MergeError> {
	let mut handler = super::automatic_conflict_handler(target_path, context, resolution_map);
	let effective_policies = super::effective_merge_policies(context);
	compute_reference_dag_merge(
		ReferenceDagMergeRequest {
			input: DagMergeInputRequest {
				file_path: target_path,
				contributors,
				mod_dag: context.mod_dag,
				ignore_replace_path: context.ignore_replace_path,
				dep_overrides: context.dep_overrides,
				script_cache: Some(context.script_cache),
			},
			merge_key_source: context.merge_key_source,
			policies: &effective_policies,
			game_version: context.cache_game_version,
		},
		&mut handler,
	)
	.map_err(|error| MergeError::Validation {
		path: Some(target_path.to_string()),
		message: format!("address-patch reference DAG merge failed: {error}"),
	})
}

fn run_reference_definition_module_engine(
	target_path: &str,
	views: &CrossFileModuleViews,
	context: &StructuralMergeContext<'_>,
	resolution_map: &ResolutionMap,
) -> Result<ReferenceDagMergeComputation, MergeError> {
	let mut handler = super::automatic_conflict_handler(target_path, context, resolution_map);
	let effective_policies = super::effective_merge_policies(context);
	let base_statements = merge_ancestor_statements(views.vanilla.as_ref());
	compute_reference_dag_merge_from_parsed(ReferenceParsedDagMergeRequest {
		file_dag: &views.file_dag,
		base_statements: &base_statements,
		template: template_for(&views.file_dag, views.vanilla.as_ref(), &views.contributors),
		contributors: &views.contributors,
		merge_key_source: context.merge_key_source,
		policies: &effective_policies,
		handler: &mut handler,
		mod_hashes: None,
		game_version: context.cache_game_version,
	})
	.map_err(|error| MergeError::Validation {
		path: Some(target_path.to_string()),
		message: format!("address-patch reference definition-module DAG merge failed: {error}"),
	})
}

pub(super) fn survivor_views(
	target_path: &str,
	merge_result: &PatchMergeResult,
	vanilla: Option<&ParsedScriptFile>,
	mod_display_names: &HashMap<String, String>,
	emit_options: &EmitOptions,
) -> Result<Vec<ConflictView>, MergeError> {
	let vanilla_lookup = |address| vanilla_snippet_for_address(vanilla, address, emit_options);
	merge_result
		.conflicts
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Conflict {
				address,
				patches,
				reason,
			} => Some((address, patches, reason)),
			_ => None,
		})
		.map(|(address, patches, reason)| {
			let address_path = address.path.join("/");
			let conflict_id =
				compute_conflict_id(Path::new(target_path), &address_path, &address.key);
			build_conflict_view(
				Path::new(target_path),
				address,
				&PatchConflict {
					patches: patches.clone(),
					reason: reason.clone(),
				},
				conflict_id,
				mod_display_names,
				vanilla_lookup(address),
				emit_options,
			)
		})
		.collect()
}

pub(super) fn unresolved_report(
	target_path: &str,
	merge_result: &PatchMergeResult,
	mod_versions: &HashMap<String, String>,
) -> StructuralConflictReport {
	let conflict_keys = merge_result
		.conflicts
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Conflict {
				address, reason, ..
			} => Some(format!("{}: {}", address.key, reason)),
			_ => None,
		})
		.collect::<Vec<_>>();
	StructuralConflictReport {
		reason: format!(
			"structural merge has {} unresolved conflict(s): {}",
			conflict_keys.len(),
			conflict_keys.join("; "),
		),
		leaf_conflicts: leaf_conflicts(target_path, &merge_result.conflicts, mod_versions),
		handler_resolutions: merge_result.handler_resolutions.clone(),
	}
}

fn leaf_conflicts(
	target_path: &str,
	conflicts: &[PatchResolution],
	mod_versions: &HashMap<String, String>,
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
					kind: classify_conflict_kind(Path::new(target_path), &ast_path, reason),
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
