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
	/// Append a bare-value `Item` inside a block (e.g. adding `TRE` to
	/// `allowed_tags = { FRA ENG }`). `path` is the path including the parent
	/// block's key; the item is added at the end of that block's body.
	AppendBlockItem { path: AstPath, value: AstValue },
	/// Remove a bare-value `Item` from a block. Matched by span-ignoring
	/// structural equality on the value.
	RemoveBlockItem { path: AstPath, value: AstValue },
	/// Synthesized from a same-file `RemoveNode { key: X }` +
	/// `InsertNode { key: Y }` pair (X != Y) where the removed body and the
	/// inserted statement body are AST-semantically equal. Marks an in-place
	/// rename; subsequent patches addressed at X (or any path that traverses
	/// X) are rewritten to Y during merge resolution.
	Rename {
		path: AstPath,
		old_key: String,
		new_key: String,
	},
}

/// Detect zero-cost renames within a single mod's patch list for one file.
///
/// Pairs `RemoveNode { path, key: X }` with `InsertNode { path, key: Y }` at
/// the same parent path when X != Y and their bodies are AST-semantically
/// equal. Emits a `Rename` patch and removes the paired Remove/Insert from
/// the original list. Patches that do not pair are left untouched.
///
/// Mirrors git's hash-equal blob rename detection: only exact body equality
/// counts, so there are no fuzzy false positives.
pub fn fold_renames(patches: Vec<ClausewitzPatch>) -> Vec<ClausewitzPatch> {
	use std::collections::{HashMap as Map, HashSet as Set};

	let mut removes_at: Map<AstPath, Vec<usize>> = Map::new();
	let mut inserts_at: Map<AstPath, Vec<usize>> = Map::new();

	for (i, p) in patches.iter().enumerate() {
		match p {
			ClausewitzPatch::RemoveNode { path, .. } => {
				removes_at.entry(path.clone()).or_default().push(i);
			}
			ClausewitzPatch::InsertNode { path, .. } => {
				inserts_at.entry(path.clone()).or_default().push(i);
			}
			_ => {}
		}
	}

	let mut consumed: Set<usize> = Set::new();
	let mut renames: Vec<(AstPath, String, String)> = Vec::new();

	for (path, rem_indices) in &removes_at {
		let Some(ins_indices) = inserts_at.get(path) else {
			continue;
		};
		for &ri in rem_indices {
			if consumed.contains(&ri) {
				continue;
			}
			let (rkey, rbody) = match &patches[ri] {
				ClausewitzPatch::RemoveNode {
					key,
					removed: AstStatement::Assignment { value, .. },
					..
				} => (key.clone(), value),
				_ => continue,
			};
			for &ii in ins_indices {
				if consumed.contains(&ii) {
					continue;
				}
				let (ikey, ibody) = match &patches[ii] {
					ClausewitzPatch::InsertNode {
						key,
						statement: AstStatement::Assignment { value, .. },
						..
					} => (key.clone(), value),
					_ => continue,
				};
				if ikey == rkey {
					continue;
				}
				if ast_values_semantically_equal(rbody, ibody) {
					consumed.insert(ri);
					consumed.insert(ii);
					renames.push((path.clone(), rkey.clone(), ikey));
					break;
				}
			}
		}
	}

	if renames.is_empty() {
		return patches;
	}

	let mut out: Vec<ClausewitzPatch> = Vec::with_capacity(patches.len());
	for (i, p) in patches.into_iter().enumerate() {
		if !consumed.contains(&i) {
			out.push(p);
		}
	}
	for (path, old_key, new_key) in renames {
		out.push(ClausewitzPatch::Rename {
			path,
			old_key,
			new_key,
		});
	}
	out
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

/// Diff two block bodies (children of a parent block) and produce a flat
/// list of `ClausewitzPatch` operations. Used by
/// `BlockPatchPolicy::Recurse` to deep-merge multiple mods' replacements
/// of the same block.
///
/// Patch paths are emitted relative to `parent_path`. Pass `&[]` to make
/// patches addressable directly against the body via `apply_patches`.
/// `merge_key_source` is currently always `AssignmentKey`; included for
/// symmetry with `diff_ast`.
pub fn diff_block_bodies(
	base_items: &[AstStatement],
	overlay_items: &[AstStatement],
	parent_path: &[String],
	depth: usize,
	merge_key_source: MergeKeySource,
) -> Vec<ClausewitzPatch> {
	// Bare-Item set diff (e.g. `{ FRA ENG }`).
	let base_block_items: Vec<&AstValue> = base_items
		.iter()
		.filter_map(|s| match s {
			AstStatement::Item { value, .. } => Some(value),
			_ => None,
		})
		.collect();
	let overlay_block_items: Vec<&AstValue> = overlay_items
		.iter()
		.filter_map(|s| match s {
			AstStatement::Item { value, .. } => Some(value),
			_ => None,
		})
		.collect();
	let removed_items: Vec<AstValue> = base_block_items
		.iter()
		.copied()
		.filter(|bv| {
			!overlay_block_items
				.iter()
				.any(|ov| ast_values_semantically_equal(ov, bv))
		})
		.cloned()
		.collect();
	let added_items: Vec<AstValue> = overlay_block_items
		.iter()
		.copied()
		.filter(|ov| {
			!base_block_items
				.iter()
				.any(|bv| ast_values_semantically_equal(bv, ov))
		})
		.cloned()
		.collect();

	let mut patches = Vec::new();
	for v in removed_items {
		patches.push(ClausewitzPatch::RemoveBlockItem {
			path: parent_path.to_vec(),
			value: v,
		});
	}
	for v in added_items {
		patches.push(ClausewitzPatch::AppendBlockItem {
			path: parent_path.to_vec(),
			value: v,
		});
	}

	// Per-key Assignment diff (recurses through nested blocks via diff_blocks).
	let base_entries = extract_keyed_entries(base_items, merge_key_source);
	let overlay_entries = extract_keyed_entries(overlay_items, merge_key_source);
	let child_patches = diff_entry_maps(&base_entries, &overlay_entries, parent_path, depth);
	patches.extend(child_patches);
	patches
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
		MergeKeySource::ContainerChildFieldValue {
			container,
			child_key_field,
			child_types,
		} => extract_container_child_field_value_entries(
			statements,
			container,
			child_key_field,
			child_types,
		),
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

fn extract_container_child_field_value_entries(
	statements: &[AstStatement],
	container: &str,
	child_key_field: &str,
	child_types: &[&str],
) -> Vec<KeyedEntry> {
	let mut out = Vec::new();
	for stmt in statements {
		let AstStatement::Assignment { key, value, .. } = stmt else {
			continue;
		};
		if key != container {
			out.push(KeyedEntry {
				merge_key: key.clone(),
				statement: stmt.clone(),
				path_prefix: Vec::new(),
			});
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		for child in items {
			let Some(merge_key) =
				container_child_field_value_key(child, child_key_field, child_types)
			else {
				continue;
			};
			out.push(KeyedEntry {
				merge_key,
				statement: child.clone(),
				path_prefix: vec![key.clone()],
			});
		}
	}
	out
}

fn container_child_field_value_key(
	stmt: &AstStatement,
	child_key_field: &str,
	child_types: &[&str],
) -> Option<String> {
	let AstStatement::Assignment { key, value, .. } = stmt else {
		return None;
	};
	if (child_types.is_empty() || child_types.contains(&key.as_str()))
		&& let AstValue::Block { items, .. } = value
		&& let Some(field_value) = scalar_assignment_value(items, child_key_field)
	{
		return Some(format!("{key}:{field_value}"));
	}
	Some(key.clone())
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

pub(crate) fn values_equal_ignoring_span(a: &AstValue, b: &AstValue) -> bool {
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

fn scalar_values_semantically_equal(a: &ScalarValue, b: &ScalarValue) -> bool {
	match (a, b) {
		(ScalarValue::Identifier(a), ScalarValue::String(b))
		| (ScalarValue::String(b), ScalarValue::Identifier(a)) => {
			a == b && is_valid_bare_identifier_text(a)
		}
		_ => a == b,
	}
}

fn is_valid_bare_identifier_text(value: &str) -> bool {
	let Some(&first) = value.as_bytes().first() else {
		return false;
	};
	!matches!(first, b'"' | b'-' | b'0'..=b'9')
		&& !matches!(value.to_ascii_lowercase().as_str(), "yes" | "no")
		&& !value.bytes().any(|byte| {
			matches!(
				byte,
				b' ' | b'\t' | b'\r' | b'\n' | b'=' | b'{' | b'}' | b'#'
			)
		})
}

// ---------------------------------------------------------------------------
// Semantic equality (ignores spans AND comments)
// ---------------------------------------------------------------------------
//
// `values_equal_ignoring_span` and friends above compare comment text. That
// is too strict for *patch convergence*: when two mods produce the same
// `ReplaceBlock` content but differ only in comments, blank lines, or
// formatting, we still want them to converge.
//
// The helpers below compare AST nodes for semantic equivalence:
//   * spans are ignored (every value/statement carries spans, never compared);
//   * `AstStatement::Comment` is filtered out of block bodies before zipping;
//   * order of remaining (non-comment) statements is preserved — a different
//     order is treated as a real difference.

/// Semantic equality on `AstValue` — ignores spans and (inside blocks) any
/// `Comment` statements. Order of the remaining statements matters.
pub(crate) fn ast_values_semantically_equal(a: &AstValue, b: &AstValue) -> bool {
	match (a, b) {
		(AstValue::Scalar { value: va, .. }, AstValue::Scalar { value: vb, .. }) => {
			scalar_values_semantically_equal(va, vb)
		}
		(AstValue::Block { items: ia, .. }, AstValue::Block { items: ib, .. }) => {
			let ia: Vec<&AstStatement> = ia
				.iter()
				.filter(|s| !matches!(s, AstStatement::Comment { .. }))
				.collect();
			let ib: Vec<&AstStatement> = ib
				.iter()
				.filter(|s| !matches!(s, AstStatement::Comment { .. }))
				.collect();
			ia.len() == ib.len()
				&& ia
					.iter()
					.zip(ib.iter())
					.all(|(sa, sb)| ast_statements_semantically_equal(sa, sb))
		}
		_ => false,
	}
}

/// Semantic equality on `AstStatement`. Comments compared at the top level
/// (i.e. when the caller hands two `Comment` statements directly) are
/// considered equal — convergence at that granularity treats comment-only
/// patches as equivalent. Inside blocks, comments are filtered out by
/// `ast_values_semantically_equal` and never reach this function.
pub(crate) fn ast_statements_semantically_equal(a: &AstStatement, b: &AstStatement) -> bool {
	match (a, b) {
		(
			AstStatement::Assignment {
				key: ka, value: va, ..
			},
			AstStatement::Assignment {
				key: kb, value: vb, ..
			},
		) => ka == kb && ast_values_semantically_equal(va, vb),
		(AstStatement::Item { value: va, .. }, AstStatement::Item { value: vb, .. }) => {
			ast_values_semantically_equal(va, vb)
		}
		(AstStatement::Comment { .. }, AstStatement::Comment { .. }) => true,
		_ => false,
	}
}

/// Semantic equality on a top-level statement list — compares two file
/// bodies (or any sequence of `AstStatement`) ignoring spans, with
/// comments filtered out at every level so cosmetic-only differences do
/// not register. Used by the file-level NoOp detector to recognize when a
/// patch-merged output is byte-equivalent to the vanilla base, in which
/// case shipping it would just shadow the game's own copy.
pub fn ast_statement_lists_semantically_equal(a: &[AstStatement], b: &[AstStatement]) -> bool {
	let a_filtered: Vec<&AstStatement> = a
		.iter()
		.filter(|s| !matches!(s, AstStatement::Comment { .. }))
		.collect();
	let b_filtered: Vec<&AstStatement> = b
		.iter()
		.filter(|s| !matches!(s, AstStatement::Comment { .. }))
		.collect();
	a_filtered.len() == b_filtered.len()
		&& a_filtered
			.iter()
			.zip(b_filtered.iter())
			.all(|(sa, sb)| ast_statements_semantically_equal(sa, sb))
}

/// Semantic equality on `ClausewitzPatch`. Compares the patch variant, path,
/// key, and embedded AST nodes via the span/comment-tolerant helpers above.
pub(crate) fn patches_semantically_equal(a: &ClausewitzPatch, b: &ClausewitzPatch) -> bool {
	match (a, b) {
		(
			ClausewitzPatch::SetValue {
				path: pa,
				key: ka,
				old_value: oa,
				new_value: na,
			},
			ClausewitzPatch::SetValue {
				path: pb,
				key: kb,
				old_value: ob,
				new_value: nb,
			},
		) => {
			pa == pb
				&& ka == kb && ast_values_semantically_equal(oa, ob)
				&& ast_values_semantically_equal(na, nb)
		}
		(
			ClausewitzPatch::RemoveNode {
				path: pa,
				key: ka,
				removed: ra,
			},
			ClausewitzPatch::RemoveNode {
				path: pb,
				key: kb,
				removed: rb,
			},
		) => pa == pb && ka == kb && ast_statements_semantically_equal(ra, rb),
		(
			ClausewitzPatch::InsertNode {
				path: pa,
				key: ka,
				statement: sa,
			},
			ClausewitzPatch::InsertNode {
				path: pb,
				key: kb,
				statement: sb,
			},
		) => pa == pb && ka == kb && ast_statements_semantically_equal(sa, sb),
		(
			ClausewitzPatch::AppendListItem {
				path: pa,
				key: ka,
				value: va,
			},
			ClausewitzPatch::AppendListItem {
				path: pb,
				key: kb,
				value: vb,
			},
		) => pa == pb && ka == kb && ast_values_semantically_equal(va, vb),
		(
			ClausewitzPatch::RemoveListItem {
				path: pa,
				key: ka,
				value: va,
			},
			ClausewitzPatch::RemoveListItem {
				path: pb,
				key: kb,
				value: vb,
			},
		) => pa == pb && ka == kb && ast_values_semantically_equal(va, vb),
		(
			ClausewitzPatch::ReplaceBlock {
				path: pa,
				key: ka,
				old_statement: oa,
				new_statement: na,
			},
			ClausewitzPatch::ReplaceBlock {
				path: pb,
				key: kb,
				old_statement: ob,
				new_statement: nb,
			},
		) => {
			pa == pb
				&& ka == kb && ast_statements_semantically_equal(oa, ob)
				&& ast_statements_semantically_equal(na, nb)
		}
		(
			ClausewitzPatch::AppendBlockItem {
				path: pa,
				value: va,
			},
			ClausewitzPatch::AppendBlockItem {
				path: pb,
				value: vb,
			},
		) => pa == pb && ast_values_semantically_equal(va, vb),
		(
			ClausewitzPatch::RemoveBlockItem {
				path: pa,
				value: va,
			},
			ClausewitzPatch::RemoveBlockItem {
				path: pb,
				value: vb,
			},
		) => pa == pb && ast_values_semantically_equal(va, vb),
		_ => false,
	}
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

	// Per-Item set diff for bare values (e.g. `allowed_tags = { FRA ENG }`).
	// Items are matched across base/overlay by span-ignoring structural value
	// equality. Items missing from overlay produce `RemoveBlockItem`; items
	// new in overlay produce `AppendBlockItem`. Their count contributes to
	// the ReplaceBlock threshold below.
	let base_block_items: Vec<&AstValue> = base_items
		.iter()
		.filter_map(|s| match s {
			AstStatement::Item { value, .. } => Some(value),
			_ => None,
		})
		.collect();
	let overlay_block_items: Vec<&AstValue> = overlay_items
		.iter()
		.filter_map(|s| match s {
			AstStatement::Item { value, .. } => Some(value),
			_ => None,
		})
		.collect();
	let removed_items: Vec<AstValue> = base_block_items
		.iter()
		.copied()
		.filter(|bv| {
			!overlay_block_items
				.iter()
				.any(|ov| ast_values_semantically_equal(ov, bv))
		})
		.cloned()
		.collect();
	let added_items: Vec<AstValue> = overlay_block_items
		.iter()
		.copied()
		.filter(|ov| {
			!base_block_items
				.iter()
				.any(|bv| ast_values_semantically_equal(bv, ov))
		})
		.cloned()
		.collect();
	let item_change_count = removed_items.len() + added_items.len();
	let total_item_units = base_block_items.len().max(overlay_block_items.len());

	if total_keys == 0 && total_item_units == 0 {
		// No keys and no Items on either side. Nothing to emit.
		return;
	}

	// Trial diff: count how many keys are changed.
	let mut changed = 0usize;
	for (k, base_vals) in &base_children {
		match overlay_children.get(k) {
			None => changed += 1,
			Some(overlay_vals) => {
				if !value_lists_semantically_equal(base_vals, overlay_vals) {
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

	let total_units = total_keys + total_item_units;
	let ratio = if total_units == 0 {
		0.0
	} else {
		(changed + item_change_count) as f64 / total_units as f64
	};
	if ratio > REPLACE_THRESHOLD {
		patches.push(ClausewitzPatch::ReplaceBlock {
			path: parent_path.to_vec(),
			key: key.to_string(),
			old_statement: base_stmt.clone(),
			new_statement: overlay_stmt.clone(),
		});
		return;
	}

	// Emit per-Item add/remove patches (set semantics).
	for v in removed_items {
		patches.push(ClausewitzPatch::RemoveBlockItem {
			path: child_path.clone(),
			value: v,
		});
	}
	for v in added_items {
		patches.push(ClausewitzPatch::AppendBlockItem {
			path: child_path.clone(),
			value: v,
		});
	}

	// Produce per-child patches for Assignment statements.
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

fn value_lists_semantically_equal(a: &[&AstValue], b: &[&AstValue]) -> bool {
	a.len() == b.len()
		&& a.iter()
			.zip(b.iter())
			.all(|(va, vb)| ast_values_semantically_equal(va, vb))
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

	// Compare as sets using semantic equality (ignores spans AND comments).
	// Comment-only differences must NOT trigger spurious Remove+Append pairs:
	// the patch_merge address layer fingerprints values without comments, so
	// such pairs land at the same address and surface as mixed-kind conflicts.
	for bv in &base_values {
		if !overlay_values
			.iter()
			.any(|ov| ast_values_semantically_equal(ov, bv))
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
			.any(|bv| ast_values_semantically_equal(bv, ov))
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

	fn string_scalar(value: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::String(value.to_string()),
			span: dummy_span(),
		}
	}

	fn number_scalar(value: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::Number(value.to_string()),
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
			source: String::new(),
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

	fn item(value: AstValue) -> AstStatement {
		AstStatement::Item {
			value,
			span: dummy_span(),
		}
	}

	#[test]
	fn block_item_addition_emits_append_block_item() {
		// Base: allowed_tags = { FRA ENG }, Overlay: allowed_tags = { FRA ENG TRE }
		let base = make_parsed(vec![block(
			"allowed_tags",
			vec![item(scalar("FRA")), item(scalar("ENG"))],
		)]);
		let overlay = make_parsed(vec![block(
			"allowed_tags",
			vec![
				item(scalar("FRA")),
				item(scalar("ENG")),
				item(scalar("TRE")),
			],
		)]);
		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		assert_eq!(patches.len(), 1, "got patches: {patches:?}");
		match &patches[0] {
			ClausewitzPatch::AppendBlockItem { path, value } => {
				assert_eq!(path, &vec!["allowed_tags".to_string()]);
				assert!(
					matches!(value, AstValue::Scalar { value: ScalarValue::Identifier(s), .. } if s == "TRE")
				);
			}
			other => panic!("expected AppendBlockItem, got {other:?}"),
		}
	}

	#[test]
	fn block_item_removal_emits_remove_block_item() {
		let base = make_parsed(vec![block(
			"allowed_tags",
			vec![item(scalar("FRA")), item(scalar("ENG"))],
		)]);
		let overlay = make_parsed(vec![block("allowed_tags", vec![item(scalar("FRA"))])]);
		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		assert_eq!(patches.len(), 1, "got patches: {patches:?}");
		match &patches[0] {
			ClausewitzPatch::RemoveBlockItem { path, value } => {
				assert_eq!(path, &vec!["allowed_tags".to_string()]);
				assert!(
					matches!(value, AstValue::Scalar { value: ScalarValue::Identifier(s), .. } if s == "ENG")
				);
			}
			other => panic!("expected RemoveBlockItem, got {other:?}"),
		}
	}

	#[test]
	fn block_with_mixed_items_and_assignments_diffs_both() {
		// Base: { id = evt.1 FRA }, Overlay: { id = evt.1 FRA TRE }
		let base = make_parsed(vec![block(
			"country_event",
			vec![assignment("id", scalar("evt.1")), item(scalar("FRA"))],
		)]);
		let overlay = make_parsed(vec![block(
			"country_event",
			vec![
				assignment("id", scalar("evt.1")),
				item(scalar("FRA")),
				item(scalar("TRE")),
			],
		)]);
		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		assert!(
			patches
				.iter()
				.any(|p| matches!(p, ClausewitzPatch::AppendBlockItem { .. })),
			"expected an AppendBlockItem, got {patches:?}"
		);
		assert!(
			!patches
				.iter()
				.any(|p| matches!(p, ClausewitzPatch::ReplaceBlock { .. })),
			"should not fall back to ReplaceBlock, got {patches:?}"
		);
	}

	// -----------------------------------------------------------------------
	// Semantic equality (ignores spans and comment trivia)
	// -----------------------------------------------------------------------

	fn span_at(line: usize, column: usize) -> SpanRange {
		SpanRange {
			start: Span {
				line,
				column,
				offset: 0,
			},
			end: Span {
				line,
				column,
				offset: 0,
			},
		}
	}

	fn comment(text: &str) -> AstStatement {
		AstStatement::Comment {
			text: text.to_string(),
			span: dummy_span(),
		}
	}

	fn replace_block(key: &str, old: AstStatement, new: AstStatement) -> ClausewitzPatch {
		ClausewitzPatch::ReplaceBlock {
			path: Vec::new(),
			key: key.to_string(),
			old_statement: old,
			new_statement: new,
		}
	}

	#[test]
	fn semantic_equality_ignores_comments_inside_blocks() {
		let a = block(
			"reform",
			vec![
				assignment("id", scalar("evt.1")),
				assignment("title", scalar("hello")),
			],
		);
		let b = block(
			"reform",
			vec![
				comment("# leading note"),
				assignment("id", scalar("evt.1")),
				comment("# inline note"),
				assignment("title", scalar("hello")),
				comment("# trailing note"),
			],
		);
		let pa = replace_block("reform", a.clone(), a);
		let pb = replace_block("reform", b.clone(), b);
		assert!(patches_semantically_equal(&pa, &pb));
		assert_ne!(
			pa, pb,
			"derived PartialEq should still see them as different"
		);
	}

	#[test]
	fn semantic_equality_ignores_spans() {
		let stmt_a = AstStatement::Assignment {
			key: "k".into(),
			key_span: span_at(1, 1),
			value: AstValue::Scalar {
				value: ScalarValue::Identifier("v".into()),
				span: span_at(1, 5),
			},
			span: span_at(1, 1),
		};
		let stmt_b = AstStatement::Assignment {
			key: "k".into(),
			key_span: span_at(42, 7),
			value: AstValue::Scalar {
				value: ScalarValue::Identifier("v".into()),
				span: span_at(42, 11),
			},
			span: span_at(42, 7),
		};
		let pa = replace_block("k", stmt_a.clone(), stmt_a);
		let pb = replace_block("k", stmt_b.clone(), stmt_b);
		assert!(patches_semantically_equal(&pa, &pb));
	}

	#[test]
	fn semantic_equality_detects_different_scalar_values() {
		let a = block("reform", vec![assignment("id", scalar("evt.1"))]);
		let b = block("reform", vec![assignment("id", scalar("evt.2"))]);
		let pa = replace_block("reform", a.clone(), a);
		let pb = replace_block("reform", b.clone(), b);
		assert!(!patches_semantically_equal(&pa, &pb));
	}

	#[test]
	fn semantic_equality_normalizes_matching_identifier_and_string_scalars() {
		assert!(ast_values_semantically_equal(
			&scalar("foo"),
			&string_scalar("foo")
		));
		assert!(ast_values_semantically_equal(
			&string_scalar("foo"),
			&scalar("foo")
		));
	}

	#[test]
	fn semantic_equality_keeps_different_identifier_and_string_scalars_distinct() {
		assert!(!ast_values_semantically_equal(
			&scalar("foo"),
			&string_scalar("bar")
		));
	}

	#[test]
	fn semantic_equality_keeps_number_and_string_scalars_distinct() {
		assert!(!ast_values_semantically_equal(
			&number_scalar("1"),
			&string_scalar("1")
		));
	}

	#[test]
	fn semantic_equality_does_not_normalize_invalid_identifier_text() {
		assert!(!ast_values_semantically_equal(
			&scalar("foo bar"),
			&string_scalar("foo bar")
		));
	}

	#[test]
	fn semantic_equality_normalizes_identifier_and_string_inside_blocks() {
		let bare = AstValue::Block {
			items: vec![assignment("localisation_key", scalar("RolandBook1"))],
			span: dummy_span(),
		};
		let quoted = AstValue::Block {
			items: vec![assignment("localisation_key", string_scalar("RolandBook1"))],
			span: dummy_span(),
		};

		assert!(ast_values_semantically_equal(&bare, &quoted));
	}

	#[test]
	fn semantic_equality_is_order_sensitive() {
		let a = block(
			"reform",
			vec![assignment("a", scalar("1")), assignment("b", scalar("2"))],
		);
		let b = block(
			"reform",
			vec![assignment("b", scalar("2")), assignment("a", scalar("1"))],
		);
		let pa = replace_block("reform", a.clone(), a);
		let pb = replace_block("reform", b.clone(), b);
		assert!(!patches_semantically_equal(&pa, &pb));
	}

	#[test]
	fn semantic_equality_handles_extra_comment_only_in_one_side() {
		let a = block("reform", vec![assignment("id", scalar("x"))]);
		let b = block(
			"reform",
			vec![comment("# extra"), assignment("id", scalar("x"))],
		);
		assert!(ast_statements_semantically_equal(&a, &b));
	}

	#[test]
	fn semantic_equality_distinguishes_patch_variants() {
		let stmt = assignment("id", scalar("x"));
		let insert = ClausewitzPatch::InsertNode {
			path: Vec::new(),
			key: "id".into(),
			statement: stmt.clone(),
		};
		let remove = ClausewitzPatch::RemoveNode {
			path: Vec::new(),
			key: "id".into(),
			removed: stmt,
		};
		assert!(!patches_semantically_equal(&insert, &remove));
	}

	// ---- fold_renames -----------------------------------------------------

	fn body() -> AstStatement {
		assignment(
			"reform",
			AstValue::Block {
				items: vec![
					assignment("modifier", scalar("centralization")),
					assignment("cost", scalar("100")),
				],
				span: dummy_span(),
			},
		)
	}

	fn body_with(extra: &str) -> AstStatement {
		assignment(
			"reform",
			AstValue::Block {
				items: vec![
					assignment("modifier", scalar("centralization")),
					assignment("cost", scalar(extra)),
				],
				span: dummy_span(),
			},
		)
	}

	#[test]
	fn fold_renames_pairs_remove_and_insert_with_equal_bodies() {
		let stmt_old = body();
		let stmt_new = body();
		let patches = vec![
			ClausewitzPatch::RemoveNode {
				path: vec![],
				key: "feudalism_reform".into(),
				removed: stmt_old,
			},
			ClausewitzPatch::InsertNode {
				path: vec![],
				key: "EE_feudalism_reform".into(),
				statement: stmt_new,
			},
		];
		let folded = fold_renames(patches);
		assert_eq!(folded.len(), 1);
		match &folded[0] {
			ClausewitzPatch::Rename {
				path,
				old_key,
				new_key,
			} => {
				assert!(path.is_empty());
				assert_eq!(old_key, "feudalism_reform");
				assert_eq!(new_key, "EE_feudalism_reform");
			}
			other => panic!("expected Rename, got {other:?}"),
		}
	}

	#[test]
	fn fold_renames_does_not_pair_when_keys_match() {
		let stmt_old = body();
		let stmt_new = body();
		let patches = vec![
			ClausewitzPatch::RemoveNode {
				path: vec![],
				key: "x".into(),
				removed: stmt_old,
			},
			ClausewitzPatch::InsertNode {
				path: vec![],
				key: "x".into(),
				statement: stmt_new,
			},
		];
		let folded = fold_renames(patches.clone());
		assert_eq!(folded.len(), 2);
		assert!(
			folded
				.iter()
				.all(|p| !matches!(p, ClausewitzPatch::Rename { .. }))
		);
	}

	#[test]
	fn fold_renames_does_not_pair_when_bodies_differ() {
		let patches = vec![
			ClausewitzPatch::RemoveNode {
				path: vec![],
				key: "feudalism_reform".into(),
				removed: body_with("100"),
			},
			ClausewitzPatch::InsertNode {
				path: vec![],
				key: "EE_feudalism_reform".into(),
				statement: body_with("200"),
			},
		];
		let folded = fold_renames(patches);
		assert_eq!(folded.len(), 2);
		assert!(
			folded
				.iter()
				.all(|p| !matches!(p, ClausewitzPatch::Rename { .. }))
		);
	}

	#[test]
	fn fold_renames_round_trip_via_diff_and_apply() {
		// base has X with a body; overlay renames X→Y with same body.
		let base = make_parsed(vec![body()]);
		let overlay = {
			let renamed = match body() {
				AstStatement::Assignment {
					value,
					key_span,
					span,
					..
				} => AstStatement::Assignment {
					key: "EE_reform".into(),
					key_span,
					value,
					span,
				},
				_ => unreachable!(),
			};
			make_parsed(vec![renamed])
		};
		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		let folded = fold_renames(patches);
		assert!(
			folded
				.iter()
				.any(|p| matches!(p, ClausewitzPatch::Rename { old_key, new_key, .. } if old_key == "reform" && new_key == "EE_reform")),
			"expected Rename, got {folded:?}"
		);
	}
}
