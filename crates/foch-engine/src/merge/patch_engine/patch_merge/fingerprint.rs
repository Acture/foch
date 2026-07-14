use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};

const VALUE_SCALAR: u8 = 0x01;
const VALUE_BLOCK: u8 = 0x02;
const SCALAR_IDENTIFIER: u8 = 0x10;
const SCALAR_STRING: u8 = 0x11;
const SCALAR_NUMBER: u8 = 0x12;
const SCALAR_BOOL: u8 = 0x13;
const STATEMENT_ASSIGNMENT: u8 = 0x20;
const STATEMENT_ITEM: u8 = 0x21;
const STATEMENT_COMMENT: u8 = 0x22;

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
	let length = u64::try_from(bytes.len()).expect("fingerprint input length fits in u64");
	out.extend_from_slice(&length.to_be_bytes());
	out.extend_from_slice(bytes);
}

fn encode_scalar(value: &ScalarValue, out: &mut Vec<u8>) {
	match value {
		ScalarValue::Identifier(value) => {
			out.push(SCALAR_IDENTIFIER);
			push_bytes(out, value.as_bytes());
		}
		ScalarValue::String(value) => {
			out.push(SCALAR_STRING);
			push_bytes(out, value.as_bytes());
		}
		ScalarValue::Number(value) => {
			out.push(SCALAR_NUMBER);
			push_bytes(out, value.as_bytes());
		}
		ScalarValue::Bool(value) => {
			out.push(SCALAR_BOOL);
			out.push(u8::from(*value));
		}
	}
}

fn encode_value(value: &AstValue, out: &mut Vec<u8>) {
	match value {
		AstValue::Scalar { value, .. } => {
			out.push(VALUE_SCALAR);
			encode_scalar(value, out);
		}
		AstValue::Block { items, .. } => {
			out.push(VALUE_BLOCK);
			let semantic_items = items
				.iter()
				.filter(|item| !matches!(item, AstStatement::Comment { .. }))
				.collect::<Vec<_>>();
			let item_count =
				u64::try_from(semantic_items.len()).expect("fingerprint item count fits in u64");
			out.extend_from_slice(&item_count.to_be_bytes());
			for item in semantic_items {
				encode_statement(item, out);
			}
		}
	}
}

fn encode_statement(statement: &AstStatement, out: &mut Vec<u8>) {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			out.push(STATEMENT_ASSIGNMENT);
			push_bytes(out, key.as_bytes());
			encode_value(value, out);
		}
		AstStatement::Item { value, .. } => {
			out.push(STATEMENT_ITEM);
			encode_value(value, out);
		}
		AstStatement::Comment { text, .. } => {
			out.push(STATEMENT_COMMENT);
			push_bytes(out, text.as_bytes());
		}
	}
}

fn digest(encoded: &[u8]) -> String {
	blake3::hash(encoded).to_hex().to_string()
}

/// Stable, span-ignoring fingerprint for an `AstValue`. Used to give each
/// distinct `AppendBlockItem` / `RemoveBlockItem` its own `PatchAddress` so
/// that multiple mods can append/remove different values inside the same
/// block without one clobbering the others.
pub(super) fn value_fingerprint(v: &AstValue) -> String {
	let mut encoded = Vec::new();
	encode_value(v, &mut encoded);
	digest(&encoded)
}

/// Span-ignoring fingerprint for an `AstStatement`, used by Union-policy
/// `InsertNode` / `RemoveNode` addresses that share the same `(path, key)`
/// but carry different bodies. Repeated-key parents can then keep distinct
/// insert/remove payloads at distinct addresses, while Recurse and other
/// unique-key policies deliberately collide at `(path, key)` so leaf
/// resolvers surface sibling conflicts.
pub(super) fn statement_fingerprint(stmt: &AstStatement) -> String {
	let mut encoded = Vec::new();
	encode_statement(stmt, &mut encoded);
	digest(&encoded)
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::parser::{ScalarValue, Span, SpanRange};

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

	fn scalar(value: ScalarValue) -> AstValue {
		AstValue::Scalar {
			value,
			span: span(),
		}
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

	#[test]
	fn scalar_fingerprints_include_the_scalar_type() {
		let boolean = scalar(ScalarValue::Bool(true));
		let string = scalar(ScalarValue::String("yes".to_string()));
		let identifier = scalar(ScalarValue::Identifier("yes".to_string()));

		assert_ne!(value_fingerprint(&boolean), value_fingerprint(&string));
		assert_ne!(value_fingerprint(&boolean), value_fingerprint(&identifier));
		assert_ne!(value_fingerprint(&string), value_fingerprint(&identifier));
	}

	#[test]
	fn nested_fingerprints_are_length_delimited() {
		let delimiter_in_scalar = block(vec![assignment(
			"x",
			scalar(ScalarValue::String("foo;ay=s:bar".to_string())),
		)]);
		let two_assignments = block(vec![
			assignment("x", scalar(ScalarValue::String("foo".to_string()))),
			assignment("y", scalar(ScalarValue::String("bar".to_string()))),
		]);

		assert_ne!(
			value_fingerprint(&delimiter_in_scalar),
			value_fingerprint(&two_assignments)
		);
	}
}
