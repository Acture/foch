use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use foch_core::model::ScopeKind;
use foch_language::analyzer::content_family::{
	BlockPatchPolicy, BooleanMergePolicy, CwtType, MergePolicies, ScalarMergePolicy,
};
use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue};
use foch_language::analyzer::semantic_index::{classify_script_file, script_container_scope_kind};
use foch_merge_kernel::{
	ConflictKind, MergeOutcome, NodeId, NormalizedNode, NormalizedTree, RevisionId, SourceSet,
	StructuralConflict, three_way_merge_with_policy,
};

use crate::merge::boolean::{canonical_boolean_or_body, simplify_boolean_or_body};

use super::ast_adapter::{
	AstAdapterError, denormalize_ast, normalize_ast, normalize_ast_with_findings,
};
use super::policy::{ContentFamilyMergePolicy, LocalSourceSelections};
use super::trivia::{attach_trivia, detach_trivia, merge_trivia};

#[derive(Clone, Debug)]
pub struct ClausewitzMergeOutcome {
	tentative_ast: AstFile,
	kernel: MergeOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClausewitzConflictSummary {
	pub kind: &'static str,
	pub semantic_path: Vec<String>,
	pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClausewitzScalarReduction {
	pub path: Vec<String>,
	pub inputs: Vec<(RevisionId, String)>,
	pub output: String,
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
				semantic_path: conflict.semantic_path.clone(),
				detail: conflict.detail.clone(),
			})
			.collect()
	}

	pub fn scalar_reductions(&self) -> Vec<ClausewitzScalarReduction> {
		self.kernel
			.tentative_tree()
			.nodes()
			.filter_map(|(_, node)| {
				Some(ClausewitzScalarReduction {
					path: node.scalar_reducer_path.clone()?,
					inputs: node.scalar_reducer_inputs.clone(),
					output: node.scalar_reducer_output.clone()?,
				})
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
	merge_clausewitz_files_inner(base, left, right, policies, false, None)
}

#[cfg(test)]
pub(crate) fn merge_event_files(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_inner(base, left, right, policies, true, None)
}

pub(crate) fn merge_clausewitz_files_with_source_selections(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
	source_selections: &LocalSourceSelections,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_inner(base, left, right, policies, false, Some(source_selections))
}

pub(crate) fn merge_event_files_with_source_selections(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
	source_selections: &LocalSourceSelections,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_inner(base, left, right, policies, true, Some(source_selections))
}

pub(crate) fn apply_nary_scalar_reducers(
	base: &AstFile,
	revisions: &[&AstFile],
	merged: &AstFile,
	policies: &MergePolicies,
) -> Result<AstFile, AstAdapterError> {
	if revisions.len() < 2
		|| (policies.scalar_reducer_rules.is_empty() && !is_numeric_reducer(policies.scalar))
	{
		return Ok(merged.clone());
	}

	let policy = ContentFamilyMergePolicy::new(policies);
	let base_tree = normalized_without_trivia(base, &policy)?;
	let base_scalars = reducer_scalars(&base_tree, policies)?;
	let revision_scalars = revisions
		.iter()
		.map(|revision| {
			let tree = normalized_without_trivia(revision, &policy)?;
			reducer_scalars(&tree, policies)
		})
		.collect::<Result<Vec<_>, AstAdapterError>>()?;

	let (merged_semantic, merged_trivia) = detach_trivia(merged);
	let mut merged_tree = normalize_ast(&merged_semantic, &policy)?;
	let merged_scalars = reducer_scalars(&merged_tree, policies)?;
	let mut changed_by_path: BTreeMap<Vec<String>, Vec<(RevisionId, String)>> = BTreeMap::new();
	for (index, scalars) in revision_scalars.iter().enumerate() {
		let source = RevisionId::new(u16::try_from(index + 1).map_err(|_| {
			AstAdapterError::InvalidTree("too many scalar reducer revisions".to_string())
		})?);
		for (path, scalar) in scalars {
			if base_scalars
				.get(path)
				.is_some_and(|base| base.value == scalar.value)
			{
				continue;
			}
			changed_by_path
				.entry(path.clone())
				.or_default()
				.push((source, scalar.value.clone()));
		}
	}

	for (path, inputs) in changed_by_path {
		if inputs.len() < 2 || inputs.windows(2).all(|pair| pair[0].1 == pair[1].1) {
			continue;
		}
		let reducer = scalar_reducer_for_path(policies, &path).ok_or_else(|| {
			AstAdapterError::InvalidTree(format!(
				"numeric reducer disappeared for semantic path {}",
				path.join("/")
			))
		})?;
		let output = reducer
			.reduce_numeric_values(inputs.iter().map(|(_, value)| value.as_str()))
			.ok_or_else(|| {
				AstAdapterError::InvalidTree(format!(
					"numeric reducer {reducer:?} rejected semantic path {}",
					path.join("/")
				))
			})?;
		let target = merged_scalars.get(&path).ok_or_else(|| {
			AstAdapterError::InvalidTree(format!(
				"merged tree dropped numeric reducer path {}",
				path.join("/")
			))
		})?;
		merged_tree.synthesize_scalar_value(target.node, output, path, inputs)?;
	}

	let mut reduced = denormalize_ast(merged.path.clone(), &merged_tree)?;
	attach_trivia(&mut reduced, &merged_trivia);
	Ok(reduced)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReducerScalar {
	node: NodeId,
	value: String,
}

fn normalized_without_trivia(
	file: &AstFile,
	policy: &ContentFamilyMergePolicy<'_>,
) -> Result<NormalizedTree, AstAdapterError> {
	let (semantic, _) = detach_trivia(file);
	normalize_ast(&semantic, policy)
}

fn reducer_scalars(
	tree: &NormalizedTree,
	policies: &MergePolicies,
) -> Result<BTreeMap<Vec<String>, ReducerScalar>, AstAdapterError> {
	let mut scalars = BTreeMap::new();
	let mut ambiguous_paths = BTreeSet::new();
	for (node, normalized) in tree.nodes() {
		if scalar_reducer_for_node(policies, normalized).is_none() {
			continue;
		}
		let value = normalized.value.clone().ok_or_else(|| {
			AstAdapterError::InvalidTree(format!(
				"numeric reducer target {} has no scalar value",
				normalized.policy_path.join("/")
			))
		})?;
		if !value
			.parse::<f64>()
			.ok()
			.is_some_and(|number| number.is_finite())
			|| ambiguous_paths.contains(&normalized.policy_path)
		{
			continue;
		}
		if scalars.contains_key(&normalized.policy_path) {
			scalars.remove(&normalized.policy_path);
			ambiguous_paths.insert(normalized.policy_path.clone());
			continue;
		}
		scalars.insert(
			normalized.policy_path.clone(),
			ReducerScalar { node, value },
		);
	}
	Ok(scalars)
}

fn scalar_reducer_for_node(
	policies: &MergePolicies,
	node: &NormalizedNode,
) -> Option<ScalarMergePolicy> {
	(node.children.is_empty()
		&& node.kind.starts_with("clausewitz.scalar.")
		&& node.value.is_some())
	.then(|| scalar_reducer_for_path(policies, &node.policy_path))
	.flatten()
}

fn scalar_reducer_for_path(policies: &MergePolicies, path: &[String]) -> Option<ScalarMergePolicy> {
	policies
		.scalar_reducer_rule_for_path(path)
		.map(|rule| rule.reducer)
		.or_else(|| is_numeric_reducer(policies.scalar).then_some(policies.scalar))
}

const fn is_numeric_reducer(policy: ScalarMergePolicy) -> bool {
	matches!(
		policy,
		ScalarMergePolicy::Sum
			| ScalarMergePolicy::Avg
			| ScalarMergePolicy::Max
			| ScalarMergePolicy::Min
	)
}

/// Normalize a Clausewitz file through the same semantic representation used
/// by Structured merge. This is useful when comparing generated output with a
/// differently written but semantically equivalent reference AST.
pub fn canonicalize_clausewitz_file(
	file: &AstFile,
	policies: &MergePolicies,
) -> Result<AstFile, AstAdapterError> {
	let policy = ContentFamilyMergePolicy::new(policies);
	let mut scope_cache = HashMap::new();
	let canonical = canonicalize_boolean_or_definitions(file, policies, &mut scope_cache);
	let (semantic, trivia) = detach_trivia(&canonical);
	let tree = normalize_ast(&semantic, &policy)?;
	let mut canonical = denormalize_ast(file.path.clone(), &tree)?;
	attach_trivia(&mut canonical, &trivia);
	simplify_boolean_or_definitions(&mut canonical, policies, &mut scope_cache);
	Ok(canonical)
}

fn merge_clausewitz_files_inner(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
	reduce_event_fallbacks: bool,
	source_selections: Option<&LocalSourceSelections>,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	let policy = source_selections.map_or_else(
		|| ContentFamilyMergePolicy::new(policies),
		|selections| ContentFamilyMergePolicy::with_source_selections(policies, selections),
	);
	let mut scope_cache = HashMap::new();
	let base = canonicalize_boolean_or_definitions(base, policies, &mut scope_cache);
	let left = canonicalize_boolean_or_definitions(left, policies, &mut scope_cache);
	let right = canonicalize_boolean_or_definitions(right, policies, &mut scope_cache);
	let (base, base_trivia) = detach_trivia(&base);
	let (left, left_trivia) = detach_trivia(&left);
	let (right, right_trivia) = detach_trivia(&right);
	let mut control_flow_findings = [("base", &base), ("left", &left), ("right", &right)]
		.into_iter()
		.flat_map(|(revision, file)| {
			super::control_flow::orphan_paths(&file.statements)
				.into_iter()
				.map(move |path| format!("{revision}:{path}"))
		})
		.collect::<BTreeSet<_>>();
	let merged_trivia = merge_trivia(&base_trivia, &left_trivia, &right_trivia);
	let (base_tree, base_normalization_findings) = normalize_ast_with_findings(&base, &policy)?;
	let (left_tree, left_normalization_findings) = normalize_ast_with_findings(&left, &policy)?;
	let (right_tree, right_normalization_findings) = normalize_ast_with_findings(&right, &policy)?;
	for (revision, findings) in [
		("base", base_normalization_findings),
		("left", left_normalization_findings),
		("right", right_normalization_findings),
	] {
		control_flow_findings.extend(
			findings
				.into_iter()
				.map(|finding| format!("{revision}:{finding}")),
		);
	}
	let mut kernel = three_way_merge_with_policy(&base_tree, &left_tree, &right_tree, &policy);
	let mut tentative_ast = denormalize_ast(base.path.clone(), kernel.tentative_tree())?;
	control_flow_findings.extend(
		super::control_flow::orphan_paths(&tentative_ast.statements)
			.into_iter()
			.map(|path| format!("output:{path}")),
	);
	if !control_flow_findings.is_empty() {
		let count = control_flow_findings.len();
		let examples = control_flow_findings
			.iter()
			.take(8)
			.cloned()
			.collect::<Vec<_>>()
			.join(", ");
		kernel.conflicts.push(StructuralConflict {
			kind: ConflictKind::Policy,
			parent: None,
			base: None,
			revisions: SourceSet::default(),
			semantic_path: Vec::new(),
			detail: format!("{count} control-flow finding(s) require review: {examples}"),
		});
	}
	attach_trivia(&mut tentative_ast, &merged_trivia);
	simplify_boolean_or_definitions(&mut tentative_ast, policies, &mut scope_cache);
	if reduce_event_fallbacks {
		reduce_redundant_constructor_fallbacks(&mut tentative_ast.statements);
	}
	Ok(ClausewitzMergeOutcome {
		tentative_ast,
		kernel,
	})
}

fn canonicalize_boolean_or_definitions(
	file: &AstFile,
	policies: &MergePolicies,
	scope_cache: &mut HashMap<Vec<String>, Option<ScopeKind>>,
) -> AstFile {
	let mut file = file.clone();
	let file_kind = classify_script_file(&file.path);
	BooleanConditionTransformer {
		file_path: &file.path,
		file_kind: &file_kind,
		policies,
		transform: BooleanTransform::Canonicalize,
		scope_cache,
	}
	.transform(&mut file.statements, &mut Vec::new(), ScriptContext::Data);
	file
}

fn simplify_boolean_or_definitions(
	file: &mut AstFile,
	policies: &MergePolicies,
	scope_cache: &mut HashMap<Vec<String>, Option<ScopeKind>>,
) {
	let file_kind = classify_script_file(&file.path);
	BooleanConditionTransformer {
		file_path: &file.path,
		file_kind: &file_kind,
		policies,
		transform: BooleanTransform::Simplify,
		scope_cache,
	}
	.transform(&mut file.statements, &mut Vec::new(), ScriptContext::Data);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScriptContext {
	Data,
	Trigger,
	Effect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BooleanTransform {
	Canonicalize,
	Simplify,
}

struct BooleanConditionTransformer<'a> {
	file_path: &'a Path,
	file_kind: &'a CwtType,
	policies: &'a MergePolicies,
	transform: BooleanTransform,
	scope_cache: &'a mut HashMap<Vec<String>, Option<ScopeKind>>,
}

impl BooleanConditionTransformer<'_> {
	fn transform(
		&mut self,
		statements: &mut [AstStatement],
		path: &mut Vec<String>,
		parent_context: ScriptContext,
	) {
		for statement in statements {
			let AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} = statement
			else {
				continue;
			};

			path.push(key.clone());
			let scope_kind = *self.scope_cache.entry(path.clone()).or_insert_with(|| {
				let path_refs = path.iter().map(String::as_str).collect::<Vec<_>>();
				script_container_scope_kind(
					self.file_kind.clone(),
					self.file_path,
					path_refs.as_slice(),
				)
			});
			let context = script_context(parent_context, key, scope_kind);
			let configured_definition = path.len() == 1
				&& self.policies.block_patch_policy_for_key(key) == BlockPatchPolicy::BooleanOr;
			let trigger_root = configured_definition
				|| (self.policies.boolean == BooleanMergePolicy::Or
					&& context == ScriptContext::Trigger
					&& parent_context != context
					&& !is_boolean_operator(key));

			self.transform(
				items,
				path,
				if configured_definition {
					ScriptContext::Trigger
				} else {
					context
				},
			);
			if trigger_root && !items.is_empty() {
				*items = match self.transform {
					BooleanTransform::Canonicalize => {
						canonical_boolean_or_body(std::mem::take(items))
					}
					BooleanTransform::Simplify => {
						let simplified = simplify_boolean_or_body(std::mem::take(items));
						super::control_flow::simplify_merged_trigger_predicate(&simplified)
							.unwrap_or(simplified)
					}
				};
			}
			path.pop();
		}
	}
}

fn script_context(
	parent: ScriptContext,
	key: &str,
	scope_kind: Option<ScopeKind>,
) -> ScriptContext {
	let normalized = key.to_ascii_lowercase();
	if matches!(
		normalized.as_str(),
		"trigger" | "limit" | "potential" | "allow" | "condition" | "hidden_trigger"
	) || is_boolean_operator(&normalized)
	{
		return ScriptContext::Trigger;
	}
	if matches!(
		normalized.as_str(),
		"effect"
			| "after" | "hidden_effect"
			| "immediate"
			| "on_add"
			| "on_remove"
			| "on_start"
			| "on_end"
			| "on_monthly"
			| "country_event"
			| "province_event"
			| "option"
	) {
		return ScriptContext::Effect;
	}
	if matches!(normalized.as_str(), "if" | "else_if" | "else") {
		return ScriptContext::Effect;
	}

	match scope_kind {
		Some(ScopeKind::Trigger) => ScriptContext::Trigger,
		Some(ScopeKind::Effect | ScopeKind::ScriptedEffect | ScopeKind::Event) => {
			ScriptContext::Effect
		}
		Some(
			ScopeKind::File
			| ScopeKind::Decision
			| ScopeKind::Loop
			| ScopeKind::AliasBlock
			| ScopeKind::Block,
		)
		| None => parent,
	}
}

fn is_boolean_operator(key: &str) -> bool {
	["or", "and", "not", "nor", "nand"]
		.iter()
		.any(|operator| key.eq_ignore_ascii_case(operator))
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
	if super::control_flow::branch_key(statements.get(start)?) != Some("if") {
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
		match statements
			.get(branch)
			.and_then(super::control_flow::branch_key)
		{
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
