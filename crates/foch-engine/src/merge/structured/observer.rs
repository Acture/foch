use std::collections::{BTreeMap, BTreeSet};

use foch::model::{MergeTraceContributor, MergeTraceDecision, MergeTraceEntry, MergeTracePolicy};
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
	SemanticOrigin, SemanticPartitionId, SemanticPartitionLineage, SemanticSourceDelta,
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
		resets_base: bool,
	) -> Result<TreeSourceObservation, String> {
		let partition_ids = self
			.partition_adapter
			.normalization_partitions(base, revision);
		let mut partitions = Vec::with_capacity(partition_ids.len());
		let mut lineage = BTreeMap::new();
		for partition in partition_ids {
			let base_tree = self
				.partition_adapter
				.normalize_partition(base, &partition, self.policies)
				.map_err(|error| format!("failed to normalize source-delta base: {error}"))?;
			let revision_tree = self
				.partition_adapter
				.normalize_partition(revision, &partition, self.policies)
				.map_err(|error| format!("failed to normalize source-delta revision: {error}"))?;
			let matching = self.matcher.match_trees(&base_tree, &revision_tree);
			let delta =
				RevisionDelta::between(&base_tree, RevisionId::LEFT, &revision_tree, &matching);
			let partition_lineage = apply_source_delta_lineage(
				&source,
				parent_lineage.get(&partition),
				&base_tree,
				&revision_tree,
				&delta,
				resets_base,
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
	resets_base: bool,
) -> Result<SemanticPartitionLineage, String> {
	match parent {
		Some(parent) if parent.tree != *base_tree => {
			return Err("parent lineage tree does not match contributor delta base".to_string());
		}
		None if !resets_base && !normalized_tree_is_empty_root(base_tree)? => {
			return Err("non-empty contributor delta base is missing parent lineage".to_string());
		}
		Some(_) | None => {}
	}
	if let Some(parent) = parent
		&& let Some((node, _)) = base_tree
			.nodes()
			.find(|(node, _)| !parent.origins.contains_key(node))
	{
		return Err(format!(
			"parent lineage is missing origins for base node {}",
			node.get(),
		));
	}

	let mut base_by_revision = BTreeMap::new();
	let mut revision_by_base = BTreeMap::new();
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
		if revision_by_base
			.insert(matched.base.node, matched.revision.node)
			.is_some()
		{
			return Err("contributor delta repeats a base node match".to_string());
		}
	}

	let mut replaced = BTreeSet::new();
	let mut augmented = BTreeSet::new();
	let mut deletion_augmented = BTreeSet::new();
	for operation in &delta.operations {
		match operation {
			DeltaOperation::Insert { .. } => {}
			DeltaOperation::Delete { tombstone } => {
				if tombstone.deleted.revision != RevisionId::BASE {
					return Err(
						"contributor delta deletion does not reference the base".to_string()
					);
				}
				if let Some(former_parent) = tombstone.former_parent {
					if former_parent.revision != RevisionId::BASE {
						return Err(
							"contributor delta deletion parent does not reference the base"
								.to_string(),
						);
					}
					let revision_parent = revision_by_base
						.get(&former_parent.node)
						.copied()
						.ok_or_else(|| {
							"contributor delta deletion parent has no surviving revision match"
								.to_string()
						})?;
					let revision_parent_node = revision_tree
						.node(revision_parent)
						.map_err(|error| format!("invalid surviving deletion parent: {error}"))?;
					if revision_parent_node.parent.is_some() {
						deletion_augmented.insert(revision_parent);
					}
				}
			}
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
	let mut origins = BTreeMap::new();
	let revision_is_empty_root = normalized_tree_is_empty_root(revision_tree)?;
	for (node_id, node) in revision_tree.nodes() {
		let matched_base = base_by_revision.get(&node_id).copied();
		let is_inserted = matched_base.is_none();
		let mut node_sources = if resets_base || replaced.contains(&node_id) || is_inserted {
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
			|| (resets_base && !revision_is_empty_root && node.children.is_empty())
		{
			node_sources.insert(source.clone());
		}
		if !node_sources.is_empty() {
			sources.insert(node_id, node_sources);
		}

		let mut node_origins = matched_base
			.and_then(|base| parent.and_then(|lineage| lineage.origins.get(&base)))
			.cloned()
			.unwrap_or_default();
		if (resets_base && !revision_is_empty_root)
			|| is_inserted
			|| replaced.contains(&node_id)
			|| augmented.contains(&node_id)
			|| deletion_augmented.contains(&node_id)
		{
			node_origins.insert(SemanticOrigin::Mod(source.clone()));
		}
		// Origins are a total node map. A synthetic empty root legitimately has
		// no semantic origin, but its empty set must remain addressable when a
		// later kernel join projects provenance through that root node.
		origins.insert(node_id, node_origins);
	}
	Ok(SemanticPartitionLineage {
		tree: revision_tree.clone(),
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
	use foch_language::analyzer::content_family::{MergeKeySource, MergePolicies};
	use foch_language::analyzer::parser::parse_clausewitz_content;

	use crate::merge::structured::{merge_clausewitz_files_n_way, normalize_clausewitz_partition};

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

	fn normalized(source: &str) -> NormalizedTree {
		normalize_clausewitz_partition(
			&parse(source),
			&SemanticPartitionId::File,
			&MergePolicies::default(),
		)
		.expect("normalize origin fixture")
	}

	fn source(mod_id: &str, precedence: usize) -> SemanticMergeSource {
		SemanticMergeSource {
			source_id: mod_id.to_string(),
			precedence,
		}
	}

	fn apply_origin_delta(
		base_tree: &NormalizedTree,
		revision_tree: &NormalizedTree,
		parent: Option<&SemanticPartitionLineage>,
		source: &SemanticMergeSource,
	) -> SemanticPartitionLineage {
		let matching = TreeMatcher::default().match_trees(base_tree, revision_tree);
		let delta = RevisionDelta::between(base_tree, RevisionId::LEFT, revision_tree, &matching);
		apply_source_delta_lineage(source, parent, base_tree, revision_tree, &delta, false)
			.expect("apply origin delta")
	}

	fn origins_for_value(
		lineage: &SemanticPartitionLineage,
		value: &str,
	) -> BTreeSet<SemanticOrigin> {
		let node = lineage
			.tree
			.nodes()
			.find_map(|(node, normalized)| {
				(normalized.value.as_deref() == Some(value)).then_some(node)
			})
			.unwrap_or_else(|| panic!("missing normalized value {value}"));
		lineage.origins.get(&node).cloned().unwrap_or_default()
	}

	fn condition_origins_for_tooltip(
		lineage: &SemanticPartitionLineage,
		tooltip: &str,
	) -> BTreeSet<SemanticOrigin> {
		let tooltip_node = lineage
			.tree
			.nodes()
			.find_map(|(node, normalized)| {
				(normalized.value.as_deref() == Some(tooltip)).then_some(node)
			})
			.unwrap_or_else(|| panic!("missing tooltip {tooltip}"));
		let mut current = Some(tooltip_node);
		let condition = loop {
			let node = current.expect("tooltip must be nested in a condition");
			let normalized = lineage.tree.node(node).expect("condition ancestor");
			if normalized.kind == "clausewitz.assignment:condition"
				&& normalized.value.as_deref() == Some("condition")
			{
				break node;
			}
			current = normalized.parent;
		};
		let mut origins = BTreeSet::new();
		let mut pending = vec![condition];
		while let Some(node) = pending.pop() {
			origins.extend(
				lineage
					.origins
					.get(&node)
					.unwrap_or_else(|| panic!("missing origins for node {}", node.get()))
					.iter()
					.cloned(),
			);
			pending.extend(
				lineage
					.tree
					.node(node)
					.expect("condition subtree node")
					.children
					.iter()
					.copied(),
			);
		}
		origins
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
	fn source_delta_preserves_vanilla_origin_and_records_modifier() {
		let base = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = vanilla } } }",
		);
		let revision = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = tweaked } } }",
		);
		let parent = SemanticPartitionLineage::vanilla(base.clone());
		let mod_a = source("mod_a", 10);

		let lineage = apply_origin_delta(&base, &revision, Some(&parent), &mod_a);

		assert_eq!(
			origins_for_value(&lineage, "tweaked"),
			BTreeSet::from([SemanticOrigin::Vanilla, SemanticOrigin::Mod(mod_a),]),
		);
		assert_eq!(
			origins_for_value(&lineage, "SHARED_TT"),
			BTreeSet::from([SemanticOrigin::Vanilla]),
		);
	}

	#[test]
	fn source_delta_marks_an_inserted_subtree_with_its_mod_origin() {
		let base = normalized("");
		let revision = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = from_a } } }",
		);
		let mod_a = source("mod_a", 10);

		let lineage = apply_origin_delta(&base, &revision, None, &mod_a);

		for value in ["send_warning", "condition", "SHARED_TT", "from_a"] {
			assert_eq!(
				origins_for_value(&lineage, value),
				BTreeSet::from([SemanticOrigin::Mod(mod_a.clone())]),
				"origin for {value}",
			);
		}
	}

	#[test]
	fn source_delta_reset_adopts_the_reset_source_and_preserves_semantic_ancestry() {
		let base = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = vanilla } } }",
		);
		let revision = base.clone();
		let parent = SemanticPartitionLineage::vanilla(base.clone());
		let mod_a = source("reset", 10);
		let matching = TreeMatcher::default().match_trees(&base, &revision);
		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);

		let lineage =
			apply_source_delta_lineage(&mod_a, Some(&parent), &base, &revision, &delta, true)
				.expect("apply reset origin delta");

		for value in ["send_warning", "condition", "SHARED_TT", "vanilla"] {
			assert_eq!(
				origins_for_value(&lineage, value),
				BTreeSet::from([SemanticOrigin::Vanilla, SemanticOrigin::Mod(mod_a.clone()),]),
				"origin for {value}",
			);
		}
		assert_eq!(
			lineage
				.sources
				.values()
				.flat_map(|sources| sources.iter().cloned())
				.collect::<BTreeSet<_>>(),
			BTreeSet::from([mod_a]),
			"the reset source must adopt byte-identical surviving content",
		);
	}

	#[test]
	fn source_delta_reset_does_not_attribute_an_empty_synthetic_root() {
		let base = normalized("shared = { value = vanilla }");
		let revision = normalized("");
		let parent = SemanticPartitionLineage::vanilla(base.clone());
		let reset = source("reset", 10);
		let matching = TreeMatcher::default().match_trees(&base, &revision);
		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);

		let lineage =
			apply_source_delta_lineage(&reset, Some(&parent), &base, &revision, &delta, true)
				.expect("apply empty reset delta");

		assert!(lineage.sources.is_empty());
		assert!(
			!lineage
				.origins
				.get(&lineage.tree.root())
				.expect("total empty-root origins")
				.contains(&SemanticOrigin::Mod(reset)),
		);
	}

	#[test]
	fn source_delta_rejects_missing_parent_lineage_for_a_non_empty_base() {
		let base = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = vanilla } } }",
		);
		let revision = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = tweaked } } }",
		);
		let matching = TreeMatcher::default().match_trees(&base, &revision);
		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);

		let error =
			apply_source_delta_lineage(&source("mod_a", 10), None, &base, &revision, &delta, false)
				.expect_err("non-empty bases require parent lineage");

		assert!(
			error.contains("non-empty contributor delta base"),
			"{error}"
		);
	}

	#[test]
	fn source_delta_rejects_a_partial_parent_origin_map() {
		let base = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = vanilla } } }",
		);
		let revision = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = tweaked } } }",
		);
		let matching = TreeMatcher::default().match_trees(&base, &revision);
		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);
		let mut parent = SemanticPartitionLineage::vanilla(base.clone());
		parent.origins.remove(&base.root());

		let error = apply_source_delta_lineage(
			&source("mod_a", 10),
			Some(&parent),
			&base,
			&revision,
			&delta,
			false,
		)
		.expect_err("parent origins must be total");

		assert!(error.contains("missing origins for base node"), "{error}");
	}

	#[test]
	fn source_delta_keeps_the_earliest_mod_origin_across_later_edits() {
		let empty = normalized("");
		let added = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = from_a } } }",
		);
		let modified = normalized(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = from_b } } }",
		);
		let mod_a = source("mod_a", 10);
		let mod_b = source("mod_b", 20);
		let added_lineage = apply_origin_delta(&empty, &added, None, &mod_a);

		let lineage = apply_origin_delta(&added, &modified, Some(&added_lineage), &mod_b);

		assert_eq!(
			origins_for_value(&lineage, "from_b"),
			BTreeSet::from([SemanticOrigin::Mod(mod_a), SemanticOrigin::Mod(mod_b),]),
		);
	}

	#[test]
	fn source_delta_attributes_a_delete_only_change_to_its_surviving_condition() {
		let base = normalized(
			"send_warning = {
				condition = { tooltip = TARGET_TT allow = { removed = yes retained = yes } }
				condition = { tooltip = SIBLING_TT allow = { sibling = yes } }
			}",
		);
		let revision = normalized(
			"send_warning = {
				condition = { tooltip = TARGET_TT allow = { retained = yes } }
				condition = { tooltip = SIBLING_TT allow = { sibling = yes } }
			}",
		);
		let mod_a = source("mod_a", 10);
		let parent = SemanticPartitionLineage::vanilla(base.clone());

		let lineage = apply_origin_delta(&base, &revision, Some(&parent), &mod_a);

		assert_eq!(
			condition_origins_for_tooltip(&lineage, "TARGET_TT"),
			BTreeSet::from([SemanticOrigin::Vanilla, SemanticOrigin::Mod(mod_a)]),
		);
		assert_eq!(
			condition_origins_for_tooltip(&lineage, "SIBLING_TT"),
			BTreeSet::from([SemanticOrigin::Vanilla]),
			"the deletion must not mark an unrelated sibling condition",
		);
	}

	#[test]
	fn source_delta_rejects_a_deleted_subtree_with_no_surviving_parent_match() {
		let base = normalized(
			"send_warning = { condition = { tooltip = TARGET_TT allow = { removed = yes retained = yes } } }",
		);
		let revision = normalized(
			"send_warning = { condition = { tooltip = TARGET_TT allow = { retained = yes } } }",
		);
		let matching = TreeMatcher::default().match_trees(&base, &revision);
		let mut delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);
		let former_parent = delta
			.operations
			.iter()
			.find_map(|operation| match operation {
				DeltaOperation::Delete { tombstone } => tombstone.former_parent,
				DeltaOperation::Insert { .. }
				| DeltaOperation::Update { .. }
				| DeltaOperation::Move { .. }
				| DeltaOperation::Rename { .. } => None,
			})
			.expect("deleted subtree parent");
		delta
			.matches
			.retain(|matched| matched.base != former_parent);

		let error = apply_source_delta_lineage(
			&source("mod_a", 10),
			Some(&SemanticPartitionLineage::vanilla(base.clone())),
			&base,
			&revision,
			&delta,
			false,
		)
		.expect_err("a deleted subtree parent must survive in the revision match");

		assert!(
			error.contains("deletion parent has no surviving revision match"),
			"{error}",
		);
	}

	#[test]
	fn source_delta_does_not_attach_a_top_level_delete_to_a_surviving_sibling() {
		let base = normalized(
			"send_warning = { condition = { tooltip = DELETED_TT allow = { deleted = yes } } }
			send_warning = { condition = { tooltip = SURVIVOR_TT allow = { retained = yes } } }",
		);
		let revision = normalized(
			"send_warning = { condition = { tooltip = SURVIVOR_TT allow = { retained = yes } } }",
		);
		let mod_a = source("mod_a", 10);
		let parent = SemanticPartitionLineage::vanilla(base.clone());

		let lineage = apply_origin_delta(&base, &revision, Some(&parent), &mod_a);

		assert_eq!(
			condition_origins_for_tooltip(&lineage, "SURVIVOR_TT"),
			BTreeSet::from([SemanticOrigin::Vanilla]),
			"a top-level deletion must not mark a surviving sibling definition",
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
