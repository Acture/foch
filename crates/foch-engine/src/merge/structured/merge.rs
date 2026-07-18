use foch_language::analyzer::content_family::{MergePolicies, ScalarMergePolicy};
use foch_language::analyzer::parser::AstFile;
use foch_merge_kernel::{
	ConflictKind, DeleteModifyContext, MergeOutcome, MergePolicy, NodeConflictContext,
	PolicyDecision, RevisionId, StructuralConflict, three_way_merge_with_policy,
};

use super::ast_adapter::{AstAdapterError, denormalize_ast, normalize_ast};
use super::policy::EventTreePolicy;

#[derive(Clone, Debug)]
pub(crate) struct ClausewitzMergeOutcome {
	tentative_ast: AstFile,
	kernel: MergeOutcome,
}

impl ClausewitzMergeOutcome {
	pub(crate) fn conflicts(&self) -> &[StructuralConflict] {
		&self.kernel.conflicts
	}

	pub(crate) fn resolved_ast(&self) -> Option<&AstFile> {
		self.kernel
			.conflicts
			.is_empty()
			.then_some(&self.tentative_ast)
	}

	#[cfg(test)]
	pub(crate) fn tentative_ast(&self) -> &AstFile {
		&self.tentative_ast
	}

	#[cfg(test)]
	pub(crate) fn kernel(&self) -> &MergeOutcome {
		&self.kernel
	}
}

pub(crate) fn merge_event_files(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	let policy = EventTreePolicy;
	let base_tree = normalize_ast(base, &policy)?;
	let left_tree = normalize_ast(left, &policy)?;
	let right_tree = normalize_ast(right, &policy)?;
	let kernel_policy = EventMergePolicy { policies };
	let kernel = three_way_merge_with_policy(&base_tree, &left_tree, &right_tree, &kernel_policy);
	let tentative_ast = denormalize_ast(base.path.clone(), kernel.tentative_tree())?;
	Ok(ClausewitzMergeOutcome {
		tentative_ast,
		kernel,
	})
}

struct EventMergePolicy<'a> {
	policies: &'a MergePolicies,
}

impl MergePolicy for EventMergePolicy<'_> {
	fn resolve_delete_modify(&self, context: DeleteModifyContext<'_>) -> PolicyDecision {
		if self.policies.edit_wins_over_remove
			&& context.content_changed
			&& !context.reparented
			&& !context.reordered
		{
			PolicyDecision::Resolved
		} else {
			PolicyDecision::Unresolved
		}
	}

	fn select_divergent_node(&self, context: NodeConflictContext<'_>) -> Option<RevisionId> {
		let scalar_conflict = matches!(
			context.kind,
			ConflictKind::InsertInsert | ConflictKind::Policy
		) && context.left.is_some_and(is_scalar_node)
			&& context.right.is_some_and(is_scalar_node);
		(self.policies.scalar == ScalarMergePolicy::LastWriter && scalar_conflict)
			.then_some(RevisionId::RIGHT)
	}
}

fn is_scalar_node(node: &foch_merge_kernel::NormalizedNode) -> bool {
	node.kind.starts_with("clausewitz.scalar.") && node.children.is_empty()
}
