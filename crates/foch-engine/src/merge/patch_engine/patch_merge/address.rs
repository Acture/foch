use foch_language::analyzer::content_family::{BlockPatchPolicy, MergePolicies};

use super::super::patch::{AstPath, ClausewitzPatch};
use super::PatchAddress;
use super::fingerprint::{statement_fingerprint, value_fingerprint};

pub(super) fn patch_address(patch: &ClausewitzPatch, policies: &MergePolicies) -> PatchAddress {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. } => PatchAddress {
			path: path.clone(),
			key: key.clone(),
		},
		ClausewitzPatch::RemoveNode {
			path, key, removed, ..
		} => {
			// Fingerprint InsertNode / RemoveNode bodies only when the target
			// block's policy explicitly opts in to list-like coexistence
			// (Union). For Recurse / LastWriter the top-level key is
			// unique-by-convention and sibling mods touching the same key
			// must collide so the leaf resolvers can surface a conflict
			// instead of silently allowing N divergent values to coexist.
			// BooleanOr also keeps no fingerprint so synthesis can fold
			// bodies into a single OR block at the same address.
			let fingerprint_nodes = matches!(
				policies.block_patch_policy_for_key(key),
				BlockPatchPolicy::Union
			);
			let key = if fingerprint_nodes {
				format!("__node__::{}::{}", key, statement_fingerprint(removed))
			} else {
				key.clone()
			};
			PatchAddress {
				path: path.clone(),
				key,
			}
		}
		ClausewitzPatch::InsertNode {
			path,
			key,
			statement,
		} => {
			let fingerprint_nodes = matches!(
				policies.block_patch_policy_for_key(key),
				BlockPatchPolicy::Union
			);
			let key = if fingerprint_nodes {
				format!("__node__::{}::{}", key, statement_fingerprint(statement))
			} else {
				key.clone()
			};
			PatchAddress {
				path: path.clone(),
				key,
			}
		}
		ClausewitzPatch::AppendListItem { path, key, value } => PatchAddress {
			path: path.clone(),
			key: format!("__list_item__::{}::{}", key, value_fingerprint(value)),
		},
		ClausewitzPatch::RemoveListItem { path, key, value } => PatchAddress {
			path: path.clone(),
			key: format!("__list_item__::{}::{}", key, value_fingerprint(value)),
		},
		ClausewitzPatch::ReplaceBlock { path, key, .. } => PatchAddress {
			path: path.clone(),
			key: key.clone(),
		},
		ClausewitzPatch::AppendBlockItem { path, value } => PatchAddress {
			path: path.clone(),
			key: format!("__append_block_item__::{}", value_fingerprint(value)),
		},
		ClausewitzPatch::RemoveBlockItem { path, value } => PatchAddress {
			path: path.clone(),
			key: format!("__remove_block_item__::{}", value_fingerprint(value)),
		},
		ClausewitzPatch::Rename { path, old_key, .. } => PatchAddress {
			path: path.clone(),
			key: format!("__rename__::{old_key}"),
		},
	}
}

/// "Raw" address used to detect cross-kind sibling conflicts at the same
/// `(path, key)`. Unlike `patch_address`, this never fingerprints — so two
/// patches of *different* kinds (e.g. `SetValue(owner)` and `RemoveNode(owner)`)
/// produced by sibling mods land in the same group and can be reported as a
/// single mixed-kinds conflict.
///
/// Returns `None` for kinds that target a value rather than a named child
/// (`AppendListItem`, `RemoveListItem`, `AppendBlockItem`, `RemoveBlockItem`)
/// or that operate on a different conceptual axis (`Rename`). Cross-kind
/// detection is restricted to the four "named-key replacement" variants.
pub(super) fn patch_raw_address(patch: &ClausewitzPatch) -> Option<(AstPath, String)> {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. }
		| ClausewitzPatch::RemoveNode { path, key, .. }
		| ClausewitzPatch::InsertNode { path, key, .. }
		| ClausewitzPatch::ReplaceBlock { path, key, .. } => Some((path.clone(), key.clone())),
		_ => None,
	}
}

/// Discriminant tag for patch variant, used to detect mixed-kind conflicts.
pub(super) fn patch_kind(patch: &ClausewitzPatch) -> &'static str {
	match patch {
		ClausewitzPatch::SetValue { .. } => "SetValue",
		ClausewitzPatch::RemoveNode { .. } => "RemoveNode",
		ClausewitzPatch::InsertNode { .. } => "InsertNode",
		ClausewitzPatch::AppendListItem { .. } => "AppendListItem",
		ClausewitzPatch::RemoveListItem { .. } => "RemoveListItem",
		ClausewitzPatch::ReplaceBlock { .. } => "ReplaceBlock",
		ClausewitzPatch::AppendBlockItem { .. } => "AppendBlockItem",
		ClausewitzPatch::RemoveBlockItem { .. } => "RemoveBlockItem",
		ClausewitzPatch::Rename { .. } => "Rename",
	}
}
