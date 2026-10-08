//! Generate the test layer and launch description for one game session.
//!
//! Protocol v2 extends the v1 runner contract (same environment variables
//! and bundle role) with several cases per session, per-clause checks,
//! fixture checks and multi-step cases. Each case logs engine `EVENT` lines
//! `FOCH_TEST_V2 <run> <session digest> c<index> <record>`, where a record is
//! `BEGIN`, `PASS <check>`, `FAIL <check>` or `END`.

use crate::model::{
	AiMode, CaseIndex, Clause, EventId, FixtureName, GameDate, NativeBlock, RunId, Step, Tag,
	digest,
};
use crate::plan::{Isolation, Plan, SessionId};
use crate::source::SourceSpan;
use foch::game::eu4::script::emit_native_statements;
use foch::game::eu4::script::parser::{AstStatement, AstValue, parse_clausewitz_content};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

pub const PROTOCOL_VERSION: u32 = 2;
const MARKER: &str = "FOCH_TEST_V2";
const RESERVED: &str = "FOCH_TEST_";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunContext {
	pub run_id: RunId,
	/// Mod under test; used for readable generated names.
	pub mod_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckKind {
	/// The case runs in its declared country.
	Scope,
	Fixture {
		fixture: FixtureName,
		clause: u32,
	},
	Invoked {
		step: u32,
	},
	Expect {
		step: u32,
		clause: u32,
	},
}

impl CheckKind {
	/// The record name logged by the generated script.
	pub fn label(&self) -> String {
		match self {
			Self::Scope => "scope".into(),
			Self::Fixture { fixture, clause } => format!("fixture.{fixture}.{clause}"),
			Self::Invoked { step } => format!("invoked.{step}"),
			Self::Expect { step, clause } => format!("expect.{step}.{clause}"),
		}
	}
}

/// A check the log must report, on the given engine date, in plan order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlannedCheck {
	pub case: CaseIndex,
	pub kind: CheckKind,
	pub date: GameDate,
	/// Source of the condition or step, for failure reports.
	pub span: SourceSpan,
}

/// When a case begins and ends inside the session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CaseWindow {
	pub case: CaseIndex,
	pub begin: GameDate,
	pub end: GameDate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Launch {
	pub args: Vec<String>,
	pub ai: AiMode,
	pub player: Tag,
	pub start_date: GameDate,
	pub end_date: GameDate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Requirements {
	pub start_date: GameDate,
	pub ai_off: bool,
	pub shared_session: bool,
}

/// Everything one game session needs. Files are relative to a fresh bundle
/// directory; the caller writes them and launches the runner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
	pub protocol: u32,
	pub run_id: RunId,
	pub mod_name: String,
	pub session: SessionId,
	pub cases: Vec<CaseIndex>,
	pub marker_prefix: String,
	pub namespace: String,
	pub test_mod_name: String,
	pub files: BTreeMap<String, String>,
	pub launch: Launch,
	pub checks: Vec<PlannedCheck>,
	pub windows: Vec<CaseWindow>,
	pub requires: Requirements,
}

impl Bundle {
	/// Prefix of a single case's records.
	pub fn case_prefix(&self, case: CaseIndex) -> String {
		format!("{} c{}", self.marker_prefix, case.0)
	}
}

/// What a runner declares it can provide.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunnerCapabilities {
	pub start_dates: Vec<GameDate>,
	pub ai_off: bool,
	pub shared_sessions: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Unsupported {
	pub reasons: Vec<String>,
}

impl fmt::Display for Unsupported {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			formatter,
			"runner cannot run this session: {}",
			self.reasons.join("; ")
		)
	}
}

impl std::error::Error for Unsupported {}

/// A runner that lacks a required capability must be refused, never asked
/// to approximate it.
pub fn check_capabilities(
	bundle: &Bundle,
	capabilities: &RunnerCapabilities,
) -> Result<(), Unsupported> {
	let mut reasons = Vec::new();
	if !capabilities
		.start_dates
		.contains(&bundle.requires.start_date)
	{
		reasons.push(format!(
			"start date {} is not provided",
			bundle.requires.start_date
		));
	}
	if bundle.requires.ai_off && !capabilities.ai_off {
		reasons.push("disabling AI is not supported".into());
	}
	if bundle.requires.shared_session && !capabilities.shared_sessions {
		reasons.push("several cases in one session are not supported".into());
	}
	if reasons.is_empty() {
		Ok(())
	} else {
		Err(Unsupported { reasons })
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompileError {
	pub span: Option<SourceSpan>,
	pub message: String,
}

impl fmt::Display for CompileError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match &self.span {
			Some(span) => write!(formatter, "{span}: {}", self.message),
			None => formatter.write_str(&self.message),
		}
	}
}

impl std::error::Error for CompileError {}

fn fail(span: Option<&SourceSpan>, message: impl Into<String>) -> CompileError {
	CompileError {
		span: span.cloned(),
		message: message.into(),
	}
}

pub fn compile(
	plan: &Plan,
	session: SessionId,
	context: &RunContext,
) -> Result<Bundle, CompileError> {
	let session = plan
		.sessions
		.get(session.0)
		.filter(|candidate| candidate.id == session)
		.ok_or_else(|| fail(None, "unknown session"))?;
	crate::model::valid_mod_name(&context.mod_name).map_err(|message| fail(None, message))?;
	if session.cases.is_empty() {
		return Err(fail(None, "session has no cases"));
	}

	#[derive(Serialize)]
	struct Identity<'a> {
		run: &'a RunId,
		session: SessionId,
		cases: Vec<&'a str>,
	}
	let identity = Identity {
		run: &context.run_id,
		session: session.id,
		cases: session
			.cases
			.iter()
			.map(|&index| plan.case(index).case.content_id.as_str())
			.collect(),
	};
	let digest = digest(&identity);
	let marker_prefix = format!("{MARKER} {} {}", context.run_id, &digest[..16]);
	let slug = slug(&context.mod_name);
	let mut namespace = match &slug {
		Some(slug) => format!("foch_test_{slug}_{}", &digest[..12]),
		None => format!("foch_test_{}", &digest[..12]),
	};
	if plan.events.iter().any(|event| {
		event
			.id
			.as_str()
			.split_once('.')
			.is_some_and(|(ns, _)| ns == namespace)
	}) {
		namespace.push_str("_checks");
	}

	let mut events = String::new();
	let mut start = String::new();
	let mut checks = Vec::new();
	let mut windows = Vec::new();
	let mut next_event = 1u32;
	let mut end_date = session.date;
	for &index in &session.cases {
		let planned = plan.case(index);
		let case = &planned.case;
		if case.start.date != session.date {
			return Err(fail(
				Some(&case.origin),
				"case start date differs from its session",
			));
		}
		let mut writer = CaseWriter {
			namespace: &namespace,
			prefix: format!("{marker_prefix} c{}", index.0),
			case: index,
			date: session.date,
			next_event: &mut next_event,
			events: &mut events,
			checks: &mut checks,
			body: String::new(),
			chained: None,
		};
		let first = writer.open();
		start.push_str(&format!(
			"{} = {{ country_event = {{ id = {namespace}.{first} }} }}\n",
			case.start.tag
		));
		writer.log("BEGIN");
		writer.check(
			CheckKind::Scope,
			&format!("tag = {}", case.start.tag),
			&case.origin,
		);
		for fixture in &planned.fixtures {
			if let Some(effect) = &fixture.effect {
				writer.body.push_str(&native(effect)?);
			}
			for (clause_index, clause) in fixture.check.iter().enumerate() {
				writer.check(
					CheckKind::Fixture {
						fixture: fixture.name.clone(),
						clause: clause_index as u32,
					},
					&condition(clause)?,
					&clause.span,
				);
			}
		}
		let mut pending_days: Option<u32> = None;
		for (step_index, step) in case.steps.iter().enumerate() {
			let step_index = step_index as u32;
			match step {
				Step::AdvanceDays { days, .. } => {
					*pending_days.get_or_insert(0) += days;
					continue;
				}
				_ if pending_days.is_some() => writer.chain(pending_days.take().unwrap_or(0))?,
				_ => {}
			}
			match step {
				Step::Effect { block } => writer.body.push_str(&native(block)?),
				Step::Fire { event, span } => {
					writer.fire(event);
					writer.check_unconditional(CheckKind::Invoked { step: step_index }, span);
					// Later steps observe the event's effects from a new event,
					// matching the verified v1 call sequence.
					pending_days = Some(0);
				}
				Step::Expect { clauses } => {
					for (clause_index, clause) in clauses.iter().enumerate() {
						writer.check(
							CheckKind::Expect {
								step: step_index,
								clause: clause_index as u32,
							},
							&condition(clause)?,
							&clause.span,
						);
					}
				}
				Step::AdvanceDays { .. } => unreachable!("handled above"),
			}
		}
		if let Some(days) = pending_days {
			writer.chain(days)?;
		}
		writer.log("END");
		let case_end = writer.date;
		writer.close();
		windows.push(CaseWindow {
			case: index,
			begin: session.date,
			end: case_end,
		});
		end_date = end_date.max(case_end);
	}

	let events = canonical(
		&format!("namespace = {namespace}\n{events}"),
		"generated test events",
	)?;
	let start_file = format!("{namespace}_start.txt");
	let start = canonical(&start, "generated start effects")?;
	let mut commands = format!("speed 0\nrun {start_file}\n");
	if end_date != session.date {
		commands.push_str("speed 5\n");
	}
	let test_mod_name = format!("{} tests [{}]", context.mod_name, &digest[..6]);
	let mut files = BTreeMap::from([
		("commands.txt".to_string(), commands),
		(start_file, start),
		(format!("test_mod/events/{namespace}.txt"), events),
		(
			"test_mod/descriptor.mod".to_string(),
			format!("name = {}\n", quote(&test_mod_name)),
		),
	]);
	for helper in &plan.helpers {
		let relative = helper
			.path
			.as_str()
			.strip_prefix("tests/events/")
			.unwrap_or(helper.path.as_str())
			.replace('/', "_");
		files.insert(
			format!("test_mod/events/{namespace}_helper_{relative}"),
			helper.text.clone(),
		);
	}
	Ok(Bundle {
		protocol: PROTOCOL_VERSION,
		run_id: context.run_id.clone(),
		mod_name: context.mod_name.clone(),
		session: session.id,
		cases: session.cases.clone(),
		marker_prefix,
		namespace,
		test_mod_name,
		files,
		launch: Launch {
			args: vec![
				format!("-start_tag={}", session.player),
				"-auto_run=commands.txt".into(),
				"-seed=42".into(),
				"-debug".into(),
			],
			ai: session.ai,
			player: session.player.clone(),
			start_date: session.date,
			end_date,
		},
		checks,
		windows,
		requires: Requirements {
			start_date: session.date,
			ai_off: session.ai == AiMode::Off,
			shared_session: session.isolation == Isolation::Shared,
		},
	})
}

/// Writes one case's chain of hidden events.
struct CaseWriter<'a> {
	namespace: &'a str,
	prefix: String,
	case: CaseIndex,
	date: GameDate,
	next_event: &'a mut u32,
	events: &'a mut String,
	checks: &'a mut Vec<PlannedCheck>,
	body: String,
	chained: Option<u32>,
}

impl CaseWriter<'_> {
	fn open(&mut self) -> u32 {
		let id = *self.next_event;
		*self.next_event += 1;
		self.chained = Some(id);
		self.body.clear();
		id
	}

	fn close(&mut self) {
		let id = self.chained.take().expect("an event is open");
		self.events.push_str(&format!(
			"country_event = {{\nid = {}.{id}\nhidden = yes\nis_triggered_only = yes\n\
			title = \"Foch runtime test\"\ndesc = \"Foch runtime test\"\npicture = ADVISOR_eventPicture\n\
			immediate = {{\n{}}}\noption = {{ name = OK }}\n}}\n",
			self.namespace, self.body
		));
	}

	/// Continue in a new event after `days` real game days.
	fn chain(&mut self, days: u32) -> Result<(), CompileError> {
		let next = *self.next_event;
		let delay = if days == 0 {
			String::new()
		} else {
			format!(" days = {days}")
		};
		self.body.push_str(&format!(
			"country_event = {{ id = {}.{next}{delay} }}\n",
			self.namespace
		));
		self.close();
		self.open();
		self.date = self
			.date
			.add_days(days)
			.map_err(|message| fail(None, message))?;
		Ok(())
	}

	fn log(&mut self, record: &str) {
		self.body
			.push_str(&format!("log = \"{} {record}\"\n", self.prefix));
	}

	fn fire(&mut self, event: &EventId) {
		self.body
			.push_str(&format!("country_event = {{ id = {event} }}\n"));
	}

	fn check(&mut self, kind: CheckKind, condition: &str, span: &SourceSpan) {
		let label = kind.label();
		self.body.push_str(&format!(
			"if = {{\nlimit = {{\n{condition}\n}}\nlog = \"{prefix} PASS {label}\"\n}}\n\
			else = {{ log = \"{prefix} FAIL {label}\" }}\n",
			prefix = self.prefix
		));
		self.record(kind, span);
	}

	fn check_unconditional(&mut self, kind: CheckKind, span: &SourceSpan) {
		self.log(&format!("PASS {}", kind.label()));
		self.record(kind, span);
	}

	fn record(&mut self, kind: CheckKind, span: &SourceSpan) {
		self.checks.push(PlannedCheck {
			case: self.case,
			kind,
			date: self.date,
			span: span.clone(),
		});
	}
}

/// `Mod Name` -> `mod_name`; `None` when nothing ASCII remains.
fn slug(name: &str) -> Option<String> {
	let mut slug = String::new();
	for character in name.chars() {
		if character.is_ascii_alphanumeric() {
			slug.push(character.to_ascii_lowercase());
		} else if !slug.ends_with('_') && !slug.is_empty() {
			slug.push('_');
		}
	}
	let slug: String = slug.trim_end_matches('_').chars().take(24).collect();
	let slug = slug.trim_end_matches('_').to_string();
	(!slug.is_empty()).then_some(slug)
}

fn quote(text: &str) -> String {
	format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn native(block: &NativeBlock) -> Result<String, CompileError> {
	validated(&block.text, &block.span)
}

fn condition(clause: &Clause) -> Result<String, CompileError> {
	validated(&clause.text, &clause.span)
}

/// Re-parses user script inside a wrapper whose closing offset is known, so
/// user text cannot escape its block, and re-emits it canonically.
fn validated(source: &str, span: &SourceSpan) -> Result<String, CompileError> {
	if source.contains(RESERVED) {
		return Err(fail(
			Some(span),
			"script uses reserved test protocol markers",
		));
	}
	let wrapped = format!("foch_validation = {{\n{source}\n}}\n");
	let parsed = parse_clausewitz_content(crate::game_path("validation.txt"), &wrapped);
	if let Some(diagnostic) = parsed.diagnostics.first() {
		return Err(fail(
			Some(span),
			format!("invalid script: {}", diagnostic.message),
		));
	}
	let [
		AstStatement::Assignment {
			value: AstValue::Block { items, span: block },
			..
		},
	] = parsed.ast.statements.as_slice()
	else {
		return Err(fail(Some(span), "script escapes its native block"));
	};
	if block.end.offset != wrapped.len() - 1 {
		return Err(fail(Some(span), "script contains unbalanced braces"));
	}
	emit_assignments(items).map_err(|message| fail(Some(span), message))
}

fn canonical(source: &str, label: &str) -> Result<String, CompileError> {
	let parsed = parse_clausewitz_content(crate::game_path(&format!("{label}.txt")), source);
	if let Some(diagnostic) = parsed.diagnostics.first() {
		return Err(fail(
			None,
			format!("invalid {label}: {}", diagnostic.message),
		));
	}
	emit_assignments(&parsed.ast.statements)
		.map_err(|message| fail(None, format!("invalid {label}: {message}")))
}

fn emit_assignments(statements: &[AstStatement]) -> Result<String, String> {
	if !statements
		.iter()
		.any(|item| matches!(item, AstStatement::Assignment { .. }))
		|| statements
			.iter()
			.any(|item| matches!(item, AstStatement::Item { .. }))
	{
		return Err("script must contain native assignments".into());
	}
	validate_keys(statements)?;
	emit_native_statements(statements)
}

fn validate_keys(statements: &[AstStatement]) -> Result<(), String> {
	for statement in statements {
		let value = match statement {
			AstStatement::Assignment { key, value, .. } => {
				if key.is_empty()
					|| key.chars().any(|character| {
						character.is_whitespace()
							|| character.is_control()
							|| matches!(character, '"' | '{' | '}' | '#' | '=' | ',' | '\\')
					}) {
					return Err("assignment key cannot be emitted as a native identifier".into());
				}
				value
			}
			AstStatement::Item { value, .. } => value,
			AstStatement::Comment { .. } => continue,
		};
		if let AstValue::Block { items, .. } = value {
			validate_keys(items)?;
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slugs_are_ascii_identifiers() {
		assert_eq!(
			slug("Extended Timeline!").as_deref(),
			Some("extended_timeline")
		);
		assert_eq!(slug("中文模组"), None);
		assert_eq!(slug("a--b"), Some("a_b".into()));
	}

	#[test]
	fn user_script_cannot_escape_or_forge_markers() {
		let span = SourceSpan {
			path: crate::source::RelPath::new("events/a.txt").unwrap(),
			start: Default::default(),
			end: Default::default(),
		};
		for script in [
			"} log = hacked",
			"if = {",
			"log = \"FOCH_TEST_V2 x\"",
			"bare_item",
			"if = { \"bad key\" = yes }",
		] {
			assert!(validated(script, &span).is_err(), "{script}");
		}
		assert_eq!(
			validated("set_country_flag = a # note", &span).unwrap(),
			"set_country_flag = a\n# note\n"
		);
	}
}
