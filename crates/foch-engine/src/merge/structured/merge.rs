use foch_language::analyzer::content_family::{
	MergePolicies, OneSidedRemovalPolicy, ScalarMergePolicy,
};
use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue};
use foch_merge_kernel::{
	ChildSetContext, ConflictKind, DeleteModifyContext, DeleteUnchangedContext, MergeOutcome,
	MergePolicy, NodeConflictContext, PolicyDecision, RevisionId, StructuralConflict,
	three_way_merge_with_policy,
};

use super::ast_adapter::{AstAdapterError, denormalize_ast, normalize_ast};
use super::policy::DefaultClausewitzTreePolicy;

#[derive(Clone, Debug)]
pub struct ClausewitzMergeOutcome {
	tentative_ast: AstFile,
	kernel: MergeOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClausewitzConflictSummary {
	pub kind: &'static str,
	pub detail: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClausewitzMergeTimings {
	pub matcher_ns: u64,
	pub pcs_ns: u64,
	pub policy_ns: u64,
}

impl ClausewitzMergeOutcome {
	pub fn conflicts(&self) -> &[StructuralConflict] {
		&self.kernel.conflicts
	}

	pub fn resolved_ast(&self) -> Option<&AstFile> {
		self.kernel
			.conflicts
			.is_empty()
			.then_some(&self.tentative_ast)
	}

	pub fn tentative_ast(&self) -> &AstFile {
		&self.tentative_ast
	}

	pub fn conflict_summaries(&self) -> Vec<ClausewitzConflictSummary> {
		self.kernel
			.conflicts
			.iter()
			.map(|conflict| ClausewitzConflictSummary {
				kind: conflict_kind_name(conflict.kind),
				detail: conflict.detail.clone(),
			})
			.collect()
	}

	pub fn timings(&self) -> ClausewitzMergeTimings {
		ClausewitzMergeTimings {
			matcher_ns: self.kernel.timings.matcher_ns,
			pcs_ns: self.kernel.timings.pcs_ns,
			policy_ns: self.kernel.timings.policy_ns,
		}
	}

	#[cfg(test)]
	pub(crate) fn kernel(&self) -> &MergeOutcome {
		&self.kernel
	}
}

fn conflict_kind_name(kind: ConflictKind) -> &'static str {
	match kind {
		ConflictKind::AmbiguousMatch => "ambiguous_match",
		ConflictKind::InsertInsert => "insert_insert",
		ConflictKind::DeleteModify => "delete_modify",
		ConflictKind::MoveMove => "move_move",
		ConflictKind::Ordering => "ordering",
		ConflictKind::ValueSlot => "value_slot",
		ConflictKind::DuplicateSignature => "duplicate_signature",
		ConflictKind::Policy => "policy",
	}
}

/// Merge three parseable Clausewitz ASTs without content-family-specific
/// post-processing. The caller owns merge-unit construction and publication.
pub fn merge_clausewitz_files(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_inner(base, left, right, policies, false)
}

pub(crate) fn merge_event_files(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_inner(base, left, right, policies, true)
}

fn merge_clausewitz_files_inner(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
	reduce_event_fallbacks: bool,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	let policy = DefaultClausewitzTreePolicy;
	let base_tree = normalize_ast(base, &policy)?;
	let left_tree = normalize_ast(left, &policy)?;
	let right_tree = normalize_ast(right, &policy)?;
	let kernel_policy = ClausewitzMergePolicy { policies };
	let kernel = three_way_merge_with_policy(&base_tree, &left_tree, &right_tree, &kernel_policy);
	let mut tentative_ast = denormalize_ast(base.path.clone(), kernel.tentative_tree())?;
	if reduce_event_fallbacks {
		reduce_redundant_constructor_fallbacks(&mut tentative_ast.statements);
	}
	Ok(ClausewitzMergeOutcome {
		tentative_ast,
		kernel,
	})
}

#[derive(Clone, Copy, Debug)]
struct ControlFlowChain {
	end: usize,
	defines_ruler_on_all_paths: bool,
	empty_ruler_fallback: Option<usize>,
}

fn reduce_redundant_constructor_fallbacks(statements: &mut Vec<AstStatement>) {
	for statement in statements.iter_mut() {
		let (AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. }) = statement
		else {
			continue;
		};
		if let AstValue::Block { items, .. } = value {
			reduce_redundant_constructor_fallbacks(items);
		}
	}

	let mut removals = Vec::new();
	let mut previous_defines_ruler = false;
	let mut index = 0;
	while index < statements.len() {
		let Some(chain) = inspect_control_flow_chain(statements, index) else {
			previous_defines_ruler = false;
			index += 1;
			continue;
		};
		if previous_defines_ruler && let Some(fallback) = chain.empty_ruler_fallback {
			removals.push(fallback);
		}
		previous_defines_ruler |= chain.defines_ruler_on_all_paths;
		index = chain.end;
	}
	for removal in removals.into_iter().rev() {
		statements.remove(removal);
	}
}

fn inspect_control_flow_chain(
	statements: &[AstStatement],
	start: usize,
) -> Option<ControlFlowChain> {
	if statement_key(statements.get(start)?) != Some("if") {
		return None;
	}
	let mut all_guarded_define_ruler =
		statement_has_top_level_effect(&statements[start], "define_ruler");
	let mut cursor = start + 1;
	let mut terminal_else = None;
	loop {
		let mut branch = cursor;
		while statements
			.get(branch)
			.is_some_and(|statement| matches!(statement, AstStatement::Comment { .. }))
		{
			branch += 1;
		}
		match statements.get(branch).and_then(statement_key) {
			Some("else_if") => {
				all_guarded_define_ruler &=
					statement_has_top_level_effect(&statements[branch], "define_ruler");
				cursor = branch + 1;
			}
			Some("else") => {
				terminal_else = Some(branch);
				cursor = branch + 1;
				break;
			}
			_ => break,
		}
	}
	let else_defines_ruler = terminal_else
		.is_some_and(|branch| statement_has_top_level_effect(&statements[branch], "define_ruler"));
	Some(ControlFlowChain {
		end: cursor,
		defines_ruler_on_all_paths: all_guarded_define_ruler && else_defines_ruler,
		empty_ruler_fallback: terminal_else
			.filter(|branch| is_empty_ruler_fallback(&statements[*branch])),
	})
}

fn statement_key(statement: &AstStatement) -> Option<&str> {
	match statement {
		AstStatement::Assignment { key, .. } => Some(key),
		AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
	}
}

fn statement_has_top_level_effect(statement: &AstStatement, effect: &str) -> bool {
	let AstStatement::Assignment {
		value: AstValue::Block { items, .. },
		..
	} = statement
	else {
		return false;
	};
	items.iter().any(|item| statement_key(item) == Some(effect))
}

fn is_empty_ruler_fallback(statement: &AstStatement) -> bool {
	let AstStatement::Assignment {
		key,
		value: AstValue::Block { items, .. },
		..
	} = statement
	else {
		return false;
	};
	if key != "else" {
		return false;
	}
	let mut effects = items
		.iter()
		.filter(|item| !matches!(item, AstStatement::Comment { .. }));
	let Some(AstStatement::Assignment {
		key,
		value: AstValue::Block { items, .. },
		..
	}) = effects.next()
	else {
		return false;
	};
	key == "define_ruler"
		&& effects.next().is_none()
		&& items
			.iter()
			.all(|item| matches!(item, AstStatement::Comment { .. }))
}

struct ClausewitzMergePolicy<'a> {
	policies: &'a MergePolicies,
}

impl MergePolicy for ClausewitzMergePolicy<'_> {
	fn resolve_delete_unchanged(&self, context: DeleteUnchangedContext<'_>) -> PolicyDecision {
		let scripted_hook_from_missing_container = context
			.base
			.value
			.as_deref()
			.is_some_and(|key| key.starts_with("pre_") || key.starts_with("post_"))
			&& !context.parent_present_in_both_revisions
			&& context.present_parent_changed_from_base;
		let union_safe_control_branch = (context
			.base
			.kind
			.starts_with("clausewitz.control_flow.guarded_branch:")
			|| context
				.base
				.kind
				.starts_with("clausewitz.control_flow.chain:"))
			&& !context.base.kind.contains(":exclusive:")
			&& context.deleted_parent_has_same_kind_sibling
			&& context.parent_present_in_both_revisions;
		let additive_boolean_predicate = context
			.base_parent
			.is_some_and(|parent| is_boolean_block_kind(&parent.kind))
			&& context.parent_present_in_both_revisions
			&& context.present_parent_changed_from_base;
		if self.policies.one_sided_removal == OneSidedRemovalPolicy::PreserveIfParentSurvives
			&& (scripted_hook_from_missing_container
				|| union_safe_control_branch
				|| additive_boolean_predicate)
			&& context.base_parent.is_some_and(|parent| {
				parent.child_cardinality == foch_merge_kernel::ChildCardinality::Many
			}) {
			PolicyDecision::Resolved
		} else {
			PolicyDecision::Unresolved
		}
	}

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

	fn permits_ancestor_closure(&self, node: &foch_merge_kernel::NormalizedNode) -> bool {
		node.kind.starts_with("clausewitz.block")
			|| matches!(
				node.value.as_deref(),
				Some("immediate" | "hidden_effect" | "after")
			)
	}

	fn select_child_revision(&self, context: ChildSetContext<'_>) -> Option<RevisionId> {
		let (Some(base), Some(left), Some(right)) = (context.base, context.left, context.right)
		else {
			return None;
		};
		(is_negated_boolean_block_kind(&base.kind)
			&& left.subtree_hash != base.subtree_hash
			&& right.subtree_hash != base.subtree_hash)
			.then_some(RevisionId::RIGHT)
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

fn is_boolean_block_kind(kind: &str) -> bool {
	matches!(
		kind,
		"clausewitz.block:AND"
			| "clausewitz.block:NAND"
			| "clausewitz.block:NOR"
			| "clausewitz.block:NOT"
			| "clausewitz.block:OR"
	)
}

fn is_negated_boolean_block_kind(kind: &str) -> bool {
	matches!(
		kind,
		"clausewitz.block:NAND" | "clausewitz.block:NOR" | "clausewitz.block:NOT"
	)
}

fn is_scalar_node(node: &foch_merge_kernel::NormalizedNode) -> bool {
	node.kind.starts_with("clausewitz.scalar.") && node.children.is_empty()
}
