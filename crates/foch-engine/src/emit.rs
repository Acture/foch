use crate::merge::MergeError;
use foch_core::config::DEFAULT_EMIT_INDENT;
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum EmitOrdering {
	#[default]
	Preserve,
	FixedTopLevel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EmitOptions {
	indent: String,
	ordering: EmitOrdering,
}

impl EmitOptions {
	pub(crate) fn with_indent(indent: impl Into<String>) -> Self {
		Self {
			indent: indent.into(),
			ordering: EmitOrdering::default(),
		}
	}

	pub(crate) fn indent(&self) -> &str {
		&self.indent
	}

	pub(crate) fn ordering(&self) -> EmitOrdering {
		self.ordering
	}

	pub(crate) fn with_ordering(mut self, ordering: EmitOrdering) -> Self {
		self.ordering = ordering;
		self
	}
}

impl Default for EmitOptions {
	fn default() -> Self {
		Self::with_indent(DEFAULT_EMIT_INDENT)
	}
}

pub(crate) fn emit_clausewitz_statements(
	statements: &[AstStatement],
) -> Result<String, MergeError> {
	emit_clausewitz_statements_with_options(statements, &EmitOptions::default())
}

pub(crate) fn emit_clausewitz_statements_with_options(
	statements: &[AstStatement],
	options: &EmitOptions,
) -> Result<String, MergeError> {
	let mut out = String::new();
	emit_statements(statements, 0, &mut out, options)?;
	Ok(out)
}

fn emit_statements(
	statements: &[AstStatement],
	indent: usize,
	out: &mut String,
	options: &EmitOptions,
) -> Result<(), MergeError> {
	if options.ordering() == EmitOrdering::FixedTopLevel && indent == 0 {
		for group in ordered_statement_groups(statements) {
			for statement in group {
				emit_statement(statement, indent, out, options)?;
			}
		}
		return Ok(());
	}

	for statement in statements {
		emit_statement(statement, indent, out, options)?;
	}
	Ok(())
}

fn emit_statement(
	statement: &AstStatement,
	indent: usize,
	out: &mut String,
	options: &EmitOptions,
) -> Result<(), MergeError> {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			indent_into(out, indent, options.indent());
			out.push_str(key);
			out.push_str(" = ");
			emit_value(value, indent, out, options)?;
			out.push('\n');
			Ok(())
		}
		AstStatement::Item { value, .. } => {
			indent_into(out, indent, options.indent());
			emit_value(value, indent, out, options)?;
			out.push('\n');
			Ok(())
		}
		AstStatement::Comment { text, .. } => {
			indent_into(out, indent, options.indent());
			out.push_str("# ");
			out.push_str(text);
			out.push('\n');
			Ok(())
		}
	}
}

fn emit_value(
	value: &AstValue,
	indent: usize,
	out: &mut String,
	options: &EmitOptions,
) -> Result<(), MergeError> {
	match value {
		AstValue::Scalar { value, .. } => {
			out.push_str(&render_scalar(value));
			Ok(())
		}
		AstValue::Block { items, .. } => {
			out.push_str("{\n");
			emit_statements(items, indent + 1, out, options)?;
			indent_into(out, indent, options.indent());
			out.push('}');
			Ok(())
		}
	}
}

fn render_scalar(value: &ScalarValue) -> String {
	match value {
		ScalarValue::Identifier(value) => value.clone(),
		// The parser stores the quoted token body verbatim, including Clausewitz
		// backslash sequences. Re-escaping it would mutate paths on every emit.
		ScalarValue::String(value) => format!("\"{value}\""),
		ScalarValue::Number(value) => value.clone(),
		ScalarValue::Bool(value) => {
			if *value {
				"yes".to_string()
			} else {
				"no".to_string()
			}
		}
	}
}

fn indent_into(out: &mut String, indent: usize, indent_text: &str) {
	for _ in 0..indent {
		out.push_str(indent_text);
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct StatementSortKey {
	kind: u8,
	key: String,
	value: String,
}

fn ordered_statement_groups(statements: &[AstStatement]) -> Vec<Vec<&AstStatement>> {
	let mut groups = Vec::new();
	let mut leading_comments = Vec::new();

	for statement in statements {
		if matches!(statement, AstStatement::Comment { .. }) {
			leading_comments.push(statement);
			continue;
		}

		let mut group = std::mem::take(&mut leading_comments);
		group.push(statement);
		groups.push(group);
	}

	if !leading_comments.is_empty() {
		groups.push(leading_comments);
	}

	groups.sort_by_key(|group| group_sort_key(group));
	groups
}

fn group_sort_key(group: &[&AstStatement]) -> StatementSortKey {
	group
		.iter()
		.find(|statement| !matches!(statement, AstStatement::Comment { .. }))
		.map(|statement| statement_sort_key(statement))
		.unwrap_or_else(|| {
			let key = group
				.iter()
				.filter_map(|statement| match statement {
					AstStatement::Comment { text, .. } => Some(text.as_str()),
					_ => None,
				})
				.collect::<Vec<_>>()
				.join("\n");
			StatementSortKey {
				kind: 2,
				key,
				value: String::new(),
			}
		})
}

fn statement_sort_key(statement: &AstStatement) -> StatementSortKey {
	match statement {
		AstStatement::Assignment { key, value, .. } => StatementSortKey {
			kind: 0,
			key: key.clone(),
			value: value_sort_key(value),
		},
		AstStatement::Item { value, .. } => StatementSortKey {
			kind: 1,
			key: String::new(),
			value: value_sort_key(value),
		},
		AstStatement::Comment { text, .. } => StatementSortKey {
			kind: 2,
			key: text.clone(),
			value: String::new(),
		},
	}
}

fn value_sort_key(value: &AstValue) -> String {
	match value {
		AstValue::Scalar { value, .. } => scalar_sort_key(value),
		AstValue::Block { items, .. } => {
			let mut keys = items
				.iter()
				.filter(|item| !matches!(item, AstStatement::Comment { .. }))
				.map(statement_sort_key)
				.collect::<Vec<_>>();
			keys.sort();
			keys.into_iter()
				.map(|key| format!("{}:{}={}", key.kind, key.key, key.value))
				.collect::<Vec<_>>()
				.join(";")
		}
	}
}

fn scalar_sort_key(value: &ScalarValue) -> String {
	match value {
		ScalarValue::Identifier(value) => format!("i:{value}"),
		ScalarValue::String(value) => format!("s:{value}"),
		ScalarValue::Number(value) => format!("n:{value}"),
		ScalarValue::Bool(value) => format!("b:{value}"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::parser::{Span, SpanRange};

	#[test]
	fn default_emit_options_keep_tab_indentation() {
		let statements = nested_statements();

		let emitted = emit_clausewitz_statements(&statements).expect("emit statements");
		let explicit_default = emit_clausewitz_statements_with_options(
			&statements,
			&EmitOptions::with_indent(DEFAULT_EMIT_INDENT),
		)
		.expect("emit statements with default options");

		let expected = "root = {\n\tchild = {\n\t\tleaf = yes\n\t}\n}\n";
		assert_eq!(emitted, expected);
		assert_eq!(explicit_default, expected);
	}

	#[test]
	fn custom_emit_options_indent_each_level_verbatim() {
		let emitted = emit_clausewitz_statements_with_options(
			&nested_statements(),
			&EmitOptions::with_indent("  "),
		)
		.expect("emit statements with custom options");

		assert_eq!(emitted, "root = {\n  child = {\n    leaf = yes\n  }\n}\n");
	}

	#[test]
	fn string_emission_preserves_clausewitz_escape_text() {
		let statements = vec![
			assignment(
				"textureFile",
				scalar(ScalarValue::String(
					r"gfx\\interface\\small_tiles_dialog.dds".to_string(),
				)),
			),
			assignment(
				"tooltip",
				scalar(ScalarValue::String(r#"say \"hello\""#.to_string())),
			),
		];

		let emitted = emit_clausewitz_statements(&statements).expect("emit string values");

		assert_eq!(
			emitted,
			r#"textureFile = "gfx\\interface\\small_tiles_dialog.dds"
tooltip = "say \"hello\""
"#
		);
	}

	#[test]
	fn fixed_top_level_order_sorts_definitions_and_keeps_nested_order() {
		let statements = vec![
			assignment(
				"z_root",
				block(vec![
					assignment("b", scalar_id("2")),
					assignment("a", scalar_id("1")),
				]),
			),
			comment("foch: a_root from mod-a"),
			assignment(
				"a_root",
				block(vec![
					assignment("z", scalar_id("2")),
					assignment("a", scalar_id("1")),
				]),
			),
		];

		let emitted = emit_clausewitz_statements_with_options(
			&statements,
			&EmitOptions::default().with_ordering(EmitOrdering::FixedTopLevel),
		)
		.expect("emit fixed order");

		assert_eq!(
			emitted,
			"# foch: a_root from mod-a\n\
a_root = {\n\
\tz = 2\n\
\ta = 1\n\
}\n\
z_root = {\n\
\tb = 2\n\
\ta = 1\n\
}\n"
		);
	}

	fn nested_statements() -> Vec<AstStatement> {
		vec![assignment(
			"root",
			block(vec![assignment(
				"child",
				block(vec![assignment("leaf", scalar(ScalarValue::Bool(true)))]),
			)]),
		)]
	}

	fn assignment(key: &str, value: AstValue) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: span(),
			value,
			span: span(),
		}
	}

	fn comment(text: &str) -> AstStatement {
		AstStatement::Comment {
			text: text.to_string(),
			span: span(),
		}
	}

	fn scalar_id(value: &str) -> AstValue {
		scalar(ScalarValue::Identifier(value.to_string()))
	}

	fn block(items: Vec<AstStatement>) -> AstValue {
		AstValue::Block {
			items,
			span: span(),
		}
	}

	fn scalar(value: ScalarValue) -> AstValue {
		AstValue::Scalar {
			value,
			span: span(),
		}
	}

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
}
