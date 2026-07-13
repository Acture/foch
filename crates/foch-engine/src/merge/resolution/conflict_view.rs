use std::collections::HashMap;
use std::path::{Path, PathBuf};

use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};

use crate::emit::{EmitOptions, emit_clausewitz_statements_with_options};

use super::super::error::MergeError;
use super::super::patch::ClausewitzPatch;
use super::super::patch_merge::{PatchAddress, PatchConflict};

const MAX_SUMMARY_CHARS: usize = 80;
const MAX_CHILD_PREVIEW_ENTRIES: usize = 3;

#[derive(Debug, Clone)]
pub struct CandidateView {
	pub mod_id: String,
	pub mod_display_name: String,
	pub precedence: usize,
	pub patch_summary: Vec<String>,
	pub patch_rendered: String,
}

#[derive(Debug, Clone)]
pub struct ConflictView {
	pub file_path: PathBuf,
	pub address_path: Vec<String>,
	pub address_key: String,
	pub conflict_id: String,
	pub reason: String,
	pub vanilla_snippet: Option<String>,
	pub candidates: Vec<CandidateView>,
}

pub(crate) fn build_conflict_view(
	file_path: &Path,
	address: &PatchAddress,
	conflict: &PatchConflict,
	conflict_id: String,
	mod_display_names: &HashMap<String, String>,
	vanilla_snippet: Option<String>,
	emit_options: &EmitOptions,
) -> Result<ConflictView, MergeError> {
	let candidates = conflict
		.patches
		.iter()
		.map(|patch| {
			Ok(CandidateView {
				mod_id: patch.mod_id.clone(),
				mod_display_name: mod_display_names
					.get(&patch.mod_id)
					.filter(|name| !name.trim().is_empty())
					.cloned()
					.unwrap_or_else(|| patch.mod_id.clone()),
				precedence: patch.precedence,
				patch_summary: concise_patch_summary(&patch.patch),
				patch_rendered: render_patch(&patch.patch, emit_options)?,
			})
		})
		.collect::<Result<Vec<_>, MergeError>>()?;

	Ok(ConflictView {
		file_path: file_path.to_path_buf(),
		address_path: address.path.clone(),
		address_key: address.key.clone(),
		conflict_id,
		reason: conflict.reason.clone(),
		vanilla_snippet,
		candidates,
	})
}

pub(crate) fn build_decision_conflict_view(
	file_path: &Path,
	address: &PatchAddress,
	conflict: &PatchConflict,
	conflict_id: String,
	mod_display_names: &HashMap<String, String>,
) -> ConflictView {
	let candidates = conflict
		.patches
		.iter()
		.map(|patch| CandidateView {
			mod_id: patch.mod_id.clone(),
			mod_display_name: mod_display_names
				.get(&patch.mod_id)
				.filter(|name| !name.trim().is_empty())
				.cloned()
				.unwrap_or_else(|| patch.mod_id.clone()),
			precedence: patch.precedence,
			patch_summary: Vec::new(),
			patch_rendered: String::new(),
		})
		.collect();

	ConflictView {
		file_path: file_path.to_path_buf(),
		address_path: address.path.clone(),
		address_key: address.key.clone(),
		conflict_id,
		reason: conflict.reason.clone(),
		vanilla_snippet: None,
		candidates,
	}
}

fn render_patch(patch: &ClausewitzPatch, emit_options: &EmitOptions) -> Result<String, MergeError> {
	match patch {
		ClausewitzPatch::SetValue { key, new_value, .. } => {
			emit_statement(&assignment(key, new_value.clone()), emit_options)
		}
		ClausewitzPatch::ReplaceBlock { new_statement, .. } => {
			emit_statement(new_statement, emit_options)
		}
		ClausewitzPatch::InsertNode { statement, .. } => emit_statement(statement, emit_options),
		ClausewitzPatch::RemoveNode { .. } => Ok("(removed)".to_string()),
		ClausewitzPatch::AppendListItem { value, .. }
		| ClausewitzPatch::AppendBlockItem { value, .. } => {
			render_prefixed_value("(appends)", value, emit_options)
		}
		ClausewitzPatch::RemoveListItem { value, .. }
		| ClausewitzPatch::RemoveBlockItem { value, .. } => {
			render_prefixed_value("(removes)", value, emit_options)
		}
		ClausewitzPatch::Rename {
			old_key, new_key, ..
		} => Ok(format!("(renames \"{old_key}\" -> \"{new_key}\")")),
	}
}

fn emit_statement(
	statement: &AstStatement,
	emit_options: &EmitOptions,
) -> Result<String, MergeError> {
	emit_clausewitz_statements_with_options(std::slice::from_ref(statement), emit_options)
}

fn render_prefixed_value(
	prefix: &str,
	value: &AstValue,
	emit_options: &EmitOptions,
) -> Result<String, MergeError> {
	let rendered = emit_clausewitz_statements_with_options(
		&[AstStatement::Item {
			value: value.clone(),
			span: synthetic_span(),
		}],
		emit_options,
	)?;
	Ok(format!("{prefix} {}", rendered.trim_end()))
}

fn assignment(key: &str, value: AstValue) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: synthetic_span(),
		value,
		span: synthetic_span(),
	}
}

fn synthetic_span() -> SpanRange {
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

fn concise_patch_summary(patch: &ClausewitzPatch) -> Vec<String> {
	match patch {
		ClausewitzPatch::SetValue {
			key,
			old_value,
			new_value,
			..
		} => vec![format!(
			"set \"{key}\": {} → {}",
			value_summary(old_value),
			value_summary(new_value)
		)],
		ClausewitzPatch::RemoveNode { key, removed, .. } => remove_node_summary(key, removed),
		ClausewitzPatch::InsertNode { key, statement, .. } => insert_node_summary(key, statement),
		ClausewitzPatch::ReplaceBlock {
			key, new_statement, ..
		} => block_patch_summary(format!("replace block \"{key}\""), new_statement, '+'),
		ClausewitzPatch::AppendListItem { key, value, .. } => {
			vec![format!(
				"append to list \"{key}\": {}",
				value_summary(value)
			)]
		}
		ClausewitzPatch::RemoveListItem { key, value, .. } => {
			vec![format!(
				"remove from list \"{key}\": {}",
				value_summary(value)
			)]
		}
		ClausewitzPatch::AppendBlockItem { value, .. } => {
			vec![format!("append item: {}", value_summary(value))]
		}
		ClausewitzPatch::RemoveBlockItem { value, .. } => {
			vec![format!("remove item: {}", value_summary(value))]
		}
		ClausewitzPatch::Rename {
			old_key, new_key, ..
		} => vec![format!("rename \"{old_key}\" → \"{new_key}\"")],
	}
}

fn remove_node_summary(key: &str, removed: &AstStatement) -> Vec<String> {
	let mut lines = vec![format!(
		"remove \"{key}\" (was: {})",
		statement_value_summary(removed)
	)];
	if let AstStatement::Assignment {
		value: AstValue::Block { items, .. },
		..
	} = removed
	{
		lines.extend(child_preview_lines(items, '-'));
	}
	lines
}

fn insert_node_summary(key: &str, statement: &AstStatement) -> Vec<String> {
	if statement_block_items(statement).is_some() {
		return block_patch_summary(format!("insert \"{key}\""), statement, '+');
	}
	vec![format!(
		"insert \"{key}\" = {}",
		statement_value_summary(statement)
	)]
}

fn block_patch_summary(prefix: String, statement: &AstStatement, marker: char) -> Vec<String> {
	let mut lines = vec![format!(
		"{prefix} ({} entries)",
		statement_entry_count(statement)
	)];
	if let Some(items) = statement_block_items(statement) {
		lines.extend(child_preview_lines(items, marker));
	}
	lines
}

fn statement_block_items(statement: &AstStatement) -> Option<&[AstStatement]> {
	match statement {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		}
		| AstStatement::Item {
			value: AstValue::Block { items, .. },
			..
		} => Some(items),
		AstStatement::Assignment { .. }
		| AstStatement::Item { .. }
		| AstStatement::Comment { .. } => None,
	}
}

fn child_preview_lines(items: &[AstStatement], marker: char) -> Vec<String> {
	let entries: Vec<String> = items
		.iter()
		.filter_map(|statement| child_preview_line(statement, marker))
		.collect();
	let mut lines: Vec<String> = entries
		.iter()
		.take(MAX_CHILD_PREVIEW_ENTRIES)
		.cloned()
		.collect();
	let remaining = entries.len().saturating_sub(MAX_CHILD_PREVIEW_ENTRIES);
	if remaining > 0 {
		lines.push(format!("  {marker} … ({remaining} more)"));
	}
	lines
}

fn child_preview_line(statement: &AstStatement, marker: char) -> Option<String> {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			Some(format!("  {marker} {key} = {}", value_summary(value)))
		}
		AstStatement::Item { value, .. } => Some(format!("  {marker} {}", value_summary(value))),
		AstStatement::Comment { .. } => None,
	}
}

fn statement_value_summary(statement: &AstStatement) -> String {
	match statement {
		AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
			value_summary(value)
		}
		AstStatement::Comment { text, .. } => format!("# {}", sanitize_summary(text)),
	}
}

fn statement_entry_count(statement: &AstStatement) -> usize {
	match statement {
		AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
			value_entry_count(value)
		}
		AstStatement::Comment { .. } => 0,
	}
}

fn value_entry_count(value: &AstValue) -> usize {
	match value {
		AstValue::Block { items, .. } => items.len(),
		AstValue::Scalar { .. } => 1,
	}
}

fn value_summary(value: &AstValue) -> String {
	match value {
		AstValue::Scalar { value, .. } => {
			truncate_summary(&sanitize_summary(&scalar_summary(value)))
		}
		AstValue::Block { items, .. } => format!("{{ {} entries }}", items.len()),
	}
}

fn scalar_summary(value: &ScalarValue) -> String {
	match value {
		ScalarValue::Identifier(value) | ScalarValue::Number(value) => value.clone(),
		ScalarValue::String(value) => format!("\"{value}\""),
		ScalarValue::Bool(value) => {
			if *value {
				"yes".to_string()
			} else {
				"no".to_string()
			}
		}
	}
}

fn sanitize_summary(value: &str) -> String {
	let mut out = String::new();
	for c in value.chars() {
		match c {
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
			c => out.push(c),
		}
	}
	out
}

fn truncate_summary(value: &str) -> String {
	let mut chars = value.chars();
	let truncated: String = chars.by_ref().take(MAX_SUMMARY_CHARS).collect();
	if chars.next().is_some() {
		format!("{truncated}…")
	} else {
		truncated
	}
}

#[cfg(test)]
mod tests {
	use super::super::conflict_handler::{ConflictDecision, ConflictHandler};
	use super::*;

	struct HighestPrecedenceHandler;

	impl ConflictHandler for HighestPrecedenceHandler {
		fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
			view.candidates
				.iter()
				.max_by_key(|candidate| candidate.precedence)
				.map(|candidate| ConflictDecision::PickMod {
					mod_id: candidate.mod_id.clone(),
					record: None,
				})
				.unwrap_or(ConflictDecision::Defer { record: None })
		}
	}

	#[test]
	fn handler_can_decide_from_conflict_view_alone() {
		let view = ConflictView {
			file_path: PathBuf::from("common/example.txt"),
			address_path: vec!["root".to_string()],
			address_key: "owner".to_string(),
			conflict_id: "abc123".to_string(),
			reason: "conflicting scalar values".to_string(),
			vanilla_snippet: Some("owner = FRA".to_string()),
			candidates: vec![
				CandidateView {
					mod_id: "mod-low".to_string(),
					mod_display_name: "Low".to_string(),
					precedence: 1,
					patch_summary: vec!["set owner".to_string()],
					patch_rendered: "owner = LOW".to_string(),
				},
				CandidateView {
					mod_id: "mod-high".to_string(),
					mod_display_name: "High".to_string(),
					precedence: 9,
					patch_summary: vec!["set owner".to_string()],
					patch_rendered: "owner = HIGH".to_string(),
				},
			],
		};

		let decision = HighestPrecedenceHandler.on_conflict(&view);

		assert_eq!(
			decision,
			ConflictDecision::PickMod {
				mod_id: "mod-high".to_string(),
				record: None
			}
		);
	}

	#[test]
	fn string_summary_preserves_clausewitz_escape_text() {
		let value = AstValue::Scalar {
			value: ScalarValue::String(r#"gfx\\interface\\icon.dds: \"label\""#.to_string()),
			span: synthetic_span(),
		};

		assert_eq!(
			value_summary(&value),
			r#""gfx\\interface\\icon.dds: \"label\"""#
		);
	}
}
