use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::content_family::MergePolicies;
use foch_language::analyzer::parser::{AstFile, AstStatement};
use foch_merge_kernel::{
	ConflictNodeId, ConflictResolution, MergeInputId, NodeId, NormalizedTree, RevisionId,
	SourceNodeRef, StructuralConflict,
};

use crate::emit::emit_clausewitz_statements;
use crate::merge::conflict_handler::{
	ConflictDecision, ConflictHandler, ConflictMetadataCandidate, ConflictMetadataView,
	ConflictViewRequirement, MetadataConflictDecision,
};
use crate::merge::conflict_view::{CandidateView, ConflictView};
use crate::merge::kernel::{KernelMergeInput, KernelRevision};
use crate::merge::model::{
	ExternalFileResolution, MergeOutputDirective, SemanticConflictCandidate,
	SemanticMergeComputation, SemanticMergeConflict, SemanticMergeFacts, SemanticMergeSource,
	SemanticPartitionId, SemanticPartitionLineage, VanillaBaseMode,
};
use crate::merge::planning::dag::{FileDag, ModId};
use crate::merge::planning::dag_join::DagJoinScope;
use crate::merge::planning::dag_pipeline::{
	DagJoinProtocol, DagJoinRequest, DagJoinRevision, EffectiveNodeProtocol, EffectiveNodeRequest,
};

use super::ast_adapter::denormalize_statement;
use super::definition_module::definition_module_partition_ids;
use super::observer::TreeSourceObserver;
use super::trivia::detach_trivia;
use super::{
	ClausewitzKernelFacts, merge_clausewitz_definition_module_n_way_with_resolutions,
	merge_clausewitz_files_n_way_with_resolutions, merge_event_files_n_way_with_resolutions,
};

pub(crate) type TreeConflictResolutions = BTreeMap<ConflictNodeId, ConflictResolution>;
pub(crate) type TreeDagState = SemanticMergeComputation;

/// Build the public, file-scoped identity for a semantic-tree conflict.
///
/// `ConflictNodeId` remains the kernel-internal resolution key. Its derivation
/// intentionally excludes the target path, so the same structural conflict in
/// two files can have the same raw id. Public and persisted ids bind that raw
/// identity to the slash-normalized merge target and retain the full digest.
pub(crate) fn semantic_conflict_id(target_path: &Path, raw_conflict_id: ConflictNodeId) -> String {
	let normalized_target_path = target_path.to_string_lossy().replace('\\', "/");
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-semantic-conflict-v1\0");
	hasher.update(normalized_target_path.as_bytes());
	hasher.update(b"\0");
	hasher.update(raw_conflict_id.as_bytes());
	hasher.finalize().to_hex().to_string()
}

pub(crate) trait TreePartitionAdapter {
	fn normalization_partitions(
		&self,
		_base: &AstFile,
		_revision: &AstFile,
	) -> Vec<SemanticPartitionId> {
		vec![SemanticPartitionId::File]
	}

	fn normalize_partition(
		&self,
		file: &AstFile,
		partition: &SemanticPartitionId,
		policies: &MergePolicies,
	) -> Result<foch_merge_kernel::NormalizedTree, super::AstAdapterError> {
		super::normalize_clausewitz_partition(file, partition, policies)
	}
}

pub(crate) trait TreeJoinProtocol {
	fn name(&self) -> &'static str;

	fn supports_sparse_reset_layers(&self) -> bool {
		false
	}

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

	fn normalize_partition(
		&self,
		file: &AstFile,
		partition: &SemanticPartitionId,
		policies: &MergePolicies,
	) -> Result<foch_merge_kernel::NormalizedTree, super::AstAdapterError> {
		if matches!(partition, SemanticPartitionId::File) {
			return super::normalize_clausewitz_partition(file, partition, policies);
		}
		// The module join detaches trivia before selecting and canonicalizing each
		// definition. Lineage must normalize in that same order: comments inside a
		// trigger otherwise change the temporary Boolean-OR wrapper shape.
		let (semantic, _) = detach_trivia(file);
		super::normalize_clausewitz_partition(&semantic, partition, policies)
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DefinitionModuleJoin;

impl TreeJoinProtocol for DefinitionModuleJoin {
	fn name(&self) -> &'static str {
		"definition-module"
	}

	fn supports_sparse_reset_layers(&self) -> bool {
		true
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
	#[cfg(test)]
	metadata_view_build_count: usize,
	#[cfg(test)]
	full_view_build_count: usize,
}

struct SemanticCandidateIndex {
	tree: NormalizedTree,
}

impl SemanticCandidateIndex {
	fn new(
		file: &AstFile,
		partition: &SemanticPartitionId,
		partition_adapter: &dyn TreePartitionAdapter,
		policies: &MergePolicies,
	) -> Result<Self, String> {
		let tree = partition_adapter
			.normalize_partition(file, partition, policies)
			.map_err(|error| format!("failed to normalize conflict candidate: {error}"))?;
		Ok(Self { tree })
	}

	fn statement(
		&self,
		node_id: NodeId,
		semantic_path: &[String],
	) -> Result<Option<AstStatement>, String> {
		let mut current = Some(node_id);
		while let Some(node_id) = current {
			let node = match self.tree.node(node_id) {
				Ok(node) => node,
				Err(_) => return Ok(None),
			};
			if node.kind.starts_with("clausewitz.assignment:")
				&& node.policy_path.starts_with(semantic_path)
			{
				return denormalize_statement(&self.tree, node_id)
					.map(Some)
					.map_err(|error| format!("failed to render conflict candidate: {error}"));
			}
			current = node.parent;
		}
		Ok(None)
	}
}

struct ConflictRecordBuilder<'a> {
	input: &'a KernelMergeInput,
	policies: &'a MergePolicies,
	partition_adapter: &'a dyn TreePartitionAdapter,
	partitions: BTreeSet<SemanticPartitionId>,
	indexes: BTreeMap<(RevisionId, SemanticPartitionId), SemanticCandidateIndex>,
	#[cfg(test)]
	record_build_count: usize,
	#[cfg(test)]
	normalized_input_count: usize,
}

impl<'a> ConflictRecordBuilder<'a> {
	fn new(
		input: &'a KernelMergeInput,
		policies: &'a MergePolicies,
		partition_adapter: &'a dyn TreePartitionAdapter,
	) -> Self {
		let partitions = input
			.revisions
			.iter()
			.flat_map(|revision| {
				partition_adapter.normalization_partitions(&input.base, &revision.ast)
			})
			.collect();
		Self {
			input,
			policies,
			partition_adapter,
			partitions,
			indexes: BTreeMap::new(),
			#[cfg(test)]
			record_build_count: 0,
			#[cfg(test)]
			normalized_input_count: 0,
		}
	}

	fn conflict_partition(&self, semantic_path: &[String]) -> Option<SemanticPartitionId> {
		if self.partitions.contains(&SemanticPartitionId::File) {
			return Some(SemanticPartitionId::File);
		}
		let partition = SemanticPartitionId::Definition(semantic_path.first()?.clone());
		self.partitions.contains(&partition).then_some(partition)
	}

	fn input_file(&self, revision_id: RevisionId) -> Result<&AstFile, String> {
		if revision_id == RevisionId::BASE {
			Ok(&self.input.base)
		} else {
			Ok(&input_revision(self.input, revision_id)?.ast)
		}
	}

	fn node_statement(
		&mut self,
		revision_id: RevisionId,
		node_id: NodeId,
		expected_input: Option<MergeInputId>,
		partition: &SemanticPartitionId,
		semantic_path: &[String],
	) -> Result<Option<AstStatement>, String> {
		let cache_key = (revision_id, partition.clone());
		if !self.indexes.contains_key(&cache_key) {
			let index = SemanticCandidateIndex::new(
				self.input_file(revision_id)?,
				partition,
				self.partition_adapter,
				self.policies,
			)?;
			self.indexes.insert(cache_key.clone(), index);
			#[cfg(test)]
			{
				self.normalized_input_count += 1;
			}
		}
		let index = self
			.indexes
			.get(&cache_key)
			.expect("semantic candidate index was inserted");
		if expected_input
			.is_some_and(|expected| MergeInputId::from_tree(revision_id, &index.tree) != expected)
		{
			return Ok(None);
		}
		index.statement(node_id, semantic_path)
	}

	fn record(&mut self, conflict: &StructuralConflict) -> Result<SemanticMergeConflict, String> {
		#[cfg(test)]
		{
			self.record_build_count += 1;
		}
		let partition = self.conflict_partition(&conflict.semantic_path);
		let base_statement = match (partition.as_ref(), conflict.base) {
			(Some(partition), Some(base)) => self.node_statement(
				base.revision,
				base.node,
				None,
				partition,
				&conflict.semantic_path,
			)?,
			_ => None,
		};
		let mut candidates = Vec::new();
		for source in conflict
			.candidates
			.iter()
			.filter(|candidate| candidate.input().revision != RevisionId::BASE)
		{
			let revision = input_revision(self.input, source.input().revision)?;
			let statement = match (source, partition.as_ref()) {
				(SourceNodeRef::Node { input, node }, Some(partition)) => self.node_statement(
					input.revision,
					*node,
					Some(*input),
					partition,
					&conflict.semantic_path,
				)?,
				(SourceNodeRef::Tombstone { .. }, _) => None,
				(SourceNodeRef::Node { .. }, None) => None,
			};
			candidates.push(SemanticConflictCandidate {
				source: *source,
				source_id: revision.source_id.clone(),
				precedence: revision.precedence,
				statement,
			});
		}
		Ok(SemanticMergeConflict {
			conflict: conflict.clone(),
			reason: format!("{}: {}", conflict.kind, conflict.detail),
			base_statement,
			source_selectable: !candidates.is_empty(),
			candidates,
		})
	}
}

fn resolve_tree_conflicts(
	builder: &mut ConflictRecordBuilder<'_>,
	conflicts: &[StructuralConflict],
	handler: &mut dyn ConflictHandler,
) -> Result<TreeConflictResolution, String> {
	let mut resolution = TreeConflictResolution::default();
	let conflict_count = conflicts.len();
	let view_requirement = handler.conflict_view_requirement();
	handler.set_conflict_progress(0, conflict_count);
	for (index, conflict) in conflicts.iter().enumerate() {
		handler.set_conflict_progress(index + 1, conflict_count);
		let conflict_id = semantic_conflict_id(&builder.input.base.path, conflict.id);
		let mut prebuilt_record = None;
		let decision = match view_requirement {
			ConflictViewRequirement::DeferWithoutView => ConflictDecision::Defer { record: None },
			ConflictViewRequirement::Metadata => {
				#[cfg(test)]
				{
					resolution.metadata_view_build_count += 1;
				}
				let view = tree_conflict_metadata(builder.input, conflict)?;
				match handler.on_conflict_metadata(&view) {
					MetadataConflictDecision::Decision(decision) => decision,
					MetadataConflictDecision::NeedsFullView => {
						#[cfg(test)]
						{
							resolution.full_view_build_count += 1;
						}
						let record = builder.record(conflict)?;
						let view = semantic_conflict_view(&builder.input.base.path, &record)?;
						let decision = handler.on_conflict(&view);
						prebuilt_record = Some(record);
						decision
					}
				}
			}
			ConflictViewRequirement::Full => {
				#[cfg(test)]
				{
					resolution.full_view_build_count += 1;
				}
				let record = builder.record(conflict)?;
				let view = semantic_conflict_view(&builder.input.base.path, &record)?;
				let decision = handler.on_conflict(&view);
				prebuilt_record = Some(record);
				decision
			}
		};
		match decision {
			ConflictDecision::PickCandidate {
				candidate,
				record: log,
			} if conflict
				.candidates
				.iter()
				.any(|candidate| candidate.input().revision != RevisionId::BASE) =>
			{
				let Some(candidate) = conflict
					.candidates
					.iter()
					.filter(|candidate| candidate.input().revision != RevisionId::BASE)
					.nth(candidate)
				else {
					push_unresolved_record(builder, conflict, prebuilt_record, &mut resolution)?;
					continue;
				};
				let selected = conflict
					.select(*candidate)
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
				push_unresolved_record(builder, conflict, prebuilt_record, &mut resolution)?;
			}
			ConflictDecision::Defer { record: None } => {
				push_unresolved_record(builder, conflict, prebuilt_record, &mut resolution)?;
			}
			ConflictDecision::Defer { record: Some(log) } => {
				resolution.handler_resolutions.push(log);
				push_unresolved_record(builder, conflict, prebuilt_record, &mut resolution)?;
			}
			ConflictDecision::UseFile(path) => {
				resolution.handled_conflicts.insert(conflict.id, ());
				resolution.resolved_conflict_ids.push(conflict_id);
				resolution
					.output_directives
					.push(MergeOutputDirective::UseFile(ExternalFileResolution::Live(
						path,
					)));
			}
			ConflictDecision::UseFrozenFile(path) => {
				resolution.handled_conflicts.insert(conflict.id, ());
				resolution.resolved_conflict_ids.push(conflict_id);
				resolution
					.output_directives
					.push(MergeOutputDirective::UseFile(
						ExternalFileResolution::Frozen(path),
					));
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
					builder.input.base.path.display(),
					conflict.semantic_path.join("/"),
				));
			}
		}
	}
	Ok(resolution)
}

fn push_unresolved_record(
	builder: &mut ConflictRecordBuilder<'_>,
	conflict: &StructuralConflict,
	prebuilt_record: Option<SemanticMergeConflict>,
	resolution: &mut TreeConflictResolution,
) -> Result<(), String> {
	resolution.unresolved_conflicts.push(match prebuilt_record {
		Some(record) => record,
		None => builder.record(conflict)?,
	});
	Ok(())
}

fn tree_conflict_metadata(
	input: &KernelMergeInput,
	conflict: &StructuralConflict,
) -> Result<ConflictMetadataView, String> {
	let (address_path, address_key) = split_semantic_address(&conflict.semantic_path);
	let candidates = conflict
		.candidates
		.iter()
		.filter(|candidate| candidate.input().revision != RevisionId::BASE)
		.map(|source| {
			let revision = input_revision(input, source.input().revision)?;
			Ok(ConflictMetadataCandidate {
				mod_id: revision.source_id.clone(),
				precedence: revision.precedence,
			})
		})
		.collect::<Result<Vec<_>, String>>()?;
	Ok(ConflictMetadataView {
		file_path: input.base.path.clone(),
		address_path,
		address_key,
		conflict_id: semantic_conflict_id(&input.base.path, conflict.id),
		reason: format!("{}: {}", conflict.kind, conflict.detail),
		candidates,
	})
}

fn collect_unresolved_conflict_records(
	builder: &mut ConflictRecordBuilder<'_>,
	conflicts: &[StructuralConflict],
	handled_conflicts: &BTreeMap<ConflictNodeId, ()>,
	reusable_records: Vec<SemanticMergeConflict>,
	existing_ids: &BTreeSet<ConflictNodeId>,
) -> Result<Vec<SemanticMergeConflict>, String> {
	let mut reusable_records = reusable_records
		.into_iter()
		.map(|record| (record.conflict.id, record))
		.collect::<BTreeMap<_, _>>();
	let mut seen_ids = existing_ids.clone();
	let mut records = Vec::new();
	for conflict in conflicts {
		if handled_conflicts.contains_key(&conflict.id) || !seen_ids.insert(conflict.id) {
			continue;
		}
		records.push(match reusable_records.remove(&conflict.id) {
			Some(record) => record,
			None => builder.record(conflict)?,
		});
	}
	Ok(records)
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
			let candidate_rendered = match (&candidate.statement, candidate.source) {
				(Some(statement), _) => emit_clausewitz_statements(std::slice::from_ref(statement))
					.map(|rendered| rendered.trim_end().to_string())
					.map_err(|error| format!("failed to emit tree conflict candidate: {error}")),
				(None, SourceNodeRef::Tombstone { .. }) => Ok("(removed)".to_string()),
				(None, SourceNodeRef::Node { .. }) => Ok("(unrenderable)".to_string()),
			}?;
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
		conflict_id: semantic_conflict_id(file_path, record.conflict.id),
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
					for (node, origins) in &lineage.origins {
						existing
							.origins
							.entry(*node)
							.or_default()
							.extend(origins.iter().cloned());
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

/// A definition-module view owned by `replace_path` is a sparse loader layer,
/// not a complete snapshot. If any active branch retains a vanilla definition,
/// an absent copy in that reset-owner layer is therefore neutral. Padding that
/// absent copy with the merge ancestor lets the ordinary N-way kernel express
/// that rule; definitions absent from every branch remain unpadded and are
/// still deleted. Non-reset revisions are deliberately never padded: their
/// omission can be an authoritative deletion in an effective descendant view.
///
/// The matching vanilla lineage is padded with the statement, and the reset
/// owner's synthetic deletion partition is removed from its source observation.
/// This keeps the neutral input provenance- and participant-free instead of
/// attributing the retained definition to the reset mod that omitted it.
fn neutralize_sparse_reset_definition_absence(
	file_dag: &FileDag,
	base: &TreeDagState,
	revisions: &[DagJoinRevision<'_, TreeDagState>],
) -> Result<Option<Vec<TreeDagState>>, String> {
	if std::iter::once(base)
		.chain(revisions.iter().map(|revision| revision.state))
		.flat_map(|state| &state.statements)
		.any(|statement| matches!(statement, AstStatement::Item { .. }))
	{
		return Ok(None);
	}

	let retained_keys = revisions
		.iter()
		.flat_map(|revision| &revision.state.statements)
		.filter_map(|statement| match statement {
			AstStatement::Assignment { key, .. } => Some(key.clone()),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
		})
		.collect::<BTreeSet<_>>();
	let mut retained_base_groups = BTreeMap::<String, Vec<AstStatement>>::new();
	for statement in &base.statements {
		let AstStatement::Assignment { key, .. } = statement else {
			continue;
		};
		if retained_keys.contains(key) {
			retained_base_groups
				.entry(key.clone())
				.or_default()
				.push(statement.clone());
		}
	}
	if retained_base_groups.is_empty() {
		return Ok(None);
	}

	let mut states = revisions
		.iter()
		.map(|revision| revision.state.clone())
		.collect::<Vec<_>>();
	for (revision, state) in revisions.iter().zip(&mut states) {
		if !file_dag.replaces_path(revision.mod_id) {
			continue;
		}
		let present_keys = state
			.statements
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment { key, .. } => Some(key.clone()),
				AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
			})
			.collect::<BTreeSet<_>>();
		let mut neutral_partitions = BTreeSet::new();
		for (key, base_group) in &retained_base_groups {
			if present_keys.contains(key) {
				continue;
			}
			state.statements.extend(base_group.iter().cloned());
			let partition = SemanticPartitionId::Definition(key.clone());
			let lineage = base.partition_lineage.get(&partition).ok_or_else(|| {
				format!(
					"non-empty reset merge ancestor is missing lineage for partition {partition:?}",
				)
			})?;
			state
				.partition_lineage
				.insert(partition.clone(), lineage.clone());
			neutral_partitions.insert(partition);
		}
		for delta in &mut state.source_deltas {
			if delta.source.source_id == revision.mod_id.0
				&& delta.source.precedence == revision.precedence
			{
				delta
					.partitions
					.retain(|partition| !neutral_partitions.contains(&partition.partition));
			}
		}
	}
	Ok(Some(states))
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
	let mut origins = BTreeMap::new();
	for (output_node, input_sources) in &facts.outcome.provenance {
		let mut original_sources = BTreeSet::new();
		let mut original_origins = BTreeSet::new();
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
				let node_origins = lineage.origins.get(&input_source.node).ok_or_else(|| {
					format!(
						"lineage is missing origins for input node {} in partition {:?}",
						input_source.node.get(),
						facts.partition,
					)
				})?;
				original_origins.extend(node_origins.iter().cloned());
			} else if !normalized_tree_is_empty_root(expected_tree)? {
				return Err(format!(
					"non-empty merge input is missing lineage for partition {:?}",
					facts.partition,
				));
			}
		}
		if !original_sources.is_empty() {
			sources.insert(*output_node, original_sources);
		}
		// Preserve a total node map, including the originless synthetic root used
		// by KnownAbsent and ExplicitlyDisabled empty-base joins.
		origins.insert(*output_node, original_origins);
	}
	Ok(SemanticPartitionLineage {
		tree: facts.outcome.tentative_tree().clone(),
		sources,
		origins,
	})
}

fn normalized_tree_is_empty_root(tree: &NormalizedTree) -> Result<bool, String> {
	let root = tree
		.node(tree.root())
		.map_err(|error| format!("normalized tree has an invalid root: {error}"))?;
	Ok(tree.len() == 1 && root.children.is_empty())
}

pub(crate) struct TreeDagProtocol<'a> {
	kernel: TreeMergeKernel<'a>,
	partition_adapter: &'a dyn TreePartitionAdapter,
	source_observer: TreeSourceObserver<'a>,
	has_vanilla_base: bool,
	vanilla_base_mode: VanillaBaseMode,
	handler: &'a mut dyn ConflictHandler,
}

impl<'a> TreeDagProtocol<'a> {
	pub(crate) fn new(
		partition_adapter: &'a dyn TreePartitionAdapter,
		join: &'a dyn TreeJoinProtocol,
		policies: &'a MergePolicies,
		has_vanilla_base: bool,
		vanilla_base_mode: VanillaBaseMode,
		handler: &'a mut dyn ConflictHandler,
	) -> Self {
		Self {
			kernel: TreeMergeKernel::new(join, policies),
			partition_adapter,
			source_observer: TreeSourceObserver::new(partition_adapter, policies),
			has_vanilla_base,
			vanilla_base_mode,
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
			request.resets_base,
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
			unresolved_conflicts: Vec::new(),
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
		if sinks.len() > 1
			&& self.vanilla_base_mode.requires_non_empty()
			&& (!self.has_vanilla_base || root.statements.is_empty())
		{
			return Err(format!(
				"tree merge unsupported for {}: a non-empty vanilla base is required",
				file_dag.file_path(),
			));
		}
		Ok(())
	}

	fn join(&mut self, request: DagJoinRequest<'_, TreeDagState>) -> Result<TreeDagState, String> {
		if self.vanilla_base_mode.requires_non_empty() && request.base.statements.is_empty() {
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
		let adjusted_states = if self.kernel.join.supports_sparse_reset_layers()
			&& request.file_dag.has_replace_path_owner()
		{
			neutralize_sparse_reset_definition_absence(
				request.file_dag,
				request.base,
				&request.revisions,
			)?
		} else {
			None
		};
		let merge_revisions = request
			.revisions
			.iter()
			.enumerate()
			.map(|(index, revision)| DagJoinRevision {
				mod_id: revision.mod_id,
				precedence: revision.precedence,
				state: adjusted_states
					.as_ref()
					.map_or(revision.state, |states| &states[index]),
			})
			.collect::<Vec<_>>();
		let mut state = merge_tree_metadata(request.base, &merge_revisions);
		let revisions = merge_revisions
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
		let mut record_builder =
			ConflictRecordBuilder::new(&input, self.kernel.policies, self.partition_adapter);
		let mut resolution =
			resolve_tree_conflicts(&mut record_builder, &probe.conflicts, self.handler)?;
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
			let lineage = compose_join_lineage(&facts, request.base, &merge_revisions)?;
			state
				.partition_lineage
				.insert(facts.partition.clone(), lineage);
			if !state.merge_facts.contains(&facts) {
				state.merge_facts.push(facts);
			}
		}
		for conflict in &outcome.conflicts {
			if resolution.resolutions.contains_key(&conflict.id) {
				return Err(format!(
					"exact source selection did not resolve {} at {}",
					conflict.kind,
					conflict.semantic_path.join("/"),
				));
			}
		}
		let replayed_conflict_ids = outcome
			.conflicts
			.iter()
			.map(|conflict| conflict.id)
			.collect::<BTreeSet<_>>();
		state.unresolved_conflicts.retain(|record| {
			!resolution
				.handled_conflicts
				.contains_key(&record.conflict.id)
				&& !replayed_conflict_ids.contains(&record.conflict.id)
		});
		let existing_unresolved_ids = state
			.unresolved_conflicts
			.iter()
			.map(|conflict| conflict.conflict.id)
			.collect::<BTreeSet<_>>();
		let reusable_records = std::mem::take(&mut resolution.unresolved_conflicts);
		state
			.unresolved_conflicts
			.extend(collect_unresolved_conflict_records(
				&mut record_builder,
				&outcome.conflicts,
				&resolution.handled_conflicts,
				reusable_records,
				&existing_unresolved_ids,
			)?);
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
		Ok(state)
	}
}

#[cfg(test)]
mod tests {
	use std::cell::RefCell;
	use std::collections::{BTreeMap, BTreeSet};
	use std::path::{Path, PathBuf};

	use foch_core::config::{ResolutionDecision, ResolutionMap};
	use foch_language::analyzer::content_family::{
		CwtType, GameProfile, MergePolicies, ScalarMergePolicy, ScalarReducerRule,
	};
	use foch_language::analyzer::parser::{
		AstFile, AstStatement, AstValue, ScalarValue, Span, SpanRange, parse_clausewitz_content,
	};
	use foch_language::analyzer::semantic_index::ParsedScriptFile;
	use foch_merge_kernel::{ConflictResolution, DeltaOperation, RevisionId};

	use super::{
		ClausewitzFileAdapter, ClausewitzFileJoin, ConflictRecordBuilder, DefinitionModuleAdapter,
		DefinitionModuleJoin, TreeConflictResolutions, TreeDagProtocol, TreeDagState,
		TreeJoinProtocol, TreeMergeKernel, TreeMergeStep, TreePartitionAdapter,
		collect_unresolved_conflict_records, compose_join_lineage, resolve_tree_conflicts,
		semantic_conflict_id,
	};
	use crate::emit::emit_clausewitz_statements;
	use crate::merge::conflict_handler::{
		ChainHandler, ConflictDecision, ConflictHandler, ConflictViewRequirement, DeferHandler,
		LookupHandler,
	};
	use crate::merge::conflict_view::ConflictView;
	use crate::merge::kernel::{KernelMergeInput, KernelRevision};
	use crate::merge::model::{
		SemanticMergeSource, SemanticOrigin, SemanticPartitionId, SemanticPartitionLineage,
		VanillaBaseMode,
	};
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
		seen_conflict_ids: Vec<String>,
	}

	impl ConflictHandler for PickCandidateHandler {
		fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
			self.seen_conflict_ids.push(view.conflict_id.clone());
			ConflictDecision::PickCandidate {
				candidate: self.candidate,
				record: None,
			}
		}
	}

	#[derive(Default)]
	struct FullViewDeferHandler {
		seen_views: usize,
	}

	impl ConflictHandler for FullViewDeferHandler {
		fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
			assert!(
				view.candidates
					.iter()
					.all(|candidate| !candidate.candidate_rendered.is_empty()),
				"full-view handlers must receive rendered candidates",
			);
			self.seen_views += 1;
			ConflictDecision::Defer { record: None }
		}
	}

	#[derive(Default)]
	struct CaptureDeferHandler {
		views: Vec<ConflictView>,
	}

	impl ConflictHandler for CaptureDeferHandler {
		fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
			self.views.push(view.clone());
			ConflictDecision::Defer { record: None }
		}
	}

	struct UseFileHandler;

	impl ConflictHandler for UseFileHandler {
		fn on_conflict(&mut self, _: &ConflictView) -> ConflictDecision {
			ConflictDecision::UseFile(PathBuf::from("resolved.txt"))
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
		let base = vanilla_tree_state("entry = { left = no right = no third = no }\n", &policies);
		let a = mod_file_tree_state(
			"entry = { left = yes right = no third = no }\n",
			"a",
			10,
			&policies,
		);
		let b = mod_file_tree_state(
			"entry = { left = no right = yes third = no }\n",
			"b",
			20,
			&policies,
		);
		let c = mod_file_tree_state(
			"entry = { left = no right = no third = yes }\n",
			"c",
			30,
			&policies,
		);
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
			VanillaBaseMode::Required,
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
	fn complete_overlay_clears_tentative_parent_conflicts_for_a_single_sink() {
		let policies = MergePolicies::default();
		let (base, _left, _right, intermediate, _ids, _file_dag) =
			conflicting_tree_dag_fixture(&policies);
		assert!(!intermediate.unresolved_conflicts.is_empty());
		let source = parsed_script_file("c", "value = 3\n");
		let mod_id = ModId::from("c");
		let mut handler = DeferHandler;
		let mut protocol = TreeDagProtocol::new(
			&ClausewitzFileAdapter,
			&ClausewitzFileJoin,
			&policies,
			true,
			VanillaBaseMode::Required,
			&mut handler,
		);

		let final_sink = protocol
			.effective_node(EffectiveNodeRequest {
				mod_id: &mod_id,
				precedence: 30,
				resets_base: false,
				parent: &intermediate,
				source: &source,
			})
			.expect("apply complete downstream overlay");

		assert_eq!(final_sink.statements, source.ast.statements);
		assert!(
			final_sink.unresolved_conflicts.is_empty(),
			"a complete overlay replaces the tentative parent snapshot",
		);
		assert!(
			!base.statements.is_empty(),
			"fixture must retain a real vanilla base"
		);
	}

	#[test]
	fn unrelated_later_join_preserves_inherited_unresolved_conflicts() {
		let policies = MergePolicies::default();
		let (_base, _left, _right, intermediate, ids, mut file_dag) =
			conflicting_tree_dag_fixture(&policies);
		let inherited = intermediate.unresolved_conflicts.clone();
		let pass_left = intermediate.clone();
		let pass_right = intermediate.clone();
		let pass_ids = [ModId::from("pass-left"), ModId::from("pass-right")];
		file_dag.file_path = "common/test.txt".to_string();
		let plan = plan_dag_join(&pass_ids, &file_dag, DagJoinScope::Final)
			.expect("plan unrelated pass-through join");
		let mut handler = DeferHandler;
		let mut protocol = TreeDagProtocol::new(
			&ClausewitzFileAdapter,
			&ClausewitzFileJoin,
			&policies,
			true,
			VanillaBaseMode::Required,
			&mut handler,
		);

		let joined = protocol
			.join(DagJoinRequest {
				plan: &plan,
				file_dag: &file_dag,
				base: &intermediate,
				revisions: vec![
					DagJoinRevision {
						mod_id: &pass_ids[0],
						precedence: 30,
						state: &pass_left,
					},
					DagJoinRevision {
						mod_id: &pass_ids[1],
						precedence: 40,
						state: &pass_right,
					},
				],
			})
			.expect("join unrelated pass-through states");

		assert_eq!(joined.unresolved_conflicts, inherited);
		assert_eq!(ids.len(), 2);
	}

	#[test]
	fn exact_current_resolution_replaces_same_id_inherited_unresolved_record() {
		let policies = MergePolicies::default();
		let (base, left, right, intermediate, ids, file_dag) =
			conflicting_tree_dag_fixture(&policies);
		let mut inherited_base = base;
		inherited_base.unresolved_conflicts = intermediate.unresolved_conflicts;
		let plan =
			plan_dag_join(&ids, &file_dag, DagJoinScope::Final).expect("plan exact replay join");
		let mut handler = PickCandidateHandler {
			candidate: 0,
			seen_conflict_ids: Vec::new(),
		};
		let mut protocol = TreeDagProtocol::new(
			&ClausewitzFileAdapter,
			&ClausewitzFileJoin,
			&policies,
			true,
			VanillaBaseMode::Required,
			&mut handler,
		);

		let joined = protocol
			.join(DagJoinRequest {
				plan: &plan,
				file_dag: &file_dag,
				base: &inherited_base,
				revisions: vec![
					DagJoinRevision {
						mod_id: &ids[0],
						precedence: 10,
						state: &left,
					},
					DagJoinRevision {
						mod_id: &ids[1],
						precedence: 20,
						state: &right,
					},
				],
			})
			.expect("replay exact current resolution");

		assert!(joined.unresolved_conflicts.is_empty());
	}

	#[test]
	fn output_directive_does_not_revalidate_an_ancestor_exact_resolution() {
		let policies = MergePolicies::default();
		let (base, left, right, intermediate, ids, file_dag) =
			conflicting_tree_dag_fixture(&policies);
		let old_record = intermediate
			.unresolved_conflicts
			.first()
			.expect("fixture conflict");
		let old_exact = old_record
			.conflict
			.select(
				old_record
					.candidates
					.first()
					.expect("selectable candidate")
					.source,
			)
			.expect("valid inherited exact selection");
		let mut inherited_base = base;
		inherited_base.conflict_resolutions.push(old_exact);
		let plan = plan_dag_join(&ids, &file_dag, DagJoinScope::Final)
			.expect("plan output directive join");
		let mut handler = UseFileHandler;
		let mut protocol = TreeDagProtocol::new(
			&ClausewitzFileAdapter,
			&ClausewitzFileJoin,
			&policies,
			true,
			VanillaBaseMode::Required,
			&mut handler,
		);

		let joined = protocol.join(DagJoinRequest {
			plan: &plan,
			file_dag: &file_dag,
			base: &inherited_base,
			revisions: vec![
				DagJoinRevision {
					mod_id: &ids[0],
					precedence: 10,
					state: &left,
				},
				DagJoinRevision {
					mod_id: &ids[1],
					precedence: 20,
					state: &right,
				},
			],
		});

		assert!(
			joined.is_ok(),
			"output directives are not exact replay selections: {joined:?}",
		);
	}

	#[test]
	fn compose_join_lineage_rejects_a_missing_non_empty_input_lineage() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("entry = { left = no right = no }\n"),
			vec![parsed_revision(
				"mod-a",
				10,
				"entry = { left = yes right = no }\n",
			)],
		);
		let outcome = kernel
			.merge_tentative(&input)
			.expect("merge lineage fixture");
		let facts = outcome.merge_facts.first().expect("file merge facts");
		let base = tree_state("entry = { left = no right = no }\n");
		let revision = tree_state("entry = { left = yes right = no }\n");
		let mod_id = ModId::from("mod-a");
		let revisions = [DagJoinRevision {
			mod_id: &mod_id,
			precedence: 10,
			state: &revision,
		}];

		let error = compose_join_lineage(facts, &base, &revisions)
			.expect_err("non-empty merge inputs require lineage");

		assert!(error.contains("non-empty merge input"), "{error}");
	}

	#[test]
	fn effective_definition_node_records_parent_relative_partitioned_deltas() {
		let policies = MergePolicies::default();
		let parent = vanilla_definition_tree_state(
			"alpha = { value = 1 }\nbeta = { value = 1 }\n",
			&policies,
		);
		let source = parsed_script_file("mod-a", "beta = { value = 2 }\n");
		let mod_id = ModId::from("mod-a");
		let mut handler = DeferHandler;
		let mut protocol = TreeDagProtocol::new(
			&DefinitionModuleAdapter,
			&DefinitionModuleJoin,
			&policies,
			true,
			VanillaBaseMode::Required,
			&mut handler,
		);

		let state = protocol
			.effective_node(EffectiveNodeRequest {
				mod_id: &mod_id,
				precedence: 40,
				resets_base: false,
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
	fn definition_module_lineage_matches_join_input_after_trivia_detach() {
		let mut failures = Vec::new();
		for (path, definition) in [
			("common/cb_types/zzz_foch_cb_types.txt", "cb_fidei_defensor"),
			(
				"common/new_diplomatic_actions/zzz_foch_new_diplomatic_actions.txt",
				"EE_spa_buy_india_port",
			),
			(
				"common/peace_treaties/zzz_foch_peace_treaties.txt",
				"IC_peace",
			),
			(
				"common/scripted_effects/zzz_foch_scripted_effects.txt",
				"ME_change_all_subject_colors",
			),
		] {
			let policies = foch_language::analyzer::eu4_profile::eu4_profile()
				.classify_content_family(Path::new(path))
				.unwrap_or_else(|| panic!("classify {path}"))
				.merge_policies;
			let base = vanilla_definition_tree_state("retained = { value = 0 }\n", &policies);
			let left_source = format!(
				"retained = {{ value = 0 }}\n\
				 {definition} = {{\n\
				 \ttrigger = {{\n\
				 \t\talways = yes # trivia must not affect the boolean tree\n\
				 \t}}\n\
				 }}\n"
			);
			let left_file = parsed_script_file_at(path, "left", &left_source);
			let right_file = parsed_script_file_at(path, "right", "retained = { value = 0 }\n");
			let ids = [ModId::from("left"), ModId::from("right")];
			let mut file_dag = FileDag::default();
			file_dag.file_path = path.to_string();
			let plan = plan_dag_join(&ids, &file_dag, DagJoinScope::Final)
				.expect("plan independent definition-module join");
			let mut handler = DeferHandler;
			let mut protocol = TreeDagProtocol::new(
				&DefinitionModuleAdapter,
				&DefinitionModuleJoin,
				&policies,
				true,
				VanillaBaseMode::Required,
				&mut handler,
			);
			let left = protocol
				.effective_node(EffectiveNodeRequest {
					mod_id: &ids[0],
					precedence: 10,
					resets_base: false,
					parent: &base,
					source: &left_file,
				})
				.expect("observe inserted definition");
			let right = protocol
				.effective_node(EffectiveNodeRequest {
					mod_id: &ids[1],
					precedence: 20,
					resets_base: false,
					parent: &base,
					source: &right_file,
				})
				.expect("observe unchanged module");

			let state = match protocol.join(DagJoinRequest {
				plan: &plan,
				file_dag: &file_dag,
				base: &base,
				revisions: vec![
					DagJoinRevision {
						mod_id: &ids[0],
						precedence: 10,
						state: &left,
					},
					DagJoinRevision {
						mod_id: &ids[1],
						precedence: 20,
						state: &right,
					},
				],
			}) {
				Ok(state) => state,
				Err(error) => {
					failures.push((definition, error));
					continue;
				}
			};
			let partition = SemanticPartitionId::Definition(definition.to_string());
			let lineage = state
				.partition_lineage
				.get(&partition)
				.unwrap_or_else(|| panic!("missing lineage for {definition}"));
			let output = AstFile {
				path: PathBuf::from(path),
				statements: state.statements.clone(),
			};
			let normalized = DefinitionModuleAdapter
				.normalize_partition(&output, &partition, &policies)
				.unwrap_or_else(|error| panic!("normalize joined {definition}: {error}"));
			assert_eq!(lineage.tree, normalized, "output lineage for {definition}");
			assert!(
				lineage
					.sources
					.values()
					.flatten()
					.any(|source| source.source_id == "left"),
				"missing original source for {definition}"
			);
		}
		assert!(failures.is_empty(), "lineage failures: {failures:#?}");
	}

	#[test]
	fn definition_module_file_fallback_lineage_matches_join_input_with_trivia() {
		let path = "common/cb_types/zzz_foch_cb_types.txt";
		let policies = foch_language::analyzer::eu4_profile::eu4_profile()
			.classify_content_family(Path::new(path))
			.expect("classify cb types")
			.merge_policies;
		let base_source = "loose_item\nretained = { trigger = { always = yes # trivia\n} }\n";
		let left_source =
			"loose_item\nretained = { trigger = { always = yes # trivia\n} value = 1 }\n";
		let base = vanilla_definition_tree_state(base_source, &policies);
		let left_file = parsed_script_file_at(path, "left", left_source);
		let right_file = parsed_script_file_at(path, "right", base_source);
		let ids = [ModId::from("left"), ModId::from("right")];
		let mut file_dag = FileDag::default();
		file_dag.file_path = path.to_string();
		let plan = plan_dag_join(&ids, &file_dag, DagJoinScope::Final)
			.expect("plan file-fallback definition-module join");
		let mut handler = DeferHandler;
		let mut protocol = TreeDagProtocol::new(
			&DefinitionModuleAdapter,
			&DefinitionModuleJoin,
			&policies,
			true,
			VanillaBaseMode::Required,
			&mut handler,
		);
		let left = protocol
			.effective_node(EffectiveNodeRequest {
				mod_id: &ids[0],
				precedence: 10,
				resets_base: false,
				parent: &base,
				source: &left_file,
			})
			.expect("observe left file fallback");
		let right = protocol
			.effective_node(EffectiveNodeRequest {
				mod_id: &ids[1],
				precedence: 20,
				resets_base: false,
				parent: &base,
				source: &right_file,
			})
			.expect("observe right file fallback");
		let state = protocol
			.join(DagJoinRequest {
				plan: &plan,
				file_dag: &file_dag,
				base: &base,
				revisions: vec![
					DagJoinRevision {
						mod_id: &ids[0],
						precedence: 10,
						state: &left,
					},
					DagJoinRevision {
						mod_id: &ids[1],
						precedence: 20,
						state: &right,
					},
				],
			})
			.expect("join file-fallback definition module");
		let lineage = state
			.partition_lineage
			.get(&SemanticPartitionId::File)
			.expect("file fallback lineage");
		let output = AstFile {
			path: PathBuf::from(path),
			statements: state.statements,
		};
		let normalized = DefinitionModuleAdapter
			.normalize_partition(&output, &SemanticPartitionId::File, &policies)
			.expect("normalize joined file fallback");
		assert_eq!(lineage.tree, normalized);
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
	fn default_defer_builds_no_views_and_reuses_each_unresolved_record() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("first = 0\nsecond = 0\n"),
			vec![
				parsed_revision("a", 10, "first = 1\nsecond = 1\n"),
				parsed_revision("b", 20, "first = 2\nsecond = 2\n"),
			],
		);
		let probe = kernel.merge_tentative(&input).expect("probe conflicts");
		assert!(
			probe.conflicts.len() >= 2,
			"expected a multi-conflict fixture"
		);
		let mut builder = ConflictRecordBuilder::new(&input, &policies, &ClausewitzFileAdapter);
		let mut handler = DeferHandler;
		assert_eq!(
			handler.conflict_view_requirement(),
			ConflictViewRequirement::DeferWithoutView,
		);

		let mut resolution = resolve_tree_conflicts(&mut builder, &probe.conflicts, &mut handler)
			.expect("defer conflicts");

		assert_eq!(resolution.metadata_view_build_count, 0);
		assert_eq!(resolution.full_view_build_count, 0);
		assert_eq!(builder.record_build_count, probe.conflicts.len());
		assert_eq!(
			builder.normalized_input_count,
			input.revisions.len() + 1,
			"each input AST should be normalized once across all conflicts",
		);
		let build_count = builder.record_build_count;
		let records = collect_unresolved_conflict_records(
			&mut builder,
			&probe.conflicts,
			&BTreeMap::new(),
			std::mem::take(&mut resolution.unresolved_conflicts),
			&BTreeSet::new(),
		)
		.expect("reuse unresolved records for final outcome");
		assert_eq!(records.len(), probe.conflicts.len());
		assert_eq!(
			builder.record_build_count, build_count,
			"final outcome must reuse records produced during handler dispatch",
		);
	}

	#[test]
	fn full_view_handler_still_receives_rendered_candidates() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("value = 0\n"),
			vec![
				parsed_revision("a", 10, "value = 1\n"),
				parsed_revision("b", 20, "value = 2\n"),
			],
		);
		let probe = kernel.merge_tentative(&input).expect("probe conflicts");
		assert!(!probe.conflicts.is_empty());
		let mut builder = ConflictRecordBuilder::new(&input, &policies, &ClausewitzFileAdapter);
		let mut handler = FullViewDeferHandler::default();

		let resolution = resolve_tree_conflicts(&mut builder, &probe.conflicts, &mut handler)
			.expect("dispatch full views");

		assert_eq!(resolution.metadata_view_build_count, 0);
		assert_eq!(resolution.full_view_build_count, probe.conflicts.len());
		assert_eq!(handler.seen_views, probe.conflicts.len());
		assert_eq!(builder.record_build_count, probe.conflicts.len());
	}

	#[test]
	fn definition_module_conflict_views_render_exact_duplicate_path_candidates() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&DefinitionModuleJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file(
				"before = { keep = 0 }\ntarget = { child = { id = a value = 0 } child = { id = b value = 0 } }\n",
			),
			vec![
				parsed_revision(
					"a",
					10,
					"before = { keep = 0 }\ntarget = { child = { id = a value = 1 } child = { id = b value = 0 } }\n",
				),
				parsed_revision(
					"b",
					20,
					"before = { keep = 0 }\ntarget = { child = { id = a value = 2 } child = { id = b value = 0 } }\n",
				),
			],
		);
		let probe = kernel.merge_tentative(&input).expect("probe conflicts");
		assert!(!probe.conflicts.is_empty(), "duplicate paths must conflict");
		let mut builder = ConflictRecordBuilder::new(&input, &policies, &DefinitionModuleAdapter);
		let mut handler = CaptureDeferHandler::default();

		resolve_tree_conflicts(&mut builder, &probe.conflicts, &mut handler)
			.expect("render exact definition candidates");

		let rendered = handler
			.views
			.iter()
			.flat_map(|view| view.candidates.iter())
			.map(|candidate| candidate.candidate_rendered.as_str())
			.collect::<Vec<_>>();
		assert!(
			handler.views.iter().any(|view| {
				view.vanilla_snippet
					.as_deref()
					.is_some_and(|snippet| snippet.contains("value = 0"))
			}),
			"the exact partition-scoped base node must render despite duplicate paths",
		);
		assert!(
			rendered.iter().all(|candidate| *candidate != "(removed)"),
			"node candidates must never be reported as deletions: {rendered:?}",
		);
		assert!(
			rendered
				.iter()
				.any(|candidate| candidate.contains("value = 1")),
			"missing exact a candidate: {rendered:?}",
		);
		assert!(
			rendered
				.iter()
				.any(|candidate| candidate.contains("value = 2")),
			"missing exact b candidate: {rendered:?}",
		);
	}

	#[test]
	fn only_tombstones_render_as_removed() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("value = 0\n"),
			vec![
				parsed_revision("deleted", 10, ""),
				parsed_revision("modified", 20, "value = 2\n"),
			],
		);
		let probe = kernel.merge_tentative(&input).expect("probe conflicts");
		assert!(!probe.conflicts.is_empty());
		let mut builder = ConflictRecordBuilder::new(&input, &policies, &ClausewitzFileAdapter);
		let mut handler = CaptureDeferHandler::default();

		resolve_tree_conflicts(&mut builder, &probe.conflicts, &mut handler)
			.expect("render delete-modify candidates");

		for (conflict, view) in probe.conflicts.iter().zip(&handler.views) {
			let sources = conflict
				.candidates
				.iter()
				.filter(|source| source.input().revision != RevisionId::BASE);
			for (source, candidate) in sources.zip(&view.candidates) {
				assert_eq!(
					candidate.candidate_rendered == "(removed)",
					matches!(source, foch_merge_kernel::SourceNodeRef::Tombstone { .. }),
					"candidate kind and rendering diverged: {source:?}",
				);
			}
		}
	}

	#[test]
	fn node_with_a_mismatched_partition_root_renders_as_unrenderable() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("value = 0\n"),
			vec![
				parsed_revision("a", 10, "value = 1\n"),
				parsed_revision("b", 20, "value = 2\n"),
			],
		);
		let probe = kernel.merge_tentative(&input).expect("probe conflicts");
		let mut conflict = probe.conflicts.first().expect("fixture conflict").clone();
		let source_index = conflict
			.candidates
			.iter()
			.position(|source| {
				matches!(
					source,
					foch_merge_kernel::SourceNodeRef::Node { input, .. }
						if input.revision == RevisionId::new(1)
				)
			})
			.expect("a node candidate");
		let wrong_root_hash = conflict
			.candidates
			.iter()
			.find(|source| source.input().revision == RevisionId::new(2))
			.expect("b candidate")
			.input()
			.root_hash;
		let foch_merge_kernel::SourceNodeRef::Node {
			input: mut candidate_input,
			node,
		} = conflict.candidates[source_index]
		else {
			unreachable!("source index was filtered to node candidates")
		};
		candidate_input.root_hash = wrong_root_hash;
		conflict.candidates[source_index] = foch_merge_kernel::SourceNodeRef::Node {
			input: candidate_input,
			node,
		};
		let mut builder = ConflictRecordBuilder::new(&input, &policies, &ClausewitzFileAdapter);

		let record = builder.record(&conflict).expect("build guarded record");
		let view = super::semantic_conflict_view(&input.base.path, &record)
			.expect("render guarded conflict");
		let candidate_index = conflict.candidates[..source_index]
			.iter()
			.filter(|source| source.input().revision != RevisionId::BASE)
			.count();

		assert_eq!(
			view.candidates[candidate_index].candidate_rendered,
			"(unrenderable)",
		);
	}

	#[test]
	fn lookup_builds_full_view_only_for_the_matching_named_handler() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileJoin, &policies);
		let input = KernelMergeInput::new(
			parsed_file("first = 0\nsecond = 0\nthird = 0\n"),
			vec![
				parsed_revision("a", 10, "first = 1\nsecond = 1\nthird = 1\n"),
				parsed_revision("b", 20, "first = 2\nsecond = 2\nthird = 2\n"),
			],
		);
		let probe = kernel.merge_tentative(&input).expect("probe conflicts");
		assert!(probe.conflicts.len() >= 3, "expected three conflicts");
		let selected_id = semantic_conflict_id(&input.base.path, probe.conflicts[1].id);
		let map = ResolutionMap {
			by_conflict_id: BTreeMap::from([(
				selected_id,
				ResolutionDecision::Handler("defer".to_string()),
			)]),
			..ResolutionMap::default()
		};
		let mut handler = ChainHandler {
			first: LookupHandler::new(&map, input.base.path.clone()),
			second: DeferHandler,
		};
		let mut builder = ConflictRecordBuilder::new(&input, &policies, &ClausewitzFileAdapter);

		let resolution = resolve_tree_conflicts(&mut builder, &probe.conflicts, &mut handler)
			.expect("dispatch lookup conflicts");

		assert_eq!(resolution.metadata_view_build_count, probe.conflicts.len());
		assert_eq!(resolution.full_view_build_count, 1);
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
		let expected_conflict_ids = probe
			.conflicts
			.iter()
			.map(|conflict| semantic_conflict_id(&input.base.path, conflict.id))
			.collect::<Vec<_>>();
		let mut handler = PickCandidateHandler {
			candidate: 1,
			seen_conflict_ids: Vec::new(),
		};

		let mut record_builder =
			ConflictRecordBuilder::new(&input, &policies, &ClausewitzFileAdapter);
		let resolution =
			resolve_tree_conflicts(&mut record_builder, &probe.conflicts, &mut handler)
				.expect("resolve exact candidates");

		assert!(resolution.unresolved_conflicts.is_empty());
		assert_eq!(handler.seen_conflict_ids, expected_conflict_ids);
		assert_eq!(resolution.resolved_conflict_ids, expected_conflict_ids);
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

	fn conflicting_tree_dag_fixture(
		policies: &MergePolicies,
	) -> (
		TreeDagState,
		TreeDagState,
		TreeDagState,
		TreeDagState,
		[ModId; 2],
		FileDag,
	) {
		let base = vanilla_tree_state("value = 0\n", policies);
		let sources = [
			parsed_script_file("a", "value = 1\n"),
			parsed_script_file("b", "value = 2\n"),
		];
		let ids = [ModId::from("a"), ModId::from("b")];
		let mut file_dag = FileDag::default();
		file_dag.file_path = "common/test.txt".to_string();
		let plan = plan_dag_join(&ids, &file_dag, DagJoinScope::Intermediate)
			.expect("plan conflicting intermediate join");
		let mut handler = DeferHandler;
		let mut protocol = TreeDagProtocol::new(
			&ClausewitzFileAdapter,
			&ClausewitzFileJoin,
			policies,
			true,
			VanillaBaseMode::Required,
			&mut handler,
		);
		let left = protocol
			.effective_node(EffectiveNodeRequest {
				mod_id: &ids[0],
				precedence: 10,
				resets_base: false,
				parent: &base,
				source: &sources[0],
			})
			.expect("observe left conflict branch");
		let right = protocol
			.effective_node(EffectiveNodeRequest {
				mod_id: &ids[1],
				precedence: 20,
				resets_base: false,
				parent: &base,
				source: &sources[1],
			})
			.expect("observe right conflict branch");
		let intermediate = protocol
			.join(DagJoinRequest {
				plan: &plan,
				file_dag: &file_dag,
				base: &base,
				revisions: vec![
					DagJoinRevision {
						mod_id: &ids[0],
						precedence: 10,
						state: &left,
					},
					DagJoinRevision {
						mod_id: &ids[1],
						precedence: 20,
						state: &right,
					},
				],
			})
			.expect("build conflicting intermediate state");
		(base, left, right, intermediate, ids, file_dag)
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
		parsed_script_file_at("common/test.txt", mod_id, source)
	}

	fn parsed_script_file_at(path: &str, mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from(path);
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

	fn vanilla_tree_state(source: &str, policies: &MergePolicies) -> TreeDagState {
		lineaged_tree_state(
			source,
			&ClausewitzFileAdapter,
			policies,
			SemanticOrigin::Vanilla,
		)
	}

	fn vanilla_definition_tree_state(source: &str, policies: &MergePolicies) -> TreeDagState {
		lineaged_tree_state(
			source,
			&DefinitionModuleAdapter,
			policies,
			SemanticOrigin::Vanilla,
		)
	}

	fn mod_file_tree_state(
		source: &str,
		mod_id: &str,
		precedence: usize,
		policies: &MergePolicies,
	) -> TreeDagState {
		lineaged_tree_state(
			source,
			&ClausewitzFileAdapter,
			policies,
			SemanticOrigin::Mod(SemanticMergeSource {
				source_id: mod_id.to_string(),
				precedence,
			}),
		)
	}

	fn lineaged_tree_state(
		source: &str,
		adapter: &dyn TreePartitionAdapter,
		policies: &MergePolicies,
		origin: SemanticOrigin,
	) -> TreeDagState {
		let file = parsed_file(source);
		let mut state = tree_state(source);
		for partition in adapter.normalization_partitions(&file, &file) {
			let tree = adapter
				.normalize_partition(&file, &partition, policies)
				.expect("normalize lineaged test state");
			let origins = tree
				.nodes()
				.map(|(node, _)| (node, BTreeSet::from([origin.clone()])))
				.collect();
			state.partition_lineage.insert(
				partition,
				SemanticPartitionLineage {
					tree,
					sources: BTreeMap::new(),
					origins,
				},
			);
		}
		state
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
