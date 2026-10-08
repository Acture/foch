//! Test meaning of `#test` annotations and their modifiers.
//!
//! `foch-annotation` finds, parses and type-checks the annotations and
//! verifies the target event; this module turns them into cases.

use super::{CaseFields, Collection, event_info, total_days};
use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::model::{CaseName, ContentId, Marker, Marks, NodeId, Start, Step, TestCase, XFail};
use crate::source::SourceFile;
use foch::game::eu4::script::parser::AstStatement;
use foch_annotation::builtin::{REGISTRY, TEST};
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
				Ok(case) => out.cases.push(case),
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

fn bind(
	file: &SourceFile,
	target: &Target,
	applied: &Applied,
	position: u32,
) -> Result<TestCase, String> {
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
	let mut case = TestCase {
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
			date: fields.time.ok_or("missing required test parameter time")?,
			tag: fields.tag.ok_or("missing required test parameter tag")?,
		},
		ai: fields.ai.unwrap_or_default(),
		uses: fields.uses.unwrap_or_default(),
		steps,
		marks,
	};
	if case.marks.xfail.is_some() && case.is_smoke() {
		return Err("#xfail requires an expect block; a smoke test has nothing to fail".into());
	}
	case.compute_content_id();
	Ok(case)
}
