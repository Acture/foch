// SPDX-License-Identifier: GPL-3.0-only
//
// Owned-tree adaptation of Mergiraf 0.18.0 `src/class_mapping.rs` at upstream
// revision e8e13887b85b8cb56b1dc1624c5f94e3d39182b6.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Matching, NodeId, NormalizedTree, RevisionId, RevisionNode};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ClassId(u32);

impl ClassId {
	pub const fn new(value: u32) -> Self {
		Self(value)
	}

	pub const fn get(self) -> u32 {
		self.0
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RevisionClass {
	pub id: ClassId,
	pub members: BTreeMap<RevisionId, NodeId>,
}

impl RevisionClass {
	pub fn get(&self, revision: RevisionId) -> Option<NodeId> {
		self.members.get(&revision).copied()
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClassMapping {
	classes: Vec<Option<RevisionClass>>,
	node_to_class: BTreeMap<RevisionNode, ClassId>,
}

impl ClassMapping {
	pub fn from_matchings(
		base: &NormalizedTree,
		left: &NormalizedTree,
		right: &NormalizedTree,
		base_left: &Matching,
		base_right: &Matching,
		left_right: &Matching,
	) -> Self {
		let mut mapping = Self {
			classes: Vec::with_capacity(base.len() + left.len() + right.len()),
			node_to_class: BTreeMap::new(),
		};
		mapping.add_revision(RevisionId::BASE, base);
		mapping.add_revision(RevisionId::LEFT, left);
		mapping.add_revision(RevisionId::RIGHT, right);
		mapping.add_matching(base_left, RevisionId::BASE, RevisionId::LEFT);
		mapping.add_matching(base_right, RevisionId::BASE, RevisionId::RIGHT);
		mapping.add_matching(left_right, RevisionId::LEFT, RevisionId::RIGHT);
		mapping
	}

	pub fn class_of(&self, node: RevisionNode) -> ClassId {
		self.node_to_class[&node]
	}

	pub fn class(&self, id: ClassId) -> &RevisionClass {
		self.classes[id.get() as usize]
			.as_ref()
			.expect("class id resolves to a live class")
	}

	pub fn classes(&self) -> impl Iterator<Item = &RevisionClass> {
		self.classes.iter().filter_map(Option::as_ref)
	}

	fn add_revision(&mut self, revision: RevisionId, tree: &NormalizedTree) {
		for (node, _) in tree.nodes() {
			let id = ClassId::new(self.classes.len() as u32);
			self.classes.push(Some(RevisionClass {
				id,
				members: BTreeMap::from([(revision, node)]),
			}));
			self.node_to_class
				.insert(RevisionNode::new(revision, node), id);
		}
	}

	fn add_matching(
		&mut self,
		matching: &Matching,
		left_revision: RevisionId,
		right_revision: RevisionId,
	) {
		for record in matching.records() {
			let left = RevisionNode::new(left_revision, record.left);
			let right = RevisionNode::new(right_revision, record.right);
			self.try_union(left, right);
		}
	}

	fn try_union(&mut self, left: RevisionNode, right: RevisionNode) -> bool {
		let left_class = self.class_of(left);
		let right_class = self.class_of(right);
		if left_class == right_class {
			return true;
		}
		let (leader, absorbed) = if left_class < right_class {
			(left_class, right_class)
		} else {
			(right_class, left_class)
		};
		let absorbed_members = self.class(absorbed).members.clone();
		if absorbed_members.iter().any(|(revision, node)| {
			self.class(leader)
				.members
				.get(revision)
				.is_some_and(|known| known != node)
		}) {
			return false;
		}
		self.classes[absorbed.get() as usize] = None;
		let leader_class = self.classes[leader.get() as usize]
			.as_mut()
			.expect("leader class remains live");
		for (revision, node) in absorbed_members {
			leader_class.members.insert(revision, node);
			self.node_to_class
				.insert(RevisionNode::new(revision, node), leader);
		}
		true
	}
}

#[cfg(test)]
mod tests {
	use crate::{TreeMatcher, TreeNode};

	use super::*;

	#[test]
	fn every_class_contains_at_most_one_node_per_revision() {
		let base = NormalizedTree::from_root(TreeNode::branch(
			"root",
			vec![TreeNode::leaf("value", "a"), TreeNode::leaf("value", "b")],
		))
		.unwrap();
		let left = base.clone();
		let right = NormalizedTree::from_root(TreeNode::branch(
			"root",
			vec![TreeNode::leaf("value", "b"), TreeNode::leaf("value", "a")],
		))
		.unwrap();
		let matcher = TreeMatcher::default();
		let mapping = ClassMapping::from_matchings(
			&base,
			&left,
			&right,
			&matcher.match_trees(&base, &left),
			&matcher.match_trees(&base, &right),
			&matcher.match_trees(&left, &right),
		);

		for class in mapping.classes() {
			assert!(class.members.len() <= 3);
		}
	}
}
