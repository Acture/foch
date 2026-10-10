use crate::game::eu4::script::parser::{AstStatement, AstValue, ScalarValue};
use crate::merge::MergeError;
use crate::project::DEFAULT_EMIT_INDENT;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EmitOptions {
	indent: String,
}

impl EmitOptions {
	pub(crate) fn with_indent(indent: impl Into<String>) -> Self {
		Self {
			indent: indent.into(),
		}
	}

	pub(crate) fn indent(&self) -> &str {
		&self.indent
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

/// Emits native statements with the default indent for callers outside the
/// merge pipeline, such as generated runtime tests.
pub fn emit_native_statements(statements: &[AstStatement]) -> Result<String, String> {
	emit_clausewitz_statements(statements).map_err(|error| error.to_string())
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::game::eu4::script::parser::{Span, SpanRange};

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
