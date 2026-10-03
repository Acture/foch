use std::collections::BTreeMap;

use crate::game::eu4::content::{MergeKeySource, MergePolicies, NamedContainerPolicy};
use crate::game::eu4::script::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};

use crate::merge::semantic_fingerprint::{statement_fingerprint, value_fingerprint};

const SCROLL_LAYER_PREFIX: &str = "foch_scroll_layer_";
const SCROLL_LAYER_HEIGHT: i64 = 1000;
const SCROLL_VIEWPORT_WIDTH: i64 = 1000;
const SCROLL_VIEWPORT_HEIGHT: i64 = 600;
const SCROLL_BAR_SPRITE: &str = "standardlistbox_slider";

pub(crate) fn scroll_stack_variant_identity(
	policies: &MergePolicies,
	key: &str,
	value: &AstValue,
) -> Option<String> {
	let name = scroll_stack_name(policies, key, value)?;
	Some(format!("{key}:{name}:{}", value_fingerprint(value)))
}

pub(crate) fn coalesce_scroll_stack_variants(
	statements: &mut Vec<AstStatement>,
	policies: &MergePolicies,
) {
	if policies.named_container != NamedContainerPolicy::ScrollStack {
		return;
	}
	for statement in statements.iter_mut() {
		if let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = statement
		{
			coalesce_scroll_stack_variants(items, policies);
		}
	}

	let mut positions = BTreeMap::<(String, String), usize>::new();
	let mut coalesced = Vec::with_capacity(statements.len());
	for statement in std::mem::take(statements) {
		let identity = match &statement {
			AstStatement::Assignment { key, value, .. } => {
				scroll_stack_name(policies, key, value).map(|name| (key.clone(), name))
			}
			AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
		};
		let Some(identity) = identity else {
			coalesced.push(statement);
			continue;
		};
		let Some(index) = positions.get(&identity).copied() else {
			positions.insert(identity, coalesced.len());
			coalesced.push(statement);
			continue;
		};
		if statement_fingerprint(&coalesced[index]) == statement_fingerprint(&statement) {
			continue;
		}
		if let Some(stacked) = synthesize_scroll_stack(&coalesced[index], &statement) {
			coalesced[index] = stacked;
		} else {
			coalesced.push(statement);
		}
	}
	*statements = coalesced;
}

fn scroll_stack_name(policies: &MergePolicies, key: &str, value: &AstValue) -> Option<String> {
	if policies.named_container != NamedContainerPolicy::ScrollStack {
		return None;
	}
	let MergeKeySource::ChildFieldValue {
		child_key_field,
		child_types,
	} = policies.nested_merge_key_source
	else {
		return None;
	};
	if !child_types.is_empty() && !child_types.contains(&key) {
		return None;
	}
	scalar_field(value, child_key_field)
}

fn scalar_field(value: &AstValue, field: &str) -> Option<String> {
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	items.iter().find_map(|statement| match statement {
		AstStatement::Assignment {
			key,
			value: AstValue::Scalar { value, .. },
			..
		} if key == field => Some(value.as_text()),
		AstStatement::Assignment { .. }
		| AstStatement::Item { .. }
		| AstStatement::Comment { .. } => None,
	})
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

fn scalar_assignment(key: &str, value: ScalarValue) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: synthetic_span(),
		value: AstValue::Scalar {
			value,
			span: synthetic_span(),
		},
		span: synthetic_span(),
	}
}

fn block_assignment(key: &str, items: Vec<AstStatement>) -> AstStatement {
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

fn xy_block(key: &str, x: i64, y: i64) -> AstStatement {
	block_assignment(
		key,
		vec![
			scalar_assignment("x", ScalarValue::Number(x.to_string())),
			scalar_assignment("y", ScalarValue::Number(y.to_string())),
		],
	)
}

fn body_name(body: &[AstStatement]) -> Option<String> {
	body.iter().find_map(|statement| match statement {
		AstStatement::Assignment {
			key,
			value: AstValue::Scalar { value, .. },
			..
		} if key == "name" => Some(value.as_text()),
		AstStatement::Assignment { .. }
		| AstStatement::Item { .. }
		| AstStatement::Comment { .. } => None,
	})
}

fn strip_name(body: &[AstStatement]) -> Vec<AstStatement> {
	body.iter()
		.filter(
			|statement| !matches!(statement, AstStatement::Assignment { key, .. } if key == "name"),
		)
		.cloned()
		.collect()
}

fn is_scroll_stack_body(body: &[AstStatement]) -> bool {
	body.iter().any(|statement| match statement {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} if key == "containerWindowType" => {
			body_name(items).is_some_and(|name| name.starts_with(SCROLL_LAYER_PREFIX))
		}
		_ => false,
	})
}

fn make_layer(index: usize, widgets: Vec<AstStatement>) -> AstStatement {
	let mut items = vec![
		scalar_assignment(
			"name",
			ScalarValue::String(format!("{SCROLL_LAYER_PREFIX}{index}")),
		),
		xy_block("position", 0, index as i64 * SCROLL_LAYER_HEIGHT),
	];
	items.extend(widgets);
	block_assignment("containerWindowType", items)
}

pub(crate) fn synthesize_scroll_stack(
	existing: &AstStatement,
	incoming: &AstStatement,
) -> Option<AstStatement> {
	let (key, existing_body) = match existing {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} => (key.clone(), items),
		_ => return None,
	};
	let AstStatement::Assignment {
		value: AstValue::Block {
			items: incoming_body,
			..
		},
		..
	} = incoming
	else {
		return None;
	};
	let name = body_name(existing_body)?;
	let mut parent = Vec::new();
	let mut layers = Vec::new();
	if is_scroll_stack_body(existing_body) {
		let existing_layers = existing_body
			.iter()
			.filter(|statement| {
				matches!(statement, AstStatement::Assignment { key, .. } if key == "containerWindowType")
			})
			.count();
		for statement in existing_body {
			match statement {
				AstStatement::Assignment { key, .. } if key == "containerWindowType" => {
					layers.push(statement.clone());
				}
				AstStatement::Assignment { key, .. }
					if key == "size" || key == "verticalScrollbar" => {}
				other => parent.push(other.clone()),
			}
		}
		layers.push(make_layer(existing_layers, strip_name(incoming_body)));
	} else {
		parent.push(scalar_assignment("name", ScalarValue::String(name)));
		layers.push(make_layer(0, strip_name(existing_body)));
		layers.push(make_layer(1, strip_name(incoming_body)));
	}
	parent.push(xy_block(
		"size",
		SCROLL_VIEWPORT_WIDTH,
		SCROLL_VIEWPORT_HEIGHT,
	));
	parent.push(scalar_assignment(
		"verticalScrollbar",
		ScalarValue::String(SCROLL_BAR_SPRITE.to_string()),
	));
	parent.extend(layers);
	Some(block_assignment(&key, parent))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn policies() -> MergePolicies {
		MergePolicies {
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "name",
				child_types: &["windowType"],
			},
			named_container: NamedContainerPolicy::ScrollStack,
			..MergePolicies::default()
		}
	}

	fn name(value: &str) -> AstStatement {
		scalar_assignment("name", ScalarValue::String(value.to_string()))
	}

	fn widget(id: &str, icon: &str) -> AstStatement {
		block_assignment(
			"windowType",
			vec![name(id), block_assignment("iconType", vec![name(icon)])],
		)
	}

	fn collect_names(statements: &[AstStatement], output: &mut Vec<String>) {
		for statement in statements {
			if let AstStatement::Assignment { key, value, .. } = statement {
				if key == "name"
					&& let AstValue::Scalar { value, .. } = value
				{
					output.push(value.as_text());
				}
				if let AstValue::Block { items, .. } = value {
					collect_names(items, output);
				}
			}
		}
	}

	fn names_in(statement: &AstStatement) -> Vec<String> {
		let mut output = Vec::new();
		collect_names(std::slice::from_ref(statement), &mut output);
		output
	}

	#[test]
	fn synthesize_scroll_stack_keeps_both_bodies_lossless() {
		let stacked = synthesize_scroll_stack(&widget("X", "icon_a"), &widget("X", "icon_b"))
			.expect("scroll stack");
		let names = names_in(&stacked);
		for expected in ["X", "icon_a", "icon_b"] {
			assert!(
				names.contains(&expected.to_string()),
				"{expected}: {names:?}"
			);
		}
		assert_eq!(
			names
				.iter()
				.filter(|name| name.starts_with(SCROLL_LAYER_PREFIX))
				.count(),
			2,
		);
	}

	#[test]
	fn coalesce_scroll_stack_variants_appends_contributors_flat() {
		let mut statements = vec![
			widget("X", "icon_a"),
			widget("X", "icon_b"),
			widget("X", "icon_c"),
		];

		coalesce_scroll_stack_variants(&mut statements, &policies());

		assert_eq!(statements.len(), 1);
		let names = names_in(&statements[0]);
		for expected in ["icon_a", "icon_b", "icon_c"] {
			assert!(
				names.contains(&expected.to_string()),
				"{expected}: {names:?}"
			);
		}
		assert_eq!(
			names
				.iter()
				.filter(|name| name.starts_with(SCROLL_LAYER_PREFIX))
				.count(),
			3,
		);
	}
}
