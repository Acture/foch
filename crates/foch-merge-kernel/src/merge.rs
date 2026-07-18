// SPDX-License-Identifier: GPL-3.0-only
//
// Parser-independent adaptation of Mergiraf 0.18.0 `src/merge_3dm.rs`,
// `src/changeset.rs`, and `src/merged_tree.rs` at upstream revision
// e8e13887b85b8cb56b1dc1624c5f94e3d39182b6. foch reconstructs an owned
// semantic tree and typed conflicts instead of rendering source text.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::pcs::merge_order;
use crate::{
	ChildCardinality, ChildOrder, ClassId, ClassMapping, ConflictKind, ConservativeMergePolicy,
	DeleteModifyContext, Matching, MergeOutcome, MergePolicy, MergeTimings, NodeConflictContext,
	NodeId, NormalizedNode, NormalizedTree, PolicyDecision, RevisionClass, RevisionId,
	RevisionNode, SourceSet, StructuralConflict, TreeMatcher, TreeNode,
};

struct Trees<'a> {
	base: &'a NormalizedTree,
	left: &'a NormalizedTree,
	right: &'a NormalizedTree,
}

impl Trees<'_> {
	fn get(&self, revision: RevisionId) -> &NormalizedTree {
		match revision {
			RevisionId::BASE => self.base,
			RevisionId::LEFT => self.left,
			RevisionId::RIGHT => self.right,
			other => panic!("unsupported three-way revision {}", other.get()),
		}
	}

	fn node(&self, node: RevisionNode) -> &NormalizedNode {
		self.get(node.revision).node(node.node).unwrap()
	}
}

#[derive(Clone, Debug)]
struct ClassState {
	selected: RevisionNode,
	sources: SourceSet,
	parent: Option<ClassId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlacementChange {
	Unchanged,
	Reparented,
	Reordered,
}

pub fn three_way_merge(
	base: &NormalizedTree,
	left: &NormalizedTree,
	right: &NormalizedTree,
) -> MergeOutcome {
	three_way_merge_with_policy(base, left, right, &ConservativeMergePolicy)
}

pub fn three_way_merge_with_policy(
	base: &NormalizedTree,
	left: &NormalizedTree,
	right: &NormalizedTree,
	policy: &dyn MergePolicy,
) -> MergeOutcome {
	let trees = Trees { base, left, right };
	let matcher_started = Instant::now();
	let matcher = TreeMatcher::default();
	let base_left = matcher.match_trees(base, left);
	let base_right = matcher.match_trees(base, right);
	let left_right_seed = Matching::compose_through_base(&base_left, &base_right);
	let left_right = matcher.match_trees_with_seed(left, right, Some(&left_right_seed));
	let matcher_ns = nanos(matcher_started.elapsed());

	let pcs_started = Instant::now();
	let mapping =
		ClassMapping::from_matchings(base, left, right, &base_left, &base_right, &left_right);
	let mut conflicts = Vec::new();
	record_match_ambiguities(
		&base_left,
		RevisionId::BASE,
		RevisionId::LEFT,
		&mut conflicts,
	);
	record_match_ambiguities(
		&base_right,
		RevisionId::BASE,
		RevisionId::RIGHT,
		&mut conflicts,
	);
	record_match_ambiguities(
		&left_right,
		RevisionId::LEFT,
		RevisionId::RIGHT,
		&mut conflicts,
	);

	let mut policy_ns = 0;
	let mut states = select_classes(&trees, &mapping, policy, &mut policy_ns, &mut conflicts);
	assign_parents(&trees, &mapping, &mut states, &mut conflicts);
	let root_class = mapping.class_of(RevisionNode::new(RevisionId::BASE, base.root()));
	let mut sources_preorder = Vec::new();
	let mut visiting = BTreeSet::new();
	let mut emitted = BTreeSet::new();
	let root = build_class(
		root_class,
		&trees,
		&mapping,
		&states,
		&mut conflicts,
		&mut sources_preorder,
		&mut visiting,
		&mut emitted,
	);
	for class in states.keys().filter(|class| !emitted.contains(class)) {
		conflicts.push(class_conflict(
			ConflictKind::Policy,
			*class,
			states.get(class).and_then(|state| state.parent),
			&mapping,
			format!(
				"live class {} is unreachable from the merged root",
				class.get()
			),
		));
	}
	let tree = NormalizedTree::from_root(root).expect("merged tree fits the normalized arena");
	let provenance = sources_preorder
		.into_iter()
		.enumerate()
		.map(|(index, sources)| (NodeId::new(index as u32), sources))
		.collect();
	let pcs_ns = nanos(pcs_started.elapsed()).saturating_sub(policy_ns);

	MergeOutcome {
		tentative_tree: tree,
		provenance,
		conflicts,
		timings: MergeTimings {
			matcher_ns,
			pcs_ns,
			policy_ns,
		},
	}
}

fn record_match_ambiguities(
	matching: &Matching,
	left_revision: RevisionId,
	right_revision: RevisionId,
	conflicts: &mut Vec<StructuralConflict>,
) {
	for ambiguity in matching.ambiguities() {
		let revisions = std::iter::once(RevisionNode::new(left_revision, ambiguity.left)).chain(
			ambiguity
				.candidates
				.iter()
				.copied()
				.map(|node| RevisionNode::new(right_revision, node)),
		);
		conflicts.push(StructuralConflict {
			kind: ConflictKind::AmbiguousMatch,
			parent: None,
			base: (left_revision == RevisionId::BASE)
				.then_some(RevisionNode::new(left_revision, ambiguity.left)),
			revisions: SourceSet::new(revisions),
			detail: format!(
				"node {} has {} equally ranked candidates at score {}",
				ambiguity.left.get(),
				ambiguity.candidates.len(),
				ambiguity.score
			),
		});
	}
}

fn select_classes(
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	policy: &dyn MergePolicy,
	policy_ns: &mut u64,
	conflicts: &mut Vec<StructuralConflict>,
) -> BTreeMap<ClassId, ClassState> {
	let mut states = BTreeMap::new();
	for class in mapping.classes() {
		let base = class.get(RevisionId::BASE);
		let left = class.get(RevisionId::LEFT);
		let right = class.get(RevisionId::RIGHT);
		let keep = match (base, left, right) {
			(Some(_), None, None) => false,
			(Some(base_id), Some(present), None) => {
				let base_node = trees.base.node(base_id).unwrap();
				let present_node = trees.left.node(present).unwrap();
				let content_unchanged = base_node.subtree_hash == present_node.subtree_hash;
				let placement = placement_change(trees, mapping, class, RevisionId::LEFT);
				let unchanged = content_unchanged && placement == PlacementChange::Unchanged;
				let covered = delete_modify_covered_by_ancestor(
					trees,
					mapping,
					class,
					RevisionId::RIGHT,
					RevisionId::LEFT,
				);
				let resolved = !unchanged
					&& !covered && policy_resolves_delete_modify(
					policy,
					policy_ns,
					base_node,
					present_node,
					RevisionId::RIGHT,
					RevisionId::LEFT,
					content_unchanged,
					placement,
				);
				if !unchanged && !covered && !resolved {
					conflicts.push(class_conflict(
						ConflictKind::DeleteModify,
						class.id,
						parent_class(trees, mapping, class, RevisionId::BASE),
						mapping,
						delete_change_detail("right", "left", content_unchanged, placement),
					));
				}
				!unchanged
			}
			(Some(base_id), None, Some(present)) => {
				let base_node = trees.base.node(base_id).unwrap();
				let present_node = trees.right.node(present).unwrap();
				let content_unchanged = base_node.subtree_hash == present_node.subtree_hash;
				let placement = placement_change(trees, mapping, class, RevisionId::RIGHT);
				let unchanged = content_unchanged && placement == PlacementChange::Unchanged;
				let covered = delete_modify_covered_by_ancestor(
					trees,
					mapping,
					class,
					RevisionId::LEFT,
					RevisionId::RIGHT,
				);
				let resolved = !unchanged
					&& !covered && policy_resolves_delete_modify(
					policy,
					policy_ns,
					base_node,
					present_node,
					RevisionId::LEFT,
					RevisionId::RIGHT,
					content_unchanged,
					placement,
				);
				if !unchanged && !covered && !resolved {
					conflicts.push(class_conflict(
						ConflictKind::DeleteModify,
						class.id,
						parent_class(trees, mapping, class, RevisionId::BASE),
						mapping,
						delete_change_detail("left", "right", content_unchanged, placement),
					));
				}
				!unchanged
			}
			_ => true,
		};
		if !keep {
			continue;
		}
		let selected = select_revision_node(trees, class, mapping, policy, policy_ns, conflicts);
		let sources = SourceSet::new(
			class
				.members
				.iter()
				.map(|(revision, node)| RevisionNode::new(*revision, *node)),
		);
		states.insert(
			class.id,
			ClassState {
				selected,
				sources,
				parent: None,
			},
		);
	}
	states
}

fn delete_modify_covered_by_ancestor(
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	class: &RevisionClass,
	deleted_revision: RevisionId,
	present_revision: RevisionId,
) -> bool {
	let Some(base) = class.get(RevisionId::BASE) else {
		return false;
	};
	let mut ancestor = trees.base.node(base).unwrap().parent;
	while let Some(node) = ancestor {
		let ancestor_class =
			mapping.class(mapping.class_of(RevisionNode::new(RevisionId::BASE, node)));
		if ancestor_class.get(deleted_revision).is_none()
			&& ancestor_class.get(present_revision).is_some()
		{
			return true;
		}
		ancestor = trees.base.node(node).unwrap().parent;
	}
	false
}

#[allow(clippy::too_many_arguments)]
fn policy_resolves_delete_modify(
	policy: &dyn MergePolicy,
	policy_ns: &mut u64,
	base: &NormalizedNode,
	present: &NormalizedNode,
	deleted_revision: RevisionId,
	present_revision: RevisionId,
	content_unchanged: bool,
	placement: PlacementChange,
) -> bool {
	let decision = measure_policy(policy_ns, || {
		policy.resolve_delete_modify(DeleteModifyContext {
			base,
			present,
			deleted_revision,
			present_revision,
			content_changed: !content_unchanged,
			reparented: placement == PlacementChange::Reparented,
			reordered: placement == PlacementChange::Reordered,
		})
	});
	decision == PolicyDecision::Resolved
}

fn delete_change_detail(
	deleted_revision: &str,
	present_revision: &str,
	content_unchanged: bool,
	placement: PlacementChange,
) -> String {
	let change = match (content_unchanged, placement) {
		(false, PlacementChange::Unchanged) => "modified",
		(false, PlacementChange::Reparented) => "modified and reparented",
		(false, PlacementChange::Reordered) => "modified and reordered",
		(true, PlacementChange::Reparented) => "reparented",
		(true, PlacementChange::Reordered) => "reordered",
		(true, PlacementChange::Unchanged) => unreachable!("unchanged delete is not a conflict"),
	};
	format!("{deleted_revision} deleted a subtree {change} by {present_revision}")
}

fn placement_change(
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	class: &RevisionClass,
	present_revision: RevisionId,
) -> PlacementChange {
	let base_parent = parent_class(trees, mapping, class, RevisionId::BASE);
	let present_parent = parent_class(trees, mapping, class, present_revision);
	if base_parent != present_parent {
		return PlacementChange::Reparented;
	}
	let Some(parent) = base_parent else {
		return PlacementChange::Unchanged;
	};
	if relative_order_changed(trees, mapping, class, present_revision, parent) {
		PlacementChange::Reordered
	} else {
		PlacementChange::Unchanged
	}
}

fn relative_order_changed(
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	class: &RevisionClass,
	present_revision: RevisionId,
	parent: ClassId,
) -> bool {
	let parent_class = mapping.class(parent);
	let (Some(base_parent), Some(present_parent)) = (
		parent_class.get(RevisionId::BASE),
		parent_class.get(present_revision),
	) else {
		return false;
	};
	let base_node = trees.base.node(base_parent).unwrap();
	let present_node = trees.get(present_revision).node(present_parent).unwrap();
	if base_node.child_cardinality == ChildCardinality::ExactlyOne
		|| present_node.child_cardinality == ChildCardinality::ExactlyOne
		|| base_node.child_order != ChildOrder::Ordered
		|| present_node.child_order != ChildOrder::Ordered
	{
		return false;
	}

	let base_children = base_node
		.children
		.iter()
		.map(|child| mapping.class_of(RevisionNode::new(RevisionId::BASE, *child)))
		.collect::<Vec<_>>();
	let present_children = present_node
		.children
		.iter()
		.map(|child| mapping.class_of(RevisionNode::new(present_revision, *child)))
		.collect::<Vec<_>>();
	let Some(base_index) = base_children.iter().position(|child| *child == class.id) else {
		return false;
	};
	let Some(present_index) = present_children.iter().position(|child| *child == class.id) else {
		return false;
	};
	let present_positions = present_children
		.iter()
		.enumerate()
		.map(|(index, child)| (*child, index))
		.collect::<BTreeMap<_, _>>();
	base_children
		.iter()
		.enumerate()
		.filter(|(_, sibling)| **sibling != class.id)
		.any(|(index, sibling)| {
			present_positions
				.get(sibling)
				.is_some_and(|present| (index < base_index) != (*present < present_index))
		})
}

fn select_revision_node(
	trees: &Trees<'_>,
	class: &RevisionClass,
	mapping: &ClassMapping,
	policy: &dyn MergePolicy,
	policy_ns: &mut u64,
	conflicts: &mut Vec<StructuralConflict>,
) -> RevisionNode {
	let base = class
		.get(RevisionId::BASE)
		.map(|node| RevisionNode::new(RevisionId::BASE, node));
	let left = class
		.get(RevisionId::LEFT)
		.map(|node| RevisionNode::new(RevisionId::LEFT, node));
	let right = class
		.get(RevisionId::RIGHT)
		.map(|node| RevisionNode::new(RevisionId::RIGHT, node));
	match (base, left, right) {
		(_, Some(left), Some(right)) if shallow_eq(trees.node(left), trees.node(right)) => left,
		(Some(base), Some(left), Some(right)) if shallow_eq(trees.node(base), trees.node(left)) => {
			right
		}
		(Some(base), Some(left), Some(right))
			if shallow_eq(trees.node(base), trees.node(right)) =>
		{
			left
		}
		(Some(base), Some(left), Some(right)) => {
			if let Some(selected) = select_divergent_node(
				trees,
				policy,
				policy_ns,
				ConflictKind::Policy,
				Some(base),
				Some(left),
				Some(right),
			) {
				return selected;
			}
			conflicts.push(class_conflict(
				ConflictKind::Policy,
				class.id,
				None,
				mapping,
				"left and right changed the same node differently".to_string(),
			));
			right
		}
		(None, Some(left), Some(right)) => {
			if let Some(selected) = select_divergent_node(
				trees,
				policy,
				policy_ns,
				ConflictKind::InsertInsert,
				None,
				Some(left),
				Some(right),
			) {
				return selected;
			}
			conflicts.push(class_conflict(
				ConflictKind::InsertInsert,
				class.id,
				None,
				mapping,
				"left and right inserted different content into one matched class".to_string(),
			));
			right
		}
		(_, Some(left), None) => left,
		(_, None, Some(right)) => right,
		(Some(base), None, None) => base,
		(None, None, None) => unreachable!("revision class is never empty"),
	}
}

#[allow(clippy::too_many_arguments)]
fn select_divergent_node(
	trees: &Trees<'_>,
	policy: &dyn MergePolicy,
	policy_ns: &mut u64,
	kind: ConflictKind,
	base: Option<RevisionNode>,
	left: Option<RevisionNode>,
	right: Option<RevisionNode>,
) -> Option<RevisionNode> {
	let selected_revision = measure_policy(policy_ns, || {
		policy.select_divergent_node(NodeConflictContext {
			kind,
			base: base.map(|node| trees.node(node)),
			left: left.map(|node| trees.node(node)),
			right: right.map(|node| trees.node(node)),
		})
	});
	match selected_revision {
		Some(RevisionId::BASE) => base,
		Some(RevisionId::LEFT) => left,
		Some(RevisionId::RIGHT) => right,
		_ => None,
	}
}

fn shallow_eq(left: &NormalizedNode, right: &NormalizedNode) -> bool {
	left.kind == right.kind
		&& left.value == right.value
		&& left.anchor == right.anchor
		&& left.signature == right.signature
		&& left.child_order == right.child_order
		&& left.child_cardinality == right.child_cardinality
}

fn assign_parents(
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	states: &mut BTreeMap<ClassId, ClassState>,
	conflicts: &mut Vec<StructuralConflict>,
) {
	let class_ids = states.keys().copied().collect::<Vec<_>>();
	for class_id in class_ids {
		let class = mapping.class(class_id);
		let base = parent_class(trees, mapping, class, RevisionId::BASE);
		let left = parent_class(trees, mapping, class, RevisionId::LEFT);
		let right = parent_class(trees, mapping, class, RevisionId::RIGHT);
		let parent = match (
			class.get(RevisionId::BASE),
			class.get(RevisionId::LEFT),
			class.get(RevisionId::RIGHT),
		) {
			(Some(_), Some(_), None) | (None, Some(_), None) => left,
			(Some(_), None, Some(_)) | (None, None, Some(_)) => right,
			_ if left == right => left,
			_ if left == base => right,
			_ if right == base => left,
			_ if left.is_none() => right,
			_ if right.is_none() => left,
			_ => {
				conflicts.push(class_conflict(
					ConflictKind::MoveMove,
					class_id,
					base,
					mapping,
					"left and right moved the same node to different parents".to_string(),
				));
				base.or(right).or(left)
			}
		};
		states.get_mut(&class_id).unwrap().parent = parent;
	}
}

fn parent_class(
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	class: &RevisionClass,
	revision: RevisionId,
) -> Option<ClassId> {
	let node = class.get(revision)?;
	let parent = trees.get(revision).node(node).unwrap().parent?;
	Some(mapping.class_of(RevisionNode::new(revision, parent)))
}

#[allow(clippy::too_many_arguments)]
fn build_class(
	class_id: ClassId,
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	states: &BTreeMap<ClassId, ClassState>,
	conflicts: &mut Vec<StructuralConflict>,
	sources_preorder: &mut Vec<SourceSet>,
	visiting: &mut BTreeSet<ClassId>,
	emitted: &mut BTreeSet<ClassId>,
) -> TreeNode {
	let state = states
		.get(&class_id)
		.expect("merged root and children have live class states");
	if !visiting.insert(class_id) {
		conflicts.push(class_conflict(
			ConflictKind::MoveMove,
			class_id,
			state.parent,
			mapping,
			"merged parent relation contains a cycle".to_string(),
		));
		return clone_revision_subtree(
			trees,
			state.selected,
			Some(&state.sources),
			sources_preorder,
		);
	}
	emitted.insert(class_id);
	sources_preorder.push(state.sources.clone());
	let selected = trees.node(state.selected);
	let children = merged_children(class_id, selected, trees, mapping, states, conflicts)
		.into_iter()
		.map(|child| {
			build_class(
				child,
				trees,
				mapping,
				states,
				conflicts,
				sources_preorder,
				visiting,
				emitted,
			)
		})
		.collect();
	visiting.remove(&class_id);
	TreeNode {
		kind: selected.kind.clone(),
		value: selected.value.clone(),
		anchor: selected.anchor.clone(),
		signature: selected.signature.clone(),
		child_order: selected.child_order,
		child_cardinality: selected.child_cardinality,
		children,
	}
}

fn clone_revision_subtree(
	trees: &Trees<'_>,
	source: RevisionNode,
	root_sources: Option<&SourceSet>,
	sources_preorder: &mut Vec<SourceSet>,
) -> TreeNode {
	let node = trees.node(source);
	sources_preorder.push(
		root_sources
			.cloned()
			.unwrap_or_else(|| SourceSet::new([source])),
	);
	let children = node
		.children
		.iter()
		.map(|child| {
			clone_revision_subtree(
				trees,
				RevisionNode::new(source.revision, *child),
				None,
				sources_preorder,
			)
		})
		.collect();
	TreeNode {
		kind: node.kind.clone(),
		value: node.value.clone(),
		anchor: node.anchor.clone(),
		signature: node.signature.clone(),
		child_order: node.child_order,
		child_cardinality: node.child_cardinality,
		children,
	}
}

fn merged_children(
	parent: ClassId,
	selected: &NormalizedNode,
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	states: &BTreeMap<ClassId, ClassState>,
	conflicts: &mut Vec<StructuralConflict>,
) -> Vec<ClassId> {
	let class = mapping.class(parent);
	if selected.child_cardinality == ChildCardinality::ExactlyOne {
		return merge_exactly_one_child(parent, trees, mapping, states, conflicts);
	}
	let base = child_sequence(trees, mapping, states, class, RevisionId::BASE, parent);
	let left = child_sequence(trees, mapping, states, class, RevisionId::LEFT, parent);
	let right = child_sequence(trees, mapping, states, class, RevisionId::RIGHT, parent);
	if selected.child_order == ChildOrder::Commutative {
		return commutative_children(
			parent, &base, &left, &right, trees, mapping, states, conflicts,
		);
	}
	match merge_order(parent, &base, &left, &right) {
		Ok(children) => children,
		Err(cycle) => {
			conflicts.push(class_conflict(
				ConflictKind::Ordering,
				parent,
				states.get(&parent).and_then(|state| state.parent),
				mapping,
				format!(
					"PCS constraints contain a cycle across {} nodes and {} triples",
					cycle.remaining.len(),
					cycle.triples.len()
				),
			));
			fallback_order(&base, &left, &right)
		}
	}
}

fn merge_exactly_one_child(
	parent: ClassId,
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	states: &BTreeMap<ClassId, ClassState>,
	conflicts: &mut Vec<StructuralConflict>,
) -> Vec<ClassId> {
	let class = mapping.class(parent);
	let base = slot_child_class(trees, mapping, class, RevisionId::BASE);
	let left = slot_child_class(trees, mapping, class, RevisionId::LEFT);
	let right = slot_child_class(trees, mapping, class, RevisionId::RIGHT);
	let selected = match (base, left, right) {
		(_, Some(left), Some(right)) if left == right => left,
		(Some(base), Some(left), Some(right)) if left == base => right,
		(Some(base), Some(left), Some(right)) if right == base => left,
		(Some(base), Some(left), Some(right)) => {
			conflicts.push(value_slot_conflict(
				parent,
				Some(base),
				Some(left),
				Some(right),
				mapping,
				"left and right replaced the same required value slot differently",
			));
			right
		}
		(None, Some(left), Some(right)) => {
			conflicts.push(value_slot_conflict(
				parent,
				None,
				Some(left),
				Some(right),
				mapping,
				"left and right inserted different values into the same required slot",
			));
			right
		}
		(_, Some(left), None) => left,
		(_, None, Some(right)) => right,
		(Some(base), None, None) => base,
		(None, None, None) => unreachable!("a live exactly-one parent has a value child"),
	};
	let selected = states
		.contains_key(&selected)
		.then_some(selected)
		.or_else(|| {
			[right, left, base]
				.into_iter()
				.flatten()
				.find(|candidate| states.contains_key(candidate))
		})
		.expect("a live exactly-one parent retains one live value class");
	vec![selected]
}

fn slot_child_class(
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	class: &RevisionClass,
	revision: RevisionId,
) -> Option<ClassId> {
	let node = class.get(revision)?;
	let [child] = trees.get(revision).node(node).unwrap().children.as_slice() else {
		unreachable!("normalized exactly-one nodes have one child")
	};
	Some(mapping.class_of(RevisionNode::new(revision, *child)))
}

fn value_slot_conflict(
	parent: ClassId,
	base: Option<ClassId>,
	left: Option<ClassId>,
	right: Option<ClassId>,
	mapping: &ClassMapping,
	detail: &str,
) -> StructuralConflict {
	let revisions = [base, left, right]
		.into_iter()
		.flatten()
		.flat_map(|class| {
			mapping
				.class(class)
				.members
				.iter()
				.map(|(revision, node)| RevisionNode::new(*revision, *node))
				.collect::<Vec<_>>()
		})
		.collect::<Vec<_>>();
	StructuralConflict {
		kind: ConflictKind::ValueSlot,
		parent: Some(parent),
		base: base.and_then(|class| {
			mapping
				.class(class)
				.get(RevisionId::BASE)
				.map(|node| RevisionNode::new(RevisionId::BASE, node))
		}),
		revisions: SourceSet::new(revisions),
		detail: detail.to_string(),
	}
}

fn child_sequence(
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	states: &BTreeMap<ClassId, ClassState>,
	class: &RevisionClass,
	revision: RevisionId,
	merged_parent: ClassId,
) -> Vec<ClassId> {
	let Some(node) = class.get(revision) else {
		return Vec::new();
	};
	trees
		.get(revision)
		.node(node)
		.unwrap()
		.children
		.iter()
		.map(|child| mapping.class_of(RevisionNode::new(revision, *child)))
		.filter(|child| {
			states
				.get(child)
				.is_some_and(|state| state.parent == Some(merged_parent))
		})
		.collect()
}

#[allow(clippy::too_many_arguments)]
fn commutative_children(
	parent: ClassId,
	base: &[ClassId],
	left: &[ClassId],
	right: &[ClassId],
	trees: &Trees<'_>,
	mapping: &ClassMapping,
	states: &BTreeMap<ClassId, ClassState>,
	conflicts: &mut Vec<StructuralConflict>,
) -> Vec<ClassId> {
	let mut children = base
		.iter()
		.chain(left)
		.chain(right)
		.copied()
		.collect::<BTreeSet<_>>()
		.into_iter()
		.collect::<Vec<_>>();
	children.sort_by_key(|class| {
		let node = trees.node(states[class].selected);
		(
			node.signature.clone(),
			node.anchor.clone(),
			node.kind.clone(),
			*class,
		)
	});
	let mut signatures = BTreeMap::new();
	for child in &children {
		let node = trees.node(states[child].selected);
		let Some(signature) = &node.signature else {
			continue;
		};
		if let Some(previous) = signatures.insert(signature.clone(), *child)
			&& previous != *child
		{
			conflicts.push(class_conflict(
				ConflictKind::DuplicateSignature,
				*child,
				Some(parent),
				mapping,
				format!("duplicate commutative signature `{signature}`"),
			));
		}
	}
	children
}

fn fallback_order(base: &[ClassId], left: &[ClassId], right: &[ClassId]) -> Vec<ClassId> {
	let mut result = Vec::new();
	for child in base.iter().chain(left).chain(right) {
		if !result.contains(child) {
			result.push(*child);
		}
	}
	result
}

fn class_conflict(
	kind: ConflictKind,
	class: ClassId,
	parent: Option<ClassId>,
	mapping: &ClassMapping,
	detail: String,
) -> StructuralConflict {
	let revision_class = mapping.class(class);
	StructuralConflict {
		kind,
		parent,
		base: revision_class
			.get(RevisionId::BASE)
			.map(|node| RevisionNode::new(RevisionId::BASE, node)),
		revisions: SourceSet::new(
			revision_class
				.members
				.iter()
				.map(|(revision, node)| RevisionNode::new(*revision, *node)),
		),
		detail,
	}
}

fn nanos(duration: Duration) -> u64 {
	u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn measure_policy<T>(policy_ns: &mut u64, operation: impl FnOnce() -> T) -> T {
	let started = Instant::now();
	let result = operation();
	*policy_ns = policy_ns.saturating_add(nanos(started.elapsed()));
	result
}

#[cfg(test)]
mod tests {
	use crate::TreeNode;

	use super::*;

	fn root(children: Vec<TreeNode>) -> NormalizedTree {
		NormalizedTree::from_root(TreeNode::branch("root", children)).unwrap()
	}

	fn slot(value: TreeNode) -> NormalizedTree {
		NormalizedTree::from_root(
			TreeNode::branch("slot", vec![value])
				.with_child_cardinality(ChildCardinality::ExactlyOne),
		)
		.unwrap()
	}

	fn slot_child(tree: &NormalizedTree) -> &NormalizedNode {
		let root = tree.node(tree.root()).unwrap();
		let [child] = root.children.as_slice() else {
			panic!("exactly-one output must retain one child")
		};
		tree.node(*child).unwrap()
	}

	fn values(tree: &NormalizedTree) -> Vec<String> {
		tree.nodes()
			.filter_map(|(_, node)| node.value.clone())
			.collect()
	}

	fn anchored_container(name: &str, children: Vec<TreeNode>) -> TreeNode {
		TreeNode::branch("container", children).with_anchor("container", name)
	}

	fn movable_entry() -> TreeNode {
		TreeNode::leaf("entry", "payload").with_anchor("entry", "movable")
	}

	fn sibling(name: &str) -> TreeNode {
		TreeNode::leaf("entry", name).with_anchor("entry", name)
	}

	fn moved_entry_parent(tree: &NormalizedTree) -> String {
		let (_, entry) = tree
			.nodes()
			.find(|(_, node)| node.value.as_deref() == Some("payload"))
			.expect("tentative tree retains the moved entry");
		let parent = tree
			.node(entry.parent.expect("entry remains attached"))
			.unwrap();
		parent
			.anchor
			.as_ref()
			.expect("container has a semantic anchor")
			.value
			.clone()
	}

	struct EditWins;

	impl MergePolicy for EditWins {
		fn resolve_delete_modify(&self, context: DeleteModifyContext<'_>) -> PolicyDecision {
			if context.content_changed && !context.reparented && !context.reordered {
				PolicyDecision::Resolved
			} else {
				PolicyDecision::Unresolved
			}
		}
	}

	struct RightLeafWins;

	impl MergePolicy for RightLeafWins {
		fn select_divergent_node(&self, context: NodeConflictContext<'_>) -> Option<RevisionId> {
			(context.left.is_some_and(|node| node.children.is_empty())
				&& context.right.is_some_and(|node| node.children.is_empty()))
			.then_some(RevisionId::RIGHT)
		}
	}

	#[test]
	fn independent_ordered_insertions_are_amalgamated() {
		let base = root(vec![TreeNode::leaf("value", "base")]);
		let left = root(vec![
			TreeNode::leaf("value", "left"),
			TreeNode::leaf("value", "base"),
		]);
		let right = root(vec![
			TreeNode::leaf("value", "base"),
			TreeNode::leaf("value", "right"),
		]);

		let outcome = three_way_merge(&base, &left, &right);

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert_eq!(
			values(outcome.resolved_tree().unwrap()),
			vec!["left", "base", "right"]
		);
	}

	#[test]
	fn exactly_one_block_values_merge_their_contents_in_one_slot() {
		let value = |field: &str| {
			TreeNode::branch(
				"block",
				vec![TreeNode::leaf(format!("field:{field}"), "yes")],
			)
		};
		let base = slot(value("base_only"));
		let left = slot(value("left_only"));
		let right = slot(value("right_only"));

		let outcome = three_way_merge(&base, &left, &right);

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		let tree = outcome.resolved_tree().unwrap();
		let block = slot_child(tree);
		assert_eq!(block.kind, "block");
		assert_eq!(
			block
				.children
				.iter()
				.map(|child| tree.node(*child).unwrap().kind.as_str())
				.collect::<Vec<_>>(),
			vec!["field:left_only", "field:right_only"]
		);
	}

	#[test]
	fn one_sided_required_slot_type_replacement_wins() {
		let base = slot(TreeNode::leaf("scalar", "old"));
		let left = slot(TreeNode::branch(
			"block",
			vec![TreeNode::leaf("field:new", "yes")],
		));
		let right = base.clone();

		let outcome = three_way_merge(&base, &left, &right);

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert_eq!(slot_child(outcome.resolved_tree().unwrap()).kind, "block");
	}

	#[test]
	fn divergent_required_slot_type_replacements_conflict_without_breaking_cardinality() {
		let base = slot(TreeNode::leaf("scalar", "old"));
		let left = slot(TreeNode::branch(
			"block",
			vec![TreeNode::leaf("field:left", "yes")],
		));
		let right = slot(TreeNode::leaf("number", "1"));

		let outcome = three_way_merge(&base, &left, &right);

		assert!(
			outcome
				.conflicts
				.iter()
				.any(|conflict| conflict.kind == ConflictKind::ValueSlot),
			"{:?}",
			outcome.conflicts
		);
		assert!(outcome.resolved_tree().is_none());
		assert_eq!(slot_child(outcome.tentative_tree()).kind, "number");
	}

	#[test]
	fn one_sided_scalar_edit_wins_over_unchanged_revision() {
		let base = root(vec![TreeNode::leaf("value", "old")]);
		let left = root(vec![TreeNode::leaf("value", "new")]);
		let right = base.clone();

		let outcome = three_way_merge(&base, &left, &right);

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert_eq!(values(outcome.resolved_tree().unwrap()), vec!["new"]);
	}

	#[test]
	fn delete_modify_is_an_explicit_conflict() {
		let base = root(vec![
			TreeNode::branch("entry", vec![TreeNode::leaf("value", "old")])
				.with_anchor("entry", "a"),
		]);
		let left = root(Vec::new());
		let right = root(vec![
			TreeNode::branch("entry", vec![TreeNode::leaf("value", "new")])
				.with_anchor("entry", "a"),
		]);

		let outcome = three_way_merge(&base, &left, &right);

		assert_eq!(
			outcome
				.conflicts
				.iter()
				.filter(|conflict| conflict.kind == ConflictKind::DeleteModify)
				.count(),
			1,
			"a deleted ancestor subsumes descendant delete/modify reports: {:?}",
			outcome.conflicts
		);
		assert!(outcome.resolved_tree().is_none());
		assert_eq!(values(outcome.tentative_tree()), vec!["new"]);
	}

	#[test]
	fn policy_can_resolve_content_edit_over_delete() {
		let base = root(vec![
			TreeNode::branch("entry", vec![TreeNode::leaf("value", "old")])
				.with_anchor("entry", "a"),
		]);
		let left = root(Vec::new());
		let right = root(vec![
			TreeNode::branch("entry", vec![TreeNode::leaf("value", "new")])
				.with_anchor("entry", "a"),
		]);

		let outcome = three_way_merge_with_policy(&base, &left, &right, &EditWins);

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert_eq!(values(outcome.resolved_tree().unwrap()), vec!["new"]);
	}

	#[test]
	fn policy_can_select_a_divergent_leaf_revision() {
		let value = |value| TreeNode::leaf("value", value).with_anchor("field", "same");
		let base = root(vec![value("old")]);
		let left = root(vec![value("left")]);
		let right = root(vec![value("right")]);

		let outcome = three_way_merge_with_policy(&base, &left, &right, &RightLeafWins);

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert_eq!(values(outcome.resolved_tree().unwrap()), vec!["right"]);
	}

	#[test]
	fn left_delete_right_reparent_is_an_explicit_conflict_without_silent_drop() {
		let base = root(vec![
			anchored_container("a", vec![movable_entry()]),
			anchored_container("b", Vec::new()),
		]);
		let left = root(vec![
			anchored_container("a", Vec::new()),
			anchored_container("b", Vec::new()),
		]);
		let right = root(vec![
			anchored_container("a", Vec::new()),
			anchored_container("b", vec![movable_entry()]),
		]);

		let outcome = three_way_merge(&base, &left, &right);

		assert!(
			outcome
				.conflicts
				.iter()
				.any(|conflict| conflict.kind == ConflictKind::DeleteModify),
			"{:?}",
			outcome.conflicts
		);
		assert!(outcome.resolved_tree().is_none());
		assert_eq!(moved_entry_parent(outcome.tentative_tree()), "b");
	}

	#[test]
	fn right_delete_left_reparent_is_an_explicit_conflict_without_silent_drop() {
		let base = root(vec![
			anchored_container("a", vec![movable_entry()]),
			anchored_container("b", Vec::new()),
		]);
		let left = root(vec![
			anchored_container("a", Vec::new()),
			anchored_container("b", vec![movable_entry()]),
		]);
		let right = root(vec![
			anchored_container("a", Vec::new()),
			anchored_container("b", Vec::new()),
		]);

		let outcome = three_way_merge(&base, &left, &right);

		assert!(
			outcome
				.conflicts
				.iter()
				.any(|conflict| conflict.kind == ConflictKind::DeleteModify),
			"{:?}",
			outcome.conflicts
		);
		assert!(outcome.resolved_tree().is_none());
		assert_eq!(moved_entry_parent(outcome.tentative_tree()), "b");
	}

	#[test]
	fn delete_reorder_under_ordered_parent_is_an_explicit_conflict() {
		let base = root(vec![anchored_container(
			"a",
			vec![sibling("before"), movable_entry(), sibling("after")],
		)]);
		let left = root(vec![anchored_container(
			"a",
			vec![sibling("before"), sibling("after")],
		)]);
		let right = root(vec![anchored_container(
			"a",
			vec![sibling("before"), sibling("after"), movable_entry()],
		)]);

		let outcome = three_way_merge(&base, &left, &right);

		assert!(
			outcome.conflicts.iter().any(|conflict| {
				conflict.kind == ConflictKind::DeleteModify && conflict.detail.contains("reordered")
			}),
			"{:?}",
			outcome.conflicts
		);
		assert!(outcome.resolved_tree().is_none());
		assert_eq!(moved_entry_parent(outcome.tentative_tree()), "a");
		assert_eq!(
			values(outcome.tentative_tree()),
			vec!["before", "after", "payload"]
		);
	}

	#[test]
	fn delete_permutation_under_commutative_parent_is_not_a_move_conflict() {
		let container =
			|children| anchored_container("a", children).with_child_order(ChildOrder::Commutative);
		let base = root(vec![container(vec![movable_entry(), sibling("other")])]);
		let left = root(vec![container(vec![sibling("other")])]);
		let right = root(vec![container(vec![sibling("other"), movable_entry()])]);

		let outcome = three_way_merge(&base, &left, &right);

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert!(
			!values(outcome.resolved_tree().unwrap())
				.iter()
				.any(|value| value == "payload")
		);
	}

	#[test]
	fn incompatible_two_sided_scalar_edits_conflict() {
		let base = root(vec![TreeNode::leaf("value", "old")]);
		let left = root(vec![TreeNode::leaf("value", "left")]);
		let right = root(vec![TreeNode::leaf("value", "right")]);

		let outcome = three_way_merge(&base, &left, &right);

		assert!(
			outcome
				.conflicts
				.iter()
				.any(|conflict| conflict.kind == ConflictKind::Policy)
		);
		assert!(outcome.resolved_tree().is_none());
		assert!(!outcome.tentative_tree().is_empty());
	}

	#[test]
	fn independent_deletions_under_commutative_parent_are_combined() {
		let commutative_root = |children| {
			NormalizedTree::from_root(
				TreeNode::branch("root", children).with_child_order(ChildOrder::Commutative),
			)
			.unwrap()
		};
		let base = commutative_root(vec![
			TreeNode::leaf("value", "a"),
			TreeNode::leaf("value", "b"),
		]);
		let left = commutative_root(vec![TreeNode::leaf("value", "a")]);
		let right = commutative_root(vec![TreeNode::leaf("value", "b")]);

		let outcome = three_way_merge(&base, &left, &right);

		assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
		assert!(values(outcome.resolved_tree().unwrap()).is_empty());
	}
}
