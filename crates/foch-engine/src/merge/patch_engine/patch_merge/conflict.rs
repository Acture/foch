use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use foch_core::config::compute_conflict_id;

use super::super::patch::AstPath;
use super::address::{patch_kind, patch_raw_address};
use super::{
	AttributedPatch, PatchAddress, PatchConflict, PatchMergeResult, PatchMergeStats,
	PatchResolution,
};
use crate::merge::conflict_handler::{ConflictDecision, ConflictHandler};
use crate::merge::conflict_view::build_decision_conflict_view;
use crate::merge::error::MergeError;

/// Cross-kind sibling conflict detected before per-address dispatch.
///
/// `split_addresses` lists every fingerprinted `PatchAddress` whose patches
/// fed into this conflict — the caller drops them from the per-address map so
/// they aren't double-resolved alongside the synthesized conflict.
pub(super) struct CrossKindConflict {
	pub(super) address: PatchAddress,
	pub(super) patches: Vec<AttributedPatch>,
	pub(super) reason: String,
	pub(super) split_addresses: Vec<PatchAddress>,
}

pub(super) fn detect_cross_kind_sibling_conflicts(
	by_address: &HashMap<PatchAddress, Vec<AttributedPatch>>,
	stats: &mut PatchMergeStats,
) -> Vec<CrossKindConflict> {
	// Group fingerprinted addresses by their underlying raw (path, key).
	let mut by_raw: HashMap<(AstPath, String), Vec<&PatchAddress>> = HashMap::new();
	for addr in by_address.keys() {
		let Some(first) = by_address.get(addr).and_then(|patches| patches.first()) else {
			continue;
		};
		let Some(raw) = patch_raw_address(&first.patch) else {
			continue;
		};
		by_raw.entry(raw).or_default().push(addr);
	}

	let mut conflicts = Vec::new();
	for ((path, key), addrs) in by_raw {
		if addrs.len() < 2 {
			continue;
		}

		let mut kinds: HashSet<&'static str> = HashSet::new();
		let mut contributors: HashSet<&str> = HashSet::new();
		for addr in &addrs {
			for patch in by_address.get(*addr).into_iter().flatten() {
				kinds.insert(patch_kind(&patch.patch));
				contributors.insert(patch.mod_id.as_str());
			}
		}

		// Multiple kinds at the same (path, key) from sibling mods → ambiguous;
		// must escalate to a real conflict instead of silently applying both.
		if kinds.len() > 1 && contributors.len() > 1 {
			let mut combined: Vec<AttributedPatch> = addrs
				.iter()
				.flat_map(|a| by_address.get(*a).cloned().unwrap_or_default())
				.collect();
			combined.sort_by(|a, b| {
				a.precedence
					.cmp(&b.precedence)
					.then_with(|| a.mod_id.cmp(&b.mod_id))
			});
			let mut kind_list: Vec<&str> = kinds.iter().copied().collect();
			kind_list.sort_unstable();
			stats.conflict_patches += 1;
			conflicts.push(CrossKindConflict {
				address: PatchAddress {
					path: path.clone(),
					key: key.clone(),
				},
				patches: combined,
				reason: format!(
					"sibling mods produced incompatible patch kinds at the same key: {}",
					kind_list.join(", ")
				),
				split_addresses: addrs.into_iter().cloned().collect(),
			});
		}
	}

	conflicts
}

pub(super) fn apply_conflict_decision(
	result: &mut PatchMergeResult,
	handler: &mut dyn ConflictHandler,
	current_file: Option<&Path>,
	address: PatchAddress,
	patches: Vec<AttributedPatch>,
	reason: String,
) -> Result<(), MergeError> {
	let conflict_path = conflict_path_for_handler(&address);
	let fallback_file = PathBuf::from(&conflict_path);
	let conflict_file = current_file.unwrap_or(&fallback_file);
	let conflict_id = compute_conflict_id(conflict_file, &address.path.join("/"), &address.key);
	let conflict = PatchConflict { patches, reason };
	let view = build_decision_conflict_view(
		conflict_file,
		&address,
		&conflict,
		conflict_id,
		&HashMap::new(),
	);

	match handler.on_conflict(&view) {
		ConflictDecision::Defer { record } => {
			if let Some(record) = record {
				result.handler_resolutions.push(record);
			}
			result.conflicts.push(PatchResolution::Conflict {
				address,
				patches: conflict.patches,
				reason: conflict.reason,
			});
		}
		ConflictDecision::PickMod { mod_id, record } => {
			let Some(chosen) = conflict
				.patches
				.iter()
				.find(|patch| patch.mod_id == mod_id)
				.cloned()
			else {
				// Stale pick: the conflict_id matches an earlier resolution
				// whose target mod is no longer a contributor at this address
				// (typical after a prior pick reshapes the parent block).
				// Defer instead of erroring so the user can re-arbitrate on
				// the next interactive pass; the surviving conflict still
				// surfaces in the report.
				eprintln!(
					"[foch] stale pick for {conflict_path}: mod `{mod_id}` is no longer a contributor; deferring"
				);
				result.conflicts.push(PatchResolution::Conflict {
					address,
					patches: conflict.patches,
					reason: conflict.reason,
				});
				return Ok(());
			};
			result.handler_resolved_count += 1;
			if let Some(record) = record {
				result.handler_resolutions.push(record);
			}
			result
				.resolved
				.push(PatchResolution::Resolved(chosen.patch));
		}
		ConflictDecision::UseFile(source_path) => {
			result.handler_resolved_count += 1;
			// Inline use_file is a whole-file materialization decision. Key it by
			// the real target file so write_patch_merge_output can honor it; the
			// old synthetic AST key was unreachable by the materializer.
			result
				.external_file_resolutions
				.insert(conflict_file.to_path_buf(), source_path);
		}
		ConflictDecision::KeepExisting => {
			result.handler_resolved_count += 1;
			// Same whole-file keying as use_file: the output writer checks by
			// target path, not by a synthetic conflict address.
			result
				.keep_existing_paths
				.insert(conflict_file.to_path_buf());
		}
		ConflictDecision::Abort => {
			return Err(MergeError::Validation {
				path: Some(conflict_path),
				message: format!("conflict handler aborted merge: {}", conflict.reason),
			});
		}
	}

	Ok(())
}

fn conflict_path_for_handler(address: &PatchAddress) -> String {
	if address.path.is_empty() {
		return address.key.clone();
	}

	let mut path = address.path.join("/");
	if !address.key.is_empty() {
		path.push('/');
		path.push_str(&address.key);
	}
	path
}
