use std::path::{Path, PathBuf};

use crate::analyzer::parser::{AstStatement, AstValue, parse_clausewitz_content};

use super::model::{
	CwtAlias, CwtEnum, CwtLink, CwtOption, CwtSchema, CwtScope, CwtSubtype, CwtType,
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

pub fn parse_bracket_key(key: &str) -> Option<(&str, &str)> {
	let open = key.find('[')?;
	let close = key.rfind(']')?;
	if close + 1 != key.len() {
		return None;
	}

	let head = key[..open].trim();
	let inner = key[open + 1..close].trim();
	if head.is_empty() || inner.is_empty() {
		return None;
	}

	Some((head, inner))
}

fn interpret_schema(statements: &[AstStatement]) -> CwtSchema {
	let mut schema = CwtSchema::default();
	let mut pending_options = Vec::new();

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
					"enums" => schema.enums.extend(read_enums(value)),
					"scopes" => schema.scopes.extend(read_scopes(value)),
					"links" => schema.links.extend(read_links(value)),
					_ => {
						if let Some(alias) =
							read_alias_assignment(key, value, std::mem::take(&mut pending_options))
						{
							schema.aliases.push(alias);
						}
					}
				}
				pending_options.clear();
			}
			AstStatement::Item { .. } => pending_options.clear(),
		}
	}

	schema
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
