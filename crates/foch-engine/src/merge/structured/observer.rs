use std::collections::{BTreeMap, BTreeSet};

use foch_core::model::{
	MergeTraceContributor, MergeTraceDecision, MergeTraceEntry, MergeTracePolicy,
};
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, DivergentBlockPolicy, MergePolicies, NamedContainerPolicy,
};
use foch_language::analyzer::parser::AstFile;
use foch_merge_kernel::{
	DeltaOperation, MergeDecisionEvidence, MergeDecisionResult, NormalizedTree, RevisionDelta,
	RevisionId, RevisionNode, TreeMatcher,
};

use crate::merge::model::{
	SemanticDeltaPartition, SemanticMergeComputation, SemanticMergeFacts, SemanticMergeSource,
	SemanticPartitionId, SemanticPartitionLineage, SemanticSourceDelta,
};

use super::top_level_assignment_key;
use super::tree_kernel::TreePartitionAdapter;

pub(super) struct TreeSourceObservation {
	pub(super) delta: SemanticSourceDelta,
	pub(super) lineage: BTreeMap<SemanticPartitionId, SemanticPartitionLineage>,
}

pub(super) struct TreeSourceObserver<'a> {
	partition_adapter: &'a dyn TreePartitionAdapter,
	policies: &'a MergePolicies,
	matcher: TreeMatcher,
}

impl<'a> TreeSourceObserver<'a> {
	pub(super) fn new(
		partition_adapter: &'a dyn TreePartitionAdapter,
		policies: &'a MergePolicies,
	) -> Self {
		Self {
			partition_adapter,
			policies,
			matcher: TreeMatcher::default(),
		}
	}

	pub(super) fn observe(
		&self,
		base: &AstFile,
		revision: &AstFile,
		source: SemanticMergeSource,
		parent_lineage: &BTreeMap<SemanticPartitionId, SemanticPartitionLineage>,
	) -> Result<TreeSourceObservation, String> {
		let partition_ids = self
			.partition_adapter
			.normalization_partitions(base, revision);
		let mut partitions = Vec::with_capacity(partition_ids.len());
		let mut lineage = BTreeMap::new();
		for partition in partition_ids {
			let base_tree = super::normalize_clausewitz_partition(base, &partition, self.policies)
				.map_err(|error| format!("failed to normalize source-delta base: {error}"))?;
			let revision_tree =
				super::normalize_clausewitz_partition(revision, &partition, self.policies)
					.map_err(|error| {
						format!("failed to normalize source-delta revision: {error}")
					})?;
			let matching = self.matcher.match_trees(&base_tree, &revision_tree);
			let delta =
				RevisionDelta::between(&base_tree, RevisionId::LEFT, &revision_tree, &matching);
			let partition_lineage = apply_source_delta_lineage(
				&source,
				parent_lineage.get(&partition),
				&base_tree,
				&revision_tree,
				&delta,
			)?;
			if lineage
				.insert(partition.clone(), partition_lineage)
				.is_some()
			{
				return Err("source observation repeated a semantic partition".to_string());
			}
			partitions.push(SemanticDeltaPartition {
				partition,
				base_tree,
				revision_tree,
				delta,
			});
		}
		partitions.sort_by(|left, right| left.partition.cmp(&right.partition));
		Ok(TreeSourceObservation {
			delta: SemanticSourceDelta { source, partitions },
			lineage,
		})
	}
}

fn apply_source_delta_lineage(
	source: &SemanticMergeSource,
	parent: Option<&SemanticPartitionLineage>,
	base_tree: &NormalizedTree,
	revision_tree: &NormalizedTree,
	delta: &RevisionDelta,
) -> Result<SemanticPartitionLineage, String> {
	if let Some(parent) = parent
		&& parent.tree != *base_tree
	{
		return Err("parent lineage tree does not match contributor delta base".to_string());
	}

	let mut base_by_revision = BTreeMap::new();
	for matched in &delta.matches {
		if matched.base.revision != RevisionId::BASE
			|| matched.revision.revision == RevisionId::BASE
		{
			return Err("contributor delta contains an invalid node match".to_string());
		}
		if base_by_revision
			.insert(matched.revision.node, matched.base.node)
			.is_some()
		{
			return Err("contributor delta repeats a revision node match".to_string());
		}
	}

	let mut replaced = BTreeSet::new();
	let mut augmented = BTreeSet::new();
	for operation in &delta.operations {
		match operation {
			DeltaOperation::Insert { .. } | DeltaOperation::Delete { .. } => {}
			DeltaOperation::Update { revision, .. } => {
				replaced.insert(revision.node);
			}
			DeltaOperation::Move { revision, .. } | DeltaOperation::Rename { revision, .. } => {
				augmented.insert(revision.node);
			}
		}
	}
	for ordering in &delta.ordering {
		if [&ordering.parent, &ordering.before, &ordering.after]
			.iter()
			.any(|reference| reference.base.is_none())
		{
			continue;
		}
		for node in [
			ordering.parent.source,
			ordering.before.source,
			ordering.after.source,
		] {
			if node.revision != RevisionId::BASE {
				augmented.insert(node.node);
			}
		}
	}

	let mut sources = BTreeMap::new();
	for (node_id, node) in revision_tree.nodes() {
		let matched_base = base_by_revision.get(&node_id).copied();
		let is_inserted = matched_base.is_none();
		let mut node_sources = if replaced.contains(&node_id) || is_inserted {
			BTreeSet::new()
		} else {
			matched_base
				.and_then(|base| parent.and_then(|lineage| lineage.sources.get(&base)))
				.cloned()
				.unwrap_or_default()
		};
		if replaced.contains(&node_id)
			|| augmented.contains(&node_id)
			|| (is_inserted && node.children.is_empty())
		{
			node_sources.insert(source.clone());
		}
		if !node_sources.is_empty() {
			sources.insert(node_id, node_sources);
		}
	}
	Ok(SemanticPartitionLineage {
		tree: revision_tree.clone(),
		sources,
	})
}

#[derive(Default)]
struct DefinitionDecisionEvidence {
	decision_count: usize,
	combines_children: bool,
	revisions: BTreeSet<RevisionId>,
}

/// Project parser-independent kernel evidence into the stable, definition-level
/// merge trace used by reports. Materialization only publishes this projection;
/// it does not infer merge decisions from rendered output.
pub(crate) fn observe_merge_trace(
	provenance: &BTreeMap<String, Vec<String>>,
	participants: &BTreeMap<String, Vec<MergeTraceContributor>>,
	descriptor: &ContentFamilyDescriptor,
	semantic: Option<&SemanticMergeComputation>,
) -> Result<BTreeMap<String, MergeTraceEntry>, String> {
	let evidence = semantic
		.map(collect_definition_decision_evidence)
		.transpose()?
		.unwrap_or_default();
	let mut trace = BTreeMap::new();
	for (key, adopted_mods) in provenance {
		let policy = trace_policy_for_key(descriptor, key);
		let all_participants = participants.get(key).cloned().unwrap_or_default();
		let contributors = adopted_contributors(adopted_mods, &all_participants);
		let decision = trace_decision(
			policy,
			&contributors,
			all_participants.len(),
			evidence.get(key),
		);
		trace.insert(
			key.clone(),
			MergeTraceEntry {
				contributors,
				policy,
				decision,
			},
		);
	}
	Ok(trace)
}

fn adopted_contributors(
	adopted_mods: &[String],
	participants: &[MergeTraceContributor],
) -> Vec<MergeTraceContributor> {
	let mut contributors = adopted_mods
		.iter()
		.map(|mod_id| {
			participants
				.iter()
				.find(|participant| participant.mod_id == *mod_id)
				.cloned()
				.unwrap_or_else(|| MergeTraceContributor {
					mod_id: mod_id.clone(),
					precedence: usize::MAX,
					dag_level: usize::MAX,
				})
		})
		.collect::<Vec<_>>();
	contributors.sort_by(|left, right| {
		left.dag_level
			.cmp(&right.dag_level)
			.then_with(|| left.precedence.cmp(&right.precedence))
			.then_with(|| left.mod_id.cmp(&right.mod_id))
	});
	contributors
}

fn collect_definition_decision_evidence(
	semantic: &SemanticMergeComputation,
) -> Result<BTreeMap<String, DefinitionDecisionEvidence>, String> {
	let mut by_definition = BTreeMap::<String, DefinitionDecisionEvidence>::new();
	for facts in &semantic.merge_facts {
		for decision in &facts.outcome.decisions {
			let Some(key) = decision_definition_key(facts, decision)? else {
				continue;
			};
			let evidence = by_definition.entry(key).or_default();
			evidence.decision_count += 1;
			evidence.combines_children |=
				matches!(decision.result, MergeDecisionResult::CombineChildren);
			evidence.revisions.extend(
				decision
					.contributors
					.iter()
					.map(|source| source.revision)
					.filter(|revision| *revision != RevisionId::BASE),
			);
		}
	}
	Ok(by_definition)
}

fn decision_definition_key(
	facts: &SemanticMergeFacts,
	decision: &MergeDecisionEvidence,
) -> Result<Option<String>, String> {
	if let SemanticPartitionId::Definition(key) = &facts.partition {
		return Ok(Some(key.clone()));
	}
	let Some(source) = decision_source(decision) else {
		return Ok(None);
	};
	let tree = tree_for_revision(facts, source.revision)?;
	top_level_assignment_key(tree, source.node)
		.map(|key| key.map(str::to_string))
		.map_err(|error| format!("failed to project merge evidence to a definition: {error}"))
}

fn decision_source(decision: &MergeDecisionEvidence) -> Option<RevisionNode> {
	match &decision.result {
		MergeDecisionResult::SelectSource { source } => Some(*source),
		MergeDecisionResult::Delete { base, .. } => Some(*base),
		MergeDecisionResult::SynthesizeScalar { .. } | MergeDecisionResult::CombineChildren => {
			decision.contributors.iter().next().copied()
		}
	}
}

fn tree_for_revision(
	facts: &SemanticMergeFacts,
	revision: RevisionId,
) -> Result<&foch_merge_kernel::NormalizedTree, String> {
	if revision == RevisionId::BASE {
		return Ok(&facts.base_tree);
	}
	facts.revision_trees.get(&revision).ok_or_else(|| {
		format!(
			"merge evidence references unknown revision {} in partition {:?}",
			revision.get(),
			facts.partition,
		)
	})
}

fn trace_policy_for_key(descriptor: &ContentFamilyDescriptor, key: &str) -> MergeTracePolicy {
	match descriptor
		.merge_policies
		.divergent_block_policy_for_key(key)
	{
		DivergentBlockPolicy::Union => MergeTracePolicy::Union,
		DivergentBlockPolicy::BooleanOr => MergeTracePolicy::BooleanOr,
		DivergentBlockPolicy::LastWriter => MergeTracePolicy::Overlay,
		DivergentBlockPolicy::Recurse => {
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
	evidence: Option<&DefinitionDecisionEvidence>,
) -> MergeTraceDecision {
	let union_policy = matches!(
		policy,
		MergeTracePolicy::Union | MergeTracePolicy::BooleanOr | MergeTracePolicy::NamedContainer
	);
	let evidence_combines_sources = evidence.is_some_and(|evidence| {
		evidence.combines_children || evidence.revisions.len() > 1 || evidence.decision_count > 1
	});
	if contributors.len() > 1 && union_policy && (evidence.is_none() || evidence_combines_sources) {
		return MergeTraceDecision::Unioned;
	}
	if contributors.len() == 1 && participant_count > 1 {
		return MergeTraceDecision::Overridden;
	}
	MergeTraceDecision::Adopted
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::*;
	use foch_language::analyzer::content_family::MergeKeySource;
	use foch_language::analyzer::parser::parse_clausewitz_content;

	use crate::merge::structured::merge_clausewitz_files_n_way;

	fn participant(mod_id: &str, precedence: usize, dag_level: usize) -> MergeTraceContributor {
		MergeTraceContributor {
			mod_id: mod_id.to_string(),
			precedence,
			dag_level,
		}
	}

	fn parse(source: &str) -> AstFile {
		let parsed = parse_clausewitz_content(PathBuf::from("common/test.txt"), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast
	}

	#[test]
	fn trace_derivation_marks_union_of_two_mods() {
		let descriptor =
			ContentFamilyDescriptor::prefix("common/scripted_effects", "common/scripted_effects/")
				.merge_key(MergeKeySource::AssignmentKey)
				.divergent_block_policy(DivergentBlockPolicy::Union)
				.build();
		let provenance = BTreeMap::from([(
			"test_shared_effect".to_string(),
			vec!["mod_a".to_string(), "mod_b".to_string()],
		)]);
		let participants = BTreeMap::from([(
			"test_shared_effect".to_string(),
			vec![participant("mod_a", 1, 0), participant("mod_b", 2, 0)],
		)]);

		let trace = observe_merge_trace(&provenance, &participants, &descriptor, None).unwrap();
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
			.divergent_block_policy(DivergentBlockPolicy::LastWriter)
			.build();
		let provenance = BTreeMap::from([("shared_key".to_string(), vec!["mod_b".to_string()])]);
		let participants = BTreeMap::from([(
			"shared_key".to_string(),
			vec![participant("mod_a", 1, 0), participant("mod_b", 2, 1)],
		)]);

		let trace = observe_merge_trace(&provenance, &participants, &descriptor, None).unwrap();
		let entry = trace.get("shared_key").expect("trace entry");
		assert_eq!(entry.policy, MergeTracePolicy::Overlay);
		assert_eq!(entry.decision, MergeTraceDecision::Overridden);
		assert_eq!(entry.contributors[0].mod_id, "mod_b");
	}

	#[test]
	fn observer_projects_kernel_evidence_to_its_definition() {
		let descriptor = ContentFamilyDescriptor::prefix("common/test", "common/test/")
			.merge_key(MergeKeySource::AssignmentKey)
			.divergent_block_policy(DivergentBlockPolicy::Union)
			.build();
		let base = parse("shared = { base = yes }\n");
		let left = parse("shared = { base = yes left = yes }\n");
		let right = parse("shared = { base = yes right = yes }\n");
		let outcome =
			merge_clausewitz_files_n_way(&base, &[&left, &right], &descriptor.merge_policies)
				.expect("merge independent children");
		let statements = outcome.tentative_ast().statements.clone();
		let (_, facts) = outcome.into_parts(SemanticPartitionId::File);
		let semantic = SemanticMergeComputation {
			statements,
			source_deltas: Vec::new(),
			merge_facts: vec![SemanticMergeFacts {
				partition: facts.partition,
				sources: BTreeMap::from([
					(
						RevisionId::new(1),
						SemanticMergeSource {
							source_id: "mod_a".to_string(),
							precedence: 1,
						},
					),
					(
						RevisionId::new(2),
						SemanticMergeSource {
							source_id: "mod_b".to_string(),
							precedence: 2,
						},
					),
				]),
				base_tree: facts.base_tree,
				revision_trees: facts.revision_trees,
				outcome: facts.outcome,
			}],
			partition_lineage: BTreeMap::new(),
			unresolved_conflicts: Vec::new(),
			handler_resolutions: Vec::new(),
			resolved_conflict_ids: Vec::new(),
			conflict_resolutions: Vec::new(),
			output_directives: Vec::new(),
		};

		let evidence = collect_definition_decision_evidence(&semantic).unwrap();
		let shared = evidence.get("shared").expect("shared definition evidence");
		assert!(shared.decision_count > 0);
		assert!(shared.revisions.contains(&RevisionId::new(1)));
		assert!(shared.revisions.contains(&RevisionId::new(2)));
	}
}
