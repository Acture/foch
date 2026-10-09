//! `test = { ... }` and `fixture = { ... }` blocks below the mod's `tests/`.
//!
//! The game does not load this directory, so these are plain script blocks.
//! Inside a test block, `effect`, `fire`, `advance_days` and `expect` are
//! steps executed in source order; the remaining keys are metadata typed by
//! the same schema as `#test(...)`.

use super::{CaseFields, Collection, step, total_days};
use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::model::{
	CaseName, ContentId, Fixture, FixtureName, Marker, Marks, NodeId, Start, TestCase, XFail,
};
use crate::source::{SourceFile, SourceSpan};
use foch::game::eu4::script::parser::{AstStatement, AstValue};
use foch_annotation::value::{Value, ValueType, boolean, parse_value, scalar};

pub(super) fn collect(file: &SourceFile, statements: &[AstStatement], out: &mut Collection) {
	for statement in statements {
		let (key, value, span) = match statement {
			AstStatement::Comment { .. } => continue,
			AstStatement::Item { span, .. } => {
				out.diagnostics.push(error(
					SourceSpan::from_parser(&file.path, span),
					"expected test = { ... } or fixture = { ... }",
				));
				continue;
			}
			AstStatement::Assignment {
				key, value, span, ..
			} => (key, value, span),
		};
		let origin = SourceSpan::from_parser(&file.path, span);
		let AstValue::Block { items, .. } = value else {
			out.diagnostics
				.push(error(origin, format!("{key} must be a block")));
			continue;
		};
		let result = match key.as_str() {
			"test" => parse_test(file, items, origin.clone()).map(|case| out.cases.push(case)),
			"fixture" => {
				parse_fixture(file, items, origin.clone()).map(|fixture| out.fixtures.push(fixture))
			}
			other => Err(format!(
				"unexpected {other}; files below tests/ contain test and fixture blocks, helper events belong below tests/events/"
			)),
		};
		if let Err(message) = result {
			out.diagnostics.push(error(origin, message));
		}
	}
}

fn error(span: SourceSpan, message: impl Into<String>) -> Diagnostic {
	Diagnostic::error(DiagnosticCode::Annotation, Some(span), message)
}

/// `skip = yes` or `skip = "reason"`.
/// `skip = yes` or `skip = "reason"`. `skip = no` is rejected rather than
/// silently skipping with the reason "no"; omit the key to not skip.
fn marker(value: &AstValue) -> Result<Marker, String> {
	match boolean(value) {
		Ok(true) => Ok(Marker { reason: None }),
		Ok(false) => Err("use yes or a reason string; omit the key to not skip".into()),
		Err(_) => Ok(Marker {
			reason: Some(scalar(value)?),
		}),
	}
}

fn xfail(value: &AstValue) -> Result<XFail, String> {
	let AstValue::Block { items, .. } = value else {
		return Ok(XFail {
			reason: marker(value)?.reason,
			strict: false,
		});
	};
	let mut xfail = XFail::default();
	for item in items {
		match item {
			AstStatement::Comment { .. } => {}
			AstStatement::Assignment { key, value, .. } if key == "reason" => {
				xfail.reason = Some(scalar(value)?);
			}
			AstStatement::Assignment { key, value, .. } if key == "strict" => {
				xfail.strict = boolean(value)?;
			}
			_ => return Err("xfail accepts reason and strict".into()),
		}
	}
	Ok(xfail)
}

fn parse_test(
	file: &SourceFile,
	items: &[AstStatement],
	origin: SourceSpan,
) -> Result<TestCase, String> {
	let mut fields = CaseFields::default();
	let mut marks = Marks::default();
	let mut steps = Vec::new();
	for item in items {
		let (key, value) = match item {
			AstStatement::Comment { .. } => continue,
			AstStatement::Item { .. } => {
				return Err("test blocks contain key = value entries".into());
			}
			AstStatement::Assignment { key, value, .. } => (key.as_str(), value),
		};
		if fields.apply(file, key, value)? {
			continue;
		}
		if let Some(step) = step(file, key, value)? {
			steps.push(step);
			continue;
		}
		let duplicate = || format!("duplicate test key {key}");
		match key {
			"mark" => match parse_value(&file.path, &file.text, value, ValueType::Identifiers) {
				Ok(Value::Identifiers(labels)) => marks.labels.extend(labels),
				Ok(_) => unreachable!("identifiers parse as identifiers"),
				Err(message) => return Err(format!("mark: {message}")),
			},
			"skip" => {
				if marks.skip.replace(marker(value)?).is_some() {
					return Err(duplicate());
				}
			}
			"ignore" => {
				if marks.ignore.replace(marker(value)?).is_some() {
					return Err(duplicate());
				}
			}
			"xfail" => {
				if marks.xfail.replace(xfail(value)?).is_some() {
					return Err(duplicate());
				}
			}
			other => return Err(format!("unknown test key {other}")),
		}
	}
	if marks.skip.is_some() && marks.ignore.is_some() {
		return Err("a test cannot be both skip and ignore".into());
	}
	if steps.is_empty() {
		return Err("a test needs at least one effect, fire, advance_days or expect step".into());
	}
	total_days(&steps)?;
	marks.session = fields.session;
	marks.requires_player = fields.player.unwrap_or(false);
	let mut case = TestCase {
		node: NodeId {
			mod_name: file.mod_name.clone(),
			path: file.path.clone(),
			target: None,
			name: CaseName::Named(fields.name.ok_or("a test block requires name")?),
			params: Vec::new(),
		},
		content_id: ContentId(String::new()),
		origin,
		start: Start {
			date: fields.time.ok_or("a test block requires time")?,
			tag: fields.tag.ok_or("a test block requires tag")?,
		},
		ai: fields.ai.unwrap_or_default(),
		uses: fields.uses.unwrap_or_default(),
		steps,
		marks,
	};
	if case.marks.xfail.is_some() && case.is_smoke() {
		return Err("xfail requires an expect step; a smoke test has nothing to fail".into());
	}
	case.compute_content_id();
	Ok(case)
}

fn parse_fixture(
	file: &SourceFile,
	items: &[AstStatement],
	origin: SourceSpan,
) -> Result<Fixture, String> {
	let (mut name, mut uses, mut effect, mut check) = (None, None, None, None);
	for item in items {
		let (key, value) = match item {
			AstStatement::Comment { .. } => continue,
			AstStatement::Item { .. } => {
				return Err("fixture blocks contain key = value entries".into());
			}
			AstStatement::Assignment { key, value, .. } => (key.as_str(), value),
		};
		let value_type = match key {
			"name" => ValueType::Text,
			"use" => ValueType::Identifiers,
			"effect" => ValueType::Effects,
			"check" => ValueType::Conditions,
			other => return Err(format!("unknown fixture key {other}")),
		};
		let parsed = parse_value(&file.path, &file.text, value, value_type)
			.map_err(|message| format!("{key}: {message}"))?;
		let fresh = match parsed {
			Value::Text(text) => name.replace(FixtureName::parse(&text)?).is_none(),
			Value::Identifiers(names) => uses
				.replace(
					names
						.iter()
						.map(|name| FixtureName::parse(name))
						.collect::<Result<Vec<_>, _>>()?,
				)
				.is_none(),
			Value::Effects(block) => effect.replace(block).is_none(),
			Value::Conditions(clauses) => check.replace(clauses).is_none(),
			_ => unreachable!("fixture keys have fixed types"),
		};
		if !fresh {
			return Err(format!("duplicate fixture key {key}"));
		}
	}
	if effect.is_none() && check.is_none() {
		return Err("a fixture needs an effect, a check, or both".into());
	}
	Ok(Fixture {
		name: name.ok_or("a fixture requires name")?,
		origin,
		uses: uses.unwrap_or_default(),
		effect,
		check: check.unwrap_or_default(),
	})
}
