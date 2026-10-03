use serde::{Deserialize, Serialize};

use crate::merge::kernel::{
	ChildOrder, Matching, NodeId, NormalizedNode, NormalizedTree, RevisionId, RevisionNode,
	SemanticKey, SubtreeHash,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct MergeInputId {
	pub revision: RevisionId,
	pub root_hash: SubtreeHash,
}

impl MergeInputId {
	pub fn from_tree(revision: RevisionId, tree: &NormalizedTree) -> Self {
		Self {
			revision,
			root_hash: tree
				.node(tree.root())
				.expect("a normalized tree always contains its root")
				.subtree_hash,
		}
	}
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NodeIdentity {
	pub anchor: Option<SemanticKey>,
	pub value: Option<String>,
}

impl NodeIdentity {
	fn from_node(node: &NormalizedNode) -> Self {
		Self {
			anchor: node.anchor.clone(),
			value: node.value.clone(),
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DeltaNodeRef {
	pub source: RevisionNode,
	pub base: Option<RevisionNode>,
}

impl DeltaNodeRef {
	fn from_revision(revision: RevisionId, node: NodeId, matching: &Matching) -> Self {
		Self {
			source: RevisionNode::new(revision, node),
			base: matching
				.get_from_right(node)
				.map(|base| RevisionNode::new(RevisionId::BASE, base)),
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DeltaNodeMatch {
	pub base: RevisionNode,
	pub revision: RevisionNode,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Tombstone {
	pub deleted: RevisionNode,
	pub deleted_by: RevisionId,
	pub former_parent: Option<RevisionNode>,
	pub subtree_hash: SubtreeHash,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DeltaOperation {
	Insert {
		node: RevisionNode,
		parent: Option<RevisionNode>,
		position: usize,
		subtree_hash: SubtreeHash,
	},
	Delete {
		tombstone: Tombstone,
	},
	Update {
		base: RevisionNode,
		revision: RevisionNode,
	},
	Move {
		base: RevisionNode,
		revision: RevisionNode,
		from_parent: Option<RevisionNode>,
		to_parent: Option<RevisionNode>,
	},
	Rename {
		base: RevisionNode,
		revision: RevisionNode,
		from: NodeIdentity,
		to: NodeIdentity,
	},
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct OrderingFact {
	pub parent: DeltaNodeRef,
	pub before: DeltaNodeRef,
	pub after: DeltaNodeRef,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RevisionDelta {
	pub base: MergeInputId,
	pub revision: MergeInputId,
	pub matches: Vec<DeltaNodeMatch>,
	pub operations: Vec<DeltaOperation>,
	pub ordering: Vec<OrderingFact>,
}

impl RevisionDelta {
	pub fn between(
		base: &NormalizedTree,
		revision: RevisionId,
		revision_tree: &NormalizedTree,
		matching: &Matching,
	) -> Self {
		assert_ne!(
			revision,
			RevisionId::BASE,
			"a revision delta requires a non-base revision"
		);

		let matches = matching
			.records()
			.map(|record| DeltaNodeMatch {
				base: RevisionNode::new(RevisionId::BASE, record.left),
				revision: RevisionNode::new(revision, record.right),
			})
			.collect();
		let mut operations = Vec::new();
		for (base_id, base_node) in base.nodes() {
			let Some(revision_id) = matching.get_from_left(base_id) else {
				if is_unmatched_subtree_root(base_node.parent, matching, true) {
					operations.push(DeltaOperation::Delete {
						tombstone: Tombstone {
							deleted: RevisionNode::new(RevisionId::BASE, base_id),
							deleted_by: revision,
							former_parent: base_node
								.parent
								.map(|parent| RevisionNode::new(RevisionId::BASE, parent)),
							subtree_hash: base_node.subtree_hash,
						},
					});
				}
				continue;
			};

			let revision_node = revision_tree
				.node(revision_id)
				.expect("matching points to a node in the revision tree");
			let base_ref = RevisionNode::new(RevisionId::BASE, base_id);
			let revision_ref = RevisionNode::new(revision, revision_id);
			if identity_changed(base_node, revision_node) {
				operations.push(DeltaOperation::Rename {
					base: base_ref,
					revision: revision_ref,
					from: NodeIdentity::from_node(base_node),
					to: NodeIdentity::from_node(revision_node),
				});
			}
			if content_changed(base_node, revision_node) {
				operations.push(DeltaOperation::Update {
					base: base_ref,
					revision: revision_ref,
				});
			}

			let expected_parent = base_node
				.parent
				.and_then(|parent| matching.get_from_left(parent));
			if revision_node.parent != expected_parent {
				operations.push(DeltaOperation::Move {
					base: base_ref,
					revision: revision_ref,
					from_parent: base_node
						.parent
						.map(|parent| RevisionNode::new(RevisionId::BASE, parent)),
					to_parent: revision_node
						.parent
						.map(|parent| RevisionNode::new(revision, parent)),
				});
			}
		}

		for (revision_id, revision_node) in revision_tree.nodes() {
			if matching.get_from_right(revision_id).is_some()
				|| !is_unmatched_subtree_root(revision_node.parent, matching, false)
			{
				continue;
			}
			operations.push(DeltaOperation::Insert {
				node: RevisionNode::new(revision, revision_id),
				parent: revision_node
					.parent
					.map(|parent| RevisionNode::new(revision, parent)),
				position: sibling_position(revision_tree, revision_id),
				subtree_hash: revision_node.subtree_hash,
			});
		}

		let ordering = ordering_facts(revision, base, revision_tree, matching);
		Self {
			base: MergeInputId::from_tree(RevisionId::BASE, base),
			revision: MergeInputId::from_tree(revision, revision_tree),
			matches,
			operations,
			ordering,
		}
	}

	#[cfg(test)]
	pub fn tombstones(&self) -> impl Iterator<Item = &Tombstone> {
		self.operations
			.iter()
			.filter_map(|operation| match operation {
				DeltaOperation::Delete { tombstone } => Some(tombstone),
				_ => None,
			})
	}
}

fn is_unmatched_subtree_root(parent: Option<NodeId>, matching: &Matching, base_side: bool) -> bool {
	let Some(parent) = parent else {
		return true;
	};
	if base_side {
		matching.get_from_left(parent).is_some()
	} else {
		matching.get_from_right(parent).is_some()
	}
}

fn identity_changed(base: &NormalizedNode, revision: &NormalizedNode) -> bool {
	base.anchor != revision.anchor
		|| (!base.children.is_empty()
			&& !revision.children.is_empty()
			&& base.value != revision.value)
}

fn content_changed(base: &NormalizedNode, revision: &NormalizedNode) -> bool {
	base.kind != revision.kind
		|| (base.children.is_empty()
			&& revision.children.is_empty()
			&& base.value != revision.value)
		|| base.signature != revision.signature
		|| base.child_order != revision.child_order
		|| base.child_cardinality != revision.child_cardinality
}

fn sibling_position(tree: &NormalizedTree, node: NodeId) -> usize {
	let Some(parent) = tree
		.node(node)
		.expect("node belongs to the supplied tree")
		.parent
	else {
		return 0;
	};
	tree.node(parent)
		.expect("parent belongs to the supplied tree")
		.children
		.iter()
		.position(|child| *child == node)
		.expect("parent contains its child")
}

fn ordering_facts(
	revision: RevisionId,
	base: &NormalizedTree,
	revision_tree: &NormalizedTree,
	matching: &Matching,
) -> Vec<OrderingFact> {
	let mut facts = Vec::new();
	for (revision_parent_id, revision_parent) in revision_tree.nodes() {
		if revision_parent.child_order != ChildOrder::Ordered {
			continue;
		}
		let parent = DeltaNodeRef::from_revision(revision, revision_parent_id, matching);
		for pair in revision_parent.children.windows(2) {
			let before = DeltaNodeRef::from_revision(revision, pair[0], matching);
			let after = DeltaNodeRef::from_revision(revision, pair[1], matching);
			if ordering_is_new(base, &parent, &before, &after) {
				facts.push(OrderingFact {
					parent,
					before,
					after,
				});
			}
		}
	}
	facts
}

fn ordering_is_new(
	base: &NormalizedTree,
	parent: &DeltaNodeRef,
	before: &DeltaNodeRef,
	after: &DeltaNodeRef,
) -> bool {
	let (Some(base_parent), Some(base_before), Some(base_after)) =
		(parent.base, before.base, after.base)
	else {
		return true;
	};
	let base_parent_node = base
		.node(base_parent.node)
		.expect("base reference belongs to the base tree");
	if base
		.node(base_before.node)
		.expect("base reference belongs to the base tree")
		.parent
		!= Some(base_parent.node)
		|| base
			.node(base_after.node)
			.expect("base reference belongs to the base tree")
			.parent != Some(base_parent.node)
	{
		return true;
	}
	let before_position = base_parent_node
		.children
		.iter()
		.position(|node| *node == base_before.node)
		.expect("base parent contains its child");
	let after_position = base_parent_node
		.children
		.iter()
		.position(|node| *node == base_after.node)
		.expect("base parent contains its child");
	before_position >= after_position
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::merge::kernel::{TreeMatcher, TreeNode};

	fn normalize(root: TreeNode) -> NormalizedTree {
		NormalizedTree::from_root(root).unwrap()
	}

	fn anchored_leaf(name: &str, value: &str) -> TreeNode {
		TreeNode::leaf("field", value).with_anchor("field", name)
	}

	#[test]
	fn delta_serialization_is_deterministic() {
		let base = normalize(TreeNode::branch(
			"root",
			vec![anchored_leaf("a", "one"), anchored_leaf("b", "two")],
		));
		let revision = normalize(TreeNode::branch(
			"root",
			vec![anchored_leaf("b", "changed"), anchored_leaf("c", "three")],
		));
		let matching = TreeMatcher::default().match_trees(&base, &revision);

		let first = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);
		let second = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);

		assert_eq!(first, second);
		assert_eq!(
			first.matches,
			vec![
				DeltaNodeMatch {
					base: RevisionNode::new(RevisionId::BASE, NodeId::new(0)),
					revision: RevisionNode::new(RevisionId::LEFT, NodeId::new(0)),
				},
				DeltaNodeMatch {
					base: RevisionNode::new(RevisionId::BASE, NodeId::new(2)),
					revision: RevisionNode::new(RevisionId::LEFT, NodeId::new(1)),
				},
			]
		);
		assert_eq!(
			serde_json::to_string(&first).unwrap(),
			serde_json::to_string(&second).unwrap()
		);
	}

	#[test]
	fn deletion_is_a_tombstone_for_the_unmatched_subtree_root() {
		let base = normalize(TreeNode::branch(
			"root",
			vec![
				anchored_leaf("kept", "one"),
				TreeNode::branch("removed", vec![TreeNode::leaf("nested", "value")])
					.with_anchor("field", "removed"),
			],
		));
		let revision = normalize(TreeNode::branch("root", vec![anchored_leaf("kept", "one")]));
		let matching = TreeMatcher::default().match_trees(&base, &revision);

		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);
		let tombstones = delta.tombstones().collect::<Vec<_>>();

		assert_eq!(tombstones.len(), 1);
		assert_eq!(tombstones[0].deleted.node, NodeId::new(2));
		assert_eq!(
			delta
				.operations
				.iter()
				.filter(|operation| matches!(operation, DeltaOperation::Insert { .. }))
				.count(),
			0
		);
	}

	#[test]
	fn move_is_not_reported_as_delete_and_insert() {
		let movable = || anchored_leaf("movable", "value");
		let container = |name: &str, children: Vec<TreeNode>| {
			TreeNode::branch("container", children).with_anchor("container", name)
		};
		let base = normalize(TreeNode::branch(
			"root",
			vec![
				container("left", vec![movable()]),
				container("right", Vec::new()),
			],
		));
		let revision = normalize(TreeNode::branch(
			"root",
			vec![
				container("left", Vec::new()),
				container("right", vec![movable()]),
			],
		));
		let matching = TreeMatcher::default().match_trees(&base, &revision);

		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);

		assert!(
			delta
				.operations
				.iter()
				.any(|operation| matches!(operation, DeltaOperation::Move { .. }))
		);
		assert!(!delta.operations.iter().any(|operation| matches!(
			operation,
			DeltaOperation::Delete { .. } | DeltaOperation::Insert { .. }
		)));
	}

	#[test]
	fn rename_is_not_reported_as_delete_and_insert() {
		let named = |name: &str| {
			let mut node = TreeNode::branch("definition", vec![TreeNode::leaf("body", "same")])
				.with_signature("same-definition");
			node.value = Some(name.to_string());
			node
		};
		let base = normalize(TreeNode::branch("root", vec![named("old")]));
		let revision = normalize(TreeNode::branch("root", vec![named("new")]));
		let matching = TreeMatcher::default().match_trees(&base, &revision);

		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);

		assert!(
			delta
				.operations
				.iter()
				.any(|operation| matches!(operation, DeltaOperation::Rename { .. }))
		);
		assert!(!delta.operations.iter().any(|operation| matches!(
			operation,
			DeltaOperation::Delete { .. } | DeltaOperation::Insert { .. }
		)));
	}

	#[test]
	fn reordered_children_emit_revision_local_ordering_fact() {
		let base = normalize(TreeNode::branch(
			"root",
			vec![anchored_leaf("a", "one"), anchored_leaf("b", "two")],
		));
		let revision = normalize(TreeNode::branch(
			"root",
			vec![anchored_leaf("b", "two"), anchored_leaf("a", "one")],
		));
		let matching = TreeMatcher::default().match_trees(&base, &revision);

		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);

		assert_eq!(delta.ordering.len(), 1);
		assert_eq!(delta.ordering[0].before.base.unwrap().node, NodeId::new(2));
		assert_eq!(delta.ordering[0].after.base.unwrap().node, NodeId::new(1));
	}
}
