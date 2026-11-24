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
	AiMode, DecisionInfo, EventInfo, Fixture, FixtureName, GameDate, HelperFile, MAX_ADVANCE_DAYS,
	ScriptedDef, ScriptedKind, SessionRequest, Start, Step, Tag, TestCase, valid_case_name,
	valid_mod_name,
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
	pub scripted: Vec<ScriptedDef>,
	pub decisions: Vec<DecisionInfo>,
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
			SourceKind::Scripted => scripted_collect(file, statements, &mut collection),
			SourceKind::Decisions => decisions_collect(file, statements, &mut collection),
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

/// Collect country decisions from `decisions/*.txt`.
///
/// The game nests each decision under `country_decisions = { ... }`. The
/// container itself is not a decision, and a top-level `name = { ... }`
/// outside that container is not one either.
fn decisions_collect(file: &SourceFile, statements: &[AstStatement], out: &mut Collection) {
	out.decisions
		.extend(country_decisions(&file.path, &file.text, statements));
}

/// Country decisions nested in `country_decisions` blocks, in source order.
/// A later block that repeats a name is a later definition; callers that
/// overlay playset layers decide which one survives.
pub(crate) fn country_decisions(
	path: &crate::source::RelPath,
	text: &str,
	statements: &[AstStatement],
) -> Vec<DecisionInfo> {
	let mut decisions = Vec::new();
	for statement in statements {
		let AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} = statement
		else {
			continue;
		};
		if key != "country_decisions" {
			continue;
		}
		for item in items {
			let AstStatement::Assignment {
				key,
				value: AstValue::Block { span, .. },
				..
			} = item
			else {
				continue;
			};
			let start = span.start.offset + 1;
			let end = span.end.offset.saturating_sub(1);
			if start > end || end > text.len() {
				continue;
			}
			decisions.push(DecisionInfo {
				name: key.clone(),
				origin: SourceSpan::from_parser(path, span),
				body: text[start..end].into(),
			});
		}
	}
	decisions
}

/// Collect top-level `name = { ... }` definitions from a scripted directory.
/// The path decides the kind: `common/scripted_triggers/` holds condition
/// blocks, `common/scripted_effects/` holds effect blocks.
fn scripted_collect(file: &SourceFile, statements: &[AstStatement], out: &mut Collection) {
	let kind = if file.path.is_below("common/scripted_effects") {
		ScriptedKind::Effect
	} else if file.path.is_below("common/scripted_triggers") {
		ScriptedKind::Trigger
	} else {
		out.diagnostics.push(Diagnostic::error(
			DiagnosticCode::Annotation,
			None,
			format!(
				"{}: scripted sources must be under common/scripted_*",
				file.path
			),
		));
		return;
	};
	for statement in statements {
		let AstStatement::Assignment {
			key,
			value: AstValue::Block { span, .. },
			..
		} = statement
		else {
			continue;
		};
		if !identifier_like(key) {
			out.diagnostics.push(Diagnostic::error(
				DiagnosticCode::Annotation,
				Some(SourceSpan::from_parser(&file.path, span)),
				format!("{key:?} is not a valid scripted definition name"),
			));
			continue;
		}
		out.scripted.push(ScriptedDef {
			name: key.clone(),
			kind,
			body: file.text[span.start.offset + 1..span.end.offset - 1].into(),
			origin: SourceSpan::from_parser(&file.path, span),
		});
	}
}

/// A scripted definition name: an identifier, so it cannot collide with the
/// numeric-keyed or symbolic forms inside blocks.
fn identifier_like(key: &str) -> bool {
	!key.is_empty()
		&& key
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
		&& !key.bytes().next().is_some_and(|byte| byte.is_ascii_digit())
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
		("expect_receipt", _) => {
			let AstValue::Block { items, .. } = value else {
				return Err("expect_receipt must be a block".into());
			};
			receipt_step(file, items, span())?
		}
		(_, None) => return Ok(None),
		("effect", Some(Value::Effects(block))) => Step::Effect { block },
		("expect", Some(Value::Conditions(clauses))) => Step::Expect { clauses },
		("advance_days", Some(Value::Integer(days))) => Step::AdvanceDays { days, span: span() },
		("fire", Some(Value::EventId(event))) => Step::Fire {
			event,
			span: span(),
		},
		("take_decision", Some(Value::Text(name))) => Step::TakeDecision { name, span: span() },
		("cheat", Some(Value::Text(command))) => Step::Cheat {
			command,
			span: span(),
		},
		("save", Some(Value::Text(slot))) => Step::Save {
			slot: save_slot(&slot)?,
			span: span(),
		},
		("reload", Some(Value::Text(slot))) => Step::Reload {
			slot: save_slot(&slot)?,
			span: span(),
		},
		_ => unreachable!("step types are fixed by the schema"),
	}))
}

/// Parse one `expect_receipt` block's entries, shared by the `tests/` block
/// path and the inline `#test` annotation path.
fn receipt_step(
	file: &SourceFile,
	items: &[AstStatement],
	span: SourceSpan,
) -> Result<Step, String> {
	let _ = file;
	let mut plugin: Option<String> = None;
	let mut fields: Vec<(String, String)> = Vec::new();
	let mut numeric: Vec<crate::model::ReceiptRange> = Vec::new();
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			// Comments are fine; a bare item drops a constraint the
			// author wrote, so it is a collection error.
			if matches!(item, AstStatement::Comment { .. }) {
				continue;
			}
			return Err(
				"expect_receipt holds an entry without a key; every constraint must be key = value"
					.into(),
			);
		};
		let AstValue::Scalar {
			value:
				ScalarValue::Identifier(text) | ScalarValue::String(text) | ScalarValue::Number(text),
			..
		} = value
		else {
			// A nested block of field = { min = .. max = .. } ranges;
			// every other block-shaped field is unreadable.
			if key != "numeric" {
				return Err(format!("expect_receipt field {key} needs a plain value"));
			}
			let AstValue::Block { items, .. } = value else {
				return Err("numeric must be a block of field = { min = .. max = .. }".into());
			};
			for entry in items {
				let AstStatement::Assignment {
					key: field, value, ..
				} = entry
				else {
					if matches!(entry, AstStatement::Comment { .. }) {
						continue;
					}
					return Err("numeric holds field = { min = .. max = .. } entries".into());
				};
				let AstValue::Block { items, .. } = value else {
					return Err(format!(
						"numeric field {field} needs a min = .. and/or max = .. block"
					));
				};
				if fields.iter().any(|(existing, _)| existing == field) {
					return Err(format!(
						"expect_receipt field {field} is constrained twice, once as an exact string and once numerically; keep one form"
					));
				}
				if numeric.iter().any(|range| range.field == *field) {
					return Err(format!("numeric constrains {field} twice"));
				}
				let (mut min, mut max) = (None, None);
				for bound in items {
					let AstStatement::Assignment { key, value, .. } = bound else {
						if matches!(bound, AstStatement::Comment { .. }) {
							continue;
						}
						return Err(format!("numeric field {field} holds min and/or max bounds"));
					};
					let AstValue::Scalar {
						value: ScalarValue::Number(text),
						..
					} = value
					else {
						return Err(format!("numeric bound {key} of {field} needs a number"));
					};
					let parsed: f64 = text
						.parse()
						.map_err(|_| format!("numeric bound {key} of {field} is not a number"))?;
					if !parsed.is_finite() {
						return Err(format!("numeric bound {key} of {field} must be finite"));
					}
					match key.as_str() {
						"min" => min = Some(parsed),
						"max" => max = Some(parsed),
						other => {
							return Err(format!(
								"numeric field {field} takes min and/or max, not {other}"
							));
						}
					}
				}
				if min.is_none() && max.is_none() {
					return Err(format!(
						"numeric field {field} needs at least one of min or max"
					));
				}
				numeric.push(crate::model::ReceiptRange {
					field: field.clone(),
					min,
					max,
				});
			}
			continue;
		};
		if key == "plugin" {
			if plugin.is_some() {
				return Err("expect_receipt names plugin twice".into());
			}
			plugin = Some(text.clone());
		} else if let Some(previous) = fields.iter().find(|(existing, _)| existing == key) {
			return Err(format!(
				"expect_receipt field {key} is given twice ({:?} and {:?}); \
				 conflicting correlation constraints cannot be honored",
				previous.1, text,
			));
		} else {
			fields.push((key.clone(), text.clone()));
		}
	}
	let plugin = plugin.ok_or_else(|| "expect_receipt needs a plugin = ... field".to_string())?;
	// Correlation is mandatory: the receipt must name the case it belongs
	// to and carry a request token the fixture issued with the command.
	// Without both, a same-session receipt for another case could satisfy
	// this step.
	for required in ["case", "request"] {
		if !fields.iter().any(|(key, _)| key == required) {
			return Err(format!(
				"expect_receipt needs a {required} = ... field correlating it to one command of this case"
			));
		}
	}
	Ok(Step::ExpectReceipt {
		plugin,
		fields,
		numeric,
		span,
	})
}

/// A save slot name becomes one console line (`save_game <slot>`), so it
/// may only carry characters the console reads as one token.
fn save_slot(slot: &str) -> Result<String, String> {
	if !slot.is_empty()
		&& slot
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
	{
		Ok(slot.to_string())
	} else {
		Err(format!(
			"save slot {slot:?} must be a non-empty name of letters, digits, _ or -"
		))
	}
}

fn typed_step(file: &SourceFile, key: &str, value: &AstValue) -> Result<Option<Value>, String> {
	match key {
		// expect_receipt has its own block shape; step() parses it.
		"expect_receipt" => Ok(None),
		"effect" | "expect" | "advance_days" => typed(file, key, value).map(Some),
		"take_decision" | "cheat" | "save" | "reload" => {
			parse_value(&file.path, &file.text, value, ValueType::Text)
				.map(Some)
				.map_err(|message| format!("{key}: {message}"))
		}
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
