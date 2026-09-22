use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::merge::kernel::pcs::merge_orders;
use crate::merge::kernel::{
	ChildCardinality, ChildOrder, ClassId, ClassMapping, ConflictKind, DeltaOperation, Matching,
	MergeDecisionEvidence, MergeDecisionReason, MergeDecisionResult, MergeInputId, MergePolicyKind,
	NormalizedNode, NormalizedTree, RevisionDelta, RevisionId, RevisionNode, RevisionSourceRef,
	SourceSet, StructuralConflictDraft, TreeMatcher,
};

#[derive(Clone, Copy, Debug)]
pub struct MergeRevision<'tree> {
	pub id: RevisionId,
	pub tree: &'tree NormalizedTree,
}

impl<'tree> MergeRevision<'tree> {
	pub const fn new(id: RevisionId, tree: &'tree NormalizedTree) -> Self {
		Self { id, tree }
	}
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum NWayInputError {
	#[error("an N-way merge requires at least one non-base revision")]
	NoRevisions,
	#[error("revision 0 is reserved for the merge base")]
	BaseRevision,
	#[error("revision {0:?} appears more than once")]
	DuplicateRevision(RevisionId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NWayCorrespondence {
	pub(crate) revision_order: Vec<RevisionId>,
	pub(crate) inputs: BTreeMap<RevisionId, MergeInputId>,
	pub(crate) matchings: BTreeMap<(RevisionId, RevisionId), Matching>,
	pub(crate) revision_deltas: BTreeMap<RevisionId, RevisionDelta>,
	pub(crate) classes: ClassMapping,
	pub(crate) class_facts: BTreeMap<ClassId, NWayClassFacts>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NWayClassFacts {
	pub class: ClassId,
	pub base: Option<RevisionNode>,
	pub present: SourceSet,
	pub subtree_changed: SourceSet,
	pub moved: SourceSet,
	pub reordered: SourceSet,
	pub deleted_by: Vec<RevisionId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NWayClassSelection {
	pub selected: Option<RevisionNode>,
	pub subtree_selected: bool,
	pub scalar_synthesis: Option<NWayScalarSynthesis>,
	pub child_revision: Option<RevisionId>,
	pub sources: SourceSet,
	pub parent: Option<ClassId>,
	pub children: Vec<ClassId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NWayScalarSynthesis {
	pub reducer_path: Vec<String>,
	pub inputs: Vec<(RevisionId, String)>,
	pub output: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NWaySelectionPlan {
	pub classes: BTreeMap<ClassId, NWayClassSelection>,
	pub policy_excluded: BTreeSet<ClassId>,
	pub decisions: Vec<MergeDecisionEvidence>,
	pub conflicts: Vec<StructuralConflictDraft>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NWayExactSelection {
	Subtree {
		source: RevisionNode,
		contributors: SourceSet,
	},
	Delete {
		base: RevisionNode,
		deleted_by: RevisionId,
		contributors: SourceSet,
	},
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct NWaySelectionOverrides {
	pub selections: BTreeMap<ClassId, NWayExactSelection>,
	pub restored_ancestors: BTreeMap<ClassId, RevisionNode>,
	pub parents: BTreeMap<ClassId, Option<ClassId>>,
	pub child_revisions: BTreeMap<ClassId, RevisionId>,
	pub excluded: BTreeSet<ClassId>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NWayCorrespondenceTimings {
	pub matcher_ns: u64,
	pub mapping_ns: u64,
	pub delta_ns: u64,
}

impl NWayCorrespondence {
	#[cfg(test)]
	pub fn build(
		base: &NormalizedTree,
		revisions: &[MergeRevision<'_>],
	) -> Result<Self, NWayInputError> {
		Self::build_profiled(base, revisions).map(|(correspondence, _)| correspondence)
	}

	pub(crate) fn build_profiled(
		base: &NormalizedTree,
		revisions: &[MergeRevision<'_>],
	) -> Result<(Self, NWayCorrespondenceTimings), NWayInputError> {
		if revisions.is_empty() {
			return Err(NWayInputError::NoRevisions);
		}
		let mut seen = BTreeSet::from([RevisionId::BASE]);
		for revision in revisions {
			if revision.id == RevisionId::BASE {
				return Err(NWayInputError::BaseRevision);
			}
			if !seen.insert(revision.id) {
				return Err(NWayInputError::DuplicateRevision(revision.id));
			}
		}

		let matcher_started = Instant::now();
		let matcher = TreeMatcher::default();
		let mut matchings = BTreeMap::new();
		for revision in revisions {
			matchings.insert(
				(RevisionId::BASE, revision.id),
				matcher.match_trees(base, revision.tree),
			);
		}
		for (left_index, left) in revisions.iter().enumerate() {
			for right in &revisions[left_index + 1..] {
				let left_base = &matchings[&(RevisionId::BASE, left.id)];
				let right_base = &matchings[&(RevisionId::BASE, right.id)];
				let seed = Matching::compose_through_base(left_base, right_base);
				matchings.insert(
					(left.id, right.id),
					matcher.match_trees_with_seed(left.tree, right.tree, Some(&seed)),
				);
			}
		}
		let matcher_ns = nanos(matcher_started.elapsed());

		let mapping_started = Instant::now();
		let revision_trees = std::iter::once((RevisionId::BASE, base))
			.chain(
				revisions
					.iter()
					.map(|revision| (revision.id, revision.tree)),
			)
			.collect::<Vec<_>>();
		let matching_refs = matchings
			.iter()
			.map(|(&(left, right), matching)| (left, right, matching))
			.collect::<Vec<_>>();
		let classes =
			ClassMapping::from_revision_matchings(revision_trees.iter().copied(), matching_refs);
		let mapping_ns = nanos(mapping_started.elapsed());

		let delta_started = Instant::now();
		let mut inputs = BTreeMap::from([(
			RevisionId::BASE,
			MergeInputId::from_tree(RevisionId::BASE, base),
		)]);
		let mut revision_deltas = BTreeMap::new();
		for revision in revisions {
			inputs.insert(
				revision.id,
				MergeInputId::from_tree(revision.id, revision.tree),
			);
			revision_deltas.insert(
				revision.id,
				RevisionDelta::between(
					base,
					revision.id,
					revision.tree,
					&matchings[&(RevisionId::BASE, revision.id)],
				),
			);
		}
		let delta_ns = nanos(delta_started.elapsed());
		let class_facts = build_class_facts(base, revisions, &classes, &revision_deltas);

		Ok((
			Self {
				revision_order: revisions.iter().map(|revision| revision.id).collect(),
				inputs,
				matchings,
				revision_deltas,
				classes,
				class_facts,
			},
			NWayCorrespondenceTimings {
				matcher_ns,
				mapping_ns,
				delta_ns,
			},
		))
	}

	#[cfg(test)]
	pub fn revision_order(&self) -> &[RevisionId] {
		&self.revision_order
	}

	#[cfg(test)]
	pub fn inputs(&self) -> &BTreeMap<RevisionId, MergeInputId> {
		&self.inputs
	}

	#[cfg(test)]
	pub fn matching(&self, left: RevisionId, right: RevisionId) -> Option<&Matching> {
		self.matchings.get(&(left, right))
	}

	#[cfg(test)]
	pub fn revision_deltas(&self) -> &BTreeMap<RevisionId, RevisionDelta> {
		&self.revision_deltas
	}

	#[cfg(test)]
	pub fn classes(&self) -> &ClassMapping {
		&self.classes
	}

	#[cfg(test)]
	pub fn class_facts(&self) -> &BTreeMap<ClassId, NWayClassFacts> {
		&self.class_facts
	}

	pub(crate) fn subtree_variant_descendants(
		&self,
		root: RevisionNode,
		base: &NormalizedTree,
		revisions: &[MergeRevision<'_>],
	) -> BTreeSet<ClassId> {
		let root_class = self.classes.class_of(root);
		let mut descendants = BTreeSet::new();
		for (revision, root_node) in &self.classes.class(root_class).members {
			let tree = tree_for_revision(base, revisions, *revision);
			let mut pending = tree.node(*root_node).unwrap().children.clone();
			while let Some(node) = pending.pop() {
				descendants.insert(self.classes.class_of(RevisionNode::new(*revision, node)));
				pending.extend(tree.node(node).unwrap().children.iter().copied());
			}
		}
		descendants
	}

	#[cfg(test)]
	pub fn conservative_selection(
		&self,
		base: &NormalizedTree,
		revisions: &[MergeRevision<'_>],
	) -> NWaySelectionPlan {
		let mut plan = self.conservative_class_selection(base, revisions);
		assign_nway_parents(
			base,
			revisions,
			self,
			&NWaySelectionOverrides::default(),
			&mut plan,
		);
		assign_nway_children(base, revisions, self, &mut plan);
		plan
	}

	pub(crate) fn selection_with_policy_and_overrides_profiled(
		&self,
		base: &NormalizedTree,
		revisions: &[MergeRevision<'_>],
		policy: &dyn crate::merge::kernel::MergePolicy,
		overrides: &NWaySelectionOverrides,
	) -> (NWaySelectionPlan, u64) {
		let mut plan = self.conservative_class_selection(base, revisions);
		let policy_started = Instant::now();
		crate::merge::kernel::nway_policy::apply_nway_policy(
			base, revisions, self, policy, &mut plan,
		);
		apply_selection_overrides(&mut plan, overrides, self);
		let policy_ns = nanos(policy_started.elapsed());
		assign_nway_parents(base, revisions, self, overrides, &mut plan);
		assign_nway_children(base, revisions, self, &mut plan);
		(plan, policy_ns)
	}

	fn conservative_class_selection(
		&self,
		base: &NormalizedTree,
		revisions: &[MergeRevision<'_>],
	) -> NWaySelectionPlan {
		let mut plan = NWaySelectionPlan::default();
		for class in self.classes.classes() {
			let facts = &self.class_facts[&class.id];
			let sources = SourceSet::new(
				class
					.members
					.iter()
					.map(|(revision, node)| RevisionNode::new(*revision, *node)),
			);
			let present = revisions
				.iter()
				.filter_map(|revision| {
					class
						.get(revision.id)
						.map(|node| (revision, RevisionNode::new(revision.id, node)))
				})
				.collect::<Vec<_>>();
			let selected = match facts.base {
				None => select_inserted_class(
					class.id,
					&present,
					sources.clone(),
					&mut plan.decisions,
					&mut plan.conflicts,
				),
				Some(base_source) => select_base_class(
					class.id,
					base,
					base_source,
					&present,
					facts,
					sources.clone(),
					&mut plan.decisions,
					&mut plan.conflicts,
				),
			};
			plan.classes.insert(
				class.id,
				NWayClassSelection {
					selected,
					subtree_selected: false,
					scalar_synthesis: None,
					child_revision: None,
					sources,
					parent: None,
					children: Vec::new(),
				},
			);
		}
		plan
	}
}

fn apply_selection_overrides(
	plan: &mut NWaySelectionPlan,
	overrides: &NWaySelectionOverrides,
	correspondence: &NWayCorrespondence,
) {
	for class in &overrides.excluded {
		plan.policy_excluded.insert(*class);
		if !overrides.selections.contains_key(class)
			&& let Some(selection) = plan.classes.get_mut(class)
		{
			selection.selected = None;
			selection.subtree_selected = false;
			selection.scalar_synthesis = None;
			selection.child_revision = None;
		}
	}
	for (class, source) in &overrides.restored_ancestors {
		let selection = plan
			.classes
			.get_mut(class)
			.expect("an exact resolution ancestor belongs to the correspondence");
		selection.selected = Some(*source);
		selection.subtree_selected = false;
		selection.scalar_synthesis = None;
		selection.child_revision = None;
		selection.sources.insert(*source);
		plan.policy_excluded.remove(class);
	}
	for (class, exact) in &overrides.selections {
		clear_overridden_class_state(plan, correspondence, *class);
		let selection = plan
			.classes
			.get_mut(class)
			.expect("an exact resolution target belongs to the correspondence");
		selection.scalar_synthesis = None;
		selection.child_revision = None;
		match exact {
			NWayExactSelection::Subtree {
				source,
				contributors,
			} => {
				selection.selected = Some(*source);
				selection.subtree_selected = true;
				for contributor in contributors.iter().copied() {
					selection.sources.insert(contributor);
				}
				plan.policy_excluded.remove(class);
				plan.decisions.push(MergeDecisionEvidence {
					affected_class: *class,
					policy: MergePolicyKind::ManualResolution,
					reason: MergeDecisionReason::ExplicitResolution,
					contributors: selection.sources.clone(),
					result: MergeDecisionResult::SelectSource { source: *source },
				});
			}
			NWayExactSelection::Delete {
				base,
				deleted_by,
				contributors,
			} => {
				selection.selected = None;
				selection.subtree_selected = false;
				for contributor in contributors.iter().copied() {
					selection.sources.insert(contributor);
				}
				plan.policy_excluded.insert(*class);
				plan.decisions.push(MergeDecisionEvidence {
					affected_class: *class,
					policy: MergePolicyKind::ManualResolution,
					reason: MergeDecisionReason::ExplicitResolution,
					contributors: selection.sources.clone(),
					result: MergeDecisionResult::Delete {
						deleted_by: vec![*deleted_by],
						base: *base,
					},
				});
			}
		}
	}
	for (class, revision) in &overrides.child_revisions {
		if let Some(selection) = plan.classes.get_mut(class) {
			selection.child_revision = Some(*revision);
		}
	}
}

fn clear_overridden_class_state(
	plan: &mut NWaySelectionPlan,
	correspondence: &NWayCorrespondence,
	class: ClassId,
) {
	plan.decisions
		.retain(|decision| decision.affected_class != class);
	plan.conflicts.retain(|conflict| {
		let affects_class = conflict
			.revisions
			.iter()
			.any(|source| correspondence.classes.class_of(*source) == class)
			|| conflict
				.base
				.is_some_and(|source| correspondence.classes.class_of(source) == class);
		!affects_class
	});
}

fn assign_nway_children(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	plan: &mut NWaySelectionPlan,
) {
	let live_classes = plan
		.classes
		.iter()
		.filter_map(|(class, selection)| selection.selected.map(|_| *class))
		.collect::<Vec<_>>();
	for parent in live_classes {
		if plan.classes[&parent].subtree_selected {
			continue;
		}
		let selected = plan.classes[&parent]
			.selected
			.expect("live class has a selected source");
		let selected_node = tree_for_revision(base, revisions, selected.revision)
			.node(selected.node)
			.unwrap();
		let base_sequence =
			class_child_sequence(base, RevisionId::BASE, parent, correspondence, plan);
		let revision_sequences = revisions
			.iter()
			.map(|revision| {
				class_child_sequence(revision.tree, revision.id, parent, correspondence, plan)
			})
			.collect::<Vec<_>>();
		let conflicts_before = plan.conflicts.len();
		let selected_children = plan.classes[&parent].child_revision.and_then(|revision| {
			if revision == RevisionId::BASE {
				return Some(base_sequence.clone());
			}
			revisions
				.iter()
				.position(|candidate| candidate.id == revision)
				.map(|index| revision_sequences[index].clone())
		});
		let children = if let Some(children) = selected_children {
			let retained = children.iter().copied().collect::<BTreeSet<_>>();
			for excluded in base_sequence
				.iter()
				.chain(revision_sequences.iter().flatten())
				.copied()
				.filter(|child| !retained.contains(child))
			{
				plan.policy_excluded.insert(excluded);
			}
			plan.decisions.push(MergeDecisionEvidence {
				affected_class: parent,
				policy: MergePolicyKind::ChildSetSelection,
				reason: MergeDecisionReason::ExplicitDomainRule,
				contributors: plan.classes[&parent].sources.clone(),
				result: MergeDecisionResult::CombineChildren,
			});
			children
		} else {
			match selected_node.child_cardinality {
				ChildCardinality::ExactlyOne => required_slot_nway_child(
					base,
					revisions,
					parent,
					&base_sequence,
					&revision_sequences,
					correspondence,
					plan,
				),
				ChildCardinality::Many if selected_node.child_order == ChildOrder::Commutative => {
					commutative_nway_children(
						base,
						revisions,
						&base_sequence,
						&revision_sequences,
						plan,
					)
				}
				ChildCardinality::Many => {
					let revision_refs = revision_sequences
						.iter()
						.map(Vec::as_slice)
						.collect::<Vec<_>>();
					match merge_orders(parent, &base_sequence, &revision_refs) {
						Ok(children) => children,
						Err(cycle) => {
							let parent_class = correspondence.classes.class(parent);
							let mut conflict = StructuralConflictDraft::new(
								ConflictKind::Ordering,
								Some(parent),
								parent_class
									.get(RevisionId::BASE)
									.map(|node| RevisionNode::new(RevisionId::BASE, node)),
								plan.classes[&parent].sources.clone(),
								Vec::new(),
								format!(
									"ordering constraints under class {} form a cycle across {} nodes",
									parent.get(),
									cycle.remaining.len(),
								),
							);
							conflict.semantic_path =
								class_common_policy_path(base, revisions, parent_class);
							plan.conflicts.push(conflict);
							fallback_nway_order(&base_sequence, &revision_sequences)
						}
					}
				}
			}
		};
		if selected_node.child_cardinality == ChildCardinality::Many
			&& revision_sequences
				.iter()
				.any(|sequence| sequence != &base_sequence)
			&& plan.conflicts.len() == conflicts_before
		{
			plan.decisions.push(MergeDecisionEvidence {
				affected_class: parent,
				policy: MergePolicyKind::Ordering,
				reason: MergeDecisionReason::StructuralConstraint,
				contributors: plan.classes[&parent].sources.clone(),
				result: MergeDecisionResult::CombineChildren,
			});
		}
		plan.classes
			.get_mut(&parent)
			.expect("selection exists for every class")
			.children = children;
	}
}

#[allow(clippy::too_many_arguments)]
fn required_slot_nway_child(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	parent: ClassId,
	base_sequence: &[ClassId],
	revision_sequences: &[Vec<ClassId>],
	correspondence: &NWayCorrespondence,
	plan: &mut NWaySelectionPlan,
) -> Vec<ClassId> {
	let candidates = base_sequence
		.iter()
		.chain(revision_sequences.iter().flatten())
		.copied()
		.collect::<BTreeSet<_>>();
	if candidates.len() == 1 {
		return candidates.into_iter().collect();
	}

	let selected_parent = plan.classes[&parent]
		.selected
		.expect("a live required-slot parent has a selected source");
	let selected_parent_node = tree_for_revision(base, revisions, selected_parent.revision)
		.node(selected_parent.node)
		.unwrap();
	let [selected_child_node] = selected_parent_node.children.as_slice() else {
		unreachable!("normalized exactly-one nodes always have one source child")
	};
	let selected_child = correspondence.classes.class_of(RevisionNode::new(
		selected_parent.revision,
		*selected_child_node,
	));
	let tentative = revision_sequences
		.iter()
		.rev()
		.find_map(|sequence| sequence.first().copied())
		.or_else(|| base_sequence.first().copied())
		.unwrap_or(selected_child);

	if plan.classes[&tentative].selected.is_none() {
		let source = correspondence
			.classes
			.class(tentative)
			.get(selected_parent.revision)
			.map(|node| RevisionNode::new(selected_parent.revision, node))
			.unwrap_or(RevisionNode::new(
				selected_parent.revision,
				*selected_child_node,
			));
		let selection = plan
			.classes
			.get_mut(&tentative)
			.expect("every required-slot child has a selection record");
		selection.selected = Some(source);
		selection.parent = Some(parent);
	}

	let child_sources = candidates
		.iter()
		.copied()
		.chain(std::iter::once(selected_child))
		.flat_map(|class| {
			correspondence
				.classes
				.class(class)
				.members
				.iter()
				.map(|(revision, node)| RevisionNode::new(*revision, *node))
				.collect::<Vec<_>>()
		});
	let parent_class = correspondence.classes.class(parent);
	let mut conflict = StructuralConflictDraft::new(
		ConflictKind::ValueSlot,
		Some(parent),
		parent_class.get(RevisionId::BASE).and_then(|base_parent| {
			let [base_child] = base.node(base_parent).ok()?.children.as_slice() else {
				return None;
			};
			Some(RevisionNode::new(RevisionId::BASE, *base_child))
		}),
		SourceSet::new(child_sources),
		Vec::new(),
		if candidates.is_empty() {
			format!(
				"required child slot under class {} lost every selected value; retained the selected source value tentatively",
				parent.get(),
			)
		} else {
			format!(
				"required child slot under class {} contains {} divergent values",
				parent.get(),
				candidates.len(),
			)
		},
	);
	conflict.semantic_path = class_common_policy_path(base, revisions, parent_class);
	plan.conflicts.push(conflict);
	vec![tentative]
}

fn class_child_sequence(
	tree: &NormalizedTree,
	revision: RevisionId,
	parent: ClassId,
	correspondence: &NWayCorrespondence,
	plan: &NWaySelectionPlan,
) -> Vec<ClassId> {
	let parent_class = correspondence.classes.class(parent);
	let Some(parent_node) = parent_class.get(revision) else {
		return Vec::new();
	};
	tree.node(parent_node)
		.unwrap()
		.children
		.iter()
		.map(|child| {
			correspondence
				.classes
				.class_of(RevisionNode::new(revision, *child))
		})
		.filter(|child| {
			plan.classes[child].selected.is_some() && plan.classes[child].parent == Some(parent)
		})
		.collect()
}

fn commutative_nway_children(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	base_sequence: &[ClassId],
	revision_sequences: &[Vec<ClassId>],
	plan: &NWaySelectionPlan,
) -> Vec<ClassId> {
	let mut children = base_sequence
		.iter()
		.chain(revision_sequences.iter().flatten())
		.copied()
		.collect::<BTreeSet<_>>()
		.into_iter()
		.collect::<Vec<_>>();
	children.sort_by_key(|class| {
		let selected = plan.classes[class]
			.selected
			.expect("commutative child is live");
		let node = tree_for_revision(base, revisions, selected.revision)
			.node(selected.node)
			.unwrap();
		(
			node.signature.clone(),
			node.anchor.clone(),
			node.kind.clone(),
			*class,
		)
	});
	children
}

fn fallback_nway_order(base: &[ClassId], revisions: &[Vec<ClassId>]) -> Vec<ClassId> {
	let mut order = Vec::new();
	for class in base.iter().chain(revisions.iter().flatten()) {
		if !order.contains(class) {
			order.push(*class);
		}
	}
	order
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

fn assign_nway_parents(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	overrides: &NWaySelectionOverrides,
	plan: &mut NWaySelectionPlan,
) {
	for class in correspondence.classes.classes() {
		if plan.classes[&class.id].selected.is_none() {
			continue;
		}
		if let Some(parent) = overrides.parents.get(&class.id) {
			plan.classes
				.get_mut(&class.id)
				.expect("selection exists for every class")
				.parent = *parent;
			continue;
		}
		let base_source = class
			.get(RevisionId::BASE)
			.map(|node| RevisionNode::new(RevisionId::BASE, node));
		let base_parent = base_source
			.and_then(|source| parent_class_for_source(base, source, &correspondence.classes));
		let revision_parents = revisions
			.iter()
			.filter_map(|revision| {
				class.get(revision.id).map(|node| {
					let source = RevisionNode::new(revision.id, node);
					(
						source,
						parent_class_for_source(revision.tree, source, &correspondence.classes),
					)
				})
			})
			.collect::<Vec<_>>();
		let changed_parents = if base_source.is_some() {
			revision_parents
				.iter()
				.filter(|(_, parent)| *parent != base_parent)
				.copied()
				.collect::<Vec<_>>()
		} else {
			revision_parents.clone()
		};
		let distinct = changed_parents
			.iter()
			.map(|(_, parent)| *parent)
			.collect::<BTreeSet<_>>();
		let parent = match distinct.len() {
			0 => base_parent,
			1 => *distinct.first().expect("one changed parent"),
			_ => {
				let tentative = changed_parents
					.last()
					.map(|(_, parent)| *parent)
					.expect("divergent parents have a tentative value");
				let sources = plan.classes[&class.id].sources.clone();
				let mut conflict = StructuralConflictDraft::new(
					ConflictKind::MoveMove,
					base_parent,
					base_source,
					sources,
					Vec::new(),
					format!(
						"revisions moved class {} to {} different parents",
						class.id.get(),
						distinct.len(),
					),
				);
				conflict.semantic_path = class_common_policy_path(base, revisions, class);
				plan.conflicts.push(conflict);
				tentative
			}
		};
		if distinct.len() == 1 {
			let source = changed_parents
				.last()
				.map(|(source, _)| *source)
				.expect("one changed parent has a source");
			plan.decisions.push(MergeDecisionEvidence {
				affected_class: class.id,
				policy: MergePolicyKind::Ordering,
				reason: if changed_parents.len() == 1 {
					MergeDecisionReason::OneSidedChange
				} else {
					MergeDecisionReason::EquivalentChanges
				},
				contributors: plan.classes[&class.id].sources.clone(),
				result: MergeDecisionResult::SelectSource { source },
			});
		}
		plan.classes
			.get_mut(&class.id)
			.expect("selection exists for every class")
			.parent = parent;
	}
}

fn parent_class_for_source(
	tree: &NormalizedTree,
	source: RevisionNode,
	classes: &ClassMapping,
) -> Option<ClassId> {
	tree.node(source.node)
		.unwrap()
		.parent
		.map(|parent| classes.class_of(RevisionNode::new(source.revision, parent)))
}

fn class_common_policy_path(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	class: &crate::merge::kernel::RevisionClass,
) -> Vec<String> {
	let base_node = class
		.get(RevisionId::BASE)
		.map(|node| base.node(node).unwrap());
	common_policy_path(
		base_node
			.into_iter()
			.chain(revisions.iter().filter_map(|revision| {
				class
					.get(revision.id)
					.map(|node| revision.tree.node(node).unwrap())
			})),
	)
}

fn select_inserted_class(
	class: ClassId,
	present: &[(&MergeRevision<'_>, RevisionNode)],
	sources: SourceSet,
	decisions: &mut Vec<MergeDecisionEvidence>,
	conflicts: &mut Vec<StructuralConflictDraft>,
) -> Option<RevisionNode> {
	let (_, first) = present.first()?;
	if present.iter().skip(1).all(|(revision, source)| {
		shallow_eq(
			present[0].0.tree.node(first.node).unwrap(),
			revision.tree.node(source.node).unwrap(),
		)
	}) {
		decisions.push(MergeDecisionEvidence {
			affected_class: class,
			policy: MergePolicyKind::ConservativeStructural,
			reason: if present.len() == 1 {
				MergeDecisionReason::OneSidedChange
			} else {
				MergeDecisionReason::EquivalentChanges
			},
			contributors: sources,
			result: MergeDecisionResult::SelectSource { source: *first },
		});
		return Some(*first);
	}
	let selected = present
		.last()
		.map(|(_, source)| *source)
		.expect("an inserted class has at least one source");
	let mut conflict = StructuralConflictDraft::new(
		ConflictKind::InsertInsert,
		None,
		None,
		sources,
		Vec::new(),
		format!(
			"{} revisions inserted divergent content into class {}",
			present.len(),
			class.get(),
		),
	);
	conflict.semantic_path = common_policy_path(
		present
			.iter()
			.map(|(revision, source)| revision.tree.node(source.node).unwrap()),
	);
	conflicts.push(conflict);
	Some(selected)
}

#[allow(clippy::too_many_arguments)]
fn select_base_class(
	class: ClassId,
	base: &NormalizedTree,
	base_source: RevisionNode,
	present: &[(&MergeRevision<'_>, RevisionNode)],
	facts: &NWayClassFacts,
	sources: SourceSet,
	decisions: &mut Vec<MergeDecisionEvidence>,
	conflicts: &mut Vec<StructuralConflictDraft>,
) -> Option<RevisionNode> {
	if present.is_empty() {
		decisions.push(MergeDecisionEvidence {
			affected_class: class,
			policy: MergePolicyKind::ConservativeStructural,
			reason: MergeDecisionReason::OneSidedChange,
			contributors: sources,
			result: MergeDecisionResult::Delete {
				deleted_by: facts.deleted_by.clone(),
				base: base_source,
			},
		});
		return None;
	}
	let base_node = base.node(base_source.node).unwrap();
	if !facts.deleted_by.is_empty() {
		let modified = present.iter().any(|(_, source)| {
			facts
				.subtree_changed
				.iter()
				.any(|changed| changed == source)
				|| facts.moved.iter().any(|moved| moved == source)
				|| facts.reordered.iter().any(|reordered| reordered == source)
		});
		if !modified {
			decisions.push(MergeDecisionEvidence {
				affected_class: class,
				policy: MergePolicyKind::ConservativeStructural,
				reason: MergeDecisionReason::OneSidedChange,
				contributors: sources,
				result: MergeDecisionResult::Delete {
					deleted_by: facts.deleted_by.clone(),
					base: base_source,
				},
			});
			return None;
		}
		let selected = present
			.last()
			.map(|(_, source)| *source)
			.expect("a modified surviving class has a source");
		let change_kinds = [
			(!facts.subtree_changed.is_empty()).then_some("modified"),
			(!facts.moved.is_empty()).then_some("moved"),
			(!facts.reordered.is_empty()).then_some("reordered"),
		]
		.into_iter()
		.flatten()
		.collect::<Vec<_>>()
		.join(", ");
		let mut conflict = StructuralConflictDraft::new(
			ConflictKind::DeleteModify,
			None,
			Some(base_source),
			sources,
			Vec::new(),
			format!(
				"revisions {} deleted class {} while another revision changed it ({change_kinds})",
				facts
					.deleted_by
					.iter()
					.map(|revision| revision.get().to_string())
					.collect::<Vec<_>>()
					.join(", "),
				class.get(),
			),
		);
		// Keep all revisions as evidence, but only changed survivors are choices.
		// Deletions are added as tombstones below; the ancestor keeps its own slot.
		conflict.candidates.retain(|candidate| match candidate {
			RevisionSourceRef::Node(source) => {
				*source == base_source
					|| facts
						.subtree_changed
						.iter()
						.chain(facts.moved.iter())
						.chain(facts.reordered.iter())
						.any(|changed| changed == source)
			}
			RevisionSourceRef::Tombstone { .. } => true,
		});
		for revision in &facts.deleted_by {
			conflict = conflict.with_candidate(RevisionSourceRef::Tombstone {
				revision: *revision,
				base_node: base_source.node,
			});
		}
		conflict.semantic_path = common_policy_path(
			std::iter::once(base_node).chain(
				present
					.iter()
					.map(|(revision, source)| revision.tree.node(source.node).unwrap()),
			),
		);
		conflicts.push(conflict);
		return Some(selected);
	}

	let changed = present
		.iter()
		.filter(|(revision, source)| {
			!shallow_eq(base_node, revision.tree.node(source.node).unwrap())
		})
		.copied()
		.collect::<Vec<_>>();
	let Some((first_revision, first_source)) = changed.first().copied() else {
		return Some(base_source);
	};
	if changed.iter().skip(1).all(|(revision, source)| {
		shallow_eq(
			first_revision.tree.node(first_source.node).unwrap(),
			revision.tree.node(source.node).unwrap(),
		)
	}) {
		decisions.push(MergeDecisionEvidence {
			affected_class: class,
			policy: MergePolicyKind::ConservativeStructural,
			reason: if changed.len() == 1 {
				MergeDecisionReason::OneSidedChange
			} else {
				MergeDecisionReason::EquivalentChanges
			},
			contributors: sources,
			result: MergeDecisionResult::SelectSource {
				source: first_source,
			},
		});
		return Some(first_source);
	}

	let selected = changed
		.last()
		.map(|(_, source)| *source)
		.expect("divergent changes have a tentative source");
	let mut conflict = StructuralConflictDraft::new(
		ConflictKind::Policy,
		None,
		Some(base_source),
		sources,
		Vec::new(),
		format!(
			"{} revisions changed class {} differently",
			changed.len(),
			class.get(),
		),
	);
	// Unmodified carriers belong to the input history, not the final alternatives.
	conflict.candidates.retain(|candidate| match candidate {
		RevisionSourceRef::Node(source) => {
			*source == base_source
				|| changed
					.iter()
					.any(|(_, changed_source)| changed_source == source)
		}
		RevisionSourceRef::Tombstone { .. } => true,
	});
	conflict.semantic_path = common_policy_path(
		std::iter::once(base_node).chain(
			changed
				.iter()
				.map(|(revision, source)| revision.tree.node(source.node).unwrap()),
		),
	);
	conflicts.push(conflict);
	Some(selected)
}

fn shallow_eq(left: &NormalizedNode, right: &NormalizedNode) -> bool {
	left.kind == right.kind
		&& left.value == right.value
		&& left.anchor == right.anchor
		&& left.signature == right.signature
		&& left.child_order == right.child_order
		&& left.child_cardinality == right.child_cardinality
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

fn build_class_facts(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	classes: &ClassMapping,
	deltas: &BTreeMap<RevisionId, RevisionDelta>,
) -> BTreeMap<ClassId, NWayClassFacts> {
	classes
		.classes()
		.map(|class| {
			let base_node = class.get(RevisionId::BASE);
			let base_ref = base_node.map(|node| RevisionNode::new(RevisionId::BASE, node));
			let mut present = SourceSet::default();
			let mut subtree_changed = SourceSet::default();
			let mut moved = SourceSet::default();
			let mut reordered = SourceSet::default();
			let mut deleted_by = Vec::new();
			for revision in revisions {
				let Some(node) = class.get(revision.id) else {
					if base_node.is_some() {
						deleted_by.push(revision.id);
					}
					continue;
				};
				let source = RevisionNode::new(revision.id, node);
				present.insert(source);
				if base_node.is_none()
					|| base_node.is_some_and(|base_node| {
						base.node(base_node).unwrap().subtree_hash
							!= revision.tree.node(node).unwrap().subtree_hash
					}) {
					subtree_changed.insert(source);
				}
				if deltas[&revision.id].operations.iter().any(|operation| {
					matches!(
						operation,
						DeltaOperation::Move {
							revision: moved,
							..
						} if *moved == source
					)
				}) {
					moved.insert(source);
				}
				if deltas[&revision.id].ordering.iter().any(|fact| {
					fact.before.base.is_some()
						&& fact.after.base.is_some()
						&& (fact.before.source == source || fact.after.source == source)
				}) {
					reordered.insert(source);
				}
			}
			(
				class.id,
				NWayClassFacts {
					class: class.id,
					base: base_ref,
					present,
					subtree_changed,
					moved,
					reordered,
					deleted_by,
				},
			)
		})
		.collect()
}

fn nanos(duration: Duration) -> u64 {
	u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::merge::kernel::{
		ConservativeMergePolicy, MergePolicy, NWayClassContext, NWayDeleteContext, NodeId,
		PolicyDecision, RevisionNode, SourceNodeRef, TreeNode, n_way_merge,
		n_way_merge_with_policy, n_way_merge_with_policy_and_resolutions,
	};

	fn normalize(root: TreeNode) -> NormalizedTree {
		NormalizedTree::from_root(root).unwrap()
	}

	fn root(children: Vec<TreeNode>) -> NormalizedTree {
		normalize(TreeNode::branch("root", children))
	}

	fn field(value: &str) -> TreeNode {
		TreeNode::leaf("field", value).with_anchor("field", "shared")
	}

	#[test]
	fn correspondence_retains_every_original_revision_in_one_class() {
		let base = root(vec![field("base")]);
		let first = root(vec![field("one")]);
		let second = root(vec![field("two")]);
		let third = root(vec![field("three")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
			MergeRevision::new(RevisionId::new(3), &third),
		];

		let correspondence = NWayCorrespondence::build(&base, &revisions).unwrap();
		let base_field = base.node(base.root()).unwrap().children[0];
		let class = correspondence.classes().class(
			correspondence
				.classes()
				.class_of(RevisionNode::new(RevisionId::BASE, base_field)),
		);

		assert_eq!(class.members.len(), 4);
		assert_eq!(
			correspondence.revision_order(),
			&[RevisionId::new(1), RevisionId::new(2), RevisionId::new(3)],
		);
		assert_eq!(correspondence.revision_deltas().len(), 3);
		assert_eq!(correspondence.inputs().len(), 4);
		let facts = &correspondence.class_facts()[&class.id];
		assert_eq!(facts.present.len(), 3);
		assert_eq!(facts.subtree_changed.len(), 3);
		assert!(facts.deleted_by.is_empty());
	}

	#[test]
	fn cross_revision_recovery_matches_insertions_absent_from_base() {
		let base = root(Vec::new());
		let first = root(vec![field("one")]);
		let second = root(vec![field("two")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
		];

		let correspondence = NWayCorrespondence::build(&base, &revisions).unwrap();
		let first_field = first.node(first.root()).unwrap().children[0];
		let class = correspondence.classes().class(
			correspondence
				.classes()
				.class_of(RevisionNode::new(RevisionId::new(1), first_field)),
		);

		assert_eq!(class.members.len(), 2);
		assert!(!class.members.contains_key(&RevisionId::BASE));
		assert!(
			correspondence
				.matching(RevisionId::new(1), RevisionId::new(2))
				.is_some_and(|matching| matching.get_from_left(first_field).is_some()),
		);
	}

	#[test]
	fn duplicate_and_base_revision_ids_are_rejected() {
		let base = root(Vec::new());
		let revision = root(Vec::new());

		assert_eq!(
			NWayCorrespondence::build(&base, &[MergeRevision::new(RevisionId::BASE, &revision)]),
			Err(NWayInputError::BaseRevision),
		);
		assert_eq!(
			NWayCorrespondence::build(
				&base,
				&[
					MergeRevision::new(RevisionId::new(1), &revision),
					MergeRevision::new(RevisionId::new(1), &revision),
				],
			),
			Err(NWayInputError::DuplicateRevision(RevisionId::new(1))),
		);
	}

	#[test]
	fn class_facts_preserve_deletions_and_moves_per_original_revision() {
		let container = |name: &str, children: Vec<TreeNode>| {
			TreeNode::branch("container", children).with_anchor("container", name)
		};
		let entry = || TreeNode::leaf("entry", "value").with_anchor("entry", "same");
		let base = root(vec![
			container("a", vec![entry()]),
			container("b", Vec::new()),
		]);
		let deleted = root(vec![container("a", Vec::new()), container("b", Vec::new())]);
		let moved = root(vec![
			container("a", Vec::new()),
			container("b", vec![entry()]),
		]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &deleted),
			MergeRevision::new(RevisionId::new(2), &moved),
		];

		let correspondence = NWayCorrespondence::build(&base, &revisions).unwrap();
		let base_a = base.node(base.root()).unwrap().children[0];
		let base_entry = base.node(base_a).unwrap().children[0];
		let class = correspondence
			.classes()
			.class_of(RevisionNode::new(RevisionId::BASE, base_entry));
		let facts = &correspondence.class_facts()[&class];

		assert_eq!(facts.deleted_by, vec![RevisionId::new(1)]);
		assert_eq!(facts.present.len(), 1);
		assert_eq!(facts.moved.len(), 1);
	}

	#[test]
	fn selection_considers_all_divergent_original_revisions_once() {
		let base = root(vec![field("base")]);
		let first = root(vec![field("one")]);
		let second = root(vec![field("two")]);
		let third = root(vec![field("three")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
			MergeRevision::new(RevisionId::new(3), &third),
		];
		let correspondence = NWayCorrespondence::build(&base, &revisions).unwrap();
		let base_field = base.node(base.root()).unwrap().children[0];
		let class = correspondence
			.classes()
			.class_of(RevisionNode::new(RevisionId::BASE, base_field));

		let plan = correspondence.conservative_selection(&base, &revisions);

		assert_eq!(plan.conflicts.len(), 1);
		assert_eq!(plan.conflicts[0].kind, ConflictKind::Policy);
		assert_eq!(plan.conflicts[0].candidates.len(), 4);
		assert_eq!(
			plan.classes[&class].selected.unwrap().revision,
			RevisionId::new(3),
		);
	}

	#[test]
	fn selection_preserves_delete_modify_tombstone_candidates() {
		let base = root(vec![field("base")]);
		let deleted = root(Vec::new());
		let modified = root(vec![field("changed")]);
		let unchanged = base.clone();
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &deleted),
			MergeRevision::new(RevisionId::new(2), &modified),
			MergeRevision::new(RevisionId::new(3), &unchanged),
		];
		let correspondence = NWayCorrespondence::build(&base, &revisions).unwrap();

		let plan = correspondence.conservative_selection(&base, &revisions);

		assert_eq!(plan.conflicts.len(), 1);
		assert_eq!(plan.conflicts[0].kind, ConflictKind::DeleteModify);
		assert_eq!(plan.conflicts[0].candidates.len(), 3);
		assert!(
			!plan.conflicts[0]
				.candidates
				.iter()
				.any(|candidate| matches!(
					candidate, RevisionSourceRef::Node(source) if source.revision == RevisionId::new(3)
				))
		);
		assert!(
			plan.conflicts[0]
				.candidates
				.iter()
				.any(|candidate| matches!(
					candidate,
					RevisionSourceRef::Tombstone { revision, .. }
						if *revision == RevisionId::new(1)
				)),
		);
	}

	#[test]
	fn final_conflict_candidates_exclude_unchanged_revisions_and_keep_equivalent_changes() {
		let base: NormalizedTree = root(vec![field("base")]);
		let first: NormalizedTree = root(vec![field("one")]);
		let second: NormalizedTree = root(vec![field("two")]);
		let equivalent: NormalizedTree = first.clone();
		let unchanged: NormalizedTree = base.clone();
		let revisions: [MergeRevision<'_>; 4] = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
			MergeRevision::new(RevisionId::new(3), &equivalent),
			MergeRevision::new(RevisionId::new(4), &unchanged),
		];
		let outcome: crate::merge::kernel::MergeOutcome = n_way_merge(&base, &revisions).unwrap();
		assert_eq!(outcome.conflicts.len(), 1);
		let conflict: &crate::merge::kernel::StructuralConflict = &outcome.conflicts[0];
		assert_eq!(
			conflict.revisions.len(),
			5,
			"retain complete input evidence"
		);
		let candidates: Vec<RevisionId> = conflict
			.candidates
			.iter()
			.map(|candidate| candidate.input().revision)
			.collect();
		assert_eq!(
			candidates,
			[
				RevisionId::BASE,
				RevisionId::new(1),
				RevisionId::new(2),
				RevisionId::new(3)
			]
		);
		let unchanged_source: SourceNodeRef = SourceNodeRef::Node {
			input: crate::merge::kernel::MergeInputId::from_tree(RevisionId::new(4), &unchanged),
			node: unchanged.node(unchanged.root()).unwrap().children[0],
		};
		assert!(
			conflict.select(unchanged_source).is_err(),
			"unchanged carrier must not be selectable"
		);
	}

	#[test]
	fn selection_applies_uncontested_deletion_without_conflict() {
		let base = root(vec![field("base")]);
		let deleted = root(Vec::new());
		let unchanged = base.clone();
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &deleted),
			MergeRevision::new(RevisionId::new(2), &unchanged),
		];
		let correspondence = NWayCorrespondence::build(&base, &revisions).unwrap();
		let base_field = base.node(base.root()).unwrap().children[0];
		let class = correspondence
			.classes()
			.class_of(RevisionNode::new(RevisionId::BASE, base_field));

		let plan = correspondence.conservative_selection(&base, &revisions);

		assert!(plan.conflicts.is_empty());
		assert!(plan.classes[&class].selected.is_none());
		assert!(plan.decisions.iter().any(|decision| {
			decision.affected_class == class
				&& matches!(
					&decision.result,
					MergeDecisionResult::Delete { deleted_by, .. }
						if deleted_by == &[RevisionId::new(1)]
				)
		}));
	}

	#[test]
	fn selection_reports_conflicting_reparents_across_original_revisions() {
		let container = |name: &str, children: Vec<TreeNode>| {
			TreeNode::branch("container", children).with_anchor("container", name)
		};
		let entry = || TreeNode::leaf("entry", "value").with_anchor("entry", "same");
		let base = root(vec![
			container("a", vec![entry()]),
			container("b", Vec::new()),
			container("c", Vec::new()),
		]);
		let move_to_b = root(vec![
			container("a", Vec::new()),
			container("b", vec![entry()]),
			container("c", Vec::new()),
		]);
		let move_to_c = root(vec![
			container("a", Vec::new()),
			container("b", Vec::new()),
			container("c", vec![entry()]),
		]);
		let unchanged = base.clone();
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &move_to_b),
			MergeRevision::new(RevisionId::new(2), &move_to_c),
			MergeRevision::new(RevisionId::new(3), &unchanged),
		];
		let correspondence = NWayCorrespondence::build(&base, &revisions).unwrap();
		let base_a = base.node(base.root()).unwrap().children[0];
		let base_entry = base.node(base_a).unwrap().children[0];
		let entry_class = correspondence
			.classes()
			.class_of(RevisionNode::new(RevisionId::BASE, base_entry));
		let move_to_c_root = move_to_c.node(move_to_c.root()).unwrap();
		let c_class = correspondence.classes().class_of(RevisionNode::new(
			RevisionId::new(2),
			move_to_c_root.children[2],
		));

		let plan = correspondence.conservative_selection(&base, &revisions);

		assert!(
			plan.conflicts
				.iter()
				.any(|conflict| conflict.kind == ConflictKind::MoveMove),
			"{:?}",
			plan.conflicts,
		);
		assert_eq!(plan.classes[&entry_class].parent, Some(c_class));
	}

	#[test]
	fn selection_orders_all_independent_insertions_in_one_pcs_merge() {
		let anchored = |name: &str| TreeNode::leaf("entry", name).with_anchor("entry", name);
		let base = root(vec![anchored("base")]);
		let first = root(vec![anchored("left"), anchored("base")]);
		let second = root(vec![anchored("base"), anchored("middle")]);
		let third = root(vec![anchored("base"), anchored("right")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
			MergeRevision::new(RevisionId::new(3), &third),
		];
		let correspondence = NWayCorrespondence::build(&base, &revisions).unwrap();
		let root_class = correspondence
			.classes()
			.class_of(RevisionNode::new(RevisionId::BASE, base.root()));

		let plan = correspondence.conservative_selection(&base, &revisions);
		let values = plan.classes[&root_class]
			.children
			.iter()
			.map(|class| {
				let selected = plan.classes[class].selected.unwrap();
				tree_for_revision(&base, &revisions, selected.revision)
					.node(selected.node)
					.unwrap()
					.value
					.clone()
					.unwrap()
			})
			.collect::<Vec<_>>();

		assert_eq!(values, vec!["left", "base", "middle", "right"]);
		assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
	}

	#[test]
	fn merge_materializes_all_independent_insertions_in_one_tree() {
		let anchored = |name: &str| TreeNode::leaf("entry", name).with_anchor("entry", name);
		let base = root(vec![anchored("base")]);
		let first = root(vec![anchored("left"), anchored("base")]);
		let second = root(vec![anchored("base"), anchored("middle")]);
		let third = root(vec![anchored("base"), anchored("right")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
			MergeRevision::new(RevisionId::new(3), &third),
		];

		let outcome = n_way_merge(&base, &revisions).unwrap();
		let output_root = outcome
			.tentative_tree()
			.node(outcome.tentative_tree().root())
			.unwrap();
		let values = output_root
			.children
			.iter()
			.map(|child| {
				outcome
					.tentative_tree()
					.node(*child)
					.unwrap()
					.value
					.clone()
					.unwrap()
			})
			.collect::<Vec<_>>();

		assert_eq!(values, vec!["left", "base", "middle", "right"]);
		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert_eq!(outcome.revision_deltas.len(), 3);
		assert_eq!(
			outcome.provenance[&outcome.tentative_tree().root()].len(),
			4,
		);
		assert!(outcome.decisions.iter().any(|decision| {
			decision.affected_class == correspondence_root_class(&base, &revisions)
				&& decision.result == MergeDecisionResult::CombineChildren
		}));
	}

	#[test]
	fn merge_materializes_a_valid_tentative_tree_for_conflicting_reparents() {
		let container = |name: &str, children: Vec<TreeNode>| {
			TreeNode::branch("container", children).with_anchor("container", name)
		};
		let entry = || TreeNode::leaf("entry", "value").with_anchor("entry", "same");
		let base = root(vec![
			container("a", vec![entry()]),
			container("b", Vec::new()),
			container("c", Vec::new()),
		]);
		let move_to_b = root(vec![
			container("a", Vec::new()),
			container("b", vec![entry()]),
			container("c", Vec::new()),
		]);
		let move_to_c = root(vec![
			container("a", Vec::new()),
			container("b", Vec::new()),
			container("c", vec![entry()]),
		]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &move_to_b),
			MergeRevision::new(RevisionId::new(2), &move_to_c),
		];

		let outcome = n_way_merge(&base, &revisions).unwrap();
		let conflict = outcome
			.conflicts
			.iter()
			.find(|conflict| conflict.kind == ConflictKind::MoveMove)
			.expect("the divergent reparent remains an explicit conflict");

		assert_eq!(conflict.candidates.len(), 3);
		assert_eq!(outcome.tentative_tree().len(), base.len());
		assert_eq!(outcome.provenance.len(), outcome.tentative_tree().len());
	}

	#[test]
	fn merge_binds_delete_modify_candidates_to_original_inputs() {
		let base = root(vec![field("base")]);
		let deleted = root(Vec::new());
		let modified = root(vec![field("modified")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &deleted),
			MergeRevision::new(RevisionId::new(2), &modified),
		];

		let outcome = n_way_merge(&base, &revisions).unwrap();
		let conflict = outcome
			.conflicts
			.iter()
			.find(|conflict| conflict.kind == ConflictKind::DeleteModify)
			.expect("delete/modify remains unresolved");

		assert!(conflict.candidates.iter().any(|candidate| matches!(
			candidate,
			crate::merge::kernel::SourceNodeRef::Tombstone { input, .. }
				if input.revision == RevisionId::new(1)
		)));
		assert!(conflict.candidates.iter().any(|candidate| matches!(
			candidate,
			crate::merge::kernel::SourceNodeRef::Node { input, .. }
				if input.revision == RevisionId::new(2)
		)));
	}

	#[test]
	fn nway_policy_can_select_the_last_original_revision() {
		struct LastWriter;

		impl MergePolicy for LastWriter {
			fn resolve_nway_divergent_node(&self, context: NWayClassContext<'_>) -> PolicyDecision {
				PolicyDecision::Select(
					context
						.contributors
						.last()
						.expect("divergent contributors are nonempty")
						.source
						.revision,
				)
			}
		}

		let base = root(vec![field("base")]);
		let first = root(vec![field("one")]);
		let second = root(vec![field("two")]);
		let third = root(vec![field("three")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
			MergeRevision::new(RevisionId::new(3), &third),
		];

		let outcome = n_way_merge_with_policy(&base, &revisions, &LastWriter).unwrap();
		let child = outcome.tentative_tree().node(NodeId::new(1)).unwrap();

		assert_eq!(child.value.as_deref(), Some("three"));
		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert!(outcome.decisions.iter().any(|decision| {
			decision.policy == MergePolicyKind::DivergentNode
				&& decision.result
					== MergeDecisionResult::SelectSource {
						source: RevisionNode::new(RevisionId::new(3), NodeId::new(1)),
					}
		}));
	}

	#[test]
	fn nway_policy_can_synthesize_one_scalar_from_all_changed_revisions() {
		struct Sum;

		impl MergePolicy for Sum {
			fn resolve_nway_divergent_node(&self, context: NWayClassContext<'_>) -> PolicyDecision {
				let sum = context
					.contributors
					.iter()
					.filter(|view| view.shallow_changed)
					.map(|view| view.node.value.as_deref().unwrap().parse::<i64>().unwrap())
					.sum::<i64>();
				PolicyDecision::SynthesizeScalar(sum.to_string())
			}
		}

		let base = root(vec![field("0")]);
		let first = root(vec![field("1")]);
		let second = root(vec![field("2")]);
		let third = root(vec![field("3")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
			MergeRevision::new(RevisionId::new(3), &third),
		];

		let outcome = n_way_merge_with_policy(&base, &revisions, &Sum).unwrap();
		let child = outcome.tentative_tree().node(NodeId::new(1)).unwrap();

		assert_eq!(child.value.as_deref(), Some("6"));
		assert_eq!(child.scalar_reducer_inputs.len(), 3);
		assert_eq!(child.scalar_reducer_output.as_deref(), Some("6"));
		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
	}

	#[test]
	fn exact_resolution_selects_one_original_subtree() {
		let base = root(vec![field("base")]);
		let first = root(vec![field("one")]);
		let second = root(vec![field("two")]);
		let third = root(vec![field("three")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
			MergeRevision::new(RevisionId::new(3), &third),
		];
		let probe = n_way_merge(&base, &revisions).unwrap();
		let conflict = probe
			.conflicts
			.iter()
			.find(|conflict| conflict.kind == ConflictKind::Policy)
			.expect("divergent scalar conflict");
		let selected = conflict
			.candidates
			.iter()
			.copied()
			.find(|candidate| candidate.input().revision == RevisionId::new(1))
			.expect("first revision candidate");
		let resolution = conflict.select(selected).unwrap();

		let outcome = n_way_merge_with_policy_and_resolutions(
			&base,
			&revisions,
			&ConservativeMergePolicy,
			&[resolution],
		)
		.unwrap();

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		let root = outcome
			.tentative_tree()
			.node(outcome.tentative_tree().root())
			.unwrap();
		let [field] = root.children.as_slice() else {
			panic!("expected one selected field")
		};
		assert_eq!(
			outcome
				.tentative_tree()
				.node(*field)
				.unwrap()
				.value
				.as_deref(),
			Some("one"),
		);
		assert!(outcome.decisions.iter().any(|decision| {
			decision.policy == MergePolicyKind::ManualResolution
				&& matches!(
					decision.result,
					MergeDecisionResult::SelectSource { source }
						if source.revision == RevisionId::new(1)
				)
		}));
	}

	#[test]
	fn exact_resolution_selects_a_whole_subtree_across_delete_modify() {
		let assignment = |value: &str| {
			TreeNode::branch("assignment", vec![field(value)])
				.with_anchor("assignment", "shared")
				.with_child_cardinality(ChildCardinality::ExactlyOne)
		};
		let base = root(vec![assignment("base")]);
		let first = root(vec![assignment("one")]);
		let deleted = root(Vec::new());
		let third = root(vec![assignment("three")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &deleted),
			MergeRevision::new(RevisionId::new(3), &third),
		];
		let probe = n_way_merge(&base, &revisions).unwrap();
		let conflict = probe
			.conflicts
			.iter()
			.find(|conflict| conflict.kind == ConflictKind::DeleteModify)
			.expect("delete/modify conflict");
		let selected = conflict
			.candidates
			.iter()
			.copied()
			.find(|candidate| candidate.input().revision == RevisionId::new(1))
			.expect("first revision candidate");
		let resolution = conflict.select(selected).unwrap();

		let outcome = n_way_merge_with_policy_and_resolutions(
			&base,
			&revisions,
			&ConservativeMergePolicy,
			&[resolution],
		)
		.unwrap();

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert_eq!(
			outcome
				.tentative_tree()
				.node(NodeId::new(2))
				.unwrap()
				.value
				.as_deref(),
			Some("one"),
		);
	}

	#[test]
	fn exact_value_slot_resolution_clears_conflicts_on_the_selected_class() {
		let slot = |kind: &str, value: &str| {
			TreeNode::branch("slot", vec![TreeNode::leaf(kind, value)])
				.with_anchor("slot", "shared")
				.with_child_cardinality(ChildCardinality::ExactlyOne)
		};
		let base = root(vec![slot("bool", "false")]);
		let first = root(vec![slot("bool", "true")]);
		let second = root(vec![slot("identifier", "maybe")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
		];
		let probe = n_way_merge(&base, &revisions).unwrap();
		let conflict = probe
			.conflicts
			.iter()
			.find(|conflict| conflict.kind == ConflictKind::ValueSlot)
			.expect("divergent required slot conflict");
		let selected = conflict
			.candidates
			.iter()
			.copied()
			.find(|candidate| candidate.input().revision == RevisionId::new(1))
			.expect("first revision candidate");
		let resolution = conflict.select(selected).unwrap();

		let outcome = n_way_merge_with_policy_and_resolutions(
			&base,
			&revisions,
			&ConservativeMergePolicy,
			&[resolution],
		)
		.unwrap();

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		let root = outcome.tentative_tree().node(NodeId::new(0)).unwrap();
		let slot = outcome.tentative_tree().node(root.children[0]).unwrap();
		assert_eq!(
			outcome
				.tentative_tree()
				.node(slot.children[0])
				.unwrap()
				.value
				.as_deref(),
			Some("true"),
		);
	}

	#[test]
	fn exact_resolution_can_select_an_original_tombstone() {
		let base = root(vec![field("base")]);
		let deleted = root(Vec::new());
		let modified = root(vec![field("changed")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &deleted),
			MergeRevision::new(RevisionId::new(2), &modified),
		];
		let probe = n_way_merge(&base, &revisions).unwrap();
		let conflict = probe
			.conflicts
			.iter()
			.find(|conflict| conflict.kind == ConflictKind::DeleteModify)
			.expect("delete/modify conflict");
		let selected = conflict
			.candidates
			.iter()
			.copied()
			.find(|candidate| matches!(candidate, SourceNodeRef::Tombstone { .. }))
			.expect("deletion candidate");
		let resolution = conflict.select(selected).unwrap();

		let outcome = n_way_merge_with_policy_and_resolutions(
			&base,
			&revisions,
			&ConservativeMergePolicy,
			&[resolution],
		)
		.unwrap();

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert!(
			outcome
				.tentative_tree()
				.node(outcome.tentative_tree().root())
				.unwrap()
				.children
				.is_empty(),
		);
	}

	#[test]
	fn nway_delete_policy_can_preserve_a_modified_original_revision() {
		struct EditWins;

		impl MergePolicy for EditWins {
			fn resolve_nway_delete(&self, context: NWayDeleteContext<'_>) -> PolicyDecision {
				if context
					.class
					.contributors
					.iter()
					.any(|view| view.subtree_changed)
				{
					PolicyDecision::Resolved
				} else {
					PolicyDecision::Unresolved
				}
			}
		}

		let base = root(vec![field("base")]);
		let deleted = root(Vec::new());
		let modified = root(vec![field("modified")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &deleted),
			MergeRevision::new(RevisionId::new(2), &modified),
		];

		let outcome = n_way_merge_with_policy(&base, &revisions, &EditWins).unwrap();
		let child = outcome.tentative_tree().node(NodeId::new(1)).unwrap();

		assert_eq!(child.value.as_deref(), Some("modified"));
		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert!(outcome.decisions.iter().any(|decision| {
			decision.policy == MergePolicyKind::DeleteModify
				&& decision.result
					== MergeDecisionResult::SelectSource {
						source: RevisionNode::new(RevisionId::new(2), NodeId::new(1)),
					}
		}));
	}

	#[test]
	fn nway_delete_policy_can_preserve_a_complete_original_subtree() {
		struct PreserveSubtree;

		impl MergePolicy for PreserveSubtree {
			fn select_nway_deleted_subtree(
				&self,
				context: NWayDeleteContext<'_>,
			) -> Option<RevisionId> {
				context
					.class
					.contributors
					.last()
					.map(|view| view.source.revision)
			}
		}

		let container = || {
			TreeNode::branch("container", vec![field("retained")])
				.with_anchor("container", "shared")
		};
		let base = root(vec![container()]);
		let deleted = root(Vec::new());
		let present = root(vec![container()]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &deleted),
			MergeRevision::new(RevisionId::new(2), &present),
		];

		let outcome = n_way_merge_with_policy(&base, &revisions, &PreserveSubtree).unwrap();
		let tree = outcome.tentative_tree();
		let container = tree
			.node(tree.node(tree.root()).unwrap().children[0])
			.unwrap();

		assert_eq!(container.children.len(), 1);
		assert_eq!(
			tree.node(container.children[0]).unwrap().value.as_deref(),
			Some("retained"),
		);
		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert!(outcome.decisions.iter().any(|decision| {
			decision.policy == MergePolicyKind::SubtreeSelection
				&& matches!(
					decision.result,
					MergeDecisionResult::SelectSource { source }
						if source.revision == RevisionId::new(2)
				)
		}));
	}

	#[test]
	fn nway_subtree_policy_excludes_descendant_conflicts() {
		struct LastSubtree;

		impl MergePolicy for LastSubtree {
			fn select_nway_subtree(&self, context: NWayClassContext<'_>) -> Option<RevisionId> {
				(context
					.base
					.is_some_and(|base| base.node.kind == "container"))
				.then(|| context.contributors.last().unwrap().source.revision)
			}
		}

		let container = |value: &str| {
			TreeNode::branch("container", vec![field(value)]).with_anchor("container", "shared")
		};
		let base = root(vec![container("base")]);
		let first = root(vec![container("one")]);
		let second = root(vec![container("two")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
		];

		let outcome = n_way_merge_with_policy(&base, &revisions, &LastSubtree).unwrap();
		let container = outcome.tentative_tree().node(NodeId::new(1)).unwrap();
		let value = outcome
			.tentative_tree()
			.node(container.children[0])
			.unwrap()
			.value
			.as_deref();

		assert_eq!(value, Some("two"));
		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert!(
			outcome
				.decisions
				.iter()
				.any(|decision| { decision.policy == MergePolicyKind::SubtreeSelection })
		);
		assert!(
			!outcome
				.decisions
				.iter()
				.any(|decision| { decision.policy == MergePolicyKind::DivergentNode })
		);
	}

	#[test]
	fn merge_suppresses_delete_modify_conflicts_below_a_deleted_ancestor() {
		let entry = |value: &str| {
			TreeNode::branch("entry", vec![TreeNode::leaf("value", value)])
				.with_anchor("entry", "a")
		};
		let base = root(vec![entry("old")]);
		let deleted = root(Vec::new());
		let modified = root(vec![entry("new")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &deleted),
			MergeRevision::new(RevisionId::new(2), &modified),
		];

		let outcome = n_way_merge(&base, &revisions).unwrap();
		let conflicts = outcome
			.conflicts
			.iter()
			.filter(|conflict| conflict.kind == ConflictKind::DeleteModify)
			.collect::<Vec<_>>();

		assert_eq!(conflicts.len(), 1, "{:?}", outcome.conflicts);
		assert!(conflicts[0].candidates.iter().any(|candidate| matches!(
			candidate,
			SourceNodeRef::Tombstone { input, .. } if input.revision == RevisionId::new(1)
		)));
		assert!(conflicts[0].candidates.iter().any(|candidate| matches!(
			candidate,
			SourceNodeRef::Node { input, .. } if input.revision == RevisionId::new(2)
		)));
	}

	#[test]
	fn merge_reports_delete_against_reorder_under_an_ordered_parent() {
		let container =
			|children| TreeNode::branch("container", children).with_anchor("container", "a");
		let entry = || TreeNode::leaf("entry", "payload").with_anchor("entry", "movable");
		let sibling = |name: &str| TreeNode::leaf("entry", name).with_anchor("entry", name);
		let base = root(vec![container(vec![
			sibling("before"),
			entry(),
			sibling("after"),
		])]);
		let deleted = root(vec![container(vec![sibling("before"), sibling("after")])]);
		let reordered = root(vec![container(vec![
			sibling("before"),
			sibling("after"),
			entry(),
		])]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &deleted),
			MergeRevision::new(RevisionId::new(2), &reordered),
		];

		let outcome = n_way_merge(&base, &revisions).unwrap();

		assert!(
			outcome.conflicts.iter().any(|conflict| {
				conflict.kind == ConflictKind::DeleteModify && conflict.detail.contains("reordered")
			}),
			"{:?}",
			outcome.conflicts
		);
		assert!(outcome.resolved_tree().is_none());
		assert_eq!(
			outcome
				.tentative_tree()
				.nodes()
				.filter_map(|(_, node)| node.value.as_deref())
				.collect::<Vec<_>>(),
			vec!["before", "after", "payload"],
		);
	}

	#[test]
	fn merge_combines_independent_deletions_under_a_commutative_parent() {
		let commutative_root = |children| {
			normalize(TreeNode::branch("root", children).with_child_order(ChildOrder::Commutative))
		};
		let base = commutative_root(vec![
			TreeNode::leaf("value", "a"),
			TreeNode::leaf("value", "b"),
		]);
		let first = commutative_root(vec![TreeNode::leaf("value", "a")]);
		let second = commutative_root(vec![TreeNode::leaf("value", "b")]);
		let revisions = [
			MergeRevision::new(RevisionId::new(1), &first),
			MergeRevision::new(RevisionId::new(2), &second),
		];

		let outcome = n_way_merge(&base, &revisions).unwrap();

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert!(
			outcome
				.resolved_tree()
				.unwrap()
				.node(outcome.tentative_tree().root())
				.unwrap()
				.children
				.is_empty()
		);
	}

	fn correspondence_root_class(
		base: &NormalizedTree,
		revisions: &[MergeRevision<'_>],
	) -> ClassId {
		NWayCorrespondence::build(base, revisions)
			.unwrap()
			.classes()
			.class_of(RevisionNode::new(RevisionId::BASE, base.root()))
	}
}
