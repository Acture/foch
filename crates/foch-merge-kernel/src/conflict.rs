use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ClassId, NodeId, NormalizedTree, RevisionNode, SourceSet};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
	AmbiguousMatch,
	InsertInsert,
	DeleteModify,
	MoveMove,
	Ordering,
	ValueSlot,
	DuplicateSignature,
	Policy,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StructuralConflict {
	pub kind: ConflictKind,
	pub parent: Option<ClassId>,
	pub base: Option<RevisionNode>,
	pub revisions: SourceSet,
	pub detail: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeTimings {
	pub matcher_ns: u64,
	pub pcs_ns: u64,
	pub policy_ns: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeOutcome {
	pub(crate) tentative_tree: NormalizedTree,
	pub provenance: BTreeMap<NodeId, SourceSet>,
	pub conflicts: Vec<StructuralConflict>,
	pub timings: MergeTimings,
}

impl MergeOutcome {
	pub fn has_conflicts(&self) -> bool {
		!self.conflicts.is_empty()
	}

	pub fn resolved_tree(&self) -> Option<&NormalizedTree> {
		(!self.has_conflicts()).then_some(&self.tentative_tree)
	}

	pub fn tentative_tree(&self) -> &NormalizedTree {
		&self.tentative_tree
	}
}
