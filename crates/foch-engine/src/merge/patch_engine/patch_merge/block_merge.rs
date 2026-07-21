use std::collections::{HashMap, HashSet, VecDeque};

use foch_language::analyzer::content_family::{
	MergeKeySource, MergePolicies, NamedContainerPolicy,
};
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};

use crate::merge::boolean::combine_boolean_or_bodies;

use super::super::super::conflict_handler::DeferHandler;
use super::super::patch::{AstPath, ClausewitzPatch, ast_statements_semantically_equal};
use super::address::patch_address;
use super::fingerprint::{statement_fingerprint, value_fingerprint};
use super::{
	AttributedPatch, PatchAddress, PatchMergeStats, PatchResolution, merge_patch_sets,
	patch_sort_key,
};

fn sort_recursive_candidates(candidates: &mut [(String, usize, Vec<ClausewitzPatch>)]) {
	candidates.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
}

#[derive(Hash, Eq, PartialEq)]
enum RecursiveSourceIdentity {
	Statement(AstPath, String, String),
	AssignmentValue(AstPath, String, String),
	ListOccurrence(AstPath, String, String, usize),
	BlockItem(AstPath, String),
	AssignmentKey(AstPath, String),
}

fn collect_recursive_source_positions(
	statements: &[AstStatement],
	path: &[String],
	next_position: &mut usize,
	positions: &mut HashMap<RecursiveSourceIdentity, VecDeque<usize>>,
	list_occurrences: &mut HashMap<(AstPath, String, String), usize>,
) {
	for statement in statements {
		match statement {
			AstStatement::Assignment { key, value, .. } => {
				let position = *next_position;
				*next_position += 1;
				let value_identity = value_fingerprint(value);
				let occurrence_key = (path.to_vec(), key.clone(), value_identity.clone());
				let occurrence = list_occurrences.entry(occurrence_key).or_default();
				positions
					.entry(RecursiveSourceIdentity::ListOccurrence(
						path.to_vec(),
						key.clone(),
						value_identity,
						*occurrence,
					))
					.or_default()
					.push_back(position);
				*occurrence += 1;
				positions
					.entry(RecursiveSourceIdentity::Statement(
						path.to_vec(),
						key.clone(),
						statement_fingerprint(statement),
					))
					.or_default()
					.push_back(position);
				positions
					.entry(RecursiveSourceIdentity::AssignmentValue(
						path.to_vec(),
						key.clone(),
						value_fingerprint(value),
					))
					.or_default()
					.push_back(position);
				positions
					.entry(RecursiveSourceIdentity::AssignmentKey(
						path.to_vec(),
						key.clone(),
					))
					.or_default()
					.push_back(position);
				if let AstValue::Block { items, .. } = value {
					let mut child_path = path.to_vec();
					child_path.push(key.clone());
					collect_recursive_source_positions(
						items,
						&child_path,
						next_position,
						positions,
						list_occurrences,
					);
				}
			}
			AstStatement::Item { value, .. } => {
				let position = *next_position;
				*next_position += 1;
				positions
					.entry(RecursiveSourceIdentity::BlockItem(
						path.to_vec(),
						value_fingerprint(value),
					))
					.or_default()
					.push_back(position);
			}
			AstStatement::Comment { .. } => {}
		}
	}
}

fn recursive_patch_source_position(
	patch: &ClausewitzPatch,
	positions: &mut HashMap<RecursiveSourceIdentity, VecDeque<usize>>,
) -> Option<usize> {
	let identity = match patch {
		ClausewitzPatch::InsertNode {
			path,
			key,
			statement,
		}
		| ClausewitzPatch::ReplaceBlock {
			path,
			key,
			new_statement: statement,
			..
		} => RecursiveSourceIdentity::Statement(
			path.clone(),
			key.clone(),
			statement_fingerprint(statement),
		),
		ClausewitzPatch::SetValue {
			path,
			key,
			new_value,
			..
		} => RecursiveSourceIdentity::AssignmentValue(
			path.clone(),
			key.clone(),
			value_fingerprint(new_value),
		),
		ClausewitzPatch::AppendListItem {
			path,
			key,
			value: new_value,
			target_occurrence,
		} => RecursiveSourceIdentity::ListOccurrence(
			path.clone(),
			key.clone(),
			value_fingerprint(new_value),
			target_occurrence.identity_ordinal(),
		),
		ClausewitzPatch::AppendBlockItem { path, value } => {
			RecursiveSourceIdentity::BlockItem(path.clone(), value_fingerprint(value))
		}
		ClausewitzPatch::Rename { path, new_key, .. } => {
			RecursiveSourceIdentity::AssignmentKey(path.clone(), new_key.clone())
		}
		ClausewitzPatch::RemoveNode { .. }
		| ClausewitzPatch::RemoveListItem { .. }
		| ClausewitzPatch::RemoveBlockItem { .. } => return None,
	};
	positions.get_mut(&identity)?.pop_front()
}

pub(crate) fn order_patches_by_source(
	patches: &mut Vec<ClausewitzPatch>,
	base_body: &[AstStatement],
	overlay_body: &[AstStatement],
) {
	let mut positions = HashMap::new();
	let mut position_occurrences = HashMap::new();
	let mut next_position = 0;
	collect_recursive_source_positions(
		overlay_body,
		&[],
		&mut next_position,
		&mut positions,
		&mut position_occurrences,
	);
	let mut consumed = HashMap::new();
	let mut consumed_occurrences = HashMap::new();
	let mut next_consumed_position = 0;
	collect_recursive_source_positions(
		base_body,
		&[],
		&mut next_consumed_position,
		&mut consumed,
		&mut consumed_occurrences,
	);
	for (identity, occurrences) in consumed {
		if matches!(&identity, RecursiveSourceIdentity::ListOccurrence(..)) {
			continue;
		}
		let Some(remaining) = positions.get_mut(&identity) else {
			continue;
		};
		for _ in 0..occurrences.len() {
			remaining.pop_front();
		}
	}
	let mut indexed = patches
		.drain(..)
		.enumerate()
		.map(|(original_index, patch)| {
			let source_position = recursive_patch_source_position(&patch, &mut positions);
			(original_index, source_position, patch)
		})
		.collect::<Vec<_>>();
	indexed.sort_by_key(|(original_index, source_position, _)| {
		(
			source_position.is_some(),
			source_position.unwrap_or(*original_index),
			*original_index,
		)
	});
	patches.extend(indexed.into_iter().map(|(_, _, patch)| patch));
}

fn recursive_patch_origins(
	candidates: &[(String, usize, Vec<ClausewitzPatch>)],
	policies: &MergePolicies,
) -> HashMap<PatchAddress, (usize, String, usize)> {
	let mut origins = HashMap::new();
	for (mod_id, precedence, patches) in candidates {
		for (source_ordinal, patch) in patches.iter().enumerate() {
			let candidate = (*precedence, mod_id.clone(), source_ordinal);
			origins
				.entry(patch_address(patch, policies))
				.and_modify(|known: &mut (usize, String, usize)| {
					if &candidate < known {
						known.clone_from(&candidate);
					}
				})
				.or_insert(candidate);
		}
	}
	origins
}

fn sort_recursive_application(
	patches: &mut [ClausewitzPatch],
	origins: &HashMap<PatchAddress, (usize, String, usize)>,
	policies: &MergePolicies,
) {
	patches.sort_by(|left, right| {
		let left_origin = origins
			.get(&patch_address(left, policies))
			.cloned()
			.unwrap_or((usize::MAX, String::new(), usize::MAX));
		let right_origin = origins
			.get(&patch_address(right, policies))
			.cloned()
			.unwrap_or((usize::MAX, String::new(), usize::MAX));
		left_origin
			.cmp(&right_origin)
			.then_with(|| patch_sort_key(left).cmp(&patch_sort_key(right)))
	});
}

/// Attempt to union list-like block replacements by keeping the base body's
/// first occurrence of each item, then appending unique items from every
/// replacement body in precedence order.
pub(super) fn try_union_block_merge(attributed: &[AttributedPatch]) -> Option<ClausewitzPatch> {
	if attributed.len() < 2 {
		return None;
	}

	let mut replacements: Vec<(String, usize, &AstStatement, &AstStatement, AstPath, String)> =
		Vec::with_capacity(attributed.len());
	for a in attributed {
		match &a.patch {
			ClausewitzPatch::ReplaceBlock {
				old_statement,
				new_statement,
				path,
				key,
			} => replacements.push((
				a.mod_id.clone(),
				a.precedence,
				old_statement,
				new_statement,
				path.clone(),
				key.clone(),
			)),
			_ => return None,
		}
	}
	let ancestor_idx = replacements
		.iter()
		.enumerate()
		.min_by_key(|(_, (_, prec, _, _, _, _))| *prec)
		.map(|(i, _)| i)?;
	let ancestor_body = statement_block_body(replacements[ancestor_idx].2)?;

	let mut seen: HashSet<String> = HashSet::new();
	let mut union_body: Vec<AstStatement> = Vec::new();
	push_unique_block_items(ancestor_body, &mut seen, &mut union_body);

	replacements.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
	for (_, _, _, new_statement, _, _) in &replacements {
		let body = statement_block_body(new_statement)?;
		push_unique_block_items(body, &mut seen, &mut union_body);
	}

	let representative = replacements
		.iter()
		.max_by_key(|(_, prec, _, _, _, _)| *prec)
		.unwrap();
	Some(ClausewitzPatch::ReplaceBlock {
		path: representative.4.clone(),
		key: representative.5.clone(),
		old_statement: representative.2.clone(),
		new_statement: with_block_body(representative.3, union_body),
	})
}

fn push_unique_block_items(
	items: &[AstStatement],
	seen: &mut HashSet<String>,
	out: &mut Vec<AstStatement>,
) {
	for item in items {
		let fingerprint = union_item_fingerprint(item);
		if seen.insert(fingerprint) {
			out.push(item.clone());
		}
	}
}

fn union_item_fingerprint(item: &AstStatement) -> String {
	statement_fingerprint(item)
}

/// Attempt to deep-merge multiple mods' `ReplaceBlock` patches at the same
/// address by re-running the diff/merge pipeline against the bodies. Used by
/// `BlockPatchPolicy::Recurse` to handle date-keyed history blocks where each
/// mod typically modifies a different field inside the same date container.
///
/// Returns:
/// - `Some(AutoMerged)` when nested resolution is fully clean
/// - `Some(Conflict)` when nested resolution surfaces sub-conflicts (the
///   original block-level address is preserved with sub-conflict reasons)
/// - `None` when the heuristic does not apply (e.g. patches are not all
///   `ReplaceBlock` with a common base, or bodies are not blocks)
pub(super) fn try_recursive_block_merge(
	addr: &PatchAddress,
	attributed: &[AttributedPatch],
	policies: &MergePolicies,
	stats: &mut PatchMergeStats,
) -> Option<PatchResolution> {
	if attributed.len() < 2 {
		return None;
	}

	// All patches must be ReplaceBlock. Each mod's `old_statement` is its
	// diff base — for chained diffs against playlist predecessors these
	// differ across mods. The lowest-precedence mod's `old_statement` is
	// the closest available approximation of the common ancestor (it
	// diffed against base game / synthetic base directly).
	let mut overlays: Vec<(String, usize, &AstStatement, &AstStatement, AstPath, String)> =
		Vec::with_capacity(attributed.len());
	for a in attributed {
		match &a.patch {
			ClausewitzPatch::ReplaceBlock {
				old_statement,
				new_statement,
				path,
				key,
			} => overlays.push((
				a.mod_id.clone(),
				a.precedence,
				old_statement,
				new_statement,
				path.clone(),
				key.clone(),
			)),
			_ => return None,
		}
	}
	overlays.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));

	// Pick the lowest-precedence mod as the ancestor source. Its `old`
	// reflects the deepest base reachable from this address.
	let ancestor_idx = overlays
		.iter()
		.enumerate()
		.min_by_key(|(_, t)| t.1)
		.map(|(i, _)| i)?;
	let ancestor_stmt: &AstStatement = overlays[ancestor_idx].2;
	let ancestor_body = statement_block_body(ancestor_stmt)?;

	// Re-derive each mod's intent against the common ancestor by diffing
	// the ancestor body against the mod's `new` body. This avoids leaking
	// chained-predecessor edits into a mod's apparent intent.
	let mut mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)> =
		Vec::with_capacity(overlays.len());
	for (mod_id, prec, _old, new_stmt, _path, _key) in &overlays {
		let new_body = statement_block_body(new_stmt)?;
		let mut patches = super::super::patch::diff_block_bodies(
			ancestor_body,
			new_body,
			&[],
			0,
			policies.nested_merge_key_source,
		);
		order_patches_by_source(&mut patches, ancestor_body, new_body);
		mod_patches.push((mod_id.clone(), *prec, patches));
	}

	// Recursively resolve nested patches with the same policies.
	sort_recursive_candidates(&mut mod_patches);
	let patch_origins = recursive_patch_origins(&mod_patches, policies);
	let mut handler = DeferHandler;
	let nested = merge_patch_sets(mod_patches, policies, &mut handler).ok()?;
	stats.accumulate(&nested.stats);

	if !nested.conflicts.is_empty() {
		// Bubble up as a single conflict with detailed sub-reasons so users
		// can see exactly which fields inside the date block diverged.
		let reasons: Vec<String> = nested
			.conflicts
			.iter()
			.filter_map(|c| match c {
				PatchResolution::Conflict {
					address, reason, ..
				} => Some(format!("{}: {}", address.key, reason)),
				_ => None,
			})
			.collect();
		stats.conflict_patches += 1;
		return Some(PatchResolution::Conflict {
			address: addr.clone(),
			reason: format!(
				"deep merge of replaced block has {} unresolved sub-conflict(s): {}",
				nested.conflicts.len(),
				reasons.join("; ")
			),
			patches: attributed.to_vec(),
		});
	}

	// Apply resolved nested patches to the base body to synthesize the merged
	// body. Use `apply_patches` from `patch_apply` (paths are relative).
	let mut resolved_patches: Vec<ClausewitzPatch> = nested
		.resolved
		.into_iter()
		.filter_map(|r| match r {
			PatchResolution::Resolved(p) => Some(p),
			PatchResolution::AutoMerged { result, .. } => Some(result),
			PatchResolution::Conflict { .. } => None,
		})
		.collect();
	sort_recursive_application(&mut resolved_patches, &patch_origins, policies);

	let merged_body = super::super::patch_apply::apply_patches(
		ancestor_body,
		&resolved_patches,
		policies.nested_merge_key_source,
	);
	let merged_stmt = with_block_body(ancestor_stmt, merged_body);

	// Use the highest-precedence patch's (path, key) as the representative.
	// Preserve the highest-precedence mod's `old_statement` so downstream
	// `apply_patches` finds the same base it expects.
	let representative = overlays
		.iter()
		.max_by_key(|(_, prec, _, _, _, _)| *prec)
		.unwrap();
	let path = representative.4.clone();
	let key = representative.5.clone();
	let representative_old = representative.2.clone();

	let mods: Vec<String> = overlays.iter().map(|overlay| overlay.0.clone()).collect();
	stats.auto_merged_patches += 1;
	let _ = policies; // silence unused warnings if added later
	Some(PatchResolution::AutoMerged {
		result: ClausewitzPatch::ReplaceBlock {
			path,
			key,
			old_statement: representative_old,
			new_statement: merged_stmt,
		},
		strategy: "recursive_block_merge".to_string(),
		contributing_mods: mods,
	})
}

/// Merge a block removal against sibling replacements by expanding both
/// branches relative to the common block body. This is intentionally limited
/// to replacements that do not edit an existing child: additions and removals
/// can be combined structurally, while a genuine child edit is left to the
/// family's explicit edit-vs-remove policy.
struct RemoveReplaceBranch<'a> {
	mod_id: String,
	precedence: usize,
	base: &'a AstStatement,
	target: Option<&'a AstStatement>,
	path: AstPath,
	key: String,
}

pub(super) fn try_recursive_remove_replace_merge(
	addr: &PatchAddress,
	attributed: &[AttributedPatch],
	policies: &MergePolicies,
	stats: &mut PatchMergeStats,
) -> Option<PatchResolution> {
	if attributed.len() < 2 {
		return None;
	}

	let mut branches = Vec::with_capacity(attributed.len());
	let mut replacement_count = 0;
	let mut removal_count = 0;
	for attributed_patch in attributed {
		match &attributed_patch.patch {
			ClausewitzPatch::RemoveNode { path, key, removed } => {
				removal_count += 1;
				branches.push(RemoveReplaceBranch {
					mod_id: attributed_patch.mod_id.clone(),
					precedence: attributed_patch.precedence,
					base: removed,
					target: None,
					path: path.clone(),
					key: key.clone(),
				});
			}
			ClausewitzPatch::ReplaceBlock {
				path,
				key,
				old_statement,
				new_statement,
			} => {
				replacement_count += 1;
				branches.push(RemoveReplaceBranch {
					mod_id: attributed_patch.mod_id.clone(),
					precedence: attributed_patch.precedence,
					base: old_statement,
					target: Some(new_statement),
					path: path.clone(),
					key: key.clone(),
				});
			}
			_ => return None,
		}
	}
	if replacement_count == 0 || removal_count == 0 {
		return None;
	}
	branches.sort_by(|left, right| {
		left.precedence
			.cmp(&right.precedence)
			.then_with(|| left.mod_id.cmp(&right.mod_id))
	});

	let ancestor_statement = branches
		.iter()
		.min_by_key(|branch| branch.precedence)
		.map(|branch| branch.base)?;
	let ancestor_body = statement_block_body(ancestor_statement)?;
	let empty_body: Vec<AstStatement> = Vec::new();
	let mut branch_patches = Vec::with_capacity(branches.len());
	for branch in &branches {
		let target_body = match branch.target {
			Some(statement) => statement_block_body(statement)?,
			None => &empty_body,
		};
		let mut patches = super::super::patch::diff_block_bodies_including_empty(
			ancestor_body,
			target_body,
			&[],
			0,
			policies.nested_merge_key_source,
		);
		if branch.target.is_some() && patches.iter().any(replacement_patch_edits_existing_child) {
			return None;
		}
		order_patches_by_source(&mut patches, ancestor_body, target_body);
		branch_patches.push((branch.mod_id.clone(), branch.precedence, patches));
	}

	sort_recursive_candidates(&mut branch_patches);
	let patch_origins = recursive_patch_origins(&branch_patches, policies);
	let mut handler = DeferHandler;
	let nested = merge_patch_sets(branch_patches, policies, &mut handler).ok()?;
	stats.accumulate(&nested.stats);
	if !nested.conflicts.is_empty() {
		let reasons = nested
			.conflicts
			.iter()
			.filter_map(|conflict| match conflict {
				PatchResolution::Conflict {
					address, reason, ..
				} => Some(format!("{}: {}", address.key, reason)),
				_ => None,
			})
			.collect::<Vec<_>>();
		stats.conflict_patches += 1;
		return Some(PatchResolution::Conflict {
			address: addr.clone(),
			reason: format!(
				"deep merge of removed/replaced block has {} unresolved sub-conflict(s): {}",
				nested.conflicts.len(),
				reasons.join("; ")
			),
			patches: attributed.to_vec(),
		});
	}

	let mut resolved_patches = nested
		.resolved
		.into_iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Resolved(patch) => Some(patch),
			PatchResolution::AutoMerged { result, .. } => Some(result),
			PatchResolution::Conflict { .. } => None,
		})
		.collect::<Vec<_>>();
	sort_recursive_application(&mut resolved_patches, &patch_origins, policies);
	let merged_body = super::super::patch_apply::apply_patches(
		ancestor_body,
		&resolved_patches,
		policies.nested_merge_key_source,
	);

	let representative = branches
		.iter()
		.filter(|branch| branch.target.is_some())
		.max_by_key(|branch| branch.precedence)?;
	let new_statement = with_block_body(representative.target?, merged_body);
	let contributing_mods = branches
		.iter()
		.map(|branch| branch.mod_id.clone())
		.collect();
	stats.auto_merged_patches += 1;
	Some(PatchResolution::AutoMerged {
		result: ClausewitzPatch::ReplaceBlock {
			path: representative.path.clone(),
			key: representative.key.clone(),
			old_statement: representative.base.clone(),
			new_statement,
		},
		strategy: "recursive_remove_replace_merge".to_string(),
		contributing_mods,
	})
}

fn replacement_patch_edits_existing_child(patch: &ClausewitzPatch) -> bool {
	let path = match patch {
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
	!path.is_empty()
		|| matches!(
			patch,
			ClausewitzPatch::SetValue { .. }
				| ClausewitzPatch::ReplaceBlock { .. }
				| ClausewitzPatch::Rename { .. }
		)
}

/// Attempt to deep-merge multiple mods' `InsertNode` patches at the same
/// address by treating the inserted block bodies as edits from an empty
/// ancestor. This covers full-union merges without game base where sibling
/// mods both introduce the same top-level definition, but with complementary
/// children. Scalar disagreements inside the inserted block still surface as
/// nested conflicts through the normal leaf resolvers.
pub(super) fn try_recursive_insert_merge(
	addr: &PatchAddress,
	attributed: &[AttributedPatch],
	policies: &MergePolicies,
	stats: &mut PatchMergeStats,
) -> Option<PatchResolution> {
	if attributed.len() < 2 {
		return None;
	}

	let mut inserts: Vec<(String, usize, &AstStatement, AstPath, String)> =
		Vec::with_capacity(attributed.len());
	for a in attributed {
		match &a.patch {
			ClausewitzPatch::InsertNode {
				statement,
				path,
				key,
			} => inserts.push((
				a.mod_id.clone(),
				a.precedence,
				statement,
				path.clone(),
				key.clone(),
			)),
			_ => return None,
		}
	}
	inserts.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));

	let ancestor_body: Vec<AstStatement> = Vec::new();
	let mut mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)> =
		Vec::with_capacity(inserts.len());
	for (mod_id, prec, statement, _path, _key) in &inserts {
		let new_body = statement_block_body(statement)?;
		let mut patches = super::super::patch::diff_block_bodies(
			&ancestor_body,
			new_body,
			&[],
			0,
			policies.nested_merge_key_source,
		);
		order_patches_by_source(&mut patches, &ancestor_body, new_body);
		mod_patches.push((mod_id.clone(), *prec, patches));
	}

	sort_recursive_candidates(&mut mod_patches);
	let patch_origins = recursive_patch_origins(&mod_patches, policies);
	let mut handler = DeferHandler;
	let nested = merge_patch_sets(mod_patches, policies, &mut handler).ok()?;
	stats.accumulate(&nested.stats);

	if !nested.conflicts.is_empty() {
		let reasons: Vec<String> = nested
			.conflicts
			.iter()
			.filter_map(|c| match c {
				PatchResolution::Conflict {
					address, reason, ..
				} => Some(format!("{}: {}", address.key, reason)),
				_ => None,
			})
			.collect();
		stats.conflict_patches += 1;
		return Some(PatchResolution::Conflict {
			address: addr.clone(),
			reason: format!(
				"deep merge of inserted block has {} unresolved sub-conflict(s): {}",
				nested.conflicts.len(),
				reasons.join("; ")
			),
			patches: attributed.to_vec(),
		});
	}

	let mut resolved_patches: Vec<ClausewitzPatch> = nested
		.resolved
		.into_iter()
		.filter_map(|r| match r {
			PatchResolution::Resolved(p) => Some(p),
			PatchResolution::AutoMerged { result, .. } => Some(result),
			PatchResolution::Conflict { .. } => None,
		})
		.collect();
	sort_recursive_application(&mut resolved_patches, &patch_origins, policies);

	let merged_body = super::super::patch_apply::apply_patches(
		&ancestor_body,
		&resolved_patches,
		policies.nested_merge_key_source,
	);
	let representative = inserts
		.iter()
		.max_by_key(|(_, prec, _, _, _)| *prec)
		.unwrap();
	let merged_stmt = with_block_body(representative.2, merged_body);

	let mods: Vec<String> = inserts.iter().map(|insert| insert.0.clone()).collect();
	stats.auto_merged_patches += 1;
	Some(PatchResolution::AutoMerged {
		result: ClausewitzPatch::InsertNode {
			path: representative.3.clone(),
			key: representative.4.clone(),
			statement: merged_stmt,
		},
		strategy: "recursive_insert_merge".to_string(),
		contributing_mods: mods,
	})
}

/// Attempt named-container merge across N mod ReplaceBlock patches at the same
/// address. Returns the merged ReplaceBlock if applicable, else `None`.
pub(super) fn try_replace_block_named_container_merge(
	attributed: &[AttributedPatch],
	policies: &MergePolicies,
) -> Option<ClausewitzPatch> {
	if attributed.len() < 2 {
		return None;
	}

	// All patches must be ReplaceBlock with a common `old_statement` (base).
	let first = match &attributed[0].patch {
		ClausewitzPatch::ReplaceBlock { .. } => &attributed[0].patch,
		_ => return None,
	};
	let (path, key, base_old) = match first {
		ClausewitzPatch::ReplaceBlock {
			path,
			key,
			old_statement,
			..
		} => (path.clone(), key.clone(), old_statement.clone()),
		_ => return None,
	};
	for a in attributed.iter().skip(1) {
		match &a.patch {
			ClausewitzPatch::ReplaceBlock { old_statement, .. } => {
				if !ast_equal_ignoring_spans(&base_old, old_statement) {
					return None;
				}
			}
			_ => return None,
		}
	}

	let base_body = statement_block_body(&base_old)?;
	if !items_are_named_container(base_body, policy_allow_scalars(policies)) {
		return None;
	}

	// Sort by precedence ascending so highest-precedence is last (used by OverlayWins).
	let mut ordered: Vec<&AttributedPatch> = attributed.iter().collect();
	ordered.sort_by_key(|a| a.precedence);

	let candidate_owned: Vec<(String, Vec<AstStatement>)> = ordered
		.iter()
		.map(|a| {
			let stmt = match &a.patch {
				ClausewitzPatch::ReplaceBlock { new_statement, .. } => new_statement,
				_ => unreachable!(),
			};
			let body = statement_block_body(stmt).cloned().unwrap_or_default();
			(a.mod_id.clone(), body)
		})
		.collect();

	for (_id, body) in &candidate_owned {
		if !items_are_named_container(body, policy_allow_scalars(policies)) {
			return None;
		}
	}

	let candidate_refs: Vec<(&str, &[AstStatement])> = candidate_owned
		.iter()
		.map(|(id, body)| (id.as_str(), body.as_slice()))
		.collect();

	let merged_body = merge_named_container_bodies(base_body, &candidate_refs, policies).ok()?;

	let merged_stmt = with_block_body(&base_old, merged_body);

	Some(ClausewitzPatch::ReplaceBlock {
		path,
		key,
		old_statement: base_old,
		new_statement: merged_stmt,
	})
}

fn policy_allow_scalars(_policies: &MergePolicies) -> bool {
	// Both SuffixRename and OverlayWins tolerate scalar passthrough at the body
	// level; the gating is done per-child via items_are_named_container.
	true
}

// ---------------------------------------------------------------------------
// Named-container 3-way merge (used by ReplaceBlock resolution and exposed for
// reuse). Operates directly on AST bodies; never reuses `merge/ir.rs`.
// ---------------------------------------------------------------------------

/// Identity of a child statement inside a named-container body.
///
/// `key` is the assignment key (e.g. `windowType`, `iconType`). `name` is the
/// inner `name = "..."` field's value, when present — this is what
/// distinguishes two `windowType` siblings inside a parent container.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ChildIdentity {
	pub key: String,
	pub name: Option<String>,
}

/// Errors from `merge_named_container_bodies`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NamedContainerMergeError {
	/// Bodies do not look like a named container (failed gating heuristics).
	NotNamedContainer,
	/// Conflict that policy refused to resolve (e.g. OverlayWins requested but
	/// candidates are unordered; reserved for future strict modes).
	UnresolvableConflict,
}

/// Compute the identity of an `AstStatement` for named-container indexing.
///
/// Returns:
/// - `Some({ key, name: Some(...) })` for `key = { name = "..." ... }` blocks
/// - `Some({ key, name: None })` for any other `key = <value>` assignment
/// - `None` for items / comments (no stable identity)
pub fn child_identity(stmt: &AstStatement) -> Option<ChildIdentity> {
	match stmt {
		AstStatement::Assignment { key, value, .. } => {
			let name = block_name_field(value);
			Some(ChildIdentity {
				key: key.clone(),
				name,
			})
		}
		_ => None,
	}
}

/// Extract the inner `name = "..."` (or `name = identifier`) field from a block
/// value, if present.
fn block_name_field(value: &AstValue) -> Option<String> {
	let items = match value {
		AstValue::Block { items, .. } => items,
		_ => return None,
	};
	for stmt in items {
		if let AstStatement::Assignment {
			key,
			value: AstValue::Scalar { value: sv, .. },
			..
		} = stmt && key == "name"
		{
			return Some(sv.as_text());
		}
	}
	None
}

/// Heuristic: is the given body shaped like a named-container body?
///
/// - At least one block-typed child is required.
/// - Block-typed children must have unique `ChildIdentity` (or be exactly equal,
///   which we tolerate as a duplicate definition).
/// - When `allow_scalars` is `false`, the body must contain only block children
///   (no scalar/assignment-with-scalar siblings).
pub fn items_are_named_container(body: &[AstStatement], allow_scalars: bool) -> bool {
	let mut block_children = 0usize;
	let mut seen: Vec<(ChildIdentity, &AstStatement)> = Vec::new();
	for stmt in body {
		match stmt {
			AstStatement::Comment { .. } => continue,
			AstStatement::Item { .. } => {
				if !allow_scalars {
					return false;
				}
			}
			AstStatement::Assignment { value, .. } => match value {
				AstValue::Block { .. } => {
					block_children += 1;
					let id = match child_identity(stmt) {
						Some(id) => id,
						None => return false,
					};
					for (other_id, other_stmt) in &seen {
						if other_id == &id && !ast_equal_ignoring_spans(other_stmt, stmt) {
							return false;
						}
					}
					seen.push((id, stmt));
				}
				AstValue::Scalar { .. } => {
					if !allow_scalars {
						return false;
					}
				}
			},
		}
	}
	block_children > 0
}

/// Span-stripped structural equality on statements. Two statements are equal
/// here iff they would print identically modulo whitespace/positions.
pub fn ast_equal_ignoring_spans(a: &AstStatement, b: &AstStatement) -> bool {
	match (a, b) {
		(
			AstStatement::Assignment {
				key: ka, value: va, ..
			},
			AstStatement::Assignment {
				key: kb, value: vb, ..
			},
		) => ka == kb && ast_value_equal_ignoring_spans(va, vb),
		(AstStatement::Item { value: va, .. }, AstStatement::Item { value: vb, .. }) => {
			ast_value_equal_ignoring_spans(va, vb)
		}
		(AstStatement::Comment { text: ta, .. }, AstStatement::Comment { text: tb, .. }) => {
			ta == tb
		}
		_ => false,
	}
}

fn ast_value_equal_ignoring_spans(a: &AstValue, b: &AstValue) -> bool {
	match (a, b) {
		(AstValue::Scalar { value: va, .. }, AstValue::Scalar { value: vb, .. }) => va == vb,
		(AstValue::Block { items: ia, .. }, AstValue::Block { items: ib, .. }) => {
			if ia.len() != ib.len() {
				return false;
			}
			ia.iter()
				.zip(ib.iter())
				.all(|(x, y)| ast_equal_ignoring_spans(x, y))
		}
		_ => false,
	}
}

/// Suffix-rename a named child by appending `_<sanitized_mod_id>` either to its
/// inner `name = "..."` field (preferred) or otherwise to its assignment key.
///
/// Statements without an identity (items/comments) are returned unchanged.
pub fn rename_named_child(stmt: &AstStatement, mod_id: &str) -> AstStatement {
	let suffix = sanitize_mod_id(mod_id);
	match stmt {
		AstStatement::Assignment {
			key,
			key_span,
			value,
			span,
		} => {
			if let AstValue::Block { items, span: bspan } = value
				&& items
					.iter()
					.any(|s| matches!(s, AstStatement::Assignment { key, .. } if key == "name"))
			{
				let renamed_items: Vec<AstStatement> = items
					.iter()
					.map(|s| match s {
						AstStatement::Assignment {
							key: k,
							key_span,
							value: AstValue::Scalar {
								value: sv,
								span: ssp,
							},
							span,
						} if k == "name" => {
							let new_text = format!("{}_{}", sv.as_text(), suffix);
							let new_scalar = match sv {
								ScalarValue::Identifier(_) => ScalarValue::Identifier(new_text),
								ScalarValue::String(_) => ScalarValue::String(new_text),
								ScalarValue::Number(_) => ScalarValue::Identifier(new_text),
								ScalarValue::Bool(_) => ScalarValue::Identifier(new_text),
							};
							AstStatement::Assignment {
								key: k.clone(),
								key_span: key_span.clone(),
								value: AstValue::Scalar {
									value: new_scalar,
									span: ssp.clone(),
								},
								span: span.clone(),
							}
						}
						other => other.clone(),
					})
					.collect();
				return AstStatement::Assignment {
					key: key.clone(),
					key_span: key_span.clone(),
					value: AstValue::Block {
						items: renamed_items,
						span: bspan.clone(),
					},
					span: span.clone(),
				};
			}
			AstStatement::Assignment {
				key: format!("{key}_{suffix}"),
				key_span: key_span.clone(),
				value: value.clone(),
				span: span.clone(),
			}
		}
		_ => stmt.clone(),
	}
}

fn sanitize_mod_id(mod_id: &str) -> String {
	mod_id
		.chars()
		.map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
		.collect()
}

/// 3-way merge a base named-container body with N candidate (post-modification)
/// bodies from different mods, producing a unioned body.
///
/// `candidate_bodies` should be ordered by ascending precedence (higher
/// precedence later) — this matters for `OverlayWins`.
pub fn merge_named_container_bodies(
	base_body: &[AstStatement],
	candidate_bodies: &[(&str, &[AstStatement])],
	policies: &MergePolicies,
) -> Result<Vec<AstStatement>, NamedContainerMergeError> {
	let allow_scalars = policy_allow_scalars(policies);
	// Require that at least one of (base, candidates) is a recognizable
	// named-container body, and that none of them contradicts the shape.
	let any_qualifies = items_are_named_container(base_body, allow_scalars)
		|| candidate_bodies
			.iter()
			.any(|(_, body)| items_are_named_container(body, allow_scalars));
	if !any_qualifies {
		return Err(NamedContainerMergeError::NotNamedContainer);
	}
	if !valid_named_container_shape(base_body, allow_scalars) {
		return Err(NamedContainerMergeError::NotNamedContainer);
	}
	for (_, body) in candidate_bodies {
		if !valid_named_container_shape(body, allow_scalars) {
			return Err(NamedContainerMergeError::NotNamedContainer);
		}
	}

	// Start from base; index identifiable children by identity for O(1) lookup.
	let mut result: Vec<AstStatement> = base_body.to_vec();
	let mut index: HashMap<ChildIdentity, usize> = HashMap::new();
	let mut base_index: HashMap<ChildIdentity, AstStatement> = HashMap::new();
	for (i, stmt) in result.iter().enumerate() {
		if let Some(id) = child_identity(stmt) {
			base_index.insert(id.clone(), stmt.clone());
			index.insert(id, i);
		}
	}

	for (mod_id, body) in candidate_bodies {
		for stmt in *body {
			let id = match child_identity(stmt) {
				Some(id) => id,
				None => {
					if !result.iter().any(|s| ast_equal_ignoring_spans(s, stmt)) {
						result.push(stmt.clone());
					}
					continue;
				}
			};
			let is_block = matches!(
				stmt,
				AstStatement::Assignment {
					value: AstValue::Block { .. },
					..
				}
			);
			if !is_block {
				// Scalar assignment: last-writer at same identity.
				match index.get(&id).copied() {
					Some(idx) => {
						if !ast_equal_ignoring_spans(&result[idx], stmt) {
							result[idx] = stmt.clone();
						}
					}
					None => {
						let new_idx = result.len();
						result.push(stmt.clone());
						index.insert(id.clone(), new_idx);
					}
				}
				continue;
			}
			match index.get(&id).copied() {
				None => {
					let new_idx = result.len();
					result.push(stmt.clone());
					index.insert(id.clone(), new_idx);
				}
				Some(idx) => {
					if ast_equal_ignoring_spans(&result[idx], stmt) {
						continue;
					}
					if let Some(merged) =
						try_recursive_named_merge(&result[idx], stmt, mod_id, policies)
					{
						result[idx] = merged;
					} else {
						match policies.named_container {
							NamedContainerPolicy::Conflict => {
								if let Some(merged) = try_shared_base_or_subset_merge(
									base_index.get(&id),
									&result[idx],
									stmt,
								) {
									result[idx] = merged;
									continue;
								}
								// Sibling mods target the same named identity
								// with bodies that cannot be merged structurally
								// → defer to the user instead of silently
								// renaming or overwriting.
								return Err(NamedContainerMergeError::UnresolvableConflict);
							}
							NamedContainerPolicy::OverlayWins => {
								result[idx] = stmt.clone();
							}
							NamedContainerPolicy::ScrollStack => {
								match synthesize_scroll_stack(&result[idx], stmt) {
									Some(stacked) => result[idx] = stacked,
									None => {
										return Err(NamedContainerMergeError::UnresolvableConflict);
									}
								}
							}
							NamedContainerPolicy::SuffixRename => {
								let renamed = rename_named_child(stmt, mod_id);
								if let Some(new_id) = child_identity(&renamed) {
									let new_idx = result.len();
									result.push(renamed);
									index.entry(new_id).or_insert(new_idx);
								} else {
									result.push(renamed);
								}
							}
						}
					}
				}
			}
		}
	}

	Ok(result)
}

fn try_shared_base_or_subset_merge(
	base: Option<&AstStatement>,
	existing: &AstStatement,
	candidate: &AstStatement,
) -> Option<AstStatement> {
	if statement_contains_all_real_content(existing, candidate) {
		return Some(existing.clone());
	}
	if statement_contains_all_real_content(candidate, existing) {
		return Some(candidate.clone());
	}

	let base = base?;
	let base_body = statement_block_body(base)?;
	if !statements_have_real_content(base_body)
		|| !statement_contains_all_real_content(existing, base)
		|| !statement_contains_all_real_content(candidate, base)
	{
		return None;
	}

	let existing_body = statement_block_body(existing)?;
	let candidate_body = statement_block_body(candidate)?;
	let union_body = union_statement_bodies(base_body, existing_body, candidate_body);
	if has_divergent_duplicate_identity(&union_body) {
		return None;
	}
	Some(with_block_body(existing, union_body))
}

fn statement_contains_all_real_content(superset: &AstStatement, subset: &AstStatement) -> bool {
	match (statement_block_body(superset), statement_block_body(subset)) {
		(Some(superset_body), Some(subset_body)) => {
			body_contains_all_real_content(superset_body, subset_body)
		}
		_ => ast_statements_semantically_equal(superset, subset),
	}
}

fn body_contains_all_real_content(superset: &[AstStatement], subset: &[AstStatement]) -> bool {
	subset
		.iter()
		.filter(|stmt| statement_has_real_content(stmt))
		.all(|needle| {
			superset
				.iter()
				.any(|candidate| ast_statements_semantically_equal(candidate, needle))
		})
}

fn union_statement_bodies(
	base_body: &[AstStatement],
	existing_body: &[AstStatement],
	candidate_body: &[AstStatement],
) -> Vec<AstStatement> {
	let mut union = Vec::new();
	push_unique_statements(base_body, &mut union);
	push_unique_statements(existing_body, &mut union);
	push_unique_statements(candidate_body, &mut union);
	union
}

fn push_unique_statements(source: &[AstStatement], out: &mut Vec<AstStatement>) {
	for stmt in source {
		if !out
			.iter()
			.any(|existing| ast_statements_semantically_equal(existing, stmt))
		{
			out.push(stmt.clone());
		}
	}
}

fn has_divergent_duplicate_identity(body: &[AstStatement]) -> bool {
	let mut seen: Vec<(ChildIdentity, &AstStatement)> = Vec::new();
	for stmt in body.iter().filter(|stmt| statement_has_real_content(stmt)) {
		let Some(id) = child_identity(stmt) else {
			continue;
		};
		for (other_id, other_stmt) in &seen {
			if other_id == &id && !ast_statements_semantically_equal(other_stmt, stmt) {
				return true;
			}
		}
		seen.push((id, stmt));
	}
	false
}

fn statements_have_real_content(statements: &[AstStatement]) -> bool {
	statements.iter().any(statement_has_real_content)
}

fn statement_has_real_content(statement: &AstStatement) -> bool {
	match statement {
		AstStatement::Comment { .. } => false,
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		}
		| AstStatement::Item {
			value: AstValue::Block { items, .. },
			..
		} => statements_have_real_content(items),
		AstStatement::Assignment { .. } | AstStatement::Item { .. } => true,
	}
}

/// Looser validity gate used during recursion: a body is acceptable if it has
/// no scalars (when `!allow_scalars`) and no duplicate-identity block children
/// — but it need not contain any blocks itself (it may be empty / scalar-only
/// if `allow_scalars`).
fn valid_named_container_shape(body: &[AstStatement], allow_scalars: bool) -> bool {
	let mut seen: Vec<(ChildIdentity, &AstStatement)> = Vec::new();
	for stmt in body {
		match stmt {
			AstStatement::Comment { .. } => continue,
			AstStatement::Item { .. } => {
				if !allow_scalars {
					return false;
				}
			}
			AstStatement::Assignment { value, .. } => match value {
				AstValue::Block { .. } => {
					let id = match child_identity(stmt) {
						Some(id) => id,
						None => return false,
					};
					for (other_id, other_stmt) in &seen {
						if other_id == &id && !ast_equal_ignoring_spans(other_stmt, stmt) {
							return false;
						}
					}
					seen.push((id, stmt));
				}
				AstValue::Scalar { .. } => {
					if !allow_scalars {
						return false;
					}
				}
			},
		}
	}
	true
}

/// Attempt to merge two same-identity block children by recursing into their
/// bodies as named-container bodies. Returns `Some(merged)` only when at least
/// one side has nested block children (so we are confident the inner body is a
/// real named container, not a trigger / position spec / scalar leaf block).
fn try_recursive_named_merge(
	existing: &AstStatement,
	candidate: &AstStatement,
	candidate_mod_id: &str,
	policies: &MergePolicies,
) -> Option<AstStatement> {
	let existing_value = match existing {
		AstStatement::Assignment { value, .. } => value,
		_ => return None,
	};
	let candidate_value = match candidate {
		AstStatement::Assignment { value, .. } => value,
		_ => return None,
	};
	let existing_body = match existing_value {
		AstValue::Block { items, .. } => items,
		_ => return None,
	};
	let candidate_body = match candidate_value {
		AstValue::Block { items, .. } => items,
		_ => return None,
	};
	let allow_scalars = policy_allow_scalars(policies);
	let either_has_blocks = items_are_named_container(existing_body, allow_scalars)
		|| items_are_named_container(candidate_body, allow_scalars);
	if !either_has_blocks {
		return None;
	}
	if !valid_named_container_shape(existing_body, allow_scalars)
		|| !valid_named_container_shape(candidate_body, allow_scalars)
	{
		return None;
	}
	let merged = merge_named_container_bodies(
		existing_body,
		&[(candidate_mod_id, candidate_body.as_slice())],
		policies,
	)
	.ok()?;
	Some(with_block_body(existing, merged))
}

fn statement_block_body(stmt: &AstStatement) -> Option<&Vec<AstStatement>> {
	match stmt {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => Some(items),
		AstStatement::Item {
			value: AstValue::Block { items, .. },
			..
		} => Some(items),
		_ => None,
	}
}

fn with_block_body(stmt: &AstStatement, items: Vec<AstStatement>) -> AstStatement {
	match stmt {
		AstStatement::Assignment {
			key,
			key_span,
			value: AstValue::Block { span, .. },
			span: outer_span,
		} => AstStatement::Assignment {
			key: key.clone(),
			key_span: key_span.clone(),
			value: AstValue::Block {
				items,
				span: span.clone(),
			},
			span: outer_span.clone(),
		},
		AstStatement::Item {
			value: AstValue::Block { span, .. },
			span: outer_span,
		} => AstStatement::Item {
			value: AstValue::Block {
				items,
				span: span.clone(),
			},
			span: outer_span.clone(),
		},
		other => other.clone(),
	}
}

// ---------------------------------------------------------------------------
// BooleanOr synthesis
// ---------------------------------------------------------------------------

/// Build a zero-length span placeholder for synthesized AST nodes.
fn synthetic_span() -> SpanRange {
	let zero = Span {
		line: 0,
		column: 0,
		offset: 0,
	};
	SpanRange {
		start: zero.clone(),
		end: zero,
	}
}

/// Extract the block-typed body of a statement of the form `key = { ... }`.
/// Returns `None` if the statement is not an `Assignment` whose value is a
/// `Block` — BooleanOr only makes sense for block-bodied keys.
fn extract_block_body(stmt: &AstStatement) -> Option<Vec<AstStatement>> {
	match stmt {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => Some(items.clone()),
		_ => None,
	}
}

// ---------------------------------------------------------------------------
// Scroll-stack synthesis (GUI keep-both)
// ---------------------------------------------------------------------------

const SCROLL_LAYER_PREFIX: &str = "foch_scroll_layer_";
const SCROLL_LAYER_HEIGHT: i64 = 1000;
const SCROLL_VIEWPORT_WIDTH: i64 = 1000;
const SCROLL_VIEWPORT_HEIGHT: i64 = 600;
const SCROLL_BAR_SPRITE: &str = "standardlistbox_slider";

fn scalar_assignment(key: &str, value: ScalarValue) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: synthetic_span(),
		value: AstValue::Scalar {
			value,
			span: synthetic_span(),
		},
		span: synthetic_span(),
	}
}

fn block_assignment(key: &str, items: Vec<AstStatement>) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: synthetic_span(),
		value: AstValue::Block {
			items,
			span: synthetic_span(),
		},
		span: synthetic_span(),
	}
}

fn xy_block(key: &str, x: i64, y: i64) -> AstStatement {
	block_assignment(
		key,
		vec![
			scalar_assignment("x", ScalarValue::Number(x.to_string())),
			scalar_assignment("y", ScalarValue::Number(y.to_string())),
		],
	)
}

/// The `name = "..."` value of a body, if present.
fn body_name(body: &[AstStatement]) -> Option<String> {
	body.iter().find_map(|s| match s {
		AstStatement::Assignment {
			key,
			value: AstValue::Scalar { value, .. },
			..
		} if key == "name" => Some(value.as_text()),
		_ => None,
	})
}

/// Body with the top-level `name = ...` assignment removed, so it can be nested
/// inside a freshly-named container without a duplicate identity.
fn strip_name(body: &[AstStatement]) -> Vec<AstStatement> {
	body.iter()
		.filter(|s| !matches!(s, AstStatement::Assignment { key, .. } if key == "name"))
		.cloned()
		.collect()
}

/// True if `body` already contains foch scroll-stack layers (built by a
/// previous pairwise merge), so further contributors append rather than nest.
fn is_scroll_stack_body(body: &[AstStatement]) -> bool {
	body.iter().any(|s| match s {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} if key == "containerWindowType" => {
			body_name(items).is_some_and(|n| n.starts_with(SCROLL_LAYER_PREFIX))
		}
		_ => false,
	})
}

fn make_layer(index: usize, widgets: Vec<AstStatement>) -> AstStatement {
	let mut items = vec![
		scalar_assignment(
			"name",
			ScalarValue::String(format!("{SCROLL_LAYER_PREFIX}{index}")),
		),
		xy_block("position", 0, index as i64 * SCROLL_LAYER_HEIGHT),
	];
	items.extend(widgets);
	block_assignment("containerWindowType", items)
}

/// Synthesize a single same-name widget that keeps BOTH contributors' bodies as
/// vertically-offset child containers inside a scrollable parent (GUI keep-both).
///
/// Lossless: every non-`name` widget from both sides survives, under its own
/// `containerWindowType` namespace, offset so they don't overlap. The parent
/// keeps the original `name` (so the engine still resolves it) plus a size and a
/// `verticalScrollbar`. The scroll *behaviour* is best-effort and should be
/// eyeballed in-game; the merge guarantee is only that no content is dropped.
/// Returns `None` if either side isn't a block-bodied named widget.
fn synthesize_scroll_stack(
	existing: &AstStatement,
	incoming: &AstStatement,
) -> Option<AstStatement> {
	let (key, existing_body) = match existing {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} => (key.clone(), items),
		_ => return None,
	};
	let incoming_body = extract_block_body(incoming)?;
	let name = body_name(existing_body)?;

	let mut parent: Vec<AstStatement> = Vec::new();
	let mut layers: Vec<AstStatement> = Vec::new();

	if is_scroll_stack_body(existing_body) {
		// Already a stack: keep its parent props + layers, append a new layer.
		let existing_layers = existing_body
			.iter()
			.filter(
				|s| matches!(s, AstStatement::Assignment { key, .. } if key == "containerWindowType"),
			)
			.count();
		for s in existing_body {
			match s {
				AstStatement::Assignment { key, .. } if key == "containerWindowType" => {
					layers.push(s.clone());
				}
				// size / verticalScrollbar are rebuilt below.
				AstStatement::Assignment { key, .. }
					if key == "size" || key == "verticalScrollbar" => {}
				other => parent.push(other.clone()),
			}
		}
		layers.push(make_layer(existing_layers, strip_name(&incoming_body)));
	} else {
		parent.push(scalar_assignment("name", ScalarValue::String(name)));
		layers.push(make_layer(0, strip_name(existing_body)));
		layers.push(make_layer(1, strip_name(&incoming_body)));
	}

	parent.push(xy_block(
		"size",
		SCROLL_VIEWPORT_WIDTH,
		SCROLL_VIEWPORT_HEIGHT,
	));
	parent.push(scalar_assignment(
		"verticalScrollbar",
		ScalarValue::String(SCROLL_BAR_SPRITE.to_string()),
	));
	parent.extend(layers);

	Some(block_assignment(&key, parent))
}

pub(super) fn synthesize_scroll_stacked_insert(
	attributed: &[AttributedPatch],
) -> Option<ClausewitzPatch> {
	let mut inserts = attributed.iter().map(|a| match &a.patch {
		ClausewitzPatch::InsertNode {
			statement,
			path,
			key,
		} => Some((statement, path, key)),
		_ => None,
	});
	let (first_statement, path, key) = inserts.next()??;
	let mut stacked = first_statement.clone();
	for insert in inserts {
		let (statement, _, _) = insert?;
		stacked = synthesize_scroll_stack(&stacked, statement)?;
	}
	Some(ClausewitzPatch::InsertNode {
		path: path.clone(),
		key: key.clone(),
		statement: stacked,
	})
}

/// Pull the AST body that each contributor wants to install at `addr`,
/// from either an `InsertNode` or a `ReplaceBlock` patch.
fn contributor_body(patch: &ClausewitzPatch) -> Option<Vec<AstStatement>> {
	match patch {
		ClausewitzPatch::InsertNode { statement, .. } => extract_block_body(statement),
		ClausewitzPatch::ReplaceBlock { new_statement, .. } => extract_block_body(new_statement),
		_ => None,
	}
}

/// Synthesize a single patch whose body is `{ OR = { <d_0> <d_1> ... } }`,
/// where each disjunct `<d_i>` is contributor `i`'s body (inlined if it is a
/// single statement, else wrapped in `AND = { ... }`). This expresses the
/// intended Boolean-OR semantics — the merged key holds if *any* contributor's
/// body holds — preserving the original key. Returns `None` (leaving resolution
/// to the caller's default behavior) if any contributor isn't a block-bodied
/// assignment.
///
/// NOTE: the disjuncts live inside ONE shared `OR` (an OR of conjunctions). They
/// must NOT be emitted as sibling `OR = { ... }` blocks, because trigger-block
/// siblings are an implicit AND — that would invert the semantics to the
/// intersection of the contributors. No cross-contributor deduplication is
/// performed: even byte-identical bodies (which would have already
/// short-circuited via the convergence check upstream) are treated as separate
/// disjuncts here, matching the caller's contract that `attributed.len() >= 2`
/// and the bodies differ.
pub(super) fn synthesize_boolean_or(
	addr: &PatchAddress,
	attributed: &[AttributedPatch],
) -> Option<ClausewitzPatch> {
	let bodies: Option<Vec<Vec<AstStatement>>> = attributed
		.iter()
		.map(|a| contributor_body(&a.patch))
		.collect();
	let bodies = bodies?;
	// Skip empty bodies: emitting `OR = {}` is meaningless and would
	// short-circuit trigger evaluation in unintended ways.
	let bodies: Vec<Vec<AstStatement>> = bodies.into_iter().filter(|b| !b.is_empty()).collect();
	if bodies.len() < 2 {
		return None;
	}

	let items = combine_boolean_or_bodies(bodies)?;

	let synthesized_value = AstValue::Block {
		items,
		span: synthetic_span(),
	};
	let synthesized_stmt = AstStatement::Assignment {
		key: addr.key.clone(),
		key_span: synthetic_span(),
		value: synthesized_value,
		span: synthetic_span(),
	};

	// Reuse the first attributed patch's variant + path/key so downstream
	// consumers see a structurally equivalent operation.
	match &attributed[0].patch {
		ClausewitzPatch::InsertNode { path, key, .. } => Some(ClausewitzPatch::InsertNode {
			path: path.clone(),
			key: key.clone(),
			statement: synthesized_stmt,
		}),
		ClausewitzPatch::ReplaceBlock {
			path,
			key,
			old_statement,
			..
		} => Some(ClausewitzPatch::ReplaceBlock {
			path: path.clone(),
			key: key.clone(),
			old_statement: old_statement.clone(),
			new_statement: synthesized_stmt,
		}),
		_ => None,
	}
}

// ---------------------------------------------------------------------------
// Conflict-rename
// ---------------------------------------------------------------------------

/// Produce a copy of `stmt` whose merge identity is suffixed with `mod_id`,
/// allowing two conflicting `InsertNode` patches at the same `PatchAddress` to
/// coexist in the merged output. The "identity" location depends on which
/// merge-key source the content family uses:
///
/// * `AssignmentKey` / `ContainerChildKey` — rename the top-level assignment
///   key (e.g. `pragmatic_sanction` → `pragmatic_sanction_mod_a`).
/// * `ContainerChildFieldValue` / `ChildFieldValue` — rename the child
///   assignment's inner identity field when present (e.g. `name = widget` →
///   `name = widget_mod_a`).
/// * `FieldValue(field)` — rename the inner scalar field that supplies the
///   merge key (e.g. `id = test.1` → `id = test.1_mod_a`).
/// * `LeafPath` — the path itself is the identity and cannot be safely
///   suffixed without changing semantics, so the statement is returned
///   unchanged. Callers should fall back to a last-writer policy in that
///   case.
///
/// Comments and bare items are returned unchanged: they have no merge key
/// to rename.
pub fn rename_for_conflict(
	stmt: &AstStatement,
	key_source: MergeKeySource,
	mod_id: &str,
) -> AstStatement {
	match key_source {
		MergeKeySource::AssignmentKey | MergeKeySource::ContainerChildKey => {
			rename_top_level_key(stmt, mod_id)
		}
		MergeKeySource::ContainerChildFieldValue {
			child_key_field, ..
		} => rename_inner_field_value(stmt, child_key_field, mod_id),
		MergeKeySource::ChildFieldValue {
			child_key_field, ..
		} => rename_inner_field_value(stmt, child_key_field, mod_id),
		MergeKeySource::FieldValue(field) => rename_inner_field_value(stmt, field, mod_id),
		MergeKeySource::LeafPath => stmt.clone(),
	}
}

fn rename_top_level_key(stmt: &AstStatement, mod_id: &str) -> AstStatement {
	match stmt {
		AstStatement::Assignment {
			key,
			key_span,
			value,
			span,
		} => AstStatement::Assignment {
			key: format!("{key}_{mod_id}"),
			key_span: key_span.clone(),
			value: value.clone(),
			span: span.clone(),
		},
		other => other.clone(),
	}
}

fn rename_inner_field_value(stmt: &AstStatement, field: &str, mod_id: &str) -> AstStatement {
	let AstStatement::Assignment {
		key,
		key_span,
		value: AstValue::Block {
			items,
			span: block_span,
		},
		span,
	} = stmt
	else {
		return stmt.clone();
	};

	let new_items: Vec<AstStatement> = items
		.iter()
		.map(|item| match item {
			AstStatement::Assignment {
				key: ikey,
				key_span: iks,
				value: AstValue::Scalar {
					value: sv,
					span: sspan,
				},
				span: ispan,
			} if ikey == field => {
				let new_text = format!("{}_{}", sv.as_text(), mod_id);
				let renamed = match sv {
					ScalarValue::Identifier(_) => ScalarValue::Identifier(new_text),
					ScalarValue::String(_) => ScalarValue::String(new_text),
					// Numbers and booleans become identifiers once suffixed —
					// the result is no longer a valid number/bool literal.
					ScalarValue::Number(_) | ScalarValue::Bool(_) => {
						ScalarValue::Identifier(new_text)
					}
				};
				AstStatement::Assignment {
					key: ikey.clone(),
					key_span: iks.clone(),
					value: AstValue::Scalar {
						value: renamed,
						span: sspan.clone(),
					},
					span: ispan.clone(),
				}
			}
			other => other.clone(),
		})
		.collect();

	AstStatement::Assignment {
		key: key.clone(),
		key_span: key_span.clone(),
		value: AstValue::Block {
			items: new_items,
			span: block_span.clone(),
		},
		span: span.clone(),
	}
}

#[cfg(test)]
mod scroll_stack_tests {
	use super::*;

	fn name(value: &str) -> AstStatement {
		scalar_assignment("name", ScalarValue::String(value.to_string()))
	}

	/// `windowType = { name="<id>" iconType = { name="<icon>" } }`
	fn widget(id: &str, icon: &str) -> AstStatement {
		block_assignment(
			"windowType",
			vec![name(id), block_assignment("iconType", vec![name(icon)])],
		)
	}

	fn collect_names(stmts: &[AstStatement], out: &mut Vec<String>) {
		for s in stmts {
			if let AstStatement::Assignment { key, value, .. } = s {
				if key == "name"
					&& let AstValue::Scalar { value, .. } = value
				{
					out.push(value.as_text());
				}
				if let AstValue::Block { items, .. } = value {
					collect_names(items, out);
				}
			}
		}
	}

	fn names_in(stmt: &AstStatement) -> Vec<String> {
		let mut out = Vec::new();
		collect_names(std::slice::from_ref(stmt), &mut out);
		out
	}

	#[test]
	fn synthesize_scroll_stack_keeps_both_bodies_lossless() {
		let a = widget("X", "icon_a");
		let b = widget("X", "icon_b");
		let stacked = synthesize_scroll_stack(&a, &b).expect("scroll-stack");
		let names = names_in(&stacked);
		assert!(
			names.contains(&"X".to_string()),
			"parent name kept: {names:?}"
		);
		assert!(
			names.contains(&"icon_a".to_string()),
			"mod A widget kept: {names:?}"
		);
		assert!(
			names.contains(&"icon_b".to_string()),
			"mod B widget kept: {names:?}"
		);
		assert_eq!(
			names
				.iter()
				.filter(|n| n.starts_with(SCROLL_LAYER_PREFIX))
				.count(),
			2,
			"two scroll layers: {names:?}"
		);
		// The parent keeps the original key + exactly one name = the engine still
		// resolves it; the divergent bodies live in separate child namespaces.
		assert!(matches!(&stacked, AstStatement::Assignment { key, .. } if key == "windowType"));
	}

	#[test]
	fn synthesize_scroll_stack_appends_third_contributor_flat() {
		let ab =
			synthesize_scroll_stack(&widget("X", "icon_a"), &widget("X", "icon_b")).expect("ab");
		let abc = synthesize_scroll_stack(&ab, &widget("X", "icon_c")).expect("abc");
		let names = names_in(&abc);
		for w in ["icon_a", "icon_b", "icon_c"] {
			assert!(names.contains(&w.to_string()), "{w} kept: {names:?}");
		}
		assert_eq!(
			names
				.iter()
				.filter(|n| n.starts_with(SCROLL_LAYER_PREFIX))
				.count(),
			3,
			"three flat layers (not nested): {names:?}"
		);
	}
}
