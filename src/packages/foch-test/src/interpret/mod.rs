//! The offline layer: interpret planned cases against a small, explicit
//! model of EU4 semantics instead of launching the game.
//!
//! The model is organized by [`pins::SemanticsCatalog`]: every primitive the
//! interpreter executes is a catalog entry. A `pinned` catalog additionally
//! ties each entry to a decompiled handler function and refuses anything
//! whose evidence is missing; the embedded seed is `unpinned`, and every
//! result records that its semantics are not evidence-backed. The interpreter is an author-feedback layer, like a
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

mod overlay;
pub mod pins;

use crate::lint::ProjectFacts;
use crate::model::{
	AiMode, CaseIndex, Clause, ContentId, DecisionInfo, EventId, EventInfo, EventKind, GameDate,
	NativeBlock, NodeId, RunId, ScriptedDef, ScriptedKind, Start, Step, Tag,
};
use crate::plan::{NotRun, NotRunReason, Plan, PlannedCase};
use crate::source::SourceSpan;
use foch::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, parse_clausewitz_content,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

pub use overlay::{
	ContentLayer, CoreEdit, CountrySlice, HistoryTimeline, LayerFile, ProvinceSlice,
	SubjectTypeSet, SubjectWarPolicy, definition_facts, fold_history, overlay_decisions,
	overlay_history, overlay_subject_types,
};
pub use pins::{EvidenceStatus, HandlerPin, SemanticsCatalog};

/// Marks every offline result, in JSON and in reports, as its own layer.
pub const LAYER: &str = "offline";

/// Upper bound of scheduled-event deliveries one case may cause, so a
/// self-scheduling event pair cannot run the model forever.
const MAX_EVENT_DELIVERIES: usize = 4096;

/// Upper bound of nested scripted-trigger inlining, so mutually inlining
/// definitions cannot recurse forever.
const MAX_SCRIPTED_DEPTH: u32 = 256;

/// The primitive names the interpreter implements itself, by context.
/// Static on purpose: a catalog that omits an entry cannot un-make a
/// builtin, so a scripted definition never shadows one and a missing entry
/// still refuses at the gate below. Keep in sync with the match arms of
/// `exec_assignment` and `eval_assignment`; the structural passthroughs
/// (`EFFECT_CONTROL`/`CONDITION_CONTROL`) are not catalog primitives and
/// are not listed here.
const BUILTIN_EFFECTS: &[&str] = &[
	"add_base_manpower",
	"add_base_production",
	"add_base_tax",
	"add_core",
	"cede_province",
	"change_variable",
	"clr_country_flag",
	"clr_global_flag",
	"clr_province_flag",
	"country_event",
	"create_march",
	"create_subject",
	"create_union",
	"create_vassal",
	"every_owned_province",
	"every_subject_country",
	"grant_independence",
	"log",
	"province_event",
	"random",
	"release",
	"remove_core",
	"set_country_flag",
	"set_global_flag",
	"set_province_flag",
	"set_variable",
	"vassalize",
];
const BUILTIN_TRIGGERS: &[&str] = &[
	"always",
	"any_owned_province",
	"any_subject_country",
	"base_manpower",
	"base_production",
	"base_tax",
	"check_variable",
	"controlled_by",
	"development",
	"has_country_flag",
	"has_global_flag",
	"has_province_flag",
	"has_variable",
	"is_core",
	"is_subject",
	"is_subject_of",
	"is_subject_of_type",
	"owned_by",
	"owns",
	"owns_core_province",
	"tag",
];

/// Province-only effects. A country scope refuses these instead of guessing.
const PROVINCE_EFFECTS: &[&str] = &[
	"add_base_manpower",
	"add_base_production",
	"add_base_tax",
	"add_core",
	"cede_province",
	"clr_province_flag",
	"province_event",
	"remove_core",
	"set_province_flag",
];

/// Province-only triggers.
const PROVINCE_TRIGGERS: &[&str] = &[
	"base_manpower",
	"base_production",
	"base_tax",
	"controlled_by",
	"development",
	"has_province_flag",
	"is_core",
	"owned_by",
];

/// Effects that do not read or write the current country or province.
const NEUTRAL_EFFECTS: &[&str] = &["clr_global_flag", "log", "random", "set_global_flag"];

/// Triggers that do not read the current country or province.
const NEUTRAL_TRIGGERS: &[&str] = &["always", "has_global_flag"];

/// Whether `key` names a builtin primitive in this context.
fn builtin(key: &str, context: Context) -> bool {
	match context {
		Context::Effect => BUILTIN_EFFECTS.contains(&key),
		Context::Condition => BUILTIN_TRIGGERS.contains(&key),
	}
}

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

/// Player-facing numeric country stats, as (effect key, trigger key). The
/// model starts every slot at `0`, adds accumulate, and the trigger family
/// compares `>=`. The starting value is a modeling choice recorded in the
/// catalog facts and in every trace: whether it matches the game's own
/// start is the author's judgement until a base-snapshot seed lands.
const NUMBER_SLOTS: &[(&str, &str)] = &[
	("add_adm_power", "adm_power"),
	("add_dip_power", "dip_power"),
	("add_mil_power", "mil_power"),
	("add_treasury", "treasury"),
	("add_stability", "stability"),
	("add_prestige", "prestige"),
	("add_legitimacy", "legitimacy"),
	("add_republican_tradition", "republican_tradition"),
	("add_devotion", "devotion"),
	("add_horde_unity", "horde_unity"),
	("add_absolutism", "absolutism"),
	("add_army_tradition", "army_tradition"),
	("add_navy_tradition", "navy_tradition"),
	("add_manpower", "manpower"),
];

/// Console cheat commands the model maps to numeric slots, as (command,
/// slot). Only these are accepted; simulation toggles (`fow`, `ti`,
/// `yesman`, `observe`) and out-of-model cheats refuse with the reason.
/// The built-in runner passes the raw line to the game console instead.
const CHEATS: &[(&str, &str)] = &[
	("cash", "treasury"),
	("adm", "adm_power"),
	("dip", "dip_power"),
	("mil", "mil_power"),
	("prestige", "prestige"),
	("stability", "stability"),
	("manpower", "manpower"),
];

/// What a cheat step does in the model.
pub(crate) enum CheatKind {
	/// Adds to a numeric slot.
	Mapped { slot: &'static str, amount: f64 },
	/// Runs as a no-op in the model; the console owns the real behavior —
	/// including rejecting names it does not know, where the author sees it.
	Passthrough { name: String },
}

/// Classify a cheat command line. Mapped commands change numeric slots;
/// everything else passes through: the framework does not gatekeep the
/// console's command set.
pub(crate) fn cheat_kind(command: &str) -> Result<CheatKind, String> {
	let mut parts = command.split_whitespace();
	let name = parts.next().unwrap_or_default();
	if let Some((_, slot)) = CHEATS.iter().find(|(cheat, _)| *cheat == name) {
		let amount = parts
			.next()
			.and_then(|text| text.parse::<f64>().ok())
			.ok_or_else(|| format!("cheat \"{command}\": needs a number"))?;
		if parts.next().is_some() {
			return Err(format!(
				"cheat \"{command}\": expected one command and one number"
			));
		}
		return Ok(CheatKind::Mapped { slot, amount });
	}
	Ok(CheatKind::Passthrough {
		name: name.to_string(),
	})
}

/// The slot an effect key adds to.
fn number_effect(key: &str) -> Option<&'static str> {
	NUMBER_SLOTS
		.iter()
		.find(|(effect, _)| *effect == key)
		.map(|(_, trigger)| *trigger)
}

/// The slot a trigger key compares.
fn number_trigger(key: &str) -> Option<&'static str> {
	NUMBER_SLOTS
		.iter()
		.find(|(_, trigger)| *trigger == key)
		.map(|(_, trigger)| *trigger)
}

/// The scope aliases the model resolves.
const ALIASES: &[&str] = &["THIS", "ROOT", "PREV", "FROM"];

/// Identity of one offline run, mirroring [`crate::compile::RunContext`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterpretOptions {
	pub run_id: RunId,
	/// Mod under test; recorded so a result file is readable on its own.
	pub mod_name: String,
	/// Record each case's run trace (observer mode).
	pub observe: bool,
	/// Starting state already folded for one date. Ignored when [`Self::history`]
	/// is set, because each case folds the timeline at its own start date.
	pub seed: Option<SeedFacts>,
	/// Dated history from the ordered playset. Each case applies the blocks
	/// on or before its own start date.
	pub history: Option<HistoryTimeline>,
	/// Subject types from the ordered playset. Absent means none resolve;
	/// vanilla type names are not assumed.
	pub subject_types: Option<SubjectTypeSet>,
}

/// Starting state the caller supplies from the game's own history files:
/// the countries existing at the bookmark and their religion, culture,
/// government and rank. Numeric stats stay at the modeled 0 — the history
/// files do not carry them; engine defaults are not foch's to invent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedFacts {
	pub tags: BTreeSet<Tag>,
	pub countries: BTreeMap<Tag, SeededCountry>,
	/// Provinces that survived the history overlay, folded at the same date
	/// as `countries`. Ids are whatever the files named, with no vanilla cap.
	#[serde(default)]
	pub provinces: BTreeMap<u32, ProvinceSeed>,
}

/// A province's scripted slots at one date. Absent fields were not written
/// by the history that applies on that date.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProvinceSeed {
	pub owner: Option<Tag>,
	pub controller: Option<Tag>,
	pub religion: Option<String>,
	pub culture: Option<String>,
	/// Absent when the history that applies never assigned the slot.
	#[serde(default)]
	pub base_tax: Option<i64>,
	#[serde(default)]
	pub base_production: Option<i64>,
	#[serde(default)]
	pub base_manpower: Option<i64>,
	/// `None` means no applied slice mentioned `add_core` or `remove_core`.
	/// An empty set is a known absence of cores.
	#[serde(default)]
	pub cores: Option<BTreeSet<Tag>>,
}

/// A country's scripted starting slots; `None` stays unknown until written.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SeededCountry {
	pub religion: Option<String>,
	pub culture: Option<String>,
	pub government: Option<String>,
	pub government_rank: Option<i64>,
	/// `capital = <province id>` from country history. Absent when unseeded.
	#[serde(default)]
	pub capital: Option<u32>,
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

/// A country serializes as its tag string. A province serializes as an object
/// so an existing country trace still reads as `"scope": "SWE"`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ObservedScope {
	Country(Tag),
	Province { province: u32 },
}

impl fmt::Display for ObservedScope {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Country(tag) => write!(formatter, "{tag}"),
			Self::Province { province } => write!(formatter, "province {province}"),
		}
	}
}

/// One observed moment of a model run: what the model did, on which
/// calendar day, in which scope. The model knows its own state, so an
/// offline run can report the whole trace — the one thing the online layer
/// structurally cannot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
	pub date: GameDate,
	pub scope: ObservedScope,
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
	/// A named slot write: religion, culture, government, government rank.
	SlotSet {
		slot: String,
		value: String,
	},
	WarChanged {
		with: Tag,
		at_war: bool,
	},
	AllianceChanged {
		with: Tag,
		allied: bool,
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
	let mut scripted_effects = BTreeMap::new();
	let mut scripted_triggers = BTreeMap::new();
	for definition in &plan.scripted {
		match definition.kind {
			ScriptedKind::Effect => {
				scripted_effects.insert(definition.name.as_str(), definition);
			}
			ScriptedKind::Trigger => {
				scripted_triggers.insert(definition.name.as_str(), definition);
			}
		}
	}
	let interpreter = Interpreter {
		facts,
		catalog,
		events: plan.events.iter().map(|event| (&event.id, event)).collect(),
		scripted_effects,
		scripted_triggers,
		decisions: plan
			.decisions
			.iter()
			.map(|decision| (decision.name.as_str(), decision))
			.collect(),
		observe: options.observe,
		seed: options.seed.as_ref(),
		history: options.history.as_ref(),
		subject_types: options.subject_types.as_ref(),
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
	/// Wars, stored as the attacker facing each defender; both directions
	/// answer the trigger family (offensive for the attacker,
	/// defensive for the defender).
	wars: BTreeMap<Tag, BTreeSet<Tag>>,
	/// The same wars indexed from the defender: a defender is at war too,
	/// and which side declared stays distinguishable.
	wars_against: BTreeMap<Tag, BTreeSet<Tag>>,
	alliances: BTreeMap<Tag, BTreeSet<Tag>>,
	/// Enum and number slots a case can write; absent means unknown, and an
	/// unwritten read refuses rather than guessing a starting value.
	religions: BTreeMap<Tag, String>,
	cultures: BTreeMap<Tag, String>,
	governments: BTreeMap<Tag, String>,
	government_ranks: BTreeMap<Tag, i64>,
	/// The model world `every_country` iterates: the countries the case has
	/// named, or — when a seed is supplied — every country existing at the
	/// bookmark per the game's own history files.
	named: BTreeSet<Tag>,
	/// Player-facing numeric stats; absent entries read as the modeled 0.
	numbers: BTreeMap<Tag, BTreeMap<&'static str, f64>>,
	/// Subject → overlord and subject type. One country has at most one overlord.
	subjects: BTreeMap<Tag, SubjectLink>,
	/// Provinces the history overlay seeded, plus any a case effect has written.
	/// A missing entry, or a `None` slot, is unknown: reads refuse.
	provinces: BTreeMap<u32, ProvinceState>,
	/// Country capitals seeded from history. Absent means `capital_scope` refuses.
	capitals: BTreeMap<Tag, u32>,
	/// Observer mode: the run trace, recorded only when asked for.
	trace: Vec<Observation>,
	recording: bool,
}

/// A country's overlord and the subject type of that link.
struct SubjectLink {
	overlord: Tag,
	kind: String,
}

impl World {
	fn note(&mut self, scope: &ScopeRef, event: Observed) {
		if self.recording {
			self.trace.push(Observation {
				date: self.date,
				scope: match scope {
					ScopeRef::Country(tag) => ObservedScope::Country(tag.clone()),
					ScopeRef::Province(id) => ObservedScope::Province { province: *id },
				},
				event,
			});
		}
	}

	fn note_tag(&mut self, tag: &Tag, event: Observed) {
		self.note(&ScopeRef::Country(tag.clone()), event);
	}
}

#[derive(Default)]
struct CountryState {
	flags: BTreeSet<String>,
	variables: BTreeMap<String, f64>,
}

/// One province in the model. `None` slots were not seeded and have not been
/// written; reading them refuses. Flags start empty, the same as country flags.
#[derive(Default)]
struct ProvinceState {
	owner: Option<Tag>,
	controller: Option<Tag>,
	base_tax: Option<i64>,
	base_production: Option<i64>,
	base_manpower: Option<i64>,
	cores: Option<BTreeSet<Tag>>,
	flags: BTreeSet<String>,
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

	/// A province the case is writing. Creating the entry does not invent
	/// owner, controller, bases, or cores; those stay `None` until seeded or set.
	fn province_mut(&mut self, id: u32) -> &mut ProvinceState {
		self.provinces.entry(id).or_default()
	}

	/// Add to a seeded base. An absent province or an absent slot refuses.
	fn add_base(&mut self, id: u32, slot: &str, amount: i64) -> Result<(), String> {
		let Some(province) = self.provinces.get_mut(&id) else {
			return Err(format!("{slot} is unknown for province {id}"));
		};
		let cell = match slot {
			"base_tax" => &mut province.base_tax,
			"base_production" => &mut province.base_production,
			"base_manpower" => &mut province.base_manpower,
			_ => unreachable!("base slot {slot}"),
		};
		let Some(current) = cell else {
			return Err(format!("{slot} is unknown for province {id}"));
		};
		*current += amount;
		Ok(())
	}

	/// Provinces whose seeded or written owner is `tag`, in ascending id order.
	fn owned_provinces(&self, tag: &Tag) -> Vec<u32> {
		self.provinces
			.iter()
			.filter(|(_, province)| province.owner.as_ref() == Some(tag))
			.map(|(id, _)| *id)
			.collect()
	}

	/// Subjects of `tag`, in tag order.
	fn subjects_of(&self, tag: &Tag) -> Vec<Tag> {
		self.subjects
			.iter()
			.filter(|(_, link)| &link.overlord == tag)
			.map(|(subject, _)| subject.clone())
			.collect()
	}

	fn overlord_of(&self, tag: &Tag) -> Option<Tag> {
		self.subjects.get(tag).map(|link| link.overlord.clone())
	}

	/// Record `subject` under `overlord`. A link that would make either
	/// country its own overlord, directly or through the chain, is refused.
	fn link_subject(&mut self, subject: Tag, overlord: Tag, kind: String) -> Result<(), String> {
		if subject == overlord {
			return Err(format!("{subject} cannot be its own subject"));
		}
		let mut cursor = Some(overlord.clone());
		let mut guard = 0usize;
		while let Some(tag) = cursor {
			if tag == subject {
				return Err(format!(
					"linking {subject} under {overlord} would cycle the subject chain"
				));
			}
			cursor = self.subjects.get(&tag).map(|link| link.overlord.clone());
			guard += 1;
			if guard > 64 {
				return Err("subject chain is too deep to link".into());
			}
		}
		self.named.insert(subject.clone());
		self.named.insert(overlord.clone());
		self.subjects
			.insert(subject, SubjectLink { overlord, kind });
		Ok(())
	}
}

/// The stand-in for untouched countries, so reads stay empty.
static MISSING_COUNTRY: CountryState = CountryState {
	flags: BTreeSet::new(),
	variables: BTreeMap::new(),
};

/// One scope on the THIS/ROOT/PREV/FROM chain. Country and province scopes
/// share the chain; a primitive still refuses the scope it does not belong to.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ScopeRef {
	Country(Tag),
	Province(u32),
}

impl fmt::Display for ScopeRef {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Country(tag) => write!(formatter, "{tag}"),
			Self::Province(id) => write!(formatter, "province {id}"),
		}
	}
}

/// The scope chain while executing. `ROOT` is the case's start country until
/// a province event replaces it with the target province. `FROM` is the scope
/// that fired the current event. `PREV` is the scope a block was entered from.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ScopeCtx {
	current: ScopeRef,
	prev: Option<ScopeRef>,
	root: ScopeRef,
	from: Option<ScopeRef>,
}

impl ScopeCtx {
	fn start(tag: &Tag) -> Self {
		let country = ScopeRef::Country(tag.clone());
		Self {
			current: country.clone(),
			prev: None,
			root: country,
			from: None,
		}
	}

	/// Enter a scope block targeting `scope`: PREV becomes the scope left.
	fn enter(&self, scope: ScopeRef) -> Self {
		Self {
			current: scope,
			prev: Some(self.current.clone()),
			root: self.root.clone(),
			from: self.from.clone(),
		}
	}

	/// The scope a fired country event runs in: same country, `FROM` set to
	/// the firing scope, `ROOT` unchanged, and no enclosing `PREV`.
	fn fired_country(&self) -> Result<Self, String> {
		let ScopeRef::Country(_) = &self.current else {
			return Err(format!(
				"country_event needs a country scope; the current scope is {}",
				self.current
			));
		};
		Ok(Self {
			current: self.current.clone(),
			prev: None,
			root: self.root.clone(),
			from: Some(self.current.clone()),
		})
	}

	/// The scope a fired province event runs in. `ROOT` is the target
	/// province and `FROM` is the scope that executed `province_event`.
	fn fired_province(&self) -> Result<Self, String> {
		let ScopeRef::Province(_) = &self.current else {
			return Err(format!(
				"province_event needs a province scope; the current scope is {}",
				self.current
			));
		};
		Ok(Self {
			current: self.current.clone(),
			prev: None,
			root: self.current.clone(),
			from: Some(self.current.clone()),
		})
	}

	fn resolve(&self, alias: &str) -> Option<ScopeRef> {
		match alias {
			"THIS" => Some(self.current.clone()),
			"ROOT" => Some(self.root.clone()),
			"PREV" => self.prev.clone(),
			"FROM" => self.from.clone(),
			_ => None,
		}
	}

	fn tag(&self) -> &Tag {
		match &self.current {
			ScopeRef::Country(tag) => tag,
			ScopeRef::Province(id) => {
				unreachable!("province {id} was checked before a country primitive")
			}
		}
	}

	fn province(&self) -> u32 {
		match &self.current {
			ScopeRef::Province(id) => *id,
			ScopeRef::Country(tag) => {
				unreachable!("{tag} was checked before a province primitive")
			}
		}
	}
}

/// What a scope-shaped key names.
#[derive(Clone, Copy, PartialEq)]
enum ScopeKey {
	Alias(&'static str),
	Tag,
	/// The current country's overlord. Resolved at execution; the scan still
	/// walks the block, because whether a subject link exists is runtime state.
	Overlord,
	Province(u32),
	Capital,
	Owner,
	Controller,
}

/// Whether `key` opens a scope block in this context. `NOT`/`AND` are logic
/// keywords no country uses; `NOR` is Norway in effect blocks and logic in
/// condition blocks, exactly as the lint's footprint over-approximation
/// decides. A numeric key is a province id, not a country.
fn scope_key(key: &str, context: Context) -> Option<ScopeKey> {
	if key == "overlord" {
		return Some(ScopeKey::Overlord);
	}
	if key == "capital_scope" {
		return Some(ScopeKey::Capital);
	}
	if key == "owner" {
		return Some(ScopeKey::Owner);
	}
	if key == "controller" {
		return Some(ScopeKey::Controller);
	}
	if let Some(alias) = ALIASES.iter().find(|alias| **alias == key) {
		return Some(ScopeKey::Alias(alias));
	}
	if let Some(id) = province_id_key(key) {
		return Some(ScopeKey::Province(id));
	}
	if matches!(key, "NOT" | "AND") {
		return None;
	}
	if context == Context::Condition && key == "NOR" {
		return None;
	}
	Tag::parse(key).is_ok().then_some(ScopeKey::Tag)
}

fn province_id_key(key: &str) -> Option<u32> {
	if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_digit()) {
		return None;
	}
	let id = key.parse::<u32>().ok()?;
	(id >= 1).then_some(id)
}

/// Concrete kind of a scope, for the precheck. `Dynamic` is an alias whose
/// country-or-province kind is only known once an event is fired.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
	Country,
	Province,
	Dynamic,
}

/// Static scope knowledge the scan carries, so PREV/FROM resolvability and
/// country/province mismatches are refused before execution.
#[derive(Clone)]
struct ScanScope {
	stack: Vec<ScopeKind>,
	/// Whether execution is inside a fired event (so FROM resolves).
	in_event: bool,
	from_kind: ScopeKind,
	root_kind: ScopeKind,
}

impl ScanScope {
	fn case() -> Self {
		Self {
			stack: vec![ScopeKind::Country],
			in_event: false,
			from_kind: ScopeKind::Dynamic,
			root_kind: ScopeKind::Country,
		}
	}

	fn current(&self) -> ScopeKind {
		self.stack.last().copied().unwrap_or(ScopeKind::Country)
	}

	fn enter_kind(mut self, kind: ScopeKind) -> Self {
		self.stack.push(kind);
		self
	}

	fn enter_alias(self, alias: &str) -> Self {
		let kind = match alias {
			"THIS" => self.current(),
			"ROOT" => self.root_kind,
			"PREV" => self
				.stack
				.iter()
				.rev()
				.nth(1)
				.copied()
				.unwrap_or(ScopeKind::Dynamic),
			"FROM" => self.from_kind,
			_ => ScopeKind::Dynamic,
		};
		self.enter_kind(kind)
	}

	fn for_event(kind: EventKind) -> Self {
		let scope_kind = match kind {
			EventKind::Country => ScopeKind::Country,
			EventKind::Province => ScopeKind::Province,
		};
		Self {
			stack: vec![scope_kind],
			in_event: true,
			from_kind: ScopeKind::Dynamic,
			root_kind: scope_kind,
		}
	}

	fn prev_known(&self) -> bool {
		self.stack.len() > 1
	}
}

struct Interpreter<'a> {
	facts: &'a ProjectFacts,
	catalog: &'a SemanticsCatalog,
	events: BTreeMap<&'a EventId, &'a EventInfo>,
	/// The mod's scripted definitions, inlined wherever their names appear in
	/// the matching kind of block. The two kinds index separately, so one
	/// name may serve as both an effect and a trigger; within one kind the
	/// later source keeps the existing last-wins boundary.
	scripted_effects: BTreeMap<&'a str, &'a ScriptedDef>,
	scripted_triggers: BTreeMap<&'a str, &'a ScriptedDef>,
	/// The mod's decisions, applied by `take_decision` steps.
	decisions: BTreeMap<&'a str, &'a DecisionInfo>,
	/// Starting state from the game's history files, when supplied.
	seed: Option<&'a SeedFacts>,
	/// Dated history, folded at each case's start date.
	history: Option<&'a HistoryTimeline>,
	/// Resolved subject types. A missing set resolves nothing.
	subject_types: Option<&'a SubjectTypeSet>,
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
			wars: BTreeMap::new(),
			wars_against: BTreeMap::new(),
			alliances: BTreeMap::new(),
			religions: BTreeMap::new(),
			cultures: BTreeMap::new(),
			governments: BTreeMap::new(),
			government_ranks: BTreeMap::new(),
			named: BTreeSet::from([case.start.tag.clone()]),
			numbers: BTreeMap::new(),
			subjects: BTreeMap::new(),
			provinces: BTreeMap::new(),
			capitals: BTreeMap::new(),
			trace: Vec::new(),
			recording: self.observe,
		};
		let folded = self
			.history
			.map(|timeline| fold_history(timeline, case.start.date));
		// A timeline is folded at this case's date. A snapshot seed is the
		// caller's already-folded bookmark and applies only when no timeline
		// was given.
		let seed = folded.as_ref().or(self.seed);
		if let Some(seed) = seed {
			// The seeded world is the countries existing at this date;
			// every_country iterates it instead of the case's named set.
			for (tag, country) in &seed.countries {
				if let Some(religion) = &country.religion {
					world.religions.insert(tag.clone(), religion.clone());
				}
				if let Some(culture) = &country.culture {
					world.cultures.insert(tag.clone(), culture.clone());
				}
				if let Some(government) = &country.government {
					world.governments.insert(tag.clone(), government.clone());
				}
				if let Some(rank) = country.government_rank {
					world.government_ranks.insert(tag.clone(), rank);
				}
				if let Some(capital) = country.capital {
					world.capitals.insert(tag.clone(), capital);
				}
			}
			for (id, province) in &seed.provinces {
				world.provinces.insert(
					*id,
					ProvinceState {
						owner: province.owner.clone(),
						controller: province.controller.clone(),
						base_tax: province.base_tax,
						base_production: province.base_production,
						base_manpower: province.base_manpower,
						cores: province.cores.clone(),
						flags: BTreeSet::new(),
					},
				);
			}
			world.named.extend(seed.tags.iter().cloned());
		}
		world.note_tag(&case.start.tag, Observed::CaseStart);
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
				Step::TakeDecision { .. } => "take_decision",
				Step::Cheat { .. } => "cheat",
				Step::Save { .. } => "save",
				Step::Reload { .. } => "reload",
				Step::ExpectReceipt { .. } => "expect_receipt",
			};
			world.note_tag(
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
				// The scan refuses receipt steps before execution; if the two
				// ever disagree the case still reports a clean refusal
				// instead of panicking.
				Step::ExpectReceipt { span, .. } => {
					return unsupported(
						vec![format!(
							"{span}: plugin receipts need a real game session; \
							 run this case with --with-plugins instead of --offline"
						)],
						failures,
						passed,
						std::mem::take(&mut world.trace),
					);
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
				// The scan refuses save/reload steps before execution; if the
				// two ever disagree the case still reports a clean refusal
				// instead of panicking.
				Step::Save { span, .. } | Step::Reload { span, .. } => {
					return unsupported(
						vec![format!(
							"{span}: save/reload cycles run on the real game; the model 							 does not serialize campaign state"
						)],
						failures,
						passed,
						std::mem::take(&mut world.trace),
					);
				}
				Step::Cheat { command, .. } => match cheat_kind(command) {
					Ok(CheatKind::Mapped { slot, amount }) => {
						let current = {
							let slots = world.numbers.entry(case.start.tag.clone()).or_default();
							let value = slots.entry(slot).or_insert(0.0);
							*value += amount;
							*value
						};
						world.note_tag(
							&case.start.tag,
							Observed::SlotSet {
								slot: slot.into(),
								value: current.to_string(),
							},
						);
					}
					Ok(CheatKind::Passthrough { name }) => {
						// The model has no AI, fog or console to run it on;
						// the note says so, the runner passes the line
						// through and the game answers for itself.
						world.note_tag(
							&case.start.tag,
							Observed::SlotSet {
								slot: "cheat".into(),
								value: format!("{name} passes through to the console"),
							},
						);
					}
					Err(reason) => {
						return unsupported(
							vec![reason],
							failures,
							passed,
							std::mem::take(&mut world.trace),
						);
					}
				},
				Step::TakeDecision { name, .. } => {
					if let Err(reason) = self.take_decision(&mut world, &root, name) {
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

	/// Apply a collected decision: `potential` and `allow` gate the step (a
	/// decision the country cannot currently take is a setup error), then
	/// its `effect` block runs in the case's country scope.
	fn take_decision(&self, world: &mut World, ctx: &ScopeCtx, name: &str) -> Result<(), String> {
		let decision = self
			.decisions
			.get(name)
			.ok_or_else(|| format!("take_decision {name}: no such decision was collected"))?;
		for statement in parse(&decision.body) {
			let AstStatement::Assignment { key, value, .. } = statement else {
				continue;
			};
			if (key == "potential" || key == "allow")
				&& let AstValue::Block { items, .. } = &value
				&& !items.is_empty()
			{
				let gate = format!(
					"{} {}",
					if key == "potential" {
						"unavailable"
					} else {
						"not allowed"
					},
					"to take"
				);
				if !self.eval_statements(world, ctx, items, &decision.origin, 0)? {
					world.note(
						&ctx.current,
						Observed::SlotSet {
							slot: "decision".into(),
							value: format!("{name}: {gate}"),
						},
					);
					return Err(format!(
						"take_decision {name}: the country is {} — the decision's {key} 						 block does not hold in the model",
						gate
					));
				}
			}
		}
		world.note(
			&ctx.current,
			Observed::SlotSet {
				slot: "decision".into(),
				value: name.to_string(),
			},
		);
		for statement in parse(&decision.body) {
			let AstStatement::Assignment { key, value, .. } = statement else {
				continue;
			};
			if key == "effect"
				&& let AstValue::Block { items, .. } = &value
			{
				let mut visited = BTreeSet::new();
				self.exec_statements(world, ctx, items, &decision.origin, &mut visited)?;
			}
		}
		Ok(())
	}

	/// Advance the calendar and deliver every event due by the new date, in
	/// scheduling order within a date.
	fn advance(&self, world: &mut World, days: u32) -> Result<(), String> {
		let new_date = world.date.add_days(days)?;
		// Peek before removing: an event scheduled past the new date stays
		// scheduled for a later advance. Popping first would drop exactly
		// that not-yet-due event.
		while let Some((&due, _)) = world.scheduled.first_key_value()
			&& due <= new_date
		{
			let (_, batch) = world
				.scheduled
				.pop_first()
				.expect("the peeked earliest entry exists");
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
		// The visited set is the active delivery stack, not a history: an
		// event already delivering in this chain is a cycle, while the same
		// id fired again after its delivery returned runs again.
		if !visited.insert(event.to_string()) {
			return Err(format!(
				"event {event} fires itself through immediate blocks; the model refuses the cycle"
			));
		}
		let result = self.deliver_event(world, ctx, event, visited);
		visited.remove(&event.to_string());
		result
	}

	/// Run one collected event's `immediate` block in `ctx`; the caller owns
	/// the cycle bookkeeping.
	fn deliver_event(
		&self,
		world: &mut World,
		ctx: &ScopeCtx,
		event: &EventId,
		visited: &mut BTreeSet<String>,
	) -> Result<(), String> {
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
				let fired = match info.kind {
					EventKind::Country => ctx.fired_country()?,
					EventKind::Province => ctx.fired_province()?,
				};
				self.exec_statements(world, &fired, &items, &info.origin, visited)?;
			}
		}
		Ok(())
	}

	/// Collect every reason the case cannot be interpreted, before running
	/// anything, so a refusal lists all of its causes at once.
	fn scan_case(&self, planned: &PlannedCase, reasons: &mut Vec<String>) {
		let scope = ScanScope::case();
		// Event bodies already scanned: each is validated once per case, so
		// chains (including delayed ones) cannot recurse forever. Immediate
		// cycles are refused at execution, with the same message.
		let mut seen = BTreeSet::new();
		for fixture in &planned.fixtures {
			if let Some(block) = &fixture.effect {
				self.scan_block(block, Context::Effect, scope.clone(), &mut seen, reasons);
			}
			for clause in &fixture.check {
				self.scan_clause(clause, scope.clone(), &mut seen, reasons);
			}
		}
		for step in &planned.case.steps {
			match step {
				Step::Effect { block } => {
					self.scan_block(block, Context::Effect, scope.clone(), &mut seen, reasons)
				}
				Step::Expect { clauses } => {
					for clause in clauses {
						self.scan_clause(clause, scope.clone(), &mut seen, reasons);
					}
				}
				Step::Fire { event, span } => {
					self.scan_event(event, span, EventKind::Country, &mut seen, reasons);
				}
				Step::TakeDecision { name, span } => {
					self.scan_decision(name, span, scope.clone(), &mut seen, reasons);
				}
				// Cheats pass the scan unmolested; mapped commands that
				// are malformed surface at execution with the reason.
				Step::Cheat { .. } => {}
				Step::Save { slot, span } | Step::Reload { slot, span } => {
					reasons.push(format!(
						"{span}: save/reload cycles run on the real game; the model 						 does not serialize campaign state (slot {slot})"
					));
				}
				Step::ExpectReceipt { span, .. } => {
					reasons.push(format!(
						"{span}: plugin receipts need a real game session; \
						 run this case with --with-plugins instead of --offline"
					));
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
		expected: EventKind,
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
		if info.kind != expected {
			let (found, effect) = match info.kind {
				EventKind::Province => ("province event", "province_event"),
				EventKind::Country => ("country event", "country_event"),
			};
			reasons.push(format!(
				"{span}: {event} is a {found}; fire it with {effect} from a matching scope"
			));
			return;
		}
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
					ScanScope::for_event(info.kind),
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
				} => self.scan_scripted_name(name, context, scope.clone(), where_, seen, reasons),
				AstStatement::Item { .. } => reasons.push(format!(
					"{where_}: a statement without a key cannot be interpreted offline"
				)),
				AstStatement::Assignment { key, value, .. } => {
					self.scan_assignment(key, value, context, scope.clone(), where_, seen, reasons)
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
			let next = match target {
				ScopeKey::Alias(alias) => {
					let resolvable = match alias {
						"PREV" => scope.prev_known(),
						"FROM" => scope.in_event,
						_ => true,
					};
					if !resolvable {
						reasons.push(format!(
							"{where_}: {alias} has no scope to name at this position"
						));
					}
					scope.enter_alias(alias)
				}
				ScopeKey::Tag | ScopeKey::Overlord => {
					if matches!(target, ScopeKey::Overlord)
						&& scope.current() == ScopeKind::Province
					{
						reasons.push(format!(
							"{where_}: overlord is a country scope change and the current scope is a province"
						));
					}
					scope.enter_kind(ScopeKind::Country)
				}
				ScopeKey::Province(_) => scope.enter_kind(ScopeKind::Province),
				ScopeKey::Capital => {
					if scope.current() == ScopeKind::Province {
						reasons.push(format!(
							"{where_}: capital_scope is a country scope change and the current scope is a province"
						));
					}
					scope.enter_kind(ScopeKind::Province)
				}
				ScopeKey::Owner | ScopeKey::Controller => {
					if scope.current() == ScopeKind::Country {
						reasons.push(format!(
							"{where_}: {key} is a province scope change and the current scope is a country"
						));
					}
					scope.enter_kind(ScopeKind::Country)
				}
			};
			self.scan_statements(items, where_, context, next, seen, reasons);
			return;
		}
		if context == Context::Effect && key == "random" {
			if let Err(reason) = self.catalog.permits(key) {
				reasons.push(self.refusal(key, &reason, context, where_));
				return;
			}
			let AstValue::Block { items, .. } = value else {
				reasons.push(format!("{where_}: {key} must be a block"));
				return;
			};
			match split_chance(items) {
				Some((chance, body)) => {
					if !(chance == 0.0 || chance == 100.0) {
						reasons.push(format!(
							"{where_}: random with chance {chance} needs the game's RNG; 							 the offline model only decides the constant corners 0 and 100"
						));
					}
					self.scan_statements(&body, where_, context, scope, seen, reasons);
				}
				None => reasons.push(format!(
					"{where_}: random needs a constant chance to be decided offline"
				)),
			}
			return;
		}
		if let Some(message) = scope_mismatch(key, context, scope.current()) {
			reasons.push(format!("{where_}: {message}"));
		}
		if context == Context::Effect && (key == "country_event" || key == "province_event") {
			if let Err(reason) = self.catalog.permits(key) {
				reasons.push(self.refusal(key, &reason, context, where_));
			} else {
				let expected = if key == "province_event" {
					EventKind::Province
				} else {
					EventKind::Country
				};
				self.scan_fired_event(key, value, where_, expected, seen, reasons);
			}
			return;
		}
		if context == Context::Effect
			&& matches!(
				key,
				"every_country" | "every_owned_province" | "every_subject_country"
			) {
			// The pin gate applies to the iterator itself and to every
			// statement inside it. A missing handler does not skip the nested
			// check: the precheck lists every reason.
			if let Err(reason) = self.catalog.permits(key) {
				reasons.push(self.refusal(key, &reason, context, where_));
			}
			let AstValue::Block { items, .. } = value else {
				reasons.push(format!("{where_}: {key} must be a block"));
				return;
			};
			let inner = match key {
				"every_owned_province" => scope.clone().enter_kind(ScopeKind::Province),
				_ => scope.clone().enter_kind(ScopeKind::Country),
			};
			self.scan_limited(items, key, Context::Effect, inner, where_, seen, reasons);
			return;
		}
		if context == Context::Condition
			&& matches!(key, "any_owned_province" | "any_subject_country")
		{
			if let Err(reason) = self.catalog.permits(key) {
				reasons.push(self.refusal(key, &reason, context, where_));
			}
			let AstValue::Block { items, .. } = value else {
				reasons.push(format!("{where_}: {key} must be a block"));
				return;
			};
			let inner = if key == "any_owned_province" {
				scope.clone().enter_kind(ScopeKind::Province)
			} else {
				scope.clone().enter_kind(ScopeKind::Country)
			};
			self.scan_limited(items, key, Context::Condition, inner, where_, seen, reasons);
			return;
		}
		if let Some(definition) = self.scripted_in(key, context) {
			// A scripted name that collides with a catalog primitive would
			// resolve differently in the scan and in execution; refuse the
			// call so the verdict cannot outrun the evidence.
			if builtin(key, context) || self.catalog.primitives.contains_key(key) {
				reasons.push(format!(
					"{where_}: scripted {} {key} collides with a modeled primitive; 					 rename the definition",
					Self::kind_word(context)
				));
				return;
			}
			// A scripted definition inlines its body; the kind must fit the
			// block. Cycles are caught at execution, as event chains are.
			// The call's operand is validated before the dedup so every call
			// site is checked, not only the first.
			match context {
				Context::Condition => {
					// A trigger call is bare, `= yes`, or `= no` (negated);
					// any other operand is not a call the model can read.
					if boolean(value).is_none() {
						reasons.push(format!(
							"{where_}: scripted trigger {key} is called with an operand 							 the model does not read; use a bare call, = yes or = no"
						));
					}
				}
				Context::Effect => {
					reasons.push(format!(
						"{where_}: scripted effect {key} is called bare, without = ..."
					));
				}
			}
			if !seen.insert(format!("scripted:{}:{key}", Self::kind_word(context))) {
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
		if let Some(definition) = self.scripted_any(key) {
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

	/// Scan a collected decision: `potential`/`allow` are conditions, the
	/// `effect` block holds effects; other keys refuse.
	fn scan_decision(
		&self,
		name: &str,
		span: &SourceSpan,
		scope: ScanScope,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		let Some(decision) = self.decisions.get(name) else {
			reasons.push(format!(
				"{span}: take_decision {name}: no such decision was collected"
			));
			return;
		};
		for statement in parse(&decision.body) {
			let AstStatement::Assignment { key, value, .. } = statement else {
				continue;
			};
			let AstValue::Block { items, .. } = &value else {
				continue;
			};
			if key == "effect" {
				self.scan_statements(
					items,
					&decision.origin,
					Context::Effect,
					scope.clone(),
					seen,
					reasons,
				);
			} else if key == "potential" || key == "allow" {
				self.scan_statements(
					items,
					&decision.origin,
					Context::Condition,
					scope.clone(),
					seen,
					reasons,
				);
			} else if !matches!(key.as_str(), "ai_will_do" | "alert" | "button") {
				reasons.push(format!(
					"{span}: decision {name} has {key}, which the offline model does not execute"
				));
			}
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
			if builtin(name, context) || self.catalog.primitives.contains_key(name) {
				reasons.push(format!(
					"{where_}: scripted {} {name} collides with a modeled primitive; 					 rename the definition",
					Self::kind_word(context)
				));
				return;
			}
			if !seen.insert(format!("scripted:{}:{name}", Self::kind_word(context))) {
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
		if let Some(definition) = self.scripted_any(name) {
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
		match context {
			Context::Effect => self.scripted_effects.get(key).copied(),
			Context::Condition => self.scripted_triggers.get(key).copied(),
		}
	}

	/// The word for a scripted definition's kind in call-stack keys.
	fn kind_word(context: Context) -> &'static str {
		match context {
			Context::Effect => "effect",
			Context::Condition => "trigger",
		}
	}

	/// A scripted definition of either kind under `name`, for messages about
	/// a kind mismatch.
	fn scripted_any<'d>(&'d self, key: &str) -> Option<&'d ScriptedDef> {
		self.scripted_effects
			.get(key)
			.or_else(|| self.scripted_triggers.get(key))
			.copied()
	}

	/// A `country_event` or `province_event` effect: the target must be a
	/// collected invocable event of that kind, whose immediate content is scanned too.
	fn scan_fired_event(
		&self,
		key: &str,
		value: &AstValue,
		where_: &SourceSpan,
		expected: EventKind,
		seen: &mut BTreeSet<String>,
		reasons: &mut Vec<String>,
	) {
		match event_args(key, value) {
			Ok(args) => self.scan_event(&args.event, where_, expected, seen, reasons),
			Err(reason) => reasons.push(format!("{where_}: {reason}")),
		}
	}

	/// An iterator block: `limit` is a condition (zero or one), and the rest
	/// use `body`. Both are scanned in `inner`, the scope each iteration enters.
	#[allow(clippy::too_many_arguments)]
	fn scan_limited(
		&self,
		items: &[AstStatement],
		key: &str,
		body: Context,
		inner: ScanScope,
		where_: &SourceSpan,
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
					self.scan_statements(
						items,
						where_,
						Context::Condition,
						inner.clone(),
						seen,
						reasons,
					);
				}
				other => self.scan_statements(
					std::slice::from_ref(other),
					where_,
					body,
					inner.clone(),
					seen,
					reasons,
				),
			}
		}
		if limits > 1 {
			reasons.push(format!("{where_}: {key} needs exactly one limit block"));
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
					self.scan_statements(
						items,
						where_,
						Context::Condition,
						scope.clone(),
						seen,
						reasons,
					);
				}
				other => self.scan_statements(
					std::slice::from_ref(other),
					where_,
					Context::Effect,
					scope.clone(),
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
	/// `name` may be used only when the ordered subject-type overlay resolved it.
	fn require_subject_type(&self, name: &str, where_: &SourceSpan) -> Result<(), String> {
		let Some(set) = self.subject_types else {
			return Err(format!(
				"{where_}: subject type {name} is not in the ordered subject_types overlay"
			));
		};
		if set.resolved.contains(name) {
			return Ok(());
		}
		if let Some(reason) = set.unresolved.get(name) {
			return Err(format!("{where_}: subject type {name} {reason}"));
		}
		Err(format!(
			"{where_}: subject type {name} is not in the ordered subject_types overlay"
		))
	}

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
					// Active call stack: inlining itself is a cycle; a later
					// second call after returning runs again.
					if !visited.insert(format!("scripted:effect:{name}")) {
						return Err(format!(
							"{where_}: {name} inlines itself through scripted definitions; the 							 model refuses the cycle"
						));
					}
					let result = self.exec_statements(
						world,
						ctx,
						&parse(&definition.body),
						&definition.origin,
						visited,
					);
					visited.remove(&format!("scripted:effect:{name}"));
					result?;
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
					// Comments between the branches are not chain
					// terminators; any other statement ends the chain and
					// keeps its own turn.
					while chain < statements.len() {
						match &statements[chain] {
							AstStatement::Assignment { key, .. }
								if key == "else_if" || key == "else" =>
							{
								chain += 1;
							}
							AstStatement::Comment { .. } => chain += 1,
							_ => break,
						}
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
			let Some(scope) = open_scope(world, ctx, key, target, where_)? else {
				return Ok(());
			};
			if let ScopeRef::Country(tag) = &scope {
				world.named.insert(tag.clone());
			}
			return self.exec_statements(world, &ctx.enter(scope), items, where_, visited);
		}
		// The primitive that actually runs honors the handler gate; the scan
		// is not the only line of defense. The static table decides what is
		// a builtin, so a catalog that omits the entry refuses here instead
		// of silently running the arm.
		if builtin(key, Context::Effect) {
			self.catalog
				.permits(key)
				.map_err(|reason| format!("{where_}: {reason}"))?;
		}
		if let Some(message) = scope_mismatch(key, Context::Effect, concrete_kind(&ctx.current)) {
			return Err(format!("{where_}: {message}"));
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
					world.country_mut(ctx.tag()).flags.insert(flag.to_string());
				} else {
					world.note(
						&ctx.current,
						Observed::CountryFlagCleared {
							flag: flag.to_string(),
						},
					);
					world.country_mut(ctx.tag()).flags.remove(flag);
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
					let country = world.country_mut(ctx.tag());
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
			other if number_effect(other).is_some() => {
				let amount = match value {
					AstValue::Scalar {
						value: ScalarValue::Number(text),
						..
					} => text
						.parse::<f64>()
						.map_err(|_| format!("{where_}: {other} needs a number"))?,
					_ => return Err(format!("{where_}: {other} needs a number")),
				};
				let slot = number_effect(other).expect("checked above");
				let current = {
					let slots = world.numbers.entry(ctx.tag().clone()).or_default();
					let value = slots.entry(slot).or_insert(0.0);
					*value += amount;
					*value
				};
				world.note(
					&ctx.current,
					Observed::SlotSet {
						slot: slot.into(),
						value: current.to_string(),
					},
				);
			}
			"declare_war_with_cb" => {
				let AstValue::Block { items, .. } = value else {
					return Err(format!("{where_}: declare_war_with_cb needs who = ..."));
				};
				let mut who = None;
				for statement in items {
					let AstStatement::Assignment { key, value, .. } = statement else {
						continue;
					};
					if key == "who" {
						who = Tag::parse(name_of(value).ok_or_else(|| {
							format!("{where_}: declare_war_with_cb needs who = <tag>")
						})?)
						.ok();
					}
				}
				let who =
					who.ok_or_else(|| format!("{where_}: declare_war_with_cb needs who = <tag>"))?;
				world.named.insert(who.clone());
				world
					.wars
					.entry(ctx.tag().clone())
					.or_default()
					.insert(who.clone());
				world
					.wars_against
					.entry(who.clone())
					.or_default()
					.insert(ctx.tag().clone());
				world.note(
					&ctx.current,
					Observed::WarChanged {
						with: who,
						at_war: true,
					},
				);
			}
			"white_peace" => {
				let other = Tag::parse(
					name_of(value)
						.ok_or_else(|| format!("{where_}: white_peace needs a country tag"))?,
				)
				.map_err(|reason| format!("{where_}: {reason}"))?;
				world.named.insert(other.clone());
				if let Some(set) = world.wars.get_mut(ctx.tag()) {
					set.remove(&other);
				}
				if let Some(set) = world.wars.get_mut(&other) {
					set.remove(ctx.tag());
				}
				if let Some(set) = world.wars_against.get_mut(ctx.tag()) {
					set.remove(&other);
				}
				if let Some(set) = world.wars_against.get_mut(&other) {
					set.remove(ctx.tag());
				}
				world.note(
					&ctx.current,
					Observed::WarChanged {
						with: other,
						at_war: false,
					},
				);
			}
			"create_alliance" | "break_alliance" => {
				let other = Tag::parse(
					name_of(value).ok_or_else(|| format!("{where_}: {key} needs a country tag"))?,
				)
				.map_err(|reason| format!("{where_}: {reason}"))?;
				world.named.insert(other.clone());
				let allied = key == "create_alliance";
				let set = world.alliances.entry(ctx.tag().clone()).or_default();
				if allied {
					set.insert(other.clone());
				} else {
					set.remove(&other);
				}
				let set = world.alliances.entry(other.clone()).or_default();
				if allied {
					set.insert(ctx.tag().clone());
				} else {
					set.remove(ctx.tag());
				}
				world.note(
					&ctx.current,
					Observed::AllianceChanged {
						with: other,
						allied,
					},
				);
			}
			"change_religion" | "change_primary_culture" | "change_government" => {
				let slot = match key {
					"change_religion" => ("religions", "religion"),
					"change_primary_culture" => ("cultures", "culture"),
					_ => ("governments", "government"),
				};
				let value = name_of(value)
					.ok_or_else(|| format!("{where_}: {key} needs a name"))?
					.to_string();
				match slot.0 {
					"religions" => world.religions.insert(ctx.tag().clone(), value.clone()),
					"cultures" => world.cultures.insert(ctx.tag().clone(), value.clone()),
					_ => world.governments.insert(ctx.tag().clone(), value.clone()),
				};
				world.note(
					&ctx.current,
					Observed::SlotSet {
						slot: slot.1.into(),
						value,
					},
				);
			}
			"set_government_rank" => {
				let rank = match value {
					AstValue::Scalar {
						value: ScalarValue::Number(text),
						..
					} => text
						.parse::<i64>()
						.map_err(|_| format!("{where_}: set_government_rank needs a number"))?,
					_ => return Err(format!("{where_}: set_government_rank needs a number")),
				};
				world.government_ranks.insert(ctx.tag().clone(), rank);
				world.note(
					&ctx.current,
					Observed::SlotSet {
						slot: "government_rank".into(),
						value: rank.to_string(),
					},
				);
			}
			"create_subject" => {
				let (subject, kind) = subject_creation(value).ok_or_else(|| {
					format!("{where_}: create_subject needs subject_type and who or subject")
				})?;
				self.require_subject_type(&kind, where_)?;
				world
					.link_subject(subject, ctx.tag().clone(), kind)
					.map_err(|reason| format!("{where_}: {reason}"))?;
			}
			"create_vassal" | "vassalize" | "release" => {
				let subject = Tag::parse(
					name_of(value).ok_or_else(|| format!("{where_}: {key} needs a country tag"))?,
				)
				.map_err(|reason| format!("{where_}: {reason}"))?;
				self.require_subject_type("vassal", where_)?;
				world
					.link_subject(subject, ctx.tag().clone(), "vassal".into())
					.map_err(|reason| format!("{where_}: {reason}"))?;
			}
			"create_march" => {
				let subject = Tag::parse(
					name_of(value)
						.ok_or_else(|| format!("{where_}: create_march needs a country tag"))?,
				)
				.map_err(|reason| format!("{where_}: {reason}"))?;
				self.require_subject_type("march", where_)?;
				world
					.link_subject(subject, ctx.tag().clone(), "march".into())
					.map_err(|reason| format!("{where_}: {reason}"))?;
			}
			"create_union" => {
				let subject = Tag::parse(
					name_of(value)
						.ok_or_else(|| format!("{where_}: create_union needs a country tag"))?,
				)
				.map_err(|reason| format!("{where_}: {reason}"))?;
				self.require_subject_type("personal_union", where_)?;
				world
					.link_subject(subject, ctx.tag().clone(), "personal_union".into())
					.map_err(|reason| format!("{where_}: {reason}"))?;
			}
			"grant_independence" => {
				let free = boolean(value).ok_or_else(|| {
					format!(
						"{where_}: grant_independence needs yes, or a block the model does not read"
					)
				})?;
				if free {
					world.subjects.remove(ctx.tag());
				}
			}
			"every_country" => {
				self.catalog
					.permits(key)
					.map_err(|reason| format!("{where_}: {reason}"))?;
				let AstValue::Block { items, .. } = value else {
					return Err(format!("{where_}: every_country must be a block"));
				};
				// The limit is optional here; multiple ones refuse.
				let (limit, body) = match split_limit(items) {
					Some(split) => split,
					None if !items.iter().any(|statement| {
						matches!(
							statement,
							AstStatement::Assignment { key, .. } if key == "limit"
						)
					}) =>
					{
						(Vec::new(), items.to_vec())
					}
					None => {
						return Err(format!(
							"{where_}: every_country needs exactly one limit block"
						));
					}
				};
				// The model world holds only the countries the case has
				// named; that is what this iteration covers.
				for tag in world.named.clone() {
					let scope = ctx.enter(ScopeRef::Country(tag));
					if limit.is_empty() || self.eval_statements(world, &scope, &limit, where_, 0)? {
						self.exec_statements(world, &scope, &body, where_, visited)?;
					}
				}
			}
			"set_province_flag" | "clr_province_flag" => {
				let id = ctx.province();
				let flag =
					name_of(value).ok_or_else(|| format!("{where_}: {key} needs a flag name"))?;
				let province = world.province_mut(id);
				if key == "set_province_flag" {
					province.flags.insert(flag.to_string());
				} else {
					province.flags.remove(flag);
				}
			}
			"add_base_tax" | "add_base_production" | "add_base_manpower" => {
				let id = ctx.province();
				let amount = integer_amount(value)
					.ok_or_else(|| format!("{where_}: {key} needs an integer"))?;
				let slot = match key {
					"add_base_tax" => "base_tax",
					"add_base_production" => "base_production",
					_ => "base_manpower",
				};
				world
					.add_base(id, slot, amount)
					.map_err(|reason| format!("{where_}: {reason}"))?;
			}
			"cede_province" => {
				let id = ctx.province();
				let target = country_operand(ctx, value, key, where_)?;
				world.named.insert(target.clone());
				let province = world.province_mut(id);
				province.owner = Some(target.clone());
				province.controller = Some(target);
			}
			"add_core" => {
				let id = ctx.province();
				let target = country_operand(ctx, value, key, where_)?;
				world.named.insert(target.clone());
				world
					.province_mut(id)
					.cores
					.get_or_insert_with(BTreeSet::new)
					.insert(target);
			}
			"remove_core" => {
				let id = ctx.province();
				let target = country_operand(ctx, value, key, where_)?;
				let cores = world
					.provinces
					.get_mut(&id)
					.and_then(|province| province.cores.as_mut())
					.ok_or_else(|| format!("{where_}: cores are unknown for province {id}"))?;
				cores.remove(&target);
			}
			"every_owned_province" | "every_subject_country" => {
				let (limit, body) = iterator_parts(value, key, where_)?;
				let scopes = match key {
					"every_owned_province" => world
						.owned_provinces(ctx.tag())
						.into_iter()
						.map(ScopeRef::Province)
						.collect::<Vec<_>>(),
					_ => world
						.subjects_of(ctx.tag())
						.into_iter()
						.map(ScopeRef::Country)
						.collect(),
				};
				for scope in scopes {
					let entered = ctx.enter(scope);
					if limit.is_empty()
						|| self.eval_statements(world, &entered, &limit, where_, 0)?
					{
						self.exec_statements(world, &entered, &body, where_, visited)?;
					}
				}
			}
			"country_event" | "province_event" => {
				let args =
					event_args(key, value).map_err(|reason| format!("{where_}: {reason}"))?;
				let kind = self
					.events
					.get(&args.event)
					.map(|info| info.kind)
					.ok_or_else(|| {
						format!(
							"{where_}: fires {}, whose body was not collected",
							args.event
						)
					})?;
				let expected = if key == "province_event" {
					EventKind::Province
				} else {
					EventKind::Country
				};
				if kind != expected {
					let (found, effect) = match kind {
						EventKind::Province => ("province event", "province_event"),
						EventKind::Country => ("country event", "country_event"),
					};
					return Err(format!(
						"{where_}: {} is a {found}; fire it with {effect} from a matching scope",
						args.event
					));
				}
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
					let fired = match kind {
						EventKind::Country => ctx.fired_country(),
						EventKind::Province => ctx.fired_province(),
					}
					.map_err(|reason| format!("{where_}: {reason}"))?;
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
			other => {
				if let Some(definition) = self.scripted_in(other, Context::Effect) {
					// Active call stack: a definition inlining itself is a
					// cycle; the same definition called again after returning
					// runs again.
					if !visited.insert(format!("scripted:effect:{other}")) {
						return Err(format!(
							"{where_}: {other} inlines itself through scripted definitions; the 							 model refuses the cycle"
						));
					}
					let result = self.exec_statements(
						world,
						ctx,
						&parse(&definition.body),
						&definition.origin,
						visited,
					);
					visited.remove(&format!("scripted:effect:{other}"));
					return result;
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
			let Some(scope) = open_scope(world, ctx, key, target, where_)? else {
				return Ok(false);
			};
			return self.eval_statements(world, &ctx.enter(scope), items, where_, depth + 1);
		}
		// Same gate as execution: the trigger that actually decides honors
		// the handler evidence, not only the scan that approved the plan —
		// and a catalog omitting the entry refuses instead of deciding.
		if builtin(key, Context::Condition) {
			self.catalog
				.permits(key)
				.map_err(|reason| format!("{where_}: {reason}"))?;
		}
		if let Some(message) = scope_mismatch(key, Context::Condition, concrete_kind(&ctx.current))
		{
			return Err(format!("{where_}: {message}"));
		}
		Ok(match key {
			"always" => {
				boolean(value).ok_or_else(|| format!("{where_}: always needs yes or no"))?
			}
			"has_country_flag" => {
				let flag = name_of(value)
					.ok_or_else(|| format!("{where_}: has_country_flag needs a flag name"))?;
				world.country(ctx.tag()).flags.contains(flag)
			}
			"has_global_flag" => {
				let flag = name_of(value)
					.ok_or_else(|| format!("{where_}: has_global_flag needs a flag name"))?;
				world.global_flags.contains(flag)
			}
			"has_variable" => {
				let name = name_of(value)
					.ok_or_else(|| format!("{where_}: has_variable needs a variable name"))?;
				world.country(ctx.tag()).variables.contains_key(name)
			}
			"check_variable" => {
				let (which, wanted) = variable_args(value).ok_or_else(|| {
					format!("{where_}: check_variable needs which = ... and value = ...")
				})?;
				let observed = world
					.country(ctx.tag())
					.variables
					.get(which)
					.copied()
					.unwrap_or(0.0);
				observed >= wanted
			}
			"tag" => {
				let wanted =
					name_of(value).ok_or_else(|| format!("{where_}: tag needs a country tag"))?;
				ctx.tag().as_str() == wanted
			}
			other if number_trigger(other).is_some() => {
				let wanted = match value {
					AstValue::Scalar {
						value: ScalarValue::Number(text),
						..
					} => text
						.parse::<f64>()
						.map_err(|_| format!("{where_}: {other} needs a number"))?,
					_ => return Err(format!("{where_}: {other} needs a number")),
				};
				let slot = number_trigger(other).expect("checked above");
				let observed = world
					.numbers
					.get(ctx.tag())
					.and_then(|slots| slots.get(slot))
					.copied()
					.unwrap_or(0.0);
				observed >= wanted
			}
			"offensive_war_with" | "defensive_war_with" => {
				let other = Tag::parse(
					name_of(value).ok_or_else(|| format!("{where_}: {key} needs a country tag"))?,
				)
				.map_err(|reason| format!("{where_}: {reason}"))?;
				// The direction a war was declared in stays recorded:
				// offensive reads the declarer's index, defensive the
				// defender's.
				if key == "offensive_war_with" {
					world
						.wars
						.get(ctx.tag())
						.is_some_and(|set| set.contains(&other))
				} else {
					world
						.wars_against
						.get(ctx.tag())
						.is_some_and(|set| set.contains(&other))
				}
			}
			"is_at_war" => {
				let wanted =
					boolean(value).ok_or_else(|| format!("{where_}: is_at_war needs yes or no"))?;
				// Either side of any war: a defender is at war too.
				let at_war = world.wars.get(ctx.tag()).is_some_and(|set| !set.is_empty())
					|| world
						.wars_against
						.get(ctx.tag())
						.is_some_and(|set| !set.is_empty());
				at_war == wanted
			}
			"is_subject" => {
				let wanted = boolean(value)
					.ok_or_else(|| format!("{where_}: is_subject needs yes or no"))?;
				world.subjects.contains_key(ctx.tag()) == wanted
			}
			"is_subject_of" => {
				let overlord = Tag::parse(
					name_of(value)
						.ok_or_else(|| format!("{where_}: is_subject_of needs a country tag"))?,
				)
				.map_err(|reason| format!("{where_}: {reason}"))?;
				world
					.subjects
					.get(ctx.tag())
					.is_some_and(|link| link.overlord == overlord)
			}
			"is_subject_of_type" => {
				let kind = name_of(value)
					.ok_or_else(|| format!("{where_}: is_subject_of_type needs a subject type"))?
					.to_string();
				self.require_subject_type(&kind, where_)?;
				world
					.subjects
					.get(ctx.tag())
					.is_some_and(|link| link.kind == kind)
			}
			"alliance_with" => {
				let other = Tag::parse(
					name_of(value)
						.ok_or_else(|| format!("{where_}: alliance_with needs a country tag"))?,
				)
				.map_err(|reason| format!("{where_}: {reason}"))?;
				world
					.alliances
					.get(ctx.tag())
					.is_some_and(|set| set.contains(&other))
			}
			"religion" | "primary_culture" | "government" => {
				let wanted = name_of(value)
					.ok_or_else(|| format!("{where_}: {key} needs a name"))?
					.to_string();
				let known = match key {
					"religion" => world.religions.get(ctx.tag()),
					"primary_culture" => world.cultures.get(ctx.tag()),
					_ => world.governments.get(ctx.tag()),
				};
				// Without a base-snapshot seed the starting value is
				// unknown: reading it before the case writes one refuses
				// rather than guessing.
				let Some(current) = known else {
					return Err(format!(
						"{where_}: {key} is unknown until the case writes it or a 						 base-snapshot seed supplies it"
					));
				};
				*current == wanted
			}
			"government_rank" => {
				let wanted = match value {
					AstValue::Scalar {
						value: ScalarValue::Number(text),
						..
					} => text
						.parse::<i64>()
						.map_err(|_| format!("{where_}: government_rank needs a number"))?,
					_ => return Err(format!("{where_}: government_rank needs a number")),
				};
				let Some(current) = world.government_ranks.get(ctx.tag()) else {
					return Err(format!(
						"{where_}: government_rank is unknown until the case writes it or a 						 base-snapshot seed supplies it"
					));
				};
				*current >= wanted
			}
			"has_province_flag" => {
				let id = ctx.province();
				let flag = name_of(value)
					.ok_or_else(|| format!("{where_}: has_province_flag needs a flag name"))?;
				world
					.provinces
					.get(&id)
					.is_some_and(|province| province.flags.contains(flag))
			}
			"owned_by" | "controlled_by" | "is_core" => {
				let id = ctx.province();
				let wanted = country_operand(ctx, value, key, where_)?;
				let province = world.provinces.get(&id);
				match key {
					"owned_by" => {
						let owner =
							province
								.and_then(|item| item.owner.clone())
								.ok_or_else(|| {
									format!("{where_}: province {id} has no seeded owner")
								})?;
						owner == wanted
					}
					"controlled_by" => {
						let controller = province
							.and_then(|item| item.controller.clone())
							.ok_or_else(|| {
								format!("{where_}: province {id} has no seeded controller")
							})?;
						controller == wanted
					}
					_ => {
						let cores =
							province
								.and_then(|item| item.cores.clone())
								.ok_or_else(|| {
									format!("{where_}: cores are unknown for province {id}")
								})?;
						cores.contains(&wanted)
					}
				}
			}
			"base_tax" | "base_production" | "base_manpower" => {
				let id = ctx.province();
				let wanted = integer_amount(value)
					.ok_or_else(|| format!("{where_}: {key} needs an integer"))?;
				let current = world.provinces.get(&id).and_then(|province| match key {
					"base_tax" => province.base_tax,
					"base_production" => province.base_production,
					_ => province.base_manpower,
				});
				let Some(current) = current else {
					return Err(format!("{where_}: {key} is unknown for province {id}"));
				};
				current >= wanted
			}
			"development" => {
				let id = ctx.province();
				let wanted = integer_amount(value)
					.ok_or_else(|| format!("{where_}: development needs an integer"))?;
				let province = world.provinces.get(&id);
				let tax = province.and_then(|item| item.base_tax);
				let production = province.and_then(|item| item.base_production);
				let manpower = province.and_then(|item| item.base_manpower);
				let mut missing = Vec::new();
				if tax.is_none() {
					missing.push("base_tax");
				}
				if production.is_none() {
					missing.push("base_production");
				}
				if manpower.is_none() {
					missing.push("base_manpower");
				}
				if !missing.is_empty() {
					return Err(format!(
						"{where_}: development is unknown for province {id}: {}",
						missing.join(", ")
					));
				}
				tax.expect("checked") + production.expect("checked") + manpower.expect("checked")
					>= wanted
			}
			"owns" | "owns_core_province" => {
				let id = province_id_of(value)
					.ok_or_else(|| format!("{where_}: {key} needs a province id"))?;
				let owner = world
					.provinces
					.get(&id)
					.and_then(|province| province.owner.clone())
					.ok_or_else(|| format!("{where_}: province {id} has no seeded owner"))?;
				if key == "owns" {
					owner == *ctx.tag()
				} else if &owner != ctx.tag() {
					false
				} else {
					let cores = world
						.provinces
						.get(&id)
						.and_then(|province| province.cores.clone())
						.ok_or_else(|| format!("{where_}: cores are unknown for province {id}"))?;
					cores.contains(ctx.tag())
				}
			}
			"any_owned_province" | "any_subject_country" => {
				let (limit, body) = iterator_parts(value, key, where_)?;
				let scopes = if key == "any_owned_province" {
					world
						.owned_provinces(ctx.tag())
						.into_iter()
						.map(ScopeRef::Province)
						.collect::<Vec<_>>()
				} else {
					world
						.subjects_of(ctx.tag())
						.into_iter()
						.map(ScopeRef::Country)
						.collect()
				};
				let mut matched = false;
				for scope in scopes {
					let entered = ctx.enter(scope);
					let limited = if limit.is_empty() {
						true
					} else {
						self.eval_statements(world, &entered, &limit, where_, depth + 1)?
					};
					if limited && self.eval_statements(world, &entered, &body, where_, depth + 1)? {
						matched = true;
						break;
					}
				}
				matched
			}
			"AND" | "OR" | "NOT" | "NOR" | "NAND" | "hidden_trigger" => {
				let AstValue::Block { items, .. } = value else {
					return Err(format!("{where_}: {key} must be a block"));
				};
				let mut verdicts = Vec::new();
				for statement in items {
					match statement {
						AstStatement::Assignment { key, value, .. } => {
							verdicts.push(self.eval_assignment(
								world,
								ctx,
								key,
								value,
								where_,
								depth + 1,
							)?);
						}
						AstStatement::Item {
							value:
								AstValue::Scalar {
									value: ScalarValue::Identifier(name),
									..
								},
							..
						} => {
							// A bare identifier names a scripted trigger; EU4
							// reads it as its own conjunctive condition.
							let definition =
								self.scripted_in(name, Context::Condition).ok_or_else(|| {
									format!(
										"{where_}: {name} reached evaluation without 											 passing the scan"
									)
								})?;
							verdicts.push(self.eval_statements(
								world,
								ctx,
								&parse(&definition.body),
								&definition.origin,
								depth + 1,
							)?);
						}
						AstStatement::Comment { .. } => {}
						other => {
							return Err(format!(
								"{where_}: {key} cannot evaluate {other:?} inside a logic block"
							));
						}
					}
				}
				if verdicts.is_empty() {
					return Err(format!(
						"{where_}: {key} with no conditions cannot be decided offline"
					));
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
					// `= yes` affirms the scripted trigger's body, `= no`
					// negates it; any other operand is refused.
					let Some(operand) = boolean(value) else {
						return Err(format!(
							"{where_}: scripted trigger {other} is called with an operand 							 the model does not read"
						));
					};
					let holds = self.eval_statements(
						world,
						ctx,
						&parse(&definition.body),
						&definition.origin,
						depth + 1,
					)?;
					return Ok(if operand { holds } else { !holds });
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
			if let Some(slot) = number_trigger(key)
				&& let AstValue::Scalar {
					value: ScalarValue::Number(text),
					..
				} = value && let Ok(wanted) = text.parse::<f64>()
			{
				let observed = world
					.numbers
					.get(ctx.tag())
					.and_then(|slots| slots.get(slot))
					.copied()
					.unwrap_or(0.0);
				return format!(
					"the model has {slot} = {observed} on {}, expected at least {wanted}",
					ctx.current
				);
			}
			let AstValue::Scalar { value, .. } = value else {
				if key == "check_variable"
					&& let Some((which, wanted)) = variable_args(value)
				{
					let observed = world
						.country(ctx.tag())
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

/// `create_subject`: the subject tag (`who` or `subject`) and `subject_type`.
fn subject_creation(value: &AstValue) -> Option<(Tag, String)> {
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	let mut kind = None;
	let mut subject = None;
	for statement in items {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		match key.as_str() {
			"subject_type" => kind = name_of(value).map(str::to_string),
			"who" | "subject" => {
				subject = name_of(value).and_then(|text| Tag::parse(text).ok());
			}
			_ => {}
		}
	}
	Some((subject?, kind?))
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

/// The scope a primitive requires. `None` means the primitive is neutral, or
/// not a modeled primitive (a scripted name must not be labeled by guess).
fn primitive_scope(key: &str, context: Context) -> Option<ScopeKind> {
	let (province, neutral) = match context {
		Context::Effect => (PROVINCE_EFFECTS, NEUTRAL_EFFECTS),
		Context::Condition => (PROVINCE_TRIGGERS, NEUTRAL_TRIGGERS),
	};
	if province.contains(&key) {
		return Some(ScopeKind::Province);
	}
	if neutral.contains(&key) {
		return None;
	}
	if context == Context::Effect && (number_effect(key).is_some() || key == "every_country") {
		return Some(ScopeKind::Country);
	}
	if context == Context::Condition && number_trigger(key).is_some() {
		return Some(ScopeKind::Country);
	}
	if builtin(key, context) {
		return Some(ScopeKind::Country);
	}
	const COUNTRY_EFFECTS: &[&str] = &[
		"break_alliance",
		"change_government",
		"change_primary_culture",
		"change_religion",
		"create_alliance",
		"declare_war_with_cb",
		"set_government_rank",
		"white_peace",
	];
	const COUNTRY_TRIGGERS: &[&str] = &[
		"alliance_with",
		"defensive_war_with",
		"government",
		"government_rank",
		"is_at_war",
		"offensive_war_with",
		"primary_culture",
		"religion",
	];
	match context {
		Context::Effect if COUNTRY_EFFECTS.contains(&key) => Some(ScopeKind::Country),
		Context::Condition if COUNTRY_TRIGGERS.contains(&key) => Some(ScopeKind::Country),
		_ => None,
	}
}

fn concrete_kind(scope: &ScopeRef) -> ScopeKind {
	match scope {
		ScopeRef::Country(_) => ScopeKind::Country,
		ScopeRef::Province(_) => ScopeKind::Province,
	}
}

/// A country primitive in a province scope, or the reverse. `Dynamic` waits
/// until execution knows which kind an alias names.
fn scope_mismatch(key: &str, context: Context, current: ScopeKind) -> Option<String> {
	let needed = primitive_scope(key, context)?;
	if current == ScopeKind::Dynamic || current == needed {
		return None;
	}
	let noun = match context {
		Context::Effect => "effect",
		Context::Condition => "trigger",
	};
	match (needed, current) {
		(ScopeKind::Country, ScopeKind::Province) => Some(format!(
			"{key} is a country {noun} and the current scope is a province"
		)),
		(ScopeKind::Province, ScopeKind::Country) => Some(format!(
			"{key} is a province {noun} and the current scope is a country"
		)),
		_ => None,
	}
}

/// The scope a block enters. `Ok(None)` is an overlord block with no overlord:
/// effects do nothing and triggers are false. Every other miss is a refusal.
fn open_scope(
	world: &World,
	ctx: &ScopeCtx,
	key: &str,
	target: ScopeKey,
	where_: &SourceSpan,
) -> Result<Option<ScopeRef>, String> {
	match target {
		ScopeKey::Alias(alias) => {
			let scope = ctx.resolve(alias).ok_or_else(|| {
				format!("{where_}: {alias} has no scope to name at this position")
			})?;
			Ok(Some(scope))
		}
		ScopeKey::Tag => Ok(Some(ScopeRef::Country(
			Tag::parse(key).expect("scope keys parse as tags"),
		))),
		ScopeKey::Overlord => {
			let ScopeRef::Country(tag) = &ctx.current else {
				return Err(format!(
					"{where_}: overlord is a country scope change and the current scope is {}",
					ctx.current
				));
			};
			Ok(world.overlord_of(tag).map(ScopeRef::Country))
		}
		ScopeKey::Province(id) => Ok(Some(ScopeRef::Province(id))),
		ScopeKey::Capital => {
			let ScopeRef::Country(tag) = &ctx.current else {
				return Err(format!(
					"{where_}: capital_scope is a country scope change and the current scope is {}",
					ctx.current
				));
			};
			match world.capitals.get(tag) {
				Some(id) => Ok(Some(ScopeRef::Province(*id))),
				None => Err(format!("{where_}: {tag} has no seeded capital")),
			}
		}
		ScopeKey::Owner => {
			let ScopeRef::Province(id) = &ctx.current else {
				return Err(format!(
					"{where_}: owner is a province scope change and the current scope is {}",
					ctx.current
				));
			};
			match world
				.provinces
				.get(id)
				.and_then(|province| province.owner.clone())
			{
				Some(tag) => Ok(Some(ScopeRef::Country(tag))),
				None => Err(format!("{where_}: province {id} has no seeded owner")),
			}
		}
		ScopeKey::Controller => {
			let ScopeRef::Province(id) = &ctx.current else {
				return Err(format!(
					"{where_}: controller is a province scope change and the current scope is {}",
					ctx.current
				));
			};
			match world
				.provinces
				.get(id)
				.and_then(|province| province.controller.clone())
			{
				Some(tag) => Ok(Some(ScopeRef::Country(tag))),
				None => Err(format!("{where_}: province {id} has no seeded controller")),
			}
		}
	}
}

/// A country named by a tag or by THIS/ROOT/PREV/FROM. A province alias refuses.
fn country_operand(
	ctx: &ScopeCtx,
	value: &AstValue,
	key: &str,
	where_: &SourceSpan,
) -> Result<Tag, String> {
	let text = name_of(value).ok_or_else(|| format!("{where_}: {key} needs a country"))?;
	if let Some(alias) = ALIASES.iter().find(|alias| **alias == text) {
		let scope = ctx
			.resolve(alias)
			.ok_or_else(|| format!("{where_}: {alias} has no scope to name at this position"))?;
		return match scope {
			ScopeRef::Country(tag) => Ok(tag),
			ScopeRef::Province(id) => Err(format!(
				"{where_}: {key} needs a country; {alias} is province {id}"
			)),
		};
	}
	Tag::parse(text).map_err(|reason| format!("{where_}: {reason}"))
}

fn province_id_of(value: &AstValue) -> Option<u32> {
	let text = match value {
		AstValue::Scalar {
			value: ScalarValue::Number(text),
			..
		} => text.as_str(),
		AstValue::Scalar {
			value: ScalarValue::Identifier(text) | ScalarValue::String(text),
			..
		} => text.as_str(),
		_ => return None,
	};
	province_id_key(text)
}

/// An integer operand. A fractional number is refused rather than truncated.
fn integer_amount(value: &AstValue) -> Option<i64> {
	let AstValue::Scalar {
		value: ScalarValue::Number(text),
		..
	} = value
	else {
		return None;
	};
	text.parse::<i64>().ok()
}

/// `limit` is optional. More than one, or a limit that is not a block, refuses.
fn iterator_parts(
	value: &AstValue,
	key: &str,
	where_: &SourceSpan,
) -> Result<(Vec<AstStatement>, Vec<AstStatement>), String> {
	let AstValue::Block { items, .. } = value else {
		return Err(format!("{where_}: {key} must be a block"));
	};
	match split_limit(items) {
		Some(split) => Ok(split),
		None if !items.iter().any(
			|statement| matches!(statement, AstStatement::Assignment { key, .. } if key == "limit"),
		) =>
		{
			Ok((Vec::new(), items.to_vec()))
		}
		None => Err(format!("{where_}: {key} needs exactly one limit block")),
	}
}

/// The `id` and optional `days` of a `country_event` or `province_event` effect.
fn event_args(key: &str, value: &AstValue) -> Result<FireArgs, String> {
	let AstValue::Block { items, .. } = value else {
		return Err(format!("{key} needs an id block"));
	};
	let mut event = None;
	let mut days = 0;
	for statement in items {
		let AstStatement::Assignment {
			key: field, value, ..
		} = statement
		else {
			continue;
		};
		match field.as_str() {
			"id" => {
				event = Some(EventId::parse(
					name_of(value).ok_or_else(|| format!("{key} id must be a name"))?,
				)?);
			}
			"days" => {
				days = match value {
					AstValue::Scalar {
						value: ScalarValue::Number(text),
						..
					} => text
						.parse::<u32>()
						.map_err(|_| format!("{key} days must be a non-negative number"))?,
					_ => return Err(format!("{key} days must be a number")),
				};
			}
			other => {
				return Err(format!(
					"{key} does not take {other}; the offline model refuses unverified forms"
				));
			}
		}
	}
	Ok(FireArgs {
		event: event.ok_or_else(|| format!("{key} needs an id"))?,
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
				seed: None,
				history: None,
				subject_types: None,
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
	fn logic_blocks_evaluate_bare_scripted_triggers() {
		// A bare identifier inside AND/OR/NOT names a scripted trigger; the
		// review counterexample: AND = { is_false } passed vacuously and
		// OR = { is_true } failed, both from skipping the bare items.
		const CASES: &str = r#"test = {
	name = and_false
	time = 1444.11.11
	tag = SWE
	expect = { AND = { is_false } }
}
test = {
	name = or_true
	time = 1444.11.11
	tag = SWE
	expect = { OR = { is_true } }
}
test = {
	name = not_false
	time = 1444.11.11
	tag = SWE
	expect = { NOT = { is_false } }
}
test = {
	name = mixed_and
	time = 1444.11.11
	tag = SWE
	effect = { set_country_flag = ready }
	expect = { AND = { has_country_flag = ready is_true } }
}"#;
		let sources = [
			file(
				"common/scripted_triggers/false.txt",
				SourceKind::Scripted,
				"is_false = { always = no }",
			),
			file(
				"common/scripted_triggers/true.txt",
				SourceKind::Scripted,
				"is_true = { always = yes }",
			),
			file("tests/scenario.txt", SourceKind::TestsDir, CASES),
		];
		let result = interpret_sources(
			&sources,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		assert_eq!(case(&result, "and_false").outcome, Outcome::Fail);
		assert_eq!(case(&result, "or_true").outcome, Outcome::Pass);
		assert_eq!(case(&result, "not_false").outcome, Outcome::Pass);
		assert_eq!(
			case(&result, "mixed_and").outcome,
			Outcome::Pass,
			"{:#?}",
			case(&result, "mixed_and")
		);
	}

	/// A pinned catalog built from the embedded seed with synthetic
	/// handlers, minus one primitive — the partial-catalog shape of the
	/// external review repro.
	fn pinned_without(name: &str) -> SemanticsCatalog {
		let mut catalog = SemanticsCatalog::embedded();
		catalog.status = EvidenceStatus::Pinned;
		catalog.game_version = Some("synthetic".into());
		catalog.binary_sha256 = Some("0".repeat(64));
		for primitive in catalog.primitives.values_mut() {
			primitive.handler = Some(HandlerPin {
				symbol: "synthetic".into(),
				address: 1,
				end: 2,
				pseudocode_sha256: "0".repeat(64),
			});
		}
		catalog.primitives.remove(name);
		catalog
	}

	#[test]
	fn an_omitted_catalog_primitive_cannot_run_through_a_scripted_shadow() {
		// The review repro: with `always` removed from a pinned catalog, a
		// scripted trigger named `always` made the scan resolve the call as
		// the scripted body while evaluation ran the builtin arm and the
		// case passed with pinned evidence. The builtin table is static, so
		// the missing entry cannot un-make the builtin: the call collides in
		// the scan and the executed builtin would refuse at the gate anyway.
		const SHADOW: &str = "always = { tag = DAN }";
		const CASES: &str = r#"test = {
	name = missing_gate
	time = 1444.11.11
	tag = SWE
	expect = { always = yes }
}"#;
		let sources = [
			file(
				"common/scripted_triggers/shadow.txt",
				SourceKind::Scripted,
				SHADOW,
			),
			file("tests/repro.txt", SourceKind::TestsDir, CASES),
		];
		let catalog = pinned_without("always");
		let result = interpret_sources(&sources, &ProjectFacts::default(), &catalog);
		assert_eq!(result.evidence, EvidenceStatus::Pinned);
		let verdict = case(&result, "missing_gate");
		assert_ne!(verdict.outcome, Outcome::Pass, "{verdict:#?}");
		assert!(
			verdict.unsupported[0].contains("collides"),
			"{:#?}",
			verdict.unsupported
		);
	}

	#[test]
	fn a_partial_catalog_still_supports_unrelated_cases_and_legal_scripted() {
		// The same partial catalog (still missing `always`) runs a case that
		// never touches the omitted primitive, including a legal scripted
		// pair that collides with nothing.
		const CASES: &str = r#"test = {
	name = untouched
	time = 1444.11.11
	tag = SWE
	effect = { prep }
	expect = { is_ready }
}"#;
		let sources = [
			file(
				"common/scripted_effects/prep.txt",
				SourceKind::Scripted,
				"prep = { set_country_flag = ready }",
			),
			file(
				"common/scripted_triggers/ready.txt",
				SourceKind::Scripted,
				"is_ready = { has_country_flag = ready }",
			),
			file("tests/scenario.txt", SourceKind::TestsDir, CASES),
		];
		let catalog = pinned_without("always");
		let result = interpret_sources(&sources, &ProjectFacts::default(), &catalog);
		let verdict = case(&result, "untouched");
		assert_eq!(verdict.outcome, Outcome::Pass, "{verdict:#?}");
	}

	#[test]
	fn a_scripted_name_cannot_shadow_a_catalog_primitive() {
		// A scripted trigger named after a catalog primitive resolves as the
		// scripted body in the scan but as the primitive in evaluation: with
		// `always = { always = no }` defined, `expect = { always = yes }`
		// passed although the definition the scan approved evaluates false.
		// The assignment-form effect twin of this shadow is refused by the
		// bare-call rule and stays refused here.
		let text = r#"{
			"format": 1,
			"status": "pinned",
			"game_version": "1.37.5",
			"binary_sha256": "abc",
			"primitives": {
				"always": {
					"kind": "trigger",
					"facts": {},
					"handler": {
						"symbol": "CDebug::Always",
						"address": 16,
						"end": 64,
						"pseudocode_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
					}
				},
				"set_country_flag": {
					"kind": "effect",
					"facts": {},
					"handler": null
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
				}
			}
		}"#;
		let pinned = SemanticsCatalog::parse(text).unwrap();
		const CASES: &str = r#"test = {
	name = trigger_shadow
	time = 1444.11.11
	tag = SWE
	expect = { always = yes }
}
test = {
	name = effect_shadow
	time = 1444.11.11
	tag = SWE
	effect = { set_country_flag = x }
	expect = { has_country_flag = x }
}"#;
		let sources = [
			file(
				"common/scripted_triggers/always.txt",
				SourceKind::Scripted,
				"always = { always = no }",
			),
			file(
				"common/scripted_effects/shadow.txt",
				SourceKind::Scripted,
				"set_country_flag = { set_country_flag = x }",
			),
			file("tests/scenario.txt", SourceKind::TestsDir, CASES),
		];
		let result = interpret_sources(&sources, &ProjectFacts::default(), &pinned);
		for name in ["trigger_shadow", "effect_shadow"] {
			let verdict = case(&result, name);
			assert_ne!(verdict.outcome, Outcome::Pass, "{name}: {verdict:#?}");
			assert!(
				verdict.unsupported[0].contains("collides"),
				"{name}: {:#?}",
				verdict.unsupported
			);
		}
	}

	#[test]
	fn one_scripted_name_serves_both_its_kinds() {
		// `shared` is defined once as an effect and once as a trigger; the
		// later source must not erase the earlier kind, in either input
		// order.
		const EFFECT: (&str, &str) = (
			"common/scripted_effects/shared.txt",
			"shared = { set_country_flag = ready }",
		);
		const TRIGGER: (&str, &str) = (
			"common/scripted_triggers/shared.txt",
			"shared = { has_country_flag = ready }",
		);
		const CASES: &str = r#"test = {
	name = both_kinds
	time = 1444.11.11
	tag = SWE
	effect = { shared }
	expect = { shared }
}"#;
		for (first, second) in [(EFFECT, TRIGGER), (TRIGGER, EFFECT)] {
			let sources = [
				file(first.0, SourceKind::Scripted, first.1),
				file(second.0, SourceKind::Scripted, second.1),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			];
			let result = interpret_sources(
				&sources,
				&ProjectFacts::default(),
				&SemanticsCatalog::embedded(),
			);
			let verdict = case(&result, "both_kinds");
			assert_eq!(
				verdict.outcome,
				Outcome::Pass,
				"{first:?} then {second:?}: {verdict:#?}"
			);
		}
	}

	#[test]
	fn a_defender_knows_it_is_at_war() {
		// The review counterexample: SWE declares on DAN. DAN's
		// defensive_war_with = SWE held, but is_at_war = yes was false because
		// only the attacker's out-edge was registered. Both sides of a war
		// know it, and white_peace ends it for both.
		const CASES: &str = r#"test = {
	name = attacker_view
	time = 1444.11.11
	tag = SWE
	effect = { declare_war_with_cb = { who = DAN cb = cb_conquest } }
	expect = { offensive_war_with = DAN is_at_war = yes }
}
test = {
	name = defender_view
	time = 1444.11.11
	tag = DAN
	effect = { SWE = { declare_war_with_cb = { who = DAN cb = cb_conquest } } }
	expect = { defensive_war_with = SWE is_at_war = yes }
}
test = {
	name = after_peace
	time = 1444.11.11
	tag = SWE
	effect = { declare_war_with_cb = { who = DAN cb = cb_conquest } white_peace = DAN }
	expect = { NOT = { offensive_war_with = DAN } is_at_war = no }
}
test = {
	name = defender_after_peace
	time = 1444.11.11
	tag = DAN
	effect = {
		SWE = { declare_war_with_cb = { who = DAN cb = cb_conquest } }
		white_peace = SWE
	}
	expect = { NOT = { defensive_war_with = SWE } is_at_war = no }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		for name in [
			"attacker_view",
			"defender_view",
			"after_peace",
			"defender_after_peace",
		] {
			let verdict = case(&result, name);
			assert_eq!(verdict.outcome, Outcome::Pass, "{name}: {verdict:#?}");
		}
	}

	#[test]
	fn save_reload_cycles_are_refused_offline() {
		// The offline model does not serialize campaign state; the cycle runs
		// on the real game and the case says so instead of passing.
		const CASES: &str = r#"test = {
	name = cycler
	time = 1444.11.11
	tag = SWE
	save = "slot_a"
	effect = { set_country_flag = staged }
	reload = "slot_a"
	expect = { has_country_flag = staged }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let verdict = case(&result, "cycler");
		assert_eq!(verdict.outcome, Outcome::Unsupported, "{verdict:#?}");
		assert!(
			verdict.unsupported[0].contains("does not serialize"),
			"{:#?}",
			verdict.unsupported
		);
	}

	#[test]
	fn a_comment_does_not_break_an_if_else_chain() {
		// A comment line between the branches is not a chain terminator;
		// the else must still belong to the if when it executes.
		const CASES: &str = r#"test = {
	name = then_ran
	time = 1444.11.11
	tag = SWE
	effect = {
		if = { limit = { always = yes } set_country_flag = good }
		# a comment between the branches
		else = { set_country_flag = bad }
	}
	expect = { has_country_flag = good NOT = { has_country_flag = bad } }
}
test = {
	name = else_ran
	time = 1444.11.11
	tag = SWE
	effect = {
		if = { limit = { always = no } set_country_flag = good }
		# a comment between the branches
		else = { set_country_flag = bad }
	}
	expect = { NOT = { has_country_flag = good } has_country_flag = bad }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		for name in ["then_ran", "else_ran"] {
			let verdict = case(&result, name);
			assert_eq!(verdict.outcome, Outcome::Pass, "{name}: {verdict:#?}");
		}
	}

	#[test]
	fn comments_across_an_else_if_chain_keep_their_branches() {
		const CASES: &str = r#"test = {
	name = middle_branch
	time = 1444.11.11
	tag = SWE
	effect = {
		if = { limit = { always = no } set_country_flag = first }
		# why not
		else_if = { limit = { always = yes } set_country_flag = middle }
		# fallback
		else = { set_country_flag = last }
	}
	expect = {
		NOT = { has_country_flag = first }
		has_country_flag = middle
		NOT = { has_country_flag = last }
	}
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let verdict = case(&result, "middle_branch");
		assert_eq!(verdict.outcome, Outcome::Pass, "{:#?}", verdict);
	}

	#[test]
	fn a_scripted_trigger_call_honors_its_yes_no_operand() {
		// is_ready = no negates the scripted trigger; treating every call
		// as the bare form read `no` as `yes` and passed the wrong verdict.
		const CASES: &str = r#"test = {
	name = refused
	time = 1444.11.11
	tag = SWE
	expect = { is_ready = no }
}
test = {
	name = granted
	time = 1444.11.11
	tag = SWE
	expect = { is_ready = yes }
}
test = {
	name = false_negated
	time = 1444.11.11
	tag = SWE
	expect = { is_false = no }
}
test = {
	name = false_affirmed
	time = 1444.11.11
	tag = SWE
	expect = { is_false = yes }
}"#;
		let sources = [
			file(
				"common/scripted_triggers/ready.txt",
				SourceKind::Scripted,
				"is_ready = { always = yes }",
			),
			file(
				"common/scripted_triggers/false.txt",
				SourceKind::Scripted,
				"is_false = { always = no }",
			),
			file("tests/scenario.txt", SourceKind::TestsDir, CASES),
		];
		let result = interpret_sources(
			&sources,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		assert_eq!(
			case(&result, "refused").outcome,
			Outcome::Fail,
			"is_ready = no must not pass as if it were yes"
		);
		assert_eq!(case(&result, "granted").outcome, Outcome::Pass);
		assert_eq!(
			case(&result, "false_negated").outcome,
			Outcome::Pass,
			"negating an always-no trigger holds"
		);
		assert_eq!(case(&result, "false_affirmed").outcome, Outcome::Fail);
	}

	#[test]
	fn a_scripted_trigger_with_an_unmodeled_operand_is_refused() {
		const CASES: &str = r#"test = {
	name = odd_operand
	time = 1444.11.11
	tag = SWE
	expect = { is_ready = maybe }
}"#;
		let sources = [
			file(
				"common/scripted_triggers/ready.txt",
				SourceKind::Scripted,
				"is_ready = { always = yes }",
			),
			file("tests/scenario.txt", SourceKind::TestsDir, CASES),
		];
		let result = interpret_sources(
			&sources,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let verdict = case(&result, "odd_operand");
		assert_eq!(verdict.outcome, Outcome::Unsupported, "{:#?}", verdict);
		assert!(
			verdict.unsupported[0].contains("= yes or = no"),
			"{:#?}",
			verdict.unsupported
		);
	}

	#[test]
	fn an_empty_logic_block_is_not_vacuously_true() {
		const CASES: &str = r#"test = {
	name = empty_and
	time = 1444.11.11
	tag = SWE
	expect = { AND = { } }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let verdict = case(&result, "empty_and");
		assert_eq!(verdict.outcome, Outcome::Unsupported, "{:#?}", verdict);
	}

	#[test]
	fn sequential_same_id_events_are_not_a_cycle() {
		// Firing B twice in a row from one immediate block is legal script;
		// only an event already delivering in the current chain is a cycle.
		const DOUBLE: &str = r#"country_event = {
	id = cases.9
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { country_event = { id = cases.10 } country_event = { id = cases.10 } }
	option = { name = OK }
}"#;
		const TICK: &str = r#"country_event = {
	id = cases.10
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { change_variable = { which = ticks value = 1 } }
	option = { name = OK }
}"#;
		const CASES: &str = r#"test = {
	name = double_fire
	time = 1444.11.11
	tag = SWE
	fire = cases.9
	expect = { check_variable = { which = ticks value = 2 } }
}"#;
		let result = interpret_sources(
			&[
				file(
					"events/cases.txt",
					SourceKind::Inline,
					&format!(
						"namespace = cases
{DOUBLE}
{TICK}"
					),
				),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let verdict = case(&result, "double_fire");
		assert_eq!(verdict.outcome, Outcome::Pass, "{:#?}", verdict);
	}

	#[test]
	fn a_self_firing_event_is_still_refused() {
		const LOOP: &str = r#"country_event = {
	id = cases.11
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { country_event = { id = cases.11 } }
	option = { name = OK }
}"#;
		const CASES: &str = r#"test = {
	name = self_fire
	time = 1444.11.11
	tag = SWE
	fire = cases.11
	expect = { always = yes }
}"#;
		let result = interpret_sources(
			&[
				file(
					"events/cases.txt",
					SourceKind::Inline,
					&format!(
						"namespace = cases
{LOOP}"
					),
				),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let verdict = case(&result, "self_fire");
		assert_eq!(verdict.outcome, Outcome::Unsupported);
		assert!(
			verdict.unsupported[0].contains("refuses the cycle"),
			"{:#?}",
			verdict.unsupported
		);
	}

	#[test]
	fn sequential_scripted_calls_are_not_a_cycle() {
		// Calling one scripted effect twice from one block runs both; only a
		// definition inlining itself is a cycle.
		const CASES: &str = r#"test = {
	name = twice
	time = 1444.11.11
	tag = SWE
	effect = { tick tick }
	expect = { check_variable = { which = ticks value = 2 } }
}"#;
		let result = interpret_sources(
			&[
				file(
					"common/scripted_effects/tick.txt",
					SourceKind::Scripted,
					"tick = { change_variable = { which = ticks value = 1 } }",
				),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let verdict = case(&result, "twice");
		assert_eq!(verdict.outcome, Outcome::Pass, "{:#?}", verdict);
	}

	#[test]
	fn a_self_inlining_scripted_effect_is_still_refused() {
		const CASES: &str = r#"test = {
	name = self_inline
	time = 1444.11.11
	tag = SWE
	effect = { spin }
	expect = { always = yes }
}"#;
		let result = interpret_sources(
			&[
				file(
					"common/scripted_effects/spin.txt",
					SourceKind::Scripted,
					"spin = { spin }",
				),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let verdict = case(&result, "self_inline");
		assert_eq!(verdict.outcome, Outcome::Unsupported);
		assert!(
			verdict.unsupported[0].contains("refuses the cycle"),
			"{:#?}",
			verdict.unsupported
		);
	}

	#[test]
	fn a_not_yet_due_event_survives_an_advance_that_ends_earlier() {
		// days = 2 split across two advance_days = 1 steps: the first
		// advance ends before the event is due, so the flag must still be
		// absent (NOT holds), and the second advance delivers it. Popping
		// the earliest entry before comparing dates drops exactly this
		// event on the first advance.
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
		const SPLIT: &str = r#"test = {
	name = split_advance
	time = 1444.11.11
	tag = SWE
	fire = cases.4
	advance_days = 1
	expect = { NOT = { has_country_flag = called } }
	advance_days = 1
	expect = { has_country_flag = called }
}"#;
		let result = scenario(
			&format!(
				"{EVENT}
{TIMER}"
			),
			SPLIT,
		);
		let verdict = case(&result, "split_advance");
		assert_eq!(verdict.outcome, Outcome::Pass, "{:#?}", verdict.failures);
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
				[
					"add_ruler_personality".to_string(),
					"every_country".to_string(),
				]
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
#test(time=1444.11.11, tag=SWE, name=numeric, effect={ add_ruler_personality = x }, expect={ adm_power = 50 })
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
				.any(|reason| reason.contains("add_ruler_personality is a known game effect")),
			"{:#?}",
			numeric.unsupported
		);
		// adm_power is a modeled numeric trigger now; only the effect refuses.
		// every_country is modeled over the case's named world: this one
		// runs (it has no expectation, so it is a smoke), and the war case
		// now fails on its own clause instead of refusing.
		assert_eq!(case(&result, "iteration").outcome, Outcome::Smoke);
		assert_eq!(case(&result, "war").outcome, Outcome::Fail);
		assert_eq!(result.summary().unsupported, 1);
		assert_eq!(result.exit_code(), 1, "the personality case still refuses");
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
	fn random_list_is_refused_with_its_weights_unread() {
		// The review counterexample: a weight-0 branch is not a certain pick,
		// and the model does not read the weights at all, so every
		// random_list is refused instead of running a "single survivor".
		const CASES: &str = r#"test = {
	name = zero_weight
	time = 1444.11.11
	tag = SWE
	effect = { random_list = { 0 = { set_country_flag = never } } }
	expect = { has_country_flag = never }
}
test = {
	name = one_of_two
	time = 1444.11.11
	tag = SWE
	effect = { random_list = { 50 = { trigger = { always = no } set_country_flag = lost } 50 = { set_country_flag = won } } }
	expect = { always = yes }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		for name in ["zero_weight", "one_of_two"] {
			let verdict = case(&result, name);
			assert_eq!(
				verdict.outcome,
				Outcome::Unsupported,
				"{name}: {verdict:#?}"
			);
			assert!(
				verdict.unsupported[0].contains("random_list"),
				"{name}: {:#?}",
				verdict.unsupported
			);
		}
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
				seed: None,
				history: None,
				subject_types: None,
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

	#[test]
	fn wars_alliances_and_slots_round_trip() {
		const CASES: &str = r#"test = {
	name = diplomacy
	time = 1444.11.11
	tag = SWE
	effect = {
		declare_war_with_cb = { who = DAN casus_belli = cb_restore_personal_union }
		create_alliance = NOR
		change_religion = protestant
		change_primary_culture = swedish
		change_government = republic
		set_government_rank = 2
	}
	expect = {
		offensive_war_with = DAN
		DAN = { defensive_war_with = SWE }
		is_at_war = yes
		alliance_with = NOR
		religion = protestant
		primary_culture = swedish
		government = republic
		government_rank = 2
	}
	effect = { white_peace = DAN break_alliance = NOR }
	expect = {
		NOT = { offensive_war_with = DAN }
		is_at_war = no
		NOT = { alliance_with = NOR }
	}
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let diplomacy = case(&result, "diplomacy");
		assert_eq!(diplomacy.outcome, Outcome::Pass, "{:#?}", diplomacy);
	}

	#[test]
	fn a_seed_decides_unwritten_reads_and_the_world() {
		const CASES: &str = r#"test = {
	name = seeded_read
	time = 1444.11.11
	tag = SWE
	expect = { religion = catholic primary_culture = swedish NOT = { religion = protestant } }
}
test = {
	name = seeded_world
	time = 1444.11.11
	tag = SWE
	effect = { every_country = { limit = { religion = catholic } set_country_flag = catholic_seen } }
	expect = { has_country_flag = catholic_seen DAN = { has_country_flag = catholic_seen } NOR = { NOT = { has_country_flag = catholic_seen } } }
}"#;
		let sources = [file("tests/scenario.txt", SourceKind::TestsDir, CASES)];
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
		let seed = SeedFacts {
			tags: ["SWE", "DAN", "NOR"]
				.iter()
				.map(|tag| Tag::parse(tag).unwrap())
				.collect(),
			countries: [
				("SWE", "catholic", "swedish"),
				("DAN", "catholic", "danish"),
				("NOR", "protestant", "norwegian"),
			]
			.iter()
			.map(|(tag, religion, culture)| {
				(
					Tag::parse(tag).unwrap(),
					SeededCountry {
						religion: Some((*religion).into()),
						culture: Some((*culture).into()),
						government: None,
						government_rank: None,
						capital: None,
					},
				)
			})
			.collect(),
			provinces: BTreeMap::new(),
		};
		let result = interpret(
			&plan,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
			&InterpretOptions {
				run_id: RunId::parse("run-1").unwrap(),
				mod_name: "Offline Mod".into(),
				observe: false,
				seed: Some(seed),
				history: None,
				subject_types: None,
			},
		);
		let seeded_read = case(&result, "seeded_read");
		assert_eq!(seeded_read.outcome, Outcome::Pass, "{:#?}", seeded_read);
		let world = case(&result, "seeded_world");
		assert_eq!(world.outcome, Outcome::Pass, "{:#?}", world);
	}

	#[test]
	fn unwritten_slot_reads_refuse() {
		const CASES: &str = r#"test = {
	name = unseeded
	time = 1444.11.11
	tag = SWE
	expect = { religion = catholic }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let unseeded = case(&result, "unseeded");
		assert_eq!(unseeded.outcome, Outcome::Unsupported);
		assert!(
			unseeded.unsupported[0].contains("unknown until the case writes it"),
			"{:#?}",
			unseeded.unsupported
		);
	}

	#[test]
	fn every_country_iterates_the_named_world() {
		const CASES: &str = r#"test = {
	name = iterate_world
	time = 1444.11.11
	tag = SWE
	effect = {
		DAN = { set_country_flag = dan_seen }
		every_country = { limit = { NOT = { tag = SWE } } set_country_flag = iterated }
	}
	expect = {
		DAN = { has_country_flag = iterated has_country_flag = dan_seen }
		NOT = { has_country_flag = iterated }
	}
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let iterate = case(&result, "iterate_world");
		assert_eq!(iterate.outcome, Outcome::Pass, "{:#?}", iterate);
	}

	fn pinned_catalog(body: &str) -> SemanticsCatalog {
		let text = format!(
			r#"{{
			"format": 1,
			"status": "pinned",
			"game_version": "1.37.5",
			"binary_sha256": "abc",
			"primitives": {{ {body} }}
		}}"#
		);
		SemanticsCatalog::parse(&text).unwrap()
	}

	const PINNED_HANDLER: &str = r#""handler": {
		"symbol": "CCountry::Example",
		"address": 16,
		"end": 64,
		"pseudocode_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
	}"#;

	/// An unpinned effect inside `every_country` is still refused. The
	/// iterator's own handler must not hide the nested pin check.
	#[test]
	fn every_country_precheck_does_not_bypass_an_unpinned_body() {
		let catalog = pinned_catalog(&format!(
			r#"
			"every_country": {{ "kind": "effect", "facts": {{}}, {PINNED_HANDLER} }},
			"set_country_flag": {{ "kind": "effect", "facts": {{}}, {PINNED_HANDLER} }},
			"has_country_flag": {{ "kind": "trigger", "facts": {{}}, {PINNED_HANDLER} }},
			"add_prestige": {{ "kind": "effect", "facts": {{}}, "handler": null }}
			"#
		));
		const CASES: &str = r#"test = {
	name = bypass
	time = 1444.11.11
	tag = SWE
	effect = { every_country = { add_prestige = 1 set_country_flag = seen } }
	expect = { has_country_flag = seen }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&catalog,
		);
		let bypass = case(&result, "bypass");
		assert_eq!(bypass.outcome, Outcome::Unsupported);
		assert!(
			bypass
				.unsupported
				.iter()
				.any(|reason| reason.contains("add_prestige")
					&& reason.contains("no decompiled handler evidence")),
			"{:#?}",
			bypass.unsupported
		);
	}

	/// A missing handler on `every_country` still scans the limit and the body.
	#[test]
	fn every_country_precheck_lists_nested_unpinned_reasons_too() {
		let catalog = pinned_catalog(
			r#"
			"every_country": { "kind": "effect", "facts": {}, "handler": null },
			"always": { "kind": "trigger", "facts": {}, "handler": null },
			"add_prestige": { "kind": "effect", "facts": {}, "handler": null }
			"#,
		);
		const CASES: &str = r#"test = {
	name = all_reasons
	time = 1444.11.11
	tag = SWE
	effect = { every_country = { limit = { always = yes } add_prestige = 1 } }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&catalog,
		);
		let reasons = case(&result, "all_reasons").unsupported.join("\n");
		assert!(reasons.contains("every_country"), "{reasons}");
		assert!(reasons.contains("always"), "{reasons}");
		assert!(reasons.contains("add_prestige"), "{reasons}");
		assert_eq!(
			reasons.matches("no decompiled handler evidence").count(),
			3,
			"{reasons}"
		);
	}

	const DECISION_FILE: &str = r#"country_decisions = {
	unify_japan_test = {
		potential = { tag = SWE }
		allow = { has_country_flag = ready }
		effect = { set_country_flag = decided }
	}
}"#;

	#[test]
	fn decisions_apply_their_effect_when_allowed() {
		const CASES: &str = r#"test = {
	name = takes_decision
	time = 1444.11.11
	tag = SWE
	effect = { set_country_flag = ready }
	take_decision = unify_japan_test
	expect = { has_country_flag = decided }
}"#;
		let result = interpret_sources(
			&[
				file("decisions/test.txt", SourceKind::Decisions, DECISION_FILE),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let takes = case(&result, "takes_decision");
		assert_eq!(takes.outcome, Outcome::Pass, "{:#?}", takes);
	}

	#[test]
	fn a_decision_whose_allow_fails_refuses_with_the_reason() {
		const CASES: &str = r#"test = {
	name = blocked_decision
	time = 1444.11.11
	tag = SWE
	take_decision = unify_japan_test
	expect = { always = yes }
}"#;
		let result = interpret_sources(
			&[
				file("decisions/test.txt", SourceKind::Decisions, DECISION_FILE),
				file("tests/scenario.txt", SourceKind::TestsDir, CASES),
			],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let blocked = case(&result, "blocked_decision");
		assert_eq!(blocked.outcome, Outcome::Unsupported);
		assert!(
			blocked.unsupported[0].contains("not allowed"),
			"{:#?}",
			blocked.unsupported
		);
	}

	#[test]
	fn numeric_slots_start_at_zero_and_accumulate() {
		const CASES: &str = r#"test = {
	name = treasury_run
	time = 1444.11.11
	tag = SWE
	effect = { add_adm_power = 50 add_treasury = 25.5 add_treasury = 10 add_stability = 1 }
	expect = {
		adm_power = 50
		treasury = 35
		stability = 1
		NOT = { prestige = 1 }
	}
}
test = {
	name = below_threshold
	time = 1444.11.11
	tag = SWE
	effect = { add_prestige = 30 }
	expect = { prestige = 50 }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let run = case(&result, "treasury_run");
		assert_eq!(run.outcome, Outcome::Pass, "{:#?}", run);
		let below = case(&result, "below_threshold");
		assert_eq!(below.outcome, Outcome::Fail);
		assert_eq!(
			below.failures[0].message,
			"the model has prestige = 30 on SWE, expected at least 50"
		);
	}

	#[test]
	fn cheat_commands_map_to_slots_offline() {
		const CASES: &str = r#"test = {
	name = cheat_run
	time = 1444.11.11
	tag = SWE
	cheat = "cash 500"
	cheat = "adm 100"
	effect = { add_prestige = 10 }
	expect = { treasury = 500 adm_power = 100 prestige = 10 }
}"#;
		let result = interpret_sources(
			&[file("tests/scenario.txt", SourceKind::TestsDir, CASES)],
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
		);
		let run = case(&result, "cheat_run");
		assert_eq!(run.outcome, Outcome::Pass, "{:#?}", run);
	}

	#[test]
	fn simulation_toggles_pass_through_with_a_note() {
		const CASES: &str = r#"test = {
	name = toggled
	time = 1444.11.11
	tag = SWE
	cheat = "yesman"
	effect = { set_country_flag = ready }
	expect = { has_country_flag = ready }
}"#;
		let sources = [file("tests/scenario.txt", SourceKind::TestsDir, CASES)];
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
		let result = interpret(
			&plan,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
			&InterpretOptions {
				run_id: RunId::parse("run-1").unwrap(),
				mod_name: "Offline Mod".into(),
				observe: true,
				seed: None,
				history: None,
				subject_types: None,
			},
		);
		let toggled = case(&result, "toggled");
		assert_eq!(toggled.outcome, Outcome::Pass, "{:#?}", toggled);
		assert!(
			toggled.trace.iter().any(|observation| matches!(
				&observation.event,
				Observed::SlotSet { slot, value }
					if slot == "cheat" && value.contains("yesman passes through")
			)),
			"{:#?}",
			toggled.trace
		);
	}

	#[test]
	fn an_unmapped_cheat_name_passes_through() {
		const CASES: &str = r#"test = {
	name = any_command
	time = 1444.11.11
	tag = SWE
	cheat = "casg 500"
	effect = { set_country_flag = ready }
	expect = { has_country_flag = ready }
}"#;
		let sources = [file("tests/scenario.txt", SourceKind::TestsDir, CASES)];
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
		let result = interpret(
			&plan,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
			&InterpretOptions {
				run_id: RunId::parse("run-1").unwrap(),
				mod_name: "Offline Mod".into(),
				observe: true,
				seed: None,
				history: None,
				subject_types: None,
			},
		);
		let any_command = case(&result, "any_command");
		assert_eq!(any_command.outcome, Outcome::Pass, "{:#?}", any_command);
		assert!(
			any_command.trace.iter().any(|observation| matches!(
				&observation.event,
				Observed::SlotSet { slot, value }
					if slot == "cheat" && value.contains("casg passes through")
			)),
			"{:#?}",
			any_command.trace
		);
	}

	#[test]
	fn history_date_blocks_follow_each_case_date() {
		let timeline = overlay_history(&[
			ContentLayer {
				replace_paths: Vec::new(),
				files: vec![
					LayerFile {
						path: "history/countries/SWE - Sweden.txt".into(),
						text: "religion = catholic\nprimary_culture = swedish\n1500.1.1 = { religion = protestant }\n".into(),
					},
					LayerFile {
						path: "history/countries/DAN - Denmark.txt".into(),
						text: "1500.1.1 = { religion = protestant }\n".into(),
					},
				],
			},
			ContentLayer {
				replace_paths: Vec::new(),
				files: vec![LayerFile {
					path: "history/countries/Q01 - Custom.txt".into(),
					text: "religion = norse\n".into(),
				}],
			},
		])
		.unwrap();
		const CASES: &str = r#"test = {
	name = early
	time = 1444.11.11
	tag = SWE
	expect = { religion = catholic NOT = { religion = protestant } }
}
test = {
	name = later
	time = 1500.1.1
	tag = SWE
	expect = { religion = protestant primary_culture = swedish }
}
test = {
	name = custom
	time = 1444.11.11
	tag = Q01
	expect = { religion = norse }
}
test = {
	name = not_yet
	time = 1444.11.11
	tag = DAN
	expect = { religion = protestant }
}"#;
		let sources = [file("tests/scenario.txt", SourceKind::TestsDir, CASES)];
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
		let result = interpret(
			&plan,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
			&InterpretOptions {
				run_id: RunId::parse("run-1").unwrap(),
				mod_name: "Offline Mod".into(),
				observe: false,
				seed: None,
				history: Some(timeline),
				subject_types: None,
			},
		);
		assert_eq!(
			case(&result, "early").outcome,
			Outcome::Pass,
			"{:#?}",
			case(&result, "early")
		);
		assert_eq!(
			case(&result, "later").outcome,
			Outcome::Pass,
			"{:#?}",
			case(&result, "later")
		);
		assert_eq!(
			case(&result, "custom").outcome,
			Outcome::Pass,
			"{:#?}",
			case(&result, "custom")
		);
		let not_yet = case(&result, "not_yet");
		assert_eq!(not_yet.outcome, Outcome::Unsupported);
		assert!(
			not_yet.unsupported[0].contains("unknown until the case writes it"),
			"{:#?}",
			not_yet.unsupported
		);
	}

	#[test]
	fn subject_links_use_resolved_types_and_the_overlord_scope() {
		let types = overlay_subject_types(&[ContentLayer {
			replace_paths: Vec::new(),
			files: vec![LayerFile {
				path: "common/subject_types/00_subject_types.txt".into(),
				text: "default = {}\nvassal = { copy_from = default }\nee_tributary = { copy_from = vassal }\n".into(),
			}],
		}])
		.unwrap();
		const CASES: &str = r#"test = {
	name = linked
	time = 1444.11.11
	tag = SWE
	effect = {
		create_subject = { subject_type = ee_tributary subject = NOR }
		NOR = { overlord = { set_country_flag = paid } }
	}
	expect = {
		NOR = {
			is_subject = yes
			is_subject_of = SWE
			is_subject_of_type = ee_tributary
			overlord = { has_country_flag = paid }
		}
	}
}
test = {
	name = freed
	time = 1444.11.11
	tag = NOR
	effect = {
		SWE = { create_vassal = NOR }
		grant_independence = yes
	}
	expect = { is_subject = no }
}
test = {
	name = unknown_type
	time = 1444.11.11
	tag = SWE
	effect = { create_march = DAN }
	expect = { always = yes }
}"#;
		let sources = [file("tests/scenario.txt", SourceKind::TestsDir, CASES)];
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
		let result = interpret(
			&plan,
			&ProjectFacts::default(),
			&SemanticsCatalog::embedded(),
			&InterpretOptions {
				run_id: RunId::parse("run-1").unwrap(),
				mod_name: "Offline Mod".into(),
				observe: false,
				seed: None,
				history: None,
				subject_types: Some(types),
			},
		);
		assert_eq!(
			case(&result, "linked").outcome,
			Outcome::Pass,
			"{:#?}",
			case(&result, "linked")
		);
		assert_eq!(
			case(&result, "freed").outcome,
			Outcome::Pass,
			"{:#?}",
			case(&result, "freed")
		);
		let unknown = case(&result, "unknown_type");
		assert_eq!(unknown.outcome, Outcome::Unsupported);
		assert!(
			unknown.unsupported[0].contains("march"),
			"{:#?}",
			unknown.unsupported
		);
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

	fn history_of(files: &[(&str, &str)]) -> HistoryTimeline {
		overlay_history(&[ContentLayer {
			replace_paths: Vec::new(),
			files: files
				.iter()
				.map(|(path, text)| LayerFile {
					path: (*path).into(),
					text: (*text).into(),
				})
				.collect(),
		}])
		.unwrap()
	}

	fn interpret_case(
		events: &str,
		tests: &str,
		history: Option<HistoryTimeline>,
		subject_types: Option<SubjectTypeSet>,
		catalog: &SemanticsCatalog,
	) -> Interpretation {
		let mut sources = Vec::new();
		if !events.is_empty() {
			sources.push(file(
				"events/cases.txt",
				SourceKind::Inline,
				&format!("namespace = cases\n{events}"),
			));
		}
		sources.push(file("tests/scenario.txt", SourceKind::TestsDir, tests));
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
		interpret(
			&plan,
			&ProjectFacts::default(),
			catalog,
			&InterpretOptions {
				run_id: RunId::parse("run-1").unwrap(),
				mod_name: "Offline Mod".into(),
				observe: false,
				seed: None,
				history,
				subject_types,
			},
		)
	}

	const PROVINCE_HISTORY: &[(&str, &str)] = &[
		("history/countries/SWE - Sweden.txt", "capital = 1\n"),
		(
			"history/provinces/1-Stockholm.txt",
			"owner = SWE\ncontroller = SWE\nbase_tax = 5\nbase_production = 4\nbase_manpower = 3\nadd_core = SWE\n",
		),
		("history/provinces/2-Oland.txt", "owner = SWE\n"),
		(
			"history/provinces/3-Bergslagen.txt",
			"owner = SWE\ncontroller = DAN\nbase_tax = 1\nbase_production = 1\nbase_manpower = 1\nadd_core = SWE\n",
		),
	];

	#[test]
	fn history_seeded_provinces_switch_scope_and_round_trip() {
		const CASES: &str = r#"test = {
	name = seeded
	time = 1444.11.11
	tag = SWE
	effect = {
		capital_scope = { set_province_flag = capital_seen }
		1 = {
			owner = {
				set_country_flag = owner_is_swe
				PREV = { set_province_flag = prev_province }
			}
			controller = { set_country_flag = controller_is_swe }
			ROOT = { set_country_flag = root_is_swe }
		}
		DAN = { 1 = { PREV = { set_country_flag = prev_is_dan } } }
		every_owned_province = {
			if = {
				limit = { NOT = { PREV = { has_country_flag = counted } } }
				set_province_flag = first_owned
				PREV = { set_country_flag = counted }
			}
			limit = { owned_by = SWE }
			set_province_flag = visited
		}
	}
	expect = {
		owns = 1
		owns_core_province = 1
		capital_scope = { has_province_flag = capital_seen }
		1 = {
			has_province_flag = capital_seen
			has_province_flag = prev_province
			has_province_flag = first_owned
			has_province_flag = visited
			owned_by = SWE
			controlled_by = SWE
			is_core = SWE
			base_tax = 5
			base_production = 4
			base_manpower = 3
			development = 12
			NOT = { development = 13 }
		}
		2 = { has_province_flag = visited NOT = { has_province_flag = first_owned } }
		3 = {
			has_province_flag = visited
			NOT = { has_province_flag = first_owned }
			controlled_by = DAN
		}
		has_country_flag = owner_is_swe
		has_country_flag = controller_is_swe
		has_country_flag = root_is_swe
		DAN = { has_country_flag = prev_is_dan }
	}
}"#;
		let result = interpret_case(
			"",
			CASES,
			Some(history_of(PROVINCE_HISTORY)),
			None,
			&SemanticsCatalog::embedded(),
		);
		let seeded = case(&result, "seeded");
		assert_eq!(seeded.outcome, Outcome::Pass, "{seeded:#?}");
	}

	#[test]
	fn province_flags_cores_bases_and_cede_update_seeded_slots() {
		const CASES: &str = r#"test = {
	name = edited
	time = 1444.11.11
	tag = SWE
	effect = {
		1 = {
			set_province_flag = marked
			clr_province_flag = marked
			set_province_flag = kept
			add_base_tax = 1
			add_base_production = -1
			add_base_manpower = 2
			add_core = NOR
			remove_core = SWE
			cede_province = DAN
		}
	}
	expect = {
		1 = {
			NOT = { has_province_flag = marked }
			has_province_flag = kept
			base_tax = 6
			base_production = 3
			base_manpower = 5
			development = 14
			is_core = NOR
			NOT = { is_core = SWE }
			owned_by = DAN
			controlled_by = DAN
		}
		NOT = { owns = 1 }
		DAN = { owns = 1 NOT = { owns_core_province = 1 } }
	}
}"#;
		let result = interpret_case(
			"",
			CASES,
			Some(history_of(PROVINCE_HISTORY)),
			None,
			&SemanticsCatalog::embedded(),
		);
		let edited = case(&result, "edited");
		assert_eq!(edited.outcome, Outcome::Pass, "{edited:#?}");
	}

	#[test]
	fn province_events_fire_immediately_and_after_advance_days() {
		const EVENTS: &str = r#"province_event = {
	id = cases.2
	hidden = yes
	is_triggered_only = yes
	immediate = {
		set_province_flag = immediate_flag
		FROM = { set_province_flag = from_flag }
		ROOT = { set_province_flag = root_flag }
		owner = { set_country_flag = owner_flag }
	}
}
province_event = {
	id = cases.3
	hidden = yes
	is_triggered_only = yes
	immediate = { set_province_flag = delayed_flag }
}"#;
		const CASES: &str = r#"test = {
	name = events
	time = 1444.11.11
	tag = SWE
	effect = {
		1 = {
			province_event = { id = cases.2 }
			province_event = { id = cases.3 days = 4 }
		}
	}
	expect = {
		1 = {
			has_province_flag = immediate_flag
			has_province_flag = from_flag
			has_province_flag = root_flag
			NOT = { has_province_flag = delayed_flag }
		}
		has_country_flag = owner_flag
	}
	advance_days = 4
	expect = { 1 = { has_province_flag = delayed_flag } }
}"#;
		let result = interpret_case(
			EVENTS,
			CASES,
			Some(history_of(PROVINCE_HISTORY)),
			None,
			&SemanticsCatalog::embedded(),
		);
		let events = case(&result, "events");
		assert_eq!(events.outcome, Outcome::Pass, "{events:#?}");
	}

	#[test]
	fn subject_iterators_visit_subjects_in_tag_order() {
		let types = overlay_subject_types(&[ContentLayer {
			replace_paths: Vec::new(),
			files: vec![LayerFile {
				path: "common/subject_types/00_subject_types.txt".into(),
				text: "default = {}\nvassal = { copy_from = default }\n".into(),
			}],
		}])
		.unwrap();
		const CASES: &str = r#"test = {
	name = subjects
	time = 1444.11.11
	tag = SWE
	effect = {
		create_subject = { subject_type = vassal subject = HOL }
		create_subject = { subject_type = vassal subject = DAN }
		every_subject_country = {
			if = {
				limit = { NOT = { PREV = { has_country_flag = ordered } } }
				set_country_flag = first
				PREV = { set_country_flag = ordered }
			}
		}
	}
	expect = {
		any_subject_country = { tag = HOL }
		NOT = { any_subject_country = { tag = FRA } }
		any_subject_country = { limit = { tag = DAN } has_country_flag = first }
		DAN = { has_country_flag = first is_subject_of = SWE }
		NOT = { HOL = { has_country_flag = first } }
	}
}"#;
		let result = interpret_case("", CASES, None, Some(types), &SemanticsCatalog::embedded());
		let subjects = case(&result, "subjects");
		assert_eq!(subjects.outcome, Outcome::Pass, "{subjects:#?}");
	}

	#[test]
	fn province_scopes_refuse_the_wrong_kind_and_unseeded_facts() {
		const CASES: &str = r#"test = {
	name = country_effect
	time = 1444.11.11
	tag = SWE
	effect = { 1 = { set_country_flag = no } }
}
test = {
	name = province_trigger
	time = 1444.11.11
	tag = SWE
	expect = { has_province_flag = x }
}
test = {
	name = unseeded_base
	time = 1444.11.11
	tag = SWE
	expect = { 2 = { base_tax = 1 } }
}
test = {
	name = unseeded_add
	time = 1444.11.11
	tag = SWE
	effect = { 2 = { add_base_tax = 1 } }
}
test = {
	name = unseeded_capital
	time = 1444.11.11
	tag = DAN
	effect = { capital_scope = { set_province_flag = x } }
}
test = {
	name = unknown_owner
	time = 1444.11.11
	tag = SWE
	effect = { 9 = { owner = { set_country_flag = no } } }
}
test = {
	name = unknown_cores
	time = 1444.11.11
	tag = SWE
	expect = { owns_core_province = 2 }
}
test = {
	name = unknown_controller
	time = 1444.11.11
	tag = SWE
	effect = { 2 = { controller = { set_country_flag = no } } }
}"#;
		let result = interpret_case(
			"",
			CASES,
			Some(history_of(PROVINCE_HISTORY)),
			None,
			&SemanticsCatalog::embedded(),
		);
		let reason = |name: &str| case(&result, name).unsupported.join("\n");
		assert_eq!(
			case(&result, "country_effect").outcome,
			Outcome::Unsupported
		);
		assert!(
			reason("country_effect").contains("set_country_flag is a country effect"),
			"{}",
			reason("country_effect")
		);
		assert!(
			reason("province_trigger").contains("has_province_flag is a province trigger"),
			"{}",
			reason("province_trigger")
		);
		assert!(
			reason("unseeded_base").contains("base_tax is unknown for province 2"),
			"{}",
			reason("unseeded_base")
		);
		assert!(
			reason("unseeded_add").contains("base_tax is unknown for province 2"),
			"{}",
			reason("unseeded_add")
		);
		assert!(
			reason("unseeded_capital").contains("DAN has no seeded capital"),
			"{}",
			reason("unseeded_capital")
		);
		assert!(
			reason("unknown_owner").contains("province 9 has no seeded owner"),
			"{}",
			reason("unknown_owner")
		);
		assert!(
			reason("unknown_cores").contains("cores are unknown for province 2"),
			"{}",
			reason("unknown_cores")
		);
		assert!(
			reason("unknown_controller").contains("province 2 has no seeded controller"),
			"{}",
			reason("unknown_controller")
		);
	}

	#[test]
	fn a_pinned_catalog_refuses_an_unpinned_province_primitive() {
		let catalog = pinned_catalog(
			r#""set_province_flag": { "kind": "effect", "facts": {}, "handler": null }"#,
		);
		const CASES: &str = r#"test = {
	name = unpinned
	time = 1444.11.11
	tag = SWE
	effect = { 1 = { set_province_flag = x } }
}"#;
		let result = interpret_case("", CASES, None, None, &catalog);
		let unpinned = case(&result, "unpinned");
		assert_eq!(unpinned.outcome, Outcome::Unsupported);
		assert!(
			unpinned.unsupported.iter().any(|reason| {
				reason.contains("set_province_flag")
					&& reason.contains("no decompiled handler evidence")
			}),
			"{:#?}",
			unpinned.unsupported
		);
	}
}
