// Patch set merging: given N mods' patch sets against a common base, merge
// them into a single resolved patch set with conflict detection.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use foch_core::config::compute_conflict_id;
use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::content_family::{BlockPatchPolicy, MergePolicies, ScalarMergePolicy};
#[cfg(test)]
use foch_language::analyzer::content_family::{MergeKeySource, NamedContainerPolicy};
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};

#[cfg(test)]
use super::super::conflict_handler::DeferHandler;
use super::super::conflict_handler::{ConflictDecision, ConflictHandler};
use super::super::conflict_view::build_decision_conflict_view;
use super::super::error::MergeError;
use super::patch::{AstPath, ClausewitzPatch, patches_semantically_equal};

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
pub(crate) enum PatchResolution {
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
pub(crate) struct PatchMergeResult {
	pub resolved: Vec<PatchResolution>,
	pub conflicts: Vec<PatchResolution>,
	pub stats: PatchMergeStats,
	pub handler_resolved_count: usize,
	pub handler_resolutions: Vec<HandlerResolutionRecord>,
	pub external_file_resolutions: HashMap<PathBuf, PathBuf>,
	pub keep_existing_paths: HashSet<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PatchMergeStats {
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
fn patch_raw_address(patch: &ClausewitzPatch) -> Option<(AstPath, String)> {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. }
		| ClausewitzPatch::RemoveNode { path, key, .. }
		| ClausewitzPatch::InsertNode { path, key, .. }
		| ClausewitzPatch::ReplaceBlock { path, key, .. } => Some((path.clone(), key.clone())),
		_ => None,
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

	// Different statements at the same (path, key) from sibling mods → real
	// conflict. The fingerprint scheme already routed list-like content
	// (Union policy) through distinct addresses earlier; anything that
	// reaches this default branch is a unique-key collision the engine
	// must escalate instead of silently picking the highest-precedence
	// patch. Per the project rule, ambiguous merges must surface as
	// conflicts rather than fall back to LastWriter behind the user's back.
	let summaries: Vec<String> = attributed
		.iter()
		.map(|a| match &a.patch {
			ClausewitzPatch::InsertNode { statement, .. } => format!(
				"`{}` from `{}`",
				statement_text_for_reason(statement),
				a.mod_id
			),
			_ => unreachable!(),
		})
		.collect();
	stats.conflict_patches += 1;
	PatchResolution::Conflict {
		address: addr,
		reason: format!(
			"sibling mods inserted divergent statements at the same key: {}",
			summaries.join(", ")
		),
		patches: attributed,
	}
}

/// Multiple mods appending the same list item. Because `patch_address`
/// includes the value fingerprint, every patch in this group has identical
/// `value` → always convergent.
fn resolve_append_list_items(
	_addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	stats.convergent_patches += 1;
	PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch)
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

fn statement_text_for_reason(stmt: &AstStatement) -> String {
	match stmt {
		AstStatement::Assignment { key, value, .. } => {
			format!("{key} = {}", value_text_for_reason(value))
		}
		AstStatement::Item { value, .. } => value_text_for_reason(value),
		AstStatement::Comment { .. } => "<comment>".to_string(),
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

/// Multiple mods removing the same list item. Because `patch_address`
/// includes the value fingerprint, every patch in this group has identical
/// `value` → always convergent.
fn resolve_remove_list_items(
	_addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	stats.convergent_patches += 1;
	PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch)
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

// ---------------------------------------------------------------------------
// Child modules
// ---------------------------------------------------------------------------

mod block_merge;
#[cfg(test)]
pub(crate) use block_merge::{
	ast_equal_ignoring_spans, child_identity, items_are_named_container,
	merge_named_container_bodies, rename_for_conflict,
};
use block_merge::{
	synthesize_boolean_or, try_recursive_block_merge, try_replace_block_named_container_merge,
	try_union_block_merge,
};

mod fingerprint;
use fingerprint::{statement_fingerprint, value_fingerprint};

mod rename;
use rename::{build_rename_map, resolve_renames, rewrite_patch_for_renames};

#[cfg(test)]
mod tests;
