use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{NodeId, RevisionId};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct RevisionNode {
	pub revision: RevisionId,
	pub node: NodeId,
}

impl RevisionNode {
	pub const fn new(revision: RevisionId, node: NodeId) -> Self {
		Self { revision, node }
	}
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSet {
	nodes: BTreeSet<RevisionNode>,
}

impl SourceSet {
	pub fn new(nodes: impl IntoIterator<Item = RevisionNode>) -> Self {
		Self {
			nodes: nodes.into_iter().collect(),
		}
	}

	pub fn insert(&mut self, node: RevisionNode) -> bool {
		self.nodes.insert(node)
	}

	pub fn iter(&self) -> impl Iterator<Item = &RevisionNode> {
		self.nodes.iter()
	}

	pub fn is_empty(&self) -> bool {
		self.nodes.is_empty()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn source_sets_are_sorted_and_deduplicated() {
		let source = RevisionNode::new(RevisionId::new(2), NodeId::new(4));
		let set = SourceSet::new([
			source,
			RevisionNode::new(RevisionId::new(1), NodeId::new(8)),
			source,
		]);

		assert_eq!(set.iter().copied().collect::<Vec<_>>().len(), 2);
		assert_eq!(set.iter().next().unwrap().revision, RevisionId::new(1));
	}
}
