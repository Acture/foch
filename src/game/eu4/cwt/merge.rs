//! High-level EU4 merge guidance derived from the active CWT schema.
//!
//! Callers provide only a file path and an AST address. The compiled CWT rule
//! graph stays private to `foch` and is never exposed as part of the merge API.

use std::path::Path;

use crate::game::eu4::content::BlockMergePolicy;
use crate::game::schema::query::{
	CompiledRoot, CompiledRuleField, CompiledRuleValue, CwtQuery, RuleContext, SchemaBinding,
};
use crate::model::ConflictKind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaMergeSuggestion {
	pub suggested_identity_source: Option<SchemaMergeIdentity>,
	pub suggested_block_policy: Option<BlockMergePolicy>,
	pub schema_provenance: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaMergeIdentity {
	AssignmentKey,
	FieldValue(String),
}

/// Suggests schema-backed merge identity and block policy for an EU4 AST path.
pub fn suggest_for_conflict(file_path: &Path, ast_path: &[&str]) -> Option<SchemaMergeSuggestion> {
	let schema = super::rule_engine()?;
	suggest_for_conflict_with_query(schema, file_path, ast_path)
}

/// Classifies an unresolved EU4 merge conflict using schema evidence.
pub fn classify_conflict_kind(
	file_path: &Path,
	ast_path: &[&str],
	reason: &str,
) -> Option<ConflictKind> {
	let schema = super::rule_engine()?;
	classify_conflict_kind_with_query(schema, file_path, ast_path, reason)
}

fn suggest_for_conflict_with_query(
	schema: &CwtQuery,
	file_path: &Path,
	ast_path: &[&str],
) -> Option<SchemaMergeSuggestion> {
	let SchemaBinding::Bound { type_id, .. } = schema.bind_chain(file_path, ast_path) else {
		return None;
	};
	let definition = schema.root(type_id.as_str())?;
	Some(SchemaMergeSuggestion {
		suggested_identity_source: Some(match &definition.name_field {
			Some(field) => SchemaMergeIdentity::FieldValue(field.clone()),
			None => SchemaMergeIdentity::AssignmentKey,
		}),
		suggested_block_policy: rule_field_for_path(schema, file_path, ast_path)
			.and_then(|field| block_policy_for_value(&field.value)),
		schema_provenance: format!("{}:<{}>", path_namespace(file_path), type_id.as_str()),
	})
}

fn classify_conflict_kind_with_query(
	schema: &CwtQuery,
	file_path: &Path,
	ast_path: &[&str],
	reason: &str,
) -> Option<ConflictKind> {
	if reason.contains("deep merge of replaced block has ")
		&& !matches!(
			schema.root_binding(file_path),
			SchemaBinding::Unbound { .. }
		) {
		return Some(ConflictKind::DeepMergeable);
	}

	let rule_fields = conflict_rule_fields_for_path(schema, file_path, ast_path);
	let has_single_cardinality_match = rule_fields.iter().any(|field| {
		field
			.attributes
			.cardinality
			.and_then(|(_, max)| max)
			.is_some_and(|max| max <= 1)
	});
	let has_explicit_multi_cardinality = rule_fields.iter().any(|field| {
		field
			.attributes
			.cardinality
			.and_then(|(_, max)| max)
			.is_some_and(|max| max > 1)
	});
	if has_single_cardinality_match && !has_explicit_multi_cardinality {
		return Some(ConflictKind::SchemaCardinalityViolation);
	}

	if reason.starts_with("policy:")
		&& ast_path.len() > 1
		&& !matches!(
			schema.root_binding(file_path),
			SchemaBinding::Unbound { .. }
		) {
		return Some(ConflictKind::DeepMergeable);
	}

	if (reason.contains("sibling mods inserted divergent statements at the same key")
		|| reason.contains("multiple mods replace the same block with different content"))
		&& (root_name_field(schema, file_path).is_some()
			|| (!rule_fields.is_empty()
				&& rule_fields
					.iter()
					.all(|field| matches!(field.value, CompiledRuleValue::Block(_)))))
	{
		return Some(ConflictKind::DeepMergeable);
	}

	None
}

fn conflict_rule_fields_for_path<'schema>(
	schema: &'schema CwtQuery,
	file_path: &Path,
	ast_path: &[&str],
) -> Vec<&'schema CompiledRuleField> {
	let Some(root) = schema.bind_root(file_path) else {
		return Vec::new();
	};
	let mut matches = fields_for_segments(root, ast_path);
	if let [_root_instance, rest @ ..] = ast_path
		&& !rest.is_empty()
	{
		matches.extend(fields_for_segments(root, rest));
	}
	matches
}

fn fields_for_segments<'schema>(
	root: &'schema CompiledRoot,
	ast_path: &[&str],
) -> Vec<&'schema CompiledRuleField> {
	if ast_path.is_empty() {
		return Vec::new();
	}
	let mut current_rule_sets = vec![root.rules.as_slice()];
	current_rule_sets.extend(root.subtypes.iter().map(|subtype| subtype.rules.as_slice()));
	let mut last_matches = Vec::new();
	for (index, segment) in ast_path.iter().enumerate() {
		let mut matches = Vec::new();
		for rules in &current_rule_sets {
			matches.extend(rules.iter().filter(|field| field.key == *segment));
		}
		if matches.is_empty() {
			return Vec::new();
		}
		last_matches = matches;
		if index + 1 == ast_path.len() {
			return last_matches;
		}
		current_rule_sets = last_matches
			.iter()
			.filter_map(|field| match &field.value {
				CompiledRuleValue::Block(fields) => Some(fields.as_slice()),
				_ => None,
			})
			.collect();
		if current_rule_sets.is_empty() {
			return Vec::new();
		}
	}
	last_matches
}

fn rule_field_for_path<'schema>(
	schema: &'schema CwtQuery,
	file_path: &Path,
	ast_path: &[&str],
) -> Option<&'schema CompiledRuleField> {
	let mut context = RuleContext::RootType(schema.bind_root(file_path)?);
	let mut last_field = None;
	for (index, segment) in ast_path.iter().enumerate() {
		let field = schema.bind_field(context, segment)?;
		last_field = Some(field);
		if index + 1 == ast_path.len() {
			break;
		}
		let CompiledRuleValue::Block(_) = &field.value else {
			return None;
		};
		context = RuleContext::RuleField(field);
	}
	last_field
}

fn root_name_field<'schema>(schema: &'schema CwtQuery, file_path: &Path) -> Option<&'schema str> {
	schema.bind_root(file_path)?.name_field.as_deref()
}

fn block_policy_for_value(value: &CompiledRuleValue) -> Option<BlockMergePolicy> {
	Some(match value {
		CompiledRuleValue::Block(_) => BlockMergePolicy::Recursive,
		CompiledRuleValue::Scalar(_) | CompiledRuleValue::Marker(_) => BlockMergePolicy::Replace,
	})
}

fn path_namespace(file_path: &Path) -> String {
	let normalized = file_path.to_string_lossy().replace('\\', "/");
	let components = normalized
		.split('/')
		.filter(|segment| !segment.is_empty())
		.collect::<Vec<_>>();
	match components.as_slice() {
		[] => "unknown".to_string(),
		[only] => (*only).to_string(),
		[first, second, ..] if *first == "common" => (*second).to_string(),
		[first, ..] => (*first).to_string(),
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tempfile::TempDir;

	use super::*;
	use crate::game::schema::{CwtSchema, CwtSource};

	const EVENT_SCHEMA: &str = r#"
		types = {
			type[event] = {
				path = "game/events"
				name_field = "id"
				## type_key_filter = country_event
				subtype[country] = {}
			}
		}

		event = {
			trigger = { alias_name[trigger] = alias_match_left[trigger] }
			immediate = { alias_name[effect] = alias_match_left[effect] }
			option = { name = scalar }
		}
	"#;

	const BINDING_SCHEMA: &str = r#"
		types = {
			type[mission] = { path = "game/missions" }
		}

		mission = {
			## cardinality = 0..1
			provinces_to_highlight = {
				alias_name[trigger] = alias_match_left[trigger]
			}
		}
	"#;

	#[test]
	fn suggests_field_value_identity_from_name_field() {
		let schema = test_schema(EVENT_SCHEMA);
		let suggestion =
			suggest_for_conflict_with_query(schema.facts(), Path::new("events/example.txt"), &[])
				.expect("suggestion");
		assert_eq!(
			suggestion.suggested_identity_source,
			Some(SchemaMergeIdentity::FieldValue("id".to_string()))
		);
		assert_eq!(suggestion.suggested_block_policy, None);
		assert_eq!(suggestion.schema_provenance, "events:<event>");
	}

	#[test]
	fn suggests_assignment_key_when_schema_has_no_name_field() {
		let schema = test_schema(BINDING_SCHEMA);
		let suggestion = suggest_for_conflict_with_query(
			schema.facts(),
			Path::new("missions/example.txt"),
			&["my_mission"],
		)
		.expect("suggestion");
		assert_eq!(
			suggestion.suggested_identity_source,
			Some(SchemaMergeIdentity::AssignmentKey)
		);
		assert_eq!(suggestion.schema_provenance, "missions:<mission>");
	}

	#[test]
	fn classifies_root_name_field_conflicts_as_deep_mergeable() {
		let schema = test_schema(EVENT_SCHEMA);
		assert_eq!(
			classify_conflict_kind_with_query(
				schema.facts(),
				Path::new("events/example.txt"),
				&["country_event"],
				"sibling mods inserted divergent statements at the same key"
			),
			Some(ConflictKind::DeepMergeable)
		);
	}

	#[test]
	fn classifies_tree_policy_conflicts_below_bound_roots_as_deep_mergeable() {
		let schema = test_schema(EVENT_SCHEMA);
		assert_eq!(
			classify_conflict_kind_with_query(
				schema.facts(),
				Path::new("events/example.txt"),
				&["country_event", "immediate"],
				"policy: divergent block revisions remain unresolved"
			),
			Some(ConflictKind::DeepMergeable)
		);
	}

	#[test]
	fn classifies_single_cardinality_fields_as_schema_cardinality_violations() {
		let schema = test_schema(BINDING_SCHEMA);
		assert_eq!(
			classify_conflict_kind_with_query(
				schema.facts(),
				Path::new("missions/example.txt"),
				&["provinces_to_highlight"],
				"sibling mods inserted divergent statements at the same key"
			),
			Some(ConflictKind::SchemaCardinalityViolation)
		);
	}

	#[test]
	#[ignore = "requires vendor/cwtools-eu4-config, output/cwtools-eu4-config, or FOCH_CWTOOLS_SCHEMA_DIR"]
	fn classifies_vendor_country_history_cardinality_conflict() {
		assert_eq!(
			classify_conflict_kind(
				Path::new("history/countries/TES - Test.txt"),
				&["government_rank"],
				"sibling mods inserted divergent statements at the same key"
			),
			Some(ConflictKind::SchemaCardinalityViolation)
		);
	}

	#[test]
	#[ignore = "requires vendor/cwtools-eu4-config, output/cwtools-eu4-config, or FOCH_CWTOOLS_SCHEMA_DIR"]
	fn classifies_vendor_recursive_block_conflict_as_deep_mergeable() {
		assert_eq!(
			classify_conflict_kind(
				Path::new("common/government_reforms/test.txt"),
				&["test_reform"],
				"deep merge of replaced block has 1 unresolved sub-conflict(s)"
			),
			Some(ConflictKind::DeepMergeable)
		);
	}

	#[test]
	fn returns_none_for_unbound_schema_path() {
		let schema = test_schema(EVENT_SCHEMA);
		assert!(
			suggest_for_conflict_with_query(
				schema.facts(),
				Path::new("events/example.txt"),
				&["missing"]
			)
			.is_none()
		);
	}

	fn test_schema(source: &str) -> CwtSchema {
		let root = TempDir::new().expect("create merge schema directory");
		fs::write(root.path().join("merge.cwt"), source).expect("write merge schema");
		CwtSchema::load_with_cache(
			root.path(),
			CwtSource::UserProvided {
				path: root.path().to_path_buf(),
			},
			None,
		)
		.expect("load merge schema")
	}
}
