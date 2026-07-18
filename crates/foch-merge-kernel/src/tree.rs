use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct RevisionId(u16);

impl RevisionId {
	pub const BASE: Self = Self(0);
	pub const LEFT: Self = Self(1);
	pub const RIGHT: Self = Self(2);

	pub const fn new(value: u16) -> Self {
		Self(value)
	}

	pub const fn get(self) -> u16 {
		self.0
	}
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NodeId(u32);

impl NodeId {
	pub const fn new(value: u32) -> Self {
		Self(value)
	}

	pub const fn get(self) -> u32 {
		self.0
	}
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildOrder {
	#[default]
	Ordered,
	Commutative,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildCardinality {
	#[default]
	Many,
	ExactlyOne,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SemanticKey {
	pub namespace: String,
	pub value: String,
}

impl SemanticKey {
	pub fn new(namespace: impl Into<String>, value: impl Into<String>) -> Self {
		Self {
			namespace: namespace.into(),
			value: value.into(),
		}
	}
}

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SubtreeHash([u8; 32]);

impl SubtreeHash {
	pub const fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}
}

impl fmt::Debug for SubtreeHash {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{}", self)
	}
}

impl fmt::Display for SubtreeHash {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		for byte in self.0 {
			write!(formatter, "{byte:02x}")?;
		}
		Ok(())
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeNode {
	pub kind: String,
	pub value: Option<String>,
	pub anchor: Option<SemanticKey>,
	pub signature: Option<String>,
	pub child_order: ChildOrder,
	pub child_cardinality: ChildCardinality,
	pub children: Vec<TreeNode>,
}

impl TreeNode {
	pub fn branch(kind: impl Into<String>, children: Vec<Self>) -> Self {
		Self {
			kind: kind.into(),
			value: None,
			anchor: None,
			signature: None,
			child_order: ChildOrder::Ordered,
			child_cardinality: ChildCardinality::Many,
			children,
		}
	}

	pub fn leaf(kind: impl Into<String>, value: impl Into<String>) -> Self {
		Self {
			kind: kind.into(),
			value: Some(value.into()),
			anchor: None,
			signature: None,
			child_order: ChildOrder::Ordered,
			child_cardinality: ChildCardinality::Many,
			children: Vec::new(),
		}
	}

	pub fn with_anchor(mut self, namespace: impl Into<String>, value: impl Into<String>) -> Self {
		self.anchor = Some(SemanticKey::new(namespace, value));
		self
	}

	pub fn with_signature(mut self, signature: impl Into<String>) -> Self {
		self.signature = Some(signature.into());
		self
	}

	pub const fn with_child_order(mut self, child_order: ChildOrder) -> Self {
		self.child_order = child_order;
		self
	}

	pub const fn with_child_cardinality(mut self, child_cardinality: ChildCardinality) -> Self {
		self.child_cardinality = child_cardinality;
		self
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NormalizedNode {
	pub kind: String,
	pub value: Option<String>,
	pub anchor: Option<SemanticKey>,
	pub signature: Option<String>,
	pub child_order: ChildOrder,
	pub child_cardinality: ChildCardinality,
	pub parent: Option<NodeId>,
	pub children: Vec<NodeId>,
	pub subtree_hash: SubtreeHash,
	pub height: u32,
	pub descendant_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NormalizedTree {
	root: NodeId,
	nodes: Vec<NormalizedNode>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TreeError {
	#[error("normalized tree contains too many nodes")]
	TooManyNodes,
	#[error("node `{kind}` requires exactly one child, found {actual}")]
	InvalidChildCardinality { kind: String, actual: usize },
	#[error("node {0:?} is outside the normalized tree")]
	UnknownNode(NodeId),
}

impl NormalizedTree {
	pub fn from_root(root: TreeNode) -> Result<Self, TreeError> {
		let mut nodes = Vec::new();
		let root = flatten_node(root, None, &mut nodes)?;
		Ok(Self { root, nodes })
	}

	pub const fn root(&self) -> NodeId {
		self.root
	}

	pub fn node(&self, id: NodeId) -> Result<&NormalizedNode, TreeError> {
		self.nodes
			.get(id.get() as usize)
			.ok_or(TreeError::UnknownNode(id))
	}

	pub fn nodes(&self) -> impl ExactSizeIterator<Item = (NodeId, &NormalizedNode)> {
		self.nodes
			.iter()
			.enumerate()
			.map(|(index, node)| (NodeId::new(index as u32), node))
	}

	pub fn len(&self) -> usize {
		self.nodes.len()
	}

	pub fn is_empty(&self) -> bool {
		self.nodes.is_empty()
	}

	pub fn to_debug_json(&self) -> Result<String, serde_json::Error> {
		serde_json::to_string_pretty(self)
	}
}

fn flatten_node(
	node: TreeNode,
	parent: Option<NodeId>,
	nodes: &mut Vec<NormalizedNode>,
) -> Result<NodeId, TreeError> {
	let id = NodeId::new(u32::try_from(nodes.len()).map_err(|_| TreeError::TooManyNodes)?);
	let TreeNode {
		kind,
		value,
		anchor,
		signature,
		child_order,
		child_cardinality,
		children,
	} = node;
	if child_cardinality == ChildCardinality::ExactlyOne && children.len() != 1 {
		return Err(TreeError::InvalidChildCardinality {
			kind,
			actual: children.len(),
		});
	}
	nodes.push(NormalizedNode {
		kind,
		value,
		anchor,
		signature,
		child_order,
		child_cardinality,
		parent,
		children: Vec::with_capacity(children.len()),
		subtree_hash: SubtreeHash([0; 32]),
		height: 0,
		descendant_count: 0,
	});
	let child_ids = children
		.into_iter()
		.map(|child| flatten_node(child, Some(id), nodes))
		.collect::<Result<Vec<_>, _>>()?;
	let (subtree_hash, height, descendant_count) = summarize_node(id, &child_ids, nodes);
	let stored = &mut nodes[id.get() as usize];
	stored.children = child_ids;
	stored.subtree_hash = subtree_hash;
	stored.height = height;
	stored.descendant_count = descendant_count;
	Ok(id)
}

fn summarize_node(
	id: NodeId,
	children: &[NodeId],
	nodes: &[NormalizedNode],
) -> (SubtreeHash, u32, u32) {
	let node = &nodes[id.get() as usize];
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-normalized-tree-v1\0");
	hash_field(&mut hasher, node.kind.as_bytes());
	hash_optional_field(&mut hasher, node.value.as_deref());
	hash_optional_field(
		&mut hasher,
		node.anchor.as_ref().map(|anchor| anchor.namespace.as_str()),
	);
	hash_optional_field(
		&mut hasher,
		node.anchor.as_ref().map(|anchor| anchor.value.as_str()),
	);
	hash_optional_field(&mut hasher, node.signature.as_deref());
	hasher.update(&[match node.child_order {
		ChildOrder::Ordered => 0,
		ChildOrder::Commutative => 1,
	}]);
	hasher.update(&[match node.child_cardinality {
		ChildCardinality::Many => 0,
		ChildCardinality::ExactlyOne => 1,
	}]);
	let mut child_hashes = children
		.iter()
		.map(|child| nodes[child.get() as usize].subtree_hash)
		.collect::<Vec<_>>();
	if node.child_order == ChildOrder::Commutative {
		child_hashes.sort_unstable();
	}
	for hash in child_hashes {
		hasher.update(hash.as_bytes());
	}
	let height = children
		.iter()
		.map(|child| nodes[child.get() as usize].height)
		.max()
		.map_or(0, |height| height.saturating_add(1));
	let descendant_count = children.iter().fold(0u32, |total, child| {
		total
			.saturating_add(1)
			.saturating_add(nodes[child.get() as usize].descendant_count)
	});
	(
		SubtreeHash(*hasher.finalize().as_bytes()),
		height,
		descendant_count,
	)
}

fn hash_optional_field(hasher: &mut blake3::Hasher, value: Option<&str>) {
	match value {
		Some(value) => {
			hasher.update(&[1]);
			hash_field(hasher, value.as_bytes());
		}
		None => {
			hasher.update(&[0]);
		}
	}
}

fn hash_field(hasher: &mut blake3::Hasher, value: &[u8]) {
	hasher.update(&(value.len() as u64).to_le_bytes());
	hasher.update(value);
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample_tree() -> TreeNode {
		TreeNode::branch(
			"root",
			vec![
				TreeNode::leaf("scalar", "one").with_anchor("field", "a"),
				TreeNode::branch("block", vec![TreeNode::leaf("scalar", "two")]),
			],
		)
	}

	#[test]
	fn normalized_tree_uses_preorder_ids_and_parent_links() {
		let tree = NormalizedTree::from_root(sample_tree()).unwrap();

		assert_eq!(tree.root(), NodeId::new(0));
		assert_eq!(tree.len(), 4);
		assert_eq!(
			tree.node(NodeId::new(1)).unwrap().parent,
			Some(NodeId::new(0))
		);
		assert_eq!(
			tree.node(NodeId::new(3)).unwrap().parent,
			Some(NodeId::new(2))
		);
		assert_eq!(tree.node(NodeId::new(0)).unwrap().height, 2);
		assert_eq!(tree.node(NodeId::new(0)).unwrap().descendant_count, 3);
	}

	#[test]
	fn normalized_hashes_are_repeatable_and_content_sensitive() {
		let first = NormalizedTree::from_root(sample_tree()).unwrap();
		let second = NormalizedTree::from_root(sample_tree()).unwrap();
		let changed = NormalizedTree::from_root(TreeNode::branch(
			"root",
			vec![TreeNode::leaf("scalar", "changed")],
		))
		.unwrap();

		assert_eq!(
			first.node(first.root()).unwrap().subtree_hash,
			second.node(second.root()).unwrap().subtree_hash
		);
		assert_ne!(
			first.node(first.root()).unwrap().subtree_hash,
			changed.node(changed.root()).unwrap().subtree_hash
		);
	}

	#[test]
	fn commutative_hash_ignores_child_order_but_ordered_hash_does_not() {
		let children = vec![TreeNode::leaf("scalar", "a"), TreeNode::leaf("scalar", "b")];
		let reversed = vec![TreeNode::leaf("scalar", "b"), TreeNode::leaf("scalar", "a")];
		let commutative = NormalizedTree::from_root(
			TreeNode::branch("root", children.clone()).with_child_order(ChildOrder::Commutative),
		)
		.unwrap();
		let commutative_reversed = NormalizedTree::from_root(
			TreeNode::branch("root", reversed.clone()).with_child_order(ChildOrder::Commutative),
		)
		.unwrap();
		let ordered = NormalizedTree::from_root(TreeNode::branch("root", children)).unwrap();
		let ordered_reversed =
			NormalizedTree::from_root(TreeNode::branch("root", reversed)).unwrap();

		assert_eq!(
			commutative.node(commutative.root()).unwrap().subtree_hash,
			commutative_reversed
				.node(commutative_reversed.root())
				.unwrap()
				.subtree_hash
		);
		assert_ne!(
			ordered.node(ordered.root()).unwrap().subtree_hash,
			ordered_reversed
				.node(ordered_reversed.root())
				.unwrap()
				.subtree_hash
		);
	}

	#[test]
	fn exactly_one_cardinality_rejects_invalid_source_trees() {
		for children in [
			Vec::new(),
			vec![
				TreeNode::leaf("value", "one"),
				TreeNode::leaf("value", "two"),
			],
		] {
			let error = NormalizedTree::from_root(
				TreeNode::branch("slot", children)
					.with_child_cardinality(ChildCardinality::ExactlyOne),
			)
			.expect_err("an exactly-one node must contain one child");
			assert!(matches!(
				error,
				TreeError::InvalidChildCardinality { kind, .. } if kind == "slot"
			));
		}
	}

	#[test]
	fn cardinality_participates_in_the_structural_hash() {
		let child = || TreeNode::leaf("value", "same");
		let many = NormalizedTree::from_root(TreeNode::branch("slot", vec![child()])).unwrap();
		let exactly_one = NormalizedTree::from_root(
			TreeNode::branch("slot", vec![child()])
				.with_child_cardinality(ChildCardinality::ExactlyOne),
		)
		.unwrap();

		assert_ne!(
			many.node(many.root()).unwrap().subtree_hash,
			exactly_one.node(exactly_one.root()).unwrap().subtree_hash
		);
	}

	#[test]
	fn debug_json_is_stable() {
		let first = NormalizedTree::from_root(sample_tree())
			.unwrap()
			.to_debug_json()
			.unwrap();
		let second = NormalizedTree::from_root(sample_tree())
			.unwrap()
			.to_debug_json()
			.unwrap();

		assert_eq!(first, second);
	}
}
