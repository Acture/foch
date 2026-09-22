use crate::merge::kernel::{ClassId, ConflictKind, NormalizedNode, RevisionId, RevisionNode};

#[derive(Clone, Copy, Debug)]
pub struct NWayNodeView<'a> {
	pub source: RevisionNode,
	pub node: &'a NormalizedNode,
	pub parent: Option<&'a NormalizedNode>,
	pub shallow_changed: bool,
	pub subtree_changed: bool,
	pub reparented: bool,
	pub reordered: bool,
	pub parent_changed_from_base: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct NWayClassContext<'a> {
	pub class: ClassId,
	pub kind: ConflictKind,
	pub base: Option<NWayNodeView<'a>>,
	pub contributors: &'a [NWayNodeView<'a>],
}

#[derive(Clone, Copy, Debug)]
pub struct NWayDeleteContext<'a> {
	pub class: NWayClassContext<'a>,
	pub deleted_by: &'a [RevisionId],
	pub parent_present_in_all_revisions: bool,
	pub deleted_parent_has_same_kind_gap_replacement: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyDecision {
	Unresolved,
	Resolved,
	Select(RevisionId),
	SynthesizeScalar(String),
}

pub trait MergePolicy {
	fn resolve_nway_delete(&self, _context: NWayDeleteContext<'_>) -> PolicyDecision {
		PolicyDecision::Unresolved
	}

	/// Preserve one surviving revision's complete subtree when another
	/// revision deleted the matched semantic unit.
	fn select_nway_deleted_subtree(&self, _context: NWayDeleteContext<'_>) -> Option<RevisionId> {
		None
	}

	fn select_nway_subtree(&self, _context: NWayClassContext<'_>) -> Option<RevisionId> {
		None
	}

	fn select_nway_children(&self, _context: NWayClassContext<'_>) -> Option<RevisionId> {
		None
	}

	fn resolve_nway_divergent_node(&self, _context: NWayClassContext<'_>) -> PolicyDecision {
		PolicyDecision::Unresolved
	}

	/// Permit a missing structural ancestor to be restored so an explicitly
	/// preserved descendant remains reachable from the merged root.
	fn permits_ancestor_closure(&self, _node: &NormalizedNode) -> bool {
		false
	}
}

#[derive(Clone, Copy, Debug, Default)]
#[cfg(test)]
pub struct ConservativeMergePolicy;

#[cfg(test)]
impl MergePolicy for ConservativeMergePolicy {}
