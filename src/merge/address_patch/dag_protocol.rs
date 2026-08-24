//! Address-patch implementation of the neutral DAG execution protocols.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::game::eu4::content::{MergeKeySource, MergePolicies, ScriptFileKind};
use crate::game::eu4::script::ParsedScriptFile;
use crate::game::eu4::script::parser::{AstFile, AstStatement};
use crate::model::HandlerResolutionRecord;

use crate::merge::address_patch::cache::{DagBaseCache, ModDiffCache};
use crate::merge::planning::dag::{FileDag, ModId};
use crate::merge::planning::dag_pipeline::{
	DagJoinProtocol, DagJoinRequest, DagPipelineResult, EffectiveNodeProtocol,
	EffectiveNodeRequest, execute_dag_pipeline,
};
use crate::merge::resolution::conflict_handler::ConflictHandler;

use super::patch::{
	ClausewitzPatch, ListItemOccurrence, ListItemTarget, align_sequences_by, diff_ast_with_nested,
	fold_renames, insertion_source_slot, semantic_occurrence_ordinals,
};
use super::patch_apply::apply_patches_with_nested;
use super::patch_merge::{
	PatchMergeResult, PatchResolution, merge_patch_sets_for_file, order_patches_by_source,
	semantic_statement_identity, semantic_value_identity,
};

pub(crate) struct ReferenceDagProtocolRequest<'a> {
	pub file_dag: &'a FileDag,
	pub base_statements: &'a [AstStatement],
	pub template: Option<&'a ParsedScriptFile>,
	pub contributors: &'a HashMap<ModId, ParsedScriptFile>,
	pub merge_key_source: MergeKeySource,
	pub policies: &'a MergePolicies,
	pub handler: &'a mut dyn ConflictHandler,
	pub mod_hashes: Option<&'a HashMap<ModId, String>>,
	pub game_version: &'a str,
}

pub(crate) struct ReferenceDagProtocolOutput {
	pub mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	pub merged_statements: Vec<AstStatement>,
	pub merge_result: PatchMergeResult,
	pub parent_statements: HashMap<ModId, Vec<AstStatement>>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct ReferenceDagCaches<'a> {
	pub diff: Option<&'a ModDiffCache>,
	pub dag_base: Option<&'a DagBaseCache>,
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
pub(crate) enum DagApplyCacheScope {
	EffectiveNode,
	ResolvedBranchState,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DagApplyCacheEvent {
	pub scope: DagApplyCacheScope,
	pub hit: bool,
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
pub(crate) fn reset_dag_apply_cache_events() {
	DAG_APPLY_CACHE_EVENTS.with(|events| events.borrow_mut().clear());
}

#[cfg(test)]
pub(crate) fn dag_apply_cache_events() -> Vec<DagApplyCacheEvent> {
	DAG_APPLY_CACHE_EVENTS.with(|events| events.borrow().clone())
}

pub(crate) fn execute_reference_dag(
	request: ReferenceDagProtocolRequest<'_>,
) -> Result<ReferenceDagProtocolOutput, String> {
	let caches_enabled = request.mod_hashes.is_some_and(|hashes| !hashes.is_empty());
	let diff_cache = caches_enabled.then(ModDiffCache::open_default);
	let dag_base_cache = caches_enabled.then(DagBaseCache::open_default);
	execute_reference_dag_with_caches(
		request,
		ReferenceDagCaches {
			diff: diff_cache.as_ref(),
			dag_base: dag_base_cache.as_ref(),
		},
	)
}

pub(crate) fn execute_reference_dag_with_caches(
	request: ReferenceDagProtocolRequest<'_>,
	caches: ReferenceDagCaches<'_>,
) -> Result<ReferenceDagProtocolOutput, String> {
	let ReferenceDagProtocolRequest {
		file_dag,
		base_statements,
		template,
		contributors,
		merge_key_source,
		policies,
		handler,
		mod_hashes,
		game_version,
	} = request;
	let diff_cache_context = format!(
		"parent-relative-v10 {game_version} kernel=legacy merge_key={merge_key_source:?} nested_merge_key={:?}",
		policies.nested_merge_key_source,
	);
	let policy_debug = format!("{policies:?}");
	let policy_hash = blake3::hash(policy_debug.as_bytes()).to_hex().to_string();
	let dag_base_cache_context = format!(
		"parent-relative-v10 {game_version} kernel=legacy merge_key={merge_key_source:?} policies={policy_hash}",
	);
	let root = PatchBaselineDagState {
		statements: base_statements.to_vec(),
		intent_only_patches: Vec::new(),
		pending_conflicts: Vec::new(),
	};
	let mut protocol = PatchBaselineDagProtocol {
		file_dag,
		base_statements,
		template,
		merge_key_source,
		policies,
		handler,
		mod_hashes,
		diff_cache: caches.diff,
		dag_base_cache: caches.dag_base,
		diff_cache_context,
		dag_base_cache_context,
		mod_patches: Vec::new(),
		merge_result: PatchMergeResult::default(),
		seen_pending_conflicts: Vec::new(),
	};
	let DagPipelineResult {
		final_state,
		parent_states,
	} = execute_dag_pipeline(file_dag, contributors, root, &mut protocol)?;
	record_downstream_resolutions(
		&mut protocol.merge_result,
		&protocol.seen_pending_conflicts,
		&final_state.pending_conflicts,
		file_dag.file_path(),
	);
	protocol.merge_result.conflicts = final_state.pending_conflicts;
	normalize_merge_result(&mut protocol.merge_result);
	let PatchBaselineDagProtocol {
		mod_patches,
		merge_result,
		..
	} = protocol;
	Ok(ReferenceDagProtocolOutput {
		mod_patches,
		merged_statements: final_state.statements,
		merge_result,
		parent_statements: parent_states
			.into_iter()
			.map(|(mod_id, state)| (mod_id, state.statements))
			.collect(),
	})
}

pub(crate) fn hash_ast_statements(statements: &[AstStatement]) -> Option<String> {
	let encoded = bincode::serialize(statements).ok()?;
	Some(blake3::hash(&encoded).to_hex().to_string())
}

pub(crate) fn hash_dag_apply_input(
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
		return compute_ordered_patches(
			current_base,
			current,
			merge_key_source,
			nested_merge_key_source,
		);
	};
	if let Some(mut patches) = cache.lookup(
		target_path,
		mod_hash,
		base_view_hash,
		env!("CARGO_PKG_VERSION"),
		game_version,
	) {
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
	let patches = compute_ordered_patches(
		current_base,
		current,
		merge_key_source,
		nested_merge_key_source,
	);
	if let Err(error) = cache.store(
		target_path,
		mod_hash,
		base_view_hash,
		env!("CARGO_PKG_VERSION"),
		game_version,
		&patches,
	) {
		tracing::warn!(
			target: "crate::merge::patch_reference",
			path = %target_path,
			error = %error,
			"failed to store mod diff cache entry"
		);
	}
	patches
}

fn compute_ordered_patches(
	base: &ParsedScriptFile,
	current: &ParsedScriptFile,
	merge_key_source: MergeKeySource,
	nested_merge_key_source: MergeKeySource,
) -> Vec<ClausewitzPatch> {
	let mut patches = fold_renames(diff_ast_with_nested(
		base,
		current,
		merge_key_source,
		nested_merge_key_source,
	));
	complete_root_duplicate_deltas(
		&base.ast.statements,
		&current.ast.statements,
		&mut patches,
		merge_key_source,
	);
	order_patches_by_source(&mut patches, &base.ast.statements, &current.ast.statements);
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
	if let Err(error) = cache.store(
		deps_hash,
		file_path,
		env!("CARGO_PKG_VERSION"),
		game_version,
		&statements,
	) {
		tracing::warn!(
			target: "crate::merge::patch_reference",
			path = %file_path,
			error = %error,
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
	let mut branch_patches =
		compute_ordered_patches(&base, &effective, key_sources.root, key_sources.nested);
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

pub(crate) fn pending_after_direct_delta(
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

pub(crate) fn extend_merge_result(target: &mut PatchMergeResult, source: PatchMergeResult) {
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
		file_kind: ScriptFileKind::new("other"),
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
