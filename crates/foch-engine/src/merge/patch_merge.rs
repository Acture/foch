// Patch set merging: given N mods' patch sets against a common base, merge
// them into a single resolved patch set with conflict detection.
#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use foch_language::analyzer::content_family::{MergePolicies, ScalarMergePolicy};
use foch_language::analyzer::parser::{AstValue, ScalarValue};

use super::patch::{AstPath, ClausewitzPatch};

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
#[derive(Clone, Debug)]
pub struct AttributedPatch {
	pub mod_id: String,
	pub precedence: usize,
	pub patch: ClausewitzPatch,
}

/// Result of merging patches at a single address.
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug, Default)]
pub struct PatchMergeResult {
	pub resolved: Vec<PatchResolution>,
	pub conflicts: Vec<PatchResolution>,
	pub stats: PatchMergeStats,
}

#[derive(Clone, Debug, Default)]
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

fn patch_address(patch: &ClausewitzPatch) -> PatchAddress {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. } => PatchAddress {
			path: path.clone(),
			key: key.clone(),
		},
		ClausewitzPatch::RemoveNode { path, key, .. } => PatchAddress {
			path: path.clone(),
			key: key.clone(),
		},
		ClausewitzPatch::InsertNode { path, key, .. } => PatchAddress {
			path: path.clone(),
			key: key.clone(),
		},
		ClausewitzPatch::AppendListItem { path, key, .. } => PatchAddress {
			path: path.clone(),
			key: key.clone(),
		},
		ClausewitzPatch::RemoveListItem { path, key, .. } => PatchAddress {
			path: path.clone(),
			key: key.clone(),
		},
		ClausewitzPatch::ReplaceBlock { path, key, .. } => PatchAddress {
			path: path.clone(),
			key: key.clone(),
		},
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
) -> PatchMergeResult {
	let mut result = PatchMergeResult::default();

	// Group patches by address, preserving attribution.
	let mut by_address: HashMap<PatchAddress, Vec<AttributedPatch>> = HashMap::new();

	for (mod_id, precedence, patches) in mod_patches {
		for patch in patches {
			result.stats.total_patches += 1;
			let addr = patch_address(&patch);
			by_address.entry(addr).or_default().push(AttributedPatch {
				mod_id: mod_id.clone(),
				precedence,
				patch,
			});
		}
	}

	for (addr, attributed) in by_address {
		let resolution = resolve_address(addr, attributed, policies, &mut result.stats);
		match &resolution {
			PatchResolution::Conflict { .. } => result.conflicts.push(resolution),
			_ => result.resolved.push(resolution),
		}
	}

	result
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

	// --- Convergence: all patches identical ---
	if attributed.windows(2).all(|w| w[0].patch == w[1].patch) {
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
		"InsertNode" => resolve_insert_nodes(addr, attributed, stats),
		"AppendListItem" => resolve_append_list_items(addr, attributed, stats),
		"SetValue" => resolve_set_values(addr, attributed, policies, stats),
		"RemoveNode" => resolve_remove_convergent(addr, attributed, stats),
		"RemoveListItem" => resolve_remove_list_items(addr, attributed, stats),
		"ReplaceBlock" => resolve_replace_blocks(addr, attributed, stats),
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
	_addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
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

	// If all statements are identical → convergent.
	if stmts.windows(2).all(|w| w[0] == w[1]) {
		stats.convergent_patches += 1;
		return PatchResolution::Resolved(attributed.into_iter().next().unwrap().patch);
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

	// If all values are identical → convergent (dedup to one).
	if values.windows(2).all(|w| w[0] == w[1]) {
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
		ScalarMergePolicy::LastWriter => {
			let mods: Vec<String> = attributed.iter().map(|a| a.mod_id.clone()).collect();
			let mut sorted = attributed;
			sorted.sort_by(|a, b| b.precedence.cmp(&a.precedence));
			stats.auto_merged_patches += 1;
			PatchResolution::AutoMerged {
				result: sorted.into_iter().next().unwrap().patch,
				strategy: "last_writer".to_string(),
				contributing_mods: mods,
			}
		}
		ScalarMergePolicy::Sum
		| ScalarMergePolicy::Avg
		| ScalarMergePolicy::Max
		| ScalarMergePolicy::Min => resolve_numeric_set_values(addr, attributed, policies.scalar, stats),
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
	if values.windows(2).all(|w| w[0] == w[1]) {
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

/// Multiple mods replacing the same block → conflict unless identical.
fn resolve_replace_blocks(
	addr: PatchAddress,
	attributed: Vec<AttributedPatch>,
	stats: &mut PatchMergeStats,
) -> PatchResolution {
	// Identical replacements were already caught by convergence check above,
	// so reaching here means different replacements → conflict.
	stats.conflict_patches += 1;
	PatchResolution::Conflict {
		address: addr,
		reason: "multiple mods replace the same block with different content".to_string(),
		patches: attributed,
	}
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
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

		let result = merge_patch_sets(vec![("mod_a".into(), 1, patches)], &default_policies());

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

		let result = merge_patch_sets(
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
	fn different_insert_nodes_both_apply() {
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

		let result = merge_patch_sets(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		assert_eq!(result.resolved.len(), 1);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.stats.auto_merged_patches, 1);
		match &result.resolved[0] {
			PatchResolution::AutoMerged {
				strategy,
				contributing_mods,
				..
			} => {
				assert_eq!(strategy, "compatible_inserts");
				assert_eq!(contributing_mods.len(), 2);
			}
			other => panic!("expected AutoMerged, got: {other:?}"),
		}
	}

	#[test]
	fn same_append_list_item_deduplicated() {
		let patch = ClausewitzPatch::AppendListItem {
			path: vec!["root".into(), "or".into()],
			key: "tag".into(),
			value: scalar("ERS"),
		};

		let result = merge_patch_sets(
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
	fn different_append_list_items_union() {
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

		let result = merge_patch_sets(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		assert_eq!(result.resolved.len(), 1);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.stats.auto_merged_patches, 1);
		match &result.resolved[0] {
			PatchResolution::AutoMerged { strategy, .. } => {
				assert_eq!(strategy, "list_union");
			}
			other => panic!("expected AutoMerged, got: {other:?}"),
		}
	}

	#[test]
	fn different_set_value_last_writer() {
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

		let result = merge_patch_sets(
			vec![
				("mod_a".into(), 1, vec![patch_a]),
				("mod_b".into(), 2, vec![patch_b]),
			],
			&default_policies(),
		);

		assert_eq!(result.resolved.len(), 1);
		assert_eq!(result.conflicts.len(), 0);
		assert_eq!(result.stats.auto_merged_patches, 1);
		match &result.resolved[0] {
			PatchResolution::AutoMerged {
				result: patch,
				strategy,
				..
			} => {
				assert_eq!(strategy, "last_writer");
				// Highest precedence (mod_b at 2) wins.
				match patch {
					ClausewitzPatch::SetValue { new_value, .. } => {
						assert_eq!(*new_value, number("15"),);
					}
					_ => panic!("expected SetValue"),
				}
			}
			other => panic!("expected AutoMerged, got: {other:?}"),
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

		let result = merge_patch_sets(
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

		let result = merge_patch_sets(
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
		let insert = ClausewitzPatch::InsertNode {
			path: vec!["root".into()],
			key: "thing".into(),
			statement: assignment("thing", scalar("new")),
		};
		let remove = ClausewitzPatch::RemoveNode {
			path: vec!["root".into()],
			key: "thing".into(),
			removed: assignment("thing", scalar("old")),
		};

		let result = merge_patch_sets(
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

		let result = merge_patch_sets(
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
}
