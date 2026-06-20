use foch_language::analyzer::parser::{AstStatement, AstValue};

/// Stable, span-ignoring fingerprint for an `AstValue`. Used to give each
/// distinct `AppendBlockItem` / `RemoveBlockItem` its own `PatchAddress` so
/// that multiple mods can append/remove different values inside the same
/// block without one clobbering the others.
pub(super) fn value_fingerprint(v: &AstValue) -> String {
	let mut out = String::new();
	fingerprint_into(v, &mut out);
	out
}

/// Span-ignoring fingerprint for an `AstStatement`, used by Union-policy
/// `InsertNode` / `RemoveNode` addresses that share the same `(path, key)`
/// but carry different bodies. Repeated-key parents can then keep distinct
/// insert/remove payloads at distinct addresses, while Recurse and other
/// unique-key policies deliberately collide at `(path, key)` so leaf
/// resolvers surface sibling conflicts.
pub(super) fn statement_fingerprint(stmt: &AstStatement) -> String {
	let mut out = String::new();
	match stmt {
		AstStatement::Assignment { value, .. } => fingerprint_into(value, &mut out),
		AstStatement::Item { value, .. } => fingerprint_into(value, &mut out),
		AstStatement::Comment { text, .. } => {
			out.push('c');
			out.push(':');
			out.push_str(text);
		}
	}
	out
}

pub(super) fn fingerprint_into(v: &AstValue, out: &mut String) {
	match v {
		AstValue::Scalar { value, .. } => {
			out.push('s');
			out.push(':');
			out.push_str(&value.as_text());
		}
		AstValue::Block { items, .. } => {
			out.push('b');
			out.push('[');
			for s in items {
				match s {
					AstStatement::Assignment { key, value, .. } => {
						out.push('a');
						out.push_str(key);
						out.push('=');
						fingerprint_into(value, out);
						out.push(';');
					}
					AstStatement::Item { value, .. } => {
						out.push('i');
						fingerprint_into(value, out);
						out.push(';');
					}
					AstStatement::Comment { .. } => {}
				}
			}
			out.push(']');
		}
	}
}
