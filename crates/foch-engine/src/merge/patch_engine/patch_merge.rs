// Patch set merging: given N mods' patch sets against a common base, merge
// them into a single resolved patch set with conflict detection.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::content_family::MergePolicies;
#[cfg(test)]
use foch_language::analyzer::content_family::{BlockPatchPolicy, ScalarMergePolicy};
#[cfg(test)]
use foch_language::analyzer::content_family::{MergeKeySource, NamedContainerPolicy};
use foch_language::analyzer::parser::{AstStatement, AstValue};

#[cfg(test)]
use super::super::conflict_handler::ConflictDecision;
use super::super::conflict_handler::ConflictHandler;
#[cfg(test)]
use super::super::conflict_handler::DeferHandler;
use super::super::error::MergeError;
use super::patch::{AstPath, ClausewitzPatch, fuzzy_rename_similarity};

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
	mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	policies: &MergePolicies,
	handler: &mut dyn ConflictHandler,
	current_file: Option<&Path>,
) -> Result<PatchMergeResult, MergeError> {
	let mut result = PatchMergeResult::default();

	// --- Pre-pass: collect renames and rewrite cross-mod addresses ---
	//
	// For each `Rename { path, old_key, new_key }` emitted by any mod, every
	// other mod's patches whose `(path, key)` match — or whose path traverses
	// `old_key` at that location — must be rewritten so they target the new
	// key instead. Otherwise the renaming mod's RemoveNode would conflict
	// with the modifier mod's edits at the old key.
	let rename_map = build_rename_map(&mod_patches);
	let mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)> = mod_patches
		.into_iter()
		.map(|(mod_id, prec, patches)| {
			let rewritten = patches
				.into_iter()
				.map(|p| rewrite_patch_for_renames(p, &rename_map))
				.collect();
			(mod_id, prec, rewritten)
		})
		.collect();
	let mod_patches = drop_prefixed_rename_duplicate_inserts(mod_patches);

	// Group patches by address, preserving attribution.
	let mut by_address: HashMap<PatchAddress, Vec<AttributedPatch>> = HashMap::new();

	for (mod_id, precedence, patches) in mod_patches {
		for patch in patches {
			result.stats.total_patches += 1;
			let addr = patch_address(&patch, policies);
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
	let cross_kind_conflicts = detect_cross_kind_sibling_conflicts(&by_address, &mut result.stats);
	let cross_kind_addresses: HashSet<PatchAddress> = cross_kind_conflicts
		.iter()
		.flat_map(|conflict| conflict.split_addresses.iter().cloned())
		.collect();
	for addr in &cross_kind_addresses {
		by_address.remove(addr);
	}

	let mut pending_resolutions = Vec::new();
	for (addr, attributed) in by_address {
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
