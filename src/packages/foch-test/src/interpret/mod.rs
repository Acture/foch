//! The offline layer: interpret planned cases against a small, explicit
//! model of EU4 semantics instead of launching the game.
//!
//! The model is organized by [`pins::SemanticsCatalog`]: every primitive the
//! interpreter executes is an entry whose semantics are pinned to a
//! decompiled handler function, and a pinned catalog refuses anything whose
//! evidence is missing. The interpreter is an author-feedback layer, like a
//! compiler's, not a verification oracle: cases using anything outside the
//! modeled set are refused with all of their reasons at once, never
//! approximated, mirroring how [`crate::compile::check_capabilities`]
//! refuses instead of degrading. Results are deliberately separate types
//! from [`crate::judge`], so an offline verdict can never be mistaken for
//! game evidence.
//!
//! What the model knows is stated per primitive in the catalog: country and
//! global flags, variables with absolute values, `if` control flow, country
//! scope changes (tag blocks and the THIS/ROOT/PREV/FROM aliases), and
//! event firing through `country_event`, including delayed events that
//! `advance_days` later delivers. Numeric country statistics (mana,
//! treasury, stability, prestige) have unknown starting values without the
//! base snapshot, so both reading and writing them is refused rather than
//! guessed.

pub mod pins;

use crate::lint::ProjectFacts;
use crate::model::{
	AiMode, CaseIndex, Clause, ContentId, EventId, EventInfo, GameDate, NativeBlock, NodeId, RunId,
	ScriptedDef, ScriptedKind, Start, Step, Tag,
};
use crate::plan::{NotRun, NotRunReason, Plan, PlannedCase};
use crate::source::SourceSpan;
use foch::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, parse_clausewitz_content,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

pub use pins::{EvidenceStatus, SemanticsCatalog};

/// Marks every offline result, in JSON and in reports, as its own layer.
pub const LAYER: &str = "offline";

/// Upper bound of scheduled-event deliveries one case may cause, so a
/// self-scheduling event pair cannot run the model forever.
const MAX_EVENT_DELIVERIES: usize = 4096;

/// Upper bound of nested scripted-trigger inlining, so mutually inlining
/// definitions cannot recurse forever.
const MAX_SCRIPTED_DEPTH: u32 = 256;

/// Blocks that structure effects without changing scope.
const EFFECT_CONTROL: &[&str] = &[
	"if",
	"else_if",
	"else",
	"hidden_effect",
	"custom_tooltip",
	"tooltip",
];

/// Blocks that structure conditions without changing scope.
const CONDITION_CONTROL: &[&str] = &["AND", "OR", "NOT", "NOR", "NAND", "hidden_trigger"];

/// Event-level keys the model accepts without executing.
const EVENT_META_KEYS: &[&str] = &[
	"id",
	"title",
	"desc",
	"picture",
	"hidden",
	"is_triggered_only",
];

/// The scope aliases the model resolves.
const ALIASES: &[&str] = &["THIS", "ROOT", "PREV", "FROM"];

/// Identity of one offline run, mirroring [`crate::compile::RunContext`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InterpretOptions {
	pub run_id: RunId,
	/// Mod under test; recorded so a result file is readable on its own.
	pub mod_name: String,
	/// Record each case's run trace (observer mode).
	pub observe: bool,
}

/// One case's offline verdict. Deliberately not a [`crate::judge::Status`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
	/// Every expectation held in the model.
	Pass,
	/// No expectation; the steps completed in the model.
	Smoke,
	/// An expectation did not hold in the model.
	Fail,
	/// A fixture precondition did not hold in the model.
	SetupError,
	/// The case uses something the model refuses to approximate.
	Unsupported,
}

/// A condition that did not hold, with what the model observed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClauseFailure {
	/// Check label, matching the online layer's names (`expect.2.0`).
	pub check: String,
	pub span: SourceSpan,
	pub message: String,
}

/// One observed moment of a model run: what the model did, on which
/// calendar day, in which country's scope. The model knows its own state,
/// so an offline run can report the whole trace — the one thing the online
/// layer structurally cannot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
	pub date: GameDate,
	pub scope: Tag,
	pub event: Observed,
}

/// What the model did at one observed moment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Observed {
	CaseStart,
	/// A case step boundary: `effect`, `fire`, `advance_days` or `expect`.
	Step {
		index: usize,
		kind: String,
	},
	EventFired {
		event: EventId,
	},
	EventScheduled {
		event: EventId,
		due: GameDate,
	},
	EventDelivered {
		event: EventId,
	},
	CountryFlagSet {
		flag: String,
	},
	CountryFlagCleared {
		flag: String,
	},
	GlobalFlagSet {
		flag: String,
	},
	GlobalFlagCleared {
		flag: String,
	},
	VariableSet {
		which: String,
		value: f64,
	},
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterpretedCase {
	pub index: CaseIndex,
	pub node: NodeId,
	pub content_id: ContentId,
	pub origin: SourceSpan,
	pub start: Start,
	pub outcome: Outcome,
	/// Why the case was refused, when it was.
	pub unsupported: Vec<String>,
	pub failures: Vec<ClauseFailure>,
	/// Labels of the checks that held.
	pub passed: Vec<String>,
	pub notes: Vec<String>,
	/// The observed run trace, when [`InterpretOptions::observe`] is set.
	pub trace: Vec<Observation>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OfflineSummary {
	pub pass: usize,
	pub smoke: usize,
	pub fail: usize,
	pub setup_error: usize,
	pub unsupported: usize,
	pub skipped: usize,
	pub ignored: usize,
	pub deselected: usize,
}

/// The result of interpreting a whole plan offline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Interpretation {
	pub layer: &'static str,
	/// Whether the catalog's primitives are pinned to decompiled handlers.
	pub evidence: EvidenceStatus,
	pub run_id: RunId,
	pub mod_name: String,
	pub results: Vec<InterpretedCase>,
	pub not_run: Vec<NotRun>,
}

impl Interpretation {
	pub fn summary(&self) -> OfflineSummary {
		let mut summary = OfflineSummary::default();
		for result in &self.results {
			match result.outcome {
				Outcome::Pass => summary.pass += 1,
				Outcome::Smoke => summary.smoke += 1,
				Outcome::Fail => summary.fail += 1,
				Outcome::SetupError => summary.setup_error += 1,
				Outcome::Unsupported => summary.unsupported += 1,
			}
		}
		for not_run in &self.not_run {
			match &not_run.reason {
				NotRunReason::Deselected => summary.deselected += 1,
				NotRunReason::Skipped { .. } => summary.skipped += 1,
				NotRunReason::Ignored { .. } => summary.ignored += 1,
			}
		}
		summary
	}

	/// 0 when something was interpreted and nothing failed in the model. A
	/// run where every case was refused is 1: the author asked for offline
	/// feedback and got none.
	pub fn exit_code(&self) -> i32 {
		let summary = self.summary();
		let interpreted = summary.pass + summary.smoke + summary.fail + summary.setup_error;
		i32::from(interpreted == 0 || summary.fail > 0 || summary.setup_error > 0)
	}

	pub fn to_json(&self) -> String {
		serde_json::to_string_pretty(self).expect("interpretations serialize")
	}

	/// JUnit XML for CI display, mirroring [`crate::report::Report::to_junit`].
	/// Unsupported cases are `<skipped>`: they did not run.
	pub fn to_junit(&self) -> String {
		let summary = self.summary();
		let skipped = summary.skipped + summary.ignored + summary.unsupported;
		let tests = self.results.len() + summary.skipped + summary.ignored;
		let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
		let _ = writeln!(
			xml,
			"<testsuites><testsuite name=\"foch-offline\" tests=\"{tests}\" failures=\"{}\" \
			 errors=\"{}\" skipped=\"{skipped}\" time=\"0.000\">",
			summary.fail, summary.setup_error
		);
		for result in &self.results {
			let _ = write!(
				xml,
				"<testcase classname=\"{}\" name=\"{}\" time=\"0.000\">",
				escape(result.node.path.as_str()),
				escape(&result.node.local())
			);
			let detail: Vec<String> = result
				.failures
				.iter()
				.map(|failure| format!("{}: {}", failure.check, failure.message))
				.chain(result.unsupported.iter().cloned())
				.chain(result.notes.iter().cloned())
				.collect();
			let detail = escape(&detail.join("\n"));
			match result.outcome {
				Outcome::Fail => {
					let _ = write!(
						xml,
						"<failure message=\"{:?}\">{detail}</failure>",
						result.outcome
					);
				}
				Outcome::SetupError => {
					let _ = write!(
						xml,
						"<error message=\"{:?}\">{detail}</error>",
						result.outcome
					);
				}
				Outcome::Unsupported => {
					let _ = write!(
						xml,
						"<skipped message=\"{:?}\">{detail}</skipped>",
						result.outcome
					);
				}
				_ => {}
			}
			xml.push_str("</testcase>\n");
		}
		for not_run in &self.not_run {
			let message = match &not_run.reason {
				NotRunReason::Deselected => continue,
				NotRunReason::Skipped { message } | NotRunReason::Ignored { message } => {
					message.clone().unwrap_or_default()
				}
			};
			let _ = writeln!(
				xml,
				"<testcase classname=\"{}\" name=\"{}\"><skipped message=\"{}\"/></testcase>",
				escape(not_run.node.path.as_str()),
				escape(&not_run.node.local()),
				escape(&message)
			);
		}
		xml.push_str("</testsuite></testsuites>\n");
		xml
	}
}

/// Interpret every planned case against the offline model. Pure computation:
/// the caller supplies the plan, the project facts and the semantics
/// catalog; nothing is read, launched or guessed.
pub fn interpret(
	plan: &Plan,
	facts: &ProjectFacts,
	catalog: &SemanticsCatalog,
	options: &InterpretOptions,
) -> Interpretation {
	let interpreter = Interpreter {
		facts,
		catalog,
		events: plan.events.iter().map(|event| (&event.id, event)).collect(),
		scripted: plan
			.scripted
			.iter()
			.map(|definition| (definition.name.as_str(), definition))
			.collect(),
		observe: options.observe,
	};
	Interpretation {
		layer: LAYER,
		evidence: catalog.status,
		run_id: options.run_id.clone(),
		mod_name: options.mod_name.clone(),
		results: plan
			.cases
			.iter()
			.map(|planned| interpreter.case(planned))
			.collect(),
		not_run: plan.not_run.clone(),
	}
}

/// The offline model of one case's world: only what authored script can
/// establish. Numeric country statistics are absent on purpose; without the
/// base snapshot their starting values are unknown, so the model refuses to
/// read or write them instead of inventing numbers. `scheduled` holds events
/// delayed by `country_event ... days = N`, delivered when the calendar
/// advances; same-day deliveries run in scheduling order, which matches the
/// online layer's caveat that same-day ordering is not guaranteed.
struct World {
	date: GameDate,
	countries: BTreeMap<Tag, CountryState>,
	global_flags: BTreeSet<String>,
	scheduled: BTreeMap<GameDate, Vec<(EventId, ScopeCtx)>>,
	deliveries: usize,
	/// Observer mode: the run trace, recorded only when asked for.
	trace: Vec<Observation>,
	recording: bool,
}

impl World {
	fn note(&mut self, scope: &Tag, event: Observed) {
		if self.recording {
			self.trace.push(Observation {
				date: self.date,
				scope: scope.clone(),
				event,
			});
		}
	}
}

#[derive(Default)]
struct CountryState {
	flags: BTreeSet<String>,
	variables: BTreeMap<String, f64>,
}

impl World {
	/// Reads never invent state: a country no authored statement has touched
	/// has no flags and no variables.
	fn country(&self, tag: &Tag) -> &CountryState {
		self.countries.get(tag).unwrap_or(&MISSING_COUNTRY)
	}

	fn country_mut(&mut self, tag: &Tag) -> &mut CountryState {
		self.countries.entry(tag.clone()).or_default()
	}
}

/// The stand-in for untouched countries, so reads stay empty.
static MISSING_COUNTRY: CountryState = CountryState {
	flags: BTreeSet::new(),
	variables: BTreeMap::new(),
};

/// The country scope chain while executing: the current country, the
/// enclosing one (`PREV`), the chain's root (`ROOT`, the case's start tag)
/// and the country a fired event came from (`FROM`).
#[derive(Clone, Debug, Eq, PartialEq)]
struct ScopeCtx {
	current: Tag,
	prev: Option<Tag>,
	root: Tag,
	from: Option<Tag>,
}

impl ScopeCtx {
	fn start(tag: &Tag) -> Self {
		Self {
			current: tag.clone(),
			prev: None,
			root: tag.clone(),
			from: None,
		}
	}

	/// Enter a scope block targeting `tag`: PREV becomes the country left.
	fn enter(&self, tag: &Tag) -> Self {
		Self {
			current: tag.clone(),
			prev: Some(self.current.clone()),
			root: self.root.clone(),
			from: self.from.clone(),
		}
	}

	/// The scope a fired event runs in: the same country, with `FROM` set to
	/// the firing scope's country and no enclosing `PREV`.
	fn fired(&self, firing: &Tag) -> Self {
		Self {
			current: self.current.clone(),
			prev: None,
			root: self.root.clone(),
			from: Some(firing.clone()),
		}
	}

	fn resolve(&self, alias: &str) -> Option<Tag> {
		match alias {
			"THIS" => Some(self.current.clone()),
			"ROOT" => Some(self.root.clone()),
			"PREV" => self.prev.clone(),
			"FROM" => self.from.clone(),
			_ => None,
		}
	}
}

/// What a scope-shaped key names: an alias or a country tag.
#[derive(Clone, Copy, PartialEq)]
enum ScopeKey {
	Alias(&'static str),
	Tag,
}

/// Whether `key` opens a scope block in this context. `NOT`/`AND` are logic
/// keywords no country uses; `NOR` is Norway in effect blocks and logic in
/// condition blocks, exactly as the lint's footprint over-approximation
/// decides.
fn scope_key(key: &str, context: Context) -> Option<ScopeKey> {
	if let Some(alias) = ALIASES.iter().find(|alias| **alias == key) {
		return Some(ScopeKey::Alias(alias));
	}
	if matches!(key, "NOT" | "AND") {
		return None;
	}
	if context == Context::Condition && key == "NOR" {
		return None;
	}
	Tag::parse(key).is_ok().then_some(ScopeKey::Tag)
}

/// Static scope knowledge the scan carries, so PREV/FROM resolvability is
/// refused before execution rather than mid-run.
#[derive(Clone, Copy)]
struct ScanScope {
	/// Scope blocks entered since the case or event body started.
	depth: usize,
	/// Whether execution is inside a fired event (so FROM resolves).
	in_event: bool,
}

impl ScanScope {
	fn enter(&self) -> Self {
		Self {
			depth: self.depth + 1,
			in_event: self.in_event,
		}
	}

	fn event(&self) -> Self {
		Self {
			depth: 0,
			in_event: true,
		}
	}
}

struct Interpreter<'a> {
	facts: &'a ProjectFacts,
	catalog: &'a SemanticsCatalog,
	events: BTreeMap<&'a EventId, &'a EventInfo>,
	/// The mod's scripted definitions, inlined wherever their names appear in
	/// the matching kind of block.
	scripted: BTreeMap<&'a str, &'a ScriptedDef>,
	/// Observer mode: record each case's run trace.
	observe: bool,
}

/// Which kind of block a statement list belongs to.
#[derive(Clone, Copy, PartialEq)]
enum Context {
	Effect,
	Condition,
}

impl<'a> Interpreter<'a> {
	fn case(&self, planned: &PlannedCase) -> InterpretedCase {
		let case = &planned.case;
		let mut notes = Vec::new();
		if case.marks.xfail.is_some() {
			notes.push(
				"xfail expects divergence from the game; the offline layer reports the raw \
				 modeled verdict"
					.into(),
			);
		}
		if case.marks.requires_player {
			notes.push(
				"the model has no player identity; the case is interpreted as its start tag".into(),
			);
		}
		if case.ai == AiMode::On {
			// The model executes authored script only; whether computer-
			// controlled countries act is a runner concern that cannot change
			// what the authored statements do, so this is a note, not a
			// refusal.
			notes.push(
				"ai = on: the model does not simulate computer-controlled decisions; the 				 verdict covers authored script only"
					.into(),
			);
		}

		let unsupported = |unsupported: Vec<String>,
		                   failures: Vec<ClauseFailure>,
		                   passed: Vec<String>,
		                   trace: Vec<Observation>| InterpretedCase {
			index: planned.index,
			node: case.node.clone(),
			content_id: case.content_id.clone(),
			origin: case.origin.clone(),
			start: case.start.clone(),
			outcome: Outcome::Unsupported,
			unsupported,
			failures,
			passed,
			notes: notes.clone(),
			trace,
		};

		// Refusal before any execution: scan everything the case would run and
		// report every reason at once.
		let mut reasons = Vec::new();
		self.scan_case(planned, &mut reasons);
		if !reasons.is_empty() {
			return unsupported(reasons, Vec::new(), Vec::new(), Vec::new());
		}

		let mut world = World {
			date: case.start.date,
			countries: BTreeMap::new(),
			global_flags: BTreeSet::new(),
			scheduled: BTreeMap::new(),
			deliveries: 0,
			trace: Vec::new(),
			recording: self.observe,
		};
		world.note(&case.start.tag, Observed::CaseStart);
		let root = ScopeCtx::start(&case.start.tag);
		let mut failures = Vec::new();
		let mut passed = Vec::new();
		for fixture in &planned.fixtures {
			if let Some(block) = &fixture.effect
				&& let Err(reason) = self.exec_block(&mut world, &root, block)
			{
				return unsupported(
					vec![reason],
					failures,
					passed,
					std::mem::take(&mut world.trace),
				);
			}
			let mut setup_failed = false;
			for (index, clause) in fixture.check.iter().enumerate() {
				let label = format!("fixture.{}.{}", fixture.name, index);
				match self.eval_clause(&world, &root, clause) {
					Ok(true) => passed.push(label),
					Ok(false) => {
						setup_failed = true;
						failures.push(ClauseFailure {
							check: label,
							span: clause.span.clone(),
							message: "fixture precondition does not hold in the offline model"
								.into(),
						});
					}
					Err(reason) => {
						return unsupported(
							vec![reason],
							failures,
							passed,
							std::mem::take(&mut world.trace),
						);
					}
				}
			}
			if setup_failed {
				return InterpretedCase {
					outcome: Outcome::SetupError,
					unsupported: Vec::new(),
					failures,
					passed,
					notes,
					trace: std::mem::take(&mut world.trace),
					index: planned.index,
					node: case.node.clone(),
					content_id: case.content_id.clone(),
					origin: case.origin.clone(),
					start: case.start.clone(),
				};
			}
		}

		for (step, case_step) in case.steps.iter().enumerate() {
			let kind = match case_step {
				Step::Effect { .. } => "effect",
				Step::Fire { .. } => "fire",
				Step::AdvanceDays { .. } => "advance_days",
				Step::Expect { .. } => "expect",
			};
			world.note(
				&case.start.tag,
				Observed::Step {
					index: step,
					kind: kind.into(),
				},
			);
			match case_step {
				Step::Effect { block } => {
					if let Err(reason) = self.exec_block(&mut world, &root, block) {
						return unsupported(
							vec![reason],
							failures,
							passed,
							std::mem::take(&mut world.trace),
						);
					}
				}
				Step::Fire { event, .. } => {
					let mut visited = BTreeSet::new();
					if let Err(reason) = self.fire_event(&mut world, &root, event, &mut visited) {
						return unsupported(
							vec![reason],
							failures,
							passed,
							std::mem::take(&mut world.trace),
						);
					}
				}
				Step::AdvanceDays { days, .. } => {
					if let Err(reason) = self.advance(&mut world, *days) {
						return unsupported(
							vec![reason],
							failures,
							passed,
							std::mem::take(&mut world.trace),
						);
					}
				}
				Step::Expect { clauses } => {
					for (index, clause) in clauses.iter().enumerate() {
						let label = format!("expect.{step}.{index}");
						match self.eval_clause(&world, &root, clause) {
							Ok(true) => passed.push(label),
							Ok(false) => {
								failures.push(self.clause_failure(label, clause, &world, &root))
							}
							Err(reason) => {
								return unsupported(
									vec![reason],
									failures,
									passed,
									std::mem::take(&mut world.trace),
								);
							}
						}
					}
				}
			}
		}
		let outcome = if !failures.is_empty() {
			Outcome::Fail
		} else if case.is_smoke() {
			Outcome::Smoke
		} else {
			Outcome::Pass
		};
		InterpretedCase {
			index: planned.index,
			node: case.node.clone(),
			content_id: case.content_id.clone(),
			origin: case.origin.clone(),
			start: case.start.clone(),
			outcome,
			unsupported: Vec::new(),
			failures,
			passed,
			notes,
			trace: world.trace,
		}
	}

	/// Advance the calendar and deliver every event due by the new date, in
	/// scheduling order within a date.
	fn advance(&self, world: &mut World, days: u32) -> Result<(), String> {
		let new_date = world.date.add_days(days)?;
		while let Some((due, batch)) = world.scheduled.pop_first()
			&& due <= new_date
		{
			world.date = due;
			for (event, ctx) in batch {
				world.deliveries += 1;
				if world.deliveries > MAX_EVENT_DELIVERIES {
					return Err(format!(
						"the case schedules more than {MAX_EVENT_DELIVERIES} event deliveries; \
						 the model refuses to follow an event storm"
					));
				}
				world.note(
					&ctx.current,
					Observed::EventDelivered {
						event: event.clone(),
					},
				);
				let mut visited = BTreeSet::new();
				self.fire_event(world, &ctx, &event, &mut visited)?;
			}
		}
		world.date = new_date;
		Ok(())
	}

	/// Execute a collected event's `immediate` block in `ctx`.
	fn fire_event(
		&self,
		world: &mut World,
		ctx: &ScopeCtx,
		event: &EventId,
		visited: &mut BTreeSet<String>,
	) -> Result<(), String> {
		if !visited.insert(event.to_string()) {
			return Err(format!(
				"event {event} fires itself through immediate blocks; the model refuses the cycle"
			));
		}
		let info = self
			.events
			.get(event)
			.ok_or_else(|| format!("fires {event}, whose body was not collected"))?;
		world.note(
			&ctx.current,
			Observed::EventFired {
				event: event.clone(),
			},
		);
		for statement in parse(&info.body) {
			let AstStatement::Assignment { key, value, .. } = statement else {
				continue;
			};
			if key == "immediate"
				&& let AstValue::Block { items, .. } = value
			{
				let fired = ctx.fired(&ctx.current);
				self.exec_statements(world, &fired, &items, &info.origin, visited)?;
			}
		}
		Ok(())
	}

	/// Collect every reason the case cannot be interpreted, before running
	/// anything, so a refusal lists all of its causes at once.
	fn scan_case(&self, planned: &PlannedCase, reasons: &mut Vec<String>) {
		let scope = ScanScope {
			depth: 0,
			in_event: false,
		};
		// Event bodies already scanned: each is validated once per case, so
		// chains (including delayed ones) cannot recurse forever. Immediate
		// cycles are refused at execution, with the same message.
		let mut seen = BTreeSet::new();
		for fixture in &planned.fixtures {
			if let Some(block) = &fixture.effect {
				self.scan_block(block, Context::Effect, scope, &mut seen, reasons);
			}
			for clause in &fixture.check {
				self.scan_clause(clause, scope, &mut seen, reasons);
			}
		}
		for step in &planned.case.steps {
			match step {
				Step::Effect { block } => {
					self.scan_block(block, Context::Effect, scope, &mut seen, reasons)
				}
				Step::Expect { clauses } => {
					for clause in clauses {
						self.scan_clause(clause, scope, &mut seen, reasons);
					}
				}
				Step::Fire { event, span } => {
					self.scan_event(event, span, scope, &mut seen, reasons);
				}
				Step::AdvanceDays { .. } => {}
			}
		}
	}

	fn scan_block(
		&self,
		block: &NativeBlock,
		context: Context,
		scope: ScanScope,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		self.scan_statements(
			&parse(&block.text),
			&block.span,
			context,
			scope,
			seen,
			reasons,
		);
	}

	fn scan_clause(
		&self,
		clause: &Clause,
		scope: ScanScope,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		self.scan_statements(
			&parse(&clause.text),
			&clause.span,
			Context::Condition,
			scope,
			seen,
			reasons,
		);
	}

	/// Scan a collected event's executable content: `immediate` runs; option
	/// content beyond a name and any other event key refuse.
	fn scan_event(
		&self,
		event: &EventId,
		span: &SourceSpan,
		scope: ScanScope,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		if !seen.insert(event.to_string()) {
			return;
		}
		let Some(info) = self.events.get(event) else {
			reasons.push(format!(
				"{span}: fires {event}, whose body was not collected"
			));
			return;
		};
		if !info.invocable {
			reasons.push(format!(
				"{span}: {event} must declare hidden = yes and is_triggered_only = yes to be fired"
			));
			return;
		}
		for statement in parse(&info.body) {
			let AstStatement::Assignment { key, value, .. } = statement else {
				continue;
			};
			let AstValue::Block { items, .. } = value else {
				continue;
			};
			if key == "immediate" {
				self.scan_statements(
					&items,
					&info.origin,
					Context::Effect,
					scope.event(),
					seen,
					reasons,
				);
			} else if key == "option" {
				// A hidden event does not execute option content (verified
				// against EU4 1.37.5 native Windows, 2026-11-05), so it is
				// neither scanned nor run.
			} else if !EVENT_META_KEYS.contains(&key.as_str()) {
				reasons.push(format!(
					"{span}: event {event} has {key}, which the offline model does not execute"
				));
			}
		}
	}

	fn scan_statements(
		&self,
		statements: &[AstStatement],
		where_: &SourceSpan,
		context: Context,
		scope: ScanScope,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		for statement in statements {
			match statement {
				AstStatement::Comment { .. } => continue,
				AstStatement::Item {
					value:
						AstValue::Scalar {
							value: ScalarValue::Identifier(name),
							..
						},
					..
				} => self.scan_scripted_name(name, context, scope, where_, seen, reasons),
				AstStatement::Item { .. } => reasons.push(format!(
					"{where_}: a statement without a key cannot be interpreted offline"
				)),
				AstStatement::Assignment { key, value, .. } => {
					self.scan_assignment(key, value, context, scope, where_, seen, reasons)
				}
			}
		}
	}

	#[allow(clippy::too_many_arguments)]
	fn scan_assignment(
		&self,
		key: &str,
		value: &AstValue,
		context: Context,
		scope: ScanScope,
		where_: &SourceSpan,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		let control = match context {
			Context::Effect => EFFECT_CONTROL,
			Context::Condition => CONDITION_CONTROL,
		};
		if control.contains(&key) {
			let AstValue::Block { items, .. } = value else {
				return;
			};
			if context == Context::Effect && (key == "if" || key == "else_if") {
				self.scan_conditional(items, key, where_, scope, seen, reasons);
			} else {
				self.scan_statements(items, where_, context, scope, seen, reasons);
			}
			return;
		}
		if let Some(target) = scope_key(key, context) {
			let AstValue::Block { items, .. } = value else {
				reasons.push(format!("{where_}: {key} must open a scope block"));
				return;
			};
			if let ScopeKey::Alias(alias) = target {
				let resolvable = match alias {
					"PREV" => scope.depth > 0,
					"FROM" => scope.in_event,
					_ => true,
				};
				if !resolvable {
					reasons.push(format!(
						"{where_}: {alias} has no scope to name at this position"
					));
				}
			}
			self.scan_statements(items, where_, context, scope.enter(), seen, reasons);
			return;
		}
		if context == Context::Effect && (key == "random" || key == "random_list") {
			if let Err(reason) = self.catalog.permits(key) {
				reasons.push(self.refusal(key, &reason, context, where_));
				return;
			}
			let AstValue::Block { items, .. } = value else {
				reasons.push(format!("{where_}: {key} must be a block"));
				return;
			};
			if key == "random" {
				match split_chance(items) {
					Some((chance, body)) => {
						if !(chance == 0.0 || chance == 100.0) {
							reasons.push(format!(
								"{where_}: random with chance {chance} needs the game's RNG; 								 the offline model only decides the constant corners 0 and 100"
							));
						}
						self.scan_statements(&body, where_, context, scope, seen, reasons);
					}
					None => reasons.push(format!(
						"{where_}: random needs a constant chance to be decided offline"
					)),
				}
			} else {
				for (guard, body) in branches(items) {
					if !guard.is_empty() {
						self.scan_statements(
							&guard,
							where_,
							Context::Condition,
							scope,
							seen,
							reasons,
						);
					}
					self.scan_statements(&body, where_, context, scope, seen, reasons);
				}
			}
			return;
		}
		if context == Context::Effect && key == "country_event" {
			if let Err(reason) = self.catalog.permits(key) {
				reasons.push(self.refusal(key, &reason, context, where_));
			} else {
				self.scan_country_event(value, where_, scope, seen, reasons);
			}
			return;
		}
		if let Some(definition) = self.scripted_in(key, context) {
			// A scripted definition inlines its body; the kind must fit the
			// block. Cycles are caught at execution, as event chains are.
			if !seen.insert(format!("scripted:{key}")) {
				return;
			}
			self.scan_statements(
				&parse(&definition.body),
				&definition.origin,
				context,
				scope,
				seen,
				reasons,
			);
			return;
		}
		if let Some(definition) = self.scripted.get(key) {
			let kind = match definition.kind {
				ScriptedKind::Trigger => "trigger",
				ScriptedKind::Effect => "effect",
			};
			let wanted = match context {
				Context::Effect => "effects",
				Context::Condition => "conditions",
			};
			reasons.push(format!(
				"{where_}: {key} is a scripted {kind} and cannot run in a block of {wanted}"
			));
			return;
		}
		// A modeled primitive's block holds operands (which/value), not
		// nested script; its shape is validated at execution.
		if let Err(reason) = self.catalog.permits(key) {
			reasons.push(self.refusal(key, &reason, context, where_));
		}
	}

	/// A bare identifier statement names a scripted definition; anything else
	/// is not script the model reads.
	fn scan_scripted_name(
		&self,
		name: &str,
		context: Context,
		scope: ScanScope,
		where_: &SourceSpan,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		if let Some(definition) = self.scripted_in(name, context) {
			if !seen.insert(format!("scripted:{name}")) {
				return;
			}
			self.scan_statements(
				&parse(&definition.body),
				&definition.origin,
				context,
				scope,
				seen,
				reasons,
			);
			return;
		}
		if let Some(definition) = self.scripted.get(name) {
			let kind = match definition.kind {
				ScriptedKind::Trigger => "trigger",
				ScriptedKind::Effect => "effect",
			};
			let wanted = match context {
				Context::Effect => "effects",
				Context::Condition => "conditions",
			};
			reasons.push(format!(
				"{where_}: {name} is a scripted {kind} and cannot run in a block of {wanted}"
			));
			return;
		}
		let kind = match context {
			Context::Effect => "effect",
			Context::Condition => "trigger",
		};
		reasons.push(format!(
			"{where_}: {name} is not a known {kind} or scripted definition"
		));
	}

	/// The scripted definition named `key` when its kind fits this context.
	fn scripted_in<'d>(&'d self, key: &str, context: Context) -> Option<&'d ScriptedDef> {
		let wanted = match context {
			Context::Effect => ScriptedKind::Effect,
			Context::Condition => ScriptedKind::Trigger,
		};
		match self.scripted.get(key) {
			Some(definition) if definition.kind == wanted => Some(*definition),
			_ => None,
		}
	}

	/// A `country_event = { id = ... days = ... }` effect: the target must be
	/// a collected invocable event, whose immediate content is scanned too.
	fn scan_country_event(
		&self,
		value: &AstValue,
		where_: &SourceSpan,
		scope: ScanScope,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		match country_event_args(value) {
			Ok(args) => self.scan_event(&args.event, where_, scope, seen, reasons),
			Err(reason) => reasons.push(format!("{where_}: {reason}")),
		}
	}

	/// An `if`/`else_if` block: `limit` is a condition, the rest are effects.
	fn scan_conditional(
		&self,
		items: &[AstStatement],
		key: &str,
		where_: &SourceSpan,
		scope: ScanScope,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		let mut limits = 0;
		for statement in items {
			match statement {
				AstStatement::Comment { .. } => continue,
				AstStatement::Assignment { key, value, .. } if key == "limit" => {
					limits += 1;
					let AstValue::Block { items, .. } = value else {
						reasons.push(format!("{where_}: limit must be a block"));
						continue;
					};
					self.scan_statements(items, where_, Context::Condition, scope, seen, reasons);
				}
				other => self.scan_statements(
					std::slice::from_ref(other),
					where_,
					Context::Effect,
					scope,
					seen,
					reasons,
				),
			}
		}
		if limits != 1 {
			reasons.push(format!("{where_}: {key} needs exactly one limit block"));
		}
	}

	/// The refusal message for a key the catalog does not permit: a game
	/// primitive the model declines to approximate, an unknown name, or a
	/// catalog entry missing its decompiled evidence.
	fn refusal(
		&self,
		key: &str,
		catalog_reason: &str,
		context: Context,
		where_: &SourceSpan,
	) -> String {
		if self.catalog.primitives.contains_key(key) {
			// A catalog entry missing handler evidence in a pinned catalog.
			return format!("{where_}: {catalog_reason}");
		}
		let kind = match context {
			Context::Effect => "effect",
			Context::Condition => "trigger",
		};
		let catalog = match context {
			Context::Effect => &self.facts.effects,
			Context::Condition => &self.facts.triggers,
		};
		if catalog.as_ref().is_some_and(|names| names.contains(key)) {
			format!(
				"{where_}: {key} is a known game {kind} the offline model refuses to approximate"
			)
		} else {
			format!("{where_}: {key} is not a known {kind}")
		}
	}

	/// Execute one authored effect block in the given scope.
	fn exec_block(
		&self,
		world: &mut World,
		ctx: &ScopeCtx,
		block: &NativeBlock,
	) -> Result<(), String> {
		let mut visited = BTreeSet::new();
		self.exec_statements(world, ctx, &parse(&block.text), &block.span, &mut visited)
	}

	fn exec_statements(
		&self,
		world: &mut World,
		ctx: &ScopeCtx,
		statements: &[AstStatement],
		where_: &SourceSpan,
		visited: &mut BTreeSet<String>,
	) -> Result<(), String> {
		let mut index = 0;
		while index < statements.len() {
			match &statements[index] {
				AstStatement::Comment { .. } => {
					index += 1;
				}
				AstStatement::Item {
					value:
						AstValue::Scalar {
							value: ScalarValue::Identifier(name),
							..
						},
					..
				} => {
					let definition = self.scripted_in(name, Context::Effect).ok_or_else(|| {
						format!(
							"{where_}: {name} reached execution without passing the offline scan"
						)
					})?;
					if !visited.insert(format!("scripted:{name}")) {
						return Err(format!(
							"{where_}: {name} inlines itself through scripted definitions; the 							 model refuses the cycle"
						));
					}
					self.exec_statements(
						world,
						ctx,
						&parse(&definition.body),
						&definition.origin,
						visited,
					)?;
					index += 1;
				}
				AstStatement::Item { .. } => {
					index += 1;
				}
				AstStatement::Assignment { key, value, .. } if key == "if" => {
					let AstValue::Block { items, .. } = value else {
						return Err(format!("{where_}: if must be a block"));
					};
					let mut chain = index + 1;
					while chain < statements.len()
						&& matches!(
							&statements[chain],
							AstStatement::Assignment { key, .. }
								if key == "else_if" || key == "else"
						) {
						chain += 1;
					}
					self.exec_conditional(
						world,
						ctx,
						items,
						&statements[index + 1..chain],
						where_,
						visited,
					)?;
					index = chain;
				}
				AstStatement::Assignment { key, value, .. } => {
					self.exec_assignment(world, ctx, key, value, where_, visited)?;
					index += 1;
				}
			}
		}
		Ok(())
	}

	/// An `if` block plus the `else_if`/`else` blocks that follow it.
	fn exec_conditional(
		&self,
		world: &mut World,
		ctx: &ScopeCtx,
		items: &[AstStatement],
		chain: &[AstStatement],
		where_: &SourceSpan,
		visited: &mut BTreeSet<String>,
	) -> Result<(), String> {
		let (limit, body) = split_limit(items)
			.ok_or_else(|| format!("{where_}: if needs exactly one limit block"))?;
		if self.eval_statements(world, ctx, &limit, where_, 0)? {
			return self.exec_statements(world, ctx, &body, where_, visited);
		}
		for alternative in chain {
			let AstStatement::Assignment { key, value, .. } = alternative else {
				continue;
			};
			let AstValue::Block { items, .. } = value else {
				continue;
			};
			if key == "else" {
				return self.exec_statements(world, ctx, items, where_, visited);
			}
			let (limit, body) = split_limit(items)
				.ok_or_else(|| format!("{where_}: else_if needs exactly one limit block"))?;
			if self.eval_statements(world, ctx, &limit, where_, 0)? {
				return self.exec_statements(world, ctx, &body, where_, visited);
			}
		}
		Ok(())
	}

	fn exec_assignment(
		&self,
		world: &mut World,
		ctx: &ScopeCtx,
		key: &str,
		value: &AstValue,
		where_: &SourceSpan,
		visited: &mut BTreeSet<String>,
	) -> Result<(), String> {
		if let Some(target) = scope_key(key, Context::Effect) {
			let AstValue::Block { items, .. } = value else {
				return Err(format!("{where_}: {key} must open a scope block"));
			};
			let tag = match target {
				ScopeKey::Alias(alias) => ctx.resolve(alias).ok_or_else(|| {
					format!("{where_}: {alias} has no scope to name at this position")
				})?,
				ScopeKey::Tag => Tag::parse(key).expect("scope keys parse as tags"),
			};
			return self.exec_statements(world, &ctx.enter(&tag), items, where_, visited);
		}
		match key {
			"set_country_flag" | "clr_country_flag" => {
				let flag =
					name_of(value).ok_or_else(|| format!("{where_}: {key} needs a flag name"))?;
				if key == "set_country_flag" {
					world.note(
						&ctx.current,
						Observed::CountryFlagSet {
							flag: flag.to_string(),
						},
					);
					world
						.country_mut(&ctx.current)
						.flags
						.insert(flag.to_string());
				} else {
					world.note(
						&ctx.current,
						Observed::CountryFlagCleared {
							flag: flag.to_string(),
						},
					);
					world.country_mut(&ctx.current).flags.remove(flag);
				}
			}
			"set_global_flag" | "clr_global_flag" => {
				let flag =
					name_of(value).ok_or_else(|| format!("{where_}: {key} needs a flag name"))?;
				if key == "set_global_flag" {
					world.note(
						&ctx.current,
						Observed::GlobalFlagSet {
							flag: flag.to_string(),
						},
					);
					world.global_flags.insert(flag.to_string());
				} else {
					world.note(
						&ctx.current,
						Observed::GlobalFlagCleared {
							flag: flag.to_string(),
						},
					);
					world.global_flags.remove(flag);
				}
			}
			"set_variable" | "change_variable" => {
				let (which, amount) = variable_args(value)
					.ok_or_else(|| format!("{where_}: {key} needs which = ... and value = ..."))?;
				let value = {
					let country = world.country_mut(&ctx.current);
					if key == "set_variable" {
						country.variables.insert(which.to_string(), amount);
					} else {
						*country.variables.entry(which.to_string()).or_insert(0.0) += amount;
					}
					country.variables.get(which).copied().unwrap_or(0.0)
				};
				world.note(
					&ctx.current,
					Observed::VariableSet {
						which: which.to_string(),
						value,
					},
				);
			}
			"country_event" => {
				let args =
					country_event_args(value).map_err(|reason| format!("{where_}: {reason}"))?;
				if args.days == 0 {
					self.fire_event(world, ctx, &args.event, visited)?;
				} else {
					let due = world
						.date
						.add_days(args.days)
						.map_err(|reason| format!("{where_}: {reason}"))?;
					world.note(
						&ctx.current,
						Observed::EventScheduled {
							event: args.event.clone(),
							due,
						},
					);
					let fired = ctx.fired(&ctx.current);
					world
						.scheduled
						.entry(due)
						.or_default()
						.push((args.event, fired));
				}
			}
			"log" => {}
			"hidden_effect" | "custom_tooltip" | "tooltip" => {
				if let AstValue::Block { items, .. } = value {
					self.exec_statements(world, ctx, items, where_, visited)?;
				}
			}
			"random" => {
				let AstValue::Block { items, .. } = value else {
					return Err(format!("{where_}: random must be a block"));
				};
				// Only the deterministic corners of the chance roll are
				// decidable: 100 always runs, 0 never does.
				let (chance, body) = split_chance(items)
					.ok_or_else(|| format!("{where_}: random needs a constant chance"))?;
				match chance {
					100.0 => return self.exec_statements(world, ctx, &body, where_, visited),
					0.0 => {}
					other => {
						return Err(format!(
							"{where_}: random with chance {other} needs the game's RNG; the 							 offline model only decides the constant corners 0 and 100"
						));
					}
				}
			}
			"random_list" => {
				let AstValue::Block { items, .. } = value else {
					return Err(format!("{where_}: random_list must be a block"));
				};
				// With exactly one surviving branch the roll has a single
				// outcome; anything else needs the game's RNG.
				let mut surviving = Vec::new();
				for (limit, body) in branches(items) {
					if limit.is_empty() || self.eval_statements(world, ctx, &limit, where_, 0)? {
						surviving.push(body);
					}
				}
				match surviving.len() {
					1 => return self.exec_statements(world, ctx, &surviving[0], where_, visited),
					0 => {
						return Err(format!(
							"{where_}: no random_list branch survives its trigger, and the model 							 does not know the game's fallback"
						));
					}
					count => {
						return Err(format!(
							"{where_}: random_list with {count} surviving branches needs the 							 game's RNG; the model only runs when exactly one branch's trigger 							 holds"
						));
					}
				}
			}
			other => {
				if let Some(definition) = self.scripted_in(other, Context::Effect) {
					if !visited.insert(format!("scripted:{other}")) {
						return Err(format!(
							"{where_}: {other} inlines itself through scripted definitions; the 							 model refuses the cycle"
						));
					}
					return self.exec_statements(
						world,
						ctx,
						&parse(&definition.body),
						&definition.origin,
						visited,
					);
				}
				return Err(format!(
					"{where_}: {key} reached execution without passing the offline scan"
				));
			}
		}
		Ok(())
	}

	/// Evaluate one clause of an `expect` or fixture `check` block. A clause
	/// is a conjunction of its statements, as in the online layer.
	fn eval_clause(&self, world: &World, ctx: &ScopeCtx, clause: &Clause) -> Result<bool, String> {
		self.eval_statements(world, ctx, &parse(&clause.text), &clause.span, 0)
	}

	fn eval_statements(
		&self,
		world: &World,
		ctx: &ScopeCtx,
		statements: &[AstStatement],
		where_: &SourceSpan,
		depth: u32,
	) -> Result<bool, String> {
		if depth > MAX_SCRIPTED_DEPTH {
			return Err(format!(
				"{where_}: scripted triggers inline more than {MAX_SCRIPTED_DEPTH} levels deep"
			));
		}
		for statement in statements {
			match statement {
				AstStatement::Assignment { key, value, .. } => {
					if self.eval_assignment(world, ctx, key, value, where_, depth)? {
						continue;
					}
					return Ok(false);
				}
				AstStatement::Item {
					value:
						AstValue::Scalar {
							value: ScalarValue::Identifier(name),
							..
						},
					..
				} => {
					let definition =
						self.scripted_in(name, Context::Condition).ok_or_else(|| {
							format!("{where_}: {name} reached evaluation without passing the scan")
						})?;
					if !self.eval_statements(
						world,
						ctx,
						&parse(&definition.body),
						&definition.origin,
						depth + 1,
					)? {
						return Ok(false);
					}
				}
				_ => {}
			}
		}
		Ok(true)
	}

	fn eval_assignment(
		&self,
		world: &World,
		ctx: &ScopeCtx,
		key: &str,
		value: &AstValue,
		where_: &SourceSpan,
		depth: u32,
	) -> Result<bool, String> {
		if depth > MAX_SCRIPTED_DEPTH {
			return Err(format!(
				"{where_}: scripted triggers inline more than {MAX_SCRIPTED_DEPTH} levels deep"
			));
		}
		if !CONDITION_CONTROL.contains(&key)
			&& let Some(target) = scope_key(key, Context::Condition)
		{
			let AstValue::Block { items, .. } = value else {
				return Err(format!("{where_}: {key} must open a scope block"));
			};
			let tag = match target {
				ScopeKey::Alias(alias) => ctx.resolve(alias).ok_or_else(|| {
					format!("{where_}: {alias} has no scope to name at this position")
				})?,
				ScopeKey::Tag => Tag::parse(key).expect("scope keys parse as tags"),
			};
			return self.eval_statements(world, &ctx.enter(&tag), items, where_, depth + 1);
		}
		let country = world.country(&ctx.current);
		Ok(match key {
			"always" => {
				boolean(value).ok_or_else(|| format!("{where_}: always needs yes or no"))?
			}
			"has_country_flag" => {
				let flag = name_of(value)
					.ok_or_else(|| format!("{where_}: has_country_flag needs a flag name"))?;
				country.flags.contains(flag)
			}
			"has_global_flag" => {
				let flag = name_of(value)
					.ok_or_else(|| format!("{where_}: has_global_flag needs a flag name"))?;
				world.global_flags.contains(flag)
			}
			"has_variable" => {
				let name = name_of(value)
					.ok_or_else(|| format!("{where_}: has_variable needs a variable name"))?;
				country.variables.contains_key(name)
			}
			"check_variable" => {
				let (which, wanted) = variable_args(value).ok_or_else(|| {
					format!("{where_}: check_variable needs which = ... and value = ...")
				})?;
				let observed = country.variables.get(which).copied().unwrap_or(0.0);
				observed >= wanted
			}
			"tag" => {
				let wanted =
					name_of(value).ok_or_else(|| format!("{where_}: tag needs a country tag"))?;
				ctx.current.as_str() == wanted
			}
			"AND" | "OR" | "NOT" | "NOR" | "NAND" | "hidden_trigger" => {
				let AstValue::Block { items, .. } = value else {
					return Err(format!("{where_}: {key} must be a block"));
				};
				let mut verdicts = Vec::new();
				for statement in items {
					let AstStatement::Assignment { key, value, .. } = statement else {
						continue;
					};
					verdicts.push(self.eval_assignment(
						world,
						ctx,
						key,
						value,
						where_,
						depth + 1,
					)?);
				}
				match key {
					"OR" => verdicts.iter().any(|&holds| holds),
					"NOT" => !verdicts.iter().all(|&holds| holds),
					"NOR" => !verdicts.iter().any(|&holds| holds),
					"NAND" => !verdicts.iter().all(|&holds| holds),
					_ => verdicts.iter().all(|&holds| holds),
				}
			}
			other => {
				if let Some(definition) = self.scripted_in(other, Context::Condition) {
					return self.eval_statements(
						world,
						ctx,
						&parse(&definition.body),
						&definition.origin,
						depth + 1,
					);
				}
				return Err(format!(
					"{where_}: {key} reached evaluation without passing the offline scan"
				));
			}
		})
	}

	/// The failure report for one clause, including what the model observed.
	fn clause_failure(
		&self,
		check: String,
		clause: &Clause,
		world: &World,
		ctx: &ScopeCtx,
	) -> ClauseFailure {
		let message = self.observation(clause, world, ctx);
		ClauseFailure {
			check,
			span: clause.span.clone(),
			message,
		}
	}

	/// What the model saw when a clause did not hold. Stated as model state,
	/// never as a game value.
	fn observation(&self, clause: &Clause, world: &World, ctx: &ScopeCtx) -> String {
		for statement in parse(&clause.text) {
			let AstStatement::Assignment { key, value, .. } = &statement else {
				continue;
			};
			let AstValue::Scalar { value, .. } = value else {
				if key == "check_variable"
					&& let Some((which, wanted)) = variable_args(value)
				{
					let observed = world
						.country(&ctx.current)
						.variables
						.get(which)
						.copied()
						.unwrap_or(0.0);
					return format!(
						"the model has {which} = {observed} on {}, expected at least {wanted}",
						ctx.current
					);
				}
				continue;
			};
			let text = value.as_text();
			return match key.as_str() {
				"has_country_flag" => format!("the model has no country flag {text}"),
				"has_global_flag" => format!("the model has no global flag {text}"),
				"has_variable" => format!("the model has no variable {text}"),
				"tag" => format!("the case runs as {} at this point, not {text}", ctx.current),
				"always" => format!("always = {text}"),
				_ => "the clause does not hold in the offline model".into(),
			};
		}
		"the clause does not hold in the offline model".into()
	}
}

/// The constant `chance` and body of a `random` block; `None` unless the
/// chance is a plain number.
fn split_chance(items: &[AstStatement]) -> Option<(f64, Vec<AstStatement>)> {
	let mut chance = None;
	let mut body = Vec::new();
	for statement in items {
		match statement {
			AstStatement::Assignment { key, value, .. } if key == "chance" => {
				let AstValue::Scalar {
					value: ScalarValue::Number(text),
					..
				} = value
				else {
					return None;
				};
				chance = text.parse().ok();
			}
			AstStatement::Comment { .. } => {}
			other => body.push(other.clone()),
		}
	}
	chance.map(|chance| (chance, body))
}

/// The weighted branches of a `random_list` block: each numeric-keyed block
/// contributes its optional `trigger` limit and its effect body.
fn branches(items: &[AstStatement]) -> Vec<(Vec<AstStatement>, Vec<AstStatement>)> {
	let mut branches = Vec::new();
	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		if key.bytes().any(|byte| !byte.is_ascii_digit()) {
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		branches.push(split_guard(items));
	}
	branches
}

/// The `trigger` guard block and the effect body of a random_list branch.
fn split_guard(items: &[AstStatement]) -> (Vec<AstStatement>, Vec<AstStatement>) {
	let mut guard = Vec::new();
	let mut body = Vec::new();
	for statement in items {
		match statement {
			AstStatement::Assignment { key, value, .. } if key == "trigger" => {
				if let AstValue::Block { items, .. } = value {
					guard = items.clone();
				}
			}
			AstStatement::Comment { .. } => {}
			other => body.push(other.clone()),
		}
	}
	(guard, body)
}

/// `limit` block and body of an `if`/`else_if` block; `None` unless exactly
/// one `limit` is present.
fn split_limit(items: &[AstStatement]) -> Option<(Vec<AstStatement>, Vec<AstStatement>)> {
	let mut limit: Option<&[AstStatement]> = None;
	let mut body = Vec::new();
	for statement in items {
		match statement {
			AstStatement::Assignment { key, value, .. } if key == "limit" => {
				let AstValue::Block { items, .. } = value else {
					return None;
				};
				if limit.is_some() {
					return None;
				}
				limit = Some(items);
			}
			AstStatement::Comment { .. } => {}
			other => body.push(other.clone()),
		}
	}
	limit.map(|limit| (limit.to_vec(), body))
}

fn parse(text: &str) -> Vec<AstStatement> {
	parse_clausewitz_content(crate::game_path("interpret.txt"), text)
		.ast
		.statements
}

/// A flag or variable name as authored: an identifier or a quoted string.
fn name_of(value: &AstValue) -> Option<&str> {
	match value {
		AstValue::Scalar {
			value: ScalarValue::Identifier(text) | ScalarValue::String(text),
			..
		} => Some(text),
		_ => None,
	}
}

fn boolean(value: &AstValue) -> Option<bool> {
	match value {
		AstValue::Scalar {
			value: ScalarValue::Bool(wanted),
			..
		} => Some(*wanted),
		AstValue::Scalar {
			value: ScalarValue::Identifier(text),
			..
		} => match text.as_str() {
			"yes" => Some(true),
			"no" => Some(false),
			_ => None,
		},
		_ => None,
	}
}

/// `which = <name> value = <number>` from a set_variable, change_variable or
/// check_variable block.
fn variable_args(value: &AstValue) -> Option<(&str, f64)> {
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	let mut which = None;
	let mut amount = None;
	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		match key.as_str() {
			"which" => which = name_of(value),
			"value" => {
				amount = match value {
					AstValue::Scalar {
						value: ScalarValue::Number(text),
						..
					} => text.parse().ok(),
					_ => None,
				};
			}
			_ => {}
		}
	}
	Some((which?, amount?))
}

/// The `id` and optional `days` of a `country_event = { ... }` effect.
fn country_event_args(value: &AstValue) -> Result<FireArgs, String> {
	let AstValue::Block { items, .. } = value else {
		return Err("country_event needs an id block".into());
	};
	let mut event = None;
	let mut days = 0;
	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		match key.as_str() {
			"id" => {
				event = Some(EventId::parse(
					name_of(value).ok_or("country_event id must be a name")?,
				)?);
			}
			"days" => {
				days = match value {
					AstValue::Scalar {
						value: ScalarValue::Number(text),
						..
					} => text.parse::<u32>().map_err(|_| {
						"country_event days must be a non-negative number".to_string()
					})?,
					_ => return Err("country_event days must be a number".into()),
				};
			}
			other => {
				return Err(format!(
					"country_event does not take {other}; the offline model refuses unverified \
					 forms"
				));
			}
		}
	}
	Ok(FireArgs {
		event: event.ok_or("country_event needs an id")?,
		days,
	})
}

struct FireArgs {
	event: EventId,
	days: u32,
}

fn escape(text: &str) -> String {
	text.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
		.replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::RunId;
	use crate::plan::{ExpandOptions, GroupOptions};
	use crate::source::{RelPath, SourceFile, SourceKind};
	use crate::{collect, expand, group, lint};

	fn file(path: &str, kind: SourceKind, text: &str) -> SourceFile {
		SourceFile {
			mod_name: "Offline Mod".into(),
			path: RelPath::new(path).unwrap(),
			kind,
			text: text.into(),
		}
	}

	fn interpret_sources(
		sources: &[SourceFile],
		facts: &ProjectFacts,
		catalog: &SemanticsCatalog,
	) -> Interpretation {
		let collection = collect(sources);
		assert!(
			collection.diagnostics.is_empty(),
			"{:#?}",
			collection.diagnostics
		);
		let expanded = expand(&collection, &ExpandOptions::default()).unwrap();
		let linted = lint(&expanded, facts);
		let plan = group(
			expanded,
			&linted,
			&GroupOptions {
				runner_shared_sessions: false,
				..GroupOptions::default()
			},
		);
		interpret(
			&plan,
			facts,
			catalog,
			&InterpretOptions {
				run_id: RunId::parse("run-1").unwrap(),
				mod_name: "Offline Mod".into(),
				observe: false,
			},
		)
	}

	fn outcome(text: &str) -> Interpretation {
		interpret_sources(
			&[file("events/cases.txt", SourceKind::Inline, text)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		)
	}

	fn case<'a>(result: &'a Interpretation, name: &str) -> &'a InterpretedCase {
		result
			.results
			.iter()
			.find(|case| case.node.name.to_string().contains(name))
			.unwrap_or_else(|| panic!("no case named {name} in {:?}", result.results))
	}

	/// Inline annotations in front of [`EVENT`], avoiding brace escaping in
	/// `format!` strings.
	fn annotated(annotations: &str) -> Interpretation {
		outcome(&format!("{annotations}\n{EVENT}"))
	}

	/// Event definitions in the mod's `events/`, `test =` blocks in `tests/`:
	/// the layout each kind of source must have to collect.
	fn scenario(events: &str, tests: &str) -> Interpretation {
		interpret_sources(
			&[
				file(
					"events/cases.txt",
					SourceKind::Inline,
					&format!("namespace = cases\n{events}"),
				),
				file("tests/scenario.txt", SourceKind::TestsDir, tests),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		)
	}

	const EVENT: &str = r#"country_event = {
	id = cases.1
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { set_country_flag = called }
	option = { name = OK }
}"#;

	#[test]
	fn flags_pass_fail_and_smoke_offline() {
		let result = annotated(
			r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=smoke)
#test(time=1444.11.11, tag=SWE, name=flag_set, expect={ has_country_flag = called })
#test(time=1444.11.11, tag=SWE, name=flag_missing, expect={ has_country_flag = never_set })
#test(time=1444.11.11, tag=SWE, name=delayed, advance_days=2, expect={ has_country_flag = called })"#,
		);
		assert_eq!(case(&result, "smoke").outcome, Outcome::Smoke);
		let set = case(&result, "flag_set");
		assert_eq!(set.outcome, Outcome::Pass);
		// Inline steps are [fire, expect], so the expect clause is step 1.
		assert_eq!(set.passed, ["expect.1.0"]);
		let missing = case(&result, "flag_missing");
		assert_eq!(missing.outcome, Outcome::Fail);
		assert_eq!(missing.failures[0].check, "expect.1.0");
		assert_eq!(
			missing.failures[0].message,
			"the model has no country flag never_set"
		);
		// Flags do not decay with the calendar, so a delayed read is known.
		assert_eq!(case(&result, "delayed").outcome, Outcome::Pass);
		assert_eq!(result.summary().pass, 2);
		assert_eq!(result.exit_code(), 1);
	}

	#[test]
	fn variables_and_logic_are_modeled() {
		let result = annotated(
			r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=vars,
#     effect={ set_variable = { which = progress value = 3 } change_variable = { which = progress value = 2 } },
#     expect={ check_variable = { which = progress value = 4 } has_variable = progress NOT = { has_variable = missing } })"#,
		);
		let vars = case(&result, "vars");
		assert_eq!(vars.outcome, Outcome::Pass, "{:#?}", vars.failures);
		assert_eq!(vars.passed.len(), 3);
	}

	#[test]
	fn check_variable_reports_the_modeled_value() {
		let result = annotated(
			r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=low,
#     effect={ set_variable = { which = progress value = 3 } },
#     expect={ check_variable = { which = progress value = 4 } })"#,
		);
		let low = case(&result, "low");
		assert_eq!(low.outcome, Outcome::Fail);
		assert_eq!(
			low.failures[0].message,
			"the model has progress = 3 on SWE, expected at least 4"
		);
	}

	#[test]
	fn if_control_flow_executes_the_matching_branch() {
		let result = annotated(
			r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=branch,
#     effect={ if = { limit = { always = no } set_country_flag = wrong } else_if = { limit = { always = yes } set_country_flag = right } else = { set_country_flag = fallback } },
#     expect={ has_country_flag = right NOT = { has_country_flag = wrong } NOT = { has_country_flag = fallback } })"#,
		);
		let branch = case(&result, "branch");
		assert_eq!(branch.outcome, Outcome::Pass, "{:#?}", branch.failures);
	}

	const CHAIN: &str = r#"country_event = {
	id = cases.2
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { FROM = { set_country_flag = from_firer } }
	option = { name = OK }
}"#;

	const RELAY: &str = r#"country_event = {
	id = cases.3
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { country_event = { id = cases.2 } }
	option = { name = OK }
}"#;

	#[test]
	fn tag_scopes_switch_the_modeled_country() {
		const CASE: &str = r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=neighborhood,
#     effect={ DAN = { set_country_flag = danish_flag } },
#     expect={ DAN = { has_country_flag = danish_flag } NOT = { has_country_flag = danish_flag } })"#;
		let result = outcome(&format!("{CASE}\n{EVENT}"));
		let scoped = case(&result, "neighborhood");
		assert_eq!(scoped.outcome, Outcome::Pass, "{:#?}", scoped.failures);
	}

	#[test]
	fn aliases_resolve_against_the_scope_chain() {
		const CASE: &str = r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=aliases,
#     effect={ DAN = { set_country_flag = seen_by_dan PREV = { set_country_flag = prev_swe } } ROOT = { set_country_flag = root_swe } },
#     expect={ has_country_flag = called DAN = { has_country_flag = seen_by_dan } ROOT = { has_country_flag = root_swe } })"#;
		let result = outcome(&format!("{CASE}\n{EVENT}"));
		let aliases = case(&result, "aliases");
		assert_eq!(aliases.outcome, Outcome::Pass, "{:#?}", aliases.failures);
	}

	#[test]
	fn fired_events_see_their_firer_through_from() {
		const CHAIN_TEST: &str = r#"test = { name = chain time = 1444.11.11 tag = SWE fire = cases.3 expect = { has_country_flag = from_firer } }"#;
		let result = scenario(&format!("{RELAY}\n{CHAIN}"), CHAIN_TEST);
		// cases.3's immediate fires cases.2, whose FROM names the firing
		// country (SWE); the flag lands there.
		let chain = case(&result, "chain");
		assert_eq!(chain.outcome, Outcome::Pass, "{:#?}", chain.failures);
	}

	#[test]
	fn delayed_events_are_delivered_by_advance_days() {
		const TIMER: &str = r#"country_event = {
	id = cases.4
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { country_event = { id = cases.1 days = 2 } }
	option = { name = OK }
}"#;
		const SAME_DAY: &str = r#"test = {
	name = same_day
	time = 1444.11.11
	tag = SWE
	fire = cases.4
	expect = { has_country_flag = called }
}"#;
		const AFTER: &str = r#"test = {
	name = after_due
	time = 1444.11.11
	tag = SWE
	fire = cases.4
	advance_days = 2
	expect = { has_country_flag = called }
}"#;
		let result = scenario(
			&format!("{EVENT}\n{TIMER}"),
			&format!("{SAME_DAY}\n{AFTER}"),
		);
		assert_eq!(
			case(&result, "same_day").outcome,
			Outcome::Fail,
			"the delayed event has not fired on the scheduling day"
		);
		let after = case(&result, "after_due");
		assert_eq!(after.outcome, Outcome::Pass, "{:#?}", after.failures);
	}

	#[test]
	fn immediate_event_cycles_are_refused_up_front() {
		let sources = [file(
			"events/cases.txt",
			SourceKind::Inline,
			r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=loop)
country_event = { id = cases.1 hidden = yes is_triggered_only = yes immediate = { country_event = { id = cases.1 } } }
"#,
		)];
		let result = interpret_sources(
			&sources,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let refused = case(&result, "loop");
		assert_eq!(refused.outcome, Outcome::Unsupported);
		assert!(
			refused.unsupported[0].contains("fires itself"),
			"{:#?}",
			refused.unsupported
		);
	}

	#[test]
	fn a_scheduled_event_storm_is_refused_instead_of_looping() {
		const PING: &str = r#"country_event = {
	id = cases.5
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { country_event = { id = cases.6 days = 1 } }
	option = { name = OK }
}"#;
		const PONG: &str = r#"country_event = {
	id = cases.6
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { country_event = { id = cases.5 days = 1 } }
	option = { name = OK }
}"#;
		const STORM: &str = r#"test = {
	name = storm
	time = 1444.11.11
	tag = SWE
	fire = cases.5
	advance_days = 10000
	expect = { always = yes }
}"#;
		let result = scenario(&format!("{PING}\n{PONG}"), STORM);
		let storm = case(&result, "storm");
		assert_eq!(storm.outcome, Outcome::Unsupported);
		assert!(
			storm.unsupported[0].contains("event deliveries"),
			"{:#?}",
			storm.unsupported
		);
	}

	#[test]
	fn unmodeled_primitives_refuse_with_all_reasons() {
		let facts = ProjectFacts {
			effects: Some(
				["add_adm_power".to_string(), "every_country".to_string()]
					.into_iter()
					.collect(),
			),
			triggers: Some(["is_at_war".to_string()].into_iter().collect()),
			..ProjectFacts::default()
		};
		let sources = [file(
			"events/cases.txt",
			SourceKind::Inline,
			r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=numeric, effect={ add_adm_power = 50 }, expect={ adm_power = 50 })
country_event = { id = cases.1 hidden = yes is_triggered_only = yes immediate = {} }
#test(time=1444.11.11, tag=SWE, name=war, expect={ is_at_war = yes })
country_event = { id = cases.2 hidden = yes is_triggered_only = yes immediate = {} }
#test(time=1444.11.11, tag=SWE, name=iteration, effect={ every_country = { set_country_flag = x } })
country_event = { id = cases.3 hidden = yes is_triggered_only = yes immediate = {} }
"#,
		)];
		let result = interpret_sources(&sources, &facts, &SemanticsCatalog::embedded());
		let numeric = case(&result, "numeric");
		assert_eq!(numeric.outcome, Outcome::Unsupported);
		assert!(
			numeric
				.unsupported
				.iter()
				.any(|reason| reason.contains("add_adm_power is a known game effect")),
			"{:#?}",
			numeric.unsupported
		);
		assert!(
			numeric
				.unsupported
				.iter()
				.any(|reason| reason.contains("adm_power is not a known trigger")),
			"{:#?}",
			numeric.unsupported
		);
		let iteration = case(&result, "iteration");
		assert_eq!(iteration.outcome, Outcome::Unsupported);
		assert!(
			iteration.unsupported[0].contains("every_country"),
			"{:#?}",
			iteration.unsupported
		);
		assert_eq!(result.summary().unsupported, 3);
		assert_eq!(result.exit_code(), 1, "nothing was interpreted");
	}

	#[test]
	fn a_pinned_catalog_refuses_primitives_without_evidence() {
		let text = r#"{
			"format": 1,
			"status": "pinned",
			"game_version": "1.37.5",
			"binary_sha256": "abc",
			"primitives": {
				"set_country_flag": {
					"kind": "effect",
					"facts": {},
					"handler": {
						"symbol": "CCountry::SetFlag",
						"address": 16,
						"end": 64,
						"pseudocode_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
					}
				},
				"has_country_flag": {
					"kind": "trigger",
					"facts": {},
					"handler": {
						"symbol": "CCountry::HasFlag",
						"address": 80,
						"end": 128,
						"pseudocode_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
					}
				},
				"always": {"kind": "trigger", "facts": {}, "handler": null}
			}
		}"#;
		let pinned = SemanticsCatalog::parse(text).unwrap();
		const PINNED: &str = r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=pinned_case, expect={ has_country_flag = called })"#;
		const UNPINNED: &str = r#"#test(time=1444.11.11, tag=SWE, name=unpinned_case, expect={ always = no })
country_event = { id = cases.2 hidden = yes is_triggered_only = yes immediate = {} }"#;
		let text = format!("{PINNED}\n{EVENT}\n{UNPINNED}");
		let sources = [file("events/cases.txt", SourceKind::Inline, &text)];
		let result = interpret_sources(&sources, &ProjectFacts::default(), &pinned);
		let pinned_case = case(&result, "pinned_case");
		assert_eq!(pinned_case.outcome, Outcome::Pass, "{:#?}", pinned_case);
		let unpinned = case(&result, "unpinned_case");
		assert_eq!(unpinned.outcome, Outcome::Unsupported);
		assert!(
			unpinned.unsupported[0].contains("always has no decompiled handler evidence"),
			"{:#?}",
			unpinned.unsupported
		);
	}

	/// Verified against the real game on 2026-11-05 (EU4 1.37.5, native
	/// Windows): firing a hidden event does not execute its option content,
	/// so the model agrees — the flag the option would set is absent.
	#[test]
	fn hidden_event_options_do_not_execute() {
		const OPTIONED: &str = r#"country_event = {
	id = cases.7
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { set_country_flag = from_immediate }
	option = { name = A set_country_flag = from_option }
}"#;
		const CASES: &str = r#"test = { name = option_probe time = 1444.11.11 tag = SWE fire = cases.7 expect = { has_country_flag = from_immediate NOT = { has_country_flag = from_option } } }"#;
		let result = scenario(
			&format!(
				"{EVENT}
{OPTIONED}"
			),
			CASES,
		);
		let probe = case(&result, "option_probe");
		assert_eq!(probe.outcome, Outcome::Pass, "{:#?}", probe);
		// The differential twin: the option's flag alone is absent.
		const LONELY: &str = r#"test = { name = option_alone time = 1444.11.11 tag = SWE fire = cases.7 expect = { has_country_flag = from_option } }"#;
		let lonely = scenario(
			&format!(
				"{EVENT}
{OPTIONED}"
			),
			LONELY,
		);
		assert_eq!(
			case(&lonely, "option_alone").outcome,
			Outcome::Fail,
			"the model must agree with the game: option content does not run"
		);
	}

	#[test]
	fn missing_targets_refuse() {
		let sources = [file(
			"tests/scenario.txt",
			SourceKind::TestsDir,
			"test = { name = fires_absent time = 1444.11.11 tag = SWE fire = absent.9 }",
		)];
		let result = interpret_sources(
			&sources,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let foreign = case(&result, "fires_absent");
		assert_eq!(foreign.outcome, Outcome::Unsupported);
		assert!(
			foreign.unsupported[0].contains("fires absent.9"),
			"{:#?}",
			foreign.unsupported
		);
	}

	#[test]
	fn country_event_refuses_unverified_shapes() {
		let sources = [
			file(
				"events/cases.txt",
				SourceKind::Inline,
				r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=tooltip_form)
country_event = { id = cases.1 hidden = yes is_triggered_only = yes immediate = {} }
"#,
			),
			file(
				"tests/scenario.txt",
				SourceKind::TestsDir,
				r#"test = { name = odd_form time = 1444.11.11 tag = SWE effect = { country_event = { id = cases.1 tooltip = x } } }"#,
			),
		];
		let result = interpret_sources(
			&sources,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let odd = case(&result, "odd_form");
		assert_eq!(odd.outcome, Outcome::Unsupported);
		assert!(
			odd.unsupported[0].contains("does not take tooltip"),
			"{:#?}",
			odd.unsupported
		);
	}

	#[test]
	fn fixtures_prepare_and_check_before_steps() {
		const USES_FIXTURE: &str = r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=uses_fixture, use={ prepared }, expect={ has_country_flag = prepared })"#;
		let sources = [
			file(
				"events/cases.txt",
				SourceKind::Inline,
				&format!("{USES_FIXTURE}\n{EVENT}"),
			),
			file(
				"tests/fixtures.txt",
				SourceKind::TestsDir,
				r#"fixture = {
	name = prepared
	effect = { set_country_flag = prepared }
	check = { has_country_flag = prepared }
}
fixture = {
	name = broken
	effect = { clr_country_flag = nothing }
	check = { has_country_flag = absent_flag }
}
"#,
			),
			file(
				"tests/scenario.txt",
				SourceKind::TestsDir,
				r#"test = {
	name = broken_setup
	time = 1444.11.11
	tag = SWE
	use = { broken }
	expect = { always = yes }
}
"#,
			),
		];
		let result = interpret_sources(
			&sources,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let uses = case(&result, "uses_fixture");
		assert_eq!(uses.outcome, Outcome::Pass, "{:#?}", uses.failures);
		assert_eq!(uses.passed[0], "fixture.prepared.0");
		let broken = case(&result, "broken_setup");
		assert_eq!(broken.outcome, Outcome::SetupError);
		assert_eq!(broken.failures[0].check, "fixture.broken.0");
	}

	#[test]
	fn xfail_and_ai_on_make_no_offline_claim() {
		let result = annotated(
			r#"namespace = cases
#xfail(reason="known bug", strict=yes)
#test(time=1444.11.11, tag=SWE, name=known_bug, expect={ always = no })
#test(time=1444.11.11, tag=SWE, name=wants_ai, ai=on, expect={ always = yes })"#,
		);
		let known_bug = case(&result, "known_bug");
		assert_eq!(known_bug.outcome, Outcome::Fail);
		assert!(
			known_bug.notes.iter().any(|note| note.contains("xfail")),
			"{:#?}",
			known_bug.notes
		);
		let wants_ai = case(&result, "wants_ai");
		assert_eq!(wants_ai.outcome, Outcome::Pass);
		assert!(
			wants_ai.notes.iter().any(|note| note.contains("ai = on")),
			"{:#?}",
			wants_ai.notes
		);
	}

	#[test]
	fn not_run_cases_pass_through() {
		let result = annotated(
			r#"namespace = cases
#skip(reason="needs war")
#test(time=1444.11.11, tag=SWE, name=needs_war, expect={ is_at_war = yes })"#,
		);
		assert_eq!(result.summary().skipped, 1);
		assert!(
			result.not_run[0]
				.node
				.name
				.to_string()
				.contains("needs_war")
		);
	}

	#[test]
	fn interpretation_is_deterministic_and_self_labeled() {
		let first = annotated(
			r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=stable, expect={ has_country_flag = called })"#,
		);
		let second = annotated(
			r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=stable, expect={ has_country_flag = called })"#,
		);
		assert_eq!(first, second);
		assert_eq!(first.layer, "offline");
		assert!(matches!(first.evidence, EvidenceStatus::Unpinned));
		let json: serde_json::Value = serde_json::from_str(&first.to_json()).unwrap();
		assert_eq!(json["layer"], "offline");
		assert_eq!(json["evidence"], "unpinned");
		assert_eq!(json["results"][0]["outcome"], "pass");
	}

	#[test]
	fn junit_marks_what_did_not_run_as_skipped() {
		let result = annotated(
			r#"namespace = cases
#skip(reason="needs war")
#test(time=1444.11.11, tag=SWE, name=needs_war, expect={ is_at_war = yes })"#,
		);
		let xml = result.to_junit();
		assert!(xml.contains("<testsuite name=\"foch-offline\""), "{xml}");
		assert!(xml.contains("<skipped"), "{xml}");
	}

	#[test]
	fn scripted_effects_and_triggers_inline() {
		const EVENTS: &str = r#"country_event = {
	id = cases.8
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { my_prep }
	option = { name = OK }
}"#;
		const CASES: &str = r#"test = {
	name = scripted_case
	time = 1444.11.11
	tag = SWE
	fire = cases.8
	expect = { is_ready }
}"#;
		let result = interpret_sources(
			&[
				file("events/cases.txt", SourceKind::Inline, EVENTS),
				file(
					"common/scripted_effects/prep.txt",
					SourceKind::Scripted,
					"my_prep = { set_country_flag = ready }",
				),
				file(
					"common/scripted_triggers/ready.txt",
					SourceKind::Scripted,
					"is_ready = { has_country_flag = ready }",
				),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let scripted_case = case(&result, "scripted_case");
		assert_eq!(scripted_case.outcome, Outcome::Pass, "{:#?}", scripted_case);
	}

	#[test]
	fn a_scripted_definition_of_the_wrong_kind_refuses() {
		const CASES: &str = r#"test = {
	name = mismatched
	time = 1444.11.11
	tag = SWE
	effect = { is_ready }
	expect = { always = yes }
}"#;
		let result = interpret_sources(
			&[
				file(
					"common/scripted_triggers/ready.txt",
					SourceKind::Scripted,
					"is_ready = { has_country_flag = ready }",
				),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let mismatched = case(&result, "mismatched");
		assert_eq!(mismatched.outcome, Outcome::Unsupported);
		assert!(
			mismatched.unsupported[0]
				.contains("is a scripted trigger and cannot run in a block of effects"),
			"{:#?}",
			mismatched.unsupported
		);
	}

	#[test]
	fn mutually_inlining_scripted_triggers_refuse() {
		const CASES: &str = r#"test = {
	name = cyclic
	time = 1444.11.11
	tag = SWE
	expect = { a_side }
}"#;
		let result = interpret_sources(
			&[
				file(
					"common/scripted_triggers/cycle.txt",
					SourceKind::Scripted,
					"a_side = { b_side }\nb_side = { a_side }",
				),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let cyclic = case(&result, "cyclic");
		assert_eq!(cyclic.outcome, Outcome::Unsupported);
		assert!(
			cyclic.unsupported[0].contains("inline more than"),
			"{:#?}",
			cyclic.unsupported
		);
	}

	#[test]
	fn random_decides_only_its_constant_corners() {
		const CASES: &str = r#"test = {
	name = random_corners
	time = 1444.11.11
	tag = SWE
	effect = { random = { chance = 100 set_country_flag = certain } random = { chance = 0 set_country_flag = never } }
	expect = { has_country_flag = certain NOT = { has_country_flag = never } }
}
test = {
	name = random_middle
	time = 1444.11.11
	tag = SWE
	effect = { random = { chance = 50 set_country_flag = coin } }
	expect = { always = yes }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let corners = case(&result, "random_corners");
		assert_eq!(corners.outcome, Outcome::Pass, "{:#?}", corners);
		let middle = case(&result, "random_middle");
		assert_eq!(middle.outcome, Outcome::Unsupported);
		assert!(
			middle.unsupported[0].contains("needs the game's RNG"),
			"{:#?}",
			middle.unsupported
		);
	}

	#[test]
	fn random_list_runs_when_exactly_one_branch_survives() {
		const CASES: &str = r#"test = {
	name = single_branch
	time = 1444.11.11
	tag = SWE
	effect = { random_list = { 50 = { trigger = { always = no } set_country_flag = lost } 50 = { set_country_flag = won } } }
	expect = { has_country_flag = won NOT = { has_country_flag = lost } }
}
test = {
	name = two_branches
	time = 1444.11.11
	tag = SWE
	effect = { random_list = { 50 = { set_country_flag = left } 50 = { set_country_flag = right } } }
	expect = { always = yes }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let single = case(&result, "single_branch");
		assert_eq!(single.outcome, Outcome::Pass, "{:#?}", single);
		let two = case(&result, "two_branches");
		assert_eq!(two.outcome, Outcome::Unsupported);
		assert!(
			two.unsupported[0].contains("surviving branches")
				&& two.unsupported[0].contains("game's RNG"),
			"{:#?}",
			two.unsupported
		);
	}

	#[test]
	fn observer_mode_records_the_run_trace() {
		const TIMER: &str = r#"country_event = {
	id = cases.4
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { country_event = { id = cases.1 days = 2 } }
	option = { name = OK }
}"#;
		const CASES: &str = r#"test = {
	name = observed
	time = 1444.11.11
	tag = SWE
	fire = cases.4
	advance_days = 2
	expect = { has_country_flag = called }
}"#;
		let sources = [
			file(
				"events/cases.txt",
				SourceKind::Inline,
				&format!("namespace = cases\n{EVENT}\n{TIMER}"),
			),
			file("tests/scenario.txt", SourceKind::TestsDir, CASES),
		];
		// Observe defaults off: nothing is recorded.
		assert!(
			case(
				&interpret_sources(
					&sources,
					&ProjectFacts::default(),
					&SemanticsCatalog::embedded()
				),
				"observed"
			)
			.trace
			.is_empty()
		);

		let collection = collect(&sources);
		assert!(
			collection.diagnostics.is_empty(),
			"{:#?}",
			collection.diagnostics
		);
		let expanded = expand(&collection, &ExpandOptions::default()).unwrap();
		let linted = lint(&expanded, &ProjectFacts::default());
		let plan = group(
			expanded,
			&linted,
			&GroupOptions {
				runner_shared_sessions: false,
				..GroupOptions::default()
			},
		);
		let observed = interpret(
			&plan,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
			&InterpretOptions {
				run_id: RunId::parse("run-1").unwrap(),
				mod_name: "Offline Mod".into(),
				observe: true,
			},
		);
		let trace = &case(&observed, "observed").trace;
		let kinds: Vec<&str> = trace
			.iter()
			.filter_map(|observation| match &observation.event {
				Observed::CaseStart => Some("case_start"),
				Observed::Step { kind, .. } => Some(kind.as_str()),
				Observed::EventFired { .. } => Some("event_fired"),
				Observed::EventScheduled { .. } => Some("event_scheduled"),
				Observed::EventDelivered { .. } => Some("event_delivered"),
				Observed::CountryFlagSet { .. } => Some("country_flag_set"),
				_ => None,
			})
			.collect();
		for expected in [
			"case_start",
			"fire",
			"advance_days",
			"expect",
			"event_fired",
			"event_scheduled",
			"event_delivered",
			"country_flag_set",
		] {
			assert!(
				kinds.contains(&expected),
				"missing {expected:?} in {kinds:?}"
			);
		}
		// The flag lands only after the scheduled delivery.
		let delivery = kinds
			.iter()
			.position(|kind| *kind == "event_delivered")
			.unwrap();
		let flag = kinds
			.iter()
			.position(|kind| *kind == "country_flag_set")
			.unwrap();
		assert!(delivery < flag, "{kinds:?}");
	}

	/// The implemented comparison matches the fact recorded in the catalog,
	/// which a decompilation batch pins.
	#[test]
	fn modeled_comparison_matches_the_catalog_fact() {
		let catalog = SemanticsCatalog::embedded();
		assert_eq!(catalog.fact("check_variable", "comparison"), Some(">="));
		let result = annotated(
			r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=at_least,
#     effect={ set_variable = { which = progress value = 4 } },
#     expect={ check_variable = { which = progress value = 4 } })"#,
		);
		assert_eq!(case(&result, "at_least").outcome, Outcome::Pass);
	}
}
