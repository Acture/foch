use crate::{ConflictKind, NormalizedNode, RevisionId};

#[derive(Clone, Copy, Debug)]
pub struct DeleteModifyContext<'a> {
	pub base: &'a NormalizedNode,
	pub present: &'a NormalizedNode,
	pub deleted_revision: RevisionId,
	pub present_revision: RevisionId,
	pub content_changed: bool,
	pub reparented: bool,
	pub reordered: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct NodeConflictContext<'a> {
	pub kind: ConflictKind,
	pub base: Option<&'a NormalizedNode>,
	pub left: Option<&'a NormalizedNode>,
	pub right: Option<&'a NormalizedNode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyDecision {
	Unresolved,
	Resolved,
}

pub trait MergePolicy {
	fn resolve_delete_modify(&self, _context: DeleteModifyContext<'_>) -> PolicyDecision {
		PolicyDecision::Unresolved
	}

	fn select_divergent_node(&self, _context: NodeConflictContext<'_>) -> Option<RevisionId> {
		None
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ConservativeMergePolicy;

impl MergePolicy for ConservativeMergePolicy {}
