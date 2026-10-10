//! Test meaning of `#test` annotations and their modifiers.
//!
//! `foch-annotation` finds, parses and type-checks the annotations and
//! verifies the target event; this module turns them into cases.

use super::{Axes, CaseFields, Collection, dimension, event_info, expand_cases, total_days};
use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::model::{
	CaseName, ContentId, GameDate, Marker, Marks, NodeId, Start, Step, Tag, TestCase, XFail,
};
use crate::source::SourceFile;
use foch::game::eu4::script::parser::AstStatement;
use foch_annotation::builtin::{PARAMETRIZE, REGISTRY, TEST};
use foch_annotation::extract::{Annotation, Applied, Target, extract_parsed};
use foch_annotation::value::{EventId, Value};

pub(super) fn collect(file: &SourceFile, statements: &[AstStatement], out: &mut Collection) {
	let extraction = extract_parsed(&file.path, &file.text, statements, &REGISTRY);
	out.diagnostics
		.extend(extraction.diagnostics.into_iter().map(Diagnostic::from));
	for statement in statements {
		if let Some(event) = event_info(file, statement, false) {
			out.events.push(event);
		}
	}
	for attachment in &extraction.attachments {
		let tests = attachment
			.annotations
			.iter()
			.filter(|applied| applied.annotation.name() == TEST);
		for (position, applied) in tests.enumerate() {
			match bind(file, &attachment.target, applied, position as u32) {
				Ok(cases) => out.cases.extend(cases),
				Err(message) => out.diagnostics.push(Diagnostic::error(
					DiagnosticCode::Annotation,
					Some(applied.annotation.span.clone()),
					message,
				)),
			}
		}
	}
}

fn reason(annotation: &Annotation) -> Option<String> {
	match annotation.get("reason") {
		Some(Value::Text(reason)) => Some(reason.clone()),
		_ => None,
	}
}

fn marks(modifiers: &[Annotation]) -> Result<Marks, String> {
	let mut marks = Marks::default();
	for modifier in modifiers {
		match modifier.name() {
			// Handled separately: it expands the test rather than marking it.
			PARAMETRIZE => {}
			"mark" => marks
				.labels
				.extend(modifier.positional.iter().map(|(label, _)| label.clone())),
			"skip" => {
				marks.skip = Some(Marker {
					reason: reason(modifier),
				})
			}
			"ignore" => {
				marks.ignore = Some(Marker {
					reason: reason(modifier),
				})
			}
			"xfail" => {
				marks.xfail = Some(XFail {
					reason: reason(modifier),
					strict: matches!(modifier.get("strict"), Some(Value::Bool(true))),
				})
			}
			other => return Err(format!("#{other} has no test meaning")),
		}
	}
	if marks.skip.is_some() && marks.ignore.is_some() {
		return Err("a #test cannot be both #skip and #ignore".into());
	}
	Ok(marks)
}

/// Reads the start dimensions from the `#parametrize` modifier, if present.
/// The schema fixes the value types, so the shapes here always hold.
fn axes(modifiers: &[Annotation]) -> Axes {
	let mut axes = Axes::default();
	for modifier in modifiers.iter().filter(|m| m.name() == PARAMETRIZE) {
		for argument in &modifier.arguments {
			match (argument.name.as_str(), &argument.value) {
				("tag", Value::List(values)) => {
					axes.tags = Some(values.iter().map(expect_tag).collect())
				}
				("time", Value::List(values)) => {
					axes.times = Some(values.iter().map(expect_date).collect())
				}
				_ => unreachable!("parametrize schema fixes argument types"),
			}
		}
	}
	axes
}

fn expect_tag(value: &Value) -> Tag {
	match value {
		Value::Tag(tag) => tag.clone(),
		_ => unreachable!("parametrize tag list holds tags"),
	}
}

fn expect_date(value: &Value) -> GameDate {
	match value {
		Value::Date(date) => *date,
		_ => unreachable!("parametrize time list holds dates"),
	}
}

fn bind(
	file: &SourceFile,
	target: &Target,
	applied: &Applied,
	position: u32,
) -> Result<Vec<TestCase>, String> {
	let event = EventId::parse(
		target
			.id
			.as_deref()
			.ok_or("target event requires exactly one id")?,
	)?;
	let mut fields = CaseFields::default();
	let (mut effect, mut days, mut expect) = (None, None, None);
	for argument in &applied.annotation.arguments {
		if fields.set(&argument.name, &argument.value)? {
			continue;
		}
		match (argument.name.as_str(), &argument.value) {
			("effect", Value::Effects(block)) => effect = Some(block.clone()),
			("advance_days", Value::Integer(value)) => days = Some((*value, argument.span.clone())),
			("expect", Value::Conditions(clauses)) => expect = Some(clauses.clone()),
			(other, _) => return Err(format!("#test parameter {other} is not supported here")),
		}
	}
	let mut steps = Vec::new();
	if let Some(block) = effect {
		steps.push(Step::Effect { block });
	}
	steps.push(Step::Fire {
		event: event.clone(),
		span: target.span.clone(),
	});
	if let Some((days, span)) = days.filter(|(days, _)| *days > 0) {
		steps.push(Step::AdvanceDays { days, span });
	}
	if let Some(clauses) = expect {
		steps.push(Step::Expect { clauses });
	}
	total_days(&steps)?;

	let mut marks = marks(&applied.modifiers)?;
	marks.session = fields.session;
	marks.requires_player = fields.player.unwrap_or(false);
	let axes = axes(&applied.modifiers);
	let tag = dimension(fields.tag, axes.tags, "tag")?;
	let time = dimension(fields.time, axes.times, "time")?;
	let base = TestCase {
		node: NodeId {
			mod_name: file.mod_name.clone(),
			path: file.path.clone(),
			target: Some(event),
			name: fields
				.name
				.map_or(CaseName::Index(position), CaseName::Named),
			params: Vec::new(),
		},
		content_id: ContentId(String::new()),
		origin: applied.annotation.span.clone(),
		start: Start {
			date: time.values[0],
			tag: tag.values[0].clone(),
		},
		ai: fields.ai.unwrap_or_default(),
		uses: fields.uses.unwrap_or_default(),
		steps,
		marks,
	};
	if base.marks.xfail.is_some() && base.is_smoke() {
		return Err("#xfail requires an expect block; a smoke test has nothing to fail".into());
	}
	expand_cases(base, tag, time)
}
