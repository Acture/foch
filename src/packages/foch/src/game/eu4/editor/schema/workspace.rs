use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::interpret::schema_definition_names;
use crate::game::eu4::script::parser::{
	AstStatement, AstValue, ScriptSyntax, SpanRange, parse_clausewitz_statements,
};
use crate::game::schema::CwtQuery;
use crate::game::schema::bind::DefinedNames;
use crate::game::schema::query::{CompiledComplexEnum, CompiledRuleField, CompiledRuleValue};
use crate::model::GamePath;

/// A workspace script, named by its game path.
#[derive(Clone, Copy, Debug)]
pub struct SchemaDocument<'a> {
	path: &'a GamePath,
	text: &'a str,
}

impl<'a> SchemaDocument<'a> {
	pub fn new(path: &'a GamePath, text: &'a str) -> Self {
		Self { path, text }
	}
}

#[derive(Clone, Debug, Default)]
pub struct SchemaWorkspace {
	complex_enums: HashMap<String, Vec<String>>,
	/// The names each schema type's workspace definitions take.
	definitions: HashMap<String, BTreeSet<String>>,
}

impl DefinedNames for SchemaWorkspace {
	fn complex_enum_values(&self, name: &str) -> Option<&[String]> {
		self.complex_enums.get(name).map(Vec::as_slice)
	}

	fn defines(&self, type_name: &str, name: &str) -> bool {
		self.definitions
			.get(type_name)
			.is_some_and(|names| names.contains(name))
	}
}

struct ParsedSchemaDocument<'a> {
	path: &'a GamePath,
	statements: Vec<AstStatement>,
}

pub(super) fn build(engine: &CwtQuery, documents: &[SchemaDocument<'_>]) -> SchemaWorkspace {
	let files = documents
		.iter()
		.map(|document| {
			let parsed = parse_clausewitz_statements(
				ScriptSyntax::for_game_path(document.path),
				document.text,
			);
			ParsedSchemaDocument {
				path: document.path,
				statements: parsed.statements,
			}
		})
		.collect::<Vec<_>>();
	build_dynamic_schema_values(engine, &files)
}

fn build_dynamic_schema_values(
	engine: &CwtQuery,
	files: &[ParsedSchemaDocument<'_>],
) -> SchemaWorkspace {
	let mut complex_enums = HashMap::new();
	for complex_enum in engine.complex_enums() {
		let mut values = BTreeSet::new();
		for file in files {
			if complex_enum.matches(file.path) {
				values.extend(complex_enum_file_values(complex_enum, &file.statements).into_keys());
			}
		}
		if !values.is_empty() {
			complex_enums.insert(complex_enum.name.clone(), values.into_iter().collect());
		}
	}
	let mut definitions = HashMap::<String, BTreeSet<String>>::new();
	for file in files {
		for root in engine.matching_roots(file.path) {
			definitions
				.entry(root.name.clone())
				.or_default()
				.extend(schema_definition_names(root, &file.statements, file.path));
		}
	}
	SchemaWorkspace {
		complex_enums,
		definitions,
	}
}

/// The values of a complex enum a file declares, each with where it is
/// first written.
pub(crate) fn complex_enum_file_values<'a>(
	complex_enum: &CompiledComplexEnum,
	statements: &'a [AstStatement],
) -> BTreeMap<String, &'a SpanRange> {
	let mut out = BTreeMap::new();
	if complex_enum.start_from_root {
		collect_complex_enum_rule_values(&complex_enum.name_rules, statements, &mut out);
	} else {
		collect_complex_enum_rule_values_recursive(&complex_enum.name_rules, statements, &mut out);
	}
	out
}

fn collect_complex_enum_rule_values_recursive<'a>(
	rules: &[CompiledRuleField],
	statements: &'a [AstStatement],
	out: &mut BTreeMap<String, &'a SpanRange>,
) {
	collect_complex_enum_rule_values(rules, statements, out);
	for statement in statements {
		if let Some(items) = statement_block_items(statement) {
			collect_complex_enum_rule_values_recursive(rules, items, out);
		}
	}
}

fn collect_complex_enum_rule_values<'a>(
	rules: &[CompiledRuleField],
	statements: &'a [AstStatement],
	out: &mut BTreeMap<String, &'a SpanRange>,
) {
	for rule in rules {
		collect_complex_enum_rule_values_for_rule(rule, statements, out);
	}
}

fn collect_complex_enum_rule_values_for_rule<'a>(
	rule: &CompiledRuleField,
	statements: &'a [AstStatement],
	out: &mut BTreeMap<String, &'a SpanRange>,
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
			(_, AstValue::Scalar { value, span })
				if complex_enum_rule_accepts_scalar(&rule.value) =>
			{
				insert_nonempty_dynamic_value(out, value.as_text(), span);
			}
			_ => {}
		}
	}
}

fn collect_complex_enum_name_values<'a>(
	rule_value: &CompiledRuleValue,
	statements: &'a [AstStatement],
	out: &mut BTreeMap<String, &'a SpanRange>,
) {
	for statement in statements {
		match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value,
				..
			} if complex_enum_rule_accepts_value(rule_value, value) => {
				insert_nonempty_dynamic_value(out, key.clone(), key_span);
			}
			AstStatement::Item {
				value: AstValue::Scalar {
					value: scalar,
					span,
				},
				..
			} if complex_enum_rule_accepts_scalar(rule_value) => {
				insert_nonempty_dynamic_value(out, scalar.as_text(), span);
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

fn insert_nonempty_dynamic_value<'a>(
	out: &mut BTreeMap<String, &'a SpanRange>,
	value: String,
	span: &'a SpanRange,
) {
	if !value.is_empty() {
		out.entry(value).or_insert(span);
	}
}
