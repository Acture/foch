use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
	ClassId, ConflictNodeId, MergeDecisionEvidence, MergeInputId, NodeId, NormalizedTree,
	RevisionDelta, RevisionId, RevisionNode, SourceNodeRef, SourceSet,
};

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

impl fmt::Display for ConflictKind {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(match self {
			Self::AmbiguousMatch => "ambiguous_match",
			Self::InsertInsert => "insert_insert",
			Self::DeleteModify => "delete_modify",
			Self::MoveMove => "move_move",
			Self::Ordering => "ordering",
			Self::ValueSlot => "value_slot",
			Self::DuplicateSignature => "duplicate_signature",
			Self::Policy => "policy",
		})
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StructuralConflict {
	pub id: ConflictNodeId,
	pub kind: ConflictKind,
	pub parent: Option<ClassId>,
	pub base: Option<RevisionNode>,
	pub revisions: SourceSet,
	pub candidates: Vec<SourceNodeRef>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub semantic_path: Vec<String>,
	pub detail: String,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ConflictResolutionError {
	#[error("source {selected:?} is not a candidate for conflict {conflict}")]
	SourceNotCandidate {
		conflict: ConflictNodeId,
		selected: SourceNodeRef,
	},
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConflictResolution {
	pub conflict: StructuralConflict,
	pub selected: SourceNodeRef,
}

impl ConflictResolution {
	pub fn new(
		conflict: StructuralConflict,
		selected: SourceNodeRef,
	) -> Result<Self, ConflictResolutionError> {
		if !conflict.candidates.contains(&selected) {
			return Err(ConflictResolutionError::SourceNotCandidate {
				conflict: conflict.id,
				selected,
			});
		}
		Ok(Self { conflict, selected })
	}
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RevisionSourceRef {
	Node(RevisionNode),
	Tombstone {
		revision: RevisionId,
		base_node: NodeId,
	},
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuralConflictDraft {
	pub kind: ConflictKind,
	pub parent: Option<ClassId>,
	pub base: Option<RevisionNode>,
	pub revisions: SourceSet,
	pub candidates: Vec<RevisionSourceRef>,
	pub semantic_path: Vec<String>,
	pub detail: String,
}

impl StructuralConflictDraft {
	pub fn new(
		kind: ConflictKind,
		parent: Option<ClassId>,
		base: Option<RevisionNode>,
		revisions: SourceSet,
		semantic_path: Vec<String>,
		detail: String,
	) -> Self {
		let candidates = revisions
			.iter()
			.copied()
			.map(RevisionSourceRef::Node)
			.collect();
		Self {
			kind,
			parent,
			base,
			revisions,
			candidates,
			semantic_path,
			detail,
		}
	}

	pub fn with_candidate(mut self, candidate: RevisionSourceRef) -> Self {
		if !self.candidates.contains(&candidate) {
			self.candidates.push(candidate);
			self.candidates.sort();
		}
		self
	}
}

impl StructuralConflict {
	pub(crate) fn identify(
		draft: StructuralConflictDraft,
		inputs: impl IntoIterator<Item = MergeInputId>,
	) -> Self {
		let StructuralConflictDraft {
			kind,
			parent,
			base,
			revisions,
			candidates,
			semantic_path,
			detail,
		} = draft;
		let inputs = inputs
			.into_iter()
			.map(|input| (input.revision, input))
			.collect::<BTreeMap<_, _>>();
		let candidates = candidates
			.into_iter()
			.map(|candidate| bind_candidate(candidate, &inputs))
			.collect::<Vec<_>>();
		Self {
			id: ConflictNodeId::derive(
				inputs.values().copied(),
				kind,
				parent.map(ClassId::get),
				base,
				&candidates,
				&semantic_path,
			),
			kind,
			parent,
			base,
			revisions,
			candidates,
			semantic_path,
			detail,
		}
	}

	pub fn select(
		&self,
		source: SourceNodeRef,
	) -> Result<ConflictResolution, ConflictResolutionError> {
		ConflictResolution::new(self.clone(), source)
	}
}

fn bind_candidate(
	candidate: RevisionSourceRef,
	inputs: &BTreeMap<RevisionId, MergeInputId>,
) -> SourceNodeRef {
	match candidate {
		RevisionSourceRef::Node(source) => {
			let input = inputs
				.get(&source.revision)
				.copied()
				.expect("every conflict source belongs to a merge input");
			SourceNodeRef::Node {
				input,
				node: source.node,
			}
		}
		RevisionSourceRef::Tombstone {
			revision,
			base_node,
		} => {
			let input = inputs
				.get(&revision)
				.copied()
				.expect("every conflict tombstone belongs to a merge input");
			SourceNodeRef::Tombstone { input, base_node }
		}
	}
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeTimings {
	pub matcher_ns: u64,
	pub delta_ns: u64,
	pub pcs_ns: u64,
	pub policy_ns: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeOutcome {
	pub(crate) tentative_tree: NormalizedTree,
	pub inputs: BTreeMap<RevisionId, MergeInputId>,
	pub provenance: BTreeMap<NodeId, SourceSet>,
	pub revision_deltas: BTreeMap<RevisionId, RevisionDelta>,
	pub decisions: Vec<MergeDecisionEvidence>,
	pub conflicts: Vec<StructuralConflict>,
	pub timings: MergeTimings,
}

impl MergeOutcome {
	pub fn push_conflict(&mut self, conflict: StructuralConflictDraft) {
		self.conflicts.push(StructuralConflict::identify(
			conflict,
			self.inputs.values().copied(),
		));
	}

	pub fn source_node_ref(&self, source: RevisionNode) -> Option<SourceNodeRef> {
		self.inputs
			.get(&source.revision)
			.copied()
			.map(|input| SourceNodeRef::Node {
				input,
				node: source.node,
			})
	}

	pub fn tombstone_ref(
		&self,
		deleted_by: RevisionId,
		base_node: NodeId,
	) -> Option<SourceNodeRef> {
		self.inputs
			.get(&deleted_by)
			.copied()
			.map(|input| SourceNodeRef::Tombstone { input, base_node })
	}

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
