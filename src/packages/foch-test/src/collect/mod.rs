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
	SessionRequest, Start, Step, Tag, TestCase, valid_case_name, valid_mod_name,
};
use crate::source::{SourceFile, SourceKind, SourceSpan};
use foch::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, parse_clausewitz_content,
};
use foch_annotation::builtin::TEST_PARAMS;
use foch_annotation::value::{EventId, Value, ValueType, parse_value, scalar};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Upper bound on the cases one `#parametrize`d test may expand to, so a large
/// Cartesian product cannot exhaust memory during collection. The run-wide
/// `--max-cases` budget still applies on top of this.
pub(super) const MAX_PARAMETRIZE_CASES: usize = 256;

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

/// The start dimensions a case varies over. Empty when nothing is parametrized.
#[derive(Default)]
pub(super) struct Axes {
	pub tags: Option<Vec<Tag>>,
	pub times: Option<Vec<GameDate>>,
}

/// One resolved start dimension: its values, and whether it varies (so an
/// instance records it as a node parameter).
pub(super) struct Dimension<T> {
	pub values: Vec<T>,
	pub varies: bool,
}

/// Resolves one start dimension from the value given directly on the test and
/// the list given by `#parametrize`. Giving it in both places is an error, as
/// is giving it in neither.
pub(super) fn dimension<T>(
	direct: Option<T>,
	axis: Option<Vec<T>>,
	name: &str,
) -> Result<Dimension<T>, String> {
	match (direct, axis) {
		(Some(_), Some(_)) => Err(format!(
			"{name} is given by both the test and parametrize; give it in only one"
		)),
		(None, Some(values)) => Ok(Dimension {
			values,
			varies: true,
		}),
		(Some(value), None) => Ok(Dimension {
			values: vec![value],
			varies: false,
		}),
		(None, None) => Err(format!("missing required test parameter {name}")),
	}
}

/// Expands one collected test into its parametrized instances. `base` carries
/// the shared steps, marks and node; its start is overwritten per instance.
/// This sets each instance's start and the node parameters that distinguish
/// it, and recomputes its content identity.
pub(super) fn expand_cases(
	base: TestCase,
	tag: Dimension<Tag>,
	time: Dimension<GameDate>,
) -> Result<Vec<TestCase>, String> {
	let (tags, tag_varies) = (tag.values, tag.varies);
	let (times, time_varies) = (time.values, time.varies);
	if let Some(duplicate) = first_duplicate(tags.iter().map(Tag::as_str)) {
		return Err(format!("parametrize lists tag {duplicate} twice"));
	}
	if let Some(duplicate) = first_duplicate(times.iter().map(|time| time.to_string())) {
		return Err(format!("parametrize lists time {duplicate} twice"));
	}
	let product = tags.len() * times.len();
	if product > MAX_PARAMETRIZE_CASES {
		return Err(format!(
			"parametrize expands to {product} cases, more than the limit of {MAX_PARAMETRIZE_CASES}; shorten the lists"
		));
	}
	let mut cases = Vec::with_capacity(product);
	for date in &times {
		for tag in &tags {
			let mut case = base.clone();
			case.start = Start {
				date: *date,
				tag: tag.clone(),
			};
			case.node.params = Vec::new();
			if tag_varies {
				case.node.params.push(("tag".into(), tag.to_string()));
			}
			if time_varies {
				case.node.params.push(("time".into(), date.to_string()));
			}
			case.compute_content_id();
			cases.push(case);
		}
	}
	Ok(cases)
}

fn first_duplicate<T: AsRef<str>>(values: impl Iterator<Item = T>) -> Option<String> {
	let mut seen = HashSet::new();
	for value in values {
		if !seen.insert(value.as_ref().to_string()) {
			return Some(value.as_ref().to_string());
		}
	}
	None
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
