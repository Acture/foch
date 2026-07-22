// Patch set merging: given N mods' patch sets against a common base, merge
// them into a single resolved patch set with conflict detection.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use foch_core::model::HandlerResolutionRecord;
#[cfg(test)]
use foch_language::analyzer::content_family::NamedContainerPolicy;
#[cfg(test)]
use foch_language::analyzer::content_family::{BlockPatchPolicy, ScalarMergePolicy};
use foch_language::analyzer::content_family::{ListMergePolicy, MergeKeySource, MergePolicies};
use foch_language::analyzer::parser::{AstStatement, AstValue};

#[cfg(test)]
use super::super::conflict_handler::ConflictDecision;
use super::super::conflict_handler::ConflictHandler;
#[cfg(test)]
use super::super::conflict_handler::DeferHandler;
use super::super::error::MergeError;
use super::patch::{AstPath, ClausewitzPatch, ListItemTarget, fuzzy_rename_similarity};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Address of a patch — uniquely identifies what AST node is being changed.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub(crate) struct PatchAddress {
	pub path: AstPath,
	pub key: String,
}

/// A patch attributed to a specific mod.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AttributedPatch {
	pub mod_id: String,
	pub precedence: usize,
	pub patch: ClausewitzPatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PatchConflict {
	pub patches: Vec<AttributedPatch>,
	pub reason: String,
}

/// Result of merging patches at a single address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PatchResolution {
	/// Single mod or all mods agree — apply this patch.
	Resolved(ClausewitzPatch),
	/// Auto-resolved by policy (e.g., union of list items, highest precedence).
	AutoMerged {
		result: ClausewitzPatch,
		strategy: String,
		contributing_mods: Vec<String>,
	},
	/// Irreconcilable conflict — needs manual resolution.
	Conflict {
		address: PatchAddress,
		patches: Vec<AttributedPatch>,
		reason: String,
	},
}

/// Result of merging all patch sets.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PatchMergeResult {
	pub resolved: Vec<PatchResolution>,
	pub conflicts: Vec<PatchResolution>,
	pub stats: PatchMergeStats,
	pub handler_resolved_count: usize,
	pub handler_resolutions: Vec<HandlerResolutionRecord>,
	pub external_file_resolutions: HashMap<PathBuf, PathBuf>,
	pub keep_existing_paths: HashSet<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PatchMergeStats {
	pub total_patches: usize,
	pub single_mod_patches: usize,
	pub convergent_patches: usize,
	pub auto_merged_patches: usize,
	pub conflict_patches: usize,
	/// One mod edited a property (SetValue/ReplaceBlock/InsertNode) while
	/// another removed it (RemoveNode); the edit was kept and the remove
	/// dropped, instead of reporting a mixed-kinds conflict.
	pub edit_over_remove_resolved: usize,
}

impl PatchMergeStats {
	pub(crate) fn accumulate(&mut self, nested: &Self) {
		self.total_patches += nested.total_patches;
		self.single_mod_patches += nested.single_mod_patches;
		self.convergent_patches += nested.convergent_patches;
		self.auto_merged_patches += nested.auto_merged_patches;
		self.conflict_patches += nested.conflict_patches;
		self.edit_over_remove_resolved += nested.edit_over_remove_resolved;
	}
}

fn patch_sort_key(patch: &ClausewitzPatch) -> (AstPath, String, u8, Vec<u8>) {
	let (path, key, operation_rank) = match patch {
		ClausewitzPatch::Rename {
			path,
			old_key,
			new_key,
		} => (path.clone(), format!("{old_key}\0{new_key}"), 0),
		ClausewitzPatch::RemoveNode { path, key, .. } => (path.clone(), key.clone(), 1),
		ClausewitzPatch::RemoveListItem { path, key, .. } => (path.clone(), key.clone(), 2),
		ClausewitzPatch::RemoveBlockItem { path, .. } => (path.clone(), String::new(), 3),
		ClausewitzPatch::SetValue { path, key, .. } => (path.clone(), key.clone(), 4),
		ClausewitzPatch::ReplaceBlock { path, key, .. } => (path.clone(), key.clone(), 5),
		ClausewitzPatch::InsertNode { path, key, .. } => (path.clone(), key.clone(), 6),
		ClausewitzPatch::AppendListItem { path, key, .. } => (path.clone(), key.clone(), 7),
		ClausewitzPatch::AppendBlockItem { path, .. } => (path.clone(), String::new(), 8),
	};
	let payload = bincode::serialize(patch).unwrap_or_else(|_| format!("{patch:?}").into_bytes());
	(path, key, operation_rank, payload)
}

fn sort_contributors(mod_patches: &mut [(String, usize, Vec<ClausewitzPatch>)]) {
	mod_patches.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
}

type PatchSourceOrder = (usize, String, usize);

fn address_group_order_key(
	address: &PatchAddress,
	source_orders: &HashMap<PatchAddress, PatchSourceOrder>,
) -> (usize, String, usize, AstPath, String) {
	let (precedence, mod_id, source_ordinal) =
		source_orders
			.get(address)
			.cloned()
			.unwrap_or((usize::MAX, String::new(), usize::MAX));
	(
		precedence,
		mod_id,
		source_ordinal,
		address.path.clone(),
		address.key.clone(),
	)
}

pub(crate) fn semantic_value_identity(value: &AstValue) -> String {
	fingerprint::value_fingerprint(value)
}

pub(crate) fn semantic_statement_identity(statement: &AstStatement) -> String {
	fingerprint::statement_fingerprint(statement)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

mod address;
use address::patch_address;

mod conflict;
use conflict::{apply_conflict_decision, detect_cross_kind_sibling_conflicts};

mod resolve;
use resolve::resolve_address;

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Merge multiple mod patch sets into a single resolved set.
///
/// `mod_patches`: Vec of `(mod_id, precedence, patches)` for each mod.
/// `policies`: The content family's merge policies for auto-resolution.
pub fn merge_patch_sets(
	mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	policies: &MergePolicies,
	handler: &mut dyn ConflictHandler,
) -> Result<PatchMergeResult, MergeError> {
	merge_patch_sets_for_file(mod_patches, policies, handler, None)
}

pub(crate) fn merge_patch_sets_for_file(
	mut mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	policies: &MergePolicies,
	handler: &mut dyn ConflictHandler,
	current_file: Option<&Path>,
) -> Result<PatchMergeResult, MergeError> {
	let mut result = PatchMergeResult::default();
	sort_contributors(&mut mod_patches);

	// --- Pre-pass: collect renames and rewrite cross-mod addresses ---
	//
	// For each `Rename { path, old_key, new_key }` emitted by any mod, every
	// other mod's patches whose `(path, key)` match — or whose path traverses
	// `old_key` at that location — must be rewritten so they target the new
	// key instead. Otherwise the renaming mod's RemoveNode would conflict
	// with the modifier mod's edits at the old key.
	let rename_map = build_rename_map(&mod_patches);
	let mut mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)> = mod_patches
		.into_iter()
		.map(|(mod_id, prec, patches)| {
			let rewritten = patches
				.into_iter()
				.map(|p| rewrite_patch_for_renames(p, &rename_map))
				.collect();
			(mod_id, prec, rewritten)
		})
		.collect();
	sort_contributors(&mut mod_patches);
	let mod_patches = drop_prefixed_rename_duplicate_inserts(mod_patches);
	let mod_patches = normalize_singleton_list_inserts(mod_patches);
	let mod_patches = apply_list_policy(mod_patches, policies);
	let (mod_patches, prefix_edit_resolutions) =
		drop_removed_ancestors_with_sibling_descendant_edits(mod_patches, policies);
	result.stats.total_patches += prefix_edit_resolutions;
	result.stats.edit_over_remove_resolved += prefix_edit_resolutions;

	// Group patches by address, preserving attribution.
	let mut by_address: HashMap<PatchAddress, Vec<AttributedPatch>> = HashMap::new();
	let mut source_order_by_address: HashMap<PatchAddress, PatchSourceOrder> = HashMap::new();
	let mut source_ordinal_by_attribution: HashMap<(PatchAddress, usize, String), usize> =
		HashMap::new();

	for (mod_id, precedence, patches) in mod_patches {
		for (source_ordinal, patch) in patches.into_iter().enumerate() {
			result.stats.total_patches += 1;
			let addr = patch_address(&patch, policies);
			let source_order = (precedence, mod_id.clone(), source_ordinal);
			source_order_by_address
				.entry(addr.clone())
				.and_modify(|known| {
					if source_order < *known {
						known.clone_from(&source_order);
					}
				})
				.or_insert(source_order);
			source_ordinal_by_attribution
				.entry((addr.clone(), precedence, mod_id.clone()))
				.and_modify(|known| *known = (*known).min(source_ordinal))
				.or_insert(source_ordinal);
			by_address.entry(addr).or_default().push(AttributedPatch {
				mod_id: mod_id.clone(),
				precedence,
				patch,
			});
		}
	}

	// Cross-kind sibling conflict pre-check.
	//
	// `patch_address` fingerprints `RemoveNode` / `InsertNode` only for
	// Union-policy keys, where repeated named children are allowed to coexist.
	// That can split same-(path, key) patches of different kinds across
	// addresses — for example a fingerprinted `RemoveNode(owner)` versus an
	// unfingerprinted `SetValue(owner)`. Bucket by the kind-agnostic raw
	// `(path, key)` so these ambiguous sibling intents surface as one conflict
	// instead of applying independently.
	let mut cross_kind_conflicts =
		detect_cross_kind_sibling_conflicts(&by_address, &mut result.stats);
	for conflict in &mut cross_kind_conflicts {
		conflict.patches.sort_by(|left, right| {
			let left_address = patch_address(&left.patch, policies);
			let right_address = patch_address(&right.patch, policies);
			let left_ordinal = source_ordinal_by_attribution
				.get(&(left_address, left.precedence, left.mod_id.clone()))
				.copied()
				.unwrap_or(usize::MAX);
			let right_ordinal = source_ordinal_by_attribution
				.get(&(right_address, right.precedence, right.mod_id.clone()))
				.copied()
				.unwrap_or(usize::MAX);
			left.precedence
				.cmp(&right.precedence)
				.then_with(|| left.mod_id.cmp(&right.mod_id))
				.then_with(|| left_ordinal.cmp(&right_ordinal))
				.then_with(|| patch_sort_key(&left.patch).cmp(&patch_sort_key(&right.patch)))
		});
	}
	cross_kind_conflicts.sort_by_key(|conflict| {
		let (precedence, mod_id, source_ordinal) = conflict
			.split_addresses
			.iter()
			.filter_map(|address| source_order_by_address.get(address).cloned())
			.min()
			.unwrap_or((usize::MAX, String::new(), usize::MAX));
		(
			precedence,
			mod_id,
			source_ordinal,
			conflict.address.path.clone(),
			conflict.address.key.clone(),
		)
	});
	let cross_kind_addresses: HashSet<PatchAddress> = cross_kind_conflicts
		.iter()
		.flat_map(|conflict| conflict.split_addresses.iter().cloned())
		.collect();
	for addr in &cross_kind_addresses {
		by_address.remove(addr);
	}

	let mut address_groups = by_address.into_iter().collect::<Vec<_>>();
	address_groups
		.sort_by_key(|(address, _)| address_group_order_key(address, &source_order_by_address));
	let mut pending_resolutions = Vec::with_capacity(address_groups.len());
	for (addr, attributed) in address_groups {
		pending_resolutions.push(resolve_address(
			addr,
			attributed,
			policies,
			&mut result.stats,
		));
	}

	let total_conflicts = pending_resolutions
		.iter()
		.filter(|resolution| matches!(resolution, PatchResolution::Conflict { .. }))
		.count()
		+ cross_kind_conflicts.len();
	let mut current_conflict = 0;

	for resolution in pending_resolutions {
		match resolution {
			PatchResolution::Conflict {
				address,
				patches,
				reason,
			} => {
				current_conflict += 1;
				handler.set_conflict_progress(current_conflict, total_conflicts);
				apply_conflict_decision(
					&mut result,
					handler,
					current_file,
					address,
					patches,
					reason,
				)?;
			}
			resolution => result.resolved.push(resolution),
		}
	}

	for cross_kind in cross_kind_conflicts {
		current_conflict += 1;
		handler.set_conflict_progress(current_conflict, total_conflicts);
		apply_conflict_decision(
			&mut result,
			handler,
			current_file,
			cross_kind.address,
			cross_kind.patches,
			cross_kind.reason,
		)?;
	}

	Ok(result)
}

fn drop_removed_ancestors_with_sibling_descendant_edits(
	mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	policies: &MergePolicies,
) -> (Vec<(String, usize, Vec<ClausewitzPatch>)>, usize) {
	if !policies.edit_wins_over_remove {
		return (mod_patches, 0);
	}

	let descendant_edits = mod_patches
		.iter()
		.flat_map(|(mod_id, _, patches)| {
			patches
				.iter()
				.filter(|patch| is_descendant_edit_patch(patch))
				.map(move |patch| (mod_id.clone(), patch_path_for_prefix_match(patch).to_vec()))
		})
		.collect::<Vec<_>>();
	let mut dropped = 0;
	let filtered = mod_patches
		.into_iter()
		.map(|(mod_id, precedence, patches)| {
			let patches = patches
				.into_iter()
				.filter(|patch| {
					let ClausewitzPatch::RemoveNode { path, key, .. } = patch else {
						return true;
					};
					let mut removed_path = path.clone();
					removed_path.push(key.clone());
					let has_sibling_descendant_edit =
						descendant_edits.iter().any(|(edit_mod_id, edit_path)| {
							edit_mod_id != &mod_id && edit_path.starts_with(&removed_path)
						});
					if has_sibling_descendant_edit {
						dropped += 1;
					}
					!has_sibling_descendant_edit
				})
				.collect();
			(mod_id, precedence, patches)
		})
		.collect();
	(filtered, dropped)
}

fn is_descendant_edit_patch(patch: &ClausewitzPatch) -> bool {
	matches!(
		patch,
		ClausewitzPatch::SetValue { .. }
			| ClausewitzPatch::ReplaceBlock { .. }
			| ClausewitzPatch::InsertNode { .. }
			| ClausewitzPatch::AppendListItem { .. }
			| ClausewitzPatch::AppendBlockItem { .. }
			| ClausewitzPatch::Rename { .. }
	)
}

fn patch_path_for_prefix_match(patch: &ClausewitzPatch) -> &[String] {
	match patch {
		ClausewitzPatch::SetValue { path, .. }
		| ClausewitzPatch::RemoveNode { path, .. }
		| ClausewitzPatch::InsertNode { path, .. }
		| ClausewitzPatch::AppendListItem { path, .. }
		| ClausewitzPatch::RemoveListItem { path, .. }
		| ClausewitzPatch::ReplaceBlock { path, .. }
		| ClausewitzPatch::AppendBlockItem { path, .. }
		| ClausewitzPatch::RemoveBlockItem { path, .. }
		| ClausewitzPatch::Rename { path, .. } => path,
	}
}

fn apply_list_policy(
	mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	policies: &MergePolicies,
) -> Vec<(String, usize, Vec<ClausewitzPatch>)> {
	let has_nested_semantic_identity = matches!(
		policies.nested_merge_key_source,
		MergeKeySource::ChildFieldValue { .. } | MergeKeySource::ContainerChildFieldValue { .. }
	);
	if policies.list == ListMergePolicy::Replace
		|| mod_patches.len() <= 1
		|| !has_nested_semantic_identity
	{
		return mod_patches;
	}
	let contributor_count = mod_patches
		.iter()
		.map(|(mod_id, _, _)| mod_id)
		.collect::<HashSet<_>>()
		.len();
	let mut removers_by_address: HashMap<PatchAddress, HashSet<String>> = HashMap::new();
	for (mod_id, _, patches) in &mod_patches {
		for patch in patches {
			if matches!(
				patch,
				ClausewitzPatch::RemoveListItem {
					value: AstValue::Scalar { .. },
					..
				}
			) {
				removers_by_address
					.entry(patch_address(patch, policies))
					.or_default()
					.insert(mod_id.clone());
			}
		}
	}

	mod_patches
		.into_iter()
		.map(|(mod_id, precedence, patches)| {
			let patches = patches
				.into_iter()
				.filter(|patch| {
					if !matches!(
						patch,
						ClausewitzPatch::RemoveListItem {
							value: AstValue::Scalar { .. },
							..
						}
					) {
						return true;
					}
					removers_by_address
						.get(&patch_address(patch, policies))
						.is_some_and(|removers| removers.len() == contributor_count)
				})
				.collect();
			(mod_id, precedence, patches)
		})
		.collect()
}

fn normalize_singleton_list_inserts(
	mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
) -> Vec<(String, usize, Vec<ClausewitzPatch>)> {
	let list_addresses = mod_patches
		.iter()
		.flat_map(|(_, _, patches)| patches)
		.filter_map(|patch| match patch {
			ClausewitzPatch::AppendListItem { path, key, .. } => Some((path.clone(), key.clone())),
			_ => None,
		})
		.collect::<HashSet<_>>();
	if list_addresses.is_empty() {
		return mod_patches;
	}

	mod_patches
		.into_iter()
		.map(|(mod_id, precedence, patches)| {
			let patches = patches
				.into_iter()
				.map(|patch| match patch {
					ClausewitzPatch::InsertNode {
						path,
						key,
						statement: AstStatement::Assignment { value, .. },
					} if list_addresses.contains(&(path.clone(), key.clone())) => {
						ClausewitzPatch::AppendListItem {
							path,
							key,
							value,
							target_occurrence: ListItemTarget::new(0, 0, 0),
						}
					}
					other => other,
				})
				.collect();
			(mod_id, precedence, patches)
		})
		.collect()
}

const CROSS_MOD_INSERT_RENAME_THRESHOLD: f32 = 0.70;

fn drop_prefixed_rename_duplicate_inserts(
	mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
) -> Vec<(String, usize, Vec<ClausewitzPatch>)> {
	let mut duplicate_losers: HashSet<usize> = HashSet::new();
	for left_idx in 0..mod_patches.len() {
		let Some(left) = single_root_insert(&mod_patches[left_idx].2) else {
			continue;
		};
		for right_idx in (left_idx + 1)..mod_patches.len() {
			let Some(right) = single_root_insert(&mod_patches[right_idx].2) else {
				continue;
			};
			if !prefixed_rename_insert_pair(left, right) {
				continue;
			}
			let loser = lower_precedence_insert(
				(left_idx, &mod_patches[left_idx].0, mod_patches[left_idx].1),
				(
					right_idx,
					&mod_patches[right_idx].0,
					mod_patches[right_idx].1,
				),
			);
			duplicate_losers.insert(loser);
		}
	}

	if duplicate_losers.is_empty() {
		return mod_patches;
	}

	mod_patches
		.into_iter()
		.enumerate()
		.map(|(idx, (mod_id, precedence, patches))| {
			if duplicate_losers.contains(&idx) {
				(mod_id, precedence, Vec::new())
			} else {
				(mod_id, precedence, patches)
			}
		})
		.collect()
}

fn single_root_insert(patches: &[ClausewitzPatch]) -> Option<(&str, &AstStatement)> {
	let [
		ClausewitzPatch::InsertNode {
			path,
			key,
			statement,
		},
	] = patches
	else {
		return None;
	};
	if path.is_empty() {
		Some((key, statement))
	} else {
		None
	}
}

fn prefixed_rename_insert_pair(left: (&str, &AstStatement), right: (&str, &AstStatement)) -> bool {
	let (left_key, left_stmt) = left;
	let (right_key, right_stmt) = right;
	if left_key == right_key || !prefixed_key_pair(left_key, right_key) {
		return false;
	}
	let (Some(left_value), Some(right_value)) = (
		assignment_statement_value(left_stmt),
		assignment_statement_value(right_stmt),
	) else {
		return false;
	};
	fuzzy_rename_similarity(left_value, right_value)
		.is_some_and(|score| score >= CROSS_MOD_INSERT_RENAME_THRESHOLD)
}

fn prefixed_key_pair(left: &str, right: &str) -> bool {
	left.contains(right) || right.contains(left)
}

fn assignment_statement_value(stmt: &AstStatement) -> Option<&AstValue> {
	match stmt {
		AstStatement::Assignment { value, .. } => Some(value),
		_ => None,
	}
}

fn lower_precedence_insert(left: (usize, &str, usize), right: (usize, &str, usize)) -> usize {
	let (left_idx, left_mod, left_precedence) = left;
	let (right_idx, right_mod, right_precedence) = right;
	if (left_precedence, left_mod) < (right_precedence, right_mod) {
		left_idx
	} else {
		right_idx
	}
}

// ---------------------------------------------------------------------------
// Per-address resolution
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Child modules
// ---------------------------------------------------------------------------

mod block_merge;
pub(crate) use block_merge::order_patches_by_source;
#[cfg(test)]
pub(crate) use block_merge::{
	ast_equal_ignoring_spans, child_identity, items_are_named_container,
	merge_named_container_bodies, rename_for_conflict,
};

mod fingerprint;

mod rename;
use rename::{build_rename_map, rewrite_patch_for_renames};

#[cfg(test)]
mod tests;
