use std::collections::BTreeMap;

use foch_language::analyzer::content_family::{
	BlockMergePolicy, BlockPatchPolicy, MergeKeySource, MergePolicies, OneSidedRemovalPolicy,
	ScalarMergePolicy,
};
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_merge_kernel::{
	ChildOrder, ChildSetContext, ConflictKind, DeleteModifyContext, DeleteUnchangedContext,
	MergePolicy, NodeConflictContext, PolicyDecision, RevisionId, SemanticKey,
};

pub(crate) type LocalSourceSelections = BTreeMap<Vec<String>, RevisionId>;

pub(crate) trait ClausewitzTreePolicy {
	fn assignment_anchor(
		&self,
		parent_assignment_key: Option<&str>,
		key: &str,
		value: &AstValue,
	) -> Option<SemanticKey>;

	fn item_anchor(
		&self,
		_parent_assignment_key: Option<&str>,
		_value: &AstValue,
	) -> Option<SemanticKey> {
		None
	}

	fn assignment_signature(&self, _key: &str, _value: &AstValue) -> Option<String> {
		None
	}

	fn block_child_order(&self, _assignment_key: Option<&str>) -> ChildOrder {
		match _assignment_key {
			Some("OR") => ChildOrder::Commutative,
			_ => ChildOrder::Ordered,
		}
	}
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ContentFamilyMergePolicy<'a> {
	policies: &'a MergePolicies,
	source_selections: Option<&'a LocalSourceSelections>,
}

impl<'a> ContentFamilyMergePolicy<'a> {
	pub(crate) const fn new(policies: &'a MergePolicies) -> Self {
		Self {
			policies,
			source_selections: None,
		}
	}

	pub(crate) const fn with_source_selections(
		policies: &'a MergePolicies,
		source_selections: &'a LocalSourceSelections,
	) -> Self {
		Self {
			policies,
			source_selections: Some(source_selections),
		}
	}

	fn forced_revision(
		&self,
		base: Option<&foch_merge_kernel::NormalizedNode>,
		left: Option<&foch_merge_kernel::NormalizedNode>,
		right: Option<&foch_merge_kernel::NormalizedNode>,
	) -> Option<RevisionId> {
		let selections = self.source_selections?;
		[left, right, base]
			.into_iter()
			.flatten()
			.find_map(|node| selections.get(&node.policy_path).copied())
	}
}

impl ClausewitzTreePolicy for ContentFamilyMergePolicy<'_> {
	fn assignment_anchor(
		&self,
		parent_assignment_key: Option<&str>,
		key: &str,
		value: &AstValue,
	) -> Option<SemanticKey> {
		if matches!(key, "if" | "else_if" | "else") && matches!(value, AstValue::Block { .. }) {
			return None;
		}
		if parent_assignment_key.is_some_and(|parent| {
			self.policies.block_patch_policy_for_key(parent)
				== foch_language::analyzer::content_family::BlockPatchPolicy::Union
		}) {
			return Some(union_assignment_anchor(key, value));
		}
		if let Some(anchor) = content_family_anchor(self.policies, key, value) {
			return Some(anchor);
		}
		match key {
			"country_event" | "province_event" => scalar_field(value, "id").map(|id| {
				SemanticKey::new("clausewitz.assignment.identity", format!("{key}:{id}"))
			}),
			"option" => scalar_field(value, "name").map(|name| {
				SemanticKey::parent_scoped(
					"clausewitz.assignment.identity",
					format!("option:{name}"),
				)
			}),
			"desc" | "triggered_desc" => scalar_field(value, "desc").map(|desc| {
				SemanticKey::parent_scoped(
					"clausewitz.assignment.identity",
					format!("{key}:{desc}"),
				)
			}),
			_ => Some(SemanticKey::parent_scoped("clausewitz.assignment.key", key)),
		}
	}

	fn item_anchor(
		&self,
		parent_assignment_key: Option<&str>,
		value: &AstValue,
	) -> Option<SemanticKey> {
		parent_assignment_key
			.is_some_and(|parent| {
				self.policies.block_patch_policy_for_key(parent)
					== foch_language::analyzer::content_family::BlockPatchPolicy::Union
			})
			.then(|| {
				SemanticKey::parent_scoped(
					"clausewitz.union.item",
					crate::merge::semantic_fingerprint::value_fingerprint(value),
				)
			})
	}

	fn assignment_signature(&self, key: &str, value: &AstValue) -> Option<String> {
		match key {
			"option" => scalar_field(value, "name").map(|name| format!("option:{name}")),
			"modifier" => descendant_scalar_field(value, "tooltip")
				.map(|tooltip| format!("modifier:tooltip:{tooltip}")),
			_ => None,
		}
	}
}

impl MergePolicy for ContentFamilyMergePolicy<'_> {
	fn resolve_delete_unchanged(&self, context: DeleteUnchangedContext<'_>) -> PolicyDecision {
		if let Some(revision) =
			self.forced_revision(Some(context.base), Some(context.present), None)
		{
			return PolicyDecision::Select(revision);
		}
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
			&& context.deleted_parent_has_same_kind_gap_replacement
			&& context.parent_present_in_both_revisions;
		let additive_boolean_predicate = context
			.base_parent
			.is_some_and(|parent| is_boolean_block_kind(&parent.kind))
			&& context.parent_present_in_both_revisions
			&& context.present_parent_changed_from_base;
		let boolean_alternative = context
			.base_parent
			.is_some_and(|parent| parent.kind == "clausewitz.block:OR")
			&& context.parent_present_in_both_revisions;
		let union_block_member = context
			.base_parent
			.and_then(|parent| block_assignment_key(&parent.kind))
			.is_some_and(|key| {
				self.policies.block_patch_policy_for_key(key) == BlockPatchPolicy::Union
			}) && context.parent_present_in_both_revisions
			&& context.present_parent_changed_from_base;
		let preserve = union_block_member
			|| match self.policies.one_sided_removal {
				OneSidedRemovalPolicy::Remove => false,
				OneSidedRemovalPolicy::PreserveIfParentSurvives => {
					context.parent_present_in_both_revisions
				}
				OneSidedRemovalPolicy::PreserveAdditiveStructure => {
					scripted_hook_from_missing_container
						|| union_safe_control_branch
						|| additive_boolean_predicate
				}
				OneSidedRemovalPolicy::PreserveBooleanAlternatives => boolean_alternative,
			};
		if preserve
			&& context.base_parent.is_some_and(|parent| {
				parent.child_cardinality == foch_merge_kernel::ChildCardinality::Many
			}) {
			PolicyDecision::Resolved
		} else {
			PolicyDecision::Unresolved
		}
	}

	fn resolve_delete_modify(&self, context: DeleteModifyContext<'_>) -> PolicyDecision {
		if let Some(revision) =
			self.forced_revision(Some(context.base), Some(context.present), None)
		{
			return PolicyDecision::Select(revision);
		}
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

	fn select_subtree_revision(&self, context: ChildSetContext<'_>) -> Option<RevisionId> {
		if let Some(revision) = self.forced_revision(context.base, context.left, context.right) {
			return Some(revision);
		}
		let (Some(base), Some(left), Some(right)) = (context.base, context.left, context.right)
		else {
			return None;
		};
		let both_changed =
			left.subtree_hash != base.subtree_hash && right.subtree_hash != base.subtree_hash;
		if !both_changed {
			return None;
		}
		if is_negated_boolean_block_kind(&base.kind) {
			return Some(RevisionId::RIGHT);
		}
		let key = block_assignment_key(&base.kind)?;
		(self.policies.block == BlockMergePolicy::Replace
			&& self.policies.block_patch_policy_for_key(key) == BlockPatchPolicy::Recurse)
			.then_some(RevisionId::RIGHT)
	}

	fn resolve_divergent_node(&self, context: NodeConflictContext<'_>) -> PolicyDecision {
		if let Some(revision) = self.forced_revision(context.base, context.left, context.right) {
			return PolicyDecision::Select(revision);
		}
		let scalar_conflict = matches!(
			context.kind,
			ConflictKind::InsertInsert | ConflictKind::Policy
		) && context.left.is_some_and(is_scalar_node)
			&& context.right.is_some_and(is_scalar_node);
		if !scalar_conflict {
			return PolicyDecision::Unresolved;
		}
		let left = context.left.expect("scalar conflict has left node");
		let right = context.right.expect("scalar conflict has right node");
		if left.policy_path == right.policy_path
			&& let Some(rule) = self
				.policies
				.scalar_reducer_rule_for_path(&left.policy_path)
			&& let (Some(left_value), Some(right_value)) =
				(left.value.as_deref(), right.value.as_deref())
			&& let Some(output) = rule.reducer.reduce_numeric_pair(left_value, right_value)
		{
			return PolicyDecision::SynthesizeScalar(output);
		}
		if let (Some(left_value), Some(right_value)) =
			(left.value.as_deref(), right.value.as_deref())
			&& let Some(output) = self
				.policies
				.scalar
				.reduce_numeric_pair(left_value, right_value)
		{
			return PolicyDecision::SynthesizeScalar(output);
		}
		if self.policies.scalar == ScalarMergePolicy::LastWriter {
			PolicyDecision::Select(RevisionId::RIGHT)
		} else {
			PolicyDecision::Unresolved
		}
	}
}

fn block_assignment_key(kind: &str) -> Option<&str> {
	kind.strip_prefix("clausewitz.block:")
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

fn union_assignment_anchor(key: &str, value: &AstValue) -> SemanticKey {
	SemanticKey::parent_scoped(
		"clausewitz.union.assignment",
		format!(
			"{key}:{}",
			crate::merge::semantic_fingerprint::value_fingerprint(value)
		),
	)
}

fn content_family_anchor(
	policies: &MergePolicies,
	key: &str,
	value: &AstValue,
) -> Option<SemanticKey> {
	if let MergeKeySource::FieldValue(field) = policies.merge_key_source
		&& let Some(identity) = scalar_field(value, field)
	{
		return Some(SemanticKey::new(
			"clausewitz.assignment.identity",
			format!("{key}:{identity}"),
		));
	}
	if let MergeKeySource::ChildFieldValue {
		child_key_field,
		child_types,
	} = policies.nested_merge_key_source
		&& (child_types.is_empty() || child_types.contains(&key))
		&& let Some(identity) = scalar_field(value, child_key_field)
	{
		return Some(SemanticKey::parent_scoped(
			"clausewitz.assignment.identity",
			format!("{key}:{identity}"),
		));
	}
	match policies.merge_key_source {
		MergeKeySource::AssignmentKey
		| MergeKeySource::ContainerChildKey
		| MergeKeySource::ContainerChildFieldValue { .. }
		| MergeKeySource::LeafPath => Some(assignment_key_anchor(key)),
		MergeKeySource::ChildFieldValue {
			child_key_field,
			child_types,
		} => {
			if !child_types.is_empty() && !child_types.contains(&key) {
				return Some(assignment_key_anchor(key));
			}
			scalar_field(value, child_key_field).map(|identity| {
				SemanticKey::parent_scoped(
					"clausewitz.assignment.identity",
					format!("{key}:{identity}"),
				)
			})
		}
		MergeKeySource::FieldValue(_) => None,
	}
}

fn assignment_key_anchor(key: &str) -> SemanticKey {
	SemanticKey::parent_scoped("clausewitz.assignment.key", key)
}

fn scalar_field(value: &AstValue, field: &str) -> Option<String> {
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	items.iter().find_map(|statement| {
		let AstStatement::Assignment { key, value, .. } = statement else {
			return None;
		};
		if key != field {
			return None;
		}
		let AstValue::Scalar { value, .. } = value else {
			return None;
		};
		Some(value.as_text())
	})
}

fn descendant_scalar_field(value: &AstValue, field: &str) -> Option<String> {
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	items.iter().find_map(|statement| {
		let (key, value) = match statement {
			AstStatement::Assignment { key, value, .. } => (Some(key.as_str()), value),
			AstStatement::Item { value, .. } => (None, value),
			AstStatement::Comment { .. } => return None,
		};
		if key == Some(field)
			&& let AstValue::Scalar { value, .. } = value
		{
			return Some(value.as_text());
		}
		descendant_scalar_field(value, field)
	})
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use foch_language::analyzer::content_family::{MergeKeySource, MergePolicies};
	use foch_language::analyzer::parser::{AstStatement, AstValue, parse_clausewitz_content};
	use foch_merge_kernel::SemanticKeyScope;

	use super::{ClausewitzTreePolicy, ContentFamilyMergePolicy};

	#[test]
	fn field_value_policy_supplies_root_identity() {
		let mut policies = MergePolicies {
			merge_key_source: MergeKeySource::FieldValue("name"),
			..MergePolicies::default()
		};
		policies.nested_merge_key_source = policies.merge_key_source.nested();
		let policy = ContentFamilyMergePolicy::new(&policies);
		let (key, value) = assignment("sound = { name = first }");

		let anchor = policy
			.assignment_anchor(None, &key, &value)
			.expect("field identity");

		assert_eq!(anchor.scope, SemanticKeyScope::Global);
		assert_eq!(anchor.value, "sound:first");
	}

	#[test]
	fn nested_child_field_policy_supplies_parent_identity() {
		let policies = MergePolicies {
			merge_key_source: MergeKeySource::FieldValue("id"),
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "name",
				child_types: &["option"],
			},
			..MergePolicies::default()
		};
		let policy = ContentFamilyMergePolicy::new(&policies);
		let (key, value) = assignment("option = { name = accept }");

		let anchor = policy
			.assignment_anchor(None, &key, &value)
			.expect("nested identity");

		assert_eq!(anchor.scope, SemanticKeyScope::Parent);
		assert_eq!(anchor.value, "option:accept");
	}

	fn assignment(source: &str) -> (String, AstValue) {
		let parsed = parse_clausewitz_content(PathBuf::from("test.txt"), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		let [AstStatement::Assignment { key, value, .. }] = parsed.ast.statements.as_slice() else {
			panic!("expected one assignment")
		};
		(key.clone(), value.clone())
	}
}
