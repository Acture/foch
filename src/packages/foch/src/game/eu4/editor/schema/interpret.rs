use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::game::eu4::coercion::{alias_reads_three_decimals, alias_value_reads_three_decimals};
use crate::game::eu4::cwt::{Eu4Dialect, is_dynamic_country_tag};
use crate::game::eu4::editor::position::{byte_offset, range_from_span};
use crate::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, ScriptSyntax, SpanRange, parse_clausewitz_statements,
	unquoted_scalar,
};
use crate::game::schema::bind::{
	BoundContext, DefinedNames, SchemaAllowedValueKind, SchemaAllowedValues, SchemaBinder,
	SchemaScalar, SchemaValue, SchemaValueShape, allowed_value_marker, conditions_match,
	definition_key, is_dynamic_key_marker, rule_fields, rule_value_shape,
};
use crate::game::schema::query::{
	CompiledAlias, CompiledAliasCategory, CompiledBindFieldMatch, CompiledFieldAttributes,
	CompiledRoot, CompiledRuleField, CompiledRuleOrigin, CompiledRuleValue, CompiledSeverity,
	CwtQuery, RuleContext, SchemaScalarType, parse_schema_marker,
};
use crate::merge::semantic_fingerprint::value_fingerprint;
use crate::model::{GamePath, LocalisationDefinition, Severity};

use super::{
	EditorPosition, EditorRange, SchemaCompletion, SchemaCompletionKind, SchemaDiagnostic,
	SchemaHover, SchemaWorkspace,
};

/// The source of a diagnostic a vendored CWTools rule decides, crediting those
/// rules.
const CWTOOLS_SOURCE: &str = "cwtools";
/// The source of a diagnostic foch's extension of those rules decides: a rule
/// from foch's supplement, or foch's own refinement of how a rule applies.
const FOCH_CWTOOLS_SOURCE: &str = "foch-cwtools";
/// The source of a diagnostic from foch's own finding of how the game reads a
/// file, independent of any rule.
const FOCH_DETECT_SOURCE: &str = "foch-detect";

fn rule_source(origin: CompiledRuleOrigin) -> &'static str {
	match origin {
		CompiledRuleOrigin::Base => CWTOOLS_SOURCE,
		CompiledRuleOrigin::Extension => FOCH_CWTOOLS_SOURCE,
	}
}

/// The binder of one file, with the workspace's names when there is one.
fn schema_binder<'a>(
	engine: &'a CwtQuery,
	file_path: &'a GamePath,
	dynamic_values: Option<&'a SchemaWorkspace>,
) -> SchemaBinder<'a> {
	SchemaBinder::new(
		engine,
		&Eu4Dialect,
		dynamic_values.map(|workspace| workspace as &dyn DefinedNames),
		file_path,
	)
}

pub(super) fn schema_hover(
	engine: &CwtQuery,
	file_path: &GamePath,
	text: &str,
	position: EditorPosition,
	dynamic_values: Option<&SchemaWorkspace>,
) -> Option<SchemaHover> {
	let offset = byte_offset(text, position)?;
	let parsed = parse_clausewitz_statements(ScriptSyntax::for_game_path(file_path), text);
	let binder = schema_binder(engine, file_path, dynamic_values);
	let file = binder.file(&parsed.statements);
	let (key, range, field_match) =
		find_hover_binding(&binder, &file, &parsed.statements, text, offset)?;
	Some(SchemaHover {
		markdown: render_schema_hover_markdown(&binder, key, &field_match),
		range,
	})
}

/// The key under `offset`, where it is written, and the first form its
/// block's rules give it.
fn find_hover_binding<'a>(
	binder: &SchemaBinder<'a>,
	context: &BoundContext<'a, AstStatement>,
	statements: &'a [AstStatement],
	text: &str,
	offset: usize,
) -> Option<(&'a str, EditorRange, CompiledBindFieldMatch<'a>)> {
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
		if (key_span.start.offset..key_span.end.offset).contains(&offset) {
			let field_match = binder
				.field_alternatives(context.rules()?, key, context.subtypes())
				.into_iter()
				.next()?;
			return Some((key, range_from_span(text, key_span)?, field_match));
		}
		if let AstValue::Block { items, span } = value
			&& (span.start.offset..span.end.offset).contains(&offset)
		{
			let binding = binder.bind(context, key, value);
			let child = binder.child(context, statement, key, &binding);
			if let Some(found) = find_hover_binding(binder, &child, items, text, offset) {
				return Some(found);
			}
		}
	}
	None
}

fn render_schema_hover_markdown(
	binder: &SchemaBinder<'_>,
	key: &str,
	field_match: &CompiledBindFieldMatch<'_>,
) -> String {
	let value = field_match.value();
	let mut sections = vec![
		format!("**{key}**"),
		format!("Type: `{}`", rule_value_kind(value)),
	];
	if let Some(values) = schema_dynamic_alias_source(binder, key, field_match) {
		sections.push(format!(
			"Dynamic alias: `{}` `{}`",
			values.kind.label(),
			values.name
		));
	}
	if let Some(values) = schema_dynamic_key_source(binder, key, field_match) {
		sections.push(format!(
			"Dynamic key: `{}` `{}`",
			values.kind.label(),
			values.name
		));
	}
	if let Some(values) = binder.allowed_values(value) {
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
	binder: &SchemaBinder<'a>,
	key: &str,
	field_match: &CompiledBindFieldMatch<'a>,
) -> Option<SchemaAllowedValues<'a>> {
	let field = field_match.field();
	if field.key == key {
		return None;
	}
	let (head, name) = parse_schema_marker(&field.key)?;
	if head == "alias_name" {
		return None;
	}
	let values = binder.allowed_values_for_marker(head, name)?;
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
		CompiledRuleValue::Marker(marker) if allowed_value_marker(marker).is_some() => "Scalar",
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
	binder: &SchemaBinder<'a>,
	key: &str,
	field_match: &CompiledBindFieldMatch<'a>,
) -> Option<SchemaAllowedValues<'a>> {
	let alias = field_match.alias()?;
	if alias.name == key {
		return None;
	}
	let values = binder.dynamic_alias_values(alias)?;
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
	file_path: &GamePath,
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
) -> Option<Vec<SchemaCompletion>> {
	schema_completion_candidates_with_index(engine, file_path, text, position, prefix_lower, None)
}

pub(super) fn schema_completion_candidates_with_index(
	engine: &CwtQuery,
	file_path: &GamePath,
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
	dynamic_values: Option<&SchemaWorkspace>,
) -> Option<Vec<SchemaCompletion>> {
	if !is_schema_key_completion_position(text, position) {
		let parsed = parse_clausewitz_statements(ScriptSyntax::for_game_path(file_path), text);
		return schema_value_completion_candidates(
			engine,
			dynamic_values,
			file_path,
			&parsed.statements,
			text,
			position,
			prefix_lower,
		);
	}
	let parsed = parse_clausewitz_statements(ScriptSyntax::for_game_path(file_path), text);
	let binder = schema_binder(engine, file_path, dynamic_values);
	let file = binder.file(&parsed.statements);
	let parent = context_at_position(&binder, &file, &parsed.statements, position);
	let mut candidates = Vec::new();
	for field in rule_fields(parent.rules()?)
		.filter(|field| conditions_match(&field.conditions, parent.subtypes()))
	{
		candidates.extend(schema_completion_entries_for_field(
			&binder,
			field,
			parent.scopes(),
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
	file_path: &GamePath,
	statements: &[AstStatement],
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
) -> Option<Vec<SchemaCompletion>> {
	let line = text.lines().nth(position.line as usize).unwrap_or_default();
	let upto: String = line.chars().take(position.character as usize).collect();
	let key = current_assignment_key(&upto)?;
	let binder = schema_binder(engine, file_path, dynamic_values);
	let file = binder.file(statements);
	let parent = context_at_position(&binder, &file, statements, position);
	let field_match = binder
		.field_alternatives(parent.rules()?, key, parent.subtypes())
		.into_iter()
		.next()?;
	let values = binder.allowed_values(field_match.value())?;
	let kind = allowed_value_completion_kind(values.kind);
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

/// The context of the block that holds `position`.
fn context_at_position<'a>(
	binder: &SchemaBinder<'a>,
	context: &BoundContext<'a, AstStatement>,
	statements: &'a [AstStatement],
	position: EditorPosition,
) -> BoundContext<'a, AstStatement> {
	for statement in statements {
		match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value,
				span,
			} => {
				if span_contains_position(key_span, position) {
					return context.clone();
				}
				if let AstValue::Block {
					items,
					span: block_span,
				} = value && span_contains_position(block_span, position)
				{
					let binding = binder.bind(context, key, value);
					let child = binder.child(context, statement, key, &binding);
					return context_at_position(binder, &child, items, position);
				}
				if span_contains_position(span, position) {
					return context.clone();
				}
			}
			AstStatement::Item { value, span } => {
				if let AstValue::Block {
					items,
					span: block_span,
				} = value && span_contains_position(block_span, position)
				{
					return context_at_position(binder, context, items, position);
				}
				if span_contains_position(span, position) {
					return context.clone();
				}
			}
			AstStatement::Comment { span, .. } => {
				if span_contains_position(span, position) {
					return context.clone();
				}
			}
		}
	}
	context.clone()
}

fn allowed_value_completion_kind(kind: SchemaAllowedValueKind) -> SchemaCompletionKind {
	match kind {
		SchemaAllowedValueKind::Enum | SchemaAllowedValueKind::ComplexEnum => {
			SchemaCompletionKind::EnumMember
		}
		SchemaAllowedValueKind::Value | SchemaAllowedValueKind::ValueSet => {
			SchemaCompletionKind::Value
		}
		SchemaAllowedValueKind::Scope => SchemaCompletionKind::Reference,
	}
}

fn schema_completion_entries_for_field(
	binder: &SchemaBinder<'_>,
	field: &CompiledRuleField,
	active_scopes: &[String],
	prefix_lower: &str,
) -> Vec<SchemaCompletion> {
	let engine = binder.engine;
	let Some((head, payload)) = parse_schema_marker(&field.key) else {
		return direct_schema_completion_entry(field, prefix_lower)
			.into_iter()
			.collect();
	};
	if head != "alias_name" {
		if let Some(values) = binder.allowed_values_for_marker(head, payload) {
			return schema_dynamic_key_completion_entries(&values, prefix_lower);
		}
		return direct_schema_completion_entry(field, prefix_lower)
			.into_iter()
			.collect();
	}
	let category = CompiledAliasCategory::from_name(payload);
	let declared_modifiers = if category == CompiledAliasCategory::Modifier {
		engine.declared_modifiers()
	} else {
		&[]
	};
	let mut candidates = engine
		.aliases()
		.iter()
		.filter(|alias| alias.category == category)
		.chain(declared_modifiers)
		.filter(|alias| binder.alias_scope_matches(alias, active_scopes))
		.flat_map(|alias| schema_completion_entries_for_alias(binder, field, alias, prefix_lower))
		.collect::<Vec<_>>();
	candidates.sort_by(|left, right| left.label.cmp(&right.label));
	// An alias defined once per scope offers its name once.
	candidates.dedup_by(|right, left| right.label == left.label);
	candidates
}

fn schema_completion_entries_for_alias(
	binder: &SchemaBinder<'_>,
	field: &CompiledRuleField,
	alias: &CompiledAlias,
	prefix_lower: &str,
) -> Vec<SchemaCompletion> {
	if allowed_value_marker(&alias.name).is_some() {
		let Some(values) = binder.dynamic_alias_values(alias) else {
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

/// `[[name] ... ]` keeps its body only when a call passes `name` (`[[!name]`
/// only when it does not). The parser reads the markers as bare words, so an
/// opener written against the first key (`[[name]key = value`) arrives as part
/// of that key; the key the body assigns is the text after it.
fn schema_key_after_conditional_opener(key: &str, range: EditorRange) -> (&str, EditorRange) {
	let mut rest = key;
	while let Some(after) = rest
		.strip_prefix("[[")
		.and_then(|tail| tail.split_once(']'))
		.filter(|(name, after)| {
			let name = name.strip_prefix('!').unwrap_or(name);
			!name.is_empty()
				&& name
					.bytes()
					.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
				&& !after.is_empty()
		})
		.map(|(_, after)| after)
	{
		rest = after;
	}
	let mut range = range;
	range.start.character += (key.len() - rest.len()) as u32;
	(rest, range)
}

/// The value of the last assignment in `[[name] ... ]`, when the closing `]`
/// touches it (`[[name] key = value]`), without that `]`.
fn schema_value_before_conditional_closer(value: &AstValue) -> Cow<'_, AstValue> {
	let AstValue::Scalar {
		value: ScalarValue::Identifier(text),
		span,
	} = value
	else {
		return Cow::Borrowed(value);
	};
	let trimmed = text.trim_end_matches(']');
	if trimmed.len() == text.len() || trimmed.is_empty() || trimmed.contains('[') {
		return Cow::Borrowed(value);
	}
	let mut span = span.clone();
	span.end.column -= text.len() - trimmed.len();
	span.end.offset -= text.len() - trimmed.len();
	Cow::Owned(AstValue::Scalar {
		value: unquoted_scalar(trimmed),
		span,
	})
}

fn direct_schema_completion_entry(
	field: &CompiledRuleField,
	prefix_lower: &str,
) -> Option<SchemaCompletion> {
	if field.key.is_empty() || is_dynamic_key_marker(&field.key) {
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

pub(super) fn schema_diagnostics_for_text(
	engine: &CwtQuery,
	file_path: &GamePath,
	text: &str,
) -> Vec<SchemaDiagnostic> {
	schema_diagnostics_for_text_with_index(engine, file_path, text, None)
}

pub(super) fn schema_diagnostics_for_text_with_index(
	engine: &CwtQuery,
	file_path: &GamePath,
	text: &str,
	dynamic_values: Option<&SchemaWorkspace>,
) -> Vec<SchemaDiagnostic> {
	let parsed = parse_clausewitz_statements(ScriptSyntax::for_game_path(file_path), text);
	schema_diagnostics_for_ast_with_index(engine, file_path, &parsed.statements, dynamic_values)
}

/// The diagnostic code for a value whose block-or-scalar shape the rules
/// reject.
const SCHEMA_SHAPE_CODE: &str = "V006";

/// How many values in `statements`, read as the file at `file_path`, have a
/// shape the active rules reject: a block where a value belongs, or a value
/// where a block does.
///
/// Only the shape is counted. Other diagnostics are not reliable enough to
/// choose between readings of a broken file.
pub(crate) fn schema_shape_violations(file_path: &GamePath, statements: &[AstStatement]) -> usize {
	schema_diagnostics_for_ast_with_index(
		crate::game::eu4::cwt::rule_engine(),
		file_path,
		statements,
		None,
	)
	.iter()
	.filter(|diagnostic| diagnostic.code.as_deref() == Some(SCHEMA_SHAPE_CODE))
	.count()
}

fn schema_diagnostics_for_ast_with_index(
	engine: &CwtQuery,
	file_path: &GamePath,
	statements: &[AstStatement],
	dynamic_values: Option<&SchemaWorkspace>,
) -> Vec<SchemaDiagnostic> {
	let mut diagnostics = Vec::new();
	let binder = schema_binder(engine, file_path, dynamic_values);
	let file = binder.file(statements);
	collect_schema_diagnostics(&binder, &file, statements, None, &mut diagnostics);
	diagnostics.extend(schema_reserved_country_tag_diagnostics(
		file_path, statements,
	));
	sort_and_dedup_diagnostics(&mut diagnostics);
	diagnostics
}

/// A country defined under a tag the game hands out at runtime collides with
/// the country the game later creates under it.
fn schema_reserved_country_tag_diagnostics(
	file_path: &GamePath,
	statements: &[AstStatement],
) -> Vec<SchemaDiagnostic> {
	if !file_path.is_inside(&["common", "country_tags"], str::eq) {
		return Vec::new();
	}
	statements
		.iter()
		.filter_map(|statement| {
			let AstStatement::Assignment { key, key_span, .. } = statement else {
				return None;
			};
			is_dynamic_country_tag(key).then(|| SchemaDiagnostic {
				range: editor_range_from_span(key_span),
				severity: Some(Severity::Warning),
				code: Some("V010".to_string()),
				source: Some(FOCH_DETECT_SOURCE.to_string()),
				message: format!(
					"tag `{key}` is one the game assigns to countries it creates at runtime; a country defined under it conflicts with them"
				),
			})
		})
		.collect()
}

pub(super) fn schema_localisation_diagnostics_for_text(
	engine: &CwtQuery,
	file_path: &GamePath,
	text: &str,
	definitions: &[LocalisationDefinition],
) -> Vec<SchemaDiagnostic> {
	let parsed = parse_clausewitz_statements(ScriptSyntax::for_game_path(file_path), text);
	schema_localisation_diagnostics_for_ast(engine, file_path, &parsed.statements, definitions)
}

fn schema_localisation_diagnostics_for_ast(
	engine: &CwtQuery,
	file_path: &GamePath,
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
		.map(|reference| schema_missing_localisation_diagnostic(&reference))
		.collect::<Vec<_>>();
	sort_and_dedup_diagnostics(&mut diagnostics);
	diagnostics
}

#[derive(Clone, Debug)]
struct SchemaLocalisationReference {
	key: String,
	range: EditorRange,
	/// The rule files of the type's `localisation` rule that asks for it.
	origin: CompiledRuleOrigin,
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
	file_path: &GamePath,
	statements: &[AstStatement],
) -> Vec<SchemaLocalisationReference> {
	let Some(root) = engine.bind_root(file_path) else {
		return Vec::new();
	};
	if root.localisation.is_empty() {
		return Vec::new();
	}
	let instances = schema_root_instances(root, statements, file_path);
	let binder = schema_binder(engine, file_path, None);
	let mut references = Vec::new();
	for instance in instances {
		let path = instance.path.iter().map(String::as_str).collect::<Vec<_>>();
		let active_subtypes =
			schema_active_subtypes_for_instance(&binder, root, instance.items, &path);
		for rule in &root.localisation {
			if !conditions_match(&rule.conditions, &active_subtypes) {
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
						origin: rule.origin,
					});
				}
				continue;
			}
			if let Some((key, range)) = scalar_assignment_in_statements(instance.items, pattern) {
				references.push(SchemaLocalisationReference {
					key,
					range,
					origin: rule.origin,
				});
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

/// The names of the definitions of `root` that a file declares.
pub(super) fn schema_definition_names(
	root: &CompiledRoot,
	statements: &[AstStatement],
	file_path: &GamePath,
) -> Vec<String> {
	schema_definitions(root, statements, file_path)
		.into_iter()
		.map(|(name, _)| name)
		.collect()
}

/// The definitions of `root` that a file declares: each name, and where it
/// is written.
pub(crate) fn schema_definitions(
	root: &CompiledRoot,
	statements: &[AstStatement],
	file_path: &GamePath,
) -> Vec<(String, EditorRange)> {
	if root.type_per_file {
		return file_path
			.file_stem()
			.map(|stem| (stem.to_string(), EditorRange::default()))
			.into_iter()
			.collect();
	}
	schema_root_instances(root, statements, file_path)
		.into_iter()
		.filter(|instance| {
			root.type_key_filter
				.as_ref()
				.is_none_or(|filter| instance.path.last().is_some_and(|key| filter.matches(key)))
		})
		.map(|instance| (instance.identity, instance.identity_range))
		.collect()
}

fn schema_root_instances<'a>(
	root: &CompiledRoot,
	statements: &'a [AstStatement],
	file_path: &GamePath,
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
	file_path: &GamePath,
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
	file_path: &GamePath,
) -> (String, EditorRange) {
	if let Some(name_field) = root.name_field.as_deref()
		&& let Some((value, range)) = scalar_assignment_in_statements(items, name_field)
	{
		return (value, range);
	}
	if root.name_from_file
		&& let Some(stem) = file_path.file_stem()
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
	binder: &SchemaBinder<'_>,
	root: &CompiledRoot,
	statements: &[AstStatement],
	path: &[&str],
) -> HashSet<String> {
	let mut active = HashSet::new();
	if let Some(RuleContext::Subtype(_, subtype)) = binder.bind_context(path) {
		active.insert(subtype.name.clone());
	}
	let definition_key = definition_key(root, binder.file_path, path);
	for subtype in &root.subtypes {
		if binder.subtype_matches_body(subtype, definition_key, statements) {
			active.insert(subtype.name.clone());
		}
	}
	active
}

fn schema_missing_localisation_diagnostic(
	reference: &SchemaLocalisationReference,
) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range: reference.range,
		severity: Some(Severity::Warning),
		code: Some("missing-localisation".to_string()),
		source: Some(rule_source(reference.origin).to_string()),
		message: format!("localisation key not found: {}", reference.key),
	}
}

fn collect_schema_diagnostics<'a>(
	binder: &SchemaBinder<'a>,
	context: &BoundContext<'a, AstStatement>,
	statements: &'a [AstStatement],
	parent_range: Option<EditorRange>,
	diagnostics: &mut Vec<SchemaDiagnostic>,
) {
	let parent_context = context.rules();
	let skip_unknown = matches!(parent_context, Some(RuleContext::AliasRules(_)));
	let active_scopes = context.scopes();
	let active_subtypes = context.subtypes();
	let parent_alias = context.alias();
	let mut cardinality_bounds = HashMap::<String, SchemaCardinalityBound>::new();
	let mut present_key_counts = HashMap::<String, u32>::new();
	for statement in statements {
		match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value,
				..
			} => {
				let (key, key_range) =
					schema_key_after_conditional_opener(key, editor_range_from_span(key_span));
				let value = schema_value_before_conditional_closer(value);
				let value = value.as_ref();
				let binding = binder.bind(context, key, value);
				let alternatives = &binding.alternatives;
				let field_match = binding.matched;
				if let Some(rule_context) = parent_context
					&& field_match.is_none()
					&& !skip_unknown
					&& !binder.dialect.holds_parameter(key)
				{
					diagnostics.push(schema_unknown_key_diagnostic(
						key_range,
						key,
						schema_context_origin(rule_context),
					));
				}
				if let Some(field_match) = field_match
					&& let Some(expected) = binder.mistyped_key(&field_match.field().key, key)
				{
					diagnostics.push(schema_key_type_diagnostic(
						&field_match,
						key_range,
						key,
						expected,
					));
				}
				if let Some(field_match) = field_match
					&& let Some(diagnostic) = schema_alias_scope_diagnostic(
						binder,
						&field_match,
						key_range,
						key,
						active_scopes,
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
					&& let AstValue::Scalar { span, .. } = value
					&& let Some(scalar) = value.scalar()
					&& !binder.dialect.holds_parameter(&scalar.text())
				{
					if let Some(diagnostic) =
						schema_invalid_value_diagnostic(binder, &field_match, key, scalar, span)
					{
						diagnostics.push(diagnostic);
					}
					if let Some(diagnostic) =
						schema_scalar_type_diagnostic(binder, &field_match, key, scalar, span)
					{
						diagnostics.push(diagnostic);
					}
					if let Some(diagnostic) = schema_value_coercion_diagnostic(
						binder,
						&field_match,
						parent_alias,
						key,
						scalar,
						span,
					) {
						diagnostics.push(diagnostic);
					}
				}
				if let Some(field_match) = field_match {
					*present_key_counts
						.entry(field_match.field().key.clone())
						.or_default() += 1;
				}
				if let Some(field_match) = field_match
					&& let Some((upper_bound, origin)) =
						schema_restated_cardinality_upper(&field_match, alternatives)
				{
					let severity =
						schema_match_diagnostic_severity(&field_match, Severity::Warning);
					let bound = cardinality_bounds
						.entry(key.to_string())
						.or_insert_with(|| SchemaCardinalityBound {
							upper_bound,
							severity,
							origin,
							occurrences: Vec::new(),
						});
					if upper_bound > bound.upper_bound {
						bound.upper_bound = upper_bound;
						bound.severity = severity;
						bound.origin = origin;
					}
					bound.occurrences.push((
						key_range,
						match value {
							AstValue::Scalar { value: scalar, .. } => {
								SchemaRepeatValue::Scalar(scalar.as_text())
							}
							AstValue::Block { .. } => {
								SchemaRepeatValue::Block(value_fingerprint(value))
							}
						},
					));
				}
				if let AstStatement::Assignment {
					value: AstValue::Block {
						items,
						span: block_span,
					},
					..
				} = statement
				{
					let child = binder.child(context, statement, key, &binding);
					collect_schema_diagnostics(
						binder,
						&child,
						items,
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
				binder,
				context,
				items,
				Some(editor_range_from_span(block_span)),
				diagnostics,
			),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => {}
		}
	}
	for (key, bound) in cardinality_bounds {
		// Only a new value counts against the bound. Restating a value the
		// field already holds leaves what the game reads unchanged, unless the
		// value is a block the game may run a second time.
		let mut seen_values = HashSet::new();
		let mut counted = 0_usize;
		for (index, (range, repeat_value)) in bound.occurrences.into_iter().enumerate() {
			let is_block = matches!(repeat_value, SchemaRepeatValue::Block(_));
			if !seen_values.insert(repeat_value) {
				if is_block && index >= bound.upper_bound as usize {
					diagnostics.push(schema_repeated_block_diagnostic(
						range,
						&key,
						bound.upper_bound,
					));
				}
				continue;
			}
			counted += 1;
			if counted > bound.upper_bound as usize {
				diagnostics.push(schema_cardinality_diagnostic(
					range,
					&key,
					bound.upper_bound,
					bound.severity,
					bound.origin,
				));
			}
		}
	}
	// A file with nothing in it is a deliberate blank — the way a mod removes
	// a file it overrides — so nothing is required of it.
	let blank_file = context.is_file()
		&& statements
			.iter()
			.all(|statement| matches!(statement, AstStatement::Comment { .. }));
	if let Some(parent_context) = parent_context
		&& !skip_unknown
		&& !blank_file
	{
		let range = schema_context_diagnostic_range(parent_range, statements);
		for (field, minimum) in schema_required_fields(parent_context, active_subtypes) {
			let present = present_key_counts
				.get(&field.key)
				.copied()
				.unwrap_or_default();
			if present < minimum {
				let severity =
					schema_attributes_diagnostic_severity(&field.attributes, Severity::Error);
				diagnostics.push(schema_required_key_diagnostic(
					range,
					&field.key,
					minimum,
					severity,
					field.origin,
				));
			}
		}
	}
}

/// What one occurrence of a bounded key holds, compared to tell a
/// restatement from a new value. A block is compared by content, ignoring
/// layout and comments.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SchemaRepeatValue {
	Scalar(String),
	Block(String),
}

/// How often a key may repeat in one block, which rule files say so, and
/// where the block writes it.
struct SchemaCardinalityBound {
	upper_bound: u32,
	severity: Severity,
	origin: CompiledRuleOrigin,
	occurrences: Vec<(EditorRange, SchemaRepeatValue)>,
}

/// Which rule files decide what a context allows. Each rule an extension adds
/// to a type allows one more key; the type itself is its first definition's.
fn schema_context_origin(context: RuleContext<'_>) -> CompiledRuleOrigin {
	match context {
		RuleContext::RootType(root) | RuleContext::Subtype(root, _) => root.origin,
		RuleContext::RuleField(field) => field.origin,
		// One alias definition writes all of its rules.
		RuleContext::AliasRules(rules) => rules
			.first()
			.map_or(CompiledRuleOrigin::Base, |rule| rule.origin),
	}
}

/// The most times a key may repeat. As in CWTools, a key matched through an
/// alias wildcard is not bounded: EU4 events write `## cardinality = 0..1`
/// on an option's effect wildcard, yet an option runs any number of effects.
fn schema_cardinality_upper(field_match: &CompiledBindFieldMatch<'_>) -> Option<u32> {
	if field_match.alias().is_some() {
		return None;
	}
	schema_match_cardinality(field_match).and_then(|(_, upper_bound)| upper_bound)
}

/// The most times a key may repeat when the schema states its rule more than
/// once (Foch's supplementary rules restate a vendor rule to raise its bound):
/// the loosest of the rules with the same key and value, with the rule files
/// that set it. A bound the vendored rules already give stays theirs.
fn schema_restated_cardinality_upper(
	field_match: &CompiledBindFieldMatch<'_>,
	alternatives: &[CompiledBindFieldMatch<'_>],
) -> Option<(u32, CompiledRuleOrigin)> {
	let mut bound = (schema_cardinality_upper(field_match)?, field_match.origin());
	for alternative in alternatives {
		if alternative.field().key == field_match.field().key
			&& alternative.value().states_same_rule(field_match.value())
		{
			let upper_bound = schema_cardinality_upper(alternative)?;
			if upper_bound > bound.0
				|| (upper_bound == bound.0 && alternative.origin() == CompiledRuleOrigin::Base)
			{
				bound = (upper_bound, alternative.origin());
			}
		}
	}
	Some(bound)
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
	let mut fields = rule_fields(context)
		.filter(|field| conditions_match(&field.conditions, active_subtypes))
		.filter(|field| parse_schema_marker(&field.key).is_none())
		.filter(|field| !field.key.is_empty() && !is_dynamic_key_marker(&field.key))
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
	editor_range_from_span(statement_span(statement))
}

fn statement_span(statement: &AstStatement) -> &SpanRange {
	match statement {
		AstStatement::Assignment { span, .. }
		| AstStatement::Item { span, .. }
		| AstStatement::Comment { span, .. } => span,
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

fn schema_invalid_value_diagnostic<'a>(
	binder: &SchemaBinder<'a>,
	field_match: &CompiledBindFieldMatch<'a>,
	key: &str,
	scalar: SchemaScalar<'_>,
	span: &SpanRange,
) -> Option<SchemaDiagnostic> {
	let values = binder.allowed_values(field_match.value())?;
	let text = scalar.text();
	if binder.allowed_values_accept(&values, &text) {
		return None;
	}
	Some(SchemaDiagnostic {
		range: editor_range_from_span(span),
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			Severity::Error,
		)),
		code: Some("V003".to_string()),
		source: Some(rule_source(field_match.origin()).to_string()),
		message: format!(
			"value `{text}` for `{key}` is not in schema {} `{}` (allowed: {})",
			values.kind.label(),
			values.name,
			format_allowed_values(&values.values)
		),
	})
}

fn schema_scalar_type_diagnostic(
	binder: &SchemaBinder<'_>,
	field_match: &CompiledBindFieldMatch<'_>,
	key: &str,
	scalar: SchemaScalar<'_>,
	span: &SpanRange,
) -> Option<SchemaDiagnostic> {
	let expected = SchemaScalarType::from_rule_value(field_match.value())?;
	if binder.scalar_type_matches(expected, scalar) {
		return None;
	}
	Some(SchemaDiagnostic {
		range: editor_range_from_span(span),
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			Severity::Error,
		)),
		code: Some("V005".to_string()),
		source: Some(rule_source(field_match.origin()).to_string()),
		message: format!(
			"value `{}` for `{key}` does not match schema type `{}`",
			scalar.text(),
			expected.label()
		),
	})
}

/// Report where the game's own coercion silently changes the written value.
/// Extra decimals are a notice; a boolean the game reads as false is an error.
///
/// How many decimals the game keeps is a property of the field, not of its
/// declared type: most fields checked in game drop every digit past the third,
/// truncating rather than rounding, but `add_army_professionalism` keeps a
/// fourth, and `change_innovativeness` is declared a whole number yet keeps
/// three. So the notice is given only where the field was checked (see
/// `crate::game::eu4::coercion`), by its own alias or, for `value`, by the
/// alias of the block around it. `CToken::GetBool` is a case-sensitive
/// comparison against `yes`, so only that spelling is true.
fn schema_value_coercion_diagnostic(
	binder: &SchemaBinder<'_>,
	field_match: &CompiledBindFieldMatch<'_>,
	parent_alias: Option<&CompiledAlias>,
	key: &str,
	scalar: SchemaScalar<'_>,
	span: &SpanRange,
) -> Option<SchemaDiagnostic> {
	let (code, severity, message) = match (
		SchemaScalarType::from_rule_value(field_match.value()),
		scalar,
	) {
		(Some(SchemaScalarType::Bool), SchemaScalar::Identifier(text))
			if text.eq_ignore_ascii_case("yes") || text.eq_ignore_ascii_case("no") =>
		{
			(
				"V009",
				schema_match_diagnostic_severity(field_match, Severity::Error),
				format!(
					"`{key}` is read as false: the game compares against the exact lowercase `yes`, so `{text}` is not a boolean"
				),
			)
		}
		(_, scalar) if schema_field_reads_three_decimals(field_match, parent_alias, key) => {
			let text = binder.number_text(scalar)?;
			let (integer, fraction) = text.split_once('.')?;
			let (kept, discarded) = fraction.split_at_checked(3)?;
			if discarded.is_empty() {
				return None;
			}
			// A notice, not a fault: vanilla itself writes `0.0025`, and the
			// map tools write `0.300000`, whose dropped zeros change nothing.
			let message = if discarded.bytes().all(|byte| byte == b'0') {
				format!(
					"`{key}` keeps three decimals: the game reads `{text}` as `{integer}.{kept}`; the dropped digits are zeros, so the value is unchanged"
				)
			} else {
				format!(
					"`{key}` keeps three decimals: the game reads `{text}` as `{integer}.{kept}` and discards `{discarded}`"
				)
			};
			("V008", Severity::Info, message)
		}
		_ => return None,
	};
	Some(SchemaDiagnostic {
		range: editor_range_from_span(span),
		severity: Some(severity),
		code: Some(code.to_string()),
		source: Some(FOCH_DETECT_SOURCE.to_string()),
		message,
	})
}

/// Whether the game was seen to read this field at three decimals.
fn schema_field_reads_three_decimals(
	field_match: &CompiledBindFieldMatch<'_>,
	parent_alias: Option<&CompiledAlias>,
	key: &str,
) -> bool {
	field_match
		.alias()
		.is_some_and(|alias| alias_reads_three_decimals(&alias.category, &alias.name))
		|| key == "value"
			&& parent_alias
				.is_some_and(|alias| alias_value_reads_three_decimals(&alias.category, &alias.name))
}

fn schema_value_shape_diagnostic(
	field_match: &CompiledBindFieldMatch<'_>,
	key: &str,
	value: &AstValue,
) -> Option<SchemaDiagnostic> {
	let expected = rule_value_shape(field_match.value());
	let actual = SchemaValueShape::of(value);
	if expected == actual {
		return None;
	}
	Some(SchemaDiagnostic {
		range: editor_range_from_span(value.span()),
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			Severity::Error,
		)),
		code: Some(SCHEMA_SHAPE_CODE.to_string()),
		source: Some(rule_source(field_match.origin()).to_string()),
		message: format!(
			"value for `{key}` is a schema {}, but this assignment uses a {}",
			expected.label(),
			actual.label()
		),
	})
}

fn schema_alias_scope_diagnostic(
	binder: &SchemaBinder<'_>,
	field_match: &CompiledBindFieldMatch<'_>,
	range: EditorRange,
	key: &str,
	active_scopes: &[String],
) -> Option<SchemaDiagnostic> {
	let alias = field_match.alias()?;
	if binder.alias_scope_matches(alias, active_scopes) {
		return None;
	}
	Some(SchemaDiagnostic {
		range,
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			Severity::Error,
		)),
		code: Some("V007".to_string()),
		source: Some(rule_source(alias.origin).to_string()),
		message: format!(
			"alias `{key}` is scoped to {}, but current schema scope is {}",
			format_scope_values(&alias.attributes.scope),
			format_scope_refs(active_scopes)
		),
	})
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

/// A key written as a number of another type or range than the rules take
/// there.
fn schema_key_type_diagnostic(
	field_match: &CompiledBindFieldMatch<'_>,
	range: EditorRange,
	key: &str,
	expected: SchemaScalarType,
) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range,
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			Severity::Error,
		)),
		code: Some("V005".to_string()),
		source: Some(rule_source(field_match.origin()).to_string()),
		message: format!(
			"key `{key}` does not match schema type `{}`",
			expected.label()
		),
	}
}

fn schema_unknown_key_diagnostic(
	range: EditorRange,
	key: &str,
	origin: CompiledRuleOrigin,
) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range,
		severity: Some(Severity::Warning),
		code: Some("V001".to_string()),
		source: Some(rule_source(origin).to_string()),
		message: format!("schema does not allow key `{key}` in this context"),
	}
}

fn schema_cardinality_diagnostic(
	range: EditorRange,
	key: &str,
	upper_bound: u32,
	severity: Severity,
	origin: CompiledRuleOrigin,
) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range,
		severity: Some(severity),
		code: Some("V002".to_string()),
		source: Some(rule_source(origin).to_string()),
		message: format!("key `{key}` exceeds schema cardinality upper bound of {upper_bound}"),
	}
}

/// A notice, not a fault: whether the game reads such a block once or runs it
/// again (a `chance` factor applied twice) is not verified per key. CWTools
/// counts the copy like any other repeat; telling it apart is foch's.
fn schema_repeated_block_diagnostic(
	range: EditorRange,
	key: &str,
	upper_bound: u32,
) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range,
		severity: Some(Severity::Info),
		code: Some("V002".to_string()),
		source: Some(FOCH_CWTOOLS_SOURCE.to_string()),
		message: format!(
			"key `{key}` repeats an identical block beyond the schema cardinality upper bound of {upper_bound}; if the game reads every copy, this block takes effect again"
		),
	}
}

fn schema_required_key_diagnostic(
	range: EditorRange,
	key: &str,
	minimum: u32,
	severity: Severity,
	origin: CompiledRuleOrigin,
) -> SchemaDiagnostic {
	SchemaDiagnostic {
		range,
		severity: Some(severity),
		code: Some("V004".to_string()),
		source: Some(rule_source(origin).to_string()),
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
