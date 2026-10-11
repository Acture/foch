use super::geography::Geography;
use super::*;
use crate::game::eu4::script::parser::parse_clausewitz_content;

fn source(path: &str, text: &str) -> ScriptSource {
	ScriptSource {
		path: crate::model::GamePathBuf::parse(path).expect("test game path"),
		text: text.to_owned(),
	}
}

fn parse(path: &str, text: &str) -> AstFile {
	let parsed = parse_clausewitz_content(
		&crate::model::GamePathBuf::parse(path).expect("test game path"),
		text,
	);
	assert!(
		parsed.diagnostics.is_empty(),
		"{path}: {:?}",
		parsed.diagnostics
	);
	parsed.ast
}

struct Fixture {
	interface: Vec<AstFile>,
	custom_gui: Vec<AstFile>,
	effects: Vec<ScriptSource>,
	triggers: Vec<ScriptSource>,
}

impl Fixture {
	fn catalog(&self) -> FeatureCatalog {
		FeatureCatalog::new(GuiSources {
			interface: &self.interface,
			custom_gui: &self.custom_gui,
			scripted_effects: &self.effects,
			scripted_triggers: &self.triggers,
		})
	}
}

const RELIGION_VIEW: &str = r#"
guiTypes = {
	windowType = {
		name = "countryreligionview"
		windowType = {
			name = "purity_panel"
			scripted = yes
			position = { x = 10 y = 20 }
			guiButtonType = {
				name = "purity_knowledge_button"
				scripted = yes
			}
		}
	}
}
"#;

/// A country-host action: a scripted parent window gates the button, and the
/// effect calls a parameterized scripted effect.
fn country_action() -> Fixture {
	Fixture {
		interface: vec![parse("interface/countryreligionview.gui", RELIGION_VIEW)],
		custom_gui: vec![parse(
			"common/custom_gui/purity.txt",
			r#"
custom_window = {
	name = purity_panel
	potential = { ai = no has_country_flag = purity_mechanic }
}
custom_button = {
	name = purity_knowledge_button
	potential = { religion = manichaean }
	trigger = { is_owner_able = yes }
	effect = {
		spend_purity = { amount = 20 }
		FROM = { add_prestige = 1 }
		ROOT = { add_country_modifier = { name = purity_knowledge duration = 3650 } }
	}
}
"#,
		)],
		effects: vec![source(
			"common/scripted_effects/purity.txt",
			"spend_purity = { add_adm_power = -$amount$ set_country_flag = spent_purity }",
		)],
		triggers: vec![source(
			"common/scripted_triggers/purity.txt",
			"is_owner_able = { adm_power = 20 }",
		)],
	}
}

const PROVINCE_VIEW: &str = r#"
guiTypes = {
	windowType = {
		name = "province_window"
		windowType = {
			name = "occupation_actions"
			guiButtonType = {
				name = "transfer_occupation_button"
				scripted = yes
			}
		}
	}
}
"#;

/// A targeted action: `ROOT` is the clicked province, `FROM` the clicker,
/// and `PREV` inside a link refers back to the province.
fn targeted_action() -> Fixture {
	Fixture {
		interface: vec![parse("interface/provinceview.gui", PROVINCE_VIEW)],
		custom_gui: vec![parse(
			"common/custom_gui/occupation.txt",
			r#"
custom_button = {
	name = transfer_occupation_button
	tooltip = transfer_occupation_tt
	potential = { controller = { alliance_with = FROM } }
	effect = {
		FROM = { add_adm_power = -10 }
		controller = { add_opinion = { who = FROM modifier = ally_gave_occupation } }
		owner = { PREV = { change_controller = FROM } }
	}
}
"#,
		)],
		effects: vec![],
		triggers: vec![],
	}
}

#[test]
fn country_action_keeps_ancestor_gates_and_order_before_its_own() {
	let feature = country_action()
		.catalog()
		.extract("purity_knowledge_button")
		.unwrap();
	assert!(feature.is_complete(), "{:?}", feature.unresolved);
	assert_eq!(
		feature.host,
		HostBinding {
			window: "countryreligionview".into(),
			root: HostRoot::Actor,
		}
	);
	let gates = feature
		.conditions
		.iter()
		.map(|condition| (condition.widget.as_str(), condition.stage))
		.collect::<Vec<_>>();
	assert_eq!(
		gates,
		[
			("purity_panel", Stage::Potential),
			("purity_knowledge_button", Stage::Potential),
			("purity_knowledge_button", Stage::Trigger),
		]
	);
	let names = feature
		.widget
		.ancestry
		.iter()
		.map(|node| node.name.as_str())
		.collect::<Vec<_>>();
	assert_eq!(
		names,
		[
			"countryreligionview",
			"purity_panel",
			"purity_knowledge_button"
		]
	);
	let effect_keys = feature
		.effect
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment { key, .. } => Some(key.as_str()),
			_ => None,
		})
		.collect::<Vec<_>>();
	assert_eq!(effect_keys, ["spend_purity", "FROM", "ROOT"]);
}

#[test]
fn country_action_resolves_calls_with_their_parameters() {
	let feature = country_action()
		.catalog()
		.extract("purity_knowledge_button")
		.unwrap();
	let calls = feature
		.calls
		.iter()
		.map(|call| {
			(
				call.stage,
				call.kind,
				call.name.as_str(),
				call.params.clone(),
			)
		})
		.collect::<Vec<_>>();
	assert_eq!(
		calls,
		[
			(Stage::Trigger, ScriptKind::Trigger, "is_owner_able", vec![]),
			(
				Stage::Effect,
				ScriptKind::Effect,
				"spend_purity",
				vec![("amount".to_owned(), "20".to_owned())]
			),
		]
	);
	// Under a `ROOT = FROM` host both names denote the clicking country.
	assert_eq!(feature.bindings_of("FROM"), [&Binding::Actor]);
	assert_eq!(feature.bindings_of("ROOT"), [&Binding::Actor]);
}

#[test]
fn targeted_action_keeps_actor_and_target_distinct() {
	let feature = targeted_action()
		.catalog()
		.extract("transfer_occupation_button")
		.unwrap();
	assert!(feature.is_complete(), "{:?}", feature.unresolved);
	assert_eq!(feature.host.root, HostRoot::ClickedProvince);
	let province = Binding::Target(HostRoot::ClickedProvince);
	let controller = Binding::Link {
		from: Box::new(province.clone()),
		link: "controller".into(),
	};
	assert_eq!(
		feature.bindings_of("controller"),
		[&controller, &controller]
	);
	assert!(
		feature
			.bindings_of("FROM")
			.iter()
			.all(|binding| **binding == Binding::Actor),
		"FROM is the clicker in every stage, including `who = FROM` arguments"
	);
	assert_eq!(feature.bindings_of("FROM").len(), 4);
	// `PREV` inside `owner` is the province, not the clicking country.
	assert_eq!(feature.bindings_of("PREV"), [&province]);
	let potential_uses = feature
		.scope_uses
		.iter()
		.filter(|usage| usage.stage == Stage::Potential)
		.map(|usage| usage.token.as_str())
		.collect::<Vec<_>>();
	assert_eq!(potential_uses, ["controller", "FROM"]);
}

#[test]
fn unresolved_context_is_recorded_not_guessed() {
	let mut fixture = country_action();
	fixture.custom_gui = vec![parse(
		"common/custom_gui/purity.txt",
		r#"
custom_window = { name = purity_panel }
custom_button = {
	name = purity_knowledge_button
	effect = {
		spend_purity = yes
		PREV = { add_prestige = 1 }
		mystery_block = { add_prestige = 1 }
		loop_effect = yes
	}
}
"#,
	)];
	fixture.effects.push(source(
		"common/scripted_effects/loop.txt",
		"loop_effect = { loop_effect = yes }",
	));
	let feature = fixture
		.catalog()
		.extract("purity_knowledge_button")
		.unwrap();
	let joined = feature.unresolved.join("\n");
	assert!(joined.contains("unbound parameter `$amount$`"), "{joined}");
	assert!(joined.contains("`PREV` has no enclosing scope"), "{joined}");
	assert!(joined.contains("unknown block `mystery_block`"), "{joined}");
	assert!(
		joined.contains("recursive call to `loop_effect`"),
		"{joined}"
	);
	assert!(!feature.is_complete());
	assert_eq!(
		feature.bindings_of("PREV"),
		[&Binding::Unknown("PREV".into())]
	);
}

#[test]
fn ambiguous_or_unhosted_widgets_are_not_extracted() {
	let mut duplicated = country_action();
	duplicated.interface.push(parse(
		"interface/countryeconomyview.gui",
		r#"guiTypes = { windowType = { name = "countryeconomyview"
			guiButtonType = { name = "purity_knowledge_button" scripted = yes } } }"#,
	));
	let error = duplicated
		.catalog()
		.extract("purity_knowledge_button")
		.unwrap_err();
	assert!(error.contains("matches 2 scripted widgets"), "{error}");

	let mut unhosted = country_action();
	unhosted.interface = vec![parse(
		"interface/foch_custom.gui",
		r#"guiTypes = { windowType = { name = "anywhere"
			guiButtonType = { name = "purity_knowledge_button" scripted = yes } } }"#,
	)];
	let error = unhosted
		.catalog()
		.extract("purity_knowledge_button")
		.unwrap_err();
	assert!(
		error.contains("not under a documented host window"),
		"{error}"
	);

	let mut redefined = country_action();
	redefined.custom_gui.push(parse(
		"common/custom_gui/zz_override.txt",
		"custom_button = { name = purity_knowledge_button effect = { add_prestige = 5 } }",
	));
	let error = redefined
		.catalog()
		.extract("purity_knowledge_button")
		.unwrap_err();
	assert!(error.contains("has 2 custom_gui definitions"), "{error}");
}

#[test]
fn missing_scripted_ancestor_definition_is_unresolved() {
	let mut fixture = country_action();
	fixture.custom_gui = vec![parse(
		"common/custom_gui/purity.txt",
		"custom_button = { name = purity_knowledge_button effect = { add_prestige = 1 } }",
	)];
	let feature = fixture
		.catalog()
		.extract("purity_knowledge_button")
		.unwrap();
	assert_eq!(
		feature.unresolved,
		["scripted ancestor: `purity_panel` has no custom_gui definition"]
	);
}

#[test]
fn actions_lists_buttons_in_definition_order() {
	let actions = country_action().catalog().actions();
	assert_eq!(actions.len(), 1);
	assert_eq!(actions[0].0, "purity_knowledge_button");
	assert!(actions[0].1.is_ok());
}

#[test]
fn parameters_substitute_with_defaults_and_leave_unbound_names() {
	use super::context::substitute_for_test as substitute;
	assert_eq!(substitute("-$amount$", &[("amount", "20")]), "-20");
	assert_eq!(substitute("$type|adm$_power", &[]), "adm_power");
	assert_eq!(
		substitute("$type|adm$_power", &[("type", "dip")]),
		"dip_power"
	);
	assert_eq!(substitute("$missing$", &[]), "$missing$");
	let conditional = "a = yes [[value] b = $value$ ] [[which] c = $which$ ] [[!which] d = yes ]";
	let words = |text: String| text.split_whitespace().collect::<Vec<_>>().join(" ");
	assert_eq!(
		words(substitute(conditional, &[("value", "20")])),
		"a = yes b = 20 d = yes"
	);
	assert_eq!(
		words(substitute(
			"[[outer] x = { [[inner] y = $inner$ ] } ]",
			&[("outer", "1")]
		)),
		"x = { }"
	);
}

#[test]
fn block_arguments_are_not_commands() {
	let mut fixture = targeted_action();
	fixture.custom_gui = vec![parse(
		"common/custom_gui/occupation.txt",
		r#"
custom_button = {
	name = transfer_occupation_button
	potential = {
		custom_trigger_tooltip = { tooltip = only_allies_tt controller = { alliance_with = FROM } }
		owner = { any_owned_province = { type = all is_core = PREV } }
		NOT = { controller = FROM }
	}
	effect = {
		custom_tooltip = transfer_tt
		tooltip = { add_prestige = 1 }
	}
}
"#,
	)];
	let feature = fixture
		.catalog()
		.extract("transfer_occupation_button")
		.unwrap();
	assert!(feature.is_complete(), "{:?}", feature.unresolved);
	let owner = Binding::Link {
		from: Box::new(Binding::Target(HostRoot::ClickedProvince)),
		link: "owner".into(),
	};
	assert_eq!(feature.bindings_of("PREV"), [&owner]);
}

#[test]
fn documented_hosts_follow_the_game_example() {
	assert_eq!(
		documented_host("interface/countrydecisionview.gui", "countrydecisionsview"),
		Some(HostRoot::Actor)
	);
	assert_eq!(
		documented_host("interface\\provinceview.gui", "state_window"),
		Some(HostRoot::ClickedProvince)
	);
	assert_eq!(
		documented_host("interface/countrydiplomacyview.gui", "countrydiplimacyview"),
		Some(HostRoot::SelectedCountry)
	);
	assert_eq!(
		documented_host("interface/provinceview.gui", "countryreligionview"),
		None
	);
}

/// A country panel whose actions need nothing a decision lacks.
fn decision_ready_panel(buttons: &[(&str, &str)]) -> Fixture {
	let widgets = buttons
		.iter()
		.map(|(name, _)| format!("guiButtonType = {{ name = \"{name}\" scripted = yes }}"))
		.collect::<Vec<_>>()
		.join("\n");
	let definitions = buttons
		.iter()
		.map(|(name, effect)| {
			format!(
				"custom_button = {{
	name = {name}
	tooltip = {name}_tt
	potential = {{ religion = manichaean }}
	trigger = {{ is_owner_able = yes }}
	effect = {{ {effect} }}
}}"
			)
		})
		.collect::<Vec<_>>()
		.join("\n");
	let mut fixture = country_action();
	fixture.interface = vec![parse(
		"interface/countryreligionview.gui",
		&format!(
			"guiTypes = {{ windowType = {{ name = \"countryreligionview\"
	windowType = {{ name = \"rce_purity_window\" scripted = yes
		{widgets}
	}} }} }}"
		),
	)];
	fixture.custom_gui = vec![parse(
		"common/custom_gui/purity.txt",
		&format!(
			"custom_window = {{
	name = rce_purity_window
	potential = {{ ai = no has_country_flag = purity_mechanic }}
}}
{definitions}"
		),
	)];
	fixture
}

fn decision_ready_action(effect: &str) -> Fixture {
	decision_ready_panel(&[("rce_purity_knowledge_button", effect)])
}

const RCE: decision::SourceMod<'static> = decision::SourceMod {
	id: "3342969370",
	name: "Religion Compatibility Expanded",
};

fn keys(statements: &[AstStatement]) -> Vec<&str> {
	statements
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment { key, .. } => Some(key.as_str()),
			_ => None,
		})
		.collect()
}

fn child<'a>(statements: &'a [AstStatement], key: &str) -> &'a [AstStatement] {
	statements
		.iter()
		.find_map(|statement| match statement {
			AstStatement::Assignment {
				key: found,
				value: AstValue::Block { items, .. },
				..
			} if found == key => Some(items.as_slice()),
			_ => None,
		})
		.unwrap_or_else(|| panic!("missing block `{key}`"))
}

fn children<'a>(statements: &'a [AstStatement], key: &str) -> Vec<&'a [AstStatement]> {
	statements
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment {
				key: found,
				value: AstValue::Block { items, .. },
				..
			} if found == key => Some(items.as_slice()),
			_ => None,
		})
		.collect()
}

fn features(fixture: &Fixture) -> Vec<ExtractedFeature> {
	fixture
		.catalog()
		.actions()
		.into_iter()
		.map(|(name, result)| result.unwrap_or_else(|error| panic!("{name}: {error}")))
		.collect()
}

fn route(fixture: &Fixture, source: decision::SourceMod<'_>) -> decision::DecisionPlan {
	let catalog = fixture.catalog();
	decision::decision_route(
		&catalog,
		&features(fixture),
		source,
		&RouteContext::default(),
	)
}

fn single_decision(fixture: &Fixture) -> decision::GeneratedDecision {
	let plan = route(fixture, RCE);
	assert!(plan.rejected.is_empty(), "{:?}", plan.rejected);
	assert_eq!(plan.decisions.len(), 1);
	plan.decisions.into_iter().next().unwrap()
}

fn english(decision: &decision::GeneratedDecision) -> &str {
	&decision
		.localisation
		.iter()
		.find(|(path, _)| path.ends_with("_l_english.yml"))
		.unwrap()
		.1
}

fn script<'a>(decision: &'a decision::GeneratedDecision, path: &str) -> &'a str {
	&decision
		.scripts
		.iter()
		.find(|(found, _)| found == path)
		.unwrap_or_else(|| panic!("no {path}: {:?}", paths(decision)))
		.1
}

fn paths(decision: &decision::GeneratedDecision) -> Vec<&str> {
	decision
		.scripts
		.iter()
		.map(|(path, _)| path.as_str())
		.collect()
}

/// The main decision's body and its event's options.
fn main_parts(decision: &decision::GeneratedDecision) -> (AstFile, AstFile) {
	let id = &decision.id;
	(
		parse("d.txt", script(decision, &format!("decisions/{id}.txt"))),
		parse("e.txt", script(decision, &format!("events/{id}.txt"))),
	)
}

fn decision_body<'a>(parsed: &'a AstFile, id: &str) -> &'a [AstStatement] {
	child(child(&parsed.statements, "country_decisions"), id)
}

fn event_options(parsed: &AstFile) -> Vec<&[AstStatement]> {
	children(child(&parsed.statements, "country_event"), "option")
}

#[test]
fn a_mod_gets_one_main_decision_offering_its_actions() {
	let decision = single_decision(&decision_ready_action(
		"spend_purity = { amount = 20 } add_country_modifier = { name = purity_knowledge duration = 3650 }",
	));
	let id = &decision.id;
	assert!(id.starts_with("foch_gui_") && id.len() == 21);
	assert_eq!(decision.actions, ["rce_purity_knowledge_button"]);
	assert_eq!(
		paths(&decision),
		[
			format!("decisions/{id}.txt"),
			format!("events/{id}.txt"),
			format!("common/scripted_effects/{id}.txt"),
		]
	);
	let (decisions, events) = main_parts(&decision);
	let body = decision_body(&decisions, id);
	assert_eq!(keys(body), ["potential", "allow", "effect", "ai_will_do"]);
	// Shown to players while some entry could be offered and no workflow of
	// this decision is open.
	let potential = child(body, "potential");
	assert_eq!(keys(potential), ["ai", "NOT", "OR"]);
	assert_eq!(
		keys(child(child(potential, "OR"), "AND")),
		["ai", "has_country_flag", "religion"]
	);
	assert_eq!(
		keys(child(body, "effect")),
		["set_country_flag", "country_event"]
	);
	assert!(script(&decision, &format!("decisions/{id}.txt")).contains("factor = 0"));

	let options = event_options(&events);
	assert_eq!(options.len(), 2, "the action and cancel");
	assert_eq!(
		keys(options[0]),
		[
			"name",
			"trigger",
			"custom_tooltip",
			"if",
			&format!("{id}_release")
		]
	);
	// Panel gates, then the button's potential and trigger, checked to offer
	// the option and again when it is taken.
	assert_eq!(
		keys(child(options[0], "trigger")),
		["ai", "has_country_flag", "religion", "is_owner_able"]
	);
	assert_eq!(
		keys(child(options[0], "if")),
		["limit", "spend_purity", "add_country_modifier"]
	);
	let events_text = script(&decision, &format!("events/{id}.txt"));
	assert!(events_text.contains("amount = 20") && events_text.contains("duration = 3650"));
	assert_eq!(keys(options[1]), ["name", &format!("{id}_release")]);

	let release = parse(
		"x.txt",
		script(&decision, &format!("common/scripted_effects/{id}.txt")),
	);
	assert_eq!(
		keys(child(&release.statements, &format!("{id}_release"))),
		["clr_country_flag"]
	);
}

#[test]
fn titles_name_the_mod_and_options_the_panel_and_button() {
	let decision = single_decision(&decision_ready_action("add_prestige = 1"));
	let id = &decision.id;
	let english = english(&decision);
	assert!(
		english.contains(&format!("{id}_title:0 \"Religion Compatibility Expanded\"")),
		"{english}"
	);
	assert!(
		english.contains(&format!("{id}_entry_1:0 \"rce_purity::knowledge_button\"")),
		"{english}"
	);
	assert_eq!(
		decision.required_localisation,
		["CANCEL", "rce_purity_knowledge_button_tt"]
	);
	assert_eq!(
		decision.localisation.len(),
		decision::DECISION_LANGUAGES.len()
	);
	for (path, text) in &decision.localisation {
		let parsed = crate::game::eu4::script::localisation::parse_localisation_bytes(
			"foch",
			&crate::model::GamePathBuf::parse(path).expect("test game path"),
			text.as_bytes(),
		);
		assert!(
			parsed.parse_issues.is_empty(),
			"{path}: {:?}",
			parsed.parse_issues
		);
	}
}

#[test]
fn panel_actions_are_one_entry_with_an_event_of_their_own() {
	let decision = single_decision(&decision_ready_panel(&[
		("rce_purity_knowledge_button", "add_adm_power = -20"),
		(
			"rce_purity_wind_button",
			"add_dip_power = -10 add_prestige = 1",
		),
	]));
	let id = &decision.id;
	assert_eq!(
		decision.actions,
		["rce_purity_knowledge_button", "rce_purity_wind_button"]
	);
	let (decisions, events) = main_parts(&decision);
	// The panel is visible while its gates hold and any action's potential does.
	let visible = child(
		child(child(decision_body(&decisions, id), "potential"), "OR"),
		"AND",
	);
	assert_eq!(keys(visible), ["ai", "has_country_flag", "OR"]);
	let options = event_options(&events);
	assert_eq!(options.len(), 2, "the panel and cancel");
	assert_eq!(keys(options[0]), ["name", "trigger", "country_event"]);
	let any = child(child(options[0], "trigger"), "OR");
	assert_eq!(keys(children(any, "AND")[0]), ["religion", "is_owner_able"]);

	let panel_event = decision
		.scripts
		.iter()
		.find(|(path, _)| path.starts_with("events/") && !path.contains(id.as_str()))
		.expect("the panel has its own event");
	let parsed = parse(&panel_event.0, &panel_event.1);
	let options = event_options(&parsed);
	assert_eq!(options.len(), 3, "two actions and cancel");
	for option in &options[..2] {
		assert_eq!(
			keys(option),
			[
				"name",
				"trigger",
				"custom_tooltip",
				"if",
				&format!("{id}_release")
			]
		);
		assert_eq!(
			keys(child(option, "trigger")),
			["ai", "has_country_flag", "religion", "is_owner_able"]
		);
	}
	assert_eq!(
		keys(child(options[1], "if")),
		["limit", "add_dip_power", "add_prestige"]
	);
	assert_eq!(keys(options[2]), ["name", &format!("{id}_release")]);

	let english = english(&decision);
	assert!(english.contains(&format!("{id}_entry_1:0 \"rce_purity\"")));
	assert!(english.contains("_title:0 \"Religion Compatibility Expanded::rce_purity\""));
	assert!(english.contains("_option_1:0 \"knowledge_button\""));
	assert!(english.contains("_option_2:0 \"wind_button\""));
	assert_eq!(
		decision.required_localisation,
		[
			"CANCEL",
			"rce_purity_knowledge_button_tt",
			"rce_purity_wind_button_tt"
		]
	);
}

#[test]
fn decision_ids_are_stable_and_bound_to_the_source_mod() {
	let fixture = decision_ready_action("add_prestige = 1");
	let first = single_decision(&fixture);
	assert_eq!(first, single_decision(&fixture));
	let other = route(
		&fixture,
		decision::SourceMod {
			id: "2164202838",
			name: "Europa Expanded",
		},
	);
	assert_ne!(first.id, other.decisions[0].id);
}

#[test]
fn decision_title_drops_localisation_markup_from_the_mod_name() {
	let plan = route(
		&decision_ready_action("add_prestige = 1"),
		decision::SourceMod {
			id: "1",
			name: " Ages \"Reformed\" $KEY$ [Root.GetName] \u{a7}Y\u{a3}gold ",
		},
	);
	let english = english(&plan.decisions[0]);
	assert!(
		english.contains("_title:0 \"Ages 'Reformed' KEY Root.GetName Ygold\""),
		"{english}"
	);
}

#[test]
fn refused_actions_leave_the_rest_of_the_panel_usable() {
	let plan = route(
		&decision_ready_panel(&[
			("rce_purity_knowledge_button", "add_adm_power = -20"),
			(
				"rce_purity_wind_button",
				"event_target:chosen_heir = { add_prestige = 1 }",
			),
		]),
		RCE,
	);
	assert_eq!(plan.decisions.len(), 1);
	assert_eq!(plan.decisions[0].actions, ["rce_purity_knowledge_button"]);
	assert_eq!(plan.rejected.len(), 1);
	assert_eq!(plan.rejected[0].0, "rce_purity_wind_button");
	assert!(
		plan.rejected[0]
			.1
			.join("\n")
			.contains("event target `chosen_heir` is not saved")
	);
}

#[test]
fn decision_route_rejects_context_it_cannot_supply() {
	let reasons = |fixture: &Fixture| {
		let plan = route(fixture, RCE);
		assert!(plan.decisions.is_empty(), "{:?}", plan.decisions);
		plan.rejected
			.iter()
			.flat_map(|(_, reasons)| reasons.iter().cloned())
			.collect::<Vec<_>>()
			.join("\n")
	};
	assert!(
		reasons(&decision_ready_action(
			"event_target:chosen_heir = { add_prestige = 1 }"
		))
		.contains("event target `chosen_heir` is not saved")
	);
	single_decision(&decision_ready_action(
		"capital_scope = { save_event_target_as = home } event_target:home = { add_base_tax = 1 }",
	));
	assert!(
		reasons(&decision_ready_action(
			"mystery_block = { add_prestige = 1 }"
		))
		.contains("extraction: unknown block `mystery_block`")
	);

	let mut diplomacy = targeted_action();
	diplomacy.interface = vec![parse(
		"interface/countrydiplomacyview.gui",
		r#"guiTypes = { windowType = { name = "countrydiplimacyview"
			guiButtonType = { name = "transfer_occupation_button" scripted = yes } } }"#,
	)];
	assert!(reasons(&diplomacy).contains("only country and province hosts have a decision route"));

	let fixture = decision_ready_action("add_prestige = 1");
	let catalog = fixture.catalog();
	let mut untitled = features(&fixture);
	untitled[0].tooltip = None;
	let plan = decision::decision_route(&catalog, &untitled, RCE, &RouteContext::default());
	assert!(
		plan.rejected[0]
			.1
			.join("\n")
			.contains("no custom_gui `tooltip`")
	);
}

#[test]
fn country_from_is_rebound_to_the_deciding_country() {
	let mut fixture = decision_ready_action(
		"FROM = { add_prestige = 1 } from_effect = yes spend_purity = { amount = 20 }",
	);
	fixture.effects.push(source(
		"common/scripted_effects/from.txt",
		"from_effect = { FROM = { add_legitimacy = $gain|2$ } }",
	));
	let decision = single_decision(&fixture);
	let (_, events) = main_parts(&decision);
	// `FROM` becomes `ROOT`; the call that used it is expanded in place, with
	// its default parameter, while the call that did not stays a call.
	assert_eq!(
		keys(child(event_options(&events)[0], "if")),
		["limit", "ROOT", "ROOT", "spend_purity"]
	);
	let text = script(&decision, &format!("events/{}.txt", decision.id));
	assert!(text.contains("add_legitimacy = 2"), "{text}");
	assert!(
		!text.contains("FROM") && !text.contains("from_effect"),
		"{text}"
	);
}

fn province_fixture(potential: &str) -> Fixture {
	let mut fixture = targeted_action();
	fixture.custom_gui = vec![parse(
		"common/custom_gui/occupation.txt",
		&format!(
			r#"
custom_button = {{
	name = transfer_occupation_button
	tooltip = transfer_occupation_tt
	potential = {{ {potential} }}
	effect = {{
		FROM = {{ add_adm_power = -10 }}
		owner = {{ PREV = {{ change_controller = FROM }} }}
		ROOT = {{ add_base_tax = 1 }}
		tax_root = yes
	}}
}}
"#
		),
	)];
	fixture.effects = vec![source(
		"common/scripted_effects/tax.txt",
		"tax_root = { ROOT = { add_base_production = 1 } }",
	)];
	fixture
}

fn province_entry_id(decision: &decision::GeneratedDecision) -> String {
	decision
		.scripts
		.iter()
		.find_map(|(path, _)| {
			path.strip_prefix("common/scripted_triggers/")
				.and_then(|name| name.strip_suffix(".txt"))
		})
		.expect("a province search has scripted triggers")
		.to_owned()
}

#[test]
fn province_actions_are_offered_only_when_a_province_qualifies() {
	let decision = single_decision(&province_fixture("controller = { alliance_with = FROM }"));
	let id = &decision.id;
	let n = province_entry_id(&decision);
	for (path, text) in &decision.scripts {
		parse(path, text);
		assert!(
			!text.contains("FROM"),
			"{path}: `FROM` must be rebound\n{text}"
		);
	}
	let (decisions, events) = main_parts(&decision);
	let body = decision_body(&decisions, id);
	// The decision shows only while some province would show the button.
	let visible = child(child(child(body, "potential"), "OR"), "AND");
	let anywhere = child(child(visible, "any_province"), "OR");
	assert_eq!(keys(child(anywhere, "AND")), ["controller"]);
	assert!(script(&decision, &format!("decisions/{id}.txt")).contains("alliance_with = ROOT"));
	assert!(decision.approximate_visibility.is_empty());
	// Taking it gives the player a number, marks the provinces where the
	// button is visible...
	let effect = child(body, "effect");
	assert_eq!(
		keys(effect),
		[
			"set_country_flag",
			"if",
			&format!("{n}_mark"),
			"country_event"
		]
	);
	// ...and the search is offered only if one was marked.
	let option = event_options(&events)[0];
	assert_eq!(keys(option), ["name", "trigger", &format!("{n}_route")]);
	assert_eq!(keys(child(option, "trigger")), [format!("{n}_any")]);

	assert_eq!(
		paths(&decision)[3..],
		[
			format!("events/{n}.txt"),
			format!("common/scripted_effects/{n}.txt"),
			format!("common/scripted_triggers/{n}.txt"),
		]
	);
	let triggers = parse(
		"x.txt",
		script(&decision, &format!("common/scripted_triggers/{n}.txt")),
	);
	assert_eq!(
		keys(&triggers.statements),
		[
			format!("{n}_visible_1"),
			format!("{n}_allowed_1"),
			format!("{n}_any_s"),
			format!("{n}_unlisted_s"),
			format!("{n}_in_slot_s"),
			format!("{n}_any"),
			format!("{n}_unlisted"),
			format!("{n}_in_slot"),
		]
	);

	let effects_text = script(&decision, &format!("common/scripted_effects/{n}.txt"));
	let effects = parse("x.txt", effects_text);
	assert_eq!(
		keys(&effects.statements),
		[
			format!("{n}_run_1"),
			format!("{n}_mark_s"),
			format!("{n}_route_s"),
			format!("{n}_fill_s"),
			format!("{n}_close_s"),
			format!("{n}_mark"),
			format!("{n}_route"),
			format!("{n}_fill"),
			format!("{n}_close"),
		]
	);
	let marking = child(
		child(
			child(
				child(&effects.statements, &format!("{n}_mark_s")),
				"every_province",
			),
			"if",
		),
		"limit",
	);
	assert_eq!(keys(child(marking, "OR")), [format!("{n}_visible_1")]);

	let list = parse("x.txt", script(&decision, &format!("events/{n}.txt")));
	let options = event_options(&list);
	assert_eq!(
		options.len(),
		super::province::LISTED_PROVINCES + 2,
		"slots, more, cancel"
	);
	assert_eq!(
		keys(options[0]),
		[
			"name",
			"trigger",
			&format!("event_target:{n}_slot_1"),
			"custom_tooltip",
			&format!("{n}_run_1"),
			&format!("{id}_release"),
		]
	);
	let slot_check = child(
		child(options[0], "trigger"),
		&format!("event_target:{n}_slot_1"),
	);
	assert_eq!(
		keys(slot_check),
		[format!("{n}_in_slot"), format!("{n}_allowed_1")]
	);

	// The action runs in the chosen province, its gates checked again, with
	// `ROOT` rebound to that province and the call using it expanded.
	let run = child(
		child(
			child(&effects.statements, &format!("{n}_run_1")),
			"event_target:$target$",
		),
		"if",
	);
	assert_eq!(
		keys(run),
		[
			"limit",
			"ROOT",
			"owner",
			"event_target:$target$",
			"event_target:$target$",
		]
	);
	assert!(effects_text.contains("change_controller = ROOT"));
	assert!(effects_text.contains("add_base_production = 1") && !effects_text.contains("tax_root"));
	// Without geography the search lists provinces page by page.
	assert_eq!(
		keys(child(&effects.statements, &format!("{n}_route_s"))),
		["if", "else"]
	);

	// The release clears this player's search and the lock.
	let release = parse(
		"x.txt",
		script(&decision, &format!("common/scripted_effects/{id}.txt")),
	);
	assert_eq!(
		keys(child(&release.statements, &format!("{id}_release"))),
		[format!("{n}_close"), "clr_country_flag".into()]
	);

	let english = english(&decision);
	assert!(english.contains(&format!("{n}_slot_1_name:0 \"[{n}_slot_1.GetName]\"")));
	assert!(english.contains(&format!(
		"{n}_title:0 \"Religion Compatibility Expanded::transfer_occupation_button\""
	)));
}

#[test]
fn each_player_searches_with_province_flags_of_their_own_number() {
	let decision = single_decision(&province_fixture("controller = { alliance_with = FROM }"));
	let id = &decision.id;
	let n = province_entry_id(&decision);
	let effects_text = script(&decision, &format!("common/scripted_effects/{n}.txt"));
	let triggers_text = script(&decision, &format!("common/scripted_triggers/{n}.txt"));
	// Every province flag of the search carries the player's number.
	for text in [effects_text, triggers_text] {
		for line in text.lines().filter(|line| line.contains("province_flag")) {
			assert!(line.contains("_$slot$"), "unnumbered province flag: {line}");
		}
	}
	// A number is taken once, from the first free one, when the decision is
	// taken; a player who has one keeps it.
	let decisions = parse("d.txt", script(&decision, &format!("decisions/{id}.txt")));
	let take = child(child(decision_body(&decisions, id), "effect"), "if");
	let limit = child(child(child(take, "limit"), "NOT"), "OR");
	assert_eq!(keys(limit).len(), super::province::PLAYER_SLOTS);
	assert_eq!(keys(take).len(), 1 + super::province::PLAYER_SLOTS);
	assert_eq!(
		keys(child(take, "if")),
		["limit", "set_global_flag", "set_country_flag"]
	);
	// Dispatchers pass the deciding player's number to the numbered form.
	let effects = parse("x.txt", effects_text);
	let route = child(&effects.statements, &format!("{n}_route"));
	assert_eq!(keys(route).len(), super::province::PLAYER_SLOTS);
	let first = child(route, "if");
	assert_eq!(
		keys(child(child(first, "limit"), "ROOT")),
		["has_country_flag"]
	);
	assert_eq!(keys(child(first, &format!("{n}_route_s"))), ["slot"]);
}

#[test]
fn deep_province_references_make_the_main_decision_visibility_approximate() {
	// `ROOT` directly in scope is the candidate province; one scope deeper it
	// is `PREV`; deeper still a trigger cannot name it.
	let exact = single_decision(&province_fixture(
		"ROOT = { owner = { war_with = FROM } } owner = { is_core = ROOT }",
	));
	assert!(exact.approximate_visibility.is_empty());
	let text = script(&exact, &format!("decisions/{}.txt", exact.id));
	assert!(
		text.contains("war_with = ROOT") && text.contains("is_core = PREV"),
		"{text}"
	);

	let deep = single_decision(&province_fixture(
		"FROM = { any_ally = { ROOT = { controlled_by = PREV } } }",
	));
	assert_eq!(deep.approximate_visibility, ["transfer_occupation_button"]);
	let (decisions, _) = main_parts(&deep);
	let visible = child(
		child(decision_body(&decisions, &deep.id), "potential"),
		"OR",
	);
	assert_eq!(keys(visible), ["always"]);
}

#[test]
fn many_candidates_narrow_by_superregion_region_and_area() {
	let geography = Geography {
		superregions: vec![("europe_superregion".into(), vec!["france_region".into()])],
		regions: vec![(
			"france_region".into(),
			vec!["ile_de_france_area".into(), "normandy_area".into()],
		)],
	};
	let fixture = targeted_action();
	let catalog = fixture.catalog();
	let plan = decision::decision_route(
		&catalog,
		&features(&fixture),
		RCE,
		&RouteContext {
			geography,
			..Default::default()
		},
	);
	let decision = &plan.decisions[0];
	let n = province_entry_id(decision);
	let events = parse("events/x.txt", script(decision, &format!("events/{n}.txt")));
	let ids = children(&events.statements, "country_event")
		.iter()
		.map(|event| {
			event
				.iter()
				.find_map(|statement| match statement {
					AstStatement::Assignment {
						key,
						value: AstValue::Scalar { value, .. },
						..
					} if key == "id" => Some(value.as_text()),
					_ => None,
				})
				.unwrap()
		})
		.collect::<Vec<_>>();
	assert_eq!(
		ids,
		[
			format!("{n}.1"),
			format!("{n}.3"),
			format!("{n}.10"),
			format!("{n}.200")
		]
	);
	let areas = children(&events.statements, "country_event")[3];
	let names = children(areas, "option")
		.iter()
		.map(|option| match &option[0] {
			AstStatement::Assignment {
				value: AstValue::Scalar { value, .. },
				..
			} => value.as_text(),
			_ => panic!("option without name"),
		})
		.collect::<Vec<_>>();
	assert_eq!(
		names,
		[
			"ile_de_france_area".to_owned(),
			"normandy_area".into(),
			format!("{n}_all"),
			"CANCEL".into()
		]
	);
	let effects = parse(
		"x.txt",
		script(decision, &format!("common/scripted_effects/{n}.txt")),
	);
	assert_eq!(
		keys(child(&effects.statements, &format!("{n}_route_s"))),
		["if", "else_if", "else_if", "else_if", "else"]
	);
	for name in [
		"europe_superregion",
		"france_region",
		"normandy_area",
		"CANCEL",
	] {
		assert!(
			decision.required_localisation.iter().any(|key| key == name),
			"{name}"
		);
	}
}

#[test]
fn geography_reads_the_map_groups() {
	let superregion = parse(
		"map/superregion.txt",
		"europe_superregion = { france_region restrict_charter }\nempty_superregion = { }",
	);
	let region = parse(
		"map/region.txt",
		"france_region = { areas = { ile_de_france_area normandy_area } monsoon = { 00.01.01 } }\nrandom_new_world_region = { }",
	);
	assert_eq!(
		Geography::from_map(&superregion, &region),
		Geography {
			superregions: vec![("europe_superregion".into(), vec!["france_region".into()])],
			regions: vec![(
				"france_region".into(),
				vec!["ile_de_france_area".into(), "normandy_area".into()],
			)],
		}
	);
}

#[test]
fn inline_text_rebinds_root_and_from_like_the_script() {
	let province = province_fixture(
		r#"custom_trigger_tooltip = {
			tooltip = "[Root.GetName] is held by an ally of [FROM.GetName], here [This.GetName]"
			controller = { alliance_with = FROM }
		}"#,
	);
	let decision = single_decision(&province);
	let n = province_entry_id(&decision);
	let triggers = script(&decision, &format!("common/scripted_triggers/{n}.txt"));
	assert!(
		triggers.contains(
			"\"[$target$.GetName] is held by an ally of [Root.GetName], here [This.GetName]\""
		),
		"{triggers}"
	);

	let mut country =
		decision_ready_action("custom_tooltip = \"[From.GetName] pays\" pay_note = yes");
	country.effects.push(source(
		"common/scripted_effects/note.txt",
		"pay_note = { custom_tooltip = \"[From.Monarch.GetName] signs\" }",
	));
	let decision = single_decision(&country);
	let text = script(&decision, &format!("events/{}.txt", decision.id));
	assert!(text.contains("\"[Root.GetName] pays\""), "{text}");
	// A call whose only reference is in its text is expanded and rebound too.
	assert!(
		text.contains("\"[Root.Monarch.GetName] signs\"") && !text.contains("pay_note"),
		"{text}"
	);
	assert!(!text.contains("From"), "{text}");
}

fn context_with_text(english: &str) -> RouteContext {
	let mut context = RouteContext::default();
	context
		.localisation
		.add_file(format!("\u{feff}l_english:\n{english}").as_bytes());
	context
}

fn route_in(fixture: &Fixture, context: &RouteContext) -> decision::GeneratedDecision {
	let catalog = fixture.catalog();
	let plan = decision::decision_route(&catalog, &features(fixture), RCE, context);
	assert!(plan.rejected.is_empty(), "{:?}", plan.rejected);
	plan.decisions.into_iter().next().unwrap()
}

#[test]
fn shown_text_that_names_root_or_from_is_derived_for_the_province() {
	let mut fixture = province_fixture(
		"custom_trigger_tooltip = { tooltip = held_tt controller = { alliance_with = FROM } }",
	);
	fixture.custom_gui = vec![parse(
		"common/custom_gui/occupation.txt",
		r#"custom_button = {
	name = transfer_occupation_button
	tooltip = transfer_occupation_tt
	potential = { custom_trigger_tooltip = { tooltip = held_tt controller = { alliance_with = FROM } } }
	effect = { add_base_tax = 1 }
}"#,
	)];
	let context = context_with_text(
		" transfer_occupation_tt:0 \"Take [Root.GetName] for [From.GetName]\"\n held_tt:0 \"[Root.GetName] is held by an ally\"\n",
	);
	let decision = route_in(&fixture, &context);
	let id = &decision.id;
	let n = province_entry_id(&decision);
	// A listed province's option shows the tooltip about that province.
	let list = parse("x.txt", script(&decision, &format!("events/{n}.txt")));
	let first = event_options(&list)[0];
	let shown = format!("{id}_{n}_slot_1_transfer_occupation_tt");
	assert!(
		script(&decision, &format!("events/{n}.txt"))
			.contains(&format!("custom_tooltip = {shown}")),
		"{:?}",
		keys(first)
	);
	// Script shared by every province names the key through `$target$`.
	let triggers = script(&decision, &format!("common/scripted_triggers/{n}.txt"));
	assert!(
		triggers.contains(&format!("tooltip = {id}_$target$_held_tt")),
		"{triggers}"
	);
	let english = english(&decision);
	assert!(english.contains(&format!(
		"{shown}:0 \"Take [{n}_slot_1.GetName] for [Root.GetName]\""
	)));
	for instance in ["probe", "target", "slot_1", "slot_10"] {
		assert!(
			english.contains(&format!(
				"{id}_{n}_{instance}_held_tt:0 \"[{n}_{instance}.GetName] is held by an ally\""
			)),
			"{instance}\n{english}"
		);
	}
	assert!(
		!decision
			.required_localisation
			.iter()
			.any(|key| key == "transfer_occupation_tt" || key == "held_tt")
	);
}

#[test]
fn country_text_keeps_keys_without_scope_references() {
	let fixture = decision_ready_panel(&[
		("rce_purity_knowledge_button", "add_adm_power = -20"),
		("rce_purity_wind_button", "add_dip_power = -10"),
	]);
	let mut context = context_with_text(
		" rce_purity_knowledge_button_tt:0 \"Offer to the god\"\n rce_purity_wind_button_tt:0 \"[From.GetName] calls the wind: [Root.GetWind]\"\n wind_strong:0 \"strong for [From.GetName]\"\n wind_calm:0 \"calm\"\n",
	);
	context.custom_localisation = crate::game::eu4::derived_localisation::CustomLocalisation::new(
		&[parse(
			"customizable_localization/wind.txt",
			"defined_text = { name = GetWind text = { localisation_key = wind_strong trigger = { FROM = { prestige = 50 } } } text = { localisation_key = wind_calm } }",
		)],
	);
	let decision = route_in(&fixture, &context);
	let id = &decision.id;
	let panel_events = decision
		.scripts
		.iter()
		.find(|(path, _)| path.starts_with("events/") && !path.contains(id.as_str()))
		.unwrap();
	// Unchanged text keeps its key; text naming `From` gets a derived one.
	assert!(
		panel_events
			.1
			.contains("custom_tooltip = rce_purity_knowledge_button_tt")
	);
	assert!(
		panel_events
			.1
			.contains(&format!("custom_tooltip = {id}_rce_purity_wind_button_tt"))
	);
	let english = english(&decision);
	assert!(english.contains(&format!(
		"{id}_rce_purity_wind_button_tt:0 \"[Root.GetName] calls the wind: [Root.{id}_GetWind]\""
	)));
	assert!(english.contains(&format!("{id}_wind_strong:0 \"strong for [Root.GetName]\"")));
	// The command is copied with its trigger and key rebound.
	let commands = script(&decision, &format!("customizable_localization/{id}.txt"));
	assert!(
		commands.contains(&format!("name = {id}_GetWind")),
		"{commands}"
	);
	assert!(
		commands.contains("ROOT = {") && !commands.contains("FROM"),
		"{commands}"
	);
	assert!(commands.contains(&format!("localisation_key = {id}_wind_strong")));
	assert!(commands.contains("localisation_key = wind_calm"));
	assert!(
		decision
			.required_localisation
			.contains(&"rce_purity_knowledge_button_tt".to_owned())
	);
	// Only English text exists, so the other languages are reported.
	assert!(
		decision
			.text_problems
			.iter()
			.all(|problem| problem.contains("has no") && !problem.contains("english")),
		"{:?}",
		decision.text_problems
	);
}

#[test]
fn reviewed_overrides_replace_text_and_visibility() {
	let config: crate::project::Project = toml::from_str(
		r#"
[[gui.text]]
key = "transfer_occupation_tt"
text = "Ask the ally holding [Root.GetName]"

[[gui.visibility]]
mod = "3342969370"
button = "transfer_occupation_button"
trigger = "controller = { alliance_with = FROM }"
"#,
	)
	.unwrap();
	let context = RouteContext {
		overrides: GuiOverrides::from_config(&config.gui),
		..Default::default()
	};
	// The button's own condition names the province two scopes deep.
	let decision = route_in(
		&province_fixture("FROM = { any_ally = { ROOT = { controlled_by = PREV } } }"),
		&context,
	);
	let id = &decision.id;
	let n = province_entry_id(&decision);
	assert!(decision.approximate_visibility.is_empty());
	assert!(script(&decision, &format!("decisions/{id}.txt")).contains("alliance_with = ROOT"));
	// The reviewed text is written like the mod's and rebound the same way.
	assert!(english(&decision).contains(&format!(
		"{id}_{n}_slot_1_transfer_occupation_tt:0 \"Ask the ally holding [{n}_slot_1.GetName]\""
	)));
	assert!(
		decision.text_problems.is_empty(),
		"{:?}",
		decision.text_problems
	);
}
