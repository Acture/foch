//! Province search entry for actions whose host selects a province.
//!
//! When the main decision is taken it marks every province where an action
//! would be visible to the deciding country; the entry is offered only if
//! one is marked. The search narrows them by superregion, region and area
//! until at most [`LISTED_PROVINCES`] remain, and lists those by name. The
//! chosen province is saved as an event target and the action runs in its
//! scope with every gate checked again, so `ROOT` and `FROM` keep their host
//! meanings through [`super::context::Rewrite`]. The main decision's release
//! clears all workflow state on completion or cancel.
//!
//! Province flags are shared by every country, and a flag name cannot be
//! built at run time. So each human player who opens a search takes one of
//! [`PLAYER_SLOTS`] numbers, kept for the game, and every province flag of
//! the search carries it: players can search at the same time without
//! touching each other's candidates. Operations on flags are generated once
//! per number-taking `slot` parameter, and a dispatcher picks the player's
//! number.

use super::context::{Localise, TARGET_PARAMETER};
use super::decision::{
	Entry, Main, Panel, RouteContext, assign, block, button_label, emit, entry_label, generated_id,
	identifier, literal_text, number, own_checks, panel_gates, statements,
};
use super::geography::Geography;
use super::{ExtractedFeature, FeatureCatalog, SourceMod, Stage};
use crate::game::eu4::script::parser::AstStatement;

/// Provinces listed by name once the search has narrowed them this far.
pub const LISTED_PROVINCES: usize = 10;

/// Human players who can each run searches; EU4 multiplayer's maximum.
pub const PLAYER_SLOTS: usize = 32;

/// Country and global flag prefix of player numbers, shared by every
/// generated decision so a player keeps one number.
const PLAYER_FLAG: &str = "foch_gui_player";

struct Names {
	id: String,
}

impl Names {
	fn event(&self, number: usize) -> String {
		format!("{}.{number}", self.id)
	}
	fn name(&self, suffix: &str) -> String {
		format!("{}_{suffix}", self.id)
	}
	/// Event target of a listed province; event targets belong to the
	/// player's own event chain.
	fn slot(&self, index: usize) -> String {
		self.name(&format!("slot_{index}"))
	}
	/// A province flag of the player numbered `$slot$`.
	fn flag(&self, suffix: &str) -> String {
		self.name(&format!("{suffix}_$slot$"))
	}
	/// The per-number form of a dispatched operation.
	fn numbered(&self, operation: &str) -> String {
		self.name(&format!("{operation}_s"))
	}
}

const LIST_EVENT: usize = 1;
const ACTION_EVENT: usize = 2;
const SUPERREGION_EVENT: usize = 3;
const REGION_EVENTS: usize = 10;
const AREA_EVENTS: usize = 200;

/// Give the deciding country a player number if it has none and one is free.
pub(super) fn take_player_number() -> Vec<AstStatement> {
	let has_any = assign(
		"OR",
		block(
			(1..=PLAYER_SLOTS)
				.map(|slot| {
					assign(
						"has_country_flag",
						identifier(&format!("{PLAYER_FLAG}_{slot}")),
					)
				})
				.collect(),
		),
	);
	let mut chain = Vec::new();
	for slot in 1..=PLAYER_SLOTS {
		let flag = format!("{PLAYER_FLAG}_{slot}");
		chain.push(assign(
			if slot == 1 { "if" } else { "else_if" },
			block(vec![
				assign(
					"limit",
					block(vec![assign(
						"NOT",
						block(vec![assign("has_global_flag", identifier(&flag))]),
					)]),
				),
				assign("set_global_flag", identifier(&flag)),
				assign("set_country_flag", identifier(&flag)),
			]),
		));
	}
	vec![assign(
		"if",
		block(
			[
				vec![assign(
					"limit",
					block(vec![assign("NOT", block(vec![has_any]))]),
				)],
				chain,
			]
			.concat(),
		),
	)]
}

pub(super) fn province_entry(
	catalog: &FeatureCatalog,
	members: &[&ExtractedFeature],
	panel: Option<&Panel>,
	source: SourceMod<'_>,
	context: &RouteContext,
	main: &Main,
	texts: &Localise<'_>,
) -> Result<Entry, String> {
	let geography = &context.geography;
	let release = main.release();
	let (gui_file, ancestry) = match panel {
		Some(panel) if members.len() > 1 => (panel.gui_file.clone(), panel.ancestry.clone()),
		_ => (
			members[0].widget.gui_file.clone(),
			members[0]
				.widget
				.ancestry
				.iter()
				.map(|node| node.name.clone())
				.collect(),
		),
	};
	let ancestry = ancestry.iter().map(String::as_str).collect::<Vec<_>>();
	let n = Names {
		id: generated_id(source.id, &gui_file, &ancestry),
	};
	let gates = panel_gates(members)?;
	let grouped = members.len() > 1;

	// Each action's visibility, full checks and effect are generated once as
	// scripted triggers and effects taking the province's event target name,
	// so `$target$` is substituted by the game at every use.
	let parameter = TARGET_PARAMETER;
	// Every event target name `$target$` is called with, for derived text.
	let instances = [n.name("probe"), n.name("target")]
		.into_iter()
		.chain((1..=LISTED_PROVINCES).map(|index| n.slot(index)))
		.collect::<Vec<_>>();
	let rewrite = catalog.rewrite_text(Some(parameter), &instances, texts);
	let in_target = |statements: Vec<AstStatement>| {
		vec![assign(
			&format!("event_target:{parameter}"),
			block(statements),
		)]
	};
	let mut triggers = Vec::new();
	let mut effects = Vec::new();
	for (index, feature) in members.iter().enumerate() {
		let own = own_checks(feature);
		let mut visible =
			rewrite.statements(Stage::Potential, &statements(&gates, Stage::Potential));
		visible.extend(rewrite.statements(Stage::Potential, &statements(&own, Stage::Potential)));
		let mut allowed = visible.clone();
		allowed.extend(rewrite.statements(Stage::Trigger, &statements(&own, Stage::Trigger)));
		triggers.push(assign(
			&n.name(&format!("visible_{}", index + 1)),
			block(in_target(visible)),
		));
		triggers.push(assign(
			&n.name(&format!("allowed_{}", index + 1)),
			block(in_target(allowed)),
		));
		// The action runs in the chosen province, every gate checked again:
		// an event can stay open while the game state changes.
		let mut guarded = vec![assign(
			"limit",
			block(vec![with(
				&n.name(&format!("allowed_{}", index + 1)),
				&[("target", parameter)],
			)]),
		)];
		guarded.extend(rewrite.statements(Stage::Effect, &feature.effect));
		effects.push(assign(
			&n.name(&format!("run_{}", index + 1)),
			block(in_target(vec![assign("if", block(guarded))])),
		));
	}
	let allowed = |index: usize, target: &str| {
		with(
			&n.name(&format!("allowed_{}", index + 1)),
			&[("target", target)],
		)
	};
	let run = |index: usize| {
		vec![
			with(
				&n.name(&format!("run_{}", index + 1)),
				&[("target", &n.name("target"))],
			),
			call(&release),
		]
	};

	// Operations on the player's province flags, per number.
	let probe = n.name("probe");
	effects.push(numbered_effect(
		&n,
		"mark",
		vec![assign(
			"every_province",
			block(vec![
				assign("save_event_target_as", identifier(&probe)),
				assign(
					"if",
					block(vec![
						assign(
							"limit",
							block(vec![assign(
								"OR",
								block(
									(0..members.len())
										.map(|index| {
											with(
												&n.name(&format!("visible_{}", index + 1)),
												&[("target", &probe)],
											)
										})
										.collect(),
								),
							)]),
						),
						assign("set_province_flag", identifier(&n.flag("cand"))),
					]),
				),
			]),
		)],
	));
	effects.push(numbered_effect(&n, "route", route(&n, geography)));
	effects.push(numbered_effect(&n, "fill", fill(&n)));
	effects.push(numbered_effect(&n, "close", close(&n)));
	triggers.push(numbered_trigger(
		&n,
		"any",
		vec![assign(
			"any_province",
			block(vec![assign(
				"has_province_flag",
				identifier(&n.flag("cand")),
			)]),
		)],
	));
	triggers.push(numbered_trigger(
		&n,
		"unlisted",
		vec![assign(
			"any_province",
			block(vec![
				assign("has_province_flag", identifier(&n.flag("cand"))),
				assign(
					"NOT",
					block(vec![assign(
						"has_province_flag",
						identifier(&n.flag("listed")),
					)]),
				),
			]),
		)],
	));
	// In province scope: this province fills list position `$index$`.
	triggers.push(numbered_trigger(
		&n,
		"in_slot",
		vec![assign(
			"has_province_flag",
			identifier(&n.name("slot_$index$_$slot$")),
		)],
	));
	for (operation, parameters) in [
		("mark", &[][..]),
		("route", &[]),
		("fill", &[]),
		("close", &[]),
	] {
		effects.push(dispatch(&n, operation, parameters, false));
	}
	for (operation, parameters) in [("any", &[][..]), ("unlisted", &[]), ("in_slot", &["index"])] {
		triggers.push(dispatch(&n, operation, parameters, true));
	}

	// The main decision's `potential` iterates provinces from the deciding
	// country; a check that names the province more than one scope deep has
	// no trigger form, and the decision is then shown for this entry.
	let relative = catalog.rewrite(None);
	let visible_anywhere = members
		.iter()
		.map(|feature| {
			// A reviewed replacement stands for the gates and potential.
			let reviewed = context
				.overrides
				.visibility
				.get(&(source.id.to_owned(), feature.widget.leaf().name.clone()));
			let checks = match reviewed {
				Some(trigger) => trigger.clone(),
				None => {
					let mut checks = statements(&gates, Stage::Potential);
					checks.extend(statements(&own_checks(feature), Stage::Potential));
					checks
				}
			};
			relative
				.relative(Stage::Potential, &checks)
				.map(|checks| assign("AND", block(checks)))
		})
		.collect::<Option<Vec<_>>>()
		.map(|checks| {
			vec![assign(
				"any_province",
				block(vec![assign("OR", block(checks))]),
			)]
		});

	let mut events = vec![assign("namespace", identifier(&n.id))];
	let mut list_options = Vec::new();
	for index in 1..=LISTED_PROVINCES {
		let slot = n.slot(index);
		let mut inside = vec![with(&n.name("in_slot"), &[("index", &index.to_string())])];
		if grouped {
			inside.push(assign(
				"OR",
				block(
					(0..members.len())
						.map(|member| allowed(member, &slot))
						.collect(),
				),
			));
		} else {
			inside.push(allowed(0, &slot));
		}
		let mut option = vec![
			assign("name", identifier(&n.name(&format!("slot_{index}_name")))),
			assign(
				"trigger",
				block(vec![
					assign("has_saved_event_target", identifier(&slot)),
					assign(&format!("event_target:{slot}"), block(inside)),
				]),
			),
			assign(
				&format!("event_target:{slot}"),
				block(vec![assign(
					"save_event_target_as",
					identifier(&n.name("target")),
				)]),
			),
		];
		if grouped {
			option.push(country_event(&n.event(ACTION_EVENT)));
		} else {
			if let Some(tooltip) = &members[0].tooltip {
				let shown = catalog
					.rewrite_text(Some(&slot), &[], texts)
					.localised(tooltip);
				option.push(assign("custom_tooltip", identifier(&shown)));
			}
			option.extend(run(0));
		}
		list_options.push(assign("option", block(option)));
	}
	list_options.push(assign(
		"option",
		block(vec![
			assign("name", identifier(&n.name("more"))),
			assign("trigger", block(vec![call(&n.name("unlisted"))])),
			call(&n.name("fill")),
			country_event(&n.event(LIST_EVENT)),
		]),
	));
	list_options.push(cancel(&release));
	events.push(event(
		&n,
		LIST_EVENT,
		"list_desc",
		Some(&n.slot(1)),
		list_options,
	));

	let mut text = vec![
		(
			n.name("title"),
			format!(
				"{}::{}",
				literal_text(source.name),
				entry_label(panel, members)
			),
		),
		(
			n.name("desc"),
			"Choose a province for this action.".to_owned(),
		),
		(
			n.name("list_desc"),
			"Choose a province. Only provinces where the action can be used are shown.".to_owned(),
		),
		(n.name("more"), "Show other provinces".to_owned()),
		(n.name("all"), "List all remaining provinces".to_owned()),
		(
			n.name("narrow_desc"),
			"Too many provinces qualify. Choose where to look.".to_owned(),
		),
	];
	for index in 1..=LISTED_PROVINCES {
		text.push((
			n.name(&format!("slot_{index}_name")),
			format!("[{}.GetName]", n.slot(index)),
		));
	}
	let mut required = vec!["CANCEL".to_owned()];

	if grouped {
		let mut options = Vec::new();
		for (index, feature) in members.iter().enumerate() {
			let key = n.name(&format!("option_{}", index + 1));
			let target = n.name("target");
			let mut option = vec![
				assign("name", identifier(&key)),
				assign("trigger", block(vec![allowed(index, &target)])),
			];
			if let Some(tooltip) = &feature.tooltip {
				let shown = catalog
					.rewrite_text(Some(&target), &[], texts)
					.localised(tooltip);
				option.push(assign("custom_tooltip", identifier(&shown)));
			}
			option.extend(run(index));
			options.push(assign("option", block(option)));
			text.push((
				key,
				literal_text(button_label(&feature.widget.leaf().name, panel)),
			));
		}
		options.push(cancel(&release));
		events.push(event(
			&n,
			ACTION_EVENT,
			"desc",
			Some(&n.name("target")),
			options,
		));
	}

	if !geography.is_empty() {
		// One option per map group that still holds a candidate; the
		// dispatched `has`/`narrow` trigger and effect take the group by name.
		let narrow = |test: &str, name: &str, level: usize| {
			assign(
				"option",
				block(vec![
					assign("name", identifier(name)),
					assign("trigger", block(vec![has_group(&n, test, name)])),
					with(
						&n.name("narrow"),
						&[
							("test", test),
							("name", name),
							("level", &level.to_string()),
						],
					),
				]),
			)
		};
		effects.push(numbered_effect(
			&n,
			"narrow",
			vec![
				assign(
					"every_province",
					block(vec![
						assign(
							"limit",
							block(vec![
								assign("has_province_flag", identifier(&n.flag("cand"))),
								assign("NOT", block(vec![assign("$test$", identifier("$name$"))])),
							]),
						),
						assign("clr_province_flag", identifier(&n.flag("cand"))),
					]),
				),
				assign("set_country_flag", identifier(&n.name("level_$level$"))),
				with(&n.numbered("route"), &[("slot", "$slot$")]),
			],
		));
		effects.push(dispatch(&n, "narrow", &["test", "name", "level"], false));
		triggers.push(numbered_trigger(
			&n,
			"has",
			vec![assign(
				"any_province",
				block(vec![
					assign("has_province_flag", identifier(&n.flag("cand"))),
					assign("$test$", identifier("$name$")),
				]),
			)],
		));
		triggers.push(dispatch(&n, "has", &["test", "name"], true));
		let mut options = geography
			.superregions
			.iter()
			.map(|(name, _)| narrow("superregion", name, 1))
			.collect::<Vec<_>>();
		options.extend([list_all(&n), cancel(&release)]);
		events.push(event(&n, SUPERREGION_EVENT, "narrow_desc", None, options));
		for (index, (_, regions)) in geography.superregions.iter().enumerate() {
			let mut options = regions
				.iter()
				.map(|name| narrow("region", name, 2))
				.collect::<Vec<_>>();
			options.extend([list_all(&n), cancel(&release)]);
			events.push(event(
				&n,
				REGION_EVENTS + index,
				"narrow_desc",
				None,
				options,
			));
		}
		for (index, (_, areas)) in geography.regions.iter().enumerate() {
			let mut options = areas
				.iter()
				.map(|name| narrow("area", name, 3))
				.collect::<Vec<_>>();
			options.extend([list_all(&n), cancel(&release)]);
			events.push(event(&n, AREA_EVENTS + index, "narrow_desc", None, options));
		}
		for (name, members) in geography.superregions.iter().chain(&geography.regions) {
			required.push(name.clone());
			required.extend(members.iter().cloned());
		}
		required.sort();
		required.dedup();
	}

	Ok(Entry {
		actions: members
			.iter()
			.map(|feature| feature.widget.leaf().name.clone())
			.collect(),
		label: entry_label(panel, members),
		visible: visible_anywhere,
		available: vec![call(&n.name("any"))],
		prepare: vec![call(&n.name("mark"))],
		start: vec![call(&n.name("route"))],
		close: vec![call(&n.name("close"))],
		needs_player_number: true,
		scripts: vec![
			(format!("events/{}.txt", n.id), emit(&events)?),
			(
				format!("common/scripted_effects/{}.txt", n.id),
				emit(&effects)?,
			),
			(
				format!("common/scripted_triggers/{}.txt", n.id),
				emit(&triggers)?,
			),
		],
		text,
		required,
	})
}

/// A scripted effect over the province flags of player number `$slot$`.
fn numbered_effect(n: &Names, operation: &str, body: Vec<AstStatement>) -> AstStatement {
	assign(&n.numbered(operation), block(body))
}

fn numbered_trigger(n: &Names, operation: &str, body: Vec<AstStatement>) -> AstStatement {
	assign(&n.numbered(operation), block(body))
}

/// `<operation>` for the deciding player: pass the player's number and the
/// other `parameters` through to `<operation>_s`. A player without a number
/// has no flags; the effect does nothing and the trigger is false.
fn dispatch(n: &Names, operation: &str, parameters: &[&str], trigger: bool) -> AstStatement {
	let call_with = |slot: usize| {
		let slot = slot.to_string();
		let mut arguments = vec![("slot", slot.as_str())];
		let passed = parameters
			.iter()
			.map(|name| (*name, format!("${name}$")))
			.collect::<Vec<_>>();
		arguments.extend(passed.iter().map(|(name, value)| (*name, value.as_str())));
		with(&n.numbered(operation), &arguments)
	};
	let player = |slot: usize| {
		assign(
			"ROOT",
			block(vec![assign(
				"has_country_flag",
				identifier(&format!("{PLAYER_FLAG}_{slot}")),
			)]),
		)
	};
	let body = if trigger {
		vec![assign(
			"OR",
			block(
				(1..=PLAYER_SLOTS)
					.map(|slot| assign("AND", block(vec![player(slot), call_with(slot)])))
					.collect(),
			),
		)]
	} else {
		(1..=PLAYER_SLOTS)
			.map(|slot| {
				assign(
					if slot == 1 { "if" } else { "else_if" },
					block(vec![
						assign("limit", block(vec![player(slot)])),
						call_with(slot),
					]),
				)
			})
			.collect()
	};
	assign(&n.name(operation), block(body))
}

/// Send the search to the list once at most [`LISTED_PROVINCES`] remain,
/// otherwise to the next geographic level that still separates them.
fn route(n: &Names, geography: &Geography) -> Vec<AstStatement> {
	let list = || {
		vec![
			with(&n.numbered("fill"), &[("slot", "$slot$")]),
			country_event(&n.event(LIST_EVENT)),
		]
	};
	let few = assign(
		"NOT",
		block(vec![assign(
			"calc_true_if",
			block(vec![
				assign(
					"all_province",
					block(vec![assign(
						"has_province_flag",
						identifier(&n.flag("cand")),
					)]),
				),
				assign("amount", number(&(LISTED_PROVINCES + 1).to_string())),
			]),
		)]),
	);
	let mut chain = vec![assign(
		"if",
		block([vec![assign("limit", block(vec![few]))], list()].concat()),
	)];
	if geography.is_empty() {
		chain.push(assign("else", block(list())));
		return chain;
	}
	let level = |level: usize| {
		assign(
			"has_country_flag",
			identifier(&n.name(&format!("level_{level}"))),
		)
	};
	let has = |test: &str, name: &str| {
		with(
			&n.numbered("has"),
			&[("slot", "$slot$"), ("test", test), ("name", name)],
		)
	};
	// Within the chosen group, open the next level's event for that group.
	let branch = |test: &str, groups: &[(String, Vec<String>)], first_event: usize| {
		let mut branches = groups
			.iter()
			.enumerate()
			.map(|(index, (name, _))| {
				assign(
					if index == 0 { "if" } else { "else_if" },
					block(vec![
						assign("limit", block(vec![has(test, name)])),
						country_event(&n.event(first_event + index)),
					]),
				)
			})
			.collect::<Vec<_>>();
		branches.push(assign(
			"else",
			block(
				[
					vec![assign("set_country_flag", identifier(&n.name("level_3")))],
					list(),
				]
				.concat(),
			),
		));
		branches
	};
	chain.push(assign(
		"else_if",
		block([vec![assign("limit", block(vec![level(3)]))], list()].concat()),
	));
	chain.push(assign(
		"else_if",
		block(
			[
				vec![assign("limit", block(vec![level(2)]))],
				branch("region", &geography.regions, AREA_EVENTS),
			]
			.concat(),
		),
	));
	chain.push(assign(
		"else_if",
		block(
			[
				vec![assign("limit", block(vec![level(1)]))],
				branch("superregion", &geography.superregions, REGION_EVENTS),
			]
			.concat(),
		),
	));
	chain.push(assign(
		"else",
		block(vec![country_event(&n.event(SUPERREGION_EVENT))]),
	));
	chain
}

/// Save up to [`LISTED_PROVINCES`] unlisted candidates into the name slots.
fn fill(n: &Names) -> Vec<AstStatement> {
	let positions = (1..=LISTED_PROVINCES)
		.map(|index| n.flag(&format!("slot_{index}")))
		.collect::<Vec<_>>();
	let mut effect = vec![clear(&positions)];
	for (index, position) in positions.iter().enumerate() {
		effect.push(assign(
			"random_province",
			block(vec![
				assign(
					"limit",
					block(vec![
						assign("has_province_flag", identifier(&n.flag("cand"))),
						assign(
							"NOT",
							block(vec![assign(
								"has_province_flag",
								identifier(&n.flag("listed")),
							)]),
						),
					]),
				),
				assign("save_event_target_as", identifier(&n.slot(index + 1))),
				assign("set_province_flag", identifier(position)),
				assign("set_province_flag", identifier(&n.flag("listed"))),
			]),
		));
	}
	effect
}

/// Clear the player's province flags of the search; part of the release.
fn close(n: &Names) -> Vec<AstStatement> {
	let flags = [n.flag("cand"), n.flag("listed")]
		.into_iter()
		.chain((1..=LISTED_PROVINCES).map(|index| n.flag(&format!("slot_{index}"))))
		.collect::<Vec<_>>();
	let mut effect = vec![clear(&flags)];
	for flag in ["level_1", "level_2", "level_3"] {
		effect.push(assign("clr_country_flag", identifier(&n.name(flag))));
	}
	effect
}

/// Remove `flags` from every province that has one of them.
fn clear(flags: &[String]) -> AstStatement {
	assign(
		"every_province",
		block(
			[
				vec![assign(
					"limit",
					block(vec![assign(
						"OR",
						block(
							flags
								.iter()
								.map(|flag| assign("has_province_flag", identifier(flag)))
								.collect(),
						),
					)]),
				)],
				flags
					.iter()
					.map(|flag| assign("clr_province_flag", identifier(flag)))
					.collect(),
			]
			.concat(),
		),
	)
}

fn event(
	n: &Names,
	number: usize,
	desc: &str,
	goto: Option<&str>,
	options: Vec<AstStatement>,
) -> AstStatement {
	let mut body = vec![
		assign("id", identifier(&n.event(number))),
		assign("title", identifier(&n.name("title"))),
		assign("desc", identifier(&n.name(desc))),
		assign("picture", identifier("DIPLOMACY_eventPicture")),
		assign("is_triggered_only", identifier("yes")),
	];
	if let Some(goto) = goto {
		body.push(assign("goto", identifier(goto)));
	}
	body.extend(options);
	assign("country_event", block(body))
}

fn list_all(n: &Names) -> AstStatement {
	assign(
		"option",
		block(vec![
			assign("name", identifier(&n.name("all"))),
			assign("set_country_flag", identifier(&n.name("level_3"))),
			call(&n.name("route")),
		]),
	)
}

fn cancel(release: &str) -> AstStatement {
	assign(
		"option",
		block(vec![assign("name", identifier("CANCEL")), call(release)]),
	)
}

/// Whether a candidate remains in the map group `name` tested by `test`.
fn has_group(n: &Names, test: &str, name: &str) -> AstStatement {
	with(&n.name("has"), &[("test", test), ("name", name)])
}

/// `name = { key = value ... }`: a generated trigger or effect with arguments.
fn with(name: &str, arguments: &[(&str, &str)]) -> AstStatement {
	assign(
		name,
		block(
			arguments
				.iter()
				.map(|(key, value)| assign(key, identifier(value)))
				.collect(),
		),
	)
}

fn call(effect: &str) -> AstStatement {
	assign(effect, identifier("yes"))
}

fn country_event(id: &str) -> AstStatement {
	assign("country_event", block(vec![assign("id", identifier(id))]))
}
