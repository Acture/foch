use crate::game::eu4::script::parser::{AstStatement, AstValue, Span, SpanRange};

use super::patch::{ScalarEquality, ast_statements_equal_ignoring_comments};

pub(crate) fn canonical_boolean_or_body(body: Vec<AstStatement>) -> Vec<AstStatement> {
	let mut comments = Vec::new();
	let disjuncts = unique_disjuncts(body_to_disjuncts(body, &mut comments));
	boolean_or_body(comments, disjuncts)
}

pub(crate) fn combine_boolean_or_bodies(
	bodies: impl IntoIterator<Item = Vec<AstStatement>>,
) -> Option<Vec<AstStatement>> {
	let mut comments = Vec::new();
	let disjuncts = unique_disjuncts(
		bodies
			.into_iter()
			.flat_map(|body| body_to_disjuncts(body, &mut comments))
			.collect(),
	);
	(!disjuncts.is_empty()).then(|| boolean_or_body(comments, disjuncts))
}

pub(crate) fn simplify_boolean_or_body(body: Vec<AstStatement>) -> Vec<AstStatement> {
	let mut comments = Vec::new();
	let mut disjuncts = unique_disjuncts(body_to_disjuncts(body, &mut comments));
	if disjuncts.len() != 1 {
		return boolean_or_body(comments, disjuncts);
	}

	let disjunct = disjuncts.pop().expect("single disjunct checked");
	let mut body = comments;
	match disjunct {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} if key == "AND" => body.extend(items),
		other => body.push(other),
	}
	body
}

/// Re-emit a body from the comments lifted out of it and its disjuncts.
///
/// A body left with no disjunct carries no condition, so it yields its
/// comments rather than an empty `OR`.
fn boolean_or_body(
	mut comments: Vec<AstStatement>,
	disjuncts: Vec<AstStatement>,
) -> Vec<AstStatement> {
	if !disjuncts.is_empty() {
		comments.push(make_boolean_block("OR", disjuncts));
	}
	comments
}

fn unique_disjuncts(disjuncts: Vec<AstStatement>) -> Vec<AstStatement> {
	let mut unique: Vec<AstStatement> = Vec::with_capacity(disjuncts.len());
	for disjunct in disjuncts {
		// Deduplicate no more eagerly than the normalized tree that later
		// judges this output: it gives `SWE` and `"SWE"` different leaf kinds,
		// so collapsing them here would pick a different survivor on each side
		// of a comparison and turn an equal pair into an unequal one.
		if !unique.iter().any(|existing| {
			ast_statements_equal_ignoring_comments(existing, &disjunct, ScalarEquality::Exact)
		}) {
			unique.push(disjunct);
		}
	}
	unique
}

fn body_to_disjuncts(
	body: Vec<AstStatement>,
	comments: &mut Vec<AstStatement>,
) -> Vec<AstStatement> {
	let body = take_comments(body, comments);
	if body.len() == 1
		&& let Some(items) = boolean_block_body(&body[0], "OR")
	{
		return items
			.into_iter()
			.flat_map(|item| body_to_disjuncts(vec![item], comments))
			.collect();
	}
	if body.is_empty() {
		return Vec::new();
	}
	vec![body_to_disjunct(body)]
}

/// Lift every comment out of `body`.
///
/// Comments are trivia, but the shape chosen here outlives them: `detach_trivia`
/// removes the comment and leaves the `AND` that its presence caused, so a
/// comment-only difference would survive as a structural one. Deciding on the
/// content alone keeps the transform blind to trivia.
fn take_comments(body: Vec<AstStatement>, comments: &mut Vec<AstStatement>) -> Vec<AstStatement> {
	let mut content = Vec::with_capacity(body.len());
	for statement in body {
		if matches!(statement, AstStatement::Comment { .. }) {
			comments.push(statement);
		} else {
			content.push(statement);
		}
	}
	content
}

fn body_to_disjunct(mut body: Vec<AstStatement>) -> AstStatement {
	if body.len() == 1 {
		body.pop().expect("single statement checked")
	} else {
		make_boolean_block("AND", body)
	}
}

fn boolean_block_body(statement: &AstStatement, expected_key: &str) -> Option<Vec<AstStatement>> {
	match statement {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} if key == expected_key => Some(items.clone()),
		_ => None,
	}
}

fn make_boolean_block(key: &str, items: Vec<AstStatement>) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: synthetic_span(),
		value: AstValue::Block {
			items,
			span: synthetic_span(),
		},
		span: synthetic_span(),
	}
}

fn synthetic_span() -> SpanRange {
	let zero = Span {
		line: 0,
		column: 0,
		offset: 0,
	};
	SpanRange {
		start: zero.clone(),
		end: zero,
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use crate::game::eu4::script::emit::emit_clausewitz_statements;
	use crate::game::eu4::script::parser::{AstStatement, parse_clausewitz_content};

	use super::{canonical_boolean_or_body, simplify_boolean_or_body};

	fn body(source: &str) -> Vec<AstStatement> {
		let parsed = parse_clausewitz_content(PathBuf::from("test.txt"), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast.statements
	}

	fn emit(statements: &[AstStatement]) -> String {
		emit_clausewitz_statements(statements).expect("emit")
	}

	fn without_comments(statements: Vec<AstStatement>) -> Vec<AstStatement> {
		statements
			.into_iter()
			.filter(|statement| !matches!(statement, AstStatement::Comment { .. }))
			.collect()
	}

	/// A comment must not decide whether a body is a conjunction. `detach_trivia`
	/// removes the comment but keeps the `AND` its presence caused, so the shape
	/// would outlive the trivia and read as a content difference.
	#[test]
	fn canonicalization_shape_ignores_comments() {
		let plain = canonical_boolean_or_body(body("a = yes\n"));
		let commented = canonical_boolean_or_body(body("# note\na = yes\n"));

		assert_eq!(
			emit(&plain),
			emit(&without_comments(commented.clone())),
			"comment-only difference must not change the canonical shape"
		);
		assert!(
			emit(&commented).contains("# note"),
			"the comment itself must survive: {}",
			emit(&commented)
		);
	}

	/// A body holding nothing but comments states no condition, so it must not
	/// become an empty `OR`.
	#[test]
	fn canonicalization_of_a_comment_only_body_emits_no_disjunction() {
		let canonical = canonical_boolean_or_body(body("# only a note\n"));

		let rendered = emit(&canonical);
		assert!(!rendered.contains("OR"), "{rendered}");
		assert!(rendered.contains("# only a note"), "{rendered}");
	}

	/// `SWE` and `"SWE"` are one value to patch convergence but two leaves to the
	/// normalized tree. Collapsing them here would keep a different survivor on
	/// each side of a reordered pair and turn an equal pair into an unequal one.
	#[test]
	fn canonicalization_keeps_both_spellings_of_a_quoted_scalar() {
		let left = canonical_boolean_or_body(body("OR = {\n\ttag = SWE\n\ttag = \"SWE\"\n}\n"));
		let right = canonical_boolean_or_body(body("OR = {\n\ttag = \"SWE\"\n\ttag = SWE\n}\n"));

		assert_eq!(emit(&left).matches("tag =").count(), 2, "{}", emit(&left));
		assert_eq!(
			emit(&left).matches("tag =").count(),
			emit(&right).matches("tag =").count(),
			"both orderings must keep the same disjuncts"
		);
	}

	/// Genuine duplicates still collapse.
	#[test]
	fn canonicalization_still_drops_an_identical_disjunct() {
		let canonical = canonical_boolean_or_body(body("OR = {\n\ttag = SWE\n\ttag = SWE\n}\n"));

		assert_eq!(
			emit(&canonical).matches("tag = SWE").count(),
			1,
			"{}",
			emit(&canonical)
		);
	}

	/// The simplify path writes final merged text, so a comment inside a body
	/// that gets unwrapped must still reach the output.
	#[test]
	fn simplification_preserves_a_comment_inside_an_unwrapped_conjunction() {
		let simplified = simplify_boolean_or_body(body("# note\na = yes\nb = yes\n"));

		let rendered = emit(&simplified);
		assert!(rendered.contains("# note"), "{rendered}");
		assert!(rendered.contains("a = yes"), "{rendered}");
		assert!(rendered.contains("b = yes"), "{rendered}");
	}
}
