use std::fmt;
use std::path::Path;

use crate::game::eu4::content::{
	BlockMergePolicy, DivergentBlockPolicy, MergeKeySource, MergePolicies, NestedInsertionPolicy,
	OneSidedRemovalPolicy, ScalarMergePolicy,
};
use crate::game::eu4::script::parser::{AstStatement, AstValue};
use crate::game::schema::query::{CwtQuery, SchemaScalarType};
use crate::merge::kernel::{
	ChildOrder, ConflictKind, MergePolicy, MergePolicyKind, NWayClassContext, NWayDeleteContext,
	NormalizedNode, PolicyDecision, RevisionId, SemanticKey,
};

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

#[derive(Clone, Copy)]
pub(crate) struct ContentFamilyMergePolicy<'a> {
	policies: &'a MergePolicies,
	fields: Option<SchemaFields<'a>>,
}

impl fmt::Debug for ContentFamilyMergePolicy<'_> {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("ContentFamilyMergePolicy")
			.field("policies", self.policies)
			.field("schema_fields", &self.fields.is_some())
			.finish()
	}
}

/// The active CWT schema, bound to the file being merged.
///
/// Held as a pair rather than a resolved binding because a file can match more
/// than one root type, which `scalar_field_type` resolves per path.
#[derive(Clone, Copy)]
pub(crate) struct SchemaFields<'a> {
	schema: &'a CwtQuery,
	file_path: &'a Path,
}

impl<'a> ContentFamilyMergePolicy<'a> {
	pub(crate) const fn new(policies: &'a MergePolicies) -> Self {
		Self {
			policies,
			fields: None,
		}
	}

	/// Adds the schema evidence needed to tell a spelling difference from a
	/// value difference. Without it the policy keeps its schema-blind
	/// behavior, which is what happens wherever no CWT schema is installed.
	pub(crate) const fn with_schema_fields(
		policies: &'a MergePolicies,
		schema: Option<&'a CwtQuery>,
		file_path: &'a Path,
	) -> Self {
		Self {
			policies,
			fields: match schema {
				Some(schema) => Some(SchemaFields { schema, file_path }),
				None => None,
			},
		}
	}

	/// Resolves a divergence that only exists in the file text.
	///
	/// EU4 reads a script number through a reader far coarser than byte
	/// comparison, so contributors can write one value several ways.
	/// `0.5`, `0.50` and `0.500` are one value to the game, and so are
	/// `0.1234` and `0.1239`; reporting those as content conflicts asks the
	/// user to adjudicate a difference the game cannot see.
	///
	/// This runs after the conflict is already detected, and deliberately so.
	/// Making the leaves or their hashes coercion-aware would move subtree
	/// hashes, the tree matcher's buckets and the anchors that embed scalar
	/// text, and the same pair would then compare equal under one parent and
	/// unequal under another. Here the class is formed and the divergence
	/// marked; only the verdict changes.
	///
	/// Which reader applies is a property of the field, not of the text, so
	/// this acts only where the CWT schema states the field's type. That
	/// bounds the benefit: EU4's schema coverage is partial, and where it is
	/// silent the divergence stays a conflict.
	fn game_value_equivalence(&self, context: NWayClassContext<'_>) -> Option<PolicyDecision> {
		let fields = self.fields?;
		let first = context.contributors.first()?;
		// Only an assignment's own value is the field the schema types. A list
		// item is matched by position and its text is not a field value, so
		// the schema would be answering about the wrong thing.
		let key = assignment_key(first.parent?)?;
		let chains = script_key_chains(&first.node.policy_path, key)?;
		let mut scalar_type: Option<SchemaScalarType> = None;
		for chain in &chains {
			let chain = chain.iter().map(String::as_str).collect::<Vec<_>>();
			let Some(resolved) = crate::game::eu4::cwt::merge::scalar_field_type(
				fields.schema,
				fields.file_path,
				&chain,
			) else {
				continue;
			};
			match scalar_type {
				None => scalar_type = Some(resolved),
				Some(previous) if previous.is_same_primitive(resolved) => {}
				// Two readings of one path that disagree about the reader mean
				// the path is ambiguous, not that either answer is right.
				Some(_) => return None,
			}
		}
		let scalar_type = scalar_type?;
		let values = context
			.contributors
			.iter()
			.map(|view| {
				(view.node.kind == first.node.kind)
					.then_some(view.node.value.as_deref())
					.flatten()
			})
			.collect::<Option<Vec<_>>>()?;
		if values.len() < 2 {
			return None;
		}
		if !reads_as_one_value(scalar_type, &values) {
			return None;
		}
		Some(PolicyDecision::SynthesizeScalar {
			// Keep a spelling that was actually written, and take it from the
			// last contributor, because playset order is the precedence the
			// rest of the merge already follows. Emitting the reader's own
			// canonical form instead would put a number in the output that no
			// contributor wrote.
			value: values.last()?.to_string(),
			policy: MergePolicyKind::GameValueEquivalence,
		})
	}
}

/// Whether every contributor's text reaches the same value through the reader
/// this field's type selects.
fn reads_as_one_value(scalar_type: SchemaScalarType, values: &[&str]) -> bool {
	use crate::game::eu4::coercion::{script_fixed_point, script_int};

	match scalar_type {
		SchemaScalarType::Float { .. } => {
			let Some(first) = script_fixed_point(values[0]) else {
				return false;
			};
			values[1..]
				.iter()
				.all(|value| script_fixed_point(value) == Some(first))
		}
		SchemaScalarType::Int { .. } => {
			let Some(first) = script_int(values[0]) else {
				return false;
			};
			values[1..]
				.iter()
				.all(|value| script_int(value) == Some(first))
		}
		// `CToken::GetBool` compares against the exact lowercase `yes`, so
		// every other spelling reads as false and would collapse together —
		// including the mis-spellings the editor reports as errors. Equating
		// them here would hide the divergence rather than explain it.
		SchemaScalarType::Bool => false,
	}
}

/// The script key chains a normalized node's policy path can stand for.
///
/// Policy-path components are anchor values, and an anchor is either the bare
/// assignment key or that key followed by `:` and an identity — the shapes
/// `assignment_anchor` builds. The two are not distinguishable as text,
/// because a script key may itself contain a colon: vanilla writes
/// `event_target:<name>` as an assignment key roughly 1,900 times. So a
/// colon-bearing component yields both readings and the caller must accept
/// only an answer they agree on.
///
/// The leaf's own component is not guessed. `leaf_key` is read off the parent
/// assignment node, and a last component that matches neither reading of it
/// means this path is not a plain assignment chain.
fn script_key_chains(policy_path: &[String], leaf_key: &str) -> Option<Vec<Vec<String>>> {
	let (last, ancestors) = policy_path.split_last()?;
	if last != leaf_key && last.split_once(':').map(|(key, _)| key) != Some(leaf_key) {
		return None;
	}
	// Each ambiguous ancestor doubles the candidates. Two is already more
	// uncertainty than an answer should be built on.
	const MAX_AMBIGUOUS_ANCESTORS: usize = 2;
	if ancestors
		.iter()
		.filter(|component| component.contains(':'))
		.count()
		> MAX_AMBIGUOUS_ANCESTORS
	{
		return None;
	}
	let mut chains = vec![Vec::with_capacity(policy_path.len())];
	for component in ancestors {
		chains = match component.split_once(':') {
			Some((key, _)) => chains
				.into_iter()
				.flat_map(|chain| {
					let mut decorated = chain.clone();
					decorated.push(component.clone());
					let mut bare = chain;
					bare.push(key.to_string());
					[decorated, bare]
				})
				.collect(),
			None => chains
				.into_iter()
				.map(|mut chain| {
					chain.push(component.clone());
					chain
				})
				.collect(),
		};
	}
	for chain in &mut chains {
		chain.push(leaf_key.to_string());
	}
	Some(chains)
}

fn assignment_key(node: &NormalizedNode) -> Option<&str> {
	node.kind
		.strip_prefix("clausewitz.assignment:")
		.and(node.value.as_deref())
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
		if let Some(identity) =
			crate::merge::gui::scroll_stack_variant_identity(self.policies, key, value)
		{
			return Some(SemanticKey::parent_scoped(
				"clausewitz.scroll_stack.variant",
				identity,
			));
		}
		if self.policies.divergent_block_policy_for_key(key) == DivergentBlockPolicy::Union
			&& !matches!(value, AstValue::Block { .. })
		{
			return Some(union_assignment_anchor(key, value));
		}
		if parent_assignment_key.is_some_and(|parent| {
			self.policies.divergent_block_policy_for_key(parent) == DivergentBlockPolicy::Union
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
				self.policies.divergent_block_policy_for_key(parent)
					== crate::game::eu4::content::DivergentBlockPolicy::Union
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
	fn select_nway_deleted_subtree(&self, context: NWayDeleteContext<'_>) -> Option<RevisionId> {
		let content_changed = context
			.class
			.contributors
			.iter()
			.any(|view| view.subtree_changed);
		if !content_changed && self.resolve_nway_delete(context) == PolicyDecision::Resolved {
			return context
				.class
				.contributors
				.last()
				.map(|view| view.source.revision);
		}

		let base = context.class.base?;
		if !base
			.node
			.kind
			.starts_with("clausewitz.control_flow.guarded_branch:")
			|| base.node.kind.contains(":exclusive:")
			|| !context.deleted_parent_has_same_kind_gap_replacement
			|| !context.parent_present_in_all_revisions
		{
			return None;
		}
		context
			.class
			.contributors
			.last()
			.map(|view| view.source.revision)
	}

	fn resolve_nway_delete(&self, context: NWayDeleteContext<'_>) -> PolicyDecision {
		let content_changed = context
			.class
			.contributors
			.iter()
			.any(|view| view.subtree_changed);
		let reparented = context
			.class
			.contributors
			.iter()
			.any(|view| view.reparented);
		let reordered = context.class.contributors.iter().any(|view| view.reordered);
		if self.policies.edit_wins_over_remove && content_changed && !reparented && !reordered {
			return PolicyDecision::Resolved;
		}

		let Some(base) = context.class.base else {
			return PolicyDecision::Unresolved;
		};
		let present_parent_changed_from_base = context
			.class
			.contributors
			.iter()
			.any(|view| view.parent_changed_from_base);
		let scripted_hook_from_missing_container = base
			.node
			.value
			.as_deref()
			.is_some_and(|key| key.starts_with("pre_") || key.starts_with("post_"))
			&& !context.parent_present_in_all_revisions
			&& present_parent_changed_from_base;
		let union_safe_control_branch = (base
			.node
			.kind
			.starts_with("clausewitz.control_flow.guarded_branch:")
			|| base.node.kind.starts_with("clausewitz.control_flow.chain:"))
			&& !base.node.kind.contains(":exclusive:")
			&& context.deleted_parent_has_same_kind_gap_replacement
			&& context.parent_present_in_all_revisions;
		let additive_boolean_predicate = base
			.parent
			.is_some_and(|parent| is_boolean_block_kind(&parent.kind))
			&& context.parent_present_in_all_revisions
			&& present_parent_changed_from_base;
		let boolean_alternative = base
			.parent
			.is_some_and(|parent| parent.kind == "clausewitz.block:OR")
			&& context.parent_present_in_all_revisions;
		let union_block_member = base
			.parent
			.and_then(|parent| block_assignment_key(&parent.kind))
			.is_some_and(|key| {
				self.policies.divergent_block_policy_for_key(key) == DivergentBlockPolicy::Union
			}) && context.parent_present_in_all_revisions
			&& present_parent_changed_from_base;
		let preserve = union_block_member
			|| match self.policies.one_sided_removal {
				OneSidedRemovalPolicy::Remove => false,
				OneSidedRemovalPolicy::PreserveIfParentSurvives => {
					context.parent_present_in_all_revisions
				}
				OneSidedRemovalPolicy::PreserveAdditiveStructure => {
					scripted_hook_from_missing_container
						|| union_safe_control_branch
						|| additive_boolean_predicate
				}
				OneSidedRemovalPolicy::PreserveBooleanAlternatives => boolean_alternative,
			};
		if preserve
			&& base.parent.is_some_and(|parent| {
				parent.child_cardinality == crate::merge::kernel::ChildCardinality::Many
			}) {
			PolicyDecision::Resolved
		} else {
			PolicyDecision::Unresolved
		}
	}

	fn select_nway_subtree(&self, context: NWayClassContext<'_>) -> Option<RevisionId> {
		let base = context.base?;
		let changed = context
			.contributors
			.iter()
			.filter(|view| view.subtree_changed)
			.collect::<Vec<_>>();
		if changed.len() < 2 {
			return None;
		}
		let selected = changed.last()?.source.revision;
		if is_negated_boolean_block_kind(&base.node.kind) {
			return Some(selected);
		}
		let key = block_assignment_key(&base.node.kind)?;
		(self.policies.block == BlockMergePolicy::Replace
			&& self.policies.divergent_block_policy_for_key(key) == DivergentBlockPolicy::Recurse)
			.then_some(selected)
	}

	fn resolve_nway_divergent_node(&self, context: NWayClassContext<'_>) -> PolicyDecision {
		let changed = context
			.contributors
			.iter()
			.filter(|view| context.base.is_none() || view.shallow_changed)
			.collect::<Vec<_>>();
		if !matches!(
			context.kind,
			ConflictKind::InsertInsert | ConflictKind::Policy
		) || context
			.contributors
			.iter()
			.any(|view| !is_scalar_node(view.node))
		{
			return PolicyDecision::Unresolved;
		}
		let Some(first) = context.contributors.first().map(|view| view.node) else {
			return PolicyDecision::Unresolved;
		};
		if context
			.contributors
			.iter()
			.skip(1)
			.any(|view| view.node.policy_path != first.policy_path)
		{
			return PolicyDecision::Unresolved;
		}
		let values = || {
			context
				.contributors
				.iter()
				.map(|view| view.node.value.as_deref())
				.collect::<Option<Vec<_>>>()
		};
		// Ask whether the contributors ever disagreed before asking a reducer
		// to combine them: a reducer handed one value written two ways would
		// average or sum spellings of the same number.
		if let Some(decision) = self.game_value_equivalence(context) {
			return decision;
		}
		if let Some(rule) = self
			.policies
			.scalar_reducer_rule_for_path(&first.policy_path)
			&& let Some(values) = values()
			&& let Some(output) = rule.reducer.reduce_numeric_values(values)
		{
			return PolicyDecision::SynthesizeScalar {
				value: output,
				policy: MergePolicyKind::ScalarReducer,
			};
		}
		if let Some(values) = values()
			&& let Some(output) = self.policies.scalar.reduce_numeric_values(values)
		{
			return PolicyDecision::SynthesizeScalar {
				value: output,
				policy: MergePolicyKind::ScalarReducer,
			};
		}
		if changed.len() >= 2 && self.policies.scalar == ScalarMergePolicy::LastWriter {
			PolicyDecision::Select(changed.last().unwrap().source.revision)
		} else {
			PolicyDecision::Unresolved
		}
	}

	fn permits_ancestor_closure(&self, node: &crate::merge::kernel::NormalizedNode) -> bool {
		node.kind.starts_with("clausewitz.block")
			|| matches!(
				node.value.as_deref(),
				Some("immediate" | "hidden_effect" | "after")
			)
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

fn is_scalar_node(node: &crate::merge::kernel::NormalizedNode) -> bool {
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
	{
		// Trigger canonicalization can place an identity field below transparent
		// boolean wrappers before the tree matcher sees this assignment.
		if let Some(identity) = scalar_field_through_boolean_wrappers(value, child_key_field)
			.filter(|identity| !identity.trim().is_empty())
		{
			return Some(nested_insertion_anchor(
				policies,
				SemanticKey::parent_scoped(
					"clausewitz.assignment.identity",
					format!("{key}:{identity}"),
				),
			));
		}
		if child_types.contains(&key) {
			// A configured child without a usable identity must not match a child
			// that has one. Keep identity-less siblings on conservative similarity
			// matching because placeholder values such as `tooltip = " "` repeat.
			return Some(nested_insertion_anchor(
				policies,
				SemanticKey::parent_scoped_ordered_similarity_with_position(
					"clausewitz.assignment.identity",
					format!("{key}:<missing-{child_key_field}>"),
				),
			));
		}
	}
	match policies.merge_key_source {
		MergeKeySource::AssignmentKey
		| MergeKeySource::ContainerChildKey
		| MergeKeySource::ContainerChildFieldValue { .. }
		| MergeKeySource::LeafPath => None,
		MergeKeySource::ChildFieldValue {
			child_key_field,
			child_types,
		} => {
			if !child_types.is_empty() && !child_types.contains(&key) {
				return Some(assignment_key_anchor(key));
			}
			if let Some(identity) =
				scalar_field(value, child_key_field).filter(|identity| !identity.trim().is_empty())
			{
				return Some(SemanticKey::parent_scoped(
					"clausewitz.assignment.identity",
					format!("{key}:{identity}"),
				));
			}
			child_types.contains(&key).then(|| {
				SemanticKey::parent_scoped_ordered_similarity_with_position(
					"clausewitz.assignment.identity",
					format!("{key}:<missing-{child_key_field}>"),
				)
			})
		}
		MergeKeySource::FieldValue(_) => None,
	}
}

fn nested_insertion_anchor(policies: &MergePolicies, anchor: SemanticKey) -> SemanticKey {
	match policies.nested_insertion {
		NestedInsertionPolicy::MatchByKey => anchor,
		NestedInsertionPolicy::SourceIsolatedAppend => anchor.requiring_seeded_lineage(),
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

fn scalar_field_through_boolean_wrappers(value: &AstValue, field: &str) -> Option<String> {
	if let Some(value) = scalar_field(value, field) {
		return Some(value);
	}
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	items.iter().find_map(|statement| {
		let AstStatement::Assignment { key, value, .. } = statement else {
			return None;
		};
		matches!(key.as_str(), "AND" | "OR")
			.then(|| scalar_field_through_boolean_wrappers(value, field))
			.flatten()
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

	use crate::game::eu4::content::{MergeKeySource, MergePolicies, NestedInsertionPolicy};
	use crate::game::eu4::script::parser::{AstStatement, AstValue, parse_clausewitz_content};
	use crate::merge::kernel::{SemanticKeyLineage, SemanticKeyMatchMode, SemanticKeyScope};

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

	#[test]
	fn nested_child_field_policy_finds_canonicalized_trigger_identity() {
		let policies = MergePolicies {
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "tooltip",
				child_types: &["condition"],
			},
			..MergePolicies::default()
		};
		let policy = ContentFamilyMergePolicy::new(&policies);
		let (key, value) =
			assignment("condition = { OR = { AND = { tooltip = SHARED_PEACE_BLOCK } } }");

		let anchor = policy
			.assignment_anchor(None, &key, &value)
			.expect("canonicalized condition identity");

		assert_eq!(anchor.scope, SemanticKeyScope::Parent);
		assert_eq!(anchor.value, "condition:SHARED_PEACE_BLOCK");
	}

	#[test]
	fn nested_child_field_policy_uses_soft_missing_identity_for_nested_predicates() {
		let policies = MergePolicies {
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "tooltip",
				child_types: &["condition"],
			},
			..MergePolicies::default()
		};
		let policy = ContentFamilyMergePolicy::new(&policies);
		let (key, value) = assignment(
			"condition = { potential = { custom_trigger_tooltip = { tooltip = NESTED } } }",
		);

		let anchor = policy
			.assignment_anchor(None, &key, &value)
			.expect("identity-less condition anchor");

		assert_eq!(anchor.namespace, "clausewitz.assignment.identity");
		assert_eq!(anchor.value, "condition:<missing-tooltip>");
		assert_eq!(
			anchor.match_mode,
			SemanticKeyMatchMode::OrderedSimilarityWithPosition
		);
	}

	#[test]
	fn nested_child_field_policy_treats_blank_placeholder_as_missing_identity() {
		let policies = MergePolicies {
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "tooltip",
				child_types: &["condition"],
			},
			..MergePolicies::default()
		};
		let policy = ContentFamilyMergePolicy::new(&policies);
		let (key, value) = assignment("condition = { tooltip = \" \" }");

		let anchor = policy
			.assignment_anchor(None, &key, &value)
			.expect("blank condition anchor");

		assert_eq!(anchor.namespace, "clausewitz.assignment.identity");
		assert_eq!(anchor.value, "condition:<missing-tooltip>");
		assert_eq!(
			anchor.match_mode,
			SemanticKeyMatchMode::OrderedSimilarityWithPosition
		);
	}

	#[test]
	fn source_isolated_nested_policy_marks_keyed_and_blank_children_for_seeded_lineage() {
		let policies = MergePolicies {
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "tooltip",
				child_types: &["condition"],
			},
			nested_insertion: NestedInsertionPolicy::SourceIsolatedAppend,
			..MergePolicies::default()
		};
		let policy = ContentFamilyMergePolicy::new(&policies);

		for source in [
			"condition = { tooltip = SHARED_PEACE_BLOCK }",
			"condition = { tooltip = \" \" }",
		] {
			let (key, value) = assignment(source);
			let anchor = policy
				.assignment_anchor(None, &key, &value)
				.expect("configured condition anchor");
			assert_eq!(anchor.lineage, SemanticKeyLineage::Seeded);
		}
	}

	#[test]
	fn a_key_chain_reads_an_identity_anchor_both_ways() {
		// `country_event:demo.1` is an identity anchor over the key
		// `country_event`; `event_target:my_target` is a script key that
		// simply contains a colon. Nothing in the text tells them apart.
		let path = [
			"country_event:demo.1".to_string(),
			"immediate".to_string(),
			"add_prestige".to_string(),
		];

		let chains = super::script_key_chains(&path, "add_prestige").expect("chains");

		assert_eq!(
			chains,
			vec![
				vec![
					"country_event:demo.1".to_string(),
					"immediate".to_string(),
					"add_prestige".to_string()
				],
				vec![
					"country_event".to_string(),
					"immediate".to_string(),
					"add_prestige".to_string()
				],
			]
		);
	}

	#[test]
	fn a_key_chain_without_decoration_has_one_reading() {
		let path = ["a_thing".to_string(), "upkeep".to_string()];

		assert_eq!(
			super::script_key_chains(&path, "upkeep"),
			Some(vec![vec!["a_thing".to_string(), "upkeep".to_string()]])
		);
	}

	#[test]
	fn a_key_chain_accepts_a_decorated_leaf_component() {
		let path = ["a_thing".to_string(), "option:accept".to_string()];

		assert_eq!(
			super::script_key_chains(&path, "option"),
			Some(vec![vec!["a_thing".to_string(), "option".to_string()]])
		);
	}

	#[test]
	fn a_key_chain_refuses_a_leaf_that_is_not_the_assignment_key() {
		// A positional or fingerprint anchor is not this field's key, so the
		// path is not a plain assignment chain and nothing may be read off it.
		let path = ["a_thing".to_string(), "0".to_string()];

		assert_eq!(super::script_key_chains(&path, "upkeep"), None);
		assert_eq!(super::script_key_chains(&[], "upkeep"), None);
	}

	#[test]
	fn a_key_chain_refuses_more_ambiguity_than_it_can_justify() {
		let path = [
			"a:1".to_string(),
			"b:2".to_string(),
			"c:3".to_string(),
			"upkeep".to_string(),
		];

		assert_eq!(super::script_key_chains(&path, "upkeep"), None);
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
