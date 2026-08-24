use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use foch::game::eu4::content::{
	ContentFamilyDescriptor, ContentLoadPolicy, MergePolicies, NamedContainerPolicy,
};
use foch::game::eu4::script::ParsedScriptFile;
use foch::game::eu4::script::parser::{AstFile, AstStatement, Span, SpanRange};
use foch::model::{HandlerResolutionRecord, LeafConflictDetail, MergeReportConflictContributor};

use super::per_entry_noop::drop_per_entry_noop_duplicates;
use super::provenance_tooltip::materialize_condition_provenance_tooltips;
use super::stale_detect::{
	collect_semantic_dep_misuse_remove_counts, collect_semantic_stale_vanilla_targets,
	parse_vanilla_for_stale_detection,
};
use super::{
	StructuralConflictReport, StructuralMergeContext, StructuralMergeFailure, StructuralMergeOutput,
};
use crate::emit::{EmitOptions, EmitOrdering, emit_clausewitz_statements_with_options};
use crate::merge::model::{
	ExternalFileResolution, MergeOutputDirective, SemanticMergeComputation, SemanticMergeConflict,
};
use crate::merge::structured::{
	clausewitz_files_semantically_equivalent, observe_merge_trace, semantic_conflict_id,
	semantic_conflict_view,
};
use crate::workspace::ResolvedFileContributor;
use foch::game::eu4::cwt::merge::classify_conflict_kind;

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

pub(crate) mod reference;

fn leaf_conflicts_for_semantic(
	target_path: &str,
	conflicts: &[SemanticMergeConflict],
	mod_versions: &HashMap<String, String>,
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
				conflict_id: semantic_conflict_id(Path::new(target_path), conflict.conflict.id),
				kind: classify_conflict_kind(Path::new(target_path), &ast_path, &conflict.reason),
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
	HashMap<PathBuf, ExternalFileResolution>,
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

pub(crate) fn merge_semantic_structural_file(
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

pub(crate) fn merge_semantic_definition_module(
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
	vanilla: Option<ParsedScriptFile>,
	mut interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
	interactive_config_path: Option<&Path>,
	mut run_engine: F,
) -> Result<StructuralMergeOutput, StructuralMergeFailure>
where
	F: FnMut(
		&foch::project::ResolutionMap,
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
				),
				handler_resolutions: dag_merge.semantic.handler_resolutions.clone(),
			},
		));
	}

	let merge_policies = effective_merge_policies(&context);
	let mut merged_statements = dag_merge.merged_statements;
	crate::merge::gui::coalesce_scroll_stack_variants(&mut merged_statements, &merge_policies);
	let noop_vs_vanilla = if let Some(base) = vanilla.as_ref() {
		let merged = AstFile {
			path: base.ast.path.clone(),
			statements: merged_statements.clone(),
		};
		clausewitz_files_semantically_equivalent(&base.ast, &merged, &merge_policies).map_err(
			|error| {
				StructuralMergeFailure::Merge(MergeError::Validation {
					path: Some(target_path.to_string()),
					message: format!("failed to compare semantic no-op output: {error}"),
				})
			},
		)?
	} else {
		false
	};
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
	let tooltip_output = materialize_condition_provenance_tooltips(
		context.provenance,
		target_path,
		merged_statements,
		&dag_merge.semantic,
		&merge_policies,
		context.mod_display_names,
	)
	.map_err(|message| {
		StructuralMergeFailure::Merge(MergeError::Validation {
			path: Some(target_path.to_string()),
			message,
		})
	})?;
	let (merged_statements, provenance_localisation) = if context.provenance {
		(
			inject_provenance_comments(
				tooltip_output.statements,
				&definition_provenance,
				context.mod_display_names,
			),
			tooltip_output.localisation,
		)
	} else {
		(tooltip_output.statements, tooltip_output.localisation)
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
		provenance_localisation,
	})
}

fn prompt_conflict_views(
	target_path: &str,
	views: &[crate::merge::conflict_view::ConflictView],
	effective_map: &mut foch::project::ResolutionMap,
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
	descriptor: &ContentFamilyDescriptor,
) -> EmitOptions {
	let ordering = if descriptor_preserves_sibling_order(descriptor) {
		EmitOrdering::Preserve
	} else {
		EmitOrdering::FixedTopLevel
	};
	options.clone().with_ordering(ordering)
}

fn descriptor_preserves_sibling_order(descriptor: &ContentFamilyDescriptor) -> bool {
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
	resolution_map: &foch::project::ResolutionMap,
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
			vanilla_base_mode: context.vanilla_base_mode,
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
	resolution_map: &foch::project::ResolutionMap,
) -> Result<SemanticDagMergeComputation, MergeError> {
	let mut handler = automatic_conflict_handler(target_path, context, resolution_map);
	let effective_policies = effective_merge_policies(context);
	compute_dag_merge_from_parsed(
		&views.file_dag,
		views.vanilla.as_ref(),
		&views.contributors,
		&effective_policies,
		context.vanilla_base_mode,
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
	resolution_map: &'a foch::project::ResolutionMap,
) -> impl ConflictHandler + 'a {
	ChainHandler {
		first: LookupHandler::with_display_names(
			resolution_map,
			PathBuf::from(target_path),
			(*context.mod_display_names).clone(),
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
	use std::fs;
	use std::time::{SystemTime, UNIX_EPOCH};

	use foch::game::eu4::content::{MergePolicies, eu4};
	use foch::game::eu4::script::parser::parse_clausewitz_content;
	use foch::merge::kernel::RevisionId;
	use foch::project::{Project, ResolutionMap};

	use crate::merge::conflict_handler::{ConflictDecision, ConflictHandler};
	use crate::merge::model::{SemanticConflictCandidate, SemanticMergeConflict};
	use crate::merge::structured::merge_clausewitz_files;

	struct SequentialCandidateHandler {
		next_candidate: usize,
	}

	impl ConflictHandler for SequentialCandidateHandler {
		fn on_conflict(
			&mut self,
			_view: &crate::merge::conflict_view::ConflictView,
		) -> ConflictDecision {
			let candidate = self.next_candidate;
			self.next_candidate += 1;
			ConflictDecision::PickCandidate {
				candidate,
				record: None,
			}
		}
	}

	#[test]
	fn structured_definition_modules_keep_the_complete_resolved_output() {
		let module = eu4()
			.classify_content_family(Path::new(
				"common/scripted_triggers/zzz_foch_scripted_triggers.txt",
			))
			.expect("scripted triggers descriptor");
		let event = eu4()
			.classify_content_family(Path::new("events/test.txt"))
			.expect("events descriptor");

		assert!(preserves_complete_tree_module(module));
		assert!(!preserves_complete_tree_module(event));
	}

	#[test]
	fn identical_cross_file_semantic_conflicts_have_independent_persistence_and_replay() {
		const TARGETS: [&str; 2] = ["common/first.txt", "common/second.txt"];
		let conflicts =
			TARGETS.map(|target| semantic_scalar_conflict(target, "value = 1\n", "value = 2\n"));
		assert_eq!(
			conflicts[0].conflict.id, conflicts[1].conflict.id,
			"the kernel id intentionally excludes the target path",
		);
		let expected_ids = TARGETS
			.iter()
			.zip(&conflicts)
			.map(|(target, conflict)| semantic_conflict_id(Path::new(target), conflict.conflict.id))
			.collect::<Vec<_>>();
		assert_ne!(expected_ids[0], expected_ids[1]);
		assert!(
			expected_ids
				.iter()
				.all(|conflict_id| conflict_id.len() == 64
					&& conflict_id.bytes().all(|b| b.is_ascii_hexdigit())),
		);

		let views = TARGETS
			.iter()
			.zip(&conflicts)
			.map(|(target, conflict)| {
				semantic_conflict_view(Path::new(target), conflict).expect("semantic conflict view")
			})
			.collect::<Vec<_>>();
		assert_eq!(views[0].address_path, views[1].address_path);
		assert_eq!(views[0].address_key, views[1].address_key);
		assert_eq!(
			views
				.iter()
				.map(|view| view.conflict_id.clone())
				.collect::<Vec<_>>(),
			expected_ids
		);

		let leaf_conflicts = TARGETS
			.iter()
			.zip(&conflicts)
			.flat_map(|(target, conflict)| {
				leaf_conflicts_for_semantic(target, std::slice::from_ref(conflict), &HashMap::new())
			})
			.collect::<Vec<_>>();
		assert_eq!(
			leaf_conflicts
				.iter()
				.map(|conflict| conflict.conflict_id.clone())
				.collect::<Vec<_>>(),
			expected_ids
		);

		let config_path = project_test_dir("semantic_cross_file_conflict_ids").join("foch.toml");
		let mut picker = SequentialCandidateHandler { next_candidate: 0 };
		let outcomes = TARGETS
			.iter()
			.zip(&views)
			.flat_map(|(target, view)| {
				let prompt = prompt_survivors_and_persist(
					Path::new(target),
					std::slice::from_ref(view),
					&mut picker,
					&config_path,
				);
				assert!(!prompt.aborted);
				prompt.outcomes
			})
			.collect::<Vec<_>>();
		assert_eq!(
			outcomes
				.iter()
				.map(|outcome| outcome.conflict_id.clone())
				.collect::<Vec<_>>(),
			expected_ids
		);

		let config = Project::from_toml_str(
			&fs::read_to_string(&config_path).expect("read persisted conflict resolutions"),
		)
		.expect("parse persisted conflict resolutions");
		let resolution_map =
			ResolutionMap::from_entries(&config.resolutions).expect("index conflict resolutions");
		assert_eq!(
			LookupHandler::new(&resolution_map, PathBuf::from(TARGETS[0])).on_conflict(&views[0]),
			ConflictDecision::PickCandidate {
				candidate: 0,
				record: None,
			}
		);
		assert_eq!(
			LookupHandler::new(&resolution_map, PathBuf::from(TARGETS[1])).on_conflict(&views[1]),
			ConflictDecision::PickCandidate {
				candidate: 1,
				record: None,
			}
		);
	}

	#[test]
	fn same_address_semantic_candidate_sequences_persist_and_replay_independently() {
		const TARGET: &str = "common/test.txt";
		let conflicts = [
			semantic_scalar_conflict(TARGET, "value = 1\n", "value = 2\n"),
			semantic_scalar_conflict(TARGET, "value = 3\n", "value = 4\n"),
		];
		assert_ne!(conflicts[0].conflict.id, conflicts[1].conflict.id);
		let views = conflicts
			.iter()
			.map(|conflict| {
				semantic_conflict_view(Path::new(TARGET), conflict).expect("semantic conflict view")
			})
			.collect::<Vec<_>>();
		assert_eq!(views[0].address_path, views[1].address_path);
		assert_eq!(views[0].address_key, views[1].address_key);
		assert_ne!(views[0].conflict_id, views[1].conflict_id);

		let config_path = project_test_dir("semantic_candidate_sequence_ids").join("foch.toml");
		let mut picker = SequentialCandidateHandler { next_candidate: 0 };
		let prompt =
			prompt_survivors_and_persist(Path::new(TARGET), &views, &mut picker, &config_path);
		assert!(!prompt.aborted);
		assert_eq!(prompt.outcomes.len(), 2);

		let config = Project::from_toml_str(
			&fs::read_to_string(&config_path).expect("read persisted conflict resolutions"),
		)
		.expect("parse persisted conflict resolutions");
		let resolution_map =
			ResolutionMap::from_entries(&config.resolutions).expect("index conflict resolutions");
		let mut lookup = LookupHandler::new(&resolution_map, PathBuf::from(TARGET));
		assert_eq!(
			lookup.on_conflict(&views[0]),
			ConflictDecision::PickCandidate {
				candidate: 0,
				record: None,
			},
		);
		assert_eq!(
			lookup.on_conflict(&views[1]),
			ConflictDecision::PickCandidate {
				candidate: 1,
				record: None,
			},
		);
	}

	fn semantic_scalar_conflict(
		target_path: &str,
		left_source: &str,
		right_source: &str,
	) -> SemanticMergeConflict {
		let base = parse_test_file(target_path, "value = 0\n");
		let left = parse_test_file(target_path, left_source);
		let right = parse_test_file(target_path, right_source);
		let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default())
			.expect("compute scalar tree conflict");
		let [conflict] = outcome.conflicts() else {
			panic!(
				"expected one scalar tree conflict: {:?}",
				outcome.conflicts()
			);
		};
		let candidates = conflict
			.candidates
			.iter()
			.copied()
			.filter(|source| source.input().revision != RevisionId::BASE)
			.map(|source| SemanticConflictCandidate {
				source,
				source_id: format!("mod_{}", source.input().revision.get()),
				precedence: usize::from(source.input().revision.get()),
				statement: None,
			})
			.collect::<Vec<_>>();
		SemanticMergeConflict {
			conflict: conflict.clone(),
			reason: conflict.detail.clone(),
			base_statement: None,
			source_selectable: !candidates.is_empty(),
			candidates,
		}
	}

	fn parse_test_file(target_path: &str, source: &str) -> AstFile {
		let parsed = parse_clausewitz_content(PathBuf::from(target_path), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast
	}

	fn project_test_dir(name: &str) -> PathBuf {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("clock after epoch")
			.as_nanos();
		std::env::current_dir()
			.expect("current dir")
			.join("target")
			.join("foch-engine-tests")
			.join(format!("{name}-{}-{nanos}", std::process::id()))
	}
}
