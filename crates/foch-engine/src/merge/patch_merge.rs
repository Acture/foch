// Patch set merging: given N mods' patch sets against a common base, merge
// them into a single resolved patch set with conflict detection.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::content_family::{
	BlockPatchPolicy, MergeKeySource, MergePolicies, NamedContainerPolicy, ScalarMergePolicy,
};
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};

use super::conflict_handler::{ConflictDecision, ConflictHandler, DeferHandler};
use super::error::MergeError;
use super::patch::{AstPath, ClausewitzPatch, patches_semantically_equal};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Address of a patch — uniquely identifies what AST node is being changed.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct PatchAddress {
	pub path: AstPath,
	pub key: String,
}

/// A patch attributed to a specific mod.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributedPatch {
	pub mod_id: String,
	pub precedence: usize,
	pub patch: ClausewitzPatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchConflict {
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
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn patch_address(patch: &ClausewitzPatch, policies: &MergePolicies) -> PatchAddress {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. } => PatchAddress {
			path: path.clone(),
			key: key.clone(),
		},
		ClausewitzPatch::RemoveNode {
			path, key, removed, ..
		} => {
			// Only fingerprint InsertNode / RemoveNode bodies when the target block
			// policy is *not* BooleanOr. BooleanOr definitions intentionally collide
			// at `(path, key)` so synthesis can fold their bodies into one OR block.
			let fingerprint_nodes = !matches!(
				policies.block_patch_policy_for_key(key),
				BlockPatchPolicy::BooleanOr
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
			let fingerprint_nodes = !matches!(
				policies.block_patch_policy_for_key(key),
				BlockPatchPolicy::BooleanOr
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
fn patch_raw_address(patch: &ClausewitzPatch) -> Option<(AstPath, String)> {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. }
		| ClausewitzPatch::RemoveNode { path, key, .. }
		| ClausewitzPatch::InsertNode { path, key, .. }
		| ClausewitzPatch::ReplaceBlock { path, key, .. } => Some((path.clone(), key.clone())),
		_ => None,
	}
}

/// Stable, span-ignoring fingerprint for an `AstValue`. Used to give each
/// distinct `AppendBlockItem` / `RemoveBlockItem` its own `PatchAddress` so
/// that multiple mods can append/remove different values inside the same
/// block without one clobbering the others.
fn value_fingerprint(v: &AstValue) -> String {
	let mut out = String::new();
	fingerprint_into(v, &mut out);
	out
}

/// Span-ignoring fingerprint for an `AstStatement`, used to disambiguate
/// `InsertNode` / `RemoveNode` patches that share the same `(path, key)`
/// but carry different bodies. This matters whenever the parent block is
/// allowed to contain repeated keys (triggers, effects, lists like `OR`,
/// `AND`, `tooltip` blocks, etc.): two mods inserting `has_country_flag = X`
/// and `has_country_flag = Y` must each map to a distinct address so they
/// can both apply, instead of either colliding (mixed-kind false positive
/// when paired with an unrelated `RemoveNode`) or silently losing one
/// payload via `compatible_inserts` highest-precedence selection.
///
/// `InsertNode` is only emitted by the diff when a key was absent in base
/// and present (exactly once) in overlay, so it is always semantically
/// "add a fresh statement". Including the body fingerprint therefore
/// preserves the merge intent for both repeated-key parents and
/// genuinely-unique keys: convergent inserts still share one address;
/// distinct inserts each get their own.
fn statement_fingerprint(stmt: &AstStatement) -> String {
	let mut out = String::new();
	match stmt {
		AstStatement::Assignment { value, .. } => fingerprint_into(value, &mut out),
		AstStatement::Item { value, .. } => fingerprint_into(value, &mut out),
		AstStatement::Comment { text, .. } => {
			out.push('c');
			out.push(':');
			out.push_str(text);
		}
	}
	out
}

fn fingerprint_into(v: &AstValue, out: &mut String) {
	match v {
		AstValue::Scalar { value, .. } => {
			out.push('s');
			out.push(':');
			out.push_str(&value.as_text());
		}
		AstValue::Block { items, .. } => {
			out.push('b');
			out.push('[');
			for s in items {
				match s {
					AstStatement::Assignment { key, value, .. } => {
						out.push('a');
						out.push_str(key);
						out.push('=');
						fingerprint_into(value, out);
						out.push(';');
					}
					AstStatement::Item { value, .. } => {
						out.push('i');
						fingerprint_into(value, out);
						out.push(';');
					}
					AstStatement::Comment { .. } => {}
				}
			}
			out.push(']');
		}
	}
}

/// Discriminant tag for patch variant, used to detect mixed-kind conflicts.
fn patch_kind(patch: &ClausewitzPatch) -> &'static str {
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

/// Try to parse an `AstValue` as f64 for numeric merge policies.
fn try_parse_f64(val: &AstValue) -> Option<f64> {
	match val {
		AstValue::Scalar {
			value: ScalarValue::Number(n),
			..
		} => n.parse::<f64>().ok(),
		_ => None,
	}
}

/// Build a synthetic `AstValue::Scalar(Number)` reusing the span from a
/// reference value.
fn make_number_value(n: f64, reference_span: &AstValue) -> AstValue {
	let text = if n == n.floor() && n.abs() < 1e15 {
		format!("{}", n as i64)
	} else {
		format!("{n}")
	};
	AstValue::Scalar {
		value: ScalarValue::Number(text),
		span: reference_span.span().clone(),
	}
}

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
	// `patch_address` fingerprints `RemoveNode` / `InsertNode` (and lists)
	// with the body content so that two mods inserting different nodes under
	// the same key in a repeated-key block (e.g. multiple `has_country_flag`
	// entries inside an OR) coexist as distinct addresses. That fingerprinting
	// also has the side effect of separating same-(path, key) patches of
	// *different* kinds — for example `SetValue(owner)` lands at `(path, "owner")`
	// while `RemoveNode(owner)` lands at `(path, "__node__::owner::<fp>")`.
	// Without this pre-pass the per-address `has_mixed_kinds` check inside
	// `resolve_address` never fires for these split groups and the conflicting
	// intents (set vs remove the same key) silently both apply.
	//
	// Bucket by the kind-agnostic raw `(path, key)` and, when sibling mods
	// produced patches of multiple kinds, surface a single mixed-kinds
	// conflict that supersedes the per-fingerprint resolutions.
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
				apply_conflict_decision(&mut result, handler, address, patches, reason)?;
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
			cross_kind.address,
			cross_kind.patches,
			cross_kind.reason,
		)?;
	}

	Ok(result)
}

/// Cross-kind sibling conflict detected before per-address dispatch.
///
/// `split_addresses` lists every fingerprinted `PatchAddress` whose patches
/// fed into this conflict — the caller drops them from the per-address map so
/// they aren't double-resolved alongside the synthesized conflict.
struct CrossKindConflict {
	address: PatchAddress,
	patches: Vec<AttributedPatch>,
	reason: String,
	split_addresses: Vec<PatchAddress>,
}

fn detect_cross_kind_sibling_conflicts(
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

fn apply_conflict_decision(
	result: &mut PatchMergeResult,
	handler: &mut dyn ConflictHandler,
	address: PatchAddress,
	patches: Vec<AttributedPatch>,
	reason: String,
) -> Result<(), MergeError> {
	let conflict_path = conflict_path_for_handler(&address);
	let conflict = PatchConflict { patches, reason };

	match handler.on_conflict(&conflict_path, &address, &conflict) {
		ConflictDecision::Defer => result.conflicts.push(PatchResolution::Conflict {
			address,
			patches: conflict.patches,
			reason: conflict.reason,
		}),
		ConflictDecision::PickMod(mod_id) => {
			let Some(chosen) = conflict
				.patches
				.iter()
				.find(|patch| patch.mod_id == mod_id)
				.cloned()
			else {
				return Err(MergeError::Validation {
					path: Some(conflict_path),
					message: format!("conflict handler picked unknown mod `{mod_id}`"),
				});
			};
			result.handler_resolved_count += 1;
			result
				.resolved
				.push(PatchResolution::Resolved(chosen.patch));
		}
		ConflictDecision::PickModWithRecord { mod_id, record } => {
			let Some(chosen) = conflict
				.patches
				.iter()
				.find(|patch| patch.mod_id == mod_id)
				.cloned()
			else {
				return Err(MergeError::Validation {
					path: Some(conflict_path),
					message: format!("conflict handler picked unknown mod `{mod_id}`"),
				});
			};
			result.handler_resolved_count += 1;
			result.handler_resolutions.push(record);
			result
				.resolved
				.push(PatchResolution::Resolved(chosen.patch));
		}
		ConflictDecision::UseFile(source_path) => {
			result.handler_resolved_count += 1;
			result
				.external_file_resolutions
				.insert(PathBuf::from(conflict_path), source_path);
		}
		ConflictDecision::KeepExisting => {
			result.handler_resolved_count += 1;
			result
				.keep_existing_paths
				.insert(PathBuf::from(conflict_path));
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

// ---------------------------------------------------------------------------
// Per-address resolution
// ---------------------------------------------------------------------------

fn resolve_address(
	addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	policies: &MergePolicies,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	// --- Single mod case ---
	if attributed.len() == 1 {
		stats.single_mod_patches += 1;
		return PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch);
	}

	// --- Convergence: all patches semantically equal (ignoring spans and
	// comment/whitespace trivia) ---
	if attributed
		.windows(2)
		.all(|w| patches_semantically_equal(&w[0].patch, &w[1].patch))
	{
		stats.convergent_patches += 1;
		return PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch);
	}

	// --- Mixed patch kinds (e.g. InsertNode + RemoveNode) → conflict ---
	let kinds: Vec<&str> = attributed.iter().map(|a| patch_kind(&a.patch)).collect();
	let has_mixed_kinds = kinds.windows(2).any(|w| w[0] != w[1]);

	if has_mixed_kinds {
		stats.conflict_patches += 1;
		return PatchResolution::Conflict {
			address: addr,
			reason: format!("mixed patch kinds at same address: {}", kinds.join(", ")),
			patches: attributed,
		};
	}

	// From here on, all patches have the same variant kind.
	let kind = kinds[0];
	match kind {
		"InsertNode" => resolve_insert_nodes(addr, attributed, policies, stats),
		"AppendListItem" => resolve_append_list_items(addr, attributed, stats),
		"SetValue" => resolve_set_values(addr, attributed, policies, stats),
		"RemoveNode" => resolve_remove_convergent(addr, attributed, stats),
		"RemoveListItem" => resolve_remove_list_items(addr, attributed, stats),
		"ReplaceBlock" => resolve_replace_blocks(addr, attributed, policies, stats),
		"AppendBlockItem" => resolve_append_block_items(addr, attributed, stats),
		"RemoveBlockItem" => resolve_remove_block_items(addr, attributed, stats),
		"Rename" => resolve_renames(addr, attributed, stats),
		_ => {
			stats.conflict_patches += 1;
			PatchResolution::Conflict {
				address: addr,
				reason: format!("unhandled patch kind: {kind}"),
				patches: attributed,
			}
		}
	}
}

// ---------------------------------------------------------------------------
// Kind-specific resolvers
// ---------------------------------------------------------------------------

/// Multiple mods inserting new nodes at the same path with different keys →
/// compatible if keys differ (both apply). Conflict if same key with
/// different statements.
fn resolve_insert_nodes(
	addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	policies: &MergePolicies,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	// Extract the inserted statements.
	let stmts: Vec<_> = attributed
		.iter()
		.map(|a| match &a.patch {
			ClausewitzPatch::InsertNode { statement, .. } => statement,
			_ => unreachable!(),
		})
		.collect();

	// If all statements are semantically equal → convergent.
	if stmts
		.windows(2)
		.all(|w| super::patch::ast_statements_semantically_equal(w[0], w[1]))
	{
		stats.convergent_patches += 1;
		return PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch);
	}

	// BooleanOr policy: when each contributor inserts a block-bodied
	// statement under the same key, wrap each body inside `OR = { ... }`
	// and emit a single synthesized InsertNode.
	if policies.block_patch_policy_for_key(&addr.key) == BlockPatchPolicy::BooleanOr
		&& let Some(synth) = synthesize_boolean_or(&addr, &attributed)
	{
		let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
		stats.auto_merged_patches += 1;
		return PatchResolution::AutoMerged {
			result: synth,
			strategy: "boolean_or".to_string(),
			contributing_mods: mods,
		};
	}

	// Different statements → all insertions can coexist (they add distinct
	// content at the same path).
	let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
	// Pick the highest-precedence mod's patch as the "primary" result but
	// record all contributing mods.
	let mut sorted = attributed;
	sorted.sort_by(|a, b| b.precedence.cmp(&a.precedence));
	stats.auto_merged_patches += 1;
	PatchResolution::AutoMerged {
		result: sorted.into_iter().next().unwrap().patch,
		strategy: "compatible_inserts".to_string(),
		contributing_mods: mods,
	}
}

/// Multiple mods appending items to the same list → union (dedup identical
/// values, keep all distinct ones).
fn resolve_append_list_items(
	_addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	// Collect all appended values.
	let values: Vec<&AstValue> = attributed
		.iter()
		.map(|a| match &a.patch {
			ClausewitzPatch::AppendListItem { value, .. } => value,
			_ => unreachable!(),
		})
		.collect();

	// If all values are semantically equal → convergent (dedup to one).
	if values
		.windows(2)
		.all(|w| super::patch::ast_values_semantically_equal(w[0], w[1]))
	{
		stats.convergent_patches += 1;
		return PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch);
	}

	// Different values → union: keep all distinct items. Use the highest-
	// precedence patch as the representative result.
	let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
	let mut sorted = attributed;
	sorted.sort_by(|a, b| b.precedence.cmp(&a.precedence));
	stats.auto_merged_patches += 1;
	PatchResolution::AutoMerged {
		result: sorted.into_iter().next().unwrap().patch,
		strategy: "list_union".to_string(),
		contributing_mods: mods,
	}
}

/// Multiple mods setting the same scalar to different values. Resolve via
/// `policies.scalar`.
fn resolve_set_values(
	addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	policies: &MergePolicies,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	match policies.scalar {
		ScalarMergePolicy::Conflict => {
			// Sibling mods at the same scalar leaf cannot be silently
			// merged: there is no dependency-graph signal saying which
			// mod's value should win. Surface a conflict so the user (or
			// `[[resolutions]]`) can pick.
			let new_values: Vec<String> = attributed
				.iter()
				.map(|a| match &a.patch {
					ClausewitzPatch::SetValue { new_value, .. } => {
						format!("`{}` from `{}`", value_text_for_reason(new_value), a.mod_id)
					}
					_ => unreachable!(),
				})
				.collect();
			stats.conflict_patches += 1;
			PatchResolution::Conflict {
				address: addr,
				reason: format!(
					"sibling mods set the same scalar to divergent values: {}",
					new_values.join(", ")
				),
				patches: attributed,
			}
		}
		ScalarMergePolicy::Sum
		| ScalarMergePolicy::Avg
		| ScalarMergePolicy::Max
		| ScalarMergePolicy::Min => resolve_numeric_set_values(addr, attributed, policies.scalar, stats),
	}
}

/// Best-effort textual rendering of an `AstValue` for human-readable conflict
/// messages. Falls back to a structural marker when the value is a block.
fn value_text_for_reason(value: &AstValue) -> String {
	match value {
		AstValue::Scalar { value, .. } => value.as_text(),
		AstValue::Block { .. } => "<block>".to_string(),
	}
}

fn resolve_numeric_set_values(
	addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	policy: ScalarMergePolicy,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	// Collect all new_values (owned) so we don't hold borrows across the move.
	let new_values: Vec<AstValue> = attributed
		.iter()
		.map(|a| match &a.patch {
			ClausewitzPatch::SetValue { new_value, .. } => new_value.clone(),
			_ => unreachable!(),
		})
		.collect();

	let parsed: Vec<Option<f64>> = new_values.iter().map(try_parse_f64).collect();

	if parsed.iter().any(|p| p.is_none()) {
		stats.conflict_patches += 1;
		return PatchResolution::Conflict {
			address: addr,
			reason: format!(
				"numeric merge policy {:?} but not all values are numeric",
				policy
			),
			patches: attributed,
		};
	}

	let nums: Vec<f64> = parsed.into_iter().map(|p| p.unwrap()).collect();
	let merged = match policy {
		ScalarMergePolicy::Sum => nums.iter().sum(),
		ScalarMergePolicy::Avg => nums.iter().sum::<f64>() / nums.len() as f64,
		ScalarMergePolicy::Max => nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
		ScalarMergePolicy::Min => nums.iter().cloned().fold(f64::INFINITY, f64::min),
		_ => unreachable!(),
	};

	let merged_value = make_number_value(merged, &new_values[0]);
	let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
	let first = attributed.into_iter().next().unwrap();
	let strategy = format!("{policy:?}");

	let result = match first.patch {
		ClausewitzPatch::SetValue {
			path,
			key,
			old_value,
			..
		} => ClausewitzPatch::SetValue {
			path,
			key,
			old_value,
			new_value: merged_value,
		},
		_ => unreachable!(),
	};

	stats.auto_merged_patches += 1;
	PatchResolution::AutoMerged {
		result,
		strategy,
		contributing_mods: mods,
	}
}

/// Both mods remove the same node → convergent (apply once).
fn resolve_remove_convergent(
	_addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	// All are RemoveNode at same address. Even if the removed statement
	// differs slightly (span), the intent is the same.
	stats.convergent_patches += 1;
	PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch)
}

/// Multiple mods removing different items from a list.
fn resolve_remove_list_items(
	_addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	let values: Vec<&AstValue> = attributed
		.iter()
		.map(|a| match &a.patch {
			ClausewitzPatch::RemoveListItem { value, .. } => value,
			_ => unreachable!(),
		})
		.collect();

	// Identical removals → convergent.
	if values
		.windows(2)
		.all(|w| super::patch::ast_values_semantically_equal(w[0], w[1]))
	{
		stats.convergent_patches += 1;
		return PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch);
	}

	// Different items being removed → both apply.
	let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
	let mut sorted = attributed;
	sorted.sort_by(|a, b| b.precedence.cmp(&a.precedence));
	stats.auto_merged_patches += 1;
	PatchResolution::AutoMerged {
		result: sorted.into_iter().next().unwrap().patch,
		strategy: "compatible_removals".to_string(),
		contributing_mods: mods,
	}
}

/// Multiple mods appending the same in-block Item value (e.g. both add `TRE`
/// to `allowed_tags`). Because `patch_address` includes the value fingerprint,
/// every patch in this group has identical `value` → always convergent.
fn resolve_append_block_items(
	_addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	stats.convergent_patches += 1;
	PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch)
}

/// Multiple mods removing the same in-block Item value. Same convergence
/// reasoning as `resolve_append_block_items`.
fn resolve_remove_block_items(
	_addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	stats.convergent_patches += 1;
	PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch)
}

/// Multiple mods replacing the same block → if `BlockPatchPolicy::BooleanOr`,
/// wrap each contributor's body in `OR = { ... }` and emit a synthesized
/// `ReplaceBlock`. Otherwise try a named-container 3-way merge. Fall back to
/// a conflict when neither strategy applies.
fn resolve_replace_blocks(
	addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	policies: &MergePolicies,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	// Defensive: although `resolve_address` already collapses semantically
	// equal patches, callers may invoke this resolver directly. Treat
	// format-only divergence (whitespace / comment trivia) as convergent.
	if attributed
		.windows(2)
		.all(|w| patches_semantically_equal(&w[0].patch, &w[1].patch))
	{
		let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
		stats.auto_merged_patches += 1;
		return PatchResolution::AutoMerged {
			result: attributed.into_iter().next().unwrap().patch,
			strategy: "semantic_equivalence".to_string(),
			contributing_mods: mods,
		};
	}

	let block_policy = policies.block_patch_policy_for_key(&addr.key);
	if block_policy == BlockPatchPolicy::BooleanOr
		&& let Some(synth) = synthesize_boolean_or(&addr, &attributed)
	{
		let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
		stats.auto_merged_patches += 1;
		return PatchResolution::AutoMerged {
			result: synth,
			strategy: "boolean_or".to_string(),
			contributing_mods: mods,
		};
	}

	if block_policy == BlockPatchPolicy::Union
		&& let Some(merged) = try_union_block_merge(&attributed)
	{
		let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
		stats.auto_merged_patches += 1;
		return PatchResolution::AutoMerged {
			result: merged,
			strategy: "union_block".to_string(),
			contributing_mods: mods,
		};
	}

	if block_policy == BlockPatchPolicy::Recurse
		&& let Some(resolution) = try_recursive_block_merge(&addr, &attributed, policies, stats)
	{
		return resolution;
	}

	if let Some(merged) = try_replace_block_named_container_merge(&attributed, policies) {
		let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
		stats.auto_merged_patches += 1;
		return PatchResolution::AutoMerged {
			result: merged,
			strategy: "named_container_union".to_string(),
			contributing_mods: mods,
		};
	}

	// Identical or semantically-equivalent replacements were already caught
	// above, so reaching here means different replacements → conflict.
	stats.conflict_patches += 1;
	PatchResolution::Conflict {
		address: addr,
		reason: "multiple mods replace the same block with different content".to_string(),
		patches: attributed,
	}
}

/// Attempt to union list-like block replacements by keeping the base body's
/// first occurrence of each item, then appending unique items from every
/// replacement body in precedence order.
fn try_union_block_merge(attributed: &[AttributedPatch]) -> Option<ClausewitzPatch> {
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
	match item {
		AstStatement::Item { value, .. } => value_fingerprint(value),
		AstStatement::Assignment { key, value, .. } => {
			let mut out = String::new();
			out.push('a');
			out.push_str(key);
			out.push('=');
			fingerprint_into(value, &mut out);
			out
		}
		AstStatement::Comment { .. } => statement_fingerprint(item),
	}
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
fn try_recursive_block_merge(
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
		let patches = super::patch::diff_block_bodies(
			ancestor_body,
			new_body,
			&[],
			0,
			MergeKeySource::AssignmentKey,
		);
		mod_patches.push((mod_id.clone(), *prec, patches));
	}

	// Recursively resolve nested patches with the same policies.
	let mut handler = DeferHandler;
	let nested = merge_patch_sets(mod_patches, policies, &mut handler).ok()?;

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
	let resolved_patches: Vec<ClausewitzPatch> = nested
		.resolved
		.into_iter()
		.filter_map(|r| match r {
			PatchResolution::Resolved(p) => Some(p),
			PatchResolution::AutoMerged { result, .. } => Some(result),
			PatchResolution::Conflict { .. } => None,
		})
		.collect();

	let merged_body = super::patch_apply::apply_patches(
		ancestor_body,
		&resolved_patches,
		MergeKeySource::AssignmentKey,
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

	let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
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

/// Attempt named-container merge across N mod ReplaceBlock patches at the same
/// address. Returns the merged ReplaceBlock if applicable, else `None`.
fn try_replace_block_named_container_merge(
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
	ordered.sort_by(|a, b| a.precedence.cmp(&b.precedence));

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
/// inner `name = "..."` field (preferred) or to its assignment key (fallback).
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
	for (i, stmt) in result.iter().enumerate() {
		if let Some(id) = child_identity(stmt) {
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
								// Sibling mods target the same named identity
								// with bodies that cannot be merged structurally
								// → defer to the user instead of silently
								// renaming or overwriting.
								return Err(NamedContainerMergeError::UnresolvableConflict);
							}
							NamedContainerPolicy::OverlayWins => {
								result[idx] = stmt.clone();
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

/// Build an `OR = { <items...> }` statement that wraps the supplied body.
fn make_or_wrapper(items: Vec<AstStatement>) -> AstStatement {
	AstStatement::Assignment {
		key: "OR".to_string(),
		key_span: synthetic_span(),
		value: AstValue::Block {
			items,
			span: synthetic_span(),
		},
		span: synthetic_span(),
	}
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

/// Synthesize a single patch whose body is `{ OR = { body_0 } OR = { body_1 } ... }`,
/// preserving the original key. Returns `None` (forcing fallback to the
/// caller's default behavior) if any contributor isn't a block-bodied
/// assignment.
///
/// One `OR =` wrapper is emitted per contributor in `attributed`'s order.
/// No cross-contributor deduplication is performed: even byte-identical
/// bodies (which would have already short-circuited via the convergence
/// check upstream) are treated as separate disjuncts here, matching the
/// caller's contract that `attributed.len() >= 2` and the bodies differ.
fn synthesize_boolean_or(
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

	let or_blocks: Vec<AstStatement> = bodies.into_iter().map(make_or_wrapper).collect();

	let synthesized_value = AstValue::Block {
		items: or_blocks,
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
/// * `ContainerChildFieldValue` — rename the child assignment's inner identity
///   field when present (e.g. `name = widget` → `name = widget_mod_a`).
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Rename pre-pass helpers
// ---------------------------------------------------------------------------

fn build_rename_map(
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

fn rewrite_patch_for_renames(
	mut patch: ClausewitzPatch,
	rename_map: &HashMap<(AstPath, String), String>,
) -> ClausewitzPatch {
	if matches!(patch, ClausewitzPatch::Rename { .. }) {
		return patch;
	}
	if rename_map.is_empty() {
		return patch;
	}
	loop {
		let path = rn_patch_path_clone(&patch);
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
fn resolve_renames(
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::merge::conflict_handler::{ChainHandler, LookupHandler};
	use foch_core::config::{ResolutionDecision, ResolutionMap, compute_conflict_id};
	use foch_core::model::HandlerResolutionRecord;
	use foch_language::analyzer::content_family::MergePolicies;
	use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};

	fn span() -> SpanRange {
		SpanRange {
			start: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
			end: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
		}
	}

	fn scalar(s: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::Identifier(s.to_string()),
			span: span(),
		}
	}

	fn number(n: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::Number(n.to_string()),
			span: span(),
		}
	}

	fn assignment(key: &str, val: AstValue) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: span(),
			value: val,
			span: span(),
		}
	}

	fn default_policies() -> MergePolicies {
		MergePolicies::default()
	}

	fn merge_patch_sets_with_defer(
		mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
		policies: &MergePolicies,
	) -> PatchMergeResult {
		let mut handler = DeferHandler;
		merge_patch_sets(mod_patches, policies, &mut handler)
			.expect("defer handler should not abort")
	}

	#[test]
	fn merge_patch_sets_with_defer_handler_preserves_current_behavior() {
		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("alpha")),
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("beta")),
		};

		let mut handler = DeferHandler;
		let result = merge_patch_sets(
			vec![
				("mod_a".into(), 1, vec![patch_a.clone()]),
				("mod_b".into(), 2, vec![patch_b.clone()]),
			],
			&default_policies(),
			&mut handler,
		)
		.expect("defer handler should not abort");

		let expected = PatchMergeResult {
			conflicts: vec![PatchResolution::Conflict {
				address: PatchAddress {
					path: vec!["root".into()],
					key: "decisions".into(),
				},
				patches: vec![
					AttributedPatch {
						mod_id: "mod_a".into(),
						precedence: 1,
						patch: patch_a,
					},
					AttributedPatch {
						mod_id: "mod_b".into(),
						precedence: 2,
						patch: patch_b,
					},
				],
				reason: "multiple mods replace the same block with different content".into(),
			}],
			stats: PatchMergeStats {
				total_patches: 2,
				conflict_patches: 1,
				..PatchMergeStats::default()
			},
			..PatchMergeResult::default()
		};
		assert_eq!(result, expected);
	}

	#[test]
	fn merge_patch_sets_with_resolution_picks_correct_mod_patch() {
		let current_file = PathBuf::from("common/ideas/resolved.txt");
		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("alpha")),
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("beta")),
		};
		let conflict_id = compute_conflict_id(&current_file, "root", "decisions");
		let mut resolution_map = ResolutionMap::default();
		resolution_map.by_conflict_id.insert(
			conflict_id,
			ResolutionDecision::PreferMod("mod_a".to_string()),
		);
		let mut handler = LookupHandler::new(&resolution_map, current_file);

		let result = merge_patch_sets(
			vec![
				("mod_a".into(), 1, vec![patch_a.clone()]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
			&mut handler,
		)
		.expect("resolution map should pick mod_a");

		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.handler_resolved_count, 1);
		assert_eq!(result.resolved, vec![PatchResolution::Resolved(patch_a)]);
		assert_eq!(result.stats.conflict_patches, 1);
	}

	#[test]
	fn merge_patch_sets_records_pick_mod_handler_metadata() {
		struct MockRecordedPickHandler;

		impl ConflictHandler for MockRecordedPickHandler {
			fn on_conflict(
				&mut self,
				_: &str,
				_: &PatchAddress,
				_: &PatchConflict,
			) -> ConflictDecision {
				ConflictDecision::PickModWithRecord {
					mod_id: "mod_b".to_string(),
					record: HandlerResolutionRecord {
						path: "common/ideas/dep.txt".to_string(),
						action: "dep_implied".to_string(),
						source: Some("mod_b".to_string()),
						rationale: Some("mod mod_b declares dep on mod_a".to_string()),
					},
				}
			}
		}

		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("alpha")),
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("beta")),
		};
		let mut handler = MockRecordedPickHandler;

		let result = merge_patch_sets(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b.clone()]),
			],
			&default_policies(),
			&mut handler,
		)
		.expect("mock recorded pick should not abort");

		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.handler_resolved_count, 1);
		assert_eq!(result.resolved, vec![PatchResolution::Resolved(patch_b)]);
		assert_eq!(result.handler_resolutions.len(), 1);
		assert_eq!(result.handler_resolutions[0].action, "dep_implied");
		assert_eq!(
			result.handler_resolutions[0].rationale.as_deref(),
			Some("mod mod_b declares dep on mod_a")
		);
	}

	#[test]
	fn chain_handler_falls_through_to_second_on_defer() {
		struct MockPickHandler;

		impl ConflictHandler for MockPickHandler {
			fn on_conflict(
				&mut self,
				_: &str,
				_: &PatchAddress,
				_: &PatchConflict,
			) -> ConflictDecision {
				ConflictDecision::PickMod("mod_b".to_string())
			}
		}

		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("alpha")),
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("beta")),
		};

		let mut handler = ChainHandler {
			first: DeferHandler,
			second: MockPickHandler,
		};
		let result = merge_patch_sets(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b.clone()]),
			],
			&default_policies(),
			&mut handler,
		)
		.expect("mock pick handler should not abort");

		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.handler_resolved_count, 1);
		assert_eq!(result.resolved, vec![PatchResolution::Resolved(patch_b)]);
	}

	#[test]
	fn single_mod_patches_all_resolved() {
		let patches = vec![
			ClausewitzPatch::SetValue {
				path: vec!["root".into()],
				key: "tax".into(),
				old_value: number("5"),
				new_value: number("10"),
			},
			ClausewitzPatch::InsertNode {
				path: vec!["root".into()],
				key: "new_key".into(),
				statement: assignment("new_key", scalar("val")),
			},
		];

		let result =
			merge_patch_sets_with_defer(vec![("mod_a".into(), 1, patches)], &default_policies());

		assert_eq!(result.resolved.len(), 2);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.stats.total_patches, 2);
		assert_eq!(result.stats.single_mod_patches, 2);
	}

	#[test]
	fn identical_patches_convergent() {
		let patch = ClausewitzPatch::SetValue {
			path: vec!["root".into()],
			key: "tax".into(),
			old_value: number("5"),
			new_value: number("10"),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch.clone()]),
				("mod_b".into(), 2, vec![patch]),
			],
			&default_policies(),
		);

		assert_eq!(result.resolved.len(), 1);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.stats.convergent_patches, 1);
	}

	#[test]
	fn different_insert_nodes_independent_addresses() {
		// Two mods inserting the same key with different bodies — e.g.
		// `has_country_flag = a` and `has_country_flag = b` into the same
		// `OR` block — now get distinct addresses thanks to the body
		// fingerprint, so both apply independently instead of one being
		// silently dropped via "compatible_inserts" highest-precedence
		// selection.
		let patch_a = ClausewitzPatch::InsertNode {
			path: vec!["root".into()],
			key: "ideas".into(),
			statement: assignment("ideas", scalar("alpha")),
		};
		let patch_b = ClausewitzPatch::InsertNode {
			path: vec!["root".into()],
			key: "ideas".into(),
			statement: assignment("ideas", scalar("beta")),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		assert_eq!(result.resolved.len(), 2);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.stats.single_mod_patches, 2);
	}

	#[test]
	fn same_append_list_item_deduplicated() {
		let patch = ClausewitzPatch::AppendListItem {
			path: vec!["root".into(), "or".into()],
			key: "tag".into(),
			value: scalar("ERS"),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch.clone()]),
				("mod_b".into(), 2, vec![patch]),
			],
			&default_policies(),
		);

		assert_eq!(result.resolved.len(), 1);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.stats.convergent_patches, 1);
	}

	#[test]
	fn different_append_list_items_independent_addresses() {
		let patch_a = ClausewitzPatch::AppendListItem {
			path: vec!["root".into(), "or".into()],
			key: "tag".into(),
			value: scalar("ERS"),
		};
		let patch_b = ClausewitzPatch::AppendListItem {
			path: vec!["root".into(), "or".into()],
			key: "tag".into(),
			value: scalar("FRA"),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		// Distinct values land in independent address buckets and apply as
		// single-mod patches; no conflict and no auto-merge needed.
		assert_eq!(result.resolved.len(), 2);
		assert_eq!(result.conflicts.len(), 0);
	}

	#[test]
	fn different_set_value_sibling_conflict() {
		// Sibling mods (no dependency edge) writing the same scalar leaf to
		// divergent values must surface a manual conflict — there is no
		// dependency-graph signal saying which value should win.
		let patch_a = ClausewitzPatch::SetValue {
			path: vec!["root".into()],
			key: "tax".into(),
			old_value: number("5"),
			new_value: number("10"),
		};
		let patch_b = ClausewitzPatch::SetValue {
			path: vec!["root".into()],
			key: "tax".into(),
			old_value: number("5"),
			new_value: number("15"),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		assert_eq!(result.resolved.len(), 0);
		assert_eq!(result.conflicts.len(), 1);
		assert_eq!(result.stats.conflict_patches, 1);
		match &result.conflicts[0] {
			PatchResolution::Conflict { reason, .. } => {
				assert!(
					reason.contains("sibling mods set the same scalar"),
					"unexpected reason: {reason}"
				);
				assert!(reason.contains("10"));
				assert!(reason.contains("15"));
			}
			other => panic!("expected Conflict, got: {other:?}"),
		}
	}

	#[test]
	fn conflicting_replace_blocks() {
		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("alpha")),
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "decisions".into(),
			old_statement: assignment("decisions", scalar("old")),
			new_statement: assignment("decisions", scalar("beta")),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		assert_eq!(result.resolved.len(), 0);
		assert_eq!(result.conflicts.len(), 1);
		assert_eq!(result.stats.conflict_patches, 1);
		match &result.conflicts[0] {
			PatchResolution::Conflict {
				reason, patches, ..
			} => {
				assert!(reason.contains("replace the same block"));
				assert_eq!(patches.len(), 2);
			}
			other => panic!("expected Conflict, got: {other:?}"),
		}
	}

	#[test]
	fn stats_are_correctly_tracked() {
		// Mix of single, convergent, auto-merged, and conflict patches.
		let single = ClausewitzPatch::InsertNode {
			path: vec!["root".into()],
			key: "unique".into(),
			statement: assignment("unique", scalar("only_a")),
		};
		let convergent = ClausewitzPatch::RemoveNode {
			path: vec!["root".into()],
			key: "old_key".into(),
			removed: assignment("old_key", scalar("gone")),
		};
		let conflict_a = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "block".into(),
			old_statement: assignment("block", scalar("old")),
			new_statement: assignment("block", scalar("a_ver")),
		};
		let conflict_b = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "block".into(),
			old_statement: assignment("block", scalar("old")),
			new_statement: assignment("block", scalar("b_ver")),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				(
					"mod_a".into(),
					1,
					vec![single, convergent.clone(), conflict_a],
				),
				("mod_b".into(), 2, vec![convergent, conflict_b]),
			],
			&default_policies(),
		);

		assert_eq!(result.stats.total_patches, 5);
		assert_eq!(result.stats.single_mod_patches, 1);
		assert_eq!(result.stats.convergent_patches, 1);
		assert_eq!(result.stats.conflict_patches, 1);
		assert_eq!(result.resolved.len(), 2); // single + convergent
		assert_eq!(result.conflicts.len(), 1);
	}

	#[test]
	fn mixed_kinds_at_same_address_conflict() {
		// Insert + Remove at the same `(path, key)` AND same body still
		// share a `PatchAddress` (the body fingerprint is included for
		// both kinds), so a real Insert-X / Remove-X collision continues
		// to surface as a mixed-kind conflict.
		let insert = ClausewitzPatch::InsertNode {
			path: vec!["root".into()],
			key: "thing".into(),
			statement: assignment("thing", scalar("same")),
		};
		let remove = ClausewitzPatch::RemoveNode {
			path: vec!["root".into()],
			key: "thing".into(),
			removed: assignment("thing", scalar("same")),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![insert]),
				("mod_b".into(), 2, vec![remove]),
			],
			&default_policies(),
		);

		assert_eq!(result.conflicts.len(), 1);
		assert_eq!(result.stats.conflict_patches, 1);
		match &result.conflicts[0] {
			PatchResolution::Conflict { reason, .. } => {
				assert!(reason.contains("mixed patch kinds"));
			}
			other => panic!("expected Conflict, got: {other:?}"),
		}
	}

	#[test]
	fn rename_for_conflict_assignment_key_appends_mod_suffix() {
		let stmt = assignment(
			"pragmatic_sanction",
			AstValue::Block {
				items: vec![assignment("potential", scalar("yes"))],
				span: span(),
			},
		);

		let renamed = rename_for_conflict(&stmt, MergeKeySource::AssignmentKey, "mod_a");

		match renamed {
			AstStatement::Assignment { key, value, .. } => {
				assert_eq!(key, "pragmatic_sanction_mod_a");
				// Body is preserved as-is.
				match value {
					AstValue::Block { items, .. } => {
						assert_eq!(items.len(), 1);
						assert!(matches!(
							&items[0],
							AstStatement::Assignment { key, .. } if key == "potential"
						));
					}
					_ => panic!("expected block body"),
				}
			}
			_ => panic!("expected Assignment"),
		}

		// ContainerChildKey behaves identically (renames the top-level key).
		let renamed_container =
			rename_for_conflict(&stmt, MergeKeySource::ContainerChildKey, "mod_b");
		match renamed_container {
			AstStatement::Assignment { key, .. } => {
				assert_eq!(key, "pragmatic_sanction_mod_b");
			}
			_ => panic!("expected Assignment"),
		}
	}

	#[test]
	fn rename_for_conflict_field_value_renames_inner_id() {
		let stmt = assignment(
			"country_event",
			AstValue::Block {
				items: vec![
					assignment("id", scalar("test.1")),
					assignment("title", scalar("evt_title")),
				],
				span: span(),
			},
		);

		let renamed = rename_for_conflict(&stmt, MergeKeySource::FieldValue("id"), "mod_a");

		match renamed {
			AstStatement::Assignment { key, value, .. } => {
				// Outer key is unchanged.
				assert_eq!(key, "country_event");
				match value {
					AstValue::Block { items, .. } => {
						assert_eq!(items.len(), 2);
						// The `id` field has been renamed.
						match &items[0] {
							AstStatement::Assignment {
								key: ikey,
								value:
									AstValue::Scalar {
										value: ScalarValue::Identifier(v),
										..
									},
								..
							} => {
								assert_eq!(ikey, "id");
								assert_eq!(v, "test.1_mod_a");
							}
							other => panic!("expected scalar id field, got {other:?}"),
						}
						// Other fields are untouched.
						match &items[1] {
							AstStatement::Assignment {
								key: ikey,
								value:
									AstValue::Scalar {
										value: ScalarValue::Identifier(v),
										..
									},
								..
							} => {
								assert_eq!(ikey, "title");
								assert_eq!(v, "evt_title");
							}
							other => panic!("expected scalar title field, got {other:?}"),
						}
					}
					_ => panic!("expected block body"),
				}
			}
			_ => panic!("expected Assignment"),
		}
	}

	#[test]
	fn rename_for_conflict_leaf_path_returns_unchanged_or_lastwriter() {
		// LeafPath identities are the dotted path itself, which cannot be
		// suffix-renamed without changing semantics. The helper must return
		// the statement unchanged so callers fall back to last-writer.
		let stmt = assignment("NGame.START_YEAR", scalar("1444"));
		let renamed = rename_for_conflict(&stmt, MergeKeySource::LeafPath, "mod_a");
		assert_eq!(renamed, stmt);

		// Comments and items are similarly left alone for any key source.
		let comment = AstStatement::Comment {
			text: "# header".to_string(),
			span: span(),
		};
		assert_eq!(
			rename_for_conflict(&comment, MergeKeySource::AssignmentKey, "mod_a"),
			comment
		);
	}

	#[test]
	fn numeric_sum_policy() {
		let policies = MergePolicies {
			scalar: ScalarMergePolicy::Sum,
			..Default::default()
		};
		let patch_a = ClausewitzPatch::SetValue {
			path: vec!["root".into()],
			key: "bonus".into(),
			old_value: number("0"),
			new_value: number("5"),
		};
		let patch_b = ClausewitzPatch::SetValue {
			path: vec!["root".into()],
			key: "bonus".into(),
			old_value: number("0"),
			new_value: number("3"),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&policies,
		);

		assert_eq!(result.resolved.len(), 1);
		assert_eq!(result.stats.auto_merged_patches, 1);
		match &result.resolved[0] {
			PatchResolution::AutoMerged {
				result: patch,
				strategy,
				..
			} => {
				assert_eq!(strategy, "Sum");
				match patch {
					ClausewitzPatch::SetValue { new_value, .. } => {
						assert_eq!(*new_value, number("8"));
					}
					_ => panic!("expected SetValue"),
				}
			}
			other => panic!("expected AutoMerged, got: {other:?}"),
		}
	}

	fn block_value(items: Vec<AstStatement>) -> AstValue {
		AstValue::Block {
			items,
			span: span(),
		}
	}

	fn assignment_block(key: &str, items: Vec<AstStatement>) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: span(),
			value: block_value(items),
			span: span(),
		}
	}

	fn boolean_or_policies() -> MergePolicies {
		MergePolicies {
			block_patch: BlockPatchPolicy::BooleanOr,
			..Default::default()
		}
	}

	const COUNTRY_NAME_BLOCK_POLICIES: &[(&str, BlockPatchPolicy)] = &[
		("monarch_names", BlockPatchPolicy::Union),
		("leader_names", BlockPatchPolicy::Union),
		("ship_names", BlockPatchPolicy::Union),
		("army_names", BlockPatchPolicy::Union),
	];

	fn union_policies() -> MergePolicies {
		MergePolicies {
			block_patch: BlockPatchPolicy::Union,
			..Default::default()
		}
	}

	fn country_history_name_union_policies() -> MergePolicies {
		MergePolicies {
			block_patch: BlockPatchPolicy::Recurse,
			block_patch_policies: COUNTRY_NAME_BLOCK_POLICIES,
			..Default::default()
		}
	}

	fn bare_item(value: &str) -> AstStatement {
		AstStatement::Item {
			value: scalar(value),
			span: span(),
		}
	}

	fn replace_block_patch(
		key: &str,
		old_items: Vec<AstStatement>,
		new_items: Vec<AstStatement>,
	) -> ClausewitzPatch {
		ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: key.to_string(),
			old_statement: assignment_block(key, old_items),
			new_statement: assignment_block(key, new_items),
		}
	}

	fn block_items(stmt: &AstStatement) -> &[AstStatement] {
		match stmt {
			AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} => items,
			other => panic!("expected block assignment, got {other:?}"),
		}
	}

	fn item_texts(items: &[AstStatement]) -> Vec<String> {
		items
			.iter()
			.map(|item| match item {
				AstStatement::Item {
					value: AstValue::Scalar { value, .. },
					..
				} => value.as_text(),
				other => panic!("expected scalar item, got {other:?}"),
			})
			.collect()
	}

	fn assignment_keys(items: &[AstStatement]) -> Vec<String> {
		items
			.iter()
			.map(|item| match item {
				AstStatement::Assignment { key, .. } => key.clone(),
				other => panic!("expected assignment item, got {other:?}"),
			})
			.collect()
	}

	/// Helper: assert `stmt` is `key = { OR = { <body_0> } OR = { <body_1> } ... }`
	/// with the supplied bodies in order, and return the OR'd bodies for further
	/// inspection.
	fn assert_or_wrapped(stmt: &AstStatement, expected_key: &str) -> Vec<Vec<AstStatement>> {
		let (key, items) = match stmt {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} => (key.as_str(), items.as_slice()),
			other => panic!("expected Assignment with Block value, got: {other:?}"),
		};
		assert_eq!(key, expected_key, "outer key mismatch");
		items
			.iter()
			.map(|child| match child {
				AstStatement::Assignment {
					key,
					value: AstValue::Block { items, .. },
					..
				} => {
					assert_eq!(key, "OR", "expected OR wrapper, got key={key}");
					items.clone()
				}
				other => panic!("expected `OR = {{ ... }}`, got: {other:?}"),
			})
			.collect()
	}

	#[test]
	fn boolean_or_two_mods_modify_same_block_produces_or_or() {
		let body_a = vec![assignment("tag", scalar("ABC"))];
		let body_b = vec![assignment("culture", scalar("french"))];
		let old = assignment_block("is_great_power", vec![assignment("tag", scalar("OLD"))]);

		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "is_great_power".into(),
			old_statement: old.clone(),
			new_statement: assignment_block("is_great_power", body_a.clone()),
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "is_great_power".into(),
			old_statement: old,
			new_statement: assignment_block("is_great_power", body_b.clone()),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&boolean_or_policies(),
		);

		assert_eq!(result.resolved.len(), 1);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.stats.auto_merged_patches, 1);

		let merged_stmt = match &result.resolved[0] {
			PatchResolution::AutoMerged {
				result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
				strategy,
				contributing_mods,
			} => {
				assert_eq!(strategy, "boolean_or");
				assert_eq!(contributing_mods.len(), 2);
				new_statement
			}
			other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
		};

		let or_bodies = assert_or_wrapped(merged_stmt, "is_great_power");
		assert_eq!(or_bodies.len(), 2);
		assert_eq!(or_bodies[0], body_a);
		assert_eq!(or_bodies[1], body_b);
	}

	#[test]
	fn boolean_or_three_mods_produces_three_or_blocks() {
		let body_a = vec![assignment("tag", scalar("AAA"))];
		let body_b = vec![assignment("tag", scalar("BBB"))];
		let body_c = vec![assignment("tag", scalar("CCC"))];

		let mk = |body: &[AstStatement]| ClausewitzPatch::InsertNode {
			path: vec![],
			key: "is_powerful".into(),
			statement: assignment_block("is_powerful", body.to_vec()),
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![mk(&body_a)]),
				("mod_b".into(), 2, vec![mk(&body_b)]),
				("mod_c".into(), 3, vec![mk(&body_c)]),
			],
			&boolean_or_policies(),
		);

		assert_eq!(result.resolved.len(), 1);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.stats.auto_merged_patches, 1);

		let merged_stmt = match &result.resolved[0] {
			PatchResolution::AutoMerged {
				result: ClausewitzPatch::InsertNode { statement, .. },
				strategy,
				..
			} => {
				assert_eq!(strategy, "boolean_or");
				statement
			}
			other => panic!("expected AutoMerged InsertNode, got: {other:?}"),
		};

		let or_bodies = assert_or_wrapped(merged_stmt, "is_powerful");
		assert_eq!(or_bodies.len(), 3);
		assert_eq!(or_bodies[0], body_a);
		assert_eq!(or_bodies[1], body_b);
		assert_eq!(or_bodies[2], body_c);
	}

	#[test]
	fn boolean_or_single_modification_no_or_wrap() {
		let body = vec![assignment("tag", scalar("XYZ"))];
		let patch = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "is_lonely".into(),
			old_statement: assignment_block("is_lonely", vec![]),
			new_statement: assignment_block("is_lonely", body.clone()),
		};

		let result = merge_patch_sets_with_defer(
			vec![("mod_a".into(), 1, vec![patch])],
			&boolean_or_policies(),
		);

		assert_eq!(result.resolved.len(), 1);
		assert_eq!(result.stats.single_mod_patches, 1);
		assert_eq!(result.stats.auto_merged_patches, 0);

		match &result.resolved[0] {
			PatchResolution::Resolved(ClausewitzPatch::ReplaceBlock { new_statement, .. }) => {
				let items = match new_statement {
					AstStatement::Assignment {
						value: AstValue::Block { items, .. },
						..
					} => items,
					other => panic!("expected Assignment block, got: {other:?}"),
				};
				assert_eq!(*items, body);
				for child in items {
					if let AstStatement::Assignment { key, .. } = child {
						assert_ne!(key, "OR", "single-mod path must not introduce OR wrappers");
					}
				}
			}
			other => panic!("expected single-mod Resolved ReplaceBlock, got: {other:?}"),
		}
	}

	#[test]
	fn explicit_last_writer_block_policy_falls_through_to_conflict() {
		// `BlockPatchPolicy::LastWriter` is a deliberate escape hatch: it does
		// not actually silently pick a winner — it just sidesteps Recurse /
		// Union / BooleanOr / named-container so the final fallback in
		// `resolve_replace_blocks` reports the divergent ReplaceBlock as a
		// manual conflict. This keeps families that explicitly opt into
		// LastWriter from getting auto-merged behind their backs.
		let old = assignment_block("is_great_power", vec![]);
		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "is_great_power".into(),
			old_statement: old.clone(),
			new_statement: assignment_block("is_great_power", vec![assignment("tag", scalar("A"))]),
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "is_great_power".into(),
			old_statement: old,
			new_statement: assignment_block("is_great_power", vec![assignment("tag", scalar("B"))]),
		};

		let mut policies = default_policies();
		policies.block_patch = BlockPatchPolicy::LastWriter;
		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&policies,
		);

		assert_eq!(result.resolved.len(), 0);
		assert_eq!(result.conflicts.len(), 1);
		assert_eq!(result.stats.conflict_patches, 1);
		assert_eq!(
			MergePolicies::default().block_patch,
			BlockPatchPolicy::Recurse,
			"BlockPatchPolicy::default() must be Recurse"
		);
	}

	#[test]
	fn union_block_collects_two_overlay_items_via_fingerprint() {
		let base = vec![bare_item("Base")];
		let patch_a = replace_block_patch(
			"leader_names",
			base.clone(),
			vec![bare_item("Base"), bare_item("Afonso")],
		);
		let patch_b = replace_block_patch(
			"leader_names",
			base,
			vec![bare_item("Base"), bare_item("Bernat")],
		);

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&union_policies(),
		);

		assert_eq!(
			result.conflicts.len(),
			0,
			"got conflicts: {:?}",
			result.conflicts
		);
		assert_eq!(result.resolved.len(), 1);
		match &result.resolved[0] {
			PatchResolution::AutoMerged {
				result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
				strategy,
				..
			} => {
				assert_eq!(strategy, "union_block");
				assert_eq!(
					item_texts(block_items(new_statement)),
					vec!["Base", "Afonso", "Bernat"]
				);
			}
			other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
		}
	}

	#[test]
	fn union_block_collects_three_overlay_items_and_dedups_assignments() {
		let base = vec![assignment("Base #0", number("10"))];
		let patch_a = replace_block_patch(
			"monarch_names",
			base.clone(),
			vec![
				assignment("Base #0", number("10")),
				assignment("Afonso #0", number("0")),
			],
		);
		let patch_b = replace_block_patch(
			"monarch_names",
			base.clone(),
			vec![
				assignment("Base #0", number("10")),
				assignment("Bernat #0", number("0")),
			],
		);
		let patch_c = replace_block_patch(
			"monarch_names",
			base,
			vec![
				assignment("Base #0", number("10")),
				assignment("Afonso #0", number("0")),
				assignment("Carles #0", number("0")),
			],
		);

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
				("mod_c".into(), 3, vec![patch_c]),
			],
			&union_policies(),
		);

		assert_eq!(
			result.conflicts.len(),
			0,
			"got conflicts: {:?}",
			result.conflicts
		);
		assert_eq!(result.resolved.len(), 1);
		let new_statement = match &result.resolved[0] {
			PatchResolution::AutoMerged {
				result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
				strategy,
				..
			} => {
				assert_eq!(strategy, "union_block");
				new_statement
			}
			other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
		};
		assert_eq!(
			assignment_keys(block_items(new_statement)),
			vec!["Base #0", "Afonso #0", "Bernat #0", "Carles #0"]
		);
	}

	#[test]
	fn union_block_policy_coexists_with_recurse_for_other_blocks() {
		let vanilla_date = assignment_block("1444.11.11", vec![assignment("owner", scalar("BYZ"))]);
		let date_a = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "1444.11.11".into(),
			old_statement: vanilla_date.clone(),
			new_statement: assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]),
		};
		let date_b = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "1444.11.11".into(),
			old_statement: assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]),
			new_statement: assignment_block(
				"1444.11.11",
				vec![
					assignment("owner", scalar("BYZ")),
					assignment("controller", scalar("OTT")),
				],
			),
		};
		let base_names = vec![bare_item("Base")];
		let names_a = replace_block_patch(
			"leader_names",
			base_names.clone(),
			vec![bare_item("Base"), bare_item("Afonso")],
		);
		let names_b = replace_block_patch(
			"leader_names",
			base_names,
			vec![bare_item("Base"), bare_item("Bernat")],
		);

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![date_a, names_a]),
				("mod_b".into(), 2, vec![date_b, names_b]),
			],
			&country_history_name_union_policies(),
		);

		assert_eq!(
			result.conflicts.len(),
			0,
			"got conflicts: {:?}",
			result.conflicts
		);
		let mut strategies: Vec<&str> = result
			.resolved
			.iter()
			.filter_map(|resolution| match resolution {
				PatchResolution::AutoMerged { strategy, .. } => Some(strategy.as_str()),
				_ => None,
			})
			.collect();
		strategies.sort_unstable();
		assert_eq!(strategies, vec!["recursive_block_merge", "union_block"]);
	}

	#[test]
	fn default_recurse_policy_unions_compatible_appends() {
		// Two sibling mods append distinct bare items to the same list-shaped
		// block. Recurse re-diffs the bodies and produces independent
		// `AppendBlockItem` patches at fingerprinted addresses; both apply
		// without a conflict (the user explicitly endorsed list-append
		// coexistence: "list 追加合并我觉得没什么问题").
		let base = vec![bare_item("Base")];
		let patch_a = replace_block_patch(
			"leader_names",
			base.clone(),
			vec![bare_item("Base"), bare_item("Afonso")],
		);
		let patch_b = replace_block_patch(
			"leader_names",
			base,
			vec![bare_item("Base"), bare_item("Bernat")],
		);

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		assert_eq!(
			result.conflicts.len(),
			0,
			"got conflicts: {:?}",
			result.conflicts
		);
		assert_eq!(result.resolved.len(), 1);
		match &result.resolved[0] {
			PatchResolution::AutoMerged {
				result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
				strategy,
				..
			} => {
				assert_eq!(strategy, "recursive_block_merge");
				let names = item_texts(block_items(new_statement));
				assert!(
					names.contains(&"Afonso".to_string()),
					"missing Afonso: {names:?}"
				);
				assert!(
					names.contains(&"Bernat".to_string()),
					"missing Bernat: {names:?}"
				);
			}
			other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
		}
	}

	// -----------------------------------------------------------------------
	// Named-container merge tests
	// -----------------------------------------------------------------------

	fn string_val(s: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::String(s.to_string()),
			span: span(),
		}
	}

	fn block(items: Vec<AstStatement>) -> AstValue {
		AstValue::Block {
			items,
			span: span(),
		}
	}

	fn named_block(key: &str, name: &str, extras: Vec<AstStatement>) -> AstStatement {
		let mut items = vec![assignment("name", string_val(name))];
		items.extend(extras);
		assignment(key, block(items))
	}

	#[test]
	fn child_identity_named_block_returns_key_and_name() {
		let stmt = named_block(
			"windowType",
			"hre_window",
			vec![assignment("position", scalar("center"))],
		);
		let id = child_identity(&stmt).expect("identity");
		assert_eq!(id.key, "windowType");
		assert_eq!(id.name.as_deref(), Some("hre_window"));
	}

	#[test]
	fn child_identity_block_without_name_returns_key_only() {
		let stmt = assignment("position", block(vec![assignment("x", number("1"))]));
		let id = child_identity(&stmt).expect("identity");
		assert_eq!(id.key, "position");
		assert_eq!(id.name, None);
	}

	#[test]
	fn items_are_named_container_pure_blocks_true() {
		let body = vec![
			named_block("windowType", "a", vec![]),
			named_block("windowType", "b", vec![]),
		];
		assert!(items_are_named_container(&body, false));
		assert!(items_are_named_container(&body, true));
	}

	#[test]
	fn items_are_named_container_mixed_with_scalars_strict_false_lenient_true() {
		let body = vec![
			assignment("position", scalar("center")), // bare scalar field
			named_block("iconType", "icon_a", vec![]),
		];
		assert!(!items_are_named_container(&body, false));
		assert!(items_are_named_container(&body, true));
	}

	#[test]
	fn ast_equal_ignoring_spans_handles_different_filenames() {
		// Two structurally identical statements with different spans (here we
		// can only differ on offset/line/column) must compare equal.
		let s1 = named_block(
			"iconType",
			"icon_a",
			vec![assignment("texture", scalar("a.dds"))],
		);
		let mut s2 = s1.clone();
		// Mutate inner spans to simulate a different parse origin.
		if let AstStatement::Assignment { span, .. } = &mut s2 {
			span.start.line = 42;
			span.start.column = 7;
			span.end.line = 99;
		}
		assert!(ast_equal_ignoring_spans(&s1, &s2));
		assert_ne!(s1, s2, "raw PartialEq must differ — spans differ");
	}

	fn body_to_window_type_block(name: &str, body: Vec<AstStatement>) -> AstStatement {
		named_block("windowType", name, body)
	}

	#[test]
	fn merge_two_modded_windowtypes_unions_inner_icon_types() {
		// Base: empty windowType "hre"
		let base = vec![body_to_window_type_block("hre", vec![])];
		// Mod A adds iconType "ico_a" inside windowType
		let mod_a = vec![body_to_window_type_block(
			"hre",
			vec![named_block("iconType", "ico_a", vec![])],
		)];
		// Mod B adds iconType "ico_b" inside windowType
		let mod_b = vec![body_to_window_type_block(
			"hre",
			vec![named_block("iconType", "ico_b", vec![])],
		)];

		let merged = merge_named_container_bodies(
			&base,
			&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
			&default_policies(),
		)
		.expect("merge");

		assert_eq!(merged.len(), 1);
		// Inspect inner body: should now have both iconType ico_a and ico_b.
		let inner = match &merged[0] {
			AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} => items,
			other => panic!("expected windowType block, got {other:?}"),
		};
		// Filter only iconType children.
		let icons: Vec<_> = inner
			.iter()
			.filter_map(child_identity)
			.filter(|id| id.key == "iconType")
			.map(|id| id.name.unwrap_or_default())
			.collect();
		assert!(
			icons.contains(&"ico_a".to_string()),
			"missing ico_a: {icons:?}"
		);
		assert!(
			icons.contains(&"ico_b".to_string()),
			"missing ico_b: {icons:?}"
		);
	}

	#[test]
	fn merge_two_modded_windowtypes_recursive_into_named_subblock() {
		// Both mods modify the same iconType "ico_x", each adding distinct grandchild.
		let base = vec![body_to_window_type_block(
			"hre",
			vec![named_block("iconType", "ico_x", vec![])],
		)];
		let mod_a = vec![body_to_window_type_block(
			"hre",
			vec![named_block(
				"iconType",
				"ico_x",
				vec![named_block("hover", "h_a", vec![])],
			)],
		)];
		let mod_b = vec![body_to_window_type_block(
			"hre",
			vec![named_block(
				"iconType",
				"ico_x",
				vec![named_block("hover", "h_b", vec![])],
			)],
		)];

		let merged = merge_named_container_bodies(
			&base,
			&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
			&default_policies(),
		)
		.expect("merge");

		// Drill down: windowType.hre -> iconType.ico_x -> body should contain
		// both hover.h_a and hover.h_b.
		let window_body = match &merged[0] {
			AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} => items,
			_ => panic!("expected windowType block"),
		};
		let icon = window_body
			.iter()
			.find(|s| {
				child_identity(s)
					.map(|i| i.name.as_deref() == Some("ico_x"))
					.unwrap_or(false)
			})
			.expect("ico_x present");
		let icon_body = match icon {
			AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} => items,
			_ => panic!("ico_x should be a block"),
		};
		let hovers: Vec<_> = icon_body
			.iter()
			.filter_map(child_identity)
			.filter(|id| id.key == "hover")
			.map(|id| id.name.unwrap_or_default())
			.collect();
		assert!(
			hovers.contains(&"h_a".to_string()),
			"missing h_a: {hovers:?}"
		);
		assert!(
			hovers.contains(&"h_b".to_string()),
			"missing h_b: {hovers:?}"
		);
	}

	#[test]
	fn merge_conflict_suffix_renames_under_lenient() {
		// Same identity, both leaves (no nested named-container body) → cannot
		// recurse → SuffixRename keeps both via rename.
		let base: Vec<AstStatement> = vec![named_block("iconType", "icon_x", vec![])];
		let mod_a = vec![named_block(
			"iconType",
			"icon_x",
			vec![assignment("texture", string_val("a.dds"))],
		)];
		let mod_b = vec![named_block(
			"iconType",
			"icon_x",
			vec![assignment("texture", string_val("b.dds"))],
		)];

		let policies = MergePolicies {
			named_container: NamedContainerPolicy::SuffixRename,
			..Default::default()
		};
		let merged = merge_named_container_bodies(
			&base,
			&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
			&policies,
		)
		.expect("merge");

		// First candidate replaced base via recursive (texture is a scalar
		// passthrough — single-mod merge succeeds). Second candidate conflicts
		// with the same texture key → SuffixRename appends a renamed copy.
		let names: Vec<_> = merged
			.iter()
			.filter_map(child_identity)
			.filter(|id| id.key == "iconType")
			.map(|id| id.name.unwrap_or_default())
			.collect();
		assert!(names.iter().any(|n| n == "icon_x"), "names={names:?}");
		assert!(
			names
				.iter()
				.any(|n| n.starts_with("icon_x_") && n.contains("mod_b")),
			"expected suffix-renamed icon_x_mod_b, got names={names:?}"
		);
	}

	#[test]
	fn merge_conflict_overlay_wins_under_overlay_policy() {
		let base: Vec<AstStatement> = vec![named_block("iconType", "icon_x", vec![])];
		let mod_a = vec![named_block(
			"iconType",
			"icon_x",
			vec![assignment("texture", string_val("a.dds"))],
		)];
		let mod_b = vec![named_block(
			"iconType",
			"icon_x",
			vec![assignment("texture", string_val("b.dds"))],
		)];

		let policies = MergePolicies {
			named_container: NamedContainerPolicy::OverlayWins,
			..Default::default()
		};
		let merged = merge_named_container_bodies(
			&base,
			&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
			&policies,
		)
		.expect("merge");

		// Only one icon_x kept; its texture is mod_b's value (last in the list).
		let icons: Vec<_> = merged
			.iter()
			.filter(|s| {
				child_identity(s)
					.map(|i| i.key == "iconType")
					.unwrap_or(false)
			})
			.collect();
		assert_eq!(icons.len(), 1, "OverlayWins must keep only one entry");
		let inner = match icons[0] {
			AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} => items,
			_ => panic!("expected block"),
		};
		let texture = inner.iter().find_map(|s| match s {
			AstStatement::Assignment {
				key,
				value: AstValue::Scalar { value: sv, .. },
				..
			} if key == "texture" => Some(sv.as_text()),
			_ => None,
		});
		assert_eq!(texture.as_deref(), Some("b.dds"));
	}

	#[test]
	fn replace_block_named_container_resolves_via_merge() {
		// End-to-end: two mods produce ReplaceBlock for the same windowType
		// with different inner additions. Default Recurse re-diffs each body
		// and the additive iconType inserts at distinct identities coexist —
		// no conflict, single auto-merged ReplaceBlock.
		let base_stmt =
			body_to_window_type_block("hre", vec![named_block("iconType", "ico_x", vec![])]);
		let mod_a_stmt = body_to_window_type_block(
			"hre",
			vec![
				named_block("iconType", "ico_x", vec![]),
				named_block("iconType", "ico_a", vec![]),
			],
		);
		let mod_b_stmt = body_to_window_type_block(
			"hre",
			vec![
				named_block("iconType", "ico_x", vec![]),
				named_block("iconType", "ico_b", vec![]),
			],
		);

		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "windowType".into(),
			old_statement: base_stmt.clone(),
			new_statement: mod_a_stmt,
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec!["root".into()],
			key: "windowType".into(),
			old_statement: base_stmt,
			new_statement: mod_b_stmt,
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		assert_eq!(
			result.conflicts.len(),
			0,
			"expected merge, got conflicts: {:?}",
			result.conflicts
		);
		assert_eq!(result.resolved.len(), 1);
		match &result.resolved[0] {
			PatchResolution::AutoMerged { strategy, .. } => {
				assert!(
					matches!(
						strategy.as_str(),
						"recursive_block_merge" | "named_container_union"
					),
					"unexpected strategy: {strategy}"
				);
			}
			other => panic!("expected AutoMerged, got: {other:?}"),
		}
	}

	// ---- Rename cross-mod resolution ---------------------------------------

	#[test]
	fn rename_rewrites_set_value_at_old_key() {
		// mod_a renames X→Y; mod_b sets a value at X.
		// Expected: mod_b's SetValue is rewritten to address Y, no conflict.
		let rename = ClausewitzPatch::Rename {
			path: vec![],
			old_key: "feudalism_reform".into(),
			new_key: "EE_feudalism_reform".into(),
		};
		let set = ClausewitzPatch::SetValue {
			path: vec![],
			key: "feudalism_reform".into(),
			old_value: scalar("a"),
			new_value: scalar("b"),
		};
		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![rename]),
				("mod_b".into(), 2, vec![set]),
			],
			&default_policies(),
		);
		assert_eq!(
			result.conflicts.len(),
			0,
			"expected no conflicts, got: {:?}",
			result.conflicts
		);
		// Both the Rename and the rewritten SetValue should be resolved.
		let has_rename = result
			.resolved
			.iter()
			.any(|r| matches!(r, PatchResolution::Resolved(ClausewitzPatch::Rename { .. })));
		let rewritten_set = result.resolved.iter().any(|r| match r {
			PatchResolution::Resolved(ClausewitzPatch::SetValue { key, .. }) => {
				key == "EE_feudalism_reform"
			}
			_ => false,
		});
		assert!(has_rename, "expected Rename in resolved");
		assert!(rewritten_set, "expected SetValue rewritten to new key");
	}

	#[test]
	fn rename_rewrites_nested_path_segment() {
		// mod_a renames X→Y at root; mod_b inserts a node at path [X].
		let rename = ClausewitzPatch::Rename {
			path: vec![],
			old_key: "feudalism_reform".into(),
			new_key: "EE_feudalism_reform".into(),
		};
		let insert = ClausewitzPatch::InsertNode {
			path: vec!["feudalism_reform".into()],
			key: "modifier".into(),
			statement: assignment("modifier", scalar("centralization_modifier")),
		};
		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![rename]),
				("mod_b".into(), 2, vec![insert]),
			],
			&default_policies(),
		);
		assert_eq!(
			result.conflicts.len(),
			0,
			"expected no conflicts, got: {:?}",
			result.conflicts
		);
		let rewritten = result.resolved.iter().any(|r| match r {
			PatchResolution::Resolved(ClausewitzPatch::InsertNode { path, .. }) => {
				path == &vec!["EE_feudalism_reform".to_string()]
			}
			_ => false,
		});
		assert!(rewritten, "expected nested InsertNode path rewritten");
	}

	#[test]
	fn conflicting_renames_emit_conflict() {
		// Two mods rename the same (path, X) to different new keys.
		let rename_a = ClausewitzPatch::Rename {
			path: vec![],
			old_key: "X".into(),
			new_key: "Y1".into(),
		};
		let rename_b = ClausewitzPatch::Rename {
			path: vec![],
			old_key: "X".into(),
			new_key: "Y2".into(),
		};
		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![rename_a]),
				("mod_b".into(), 2, vec![rename_b]),
			],
			&default_policies(),
		);
		assert_eq!(
			result.conflicts.len(),
			1,
			"expected one conflict, got: {:?}",
			result.conflicts
		);
		match &result.conflicts[0] {
			PatchResolution::Conflict { reason, .. } => {
				assert!(reason.contains("conflicting renames"));
			}
			other => panic!("expected Conflict, got: {other:?}"),
		}
	}

	#[test]
	fn convergent_renames_resolve() {
		// Two mods rename the same (path, X) to the same new key.
		let mk = || ClausewitzPatch::Rename {
			path: vec![],
			old_key: "X".into(),
			new_key: "Y".into(),
		};
		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![mk()]),
				("mod_b".into(), 2, vec![mk()]),
			],
			&default_policies(),
		);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.resolved.len(), 1);
	}

	// -----------------------------------------------------------------------
	// BlockPatchPolicy::Recurse — date-keyed history deep merge
	// -----------------------------------------------------------------------

	fn recurse_policies() -> MergePolicies {
		MergePolicies {
			block_patch: BlockPatchPolicy::Recurse,
			..Default::default()
		}
	}

	#[test]
	fn recurse_merges_disjoint_date_block_changes() {
		// Vanilla:    1444.11.11 = { owner = BYZ }
		// mod_a:      1444.11.11 = { owner = OTT }                        (changes owner)
		// mod_b:      1444.11.11 = { owner = BYZ controller = OTT }       (adds controller)
		//
		// Each mod's diff is against vanilla (chained predecessor at this
		// address only has vanilla). Expected: deep-merge produces
		// `{ owner = OTT controller = OTT }`.
		let vanilla_body = vec![assignment("owner", scalar("BYZ"))];
		let a_body = vec![assignment("owner", scalar("OTT"))];
		let b_body = vec![
			assignment("owner", scalar("BYZ")),
			assignment("controller", scalar("OTT")),
		];

		let vanilla_stmt = assignment_block("1444.11.11", vanilla_body);
		let a_stmt = assignment_block("1444.11.11", a_body);
		let b_stmt = assignment_block("1444.11.11", b_body);

		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "1444.11.11".into(),
			old_statement: vanilla_stmt.clone(),
			new_statement: a_stmt,
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "1444.11.11".into(),
			// mod_b's chained diff base is mod_a's new content; carry that
			// to mirror how `compute_chained_patches` produces patches.
			old_statement: assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]),
			new_statement: b_stmt,
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&recurse_policies(),
		);

		assert_eq!(
			result.conflicts.len(),
			0,
			"got conflicts: {:?}",
			result.conflicts
		);
		assert_eq!(result.resolved.len(), 1);
		match &result.resolved[0] {
			PatchResolution::AutoMerged {
				result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
				strategy,
				..
			} => {
				assert_eq!(strategy, "recursive_block_merge");
				let body = match new_statement {
					AstStatement::Assignment {
						value: AstValue::Block { items, .. },
						..
					} => items,
					other => panic!("expected block, got {other:?}"),
				};
				// mod_a contributes owner=OTT (vs vanilla owner=BYZ).
				// mod_b contributes controller=OTT addition.
				let has_owner_ott = body.iter().any(|s| matches!(s,
					AstStatement::Assignment { key, value: AstValue::Scalar { value: ScalarValue::Identifier(v), .. }, .. }
					if key == "owner" && v == "OTT"));
				let has_controller_ott = body.iter().any(|s| matches!(s,
					AstStatement::Assignment { key, value: AstValue::Scalar { value: ScalarValue::Identifier(v), .. }, .. }
					if key == "controller" && v == "OTT"));
				assert!(
					has_owner_ott,
					"expected merged body to keep owner=OTT, got {body:?}"
				);
				assert!(
					has_controller_ott,
					"expected merged body to add controller=OTT, got {body:?}"
				);
			}
			other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
		}
	}

	#[test]
	fn recurse_sibling_scalar_conflict_bubbles_up() {
		// Both sibling mods change the same `owner` scalar inside the same
		// date block to *different* tags. Recurse re-diffs each body against
		// the common ancestor, producing two SetValues at the nested address
		// — and per `ScalarMergePolicy::Conflict` the engine must surface a
		// conflict instead of silently choosing the highest-precedence value.
		let vanilla_body = vec![assignment("owner", scalar("BYZ"))];
		let vanilla_stmt = assignment_block("1444.11.11", vanilla_body);
		let a_stmt = assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]);
		let b_stmt = assignment_block("1444.11.11", vec![assignment("owner", scalar("MAM"))]);

		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "1444.11.11".into(),
			old_statement: vanilla_stmt.clone(),
			new_statement: a_stmt.clone(),
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "1444.11.11".into(),
			old_statement: a_stmt, // chained diff base
			new_statement: b_stmt,
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&recurse_policies(),
		);

		assert_eq!(
			result.conflicts.len(),
			1,
			"expected one bubbled sub-conflict, got resolved={:?} conflicts={:?}",
			result.resolved,
			result.conflicts,
		);
		match &result.conflicts[0] {
			PatchResolution::Conflict { reason, .. } => {
				assert!(
					reason.contains("unresolved sub-conflict")
						|| reason.contains("sibling mods set the same scalar"),
					"unexpected reason: {reason}"
				);
			}
			other => panic!("expected Conflict, got: {other:?}"),
		}
	}

	#[test]
	fn recurse_default_policy_emits_cross_kind_conflict_on_owner() {
		// mod_a sets `owner` to a new value while mod_b removes `owner`
		// entirely and adds a sibling `controller`. Recurse re-diffs each
		// body and the cross-kind detector surfaces the conflicting intent
		// at `owner` as a sub-conflict (SetValue vs RemoveNode at the same
		// raw key) which bubbles up to the outer ReplaceBlock.
		let vanilla_stmt = assignment_block("1444.11.11", vec![assignment("owner", scalar("BYZ"))]);
		let a_stmt = assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]);
		let b_stmt = assignment_block("1444.11.11", vec![assignment("controller", scalar("OTT"))]);

		let patch_a = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "1444.11.11".into(),
			old_statement: vanilla_stmt.clone(),
			new_statement: a_stmt,
		};
		let patch_b = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "1444.11.11".into(),
			old_statement: vanilla_stmt,
			new_statement: b_stmt,
		};

		let result = merge_patch_sets_with_defer(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		assert_eq!(result.conflicts.len(), 1);
		match &result.conflicts[0] {
			PatchResolution::Conflict { reason, .. } => {
				assert!(
					reason.contains("incompatible patch kinds")
						|| reason.contains("unresolved sub-conflict"),
					"unexpected reason: {reason}"
				);
				assert!(reason.contains("owner"), "reason missing key: {reason}");
			}
			other => panic!("expected Conflict, got: {other:?}"),
		}
	}
}
