//! Decision route: one player-only main decision per mod.
//!
//! The main decision opens an event offering every entry of the mod: a lone
//! action, a scripted panel whose actions get an event of their own, or a
//! province search ([`super::province`]). Entries keep their original gates;
//! a province search is offered only when the decision found a province
//! where its action is visible. Decisions run with `ROOT = THIS =` the
//! deciding country, so a country-host action's `FROM` (the same clicking
//! country) is rebound to `ROOT`. Context the extraction could not bind is
//! reported, never guessed.

use std::cell::RefCell;
use std::collections::BTreeMap;

use super::context::{HostText, Localise, TARGET_PARAMETER};
use super::geography::Geography;
use super::{Binding, ExtractedFeature, FeatureCatalog, FeatureCondition, HostRoot, Stage};
use crate::game::eu4::derived_localisation::{
	CustomLocalisation, Derivation, DerivedLocalisation, EffectiveLocalisation, Rebinding,
	TextOverrides,
};
use crate::game::eu4::script::emit::emit_clausewitz_statements;
use crate::game::eu4::script::parser::parse_clausewitz_content;
use crate::game::eu4::script::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};
use crate::project::GuiConfig;

/// Enters every generated identifier, so a rule change yields new names.
pub const DECISION_ROUTE_VERSION: &str = "foch-gui-decision-v4";

/// Languages that receive generated localisation. Original text is aliased,
/// so every language resolves its own effective translation.
pub const DECISION_LANGUAGES: &[&str] = &["english", "french", "german", "spanish"];

/// The mod that contributes the actions.
#[derive(Clone, Copy, Debug)]
pub struct SourceMod<'a> {
	/// Stable identity, such as the Workshop id; it enters generated names.
	pub id: &'a str,
	/// Display name; it appears in titles.
	pub name: &'a str,
}

/// The files of one mod's main decision, ready to become generated artifacts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedDecision {
	pub id: String,
	/// Buttons this decision presents, in source order.
	pub actions: Vec<String>,
	/// `(path, text)` of the decision, its events and generated scripts.
	pub scripts: Vec<(String, String)>,
	/// `(path, text)` per language, UTF-8 with the BOM EU4 requires.
	pub localisation: Vec<(String, String)>,
	/// Original keys the generated text references; output validation must
	/// find them.
	pub required_localisation: Vec<String>,
	/// Entries whose visibility the main decision cannot test exactly; it is
	/// shown for them and the entry is checked when the decision is taken.
	pub approximate_visibility: Vec<String>,
	/// Shown text the derivation could not establish.
	pub text_problems: Vec<String>,
}

/// Reviewed replacements, written as the source mod would have written them.
#[derive(Clone, Debug, Default)]
pub struct GuiOverrides {
	pub text: TextOverrides,
	/// `(mod, button)` → visibility trigger statements.
	pub visibility: BTreeMap<(String, String), Vec<AstStatement>>,
}

impl GuiOverrides {
	pub fn from_config(config: &GuiConfig) -> Self {
		let mut overrides = Self::default();
		for entry in &config.text {
			overrides
				.text
				.insert(entry.language.as_deref(), &entry.key, &entry.text);
		}
		for entry in &config.visibility {
			let parsed = parse_clausewitz_content(
				crate::model::GamePath::new("gui_visibility.txt").expect("synthetic game path"),
				&entry.trigger,
			);
			overrides.visibility.insert(
				(entry.mod_id.clone(), entry.button.clone()),
				parsed.ast.statements,
			);
		}
		overrides
	}
}

/// What a route reads besides the extracted actions: the effective map
/// groups and text of the playset, and reviewed overrides.
#[derive(Clone, Debug, Default)]
pub struct RouteContext {
	pub geography: Geography,
	pub localisation: EffectiveLocalisation,
	pub custom_localisation: CustomLocalisation,
	pub overrides: GuiOverrides,
}

/// The main decision for one mod's actions, and each action it refuses.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DecisionPlan {
	pub decisions: Vec<GeneratedDecision>,
	pub rejected: Vec<(String, Vec<String>)>,
}

/// One option of a mod's main event.
pub(super) struct Entry {
	pub(super) actions: Vec<String>,
	/// Plain option text.
	pub(super) label: String,
	/// When the entry could be offered, for the main decision's `potential`;
	/// `None` when a trigger cannot express it.
	pub(super) visible: Option<Vec<AstStatement>>,
	/// When the option is offered.
	pub(super) available: Vec<AstStatement>,
	/// Run when the main decision is taken, before its event opens.
	pub(super) prepare: Vec<AstStatement>,
	/// Run when the option is chosen.
	pub(super) start: Vec<AstStatement>,
	/// Clears the entry's workflow state; part of the main release effect.
	pub(super) close: Vec<AstStatement>,
	/// Whether the entry keeps province flags under the player's number.
	pub(super) needs_player_number: bool,
	pub(super) scripts: Vec<(String, String)>,
	pub(super) text: Vec<(String, String)>,
	pub(super) required: Vec<String>,
}

/// Names shared by every entry of one main decision.
pub(super) struct Main {
	pub(super) id: String,
}

impl Main {
	/// Country flag held while the decision's workflow is open.
	pub(super) fn lock(&self) -> String {
		format!("{}_open", self.id)
	}
	/// Scripted effect ending the workflow: clears every entry and the lock.
	pub(super) fn release(&self) -> String {
		format!("{}_release", self.id)
	}
}

/// Generate the main decision for `features`, extracted from `catalog`, of
/// `source`. Actions under the same scripted panel and host form one entry,
/// in source order. Province searches narrow by the context's geography; an
/// empty one goes straight to listing provinces page by page. Text the
/// actions show is derived from the context's effective localisation.
pub fn decision_route(
	catalog: &FeatureCatalog,
	features: &[ExtractedFeature],
	source: SourceMod<'_>,
	context: &RouteContext,
) -> DecisionPlan {
	let mut plan = DecisionPlan::default();
	let mut groups: Vec<(Option<Panel>, Vec<&ExtractedFeature>)> = Vec::new();
	for feature in features {
		let reasons = refusal_reasons(feature);
		if !reasons.is_empty() {
			plan.rejected
				.push((feature.widget.leaf().name.clone(), reasons));
			continue;
		}
		let panel = Panel::of(feature);
		match groups
			.iter_mut()
			.find(|(existing, _)| panel.is_some() && *existing == panel)
		{
			Some((_, members)) => members.push(feature),
			None => groups.push((panel, vec![feature])),
		}
	}
	let main = Main {
		id: generated_id(source.id, "", &["main"]),
	};
	let derivation = RefCell::new(Derivation::new(
		&main.id,
		DECISION_LANGUAGES,
		&context.localisation,
		&context.custom_localisation,
		&context.overrides.text,
	));
	let localise = |key: &str, target: Option<&str>, instances: &[String]| {
		let mut derivation = derivation.borrow_mut();
		if target == Some(TARGET_PARAMETER) {
			let hosts = instances
				.iter()
				.map(|instance| HostText::new(&catalog.scripts, Some(instance)))
				.collect::<Vec<_>>();
			let hosts = hosts
				.iter()
				.map(|host| host as &dyn Rebinding)
				.collect::<Vec<_>>();
			derivation.parameterized_key(key, &hosts, TARGET_PARAMETER)
		} else {
			derivation.key(key, &HostText::new(&catalog.scripts, target))
		}
	};
	let mut entries = Vec::new();
	for (panel, members) in groups {
		let entry = if members[0].host.root == HostRoot::ClickedProvince {
			super::province::province_entry(
				catalog,
				&members,
				panel.as_ref(),
				source,
				context,
				&main,
				&localise,
			)
		} else {
			match members.as_slice() {
				[feature] => Ok(action_entry(
					catalog,
					feature,
					panel.as_ref(),
					&main,
					&localise,
				)),
				_ => panel_entry(catalog, &members, panel.as_ref(), source, &main, &localise),
			}
		};
		match entry {
			Ok(entry) => entries.push(entry),
			Err(reason) => {
				for feature in members {
					plan.rejected
						.push((feature.widget.leaf().name.clone(), vec![reason.clone()]));
				}
			}
		}
	}
	let derived = derivation.into_inner().finish();
	if !entries.is_empty() {
		match main_decision(&main, entries, source, derived) {
			Ok(decision) => plan.decisions.push(decision),
			Err(reason) => plan.rejected.push((main.id.clone(), vec![reason])),
		}
	}
	plan
}

fn main_decision(
	main: &Main,
	entries: Vec<Entry>,
	source: SourceMod<'_>,
	derived: DerivedLocalisation,
) -> Result<GeneratedDecision, String> {
	let id = &main.id;
	let mut visible = Vec::new();
	let mut approximate = Vec::new();
	let mut prepare = vec![assign("set_country_flag", identifier(&main.lock()))];
	if entries.iter().any(|entry| entry.needs_player_number) {
		prepare.extend(super::province::take_player_number());
	}
	let mut options = Vec::new();
	let mut release = Vec::new();
	let mut scripts = Vec::new();
	let mut text = vec![
		(format!("{id}_title"), literal_text(source.name)),
		(format!("{id}_desc"), literal_text(source.name)),
	];
	let mut required = vec!["CANCEL".to_owned()];
	let mut actions = Vec::new();
	for (index, entry) in entries.into_iter().enumerate() {
		match entry.visible {
			Some(checks) => visible.push(assign("AND", block(checks))),
			None => {
				// A trigger cannot test this entry exactly; the main event
				// still offers it only when it is available.
				visible.push(assign("always", identifier("yes")));
				approximate.push(entry.label.clone());
			}
		}
		prepare.extend(entry.prepare);
		let key = format!("{id}_entry_{}", index + 1);
		let mut option = vec![
			assign("name", identifier(&key)),
			assign("trigger", block(entry.available)),
		];
		option.extend(entry.start);
		options.push(assign("option", block(option)));
		release.extend(entry.close);
		scripts.extend(entry.scripts);
		text.push((key, entry.label));
		text.extend(entry.text);
		required.extend(entry.required);
		actions.extend(entry.actions);
	}
	prepare.push(assign(
		"country_event",
		block(vec![assign("id", identifier(&format!("{id}.1")))]),
	));
	options.push(assign(
		"option",
		block(vec![
			assign("name", identifier("CANCEL")),
			assign(&main.release(), identifier("yes")),
		]),
	));
	release.push(assign("clr_country_flag", identifier(&main.lock())));

	let decision = vec![assign(
		"country_decisions",
		block(vec![assign(
			id,
			block(vec![
				assign(
					"potential",
					block(vec![
						ai_no(),
						assign(
							"NOT",
							block(vec![assign("has_country_flag", identifier(&main.lock()))]),
						),
						assign("OR", block(visible)),
					]),
				),
				assign("allow", block(Vec::new())),
				assign("effect", block(prepare)),
				// A generated player entry never adds autonomous AI use.
				assign("ai_will_do", block(vec![assign("factor", number("0"))])),
			]),
		)]),
	)];
	let mut event = vec![
		assign("id", identifier(&format!("{id}.1"))),
		assign("title", identifier(&format!("{id}_title"))),
		assign("desc", identifier(&format!("{id}_desc"))),
		assign("picture", identifier("DIPLOMACY_eventPicture")),
		assign("is_triggered_only", identifier("yes")),
	];
	event.extend(options);
	let events = vec![
		assign("namespace", identifier(id)),
		assign("country_event", block(event)),
	];
	let effects = vec![assign(&main.release(), block(release))];

	let mut all_scripts = vec![
		(format!("decisions/{id}.txt"), emit(&decision)?),
		(format!("events/{id}.txt"), emit(&events)?),
		(format!("common/scripted_effects/{id}.txt"), emit(&effects)?),
	];
	all_scripts.extend(scripts);
	if !derived.commands.is_empty() {
		all_scripts.push((
			format!("customizable_localization/{id}.txt"),
			emit(&derived.commands)?,
		));
	}
	required.extend(derived.originals);
	required.sort();
	required.dedup();
	Ok(GeneratedDecision {
		id: id.clone(),
		actions,
		scripts: all_scripts,
		localisation: localisation(&text, &derived.entries),
		required_localisation: required,
		approximate_visibility: approximate,
		text_problems: derived.problems,
	})
}

/// The nearest scripted window enclosing an action inside its host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Panel {
	pub(super) gui_file: String,
	/// Named ancestry down to and including the panel window.
	pub(super) ancestry: Vec<String>,
}

impl Panel {
	fn of(feature: &ExtractedFeature) -> Option<Self> {
		let ancestry = &feature.widget.ancestry;
		let host = host_index(feature);
		let index = (host..ancestry.len() - 1)
			.rev()
			.find(|index| ancestry[*index].scripted)?;
		Some(Self {
			gui_file: feature.widget.gui_file.clone(),
			ancestry: ancestry[..=index]
				.iter()
				.map(|node| node.name.clone())
				.collect(),
		})
	}

	fn name(&self) -> &str {
		self.ancestry.last().expect("a panel has a name")
	}

	/// `rce_purity_window` reads as `rce_purity`.
	pub(super) fn label(&self) -> &str {
		self.name()
			.strip_suffix("_window")
			.unwrap_or_else(|| self.name())
	}
}

/// The part of a button's name after the prefix it shares with its panel.
pub(super) fn button_label<'a>(button: &'a str, panel: Option<&Panel>) -> &'a str {
	let Some(panel) = panel else {
		return button;
	};
	let shared = button
		.bytes()
		.zip(panel.name().bytes())
		.take_while(|(left, right)| left == right)
		.count();
	let cut = button[..shared].rfind('_').map_or(0, |index| index + 1);
	match &button[cut..] {
		"" => button,
		rest => rest,
	}
}

/// `<panel>::<button>`, `<panel>` for a whole panel, or `<button>`.
pub(super) fn entry_label(panel: Option<&Panel>, members: &[&ExtractedFeature]) -> String {
	let leaf = &members[0].widget.leaf().name;
	match (panel, members.len()) {
		(Some(panel), 1) => format!(
			"{}::{}",
			literal_text(panel.label()),
			literal_text(button_label(leaf, Some(panel)))
		),
		(Some(panel), _) => literal_text(panel.label()),
		(None, _) => literal_text(leaf),
	}
}

fn refusal_reasons(feature: &ExtractedFeature) -> Vec<String> {
	let mut reasons = feature
		.unresolved
		.iter()
		.map(|reason| format!("extraction: {reason}"))
		.collect::<Vec<_>>();
	if !matches!(
		feature.host.root,
		HostRoot::Actor | HostRoot::ClickedProvince
	) {
		reasons.push(format!(
			"host `{}` binds ROOT to {:?}; only country and province hosts have a decision route",
			feature.host.window, feature.host.root
		));
	}
	reasons.extend(context_gaps(feature));
	let host = host_index(feature);
	for condition in &feature.conditions {
		let outside = feature.widget.ancestry[..host]
			.iter()
			.any(|node| node.name == condition.widget);
		if outside {
			reasons.push(format!(
				"scripted ancestor `{}` lies outside host `{}`; its scope is not documented",
				condition.widget, feature.host.window
			));
		}
	}
	if feature.tooltip.is_none() {
		reasons.push("no custom_gui `tooltip` to describe the action".into());
	}
	reasons
}

fn host_index(feature: &ExtractedFeature) -> usize {
	feature
		.widget
		.ancestry
		.iter()
		.rposition(|node| node.name == feature.host.window)
		.unwrap_or(0)
}

/// A lone country action, run directly from the main event.
fn action_entry(
	catalog: &FeatureCatalog,
	feature: &ExtractedFeature,
	panel: Option<&Panel>,
	main: &Main,
	texts: &Localise<'_>,
) -> Entry {
	let rewrite = catalog.rewrite_text(None, &[], texts);
	let visible = rewrite.statements(
		Stage::Potential,
		&statements(&feature.conditions, Stage::Potential),
	);
	let mut available = visible.clone();
	available.extend(rewrite.statements(
		Stage::Trigger,
		&statements(&feature.conditions, Stage::Trigger),
	));
	let tooltip = rewrite.localised(&feature.tooltip.clone().unwrap_or_default());
	// Every gate is checked again when the option is taken: the event can
	// stay open while the game state changes.
	let mut guarded = vec![assign("limit", block(available.clone()))];
	guarded.extend(rewrite.statements(Stage::Effect, &feature.effect));
	Entry {
		actions: vec![feature.widget.leaf().name.clone()],
		label: entry_label(panel, &[feature]),
		visible: Some(visible),
		available,
		prepare: Vec::new(),
		start: vec![
			assign("custom_tooltip", identifier(&tooltip)),
			assign("if", block(guarded)),
			assign(&main.release(), identifier("yes")),
		],
		close: Vec::new(),
		needs_player_number: false,
		scripts: Vec::new(),
		text: Vec::new(),
		required: Vec::new(),
	}
}

/// A panel of country actions: its own event offers one option per action.
fn panel_entry(
	catalog: &FeatureCatalog,
	members: &[&ExtractedFeature],
	panel: Option<&Panel>,
	source: SourceMod<'_>,
	main: &Main,
	texts: &Localise<'_>,
) -> Result<Entry, String> {
	let rewrite = catalog.rewrite_text(None, &[], texts);
	let panel = panel.ok_or("grouped actions need a panel")?;
	let ancestry = panel
		.ancestry
		.iter()
		.map(String::as_str)
		.collect::<Vec<_>>();
	let id = generated_id(source.id, &panel.gui_file, &ancestry);
	let gates = rewrite.statements(
		Stage::Potential,
		&statements(&panel_gates(members)?, Stage::Potential),
	);
	let own = |feature: &ExtractedFeature, with_trigger: bool| {
		let mine = own_checks(feature);
		let mut all = rewrite.statements(Stage::Potential, &statements(&mine, Stage::Potential));
		if with_trigger {
			all.extend(rewrite.statements(Stage::Trigger, &statements(&mine, Stage::Trigger)));
		}
		all
	};
	let any = |with_trigger: bool| {
		assign(
			"OR",
			block(
				members
					.iter()
					.map(|feature| assign("AND", block(own(feature, with_trigger))))
					.collect(),
			),
		)
	};
	let mut visible = gates.clone();
	visible.push(any(false));
	let mut available = gates.clone();
	available.push(any(true));

	let mut options = Vec::new();
	let mut text = vec![(
		format!("{id}_title"),
		format!(
			"{}::{}",
			literal_text(source.name),
			literal_text(panel.label())
		),
	)];
	for (index, feature) in members.iter().enumerate() {
		let key = format!("{id}_option_{}", index + 1);
		let tooltip = rewrite.localised(&feature.tooltip.clone().unwrap_or_default());
		let mut checks = gates.clone();
		checks.extend(own(feature, true));
		let mut guarded = vec![assign("limit", block(checks.clone()))];
		guarded.extend(rewrite.statements(Stage::Effect, &feature.effect));
		options.push(assign(
			"option",
			block(vec![
				assign("name", identifier(&key)),
				assign("trigger", block(checks)),
				assign("custom_tooltip", identifier(&tooltip)),
				assign("if", block(guarded)),
				assign(&main.release(), identifier("yes")),
			]),
		));
		text.push((
			key,
			literal_text(button_label(&feature.widget.leaf().name, Some(panel))),
		));
	}
	options.push(assign(
		"option",
		block(vec![
			assign("name", identifier("CANCEL")),
			assign(&main.release(), identifier("yes")),
		]),
	));
	let mut event = vec![
		assign("id", identifier(&format!("{id}.1"))),
		assign("title", identifier(&format!("{id}_title"))),
		assign("desc", identifier(&format!("{id}_title"))),
		assign("picture", identifier("DIPLOMACY_eventPicture")),
		assign("is_triggered_only", identifier("yes")),
	];
	event.extend(options);
	let events = vec![
		assign("namespace", identifier(&id)),
		assign("country_event", block(event)),
	];
	Ok(Entry {
		actions: members
			.iter()
			.map(|feature| feature.widget.leaf().name.clone())
			.collect(),
		label: entry_label(Some(panel), members),
		visible: Some(visible),
		available,
		prepare: Vec::new(),
		start: vec![assign(
			"country_event",
			block(vec![assign("id", identifier(&format!("{id}.1")))]),
		)],
		close: Vec::new(),
		needs_player_number: false,
		scripts: vec![(format!("events/{id}.txt"), emit(&events)?)],
		text,
		required: Vec::new(),
	})
}

/// Gates every member shares above its own widget: the panel's chain.
pub(super) fn panel_gates(members: &[&ExtractedFeature]) -> Result<Vec<FeatureCondition>, String> {
	let shared = |feature: &ExtractedFeature| {
		let leaf = &feature.widget.leaf().name;
		feature
			.conditions
			.iter()
			.filter(|condition| condition.widget != *leaf)
			.cloned()
			.collect::<Vec<_>>()
	};
	let gates = shared(members[0]);
	if members.iter().any(|feature| shared(feature) != gates) {
		return Err("grouped actions do not share their ancestor gates".into());
	}
	Ok(gates)
}

/// The action's own `potential` and `trigger`.
pub(super) fn own_checks(feature: &ExtractedFeature) -> Vec<FeatureCondition> {
	let leaf = &feature.widget.leaf().name;
	feature
		.conditions
		.iter()
		.filter(|condition| condition.widget == *leaf)
		.cloned()
		.collect()
}

pub(super) fn statements(conditions: &[FeatureCondition], stage: Stage) -> Vec<AstStatement> {
	conditions
		.iter()
		.filter(|condition| condition.stage == stage)
		.flat_map(|condition| condition.statements.iter().cloned())
		.collect()
}

fn ai_no() -> AstStatement {
	assign("ai", identifier("no"))
}

pub(super) fn emit(statements: &[AstStatement]) -> Result<String, String> {
	emit_clausewitz_statements(statements).map_err(|error| format!("does not emit: {error}"))
}

/// One file per language: the generated `entries` in every language, then
/// that language's derived text.
fn localisation(
	entries: &[(String, String)],
	derived: &BTreeMap<String, Vec<(String, String)>>,
) -> Vec<(String, String)> {
	let id = entries
		.first()
		.and_then(|(key, _)| key.strip_suffix("_title"))
		.unwrap_or("foch_gui");
	DECISION_LANGUAGES
		.iter()
		.map(|language| {
			let mut text = format!("\u{feff}l_{language}:\n");
			let own = derived
				.get(*language)
				.map(Vec::as_slice)
				.unwrap_or_default();
			for (key, value) in entries.iter().chain(own) {
				text.push_str(&format!(" {key}:0 \"{value}\"\n"));
			}
			(format!("localisation/{id}_l_{language}.yml"), text)
		})
		.collect()
}

/// Plain text for a localisation value: drop the characters EU4 reads as key
/// references (`$`), scripted text (`[` `]`), colour (`§`) or icons (`£`),
/// and keep the quoted value intact.
pub(super) fn literal_text(text: &str) -> String {
	text.chars()
		.filter(|c| !matches!(c, '$' | '[' | ']' | '§' | '£'))
		.map(|c| if c == '"' { '\'' } else { c })
		.collect::<String>()
		.trim()
		.to_owned()
}

/// Context the route cannot supply: event targets the behaviour reads
/// without saving them first, and targets of hosts without a search.
fn context_gaps(feature: &ExtractedFeature) -> Vec<String> {
	let mut gaps = Vec::new();
	let mut saved = Vec::new();
	for usage in &feature.scope_uses {
		let location = format!("{:?} {}", usage.stage, usage.path.join(" > "));
		if let Some((_, name)) = usage.token.split_once(" = ") {
			saved.push(name.to_owned());
			continue;
		}
		match &usage.binding {
			Binding::EventTarget(name) if !saved.contains(name) => gaps.push(format!(
				"event target `{name}` is not saved by the action; used at {location}"
			)),
			Binding::Target(root) if *root != HostRoot::ClickedProvince => gaps.push(format!(
				"`{}` denotes the host's {root:?}; used at {location}",
				usage.token
			)),
			_ => {}
		}
	}
	gaps
}

/// A versioned digest of what identifies the entry point, never of mod order.
pub(super) fn generated_id(source_mod: &str, gui_file: &str, ancestry: &[&str]) -> String {
	let identity = [
		DECISION_ROUTE_VERSION,
		source_mod,
		gui_file,
		&ancestry.join("/"),
	]
	.join("\n");
	let digest = blake3::hash(identity.as_bytes()).to_hex();
	format!("foch_gui_{}", &digest[..12])
}

fn span() -> SpanRange {
	let at = Span {
		line: 0,
		column: 0,
		offset: 0,
	};
	SpanRange {
		start: at.clone(),
		end: at,
	}
}

pub(super) fn assign(key: &str, value: AstValue) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_owned(),
		key_span: span(),
		value,
		span: span(),
	}
}

pub(super) fn block(items: Vec<AstStatement>) -> AstValue {
	AstValue::Block {
		items,
		span: span(),
	}
}

pub(super) fn number(text: &str) -> AstValue {
	scalar(ScalarValue::Number(text.to_owned()))
}

pub(super) fn identifier(text: &str) -> AstValue {
	scalar(ScalarValue::Identifier(text.to_owned()))
}

fn scalar(value: ScalarValue) -> AstValue {
	AstValue::Scalar {
		value,
		span: span(),
	}
}
