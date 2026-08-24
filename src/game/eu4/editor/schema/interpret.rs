use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, SpanRange, parse_clausewitz_content,
};
use crate::game::schema::query::{
	CompiledAlias, CompiledAliasCategory, CompiledBindFieldMatch, CompiledFieldAttributes,
	CompiledLink, CompiledRoot, CompiledRuleCondition, CompiledRuleField, CompiledRuleValue,
	CompiledSeverity, CwtQuery, RuleContext,
};
use crate::model::{LocalisationDefinition, Severity};

use super::{
	EditorPosition, EditorRange, SchemaCompletion, SchemaCompletionKind, SchemaDiagnostic,
	SchemaHover, SchemaWorkspace,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct KeyPathTarget {
	parent_path: Vec<String>,
	key: String,
	range: EditorRange,
}

pub(super) fn schema_hover(
	engine: &CwtQuery,
	file_path: &Path,
	text: &str,
	position: EditorPosition,
	dynamic_values: Option<&SchemaWorkspace>,
) -> Option<SchemaHover> {
	let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
	let target = find_hover_target(&parsed.ast.statements, position, &[])?;
	let parent_path = target
		.parent_path
		.iter()
		.map(String::as_str)
		.collect::<Vec<_>>();
	let parent_context = schema_bind_context(
		engine,
		dynamic_values,
		file_path,
		&parsed.ast.statements,
		&parent_path,
	)?;
	let active_subtypes =
		schema_active_subtypes_for_path(engine, file_path, &parsed.ast.statements, &parent_path);
	let field_match = schema_bind_field_match(
		engine,
		dynamic_values,
		parent_context,
		&target.key,
		&active_subtypes,
	)?;
	Some(SchemaHover {
		markdown: render_schema_hover_markdown(engine, dynamic_values, &target.key, &field_match),
		range: target.range,
	})
}

fn find_hover_target(
	statements: &[AstStatement],
	position: EditorPosition,
	parent_path: &[String],
) -> Option<KeyPathTarget> {
	for statement in statements {
		let AstStatement::Assignment {
			key,
			key_span,
			value,
			..
		} = statement
		else {
			continue;
		};
		if span_contains_position(key_span, position) {
			return Some(KeyPathTarget {
				parent_path: parent_path.to_vec(),
				key: key.clone(),
				range: editor_range_from_span(key_span),
			});
		}
		if let AstValue::Block { items, span } = value
			&& span_contains_position(span, position)
		{
			let mut child_path = parent_path.to_vec();
			child_path.push(key.clone());
			if let Some(target) = find_hover_target(items, position, &child_path) {
				return Some(target);
			}
		}
	}
	None
}

fn render_schema_hover_markdown(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	key: &str,
	field_match: &CompiledBindFieldMatch<'_>,
) -> String {
	let value = schema_match_value(field_match);
	let mut sections = vec![
		format!("**{key}**"),
		format!("Type: `{}`", rule_value_kind(value)),
	];
	if let Some(values) = schema_dynamic_alias_source(engine, dynamic_values, key, field_match) {
		sections.push(format!(
			"Dynamic alias: `{}` `{}`",
			values.kind.label(),
			values.name
		));
	}
	if let Some(values) = schema_dynamic_key_source(engine, dynamic_values, key, field_match) {
		sections.push(format!(
			"Dynamic key: `{}` `{}`",
			values.kind.label(),
			values.name
		));
	}
	if let Some(values) = schema_allowed_values(engine, dynamic_values, value) {
		sections.push(format!(
			"Value set: `{}` `{}`",
			values.kind.label(),
			values.name
		));
		sections.push(format!(
			"Allowed values: {}",
			format_allowed_values(&values.values)
		));
	}
	if let Some(description) = schema_hover_description(field_match) {
		sections.push(description.to_string());
	}
	if let Some(scope_context) = schema_hover_scope_context(field_match) {
		sections.push(format!("Scope context: {scope_context}"));
	}
	if let Some(cardinality) = schema_hover_cardinality(field_match) {
		sections.push(format!("Cardinality: `{cardinality}`"));
	}
	sections.join("\n\n")
}

fn schema_dynamic_key_source<'a>(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	key: &str,
	field_match: &'a CompiledBindFieldMatch<'a>,
) -> Option<SchemaAllowedValues<'a>> {
	let field = field_match.field();
	if field.key == key {
		return None;
	}
	let (head, name) = parse_schema_marker(&field.key)?;
	if head == "alias_name" {
		return None;
	}
	let values = schema_allowed_values_for_marker(engine, dynamic_values, head, name)?;
	values
		.values
		.iter()
		.any(|value| value == key)
		.then_some(values)
}

fn rule_value_kind(value: &CompiledRuleValue) -> &'static str {
	match value {
		CompiledRuleValue::Scalar(_) => "Scalar",
		CompiledRuleValue::Block(_) => "Block",
		CompiledRuleValue::Marker(marker) if schema_allowed_value_marker(marker).is_some() => {
			"Scalar"
		}
		CompiledRuleValue::Marker(_) => "Marker",
	}
}

fn schema_hover_description<'a>(field_match: &'a CompiledBindFieldMatch<'a>) -> Option<&'a str> {
	field_match
		.field()
		.attributes
		.description
		.as_deref()
		.or_else(|| {
			field_match
				.alias()
				.and_then(|alias| alias.attributes.description.as_deref())
		})
}

fn schema_hover_scope_context(field_match: &CompiledBindFieldMatch<'_>) -> Option<String> {
	let field_attributes = &field_match.field().attributes;
	let alias_attributes = field_match.alias().map(|alias| &alias.attributes);
	let mut parts = Vec::new();
	if let Some(push_scope) = field_attributes
		.push_scope
		.as_deref()
		.or_else(|| alias_attributes.and_then(|attributes| attributes.push_scope.as_deref()))
	{
		parts.push(format!("push_scope=`{push_scope}`"));
	}
	let scope = if !field_attributes.scope.is_empty() {
		Some(field_attributes.scope.as_slice())
	} else {
		alias_attributes.and_then(|attributes| {
			(!attributes.scope.is_empty()).then_some(attributes.scope.as_slice())
		})
	};
	if let Some(scope) = scope {
		parts.push(format!("scope={}", format_scope_values(scope)));
	}
	let replace_scope = if !field_attributes.replace_scope.is_empty() {
		Some(&field_attributes.replace_scope)
	} else {
		alias_attributes.and_then(|attributes| {
			(!attributes.replace_scope.is_empty()).then_some(&attributes.replace_scope)
		})
	};
	if let Some(replace_scope) = replace_scope {
		let mut entries = replace_scope
			.iter()
			.map(|(source, target)| format!("`{source}`→`{target}`"))
			.collect::<Vec<_>>();
		entries.sort();
		parts.push(format!("replace_scope={}", entries.join(", ")));
	}
	(!parts.is_empty()).then(|| parts.join("; "))
}

fn schema_dynamic_alias_source<'a>(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	key: &str,
	field_match: &'a CompiledBindFieldMatch<'a>,
) -> Option<SchemaAllowedValues<'a>> {
	let alias = field_match.alias()?;
	if alias.name == key {
		return None;
	}
	let values = schema_dynamic_alias_values(engine, dynamic_values, alias)?;
	values
		.values
		.iter()
		.any(|value| value == key)
		.then_some(values)
}

fn format_scope_values(values: &[String]) -> String {
	values
		.iter()
		.map(|value| format!("`{value}`"))
		.collect::<Vec<_>>()
		.join(", ")
}

fn format_scope_refs(values: &[String]) -> String {
	values
		.iter()
		.map(|value| format!("`{value}`"))
		.collect::<Vec<_>>()
		.join(", ")
}

fn schema_hover_cardinality(field_match: &CompiledBindFieldMatch<'_>) -> Option<String> {
	schema_match_cardinality(field_match).map(format_cardinality)
}

fn format_cardinality(cardinality: (u32, Option<u32>)) -> String {
	match cardinality {
		(minimum, Some(maximum)) => format!("{minimum}..{maximum}"),
		(minimum, None) => format!("{minimum}..inf"),
	}
}

fn editor_range_from_span(span: &SpanRange) -> EditorRange {
	EditorRange {
		start: EditorPosition {
			line: span.start.line.saturating_sub(1) as u32,
			character: span.start.column.saturating_sub(1) as u32,
		},
		end: EditorPosition {
			line: span.end.line.saturating_sub(1) as u32,
			character: span.end.column.saturating_sub(1) as u32,
		},
	}
}

fn span_contains_position(span: &SpanRange, position: EditorPosition) -> bool {
	let line = position.line as usize + 1;
	let column = position.character as usize + 1;
	(line > span.start.line || (line == span.start.line && column >= span.start.column))
		&& (line < span.end.line || (line == span.end.line && column < span.end.column))
}

pub(super) fn schema_completion_candidates(
	engine: &CwtQuery,
	file_path: &Path,
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
) -> Option<Vec<SchemaCompletion>> {
	schema_completion_candidates_with_index(engine, file_path, text, position, prefix_lower, None)
}

pub(super) fn schema_completion_candidates_with_index(
	engine: &CwtQuery,
	file_path: &Path,
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
	dynamic_values: Option<&SchemaWorkspace>,
) -> Option<Vec<SchemaCompletion>> {
	if !is_schema_key_completion_position(text, position) {
		let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
		return schema_value_completion_candidates(
			engine,
			dynamic_values,
			file_path,
			&parsed.ast.statements,
			text,
			position,
			prefix_lower,
		);
	}
	let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
	let parent_path = find_completion_parent_path(&parsed.ast.statements, position, &[])?;
	let parent_path = parent_path.iter().map(String::as_str).collect::<Vec<_>>();
	let parent_context = schema_bind_context(
		engine,
		dynamic_values,
		file_path,
		&parsed.ast.statements,
		&parent_path,
	)?;
	let active_scopes = schema_active_scopes_for_path(
		engine,
		dynamic_values,
		file_path,
		&parsed.ast.statements,
		&parent_path,
	);
	let active_subtypes =
		schema_active_subtypes_for_path(engine, file_path, &parsed.ast.statements, &parent_path);
	let mut candidates = Vec::new();
	for field in completion_rule_fields(parent_context)
		.into_iter()
		.filter(|field| schema_conditions_match(&field.conditions, &active_subtypes))
	{
		candidates.extend(schema_completion_entries_for_field(
			engine,
			dynamic_values,
			field,
			&active_scopes,
			prefix_lower,
		));
	}
	candidates.sort_by(|left, right| left.label.cmp(&right.label));
	candidates.dedup_by(|left, right| left.label == right.label);
	Some(candidates)
}

fn schema_value_completion_candidates(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	file_path: &Path,
	statements: &[AstStatement],
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
) -> Option<Vec<SchemaCompletion>> {
	let line = text.lines().nth(position.line as usize).unwrap_or_default();
	let upto: String = line.chars().take(position.character as usize).collect();
	let key = current_assignment_key(&upto)?;
	let parent_path = find_completion_parent_path(statements, position, &[])?;
	let parent_path = parent_path.iter().map(String::as_str).collect::<Vec<_>>();
	let parent_context =
		schema_bind_context(engine, dynamic_values, file_path, statements, &parent_path)?;
	let active_subtypes =
		schema_active_subtypes_for_path(engine, file_path, statements, &parent_path);
	let field_match = schema_bind_field_match(
		engine,
		dynamic_values,
		parent_context,
		key,
		&active_subtypes,
	)?;
	let values = schema_allowed_values(engine, dynamic_values, schema_match_value(&field_match))?;
	let kind = values.kind.completion_kind();
	let mut candidates = values
		.values
		.iter()
		.filter_map(|value| {
			let value_lower = value.to_ascii_lowercase();
			if !prefix_lower.is_empty() && !value_lower.starts_with(prefix_lower) {
				return None;
			}
			Some(SchemaCompletion {
				label: value.clone(),
				insert_text: value.clone(),
				kind,
				detail: format!("cwt {} {}", values.kind.label(), values.name),
			})
		})
		.collect::<Vec<_>>();
	candidates.sort_by(|left, right| left.label.cmp(&right.label));
	Some(candidates)
}

fn is_schema_key_completion_position(text: &str, position: EditorPosition) -> bool {
	let line = text
		.lines()
		.nth(position.line as usize)
		.map(str::to_string)
		.unwrap_or_default();
	let upto: String = line.chars().take(position.character as usize).collect();
	current_assignment_key(&upto).is_none()
}

fn find_completion_parent_path(
	statements: &[AstStatement],
	position: EditorPosition,
	parent_path: &[String],
) -> Option<Vec<String>> {
	for statement in statements {
		match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value,
				span,
			} => {
				if span_contains_position(key_span, position) {
					return Some(parent_path.to_vec());
				}
				if let AstValue::Block { items, span } = value
					&& span_contains_position(span, position)
				{
					let mut child_path = parent_path.to_vec();
					child_path.push(key.clone());
					return find_completion_parent_path(items, position, &child_path)
						.or(Some(child_path));
				}
				if span_contains_position(span, position) {
					return Some(parent_path.to_vec());
				}
			}
			AstStatement::Item { value, span } => {
				if let AstValue::Block {
					items,
					span: block_span,
				} = value && span_contains_position(block_span, position)
				{
					return find_completion_parent_path(items, position, parent_path)
						.or_else(|| Some(parent_path.to_vec()));
				}
				if span_contains_position(span, position) {
					return Some(parent_path.to_vec());
				}
			}
			AstStatement::Comment { span, .. } => {
				if span_contains_position(span, position) {
					return Some(parent_path.to_vec());
				}
			}
		}
	}
	Some(parent_path.to_vec())
}

fn completion_rule_fields(context: RuleContext<'_>) -> Vec<&CompiledRuleField> {
	match context {
		RuleContext::RootType(root) => root.rules.iter().collect(),
		RuleContext::Subtype(root, subtype) => {
			subtype.rules.iter().chain(root.rules.iter()).collect()
		}
		RuleContext::RuleField(field) => match &field.value {
			CompiledRuleValue::Block(children) => children.iter().collect(),
			_ => Vec::new(),
		},
		RuleContext::AliasRules(rules) => rules.iter().collect(),
	}
}

fn schema_completion_entries_for_field(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	field: &CompiledRuleField,
	active_scopes: &[String],
	prefix_lower: &str,
) -> Vec<SchemaCompletion> {
	let Some((head, payload)) = parse_schema_marker(&field.key) else {
		return direct_schema_completion_entry(field, prefix_lower)
			.into_iter()
			.collect();
	};
	if head != "alias_name" {
		if let Some(values) =
			schema_allowed_values_for_marker(engine, dynamic_values, head, payload)
		{
			return schema_dynamic_key_completion_entries(&values, prefix_lower);
		}
		return direct_schema_completion_entry(field, prefix_lower)
			.into_iter()
			.collect();
	}
	let category = CompiledAliasCategory::from_name(payload);
	let mut candidates = engine
		.aliases()
		.iter()
		.filter(|alias| alias.category == category)
		.filter(|alias| schema_alias_scope_matches(engine, alias, active_scopes))
		.flat_map(|alias| {
			schema_completion_entries_for_alias(engine, dynamic_values, field, alias, prefix_lower)
		})
		.collect::<Vec<_>>();
	candidates.sort_by(|left, right| left.label.cmp(&right.label));
	candidates
}

fn schema_completion_entries_for_alias(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	field: &CompiledRuleField,
	alias: &CompiledAlias,
	prefix_lower: &str,
) -> Vec<SchemaCompletion> {
	if schema_allowed_value_marker(&alias.name).is_some() {
		let Some(values) = schema_dynamic_alias_values(engine, dynamic_values, alias) else {
			return Vec::new();
		};
		return values
			.values
			.iter()
			.filter_map(|value| {
				let value_lower = value.to_ascii_lowercase();
				if !prefix_lower.is_empty() && !value_lower.starts_with(prefix_lower) {
					return None;
				}
				Some(SchemaCompletion {
					label: value.clone(),
					insert_text: value.clone(),
					kind: SchemaCompletionKind::Function,
					detail: format!("cwt dynamic alias {} {}", values.kind.label(), values.name),
				})
			})
			.collect();
	}
	let alias_name = &alias.name;
	let alias_name_lower = alias_name.to_ascii_lowercase();
	if !prefix_lower.is_empty() && !alias_name_lower.starts_with(prefix_lower) {
		return Vec::new();
	}
	vec![SchemaCompletion {
		label: alias_name.clone(),
		insert_text: alias_name.clone(),
		kind: SchemaCompletionKind::Function,
		detail: schema_completion_detail(
			alias
				.attributes
				.description
				.as_deref()
				.or(field.attributes.description.as_deref()),
		),
	}]
}

fn schema_dynamic_key_completion_entries(
	values: &SchemaAllowedValues<'_>,
	prefix_lower: &str,
) -> Vec<SchemaCompletion> {
	values
		.values
		.iter()
		.filter_map(|value| {
			let value_lower = value.to_ascii_lowercase();
			if !prefix_lower.is_empty() && !value_lower.starts_with(prefix_lower) {
				return None;
			}
			Some(SchemaCompletion {
				label: value.clone(),
				insert_text: value.clone(),
				kind: SchemaCompletionKind::Field,
				detail: format!("cwt dynamic key {} {}", values.kind.label(), values.name),
			})
		})
		.collect()
}

fn schema_active_scopes_for_path(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	file_path: &Path,
	document_statements: &[AstStatement],
	path: &[&str],
) -> Vec<String> {
	let mut active_scopes = Vec::new();
	for index in 0..=path.len() {
		if let Some(context) = schema_bind_context(
			engine,
			dynamic_values,
			file_path,
			document_statements,
			&path[..index],
		) {
			let scopes = schema_context_own_active_scopes(context);
			if !scopes.is_empty() {
				active_scopes = scopes;
			}
		}
		if let Some(component) = path.get(index)
			&& let Some(output_scope) = schema_link_output_scope(engine, component, &active_scopes)
		{
			active_scopes = vec![output_scope.to_string()];
		}
	}
	active_scopes
}

fn schema_context_own_active_scopes(context: RuleContext<'_>) -> Vec<String> {
	match context {
		RuleContext::RootType(root) => root.push_scope.iter().cloned().collect(),
		RuleContext::Subtype(root, subtype) => {
			let scopes = schema_active_scopes_from_attributes(&subtype.attributes);
			if scopes.is_empty() {
				root.push_scope.iter().cloned().collect()
			} else {
				scopes
			}
		}
		RuleContext::RuleField(field) => schema_active_scopes_from_attributes(&field.attributes),
		RuleContext::AliasRules(_) => Vec::new(),
	}
}

fn schema_active_scopes_from_attributes(attributes: &CompiledFieldAttributes) -> Vec<String> {
	if let Some(push_scope) = attributes.push_scope.as_ref() {
		return vec![push_scope.clone()];
	}
	if let Some(this_scope) = attributes.replace_scope.get("this") {
		return vec![this_scope.clone()];
	}
	attributes.scope.clone()
}

fn schema_link_output_scope<'a>(
	engine: &'a CwtQuery,
	key: &str,
	active_scopes: &[String],
) -> Option<&'a str> {
	let link = engine.link(key)?;
	if !schema_link_accepts_active_scope(engine, link, active_scopes) {
		return None;
	}
	link.output_scope.as_deref()
}

fn schema_link_accepts_active_scope(
	engine: &CwtQuery,
	link: &CompiledLink,
	active_scopes: &[String],
) -> bool {
	if active_scopes.is_empty() || link.input_scopes.is_empty() {
		return true;
	}
	link.input_scopes.iter().any(|input_scope| {
		active_scopes
			.iter()
			.any(|active_scope| engine.scope_matches(input_scope, active_scope))
	})
}

fn schema_alias_scope_matches(
	engine: &CwtQuery,
	alias: &CompiledAlias,
	active_scopes: &[String],
) -> bool {
	if active_scopes.is_empty() || alias.attributes.scope.is_empty() {
		return true;
	}
	alias.attributes.scope.iter().any(|scope| {
		active_scopes
			.iter()
			.any(|active| engine.scope_matches(scope, active))
	})
}

fn schema_bind_context<'p>(
	engine: &'p CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	file_path: &Path,
	document_statements: &[AstStatement],
	path: &[&str],
) -> Option<RuleContext<'p>> {
	if let Some(context) = engine.bind_context(file_path, path) {
		return Some(context);
	}
	for prefix_len in (0..path.len()).rev() {
		let Some(mut context) = engine.bind_context(file_path, &path[..prefix_len]) else {
			continue;
		};
		let mut resolved_path = path[..prefix_len]
			.iter()
			.map(|segment| (*segment).to_string())
			.collect::<Vec<_>>();
		let mut resolved_all = true;
		for segment in &path[prefix_len..] {
			let resolved_refs = resolved_path.iter().map(String::as_str).collect::<Vec<_>>();
			let active_subtypes = schema_active_subtypes_for_path(
				engine,
				file_path,
				document_statements,
				&resolved_refs,
			);
			let Some(field_match) =
				schema_bind_field_match(engine, dynamic_values, context, segment, &active_subtypes)
			else {
				resolved_all = false;
				break;
			};
			context = schema_child_context_for_field_match(field_match);
			resolved_path.push((*segment).to_string());
		}
		if resolved_all {
			return Some(context);
		}
	}
	None
}

fn schema_child_context_for_field_match<'p>(
	field_match: CompiledBindFieldMatch<'p>,
) -> RuleContext<'p> {
	match field_match {
		CompiledBindFieldMatch::Field(field) => RuleContext::RuleField(field),
		CompiledBindFieldMatch::Alias { alias, .. } => {
			RuleContext::AliasRules(alias.rules.as_slice())
		}
	}
}

fn schema_bind_field_match<'p>(
	engine: &'p CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	parent: RuleContext<'p>,
	key: &str,
	active_subtypes: &HashSet<String>,
) -> Option<CompiledBindFieldMatch<'p>> {
	let static_match = engine
		.bind_field_matches(parent, key)
		.into_iter()
		.find(|field_match| {
			schema_conditions_match(&field_match.field().conditions, active_subtypes)
		});
	static_match
		.or_else(|| {
			schema_dynamic_alias_field_match(engine, dynamic_values, parent, key, active_subtypes)
		})
		.or_else(|| {
			schema_dynamic_key_field_match(engine, dynamic_values, parent, key, active_subtypes)
		})
}

fn schema_dynamic_alias_field_match<'p>(
	engine: &'p CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	parent: RuleContext<'p>,
	key: &str,
	active_subtypes: &HashSet<String>,
) -> Option<CompiledBindFieldMatch<'p>> {
	if is_schema_dynamic_key_marker(key) {
		return None;
	}
	let matches = completion_rule_fields(parent)
		.into_iter()
		.filter(|field| schema_conditions_match(&field.conditions, active_subtypes))
		.filter_map(|field| {
			let (head, payload) = parse_schema_marker(&field.key)?;
			(head == "alias_name").then_some((field, CompiledAliasCategory::from_name(payload)))
		})
		.flat_map(|(field, category)| {
			engine
				.aliases()
				.iter()
				.filter(move |alias| alias.category == category)
				.filter(move |alias| {
					schema_dynamic_alias_matches(engine, dynamic_values, alias, key)
				})
				.map(move |alias| CompiledBindFieldMatch::Alias {
					wildcard: field,
					alias,
				})
		})
		.collect::<Vec<_>>();
	match matches.as_slice() {
		[alias_match] => Some(*alias_match),
		_ => None,
	}
}

fn schema_dynamic_key_field_match<'p>(
	engine: &'p CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	parent: RuleContext<'p>,
	key: &str,
	active_subtypes: &HashSet<String>,
) -> Option<CompiledBindFieldMatch<'p>> {
	if is_schema_dynamic_key_marker(key) {
		return None;
	}
	let matches = completion_rule_fields(parent)
		.into_iter()
		.filter(|field| schema_conditions_match(&field.conditions, active_subtypes))
		.filter(|field| schema_dynamic_key_field_matches(engine, dynamic_values, field, key))
		.collect::<Vec<_>>();
	match matches.as_slice() {
		[field] => Some(CompiledBindFieldMatch::Field(field)),
		_ => None,
	}
}

fn schema_dynamic_key_field_matches(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	field: &CompiledRuleField,
	key: &str,
) -> bool {
	let Some((head, name)) = parse_schema_marker(&field.key) else {
		return false;
	};
	if head == "alias_name" {
		return false;
	}
	schema_allowed_values_for_marker(engine, dynamic_values, head, name)
		.is_some_and(|values| values.values.iter().any(|value| value == key))
}

fn schema_dynamic_alias_matches(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	alias: &CompiledAlias,
	key: &str,
) -> bool {
	schema_dynamic_alias_values(engine, dynamic_values, alias)
		.is_some_and(|values| values.values.iter().any(|value| value == key))
}

fn schema_conditions_match(
	conditions: &[CompiledRuleCondition],
	active_subtypes: &HashSet<String>,
) -> bool {
	conditions.iter().all(|condition| match condition {
		CompiledRuleCondition::SubtypeActive(label) => active_subtypes.contains(label),
		CompiledRuleCondition::SubtypeInactive(label) => !active_subtypes.contains(label),
	})
}

fn schema_active_subtypes_for_path(
	engine: &CwtQuery,
	file_path: &Path,
	statements: &[AstStatement],
	path: &[&str],
) -> HashSet<String> {
	let mut active = HashSet::new();
	if let Some(root_key) = path.first()
		&& let Some(RuleContext::Subtype(_, subtype)) = engine.bind_context(file_path, &[*root_key])
	{
		active.insert(subtype.name.clone());
	}
	let Some(root) = engine.bind_root(file_path) else {
		return active;
	};
	let Some(root_statements) = schema_root_block_statements(statements, path) else {
		return active;
	};
	for subtype in &root.subtypes {
		if !subtype.rules.is_empty()
			&& schema_rule_fields_match_ast(&subtype.rules, root_statements)
		{
			active.insert(subtype.name.clone());
		}
	}
	active
}

fn schema_root_block_statements<'a>(
	statements: &'a [AstStatement],
	path: &[&str],
) -> Option<&'a [AstStatement]> {
	let Some(root_key) = path.first() else {
		return Some(statements);
	};
	statements.iter().find_map(|statement| {
		let AstStatement::Assignment { key, value, .. } = statement else {
			return None;
		};
		if key != root_key {
			return None;
		}
		let AstValue::Block { items, .. } = value else {
			return None;
		};
		Some(items.as_slice())
	})
}

fn schema_rule_fields_match_ast(rules: &[CompiledRuleField], statements: &[AstStatement]) -> bool {
	rules.iter().all(|rule| {
		statements
			.iter()
			.any(|statement| schema_rule_field_matches_ast(rule, statement))
	})
}

fn schema_rule_field_matches_ast(rule: &CompiledRuleField, statement: &AstStatement) -> bool {
	let AstStatement::Assignment { key, value, .. } = statement else {
		return false;
	};
	key == &rule.key && schema_rule_value_matches_ast(&rule.value, value)
}

fn schema_rule_value_matches_ast(rule_value: &CompiledRuleValue, ast_value: &AstValue) -> bool {
	match (rule_value, ast_value) {
		(
			CompiledRuleValue::Scalar(expected) | CompiledRuleValue::Marker(expected),
			AstValue::Scalar { value, .. },
		) => value.as_text() == *expected,
		(CompiledRuleValue::Block(rules), AstValue::Block { items, .. }) => {
			rules.is_empty() || schema_rule_fields_match_ast(rules, items)
		}
		_ => false,
	}
}

fn direct_schema_completion_entry(
	field: &CompiledRuleField,
	prefix_lower: &str,
) -> Option<SchemaCompletion> {
	if is_schema_dynamic_key_marker(&field.key) {
		return None;
	}
	let field_key_lower = field.key.to_ascii_lowercase();
	if !prefix_lower.is_empty() && !field_key_lower.starts_with(prefix_lower) {
		return None;
	}
	Some(SchemaCompletion {
		label: field.key.clone(),
		insert_text: field.key.clone(),
		kind: SchemaCompletionKind::Field,
		detail: schema_completion_detail(field.attributes.description.as_deref()),
	})
}

fn schema_completion_detail(description: Option<&str>) -> String {
	description
		.and_then(|text| text.lines().next())
		.map(|line| format!("cwt: {line}"))
		.unwrap_or_else(|| "cwt schema field".to_string())
}

fn parse_schema_marker(text: &str) -> Option<(&str, &str)> {
	let (head, rest) = text.split_once('[')?;
	Some((head, rest.strip_suffix(']')?))
}

fn is_schema_dynamic_key_marker(key: &str) -> bool {
	is_schema_angle_dynamic_key_marker(key)
		|| matches!(
			parse_schema_marker(key),
			Some(("enum" | "value" | "value_set" | "scope", _))
		)
}

fn is_schema_angle_dynamic_key_marker(key: &str) -> bool {
	key.len() > 2
		&& key.starts_with('<')
		&& key.ends_with('>')
		&& !key.chars().any(char::is_whitespace)
}

pub(super) fn schema_diagnostics_for_text(
	engine: &CwtQuery,
	file_path: &Path,
	text: &str,
) -> Vec<SchemaDiagnostic> {
	schema_diagnostics_for_text_with_index(engine, file_path, text, None)
}

pub(super) fn schema_diagnostics_for_text_with_index(
	engine: &CwtQuery,
	file_path: &Path,
	text: &str,
	dynamic_values: Option<&SchemaWorkspace>,
) -> Vec<SchemaDiagnostic> {
	let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
	schema_diagnostics_for_ast_with_index(engine, file_path, &parsed.ast.statements, dynamic_values)
}

fn schema_diagnostics_for_ast_with_index(
	engine: &CwtQuery,
	file_path: &Path,
	statements: &[AstStatement],
	dynamic_values: Option<&SchemaWorkspace>,
) -> Vec<SchemaDiagnostic> {
	let mut diagnostics = Vec::new();
	let context = SchemaDiagnosticContext {
		engine,
		dynamic_values,
		file_path,
		document_statements: statements,
	};
	collect_schema_diagnostics(&context, statements, &[], None, &mut diagnostics);
	sort_and_dedup_diagnostics(&mut diagnostics);
	diagnostics
}

pub(super) fn schema_localisation_diagnostics_for_text(
	engine: &CwtQuery,
	file_path: &Path,
	text: &str,
	definitions: &[LocalisationDefinition],
) -> Vec<SchemaDiagnostic> {
	let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
	schema_localisation_diagnostics_for_ast(engine, file_path, &parsed.ast.statements, definitions)
}

fn schema_localisation_diagnostics_for_ast(
	engine: &CwtQuery,
	file_path: &Path,
	statements: &[AstStatement],
	definitions: &[LocalisationDefinition],
) -> Vec<SchemaDiagnostic> {
	let defined_keys = definitions
		.iter()
		.map(|definition| definition.key.as_str())
		.collect::<HashSet<_>>();
	let mut diagnostics = schema_localisation_references_for_ast(engine, file_path, statements)
		.into_iter()
		.filter(|reference| !defined_keys.contains(reference.key.as_str()))
		.map(|reference| schema_missing_localisation_diagnostic(reference.range, &reference.key))
		.collect::<Vec<_>>();
	sort_and_dedup_diagnostics(&mut diagnostics);
	diagnostics
}

#[derive(Clone, Debug)]
struct SchemaLocalisationReference {
	key: String,
	range: EditorRange,
}

#[derive(Clone, Debug)]
struct SchemaRootInstance<'a> {
	path: Vec<String>,
	items: &'a [AstStatement],
	identity: String,
	identity_range: EditorRange,
}

fn schema_localisation_references_for_ast(
	engine: &CwtQuery,
	file_path: &Path,
	statements: &[AstStatement],
) -> Vec<SchemaLocalisationReference> {
	let Some(root) = engine.bind_root(file_path) else {
		return Vec::new();
	};
	if root.localisation.is_empty() {
		return Vec::new();
	}
	let instances = schema_root_instances(root, statements, file_path);
	let mut references = Vec::new();
	for instance in instances {
		let path = instance.path.iter().map(String::as_str).collect::<Vec<_>>();
		let active_subtypes =
			schema_active_subtypes_for_instance(engine, file_path, root, instance.items, &path);
		for rule in &root.localisation {
			if !schema_conditions_match(&rule.conditions, &active_subtypes) {
				continue;
			}
			let Some(pattern) = schema_rule_scalar_pattern(&rule.value) else {
				continue;
			};
			if pattern.contains('$') {
				let key = pattern.replace('$', &instance.identity);
				if !key.is_empty() {
					references.push(SchemaLocalisationReference {
						key,
						range: instance.identity_range,
					});
				}
				continue;
			}
			if let Some((key, range)) = scalar_assignment_in_statements(instance.items, pattern) {
				references.push(SchemaLocalisationReference { key, range });
			}
		}
	}
	references.sort_by(|left, right| {
		left.key
			.cmp(&right.key)
			.then_with(|| range_start(&left.range).cmp(&range_start(&right.range)))
	});
	references.dedup_by(|left, right| left.key == right.key && left.range == right.range);
	references
}

fn schema_rule_scalar_pattern(value: &CompiledRuleValue) -> Option<&str> {
	match value {
		CompiledRuleValue::Scalar(value) | CompiledRuleValue::Marker(value) => Some(value),
		CompiledRuleValue::Block(_) => None,
	}
}

fn schema_root_instances<'a>(
	root: &CompiledRoot,
	statements: &'a [AstStatement],
	file_path: &Path,
) -> Vec<SchemaRootInstance<'a>> {
	let mut instances = Vec::new();
	collect_schema_root_instances(
		root,
		statements,
		&root.skip_root_keys,
		Vec::new(),
		file_path,
		&mut instances,
	);
	instances
}

fn collect_schema_root_instances<'a>(
	root: &CompiledRoot,
	statements: &'a [AstStatement],
	remaining_skip_keys: &[String],
	path: Vec<String>,
	file_path: &Path,
	out: &mut Vec<SchemaRootInstance<'a>>,
) {
	if let Some((skip_key, rest)) = remaining_skip_keys.split_first() {
		for statement in statements {
			let AstStatement::Assignment { key, value, .. } = statement else {
				continue;
			};
			if skip_key != "any" && key != skip_key {
				continue;
			}
			let AstValue::Block { items, .. } = value else {
				continue;
			};
			let mut child_path = path.clone();
			child_path.push(key.clone());
			collect_schema_root_instances(root, items, rest, child_path, file_path, out);
		}
		return;
	}

	for statement in statements {
		let AstStatement::Assignment {
			key,
			key_span,
			value,
			..
		} = statement
		else {
			continue;
		};
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		let mut instance_path = path.clone();
		instance_path.push(key.clone());
		let (identity, identity_range) =
			schema_root_instance_identity(root, key, key_span, items, file_path);
		out.push(SchemaRootInstance {
			path: instance_path,
			items,
			identity,
			identity_range,
		});
	}
}

fn schema_root_instance_identity(
	root: &CompiledRoot,
	key: &str,
	key_span: &SpanRange,
	items: &[AstStatement],
	file_path: &Path,
) -> (String, EditorRange) {
	if let Some(name_field) = root.name_field.as_deref()
		&& let Some((value, range)) = scalar_assignment_in_statements(items, name_field)
	{
		return (value, range);
	}
	if root.name_from_file
		&& let Some(stem) = file_path.file_stem().and_then(|value| value.to_str())
	{
		return (stem.to_string(), editor_range_from_span(key_span));
	}
	(key.to_string(), editor_range_from_span(key_span))
}

fn scalar_assignment_in_statements(
	statements: &[AstStatement],
	target_key: &str,
) -> Option<(String, EditorRange)> {
	statements.iter().find_map(|statement| {
		let AstStatement::Assignment { key, value, .. } = statement else {
			return None;
		};
		if key != target_key {
			return None;
		}
		let AstValue::Scalar { value, span } = value else {
			return None;
		};
		Some((value.as_text(), editor_range_from_span(span)))
	})
}

fn schema_active_subtypes_for_instance(
	engine: &CwtQuery,
	file_path: &Path,
	root: &CompiledRoot,
	statements: &[AstStatement],
	path: &[&str],
) -> HashSet<String> {
	let mut active = HashSet::new();
	if let Some(RuleContext::Subtype(_, subtype)) = engine.bind_context(file_path, path) {
		active.insert(subtype.name.clone());
	}
	for subtype in &root.subtypes {
		if !subtype.rules.is_empty() && schema_rule_fields_match_ast(&subtype.rules, statements) {
			active.insert(subtype.name.clone());
		}
	}
	active
}

fn schema_missing_localisation_diagnostic(range: EditorRange, key: &str) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range,
		severity: Some(Severity::Warning),
		code: Some("missing-localisation".to_string()),
		source: Some("foch".to_string()),
		message: format!("localisation key not found: {key}"),
	}
}

struct SchemaDiagnosticContext<'a> {
	engine: &'a CwtQuery,
	dynamic_values: Option<&'a SchemaWorkspace>,
	file_path: &'a Path,
	document_statements: &'a [AstStatement],
}

fn collect_schema_diagnostics(
	context: &SchemaDiagnosticContext<'_>,
	statements: &[AstStatement],
	parent_path: &[String],
	parent_range: Option<EditorRange>,
	diagnostics: &mut Vec<SchemaDiagnostic>,
) {
	let context_path = parent_path.iter().map(String::as_str).collect::<Vec<_>>();
	let parent_context = schema_bind_context(
		context.engine,
		context.dynamic_values,
		context.file_path,
		context.document_statements,
		&context_path,
	);
	let skip_unknown = matches!(parent_context, Some(RuleContext::AliasRules(_)));
	let active_scopes = schema_active_scopes_for_path(
		context.engine,
		context.dynamic_values,
		context.file_path,
		context.document_statements,
		&context_path,
	);
	let active_subtypes = schema_active_subtypes_for_path(
		context.engine,
		context.file_path,
		context.document_statements,
		&context_path,
	);
	let mut cardinality_ranges = HashMap::<String, (u32, Severity, Vec<EditorRange>)>::new();
	let mut present_key_counts = HashMap::<String, u32>::new();
	for statement in statements {
		match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value,
				..
			} => {
				let key_range = editor_range_from_span(key_span);
				let field_match = parent_context.and_then(|rule_context| {
					schema_bind_field_match(
						context.engine,
						context.dynamic_values,
						rule_context,
						key,
						&active_subtypes,
					)
				});
				if parent_context.is_some() && field_match.is_none() && !skip_unknown {
					diagnostics.push(schema_unknown_key_diagnostic(key_range, key));
				}
				if let Some(field_match) = field_match
					&& let Some(diagnostic) = schema_alias_scope_diagnostic(
						context.engine,
						&field_match,
						key_range,
						key,
						&active_scopes,
					) {
					diagnostics.push(diagnostic);
				}
				if let Some(field_match) = field_match
					&& let Some(diagnostic) =
						schema_value_shape_diagnostic(&field_match, key, value)
				{
					diagnostics.push(diagnostic);
				}
				if let Some(field_match) = field_match
					&& let AstValue::Scalar {
						value: scalar,
						span,
					} = value
				{
					if let Some(diagnostic) = schema_invalid_value_diagnostic(
						context.engine,
						context.dynamic_values,
						&field_match,
						key,
						scalar,
						span,
					) {
						diagnostics.push(diagnostic);
					}
					if let Some(diagnostic) =
						schema_scalar_type_diagnostic(&field_match, key, scalar, span)
					{
						diagnostics.push(diagnostic);
					}
				}
				if let Some(field_match) = field_match {
					*present_key_counts
						.entry(field_match.field().key.clone())
						.or_default() += 1;
				}
				if let Some(field_match) = field_match
					&& let Some(upper_bound) = schema_cardinality_upper(&field_match)
				{
					let severity =
						schema_match_diagnostic_severity(&field_match, Severity::Warning);
					let entry = cardinality_ranges
						.entry(key.clone())
						.or_insert_with(|| (upper_bound, severity, Vec::new()));
					if upper_bound > entry.0 {
						entry.0 = upper_bound;
						entry.1 = severity;
					}
					entry.2.push(key_range);
				}
				if let AstValue::Block {
					items,
					span: block_span,
				} = value
				{
					let mut child_path = parent_path.to_vec();
					child_path.push(key.clone());
					collect_schema_diagnostics(
						context,
						items,
						&child_path,
						Some(editor_range_from_span(block_span)),
						diagnostics,
					);
				}
			}
			AstStatement::Item {
				value: AstValue::Block {
					items,
					span: block_span,
				},
				..
			} => collect_schema_diagnostics(
				context,
				items,
				parent_path,
				Some(editor_range_from_span(block_span)),
				diagnostics,
			),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => {}
		}
	}
	for (key, (upper_bound, severity, ranges)) in cardinality_ranges {
		if ranges.len() <= upper_bound as usize {
			continue;
		}
		for range in ranges.into_iter().skip(upper_bound as usize) {
			diagnostics.push(schema_cardinality_diagnostic(
				range,
				&key,
				upper_bound,
				severity,
			));
		}
	}
	if let Some(parent_context) = parent_context
		&& !skip_unknown
	{
		let range = schema_context_diagnostic_range(parent_range, statements);
		for (field, minimum) in schema_required_fields(parent_context, &active_subtypes) {
			let present = present_key_counts
				.get(&field.key)
				.copied()
				.unwrap_or_default();
			if present < minimum {
				let severity =
					schema_attributes_diagnostic_severity(&field.attributes, Severity::Error);
				diagnostics.push(schema_required_key_diagnostic(
					range, &field.key, minimum, severity,
				));
			}
		}
	}
}

fn schema_cardinality_upper(field_match: &CompiledBindFieldMatch<'_>) -> Option<u32> {
	schema_match_cardinality(field_match).and_then(|(_, upper_bound)| upper_bound)
}

fn schema_match_cardinality(
	field_match: &CompiledBindFieldMatch<'_>,
) -> Option<(u32, Option<u32>)> {
	field_match
		.field()
		.attributes
		.cardinality
		.or_else(|| schema_required_cardinality(&field_match.field().attributes))
		.or_else(|| {
			field_match
				.alias()
				.and_then(|alias| schema_field_attributes_cardinality(&alias.attributes))
		})
}

fn schema_required_fields<'p>(
	context: RuleContext<'p>,
	active_subtypes: &HashSet<String>,
) -> Vec<(&'p CompiledRuleField, u32)> {
	let mut fields = completion_rule_fields(context)
		.into_iter()
		.filter(|field| schema_conditions_match(&field.conditions, active_subtypes))
		.filter(|field| parse_schema_marker(&field.key).is_none())
		.filter(|field| !is_schema_dynamic_key_marker(&field.key))
		.filter_map(|field| {
			schema_field_attributes_cardinality(&field.attributes)
				.map(|(minimum, _)| (field, minimum))
		})
		.filter(|(_, minimum)| *minimum > 0)
		.collect::<Vec<_>>();
	fields.sort_by(|(left, _), (right, _)| left.key.cmp(&right.key));
	fields.dedup_by(|(left, _), (right, _)| left.key == right.key);
	fields
}

fn schema_field_attributes_cardinality(
	attributes: &CompiledFieldAttributes,
) -> Option<(u32, Option<u32>)> {
	attributes
		.cardinality
		.or_else(|| schema_required_cardinality(attributes))
}

fn schema_required_cardinality(attributes: &CompiledFieldAttributes) -> Option<(u32, Option<u32>)> {
	attributes
		.raw
		.iter()
		.any(|(key, value)| key == "required" && value.is_empty())
		.then_some((1, None))
}

fn schema_match_diagnostic_severity(
	field_match: &CompiledBindFieldMatch<'_>,
	default: Severity,
) -> Severity {
	field_match
		.alias()
		.and_then(|alias| alias.attributes.severity)
		.or(field_match.field().attributes.severity)
		.map(cwt_diagnostic_severity)
		.unwrap_or(default)
}

fn schema_attributes_diagnostic_severity(
	attributes: &CompiledFieldAttributes,
	default: Severity,
) -> Severity {
	attributes
		.severity
		.map(cwt_diagnostic_severity)
		.unwrap_or(default)
}

fn cwt_diagnostic_severity(severity: CompiledSeverity) -> Severity {
	match severity {
		CompiledSeverity::Error => Severity::Error,
		CompiledSeverity::Warning => Severity::Warning,
		CompiledSeverity::Info => Severity::Info,
	}
}

fn schema_context_diagnostic_range(
	parent_range: Option<EditorRange>,
	statements: &[AstStatement],
) -> EditorRange {
	parent_range.unwrap_or_else(|| {
		statements
			.first()
			.map(ast_statement_range)
			.unwrap_or_else(zero_range)
	})
}

fn ast_statement_range(statement: &AstStatement) -> EditorRange {
	match statement {
		AstStatement::Assignment { span, .. }
		| AstStatement::Item { span, .. }
		| AstStatement::Comment { span, .. } => editor_range_from_span(span),
	}
}

fn zero_range() -> EditorRange {
	EditorRange {
		start: EditorPosition {
			line: 0,
			character: 0,
		},
		end: EditorPosition {
			line: 0,
			character: 0,
		},
	}
}

fn schema_invalid_value_diagnostic(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	field_match: &CompiledBindFieldMatch<'_>,
	key: &str,
	scalar: &ScalarValue,
	span: &SpanRange,
) -> Option<SchemaDiagnostic> {
	let values = schema_allowed_values(engine, dynamic_values, schema_match_value(field_match))?;
	let text = scalar.as_text();
	if values.values.iter().any(|value| value == &text) {
		return None;
	}
	Some(SchemaDiagnostic {
		range: editor_range_from_span(span),
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			Severity::Error,
		)),
		code: Some("V003".to_string()),
		source: Some("foch".to_string()),
		message: format!(
			"value `{text}` for `{key}` is not in schema {} `{}` (allowed: {})",
			values.kind.label(),
			values.name,
			format_allowed_values(&values.values)
		),
	})
}

fn schema_scalar_type_diagnostic(
	field_match: &CompiledBindFieldMatch<'_>,
	key: &str,
	scalar: &ScalarValue,
	span: &SpanRange,
) -> Option<SchemaDiagnostic> {
	let expected = schema_scalar_type(schema_match_value(field_match))?;
	if expected.matches(scalar) {
		return None;
	}
	Some(SchemaDiagnostic {
		range: editor_range_from_span(span),
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			Severity::Error,
		)),
		code: Some("V005".to_string()),
		source: Some("foch".to_string()),
		message: format!(
			"value `{}` for `{key}` does not match schema type `{}`",
			scalar.as_text(),
			expected.label()
		),
	})
}

fn schema_value_shape_diagnostic(
	field_match: &CompiledBindFieldMatch<'_>,
	key: &str,
	value: &AstValue,
) -> Option<SchemaDiagnostic> {
	let expected = schema_value_shape(schema_match_value(field_match));
	let actual = SchemaValueShape::from_ast_value(value);
	if expected == actual {
		return None;
	}
	Some(SchemaDiagnostic {
		range: editor_range_from_span(value.span()),
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			Severity::Error,
		)),
		code: Some("V006".to_string()),
		source: Some("foch".to_string()),
		message: format!(
			"value for `{key}` is a schema {}, but this assignment uses a {}",
			expected.label(),
			actual.label()
		),
	})
}

fn schema_alias_scope_diagnostic(
	engine: &CwtQuery,
	field_match: &CompiledBindFieldMatch<'_>,
	range: EditorRange,
	key: &str,
	active_scopes: &[String],
) -> Option<SchemaDiagnostic> {
	let alias = field_match.alias()?;
	if schema_alias_scope_matches(engine, alias, active_scopes) {
		return None;
	}
	Some(SchemaDiagnostic {
		range,
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			Severity::Error,
		)),
		code: Some("V007".to_string()),
		source: Some("foch".to_string()),
		message: format!(
			"alias `{key}` is scoped to {}, but current schema scope is {}",
			format_scope_values(&alias.attributes.scope),
			format_scope_refs(active_scopes)
		),
	})
}

fn schema_scalar_type(value: &CompiledRuleValue) -> Option<SchemaScalarType> {
	let value = match value {
		CompiledRuleValue::Scalar(value) | CompiledRuleValue::Marker(value) => value.as_str(),
		CompiledRuleValue::Block(_) => return None,
	};
	match value {
		"int" => Some(SchemaScalarType::Int { range: None }),
		"float" => Some(SchemaScalarType::Float { range: None }),
		"bool" => Some(SchemaScalarType::Bool),
		_ => match parse_schema_marker(value) {
			Some(("int", range)) => parse_schema_int_range(range)
				.map(|range| SchemaScalarType::Int { range: Some(range) }),
			Some(("float", range)) => parse_schema_float_range(range)
				.map(|range| SchemaScalarType::Float { range: Some(range) }),
			_ => None,
		},
	}
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SchemaScalarType {
	Int { range: Option<SchemaIntRange> },
	Float { range: Option<SchemaFloatRange> },
	Bool,
}

impl SchemaScalarType {
	fn label(&self) -> String {
		match self {
			Self::Int { range: None } => "int".to_string(),
			Self::Int { range: Some(range) } => range.label("int"),
			Self::Float { range: None } => "float".to_string(),
			Self::Float { range: Some(range) } => range.label("float"),
			Self::Bool => "bool".to_string(),
		}
	}

	fn matches(&self, scalar: &ScalarValue) -> bool {
		match self {
			Self::Int { range } => match scalar {
				ScalarValue::Number(value) => value
					.parse::<i64>()
					.is_ok_and(|number| range.is_none_or(|range| range.contains(number))),
				_ => false,
			},
			Self::Float { range } => match scalar {
				ScalarValue::Number(value) => value
					.parse::<f64>()
					.is_ok_and(|number| range.is_none_or(|range| range.contains(number))),
				_ => false,
			},
			Self::Bool => matches!(scalar, ScalarValue::Bool(_)),
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SchemaIntRange {
	minimum: i64,
	maximum: Option<i64>,
}

impl SchemaIntRange {
	fn contains(self, value: i64) -> bool {
		value >= self.minimum && self.maximum.is_none_or(|maximum| value <= maximum)
	}

	fn label(self, kind: &str) -> String {
		format!(
			"{kind}[{}..{}]",
			self.minimum,
			self.maximum
				.map(|value| value.to_string())
				.unwrap_or_else(|| "inf".to_string())
		)
	}
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SchemaFloatRange {
	minimum: f64,
	maximum: Option<f64>,
}

impl SchemaFloatRange {
	fn contains(self, value: f64) -> bool {
		value >= self.minimum && self.maximum.is_none_or(|maximum| value <= maximum)
	}

	fn label(self, kind: &str) -> String {
		format!(
			"{kind}[{}..{}]",
			self.minimum,
			self.maximum
				.map(|value| value.to_string())
				.unwrap_or_else(|| "inf".to_string())
		)
	}
}

fn parse_schema_int_range(value: &str) -> Option<SchemaIntRange> {
	let (minimum, maximum) = value.split_once("..")?;
	let minimum = minimum.trim().parse::<i64>().ok()?;
	let maximum = match maximum.trim() {
		"inf" => None,
		value => Some(value.parse::<i64>().ok()?),
	};
	Some(SchemaIntRange { minimum, maximum })
}

fn parse_schema_float_range(value: &str) -> Option<SchemaFloatRange> {
	let (minimum, maximum) = value.split_once("..")?;
	let minimum = minimum.trim().parse::<f64>().ok()?;
	let maximum = match maximum.trim() {
		"inf" => None,
		value => Some(value.parse::<f64>().ok()?),
	};
	Some(SchemaFloatRange { minimum, maximum })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchemaValueShape {
	Scalar,
	Block,
}

impl SchemaValueShape {
	fn from_ast_value(value: &AstValue) -> Self {
		match value {
			AstValue::Scalar { .. } => Self::Scalar,
			AstValue::Block { .. } => Self::Block,
		}
	}

	fn label(self) -> &'static str {
		match self {
			Self::Scalar => "scalar",
			Self::Block => "block",
		}
	}
}

fn schema_value_shape(value: &CompiledRuleValue) -> SchemaValueShape {
	match value {
		CompiledRuleValue::Scalar(_) | CompiledRuleValue::Marker(_) => SchemaValueShape::Scalar,
		CompiledRuleValue::Block(_) => SchemaValueShape::Block,
	}
}

fn schema_match_value<'p>(field_match: &CompiledBindFieldMatch<'p>) -> &'p CompiledRuleValue {
	field_match
		.alias()
		.map(|alias| &alias.value)
		.unwrap_or_else(|| &field_match.field().value)
}

fn schema_allowed_values<'a>(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	value: &'a CompiledRuleValue,
) -> Option<SchemaAllowedValues<'a>> {
	let value = match value {
		CompiledRuleValue::Scalar(value) | CompiledRuleValue::Marker(value) => value,
		CompiledRuleValue::Block(_) => return None,
	};
	let (head, name) = schema_allowed_value_marker(value)?;
	schema_allowed_values_for_marker(engine, dynamic_values, head, name)
}

fn schema_dynamic_alias_values<'a>(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	alias: &'a CompiledAlias,
) -> Option<SchemaAllowedValues<'a>> {
	let (head, name) = schema_allowed_value_marker(&alias.name)?;
	schema_allowed_values_for_marker(engine, dynamic_values, head, name)
}

fn schema_allowed_values_for_marker<'a>(
	engine: &CwtQuery,
	dynamic_values: Option<&SchemaWorkspace>,
	head: &'a str,
	name: &'a str,
) -> Option<SchemaAllowedValues<'a>> {
	let (kind, values) = match head {
		"enum" => {
			if let Some(values) = engine.enum_values(name) {
				(SchemaAllowedValueKind::Enum, values.to_vec())
			} else if let Some(complex_enum) = engine.complex_enum(name) {
				(
					SchemaAllowedValueKind::ComplexEnum,
					dynamic_values?
						.complex_enums
						.get(&complex_enum.name)
						.cloned()?,
				)
			} else {
				return None;
			}
		}
		"value" => (
			SchemaAllowedValueKind::Value,
			engine.value_set_values(name)?.to_vec(),
		),
		"value_set" => (
			SchemaAllowedValueKind::ValueSet,
			engine.value_set_values(name)?.to_vec(),
		),
		"scope" => (SchemaAllowedValueKind::Scope, engine.scope_values(name)),
		_ => return None,
	};
	(!values.is_empty()).then_some(SchemaAllowedValues { kind, name, values })
}

fn schema_allowed_value_marker(text: &str) -> Option<(&str, &str)> {
	match parse_schema_marker(text) {
		Some(("enum", name)) => Some(("enum", name)),
		Some(("value", name)) => Some(("value", name)),
		Some(("value_set", name)) => Some(("value_set", name)),
		Some(("scope", name)) => Some(("scope", name)),
		_ => None,
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SchemaAllowedValues<'a> {
	kind: SchemaAllowedValueKind,
	name: &'a str,
	values: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchemaAllowedValueKind {
	Enum,
	ComplexEnum,
	Value,
	ValueSet,
	Scope,
}

impl SchemaAllowedValueKind {
	fn label(self) -> &'static str {
		match self {
			Self::Enum => "enum",
			Self::ComplexEnum => "complex_enum",
			Self::Value => "value",
			Self::ValueSet => "value_set",
			Self::Scope => "scope",
		}
	}

	fn completion_kind(self) -> SchemaCompletionKind {
		match self {
			Self::Enum => SchemaCompletionKind::EnumMember,
			Self::ComplexEnum => SchemaCompletionKind::EnumMember,
			Self::Value => SchemaCompletionKind::Value,
			Self::ValueSet => SchemaCompletionKind::Value,
			Self::Scope => SchemaCompletionKind::Reference,
		}
	}
}

fn format_allowed_values(values: &[String]) -> String {
	const MAX_VALUES: usize = 12;
	let mut formatted = values
		.iter()
		.take(MAX_VALUES)
		.map(|value| format!("`{value}`"))
		.collect::<Vec<_>>();
	if values.len() > MAX_VALUES {
		formatted.push(format!("... {} more", values.len() - MAX_VALUES));
	}
	formatted.join(", ")
}

fn schema_unknown_key_diagnostic(range: EditorRange, key: &str) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range,
		severity: Some(Severity::Warning),
		code: Some("V001".to_string()),
		source: Some("foch".to_string()),
		message: format!("schema does not allow key `{key}` in this context"),
	}
}

fn schema_cardinality_diagnostic(
	range: EditorRange,
	key: &str,
	upper_bound: u32,
	severity: Severity,
) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range,
		severity: Some(severity),
		code: Some("V002".to_string()),
		source: Some("foch".to_string()),
		message: format!("key `{key}` exceeds schema cardinality upper bound of {upper_bound}"),
	}
}

fn schema_required_key_diagnostic(
	range: EditorRange,
	key: &str,
	minimum: u32,
	severity: Severity,
) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range,
		severity: Some(severity),
		code: Some("V004".to_string()),
		source: Some("foch".to_string()),
		message: format!("schema requires key `{key}` at least {minimum} time(s) in this context"),
	}
}

fn current_assignment_key(line_prefix: &str) -> Option<&str> {
	let equals = line_prefix.rfind('=')?;
	let before = line_prefix[..equals].trim_end();
	if before.is_empty() {
		return None;
	}
	let mut start = before.len();
	let bytes = before.as_bytes();
	while start > 0 {
		let character = bytes[start - 1] as char;
		if is_identifier_char(character) {
			start -= 1;
		} else {
			break;
		}
	}
	(start != before.len()).then_some(&before[start..])
}

fn is_identifier_char(character: char) -> bool {
	character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | ':' | '$' | '@' | '-')
}

fn sort_and_dedup_diagnostics(diagnostics: &mut Vec<SchemaDiagnostic>) {
	diagnostics.sort_by(|left, right| {
		range_start(&left.range)
			.cmp(&range_start(&right.range))
			.then_with(|| left.message.cmp(&right.message))
	});
	diagnostics.dedup_by(|left, right| {
		left.range == right.range && left.code == right.code && left.message == right.message
	});
}

fn range_start(range: &EditorRange) -> (u32, u32) {
	(range.start.line, range.start.character)
}
