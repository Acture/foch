use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{ConflictKind, MergeInputId, NodeId, RevisionNode};

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ConflictNodeId([u8; 32]);

impl ConflictNodeId {
	pub(crate) fn derive(
		inputs: impl IntoIterator<Item = MergeInputId>,
		kind: ConflictKind,
		parent: Option<u32>,
		base: Option<RevisionNode>,
		candidates: &[SourceNodeRef],
		semantic_path: &[String],
	) -> Self {
		let mut hasher = blake3::Hasher::new();
		hasher.update(b"foch-conflict-node-v1\0");
		for input in inputs {
			hash_u16(&mut hasher, input.revision.get());
			hasher.update(input.root_hash.as_bytes());
		}
		hasher.update(&[conflict_kind_tag(kind)]);
		hash_optional_u32(&mut hasher, parent);
		hash_optional_revision_node(&mut hasher, base);
		for candidate in candidates {
			hash_source_node_ref(&mut hasher, *candidate);
		}
		for component in semantic_path {
			hash_bytes(&mut hasher, component.as_bytes());
		}
		Self(*hasher.finalize().as_bytes())
	}

	pub const fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}
}

impl fmt::Debug for ConflictNodeId {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{self}")
	}
}

impl fmt::Display for ConflictNodeId {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		for byte in self.0 {
			write!(formatter, "{byte:02x}")?;
		}
		Ok(())
	}
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SourceNodeRef {
	Node {
		input: MergeInputId,
		node: NodeId,
	},
	Tombstone {
		input: MergeInputId,
		base_node: NodeId,
	},
}

impl SourceNodeRef {
	pub const fn input(self) -> MergeInputId {
		match self {
			Self::Node { input, .. } | Self::Tombstone { input, .. } => input,
		}
	}

	pub const fn node(self) -> Option<NodeId> {
		match self {
			Self::Node { node, .. } => Some(node),
			Self::Tombstone { .. } => None,
		}
	}

	pub const fn base_node(self) -> Option<NodeId> {
		match self {
			Self::Node { .. } => None,
			Self::Tombstone { base_node, .. } => Some(base_node),
		}
	}
}

fn hash_source_node_ref(hasher: &mut blake3::Hasher, source: SourceNodeRef) {
	match source {
		SourceNodeRef::Node { input, node } => {
			hasher.update(&[0]);
			hash_merge_input(hasher, input);
			hasher.update(&node.get().to_le_bytes());
		}
		SourceNodeRef::Tombstone { input, base_node } => {
			hasher.update(&[1]);
			hash_merge_input(hasher, input);
			hasher.update(&base_node.get().to_le_bytes());
		}
	}
}

fn hash_merge_input(hasher: &mut blake3::Hasher, input: MergeInputId) {
	hash_u16(hasher, input.revision.get());
	hasher.update(input.root_hash.as_bytes());
}

fn conflict_kind_tag(kind: ConflictKind) -> u8 {
	match kind {
		ConflictKind::AmbiguousMatch => 0,
		ConflictKind::InsertInsert => 1,
		ConflictKind::DeleteModify => 2,
		ConflictKind::MoveMove => 3,
		ConflictKind::Ordering => 4,
		ConflictKind::ValueSlot => 5,
		ConflictKind::DuplicateSignature => 6,
		ConflictKind::Policy => 7,
	}
}

fn hash_optional_u32(hasher: &mut blake3::Hasher, value: Option<u32>) {
	match value {
		Some(value) => {
			hasher.update(&[1]);
			hasher.update(&value.to_le_bytes());
		}
		None => {
			hasher.update(&[0]);
		}
	}
}

fn hash_optional_revision_node(hasher: &mut blake3::Hasher, revision_node: Option<RevisionNode>) {
	match revision_node {
		Some(revision_node) => {
			hasher.update(&[1]);
			hash_revision_node(hasher, revision_node);
		}
		None => {
			hasher.update(&[0]);
		}
	}
}

fn hash_revision_node(hasher: &mut blake3::Hasher, revision_node: RevisionNode) {
	hash_u16(hasher, revision_node.revision.get());
	hasher.update(&revision_node.node.get().to_le_bytes());
}

fn hash_u16(hasher: &mut blake3::Hasher, value: u16) {
	hasher.update(&value.to_le_bytes());
}

fn hash_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
	hasher.update(&(value.len() as u64).to_le_bytes());
	hasher.update(value);
}
