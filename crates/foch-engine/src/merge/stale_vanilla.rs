use foch_core::model::StaleVanillaTargetDescriptor;
use foch_language::analyzer::content_family::MergeKeySource;
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::ParsedScriptFile;

use super::patch::ClausewitzPatch;

const MISSING_PATH_NOTE: &str = "vanilla snapshot for this file does not contain the target path; this remove-style patch may be cross-version drift, dependency-targeted, or intentionally guarded";
const MISSING_KEY_NOTE: &str = "vanilla snapshot contains the parent path but not the target key; this remove-style patch may be cross-version drift, dependency-targeted, or intentionally guarded";

/// Detect remove-style patches whose target is absent from the original vanilla file.
///
/// This intentionally compares against the vanilla snapshot only: additive-only files
/// are skipped, and conditional/guarded removes still warn with low confidence because
/// the patch layer does not preserve guard intent.
pub(crate) fn detect_stale_vanilla_targets(
	patches: &[ClausewitzPatch],
	file_path: &str,
	mod_id: &str,
	mod_version: &str,
	vanilla: Option<&ParsedScriptFile>,
	merge_key_source: MergeKeySource,
) -> Vec<StaleVanillaTargetDescriptor> {
	let Some(vanilla) = vanilla else {
		return Vec::new();
	};

	patches
		.iter()
		.filter_map(|patch| {
			let target = stale_target(patch)?;
			let parent = statements_at_path(&vanilla.ast.statements, target.path, merge_key_source);
			let note = match (parent, target.key) {
				(None, _) => MISSING_PATH_NOTE,
				(Some(_), None) => return None,
				(Some(statements), Some(key))
					if contains_key(statements, key, target.path.len(), merge_key_source) =>
				{
					return None;
				}
				(Some(_), Some(_)) => MISSING_KEY_NOTE,
			};
			Some(StaleVanillaTargetDescriptor {
				mod_id: mod_id.to_string(),
				mod_version: mod_version.to_string(),
				file_path: file_path.to_string(),
				patch_kind: target.kind.to_string(),
				target_path: target.path.to_vec(),
				target_key: target.key.map(str::to_string),
				note: Some(note.to_string()),
			})
		})
		.collect()
}

struct StaleTarget<'a> {
	kind: &'static str,
	path: &'a [String],
	key: Option<&'a str>,
}

fn stale_target(patch: &ClausewitzPatch) -> Option<StaleTarget<'_>> {
	match patch {
		ClausewitzPatch::RemoveNode { path, key, .. } => Some(StaleTarget {
			kind: "RemoveNode",
			path,
			key: Some(key),
		}),
		ClausewitzPatch::RemoveListItem { path, key, .. } => Some(StaleTarget {
			kind: "RemoveListItem",
			path,
			key: Some(key),
		}),
		ClausewitzPatch::RemoveBlockItem { path, .. } => Some(StaleTarget {
			kind: "RemoveBlockItem",
			path,
			key: None,
		}),
		ClausewitzPatch::Rename { path, old_key, .. } => Some(StaleTarget {
			kind: "Rename",
			path,
			key: Some(old_key),
		}),
		_ => None,
	}
}

fn statements_at_path<'a>(
	root: &'a [AstStatement],
	path: &[String],
	merge_key_source: MergeKeySource,
) -> Option<&'a [AstStatement]> {
	let mut current = root;
	for (depth, segment) in path.iter().enumerate() {
		let statement = current
			.iter()
			.find(|stmt| statement_matches_key(stmt, segment, depth, merge_key_source))?;
		current = block_items(statement)?;
	}
	Some(current)
}

fn contains_key(
	statements: &[AstStatement],
	key: &str,
	parent_depth: usize,
	merge_key_source: MergeKeySource,
) -> bool {
	statements
		.iter()
		.any(|stmt| statement_matches_key(stmt, key, parent_depth, merge_key_source))
}

fn statement_matches_key(
	stmt: &AstStatement,
	key: &str,
	depth: usize,
	merge_key_source: MergeKeySource,
) -> bool {
	match merge_key_source {
		MergeKeySource::FieldValue(field) if depth == 0 => {
			field_value(stmt, field).is_some_and(|value| value == key)
		}
		MergeKeySource::ContainerChildFieldValue { container, .. } if depth == 0 => {
			assignment_key(stmt).is_some_and(|candidate| candidate == container && key == container)
		}
		MergeKeySource::ContainerChildFieldValue {
			child_key_field,
			child_types,
			..
		} if depth == 1 => container_child_field_value_key(stmt, child_key_field, child_types)
			.is_some_and(|candidate| candidate == key),
		_ => assignment_key(stmt).is_some_and(|candidate| candidate == key),
	}
}

fn assignment_key(stmt: &AstStatement) -> Option<&str> {
	match stmt {
		AstStatement::Assignment { key, .. } => Some(key),
		_ => None,
	}
}

fn field_value(stmt: &AstStatement, field: &str) -> Option<String> {
	let AstStatement::Assignment {
		value: AstValue::Block { items, .. },
		..
	} = stmt
	else {
		return None;
	};
	scalar_assignment_value(items, field)
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

fn scalar_assignment_value(items: &[AstStatement], expected_key: &str) -> Option<String> {
	items.iter().find_map(|item| match item {
		AstStatement::Assignment {
			key,
			value: AstValue::Scalar { value, .. },
			..
		} if key == expected_key => Some(value.as_text()),
		_ => None,
	})
}

fn block_items(stmt: &AstStatement) -> Option<&[AstStatement]> {
	match stmt {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => Some(items),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use foch_language::analyzer::content_family::ScriptFileKind;
	use foch_language::analyzer::parser::parse_clausewitz_content;

	use super::*;

	const FILE_PATH: &str = "common/test/foo.txt";

	fn parsed(source: &str) -> ParsedScriptFile {
		let path = PathBuf::from(FILE_PATH);
		let parsed = parse_clausewitz_content(path.clone(), source);
		ParsedScriptFile {
			mod_id: "__game__".to_string(),
			path: path.clone(),
			relative_path: path.clone(),
			content_family: None,
			file_kind: ScriptFileKind::Other,
			module_name: "test".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn first_statement(source: &str) -> AstStatement {
		parsed(source).ast.statements.remove(0)
	}

	#[test]
	fn remove_present_vanilla_path_emits_no_descriptor() {
		let vanilla = parsed("present = yes\n");
		let patches = vec![ClausewitzPatch::RemoveNode {
			path: Vec::new(),
			key: "present".to_string(),
			removed: first_statement("present = yes\n"),
		}];

		let findings = detect_stale_vanilla_targets(
			&patches,
			FILE_PATH,
			"mod-a",
			"1.0.0",
			Some(&vanilla),
			MergeKeySource::AssignmentKey,
		);

		assert!(findings.is_empty());
	}

	#[test]
	fn remove_absent_vanilla_path_emits_descriptor() {
		let vanilla = parsed("present = { child = yes }\n");
		let patches = vec![ClausewitzPatch::RemoveNode {
			path: vec!["absent".to_string()],
			key: "child".to_string(),
			removed: first_statement("child = yes\n"),
		}];

		let findings = detect_stale_vanilla_targets(
			&patches,
			FILE_PATH,
			"mod-a",
			"1.34.0",
			Some(&vanilla),
			MergeKeySource::AssignmentKey,
		);

		assert_eq!(findings.len(), 1);
		let finding = &findings[0];
		assert_eq!(finding.mod_id, "mod-a");
		assert_eq!(finding.mod_version, "1.34.0");
		assert_eq!(finding.file_path, FILE_PATH);
		assert_eq!(finding.patch_kind, "RemoveNode");
		assert_eq!(finding.target_path, vec!["absent"]);
		assert_eq!(finding.target_key.as_deref(), Some("child"));
		assert!(
			finding
				.note
				.as_deref()
				.is_some_and(|note| note.contains("target path"))
		);
	}

	#[test]
	fn additive_patch_to_absent_path_emits_no_descriptor() {
		let vanilla = parsed("present = yes\n");
		let patches = vec![ClausewitzPatch::InsertNode {
			path: Vec::new(),
			key: "absent".to_string(),
			statement: first_statement("absent = yes\n"),
		}];

		let findings = detect_stale_vanilla_targets(
			&patches,
			FILE_PATH,
			"mod-a",
			"1.0.0",
			Some(&vanilla),
			MergeKeySource::AssignmentKey,
		);

		assert!(findings.is_empty());
	}
}
