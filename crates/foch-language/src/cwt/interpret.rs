use std::path::{Path, PathBuf};

use crate::analyzer::parser::{AstStatement, AstValue, parse_clausewitz_content};

use super::model::{
	CwtAlias, CwtComplexEnum, CwtEnum, CwtLink, CwtOption, CwtRule, CwtRuleBody, CwtRuleBodyEntry,
	CwtSchema, CwtScope, CwtSingleAlias, CwtSubtype, CwtType, CwtValueSet, CwtValueType,
	parse_bracket_key,
};

pub fn load_cwt_schema(content: &str) -> CwtSchema {
	let parsed = parse_clausewitz_content(PathBuf::from("<in-memory>.cwt"), content);
	interpret_schema(&parsed.ast.statements)
}

pub fn load_cwt_file(path: &Path) -> std::io::Result<CwtSchema> {
	let content = std::fs::read_to_string(path)?;
	let parsed = parse_clausewitz_content(path.to_path_buf(), &content);
	Ok(interpret_schema(&parsed.ast.statements))
}

fn interpret_schema(statements: &[AstStatement]) -> CwtSchema {
	let mut schema = CwtSchema::default();
	let mut pending_options = Vec::new();
	let mut pending_rule_bodies = Vec::new();

	for statement in statements {
		match statement {
			AstStatement::Comment { text, .. } => {
				if let Some(option) = parse_cwt_option(text) {
					pending_options.push(option);
				}
			}
			AstStatement::Assignment { key, value, .. } => {
				match key.as_str() {
					"types" => schema.types.extend(read_types(value)),
					"enums" => {
						schema.enums.extend(read_enums(value));
						schema.complex_enums.extend(read_complex_enums(value));
					}
					"values" => schema.value_sets.extend(read_value_sets(value)),
					"scopes" => schema.scopes.extend(read_scopes(value)),
					"links" => schema.links.extend(read_links(value)),
					_ => {
						if let Some(alias) =
							read_alias_assignment(key, value, std::mem::take(&mut pending_options))
						{
							schema.aliases.push(alias);
						} else if let Some(single_alias) = read_single_alias_assignment(key, value)
						{
							schema.single_aliases.push(single_alias);
						} else if let Some(value_set) = read_value_set_assignment(key, value) {
							schema.value_sets.push(value_set);
						} else if let Some(rules) = read_rule_body(value) {
							pending_rule_bodies.push(CwtRuleBodyEntry {
								key: key.clone(),
								rules,
							});
						}
					}
				}
				pending_options.clear();
			}
			AstStatement::Item { .. } => pending_options.clear(),
		}
	}

	attach_rule_bodies(&mut schema, pending_rule_bodies);
	schema
}

fn attach_rule_bodies(schema: &mut CwtSchema, rule_bodies: Vec<CwtRuleBodyEntry>) {
	for rule_body in rule_bodies {
		if let Some(cwt_type) = schema
			.types
			.iter_mut()
			.find(|cwt_type| cwt_type.name == rule_body.key)
		{
			cwt_type.rules.extend(rule_body.rules);
		} else {
			schema.rule_bodies.push(rule_body);
		}
	}
}

fn read_types(value: &AstValue) -> Vec<CwtType> {
	let mut types = Vec::new();
	let Some(items) = block_items(value) else {
		return types;
	};
	let mut pending_options = Vec::new();

	for statement in items {
		match statement {
			AstStatement::Comment { text, .. } => {
				if let Some(option) = parse_cwt_option(text) {
					pending_options.push(option);
				}
			}
			AstStatement::Assignment { key, value, .. } => {
				if let Some(("type", name)) = parse_bracket_key(key)
					&& let Some(mut cwt_type) = read_type(name, value)
				{
					cwt_type.options.append(&mut pending_options);
					types.push(cwt_type);
				} else {
					pending_options.clear();
				}
			}
			AstStatement::Item { .. } => pending_options.clear(),
		}
	}

	types
}

fn read_type(name: &str, value: &AstValue) -> Option<CwtType> {
	let items = block_items(value)?;
	let mut cwt_type = CwtType {
		name: name.to_string(),
		..Default::default()
	};
	let mut pending_options = Vec::new();

	for statement in items {
		match statement {
			AstStatement::Comment { text, .. } => {
				if let Some(option) = parse_cwt_option(text) {
					pending_options.push(option);
				}
			}
			AstStatement::Assignment { key, value, .. } => {
				if let Some(("subtype", name)) = parse_bracket_key(key)
					&& let Some(mut subtype) = read_subtype(name, value)
				{
					attach_options_to_subtype(&mut subtype, &mut pending_options);
					cwt_type.subtypes.push(subtype);
				} else {
					cwt_type.options.append(&mut pending_options);
					match key.as_str() {
						"path" => cwt_type.path = scalar_text(value),
						"name_field" => cwt_type.name_field = scalar_text(value),
						"name_from_file" => cwt_type.name_from_file = is_yes(value),
						"type_per_file" => cwt_type.type_per_file = is_yes(value),
						"skip_root_key" => cwt_type.skip_root_key.extend(value_texts(value)),
						_ => {}
					}
					pending_options.clear();
				}
			}
			AstStatement::Item { .. } => pending_options.clear(),
		}
	}

	Some(cwt_type)
}

fn read_subtype(name: &str, value: &AstValue) -> Option<CwtSubtype> {
	let items = block_items(value)?;
	let mut subtype = CwtSubtype {
		name: name.to_string(),
		..Default::default()
	};
	let mut pending_options = Vec::new();

	for statement in items {
		match statement {
			AstStatement::Comment { text, .. } => {
				if let Some(option) = parse_cwt_option(text) {
					pending_options.push(option);
				}
			}
			AstStatement::Assignment { key, value, .. } => {
				if key == "type_key_filter" {
					subtype.type_key_filter.extend(value_texts(value));
				}
				subtype.options.append(&mut pending_options);
				pending_options.clear();
			}
			AstStatement::Item { .. } => pending_options.clear(),
		}
	}

	Some(subtype)
}

fn attach_options_to_subtype(subtype: &mut CwtSubtype, options: &mut Vec<CwtOption>) {
	for option in options.iter() {
		if option.key == "type_key_filter" {
			subtype
				.type_key_filter
				.extend(option.value.split_whitespace().map(str::to_string));
		}
	}
	subtype.options.append(options);
}

fn read_enums(value: &AstValue) -> Vec<CwtEnum> {
	let mut enums = Vec::new();
	let Some(items) = block_items(value) else {
		return enums;
	};

	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		let Some(("enum", name)) = parse_bracket_key(key) else {
			continue;
		};
		enums.push(CwtEnum {
			name: name.to_string(),
			values: value_texts(value),
		});
	}

	enums
}

fn read_complex_enums(value: &AstValue) -> Vec<CwtComplexEnum> {
	let mut complex_enums = Vec::new();
	let Some(items) = block_items(value) else {
		return complex_enums;
	};

	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		let Some(("complex_enum", name)) = parse_bracket_key(key) else {
			continue;
		};
		let Some(complex_enum) = read_complex_enum(name, value) else {
			continue;
		};
		complex_enums.push(complex_enum);
	}

	complex_enums
}

fn read_complex_enum(name: &str, value: &AstValue) -> Option<CwtComplexEnum> {
	let items = block_items(value)?;
	let name_rules = items.iter().find_map(|statement| match statement {
		AstStatement::Assignment { key, value, .. } if key == "name" => {
			Some(read_rules(block_items(value)?))
		}
		_ => None,
	})?;

	let mut complex_enum = CwtComplexEnum {
		name: name.to_string(),
		name_rules,
		..Default::default()
	};

	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		match key.as_str() {
			"path" => complex_enum.path = scalar_text(value),
			"start_from_root" => complex_enum.start_from_root = is_yes(value),
			_ => {}
		}
	}

	Some(complex_enum)
}

fn read_value_sets(value: &AstValue) -> Vec<CwtValueSet> {
	let mut value_sets = Vec::new();
	let Some(items) = block_items(value) else {
		return value_sets;
	};

	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		if let Some(value_set) = read_value_set_assignment(key, value) {
			value_sets.push(value_set);
		}
	}

	value_sets
}

fn read_value_set_assignment(key: &str, value: &AstValue) -> Option<CwtValueSet> {
	let (head, name) = parse_bracket_key(key)?;
	if !matches!(head, "value" | "value_set") {
		return None;
	}

	Some(CwtValueSet {
		name: name.to_string(),
		values: value_texts(value),
	})
}

fn read_alias_assignment(
	key: &str,
	value: &AstValue,
	mut options: Vec<CwtOption>,
) -> Option<CwtAlias> {
	let ("alias", inner) = parse_bracket_key(key)? else {
		return None;
	};
	let (category, name) = inner.split_once(':')?;
	let category = category.trim();
	let name = name.trim();
	if category.is_empty() || name.is_empty() {
		return None;
	}

	let mut scope = scope_values_from_options(&options);
	let preceding_option_count = options.len();
	options.extend(alias_body_options(value));
	scope.extend(scope_values_from_options(
		&options[preceding_option_count..],
	));

	Some(CwtAlias {
		category: category.to_string(),
		name: name.to_string(),
		scope,
		options,
	})
}

fn read_single_alias_assignment(key: &str, value: &AstValue) -> Option<CwtSingleAlias> {
	let ("single_alias", name) = parse_bracket_key(key)? else {
		return None;
	};
	let rules = block_items(value).map(read_rules).unwrap_or_default();

	Some(CwtSingleAlias {
		name: name.to_string(),
		rules,
	})
}

fn alias_body_options(value: &AstValue) -> Vec<CwtOption> {
	let mut options = Vec::new();
	let Some(items) = block_items(value) else {
		return options;
	};

	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		if !matches!(key.as_str(), "scope" | "push_scope" | "replace_scope") {
			continue;
		}
		let value = value_texts(value).join(" ");
		if value.is_empty() {
			continue;
		}
		options.push(CwtOption {
			key: key.clone(),
			value,
		});
	}

	options
}

fn scope_values_from_options(options: &[CwtOption]) -> Vec<String> {
	options
		.iter()
		.filter(|option| option.key == "scope")
		.flat_map(|option| option.value.split_whitespace().map(str::to_string))
		.collect()
}

fn read_scopes(value: &AstValue) -> Vec<CwtScope> {
	let mut scopes = Vec::new();
	let Some(items) = block_items(value) else {
		return scopes;
	};

	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		let Some(scope_items) = block_items(value) else {
			continue;
		};
		let aliases = scope_items
			.iter()
			.find_map(|scope_statement| match scope_statement {
				AstStatement::Assignment { key, value, .. } if key == "aliases" => {
					Some(value_texts(value))
				}
				_ => None,
			})
			.unwrap_or_default();

		scopes.push(CwtScope {
			name: key.clone(),
			aliases,
		});
	}

	scopes
}

fn read_links(value: &AstValue) -> Vec<CwtLink> {
	let mut links = Vec::new();
	let Some(items) = block_items(value) else {
		return links;
	};

	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		let Some(link_items) = block_items(value) else {
			continue;
		};
		let mut link = CwtLink {
			name: key.clone(),
			..Default::default()
		};
		for link_statement in link_items {
			let AstStatement::Assignment { key, value, .. } = link_statement else {
				continue;
			};
			match key.as_str() {
				"input_scopes" => link.input_scopes.extend(value_texts(value)),
				"output_scope" => link.output_scope = scalar_text(value),
				_ => {}
			}
		}
		links.push(link);
	}

	links
}

fn read_rule_body(value: &AstValue) -> Option<Vec<CwtRule>> {
	Some(read_rules(block_items(value)?))
}

fn read_rules(items: &[AstStatement]) -> Vec<CwtRule> {
	let mut rules = Vec::new();
	let mut pending_options = Vec::new();

	for statement in items {
		match statement {
			AstStatement::Comment { text, .. } => {
				if let Some(option) = parse_cwt_option(text) {
					pending_options.push(option);
				}
			}
			AstStatement::Assignment { key, value, .. } => {
				let options = std::mem::take(&mut pending_options);
				let cardinality = cardinality_from_options(&options);
				let body = match value {
					AstValue::Scalar { value, .. } => {
						CwtRuleBody::Leaf(CwtValueType::from_token(&value.as_text()))
					}
					AstValue::Block { items, .. } => CwtRuleBody::Block(read_rules(items)),
				};
				rules.push(CwtRule {
					key: key.clone(),
					body,
					cardinality,
					options,
				});
			}
			AstStatement::Item { value, .. } => {
				let options = std::mem::take(&mut pending_options);
				let Some(text) = scalar_text(value) else {
					continue;
				};
				let cardinality = cardinality_from_options(&options);
				rules.push(CwtRule {
					key: text.clone(),
					body: CwtRuleBody::Leaf(CwtValueType::from_token(&text)),
					cardinality,
					options,
				});
			}
		}
	}

	rules
}

fn cardinality_from_options(options: &[CwtOption]) -> Option<String> {
	options
		.iter()
		.find(|option| option.key == "cardinality")
		.map(|option| option.value.clone())
}

fn parse_cwt_option(text: &str) -> Option<CwtOption> {
	let text = text.trim();
	let option_text = text.strip_prefix('#')?.trim();
	let (key, value) = option_text.split_once('=')?;
	let key = key.trim();
	let value = normalize_option_value(value.trim());
	if key.is_empty() || value.is_empty() {
		return None;
	}

	Some(CwtOption {
		key: key.to_string(),
		value: value.to_string(),
	})
}

fn normalize_option_value(value: &str) -> &str {
	value
		.strip_prefix('{')
		.and_then(|value| value.strip_suffix('}'))
		.map(str::trim)
		.unwrap_or(value)
}

fn block_items(value: &AstValue) -> Option<&[AstStatement]> {
	match value {
		AstValue::Block { items, .. } => Some(items),
		AstValue::Scalar { .. } => None,
	}
}

fn scalar_text(value: &AstValue) -> Option<String> {
	match value {
		AstValue::Scalar { value, .. } => Some(value.as_text()),
		AstValue::Block { .. } => None,
	}
}

fn value_texts(value: &AstValue) -> Vec<String> {
	match value {
		AstValue::Scalar { value, .. } => vec![value.as_text()],
		AstValue::Block { items, .. } => items.iter().filter_map(statement_text).collect(),
	}
}

fn statement_text(statement: &AstStatement) -> Option<String> {
	match statement {
		AstStatement::Item {
			value: AstValue::Scalar { value, .. },
			..
		} => Some(value.as_text()),
		AstStatement::Assignment { key, .. } => Some(key.clone()),
		AstStatement::Item {
			value: AstValue::Block { .. },
			..
		}
		| AstStatement::Comment { .. } => None,
	}
}

fn is_yes(value: &AstValue) -> bool {
	scalar_text(value).is_some_and(|value| value.eq_ignore_ascii_case("yes"))
}
