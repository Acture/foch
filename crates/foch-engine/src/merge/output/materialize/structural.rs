use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use foch_core::config::compute_conflict_id;
use foch_core::model::{
	HandlerResolutionRecord, LeafConflictDetail, MergeReportConflictContributor,
};
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, ContentLoadPolicy, MergePolicies, NamedContainerPolicy,
};
use foch_language::analyzer::parser::{AstFile, AstStatement, Span, SpanRange};

use super::per_entry_noop::drop_per_entry_noop_duplicates;
use super::stale_detect::{
	collect_semantic_dep_misuse_remove_counts, collect_semantic_stale_vanilla_targets,
	parse_vanilla_for_stale_detection,
};
use super::{
	StructuralConflictReport, StructuralMergeContext, StructuralMergeFailure, StructuralMergeOutput,
};
use crate::emit::{EmitOptions, EmitOrdering, emit_clausewitz_statements_with_options};
use crate::merge::cwt_suggestions::classify_conflict_kind;
use crate::merge::model::{MergeOutputDirective, SemanticMergeComputation, SemanticMergeConflict};
use crate::merge::structured::{
	clausewitz_files_semantically_equivalent, observe_merge_trace, semantic_conflict_view,
};
use crate::workspace::ResolvedFileContributor;
use foch_cwt::RuleEngine;

use super::super::super::conflict_handler::{
	ChainHandler, ConflictHandler, DeferHandler, DepImpliesResolutionHandler, LookupHandler,
	PriorityBoostResolutionHandler, PromptOutcomeKind, prompt_survivors_and_persist,
};
use super::super::super::error::MergeError;
use crate::merge::planning::dag_input::DagMergeInputRequest;
use crate::merge::planning::dag_merge::{
	SemanticDagMergeComputation, SemanticDagMergeRequest, compute_dag_merge_from_parsed,
	compute_dag_merge_with_handler,
};
use crate::merge::planning::module_view::CrossFileModuleViews;

pub(super) mod reference;

fn leaf_conflicts_for_semantic(
	target_path: &str,
	conflicts: &[SemanticMergeConflict],
	mod_versions: &HashMap<String, String>,
	cwt_rule_engine: Option<&RuleEngine>,
) -> Vec<LeafConflictDetail> {
	conflicts
		.iter()
		.map(|conflict| {
			let (address_path, address_key) = split_semantic_path(&conflict.conflict.semantic_path);
			let joined_path = address_path.join("/");
			let ast_path = address_path.iter().map(String::as_str).collect::<Vec<_>>();
			let mut contributors = conflict
				.candidates
				.iter()
				.map(|candidate| MergeReportConflictContributor {
					mod_id: candidate.source_id.clone(),
					mod_version: mod_versions
						.get(&candidate.source_id)
						.cloned()
						.unwrap_or_else(|| "unknown".to_string()),
					precedence: candidate.precedence,
				})
				.collect::<Vec<_>>();
			contributors.sort_by(|left, right| {
				left.precedence
					.cmp(&right.precedence)
					.then_with(|| left.mod_id.cmp(&right.mod_id))
			});
			contributors.dedup_by(|left, right| {
				left.mod_id == right.mod_id && left.precedence == right.precedence
			});
			LeafConflictDetail {
				address_path: joined_path.clone(),
				address_key: address_key.clone(),
				conflict_id: compute_conflict_id(
					Path::new(target_path),
					&joined_path,
					&address_key,
				),
				kind: cwt_rule_engine.and_then(|engine| {
					classify_conflict_kind(
						engine,
						Path::new(target_path),
						&ast_path,
						&conflict.reason,
					)
				}),
				contributors,
			}
		})
		.collect()
}

fn split_semantic_path(path: &[String]) -> (Vec<String>, String) {
	match path.split_last() {
		Some((key, parent)) => (parent.to_vec(), key.clone()),
		None => (Vec::new(), "$file".to_string()),
	}
}

fn semantic_output_metadata(
	target_path: &str,
	computation: SemanticMergeComputation,
) -> (
	Vec<HandlerResolutionRecord>,
	HashMap<PathBuf, PathBuf>,
	HashSet<PathBuf>,
) {
	let mut external_file_resolutions = HashMap::new();
	let mut keep_existing_paths = HashSet::new();
	for directive in computation.output_directives {
		match directive {
			MergeOutputDirective::UseFile(source) => {
				external_file_resolutions.insert(PathBuf::from(target_path), source);
			}
			MergeOutputDirective::KeepExisting => {
				keep_existing_paths.insert(PathBuf::from(target_path));
			}
		}
	}
	(
		computation.handler_resolutions,
		external_file_resolutions,
		keep_existing_paths,
	)
}

pub(super) fn merge_semantic_structural_file(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	context: StructuralMergeContext<'_>,
	interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
) -> Result<StructuralMergeOutput, StructuralMergeFailure> {
	let vanilla =
		parse_vanilla_for_stale_detection(target_path, contributors, context.script_cache)?;
	finish_semantic_structural_merge(
		target_path,
		contributors,
		context,
		vanilla,
		interactive_handler,
		interactive_config_path,
		|resolution_map, context| {
			run_semantic_structural_file_engine(target_path, contributors, context, resolution_map)
		},
	)
}

pub(super) fn merge_semantic_definition_module(
	target_path: &str,
	views: &CrossFileModuleViews,
	context: StructuralMergeContext<'_>,
	interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
) -> Result<StructuralMergeOutput, StructuralMergeFailure> {
	finish_semantic_structural_merge(
		target_path,
		&views.aggregate_contributors,
		context,
		views.vanilla.clone(),
		interactive_handler,
		interactive_config_path,
		|resolution_map, context| {
			run_semantic_definition_module_engine(target_path, views, context, resolution_map)
		},
	)
}

fn finish_semantic_structural_merge<F>(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	context: StructuralMergeContext<'_>,
	vanilla: Option<foch_language::analyzer::semantic_index::ParsedScriptFile>,
	mut interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
	mut run_engine: F,
) -> Result<StructuralMergeOutput, StructuralMergeFailure>
where
	F: FnMut(
		&foch_core::config::ResolutionMap,
		&StructuralMergeContext<'_>,
	) -> Result<SemanticDagMergeComputation, MergeError>,
{
	let mut effective_map = context.resolution_map.clone();
	let mut dag_merge = run_engine(&effective_map, &context)?;

	if !dag_merge.semantic.unresolved_conflicts.is_empty()
		&& let (Some(handler), Some(config_path)) =
			(interactive_handler.as_mut(), interactive_config_path)
	{
		let survivor_views = dag_merge
			.semantic
			.unresolved_conflicts
			.iter()
			.map(|conflict| {
				semantic_conflict_view(Path::new(target_path), conflict).map_err(|message| {
					MergeError::Validation {
						path: Some(target_path.to_string()),
						message,
					}
				})
			})
			.collect::<Result<Vec<_>, MergeError>>()?;
		if prompt_conflict_views(
			target_path,
			&survivor_views,
			&mut effective_map,
			&mut **handler,
			config_path,
		)? {
			dag_merge = run_engine(&effective_map, &context)?;
		}
	}

	let stale_vanilla_targets = collect_semantic_stale_vanilla_targets(
		target_path,
		&dag_merge.semantic.source_deltas,
		vanilla.as_ref(),
		&effective_merge_policies(&context),
		context.mod_versions,
	)
	.map_err(|message| {
		StructuralMergeFailure::Merge(MergeError::Validation {
			path: Some(target_path.to_string()),
			message,
		})
	})?;
	let dep_remove_counts = collect_semantic_dep_misuse_remove_counts(
		context.dep_misuse_findings,
		contributors,
		&dag_merge.semantic.source_deltas,
	);

	if !dag_merge.semantic.unresolved_conflicts.is_empty() {
		let conflict_keys = dag_merge
			.semantic
			.unresolved_conflicts
			.iter()
			.map(|conflict| {
				let (_, key) = split_semantic_path(&conflict.conflict.semantic_path);
				format!("{key}: {}", conflict.reason)
			})
			.collect::<Vec<_>>();
		let reason = format!(
			"structural merge has {} unresolved conflict(s): {}",
			conflict_keys.len(),
			conflict_keys.join("; "),
		);
		return Err(StructuralMergeFailure::Unresolved(
			StructuralConflictReport {
				reason,
				leaf_conflicts: leaf_conflicts_for_semantic(
					target_path,
					&dag_merge.semantic.unresolved_conflicts,
					context.mod_versions,
					context.cwt_rule_engine.as_deref(),
				),
				handler_resolutions: dag_merge.semantic.handler_resolutions.clone(),
			},
		));
	}

	let noop_vs_vanilla = if let Some(base) = vanilla.as_ref() {
		let merged = AstFile {
			path: base.ast.path.clone(),
			statements: dag_merge.merged_statements.clone(),
		};
		clausewitz_files_semantically_equivalent(
			&base.ast,
			&merged,
			&effective_merge_policies(&context),
		)
		.map_err(|error| {
			StructuralMergeFailure::Merge(MergeError::Validation {
				path: Some(target_path.to_string()),
				message: format!("failed to compare semantic no-op output: {error}"),
			})
		})?
	} else {
		false
	};
	let merged_statements = dag_merge.merged_statements;
	let preserve_complete_module = preserves_complete_tree_module(context.descriptor);
	let (merged_statements, per_entry_noop_skipped_count) = if preserve_complete_module {
		(merged_statements, 0)
	} else if let Some(base) = vanilla.as_ref() {
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
		Some(&dag_merge.semantic),
	)
	.map_err(|message| {
		StructuralMergeFailure::Merge(MergeError::Validation {
			path: Some(target_path.to_string()),
			message,
		})
	})?;
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
	let (handler_resolutions, external_file_resolutions, keep_existing_paths) =
		semantic_output_metadata(target_path, dag_merge.semantic);
	Ok(StructuralMergeOutput {
		rendered,
		dep_remove_counts,
		stale_vanilla_targets,
		handler_resolutions,
		external_file_resolutions,
		keep_existing_paths,
		noop_vs_vanilla,
		per_entry_noop_skipped_count,
		definition_provenance,
		merge_trace,
	})
}

fn prompt_conflict_views(
	target_path: &str,
	views: &[crate::merge::conflict_view::ConflictView],
	effective_map: &mut foch_core::config::ResolutionMap,
	handler: &mut dyn ConflictHandler,
	config_path: &Path,
) -> Result<bool, StructuralMergeFailure> {
	if views.is_empty() {
		return Ok(false);
	}
	let prompt = prompt_survivors_and_persist(Path::new(target_path), views, handler, config_path);
	let mut changed = false;
	for outcome in prompt.outcomes {
		if let PromptOutcomeKind::Picked(decision) = outcome.kind {
			effective_map
				.by_conflict_id
				.insert(outcome.conflict_id, decision);
			changed = true;
		}
	}
	if prompt.aborted {
		return Err(StructuralMergeFailure::Merge(MergeError::Validation {
			path: Some(target_path.to_string()),
			message: "merge aborted by user".to_string(),
		}));
	}
	Ok(changed)
}

fn preserves_complete_tree_module(descriptor: &ContentFamilyDescriptor) -> bool {
	matches!(
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

fn run_semantic_structural_file_engine(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	context: &StructuralMergeContext<'_>,
	resolution_map: &foch_core::config::ResolutionMap,
) -> Result<SemanticDagMergeComputation, MergeError> {
	let mut handler = automatic_conflict_handler(target_path, context, resolution_map);
	let effective_policies = effective_merge_policies(context);
	compute_dag_merge_with_handler(
		SemanticDagMergeRequest {
			input: DagMergeInputRequest {
				file_path: target_path,
				contributors,
				mod_dag: context.mod_dag,
				ignore_replace_path: context.ignore_replace_path,
				dep_overrides: context.dep_overrides,
				script_cache: Some(context.script_cache),
			},
			policies: &effective_policies,
		},
		&mut handler,
	)
	.map_err(|err| MergeError::Validation {
		path: Some(target_path.to_string()),
		message: format!("semantic DAG merge failed: {err}"),
	})
}

fn run_semantic_definition_module_engine(
	target_path: &str,
	views: &CrossFileModuleViews,
	context: &StructuralMergeContext<'_>,
	resolution_map: &foch_core::config::ResolutionMap,
) -> Result<SemanticDagMergeComputation, MergeError> {
	let mut handler = automatic_conflict_handler(target_path, context, resolution_map);
	let effective_policies = effective_merge_policies(context);
	compute_dag_merge_from_parsed(
		&views.file_dag,
		views.vanilla.as_ref(),
		&views.contributors,
		&effective_policies,
		&mut handler,
	)
	.map_err(|err| MergeError::Validation {
		path: Some(target_path.to_string()),
		message: format!("semantic definition-module DAG merge failed: {err}"),
	})
}

fn automatic_conflict_handler<'a>(
	target_path: &str,
	context: &'a StructuralMergeContext<'a>,
	resolution_map: &'a foch_core::config::ResolutionMap,
) -> impl ConflictHandler + 'a {
	ChainHandler {
		first: LookupHandler::with_display_names(
			resolution_map,
			PathBuf::from(target_path),
			(*context.mod_display_names).clone(),
			context.cwt_rule_engine.clone(),
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
	}
}

fn effective_merge_policies(context: &StructuralMergeContext<'_>) -> MergePolicies {
	let mut policies = context.descriptor.merge_policies;
	if context.gui_scroll_merge && is_gui_container_family(context) {
		policies.named_container = NamedContainerPolicy::ScrollStack;
	}
	policies
}

fn is_gui_container_family(context: &StructuralMergeContext<'_>) -> bool {
	matches!(
		context.descriptor.id.as_str(),
		"interface" | "common/interface"
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::content_family::GameProfile;
	use foch_language::analyzer::eu4_profile::eu4_profile;

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

		assert!(preserves_complete_tree_module(module));
		assert!(!preserves_complete_tree_module(event));
	}
}
