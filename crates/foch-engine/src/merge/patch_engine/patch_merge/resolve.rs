use foch_language::analyzer::content_family::{BlockPatchPolicy, MergePolicies, ScalarMergePolicy};
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};

use super::super::patch::{ClausewitzPatch, patches_semantically_equal};
use super::address::patch_kind;
use super::block_merge::{
	synthesize_boolean_or, try_recursive_block_merge, try_replace_block_named_container_merge,
	try_union_block_merge,
};
use super::rename::resolve_renames;
use super::{AttributedPatch, PatchAddress, PatchMergeStats, PatchResolution};

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

pub(super) fn resolve_address(
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
		.all(|w| crate::merge::patch::ast_statements_semantically_equal(w[0], w[1]))
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
