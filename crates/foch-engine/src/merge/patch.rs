use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use foch_language::analyzer::content_family::MergeKeySource;
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};
use foch_language::analyzer::semantic_index::{ParsedScriptFile, is_decision_container_key};

/// A path into the Clausewitz AST: sequence of keys from root to target node.
pub type AstPath = Vec<String>;

/// A structural patch operation between a base game file and a mod overlay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ClausewitzPatch {
	/// Set/change a scalar value at a path.
	SetValue {
		path: AstPath,
		key: String,
		old_value: AstValue,
		new_value: AstValue,
	},
	/// Remove a key-value pair or block.
	RemoveNode {
		path: AstPath,
		key: String,
		removed: AstStatement,
	},
	/// Insert a new key-value pair or block.
	InsertNode {
		path: AstPath,
		key: String,
		statement: AstStatement,
	},
	/// Append to a repeated-key list (e.g. `tag = ERS` added to an OR block).
	AppendListItem {
		path: AstPath,
		key: String,
		value: AstValue,
	},
	/// Remove from a repeated-key list.
	RemoveListItem {
		path: AstPath,
		key: String,
		value: AstValue,
	},
	/// Replace entire block when diff is too large to be useful per-node.
	ReplaceBlock {
		path: AstPath,
		key: String,
		old_statement: AstStatement,
		new_statement: AstStatement,
	},
}

/// Compute the structural diff between a base game file and a mod overlay,
/// producing a list of patch operations.
pub fn diff_ast(
	base: &ParsedScriptFile,
	overlay: &ParsedScriptFile,
	merge_key_source: MergeKeySource,
) -> Vec<ClausewitzPatch> {
	let base_entries = extract_keyed_entries(&base.ast.statements, merge_key_source);
	let overlay_entries = extract_keyed_entries(&overlay.ast.statements, merge_key_source);
	diff_entry_maps(&base_entries, &overlay_entries, &[], 0)
}

/// Maximum recursion depth for block diffing.  Beyond this, emit ReplaceBlock.
const MAX_DIFF_DEPTH: usize = 12;

// ---------------------------------------------------------------------------
// Keyed entry extraction
// ---------------------------------------------------------------------------

struct KeyedEntry {
	merge_key: String,
	statement: AstStatement,
	path_prefix: Vec<String>,
}

fn extract_keyed_entries(
	statements: &[AstStatement],
	merge_key_source: MergeKeySource,
) -> Vec<KeyedEntry> {
	match merge_key_source {
		MergeKeySource::AssignmentKey => extract_assignment_entries(statements),
		MergeKeySource::FieldValue(field) => extract_field_value_entries(statements, field),
		MergeKeySource::ContainerChildKey => extract_container_child_entries(statements),
		MergeKeySource::LeafPath => extract_leaf_entries(statements),
	}
}

fn extract_assignment_entries(statements: &[AstStatement]) -> Vec<KeyedEntry> {
	statements
		.iter()
		.filter_map(|stmt| {
			if let AstStatement::Assignment { key, .. } = stmt {
				Some(KeyedEntry {
					merge_key: key.clone(),
					statement: stmt.clone(),
					path_prefix: Vec::new(),
				})
			} else {
				None
			}
		})
		.collect()
}

fn extract_field_value_entries(statements: &[AstStatement], field: &str) -> Vec<KeyedEntry> {
	statements
		.iter()
		.filter_map(|stmt| {
			if let AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} = stmt
			{
				let key_val = scalar_assignment_value(items, field)?;
				Some(KeyedEntry {
					merge_key: key_val,
					statement: stmt.clone(),
					path_prefix: Vec::new(),
				})
			} else {
				None
			}
		})
		.collect()
}

fn extract_container_child_entries(statements: &[AstStatement]) -> Vec<KeyedEntry> {
	statements
		.iter()
		.flat_map(|stmt| {
			let mut out = Vec::new();
			if let AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} = stmt
			{
				if !is_decision_container_key(key) {
					return out;
				}
				for child in items {
					if let AstStatement::Assignment { key: child_key, .. } = child {
						out.push(KeyedEntry {
							merge_key: child_key.clone(),
							statement: child.clone(),
							path_prefix: vec![key.clone()],
						});
					}
				}
			}
			out
		})
		.collect()
}

fn extract_leaf_entries(statements: &[AstStatement]) -> Vec<KeyedEntry> {
	extract_assignment_entries(statements)
}

fn scalar_assignment_value(items: &[AstStatement], expected_key: &str) -> Option<String> {
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != expected_key {
			continue;
		}
		if let AstValue::Scalar { value, .. } = value {
			return Some(value.as_text());
		}
	}
	None
}

// ---------------------------------------------------------------------------
// Diff engine
// ---------------------------------------------------------------------------

/// Build a multimap from merge-key → list of statements (preserving order).
fn build_key_map(entries: &[KeyedEntry]) -> BTreeMap<String, Vec<&AstStatement>> {
	let mut map: BTreeMap<String, Vec<&AstStatement>> = BTreeMap::new();
	for entry in entries {
		map.entry(entry.merge_key.clone())
			.or_default()
			.push(&entry.statement);
	}
	map
}

fn diff_entry_maps(
	base_entries: &[KeyedEntry],
	overlay_entries: &[KeyedEntry],
	parent_path: &[String],
	depth: usize,
) -> Vec<ClausewitzPatch> {
	let base_map = build_key_map(base_entries);
	let overlay_map = build_key_map(overlay_entries);

	// Merge path prefixes: use the first entry's prefix if available.
	let path = resolve_path(parent_path, base_entries, overlay_entries);

	let mut patches = Vec::new();

	// Keys in base but not overlay → removed.
	for (key, base_stmts) in &base_map {
		if !overlay_map.contains_key(key) {
			if base_stmts.len() == 1 {
				patches.push(ClausewitzPatch::RemoveNode {
					path: path.clone(),
					key: key.clone(),
					removed: base_stmts[0].clone(),
				});
			} else {
				for stmt in base_stmts {
					if let Some(val) = statement_value(stmt) {
						patches.push(ClausewitzPatch::RemoveListItem {
							path: path.clone(),
							key: key.clone(),
							value: val.clone(),
						});
					}
				}
			}
		}
	}

	// Keys in overlay but not base → inserted.
	for (key, overlay_stmts) in &overlay_map {
		if !base_map.contains_key(key) {
			if overlay_stmts.len() == 1 {
				patches.push(ClausewitzPatch::InsertNode {
					path: path.clone(),
					key: key.clone(),
					statement: overlay_stmts[0].clone(),
				});
			} else {
				for stmt in overlay_stmts {
					if let Some(val) = statement_value(stmt) {
						patches.push(ClausewitzPatch::AppendListItem {
							path: path.clone(),
							key: key.clone(),
							value: val.clone(),
						});
					}
				}
			}
		}
	}

	// Keys in both → diff.
	for (key, base_stmts) in &base_map {
		let Some(overlay_stmts) = overlay_map.get(key) else {
			continue;
		};

		if base_stmts.len() == 1 && overlay_stmts.len() == 1 {
			diff_single_statement(
				key,
				base_stmts[0],
				overlay_stmts[0],
				&path,
				&mut patches,
				depth,
			);
		} else {
			diff_repeated_key(key, base_stmts, overlay_stmts, &path, &mut patches);
		}
	}

	patches
}

fn resolve_path(
	parent_path: &[String],
	base_entries: &[KeyedEntry],
	overlay_entries: &[KeyedEntry],
) -> AstPath {
	if !parent_path.is_empty() {
		return parent_path.to_vec();
	}
	let prefix = base_entries
		.first()
		.or(overlay_entries.first())
		.map(|e| &e.path_prefix);
	match prefix {
		Some(p) if !p.is_empty() => p.clone(),
		_ => Vec::new(),
	}
}

fn statement_value(stmt: &AstStatement) -> Option<&AstValue> {
	match stmt {
		AstStatement::Assignment { value, .. } => Some(value),
		AstStatement::Item { value, .. } => Some(value),
		AstStatement::Comment { .. } => None,
	}
}

// ---------------------------------------------------------------------------
// Span-ignoring comparison (ASTs from different files have different spans)
// ---------------------------------------------------------------------------

fn values_equal_ignoring_span(a: &AstValue, b: &AstValue) -> bool {
	match (a, b) {
		(AstValue::Scalar { value: va, .. }, AstValue::Scalar { value: vb, .. }) => va == vb,
		(AstValue::Block { items: ia, .. }, AstValue::Block { items: ib, .. }) => {
			ia.len() == ib.len()
				&& ia
					.iter()
					.zip(ib.iter())
					.all(|(sa, sb)| statements_equal_ignoring_span(sa, sb))
		}
		_ => false,
	}
}

fn statements_equal_ignoring_span(a: &AstStatement, b: &AstStatement) -> bool {
	match (a, b) {
		(
			AstStatement::Assignment {
				key: ka, value: va, ..
			},
			AstStatement::Assignment {
				key: kb, value: vb, ..
			},
		) => ka == kb && values_equal_ignoring_span(va, vb),
		(AstStatement::Item { value: va, .. }, AstStatement::Item { value: vb, .. }) => {
			values_equal_ignoring_span(va, vb)
		}
		(AstStatement::Comment { text: ta, .. }, AstStatement::Comment { text: tb, .. }) => {
			ta == tb
		}
		_ => false,
	}
}

/// Compare scalar values ignoring span (for list-level dedup).
#[allow(dead_code)]
fn scalar_values_equal(a: &ScalarValue, b: &ScalarValue) -> bool {
	a == b
}

// ---------------------------------------------------------------------------
// Single-statement diff
// ---------------------------------------------------------------------------

fn diff_single_statement(
	key: &str,
	base: &AstStatement,
	overlay: &AstStatement,
	path: &[String],
	patches: &mut Vec<ClausewitzPatch>,
	depth: usize,
) {
	if statements_equal_ignoring_span(base, overlay) {
		return;
	}

	let (Some(base_val), Some(overlay_val)) = (statement_value(base), statement_value(overlay))
	else {
		return;
	};

	match (base_val, overlay_val) {
		// Both scalars with different values.
		(AstValue::Scalar { .. }, AstValue::Scalar { .. }) => {
			patches.push(ClausewitzPatch::SetValue {
				path: path.to_vec(),
				key: key.to_string(),
				old_value: base_val.clone(),
				new_value: overlay_val.clone(),
			});
		}
		// Both blocks → recursively diff children.
		(
			AstValue::Block {
				items: base_items, ..
			},
			AstValue::Block {
				items: overlay_items,
				..
			},
		) => {
			diff_blocks(
				key,
				base,
				overlay,
				base_items,
				overlay_items,
				path,
				patches,
				depth,
			);
		}
		// Type mismatch (scalar↔block) → replace.
		_ => {
			patches.push(ClausewitzPatch::ReplaceBlock {
				path: path.to_vec(),
				key: key.to_string(),
				old_statement: base.clone(),
				new_statement: overlay.clone(),
			});
		}
	}
}

// ---------------------------------------------------------------------------
// Block-level diff
// ---------------------------------------------------------------------------

/// Threshold: if >80% of children changed, emit `ReplaceBlock`.
const REPLACE_THRESHOLD: f64 = 0.8;

#[allow(clippy::too_many_arguments)]
fn diff_blocks(
	key: &str,
	base_stmt: &AstStatement,
	overlay_stmt: &AstStatement,
	base_items: &[AstStatement],
	overlay_items: &[AstStatement],
	parent_path: &[String],
	patches: &mut Vec<ClausewitzPatch>,
	depth: usize,
) {
	// Depth limit: emit ReplaceBlock instead of recursing further.
	if depth >= MAX_DIFF_DEPTH {
		if !statements_equal_ignoring_span(base_stmt, overlay_stmt) {
			patches.push(ClausewitzPatch::ReplaceBlock {
				path: parent_path.to_vec(),
				key: key.to_string(),
				old_statement: base_stmt.clone(),
				new_statement: overlay_stmt.clone(),
			});
		}
		return;
	}
	let child_path: Vec<String> = {
		let mut p = parent_path.to_vec();
		p.push(key.to_string());
		p
	};

	let base_children = index_children(base_items);
	let overlay_children = index_children(overlay_items);

	let total_keys: usize = {
		let mut all_keys: Vec<&String> = base_children
			.keys()
			.chain(overlay_children.keys())
			.collect();
		all_keys.sort();
		all_keys.dedup();
		all_keys.len()
	};

	if total_keys == 0 {
		return;
	}

	// Trial diff: count how many keys are changed.
	let mut changed = 0usize;
	for (k, base_vals) in &base_children {
		match overlay_children.get(k) {
			None => changed += 1,
			Some(overlay_vals) => {
				if !value_lists_equal_ignoring_span(base_vals, overlay_vals) {
					changed += 1;
				}
			}
		}
	}
	for k in overlay_children.keys() {
		if !base_children.contains_key(k) {
			changed += 1;
		}
	}

	let ratio = changed as f64 / total_keys as f64;
	if ratio > REPLACE_THRESHOLD {
		patches.push(ClausewitzPatch::ReplaceBlock {
			path: parent_path.to_vec(),
			key: key.to_string(),
			old_statement: base_stmt.clone(),
			new_statement: overlay_stmt.clone(),
		});
		return;
	}

	// Produce per-child patches.
	let base_child_entries: Vec<KeyedEntry> = child_keyed_entries(base_items);
	let overlay_child_entries: Vec<KeyedEntry> = child_keyed_entries(overlay_items);
	let child_patches = diff_entry_maps(
		&base_child_entries,
		&overlay_child_entries,
		&child_path,
		depth + 1,
	);
	patches.extend(child_patches);
}

/// Build a map from child key → list of values (for change-counting).
fn index_children(items: &[AstStatement]) -> HashMap<String, Vec<&AstValue>> {
	let mut map: HashMap<String, Vec<&AstValue>> = HashMap::new();
	for item in items {
		if let AstStatement::Assignment { key, value, .. } = item {
			map.entry(key.clone()).or_default().push(value);
		}
	}
	map
}

fn value_lists_equal_ignoring_span(a: &[&AstValue], b: &[&AstValue]) -> bool {
	a.len() == b.len()
		&& a.iter()
			.zip(b.iter())
			.all(|(va, vb)| values_equal_ignoring_span(va, vb))
}

/// Convert child statements into keyed entries using `AssignmentKey` semantics.
fn child_keyed_entries(items: &[AstStatement]) -> Vec<KeyedEntry> {
	items
		.iter()
		.filter_map(|stmt| {
			if let AstStatement::Assignment { key, .. } = stmt {
				Some(KeyedEntry {
					merge_key: key.clone(),
					statement: stmt.clone(),
					path_prefix: Vec::new(),
				})
			} else {
				None
			}
		})
		.collect()
}

// ---------------------------------------------------------------------------
// Repeated-key (list semantics) diff
// ---------------------------------------------------------------------------

fn diff_repeated_key(
	key: &str,
	base_stmts: &[&AstStatement],
	overlay_stmts: &[&AstStatement],
	path: &[String],
	patches: &mut Vec<ClausewitzPatch>,
) {
	let base_values: Vec<&AstValue> = base_stmts
		.iter()
		.filter_map(|s| statement_value(s))
		.collect();
	let overlay_values: Vec<&AstValue> = overlay_stmts
		.iter()
		.filter_map(|s| statement_value(s))
		.collect();

	// Compare as sets using span-ignoring structural equality.
	for bv in &base_values {
		if !overlay_values
			.iter()
			.any(|ov| values_equal_ignoring_span(ov, bv))
		{
			patches.push(ClausewitzPatch::RemoveListItem {
				path: path.to_vec(),
				key: key.to_string(),
				value: (*bv).clone(),
			});
		}
	}

	for ov in &overlay_values {
		if !base_values
			.iter()
			.any(|bv| values_equal_ignoring_span(bv, ov))
		{
			patches.push(ClausewitzPatch::AppendListItem {
				path: path.to_vec(),
				key: key.to_string(),
				value: (*ov).clone(),
			});
		}
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

	fn dummy_span() -> SpanRange {
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
			span: dummy_span(),
		}
	}

	fn assignment(key: &str, value: AstValue) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: dummy_span(),
			value,
			span: dummy_span(),
		}
	}

	fn block(key: &str, items: Vec<AstStatement>) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: dummy_span(),
			value: AstValue::Block {
				items,
				span: dummy_span(),
			},
			span: dummy_span(),
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
	fn identical_files_produce_empty_patches() {
		let stmts = vec![
			block(
				"country_event",
				vec![
					assignment("id", scalar("evt.1")),
					assignment("title", scalar("my_event")),
				],
			),
			block("province_event", vec![assignment("id", scalar("evt.2"))]),
		];
		let base = make_parsed(stmts.clone());
		let overlay = make_parsed(stmts);

		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		assert!(
			patches.is_empty(),
			"identical files should produce no patches"
		);
	}

	#[test]
	fn added_key_produces_insert_node() {
		let base = make_parsed(vec![block(
			"event_a",
			vec![assignment("id", scalar("a.1"))],
		)]);
		let overlay = make_parsed(vec![
			block("event_a", vec![assignment("id", scalar("a.1"))]),
			block("event_b", vec![assignment("id", scalar("b.1"))]),
		]);

		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		assert_eq!(patches.len(), 1);
		assert!(
			matches!(&patches[0], ClausewitzPatch::InsertNode { key, .. } if key == "event_b"),
			"expected InsertNode for event_b, got {:?}",
			patches[0]
		);
	}

	#[test]
	fn removed_key_produces_remove_node() {
		let base = make_parsed(vec![
			block("event_a", vec![assignment("id", scalar("a.1"))]),
			block("event_b", vec![assignment("id", scalar("b.1"))]),
		]);
		let overlay = make_parsed(vec![block(
			"event_a",
			vec![assignment("id", scalar("a.1"))],
		)]);

		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		assert_eq!(patches.len(), 1);
		assert!(
			matches!(&patches[0], ClausewitzPatch::RemoveNode { key, .. } if key == "event_b"),
			"expected RemoveNode for event_b, got {:?}",
			patches[0]
		);
	}

	#[test]
	fn changed_scalar_value_produces_set_value() {
		let base = make_parsed(vec![assignment("tax_income", scalar("5"))]);
		let overlay = make_parsed(vec![assignment("tax_income", scalar("10"))]);

		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		assert_eq!(patches.len(), 1);
		assert!(
			matches!(
				&patches[0],
				ClausewitzPatch::SetValue {
					key,
					old_value: AstValue::Scalar {
						value: ScalarValue::Identifier(old),
						..
					},
					new_value: AstValue::Scalar {
						value: ScalarValue::Identifier(new),
						..
					},
					..
				} if key == "tax_income" && old == "5" && new == "10"
			),
			"expected SetValue for tax_income 5→10, got {:?}",
			patches[0]
		);
	}

	#[test]
	fn added_list_item_produces_append_list_item() {
		let base = make_parsed(vec![
			assignment("tag", scalar("TRE")),
			assignment("tag", scalar("FEO")),
		]);
		let overlay = make_parsed(vec![
			assignment("tag", scalar("TRE")),
			assignment("tag", scalar("FEO")),
			assignment("tag", scalar("ERS")),
		]);

		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		assert_eq!(patches.len(), 1);
		assert!(
			matches!(
				&patches[0],
				ClausewitzPatch::AppendListItem {
					key,
					value: AstValue::Scalar {
						value: ScalarValue::Identifier(v),
						..
					},
					..
				} if key == "tag" && v == "ERS"
			),
			"expected AppendListItem for tag=ERS, got {:?}",
			patches[0]
		);
	}

	#[test]
	fn changed_nested_block_produces_recursive_patches() {
		let base = make_parsed(vec![block(
			"country_event",
			vec![
				assignment("id", scalar("evt.1")),
				assignment("title", scalar("old_title")),
				assignment("fire_only_once", scalar("yes")),
			],
		)]);
		let overlay = make_parsed(vec![block(
			"country_event",
			vec![
				assignment("id", scalar("evt.1")),
				assignment("title", scalar("new_title")),
				assignment("fire_only_once", scalar("yes")),
			],
		)]);

		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		// Only title changed (1/3 = 33%, below 80% threshold) → recursive diff.
		assert_eq!(patches.len(), 1);
		assert!(
			matches!(
				&patches[0],
				ClausewitzPatch::SetValue {
					path,
					key,
					..
				} if key == "title" && path == &vec!["country_event".to_string()]
			),
			"expected SetValue for title inside country_event, got {:?}",
			patches[0]
		);
	}

	#[test]
	fn completely_different_block_produces_replace_block() {
		// Base block with 5 children, overlay replaces all 5 → 100% changed → ReplaceBlock.
		let base = make_parsed(vec![block(
			"country_event",
			vec![
				assignment("a", scalar("1")),
				assignment("b", scalar("2")),
				assignment("c", scalar("3")),
				assignment("d", scalar("4")),
				assignment("e", scalar("5")),
			],
		)]);
		let overlay = make_parsed(vec![block(
			"country_event",
			vec![
				assignment("v", scalar("10")),
				assignment("w", scalar("20")),
				assignment("x", scalar("30")),
				assignment("y", scalar("40")),
				assignment("z", scalar("50")),
			],
		)]);

		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		assert_eq!(patches.len(), 1);
		assert!(
			matches!(
				&patches[0],
				ClausewitzPatch::ReplaceBlock { key, .. } if key == "country_event"
			),
			"expected ReplaceBlock for country_event, got {:?}",
			patches[0]
		);
	}
}
