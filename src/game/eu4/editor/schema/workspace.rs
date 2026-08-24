use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::game::eu4::script::parser::{AstStatement, AstValue, parse_clausewitz_content};
use crate::game::schema::CwtQuery;
use crate::game::schema::query::{CompiledComplexEnum, CompiledRuleField, CompiledRuleValue};

#[derive(Clone, Copy, Debug)]
pub struct SchemaDocument<'a> {
	path: &'a Path,
	text: &'a str,
}

impl<'a> SchemaDocument<'a> {
	pub fn new(path: &'a Path, text: &'a str) -> Self {
		Self { path, text }
	}
}

#[derive(Clone, Debug, Default)]
pub struct SchemaWorkspace {
	pub(super) complex_enums: HashMap<String, Vec<String>>,
}

struct ParsedSchemaDocument {
	relative_path: PathBuf,
	statements: Vec<AstStatement>,
}

pub(super) fn build(engine: &CwtQuery, documents: &[SchemaDocument<'_>]) -> SchemaWorkspace {
	let files = documents
		.iter()
		.map(|document| {
			let parsed = parse_clausewitz_content(document.path.to_path_buf(), document.text);
			ParsedSchemaDocument {
				relative_path: document.path.to_path_buf(),
				statements: parsed.ast.statements,
			}
		})
		.collect::<Vec<_>>();
	build_dynamic_schema_values(engine, &files)
}

fn build_dynamic_schema_values(
	engine: &CwtQuery,
	files: &[ParsedSchemaDocument],
) -> SchemaWorkspace {
	let mut complex_enums = HashMap::new();
	for complex_enum in engine.complex_enums() {
		let mut values = BTreeSet::new();
		for file in files {
			if complex_enum_matches_file(complex_enum, file) {
				collect_complex_enum_file_values(complex_enum, &file.statements, &mut values);
			}
		}
		if !values.is_empty() {
			complex_enums.insert(complex_enum.name.clone(), values.into_iter().collect());
		}
	}
	SchemaWorkspace { complex_enums }
}

fn complex_enum_matches_file(
	complex_enum: &CompiledComplexEnum,
	file: &ParsedSchemaDocument,
) -> bool {
	let normalized = normalize_schema_relative_path(&file.relative_path);
	if let Some(path) = complex_enum.normalized_file_path.as_deref() {
		return normalized == path;
	}
	complex_enum
		.normalized_path
		.as_deref()
		.is_some_and(|path| normalized == path || normalized.starts_with(&format!("{path}/")))
}

fn normalize_schema_relative_path(path: &Path) -> String {
	path.to_string_lossy()
		.replace('\\', "/")
		.trim_matches('/')
		.to_ascii_lowercase()
}

fn collect_complex_enum_file_values(
	complex_enum: &CompiledComplexEnum,
	statements: &[AstStatement],
	out: &mut BTreeSet<String>,
) {
	if complex_enum.start_from_root {
		collect_complex_enum_rule_values(&complex_enum.name_rules, statements, out);
	} else {
		collect_complex_enum_rule_values_recursive(&complex_enum.name_rules, statements, out);
	}
}

fn collect_complex_enum_rule_values_recursive(
	rules: &[CompiledRuleField],
	statements: &[AstStatement],
	out: &mut BTreeSet<String>,
) {
	collect_complex_enum_rule_values(rules, statements, out);
	for statement in statements {
		if let Some(items) = statement_block_items(statement) {
			collect_complex_enum_rule_values_recursive(rules, items, out);
		}
	}
}

fn collect_complex_enum_rule_values(
	rules: &[CompiledRuleField],
	statements: &[AstStatement],
	out: &mut BTreeSet<String>,
) {
	for rule in rules {
		collect_complex_enum_rule_values_for_rule(rule, statements, out);
	}
}

fn collect_complex_enum_rule_values_for_rule(
	rule: &CompiledRuleField,
	statements: &[AstStatement],
	out: &mut BTreeSet<String>,
) {
	if rule.key == "enum_name" {
		collect_complex_enum_name_values(&rule.value, statements, out);
		return;
	}
	for statement in statements {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		if key != &rule.key {
			continue;
		}
		match (&rule.value, value) {
			(CompiledRuleValue::Block(child_rules), AstValue::Block { items, .. }) => {
				collect_complex_enum_rule_values(child_rules, items, out);
			}
			(_, AstValue::Scalar { value, .. })
				if complex_enum_rule_accepts_scalar(&rule.value) =>
			{
				insert_nonempty_dynamic_value(out, value.as_text());
			}
			_ => {}
		}
	}
}

fn collect_complex_enum_name_values(
	rule_value: &CompiledRuleValue,
	statements: &[AstStatement],
	out: &mut BTreeSet<String>,
) {
	for statement in statements {
		match statement {
			AstStatement::Assignment { key, value, .. }
				if complex_enum_rule_accepts_value(rule_value, value) =>
			{
				insert_nonempty_dynamic_value(out, key.clone());
			}
			AstStatement::Item {
				value: AstValue::Scalar { value: scalar, .. },
				..
			} if complex_enum_rule_accepts_scalar(rule_value) => {
				insert_nonempty_dynamic_value(out, scalar.as_text());
			}
			_ => {}
		}
	}
}

fn complex_enum_rule_accepts_value(rule_value: &CompiledRuleValue, value: &AstValue) -> bool {
	match value {
		AstValue::Scalar { .. } => complex_enum_rule_accepts_scalar(rule_value),
		AstValue::Block { .. } => {
			matches!(rule_value, CompiledRuleValue::Marker(marker) if marker == "enum_name")
		}
	}
}

fn complex_enum_rule_accepts_scalar(rule_value: &CompiledRuleValue) -> bool {
	match rule_value {
		CompiledRuleValue::Marker(marker) => marker == "enum_name",
		CompiledRuleValue::Scalar(value) => matches!(
			value.as_str(),
			"scalar" | "localisation" | "localization" | "localisation_synced" | "any"
		),
		CompiledRuleValue::Block(_) => false,
	}
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

fn insert_nonempty_dynamic_value(out: &mut BTreeSet<String>, value: String) {
	if !value.is_empty() {
		out.insert(value);
	}
}
