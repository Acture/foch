// Patch set merging: given N mods' patch sets against a common base, merge
// them into a single resolved patch set with conflict detection.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use foch_core::config::compute_conflict_id;
use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::content_family::{
	BlockPatchPolicy, MergeKeySource, MergePolicies, NamedContainerPolicy, ScalarMergePolicy,
};
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};

use super::super::conflict_handler::{ConflictDecision, ConflictHandler, DeferHandler};
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
/// preserving the original key. Returns `None` (leaving resolution to the
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

mod fingerprint;
use fingerprint::{fingerprint_into, statement_fingerprint, value_fingerprint};

mod rename;
use rename::{build_rename_map, resolve_renames, rewrite_patch_for_renames};

#[cfg(test)]
mod tests;
