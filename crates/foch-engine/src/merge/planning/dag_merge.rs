#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use foch_core::config::DepOverride;
use foch_core::model::{HandlerResolutionRecord, MergeTraceContributor};
use foch_language::analyzer::content_family::{CwtType, MergeKeySource, MergePolicies};
use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue};
use foch_language::analyzer::semantic_index::{ParsedScriptFile, parse_script_file};

use super::super::conflict_handler::{ConflictHandler, DeferHandler};
#[cfg(test)]
use super::super::patch::diff_ast;
use super::super::patch::{
	ClausewitzPatch, ListItemOccurrence, ListItemTarget, align_sequences_by, diff_ast_with_nested,
	fold_renames, insertion_source_slot, semantic_occurrence_ordinals,
};
use super::super::patch_apply::apply_patches_with_nested;
use super::super::patch_merge::{
	AttributedPatch, PatchAddress, PatchMergeResult, PatchResolution, merge_patch_sets_for_file,
	order_patches_by_source, semantic_statement_identity, semantic_value_identity,
};
use super::dag::{
	FileDag, IgnoreReplacePath, ModDag, ModId, induced_file_dag_with_overrides, topo_levels,
};
use super::dag_join::sink_mods;
#[cfg(test)]
use super::dag_join::{ancestry_metrics, reset_ancestry_metrics};
use super::dag_pipeline::{
	DagJoinProtocol, DagJoinRequest, DagPipelineResult, EffectiveNodeProtocol,
	EffectiveNodeRequest, execute_dag_pipeline,
};
use crate::cache::{DagBaseCache, ModDiffCache};
use crate::merge::kernel::MergeKernelMode;
use crate::merge::structured::{
	ClausewitzFileAdapter, DefinitionModuleAdapter, EventFileAdapter, TreeConflictCandidate,
	TreeDagProtocol, TreeDagState, TreeMergeAdapter, TreeMergeUnit,
};
use crate::workspace::{ResolvedFileContributor, WorkspaceScriptCache};

#[derive(Clone, Debug)]
pub(crate) struct DagMergeComputation {
	pub mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	pub base_statements: Vec<AstStatement>,
	pub merged_statements: Vec<AstStatement>,
	pub merge_result: PatchMergeResult,
	/// Per top-level definition key → mods whose content is **adopted** into the
	/// final merged output, in ascending DAG-precedence order. Overridden losers
	/// and no-op-vs-base contributors are excluded. Empty unless a mod changed a
	/// key whose content survives in `merged_statements`.
	pub definition_provenance: BTreeMap<String, Vec<String>>,
	/// Per top-level definition key → all non-base mods that directly changed
	/// that key, in DAG level / precedence order. Inherited no-op content is
	/// excluded.
	pub definition_participants: BTreeMap<String, Vec<MergeTraceContributor>>,
}

pub(crate) struct DagMergeRequest<'a> {
	pub file_path: &'a str,
	pub contributors: &'a [ResolvedFileContributor],
	pub merge_key_source: MergeKeySource,
	pub policies: &'a MergePolicies,
	pub mod_dag: &'a ModDag,
	pub ignore_replace_path: &'a IgnoreReplacePath,
	pub dep_overrides: &'a [DepOverride],
	pub game_version: &'a str,
	pub script_cache: Option<&'a WorkspaceScriptCache>,
}

struct DagMergeArgs<'a> {
	file_dag: &'a FileDag,
	vanilla: Option<&'a ParsedScriptFile>,
	contributors: &'a HashMap<ModId, ParsedScriptFile>,
	merge_key_source: MergeKeySource,
	policies: &'a MergePolicies,
	handler: &'a mut dyn ConflictHandler,
	mod_hashes: Option<&'a HashMap<ModId, String>>,
	game_version: &'a str,
	kernel: MergeKernelMode,
	tree_unit: TreeMergeUnit,
}

#[derive(Clone, Copy, Default)]
struct PatchCaches<'a> {
	diff: Option<&'a ModDiffCache>,
	dag_base: Option<&'a DagBaseCache>,
}

struct CachedDiffArgs<'a> {
	cache: Option<&'a ModDiffCache>,
	target_path: &'a str,
	mod_hash: Option<&'a str>,
	base_view_hash: Option<&'a str>,
	current_base: &'a ParsedScriptFile,
	current: &'a ParsedScriptFile,
	merge_key_source: MergeKeySource,
	nested_merge_key_source: MergeKeySource,
	game_version: &'a str,
}

struct CachedApplyArgs<'a> {
	cache: Option<&'a DagBaseCache>,
	deps_hash: Option<&'a str>,
	file_path: &'a str,
	current_statements: &'a [AstStatement],
	resolved_patches: &'a [ClausewitzPatch],
	merge_key_source: MergeKeySource,
	nested_merge_key_source: MergeKeySource,
	cache_scope: DagApplyCacheScope,
	game_version: &'a str,
}

#[derive(Clone, Copy)]
struct PatchKeySources {
	root: MergeKeySource,
	nested: MergeKeySource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DagApplyCacheScope {
	EffectiveNode,
	ResolvedBranchState,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct DagApplyCacheEvent {
	scope: DagApplyCacheScope,
	hit: bool,
}

#[derive(Clone, Debug)]
struct PatchBaselineDagState {
	statements: Vec<AstStatement>,
	intent_only_patches: Vec<ClausewitzPatch>,
	pending_conflicts: Vec<PatchResolution>,
}

struct PatchBaselineDagProtocol<'a> {
	file_dag: &'a FileDag,
	base_statements: &'a [AstStatement],
	template: Option<&'a ParsedScriptFile>,
	merge_key_source: MergeKeySource,
	policies: &'a MergePolicies,
	handler: &'a mut dyn ConflictHandler,
	mod_hashes: Option<&'a HashMap<ModId, String>>,
	diff_cache: Option<&'a ModDiffCache>,
	dag_base_cache: Option<&'a DagBaseCache>,
	diff_cache_context: String,
	dag_base_cache_context: String,
	mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	merge_result: PatchMergeResult,
	seen_pending_conflicts: Vec<PatchResolution>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PatchIntentAddress {
	Node(Vec<String>, String),
	ListItem(Vec<String>, String, String, usize),
	BlockItem(Vec<String>, String),
}

#[cfg(test)]
std::thread_local! {
	static DAG_APPLY_CACHE_EVENTS: std::cell::RefCell<Vec<DagApplyCacheEvent>> = const {
		std::cell::RefCell::new(Vec::new())
	};
}

#[cfg(test)]
fn record_dag_apply_cache_event(scope: DagApplyCacheScope, hit: bool) {
	DAG_APPLY_CACHE_EVENTS
		.with(|events| events.borrow_mut().push(DagApplyCacheEvent { scope, hit }));
}

#[cfg(not(test))]
#[inline]
fn record_dag_apply_cache_event(_scope: DagApplyCacheScope, _hit: bool) {}

#[cfg(test)]
fn reset_dag_apply_cache_events() {
	DAG_APPLY_CACHE_EVENTS.with(|events| events.borrow_mut().clear());
}

#[cfg(test)]
fn dag_apply_cache_events() -> Vec<DagApplyCacheEvent> {
	DAG_APPLY_CACHE_EVENTS.with(|events| events.borrow().clone())
}

/// Compute all patches for a single file using dependency-DAG topo order.
///
/// Each contributor is diffed against its resolved direct-parent view. The
/// resulting effective node state is retained for descendants; incomparable
/// branch frontiers are merged only at joins and at the final sink frontier.
pub(crate) fn compute_dag_merge(
	request: DagMergeRequest<'_>,
) -> Result<DagMergeComputation, String> {
	let mut handler = DeferHandler;
	compute_dag_merge_with_handler(request, &mut handler)
}

pub(crate) fn compute_dag_merge_with_handler(
	request: DagMergeRequest<'_>,
	handler: &mut dyn ConflictHandler,
) -> Result<DagMergeComputation, String> {
	compute_dag_merge_for_evaluation(request, handler, MergeKernelMode::Structured)
}

pub(crate) fn compute_dag_merge_for_evaluation(
	request: DagMergeRequest<'_>,
	handler: &mut dyn ConflictHandler,
	kernel: MergeKernelMode,
) -> Result<DagMergeComputation, String> {
	let DagMergeRequest {
		file_path,
		contributors,
		merge_key_source,
		policies,
		mod_dag,
		ignore_replace_path,
		dep_overrides,
		game_version,
		script_cache,
	} = request;
	let file_dag = induced_file_dag_with_overrides(
		mod_dag,
		file_path,
		contributors,
		ignore_replace_path,
		dep_overrides,
	);
	let vanilla = parse_vanilla_contributor(file_path, contributors, script_cache)?;
	let parsed_contributors =
		parse_active_mod_contributors(file_path, contributors, &file_dag, script_cache)?;
	let mod_hashes = contributor_mod_hashes(contributors, &file_dag);
	compute_dag_merge_from_parsed_with_cache(DagMergeArgs {
		file_dag: &file_dag,
		vanilla: vanilla.as_ref(),
		contributors: &parsed_contributors,
		merge_key_source,
		policies,
		handler,
		mod_hashes: Some(&mod_hashes),
		game_version,
		kernel,
		tree_unit: TreeMergeUnit::File,
	})
}

fn parse_vanilla_contributor(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	script_cache: Option<&WorkspaceScriptCache>,
) -> Result<Option<ParsedScriptFile>, String> {
	let Some(base) = contributors.iter().find(|c| c.is_base_game) else {
		return Ok(None);
	};
	if let Some(parsed) = parsed_from_cache(base, script_cache) {
		return Ok(Some(parsed));
	}
	parse_script_file(&base.mod_id, &base.root_path, &base.absolute_path)
		.map(Some)
		.ok_or_else(|| {
			format!(
				"failed to parse vanilla file {} for {file_path}",
				base.absolute_path.display()
			)
		})
}

fn parse_active_mod_contributors(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	file_dag: &FileDag,
	script_cache: Option<&WorkspaceScriptCache>,
) -> Result<HashMap<ModId, ParsedScriptFile>, String> {
	let by_mod: HashMap<ModId, &ResolvedFileContributor> = contributors
		.iter()
		.filter(|c| !c.is_base_game && !c.is_synthetic_base)
		.map(|c| (ModId(c.mod_id.clone()), c))
		.collect();
	let mut parsed = HashMap::new();
	for mod_id in file_dag.contributors() {
		let contributor = by_mod
			.get(mod_id)
			.ok_or_else(|| format!("missing contributor {} for {file_path}", mod_id.as_str()))?;
		let parsed_file = parsed_from_cache(contributor, script_cache)
			.or_else(|| {
				parse_script_file(
					&contributor.mod_id,
					&contributor.root_path,
					&contributor.absolute_path,
				)
			})
			.ok_or_else(|| {
				format!(
					"failed to parse mod file {} for {}",
					contributor.absolute_path.display(),
					contributor.mod_id,
				)
			})?;
		parsed.insert(mod_id.clone(), parsed_file);
	}
	Ok(parsed)
}

fn parsed_from_cache(
	contributor: &ResolvedFileContributor,
	script_cache: Option<&WorkspaceScriptCache>,
) -> Option<ParsedScriptFile> {
	let relative = contributor
		.absolute_path
		.strip_prefix(&contributor.root_path)
		.ok()?;
	script_cache?.get(&contributor.mod_id, relative).cloned()
}

fn contributor_mod_hashes(
	contributors: &[ResolvedFileContributor],
	file_dag: &FileDag,
) -> HashMap<ModId, String> {
	let by_mod: HashMap<ModId, &ResolvedFileContributor> = contributors
		.iter()
		.filter(|c| !c.is_base_game && !c.is_synthetic_base)
		.map(|c| (ModId(c.mod_id.clone()), c))
		.collect();
	let mut hashes = HashMap::new();
	for mod_id in file_dag.contributors() {
		let Some(contributor) = by_mod.get(mod_id) else {
			continue;
		};
		if let Some(hash) = contributor.mod_hash.as_ref() {
			hashes.insert(mod_id.clone(), hash.clone());
		}
	}
	hashes
}

fn hash_ast_statements(statements: &[AstStatement]) -> Option<String> {
	let encoded = bincode::serialize(statements).ok()?;
	Some(blake3::hash(&encoded).to_hex().to_string())
}

fn hash_dag_apply_input(
	current_statements: &[AstStatement],
	resolved_patches: &[ClausewitzPatch],
) -> Option<String> {
	let encoded = bincode::serialize(&(current_statements, resolved_patches)).ok()?;
	Some(blake3::hash(&encoded).to_hex().to_string())
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum RootDuplicateDeltaKind {
	Append,
	Remove,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RootDuplicateDeltaIdentity {
	kind: RootDuplicateDeltaKind,
	key: String,
	value_identity: String,
	occurrence: usize,
}

fn root_assignments_by_key(statements: &[AstStatement]) -> BTreeMap<String, Vec<&AstStatement>> {
	let mut by_key = BTreeMap::new();
	for statement in statements {
		let AstStatement::Assignment { key, .. } = statement else {
			continue;
		};
		by_key
			.entry(key.clone())
			.or_insert_with(Vec::new)
			.push(statement);
	}
	by_key
}

fn root_duplicate_delta_identity(patch: &ClausewitzPatch) -> Option<RootDuplicateDeltaIdentity> {
	match patch {
		ClausewitzPatch::AppendListItem {
			path,
			key,
			value,
			target_occurrence,
		} if path.is_empty() => Some(RootDuplicateDeltaIdentity {
			kind: RootDuplicateDeltaKind::Append,
			key: key.clone(),
			value_identity: semantic_value_identity(value),
			occurrence: target_occurrence.identity_ordinal(),
		}),
		ClausewitzPatch::RemoveListItem {
			path,
			key,
			value,
			source_occurrence,
		} if path.is_empty() => Some(RootDuplicateDeltaIdentity {
			kind: RootDuplicateDeltaKind::Remove,
			key: key.clone(),
			value_identity: semantic_value_identity(value),
			occurrence: source_occurrence.identity_ordinal(),
		}),
		_ => None,
	}
}

fn consume_represented_delta(
	represented: &mut HashMap<RootDuplicateDeltaIdentity, usize>,
	identity: &RootDuplicateDeltaIdentity,
) -> bool {
	let Some(remaining) = represented.get_mut(identity) else {
		return false;
	};
	if *remaining == 0 {
		return false;
	}
	*remaining -= 1;
	true
}

fn complete_root_duplicate_deltas(
	base_statements: &[AstStatement],
	current_statements: &[AstStatement],
	patches: &mut Vec<ClausewitzPatch>,
	merge_key_source: MergeKeySource,
) {
	if !matches!(
		merge_key_source,
		MergeKeySource::AssignmentKey | MergeKeySource::LeafPath
	) {
		return;
	}
	let base_by_key = root_assignments_by_key(base_statements);
	let current_by_key = root_assignments_by_key(current_statements);
	let repeated_keys = base_by_key
		.iter()
		.chain(&current_by_key)
		.filter_map(|(key, statements)| (statements.len() > 1).then_some(key.clone()))
		.collect::<BTreeSet<_>>();
	let mut represented = HashMap::new();
	for patch in patches.iter() {
		if let Some(identity) = root_duplicate_delta_identity(patch) {
			*represented.entry(identity).or_insert(0usize) += 1;
		}
	}

	for key in repeated_keys {
		let base = base_by_key.get(&key).map(Vec::as_slice).unwrap_or(&[]);
		let current = current_by_key.get(&key).map(Vec::as_slice).unwrap_or(&[]);
		let base_identities = base
			.iter()
			.map(|statement| semantic_statement_identity(statement))
			.collect::<Vec<_>>();
		let current_identities = current
			.iter()
			.map(|statement| semantic_statement_identity(statement))
			.collect::<Vec<_>>();
		let alignment = align_sequences_by(&base_identities, &current_identities, |left, right| {
			left == right
		});
		let base_values = base
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment { value, .. } => Some(value),
				_ => None,
			})
			.collect::<Vec<_>>();
		let current_values = current
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment { value, .. } => Some(value),
				_ => None,
			})
			.collect::<Vec<_>>();
		let base_occurrences = semantic_occurrence_ordinals(&base_values);
		let current_occurrences = semantic_occurrence_ordinals(&current_values);

		for &source_ordinal in &alignment.base_only {
			let AstStatement::Assignment { value, .. } = base[source_ordinal] else {
				unreachable!("root assignment index contains an assignment")
			};
			let source_occurrence =
				ListItemOccurrence::source(base_occurrences[source_ordinal], source_ordinal);
			let identity = RootDuplicateDeltaIdentity {
				kind: RootDuplicateDeltaKind::Remove,
				key: key.clone(),
				value_identity: semantic_value_identity(value),
				occurrence: source_occurrence.identity_ordinal(),
			};
			if !consume_represented_delta(&mut represented, &identity) {
				patches.push(ClausewitzPatch::RemoveListItem {
					path: Vec::new(),
					key: key.clone(),
					value: value.clone(),
					source_occurrence,
				});
			}
		}

		for &target_ordinal in &alignment.overlay_only {
			let AstStatement::Assignment { value, .. } = current[target_ordinal] else {
				unreachable!("root assignment index contains an assignment")
			};
			let source_slot = insertion_source_slot(&alignment, target_ordinal, base.len());
			let target_occurrence = ListItemTarget::new(
				current_occurrences[target_ordinal],
				source_slot,
				target_ordinal,
			);
			let identity = RootDuplicateDeltaIdentity {
				kind: RootDuplicateDeltaKind::Append,
				key: key.clone(),
				value_identity: semantic_value_identity(value),
				occurrence: target_occurrence.identity_ordinal(),
			};
			if !consume_represented_delta(&mut represented, &identity) {
				patches.push(ClausewitzPatch::AppendListItem {
					path: Vec::new(),
					key: key.clone(),
					value: value.clone(),
					target_occurrence,
				});
			}
		}
	}
}

fn cached_or_diff_patches(args: CachedDiffArgs<'_>) -> Vec<ClausewitzPatch> {
	let CachedDiffArgs {
		cache,
		target_path,
		mod_hash,
		base_view_hash,
		current_base,
		current,
		merge_key_source,
		nested_merge_key_source,
		game_version,
	} = args;
	let (Some(cache), Some(mod_hash), Some(base_view_hash)) = (cache, mod_hash, base_view_hash)
	else {
		let mut patches = fold_renames(diff_ast_with_nested(
			current_base,
			current,
			merge_key_source,
			nested_merge_key_source,
		));
		complete_root_duplicate_deltas(
			&current_base.ast.statements,
			&current.ast.statements,
			&mut patches,
			merge_key_source,
		);
		order_patches_by_source(
			&mut patches,
			&current_base.ast.statements,
			&current.ast.statements,
		);
		return patches;
	};
	if let Some(patches) = cache.lookup(
		target_path,
		mod_hash,
		base_view_hash,
		env!("CARGO_PKG_VERSION"),
		game_version,
	) {
		let mut patches = patches;
		complete_root_duplicate_deltas(
			&current_base.ast.statements,
			&current.ast.statements,
			&mut patches,
			merge_key_source,
		);
		order_patches_by_source(
			&mut patches,
			&current_base.ast.statements,
			&current.ast.statements,
		);
		return patches;
	}
	let mut patches = fold_renames(diff_ast_with_nested(
		current_base,
		current,
		merge_key_source,
		nested_merge_key_source,
	));
	complete_root_duplicate_deltas(
		&current_base.ast.statements,
		&current.ast.statements,
		&mut patches,
		merge_key_source,
	);
	order_patches_by_source(
		&mut patches,
		&current_base.ast.statements,
		&current.ast.statements,
	);
	if let Err(err) = cache.store(
		target_path,
		mod_hash,
		base_view_hash,
		env!("CARGO_PKG_VERSION"),
		game_version,
		&patches,
	) {
		tracing::warn!(
			target: "foch::merge::dag_merge",
			path = %target_path,
			error = %err,
			"failed to store mod diff cache entry"
		);
	}
	patches
}

fn cached_or_apply_base(args: CachedApplyArgs<'_>) -> Vec<AstStatement> {
	let CachedApplyArgs {
		cache,
		deps_hash,
		file_path,
		current_statements,
		resolved_patches,
		merge_key_source,
		nested_merge_key_source,
		cache_scope,
		game_version,
	} = args;
	let (Some(cache), Some(deps_hash)) = (cache, deps_hash) else {
		return apply_patches_with_nested(
			current_statements,
			resolved_patches,
			merge_key_source,
			nested_merge_key_source,
		);
	};
	if let Some(statements) = cache.lookup(
		deps_hash,
		file_path,
		env!("CARGO_PKG_VERSION"),
		game_version,
	) {
		record_dag_apply_cache_event(cache_scope, true);
		return statements;
	}
	record_dag_apply_cache_event(cache_scope, false);
	let statements = apply_patches_with_nested(
		current_statements,
		resolved_patches,
		merge_key_source,
		nested_merge_key_source,
	);
	if let Err(err) = cache.store(
		deps_hash,
		file_path,
		env!("CARGO_PKG_VERSION"),
		game_version,
		&statements,
	) {
		tracing::warn!(
			target: "foch::merge::dag_merge",
			path = %file_path,
			error = %err,
			"failed to store DAG base cache entry"
		);
	}
	statements
}

fn serialized_identity<T: serde::Serialize>(value: &T) -> String {
	match bincode::serialize(value) {
		Ok(encoded) => blake3::hash(&encoded).to_hex().to_string(),
		Err(_) => String::new(),
	}
}

fn normalize_merge_result(result: &mut PatchMergeResult) {
	result.handler_resolutions.sort_by(|left, right| {
		left.path
			.cmp(&right.path)
			.then_with(|| left.action.cmp(&right.action))
			.then_with(|| left.source.cmp(&right.source))
			.then_with(|| left.rationale.cmp(&right.rationale))
	});
}

fn patch_intent_addresses(patch: &ClausewitzPatch) -> Vec<PatchIntentAddress> {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. }
		| ClausewitzPatch::RemoveNode { path, key, .. }
		| ClausewitzPatch::InsertNode { path, key, .. }
		| ClausewitzPatch::ReplaceBlock { path, key, .. } => {
			vec![PatchIntentAddress::Node(path.clone(), key.clone())]
		}
		ClausewitzPatch::AppendListItem {
			path,
			key,
			value,
			target_occurrence,
		} => vec![PatchIntentAddress::ListItem(
			path.clone(),
			key.clone(),
			semantic_value_identity(value),
			target_occurrence.identity_ordinal(),
		)],
		ClausewitzPatch::RemoveListItem {
			path,
			key,
			value,
			source_occurrence,
		} => vec![PatchIntentAddress::ListItem(
			path.clone(),
			key.clone(),
			semantic_value_identity(value),
			source_occurrence.identity_ordinal(),
		)],
		ClausewitzPatch::AppendBlockItem { path, value }
		| ClausewitzPatch::RemoveBlockItem { path, value } => vec![PatchIntentAddress::BlockItem(
			path.clone(),
			serialized_identity(value),
		)],
		ClausewitzPatch::Rename {
			path,
			old_key,
			new_key,
		} => vec![
			PatchIntentAddress::Node(path.clone(), old_key.clone()),
			PatchIntentAddress::Node(path.clone(), new_key.clone()),
		],
	}
}

fn append_unique_patch(target: &mut Vec<ClausewitzPatch>, patch: &ClausewitzPatch) {
	if !target.contains(patch) {
		target.push(patch.clone());
	}
}

fn build_branch_patches(
	file_path: &str,
	template: Option<&ParsedScriptFile>,
	base_statements: &[AstStatement],
	effective_statements: &[AstStatement],
	parent_intents: &[ClausewitzPatch],
	direct_patches: &[ClausewitzPatch],
	key_sources: PatchKeySources,
) -> (Vec<ClausewitzPatch>, Vec<ClausewitzPatch>) {
	let base = synthesized_parsed_file(file_path, template, base_statements.to_vec());
	let effective = synthesized_parsed_file(file_path, template, effective_statements.to_vec());
	let mut branch_patches = fold_renames(diff_ast_with_nested(
		&base,
		&effective,
		key_sources.root,
		key_sources.nested,
	));
	complete_root_duplicate_deltas(
		base_statements,
		effective_statements,
		&mut branch_patches,
		key_sources.root,
	);
	order_patches_by_source(&mut branch_patches, base_statements, effective_statements);
	let net_addresses = branch_patches
		.iter()
		.flat_map(patch_intent_addresses)
		.collect::<BTreeSet<_>>();
	let direct_addresses = direct_patches
		.iter()
		.flat_map(patch_intent_addresses)
		.collect::<BTreeSet<_>>();
	let mut intent_only_patches = Vec::new();

	for patch in parent_intents {
		let addresses = patch_intent_addresses(patch);
		if !addresses.is_empty()
			&& addresses.iter().all(|address| {
				!direct_addresses.contains(address) && !net_addresses.contains(address)
			}) {
			append_unique_patch(&mut intent_only_patches, patch);
		}
	}
	for patch in direct_patches {
		let addresses = patch_intent_addresses(patch);
		if !addresses.is_empty()
			&& addresses
				.iter()
				.all(|address| !net_addresses.contains(address))
		{
			append_unique_patch(&mut intent_only_patches, patch);
		}
	}
	for patch in &intent_only_patches {
		append_unique_patch(&mut branch_patches, patch);
	}

	(branch_patches, intent_only_patches)
}

fn extend_unique_conflicts(target: &mut Vec<PatchResolution>, source: &[PatchResolution]) {
	for conflict in source {
		if !target.contains(conflict) {
			target.push(conflict.clone());
		}
	}
}

impl EffectiveNodeProtocol<PatchBaselineDagState> for PatchBaselineDagProtocol<'_> {
	fn effective_node(
		&mut self,
		request: EffectiveNodeRequest<'_, PatchBaselineDagState>,
	) -> Result<PatchBaselineDagState, String> {
		extend_unique_conflicts(
			&mut self.seen_pending_conflicts,
			&request.parent.pending_conflicts,
		);
		let current_base = synthesized_parsed_file(
			self.file_dag.file_path(),
			self.template,
			request.parent.statements.clone(),
		);
		let base_view_hash = hash_ast_statements(&current_base.ast.statements);
		let patches = cached_or_diff_patches(CachedDiffArgs {
			cache: self.diff_cache,
			target_path: self.file_dag.file_path(),
			mod_hash: self
				.mod_hashes
				.and_then(|hashes| hashes.get(request.mod_id).map(String::as_str)),
			base_view_hash: base_view_hash.as_deref(),
			current_base: &current_base,
			current: request.source,
			merge_key_source: self.merge_key_source,
			nested_merge_key_source: self.policies.nested_merge_key_source,
			game_version: &self.diff_cache_context,
		});
		let pending_conflicts =
			pending_after_direct_delta(&request.parent.pending_conflicts, &patches);
		let apply_hash = self
			.dag_base_cache
			.and_then(|_| hash_dag_apply_input(&request.parent.statements, &patches));
		let effective_statements = cached_or_apply_base(CachedApplyArgs {
			cache: self.dag_base_cache,
			deps_hash: apply_hash.as_deref(),
			file_path: self.file_dag.file_path(),
			current_statements: &request.parent.statements,
			resolved_patches: &patches,
			merge_key_source: self.merge_key_source,
			nested_merge_key_source: self.policies.nested_merge_key_source,
			cache_scope: DagApplyCacheScope::EffectiveNode,
			game_version: &self.dag_base_cache_context,
		});
		let (_, intent_only_patches) = build_branch_patches(
			self.file_dag.file_path(),
			self.template,
			self.base_statements,
			&effective_statements,
			&request.parent.intent_only_patches,
			&patches,
			PatchKeySources {
				root: self.merge_key_source,
				nested: self.policies.nested_merge_key_source,
			},
		);
		self.mod_patches.push((
			request.mod_id.0.clone(),
			self.file_dag.precedence_of(request.mod_id),
			patches,
		));
		Ok(PatchBaselineDagState {
			statements: effective_statements,
			intent_only_patches,
			pending_conflicts,
		})
	}
}

impl DagJoinProtocol<PatchBaselineDagState> for PatchBaselineDagProtocol<'_> {
	fn join(
		&mut self,
		request: DagJoinRequest<'_, PatchBaselineDagState>,
	) -> Result<PatchBaselineDagState, String> {
		let mut pending_conflicts = Vec::new();
		let mut all_intent_only = Vec::new();
		let mut frontier_addresses = BTreeSet::new();
		let mut patch_sets = Vec::with_capacity(request.revisions.len());
		for revision in request.revisions {
			extend_unique_conflicts(&mut pending_conflicts, &revision.state.pending_conflicts);
			let relative_intents = revision
				.state
				.intent_only_patches
				.iter()
				.filter(|patch| !request.base.intent_only_patches.contains(*patch))
				.cloned()
				.collect::<Vec<_>>();
			let (branch_patches, branch_intent_only) = build_branch_patches(
				request.file_dag.file_path(),
				self.template,
				&request.base.statements,
				&revision.state.statements,
				&[],
				&relative_intents,
				PatchKeySources {
					root: self.merge_key_source,
					nested: self.policies.nested_merge_key_source,
				},
			);
			for patch in &branch_intent_only {
				append_unique_patch(&mut all_intent_only, patch);
			}
			frontier_addresses.extend(branch_patches.iter().flat_map(patch_intent_addresses));
			patch_sets.push((
				revision.mod_id.0.clone(),
				revision.precedence,
				branch_patches,
			));
		}

		patch_sets.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
		let mut merge_result = merge_patch_sets_for_file(
			patch_sets,
			self.policies,
			self.handler,
			Some(Path::new(request.file_dag.file_path())),
		)
		.map_err(|error| error.to_string())?;
		normalize_merge_result(&mut merge_result);
		let new_conflicts = std::mem::take(&mut merge_result.conflicts);
		extend_unique_conflicts(&mut pending_conflicts, &new_conflicts);
		let resolved = resolved_patches(&merge_result);
		let mut surviving_intents = request.base.intent_only_patches.clone();
		surviving_intents.retain(|patch| {
			patch_intent_addresses(patch)
				.iter()
				.all(|address| !frontier_addresses.contains(address))
		});
		let materialized = resolved
			.into_iter()
			.filter(|patch| {
				if all_intent_only.contains(patch) {
					append_unique_patch(&mut surviving_intents, patch);
					false
				} else {
					true
				}
			})
			.collect::<Vec<_>>();
		let deps_hash = self
			.dag_base_cache
			.and_then(|_| hash_dag_apply_input(&request.base.statements, &materialized));
		let statements = cached_or_apply_base(CachedApplyArgs {
			cache: self.dag_base_cache,
			deps_hash: deps_hash.as_deref(),
			file_path: request.file_dag.file_path(),
			current_statements: &request.base.statements,
			resolved_patches: &materialized,
			merge_key_source: self.merge_key_source,
			nested_merge_key_source: self.policies.nested_merge_key_source,
			cache_scope: DagApplyCacheScope::ResolvedBranchState,
			game_version: &self.dag_base_cache_context,
		});
		extend_merge_result(&mut self.merge_result, merge_result);
		extend_unique_conflicts(&mut self.seen_pending_conflicts, &pending_conflicts);
		Ok(PatchBaselineDagState {
			statements,
			intent_only_patches: surviving_intents,
			pending_conflicts,
		})
	}
}

fn pending_after_direct_delta(
	pending_conflicts: &[PatchResolution],
	direct_patches: &[ClausewitzPatch],
) -> Vec<PatchResolution> {
	let direct_addresses = direct_patches
		.iter()
		.flat_map(overwrite_addresses)
		.collect::<HashSet<_>>();
	pending_conflicts
		.iter()
		.filter(|conflict| match conflict {
			PatchResolution::Conflict { address, .. } => !direct_addresses.contains(&(
				address.path.clone(),
				logical_conflict_key(&address.key).to_string(),
			)),
			_ => true,
		})
		.cloned()
		.collect()
}

fn logical_conflict_key(key: &str) -> &str {
	key.strip_prefix("__list_item__::")
		.and_then(|rest| rest.split_once("::").map(|(logical_key, _)| logical_key))
		.unwrap_or(key)
}

fn record_downstream_resolutions(
	merge_result: &mut PatchMergeResult,
	seen_pending: &[PatchResolution],
	final_pending: &[PatchResolution],
	file_path: &str,
) {
	for conflict in seen_pending {
		if final_pending.contains(conflict) {
			continue;
		}
		let PatchResolution::Conflict {
			address, patches, ..
		} = conflict
		else {
			continue;
		};
		let contributor_summary = patches
			.iter()
			.map(|patch| patch.mod_id.as_str())
			.collect::<Vec<_>>()
			.join(", ");
		merge_result
			.handler_resolutions
			.push(HandlerResolutionRecord {
				path: file_path.to_string(),
				action: "downstream_override".to_string(),
				source: Some(format!("{}::{}", address.path.join("/"), address.key)),
				rationale: Some(format!(
					"upstream conflict between {contributor_summary} resolved by a descendant direct delta"
				)),
			});
		merge_result.handler_resolved_count += 1;
		if merge_result.stats.conflict_patches > 0 {
			merge_result.stats.conflict_patches -= 1;
		}
	}
}

pub(crate) fn compute_dag_merge_from_parsed(
	file_dag: &FileDag,
	vanilla: Option<&ParsedScriptFile>,
	contributors: &HashMap<ModId, ParsedScriptFile>,
	merge_key_source: MergeKeySource,
	policies: &MergePolicies,
	handler: &mut dyn ConflictHandler,
) -> Result<DagMergeComputation, String> {
	compute_dag_merge_from_parsed_for_evaluation(
		file_dag,
		vanilla,
		contributors,
		merge_key_source,
		policies,
		handler,
		MergeKernelMode::Structured,
	)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_dag_merge_from_parsed_for_evaluation(
	file_dag: &FileDag,
	vanilla: Option<&ParsedScriptFile>,
	contributors: &HashMap<ModId, ParsedScriptFile>,
	merge_key_source: MergeKeySource,
	policies: &MergePolicies,
	handler: &mut dyn ConflictHandler,
	kernel: MergeKernelMode,
) -> Result<DagMergeComputation, String> {
	compute_dag_merge_from_parsed_with_cache(DagMergeArgs {
		file_dag,
		vanilla,
		contributors,
		merge_key_source,
		policies,
		handler,
		mod_hashes: None,
		game_version: "unknown",
		kernel,
		tree_unit: inferred_tree_merge_unit(vanilla, contributors),
	})
}

fn inferred_tree_merge_unit(
	vanilla: Option<&ParsedScriptFile>,
	contributors: &HashMap<ModId, ParsedScriptFile>,
) -> TreeMergeUnit {
	let is_event = vanilla
		.into_iter()
		.chain(contributors.values())
		.any(|file| file.file_kind.as_str() == "events");
	if is_event {
		TreeMergeUnit::File
	} else {
		TreeMergeUnit::DefinitionModule
	}
}

fn compute_dag_merge_from_parsed_with_cache(
	args: DagMergeArgs<'_>,
) -> Result<DagMergeComputation, String> {
	let caches_enabled = args.mod_hashes.is_some_and(|hashes| !hashes.is_empty());
	let diff_cache = caches_enabled.then(ModDiffCache::open_default);
	let dag_base_cache = caches_enabled.then(DagBaseCache::open_default);
	compute_dag_merge_from_parsed_with_caches(
		args,
		PatchCaches {
			diff: diff_cache.as_ref(),
			dag_base: dag_base_cache.as_ref(),
		},
	)
}

fn compute_dag_merge_from_parsed_with_caches(
	args: DagMergeArgs<'_>,
	caches: PatchCaches<'_>,
) -> Result<DagMergeComputation, String> {
	let DagMergeArgs {
		file_dag,
		vanilla,
		contributors,
		merge_key_source,
		policies,
		handler,
		mod_hashes,
		game_version,
		kernel,
		tree_unit,
	} = args;
	let base_statements = final_base_statements(file_dag, vanilla);
	let diff_cache_game_version = format!(
		"parent-relative-v10 {game_version} kernel={} merge_key={merge_key_source:?} nested_merge_key={:?}",
		kernel.as_str(),
		policies.nested_merge_key_source,
	);
	let policy_debug = format!("{policies:?}");
	let policy_hash = blake3::hash(policy_debug.as_bytes()).to_hex().to_string();
	let dag_base_game_version = format!(
		"parent-relative-v10 {game_version} kernel={} merge_key={merge_key_source:?} policies={policy_hash}",
		kernel.as_str(),
	);
	let template = template_for(file_dag, vanilla, contributors);
	let PatchCaches {
		diff: diff_cache,
		dag_base: dag_base_cache,
	} = caches;
	let artifacts = match kernel {
		MergeKernelMode::Legacy => {
			let root = PatchBaselineDagState {
				statements: base_statements.clone(),
				intent_only_patches: Vec::new(),
				pending_conflicts: Vec::new(),
			};
			let mut protocol = PatchBaselineDagProtocol {
				file_dag,
				base_statements: &base_statements,
				template,
				merge_key_source,
				policies,
				handler,
				mod_hashes,
				diff_cache,
				dag_base_cache,
				diff_cache_context: diff_cache_game_version,
				dag_base_cache_context: dag_base_game_version,
				mod_patches: Vec::new(),
				merge_result: PatchMergeResult::default(),
				seen_pending_conflicts: Vec::new(),
			};
			let pipeline = execute_dag_pipeline(file_dag, contributors, root, &mut protocol)?;
			let DagPipelineResult {
				final_state,
				parent_states,
				..
			} = pipeline;
			record_downstream_resolutions(
				&mut protocol.merge_result,
				&protocol.seen_pending_conflicts,
				&final_state.pending_conflicts,
				file_dag.file_path(),
			);
			protocol.merge_result.conflicts = final_state.pending_conflicts;
			normalize_merge_result(&mut protocol.merge_result);
			DagPipelineArtifacts {
				mod_patches: protocol.mod_patches,
				merged_statements: final_state.statements,
				merge_result: protocol.merge_result,
				parent_statements: parent_states
					.into_iter()
					.map(|(mod_id, state)| (mod_id, state.statements))
					.collect(),
			}
		}
		MergeKernelMode::Structured => {
			let file_adapter = ClausewitzFileAdapter;
			let event_adapter = EventFileAdapter;
			let module_adapter = DefinitionModuleAdapter;
			let adapter: &dyn TreeMergeAdapter = match tree_unit {
				TreeMergeUnit::File
					if template.is_some_and(|file| file.file_kind.as_str() == "events") =>
				{
					&event_adapter
				}
				TreeMergeUnit::File => &file_adapter,
				TreeMergeUnit::DefinitionModule => &module_adapter,
			};
			let root = TreeDagState {
				statements: base_statements.clone(),
				unresolved_conflicts: Vec::new(),
				handler_resolutions: Vec::new(),
				resolved_conflict_ids: Vec::new(),
				external_file_resolution: None,
				keep_existing: false,
			};
			let mut protocol = TreeDagProtocol::new(adapter, policies, vanilla.is_some(), handler);
			let pipeline = execute_dag_pipeline(file_dag, contributors, root, &mut protocol)?;
			let DagPipelineResult {
				final_state,
				parent_states,
				..
			} = pipeline;
			let mod_patches = compute_tree_diagnostic_patches(
				file_dag,
				contributors,
				&parent_states,
				template,
				merge_key_source,
				policies,
				mod_hashes,
				diff_cache,
				&diff_cache_game_version,
			)?;
			let merge_result = tree_merge_result(file_dag.file_path(), &final_state);
			DagPipelineArtifacts {
				mod_patches,
				merged_statements: final_state.statements,
				merge_result,
				parent_statements: parent_states
					.into_iter()
					.map(|(mod_id, state)| (mod_id, state.statements))
					.collect(),
			}
		}
	};

	let direct_definition_keys = compute_direct_definition_keys(&artifacts.mod_patches);
	let definition_provenance = compute_definition_provenance(
		&artifacts.merged_statements,
		contributors,
		file_dag,
		&artifacts.mod_patches,
		&direct_definition_keys,
		&artifacts.parent_statements,
	);
	let definition_participants =
		compute_definition_participants(&direct_definition_keys, file_dag);
	Ok(DagMergeComputation {
		mod_patches: artifacts.mod_patches,
		base_statements,
		merged_statements: artifacts.merged_statements,
		merge_result: artifacts.merge_result,
		definition_provenance,
		definition_participants,
	})
}

struct DagPipelineArtifacts {
	mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	merged_statements: Vec<AstStatement>,
	merge_result: PatchMergeResult,
	parent_statements: HashMap<ModId, Vec<AstStatement>>,
}

fn tree_merge_result(file_path: &str, state: &TreeDagState) -> PatchMergeResult {
	let conflicts = state
		.unresolved_conflicts
		.iter()
		.map(|conflict| {
			let patches = conflict
				.candidates
				.iter()
				.filter_map(|candidate| {
					tree_conflict_candidate_patch(
						&conflict.address_path,
						&conflict.address_key,
						conflict.base_statement.as_ref(),
						candidate,
					)
					.map(|patch| AttributedPatch {
						mod_id: candidate.source_id.clone(),
						precedence: candidate.precedence,
						patch,
					})
				})
				.collect::<Vec<_>>();
			PatchResolution::Conflict {
				address: PatchAddress {
					path: conflict.address_path.clone(),
					key: conflict.address_key.clone(),
				},
				patches,
				reason: conflict.reason.clone(),
			}
		})
		.collect::<Vec<_>>();
	let mut result = PatchMergeResult {
		handler_resolved_count: state.resolved_conflict_ids.len(),
		handler_resolutions: state.handler_resolutions.clone(),
		conflicts,
		..PatchMergeResult::default()
	};
	result.stats.conflict_patches = result.conflicts.len();
	result.stats.total_patches = result
		.conflicts
		.iter()
		.map(|conflict| match conflict {
			PatchResolution::Conflict { patches, .. } => patches.len(),
			PatchResolution::Resolved(_) | PatchResolution::AutoMerged { .. } => 0,
		})
		.sum();
	if let Some(source) = &state.external_file_resolution {
		result
			.external_file_resolutions
			.insert(PathBuf::from(file_path), source.clone());
	}
	if state.keep_existing {
		result.keep_existing_paths.insert(PathBuf::from(file_path));
	}
	result
}

fn tree_conflict_candidate_patch(
	path: &[String],
	key: &str,
	base: Option<&AstStatement>,
	candidate: &TreeConflictCandidate,
) -> Option<ClausewitzPatch> {
	match (base, candidate.statement.as_ref()) {
		(
			Some(AstStatement::Assignment {
				value: old_value, ..
			}),
			Some(AstStatement::Assignment {
				value: new_value, ..
			}),
		) if matches!(old_value, AstValue::Scalar { .. })
			&& matches!(new_value, AstValue::Scalar { .. }) =>
		{
			Some(ClausewitzPatch::SetValue {
				path: path.to_vec(),
				key: key.to_string(),
				old_value: old_value.clone(),
				new_value: new_value.clone(),
			})
		}
		(Some(old_statement), Some(new_statement)) => Some(ClausewitzPatch::ReplaceBlock {
			path: path.to_vec(),
			key: key.to_string(),
			old_statement: old_statement.clone(),
			new_statement: new_statement.clone(),
		}),
		(None, Some(statement)) => Some(ClausewitzPatch::InsertNode {
			path: path.to_vec(),
			key: key.to_string(),
			statement: statement.clone(),
		}),
		(Some(removed), None) => Some(ClausewitzPatch::RemoveNode {
			path: path.to_vec(),
			key: key.to_string(),
			removed: removed.clone(),
		}),
		(None, None) => None,
	}
}

#[allow(clippy::too_many_arguments)]
fn compute_tree_diagnostic_patches(
	file_dag: &FileDag,
	contributors: &HashMap<ModId, ParsedScriptFile>,
	parent_states: &HashMap<ModId, TreeDagState>,
	template: Option<&ParsedScriptFile>,
	merge_key_source: MergeKeySource,
	policies: &MergePolicies,
	mod_hashes: Option<&HashMap<ModId, String>>,
	diff_cache: Option<&ModDiffCache>,
	cache_context: &str,
) -> Result<Vec<(String, usize, Vec<ClausewitzPatch>)>, String> {
	// These deltas feed stale-vanilla, dependency-misuse, and provenance
	// reporting only. Tree DAG state and joins never consume them.
	let all_contributors = file_dag
		.contributors()
		.iter()
		.cloned()
		.collect::<BTreeSet<_>>();
	let mut mod_patches = Vec::new();
	for level in topo_levels(&all_contributors, file_dag) {
		for mod_id in level {
			let parent = parent_states.get(&mod_id).ok_or_else(|| {
				format!(
					"missing diagnostic parent state {} for {}",
					mod_id.as_str(),
					file_dag.file_path()
				)
			})?;
			let current = contributors.get(&mod_id).ok_or_else(|| {
				format!(
					"missing diagnostic contributor {} for {}",
					mod_id.as_str(),
					file_dag.file_path()
				)
			})?;
			let current_base =
				synthesized_parsed_file(file_dag.file_path(), template, parent.statements.clone());
			let base_view_hash = hash_ast_statements(&current_base.ast.statements);
			let patches = cached_or_diff_patches(CachedDiffArgs {
				cache: diff_cache,
				target_path: file_dag.file_path(),
				mod_hash: mod_hashes.and_then(|hashes| hashes.get(&mod_id).map(String::as_str)),
				base_view_hash: base_view_hash.as_deref(),
				current_base: &current_base,
				current,
				merge_key_source,
				nested_merge_key_source: policies.nested_merge_key_source,
				game_version: cache_context,
			});
			let precedence = file_dag.precedence_of(&mod_id);
			mod_patches.push((mod_id.0, precedence, patches));
		}
	}
	Ok(mod_patches)
}

/// Canonical, span-free text signature of a single statement (used as a hashable
/// identity for set operations on block children).
fn statement_signature(stmt: &AstStatement) -> String {
	semantic_statement_identity(stmt)
}

/// Signatures of the immediate children of a block-valued assignment (empty for
/// scalars / non-blocks).
fn block_child_signatures(stmt: &AstStatement) -> BTreeSet<String> {
	match stmt {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => items.iter().map(statement_signature).collect(),
		_ => BTreeSet::new(),
	}
}

/// All top-level `Assignment`s with the given key. A key can repeat at file root
/// (e.g. a scripted-effect union emits several blocks under the same name, which
/// the game runs in sequence), so provenance must aggregate across all of them.
fn same_key_statements<'a>(statements: &'a [AstStatement], key: &str) -> Vec<&'a AstStatement> {
	statements
		.iter()
		.filter(|stmt| matches!(stmt, AstStatement::Assignment { key: k, .. } if k == key))
		.collect()
}

/// Whole-statement signature multiplicities for same-key root definitions.
fn whole_signature_counts(statements: &[&AstStatement]) -> BTreeMap<String, usize> {
	let mut counts = BTreeMap::new();
	for statement in statements {
		*counts.entry(statement_signature(statement)).or_default() += 1;
	}
	counts
}

/// Union of immediate child signatures across a set of same-key blocks.
fn union_child_signatures(statements: &[&AstStatement]) -> BTreeSet<String> {
	statements
		.iter()
		.flat_map(|stmt| block_child_signatures(stmt))
		.collect()
}

fn direct_definition_contribution_survives(
	parent_statements: &[AstStatement],
	mod_statements: &[AstStatement],
	merged_statements: &[AstStatement],
	key: &str,
) -> bool {
	let parent_blocks = same_key_statements(parent_statements, key);
	let mod_blocks = same_key_statements(mod_statements, key);
	let final_blocks = same_key_statements(merged_statements, key);
	let parent_whole = whole_signature_counts(&parent_blocks);
	let mod_whole = whole_signature_counts(&mod_blocks);
	let final_whole = whole_signature_counts(&final_blocks);

	if parent_blocks.len() > 1 || mod_blocks.len() > 1 || final_blocks.len() > 1 {
		let added_survives = mod_whole.iter().any(|(signature, mod_count)| {
			let parent_count = parent_whole.get(signature).copied().unwrap_or_default();
			let final_count = final_whole.get(signature).copied().unwrap_or_default();
			*mod_count > parent_count && final_count > parent_count
		});
		if added_survives {
			return true;
		}
		return parent_whole.iter().any(|(signature, parent_count)| {
			let mod_count = mod_whole.get(signature).copied().unwrap_or_default();
			let final_count = final_whole.get(signature).copied().unwrap_or_default();
			mod_count < *parent_count && final_count < *parent_count
		});
	}

	if mod_whole
		.keys()
		.any(|signature| final_whole.contains_key(signature))
	{
		return true;
	}
	let parent_children = union_child_signatures(&parent_blocks);
	let mod_children = union_child_signatures(&mod_blocks);
	let final_children = union_child_signatures(&final_blocks);
	let added_children = mod_children
		.difference(&parent_children)
		.cloned()
		.collect::<BTreeSet<_>>();
	added_children
		.intersection(&final_children)
		.next()
		.is_some()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootListOperationKind {
	Append,
	Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RootListOperation {
	key: String,
	value_identity: String,
	occurrence: usize,
	kind: RootListOperationKind,
}

struct RootListHistory {
	operations: HashMap<ModId, Vec<RootListOperation>>,
	children: HashMap<ModId, Vec<ModId>>,
	sinks: BTreeSet<ModId>,
}

impl RootListHistory {
	fn new(mod_patches: &[(String, usize, Vec<ClausewitzPatch>)], file_dag: &FileDag) -> Self {
		let operations = mod_patches
			.iter()
			.map(|(mod_id, _, patches)| {
				let operations = patches
					.iter()
					.filter_map(|patch| match patch {
						ClausewitzPatch::AppendListItem {
							path,
							key,
							value,
							target_occurrence,
						} if path.is_empty() => Some(RootListOperation {
							key: key.clone(),
							value_identity: semantic_value_identity(value),
							occurrence: target_occurrence.identity_ordinal(),
							kind: RootListOperationKind::Append,
						}),
						ClausewitzPatch::RemoveListItem {
							path,
							key,
							value,
							source_occurrence,
						} if path.is_empty() => Some(RootListOperation {
							key: key.clone(),
							value_identity: semantic_value_identity(value),
							occurrence: source_occurrence.identity_ordinal(),
							kind: RootListOperationKind::Remove,
						}),
						_ => None,
					})
					.collect::<Vec<_>>();
				(ModId(mod_id.clone()), operations)
			})
			.collect::<HashMap<_, _>>();

		let mut children = file_dag
			.contributors()
			.iter()
			.cloned()
			.map(|mod_id| (mod_id, Vec::new()))
			.collect::<HashMap<_, _>>();
		for child in file_dag.contributors() {
			for parent in file_dag.parents_of(child) {
				children
					.entry(parent.clone())
					.or_default()
					.push(child.clone());
			}
		}
		for descendants in children.values_mut() {
			descendants.sort_by(|left, right| {
				file_dag
					.precedence_of(left)
					.cmp(&file_dag.precedence_of(right))
					.then_with(|| left.cmp(right))
			});
			descendants.dedup();
		}

		Self {
			operations,
			children,
			sinks: sink_mods(file_dag).into_iter().collect(),
		}
	}

	fn surviving_direct_intent(&self, mod_id: &ModId, key: &str) -> Option<bool> {
		let direct = self
			.operations
			.get(mod_id)?
			.iter()
			.filter(|operation| operation.key == key)
			.collect::<Vec<_>>();
		if direct.is_empty() {
			return None;
		}
		Some(
			direct
				.into_iter()
				.any(|operation| self.intent_reaches_final_sink(mod_id, operation)),
		)
	}

	fn intent_reaches_final_sink(&self, origin: &ModId, intent: &RootListOperation) -> bool {
		let opposing = match intent.kind {
			RootListOperationKind::Append => RootListOperationKind::Remove,
			RootListOperationKind::Remove => RootListOperationKind::Append,
		};
		let mut stack = vec![origin.clone()];
		let mut visited = HashSet::new();
		while let Some(candidate) = stack.pop() {
			if !visited.insert(candidate.clone()) {
				continue;
			}
			if candidate != *origin
				&& self.operations.get(&candidate).is_some_and(|operations| {
					operations.iter().any(|operation| {
						operation.key == intent.key
							&& operation.value_identity == intent.value_identity
							&& operation.occurrence == intent.occurrence
							&& operation.kind == opposing
					})
				}) {
				continue;
			}
			if self.sinks.contains(&candidate) {
				return true;
			}
			if let Some(children) = self.children.get(&candidate) {
				stack.extend(children.iter().rev().cloned());
			}
		}
		false
	}
}

fn compute_direct_definition_keys(
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
) -> HashMap<ModId, BTreeSet<String>> {
	mod_patches
		.iter()
		.map(|(mod_id, _, patches)| {
			let keys = patches
				.iter()
				.flat_map(patch_top_level_keys)
				.collect::<BTreeSet<_>>();
			(ModId(mod_id.clone()), keys)
		})
		.collect()
}

fn patch_top_level_keys(patch: &ClausewitzPatch) -> Vec<String> {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. }
		| ClausewitzPatch::RemoveNode { path, key, .. }
		| ClausewitzPatch::InsertNode { path, key, .. }
		| ClausewitzPatch::AppendListItem { path, key, .. }
		| ClausewitzPatch::RemoveListItem { path, key, .. }
		| ClausewitzPatch::ReplaceBlock { path, key, .. } => {
			vec![path.first().cloned().unwrap_or_else(|| key.clone())]
		}
		ClausewitzPatch::AppendBlockItem { path, .. }
		| ClausewitzPatch::RemoveBlockItem { path, .. } => path.first().cloned().into_iter().collect(),
		ClausewitzPatch::Rename {
			path,
			old_key,
			new_key,
		} => match path.first() {
			Some(top_level) => vec![top_level.clone()],
			None => vec![old_key.clone(), new_key.clone()],
		},
	}
}

/// Per top-level definition, the mods whose content is **adopted** into the final
/// merged output, in ascending DAG-precedence order.
///
/// A contributor is credited for a key when its parent-relative change survives
/// in the output:
///
/// - repeated-key definitions compare added/removed statements against the
///   actual parent view, so inherited siblings cannot establish provenance;
/// - singular definitions require the changed whole statement or an added
///   child to survive in the output;
/// - inherited no-ops and overridden losers are excluded, while explicit
///   restorations to vanilla and reset-time reintroductions remain eligible.
///
/// Keys can repeat at file root, so all same-key blocks are aggregated. A mod is
/// considered only for keys touched by its parent-relative direct delta, which
/// prevents a child from receiving credit for unchanged inherited definitions.
fn compute_definition_provenance(
	merged_statements: &[AstStatement],
	contributors: &HashMap<ModId, ParsedScriptFile>,
	file_dag: &FileDag,
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
	direct_definition_keys: &HashMap<ModId, BTreeSet<String>>,
	parent_statements: &HashMap<ModId, Vec<AstStatement>>,
) -> BTreeMap<String, Vec<String>> {
	let mut ordered: Vec<ModId> = file_dag.contributors().to_vec();
	ordered.sort_by_key(|mod_id| file_dag.precedence_of(mod_id));

	let keys: BTreeSet<&str> = merged_statements
		.iter()
		.filter_map(|stmt| match stmt {
			AstStatement::Assignment { key, .. } => Some(key.as_str()),
			_ => None,
		})
		.collect();

	let list_history = RootListHistory::new(mod_patches, file_dag);
	let mut provenance: BTreeMap<String, Vec<String>> = BTreeMap::new();
	for key in keys {
		let mut adopted: Vec<String> = Vec::new();
		for mod_id in &ordered {
			if !direct_definition_keys
				.get(mod_id)
				.is_some_and(|keys| keys.contains(key))
			{
				continue;
			}
			let Some(parsed) = contributors.get(mod_id) else {
				continue;
			};
			let Some(parent) = parent_statements.get(mod_id) else {
				continue;
			};
			let direct_survives = direct_definition_contribution_survives(
				parent,
				&parsed.ast.statements,
				merged_statements,
				key,
			);
			let survives = list_history
				.surviving_direct_intent(mod_id, key)
				.map_or(direct_survives, |reaches_sink| {
					reaches_sink && direct_survives
				});
			if survives {
				adopted.push(mod_id.0.clone());
			}
		}
		if !adopted.is_empty() {
			provenance.insert(key.to_string(), adopted);
		}
	}
	provenance
}

fn compute_definition_participants(
	direct_definition_keys: &HashMap<ModId, BTreeSet<String>>,
	file_dag: &FileDag,
) -> BTreeMap<String, Vec<MergeTraceContributor>> {
	let participant_set: BTreeSet<ModId> = file_dag.contributors().iter().cloned().collect();
	let levels = topo_levels(&participant_set, file_dag);
	let mut dag_level_by_mod: BTreeMap<ModId, usize> = BTreeMap::new();
	for (level_idx, level) in levels.iter().enumerate() {
		for mod_id in level {
			dag_level_by_mod.insert(mod_id.clone(), level_idx);
		}
	}

	let mut participants: BTreeMap<String, Vec<MergeTraceContributor>> = BTreeMap::new();
	let mut ordered: Vec<ModId> = file_dag.contributors().to_vec();
	ordered.sort_by_key(|mod_id| {
		(
			dag_level_by_mod.get(mod_id).copied().unwrap_or(usize::MAX),
			file_dag.precedence_of(mod_id),
			mod_id.0.clone(),
		)
	});
	for mod_id in ordered {
		let Some(keys) = direct_definition_keys.get(&mod_id) else {
			continue;
		};
		for key in keys {
			let entry = participants.entry(key.clone()).or_default();
			entry.push(MergeTraceContributor {
				mod_id: mod_id.0.clone(),
				precedence: file_dag.precedence_of(&mod_id),
				dag_level: dag_level_by_mod.get(&mod_id).copied().unwrap_or(usize::MAX),
			});
		}
	}
	participants
}

fn final_base_statements(
	file_dag: &FileDag,
	vanilla: Option<&ParsedScriptFile>,
) -> Vec<AstStatement> {
	if file_dag
		.contributors()
		.iter()
		.any(|mod_id| file_dag.replaces_path(mod_id))
	{
		Vec::new()
	} else {
		vanilla
			.map(|base| base.ast.statements.clone())
			.unwrap_or_default()
	}
}

fn resolved_patches(merge_result: &PatchMergeResult) -> Vec<ClausewitzPatch> {
	merge_result
		.resolved
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Resolved(patch) => Some(patch.clone()),
			PatchResolution::AutoMerged { result, .. } => Some(result.clone()),
			PatchResolution::Conflict { .. } => None,
		})
		.collect()
}

/// Addresses that this patch semantically "overwrites" at the given level.
///
/// A downstream level whose patch set touches any of these (path, key) pairs
/// makes a sibling-conflict at that same address moot: whatever the upstream
/// disagreement was, the downstream contributor decided what the final state
/// should be. Returning a Vec lets `Rename` contribute both endpoints.
fn overwrite_addresses(patch: &ClausewitzPatch) -> Vec<(Vec<String>, String)> {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. }
		| ClausewitzPatch::ReplaceBlock { path, key, .. }
		| ClausewitzPatch::RemoveNode { path, key, .. }
		| ClausewitzPatch::InsertNode { path, key, .. }
		| ClausewitzPatch::AppendListItem { path, key, .. }
		| ClausewitzPatch::RemoveListItem { path, key, .. } => {
			vec![(path.clone(), key.clone())]
		}
		ClausewitzPatch::Rename {
			path,
			old_key,
			new_key,
		} => vec![
			(path.clone(), old_key.clone()),
			(path.clone(), new_key.clone()),
		],
		ClausewitzPatch::AppendBlockItem { .. } | ClausewitzPatch::RemoveBlockItem { .. } => {
			Vec::new()
		}
	}
}

fn extend_merge_result(target: &mut PatchMergeResult, source: PatchMergeResult) {
	target.resolved.extend(source.resolved);
	target.conflicts.extend(source.conflicts);
	target.stats.accumulate(&source.stats);
	target.handler_resolved_count += source.handler_resolved_count;
	target
		.handler_resolutions
		.extend(source.handler_resolutions);
	target
		.external_file_resolutions
		.extend(source.external_file_resolutions);
	target
		.keep_existing_paths
		.extend(source.keep_existing_paths);
}

fn template_for<'a>(
	file_dag: &FileDag,
	vanilla: Option<&'a ParsedScriptFile>,
	contributors: &'a HashMap<ModId, ParsedScriptFile>,
) -> Option<&'a ParsedScriptFile> {
	vanilla.or_else(|| {
		file_dag
			.contributors()
			.iter()
			.find_map(|mod_id| contributors.get(mod_id))
	})
}

fn synthesized_parsed_file(
	file_path: &str,
	template: Option<&ParsedScriptFile>,
	statements: Vec<AstStatement>,
) -> ParsedScriptFile {
	let path = PathBuf::from(file_path);
	let mut parsed = template.cloned().unwrap_or_else(|| ParsedScriptFile {
		mod_id: "__foch_running_base__".to_string(),
		path: path.clone(),
		relative_path: path.clone(),
		content_family: None,
		file_kind: CwtType::new("other"),
		module_name: "running_base".to_string(),
		ast: AstFile {
			path: path.clone(),
			statements: Vec::new(),
		},
		source: String::new(),
		parse_issues: Vec::new(),
		parse_cache_hit: false,
	});
	parsed.mod_id = "__foch_running_base__".to_string();
	parsed.path = path.clone();
	parsed.relative_path = path.clone();
	parsed.ast.path = path;
	parsed.ast.statements = statements;
	parsed.source.clear();
	parsed.parse_issues.clear();
	parsed.parse_cache_hit = false;
	parsed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::playlist::PlaylistEntry;
	use foch_core::model::ModCandidate;
	use foch_language::analyzer::content_family::{CwtType, GameProfile, ListMergePolicy};
	use std::path::PathBuf;

	fn mod_with(
		mod_id: &str,
		name: &str,
		deps: Vec<&str>,
		replace_path: Vec<&str>,
	) -> ModCandidate {
		ModCandidate {
			entry: PlaylistEntry {
				steam_id: Some(mod_id.to_string()),
				..PlaylistEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: name.to_string(),
				dependencies: deps.into_iter().map(str::to_string).collect(),
				replace_path: replace_path.into_iter().map(str::to_string).collect(),
				..ModDescriptor::default()
			}),
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn mid(s: &str) -> ModId {
		ModId(s.to_string())
	}

	fn file_contributor(mod_id: &str, precedence: usize) -> ResolvedFileContributor {
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(format!("/mods/{mod_id}")),
			absolute_path: PathBuf::from(format!("/mods/{mod_id}/common/foo.txt")),
			precedence,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: None,
			mod_hash: Some(format!("hash-{mod_id}")),
		}
	}

	fn parsed_file(mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/foo.txt");
		let parsed =
			foch_language::analyzer::parser::parse_clausewitz_content(path.clone(), source);
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("other"),
			module_name: "test".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn parsed_event_file(mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("events/test.txt");
		let parsed =
			foch_language::analyzer::parser::parse_clausewitz_content(path.clone(), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("events"),
			module_name: "events".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn parsed_definition_module_file(mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/institutions/zzz_foch_institutions.txt");
		let parsed =
			foch_language::analyzer::parser::parse_clausewitz_content(path.clone(), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("institutions"),
			module_name: "institutions".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn compute_tree_event_join(
		vanilla_source: Option<&str>,
		left_source: &str,
		right_source: &str,
	) -> Result<DagMergeComputation, String> {
		let mods = vec![
			mod_with("left", "Left", vec![], vec![]),
			mod_with("right", "Right", vec![], vec![]),
		];
		let contributors = vec![file_contributor("left", 0), file_contributor("right", 1)];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"events/test.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = vanilla_source.map(|source| parsed_event_file("__game__", source));
		let inventory = HashMap::from([
			(mid("left"), parsed_event_file("left", left_source)),
			(mid("right"), parsed_event_file("right", right_source)),
		]);
		let descriptor = foch_language::analyzer::eu4_profile::eu4_profile()
			.classify_content_family(Path::new("events/test.txt"))
			.expect("events content family");
		let mut handler = DeferHandler;
		compute_dag_merge_from_parsed_for_evaluation(
			&file_dag,
			vanilla.as_ref(),
			&inventory,
			descriptor.merge_key_source.expect("events merge key"),
			&descriptor.merge_policies,
			&mut handler,
			MergeKernelMode::Structured,
		)
	}

	fn compute_tree_definition_module_join(
		vanilla_source: &str,
		left_source: &str,
		right_source: &str,
	) -> Result<DagMergeComputation, String> {
		let path = "common/institutions/zzz_foch_institutions.txt";
		let mods = vec![
			mod_with("left", "Left", vec![], vec![]),
			mod_with("right", "Right", vec![], vec![]),
		];
		let contributors = vec![file_contributor("left", 0), file_contributor("right", 1)];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			path,
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = parsed_definition_module_file("__game__", vanilla_source);
		let inventory = HashMap::from([
			(
				mid("left"),
				parsed_definition_module_file("left", left_source),
			),
			(
				mid("right"),
				parsed_definition_module_file("right", right_source),
			),
		]);
		let descriptor = foch_language::analyzer::eu4_profile::eu4_profile()
			.classify_content_family(Path::new(path))
			.expect("institutions content family");
		let mut handler = DeferHandler;
		compute_dag_merge_from_parsed_for_evaluation(
			&file_dag,
			Some(&vanilla),
			&inventory,
			descriptor.merge_key_source.expect("institutions merge key"),
			&descriptor.merge_policies,
			&mut handler,
			MergeKernelMode::Structured,
		)
	}

	fn parsed_inventory(entries: &[(&str, &str)]) -> HashMap<ModId, ParsedScriptFile> {
		entries
			.iter()
			.map(|(mod_id, source)| (mid(mod_id), parsed_file(mod_id, source)))
			.collect()
	}

	fn compute(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
	) -> DagMergeComputation {
		compute_with_overrides(mods, contribs, vanilla_source, inventory, ignore, &[])
	}

	fn compute_with_overrides(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
		dep_overrides: &[DepOverride],
	) -> DagMergeComputation {
		compute_with_merge_key(
			mods,
			contribs,
			vanilla_source,
			inventory,
			ignore,
			dep_overrides,
			MergeKeySource::AssignmentKey,
		)
	}

	fn compute_with_merge_key(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
		dep_overrides: &[DepOverride],
		merge_key_source: MergeKeySource,
	) -> DagMergeComputation {
		let mut handler = DeferHandler;
		compute_with_merge_key_and_handler(
			mods,
			contribs,
			vanilla_source,
			inventory,
			ignore,
			dep_overrides,
			merge_key_source,
			&mut handler,
		)
	}

	fn compute_with_policies(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		policies: &MergePolicies,
	) -> DagMergeComputation {
		let (dag, diags) = super::super::dag::build_mod_dag(&mods);
		assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
		let fdag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contribs,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		let mut handler = DeferHandler;
		compute_dag_merge_from_parsed(
			&fdag,
			vanilla.as_ref(),
			&inventory,
			policies.merge_key_source,
			policies,
			&mut handler,
		)
		.expect("compute DAG patches")
	}

	#[allow(clippy::too_many_arguments)]
	fn compute_with_merge_key_and_handler(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
		dep_overrides: &[DepOverride],
		merge_key_source: MergeKeySource,
		handler: &mut dyn ConflictHandler,
	) -> DagMergeComputation {
		let (dag, diags) = super::super::dag::build_mod_dag(&mods);
		assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
		let fdag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contribs,
			&ignore,
			dep_overrides,
		);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		compute_dag_merge_from_parsed(
			&fdag,
			vanilla.as_ref(),
			&inventory,
			merge_key_source,
			&MergePolicies::default(),
			handler,
		)
		.expect("compute DAG patches")
	}

	#[allow(clippy::too_many_arguments)]
	fn compute_with_test_caches(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		mod_hashes: &HashMap<ModId, String>,
		policies: &MergePolicies,
		handler: &mut dyn ConflictHandler,
		diff_cache: &ModDiffCache,
		dag_base_cache: &DagBaseCache,
	) -> DagMergeComputation {
		let (dag, diags) = super::super::dag::build_mod_dag(&mods);
		assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
		let fdag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contribs,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		compute_dag_merge_from_parsed_with_caches(
			DagMergeArgs {
				file_dag: &fdag,
				vanilla: vanilla.as_ref(),
				contributors: &inventory,
				merge_key_source: MergeKeySource::AssignmentKey,
				policies,
				handler,
				mod_hashes: Some(mod_hashes),
				game_version: "cache-regression",
				kernel: MergeKernelMode::Legacy,
				tree_unit: TreeMergeUnit::File,
			},
			PatchCaches {
				diff: Some(diff_cache),
				dag_base: Some(dag_base_cache),
			},
		)
		.expect("compute cached DAG patches")
	}

	fn assert_computation_eq(expected: &DagMergeComputation, actual: &DagMergeComputation) {
		assert_eq!(actual.mod_patches, expected.mod_patches);
		assert_eq!(actual.base_statements, expected.base_statements);
		assert_eq!(actual.merged_statements, expected.merged_statements);
		assert_eq!(actual.merge_result, expected.merge_result);
		assert_eq!(actual.definition_provenance, expected.definition_provenance);
		assert_eq!(
			actual.definition_participants,
			expected.definition_participants
		);
	}

	#[test]
	fn tree_kernel_runs_at_the_two_sink_event_final_join() {
		let base =
			"country_event = { id = demo.1 title = demo.title option = { name = demo.accept } }\n";
		let left = "country_event = { id = demo.1 title = demo.title trigger = { has_country_flag = left } option = { name = demo.accept } }\n";
		let right = "country_event = { id = demo.1 title = demo.title option = { name = demo.accept } option = { name = demo.reject } }\n";

		let result =
			compute_tree_event_join(Some(base), left, right).expect("tree event final join");
		let emitted = crate::emit::emit_clausewitz_statements(&result.merged_statements)
			.expect("emit merged event");

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			emitted,
			"country_event = {\n\
			\tid = demo.1\n\
			\ttitle = demo.title\n\
			\ttrigger = {\n\
			\t\thas_country_flag = left\n\
			\t}\n\
			\toption = {\n\
			\t\tname = demo.accept\n\
			\t}\n\
			\toption = {\n\
			\t\tname = demo.reject\n\
			\t}\n\
			}\n"
		);
	}

	#[test]
	fn tree_kernel_runs_at_the_two_sink_definition_module_final_join() {
		let base = "institution = { can_embrace = { OR = { trade_goods = ivory trade_goods = cloves } } }\n";
		let right =
			"institution = { can_embrace = { OR = { trade_goods = ivory trade_goods = fur } } }\n";

		let result = compute_tree_definition_module_join(base, base, right)
			.expect("tree definition-module final join");
		let emitted = crate::emit::emit_clausewitz_statements(&result.merged_statements)
			.expect("emit merged definition module");

		assert!(result.merge_result.conflicts.is_empty());
		for trade_good in ["ivory", "cloves", "fur"] {
			assert!(
				emitted.contains(&format!("trade_goods = {trade_good}")),
				"{emitted}"
			);
		}
	}

	#[test]
	fn tree_kernel_accepts_three_parent_intermediate_join() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec![], vec![]),
			mod_with("join", "Join", vec!["A", "B", "C"], vec![]),
			mod_with("right", "Right", vec![], vec![]),
		];
		let contributors = vec![
			file_contributor("a", 0),
			file_contributor("b", 1),
			file_contributor("c", 2),
			file_contributor("join", 3),
			file_contributor("right", 4),
		];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"events/test.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let base = parsed_event_file(
			"__game__",
			"country_event = { id = demo.1 title = demo.title }\n",
		);
		let inventory = ["a", "b", "c", "join", "right"]
			.into_iter()
			.map(|mod_id| {
				(
					mid(mod_id),
					parsed_event_file(
						mod_id,
						&format!(
							"country_event = {{ id = demo.1 title = demo.title {mod_id} = yes }}\n"
						),
					),
				)
			})
			.collect::<HashMap<_, _>>();
		let descriptor = foch_language::analyzer::eu4_profile::eu4_profile()
			.classify_content_family(Path::new("events/test.txt"))
			.expect("events content family");
		let mut handler = DeferHandler;

		let result = compute_dag_merge_from_parsed_for_evaluation(
			&file_dag,
			Some(&base),
			&inventory,
			descriptor.merge_key_source.expect("events merge key"),
			&descriptor.merge_policies,
			&mut handler,
			MergeKernelMode::Structured,
		)
		.expect("tree kernel folds all three intermediate revisions");
		let emitted = crate::emit::emit_clausewitz_statements(&result.merged_statements)
			.expect("emit tree result");

		assert!(result.merge_result.conflicts.is_empty());
		assert!(emitted.contains("join = yes"), "{emitted}");
		assert!(emitted.contains("right = yes"), "{emitted}");
	}

	#[test]
	fn tree_handler_can_select_any_of_three_original_sources() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec![], vec![]),
		];
		let contributors = vec![
			file_contributor("a", 0),
			file_contributor("b", 1),
			file_contributor("c", 2),
		];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let base = parsed_file("__game__", "value = 0\n");
		let inventory = parsed_inventory(&[
			("a", "value = 1\n"),
			("b", "value = 2\n"),
			("c", "value = 3\n"),
		]);

		for (winner, expected) in [("a", "1"), ("b", "2"), ("c", "3")] {
			let mut handler = PickModHandler { winner, calls: 0 };
			let result = compute_dag_merge_from_parsed_for_evaluation(
				&file_dag,
				Some(&base),
				&inventory,
				MergeKeySource::AssignmentKey,
				&MergePolicies::default(),
				&mut handler,
				MergeKernelMode::Structured,
			)
			.expect("resolve tree conflict by original source");

			assert_eq!(handler.calls, 1);
			assert!(result.merge_result.conflicts.is_empty());
			assert_eq!(
				root_scalar_values(&result.merged_statements, "value"),
				vec![expected]
			);
		}
	}

	#[test]
	fn unresolved_tree_conflict_reaches_manual_resolution_with_all_sources() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec![], vec![]),
		];
		let contributors = vec![
			file_contributor("a", 0),
			file_contributor("b", 1),
			file_contributor("c", 2),
		];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let base = parsed_file("__game__", "value = 0\n");
		let inventory = parsed_inventory(&[
			("a", "value = 1\n"),
			("b", "value = 2\n"),
			("c", "value = 3\n"),
		]);
		let mut handler = DeferHandler;

		let result = compute_dag_merge_from_parsed_for_evaluation(
			&file_dag,
			Some(&base),
			&inventory,
			MergeKeySource::AssignmentKey,
			&MergePolicies::default(),
			&mut handler,
			MergeKernelMode::Structured,
		)
		.expect("retain unresolved tree conflict");

		let [
			PatchResolution::Conflict {
				address, patches, ..
			},
		] = result.merge_result.conflicts.as_slice()
		else {
			panic!(
				"expected one tree conflict, got {:?}",
				result.merge_result.conflicts
			);
		};
		assert_eq!(address.path, Vec::<String>::new());
		assert_eq!(address.key, "value");
		assert_eq!(
			patches
				.iter()
				.map(|patch| patch.mod_id.as_str())
				.collect::<Vec<_>>(),
			vec!["a", "b", "c"]
		);
	}

	#[test]
	fn tree_kernel_rejects_an_implicit_empty_base() {
		let source = "country_event = { id = demo.1 title = demo.title }\n";

		let error =
			compute_tree_event_join(None, source, source).expect_err("tree merge requires vanilla");

		assert!(error.contains("non-empty vanilla base"), "{error}");
	}

	#[test]
	fn cache_identity_hashes_use_full_blake3_digest() {
		let base = parsed_file("base", "root = yes\n");
		let overlay = parsed_file("overlay", "root = yes\nextra = yes\n");
		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		let parent_hash = hash_ast_statements(&base.ast.statements).expect("parent-view hash");
		let apply_hash =
			hash_dag_apply_input(&base.ast.statements, &patches).expect("DAG-application hash");

		assert_eq!(parent_hash.len(), blake3::OUT_LEN * 2);
		assert_eq!(apply_hash.len(), blake3::OUT_LEN * 2);
	}

	#[test]
	fn extend_merge_result_aggregates_edit_over_remove_stats() {
		let mut target = PatchMergeResult::default();
		target.stats.edit_over_remove_resolved = 2;
		let mut source = PatchMergeResult::default();
		source.stats.edit_over_remove_resolved = 3;

		extend_merge_result(&mut target, source);

		assert_eq!(target.stats.edit_over_remove_resolved, 5);
	}

	fn patches_for<'a>(result: &'a DagMergeComputation, mod_id: &str) -> &'a Vec<ClausewitzPatch> {
		&result
			.mod_patches
			.iter()
			.find(|(id, _, _)| id == mod_id)
			.unwrap_or_else(|| panic!("missing patches for {mod_id}"))
			.2
	}

	fn inserted_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::InsertNode { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn removed_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::RemoveNode { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn set_value_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::SetValue { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn base_keys(result: &DagMergeComputation) -> Vec<String> {
		let mut keys: Vec<_> = result
			.base_statements
			.iter()
			.filter_map(|stmt| match stmt {
				AstStatement::Assignment { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn rendered(statements: &[AstStatement]) -> String {
		crate::emit::emit_clausewitz_statements(statements).expect("emit statements")
	}

	fn root_scalar_values(statements: &[AstStatement], expected_key: &str) -> Vec<String> {
		statements
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment {
					key,
					value: AstValue::Scalar { value, .. },
					..
				} if key == expected_key => Some(value.as_text()),
				_ => None,
			})
			.collect()
	}

	struct PickModHandler {
		winner: &'static str,
		calls: usize,
	}

	impl ConflictHandler for PickModHandler {
		fn on_conflict(
			&mut self,
			_: &crate::merge::conflict_view::ConflictView,
		) -> crate::merge::conflict_handler::ConflictDecision {
			self.calls += 1;
			crate::merge::conflict_handler::ConflictDecision::PickMod {
				mod_id: self.winner.to_string(),
				record: None,
			}
		}
	}

	fn append_list_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::AppendListItem { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn remove_list_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::RemoveListItem { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn replace_block_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::ReplaceBlock { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	#[test]
	fn single_mod_no_deps_preserves_direct_output() {
		let mod_source = "root = yes\nnew_block = {\n\tvalue = 1\n}\n";
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some("root = no\n"),
			parsed_inventory(&[("a", mod_source)]),
			IgnoreReplacePath::None,
		);
		let direct = parsed_file("a", mod_source);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			rendered(&result.merged_statements),
			rendered(&direct.ast.statements)
		);
	}

	#[test]
	fn dependency_chain_applies_parent_before_child_without_mixed_kind_conflict() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("tag = ROOT\n"),
			parsed_inventory(&[
				("a", "tag = ROOT\ntag = AAA\n"),
				("b", "tag = ROOT\ntag = AAA\ntag = BBB\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(append_list_keys(patches_for(&result, "a")), vec!["tag"]);
		assert_eq!(append_list_keys(patches_for(&result, "b")), vec!["tag"]);
		assert!(remove_list_keys(patches_for(&result, "b")).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("tag = AAA"));
		assert!(output.contains("tag = BBB"));
	}

	#[test]
	fn sibling_fork_merges_same_base_patches_in_one_level() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("flag = no\n"),
			parsed_inventory(&[("a", "flag = yes\n"), ("b", "flag = yes\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(set_value_keys(patches_for(&result, "a")), vec!["flag"]);
		assert_eq!(set_value_keys(patches_for(&result, "b")), vec!["flag"]);
		assert_eq!(result.merge_result.stats.convergent_patches, 1);
		assert!(rendered(&result.merged_statements).contains("flag = yes"));
	}

	#[test]
	fn three_level_translation_chain_replaces_inherited_block_without_remove_append() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\npirate = {\n\tname = \"Pirates\"\n}\n"),
				(
					"b",
					"root = yes\npirate = {\n\tname = \"海盗\"\n\tflag = yes\n}\n",
				),
				(
					"c",
					"root = yes\npirate = {\n\tname = \"海盗\"\n\tflag = yes\n}\nc = yes\n",
				),
			]),
			IgnoreReplacePath::None,
		);

		let b_patches = patches_for(&result, "b");
		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(replace_block_keys(b_patches), vec!["pirate"]);
		assert!(append_list_keys(b_patches).is_empty());
		assert!(remove_list_keys(b_patches).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("name = \"海盗\""));
		assert!(output.contains("c = yes"));
	}

	#[test]
	fn independent_mods_diff_against_vanilla_not_previous_mod() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("flag = no\n"),
			parsed_inventory(&[("a", "flag = yes\n"), ("b", "flag = no\n")]),
			IgnoreReplacePath::None,
		);

		assert_eq!(set_value_keys(patches_for(&result, "a")), vec!["flag"]);
		assert!(
			patches_for(&result, "b").is_empty(),
			"independent vanilla-equivalent mod must not remove mod A's changes"
		);
	}

	#[test]
	fn child_delta_against_parent_preserves_independent_root() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\nb = yes\n"),
				("c", "root = yes\na = yes\nc = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(inserted_keys(patches_for(&result, "c")), vec!["c"]);
		assert!(removed_keys(patches_for(&result, "c")).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("a = yes"), "{output}");
		assert!(output.contains("b = yes"), "{output}");
		assert!(output.contains("c = yes"), "{output}");
	}

	#[test]
	fn child_deletion_relative_to_parent_preserves_independent_root() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\nremoved_by_c = yes\n"),
				("b", "root = yes\nb = yes\n"),
				("c", "root = yes\na = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			removed_keys(patches_for(&result, "c")),
			vec!["removed_by_c"]
		);
		let output = rendered(&result.merged_statements);
		assert!(output.contains("b = yes"), "{output}");
		assert!(!output.contains("removed_by_c"), "{output}");
	}

	#[test]
	fn unrelated_deeper_branch_conflicts_with_independent_root() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("flag = no\n"),
			parsed_inventory(&[
				("a", "flag = yes\n"),
				("b", "flag = maybe\n"),
				("c", "flag = forced\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(result.merge_result.conflicts.len(), 1);
		let PatchResolution::Conflict { patches, .. } = &result.merge_result.conflicts[0] else {
			panic!("expected cross-branch conflict");
		};
		let mods = patches
			.iter()
			.map(|patch| patch.mod_id.as_str())
			.collect::<BTreeSet<_>>();
		assert_eq!(mods, BTreeSet::from(["b", "c"]));
		assert!(
			!result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|record| record.action == "downstream_override")
		);
	}

	#[test]
	fn child_restores_vanilla_after_caller_resolves_parent_conflict() {
		let mut handler = PickModHandler {
			winner: "a",
			calls: 0,
		};
		let result = compute_with_merge_key_and_handler(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("flag = no\n"),
			parsed_inventory(&[
				("a", "flag = yes\n"),
				("b", "flag = maybe\n"),
				("c", "flag = no\n"),
			]),
			IgnoreReplacePath::None,
			&[],
			MergeKeySource::AssignmentKey,
			&mut handler,
		);

		assert_eq!(handler.calls, 1);
		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(set_value_keys(patches_for(&result, "c")), vec!["flag"]);
		assert_eq!(rendered(&result.merged_statements), "flag = no\n");
		assert_eq!(prov(&result, "flag"), vec!["c".to_string()]);
	}

	#[test]
	fn multi_parent_join_merges_only_frontiers_after_shared_ancestor() {
		let joined = "flag = forced\nright = yes\njoined = yes\n";
		let mut handler = PickModHandler {
			winner: "b",
			calls: 0,
		};
		let result = compute_with_merge_key_and_handler(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
				mod_with("d", "D", vec!["B", "C"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
				file_contributor("d", 4),
			],
			Some("flag = no\n"),
			parsed_inventory(&[
				("a", "flag = yes\n"),
				("b", "flag = forced\n"),
				("c", "flag = yes\nright = yes\n"),
				("d", joined),
			]),
			IgnoreReplacePath::None,
			&[],
			MergeKeySource::AssignmentKey,
			&mut handler,
		);

		assert_eq!(handler.calls, 0, "shared ancestry is not a sibling edit");
		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(inserted_keys(patches_for(&result, "d")), vec!["joined"]);
		assert_eq!(rendered(&result.merged_statements), joined);
	}

	#[test]
	fn cached_and_cold_match_when_handler_resolution_changes_parent_view() {
		let temp = tempfile::TempDir::new().expect("temp cache root");
		let diff_cache = ModDiffCache::open(&temp.path().join("diff"));
		let dag_base_cache = DagBaseCache::open(&temp.path().join("dag"));
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec!["A", "B"], vec![]),
		];
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
		];
		let inventory = parsed_inventory(&[
			("a", "flag = yes\n"),
			("b", "flag = maybe\n"),
			("c", "flag = no\n"),
		]);
		let mod_hashes = HashMap::from([
			(mid("a"), "hash-a".to_string()),
			(mid("b"), "hash-b".to_string()),
			(mid("c"), "hash-c".to_string()),
		]);

		let run_cold = |winner| {
			let mut handler = PickModHandler { winner, calls: 0 };
			compute_with_merge_key_and_handler(
				mods.clone(),
				contribs.clone(),
				Some("flag = no\n"),
				inventory.clone(),
				IgnoreReplacePath::None,
				&[],
				MergeKeySource::AssignmentKey,
				&mut handler,
			)
		};
		let run_cached = |winner| {
			let mut handler = PickModHandler { winner, calls: 0 };
			compute_with_test_caches(
				mods.clone(),
				contribs.clone(),
				Some("flag = no\n"),
				inventory.clone(),
				&mod_hashes,
				&MergePolicies::default(),
				&mut handler,
				&diff_cache,
				&dag_base_cache,
			)
		};

		let cold_a = run_cold("a");
		let cache_miss_a = run_cached("a");
		reset_dag_apply_cache_events();
		let cache_hit_a = run_cached("a");
		let branch_events = dag_apply_cache_events()
			.into_iter()
			.filter(|event| event.scope == DagApplyCacheScope::ResolvedBranchState)
			.collect::<Vec<_>>();
		assert_computation_eq(&cold_a, &cache_miss_a);
		assert_computation_eq(&cold_a, &cache_hit_a);
		assert_eq!(
			branch_events,
			vec![DagApplyCacheEvent {
				scope: DagApplyCacheScope::ResolvedBranchState,
				hit: true,
			}],
			"the repeated run must hit the specific multi-parent resolved branch state"
		);

		let cold_b = run_cold("b");
		let cached_b = run_cached("b");
		assert_computation_eq(&cold_b, &cached_b);
		assert_eq!(rendered(&cached_b.merged_statements), "flag = no\n");
		assert_ne!(patches_for(&cache_hit_a, "c"), patches_for(&cached_b, "c"));
	}

	#[test]
	fn resolved_branch_cache_invalidates_when_merge_policy_changes() {
		use foch_language::analyzer::content_family::ScalarMergePolicy;

		let temp = tempfile::TempDir::new().expect("temp cache root");
		let diff_cache = ModDiffCache::open(&temp.path().join("diff"));
		let dag_base_cache = DagBaseCache::open(&temp.path().join("dag"));
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
		];
		let contribs = vec![file_contributor("a", 1), file_contributor("b", 2)];
		let inventory = parsed_inventory(&[
			("a", "root = yes\na = yes\n"),
			("b", "root = yes\nb = yes\n"),
		]);
		let mod_hashes = HashMap::from([
			(mid("a"), "hash-a".to_string()),
			(mid("b"), "hash-b".to_string()),
		]);
		let last_writer = MergePolicies {
			scalar: ScalarMergePolicy::LastWriter,
			..MergePolicies::default()
		};
		let run = |policies: &MergePolicies| {
			let mut handler = DeferHandler;
			compute_with_test_caches(
				mods.clone(),
				contribs.clone(),
				Some("root = yes\n"),
				inventory.clone(),
				&mod_hashes,
				policies,
				&mut handler,
				&diff_cache,
				&dag_base_cache,
			)
		};

		run(&MergePolicies::default());
		reset_dag_apply_cache_events();
		let policy_miss = run(&last_writer);
		let miss_events = dag_apply_cache_events()
			.into_iter()
			.filter(|event| event.scope == DagApplyCacheScope::ResolvedBranchState)
			.collect::<Vec<_>>();
		assert_eq!(miss_events.len(), 1);
		assert!(
			!miss_events[0].hit,
			"changed policy must invalidate branch state"
		);

		reset_dag_apply_cache_events();
		let policy_hit = run(&last_writer);
		let hit_events = dag_apply_cache_events()
			.into_iter()
			.filter(|event| event.scope == DagApplyCacheScope::ResolvedBranchState)
			.collect::<Vec<_>>();
		assert_eq!(
			hit_events,
			vec![DagApplyCacheEvent {
				scope: DagApplyCacheScope::ResolvedBranchState,
				hit: true,
			}]
		);
		assert_computation_eq(&policy_miss, &policy_hit);
	}

	#[test]
	fn cached_child_delta_invalidates_when_only_parent_view_changes() {
		let temp = tempfile::TempDir::new().expect("temp cache root");
		let diff_cache = ModDiffCache::open(&temp.path().join("diff"));
		let dag_base_cache = DagBaseCache::open(&temp.path().join("dag"));
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("c", "C", vec!["A"], vec![]),
		];
		let contribs = vec![file_contributor("a", 1), file_contributor("c", 2)];
		let hashes_v1 = HashMap::from([
			(mid("a"), "hash-a-v1".to_string()),
			(mid("c"), "hash-c-stable".to_string()),
		]);
		let hashes_v2 = HashMap::from([
			(mid("a"), "hash-a-v2".to_string()),
			(mid("c"), "hash-c-stable".to_string()),
		]);
		let mut warm_handler = DeferHandler;
		let warm = compute_with_test_caches(
			mods.clone(),
			contribs.clone(),
			Some("flag = no\n"),
			parsed_inventory(&[("a", "flag = yes\n"), ("c", "flag = no\n")]),
			&hashes_v1,
			&MergePolicies::default(),
			&mut warm_handler,
			&diff_cache,
			&dag_base_cache,
		);
		assert_eq!(set_value_keys(patches_for(&warm, "c")), vec!["flag"]);

		let inventory_v2 = parsed_inventory(&[("a", "flag = no\n"), ("c", "flag = no\n")]);
		let mut cold_handler = DeferHandler;
		let cold = compute_with_merge_key_and_handler(
			mods.clone(),
			contribs.clone(),
			Some("flag = no\n"),
			inventory_v2.clone(),
			IgnoreReplacePath::None,
			&[],
			MergeKeySource::AssignmentKey,
			&mut cold_handler,
		);
		let mut cached_handler = DeferHandler;
		let cached = compute_with_test_caches(
			mods,
			contribs,
			Some("flag = no\n"),
			inventory_v2,
			&hashes_v2,
			&MergePolicies::default(),
			&mut cached_handler,
			&diff_cache,
			&dag_base_cache,
		);

		assert_computation_eq(&cold, &cached);
		assert!(patches_for(&cached, "c").is_empty());
		assert!(prov(&cached, "flag").is_empty());
	}

	#[test]
	fn gui_named_children_let_sibling_mods_edit_different_widgets() {
		const GUI_CHILD_TYPES: &[&str] = &["windowType"];
		let key_source = MergeKeySource::ContainerChildFieldValue {
			containers: &["guiTypes"],
			child_key_field: "name",
			child_types: GUI_CHILD_TYPES,
		};
		let vanilla = r#"
			guiTypes = {
				windowType = { name = "left_widget" position = { x = 0 y = 0 } }
				windowType = { name = "right_widget" position = { x = 0 y = 0 } }
			}
		"#;
		let result = compute_with_merge_key(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(vanilla),
			parsed_inventory(&[
				(
					"a",
					r#"guiTypes = {
						windowType = { name = "left_widget" position = { x = 1 y = 0 } }
						windowType = { name = "right_widget" position = { x = 0 y = 0 } }
					}"#,
				),
				(
					"b",
					r#"guiTypes = {
						windowType = { name = "left_widget" position = { x = 0 y = 0 } }
						windowType = { name = "right_widget" position = { x = 2 y = 0 } }
					}"#,
				),
			]),
			IgnoreReplacePath::None,
			&[],
			key_source,
		);

		assert!(result.merge_result.conflicts.is_empty());
		let addresses = result
			.mod_patches
			.iter()
			.flat_map(|(_, _, patches)| patches)
			.filter_map(|patch| match patch {
				ClausewitzPatch::SetValue { path, key, .. } => Some((path.clone(), key.clone())),
				_ => None,
			})
			.collect::<Vec<_>>();
		assert!(addresses.contains(&(
			vec![
				"guiTypes".to_string(),
				"windowType:left_widget".to_string(),
				"position".to_string(),
			],
			"x".to_string(),
		)));
		assert!(addresses.contains(&(
			vec![
				"guiTypes".to_string(),
				"windowType:right_widget".to_string(),
				"position".to_string(),
			],
			"x".to_string(),
		)));
	}

	#[test]
	fn gui_named_children_conflict_same_widget_sibling_overwrites() {
		const GUI_CHILD_TYPES: &[&str] = &["windowType"];
		let key_source = MergeKeySource::ContainerChildFieldValue {
			containers: &["guiTypes"],
			child_key_field: "name",
			child_types: GUI_CHILD_TYPES,
		};
		let vanilla = r#"
			guiTypes = {
				windowType = { name = "left_widget" position = { x = 0 y = 0 } }
			}
		"#;
		let result = compute_with_merge_key(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(vanilla),
			parsed_inventory(&[
				(
					"a",
					r#"guiTypes = {
						windowType = { name = "left_widget" position = { x = 1 y = 0 } }
					}"#,
				),
				(
					"b",
					r#"guiTypes = {
						windowType = { name = "left_widget" position = { x = 2 y = 0 } }
					}"#,
				),
			]),
			IgnoreReplacePath::None,
			&[],
			key_source,
		);

		assert_eq!(result.merge_result.conflicts.len(), 1);
		match &result.merge_result.conflicts[0] {
			PatchResolution::Conflict {
				address, reason, ..
			} => {
				assert_eq!(
					address.path,
					vec![
						"guiTypes".to_string(),
						"windowType:left_widget".to_string(),
						"position".to_string(),
					]
				);
				assert_eq!(address.key, "x");
				assert!(
					reason.contains("sibling mods set the same scalar to divergent values"),
					"unexpected reason: {reason}"
				);
			}
			other => panic!("expected sibling scalar conflict, got {other:?}"),
		}
	}

	#[test]
	fn declared_dep_uses_synthesized_parent_base() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\na = yes\nb = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "a")), vec!["a"]);
		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
	}

	#[test]
	fn dep_override_diffs_child_against_vanilla_not_declared_parent() {
		let result = compute_with_overrides(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\nb = yes\n"),
			]),
			IgnoreReplacePath::None,
			&[DepOverride::new("b", "a")],
		);

		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
		assert!(removed_keys(patches_for(&result, "b")).is_empty());
	}

	#[test]
	fn transitive_chain_base_contains_all_ancestors() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\na = yes\nb = yes\n"),
				("c", "root = yes\na = yes\nb = yes\nc = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "c")), vec!["c"]);
	}

	#[test]
	fn dependency_chain_reads_only_incremental_parent_states() {
		const NODE_COUNT: usize = 16;
		let ids = (0..NODE_COUNT)
			.map(|index| format!("m{index}"))
			.collect::<Vec<_>>();
		let names = (0..NODE_COUNT)
			.map(|index| format!("M{index}"))
			.collect::<Vec<_>>();
		let mods = (0..NODE_COUNT)
			.map(|index| {
				let deps = if index == 0 {
					Vec::new()
				} else {
					vec![names[index - 1].as_str()]
				};
				mod_with(&ids[index], &names[index], deps, vec![])
			})
			.collect::<Vec<_>>();
		let contributors = ids
			.iter()
			.enumerate()
			.map(|(index, mod_id)| file_contributor(mod_id, index + 1))
			.collect::<Vec<_>>();
		let mut source = "root = yes\n".to_string();
		let mut inventory = HashMap::new();
		for (index, mod_id) in ids.iter().enumerate() {
			source.push_str(&format!("key_{index} = yes\n"));
			inventory.insert(mid(mod_id), parsed_file(mod_id, &source));
		}

		let result = compute(
			mods,
			contributors,
			Some("root = yes\n"),
			inventory,
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert!(rendered(&result.merged_statements).contains("key_15 = yes"));
		for (index, mod_id) in ids.iter().enumerate() {
			assert_eq!(
				inserted_keys(patches_for(&result, mod_id)),
				vec![format!("key_{index}")]
			);
		}
	}

	#[test]
	fn shared_chain_join_keeps_ancestry_storage_and_work_linear() {
		const NODE_COUNT: usize = 64;
		let ids = (0..NODE_COUNT)
			.map(|index| format!("chain_{index}"))
			.collect::<Vec<_>>();
		let names = (0..NODE_COUNT)
			.map(|index| format!("Chain {index}"))
			.collect::<Vec<_>>();
		let mut mods = (0..NODE_COUNT)
			.map(|index| {
				let deps = if index == 0 {
					Vec::new()
				} else {
					vec![names[index - 1].as_str()]
				};
				mod_with(&ids[index], &names[index], deps, vec![])
			})
			.collect::<Vec<_>>();
		mods.push(mod_with(
			"left",
			"Left",
			vec![names[NODE_COUNT - 1].as_str()],
			vec![],
		));
		mods.push(mod_with(
			"right",
			"Right",
			vec![names[NODE_COUNT - 1].as_str()],
			vec![],
		));
		mods.push(mod_with("join", "Join", vec!["Left", "Right"], vec![]));

		let mut contributors = ids
			.iter()
			.enumerate()
			.map(|(index, mod_id)| file_contributor(mod_id, index + 1))
			.collect::<Vec<_>>();
		contributors.push(file_contributor("left", NODE_COUNT + 1));
		contributors.push(file_contributor("right", NODE_COUNT + 2));
		contributors.push(file_contributor("join", NODE_COUNT + 3));

		let mut source = "root = yes\n".to_string();
		let mut inventory = HashMap::new();
		for (index, mod_id) in ids.iter().enumerate() {
			source.push_str(&format!("chain_value_{index} = yes\n"));
			inventory.insert(mid(mod_id), parsed_file(mod_id, &source));
		}
		let left_source = format!("{source}left = yes\n");
		let right_source = format!("{source}right = yes\n");
		let join_source = format!("{source}left = yes\nright = yes\njoin = yes\n");
		inventory.insert(mid("left"), parsed_file("left", &left_source));
		inventory.insert(mid("right"), parsed_file("right", &right_source));
		inventory.insert(mid("join"), parsed_file("join", &join_source));

		reset_ancestry_metrics();
		let result = compute(
			mods,
			contributors,
			Some("root = yes\n"),
			inventory,
			IgnoreReplacePath::None,
		);
		let metrics = ancestry_metrics();

		assert!(result.merge_result.conflicts.is_empty());
		assert!(rendered(&result.merged_statements).contains("join = yes"));
		assert!(
			metrics.work_units <= NODE_COUNT * 5,
			"shared-frontier search visited {} nodes for {NODE_COUNT} shared ancestors",
			metrics.work_units
		);
		assert!(
			metrics.peak_transient_nodes <= NODE_COUNT * 3,
			"shared-frontier search retained {} transient nodes for {NODE_COUNT} shared ancestors",
			metrics.peak_transient_nodes
		);
	}

	#[test]
	fn high_fan_in_join_measures_word_linear_common_frontier_work() {
		const DEPTH: usize = 32;
		const FAN_IN: usize = 129;
		let chain_ids = (0..DEPTH)
			.map(|index| format!("chain_{index}"))
			.collect::<Vec<_>>();
		let chain_names = (0..DEPTH)
			.map(|index| format!("Chain {index}"))
			.collect::<Vec<_>>();
		let branch_ids = (0..FAN_IN)
			.map(|index| format!("branch_{index}"))
			.collect::<Vec<_>>();
		let branch_names = (0..FAN_IN)
			.map(|index| format!("Branch {index}"))
			.collect::<Vec<_>>();

		let mut mods = (0..DEPTH)
			.map(|index| {
				let deps = if index == 0 {
					Vec::new()
				} else {
					vec![chain_names[index - 1].as_str()]
				};
				mod_with(&chain_ids[index], &chain_names[index], deps, vec![])
			})
			.collect::<Vec<_>>();
		for (branch_id, branch_name) in branch_ids.iter().zip(&branch_names) {
			mods.push(mod_with(
				branch_id,
				branch_name,
				vec![chain_names[DEPTH - 1].as_str()],
				vec![],
			));
		}
		mods.push(mod_with(
			"join",
			"Join",
			branch_names.iter().map(String::as_str).collect(),
			vec![],
		));

		let mut contributors = chain_ids
			.iter()
			.chain(&branch_ids)
			.enumerate()
			.map(|(index, mod_id)| file_contributor(mod_id, index + 1))
			.collect::<Vec<_>>();
		contributors.push(file_contributor("join", DEPTH + FAN_IN + 1));

		let mut source = "root = yes\n".to_string();
		let mut inventory = HashMap::new();
		for (index, mod_id) in chain_ids.iter().enumerate() {
			source.push_str(&format!("chain_value_{index} = yes\n"));
			inventory.insert(mid(mod_id), parsed_file(mod_id, &source));
		}
		let mut join_source = source.clone();
		for (index, mod_id) in branch_ids.iter().enumerate() {
			let branch_line = format!("branch_value_{index} = yes\n");
			inventory.insert(
				mid(mod_id),
				parsed_file(mod_id, &format!("{source}{branch_line}")),
			);
			join_source.push_str(&branch_line);
		}
		join_source.push_str("join = yes\n");
		inventory.insert(mid("join"), parsed_file("join", &join_source));

		let run = || {
			compute(
				mods.clone(),
				contributors.clone(),
				Some("root = yes\n"),
				inventory.clone(),
				IgnoreReplacePath::None,
			)
		};
		reset_ancestry_metrics();
		let result = run();
		let metrics = ancestry_metrics();
		reset_ancestry_metrics();
		let repeated = run();
		let repeated_metrics = ancestry_metrics();
		let graph_nodes = DEPTH + FAN_IN;
		let graph_edges = DEPTH - 1 + FAN_IN;
		let coverage_words = FAN_IN.div_ceil(u64::BITS as usize);
		let expected_word_unions = graph_edges * coverage_words;
		let expected_work = graph_nodes + expected_word_unions + DEPTH + (DEPTH - 1);

		assert!(result.merge_result.conflicts.is_empty());
		assert!(rendered(&result.merged_statements).contains("join = yes"));
		assert_eq!(
			rendered(&result.merged_statements),
			rendered(&repeated.merged_statements),
			"high-fan-in merge output changed across identical runs"
		);
		assert_eq!(metrics.work_units, expected_work);
		assert_eq!(metrics.coverage_word_unions, expected_word_unions);
		assert_eq!(metrics.work_units, repeated_metrics.work_units);
		assert_eq!(
			metrics.coverage_word_unions,
			repeated_metrics.coverage_word_unions
		);
		assert!(
			metrics.peak_transient_nodes <= graph_nodes * (2 + coverage_words),
			"high-fan-in frontier retained {} transient units for {graph_nodes} nodes and {coverage_words} coverage words",
			metrics.peak_transient_nodes
		);
		for index in 0..FAN_IN {
			assert!(
				rendered(&result.merged_statements)
					.contains(&format!("branch_value_{index} = yes")),
				"missing branch {index} from high-fan-in output"
			);
		}
	}

	#[test]
	fn diamond_base_merges_both_branches() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
				mod_with("d", "D", vec!["B", "C"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
				file_contributor("d", 4),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\na = yes\nb = yes\n"),
				("c", "root = yes\na = yes\nc = yes\n"),
				("d", "root = yes\na = yes\nb = yes\nc = yes\nd = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "d")), vec!["d"]);
		assert!(removed_keys(patches_for(&result, "d")).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("b = yes"), "{output}");
		assert!(output.contains("c = yes"), "{output}");
		assert!(output.contains("d = yes"), "{output}");
	}

	#[test]
	fn missing_intermediate_file_dep_lifts_to_shipping_ancestor() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("c", 3)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("c", "root = yes\na = yes\nc = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "c")), vec!["c"]);
	}

	#[test]
	fn replace_path_drops_prior_contributors_and_uses_empty_base() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec!["common"]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[("b", "b = yes\n"), ("c", "b = yes\nc = yes\n")]),
			IgnoreReplacePath::None,
		);

		assert!(
			result
				.mod_patches
				.iter()
				.all(|(mod_id, _, _)| mod_id != "a")
		);
		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
		assert_eq!(inserted_keys(patches_for(&result, "c")), vec!["c"]);
		assert!(base_keys(&result).is_empty());
	}

	#[test]
	fn ignore_replace_path_keeps_prior_contributors() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec!["common"]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\na = yes\nb = yes\n"),
				("c", "root = yes\na = yes\nb = yes\nc = yes\n"),
			]),
			IgnoreReplacePath::All,
		);

		assert_eq!(result.mod_patches.len(), 3);
		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
		assert_eq!(base_keys(&result), vec!["root"]);
	}

	#[test]
	fn no_vanilla_file_diffs_each_mod_against_empty() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			None,
			parsed_inventory(&[("a", "a = yes\n"), ("b", "b = yes\n")]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "a")), vec!["a"]);
		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
		assert!(base_keys(&result).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("a = yes"), "{output}");
		assert!(output.contains("b = yes"), "{output}");
	}

	#[test]
	fn resolved_patch_application_is_deterministic_across_repeated_runs() {
		let mut outputs = BTreeSet::new();
		for _ in 0..64 {
			let result = compute(
				vec![
					mod_with("a", "A", vec![], vec![]),
					mod_with("b", "B", vec![], vec![]),
					mod_with("c", "C", vec![], vec![]),
				],
				vec![
					file_contributor("a", 1),
					file_contributor("b", 2),
					file_contributor("c", 3),
				],
				Some("root = yes\n"),
				parsed_inventory(&[
					("a", "root = yes\nz1 = yes\nz2 = yes\nz3 = yes\n"),
					("b", "root = yes\nb1 = yes\nb2 = yes\nb3 = yes\n"),
					("c", "root = yes\nm1 = yes\nm2 = yes\nm3 = yes\n"),
				]),
				IgnoreReplacePath::None,
			);
			assert!(result.merge_result.conflicts.is_empty());
			outputs.insert(rendered(&result.merged_statements));
		}

		assert_eq!(
			outputs.len(),
			1,
			"identical DAG inputs produced divergent output orders: {outputs:#?}"
		);
		assert_eq!(
			outputs.first().expect("one deterministic output"),
			"root = yes\nz1 = yes\nz2 = yes\nz3 = yes\nb1 = yes\nb2 = yes\nb3 = yes\nm1 = yes\nm2 = yes\nm3 = yes\n",
			"resolved inserts are ordered by contributor precedence, then address"
		);
	}

	#[test]
	fn resolved_patch_application_preserves_contributor_local_source_order() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\nz_first = yes\na_second = yes\n"),
				("b", "root = yes\nmiddle_other_mod = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(
			rendered(&result.merged_statements),
			"root = yes\nz_first = yes\na_second = yes\nmiddle_other_mod = yes\n"
		);
	}

	#[test]
	fn downstream_remove_collapses_sibling_set_value_conflict() {
		// a and b are independent siblings that disagree on `flag`; c declares
		// dependencies on both and removes `flag` outright. The upstream
		// disagreement is moot — the post-pass must collapse it.
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("flag = no\n"),
			parsed_inventory(&[("a", "flag = yes\n"), ("b", "flag = maybe\n"), ("c", "")]),
			IgnoreReplacePath::None,
		);

		assert!(
			result.merge_result.conflicts.is_empty(),
			"downstream RemoveNode should collapse sibling SetValue/SetValue conflict, got {:?}",
			result.merge_result.conflicts
		);
		assert!(
			result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|r| r.action == "downstream_override"),
			"expected a downstream_override handler resolution"
		);
	}

	#[test]
	fn downstream_insert_collapses_sibling_remove_set_conflict() {
		// a removes `flag`, b sets `flag`; c declares deps on both and re-inserts
		// the key fresh. The upstream RemoveNode/SetValue disagreement is moot.
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("flag = no\n"),
			parsed_inventory(&[("a", ""), ("b", "flag = yes\n"), ("c", "flag = forced\n")]),
			IgnoreReplacePath::None,
		);

		assert!(
			result.merge_result.conflicts.is_empty(),
			"downstream re-insert should collapse sibling RemoveNode/SetValue conflict, got {:?}",
			result.merge_result.conflicts
		);
		assert!(
			result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|r| r.action == "downstream_override"),
			"expected a downstream_override handler resolution"
		);
	}

	#[test]
	fn descendant_append_list_item_collapses_parent_key_conflict() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("tag = ROOT\n"),
			parsed_inventory(&[
				("a", "tag = A\n"),
				("b", ""),
				("c", "tag = ROOT\ntag = CHILD\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(
			result.merge_result.conflicts.is_empty(),
			"child list intent must settle the parent key conflict: {:?}",
			result.merge_result.conflicts
		);
		assert_eq!(append_list_keys(patches_for(&result, "c")), vec!["tag"]);
		assert!(
			result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|record| record.action == "downstream_override")
		);
	}

	#[test]
	fn append_and_remove_list_deltas_overwrite_pending_logical_address() {
		let pending = vec![PatchResolution::Conflict {
			address: crate::merge::patch_merge::PatchAddress {
				path: Vec::new(),
				key: "tag".to_string(),
			},
			patches: Vec::new(),
			reason: "parent disagreement".to_string(),
		}];
		let base = parsed_file("base", "tag = KEEP\ntag = REMOVE\n");
		let appended = parsed_file("append", "tag = KEEP\ntag = REMOVE\ntag = APPENDED\n");
		let removed = parsed_file("remove", "tag = KEEP\n");
		let append_delta = diff_ast(&base, &appended, MergeKeySource::AssignmentKey);
		let remove_delta = diff_ast(&base, &removed, MergeKeySource::AssignmentKey);

		assert!(matches!(
			append_delta.as_slice(),
			[ClausewitzPatch::AppendListItem { key, .. }] if key == "tag"
		));
		assert!(matches!(
			remove_delta.as_slice(),
			[ClausewitzPatch::RemoveListItem { key, .. }] if key == "tag"
		));
		assert!(pending_after_direct_delta(&pending, &append_delta).is_empty());
		assert!(pending_after_direct_delta(&pending, &remove_delta).is_empty());
	}

	#[test]
	fn descendant_remove_list_item_resolves_dag_join_conflict_in_final_output() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("reset", "Reset", vec![], vec![]),
				mod_with("b", "B", vec!["Reset"], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("reset", 2),
				file_contributor("b", 3),
				file_contributor("c", 4),
			],
			Some("tag = ROOT\ntag = X\n"),
			parsed_inventory(&[
				("a", "tag = ROOT\n"),
				("reset", "tag = ROOT\n"),
				("b", "tag = ROOT\ntag = X\n"),
				("c", "tag = ROOT\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(remove_list_keys(patches_for(&result, "c")), vec!["tag"]);
		assert!(
			result.merge_result.conflicts.is_empty(),
			"descendant removal must settle the parent append/remove conflict: {:?}",
			result.merge_result.conflicts
		);
		assert_eq!(rendered(&result.merged_statements), "tag = ROOT\n");
		assert!(
			result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|record| record.action == "downstream_override")
		);
	}

	fn prov(result: &DagMergeComputation, key: &str) -> Vec<String> {
		result
			.definition_provenance
			.get(key)
			.cloned()
			.unwrap_or_default()
	}

	#[test]
	fn provenance_credits_the_single_mod_that_adds_a_block() {
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some("root = yes\n"),
			parsed_inventory(&[("a", "root = yes\nalpha = {\n\tx = 1\n}\n")]),
			IgnoreReplacePath::None,
		);
		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(prov(&result, "alpha"), vec!["a".to_string()]);
		// Unchanged vanilla key gets no provenance entry.
		assert!(prov(&result, "root").is_empty());
	}

	#[test]
	fn provenance_excludes_a_mod_that_ships_a_block_identical_to_vanilla() {
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some("shared = {\n\tx = 1\n}\n"),
			// `a` re-ships `shared` byte-identical and adds `extra`.
			parsed_inventory(&[("a", "shared = {\n\tx = 1\n}\nextra = yes\n")]),
			IgnoreReplacePath::None,
		);
		assert!(result.merge_result.conflicts.is_empty());
		assert!(
			prov(&result, "shared").is_empty(),
			"no-op-vs-base must not be credited: {:?}",
			result.definition_provenance
		);
		assert_eq!(prov(&result, "extra"), vec!["a".to_string()]);
	}

	#[test]
	fn provenance_credits_vanilla_identical_reintroduction_after_reset() {
		let result = compute(
			vec![mod_with("reset", "Reset", vec![], vec!["common"])],
			vec![file_contributor("reset", 1)],
			Some("shared = {\n\tx = 1\n}\n"),
			parsed_inventory(&[("reset", "shared = {\n\tx = 1\n}\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(prov(&result, "shared"), vec!["reset".to_string()]);
	}

	#[test]
	fn provenance_credits_a_surviving_identical_duplicate_root_definition() {
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some("shared = {\n\tx = 1\n}\n"),
			parsed_inventory(&[("a", "shared = {\n\tx = 1\n}\nshared = {\n\tx = 1\n}\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			same_key_statements(&result.merged_statements, "shared").len(),
			2,
			"patches={:?}, output={}",
			patches_for(&result, "a"),
			rendered(&result.merged_statements),
		);
		assert_eq!(prov(&result, "shared"), vec!["a".to_string()]);
	}

	#[test]
	fn duplicate_root_multiset_tracks_signed_signature_delta() {
		let parent = r#"shared = { value = A }
			shared = { value = A }
			shared = { value = B }
		"#;
		let child = r#"shared = { value = A }
			shared = { value = B }
			shared = { value = B }
		"#;
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some(parent),
			parsed_inventory(&[("a", child)]),
			IgnoreReplacePath::None,
		);
		let parent_parsed = parsed_file("parent", parent);
		let child_parsed = parsed_file("child", child);
		let a_identity = statement_signature(parent_parsed.ast.statements.first().expect("A root"));
		let b_identity = statement_signature(child_parsed.ast.statements.get(1).expect("B root"));
		let AstStatement::Assignment { value: a_value, .. } = &parent_parsed.ast.statements[0]
		else {
			panic!("A root must be an assignment");
		};
		let AstStatement::Assignment { value: b_value, .. } = &child_parsed.ast.statements[1]
		else {
			panic!("B root must be an assignment");
		};
		let patches = patches_for(&result, "a");
		let removes_identity = |identity: &str, value: &AstValue| {
			patches
				.iter()
				.filter(|patch| match patch {
					ClausewitzPatch::RemoveNode { removed, .. } => {
						statement_signature(removed) == identity
					}
					ClausewitzPatch::RemoveListItem {
						key,
						value: patch_value,
						..
					} => {
						key == "shared"
							&& semantic_value_identity(patch_value)
								== semantic_value_identity(value)
					}
					_ => false,
				})
				.count()
		};
		let inserts_identity = |identity: &str, value: &AstValue| {
			patches
				.iter()
				.filter(|patch| match patch {
					ClausewitzPatch::InsertNode { statement, .. } => {
						statement_signature(statement) == identity
					}
					ClausewitzPatch::AppendListItem {
						key,
						value: patch_value,
						..
					} => {
						key == "shared"
							&& semantic_value_identity(patch_value)
								== semantic_value_identity(value)
					}
					_ => false,
				})
				.count()
		};
		let actual = same_key_statements(&result.merged_statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();
		let expected = same_key_statements(&child_parsed.ast.statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(removes_identity(&a_identity, a_value), 1, "{patches:?}");
		assert_eq!(inserts_identity(&b_identity, b_value), 1, "{patches:?}");
		assert_eq!(removes_identity(&b_identity, b_value), 0, "{patches:?}");
		assert_eq!(inserts_identity(&a_identity, a_value), 0, "{patches:?}");
		assert_eq!(
			actual,
			expected,
			"output={}",
			rendered(&result.merged_statements)
		);
		assert_eq!(prov(&result, "shared"), vec!["a".to_string()]);
	}

	#[test]
	fn duplicate_root_occurrence_order_preserves_a_b_a() {
		let parent = "shared = { value = A }\n";
		let child = r#"shared = { value = A }
			shared = { value = B }
			shared = { value = A }
		"#;
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some(parent),
			parsed_inventory(&[("a", child)]),
			IgnoreReplacePath::None,
		);
		let child_parsed = parsed_file("child", child);
		let actual = same_key_statements(&result.merged_statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();
		let expected = same_key_statements(&child_parsed.ast.statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			actual,
			expected,
			"output={}",
			rendered(&result.merged_statements)
		);
	}

	#[test]
	fn field_value_root_deltas_do_not_duplicate_id_addressed_events() {
		let base = r#"country_event = { id = base.1 marker = base }
		"#;
		let branch_a = r#"country_event = { id = base.1 marker = base }
			country_event = { id = a.1 marker = a }
		"#;
		let branch_b = r#"country_event = { id = base.1 marker = base }
			country_event = { id = b.1 marker = b }
		"#;
		let result = compute_with_merge_key(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(base),
			parsed_inventory(&[("a", branch_a), ("b", branch_b)]),
			IgnoreReplacePath::None,
			&[],
			MergeKeySource::FieldValue("id"),
		);

		for mod_id in ["a", "b"] {
			assert!(
				patches_for(&result, mod_id).iter().all(|patch| !matches!(
					patch,
					ClausewitzPatch::AppendListItem { key, .. }
						| ClausewitzPatch::RemoveListItem { key, .. }
						if key == "country_event"
				)),
				"{mod_id} patches={:?}",
				patches_for(&result, mod_id)
			);
		}
		let output = rendered(&result.merged_statements);
		for id in ["base.1", "a.1", "b.1"] {
			assert_eq!(
				output.matches(&format!("id = {id}")).count(),
				1,
				"id={id}, output={output}, patches={:?}",
				result.mod_patches
			);
		}
	}

	#[test]
	fn event_options_with_unique_names_merge_by_name() {
		let base = r#"country_event = {
			id = test.1
			option = { name = OPTION_A base_effect = yes }
			option = { name = OPTION_B untouched = yes }
		}
		"#;
		let branch_a = r#"country_event = {
			id = test.1
			option = { name = OPTION_A base_effect = yes from_a = yes }
			option = { name = OPTION_B untouched = yes }
		}
		"#;
		let branch_b = r#"country_event = {
			id = test.1
			option = { name = OPTION_A base_effect = yes from_b = yes }
			option = { name = OPTION_B untouched = yes }
		}
		"#;
		let policies = MergePolicies {
			merge_key_source: MergeKeySource::FieldValue("id"),
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "name",
				child_types: &["option"],
			},
			..MergePolicies::default()
		};
		let result = compute_with_policies(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(base),
			parsed_inventory(&[("a", branch_a), ("b", branch_b)]),
			&policies,
		);

		assert!(result.merge_result.conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert_eq!(output.matches("name = OPTION_A").count(), 1, "{output}");
		assert_eq!(output.matches("name = OPTION_B").count(), 1, "{output}");
		assert_eq!(output.matches("from_a = yes").count(), 1, "{output}");
		assert_eq!(output.matches("from_b = yes").count(), 1, "{output}");
	}

	#[test]
	fn event_options_with_duplicate_names_keep_source_occurrences() {
		let base = "country_event = { id = test.1 }\n";
		let branch = r#"country_event = {
			id = test.1
			option = { name = OPTION_A marker = first }
			option = { name = OPTION_A marker = second }
		}
		"#;
		let policies = MergePolicies {
			merge_key_source: MergeKeySource::FieldValue("id"),
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "name",
				child_types: &["option"],
			},
			..MergePolicies::default()
		};
		let result = compute_with_policies(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some(base),
			parsed_inventory(&[("a", branch)]),
			&policies,
		);

		let output = rendered(&result.merged_statements);
		assert_eq!(output.matches("name = OPTION_A").count(), 2, "{output}");
		assert_eq!(output.matches("marker = first").count(), 1, "{output}");
		assert_eq!(output.matches("marker = second").count(), 1, "{output}");
	}

	#[test]
	fn union_lists_retain_base_items_inside_matched_repeated_blocks() {
		let base = r#"manufactories = {
			embracement_speed = {
				modifier = {
					factor = 0.8
					potential = { OR = { trade_goods = spices trade_goods = cloves } }
					custom_trigger_tooltip = { tooltip = tooltip_tradecompany }
				}
				modifier = {
					factor = 0.2
					potential = { OR = { trade_goods = fur trade_goods = cloves } }
					custom_trigger_tooltip = { tooltip = tooltip_plantations }
				}
			}
		}
		"#;
		let expanded = r#"manufactories = {
			embracement_speed = {
				modifier = {
					factor = 0.8
					potential = { OR = { trade_goods = spices trade_goods = cocoa } }
					custom_trigger_tooltip = { tooltip = tooltip_tradecompany }
				}
				modifier = {
					factor = 0.2
					potential = { OR = { trade_goods = fur trade_goods = cloves } }
					custom_trigger_tooltip = { tooltip = tooltip_plantations }
				}
			}
		}
		"#;
		let companion = base.replace(
			"embracement_speed = {",
			"compatibility_marker = yes\n\t\t\tembracement_speed = {",
		);
		let policies = MergePolicies {
			merge_key_source: MergeKeySource::AssignmentKey,
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "tooltip",
				child_types: &["modifier"],
			},
			list: ListMergePolicy::Union,
			..MergePolicies::default()
		};
		let result = compute_with_policies(
			vec![
				mod_with("expanded", "Expanded", vec![], vec![]),
				mod_with("companion", "Companion", vec![], vec![]),
			],
			vec![
				file_contributor("expanded", 1),
				file_contributor("companion", 2),
			],
			Some(base),
			parsed_inventory(&[("expanded", expanded), ("companion", &companion)]),
			&policies,
		);

		assert!(result.merge_result.conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert_eq!(
			output.matches("tooltip = tooltip_tradecompany").count(),
			1,
			"{output}"
		);
		assert_eq!(
			output.matches("tooltip = tooltip_plantations").count(),
			1,
			"{output}"
		);
		assert_eq!(
			output.matches("trade_goods = cloves").count(),
			2,
			"{output}\npatches={:?}",
			result.mod_patches
		);
		assert_eq!(output.matches("trade_goods = cocoa").count(), 1, "{output}");
		assert_eq!(
			output.matches("compatibility_marker = yes").count(),
			1,
			"{output}"
		);
	}

	fn event_merge_policies() -> MergePolicies {
		MergePolicies {
			merge_key_source: MergeKeySource::FieldValue("id"),
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "name",
				child_types: &["option"],
			},
			scalar: foch_language::analyzer::content_family::ScalarMergePolicy::LastWriter,
			list: ListMergePolicy::UnionWithRename,
			edit_wins_over_remove: true,
			..MergePolicies::default()
		}
	}

	#[test]
	fn descendant_edit_preserves_sibling_removed_ancestor_block() {
		let base = "country_event = { id = test.1 after = { base_cleanup = yes } }\n";
		let removed = "country_event = { id = test.1 }\n";
		let extended = r#"country_event = {
			id = test.1
			after = { base_cleanup = yes mod_cleanup = yes }
		}
		"#;
		let result = compute_with_policies(
			vec![
				mod_with("removed", "Removed", vec![], vec![]),
				mod_with("extended", "Extended", vec![], vec![]),
			],
			vec![
				file_contributor("removed", 1),
				file_contributor("extended", 2),
			],
			Some(base),
			parsed_inventory(&[("removed", removed), ("extended", extended)]),
			&event_merge_policies(),
		);

		assert!(result.merge_result.conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("base_cleanup = yes"), "{output}");
		assert!(output.contains("mod_cleanup = yes"), "{output}");
	}

	#[test]
	fn event_descriptions_merge_by_localisation_identity() {
		let base = r#"country_event = {
			id = test.1
			desc = { trigger = { mode = stable } desc = test.1.da }
			desc = { trigger = { mode = base } desc = test.1.db }
		}
		"#;
		let old_model = base.replace("mode = base", "mode = old_model");
		let new_model = base.replace("mode = base", "mode = new_model");
		let result = compute_with_policies(
			vec![
				mod_with("old", "Old", vec![], vec![]),
				mod_with("new", "New", vec![], vec![]),
			],
			vec![file_contributor("old", 1), file_contributor("new", 2)],
			Some(base),
			parsed_inventory(&[("old", &old_model), ("new", &new_model)]),
			&event_merge_policies(),
		);

		assert!(result.merge_result.conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert_eq!(output.matches("desc = test.1.db").count(), 1, "{output}");
		assert!(output.contains("mode = new_model"), "{output}");
		assert!(!output.contains("mode = old_model"), "{output}");
	}

	#[test]
	fn event_control_flow_merges_if_branches_without_reviving_replaced_branch() {
		let base = r#"country_event = {
			id = test.1
			option = {
				name = test.1.a
				if = { limit = { has_government_attribute = republican_virtues } define_ruler = { change_adm = 1 } }
				else = { define_ruler = {} }
				if = { limit = { has_country_flag = old_candidate_flag } add_estate_loyalty = 5 }
			}
		}
		"#;
		let ge = base.replace(
			"name = test.1.a",
			"name = test.1.a\n\t\t\t\tge_marker = yes",
		);
		let ee = r#"country_event = {
			id = test.1
			option = {
				name = test.1.a
				if = { limit = { has_country_flag = upgraded_candidate_flag } define_ruler = { change_mil = 1 } }
				else = { define_ruler = {} }
				if = { limit = { has_saved_event_target = spread_target } add_province_modifier = support }
			}
		}
		"#;
		let result = compute_tree_event_join(Some(base), &ge, ee).expect("tree control-flow join");

		assert!(result.merge_result.conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert!(
			output.contains("has_government_attribute = republican_virtues"),
			"{output}",
		);
		assert!(
			output.contains("has_country_flag = upgraded_candidate_flag"),
			"{output}"
		);
		assert!(
			output.contains("has_saved_event_target = spread_target"),
			"{output}"
		);
		assert!(
			!output.contains("has_country_flag = old_candidate_flag"),
			"{output}"
		);
	}

	#[test]
	fn descendant_removal_cancels_only_one_identical_ancestor_occurrence() {
		let result = compute(
			vec![
				mod_with("ancestor", "Ancestor", vec![], vec![]),
				mod_with("descendant", "Descendant", vec!["Ancestor"], vec![]),
			],
			vec![
				file_contributor("ancestor", 1),
				file_contributor("descendant", 2),
			],
			Some(""),
			parsed_inventory(&[
				(
					"ancestor",
					"shared = { value = A }\nshared = { value = A }\n",
				),
				("descendant", "shared = { value = A }\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			same_key_statements(&result.merged_statements, "shared").len(),
			1,
			"output={}",
			rendered(&result.merged_statements)
		);
		assert_eq!(
			prov(&result, "shared"),
			vec!["ancestor".to_string(), "descendant".to_string()]
		);
	}

	#[test]
	fn duplicate_root_removal_preserves_a_b_target_order() {
		let parent = r#"shared = { value = A }
			shared = { value = B }
			shared = { value = A }
		"#;
		let child = r#"shared = { value = A }
			shared = { value = B }
		"#;
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some(parent),
			parsed_inventory(&[("a", child)]),
			IgnoreReplacePath::None,
		);
		let child_parsed = parsed_file("child", child);
		let actual = same_key_statements(&result.merged_statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();
		let expected = same_key_statements(&child_parsed.ast.statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			actual,
			expected,
			"output={}",
			rendered(&result.merged_statements)
		);
	}

	#[test]
	fn sibling_list_insertions_use_branch_local_anchors() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("tag = C\n"),
			parsed_inventory(&[("a", "tag = A\ntag = C\n"), ("b", "tag = C\ntag = B\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			root_scalar_values(&result.merged_statements, "tag"),
			vec!["A", "C", "B"]
		);
	}

	#[test]
	fn empty_base_insert_and_append_share_logical_cardinality() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(""),
			parsed_inventory(&[("a", "tag = A\n"), ("b", "tag = A\ntag = B\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			root_scalar_values(&result.merged_statements, "tag"),
			vec!["A", "B"]
		);
	}

	#[test]
	fn independent_contributors_union_one_same_key_root() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(""),
			parsed_inventory(&[
				("a", "shared = {\n\tleft = yes\n}\n"),
				("b", "shared = {\n\tright = yes\n}\n"),
			]),
			IgnoreReplacePath::None,
		);

		let output = rendered(&result.merged_statements);
		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			same_key_statements(&result.merged_statements, "shared").len(),
			1,
			"independent structural edits must not duplicate their logical root: {output}"
		);
		assert!(output.contains("left = yes"), "{output}");
		assert!(output.contains("right = yes"), "{output}");
	}

	#[test]
	fn independent_sprite_types_branches_union_one_container() {
		const SPRITE_TYPES: MergeKeySource = MergeKeySource::ContainerChildFieldValue {
			containers: &["spriteTypes"],
			child_key_field: "name",
			child_types: &["spriteType"],
		};
		let result = compute_with_merge_key(
			vec![
				mod_with("baseline", "Baseline", vec![], vec![]),
				mod_with("a", "A", vec!["Baseline"], vec![]),
				mod_with("b", "B", vec!["Baseline"], vec![]),
			],
			vec![
				file_contributor("baseline", 1),
				file_contributor("a", 2),
				file_contributor("b", 3),
			],
			None,
			parsed_inventory(&[
				(
					"baseline",
					r#"spriteTypes = {
						spriteType = { name = "GFX_anchor" texturefile = "anchor.dds" }
					}"#,
				),
				(
					"a",
					r#"spriteTypes = {
						spriteType = { name = "GFX_left" texturefile = "left.dds" }
					}"#,
				),
				(
					"b",
					r#"spriteTypes = {
						spriteType = { name = "GFX_right" texturefile = "right.dds" }
					}"#,
				),
			]),
			IgnoreReplacePath::None,
			&[],
			SPRITE_TYPES,
		);

		let output = rendered(&result.merged_statements);
		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			same_key_statements(&result.merged_statements, "spriteTypes").len(),
			1,
			"named child union must not duplicate spriteTypes: {output}"
		);
		assert!(output.contains("GFX_left"), "{output}");
		assert!(output.contains("GFX_right"), "{output}");
	}

	#[test]
	fn duplicate_root_survival_uses_signature_multiplicity() {
		let parent = parsed_file("parent", "shared = {\n\tx = 1\n}\n");
		let contributor = parsed_file("a", "shared = {\n\tx = 1\n}\nshared = {\n\tx = 1\n}\n");

		assert!(direct_definition_contribution_survives(
			&parent.ast.statements,
			&contributor.ast.statements,
			&contributor.ast.statements,
			"shared",
		));
	}

	#[test]
	fn repeated_key_provenance_excludes_overridden_direct_item() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("tag = ROOT\ntag = SHARED\n"),
			parsed_inventory(&[
				("a", "tag = ROOT\ntag = SHARED\ntag = A\n"),
				("b", "tag = ROOT\ntag = SHARED\ntag = A\ntag = B\n"),
				("c", "tag = ROOT\ntag = SHARED\ntag = A\ntag = C\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(prov(&result, "tag"), vec!["a".to_string(), "c".to_string()]);
	}

	#[test]
	fn repeated_key_provenance_credits_only_reappend_after_removal() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("tag = ROOT\n"),
			parsed_inventory(&[
				("a", "tag = ROOT\ntag = X\n"),
				("b", "tag = ROOT\n"),
				("c", "tag = ROOT\ntag = X\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(rendered(&result.merged_statements), "tag = ROOT\ntag = X\n");
		assert_eq!(prov(&result, "tag"), vec!["c".to_string()]);
	}

	#[test]
	fn provenance_unions_credit_every_contributing_mod() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("block = {\n\ta = 1\n}\n"),
			parsed_inventory(&[
				("a", "block = {\n\ta = 1\n\tb = 2\n}\n"),
				("b", "block = {\n\ta = 1\n\tc = 3\n}\n"),
			]),
			IgnoreReplacePath::None,
		);
		assert!(result.merge_result.conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert!(
			output.contains("b = 2") && output.contains("c = 3"),
			"{output}"
		);
		assert_eq!(
			prov(&result, "block"),
			vec!["a".to_string(), "b".to_string()]
		);
	}

	#[test]
	fn provenance_excludes_the_overridden_loser_in_a_dependency_chain() {
		// `b` depends on `a` and replaces `a`'s block with an incompatible body.
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\nthing = {\n\tname = \"old\"\n}\n"),
				("b", "root = yes\nthing = {\n\tname = \"new\"\n}\n"),
			]),
			IgnoreReplacePath::None,
		);
		assert!(result.merge_result.conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("name = \"new\""), "{output}");
		assert!(!output.contains("name = \"old\""), "{output}");
		// Only the adopted winner is credited; `a`'s overridden body is excluded.
		assert_eq!(prov(&result, "thing"), vec!["b".to_string()]);
	}

	#[test]
	fn provenance_and_participants_exclude_inherited_child_content() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("c", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\nowned_by_a = {\n\tx = 1\n}\n"),
				(
					"c",
					"root = yes\nowned_by_a = {\n\tx = 1\n}\nowned_by_c = yes\n",
				),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(prov(&result, "owned_by_a"), vec!["a".to_string()]);
		assert_eq!(prov(&result, "owned_by_c"), vec!["c".to_string()]);
		let participants = result
			.definition_participants
			.get("owned_by_a")
			.expect("A's direct definition is tracked");
		assert_eq!(participants.len(), 1);
		assert_eq!(participants[0].mod_id, "a");
		assert!(
			!result.definition_participants.contains_key("root"),
			"unchanged inherited/base content is not a direct participant"
		);
	}
}
