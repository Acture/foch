use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::merge::kernel::{
	ChildOrder, ClassId, ConflictKind, MergeDecisionEvidence, MergeDecisionReason,
	MergeDecisionResult, MergePolicy, MergePolicyKind, MergeRevision, NWayClassContext,
	NWayCorrespondence, NWayDeleteContext, NWayNodeView, NWayScalarSynthesis, NWaySelectionPlan,
	NodeId, NormalizedNode, NormalizedTree, PolicyDecision, RevisionId, RevisionNode, SourceSet,
	StructuralConflictDraft,
};

/// Records keep their insertion order while policies replace only a class's
/// own state. A conflict can affect several classes: clearing it through any
/// one class must make it disappear from every other class's lookup as well.
#[derive(Default)]
struct PolicyRecords {
	conflicts: Vec<Option<StructuralConflictDraft>>,
	conflicts_by_class: BTreeMap<ClassId, Vec<usize>>,
	decisions: Vec<Option<MergeDecisionEvidence>>,
	decisions_by_class: BTreeMap<ClassId, Vec<usize>>,
}

impl PolicyRecords {
	fn take(plan: &mut NWaySelectionPlan, correspondence: &NWayCorrespondence) -> Self {
		let mut records: Self = Self::default();
		for conflict in std::mem::take(&mut plan.conflicts) {
			records.push_conflict(conflict, correspondence);
		}
		for decision in std::mem::take(&mut plan.decisions) {
			records.push_decision(decision);
		}
		records
	}

	fn push_conflict(
		&mut self,
		conflict: StructuralConflictDraft,
		correspondence: &NWayCorrespondence,
	) {
		let classes: BTreeSet<ClassId> = conflict
			.revisions
			.iter()
			.copied()
			.chain(conflict.base)
			.map(|source| correspondence.classes.class_of(source))
			.collect();
		for class in classes {
			self.conflicts_by_class
				.entry(class)
				.or_default()
				.push(self.conflicts.len());
		}
		self.conflicts.push(Some(conflict));
	}

	fn push_decision(&mut self, decision: MergeDecisionEvidence) {
		self.decisions_by_class
			.entry(decision.affected_class)
			.or_default()
			.push(self.decisions.len());
		self.decisions.push(Some(decision));
	}

	fn remove_for_class(&mut self, class: ClassId) {
		if let Some(indices) = self.decisions_by_class.remove(&class) {
			for index in indices {
				self.decisions[index] = None;
			}
		}
		if let Some(indices) = self.conflicts_by_class.get(&class) {
			for &index in indices {
				if self.conflicts[index].as_ref().is_some_and(|conflict| {
					matches!(
						conflict.kind,
						ConflictKind::InsertInsert
							| ConflictKind::DeleteModify
							| ConflictKind::Policy
					)
				}) {
					self.conflicts[index] = None;
				}
			}
		}
	}

	fn has_conflict(&self, class: ClassId) -> bool {
		self.conflicts_by_class
			.get(&class)
			.is_some_and(|indices| indices.iter().any(|&index| self.conflicts[index].is_some()))
	}

	fn finish(self, plan: &mut NWaySelectionPlan) {
		plan.conflicts = self.conflicts.into_iter().flatten().collect();
		plan.decisions = self.decisions.into_iter().flatten().collect();
	}
}

pub(crate) fn apply_nway_policy(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	policy: &dyn MergePolicy,
	plan: &mut NWaySelectionPlan,
) {
	let mut records: PolicyRecords = PolicyRecords::take(plan, correspondence);
	let class_ids = correspondence
		.classes
		.classes()
		.map(|class| class.id)
		.collect::<Vec<_>>();
	// Every deleted child of one wide parent asks about the same siblings.
	let mut gap_indexes = DeletedGapIndexes::new();
	for class_id in class_ids {
		let class = correspondence.classes.class(class_id);
		let facts = &correspondence.class_facts[&class_id];
		let (base_view, contributors) = class_views(base, revisions, correspondence, class_id);
		let kind = if facts.base.is_none() {
			ConflictKind::InsertInsert
		} else {
			ConflictKind::Policy
		};
		let context = NWayClassContext {
			class: class_id,
			kind,
			base: base_view,
			contributors: &contributors,
		};

		let mut subtree_selected = false;
		if let Some(revision) = policy.select_nway_subtree(context)
			&& let Some(source) = source_for_revision(base_view, &contributors, revision)
		{
			apply_source_selection(
				class_id,
				source,
				true,
				MergePolicyKind::SubtreeSelection,
				plan,
				&mut records,
			);
			exclude_unselected_subtree_classes(source, base, revisions, correspondence, plan);
			subtree_selected = true;
		}

		let mut deletion_resolved = facts.deleted_by.is_empty();
		if !subtree_selected && !facts.deleted_by.is_empty() {
			let delete_context = NWayDeleteContext {
				class: context,
				deleted_by: &facts.deleted_by,
				parent_present_in_all_revisions: parent_present_in_all_revisions(
					base,
					correspondence,
					class_id,
				),
				deleted_parent_has_same_kind_gap_replacement:
					deleted_parent_has_same_kind_gap_replacement(
						base,
						revisions,
						correspondence,
						class_id,
						&contributors,
						&mut gap_indexes,
					),
			};
			deletion_resolved = policy
				.select_nway_deleted_subtree(delete_context)
				.and_then(|revision| {
					source_for_revision(base_view, &contributors, revision).map(|source| {
						apply_source_selection(
							class_id,
							source,
							true,
							MergePolicyKind::SubtreeSelection,
							plan,
							&mut records,
						);
						exclude_unselected_subtree_classes(
							source,
							base,
							revisions,
							correspondence,
							plan,
						);
					})
				})
				.is_some() || apply_delete_decision(
				policy.resolve_nway_delete(delete_context),
				delete_context,
				plan,
				&mut records,
			);
		}

		let contributors_diverge = contributors_diverge(base_view, &contributors);
		let policy_relevant_change = contributors_diverge
			|| base_view.is_some()
				&& contributors
					.iter()
					.any(|contributor| contributor.shallow_changed);
		if !subtree_selected
			&& deletion_resolved
			&& plan.classes[&class_id].selected.is_some()
			&& policy_relevant_change
		{
			let decision = policy.resolve_nway_divergent_node(context);
			if !apply_divergent_decision(decision, context, plan, &mut records)
				&& contributors_diverge
				&& !records.has_conflict(class_id)
			{
				records.push_conflict(
					class_conflict(
						ConflictKind::Policy,
						class_id,
						plan.classes[&class_id].parent,
						base,
						revisions,
						correspondence,
						format!(
							"{} revisions changed class {} differently",
							contributors
								.iter()
								.filter(|view| view.shallow_changed)
								.count(),
							class_id.get(),
						),
					),
					correspondence,
				);
			}
		}

		if plan.classes[&class_id].selected.is_some() && !plan.classes[&class_id].subtree_selected {
			plan.classes
				.get_mut(&class_id)
				.expect("every class has a selection")
				.child_revision = policy.select_nway_children(context);
		}

		debug_assert_eq!(class.id, class_id);
	}
	close_policy_ancestors(base, revisions, correspondence, policy, plan, &mut records);
	close_survivor_descendants(base, correspondence, plan, &mut records);
	records.finish(plan);
}

fn apply_delete_decision(
	decision: PolicyDecision,
	context: NWayDeleteContext<'_>,
	plan: &mut NWaySelectionPlan,
	records: &mut PolicyRecords,
) -> bool {
	let class = context.class.class;
	let modified = context
		.class
		.contributors
		.iter()
		.any(|view| view.subtree_changed || view.reparented || view.reordered);
	let policy_kind = if modified {
		MergePolicyKind::DeleteModify
	} else {
		MergePolicyKind::OneSidedRemoval
	};
	match decision {
		PolicyDecision::Unresolved | PolicyDecision::SynthesizeScalar(_) => false,
		PolicyDecision::Resolved => {
			let Some(source) = context
				.class
				.contributors
				.iter()
				.rev()
				.find(|view| view.subtree_changed || view.reparented || view.reordered)
				.or_else(|| context.class.contributors.last())
				.map(|view| view.source)
			else {
				return false;
			};
			apply_source_selection(class, source, false, policy_kind, plan, records);
			true
		}
		PolicyDecision::Select(revision) if context.deleted_by.contains(&revision) => {
			records.remove_for_class(class);
			let base = context
				.class
				.base
				.expect("a deletion context always has a base")
				.source;
			let selection = plan
				.classes
				.get_mut(&class)
				.expect("every class has a selection");
			selection.selected = None;
			selection.subtree_selected = false;
			selection.scalar_synthesis = None;
			selection.child_revision = None;
			records.push_decision(MergeDecisionEvidence {
				affected_class: class,
				policy: policy_kind,
				reason: MergeDecisionReason::ExplicitDomainRule,
				contributors: selection.sources.clone(),
				result: MergeDecisionResult::Delete {
					deleted_by: context.deleted_by.to_vec(),
					base,
				},
			});
			true
		}
		PolicyDecision::Select(revision) => {
			let Some(source) =
				source_for_revision(context.class.base, context.class.contributors, revision)
			else {
				return false;
			};
			apply_source_selection(class, source, false, policy_kind, plan, records);
			true
		}
	}
}

fn apply_divergent_decision(
	decision: PolicyDecision,
	context: NWayClassContext<'_>,
	plan: &mut NWaySelectionPlan,
	records: &mut PolicyRecords,
) -> bool {
	match decision {
		PolicyDecision::Unresolved => false,
		PolicyDecision::Resolved => {
			let Some(selected) = plan.classes[&context.class].selected else {
				return false;
			};
			apply_source_selection(
				context.class,
				selected,
				false,
				MergePolicyKind::DivergentNode,
				plan,
				records,
			);
			true
		}
		PolicyDecision::Select(revision) => {
			let Some(source) = source_for_revision(context.base, context.contributors, revision)
			else {
				return false;
			};
			apply_source_selection(
				context.class,
				source,
				false,
				MergePolicyKind::DivergentNode,
				plan,
				records,
			);
			true
		}
		PolicyDecision::SynthesizeScalar(output) => {
			let Some(synthesis) = scalar_synthesis(context, output) else {
				return false;
			};
			let Some(source) = context.contributors.last().map(|view| view.source) else {
				return false;
			};
			records.remove_for_class(context.class);
			let selection = plan
				.classes
				.get_mut(&context.class)
				.expect("every class has a selection");
			selection.selected = Some(source);
			selection.subtree_selected = false;
			selection.scalar_synthesis = Some(synthesis.clone());
			selection.child_revision = None;
			records.push_decision(MergeDecisionEvidence {
				affected_class: context.class,
				policy: MergePolicyKind::ScalarReducer,
				reason: MergeDecisionReason::ExplicitDomainRule,
				contributors: selection.sources.clone(),
				result: MergeDecisionResult::SynthesizeScalar {
					value: synthesis.output,
				},
			});
			true
		}
	}
}

fn apply_source_selection(
	class: ClassId,
	source: RevisionNode,
	subtree: bool,
	policy: MergePolicyKind,
	plan: &mut NWaySelectionPlan,
	records: &mut PolicyRecords,
) {
	records.remove_for_class(class);
	let selection = plan
		.classes
		.get_mut(&class)
		.expect("every class has a selection");
	selection.selected = Some(source);
	selection.subtree_selected = subtree;
	selection.scalar_synthesis = None;
	selection.child_revision = None;
	records.push_decision(MergeDecisionEvidence {
		affected_class: class,
		policy,
		reason: MergeDecisionReason::ExplicitDomainRule,
		contributors: selection.sources.clone(),
		result: MergeDecisionResult::SelectSource { source },
	});
}

fn scalar_synthesis(context: NWayClassContext<'_>, output: String) -> Option<NWayScalarSynthesis> {
	if context.contributors.len() < 2 {
		return None;
	}
	let first = context.contributors[0].node;
	if !first.children.is_empty() || first.value.is_none() {
		return None;
	}
	if context.contributors.iter().skip(1).any(|view| {
		!view.node.children.is_empty()
			|| view.node.value.is_none()
			|| view.node.kind != first.kind
			|| view.node.policy_path != first.policy_path
	}) {
		return None;
	}
	Some(NWayScalarSynthesis {
		reducer_path: first.policy_path.clone(),
		inputs: context
			.contributors
			.iter()
			.map(|view| (view.source.revision, view.node.value.clone().unwrap()))
			.collect(),
		output,
	})
}

fn contributors_diverge(base: Option<NWayNodeView<'_>>, contributors: &[NWayNodeView<'_>]) -> bool {
	let changed = contributors
		.iter()
		.filter(|view| base.is_none() || view.shallow_changed)
		.collect::<Vec<_>>();
	let Some(first) = changed.first() else {
		return false;
	};
	changed
		.iter()
		.skip(1)
		.any(|view| !shallow_eq(first.node, view.node))
}

fn class_views<'tree>(
	base: &'tree NormalizedTree,
	revisions: &[MergeRevision<'tree>],
	correspondence: &NWayCorrespondence,
	class: ClassId,
) -> (Option<NWayNodeView<'tree>>, Vec<NWayNodeView<'tree>>) {
	let revision_class = correspondence.classes.class(class);
	let facts = &correspondence.class_facts[&class];
	let base_view = revision_class.get(RevisionId::BASE).map(|node| {
		node_view(
			base,
			revisions,
			facts,
			RevisionNode::new(RevisionId::BASE, node),
		)
	});
	let contributors = revisions
		.iter()
		.filter_map(|revision| {
			revision_class
				.get(revision.id)
				.map(|node| node_view(base, revisions, facts, RevisionNode::new(revision.id, node)))
		})
		.collect();
	(base_view, contributors)
}

fn node_view<'tree>(
	base: &'tree NormalizedTree,
	revisions: &[MergeRevision<'tree>],
	facts: &crate::merge::kernel::NWayClassFacts,
	source: RevisionNode,
) -> NWayNodeView<'tree> {
	let tree = tree_for_revision(base, revisions, source.revision);
	let node = tree.node(source.node).unwrap();
	let base_node = facts
		.base
		.map(|base_source| base.node(base_source.node).unwrap());
	let parent = node.parent.map(|parent| tree.node(parent).unwrap());
	let base_parent = base_node.and_then(|base_node| base_node.parent);
	let parent_changed_from_base = match (base_parent, node.parent) {
		(Some(base_parent), Some(parent)) => {
			base.node(base_parent).unwrap().subtree_hash != tree.node(parent).unwrap().subtree_hash
		}
		(None, None) => false,
		_ => true,
	};
	NWayNodeView {
		source,
		node,
		parent,
		shallow_changed: base_node.is_none_or(|base_node| !shallow_eq(base_node, node)),
		subtree_changed: facts
			.subtree_changed
			.iter()
			.any(|candidate| *candidate == source),
		reparented: facts.moved.iter().any(|candidate| *candidate == source),
		reordered: facts.reordered.iter().any(|candidate| *candidate == source),
		parent_changed_from_base,
	}
}

fn source_for_revision(
	base: Option<NWayNodeView<'_>>,
	contributors: &[NWayNodeView<'_>],
	revision: RevisionId,
) -> Option<RevisionNode> {
	if revision == RevisionId::BASE {
		return base.map(|view| view.source);
	}
	contributors
		.iter()
		.find(|view| view.source.revision == revision)
		.map(|view| view.source)
}

fn parent_present_in_all_revisions(
	base: &NormalizedTree,
	correspondence: &NWayCorrespondence,
	class: ClassId,
) -> bool {
	let revision_class = correspondence.classes.class(class);
	let Some(base_node) = revision_class.get(RevisionId::BASE) else {
		return false;
	};
	let Some(base_parent) = base.node(base_node).unwrap().parent else {
		return true;
	};
	let parent_class = correspondence
		.classes
		.class_of(RevisionNode::new(RevisionId::BASE, base_parent));
	let parent = correspondence.classes.class(parent_class);
	correspondence
		.revision_order
		.iter()
		.all(|revision| parent.get(*revision).is_some())
}

/// The siblings of one base parent as one deleting revision orders them,
/// built once for every deleted child of that parent.
struct DeletedGapIndex {
	/// Each base child's position under the base parent.
	base_positions: HashMap<NodeId, usize>,
	/// The gaps holding the deleting revision's children with no base
	/// counterpart, as (kind, nearest base position before, nearest base
	/// position after), each with whether exactly one base child of that kind
	/// in the gap is absent from the deleting revision. Inserted siblings in
	/// one gap answer alike, so each gap is kept once.
	inserted_gaps: Vec<(String, Option<usize>, Option<usize>, bool)>,
}

impl DeletedGapIndex {
	fn build(
		base: &NormalizedTree,
		correspondence: &NWayCorrespondence,
		base_parent_node: &NormalizedNode,
		deleted_tree: &NormalizedTree,
		deleted_parent_node: &NormalizedNode,
		deleted_revision: RevisionId,
	) -> Self {
		let base_positions = base_parent_node
			.children
			.iter()
			.enumerate()
			.map(|(index, child)| (*child, index))
			.collect::<HashMap<_, _>>();
		let base_child_absent: Vec<bool> = base_parent_node
			.children
			.iter()
			.map(|child| {
				let class = correspondence
					.classes
					.class_of(RevisionNode::new(RevisionId::BASE, *child));
				correspondence
					.classes
					.class(class)
					.get(deleted_revision)
					.is_none()
			})
			.collect();
		let base_sibling = |child: &NodeId| {
			let class = correspondence
				.classes
				.class_of(RevisionNode::new(deleted_revision, *child));
			correspondence.classes.class(class).get(RevisionId::BASE)
		};
		let mapped = deleted_parent_node
			.children
			.iter()
			.map(|child| {
				let base_child = base_sibling(child);
				(
					base_child.is_none(),
					base_child.and_then(|base_child| base_positions.get(&base_child).copied()),
				)
			})
			.collect::<Vec<_>>();
		let mut previous = Vec::with_capacity(mapped.len());
		let mut last = None;
		for (_, position) in &mapped {
			previous.push(last);
			last = position.or(last);
		}
		let mut next = vec![None; mapped.len()];
		let mut following = None;
		for (index, (_, position)) in mapped.iter().enumerate().rev() {
			next[index] = following;
			following = position.or(following);
		}
		let gaps = deleted_parent_node
			.children
			.iter()
			.enumerate()
			.filter(|(index, _)| mapped[*index].0)
			.map(|(index, child)| {
				(
					deleted_tree.node(*child).unwrap().kind.as_str(),
					previous[index],
					next[index],
				)
			})
			.collect::<BTreeSet<_>>();
		let inserted_gaps = gaps
			.into_iter()
			.map(|(kind, previous, next)| {
				let start = previous.map_or(0, |previous| previous + 1);
				let end = next.unwrap_or(base_parent_node.children.len());
				let absent_of_kind = (start..end)
					.filter(|position| {
						base_child_absent[*position]
							&& base
								.node(base_parent_node.children[*position])
								.unwrap()
								.kind == kind
					})
					.count();
				(kind.to_owned(), previous, next, absent_of_kind == 1)
			})
			.collect();
		Self {
			base_positions,
			inserted_gaps,
		}
	}
}

type DeletedGapIndexes = HashMap<(NodeId, RevisionId), Option<DeletedGapIndex>>;

fn deleted_parent_has_same_kind_gap_replacement(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	class: ClassId,
	present: &[NWayNodeView<'_>],
	gap_indexes: &mut DeletedGapIndexes,
) -> bool {
	let revision_class = correspondence.classes.class(class);
	let Some(base_node) = revision_class.get(RevisionId::BASE) else {
		return false;
	};
	let Some(base_parent) = base.node(base_node).unwrap().parent else {
		return false;
	};
	let parent_class = correspondence
		.classes
		.class_of(RevisionNode::new(RevisionId::BASE, base_parent));
	correspondence.class_facts[&class]
		.deleted_by
		.iter()
		.any(|deleted_revision| {
			let index = gap_indexes
				.entry((base_parent, *deleted_revision))
				.or_insert_with(|| {
					deleted_gap_index(
						base,
						revisions,
						correspondence,
						base_parent,
						parent_class,
						*deleted_revision,
					)
				});
			let Some(index) = index else {
				return false;
			};
			present
				.iter()
				.any(|present| deleted_gap_matches_present_kind(base_node, index, present))
		})
}

fn deleted_gap_index(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	base_parent: NodeId,
	parent_class: ClassId,
	deleted_revision: RevisionId,
) -> Option<DeletedGapIndex> {
	let deleted_parent = correspondence
		.classes
		.class(parent_class)
		.get(deleted_revision)?;
	let deleted_tree = tree_for_revision(base, revisions, deleted_revision);
	let base_parent_node = base.node(base_parent).unwrap();
	let deleted_parent_node = deleted_tree.node(deleted_parent).unwrap();
	if base_parent_node.child_order != ChildOrder::Ordered
		|| deleted_parent_node.child_order != ChildOrder::Ordered
	{
		return None;
	}
	Some(DeletedGapIndex::build(
		base,
		correspondence,
		base_parent_node,
		deleted_tree,
		deleted_parent_node,
		deleted_revision,
	))
}

/// Whether the deleting revision put exactly one same-kind node where it
/// deleted `base_node`: an inserted sibling whose nearest base neighbours
/// enclose the deleted node, and the only same-kind base node the revision
/// lacks between them.
fn deleted_gap_matches_present_kind(
	base_node: NodeId,
	index: &DeletedGapIndex,
	present: &NWayNodeView<'_>,
) -> bool {
	let Some(&base_index) = index.base_positions.get(&base_node) else {
		return false;
	};
	index
		.inserted_gaps
		.iter()
		.any(|(kind, previous_base, next_base, one_absent)| {
			*kind == present.node.kind
				&& *one_absent
				&& previous_base.is_none_or(|index| index < base_index)
				&& next_base.is_none_or(|index| base_index < index)
		})
}

fn exclude_unselected_subtree_classes(
	selected: RevisionNode,
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	plan: &mut NWaySelectionPlan,
) {
	plan.policy_excluded
		.extend(correspondence.subtree_variant_descendants(selected, base, revisions));
}

fn close_policy_ancestors(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	policy: &dyn MergePolicy,
	plan: &mut NWaySelectionPlan,
	records: &mut PolicyRecords,
) {
	let roots = plan
		.classes
		.values()
		.filter_map(|selection| selection.selected)
		.collect::<Vec<_>>();
	for root in roots {
		let tree = tree_for_revision(base, revisions, root.revision);
		let mut parent = tree.node(root.node).unwrap().parent;
		while let Some(parent_node) = parent {
			let source = RevisionNode::new(root.revision, parent_node);
			let class = correspondence.classes.class_of(source);
			if plan.classes[&class].selected.is_none() {
				let node = tree.node(parent_node).unwrap();
				if !policy.permits_ancestor_closure(node) {
					break;
				}
				records.remove_for_class(class);
				let selection = plan
					.classes
					.get_mut(&class)
					.expect("every ancestor has a selection record");
				selection.selected = Some(source);
				selection.subtree_selected = false;
				selection.scalar_synthesis = None;
				selection.child_revision = None;
				records.push_decision(MergeDecisionEvidence {
					affected_class: class,
					policy: MergePolicyKind::AncestorClosure,
					reason: MergeDecisionReason::StructuralConstraint,
					contributors: selection.sources.clone(),
					result: MergeDecisionResult::SelectSource { source },
				});
			}
			parent = tree.node(parent_node).unwrap().parent;
		}
	}
}

/// Completes a subtree that survived a removal because another revision
/// edited it. The removing revisions deleted each descendant only as part of
/// that subtree; keeping the edited ancestor without them would emit a
/// fragment no revision wrote, such as an option without its name.
fn close_survivor_descendants(
	base: &NormalizedTree,
	correspondence: &NWayCorrespondence,
	plan: &mut NWaySelectionPlan,
	records: &mut PolicyRecords,
) {
	let mut pending = vec![base.root()];
	while let Some(node) = pending.pop() {
		let children = base.node(node).unwrap().children.clone();
		pending.extend(children.iter().rev().copied());
		let class = correspondence
			.classes
			.class_of(RevisionNode::new(RevisionId::BASE, node));
		if plan.classes[&class].selected.is_some() {
			continue;
		}
		let Some(parent) = base.node(node).unwrap().parent else {
			continue;
		};
		let parent_class = correspondence
			.classes
			.class_of(RevisionNode::new(RevisionId::BASE, parent));
		let Some(parent_source) = plan.classes[&parent_class].selected else {
			continue;
		};
		let parent_deleted_by = &correspondence.class_facts[&parent_class].deleted_by;
		let deleted_by = &correspondence.class_facts[&class].deleted_by;
		if parent_deleted_by.is_empty()
			|| deleted_by.is_empty()
			|| !deleted_by
				.iter()
				.all(|revision| parent_deleted_by.contains(revision))
		{
			continue;
		}
		// Inside control flow a restored branch would run beside the chain
		// that replaced it; that stays a conflict for review.
		if inside_control_flow(base, node) {
			continue;
		}
		let Some(member) = correspondence
			.classes
			.class(class)
			.get(parent_source.revision)
		else {
			continue;
		};
		let source = RevisionNode::new(parent_source.revision, member);
		records.remove_for_class(class);
		let selection = plan
			.classes
			.get_mut(&class)
			.expect("every base node has a selection record");
		selection.selected = Some(source);
		selection.subtree_selected = false;
		selection.scalar_synthesis = None;
		selection.child_revision = None;
		records.push_decision(MergeDecisionEvidence {
			affected_class: class,
			policy: MergePolicyKind::AncestorClosure,
			reason: MergeDecisionReason::StructuralConstraint,
			contributors: selection.sources.clone(),
			result: MergeDecisionResult::SelectSource { source },
		});
	}
}

fn inside_control_flow(base: &NormalizedTree, node: NodeId) -> bool {
	let mut current = base.node(node).unwrap().parent;
	while let Some(ancestor) = current {
		let ancestor = base.node(ancestor).unwrap();
		if ancestor.kind.starts_with("clausewitz.control_flow.") {
			return true;
		}
		current = ancestor.parent;
	}
	false
}

fn class_conflict(
	kind: ConflictKind,
	class: ClassId,
	parent: Option<ClassId>,
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	detail: String,
) -> StructuralConflictDraft {
	let revision_class = correspondence.classes.class(class);
	let mut conflict = StructuralConflictDraft::new(
		kind,
		parent,
		revision_class
			.get(RevisionId::BASE)
			.map(|node| RevisionNode::new(RevisionId::BASE, node)),
		SourceSet::new(
			revision_class
				.members
				.iter()
				.map(|(revision, node)| RevisionNode::new(*revision, *node)),
		),
		Vec::new(),
		detail,
	);
	conflict.semantic_path =
		common_policy_path(revision_class.members.iter().map(|(revision, node)| {
			tree_for_revision(base, revisions, *revision)
				.node(*node)
				.unwrap()
		}));
	conflict
}

fn common_policy_path<'a>(nodes: impl IntoIterator<Item = &'a NormalizedNode>) -> Vec<String> {
	let mut paths = nodes.into_iter().map(|node| node.policy_path.as_slice());
	let Some(first) = paths.next() else {
		return Vec::new();
	};
	let mut common = first.to_vec();
	for path in paths {
		common.truncate(
			common
				.iter()
				.zip(path)
				.take_while(|(left, right)| left == right)
				.count(),
		);
		if common.is_empty() {
			break;
		}
	}
	common
}

fn shallow_eq(left: &NormalizedNode, right: &NormalizedNode) -> bool {
	left.kind == right.kind
		&& left.value == right.value
		&& left.anchor == right.anchor
		&& left.signature == right.signature
		&& left.child_order == right.child_order
		&& left.child_cardinality == right.child_cardinality
}

fn tree_for_revision<'tree>(
	base: &'tree NormalizedTree,
	revisions: &[MergeRevision<'tree>],
	revision: RevisionId,
) -> &'tree NormalizedTree {
	if revision == RevisionId::BASE {
		return base;
	}
	revisions
		.iter()
		.find(|candidate| candidate.id == revision)
		.map(|candidate| candidate.tree)
		.unwrap_or_else(|| panic!("unknown revision {}", revision.get()))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::merge::kernel::TreeNode;

	/// The quadratic-per-sibling search the gap index replaced, kept as the
	/// reference the index must agree with.
	fn reference_gap_replacement(
		base: &NormalizedTree,
		revisions: &[MergeRevision<'_>],
		correspondence: &NWayCorrespondence,
		class: ClassId,
		present: &[NWayNodeView<'_>],
	) -> bool {
		let revision_class = correspondence.classes.class(class);
		let Some(base_node) = revision_class.get(RevisionId::BASE) else {
			return false;
		};
		let Some(base_parent) = base.node(base_node).unwrap().parent else {
			return false;
		};
		let parent_class = correspondence
			.classes
			.class_of(RevisionNode::new(RevisionId::BASE, base_parent));
		correspondence.class_facts[&class]
			.deleted_by
			.iter()
			.any(|deleted_revision| {
				present.iter().any(|present| {
					reference_gap_matches(
						base,
						revisions,
						correspondence,
						base_node,
						base_parent,
						parent_class,
						*deleted_revision,
						present,
					)
				})
			})
	}

	#[allow(clippy::too_many_arguments)]
	fn reference_gap_matches(
		base: &NormalizedTree,
		revisions: &[MergeRevision<'_>],
		correspondence: &NWayCorrespondence,
		base_node: NodeId,
		base_parent: NodeId,
		parent_class: ClassId,
		deleted_revision: RevisionId,
		present: &NWayNodeView<'_>,
	) -> bool {
		let Some(deleted_parent) = correspondence
			.classes
			.class(parent_class)
			.get(deleted_revision)
		else {
			return false;
		};
		let deleted_tree = tree_for_revision(base, revisions, deleted_revision);
		let base_parent_node = base.node(base_parent).unwrap();
		let deleted_parent_node = deleted_tree.node(deleted_parent).unwrap();
		if base_parent_node.child_order != ChildOrder::Ordered
			|| deleted_parent_node.child_order != ChildOrder::Ordered
		{
			return false;
		}
		let Some(base_index) = base_parent_node
			.children
			.iter()
			.position(|child| *child == base_node)
		else {
			return false;
		};
		let mapped_base_index = |child: &NodeId| {
			let sibling_class = correspondence
				.classes
				.class_of(RevisionNode::new(deleted_revision, *child));
			let base_sibling = correspondence
				.classes
				.class(sibling_class)
				.get(RevisionId::BASE)?;
			base_parent_node
				.children
				.iter()
				.position(|candidate| *candidate == base_sibling)
		};
		deleted_parent_node
			.children
			.iter()
			.enumerate()
			.filter(|(_, sibling)| deleted_tree.node(**sibling).unwrap().kind == present.node.kind)
			.filter(|(_, sibling)| {
				let sibling_class = correspondence
					.classes
					.class_of(RevisionNode::new(deleted_revision, **sibling));
				correspondence
					.classes
					.class(sibling_class)
					.get(RevisionId::BASE)
					.is_none()
			})
			.any(|(replacement_index, _)| {
				let previous_base = deleted_parent_node.children[..replacement_index]
					.iter()
					.rev()
					.find_map(&mapped_base_index);
				let next_base = deleted_parent_node.children[replacement_index + 1..]
					.iter()
					.find_map(&mapped_base_index);
				let target_is_in_gap = previous_base.is_none_or(|index| index < base_index)
					&& next_base.is_none_or(|index| base_index < index);
				if !target_is_in_gap {
					return false;
				}
				base_parent_node
					.children
					.iter()
					.enumerate()
					.filter(|(index, sibling)| {
						previous_base.is_none_or(|previous| previous < *index)
							&& next_base.is_none_or(|next| *index < next)
							&& base.node(**sibling).unwrap().kind == present.node.kind
					})
					.filter(|(_, sibling)| {
						let sibling_class = correspondence
							.classes
							.class_of(RevisionNode::new(RevisionId::BASE, **sibling));
						correspondence
							.classes
							.class(sibling_class)
							.get(deleted_revision)
							.is_none()
					})
					.count() == 1
			})
	}

	struct Lcg(u64);

	impl Lcg {
		fn next(&mut self, bound: usize) -> usize {
			self.0 = self
				.0
				.wrapping_mul(6_364_136_223_846_793_005)
				.wrapping_add(1_442_695_040_888_963_407);
			((self.0 >> 33) as usize) % bound
		}
	}

	fn child(kind: usize, id: usize) -> TreeNode {
		TreeNode::leaf(["a", "b"][kind], format!("v{id}"))
			.with_parent_scoped_anchor("child", format!("c{id}"))
	}

	fn document(children: Vec<TreeNode>) -> NormalizedTree {
		NormalizedTree::from_root(TreeNode::branch(
			"root",
			vec![TreeNode::branch("parent", children)],
		))
		.unwrap()
	}

	/// A revision that deletes some base children and inserts new ones
	/// between the survivors, without reordering them.
	fn edited(rng: &mut Lcg, base: &[(usize, usize)], next_id: &mut usize) -> Vec<TreeNode> {
		let mut children = Vec::new();
		for &(kind, id) in base {
			for _ in 0..rng.next(3).saturating_sub(1) {
				children.push(child(rng.next(2), *next_id));
				*next_id += 1;
			}
			if rng.next(3) != 0 {
				children.push(child(kind, id));
			}
		}
		children
	}

	#[test]
	fn gap_index_agrees_with_the_sibling_search_it_replaced() {
		let mut rng = Lcg(7);
		let mut checked = 0;
		let mut replaced = 0;
		for _ in 0..300 {
			let width = 1 + rng.next(12);
			let base_children = (0..width).map(|id| (rng.next(2), id)).collect::<Vec<_>>();
			let mut next_id = width;
			let base = document(
				base_children
					.iter()
					.map(|&(kind, id)| child(kind, id))
					.collect(),
			);
			let left = document(edited(&mut rng, &base_children, &mut next_id));
			let right = document(edited(&mut rng, &base_children, &mut next_id));
			let revisions = [
				MergeRevision::new(RevisionId::LEFT, &left),
				MergeRevision::new(RevisionId::RIGHT, &right),
			];
			let correspondence = NWayCorrespondence::build(&base, &revisions).unwrap();
			let mut indexes = DeletedGapIndexes::new();
			for class in correspondence.classes.classes() {
				if correspondence.class_facts[&class.id].deleted_by.is_empty() {
					continue;
				}
				let (_, present) = class_views(&base, &revisions, &correspondence, class.id);
				let indexed = deleted_parent_has_same_kind_gap_replacement(
					&base,
					&revisions,
					&correspondence,
					class.id,
					&present,
					&mut indexes,
				);
				let searched = reference_gap_replacement(
					&base,
					&revisions,
					&correspondence,
					class.id,
					&present,
				);
				assert_eq!(indexed, searched);
				checked += 1;
				replaced += usize::from(indexed);
			}
		}
		assert!(
			checked > 100,
			"only {checked} deleted classes were compared"
		);
		assert!(
			replaced > 10 && replaced < checked,
			"{replaced} of {checked} deleted classes had a gap replacement"
		);
	}
}
