//! Collect cases, fixtures and events from source text.
//!
//! Collection never stops at the first problem: every malformed declaration
//! becomes a diagnostic and valid declarations are still returned, so an
//! editor can show all problems at once. Whether diagnostics block a run is
//! the caller's decision. Annotation syntax and value types come from
//! `foch-annotation`; this module gives them their test meaning.

mod annotation;
mod tests_dir;

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::model::{
	AiMode, EventInfo, Fixture, FixtureName, GameDate, HelperFile, MAX_ADVANCE_DAYS,
	SessionRequest, Step, Tag, TestCase, valid_case_name, valid_mod_name,
};
use crate::source::{SourceFile, SourceKind, SourceSpan};
use foch::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, parse_clausewitz_content,
};
use foch_annotation::builtin::TEST_PARAMS;
use foch_annotation::value::{EventId, Value, ValueType, parse_value, scalar};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Collection {
	pub cases: Vec<TestCase>,
	pub fixtures: Vec<Fixture>,
	pub events: Vec<EventInfo>,
	pub helpers: Vec<HelperFile>,
	pub diagnostics: Vec<Diagnostic>,
}

impl Collection {
	pub fn has_errors(&self) -> bool {
		self.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.severity == crate::diagnostic::Severity::Error)
	}
}

pub fn collect(files: &[SourceFile]) -> Collection {
	let mut collection = Collection::default();
	for file in files {
		if let Err(message) = valid_mod_name(&file.mod_name) {
			collection.diagnostics.push(Diagnostic::error(
				DiagnosticCode::InvalidMod,
				None,
				message,
			));
			continue;
		}
		if !file.path.as_str().ends_with(".txt") {
			collection.diagnostics.push(Diagnostic::error(
				DiagnosticCode::Annotation,
				None,
				format!("{}: test sources must be EU4 .txt scripts", file.path),
			));
			continue;
		}
		let parsed = parse_clausewitz_content(crate::game_path(file.path.as_str()), &file.text);
		if !parsed.diagnostics.is_empty() {
			for diagnostic in parsed.diagnostics {
				collection.diagnostics.push(Diagnostic::error(
					DiagnosticCode::Parse,
					Some(SourceSpan::from_parser(&file.path, &diagnostic.span)),
					diagnostic.message,
				));
			}
			continue;
		}
		let statements = &parsed.ast.statements;
		match file.kind {
			SourceKind::Inline => annotation::collect(file, statements, &mut collection),
			SourceKind::TestsDir if file.path.is_below("tests/events") => {
				for statement in statements {
					if let Some(event) = event_info(file, statement, true) {
						collection.events.push(event);
					}
				}
				collection.helpers.push(HelperFile {
					path: file.path.clone(),
					text: file.text.clone(),
				});
			}
			SourceKind::TestsDir => tests_dir::collect(file, statements, &mut collection),
		}
	}

	let mut nodes = HashSet::new();
	let mut cases = Vec::new();
	for case in collection.cases.drain(..) {
		if nodes.insert(case.node.to_string()) {
			cases.push(case);
		} else {
			collection.diagnostics.push(Diagnostic::error(
				DiagnosticCode::DuplicateCase,
				Some(case.origin.clone()),
				format!(
					"duplicate test {}; give each case a distinct name",
					case.node
				),
			));
		}
	}
	collection.cases = cases;

	let mut names = HashSet::new();
	let mut fixtures = Vec::new();
	for fixture in collection.fixtures.drain(..) {
		if names.insert(fixture.name.clone()) {
			fixtures.push(fixture);
		} else {
			collection.diagnostics.push(Diagnostic::error(
				DiagnosticCode::DuplicateFixture,
				Some(fixture.origin.clone()),
				format!("duplicate fixture {}", fixture.name),
			));
		}
	}
	collection.fixtures = fixtures;
	collection
}

/// Record a top-level `country_event` definition.
fn event_info(file: &SourceFile, statement: &AstStatement, helper: bool) -> Option<EventInfo> {
	let AstStatement::Assignment {
		key,
		value: AstValue::Block { items, span },
		..
	} = statement
	else {
		return None;
	};
	if key != "country_event" {
		return None;
	}
	let id = single_field(items, "id")
		.and_then(|value| scalar(value).ok())
		.and_then(|id| EventId::parse(&id).ok())?;
	let yes = |name| {
		matches!(
			single_field(items, name),
			Some(AstValue::Scalar {
				value: ScalarValue::Bool(true),
				..
			})
		)
	};
	Some(EventInfo {
		id,
		origin: SourceSpan::from_parser(&file.path, span),
		invocable: yes("hidden") && yes("is_triggered_only"),
		body: file.text[span.start.offset + 1..span.end.offset - 1].into(),
		helper,
	})
}

fn single_field<'a>(items: &'a [AstStatement], name: &str) -> Option<&'a AstValue> {
	let mut values = items.iter().filter_map(|item| match item {
		AstStatement::Assignment { key, value, .. } if key == name => Some(value),
		_ => None,
	});
	let value = values.next()?;
	values.next().is_none().then_some(value)
}

/// Parses a `tests/` block value with the type the `#test` schema declares.
fn typed(file: &SourceFile, key: &str, value: &AstValue) -> Result<Value, String> {
	let param = TEST_PARAMS
		.iter()
		.find(|param| param.name == key)
		.ok_or_else(|| format!("unknown test key {key}"))?;
	parse_value(&file.path, &file.text, value, param.value_type)
		.map_err(|message| format!("{key}: {message}"))
}

/// Case metadata shared by `#test(...)` annotations and `test = { ... }` blocks.
#[derive(Default)]
struct CaseFields {
	time: Option<GameDate>,
	tag: Option<Tag>,
	name: Option<String>,
	uses: Option<Vec<FixtureName>>,
	ai: Option<AiMode>,
	session: Option<SessionRequest>,
	player: Option<bool>,
}

const METADATA: &[&str] = &["time", "tag", "name", "use", "ai", "session", "player"];

fn set_once<T>(slot: &mut Option<T>, key: &str, value: T) -> Result<(), String> {
	if slot.is_some() {
		return Err(format!("duplicate test parameter {key}"));
	}
	*slot = Some(value);
	Ok(())
}

impl CaseFields {
	/// Stores a typed metadata value; returns false for other keys.
	fn set(&mut self, key: &str, value: &Value) -> Result<bool, String> {
		match (key, value) {
			("time", Value::Date(date)) => set_once(&mut self.time, key, *date)?,
			("tag", Value::Tag(tag)) => set_once(&mut self.tag, key, tag.clone())?,
			("name", Value::Text(name)) => {
				valid_case_name(name)?;
				set_once(&mut self.name, key, name.clone())?;
			}
			("use", Value::Identifiers(names)) => set_once(
				&mut self.uses,
				key,
				names
					.iter()
					.map(|name| FixtureName::parse(name))
					.collect::<Result<_, _>>()?,
			)?,
			("ai", Value::Choice(mode)) => set_once(
				&mut self.ai,
				key,
				if mode == "on" {
					AiMode::On
				} else {
					AiMode::Off
				},
			)?,
			("session", Value::Choice(request)) => set_once(
				&mut self.session,
				key,
				if request == "alone" {
					SessionRequest::Alone
				} else {
					SessionRequest::Auto
				},
			)?,
			("player", Value::Bool(player)) => set_once(&mut self.player, key, *player)?,
			_ => return Ok(false),
		}
		Ok(true)
	}

	/// Parses and stores a metadata key of a `tests/` block.
	fn apply(&mut self, file: &SourceFile, key: &str, value: &AstValue) -> Result<bool, String> {
		if !METADATA.contains(&key) {
			return Ok(false);
		}
		self.set(key, &typed(file, key, value)?)
	}
}

/// Parses a step keyword of a `tests/` block; returns `None` for other keys.
fn step(file: &SourceFile, key: &str, value: &AstValue) -> Result<Option<Step>, String> {
	let span = || SourceSpan::from_parser(&file.path, value.span());
	Ok(Some(match (key, typed_step(file, key, value)?) {
		(_, None) => return Ok(None),
		("effect", Some(Value::Effects(block))) => Step::Effect { block },
		("expect", Some(Value::Conditions(clauses))) => Step::Expect { clauses },
		("advance_days", Some(Value::Integer(days))) => Step::AdvanceDays { days, span: span() },
		("fire", Some(Value::EventId(event))) => Step::Fire {
			event,
			span: span(),
		},
		_ => unreachable!("step types are fixed by the schema"),
	}))
}

fn typed_step(file: &SourceFile, key: &str, value: &AstValue) -> Result<Option<Value>, String> {
	match key {
		"effect" | "expect" | "advance_days" => typed(file, key, value).map(Some),
		"fire" => parse_value(&file.path, &file.text, value, ValueType::EventId)
			.map(Some)
			.map_err(|message| format!("fire: {message}")),
		_ => Ok(None),
	}
}

fn total_days(steps: &[Step]) -> Result<(), String> {
	let total: u64 = steps
		.iter()
		.map(|step| match step {
			Step::AdvanceDays { days, .. } => u64::from(*days),
			_ => 0,
		})
		.sum();
	if total > u64::from(MAX_ADVANCE_DAYS) {
		return Err(format!(
			"a case may advance at most {MAX_ADVANCE_DAYS} days in total"
		));
	}
	Ok(())
}
