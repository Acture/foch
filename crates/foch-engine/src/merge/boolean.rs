use foch_language::analyzer::parser::{AstStatement, AstValue, Span, SpanRange};

use super::patch::ast_statements_semantically_equal;

pub(crate) fn canonical_boolean_or_body(body: Vec<AstStatement>) -> Vec<AstStatement> {
	let disjuncts = unique_disjuncts(body_to_disjuncts(body));
	if disjuncts.is_empty() {
		Vec::new()
	} else {
		vec![make_boolean_block("OR", disjuncts)]
	}
}

pub(crate) fn combine_boolean_or_bodies(
	bodies: impl IntoIterator<Item = Vec<AstStatement>>,
) -> Option<Vec<AstStatement>> {
	let disjuncts = unique_disjuncts(bodies.into_iter().flat_map(body_to_disjuncts).collect());
	(!disjuncts.is_empty()).then(|| vec![make_boolean_block("OR", disjuncts)])
}

pub(crate) fn simplify_boolean_or_body(body: Vec<AstStatement>) -> Vec<AstStatement> {
	let mut disjuncts = unique_disjuncts(body_to_disjuncts(body));
	if disjuncts.len() != 1 {
		return if disjuncts.is_empty() {
			Vec::new()
		} else {
			vec![make_boolean_block("OR", disjuncts)]
		};
	}

	let disjunct = disjuncts.pop().expect("single disjunct checked");
	match disjunct {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} if key == "AND" => items,
		other => vec![other],
	}
}

fn unique_disjuncts(disjuncts: Vec<AstStatement>) -> Vec<AstStatement> {
	let mut unique = Vec::with_capacity(disjuncts.len());
	for disjunct in disjuncts {
		if !unique
			.iter()
			.any(|existing| ast_statements_semantically_equal(existing, &disjunct))
		{
			unique.push(disjunct);
		}
	}
	unique
}

fn body_to_disjuncts(body: Vec<AstStatement>) -> Vec<AstStatement> {
	if body.len() == 1
		&& let Some(items) = boolean_block_body(&body[0], "OR")
	{
		return items
			.into_iter()
			.flat_map(|item| body_to_disjuncts(vec![item]))
			.collect();
	}
	vec![body_to_disjunct(body)]
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
