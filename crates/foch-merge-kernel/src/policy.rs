use crate::{ConflictKind, NormalizedNode, RevisionId};

#[derive(Clone, Copy, Debug)]
pub struct DeleteUnchangedContext<'a> {
	pub base: &'a NormalizedNode,
	pub present: &'a NormalizedNode,
	pub deleted_revision: RevisionId,
	pub present_revision: RevisionId,
	pub parent_present_in_both_revisions: bool,
	pub present_parent_changed_from_base: bool,
	pub deleted_parent_has_same_kind_sibling: bool,
	pub base_parent: Option<&'a NormalizedNode>,
}

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

#[derive(Clone, Copy, Debug)]
pub struct ChildSetContext<'a> {
	pub base: Option<&'a NormalizedNode>,
	pub left: Option<&'a NormalizedNode>,
	pub right: Option<&'a NormalizedNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyDecision {
	Unresolved,
	Resolved,
	Select(RevisionId),
	SynthesizeScalar(String),
}

pub trait MergePolicy {
	/// Resolve a one-sided deletion of an otherwise unchanged node in favor of
	/// the present revision. The default keeps ordinary three-way delete wins.
	fn resolve_delete_unchanged(&self, _context: DeleteUnchangedContext<'_>) -> PolicyDecision {
		PolicyDecision::Unresolved
	}

	fn resolve_delete_modify(&self, _context: DeleteModifyContext<'_>) -> PolicyDecision {
		PolicyDecision::Unresolved
	}

	/// Permit a missing structural ancestor to be restored so an explicitly
	/// preserved descendant remains reachable from the merged root.
	fn permits_ancestor_closure(&self, _node: &NormalizedNode) -> bool {
		false
	}

	fn select_child_revision(&self, _context: ChildSetContext<'_>) -> Option<RevisionId> {
		None
	}

	/// Resolve a divergent matched node. Scalar synthesis changes only the
	/// selected node's value and retains all matched revisions as provenance.
	fn resolve_divergent_node(&self, context: NodeConflictContext<'_>) -> PolicyDecision {
		self.select_divergent_node(context)
			.map_or(PolicyDecision::Unresolved, PolicyDecision::Select)
	}

	fn select_divergent_node(&self, _context: NodeConflictContext<'_>) -> Option<RevisionId> {
		None
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ConservativeMergePolicy;

impl MergePolicy for ConservativeMergePolicy {}
