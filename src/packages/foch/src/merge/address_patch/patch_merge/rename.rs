use std::collections::HashMap;

use super::super::patch::{AstPath, ClausewitzPatch};
use super::{AttributedPatch, PatchAddress, PatchMergeStats, PatchResolution};

pub(super) fn build_rename_map(
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
) -> HashMap<(AstPath, String), String> {
	let mut candidate: HashMap<(AstPath, String), Vec<String>> = HashMap::new();
	for (_mod_id, _prec, patches) in mod_patches {
		for p in patches {
			if let ClausewitzPatch::Rename {
				path,
				old_key,
				new_key,
			} = p
			{
				candidate
					.entry((path.clone(), old_key.clone()))
					.or_default()
					.push(new_key.clone());
			}
		}
	}
	candidate
		.into_iter()
		.filter_map(|(k, news)| {
			let first = news.first().cloned()?;
			if news.iter().all(|n| n == &first) {
				Some((k, first))
			} else {
				None
			}
		})
		.collect()
}

pub(super) fn rewrite_patch_for_renames(
	mut patch: ClausewitzPatch,
	rename_map: &HashMap<(AstPath, String), String>,
) -> ClausewitzPatch {
	if matches!(patch, ClausewitzPatch::Rename { .. }) {
		return patch;
	}
	if rename_map.is_empty() {
		return patch;
	}
	let original = patch.clone();
	let mut seen: std::collections::HashSet<AstPath> = std::collections::HashSet::new();
	loop {
		let path = rn_patch_path_clone(&patch);
		if !seen.insert(path.clone()) {
			// Cyclic rename graph (e.g. A→B and B→A at the same prefix) would otherwise loop forever.
			return original;
		}
		let mut changed = false;
		for split in 0..path.len() {
			let prefix = path[..split].to_vec();
			let seg = path[split].clone();
			if let Some(new_key) = rename_map.get(&(prefix, seg)) {
				rn_replace_path_segment(&mut patch, split, new_key.clone());
				changed = true;
				break;
			}
		}
		if !changed {
			break;
		}
	}
	if let Some(k) = rn_patch_key(&patch).map(|s| s.to_string()) {
		let p = rn_patch_path_clone(&patch);
		if let Some(new_key) = rename_map.get(&(p, k)) {
			rn_set_patch_key(&mut patch, new_key.clone());
		}
	}
	patch
}

fn rn_patch_path_clone(p: &ClausewitzPatch) -> AstPath {
	match p {
		ClausewitzPatch::SetValue { path, .. }
		| ClausewitzPatch::RemoveNode { path, .. }
		| ClausewitzPatch::InsertNode { path, .. }
		| ClausewitzPatch::AppendListItem { path, .. }
		| ClausewitzPatch::RemoveListItem { path, .. }
		| ClausewitzPatch::ReplaceBlock { path, .. }
		| ClausewitzPatch::AppendBlockItem { path, .. }
		| ClausewitzPatch::RemoveBlockItem { path, .. }
		| ClausewitzPatch::Rename { path, .. } => path.clone(),
	}
}

fn rn_replace_path_segment(p: &mut ClausewitzPatch, idx: usize, new_seg: String) {
	let path = match p {
		ClausewitzPatch::SetValue { path, .. }
		| ClausewitzPatch::RemoveNode { path, .. }
		| ClausewitzPatch::InsertNode { path, .. }
		| ClausewitzPatch::AppendListItem { path, .. }
		| ClausewitzPatch::RemoveListItem { path, .. }
		| ClausewitzPatch::ReplaceBlock { path, .. }
		| ClausewitzPatch::AppendBlockItem { path, .. }
		| ClausewitzPatch::RemoveBlockItem { path, .. }
		| ClausewitzPatch::Rename { path, .. } => path,
	};
	path[idx] = new_seg;
}

fn rn_patch_key(p: &ClausewitzPatch) -> Option<&str> {
	match p {
		ClausewitzPatch::SetValue { key, .. }
		| ClausewitzPatch::RemoveNode { key, .. }
		| ClausewitzPatch::InsertNode { key, .. }
		| ClausewitzPatch::AppendListItem { key, .. }
		| ClausewitzPatch::RemoveListItem { key, .. }
		| ClausewitzPatch::ReplaceBlock { key, .. } => Some(key),
		ClausewitzPatch::AppendBlockItem { .. }
		| ClausewitzPatch::RemoveBlockItem { .. }
		| ClausewitzPatch::Rename { .. } => None,
	}
}

fn rn_set_patch_key(p: &mut ClausewitzPatch, new: String) {
	match p {
		ClausewitzPatch::SetValue { key, .. }
		| ClausewitzPatch::RemoveNode { key, .. }
		| ClausewitzPatch::InsertNode { key, .. }
		| ClausewitzPatch::AppendListItem { key, .. }
		| ClausewitzPatch::RemoveListItem { key, .. }
		| ClausewitzPatch::ReplaceBlock { key, .. } => *key = new,
		_ => {}
	}
}

/// Multiple mods renaming the same `(path, old_key)`. Convergent if all pick
/// the same `new_key`; conflict otherwise (we will not silently pick one —
/// that risks corrupting whichever mod's downstream patches were rewritten
/// to the *other* `new_key`).
pub(super) fn resolve_renames(
	addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	let new_keys: Vec<String> = attributed
		.iter()
		.map(|a| match &a.patch {
			ClausewitzPatch::Rename { new_key, .. } => new_key.clone(),
			_ => unreachable!(),
		})
		.collect();
	let first = new_keys[0].clone();
	if new_keys.iter().all(|k| k == &first) {
		stats.convergent_patches += 1;
		return PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch);
	}
	stats.conflict_patches += 1;
	PatchResolution::Conflict {
		address: addr,
		reason: format!(
			"conflicting renames for same key: {}",
			new_keys.join(" vs ")
		),
		patches: attributed,
	}
}
