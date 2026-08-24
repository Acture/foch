use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use foch::game::eu4::content::{
	BooleanMergePolicy, DivergentBlockPolicy, MergePolicies, ScriptFileKind,
};
use foch::game::eu4::script::parser::{AstFile, AstStatement, AstValue};
use foch::game::eu4::script::{classify_script_file, script_container_scope_kind};
use foch::merge::kernel::{
	ConflictKind, ConflictResolution, MergeDecisionEvidence, MergeOutcome, MergeRevision,
	NormalizedTree, RevisionDelta, RevisionId, SourceSet, StructuralConflict,
	StructuralConflictDraft, n_way_merge_with_policy, n_way_merge_with_policy_and_resolutions,
};
use foch::model::ScopeKind;

use crate::merge::boolean::{canonical_boolean_or_body, simplify_boolean_or_body};
use crate::merge::model::SemanticPartitionId;

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
	kernel: MergeOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClausewitzKernelFacts {
	pub partition: SemanticPartitionId,
	pub base_tree: NormalizedTree,
	pub revision_trees: BTreeMap<RevisionId, NormalizedTree>,
	pub outcome: MergeOutcome,
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
	pub delta_ns: u64,
	pub pcs_ns: u64,
	pub policy_ns: u64,
}

impl ClausewitzMergeOutcome {
	pub fn conflicts(&self) -> &[StructuralConflict] {
		&self.kernel.conflicts
	}

	pub fn revision_deltas(&self) -> &BTreeMap<RevisionId, RevisionDelta> {
		&self.kernel.revision_deltas
	}

	pub fn decisions(&self) -> &[MergeDecisionEvidence] {
		&self.kernel.decisions
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
				outcome: self.kernel,
			},
		)
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
			delta_ns: self.kernel.timings.delta_ns,
			pcs_ns: self.kernel.timings.pcs_ns,
			policy_ns: self.kernel.timings.policy_ns,
		}
	}

	#[cfg(test)]
	pub(crate) fn kernel(&self) -> &MergeOutcome {
		&self.kernel
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
	let policy = ContentFamilyMergePolicy::new(policies);
	let mut scope_cache = HashMap::new();
	let base = canonicalize_boolean_or_definitions(base, policies, &mut scope_cache);
	let revisions = revisions
		.iter()
		.map(|revision| canonicalize_boolean_or_definitions(revision, policies, &mut scope_cache))
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
		kernel,
	})
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

pub(crate) fn clausewitz_files_semantically_equivalent(
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<bool, AstAdapterError> {
	let policy = ContentFamilyMergePolicy::new(policies);
	let mut scope_cache = HashMap::new();
	let left = canonicalize_boolean_or_definitions(left, policies, &mut scope_cache);
	let right = canonicalize_boolean_or_definitions(right, policies, &mut scope_cache);
	let (left, _) = detach_trivia(&left);
	let (right, _) = detach_trivia(&right);
	let left = normalize_ast(&left, &policy)?;
	let right = normalize_ast(&right, &policy)?;
	Ok(left.semantically_equivalent(&right))
}

pub(crate) fn clausewitz_statements_semantically_equivalent(
	left: &AstStatement,
	right: &AstStatement,
	policies: &MergePolicies,
) -> Result<bool, AstAdapterError> {
	clausewitz_files_semantically_equivalent(
		&AstFile {
			path: PathBuf::new(),
			statements: vec![left.clone()],
		},
		&AstFile {
			path: PathBuf::new(),
			statements: vec![right.clone()],
		},
		policies,
	)
}

pub(crate) fn normalize_clausewitz_partition(
	file: &AstFile,
	partition: &SemanticPartitionId,
	policies: &MergePolicies,
) -> Result<NormalizedTree, AstAdapterError> {
	let file = match partition {
		SemanticPartitionId::File => file.clone(),
		SemanticPartitionId::Definition(key) => AstFile {
			path: file.path.clone(),
			statements: file
				.statements
				.iter()
				.filter(|statement| {
					matches!(statement, AstStatement::Assignment { key: candidate, .. } if candidate == key)
				})
				.cloned()
				.collect(),
		},
	};
	let mut scope_cache = HashMap::new();
	let canonical = canonicalize_boolean_or_definitions(&file, policies, &mut scope_cache);
	let (semantic, _) = detach_trivia(&canonical);
	normalize_ast(&semantic, &ContentFamilyMergePolicy::new(policies))
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
