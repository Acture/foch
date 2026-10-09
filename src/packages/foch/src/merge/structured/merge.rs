use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::game::eu4::content::{
	BooleanMergePolicy, DivergentBlockPolicy, MergePolicies, ScriptFileKind,
};
use crate::game::eu4::cwt::rule_engine;
use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue, ScalarValue};
use crate::game::eu4::script::{classify_script_file, script_container_scope_kind};
use crate::game::schema::query::CwtQuery;
use crate::merge::kernel::{
	ConflictKind, ConflictResolution, MergeOutcome, MergeRevision, NormalizedTree, RevisionId,
	SourceSet, StructuralConflict, StructuralConflictDraft, TreeMatcher, n_way_merge_with_policy,
	n_way_merge_with_policy_and_resolutions,
};
use crate::merge::transform::tree::EntityTransform;
use crate::model::{GamePath, ScopeKind};

use crate::merge::boolean::{canonical_boolean_or_body, simplify_boolean_or_body};
use crate::merge::model::{InputRewrite, SemanticPartitionId};

use super::ast_adapter::{
	AstAdapterError, denormalize_ast, normalize_ast, normalize_ast_with_findings,
};
use super::policy::ContentFamilyMergePolicy;
use super::trivia::{attach_trivia, detach_trivia, merge_trivia_n_way};

#[derive(Clone, Debug)]
pub struct ClausewitzMergeOutcome {
	tentative_ast: AstFile,
	base_tree: NormalizedTree,
	revision_trees: BTreeMap<RevisionId, NormalizedTree>,
	input_rewrites: BTreeMap<RevisionId, InputRewrite>,
	kernel: MergeOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClausewitzKernelFacts {
	pub partition: SemanticPartitionId,
	pub base_tree: NormalizedTree,
	pub revision_trees: BTreeMap<RevisionId, NormalizedTree>,
	pub input_rewrites: BTreeMap<RevisionId, InputRewrite>,
	pub outcome: MergeOutcome,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClausewitzScalarReduction {
	pub path: Vec<String>,
	pub inputs: Vec<(RevisionId, String)>,
	pub output: String,
}

impl ClausewitzMergeOutcome {
	pub fn conflicts(&self) -> &[StructuralConflict] {
		&self.kernel.conflicts
	}

	#[cfg(test)]
	pub fn resolved_ast(&self) -> Option<&AstFile> {
		self.kernel
			.conflicts
			.is_empty()
			.then_some(&self.tentative_ast)
	}

	#[cfg(test)]
	pub fn tentative_ast(&self) -> &AstFile {
		&self.tentative_ast
	}

	pub(crate) fn into_parts(
		self,
		partition: SemanticPartitionId,
	) -> (AstFile, ClausewitzKernelFacts) {
		(
			self.tentative_ast,
			ClausewitzKernelFacts {
				partition,
				base_tree: self.base_tree,
				revision_trees: self.revision_trees,
				input_rewrites: self.input_rewrites,
				outcome: self.kernel,
			},
		)
	}

	#[cfg(test)]
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

	#[cfg(test)]
	pub(crate) fn kernel(&self) -> &MergeOutcome {
		&self.kernel
	}
}

/// Merge three parseable Clausewitz ASTs without content-family-specific
/// post-processing. The caller owns merge-unit construction and commit.
#[cfg(test)]
pub fn merge_clausewitz_files(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_n_way_inner(base, &[left, right], policies, false, &[])
}

#[cfg(test)]
pub(crate) fn merge_event_files(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_n_way_inner(base, &[left, right], policies, true, &[])
}

pub fn merge_clausewitz_files_n_way(
	base: &AstFile,
	revisions: &[&AstFile],
	policies: &MergePolicies,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_n_way_inner(base, revisions, policies, false, &[])
}

pub(crate) fn merge_clausewitz_files_n_way_with_resolutions(
	base: &AstFile,
	revisions: &[&AstFile],
	policies: &MergePolicies,
	resolutions: &[ConflictResolution],
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_n_way_inner(base, revisions, policies, false, resolutions)
}

pub(crate) fn merge_event_files_n_way_with_resolutions(
	base: &AstFile,
	revisions: &[&AstFile],
	policies: &MergePolicies,
	resolutions: &[ConflictResolution],
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_n_way_inner(base, revisions, policies, true, resolutions)
}

fn merge_clausewitz_files_n_way_inner(
	base: &AstFile,
	revisions: &[&AstFile],
	policies: &MergePolicies,
	reduce_event_fallbacks: bool,
	resolutions: &[ConflictResolution],
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	merge_clausewitz_files_n_way_with_schema(
		base,
		revisions,
		policies,
		Some(rule_engine()),
		reduce_event_fallbacks,
		resolutions,
	)
}

/// The n-way merge with its schema evidence supplied rather than looked up.
///
/// The active schema is process-global, so this is also how a test supplies a
/// schema other than the process one, or none.
pub fn merge_clausewitz_files_n_way_with_schema(
	base: &AstFile,
	revisions: &[&AstFile],
	policies: &MergePolicies,
	schema: Option<&CwtQuery>,
	reduce_event_fallbacks: bool,
	resolutions: &[ConflictResolution],
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	let entity_transform = crate::game::eu4::content::eu4()
		.classify_content_family(&base.path)
		.map(|descriptor| descriptor.infer_entity_transform(base, revisions))
		.transpose()
		.map_err(AstAdapterError::InvalidTree)?
		.flatten();
	merge_clausewitz_files_with_context(
		base,
		revisions,
		policies,
		schema,
		reduce_event_fallbacks,
		resolutions,
		entity_transform.as_deref(),
	)
}

pub(super) fn merge_clausewitz_files_with_context(
	base: &AstFile,
	revisions: &[&AstFile],
	policies: &MergePolicies,
	schema: Option<&CwtQuery>,
	reduce_event_fallbacks: bool,
	resolutions: &[ConflictResolution],
	entity_transform: Option<&dyn EntityTransform>,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	let entity_transform = entity_transform.filter(|transform| transform.applies_to(&base.path));
	let resolved_chains = super::trigger_cases::resolve_replaced_trigger_chains(
		base,
		revisions,
		&mut |branch_base, branch_revisions| {
			let Ok(outcome) = merge_clausewitz_files_with_context(
				branch_base,
				branch_revisions,
				policies,
				schema,
				false,
				&[],
				None,
			) else {
				return Ok(None);
			};
			Ok(outcome
				.conflicts()
				.is_empty()
				.then_some(outcome.tentative_ast))
		},
	)?;
	let original_revisions = revisions;
	let resolved_revisions = resolved_chains
		.as_ref()
		.map(|files| files.iter().collect::<Vec<_>>());
	let revisions = resolved_revisions.as_deref().unwrap_or(revisions);
	if let Some(transform) = entity_transform {
		for file in std::iter::once(base).chain(revisions.iter().copied()) {
			transform
				.validate(file)
				.map_err(AstAdapterError::InvalidTree)?;
		}
	}
	let policy = entity_transform.map_or_else(
		|| ContentFamilyMergePolicy::new(policies),
		|transform| ContentFamilyMergePolicy::with_transform(policies, transform),
	);
	let mut scope_cache = HashMap::new();
	let base = canonicalize_for_merge(base, policies, schema, &mut scope_cache);
	let revisions = revisions
		.iter()
		.map(|revision| canonicalize_for_merge(revision, policies, schema, &mut scope_cache))
		.collect::<Vec<_>>();
	let (base, base_trivia) = detach_trivia(&base);
	let detached_revisions = revisions.iter().map(detach_trivia).collect::<Vec<_>>();
	let revision_files = detached_revisions
		.iter()
		.map(|(file, _)| file)
		.collect::<Vec<_>>();
	let revision_trivia = detached_revisions
		.iter()
		.map(|(_, trivia)| trivia)
		.collect::<Vec<_>>();
	let mut control_flow_findings = super::control_flow::orphan_paths(&base.statements)
		.into_iter()
		.map(|path| format!("base:{path}"))
		.collect::<BTreeSet<_>>();
	for (index, revision) in revision_files.iter().enumerate() {
		control_flow_findings.extend(
			super::control_flow::orphan_paths(&revision.statements)
				.into_iter()
				.map(|path| format!("revision:{}:{path}", index + 1)),
		);
	}
	let merged_trivia = merge_trivia_n_way(&base_trivia, &revision_trivia);
	let (base_tree, base_normalization_findings) = normalize_ast_with_findings(&base, &policy)?;
	control_flow_findings.extend(
		base_normalization_findings
			.into_iter()
			.map(|finding| format!("base:{finding}")),
	);
	let revision_trees = revision_files
		.iter()
		.enumerate()
		.map(|(index, revision)| {
			let revision_id = RevisionId::new(u16::try_from(index + 1).map_err(|_| {
				AstAdapterError::InvalidTree("too many N-way revisions".to_string())
			})?);
			let (tree, findings) = normalize_ast_with_findings(revision, &policy)?;
			control_flow_findings.extend(
				findings
					.into_iter()
					.map(|finding| format!("revision:{}:{finding}", index + 1)),
			);
			Ok((revision_id, tree))
		})
		.collect::<Result<Vec<_>, AstAdapterError>>()?;
	let mut input_rewrites = BTreeMap::new();
	for ((revision, tree), (original, read)) in revision_trees.iter().zip(
		original_revisions
			.iter()
			.zip(resolved_chains.iter().flatten()),
	) {
		if **original != *read {
			let original = canonicalize_for_merge(original, policies, schema, &mut scope_cache);
			let (original, _) = detach_trivia(&original);
			let (original, _) = normalize_ast_with_findings(&original, &policy)?;
			input_rewrites.insert(*revision, input_rewrite(tree, original));
		}
	}
	let kernel_revisions = revision_trees
		.iter()
		.map(|(revision, tree)| MergeRevision::new(*revision, tree))
		.collect::<Vec<_>>();
	let mut kernel = if resolutions.is_empty() {
		n_way_merge_with_policy(&base_tree, &kernel_revisions, &policy)?
	} else {
		n_way_merge_with_policy_and_resolutions(
			&base_tree,
			&kernel_revisions,
			&policy,
			resolutions,
		)?
	};
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
		kernel.push_conflict(StructuralConflictDraft::new(
			ConflictKind::Policy,
			None,
			None,
			SourceSet::default(),
			Vec::new(),
			format!("{count} control-flow finding(s) require review: {examples}"),
		));
	}
	attach_trivia(&mut tentative_ast, &merged_trivia);
	simplify_boolean_or_definitions(&mut tentative_ast, policies, &mut scope_cache);
	if reduce_event_fallbacks {
		reduce_redundant_constructor_fallbacks(&mut tentative_ast.statements);
	}
	Ok(ClausewitzMergeOutcome {
		tentative_ast,
		base_tree,
		revision_trees: revision_trees.into_iter().collect(),
		input_rewrites,
		kernel,
	})
}

/// Trace each node of a rewritten revision to the mod's own file: to the node
/// it matches, or else to its nearest ancestor that does.
fn input_rewrite(read: &NormalizedTree, original: NormalizedTree) -> InputRewrite {
	let matching = TreeMatcher::default().match_trees(read, &original);
	let mut nodes = BTreeMap::new();
	let mut pending = vec![(read.root(), original.root())];
	while let Some((id, inherited)) = pending.pop() {
		let traced = matching.get_from_left(id).unwrap_or(inherited);
		nodes.insert(id, traced);
		let node = read.node(id).expect("a tree node");
		pending.extend(node.children.iter().map(|child| (*child, traced)));
	}
	InputRewrite { original, nodes }
}

/// Normalize a Clausewitz file through the same semantic representation used
/// by Structured merge. This is useful when comparing generated output with a
/// differently written but semantically equivalent reference AST.
#[cfg(test)]
pub fn canonicalize_clausewitz_file(
	file: &AstFile,
	policies: &MergePolicies,
) -> Result<AstFile, AstAdapterError> {
	let policy = ContentFamilyMergePolicy::new(policies);
	let mut scope_cache = HashMap::new();
	let canonical = canonicalize_for_merge(file, policies, Some(rule_engine()), &mut scope_cache);
	let (semantic, trivia) = detach_trivia(&canonical);
	let tree = normalize_ast(&semantic, &policy)?;
	let mut canonical = denormalize_ast(file.path.clone(), &tree)?;
	attach_trivia(&mut canonical, &trivia);
	simplify_boolean_or_definitions(&mut canonical, policies, &mut scope_cache);
	Ok(canonical)
}

pub(crate) fn clausewitz_files_semantically_equivalent(
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<bool, AstAdapterError> {
	let policy = ContentFamilyMergePolicy::new(policies);
	let mut scope_cache = HashMap::new();
	let left = canonicalize_for_merge(left, policies, Some(rule_engine()), &mut scope_cache);
	let right = canonicalize_for_merge(right, policies, Some(rule_engine()), &mut scope_cache);
	let (left, _) = detach_trivia(&left);
	let (right, _) = detach_trivia(&right);
	let left = normalize_ast(&left, &policy)?;
	let right = normalize_ast(&right, &policy)?;
	Ok(left.semantically_equivalent(&right))
}

/// Compare two statements of `relative_path`'s content family.
///
/// The path is a semantic input, not a label: `canonicalize_boolean_or_definitions`
/// resolves each container's script role from it, so an empty path classifies as
/// `ScriptFileKind("other")` and skips the family's boolean canonicalization.
/// Callers must pass the game-relative path the statements belong to.
pub(crate) fn clausewitz_statements_semantically_equivalent(
	relative_path: &GamePath,
	left: &AstStatement,
	right: &AstStatement,
	policies: &MergePolicies,
) -> Result<bool, AstAdapterError> {
	clausewitz_files_semantically_equivalent(
		&AstFile {
			path: relative_path.to_owned(),
			statements: vec![left.clone()],
		},
		&AstFile {
			path: relative_path.to_owned(),
			statements: vec![right.clone()],
		},
		policies,
	)
}

pub(crate) fn normalize_clausewitz_file(
	file: &AstFile,
	policies: &MergePolicies,
) -> Result<NormalizedTree, AstAdapterError> {
	normalize_clausewitz_file_with_context(file, policies, None)
}

pub(super) fn normalize_clausewitz_file_with_context(
	file: &AstFile,
	policies: &MergePolicies,
	entity_transform: Option<&dyn EntityTransform>,
) -> Result<NormalizedTree, AstAdapterError> {
	let entity_transform = entity_transform.filter(|transform| transform.applies_to(&file.path));
	if let Some(transform) = entity_transform {
		transform
			.validate(file)
			.map_err(AstAdapterError::InvalidTree)?;
	}
	let mut scope_cache = HashMap::new();
	let canonical = canonicalize_for_merge(file, policies, Some(rule_engine()), &mut scope_cache);
	let (semantic, _) = detach_trivia(&canonical);
	let policy = entity_transform.map_or_else(
		|| ContentFamilyMergePolicy::new(policies),
		|transform| ContentFamilyMergePolicy::with_transform(policies, transform),
	);
	normalize_ast(&semantic, &policy)
}

/// Bring a file to the form every identity in the merge is derived from.
///
/// Numbers first: `canonicalize_boolean_or_definitions` deduplicates disjuncts
/// by exact scalar text, so two that differ only in how one number is spelled
/// have to already read alike by then.
fn canonicalize_for_merge(
	file: &AstFile,
	policies: &MergePolicies,
	schema: Option<&CwtQuery>,
	scope_cache: &mut HashMap<Vec<String>, Option<ScopeKind>>,
) -> AstFile {
	let mut file = crate::merge::numeric::canonicalize_numeric_values(file, schema);
	canonicalize_multiline_strings(&mut file.statements);
	canonicalize_boolean_or_definitions(&file, policies, scope_cache)
}

/// A quoted string that spans lines holds script the game parses again, such
/// as an `effect_tooltip`, so its line endings and the blanks before them are
/// formatting. Mods that resave a file with other line endings or trimmed
/// lines would otherwise appear to change the value.
fn canonicalize_multiline_strings(statements: &mut [AstStatement]) {
	for statement in statements {
		let (AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. }) = statement
		else {
			continue;
		};
		match value {
			AstValue::Block { items, .. } => canonicalize_multiline_strings(items),
			AstValue::Scalar {
				value: ScalarValue::String(text),
				..
			} if text.contains(['\n', '\r']) => {
				*text = text
					.replace('\r', "")
					.split('\n')
					.map(|line| line.trim_end_matches([' ', '\t']))
					.collect::<Vec<_>>()
					.join("\n");
			}
			AstValue::Scalar { .. } => {}
		}
	}
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
pub(super) enum ScriptContext {
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
	file_path: &'a GamePath,
	file_kind: &'a ScriptFileKind,
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
				&& self.policies.divergent_block_policy_for_key(key)
					== DivergentBlockPolicy::BooleanOr;
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

pub(super) fn script_context(
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
	// Control flow keeps the context it is written in: `if`/`else` inside a
	// `limit` are trigger conditionals, not effect blocks.
	if matches!(normalized.as_str(), "if" | "else_if" | "else") {
		return match parent {
			ScriptContext::Trigger => ScriptContext::Trigger,
			_ => ScriptContext::Effect,
		};
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
