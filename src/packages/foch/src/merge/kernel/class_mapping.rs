// SPDX-License-Identifier: GPL-3.0-only
//
// Owned-tree adaptation of Mergiraf 0.18.0 `src/class_mapping.rs` at upstream
// revision e8e13887b85b8cb56b1dc1624c5f94e3d39182b6.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::merge::kernel::{Matching, NodeId, NormalizedTree, RevisionId, RevisionNode};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RejectedClassLink {
	pub left: RevisionNode,
	pub right: RevisionNode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClassMapping {
	classes: Vec<Option<RevisionClass>>,
	node_to_class: BTreeMap<RevisionNode, ClassId>,
	rejected_links: Vec<RejectedClassLink>,
}

impl ClassMapping {
	pub fn from_revision_matchings<'tree, 'matching>(
		revisions: impl IntoIterator<Item = (RevisionId, &'tree NormalizedTree)>,
		matchings: impl IntoIterator<Item = (RevisionId, RevisionId, &'matching Matching)>,
	) -> Self {
		let revisions = revisions.into_iter().collect::<Vec<_>>();
		let mut seen_revisions = BTreeSet::new();
		let mut mapping = Self {
			classes: Vec::with_capacity(
				revisions.iter().map(|(_, tree)| tree.len()).sum::<usize>(),
			),
			node_to_class: BTreeMap::new(),
			rejected_links: Vec::new(),
		};
		for (revision, tree) in revisions {
			assert!(
				seen_revisions.insert(revision),
				"revision {} appears more than once",
				revision.get(),
			);
			mapping.add_revision(revision, tree);
		}
		for (left_revision, right_revision, matching) in matchings {
			assert!(
				seen_revisions.contains(&left_revision),
				"matching references unknown revision {}",
				left_revision.get(),
			);
			assert!(
				seen_revisions.contains(&right_revision),
				"matching references unknown revision {}",
				right_revision.get(),
			);
			mapping.add_matching(matching, left_revision, right_revision);
		}
		mapping
	}

	pub fn class_of(&self, node: RevisionNode) -> ClassId {
		self.node_to_class[&node]
	}

	pub fn class_for(&self, node: RevisionNode) -> Option<ClassId> {
		self.node_to_class.get(&node).copied()
	}

	pub fn class(&self, id: ClassId) -> &RevisionClass {
		self.classes[id.get() as usize]
			.as_ref()
			.expect("class id resolves to a live class")
	}

	pub fn classes(&self) -> impl Iterator<Item = &RevisionClass> {
		self.classes.iter().filter_map(Option::as_ref)
	}

	pub fn rejected_links(&self) -> &[RejectedClassLink] {
		&self.rejected_links
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
			self.add_link(left, right);
		}
	}

	fn add_link(&mut self, left: RevisionNode, right: RevisionNode) {
		if !self.try_union(left, right) {
			let rejected = RejectedClassLink { left, right };
			if !self.rejected_links.contains(&rejected) {
				self.rejected_links.push(rejected);
			}
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
	use crate::merge::kernel::{TreeMatcher, TreeNode};

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
		let base_left = matcher.match_trees(&base, &left);
		let base_right = matcher.match_trees(&base, &right);
		let left_right = matcher.match_trees(&left, &right);
		let mapping = ClassMapping::from_revision_matchings(
			[
				(RevisionId::BASE, &base),
				(RevisionId::LEFT, &left),
				(RevisionId::RIGHT, &right),
			],
			[
				(RevisionId::BASE, RevisionId::LEFT, &base_left),
				(RevisionId::BASE, RevisionId::RIGHT, &base_right),
				(RevisionId::LEFT, RevisionId::RIGHT, &left_right),
			],
		);

		for class in mapping.classes() {
			assert!(class.members.len() <= 3);
		}
	}

	#[test]
	fn class_mapping_accepts_arbitrary_revision_sets() {
		let base = NormalizedTree::from_root(TreeNode::branch(
			"root",
			vec![TreeNode::leaf("value", "base")],
		))
		.unwrap();
		let first = base.clone();
		let second = base.clone();
		let third = base.clone();
		let matcher = TreeMatcher::default();
		let base_first = matcher.match_trees(&base, &first);
		let base_second = matcher.match_trees(&base, &second);
		let base_third = matcher.match_trees(&base, &third);
		let mapping = ClassMapping::from_revision_matchings(
			[
				(RevisionId::BASE, &base),
				(RevisionId::new(1), &first),
				(RevisionId::new(2), &second),
				(RevisionId::new(3), &third),
			],
			[
				(RevisionId::BASE, RevisionId::new(1), &base_first),
				(RevisionId::BASE, RevisionId::new(2), &base_second),
				(RevisionId::BASE, RevisionId::new(3), &base_third),
			],
		);

		assert!(mapping.classes().all(|class| class.members.len() == 4),);
	}

	#[test]
	fn inconsistent_cross_revision_links_are_not_silently_dropped() {
		let first = NormalizedTree::from_root(TreeNode::branch(
			"root",
			vec![TreeNode::leaf("value", "a"), TreeNode::leaf("value", "b")],
		))
		.unwrap();
		let second = first.clone();
		let mut mapping = ClassMapping::from_revision_matchings(
			[(RevisionId::new(1), &first), (RevisionId::new(2), &second)],
			std::iter::empty(),
		);
		let first_children = &first.node(first.root()).unwrap().children;
		let second_child = second.node(second.root()).unwrap().children[0];
		let accepted = RejectedClassLink {
			left: RevisionNode::new(RevisionId::new(1), first_children[0]),
			right: RevisionNode::new(RevisionId::new(2), second_child),
		};
		let rejected = RejectedClassLink {
			left: RevisionNode::new(RevisionId::new(1), first_children[1]),
			right: RevisionNode::new(RevisionId::new(2), second_child),
		};

		mapping.add_link(accepted.left, accepted.right);
		mapping.add_link(rejected.left, rejected.right);

		assert_eq!(mapping.rejected_links(), &[rejected]);
	}
}
