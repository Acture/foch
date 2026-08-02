use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::content_family::MergePolicies;
use foch_language::analyzer::parser::{AstFile, AstStatement};
use foch_merge_kernel::{
	ConflictNodeId, ConflictResolution, RevisionId, SourceNodeRef, StructuralConflict,
};

use crate::emit::emit_clausewitz_statements;
use crate::merge::conflict_handler::{ConflictDecision, ConflictHandler};
use crate::merge::conflict_view::{CandidateView, ConflictView};
use crate::merge::kernel::{KernelMergeInput, KernelRevision};
use crate::merge::model::{
	MergeOutputDirective, SemanticConflictCandidate, SemanticMergeComputation,
	SemanticMergeConflict, SemanticMergeFacts, SemanticMergeSource, SemanticPartitionId,
	SemanticPartitionLineage,
};
use crate::merge::planning::dag::{FileDag, ModId};
use crate::merge::planning::dag_join::DagJoinScope;
use crate::merge::planning::dag_pipeline::{
	DagJoinProtocol, DagJoinRequest, DagJoinRevision, EffectiveNodeProtocol, EffectiveNodeRequest,
};

use super::ast_adapter::{denormalize_statement, normalize_ast};
use super::definition_module::definition_module_partition_ids;
use super::observer::TreeSourceObserver;
use super::policy::ContentFamilyMergePolicy;
use super::trivia::detach_trivia;
use super::{
	ClausewitzKernelFacts, merge_clausewitz_definition_module_n_way_with_resolutions,
	merge_clausewitz_files_n_way_with_resolutions, merge_event_files_n_way_with_resolutions,
};

pub(crate) type TreeConflictResolutions = BTreeMap<ConflictNodeId, ConflictResolution>;
pub(crate) type TreeDagState = SemanticMergeComputation;

pub(crate) trait TreePartitionAdapter {
	fn normalization_partitions(
		&self,
		_base: &AstFile,
		_revision: &AstFile,
	) -> Vec<SemanticPartitionId> {
		vec![SemanticPartitionId::File]
	}
}

pub(crate) trait TreeJoinProtocol {
	fn name(&self) -> &'static str;

	fn merge_n_way(
		&self,
		base: &AstFile,
		revisions: &[&AstFile],
		policies: &MergePolicies,
		resolutions: &[ConflictResolution],
	) -> Result<TreeMergeStep, String>;
}

#[derive(Clone, Debug)]
pub(crate) struct TreeMergeStep {
	pub statements: Vec<AstStatement>,
	pub conflicts: Vec<StructuralConflict>,
	pub kernel_facts: Vec<ClausewitzKernelFacts>,
}

#[derive(Clone, Debug)]
pub(crate) struct TreeKernelOutcome {
	pub statements: Vec<AstStatement>,
	pub conflicts: Vec<StructuralConflict>,
	pub merge_facts: Vec<SemanticMergeFacts>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TreeMergeUnit {
	#[default]
	File,
	DefinitionModule,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ClausewitzFileAdapter;

impl TreePartitionAdapter for ClausewitzFileAdapter {}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ClausewitzFileJoin;

impl TreeJoinProtocol for ClausewitzFileJoin {
	fn name(&self) -> &'static str {
		"clausewitz-file"
	}

	fn merge_n_way(
		&self,
		base: &AstFile,
		revisions: &[&AstFile],
		policies: &MergePolicies,
		resolutions: &[ConflictResolution],
	) -> Result<TreeMergeStep, String> {
		let outcome =
			merge_clausewitz_files_n_way_with_resolutions(base, revisions, policies, resolutions)
				.map_err(|error| format!("Clausewitz file join failed: {error}"))?;
		Ok(clausewitz_step(outcome))
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EventFileAdapter;

impl TreePartitionAdapter for EventFileAdapter {}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EventFileJoin;

impl TreeJoinProtocol for EventFileJoin {
	fn name(&self) -> &'static str {
		"event-file"
	}

	fn merge_n_way(
		&self,
		base: &AstFile,
		revisions: &[&AstFile],
		policies: &MergePolicies,
		resolutions: &[ConflictResolution],
	) -> Result<TreeMergeStep, String> {
		let outcome =
			merge_event_files_n_way_with_resolutions(base, revisions, policies, resolutions)
				.map_err(|error| format!("event file join failed: {error}"))?;
		Ok(clausewitz_step(outcome))
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DefinitionModuleAdapter;

impl TreePartitionAdapter for DefinitionModuleAdapter {
	fn normalization_partitions(
		&self,
		base: &AstFile,
		revision: &AstFile,
	) -> Vec<SemanticPartitionId> {
		definition_module_partition_ids(&[base, revision])
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DefinitionModuleJoin;

impl TreeJoinProtocol for DefinitionModuleJoin {
	fn name(&self) -> &'static str {
		"definition-module"
	}

	fn merge_n_way(
		&self,
		base: &AstFile,
		revisions: &[&AstFile],
		policies: &MergePolicies,
		resolutions: &[ConflictResolution],
	) -> Result<TreeMergeStep, String> {
		let outcome = merge_clausewitz_definition_module_n_way_with_resolutions(
			base,
			revisions,
			policies,
			resolutions,
		)
		.map_err(|error| format!("definition-module join failed: {error}"))?;
		eprintln!(
			"[tree-module] join {} base_definitions={} active_definitions={} copy_through_definitions={} tree_definitions={}",
			base.path.display(),
			outcome.base_definitions(),
			outcome.active_definitions(),
			outcome.copy_through_definitions(),
			outcome.structured_definitions(),
		);
		let (tentative_ast, conflicts, kernel_facts) = outcome.into_tree_parts();
		Ok(TreeMergeStep {
			statements: tentative_ast.statements,
			conflicts,
			kernel_facts,
		})
	}
}

fn clausewitz_step(outcome: super::ClausewitzMergeOutcome) -> TreeMergeStep {
	let conflicts = outcome.conflicts().to_vec();
	let (tentative_ast, kernel_fact) = outcome.into_parts(SemanticPartitionId::File);
	TreeMergeStep {
		statements: tentative_ast.statements,
		conflicts,
		kernel_facts: vec![kernel_fact],
	}
}

pub(crate) struct TreeMergeKernel<'a> {
	join: &'a dyn TreeJoinProtocol,
	policies: &'a MergePolicies,
}

impl<'a> TreeMergeKernel<'a> {
	pub(crate) const fn new(join: &'a dyn TreeJoinProtocol, policies: &'a MergePolicies) -> Self {
		Self { join, policies }
	}

	#[cfg(test)]
	pub(crate) fn merge(&self, input: &KernelMergeInput) -> Result<Vec<AstStatement>, String> {
		self.merge_with_resolutions(input, &TreeConflictResolutions::new())
	}

	#[cfg(test)]
	pub(crate) fn merge_with_resolutions(
		&self,
		input: &KernelMergeInput,
		resolutions: &TreeConflictResolutions,
	) -> Result<Vec<AstStatement>, String> {
		let outcome = self.merge_tentative_with_resolutions(input, resolutions)?;
		if !outcome.conflicts.is_empty() {
			return Err(format!(
				"tree merge conflict for {}: {:?}",
				input.base.path.display(),
				outcome.conflicts,
			));
		}
		Ok(outcome.statements)
	}

	pub(crate) fn merge_tentative(
		&self,
		input: &KernelMergeInput,
	) -> Result<TreeKernelOutcome, String> {
		self.merge_tentative_with_resolutions(input, &TreeConflictResolutions::new())
	}

	pub(crate) fn merge_tentative_with_resolutions(
		&self,
		input: &KernelMergeInput,
		resolutions: &TreeConflictResolutions,
	) -> Result<TreeKernelOutcome, String> {
		input.revisions.first().ok_or_else(|| {
			format!(
				"tree merge requires at least one revision for {}",
				input.base.path.display(),
			)
		})?;
		let revision_asts = input
			.revisions
			.iter()
			.map(|revision| &revision.ast)
			.collect::<Vec<_>>();
		let step = self.join.merge_n_way(
			&input.base,
			&revision_asts,
			self.policies,
			&resolutions.values().cloned().collect::<Vec<_>>(),
		)?;
		let merge_facts = attribute_kernel_facts(input, step.kernel_facts)?;
		Ok(TreeKernelOutcome {
			statements: step.statements,
			conflicts: step.conflicts,
			merge_facts,
		})
	}

	pub(crate) fn join_protocol_name(&self) -> &'static str {
		self.join.name()
	}
}

fn attribute_kernel_facts(
	input: &KernelMergeInput,
	facts: Vec<ClausewitzKernelFacts>,
) -> Result<Vec<SemanticMergeFacts>, String> {
	let sources = input
		.revisions
		.iter()
		.enumerate()
		.map(|(index, revision)| {
			let revision_id = RevisionId::new(
				u16::try_from(index + 1)
					.map_err(|_| "too many semantic merge revisions".to_string())?,
			);
			Ok((
				revision_id,
				SemanticMergeSource {
					source_id: revision.source_id.clone(),
					precedence: revision.precedence,
				},
			))
		})
		.collect::<Result<BTreeMap<_, _>, String>>()?;
	facts
		.into_iter()
		.map(|fact| {
			if fact
				.revision_trees
				.keys()
				.any(|revision| !sources.contains_key(revision))
			{
				return Err("merge adapter returned facts for an unknown revision".to_string());
			}
			Ok(SemanticMergeFacts {
				partition: fact.partition,
				sources: sources.clone(),
				base_tree: fact.base_tree,
				revision_trees: fact.revision_trees,
				outcome: fact.outcome,
			})
		})
		.collect()
}

#[derive(Default)]
struct TreeConflictResolution {
	resolutions: TreeConflictResolutions,
	handled_conflicts: BTreeMap<ConflictNodeId, ()>,
	unresolved_conflicts: Vec<SemanticMergeConflict>,
	handler_resolutions: Vec<HandlerResolutionRecord>,
	resolved_conflict_ids: Vec<String>,
	output_directives: Vec<MergeOutputDirective>,
}

fn resolve_tree_conflicts(
	input: &KernelMergeInput,
	conflicts: &[StructuralConflict],
	policies: &MergePolicies,
	handler: &mut dyn ConflictHandler,
) -> Result<TreeConflictResolution, String> {
	let mut resolution = TreeConflictResolution::default();
	let conflict_count = conflicts.len();
	handler.set_conflict_progress(0, conflict_count);
	for (index, conflict) in conflicts.iter().enumerate() {
		handler.set_conflict_progress(index + 1, conflict_count);
		let record = tree_conflict_record(input, conflict, policies)?;
		let view = semantic_conflict_view(&input.base.path, &record)?;
		let conflict_id = conflict.id.to_string();
		match handler.on_conflict(&view) {
			ConflictDecision::PickCandidate {
				candidate,
				record: log,
			} if record.source_selectable => {
				let Some(candidate) = record.candidates.get(candidate) else {
					resolution.unresolved_conflicts.push(record);
					continue;
				};
				let selected = conflict
					.select(candidate.source)
					.map_err(|error| format!("invalid exact tree conflict selection: {error}"))?;
				resolution.resolutions.insert(conflict.id, selected);
				resolution.handled_conflicts.insert(conflict.id, ());
				resolution.resolved_conflict_ids.push(conflict_id);
				if let Some(log) = log {
					resolution.handler_resolutions.push(log);
				}
			}
			ConflictDecision::PickCandidate { record: log, .. } => {
				if let Some(log) = log {
					resolution.handler_resolutions.push(log);
				}
				resolution.unresolved_conflicts.push(record);
			}
			ConflictDecision::Defer { record: None } => {
				resolution.unresolved_conflicts.push(record);
			}
			ConflictDecision::Defer { record: Some(log) } => {
				resolution.handler_resolutions.push(log);
				resolution.unresolved_conflicts.push(record);
			}
			ConflictDecision::UseFile(path) => {
				resolution.handled_conflicts.insert(conflict.id, ());
				resolution.resolved_conflict_ids.push(conflict_id);
				resolution
					.output_directives
					.push(MergeOutputDirective::UseFile(path));
			}
			ConflictDecision::KeepExisting => {
				resolution.handled_conflicts.insert(conflict.id, ());
				resolution.resolved_conflict_ids.push(conflict_id);
				resolution
					.output_directives
					.push(MergeOutputDirective::KeepExisting);
			}
			ConflictDecision::Abort => {
				return Err(format!(
					"conflict handler aborted tree merge for {} at {}",
					input.base.path.display(),
					conflict.semantic_path.join("/"),
				));
			}
		}
	}
	Ok(resolution)
}

fn tree_conflict_record(
	input: &KernelMergeInput,
	conflict: &StructuralConflict,
	policies: &MergePolicies,
) -> Result<SemanticMergeConflict, String> {
	let base = semantic_candidate(&input.base, &conflict.semantic_path, policies)?;
	let candidates = conflict
		.candidates
		.iter()
		.filter(|candidate| candidate.input().revision != RevisionId::BASE)
		.map(|source| {
			let revision = input_revision(input, source.input().revision)?;
			let statement = match source {
				SourceNodeRef::Node { .. } => {
					semantic_candidate(&revision.ast, &conflict.semantic_path, policies)?.statement
				}
				SourceNodeRef::Tombstone { .. } => None,
			};
			Ok(SemanticConflictCandidate {
				source: *source,
				source_id: revision.source_id.clone(),
				precedence: revision.precedence,
				statement,
			})
		})
		.collect::<Result<Vec<_>, String>>()?;
	Ok(SemanticMergeConflict {
		conflict: conflict.clone(),
		reason: format!("{}: {}", conflict.kind, conflict.detail),
		base_statement: base.statement,
		source_selectable: !candidates.is_empty(),
		candidates,
	})
}

fn input_revision(
	input: &KernelMergeInput,
	revision: RevisionId,
) -> Result<&KernelRevision, String> {
	let index = usize::from(revision.get())
		.checked_sub(1)
		.ok_or_else(|| "the merge base is not a selectable mod revision".to_string())?;
	input.revisions.get(index).ok_or_else(|| {
		format!(
			"conflict references unknown revision {} for {}",
			revision.get(),
			input.base.path.display(),
		)
	})
}

struct SemanticCandidate {
	statement: Option<AstStatement>,
}

fn semantic_candidate(
	file: &AstFile,
	semantic_path: &[String],
	policies: &MergePolicies,
) -> Result<SemanticCandidate, String> {
	if semantic_path.is_empty() {
		return Ok(SemanticCandidate { statement: None });
	}
	let policy = ContentFamilyMergePolicy::new(policies);
	let (semantic, _) = detach_trivia(file);
	let tree = normalize_ast(&semantic, &policy)
		.map_err(|error| format!("failed to normalize conflict candidate: {error}"))?;
	let matching_nodes = tree
		.nodes()
		.filter(|(_, node)| node.policy_path == semantic_path)
		.collect::<Vec<_>>();
	let assignments = matching_nodes
		.iter()
		.filter(|(_, node)| node.kind.starts_with("clausewitz.assignment:"))
		.map(|(node, _)| *node)
		.collect::<Vec<_>>();
	match assignments.as_slice() {
		[node] => Ok(SemanticCandidate {
			statement: Some(
				denormalize_statement(&tree, *node)
					.map_err(|error| format!("failed to render conflict candidate: {error}"))?,
			),
		}),
		[] if matching_nodes.is_empty() => Ok(SemanticCandidate { statement: None }),
		[] | [_, _, ..] => Ok(SemanticCandidate { statement: None }),
	}
}

fn split_semantic_address(path: &[String]) -> (Vec<String>, String) {
	match path.split_last() {
		Some((key, parent)) => (parent.to_vec(), key.clone()),
		None => (Vec::new(), "$file".to_string()),
	}
}

pub(crate) fn semantic_conflict_view(
	file_path: &std::path::Path,
	record: &SemanticMergeConflict,
) -> Result<ConflictView, String> {
	let (display_path, display_key) = split_semantic_address(&record.conflict.semantic_path);
	let candidates = record
		.candidates
		.iter()
		.map(|candidate| {
			let candidate_rendered = candidate.statement.as_ref().map_or_else(
				|| Ok("(removed)".to_string()),
				|statement| {
					emit_clausewitz_statements(std::slice::from_ref(statement))
						.map(|rendered| rendered.trim_end().to_string())
						.map_err(|error| format!("failed to emit tree conflict candidate: {error}"))
				},
			)?;
			Ok(CandidateView {
				mod_id: candidate.source_id.clone(),
				mod_display_name: candidate.source_id.clone(),
				precedence: candidate.precedence,
				change_summary: vec![format!(
					"select semantic subtree {}",
					record.conflict.semantic_path.join("/")
				)],
				candidate_rendered,
			})
		})
		.collect::<Result<Vec<_>, String>>()?;
	let vanilla_snippet = record
		.base_statement
		.as_ref()
		.map(|statement| emit_clausewitz_statements(std::slice::from_ref(statement)))
		.transpose()
		.map_err(|error| format!("failed to emit vanilla conflict candidate: {error}"))?
		.map(|rendered| rendered.trim_end().to_string());
	Ok(ConflictView {
		file_path: file_path.to_path_buf(),
		address_path: display_path.clone(),
		address_key: display_key.clone(),
		conflict_id: record.conflict.id.to_string(),
		reason: record.reason.clone(),
		vanilla_snippet,
		candidates,
	})
}

fn merge_tree_metadata(
	base: &TreeDagState,
	revisions: &[DagJoinRevision<'_, TreeDagState>],
) -> TreeDagState {
	let mut state = base.clone();
	for revision in revisions {
		for (partition, lineage) in &revision.state.partition_lineage {
			match state.partition_lineage.get_mut(partition) {
				Some(existing) if existing.tree == lineage.tree => {
					for (node, sources) in &lineage.sources {
						existing
							.sources
							.entry(*node)
							.or_default()
							.extend(sources.iter().cloned());
					}
				}
				Some(_) => {
					// An active partition is replaced below from its kernel facts.
				}
				None => {
					state
						.partition_lineage
						.insert(partition.clone(), lineage.clone());
				}
			}
		}
		for delta in &revision.state.source_deltas {
			if !state.source_deltas.contains(delta) {
				state.source_deltas.push(delta.clone());
			}
		}
		for facts in &revision.state.merge_facts {
			if !state.merge_facts.contains(facts) {
				state.merge_facts.push(facts.clone());
			}
		}
		for conflict in &revision.state.unresolved_conflicts {
			if !state.unresolved_conflicts.contains(conflict) {
				state.unresolved_conflicts.push(conflict.clone());
			}
		}
		for resolution in &revision.state.handler_resolutions {
			if !state.handler_resolutions.contains(resolution) {
				state.handler_resolutions.push(resolution.clone());
			}
		}
		for conflict_id in &revision.state.resolved_conflict_ids {
			if !state.resolved_conflict_ids.contains(conflict_id) {
				state.resolved_conflict_ids.push(conflict_id.clone());
			}
		}
		for resolution in &revision.state.conflict_resolutions {
			if !state.conflict_resolutions.contains(resolution) {
				state.conflict_resolutions.push(resolution.clone());
			}
		}
		for directive in &revision.state.output_directives {
			state.push_output_directive(directive.clone());
		}
	}
	state
}

fn compose_join_lineage(
	facts: &SemanticMergeFacts,
	base: &TreeDagState,
	revisions: &[DagJoinRevision<'_, TreeDagState>],
) -> Result<SemanticPartitionLineage, String> {
	let mut revision_lineage = BTreeMap::new();
	for (revision_id, source) in &facts.sources {
		let revision = revisions
			.iter()
			.find(|revision| {
				revision.mod_id.0 == source.source_id && revision.precedence == source.precedence
			})
			.ok_or_else(|| {
				format!(
					"merge facts reference missing DAG revision {} at precedence {}",
					source.source_id, source.precedence
				)
			})?;
		revision_lineage.insert(*revision_id, &revision.state.partition_lineage);
	}

	let mut sources = BTreeMap::new();
	for (output_node, input_sources) in &facts.outcome.provenance {
		let mut original_sources = BTreeSet::new();
		for input_source in input_sources.iter() {
			let (expected_tree, lineage) = if input_source.revision == RevisionId::BASE {
				(
					&facts.base_tree,
					base.partition_lineage.get(&facts.partition),
				)
			} else {
				let expected_tree = facts
					.revision_trees
					.get(&input_source.revision)
					.ok_or_else(|| {
						format!(
							"merge provenance references unknown revision {}",
							input_source.revision.get()
						)
					})?;
				let lineage = revision_lineage
					.get(&input_source.revision)
					.and_then(|partitions| partitions.get(&facts.partition));
				(expected_tree, lineage)
			};
			expected_tree.node(input_source.node).map_err(|error| {
				format!(
					"merge provenance references an invalid input node {}: {error}",
					input_source.node.get()
				)
			})?;
			if let Some(lineage) = lineage {
				if lineage.tree != *expected_tree {
					return Err(format!(
						"lineage tree does not match merge input for partition {:?}",
						facts.partition
					));
				}
				if let Some(node_sources) = lineage.sources.get(&input_source.node) {
					original_sources.extend(node_sources.iter().cloned());
				}
			}
		}
		if !original_sources.is_empty() {
			sources.insert(*output_node, original_sources);
		}
	}
	Ok(SemanticPartitionLineage {
		tree: facts.outcome.tentative_tree().clone(),
		sources,
	})
}

pub(crate) struct TreeDagProtocol<'a> {
	kernel: TreeMergeKernel<'a>,
	source_observer: TreeSourceObserver<'a>,
	has_vanilla_base: bool,
	handler: &'a mut dyn ConflictHandler,
}

impl<'a> TreeDagProtocol<'a> {
	pub(crate) fn new(
		partition_adapter: &'a dyn TreePartitionAdapter,
		join: &'a dyn TreeJoinProtocol,
		policies: &'a MergePolicies,
		has_vanilla_base: bool,
		handler: &'a mut dyn ConflictHandler,
	) -> Self {
		Self {
			kernel: TreeMergeKernel::new(join, policies),
			source_observer: TreeSourceObserver::new(partition_adapter, policies),
			has_vanilla_base,
			handler,
		}
	}
}

impl EffectiveNodeProtocol<TreeDagState> for TreeDagProtocol<'_> {
	fn effective_node(
		&mut self,
		request: EffectiveNodeRequest<'_, TreeDagState>,
	) -> Result<TreeDagState, String> {
		// A same-path mod file is a complete overlay. Definition-module callers
		// have already folded each mod and its ancestors into the supplied view.
		let source = SemanticMergeSource {
			source_id: request.mod_id.0.clone(),
			precedence: request.precedence,
		};
		let parent = AstFile {
			path: request.source.ast.path.clone(),
			statements: request.parent.statements.clone(),
		};
		let observation = self.source_observer.observe(
			&parent,
			&request.source.ast,
			source,
			&request.parent.partition_lineage,
		)?;
		let mut source_deltas = request.parent.source_deltas.clone();
		if !source_deltas.contains(&observation.delta) {
			source_deltas.push(observation.delta);
		}
		let mut partition_lineage = request.parent.partition_lineage.clone();
		partition_lineage.extend(observation.lineage);
		Ok(TreeDagState {
			statements: request.source.ast.statements.clone(),
			source_deltas,
			merge_facts: request.parent.merge_facts.clone(),
			partition_lineage,
			unresolved_conflicts: request.parent.unresolved_conflicts.clone(),
			handler_resolutions: request.parent.handler_resolutions.clone(),
			resolved_conflict_ids: request.parent.resolved_conflict_ids.clone(),
			conflict_resolutions: request.parent.conflict_resolutions.clone(),
			output_directives: request.parent.output_directives.clone(),
		})
	}
}

impl DagJoinProtocol<TreeDagState> for TreeDagProtocol<'_> {
	fn validate_final_frontier(
		&self,
		file_dag: &FileDag,
		root: &TreeDagState,
		sinks: &[ModId],
	) -> Result<(), String> {
		if sinks.len() > 1 && (!self.has_vanilla_base || root.statements.is_empty()) {
			return Err(format!(
				"tree merge unsupported for {}: a non-empty vanilla base is required",
				file_dag.file_path(),
			));
		}
		Ok(())
	}

	fn join(&mut self, request: DagJoinRequest<'_, TreeDagState>) -> Result<TreeDagState, String> {
		if request.base.statements.is_empty() {
			return Err(format!(
				"tree merge unsupported for {} {} join: a non-empty shared base is required",
				request.file_dag.file_path(),
				match request.plan.scope() {
					DagJoinScope::Intermediate => "intermediate",
					DagJoinScope::Final => "final",
				},
			));
		}
		let path = PathBuf::from(request.file_dag.file_path());
		let mut state = merge_tree_metadata(request.base, &request.revisions);
		let revisions = request
			.revisions
			.iter()
			.map(|revision| KernelRevision {
				source_id: revision.mod_id.0.clone(),
				precedence: revision.precedence,
				ast: AstFile {
					path: path.clone(),
					statements: revision.state.statements.clone(),
				},
			})
			.collect();
		let input = KernelMergeInput::new(
			AstFile {
				path,
				statements: request.base.statements.clone(),
			},
			revisions,
		);
		let probe = self.kernel.merge_tentative(&input).map_err(|error| {
			format!(
				"{} join protocol failed for {}: {error}",
				self.kernel.join_protocol_name(),
				request.file_dag.file_path(),
			)
		})?;
		let resolution =
			resolve_tree_conflicts(&input, &probe.conflicts, self.kernel.policies, self.handler)?;
		let outcome = if resolution.resolutions.is_empty() {
			probe
		} else {
			self.kernel
				.merge_tentative_with_resolutions(&input, &resolution.resolutions)
				.map_err(|error| {
					format!(
						"{} join protocol failed while replaying exact conflict selections for {}: {error}",
						self.kernel.join_protocol_name(),
						request.file_dag.file_path(),
					)
				})?
		};
		state.statements = outcome.statements;
		for facts in outcome.merge_facts {
			let lineage = compose_join_lineage(&facts, request.base, &request.revisions)?;
			state
				.partition_lineage
				.insert(facts.partition.clone(), lineage);
			if !state.merge_facts.contains(&facts) {
				state.merge_facts.push(facts);
			}
		}
		state
			.unresolved_conflicts
			.extend(resolution.unresolved_conflicts);
		state
			.handler_resolutions
			.extend(resolution.handler_resolutions);
		for conflict_id in resolution.resolved_conflict_ids {
			if !state.resolved_conflict_ids.contains(&conflict_id) {
				state.resolved_conflict_ids.push(conflict_id);
			}
		}
		for exact in resolution.resolutions.into_values() {
			if !state.conflict_resolutions.contains(&exact) {
				state.conflict_resolutions.push(exact);
			}
		}
		for directive in resolution.output_directives {
			state.push_output_directive(directive);
		}
		for conflict in outcome.conflicts {
			if resolution.handled_conflicts.contains_key(&conflict.id) {
				if state
					.conflict_resolutions
					.iter()
					.any(|exact| exact.conflict.id == conflict.id)
				{
					return Err(format!(
						"exact source selection did not resolve {} at {}",
						conflict.kind,
						conflict.semantic_path.join("/"),
					));
				}
				continue;
			}
			let record = tree_conflict_record(&input, &conflict, self.kernel.policies)?;
			if !state
				.unresolved_conflicts
				.iter()
				.any(|existing| existing.conflict.id == record.conflict.id)
			{
				state.unresolved_conflicts.push(record);
			}
		}
		Ok(state)
	}
}

#[cfg(test)]
mod tests {
	use std::cell::RefCell;
	use std::path::PathBuf;

	use foch_language::analyzer::content_family::{
		CwtType, MergePolicies, ScalarMergePolicy, ScalarReducerRule,
	};
	use foch_language::analyzer::parser::{
		AstFile, AstStatement, AstValue, ScalarValue, Span, SpanRange, parse_clausewitz_content,
	};
	use foch_language::analyzer::semantic_index::ParsedScriptFile;
	use foch_merge_kernel::{ConflictResolution, DeltaOperation, RevisionId};

	use super::{
		ClausewitzFileAdapter, ClausewitzFileJoin, DefinitionModuleAdapter, DefinitionModuleJoin,
		TreeConflictResolutions, TreeDagProtocol, TreeDagState, TreeJoinProtocol, TreeMergeKernel,
		TreeMergeStep, resolve_tree_conflicts,
	};
	use crate::emit::emit_clausewitz_statements;
	use crate::merge::conflict_handler::{ConflictDecision, ConflictHandler, DeferHandler};
	use crate::merge::conflict_view::ConflictView;
	use crate::merge::kernel::{KernelMergeInput, KernelRevision};
	use crate::merge::model::SemanticPartitionId;
	use crate::merge::planning::dag::{FileDag, ModId};
	use crate::merge::planning::dag_join::{DagJoinScope, plan_dag_join};
	use crate::merge::planning::dag_pipeline::{
		DagJoinProtocol, DagJoinRequest, DagJoinRevision, EffectiveNodeProtocol,
		EffectiveNodeRequest,
	};

	struct RecordingJoin {
		calls: RefCell<Vec<Vec<String>>>,
	}

	struct PickCandidateHandler {
		candidate: usize,
	}

	impl ConflictHandler for PickCandidateHandler {
		fn on_conflict(&mut self, _: &ConflictView) -> ConflictDecision {
			ConflictDecision::PickCandidate {
				candidate: self.candidate,
				record: None,
			}
		}
	}

	impl TreeJoinProtocol for RecordingJoin {
		fn name(&self) -> &'static str {
			"recording"
		}

		fn merge_n_way(
			&self,
			_base: &AstFile,
			revisions: &[&AstFile],
			_policies: &MergePolicies,
			_resolutions: &[ConflictResolution],
		) -> Result<TreeMergeStep, String> {
			self.calls.borrow_mut().push(
				revisions
					.iter()
					.map(|revision| scalar_value(revision).to_string())
					.collect(),
			);
			Ok(TreeMergeStep {
				statements: revisions
					.last()
					.expect("recording adapter requires revisions")
					.statements
					.clone(),
				conflicts: Vec::new(),
				kernel_facts: Vec::new(),
			})
		}
	}

	#[test]
	fn tree_kernel_passes_all_revisions_once_in_precedence_order() {
		let join = RecordingJoin {
			calls: RefCell::new(Vec::new()),
		};
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&join, &policies);
		let input = KernelMergeInput::new(
			file("base"),
			vec![
				revision("highest", 30, "right"),
				revision("lowest", 10, "left"),
				revision("middle", 20, "middle"),
			],
		);

		let merged = kernel.merge(&input).expect("merge revisions");

		assert_eq!(scalar_statement_value(&merged), "right");
		assert_eq!(
			join.calls.into_inner(),
			vec![vec![
				"left".to_string(),
				"middle".to_string(),
				"right".to_string(),
			]],
		);
	}

	#[test]
	fn tree_kernel_reduces_all_changed_numeric_contributors_once() {
		const RULES: &[ScalarReducerRule] = &[ScalarReducerRule::new(
			&["province_trade_power_modifier"],
			ScalarMergePolicy::Avg,
		)];
		let policies = MergePolicies {
			scalar_reducer_rules: RULES,
			..MergePolicies::default()
		};
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("cloves = { province_trade_power_modifier = 0 }\n"),
			vec![
				parsed_revision("a", 10, "cloves = { province_trade_power_modifier = .2 }\n"),
				parsed_revision("b", 20, "cloves = { province_trade_power_modifier = .1 }\n"),
				parsed_revision("c", 30, "cloves = { province_trade_power_modifier = .3 }\n"),
			],
		);

		let merged = kernel.merge(&input).expect("merge all contributors");
		let output = emit_clausewitz_statements(&merged).expect("emit merged AST");

		assert!(
			output.contains("province_trade_power_modifier = .2"),
			"{output}"
		);
	}

	#[test]
	fn tree_kernel_excludes_unchanged_base_copies_from_numeric_reducer() {
		const RULES: &[ScalarReducerRule] = &[ScalarReducerRule::new(
			&["province_trade_power_modifier"],
			ScalarMergePolicy::Avg,
		)];
		let policies = MergePolicies {
			scalar_reducer_rules: RULES,
			..MergePolicies::default()
		};
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("cloves = { province_trade_power_modifier = 0 }\n"),
			vec![
				parsed_revision("a", 10, "cloves = { province_trade_power_modifier = .2 }\n"),
				parsed_revision("b", 20, "cloves = { province_trade_power_modifier = .1 }\n"),
				parsed_revision(
					"unchanged",
					30,
					"cloves = { province_trade_power_modifier = 0 }\n",
				),
			],
		);

		let merged = kernel.merge(&input).expect("merge changed contributors");
		let output = emit_clausewitz_statements(&merged).expect("emit merged AST");

		assert!(
			output.contains("province_trade_power_modifier = .15"),
			"{output}"
		);
	}

	#[test]
	fn definition_module_adapter_merges_all_original_revisions() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&DefinitionModuleJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("alpha = { left = no right = no }\nbeta = { retained = yes }\n"),
			vec![
				parsed_revision(
					"a",
					10,
					"alpha = { left = yes right = no }\nbeta = { retained = yes }\n",
				),
				parsed_revision(
					"b",
					20,
					"alpha = { left = no right = yes }\nbeta = { retained = yes }\n",
				),
				parsed_revision(
					"unchanged",
					30,
					"alpha = { left = no right = no }\nbeta = { retained = yes }\n",
				),
			],
		);

		let outcome = kernel
			.merge_tentative(&input)
			.expect("merge definition module");
		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		let output = emit_clausewitz_statements(&outcome.statements).expect("emit merged module");

		assert!(output.contains("left = yes"), "{output}");
		assert!(output.contains("right = yes"), "{output}");
		assert!(output.contains("beta ="), "{output}");
		assert!(output.contains("retained = yes"), "{output}");
		assert_eq!(outcome.merge_facts.len(), 1);
		assert_eq!(
			outcome.merge_facts[0].partition,
			SemanticPartitionId::Definition("alpha".to_string())
		);
		assert_eq!(outcome.merge_facts[0].outcome.revision_deltas.len(), 3);
		assert_eq!(outcome.merge_facts[0].revision_trees.len(), 3);
	}

	#[test]
	fn tree_dag_state_retains_kernel_deltas_provenance_and_decisions() {
		let policies = MergePolicies::default();
		let base = tree_state("entry = { left = no right = no third = no }\n");
		let a = tree_state("entry = { left = yes right = no third = no }\n");
		let b = tree_state("entry = { left = no right = yes third = no }\n");
		let c = tree_state("entry = { left = no right = no third = yes }\n");
		let ids = [ModId::from("a"), ModId::from("b"), ModId::from("c")];
		let mut file_dag = FileDag::default();
		file_dag.file_path = "common/test.txt".to_string();
		let plan =
			plan_dag_join(&ids, &file_dag, DagJoinScope::Final).expect("plan independent join");
		let revisions = vec![
			DagJoinRevision {
				mod_id: &ids[0],
				precedence: 10,
				state: &a,
			},
			DagJoinRevision {
				mod_id: &ids[1],
				precedence: 20,
				state: &b,
			},
			DagJoinRevision {
				mod_id: &ids[2],
				precedence: 30,
				state: &c,
			},
		];
		let mut handler = DeferHandler;
		let mut protocol = TreeDagProtocol::new(
			&ClausewitzFileAdapter,
			&ClausewitzFileJoin,
			&policies,
			true,
			&mut handler,
		);

		let state = protocol
			.join(DagJoinRequest {
				plan: &plan,
				file_dag: &file_dag,
				base: &base,
				revisions,
			})
			.expect("join semantic DAG");

		assert_eq!(state.merge_facts.len(), 1);
		let facts = &state.merge_facts[0];
		assert_eq!(facts.sources.len(), 3);
		assert_eq!(
			facts
				.sources
				.values()
				.map(|source| source.source_id.as_str())
				.collect::<Vec<_>>(),
			vec!["a", "b", "c"]
		);
		assert_eq!(facts.outcome.revision_deltas.len(), 3);
		assert!(
			facts
				.outcome
				.revision_deltas
				.values()
				.all(|delta| !delta.operations.is_empty())
		);
		assert!(!facts.outcome.provenance.is_empty());
		assert!(!facts.outcome.decisions.is_empty());
	}

	#[test]
	fn effective_definition_node_records_parent_relative_partitioned_deltas() {
		let policies = MergePolicies::default();
		let parent = tree_state("alpha = { value = 1 }\nbeta = { value = 1 }\n");
		let source = parsed_script_file("mod-a", "beta = { value = 2 }\n");
		let mod_id = ModId::from("mod-a");
		let mut handler = DeferHandler;
		let mut protocol = TreeDagProtocol::new(
			&DefinitionModuleAdapter,
			&DefinitionModuleJoin,
			&policies,
			true,
			&mut handler,
		);

		let state = protocol
			.effective_node(EffectiveNodeRequest {
				mod_id: &mod_id,
				precedence: 40,
				parent: &parent,
				source: &source,
			})
			.expect("observe definition overlay");

		assert_eq!(state.statements, source.ast.statements);
		assert_eq!(state.source_deltas.len(), 1);
		let delta = &state.source_deltas[0];
		assert_eq!(delta.source.source_id, "mod-a");
		assert_eq!(delta.source.precedence, 40);
		assert_eq!(
			delta
				.partitions
				.iter()
				.map(|partition| partition.partition.clone())
				.collect::<Vec<_>>(),
			vec![
				SemanticPartitionId::Definition("alpha".to_string()),
				SemanticPartitionId::Definition("beta".to_string()),
			]
		);
		assert!(
			delta
				.partitions
				.iter()
				.all(|partition| partition.base_tree.root().get() == 0)
		);
		assert!(
			delta.partitions[0]
				.delta
				.operations
				.iter()
				.any(|operation| matches!(operation, DeltaOperation::Delete { .. }))
		);
		assert!(
			delta.partitions[1]
				.delta
				.operations
				.iter()
				.any(|operation| matches!(operation, DeltaOperation::Update { .. }))
		);
	}

	#[test]
	fn tree_kernel_can_select_any_original_source_across_three_revisions() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("value = 0\n"),
			vec![
				parsed_revision("a", 10, "value = 1\n"),
				parsed_revision("b", 20, "value = 2\n"),
				parsed_revision("c", 30, "value = 3\n"),
			],
		);

		for (source, expected) in [("a", "1"), ("b", "2"), ("c", "3")] {
			let resolutions = exact_resolution_for_source(&kernel, &input, source);
			let merged = kernel
				.merge_with_resolutions(&input, &resolutions)
				.unwrap_or_else(|error| panic!("select original source {source}: {error}"));
			let output = emit_clausewitz_statements(&merged).expect("emit selected AST");
			assert!(output.contains(&format!("value = {expected}")), "{output}");
		}
	}

	#[test]
	fn tree_kernel_can_select_an_original_source_deletion() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("value = 0\n"),
			vec![
				parsed_revision("a", 10, "value = 1\n"),
				parsed_revision("b", 20, ""),
				parsed_revision("c", 30, "value = 3\n"),
			],
		);

		for (source, expected) in [("a", Some("1")), ("b", None), ("c", Some("3"))] {
			let resolutions = exact_resolution_for_source(&kernel, &input, source);
			let merged = kernel
				.merge_with_resolutions(&input, &resolutions)
				.unwrap_or_else(|error| panic!("select original source {source}: {error}"));
			let output = emit_clausewitz_statements(&merged).expect("emit selected AST");
			match expected {
				Some(value) => assert!(output.contains(&format!("value = {value}")), "{output}"),
				None => assert!(!output.contains("value ="), "{output}"),
			}
		}
	}

	#[test]
	fn handler_selects_exact_source_when_display_mod_ids_repeat() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("value = 0\n"),
			vec![
				parsed_revision("same-mod", 10, "value = 1\n"),
				parsed_revision("same-mod", 20, "value = 2\n"),
				parsed_revision("other-mod", 30, "value = 3\n"),
			],
		);
		let probe = kernel.merge_tentative(&input).expect("probe conflicts");
		assert!(!probe.conflicts.is_empty());
		let mut handler = PickCandidateHandler { candidate: 1 };

		let resolution = resolve_tree_conflicts(&input, &probe.conflicts, &policies, &mut handler)
			.expect("resolve exact candidates");

		assert!(resolution.unresolved_conflicts.is_empty());
		assert_eq!(resolution.resolutions.len(), probe.conflicts.len());
		assert!(
			resolution
				.resolutions
				.values()
				.all(|selected| { selected.selected.input().revision == RevisionId::new(2) })
		);
		let merged = kernel
			.merge_with_resolutions(&input, &resolution.resolutions)
			.expect("replay exact source selection");
		let output = emit_clausewitz_statements(&merged).expect("emit selected AST");
		assert!(output.contains("value = 2"), "{output}");
	}

	fn exact_resolution_for_source(
		kernel: &TreeMergeKernel<'_>,
		input: &KernelMergeInput,
		source_id: &str,
	) -> TreeConflictResolutions {
		let probe = kernel.merge_tentative(input).expect("probe tree conflict");
		let revision = input
			.revisions
			.iter()
			.position(|revision| revision.source_id == source_id)
			.map(|index| RevisionId::new(u16::try_from(index + 1).unwrap()))
			.expect("requested source belongs to input");
		probe
			.conflicts
			.iter()
			.map(|conflict| {
				let selected = conflict
					.candidates
					.iter()
					.copied()
					.find(|candidate| candidate.input().revision == revision)
					.expect("requested source is an exact conflict candidate");
				(
					conflict.id,
					conflict.select(selected).expect("valid exact selection"),
				)
			})
			.collect()
	}

	fn revision(source_id: &str, precedence: usize, value: &str) -> KernelRevision {
		KernelRevision {
			source_id: source_id.to_string(),
			precedence,
			ast: file(value),
		}
	}

	fn parsed_revision(source_id: &str, precedence: usize, source: &str) -> KernelRevision {
		KernelRevision {
			source_id: source_id.to_string(),
			precedence,
			ast: parsed_file(source),
		}
	}

	fn parsed_file(source: &str) -> AstFile {
		let parsed = parse_clausewitz_content(PathBuf::from("common/test.txt"), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast
	}

	fn parsed_script_file(mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/test.txt");
		let parsed = parse_clausewitz_content(path.clone(), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("other"),
			module_name: "test".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn tree_state(source: &str) -> TreeDagState {
		TreeDagState {
			statements: parsed_file(source).statements,
			source_deltas: Vec::new(),
			merge_facts: Vec::new(),
			partition_lineage: Default::default(),
			unresolved_conflicts: Vec::new(),
			handler_resolutions: Vec::new(),
			resolved_conflict_ids: Vec::new(),
			conflict_resolutions: Vec::new(),
			output_directives: Vec::new(),
		}
	}

	fn file(value: &str) -> AstFile {
		AstFile {
			path: PathBuf::from("common/test.txt"),
			statements: vec![AstStatement::Assignment {
				key: "value".to_string(),
				key_span: span(),
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(value.to_string()),
					span: span(),
				},
				span: span(),
			}],
		}
	}

	fn scalar_value(file: &AstFile) -> &str {
		scalar_statement_value(&file.statements)
	}

	fn scalar_statement_value(statements: &[AstStatement]) -> &str {
		let [
			AstStatement::Assignment {
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(value),
					..
				},
				..
			},
		] = statements
		else {
			panic!("expected one identifier assignment")
		};
		value
	}

	fn span() -> SpanRange {
		SpanRange {
			start: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
			end: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
		}
	}
}
