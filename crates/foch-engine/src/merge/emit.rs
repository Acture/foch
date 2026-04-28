#![allow(dead_code)]

use super::error::MergeError;
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};

pub fn emit_clausewitz_statements(statements: &[AstStatement]) -> Result<String, MergeError> {
	let mut out = String::new();
	for statement in statements {
		emit_statement(statement, 0, &mut out)?;
	}
	Ok(out)
}

fn emit_statement(
	statement: &AstStatement,
	indent: usize,
	out: &mut String,
) -> Result<(), MergeError> {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			indent_into(out, indent);
			out.push_str(key);
			out.push_str(" = ");
			emit_value(value, indent, out)?;
			out.push('\n');
			Ok(())
		}
		AstStatement::Item { value, .. } => {
			indent_into(out, indent);
			emit_value(value, indent, out)?;
			out.push('\n');
			Ok(())
		}
		AstStatement::Comment { text, .. } => {
			indent_into(out, indent);
			out.push_str("# ");
			out.push_str(text);
			out.push('\n');
			Ok(())
		}
	}
}

fn emit_value(value: &AstValue, indent: usize, out: &mut String) -> Result<(), MergeError> {
	match value {
		AstValue::Scalar { value, .. } => {
			out.push_str(&render_scalar(value));
			Ok(())
		}
		AstValue::Block { items, .. } => {
			out.push_str("{\n");
			for item in items {
				emit_statement(item, indent + 1, out)?;
			}
			indent_into(out, indent);
			out.push('}');
			Ok(())
		}
	}
}

fn render_scalar(value: &ScalarValue) -> String {
	match value {
		ScalarValue::Identifier(value) => value.clone(),
		ScalarValue::String(value) => format!("\"{}\"", escape_string(value)),
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

fn escape_string(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn indent_into(out: &mut String, indent: usize) {
	for _ in 0..indent {
		out.push('\t');
	}
}
