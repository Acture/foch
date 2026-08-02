use crate::{
	ChildOrder, ClassId, ConflictKind, MergeDecisionEvidence, MergeDecisionReason,
	MergeDecisionResult, MergePolicy, MergePolicyKind, MergeRevision, NWayClassContext,
	NWayCorrespondence, NWayDeleteContext, NWayNodeView, NWayScalarSynthesis, NWaySelectionPlan,
	NodeId, NormalizedNode, NormalizedTree, PolicyDecision, RevisionId, RevisionNode, SourceSet,
	StructuralConflictDraft,
};

pub(crate) fn apply_nway_policy(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	policy: &dyn MergePolicy,
	plan: &mut NWaySelectionPlan,
) {
	let class_ids = correspondence
		.classes
		.classes()
		.map(|class| class.id)
		.collect::<Vec<_>>();
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
				correspondence,
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
							correspondence,
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
				correspondence,
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
			if !apply_divergent_decision(decision, context, plan, correspondence)
				&& contributors_diverge
				&& !class_has_conflict(plan, correspondence, class_id)
			{
				plan.conflicts.push(class_conflict(
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
				));
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
	close_policy_ancestors(base, revisions, correspondence, policy, plan);
}

fn apply_delete_decision(
	decision: PolicyDecision,
	context: NWayDeleteContext<'_>,
	plan: &mut NWaySelectionPlan,
	correspondence: &NWayCorrespondence,
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
			apply_source_selection(class, source, false, policy_kind, plan, correspondence);
			true
		}
		PolicyDecision::Select(revision) if context.deleted_by.contains(&revision) => {
			remove_class_resolution_state(class, plan, correspondence);
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
			plan.decisions.push(MergeDecisionEvidence {
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
			apply_source_selection(class, source, false, policy_kind, plan, correspondence);
			true
		}
	}
}

fn apply_divergent_decision(
	decision: PolicyDecision,
	context: NWayClassContext<'_>,
	plan: &mut NWaySelectionPlan,
	correspondence: &NWayCorrespondence,
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
				correspondence,
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
				correspondence,
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
			remove_class_resolution_state(context.class, plan, correspondence);
			let selection = plan
				.classes
				.get_mut(&context.class)
				.expect("every class has a selection");
			selection.selected = Some(source);
			selection.subtree_selected = false;
			selection.scalar_synthesis = Some(synthesis.clone());
			selection.child_revision = None;
			plan.decisions.push(MergeDecisionEvidence {
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
	correspondence: &NWayCorrespondence,
) {
	remove_class_resolution_state(class, plan, correspondence);
	let selection = plan
		.classes
		.get_mut(&class)
		.expect("every class has a selection");
	selection.selected = Some(source);
	selection.subtree_selected = subtree;
	selection.scalar_synthesis = None;
	selection.child_revision = None;
	plan.decisions.push(MergeDecisionEvidence {
		affected_class: class,
		policy,
		reason: MergeDecisionReason::ExplicitDomainRule,
		contributors: selection.sources.clone(),
		result: MergeDecisionResult::SelectSource { source },
	});
}

fn remove_class_resolution_state(
	class: ClassId,
	plan: &mut NWaySelectionPlan,
	correspondence: &NWayCorrespondence,
) {
	plan.decisions
		.retain(|decision| decision.affected_class != class);
	plan.conflicts.retain(|conflict| {
		!matches!(
			conflict.kind,
			ConflictKind::InsertInsert | ConflictKind::DeleteModify | ConflictKind::Policy
		) || !conflict_affects_class(conflict, correspondence, class)
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
	facts: &crate::NWayClassFacts,
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

fn deleted_parent_has_same_kind_gap_replacement(
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
				deleted_gap_matches_present_kind(
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
fn deleted_gap_matches_present_kind(
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
				remove_class_resolution_state(class, plan, correspondence);
				let selection = plan
					.classes
					.get_mut(&class)
					.expect("every ancestor has a selection record");
				selection.selected = Some(source);
				selection.subtree_selected = false;
				selection.scalar_synthesis = None;
				selection.child_revision = None;
				plan.decisions.push(MergeDecisionEvidence {
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

fn class_has_conflict(
	plan: &NWaySelectionPlan,
	correspondence: &NWayCorrespondence,
	class: ClassId,
) -> bool {
	plan.conflicts
		.iter()
		.any(|conflict| conflict_affects_class(conflict, correspondence, class))
}

fn conflict_affects_class(
	conflict: &StructuralConflictDraft,
	correspondence: &NWayCorrespondence,
	class: ClassId,
) -> bool {
	conflict
		.revisions
		.iter()
		.any(|source| correspondence.classes.class_of(*source) == class)
		|| conflict
			.base
			.is_some_and(|source| correspondence.classes.class_of(source) == class)
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
