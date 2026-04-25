//! Apply resolved Clausewitz patches to a base AST.
//!
//! This module takes the patch operations produced by `patch::diff_ast` and
//! applies them to the original base statements, producing a modified
//! `Vec<AstStatement>` that can be fed to `emit::emit_clausewitz_statements`.
#![allow(dead_code)]

use std::collections::HashMap;

use foch_language::analyzer::content_family::MergeKeySource;
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::ParsedScriptFile;

use super::patch::{ClausewitzPatch, diff_ast};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Apply resolved patches to a base AST, producing a modified statement list.
/// Patches that insert new nodes or modify existing ones are applied in-place.
/// The result can be passed to the existing `emit` module for Clausewitz output.
pub fn apply_patches(
	base_statements: &[AstStatement],
	patches: &[ClausewitzPatch],
	merge_key_source: MergeKeySource,
) -> Vec<AstStatement> {
	apply_at_level(base_statements, patches, &[], merge_key_source)
}

/// Full pipeline: diff base vs overlay, then apply overlay's patches to base.
/// This is the "3-way merge for a single mod" shortcut.
pub fn merge_single_mod(
	base: &ParsedScriptFile,
	overlay: &ParsedScriptFile,
	merge_key_source: MergeKeySource,
) -> Vec<AstStatement> {
	let patches = diff_ast(base, overlay, merge_key_source);
	apply_patches(&base.ast.statements, &patches, merge_key_source)
}

// ---------------------------------------------------------------------------
// Core recursive application
// ---------------------------------------------------------------------------

/// Apply patches at a specific nesting level, identified by `current_path`.
///
/// Walks `statements` in order, matching each one against the subset of patches
/// that target this level, then recurses into blocks for deeper patches.
fn apply_at_level(
	statements: &[AstStatement],
	patches: &[ClausewitzPatch],
	current_path: &[String],
	merge_key_source: MergeKeySource,
) -> Vec<AstStatement> {
	// Partition patches into those targeting this level vs deeper levels.
	let (local, deeper) = partition_patches(patches, current_path);

	// Group local patches by key for fast lookup.
	let local_by_key = group_by_key(&local);

	let depth = current_path.len();

	// Track which keys have already been removed (to skip duplicates).
	let mut removed_keys: HashMap<String, usize> = HashMap::new();

	let mut result: Vec<AstStatement> = Vec::with_capacity(statements.len());

	for stmt in statements {
		let key = statement_key(stmt, merge_key_source, current_path);

		let Some(key) = key else {
			// Comments and items without a recognisable key pass through.
			result.push(stmt.clone());
			continue;
		};

		let key_patches = local_by_key.get(&key);

		// Check for RemoveNode / RemoveListItem first.
		if let Some(patches_for_key) = key_patches
			&& should_remove(stmt, patches_for_key, &mut removed_keys, &key)
		{
			continue;
		}

		// Build the (possibly modified) statement.
		let mut new_stmt = stmt.clone();

		if let Some(patches_for_key) = key_patches {
			apply_local_patches(&mut new_stmt, patches_for_key);
		}

		// Recurse into blocks for deeper patches.
		let deeper_for_key = collect_deeper_patches(&deeper, &key, depth);
		if !deeper_for_key.is_empty()
			&& let AstStatement::Assignment {
				value: AstValue::Block { items, span },
				..
			} = &new_stmt
		{
			let child_path = {
				let mut p = current_path.to_vec();
				p.push(key.clone());
				p
			};
			let new_items =
				apply_at_level(items, &deeper_for_key, &child_path, MergeKeySource::AssignmentKey);
			new_stmt = replace_block_items(&new_stmt, new_items, span.clone());
		}

		result.push(new_stmt);
	}

	// InsertNode and AppendListItem patches add statements not present in the base.
	for patch in &local {
		match patch {
			ClausewitzPatch::InsertNode { statement, .. } => {
				result.push(statement.clone());
			}
			ClausewitzPatch::AppendListItem { key, value, .. } => {
				result.push(AstStatement::Assignment {
					key: key.clone(),
					key_span: dummy_span(),
					value: value.clone(),
					span: dummy_span(),
				});
			}
			_ => {}
		}
	}

	result
}

// ---------------------------------------------------------------------------
// Patch partitioning
// ---------------------------------------------------------------------------

/// Separate patches into "local" (targeting exactly `current_path`) and "deeper"
/// (targeting a child of `current_path`, meaning path extends beyond it).
fn partition_patches(
	patches: &[ClausewitzPatch],
	current_path: &[String],
) -> (Vec<ClausewitzPatch>, Vec<ClausewitzPatch>) {
	let mut local = Vec::new();
	let mut deeper = Vec::new();

	for patch in patches {
		let patch_path = patch_path(patch);
		if patch_path == current_path {
			local.push(patch.clone());
		} else if patch_path.len() > current_path.len()
			&& patch_path[..current_path.len()] == *current_path
		{
			deeper.push(patch.clone());
		}
	}

	(local, deeper)
}

/// Collect deeper patches whose next path element (at `depth`) matches `key`.
fn collect_deeper_patches(deeper: &[ClausewitzPatch], key: &str, depth: usize) -> Vec<ClausewitzPatch> {
	deeper
		.iter()
		.filter(|p| {
			let path = patch_path(p);
			path.len() > depth && path[depth] == key
		})
		.cloned()
		.collect()
}

/// Extract the path from a patch.
fn patch_path(patch: &ClausewitzPatch) -> &[String] {
	match patch {
		ClausewitzPatch::SetValue { path, .. } => path,
		ClausewitzPatch::RemoveNode { path, .. } => path,
		ClausewitzPatch::InsertNode { path, .. } => path,
		ClausewitzPatch::AppendListItem { path, .. } => path,
		ClausewitzPatch::RemoveListItem { path, .. } => path,
		ClausewitzPatch::ReplaceBlock { path, .. } => path,
	}
}

/// Extract the key from a patch.
fn patch_key(patch: &ClausewitzPatch) -> &str {
	match patch {
		ClausewitzPatch::SetValue { key, .. } => key,
		ClausewitzPatch::RemoveNode { key, .. } => key,
		ClausewitzPatch::InsertNode { key, .. } => key,
		ClausewitzPatch::AppendListItem { key, .. } => key,
		ClausewitzPatch::RemoveListItem { key, .. } => key,
		ClausewitzPatch::ReplaceBlock { key, .. } => key,
	}
}

// ---------------------------------------------------------------------------
// Statement key extraction
// ---------------------------------------------------------------------------

/// Extract the merge key from a statement, mirroring `patch.rs`'s logic.
fn statement_key(
	stmt: &AstStatement,
	merge_key_source: MergeKeySource,
	current_path: &[String],
) -> Option<String> {
	// When recursing into blocks, children always use AssignmentKey semantics.
	// At the top level, we honour the configured merge_key_source.
	if !current_path.is_empty() {
		return assignment_key(stmt);
	}
	match merge_key_source {
		MergeKeySource::AssignmentKey | MergeKeySource::LeafPath => assignment_key(stmt),
		MergeKeySource::FieldValue(field) => field_value_key(stmt, field),
		MergeKeySource::ContainerChildKey => assignment_key(stmt),
	}
}

fn assignment_key(stmt: &AstStatement) -> Option<String> {
	match stmt {
		AstStatement::Assignment { key, .. } => Some(key.clone()),
		_ => None,
	}
}

fn field_value_key(stmt: &AstStatement, field: &str) -> Option<String> {
	if let AstStatement::Assignment {
		value: AstValue::Block { items, .. },
		..
	} = stmt
	{
		for item in items {
			if let AstStatement::Assignment {
				key,
				value: AstValue::Scalar { value, .. },
				..
			} = item
				&& key == field
			{
				return Some(value.as_text());
			}
		}
	}
	None
}

// ---------------------------------------------------------------------------
// Grouping helpers
// ---------------------------------------------------------------------------

fn group_by_key(patches: &[ClausewitzPatch]) -> HashMap<String, Vec<&ClausewitzPatch>> {
	let mut map: HashMap<String, Vec<&ClausewitzPatch>> = HashMap::new();
	for patch in patches {
		map.entry(patch_key(patch).to_string())
			.or_default()
			.push(patch);
	}
	map
}

// ---------------------------------------------------------------------------
// Patch application helpers
// ---------------------------------------------------------------------------

/// Returns true if the statement should be skipped (removed).
fn should_remove(
	stmt: &AstStatement,
	patches: &[&ClausewitzPatch],
	removed_keys: &mut HashMap<String, usize>,
	key: &str,
) -> bool {
	for patch in patches {
		match patch {
			ClausewitzPatch::RemoveNode { .. } => return true,
			ClausewitzPatch::RemoveListItem { value, .. } => {
				if let Some(stmt_val) = stmt_value(stmt)
					&& stmt_val == value
				{
					let count = removed_keys.entry(key.to_string()).or_insert(0);
					*count += 1;
					return true;
				}
			}
			_ => {}
		}
	}
	false
}

/// Apply non-structural local patches (SetValue, ReplaceBlock) to a statement.
fn apply_local_patches(stmt: &mut AstStatement, patches: &[&ClausewitzPatch]) {
	for patch in patches {
		match patch {
			ClausewitzPatch::SetValue { new_value, .. } => {
				set_stmt_value(stmt, new_value.clone());
			}
			ClausewitzPatch::ReplaceBlock { new_statement, .. } => {
				*stmt = new_statement.clone();
			}
			_ => {}
		}
	}
}


// ---------------------------------------------------------------------------
// AST helpers
// ---------------------------------------------------------------------------

fn stmt_value(stmt: &AstStatement) -> Option<&AstValue> {
	match stmt {
		AstStatement::Assignment { value, .. } => Some(value),
		AstStatement::Item { value, .. } => Some(value),
		AstStatement::Comment { .. } => None,
	}
}

fn set_stmt_value(stmt: &mut AstStatement, new_value: AstValue) {
	match stmt {
		AstStatement::Assignment { value, .. } => *value = new_value,
		AstStatement::Item { value, .. } => *value = new_value,
		AstStatement::Comment { .. } => {}
	}
}

fn replace_block_items(
	stmt: &AstStatement,
	new_items: Vec<AstStatement>,
	block_span: foch_language::analyzer::parser::SpanRange,
) -> AstStatement {
	match stmt {
		AstStatement::Assignment {
			key,
			key_span,
			span,
			..
		} => AstStatement::Assignment {
			key: key.clone(),
			key_span: key_span.clone(),
			value: AstValue::Block {
				items: new_items,
				span: block_span,
			},
			span: span.clone(),
		},
		other => other.clone(),
	}
}

fn dummy_span() -> foch_language::analyzer::parser::SpanRange {
	use foch_language::analyzer::parser::{Span, SpanRange};
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::content_family::ScriptFileKind;
	use foch_language::analyzer::parser::{AstFile, ScalarValue, Span, SpanRange};
	use std::path::PathBuf;

	fn test_span() -> SpanRange {
		SpanRange {
			start: Span {
				line: 1,
				column: 1,
				offset: 0,
			},
			end: Span {
				line: 1,
				column: 1,
				offset: 0,
			},
		}
	}

	fn scalar(value: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::Identifier(value.to_string()),
			span: test_span(),
		}
	}

	fn assignment(key: &str, value: AstValue) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: test_span(),
			value,
			span: test_span(),
		}
	}

	fn block(key: &str, items: Vec<AstStatement>) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: test_span(),
			value: AstValue::Block {
				items,
				span: test_span(),
			},
			span: test_span(),
		}
	}

	fn make_parsed(statements: Vec<AstStatement>) -> ParsedScriptFile {
		ParsedScriptFile {
			mod_id: "test".to_string(),
			path: PathBuf::from("test.txt"),
			relative_path: PathBuf::from("test.txt"),
			content_family: None,
			file_kind: ScriptFileKind::Other,
			module_name: "test".to_string(),
			ast: AstFile {
				path: PathBuf::from("test.txt"),
				statements,
			},
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	#[test]
	fn empty_patches_leave_base_unchanged() {
		let base = vec![
			assignment("tax", scalar("5")),
			assignment("manpower", scalar("1000")),
		];
		let result = apply_patches(&base, &[], MergeKeySource::AssignmentKey);
		assert_eq!(result.len(), 2);
		assert_eq!(result, base);
	}

	#[test]
	fn insert_node_appends_statement() {
		let base = vec![assignment("tax", scalar("5"))];
		let patches = vec![ClausewitzPatch::InsertNode {
			path: vec![],
			key: "manpower".to_string(),
			statement: assignment("manpower", scalar("1000")),
		}];
		let result = apply_patches(&base, &patches, MergeKeySource::AssignmentKey);
		assert_eq!(result.len(), 2);
		assert!(
			matches!(&result[1], AstStatement::Assignment { key, .. } if key == "manpower")
		);
	}

	#[test]
	fn remove_node_drops_statement() {
		let base = vec![
			assignment("tax", scalar("5")),
			assignment("manpower", scalar("1000")),
		];
		let patches = vec![ClausewitzPatch::RemoveNode {
			path: vec![],
			key: "manpower".to_string(),
			removed: assignment("manpower", scalar("1000")),
		}];
		let result = apply_patches(&base, &patches, MergeKeySource::AssignmentKey);
		assert_eq!(result.len(), 1);
		assert!(
			matches!(&result[0], AstStatement::Assignment { key, .. } if key == "tax")
		);
	}

	#[test]
	fn set_value_changes_scalar() {
		let base = vec![assignment("tax", scalar("5"))];
		let patches = vec![ClausewitzPatch::SetValue {
			path: vec![],
			key: "tax".to_string(),
			old_value: scalar("5"),
			new_value: scalar("10"),
		}];
		let result = apply_patches(&base, &patches, MergeKeySource::AssignmentKey);
		assert_eq!(result.len(), 1);
		assert!(matches!(
			&result[0],
			AstStatement::Assignment {
				key,
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(v),
					..
				},
				..
			} if key == "tax" && v == "10"
		));
	}

	#[test]
	fn append_list_item_adds_to_block() {
		// AppendListItem targets repeated keys at the same level.
		// Base has one `tag = FRA`, overlay adds a second `tag = ENG`.
		let base = vec![
			assignment("tag", scalar("FRA")),
			assignment("tag", scalar("SPA")),
		];
		let patches = vec![ClausewitzPatch::AppendListItem {
			path: vec![],
			key: "tag".to_string(),
			value: scalar("ENG"),
		}];
		let result = apply_patches(&base, &patches, MergeKeySource::AssignmentKey);
		// FRA and SPA pass through; ENG is appended via InsertNode-like logic.
		// AppendListItem at path=[] creates a new top-level assignment.
		assert_eq!(result.len(), 3);
		assert!(matches!(
			&result[2],
			AstStatement::Assignment {
				key,
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(v),
					..
				},
				..
			} if key == "tag" && v == "ENG"
		));
	}

	#[test]
	fn remove_list_item_drops_from_repeated() {
		let base = vec![
			assignment("tag", scalar("FRA")),
			assignment("tag", scalar("ENG")),
			assignment("tag", scalar("SPA")),
		];
		let patches = vec![ClausewitzPatch::RemoveListItem {
			path: vec![],
			key: "tag".to_string(),
			value: scalar("ENG"),
		}];
		let result = apply_patches(&base, &patches, MergeKeySource::AssignmentKey);
		assert_eq!(result.len(), 2);
		// FRA and SPA should remain.
		assert!(matches!(
			&result[0],
			AstStatement::Assignment {
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(v),
					..
				},
				..
			} if v == "FRA"
		));
		assert!(matches!(
			&result[1],
			AstStatement::Assignment {
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(v),
					..
				},
				..
			} if v == "SPA"
		));
	}

	#[test]
	fn nested_set_value_modifies_inner_block() {
		let base = vec![block(
			"my_trigger",
			vec![
				block("OR", vec![assignment("tag", scalar("FRA"))]),
				assignment("is_subject", scalar("no")),
			],
		)];
		let patches = vec![ClausewitzPatch::SetValue {
			path: vec!["my_trigger".to_string()],
			key: "is_subject".to_string(),
			old_value: scalar("no"),
			new_value: scalar("yes"),
		}];
		let result = apply_patches(&base, &patches, MergeKeySource::AssignmentKey);
		assert_eq!(result.len(), 1);
		if let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = &result[0]
		{
			assert_eq!(items.len(), 2);
			assert!(matches!(
				&items[1],
				AstStatement::Assignment {
					key,
					value: AstValue::Scalar {
						value: ScalarValue::Identifier(v),
						..
					},
					..
				} if key == "is_subject" && v == "yes"
			));
		} else {
			panic!("expected block assignment");
		}
	}

	#[test]
	fn replace_block_swaps_statement() {
		let old_stmt = block("my_effect", vec![assignment("add_stability", scalar("1"))]);
		let new_stmt = block(
			"my_effect",
			vec![
				assignment("add_stability", scalar("2")),
				assignment("add_prestige", scalar("10")),
			],
		);
		let base = vec![old_stmt.clone()];
		let patches = vec![ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "my_effect".to_string(),
			old_statement: old_stmt,
			new_statement: new_stmt.clone(),
		}];
		let result = apply_patches(&base, &patches, MergeKeySource::AssignmentKey);
		assert_eq!(result.len(), 1);
		assert_eq!(result[0], new_stmt);
	}

	#[test]
	fn merge_single_mod_roundtrip() {
		let base_stmts = vec![
			assignment("tax", scalar("5")),
			block("trigger", vec![assignment("tag", scalar("FRA"))]),
		];
		let overlay_stmts = vec![
			assignment("tax", scalar("10")),
			block("trigger", vec![assignment("tag", scalar("ENG"))]),
		];
		let base = make_parsed(base_stmts);
		let overlay = make_parsed(overlay_stmts.clone());

		let result = merge_single_mod(&base, &overlay, MergeKeySource::AssignmentKey);

		// Should match the overlay's content.
		assert_eq!(result.len(), 2);

		// tax should be 10
		assert!(matches!(
			&result[0],
			AstStatement::Assignment {
				key,
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(v),
					..
				},
				..
			} if key == "tax" && v == "10"
		));

		// trigger.tag should be ENG
		if let AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} = &result[1]
		{
			assert_eq!(key, "trigger");
			assert!(matches!(
				&items[0],
				AstStatement::Assignment {
					key: k,
					value: AstValue::Scalar {
						value: ScalarValue::Identifier(v),
						..
					},
					..
				} if k == "tag" && v == "ENG"
			));
		} else {
			panic!("expected block trigger");
		}
	}
}
