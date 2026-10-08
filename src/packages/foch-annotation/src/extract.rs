//! Find annotations in comments and attach them to the next definition.
//!
//! An annotation is `#name(arguments)`, possibly continued over several full
//! comment lines that each start with `#`. Arguments are Clausewitz script,
//! parsed at their original byte offsets so every span points into the file.
//! Extraction never stops at the first problem; malformed annotations become
//! diagnostics and are left out of the result.

use crate::diagnostic::{Code, Diagnostic, Severity};
use crate::schema::{AnnotationKind, AnnotationSchema, Registry};
use crate::source::{RelPath, SourceSpan};
use crate::value::{Value, game_path, identifier, parse_value, scalar};
use foch::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, Span, SpanRange, parse_clausewitz_content,
};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Argument {
	pub name: String,
	pub span: SourceSpan,
	pub value: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct Annotation {
	pub schema: &'static AnnotationSchema,
	pub span: SourceSpan,
	pub arguments: Vec<Argument>,
	/// Unnamed arguments, such as labels.
	pub positional: Vec<(String, SourceSpan)>,
}

impl Annotation {
	pub fn name(&self) -> &'static str {
		self.schema.name
	}

	pub fn get(&self, name: &str) -> Option<&Value> {
		self.arguments
			.iter()
			.find(|argument| argument.name == name)
			.map(|argument| &argument.value)
	}
}

/// A primary annotation with the modifiers written before it.
#[derive(Clone, Debug, Serialize)]
pub struct Applied {
	pub annotation: Annotation,
	pub modifiers: Vec<Annotation>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Target {
	pub key: String,
	/// The definition's scalar `id` field, if it has one.
	pub id: Option<String>,
	pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize)]
pub struct Attachment {
	pub target: Target,
	pub annotations: Vec<Applied>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Extraction {
	pub attachments: Vec<Attachment>,
	pub diagnostics: Vec<Diagnostic>,
}

pub fn extract(path: &RelPath, text: &str, registry: &Registry) -> Extraction {
	let parsed = parse_clausewitz_content(game_path(path.as_str()), text);
	if !parsed.diagnostics.is_empty() {
		return Extraction {
			attachments: Vec::new(),
			diagnostics: parsed
				.diagnostics
				.into_iter()
				.map(|diagnostic| {
					Diagnostic::error(
						Code::Parse,
						SourceSpan::from_parser(path, &diagnostic.span),
						diagnostic.message,
					)
				})
				.collect(),
		};
	}
	extract_parsed(path, text, &parsed.ast.statements, registry)
}

/// Like [`extract`], for callers that already parsed `text`.
pub fn extract_parsed(
	path: &RelPath,
	text: &str,
	statements: &[AstStatement],
	registry: &Registry,
) -> Extraction {
	let mut out = Extraction::default();
	let mut modifiers: Vec<Annotation> = Vec::new();
	let mut primaries: Vec<Applied> = Vec::new();
	let error = |code, span: &SpanRange, message: String| {
		Diagnostic::error(code, SourceSpan::from_parser(path, span), message)
	};
	let mut index = 0;
	while index < statements.len() {
		match &statements[index] {
			AstStatement::Comment {
				text: comment,
				span,
			} => {
				let Some(schema) = annotation_schema(comment, registry) else {
					if let Some(name) = unknown_annotation(comment) {
						out.diagnostics.push(Diagnostic {
							severity: Severity::Warning,
							..error(
								Code::UnknownAnnotation,
								span,
								format!("#{name}(...) is not a known annotation"),
							)
						});
					}
					index += 1;
					continue;
				};
				let start = span.clone();
				let annotation = read_annotation(statements, &mut index, text, schema.name)
					.map_err(|message| vec![error(Code::Malformed, &start, message)])
					.and_then(|raw| parse_arguments(path, schema, raw));
				match annotation {
					Err(diagnostics) => out.diagnostics.extend(diagnostics),
					Ok(annotation) => match schema.kind {
						AnnotationKind::Modifier { .. } => modifiers.push(annotation),
						AnnotationKind::Primary => {
							let mut applied = Applied {
								annotation,
								modifiers: Vec::new(),
							};
							for modifier in modifiers.drain(..) {
								attach_modifier(&mut applied, modifier, &mut out.diagnostics);
							}
							primaries.push(applied);
						}
					},
				}
			}
			statement => {
				for modifier in modifiers.drain(..) {
					let of = match modifier.schema.kind {
						AnnotationKind::Modifier { of } => of,
						AnnotationKind::Primary => unreachable!("only modifiers wait"),
					};
					out.diagnostics.push(Diagnostic::error(
						Code::Orphan,
						modifier.span.clone(),
						format!("#{} must precede a #{of}", modifier.name()),
					));
				}
				if !primaries.is_empty() {
					let target = target(path, statement);
					let mut annotations = Vec::new();
					for applied in primaries.drain(..) {
						match check_target(statement, applied.annotation.schema) {
							Ok(()) => annotations.push(applied),
							Err(message) => out.diagnostics.push(Diagnostic::error(
								Code::UnsupportedTarget,
								applied.annotation.span.clone(),
								message,
							)),
						}
					}
					if !annotations.is_empty() {
						out.attachments.push(Attachment {
							target,
							annotations,
						});
					}
				}
				nested(path, statement, registry, &mut out.diagnostics);
				index += 1;
			}
		}
	}
	for annotation in modifiers
		.into_iter()
		.chain(primaries.into_iter().map(|applied| applied.annotation))
	{
		out.diagnostics.push(Diagnostic::error(
			Code::Orphan,
			annotation.span.clone(),
			format!("#{} has no following definition", annotation.name()),
		));
	}
	out
}

fn attach_modifier(applied: &mut Applied, modifier: Annotation, diagnostics: &mut Vec<Diagnostic>) {
	let primary = applied.annotation.name();
	if modifier.schema.kind != (AnnotationKind::Modifier { of: primary }) {
		diagnostics.push(Diagnostic::error(
			Code::Orphan,
			modifier.span.clone(),
			format!("#{} does not apply to #{primary}", modifier.name()),
		));
	} else if applied
		.modifiers
		.iter()
		.any(|existing| existing.name() == modifier.name())
	{
		diagnostics.push(Diagnostic::error(
			Code::DuplicateModifier,
			modifier.span.clone(),
			format!(
				"#{} is given twice for the same #{primary}",
				modifier.name()
			),
		));
	} else {
		applied.modifiers.push(modifier);
	}
}

/// The registered annotation a comment starts, if any. A bare primary name,
/// such as `#test` without arguments, is still recognized so it is reported
/// instead of silently ignored.
pub fn annotation_schema(comment: &str, registry: &Registry) -> Option<&'static AnnotationSchema> {
	let text = comment.trim_start();
	registry.schemas.iter().find(|schema| {
		text.strip_prefix(schema.name).is_some_and(|rest| {
			rest.trim_start().starts_with('(')
				|| (schema.kind == AnnotationKind::Primary && rest.trim().is_empty())
		})
	})
}

/// `#tset(...)`-like comments: a lowercase identifier immediately followed by `(`.
fn unknown_annotation(comment: &str) -> Option<&str> {
	let text = comment.trim_start();
	let name = &text[..text.find('(')?];
	(!name.is_empty()
		&& name
			.bytes()
			.all(|byte| byte.is_ascii_lowercase() || byte == b'_'))
	.then_some(name)
}

fn nested(
	path: &RelPath,
	statement: &AstStatement,
	registry: &Registry,
	out: &mut Vec<Diagnostic>,
) {
	let value = match statement {
		AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => value,
		AstStatement::Comment { .. } => return,
	};
	if let AstValue::Block { items, .. } = value {
		for item in items {
			match item {
				AstStatement::Comment { text, span } => {
					if let Some(schema) = annotation_schema(text, registry) {
						out.push(Diagnostic::error(
							Code::Nested,
							SourceSpan::from_parser(path, span),
							format!("#{} must precede a top-level definition", schema.name),
						));
					}
				}
				_ => nested(path, item, registry, out),
			}
		}
	}
}

fn statement_span(statement: &AstStatement) -> &SpanRange {
	match statement {
		AstStatement::Assignment { span, .. }
		| AstStatement::Item { span, .. }
		| AstStatement::Comment { span, .. } => span,
	}
}

fn target(path: &RelPath, statement: &AstStatement) -> Target {
	let (key, id) = match statement {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} => (key.clone(), single_scalar(items, "id")),
		AstStatement::Assignment { key, .. } => (key.clone(), None),
		_ => (String::new(), None),
	};
	Target {
		key,
		id,
		span: SourceSpan::from_parser(path, statement_span(statement)),
	}
}

fn single_scalar(items: &[AstStatement], name: &str) -> Option<String> {
	let mut values = items.iter().filter_map(|item| match item {
		AstStatement::Assignment { key, value, .. } if key == name => Some(value),
		_ => None,
	});
	let value = values.next()?;
	values
		.next()
		.is_none()
		.then(|| scalar(value).ok())
		.flatten()
}

fn check_target(statement: &AstStatement, schema: &AnnotationSchema) -> Result<(), String> {
	let Some(rule) = schema.target else {
		return Ok(());
	};
	let AstStatement::Assignment {
		key,
		value: AstValue::Block { items, .. },
		..
	} = statement
	else {
		return Err(format!(
			"#{} must be followed by a {} definition",
			schema.name, rule.key
		));
	};
	if key != rule.key {
		return Err(format!("#{} supports {}, not {key}", schema.name, rule.key));
	}
	for field in rule.required_yes {
		let set = items
			.iter()
			.filter(|item| matches!(item, AstStatement::Assignment { key, .. } if key == field));
		let values: Vec<_> = set.collect();
		let yes = matches!(
			values.as_slice(),
			[AstStatement::Assignment {
				value: AstValue::Scalar {
					value: ScalarValue::Bool(true),
					..
				},
				..
			}]
		);
		if !yes {
			return Err(format!(
				"#{} requires the {} to declare {field} = yes exactly once",
				schema.name, rule.key
			));
		}
	}
	Ok(())
}

struct RawAnnotation {
	span: SpanRange,
	statements: Vec<AstStatement>,
	/// Annotation text at the original byte positions, blanks elsewhere.
	source: String,
}

fn parse_arguments(
	path: &RelPath,
	schema: &'static AnnotationSchema,
	raw: RawAnnotation,
) -> Result<Annotation, Vec<Diagnostic>> {
	let span = SourceSpan::from_parser(path, &raw.span);
	let mut diagnostics = Vec::new();
	let mut annotation = Annotation {
		schema,
		span: span.clone(),
		arguments: Vec::new(),
		positional: Vec::new(),
	};
	let mut given = Vec::new();
	for statement in &raw.statements {
		match statement {
			AstStatement::Comment { .. } => {}
			AstStatement::Assignment {
				key,
				key_span,
				value,
				..
			} => {
				let key_span = SourceSpan::from_parser(path, key_span);
				given.push(key.as_str());
				let Some(param) = schema.param(key) else {
					diagnostics.push(Diagnostic::error(
						Code::UnknownParameter,
						key_span,
						format!("unknown #{} parameter {key}", schema.name),
					));
					continue;
				};
				if given[..given.len() - 1].contains(&key.as_str()) {
					diagnostics.push(Diagnostic::error(
						Code::DuplicateParameter,
						key_span,
						format!("duplicate #{} parameter {key}", schema.name),
					));
					continue;
				}
				match parse_value(path, &raw.source, value, param.value_type) {
					Ok(parsed) => annotation.arguments.push(Argument {
						name: key.clone(),
						span: key_span,
						value: parsed,
					}),
					Err(message) => diagnostics.push(Diagnostic::error(
						Code::InvalidValue,
						SourceSpan::from_parser(path, value.span()),
						format!("{key}: {message}"),
					)),
				}
			}
			AstStatement::Item { value, span: item } => {
				let item_span = SourceSpan::from_parser(path, item);
				let label = scalar(value).ok().filter(|label| identifier(label));
				match (schema.positional, label) {
					(Some(_), Some(label)) => annotation.positional.push((label, item_span)),
					(Some(_), None) => diagnostics.push(Diagnostic::error(
						Code::InvalidValue,
						item_span,
						format!("#{} expects identifiers", schema.name),
					)),
					(None, _) => diagnostics.push(Diagnostic::error(
						Code::Malformed,
						item_span,
						format!("#{} expects named key=value arguments", schema.name),
					)),
				}
			}
		}
	}
	for param in schema.params.iter().filter(|param| param.required) {
		if !given.contains(&param.name) {
			diagnostics.push(Diagnostic::error(
				Code::MissingParameter,
				span.clone(),
				format!("missing required #{} parameter {}", schema.name, param.name),
			));
		}
	}
	if diagnostics.is_empty() {
		Ok(annotation)
	} else {
		Err(diagnostics)
	}
}

/// Reads one annotation, possibly continued over several full comment lines,
/// and parses its arguments as Clausewitz script at the original offsets.
fn read_annotation(
	statements: &[AstStatement],
	index: &mut usize,
	source: &str,
	name: &str,
) -> Result<RawAnnotation, String> {
	let AstStatement::Comment { span: start, .. } = &statements[*index] else {
		unreachable!("annotations start at a comment")
	};
	let first = *index;
	let start = start.clone();
	let mut virtual_source: Vec<u8> = source
		.bytes()
		.map(|byte| if byte == b'\n' { b'\n' } else { b' ' })
		.collect();
	let mut quote = false;
	let mut escape = false;
	let mut braces = 0usize;
	let mut end_offset = None;
	while *index < statements.len() {
		let AstStatement::Comment { span, .. } = &statements[*index] else {
			break;
		};
		let line_start = source[..span.start.offset]
			.rfind('\n')
			.map_or(0, |offset| offset + 1);
		if !source[line_start..span.start.offset].trim().is_empty() {
			*index += 1;
			return Err(format!(
				"#{name} and its continuation must occupy full comment lines"
			));
		}
		let mut content_start = span.start.offset + 1;
		if *index == first {
			let content = &source[content_start..span.end.offset];
			let rest = content
				.trim_start()
				.strip_prefix(name)
				.unwrap_or_default()
				.trim_start();
			if !rest.starts_with('(') {
				*index += 1;
				return Err(format!("expected #{name}(...)"));
			}
			content_start = span.end.offset - rest.len() + 1;
		}
		*index += 1;
		if span.end.offset - start.start.offset > 65_536 {
			return Err("annotation exceeds 64 KiB".into());
		}
		for offset in content_start..span.end.offset {
			let byte = source.as_bytes()[offset];
			if quote {
				virtual_source[offset] = byte;
				if escape {
					escape = false;
				} else if byte == b'\\' {
					escape = true;
				} else if byte == b'"' {
					quote = false;
				}
				continue;
			}
			match byte {
				b'#' => break,
				b'"' => {
					quote = true;
					virtual_source[offset] = byte;
				}
				b'{' => {
					braces += 1;
					virtual_source[offset] = byte;
				}
				b'}' => {
					braces = braces.checked_sub(1).ok_or("unmatched } in annotation")?;
					virtual_source[offset] = byte;
				}
				b')' if braces == 0 => {
					if !source[offset + 1..span.end.offset].trim().is_empty() {
						return Err(format!("unexpected text after #{name}(...)"));
					}
					end_offset = Some(offset + 1);
					break;
				}
				b'(' | b')' if braces == 0 => {
					return Err("unexpected parenthesis in annotation arguments".into());
				}
				b',' if braces == 0 => {}
				_ => virtual_source[offset] = byte,
			}
		}
		if end_offset.is_some() {
			break;
		}
	}
	let end_offset = end_offset.ok_or_else(|| {
		format!("unterminated #{name}(...); every continuation must be a comment")
	})?;
	virtual_source.truncate(end_offset);
	let virtual_source =
		String::from_utf8(virtual_source).map_err(|_| "invalid annotation encoding")?;
	let parsed = parse_clausewitz_content(game_path("annotation.txt"), &virtual_source);
	if let Some(error) = parsed.diagnostics.first() {
		return Err(format!(
			"invalid #{name} arguments at line {}: {}",
			error.span.start.line, error.message
		));
	}
	let end = Span {
		line: source[..end_offset]
			.bytes()
			.filter(|&byte| byte == b'\n')
			.count() + 1,
		column: end_offset
			- source[..end_offset]
				.rfind('\n')
				.map_or(0, |offset| offset + 1)
			+ 1,
		offset: end_offset,
	};
	Ok(RawAnnotation {
		span: SpanRange {
			start: start.start,
			end,
		},
		statements: parsed.ast.statements,
		source: virtual_source,
	})
}
