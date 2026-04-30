use super::{
	ScriptFileKind, build_inferred_callable_scope_map, build_semantic_index, classify_script_file,
	collect_inferred_callable_masks, effective_alias_scope_mask_with_overrides, parse_script_file,
	scope_kind,
};
use crate::analyzer::analysis::{AnalyzeOptions, analyze_visibility};
use crate::analyzer::content_family::{GameProfile, MergeKeySource};
use crate::analyzer::eu4_profile::eu4_profile;
use foch_core::model::{AnalysisMode, ScopeKind, ScopeType, SymbolKind};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn full_army_tradition_switch_effect(name: &str) -> String {
	let branches: String = (0..=100)
		.rev()
		.map(|tradition| {
			format!(
				"\t\t\t{tradition} = {{\n\t\t\t\tPREV = {{\n\t\t\t\t\tcreate_general = {{\n\t\t\t\t\t\tculture = PREV\n\t\t\t\t\t\ttradition = {tradition}\n\t\t\t\t\t}}\n\t\t\t\t}}\n\t\t\t}}\n"
			)
		})
		.collect();
	format!(
		"{name} {{\n\t$who$ = {{\n\t\ttrigger_switch = {{\n\t\t\ton_trigger = army_tradition\n{branches}\t\t}}\n\t}}\n}}\n"
	)
}

fn fourth_wave_contract_definitions() -> &'static str {
	r#"
unlock_estate_privilege = {
custom_tooltip = unlock_privilege_$estate_privilege$_tt
hidden_effect = {
	set_country_flag = unlocked_privilege_$estate_privilege$
}
[[modifier_tooltip]
	custom_tooltip = unlock_estate_privilege_modifier_tooltip_tt
	tooltip = {
		add_country_modifier = {
			name = $modifier_tooltip$
			duration = -1
			desc = UNTIL_PRIVILEGE_REVOKED
		}
	}
]
[[effect_tooltip]
	custom_tooltip = unlock_estate_privilege_effect_tooltip_tt
	tooltip = {
		$effect_tooltip$
	}
]
}

HAB_change_habsburg_glory = {
[[remove]
	add_government_power = {
		value = -$remove$
	}
]
[[amount]
	add_government_power = {
		value = $amount$
	}
]
}

add_legitimacy_or_reform_progress = {
[[amount]
	tooltip = {
		add_legitimacy_equivalent = { amount = $amount$ }
	}
]
[[value]
	tooltip = {
		add_legitimacy_equivalent = { amount = $value$ }
	}
]
}

EE_change_variable = {
[[add]
	change_variable = {
		which = $which$
		value = $add$
	}
]
[[subtract]
	subtract_variable = {
		which = $which$
		value = $subtract$
	}
]
[[divide]
	divide_variable = {
		which = $which$
		value = $divide$
	}
]
[[multiply]
	multiply_variable = {
		which = $which$
		value = $multiply$
	}
]
}

build_as_many_as_possible = {
[[upgrade_target]$pick_best_function$ = {
	scope = every_owned_province
	trigger = "$all_prior_trig$
		can_build = $new_building$"
}]
[[construct_new]$pick_best_function$ = {
	scope = every_owned_province
	trigger = "can_build = $new_building$"
}]
event_target:highest_score_trade = {
	add_building_construction = {
		building = $new_building$
		speed = $speed$
		cost = $cost$
	}
}
}

give_claims = {
[[province] custom_tooltip = $province$ ]
[[id] custom_tooltip = $id$ ]
[[area] custom_tooltip = $area$ ]
[[region] custom_tooltip = $region$ ]
}

pick_best_tags = {
[[scope] custom_tooltip = $scope$ ]
custom_tooltip = $scale$
custom_tooltip = $event_target_name$
custom_tooltip = "$global_trigger$"
[[1] custom_tooltip = "$1$" ]
[[2] custom_tooltip = "$2$" ]
[[3] custom_tooltip = "$3$" ]
[[4] custom_tooltip = "$4$" ]
[[5] custom_tooltip = "$5$" ]
[[10] custom_tooltip = "$10$" ]
}

ME_add_years_of_trade_income = {
[[years] add_years_of_trade_income = { years = $years$ } ]
[[value] add_years_of_trade_income = { years = $value$ } ]
[[amount] add_years_of_trade_income = { years = $amount$ } ]
}

ME_tim_add_spoils_of_war = {
[[add]
	add_government_power = {
		value = $add$
	}
]
[[remove]
	add_government_power = {
		value = -$remove$
	}
]
}

ME_add_power_projection = {
[[amount]
	add_power_projection = {
		amount = $amount$
	}
]
[[value]
	add_power_projection = {
		amount = $value$
	}
]
}

create_general_scaling_with_tradition_and_pips = {
create_general_with_pips = {
	tradition = 100
	[[add_fire] add_fire = $add_fire$ ]
	[[add_shock] add_shock = $add_shock$ ]
	[[add_manuever] add_manuever = $add_manuever$ ]
	[[add_siege] add_siege = $add_siege$ ]
}
}

ME_automatic_colonization_effect_module = {
any_province = {
	OR = {
		[[superregion]
			superregion = $superregion$
		]
		[[region]
			colonial_region = $region$
		]
	}
}
$target_region_effect$ = yes
}

country_event_with_insight = {
country_event = {
	id = $id$
	[[days] days = $days$]
	[[random] random = $random$]
	[[tooltip] tooltip = $tooltip$]
}
custom_tooltip = EVENT_INSIGHT_INTRO
custom_tooltip = $insight_tooltip$
[[effect_tooltip] tooltip = { $effect_tooltip$ }]
}

define_and_hire_grand_vizier = {
hire_advisor = {
	type = $type$
	[[skill] skill = $skill$]
	[[culture] culture = $culture$]
	[[religion] religion = $religion$]
	[[female] female = $female$]
	[[age] age = $age$]
	[[max_age] max_age = $max_age$]
	[[min_age] min_age = $min_age$]
	[[location] location = $location$]
}
add_country_modifier = {
	name = grand_vizier_$type$
	duration = -1
	desc = UNTIL_ADVISOR_REMOVAL
}
}

ME_override_country_name = {
[[country_name] override_country_name = $country_name$ ]
[[name] override_country_name = $name$ ]
[[country] override_country_name = $country$ ]
[[value] override_country_name = $value$ ]
[[string] override_country_name = $string$ ]
hidden_effect = {
	set_country_flag = ME_overrid_country_name
}
}

persia_indian_hegemony_decision_march_effect = {
$tag_1$ = {
	custom_tooltip = persia_indian_hegemony_decision_march_$province$_tt_release_march
}
[[tag_2] $tag_2$ = { }]
[[tag_3] $tag_3$ = { }]
[[tag_4] $tag_4$ = { }]
[[tag_5] $tag_5$ = { }]
$trade_company_region$ = {
	add_permanent_claim = event_target:persia_march_target
}
$province$ = {
	owner = {
		save_event_target_as = persia_march_target
	}
}
}

persia_indian_hegemony_decision_coup_effect = {
$province$ = {
	owner = {
		save_event_target_as = persia_coup_target
	}
}
$tag_1$ = {
	custom_tooltip = persia_indian_hegemony_decision_coup_$province$_tt_independence
}
[[tag_2] $tag_2$ = { }]
[[tag_3] $tag_3$ = { }]
[[tag_4] $tag_4$ = { }]
[[tag_5] $tag_5$ = { }]
}
"#
}

fn fourth_wave_s004_messages(call_rel_path: &[&str], call_source: &str) -> Vec<String> {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	let scripted_effects_dir = mod_root.join("common").join("scripted_effects");
	fs::create_dir_all(&scripted_effects_dir).expect("create scripted effects");
	fs::write(
		scripted_effects_dir.join("fourth_wave_contracts.txt"),
		fourth_wave_contract_definitions(),
	)
	.expect("write scripted effects");

	let mut call_path = mod_root.clone();
	for component in call_rel_path {
		call_path = call_path.join(component);
	}
	let call_parent = call_path.parent().expect("call parent");
	fs::create_dir_all(call_parent).expect("create call dir");
	fs::write(&call_path, call_source).expect("write call source");

	let parsed = [
		parse_script_file(
			"1013",
			&mod_root,
			&scripted_effects_dir.join("fourth_wave_contracts.txt"),
		)
		.expect("parsed scripted effects"),
		parse_script_file("1013", &mod_root, &call_path).expect("parsed call source"),
	];
	let index = build_semantic_index(&parsed);
	analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	)
	.strict
	.into_iter()
	.filter(|finding| finding.rule_id == "S004")
	.map(|finding| finding.message)
	.collect()
}

#[test]
fn interface_content_family_keys_gui_types_children_by_name() {
	let descriptor = eu4_profile()
		.classify_content_family(Path::new("interface/topbar.gui"))
		.expect("interface descriptor");
	match descriptor.merge_key_source.expect("merge key source") {
		MergeKeySource::ContainerChildFieldValue {
			container,
			child_key_field,
			child_types,
		} => {
			assert_eq!(container, "guiTypes");
			assert_eq!(child_key_field, "name");
			assert!(child_types.contains(&"windowType"));
			assert!(child_types.contains(&"containerWindowType"));
			assert!(child_types.contains(&"instantTextBoxType"));
			assert!(child_types.contains(&"guiButtonType"));
		}
		other => panic!("unexpected interface merge key source: {other:?}"),
	}
}

#[test]
fn classify_paths() {
	assert_eq!(
		classify_script_file(std::path::Path::new("common/on_actions/00_on_actions.txt")),
		ScriptFileKind::OnActions
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("events/common/on_actions/foo.txt")),
		ScriptFileKind::OnActions
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/scripted_effects/a.txt")),
		ScriptFileKind::ScriptedEffects
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("events/a.txt")),
		ScriptFileKind::Events
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("interface/a.gui")),
		ScriptFileKind::Ui
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/achievements.txt")),
		ScriptFileKind::Achievements
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/ages/00_default.txt")),
		ScriptFileKind::Ages
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/buildings/00_buildings.txt")),
		ScriptFileKind::Buildings
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/ideas/00_country_ideas.txt")),
		ScriptFileKind::Ideas
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/great_projects/01_monuments.txt"
		)),
		ScriptFileKind::GreatProjects
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/government_reforms/01_government_reforms.txt"
		)),
		ScriptFileKind::GovernmentReforms
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/cultures/00_cultures.txt")),
		ScriptFileKind::Cultures
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/custom_gui/AdvisorActionsGui.txt"
		)),
		ScriptFileKind::CustomGui
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/advisortypes/00_advisortypes.txt"
		)),
		ScriptFileKind::AdvisorTypes
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/event_modifiers/00_modifiers.txt"
		)),
		ScriptFileKind::EventModifiers
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/cb_types/00_cb_types.txt")),
		ScriptFileKind::CbTypes
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/government_names/00_names.txt")),
		ScriptFileKind::GovernmentNames
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"customizable_localization/00_customizable_localization.txt"
		)),
		ScriptFileKind::CustomizableLocalization
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/new_diplomatic_actions/00_actions.txt"
		)),
		ScriptFileKind::NewDiplomaticActions
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"events/common/new_diplomatic_actions/00_actions.txt"
		)),
		ScriptFileKind::NewDiplomaticActions
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("missions/example.txt")),
		ScriptFileKind::Missions
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("events/decisions/example.txt")),
		ScriptFileKind::Decisions
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/institutions/00.txt")),
		ScriptFileKind::Institutions
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/province_triggered_modifiers/00.txt"
		)),
		ScriptFileKind::ProvinceTriggeredModifiers
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/scripted_triggers/00_triggers.txt"
		)),
		ScriptFileKind::ScriptedTriggers
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/country_tags/00_countries.txt")),
		ScriptFileKind::CountryTags
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/countries/00_countries.txt")),
		ScriptFileKind::Countries
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("history/countries/FRA - France.txt")),
		ScriptFileKind::CountryHistory
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("history/provinces/1 - Stockholm.txt")),
		ScriptFileKind::ProvinceHistory
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/province_names/sorbian.txt")),
		ScriptFileKind::ProvinceNames
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("map/random/tiles/tile0.txt")),
		ScriptFileKind::RandomMapTiles
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("map/random/RandomLandNames.txt")),
		ScriptFileKind::RandomMapNames
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("map/random/RNWScenarios.txt")),
		ScriptFileKind::RandomMapScenarios
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("map/random/tweaks.lua")),
		ScriptFileKind::Other
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("history/diplomacy/hre.txt")),
		ScriptFileKind::DiplomacyHistory
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("history/advisors/00_england.txt")),
		ScriptFileKind::AdvisorHistory
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("history/wars/100yearswar.txt")),
		ScriptFileKind::Wars
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/units/00_units.txt")),
		ScriptFileKind::Units
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/religions/00_religion.txt")),
		ScriptFileKind::Religions
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/subject_types/00_subject_types.txt"
		)),
		ScriptFileKind::SubjectTypes
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/rebel_types/independence_rebels.txt"
		)),
		ScriptFileKind::RebelTypes
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/disasters/civil_war.txt")),
		ScriptFileKind::Disasters
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/government_mechanics/18_parliament_vs_monarchy.txt"
		)),
		ScriptFileKind::GovernmentMechanics
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/church_aspects/00_church_aspects.txt"
		)),
		ScriptFileKind::ChurchAspects
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/factions/00_factions.txt")),
		ScriptFileKind::Factions
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/hegemons/0_economic_hegemon.txt"
		)),
		ScriptFileKind::Hegemons
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/personal_deities/00_hindu_deities.txt"
		)),
		ScriptFileKind::PersonalDeities
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/fetishist_cults/00_fetishist_cults.txt"
		)),
		ScriptFileKind::FetishistCults
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/estate_agendas/00_generic_agendas.txt"
		)),
		ScriptFileKind::EstateAgendas
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/estate_privileges/01_church_privileges.txt"
		)),
		ScriptFileKind::EstatePrivileges
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/estates/01_church.txt")),
		ScriptFileKind::Estates
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/parliament_bribes/administrative_support.txt"
		)),
		ScriptFileKind::ParliamentBribes
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/parliament_issues/00_adm_parliament_issues.txt"
		)),
		ScriptFileKind::ParliamentIssues
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/state_edicts/edict_of_governance.txt"
		)),
		ScriptFileKind::StateEdicts
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/peace_treaties/00_peace_treaties.txt"
		)),
		ScriptFileKind::PeaceTreaties
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/bookmarks/a_new_world.txt")),
		ScriptFileKind::Bookmarks
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/policies/00_adm.txt")),
		ScriptFileKind::Policies
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/mercenary_companies/00_mercenaries.txt"
		)),
		ScriptFileKind::MercenaryCompanies
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/fervor/00_fervor.txt")),
		ScriptFileKind::Fervor
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/decrees/00_china.txt")),
		ScriptFileKind::Decrees
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/federation_advancements/00_default.txt"
		)),
		ScriptFileKind::FederationAdvancements
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/golden_bulls/00_golden_bulls.txt"
		)),
		ScriptFileKind::GoldenBulls
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/flagship_modifications/00_flagship_modifications.txt"
		)),
		ScriptFileKind::FlagshipModifications
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/powerprojection/00_static.txt")),
		ScriptFileKind::PowerProjection
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/subject_type_upgrades/00_subject_type_upgrades.txt"
		)),
		ScriptFileKind::SubjectTypeUpgrades
	);
	assert_eq!(
		classify_script_file(std::path::Path::new(
			"common/government_ranks/00_government_ranks.txt"
		)),
		ScriptFileKind::GovernmentRanks
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/technologies/adm.txt")),
		ScriptFileKind::Technologies
	);
	assert_eq!(
		classify_script_file(std::path::Path::new("common/technology.txt")),
		ScriptFileKind::TechnologyGroups
	);
}

#[test]
fn foundation_roots_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("country_tags")).expect("create country tags");
	fs::create_dir_all(mod_root.join("common").join("countries")).expect("create countries");
	fs::create_dir_all(mod_root.join("common").join("units")).expect("create units");
	fs::create_dir_all(mod_root.join("history").join("countries")).expect("create country history");
	fs::create_dir_all(mod_root.join("history").join("provinces"))
		.expect("create province history");
	fs::create_dir_all(mod_root.join("history").join("wars")).expect("create wars");
	fs::write(
		mod_root
			.join("common")
			.join("country_tags")
			.join("00_countries.txt"),
		"SWE = \"countries/Sweden.txt\"\n",
	)
	.expect("write country tags");
	fs::write(
		mod_root.join("common").join("countries").join("Sweden.txt"),
		r#"
graphical_culture = scandinaviangfx
preferred_religion = protestant
historical_idea_groups = {
quality_ideas
offensive_ideas
}
historical_units = {
western_medieval_infantry
}
"#,
	)
	.expect("write countries");
	fs::write(
		mod_root
			.join("common")
			.join("units")
			.join("swedish_tercio.txt"),
		"type = infantry
unit_type = western
offensive_fire = 2
defensive_shock = 1
",
	)
	.expect("write units");
	fs::write(
		mod_root
			.join("history")
			.join("countries")
			.join("SWE - Sweden.txt"),
		r#"
capital = 1
1448.6.20 = {
queen = {
	country_of_origin = SWE
}
}
"#,
	)
	.expect("write country history");
	fs::write(
		mod_root
			.join("history")
			.join("provinces")
			.join("1-Uppland.txt"),
		"add_core = SWE\nowner = SWE\ncontroller = SWE\n",
	)
	.expect("write province history");
	fs::write(
		mod_root
			.join("history")
			.join("wars")
			.join("afghan_maratha.txt"),
		r#"
1758.1.1 = {
add_attacker = AFG
add_defender = MAR
}
1761.1.14 = {
battle = {
	location = 521
	attacker = { country = AFG }
	defender = { country = MAR }
}
}
"#,
	)
	.expect("write war history");

	let files = vec![
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("country_tags")
				.join("00_countries.txt"),
		)
		.expect("parsed country tags"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root.join("common").join("countries").join("Sweden.txt"),
		)
		.expect("parsed countries"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("units")
				.join("swedish_tercio.txt"),
		)
		.expect("parsed units"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("history")
				.join("countries")
				.join("SWE - Sweden.txt"),
		)
		.expect("parsed country history"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("history")
				.join("provinces")
				.join("1-Uppland.txt"),
		)
		.expect("parsed province history"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("history")
				.join("wars")
				.join("afghan_maratha.txt"),
		)
		.expect("parsed war history"),
	];

	let index = build_semantic_index(&files);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/country_tags/00_countries.txt")
			&& reference.key == "country_tag:SWE"
			&& reference.value == "countries/Sweden.txt"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/countries/Sweden.txt")
			&& reference.key == "graphical_culture"
			&& reference.value == "scandinaviangfx"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/countries/Sweden.txt")
			&& reference.key == "preferred_religion"
			&& reference.value == "protestant"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/countries/Sweden.txt")
			&& reference.key == "historical_idea_groups"
			&& reference.value == "quality_ideas"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/countries/Sweden.txt")
			&& reference.key == "historical_units"
			&& reference.value == "western_medieval_infantry"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/countries/SWE - Sweden.txt")
			&& reference.key == "capital"
			&& reference.value == "1"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/countries/SWE - Sweden.txt")
			&& reference.key == "country_of_origin"
			&& reference.value == "SWE"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/provinces/1-Uppland.txt")
			&& reference.key == "owner"
			&& reference.value == "SWE"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/wars/afghan_maratha.txt")
			&& reference.key == "add_attacker"
			&& reference.value == "AFG"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/wars/afghan_maratha.txt")
			&& reference.key == "location"
			&& reference.value == "521"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/units/swedish_tercio.txt")
			&& reference.key == "type"
			&& reference.value == "infantry"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/units/swedish_tercio.txt")
			&& reference.key == "unit_type"
			&& reference.value == "western"
	}));
}

#[test]
fn common_data_roots_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("religions")).expect("create religions");
	fs::create_dir_all(mod_root.join("common").join("subject_types"))
		.expect("create subject types");
	fs::create_dir_all(mod_root.join("common").join("rebel_types")).expect("create rebel types");
	fs::create_dir_all(mod_root.join("common").join("disasters")).expect("create disasters");
	fs::create_dir_all(mod_root.join("common").join("government_mechanics"))
		.expect("create government mechanics");
	fs::write(
		mod_root
			.join("common")
			.join("religions")
			.join("00_religion.txt"),
		r#"
christian = {
center_of_religion = 118
catholic = {
	allowed_conversion = {
		protestant
	}
	heretic = { hussite }
	papacy = {
		papal_tag = PAP
	}
}
"#,
	)
	.expect("write religions");
	fs::write(
		mod_root
			.join("common")
			.join("subject_types")
			.join("00_subject_types.txt"),
		r#"
default = {
sprite = GFX_icon_vassal
diplomacy_overlord_sprite = GFX_diplomacy_leadvassal
}
march = {
copy_from = default
subject_opinion_modifier = march_subject
}
"#,
	)
	.expect("write subject types");
	fs::write(
		mod_root
			.join("common")
			.join("rebel_types")
			.join("independence_rebels.txt"),
		r#"
independence_rebels = {
gfx_type = culture_province
demands_description = "independence_rebels_demands"
}
"#,
	)
	.expect("write rebel types");
	fs::write(
		mod_root
			.join("common")
			.join("disasters")
			.join("civil_war.txt"),
		r#"
civil_war = {
on_start = civil_war.1
on_end = civil_war.100
on_monthly = {
	events = {
		civil_war.2
	}
	random_events = {
		100 = civil_war.3
	}
}
can_start = {
	NOT = { has_disaster = court_and_country }
}
}
"#,
	)
	.expect("write disasters");
	fs::write(
		mod_root
			.join("common")
			.join("government_mechanics")
			.join("18_parliament_vs_monarchy.txt"),
		r#"
parliament_vs_monarchy_mechanic = {
available = {
	has_dlc = "Domination"
}
powers = {
	governmental_power = {
		gui = parliament_vs_monarchy_gov_mech
		scaled_modifier = {
			trigger = {
				has_government_power = {
					mechanic_type = parliament_vs_monarchy_mechanic
					power_type = governmental_power
				}
			}
		}
		on_max_reached = {
			custom_tooltip = parliament_vs_monarchy_mechanic_at
			hidden_effect = {
				country_event = {
					id = flavor_gbr.113
				}
			}
		}
	}
}
}
"#,
	)
	.expect("write government mechanics");

	let files = vec![
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("religions")
				.join("00_religion.txt"),
		)
		.expect("parsed religions"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("subject_types")
				.join("00_subject_types.txt"),
		)
		.expect("parsed subject types"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("rebel_types")
				.join("independence_rebels.txt"),
		)
		.expect("parsed rebel types"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("disasters")
				.join("civil_war.txt"),
		)
		.expect("parsed disasters"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("government_mechanics")
				.join("18_parliament_vs_monarchy.txt"),
		)
		.expect("parsed government mechanics"),
	];

	let index = build_semantic_index(&files);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/religions/00_religion.txt")
			&& reference.key == "center_of_religion"
			&& reference.value == "118"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/religions/00_religion.txt")
			&& reference.key == "allowed_conversion"
			&& reference.value == "protestant"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/religions/00_religion.txt")
			&& reference.key == "papal_tag"
			&& reference.value == "PAP"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/subject_types/00_subject_types.txt")
			&& reference.key == "copy_from"
			&& reference.value == "default"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/subject_types/00_subject_types.txt")
			&& reference.key == "sprite"
			&& reference.value == "GFX_icon_vassal"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/rebel_types/independence_rebels.txt")
			&& reference.key == "demands_description"
			&& reference.value == "independence_rebels_demands"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/disasters/civil_war.txt")
			&& reference.key == "on_start"
			&& reference.value == "civil_war.1"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/disasters/civil_war.txt")
			&& reference.key == "event"
			&& reference.value == "civil_war.3"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/government_mechanics/18_parliament_vs_monarchy.txt")
			&& reference.key == "gui"
			&& reference.value == "parliament_vs_monarchy_gov_mech"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/government_mechanics/18_parliament_vs_monarchy.txt")
			&& reference.key == "country_event"
			&& reference.value == "flavor_gbr.113"
	}));
}

#[test]
fn governance_roots_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	for root in [
		"common/estate_agendas",
		"common/estate_privileges",
		"common/estates",
		"common/parliament_bribes",
		"common/parliament_issues",
		"common/state_edicts",
	] {
		fs::create_dir_all(mod_root.join(root)).expect("create governance root");
	}
	fs::write(
		mod_root
			.join("common")
			.join("estate_agendas")
			.join("00_generic_agendas.txt"),
		r#"
church_diplomatic_consultation_agenda = {
estate = clergy
can_select = {
	custom_tooltip = agenda_can_select_tt
}
task_requirements = {
	estate = clergy
}
pre_effect = {
	custom_tooltip = agenda_pre_tt
}
task_completed_effect = {
	custom_tooltip = agenda_done_tt
}
}
"#,
	)
	.expect("write estate agendas");
	fs::write(
		mod_root
			.join("common")
			.join("estate_privileges")
			.join("01_church_privileges.txt"),
		r#"
religious_diplomats = {
icon = privilege_religious_diplomats
estate = clergy
mechanics = {
	devotion
	papal_influence
}
benefits = {
	custom_tooltip = privilege_benefits_tt
}
}
"#,
	)
	.expect("write estate privileges");
	fs::write(
		mod_root
			.join("common")
			.join("estates")
			.join("01_church.txt"),
		r#"
clergy = {
custom_name = estate_clergy_custom_name
custom_desc = estate_clergy_custom_desc
privileges = {
	religious_diplomats
}
agendas = {
	church_diplomatic_consultation_agenda
}
starting_reform = monarchy_reform
independence_government = theocracy
trigger = {
	has_dlc = "Domination"
}
}
"#,
	)
	.expect("write estates");
	fs::write(
		mod_root
			.join("common")
			.join("parliament_bribes")
			.join("administrative_support.txt"),
		r#"
administrative_support = {
name = parliament_bribe_admin_support
estate = clergy
mechanic_type = parliament_vs_monarchy_mechanic
power_type = governmental_power
type = monarch_power
effect = {
	add_adm_power = 50
}
}
"#,
	)
	.expect("write parliament bribes");
	fs::write(
		mod_root
			.join("common")
			.join("parliament_issues")
			.join("00_adm_parliament_issues.txt"),
		r#"
expand_bureaucracy_issue = {
parliament_action = strengthen_government
issue = expand_bureaucracy_issue
custom_tooltip = parliament_issue_tt
effect = {
	custom_tooltip = parliament_issue_effect_tt
}
influence_scaled_modifier = {
	estate = clergy
}
}
"#,
	)
	.expect("write parliament issues");
	fs::write(
		mod_root
			.join("common")
			.join("state_edicts")
			.join("edict_of_governance.txt"),
		r#"
edict_of_governance = {
tooltip = edict_of_governance_tt
allow = {
	custom_trigger_tooltip = state_edict_allow_tt
	has_state_edict = encourage_development_edict
}
modifier = {
	state_maintenance_modifier = -0.1
}
}
"#,
	)
	.expect("write state edicts");

	let files = vec![
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("estate_agendas")
				.join("00_generic_agendas.txt"),
		)
		.expect("parsed estate agendas"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("estate_privileges")
				.join("01_church_privileges.txt"),
		)
		.expect("parsed estate privileges"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("estates")
				.join("01_church.txt"),
		)
		.expect("parsed estates"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("parliament_bribes")
				.join("administrative_support.txt"),
		)
		.expect("parsed parliament bribes"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("parliament_issues")
				.join("00_adm_parliament_issues.txt"),
		)
		.expect("parsed parliament issues"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("state_edicts")
				.join("edict_of_governance.txt"),
		)
		.expect("parsed state edicts"),
	];

	let index = build_semantic_index(&files);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/estate_agendas/00_generic_agendas.txt")
			&& reference.key == "estate"
			&& reference.value == "clergy"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/estate_agendas/00_generic_agendas.txt")
			&& reference.key == "custom_tooltip"
			&& reference.value == "agenda_done_tt"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/estate_privileges/01_church_privileges.txt")
			&& reference.key == "icon"
			&& reference.value == "privilege_religious_diplomats"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/estate_privileges/01_church_privileges.txt")
			&& reference.key == "mechanics"
			&& reference.value == "papal_influence"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/estates/01_church.txt")
			&& reference.key == "custom_name"
			&& reference.value == "estate_clergy_custom_name"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/estates/01_church.txt")
			&& reference.key == "privileges"
			&& reference.value == "religious_diplomats"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/parliament_bribes/administrative_support.txt")
			&& reference.key == "mechanic_type"
			&& reference.value == "parliament_vs_monarchy_mechanic"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/parliament_issues/00_adm_parliament_issues.txt")
			&& reference.key == "parliament_action"
			&& reference.value == "strengthen_government"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/parliament_issues/00_adm_parliament_issues.txt")
			&& reference.key == "estate"
			&& reference.value == "clergy"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/state_edicts/edict_of_governance.txt")
			&& reference.key == "tooltip"
			&& reference.value == "edict_of_governance_tt"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/state_edicts/edict_of_governance.txt")
			&& reference.key == "has_state_edict"
			&& reference.value == "encourage_development_edict"
	}));
}

#[test]
fn peace_treaties_and_bookmarks_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("peace_treaties"))
		.expect("create peace treaties");
	fs::create_dir_all(mod_root.join("common").join("bookmarks")).expect("create bookmarks");
	fs::write(
		mod_root
			.join("common")
			.join("peace_treaties")
			.join("00_peace_treaties.txt"),
		r#"
spread_dynasty = {
power_projection = vassalized_rival
is_visible = { religion_group = christian }
is_allowed = { religion = catholic }
warscore_cost = { no_provinces = 20.0 }
effect = { add_prestige = 5 }
ai_weight = {
	export_to_variable = {
		variable_name = ai_value
		value = 50
	}
}
}
"#,
	)
	.expect("write peace treaties");
	fs::write(
		mod_root
			.join("common")
			.join("bookmarks")
			.join("a_new_world.txt"),
		r#"
bookmark = {
name = "NEWWORLD_NAME"
desc = "NEWWORLD_DESC"
date = 1492.1.1
center = 2133
country = CAS
country = ENG
}
"#,
	)
	.expect("write bookmarks");

	let files = vec![
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("peace_treaties")
				.join("00_peace_treaties.txt"),
		)
		.expect("parsed peace treaties"),
		parse_script_file(
			"1000",
			&mod_root,
			&mod_root
				.join("common")
				.join("bookmarks")
				.join("a_new_world.txt"),
		)
		.expect("parsed bookmarks"),
	];

	let index = build_semantic_index(&files);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/peace_treaties/00_peace_treaties.txt")
			&& reference.key == "localisation_desc"
			&& reference.value == "spread_dynasty_desc"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/peace_treaties/00_peace_treaties.txt")
			&& reference.key == "localisation_cb_allowed"
			&& reference.value == "CB_ALLOWED_spread_dynasty"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/peace_treaties/00_peace_treaties.txt")
			&& reference.key == "localisation_peace"
			&& reference.value == "PEACE_spread_dynasty"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/peace_treaties/00_peace_treaties.txt")
			&& reference.key == "power_projection"
			&& reference.value == "vassalized_rival"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/bookmarks/a_new_world.txt")
			&& reference.key == "name"
			&& reference.value == "NEWWORLD_NAME"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/bookmarks/a_new_world.txt")
			&& reference.key == "desc"
			&& reference.value == "NEWWORLD_DESC"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/bookmarks/a_new_world.txt")
			&& reference.key == "center"
			&& reference.value == "2133"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/bookmarks/a_new_world.txt")
			&& reference.key == "country"
			&& reference.value == "CAS"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/bookmarks/a_new_world.txt")
			&& reference.key == "country"
			&& reference.value == "ENG"
	}));
}

#[test]
fn low_risk_definition_roots_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("church_aspects"))
		.expect("create church aspects");
	fs::create_dir_all(mod_root.join("common").join("factions")).expect("create factions");
	fs::create_dir_all(mod_root.join("common").join("hegemons")).expect("create hegemons");
	fs::create_dir_all(mod_root.join("common").join("personal_deities"))
		.expect("create personal deities");
	fs::create_dir_all(mod_root.join("common").join("fetishist_cults"))
		.expect("create fetishist cults");

	fs::write(
		mod_root
			.join("common")
			.join("church_aspects")
			.join("00_church_aspects.txt"),
		r#"
organised_through_bishops_aspect = {
cost = 100
potential = { religion = protestant }
trigger = { has_church_power = yes }
effect = { add_stability = 1 }
modifier = { development_cost = -0.05 }
ai_will_do = { factor = 1 }
}
"#,
	)
	.expect("write church aspects");
	fs::write(
		mod_root
			.join("common")
			.join("factions")
			.join("00_factions.txt"),
		r#"
rr_jacobins = {
allow = { has_dlc = "Rights of Man" }
monarch_power = ADM
always = yes
modifier = { global_unrest = -2 }
}
"#,
	)
	.expect("write factions");
	fs::write(
		mod_root
			.join("common")
			.join("hegemons")
			.join("0_economic_hegemon.txt"),
		r#"
economic_hegemon = {
allow = { is_great_power = yes }
base = { war_exhaustion = -0.1 }
scale = { mercenary_discipline = 0.10 }
max = { governing_capacity_modifier = 0.20 }
}
"#,
	)
	.expect("write hegemons");
	fs::write(
		mod_root
			.join("common")
			.join("personal_deities")
			.join("00_hindu_deities.txt"),
		r#"
shiva = {
sprite = 1
potential = { religion = hinduism }
trigger = { religion = hinduism }
effect = { add_prestige = 1 }
removed_effect = { add_prestige = -1 }
ai_will_do = { factor = 1 }
}
"#,
	)
	.expect("write personal deities");
	fs::write(
		mod_root
			.join("common")
			.join("fetishist_cults")
			.join("00_fetishist_cults.txt"),
		r#"
yemoja_cult = {
allow = { religion = shamanism }
sprite = 1
ai_will_do = { factor = 1 }
}
"#,
	)
	.expect("write fetishist cults");

	let files = vec![
		parse_script_file(
			"1014",
			&mod_root,
			&mod_root
				.join("common")
				.join("church_aspects")
				.join("00_church_aspects.txt"),
		)
		.expect("parsed church aspects"),
		parse_script_file(
			"1014",
			&mod_root,
			&mod_root
				.join("common")
				.join("factions")
				.join("00_factions.txt"),
		)
		.expect("parsed factions"),
		parse_script_file(
			"1014",
			&mod_root,
			&mod_root
				.join("common")
				.join("hegemons")
				.join("0_economic_hegemon.txt"),
		)
		.expect("parsed hegemons"),
		parse_script_file(
			"1014",
			&mod_root,
			&mod_root
				.join("common")
				.join("personal_deities")
				.join("00_hindu_deities.txt"),
		)
		.expect("parsed personal deities"),
		parse_script_file(
			"1014",
			&mod_root,
			&mod_root
				.join("common")
				.join("fetishist_cults")
				.join("00_fetishist_cults.txt"),
		)
		.expect("parsed fetishist cults"),
	];

	let index = build_semantic_index(&files);
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/church_aspects/00_church_aspects.txt")
			&& usage.key == "religion"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
	}));
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/church_aspects/00_church_aspects.txt")
			&& usage.key == "add_stability"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Effect
	}));
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/factions/00_factions.txt")
			&& usage.key == "has_dlc"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
	}));
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/hegemons/0_economic_hegemon.txt")
			&& usage.key == "war_exhaustion"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Block
	}));
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/personal_deities/00_hindu_deities.txt")
			&& usage.key == "add_prestige"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Effect
	}));
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/fetishist_cults/00_fetishist_cults.txt")
			&& usage.key == "religion"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/church_aspects/00_church_aspects.txt")
			&& reference.key == "localisation"
			&& reference.value == "organised_through_bishops_aspect"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/church_aspects/00_church_aspects.txt")
			&& reference.key == "localisation_desc"
			&& reference.value == "desc_organised_through_bishops_aspect"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/church_aspects/00_church_aspects.txt")
			&& reference.key == "localisation_modifier"
			&& reference.value == "organised_through_bishops_aspect_modifier"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/factions/00_factions.txt")
			&& reference.key == "localisation"
			&& reference.value == "rr_jacobins"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/factions/00_factions.txt")
			&& reference.key == "localisation_influence"
			&& reference.value == "rr_jacobins_influence"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/factions/00_factions.txt")
			&& reference.key == "monarch_power"
			&& reference.value == "ADM"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/hegemons/0_economic_hegemon.txt")
			&& reference.key == "localisation"
			&& reference.value == "economic_hegemon"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/personal_deities/00_hindu_deities.txt")
			&& reference.key == "localisation"
			&& reference.value == "shiva"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/personal_deities/00_hindu_deities.txt")
			&& reference.key == "localisation_desc"
			&& reference.value == "shiva_desc"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/fetishist_cults/00_fetishist_cults.txt")
			&& reference.key == "localisation"
			&& reference.value == "yemoja_cult"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/fetishist_cults/00_fetishist_cults.txt")
			&& reference.key == "localisation_desc"
			&& reference.value == "yemoja_cult_desc"
	}));
}

#[test]
fn policies_and_mercenary_companies_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("policies")).expect("create policies");
	fs::create_dir_all(mod_root.join("common").join("mercenary_companies"))
		.expect("create mercenary companies");
	fs::write(
		mod_root.join("common").join("policies").join("00_adm.txt"),
		r#"
the_combination_act = {
monarch_power = ADM
potential = { has_idea_group = aristocracy_ideas }
allow = { full_idea_group = aristocracy_ideas }
effect = { add_prestige = 1 }
removed_effect = { add_prestige = -1 }
ai_will_do = { factor = 1 }
}
"#,
	)
	.expect("write policies");
	fs::write(
		mod_root
			.join("common")
			.join("mercenary_companies")
			.join("00_mercenaries.txt"),
		r#"
merc_black_army = {
mercenary_desc_key = FREE_OF_ARMY_PROFESSIONALISM_COST
home_province = 153
sprites = { dlc102_hun_sprite_pack easterngfx_sprite_pack }
trigger = {
	tag = HUN
}
modifier = {
	discipline = 0.05
}
}
"#,
	)
	.expect("write mercenary companies");

	let files = vec![
		parse_script_file(
			"1015",
			&mod_root,
			&mod_root.join("common").join("policies").join("00_adm.txt"),
		)
		.expect("parsed policies"),
		parse_script_file(
			"1015",
			&mod_root,
			&mod_root
				.join("common")
				.join("mercenary_companies")
				.join("00_mercenaries.txt"),
		)
		.expect("parsed mercenary companies"),
	];

	let index = build_semantic_index(&files);
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/policies/00_adm.txt")
			&& usage.key == "has_idea_group"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
	}));
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/policies/00_adm.txt")
			&& usage.key == "add_prestige"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Effect
	}));
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
			&& usage.key == "tag"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
	}));
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
			&& usage.key == "discipline"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Block
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/policies/00_adm.txt")
			&& reference.key == "localisation"
			&& reference.value == "the_combination_act"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/policies/00_adm.txt")
			&& reference.key == "monarch_power"
			&& reference.value == "ADM"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
			&& reference.key == "localisation"
			&& reference.value == "merc_black_army"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
			&& reference.key == "mercenary_desc_key"
			&& reference.value == "FREE_OF_ARMY_PROFESSIONALISM_COST"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
			&& reference.key == "home_province"
			&& reference.value == "153"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
			&& reference.key == "sprites"
			&& reference.value == "dlc102_hun_sprite_pack"
	}));
}

#[test]
fn low_risk_common_mechanics_roots_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("fervor")).expect("create fervor");
	fs::create_dir_all(mod_root.join("common").join("decrees")).expect("create decrees");
	fs::create_dir_all(mod_root.join("common").join("federation_advancements"))
		.expect("create federation advancements");
	fs::create_dir_all(mod_root.join("common").join("golden_bulls")).expect("create golden bulls");
	fs::create_dir_all(mod_root.join("common").join("flagship_modifications"))
		.expect("create flagship modifications");
	fs::create_dir_all(mod_root.join("common").join("holy_orders")).expect("create holy orders");
	fs::create_dir_all(mod_root.join("common").join("naval_doctrines"))
		.expect("create naval doctrines");
	fs::create_dir_all(mod_root.join("common").join("defender_of_faith"))
		.expect("create defender of faith");
	fs::create_dir_all(mod_root.join("common").join("isolationism")).expect("create isolationism");
	fs::create_dir_all(mod_root.join("common").join("professionalism"))
		.expect("create professionalism");
	fs::create_dir_all(mod_root.join("common").join("powerprojection"))
		.expect("create powerprojection");
	fs::create_dir_all(mod_root.join("common").join("subject_type_upgrades"))
		.expect("create subject type upgrades");
	fs::create_dir_all(mod_root.join("common").join("government_ranks"))
		.expect("create government ranks");
	fs::write(
		mod_root.join("common").join("fervor").join("00_fervor.txt"),
		r#"
fervor_trade = {
cost_type = fervor
potential = { religion = reformed }
}
"#,
	)
	.expect("write fervor");
	fs::write(
		mod_root.join("common").join("decrees").join("00_china.txt"),
		r#"
expand_bureaucracy_decree = {
duration = 120
icon = decree_expand_bureaucracy
}
"#,
	)
	.expect("write decrees");
	fs::write(
		mod_root
			.join("common")
			.join("federation_advancements")
			.join("00_default.txt"),
		r#"
federal_constitution = {
gfx = federation_constitution
names = { federation_name_key }
effect = {
	government = federal_republic
	religion = catholic
	tag = HUN
}
}
"#,
	)
	.expect("write federation advancements");
	fs::write(
		mod_root
			.join("common")
			.join("golden_bulls")
			.join("00_golden_bulls.txt"),
		r#"
golden_bull_treasury = {
mechanics = { curia_treasury curia_powers }
}
"#,
	)
	.expect("write golden bulls");
	fs::write(
		mod_root
			.join("common")
			.join("flagship_modifications")
			.join("00_flagship_modifications.txt"),
		r#"
extra_cannons = {
cost_type = sailors
base_modification = yes
}
"#,
	)
	.expect("write flagship modifications");
	fs::write(
		mod_root
			.join("common")
			.join("holy_orders")
			.join("00_holy_orders.txt"),
		r#"
benedictines = {
icon = GFX_holy_order_benedictines
cost_type = adm_power
localization = holy_order
}
"#,
	)
	.expect("write holy orders");
	fs::write(
		mod_root
			.join("common")
			.join("naval_doctrines")
			.join("00_naval_doctrines.txt"),
		r#"
fleet_in_being = {
button_gfx = 1
country_modifier = { naval_maintenance_modifier = -0.15 }
}
"#,
	)
	.expect("write naval doctrines");
	fs::write(
		mod_root
			.join("common")
			.join("defender_of_faith")
			.join("00_defender_of_faith.txt"),
		r#"
defender_of_faith_1 = {
level = 1
range_to = 5
ai_will_do = { factor = 0.1 }
}
"#,
	)
	.expect("write defender of faith");
	fs::write(
		mod_root
			.join("common")
			.join("isolationism")
			.join("00_shinto.txt"),
		r#"
open_doors_isolation = {
isolation_value = 0
modifier = { technology_cost = -0.05 }
}
"#,
	)
	.expect("write isolationism");
	fs::write(
		mod_root
			.join("common")
			.join("professionalism")
			.join("00_modifiers.txt"),
		r#"
nothingness_modifier = {
marker_sprite = GFX_pa_rank_0
unit_sprite_start = "GFX_ap1_"
trigger = { always = yes }
}
"#,
	)
	.expect("write professionalism");
	fs::write(
		mod_root
			.join("common")
			.join("powerprojection")
			.join("00_static.txt"),
		r#"
great_power_1 = {
power = 25
}
humiliated_rival = {
power = 30
max = 30
yearly_decay = 1
}
"#,
	)
	.expect("write powerprojection");
	fs::write(
		mod_root
			.join("common")
			.join("subject_type_upgrades")
			.join("00_subject_type_upgrades.txt"),
		r#"
increase_force_limit_from_colony = {
cost = 100
effect = {
	custom_tooltip = increase_force_limit_from_colony_tt
}
modifier_overlord = {
	land_forcelimit = 5
}
}
allow_autonomous_trade = {
cost = 100
modifier_subject = {
	liberty_desire = -5
}
}
"#,
	)
	.expect("write subject type upgrades");
	fs::write(
		mod_root
			.join("common")
			.join("government_ranks")
			.join("00_government_ranks.txt"),
		r#"
2 = {
diplomats = 1
governing_capacity = 200
}
3 = {
global_autonomy = -0.05
max_absolutism = 5
}
"#,
	)
	.expect("write government ranks");

	let files = vec![
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root.join("common").join("fervor").join("00_fervor.txt"),
		)
		.expect("parsed fervor"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root.join("common").join("decrees").join("00_china.txt"),
		)
		.expect("parsed decrees"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("federation_advancements")
				.join("00_default.txt"),
		)
		.expect("parsed federation advancements"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("golden_bulls")
				.join("00_golden_bulls.txt"),
		)
		.expect("parsed golden bulls"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("flagship_modifications")
				.join("00_flagship_modifications.txt"),
		)
		.expect("parsed flagship modifications"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("holy_orders")
				.join("00_holy_orders.txt"),
		)
		.expect("parsed holy orders"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("naval_doctrines")
				.join("00_naval_doctrines.txt"),
		)
		.expect("parsed naval doctrines"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("defender_of_faith")
				.join("00_defender_of_faith.txt"),
		)
		.expect("parsed defender of faith"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("isolationism")
				.join("00_shinto.txt"),
		)
		.expect("parsed isolationism"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("professionalism")
				.join("00_modifiers.txt"),
		)
		.expect("parsed professionalism"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("powerprojection")
				.join("00_static.txt"),
		)
		.expect("parsed powerprojection"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("subject_type_upgrades")
				.join("00_subject_type_upgrades.txt"),
		)
		.expect("parsed subject type upgrades"),
		parse_script_file(
			"1017",
			&mod_root,
			&mod_root
				.join("common")
				.join("government_ranks")
				.join("00_government_ranks.txt"),
		)
		.expect("parsed government ranks"),
	];

	let index = build_semantic_index(&files);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/fervor/00_fervor.txt")
			&& reference.key == "fervor_definition"
			&& reference.value == "fervor_trade"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/fervor/00_fervor.txt")
			&& reference.key == "cost_type"
			&& reference.value == "fervor"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/decrees/00_china.txt")
			&& reference.key == "decree_definition"
			&& reference.value == "expand_bureaucracy_decree"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/decrees/00_china.txt")
			&& reference.key == "icon"
			&& reference.value == "decree_expand_bureaucracy"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/federation_advancements/00_default.txt")
			&& reference.key == "federation_advancement_definition"
			&& reference.value == "federal_constitution"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/federation_advancements/00_default.txt")
			&& reference.key == "gfx"
			&& reference.value == "federation_constitution"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/federation_advancements/00_default.txt")
			&& reference.key == "names"
			&& reference.value == "federation_name_key"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/federation_advancements/00_default.txt")
			&& reference.key == "government"
			&& reference.value == "federal_republic"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/federation_advancements/00_default.txt")
			&& reference.key == "religion"
			&& reference.value == "catholic"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/federation_advancements/00_default.txt")
			&& reference.key == "tag"
			&& reference.value == "HUN"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/golden_bulls/00_golden_bulls.txt")
			&& reference.key == "golden_bull_definition"
			&& reference.value == "golden_bull_treasury"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/golden_bulls/00_golden_bulls.txt")
			&& reference.key == "mechanics"
			&& reference.value == "curia_treasury"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/golden_bulls/00_golden_bulls.txt")
			&& reference.key == "mechanics"
			&& reference.value == "curia_powers"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/flagship_modifications/00_flagship_modifications.txt")
			&& reference.key == "flagship_modification_definition"
			&& reference.value == "extra_cannons"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/flagship_modifications/00_flagship_modifications.txt")
			&& reference.key == "cost_type"
			&& reference.value == "sailors"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/holy_orders/00_holy_orders.txt")
			&& reference.key == "holy_order_definition"
			&& reference.value == "benedictines"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/holy_orders/00_holy_orders.txt")
			&& reference.key == "cost_type"
			&& reference.value == "adm_power"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/naval_doctrines/00_naval_doctrines.txt")
			&& reference.key == "naval_doctrine_definition"
			&& reference.value == "fleet_in_being"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/naval_doctrines/00_naval_doctrines.txt")
			&& reference.key == "button_gfx"
			&& reference.value == "1"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/defender_of_faith/00_defender_of_faith.txt")
			&& reference.key == "defender_of_faith_definition"
			&& reference.value == "defender_of_faith_1"
	}));
	assert!(index.scalar_assignments.iter().any(|assignment| {
		assignment.path == Path::new("common/defender_of_faith/00_defender_of_faith.txt")
			&& assignment.key == "level"
			&& assignment.value == "1"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/isolationism/00_shinto.txt")
			&& reference.key == "isolationism_definition"
			&& reference.value == "open_doors_isolation"
	}));
	assert!(index.scalar_assignments.iter().any(|assignment| {
		assignment.path == Path::new("common/isolationism/00_shinto.txt")
			&& assignment.key == "isolation_value"
			&& assignment.value == "0"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/professionalism/00_modifiers.txt")
			&& reference.key == "professionalism_definition"
			&& reference.value == "nothingness_modifier"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/professionalism/00_modifiers.txt")
			&& reference.key == "marker_sprite"
			&& reference.value == "GFX_pa_rank_0"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/professionalism/00_modifiers.txt")
			&& reference.key == "unit_sprite_start"
			&& reference.value == "GFX_ap1_"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/powerprojection/00_static.txt")
			&& reference.key == "powerprojection_definition"
			&& reference.value == "great_power_1"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/powerprojection/00_static.txt")
			&& reference.key == "powerprojection_definition"
			&& reference.value == "humiliated_rival"
	}));
	assert!(index.scalar_assignments.iter().any(|assignment| {
		assignment.path == Path::new("common/powerprojection/00_static.txt")
			&& assignment.key == "power"
			&& assignment.value == "25"
	}));
	assert!(index.scalar_assignments.iter().any(|assignment| {
		assignment.path == Path::new("common/powerprojection/00_static.txt")
			&& assignment.key == "yearly_decay"
			&& assignment.value == "1"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/subject_type_upgrades/00_subject_type_upgrades.txt")
			&& reference.key == "subject_type_upgrade_definition"
			&& reference.value == "increase_force_limit_from_colony"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/subject_type_upgrades/00_subject_type_upgrades.txt")
			&& reference.key == "subject_type_upgrade_definition"
			&& reference.value == "allow_autonomous_trade"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/subject_type_upgrades/00_subject_type_upgrades.txt")
			&& reference.key == "custom_tooltip"
			&& reference.value == "increase_force_limit_from_colony_tt"
	}));
	assert!(index.scalar_assignments.iter().any(|assignment| {
		assignment.path == Path::new("common/subject_type_upgrades/00_subject_type_upgrades.txt")
			&& assignment.key == "cost"
			&& assignment.value == "100"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/government_ranks/00_government_ranks.txt")
			&& reference.key == "government_rank_definition"
			&& reference.value == "2"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/government_ranks/00_government_ranks.txt")
			&& reference.key == "government_rank_definition"
			&& reference.value == "3"
	}));
	assert!(index.scalar_assignments.iter().any(|assignment| {
		assignment.path == Path::new("common/government_ranks/00_government_ranks.txt")
			&& assignment.key == "diplomats"
			&& assignment.value == "1"
	}));
	assert!(index.scalar_assignments.iter().any(|assignment| {
		assignment.path == Path::new("common/government_ranks/00_government_ranks.txt")
			&& assignment.key == "global_autonomy"
			&& assignment.value == "-0.05"
	}));
}

#[test]
fn diplomacy_and_advisor_history_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("history").join("diplomacy"))
		.expect("create diplomacy history");
	fs::create_dir_all(mod_root.join("history").join("advisors")).expect("create advisor history");
	fs::write(
		mod_root.join("history").join("diplomacy").join("hre.txt"),
		r#"
alliance = {
first = FRA
second = SCO
start_date = 1444.11.11
}
1399.1.1 = {
emperor = BOH
}
1368.1.23 = {
celestial_emperor = MNG
}
"#,
	)
	.expect("write diplomacy history");
	fs::write(
		mod_root
			.join("history")
			.join("advisors")
			.join("00_england.txt"),
		r#"
advisor = {
advisor_id = 216
name = "Thomas More"
location = 236
type = theologian
culture = english
religion = catholic
date = 1444.11.11
death_date = 1460.1.1
}
"#,
	)
	.expect("write advisor history");

	let files = vec![
		parse_script_file(
			"1016",
			&mod_root,
			&mod_root.join("history").join("diplomacy").join("hre.txt"),
		)
		.expect("parsed diplomacy history"),
		parse_script_file(
			"1016",
			&mod_root,
			&mod_root
				.join("history")
				.join("advisors")
				.join("00_england.txt"),
		)
		.expect("parsed advisor history"),
	];

	let index = build_semantic_index(&files);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/diplomacy/hre.txt")
			&& reference.key == "relation_type"
			&& reference.value == "alliance"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/diplomacy/hre.txt")
			&& reference.key == "first"
			&& reference.value == "FRA"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/diplomacy/hre.txt")
			&& reference.key == "second"
			&& reference.value == "SCO"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/diplomacy/hre.txt")
			&& reference.key == "emperor"
			&& reference.value == "BOH"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/diplomacy/hre.txt")
			&& reference.key == "celestial_emperor"
			&& reference.value == "MNG"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/advisors/00_england.txt")
			&& reference.key == "advisor_definition"
			&& reference.value == "advisor_216"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/advisors/00_england.txt")
			&& reference.key == "advisor_id"
			&& reference.value == "216"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/advisors/00_england.txt")
			&& reference.key == "location"
			&& reference.value == "236"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/advisors/00_england.txt")
			&& reference.key == "type"
			&& reference.value == "theologian"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/advisors/00_england.txt")
			&& reference.key == "culture"
			&& reference.value == "english"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("history/advisors/00_england.txt")
			&& reference.key == "religion"
			&& reference.value == "catholic"
	}));
}

#[test]
fn province_names_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("province_names"))
		.expect("create province names");
	fs::write(
		mod_root
			.join("common")
			.join("province_names")
			.join("sorbian.txt"),
		"4778 = \"Zhorjelc\"\n38 = \"Drjezdzany\"\n",
	)
	.expect("write province names");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("common")
			.join("province_names")
			.join("sorbian.txt"),
	)
	.expect("parsed province names");
	let index = build_semantic_index(&[parsed]);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/province_names/sorbian.txt")
			&& reference.key == "province_name_table"
			&& reference.value == "sorbian"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/province_names/sorbian.txt")
			&& reference.key == "province_id"
			&& reference.value == "4778"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/province_names/sorbian.txt")
			&& reference.key == "province_name_literal"
			&& reference.value == "Zhorjelc"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/province_names/sorbian.txt")
			&& reference.key == "province_id"
			&& reference.value == "38"
	}));
}

#[test]
fn random_map_tiles_and_names_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("map").join("random").join("tiles"))
		.expect("create random map tiles");
	fs::write(
		mod_root
			.join("map")
			.join("random")
			.join("tiles")
			.join("tile0.txt"),
		r#"
sea_province = { 93 164 236 }
region = { 12 34 56 }
size = { 7 7 }
num_sea_provinces = 21
num_land_provinces = 34
weight = 130
continent = yes
"#,
	)
	.expect("write tile file");
	fs::write(
		mod_root
			.join("map")
			.join("random")
			.join("RandomLandNames.txt"),
		r#"
random_names = {
p_tumbletown
p_chugwater:river
}
"#,
	)
	.expect("write random land names");

	let files = vec![
		parse_script_file(
			"1016",
			&mod_root,
			&mod_root
				.join("map")
				.join("random")
				.join("tiles")
				.join("tile0.txt"),
		)
		.expect("parsed tile file"),
		parse_script_file(
			"1016",
			&mod_root,
			&mod_root
				.join("map")
				.join("random")
				.join("RandomLandNames.txt"),
		)
		.expect("parsed random names"),
	];

	let index = build_semantic_index(&files);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/tiles/tile0.txt")
			&& reference.key == "tile_definition"
			&& reference.value == "tile0"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/tiles/tile0.txt")
			&& reference.key == "tile_color_group"
			&& reference.value == "sea_province"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/tiles/tile0.txt")
			&& reference.key == "tile_color_rgb"
			&& reference.value == "93,164,236"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/tiles/tile0.txt")
			&& reference.key == "tile_size"
			&& reference.value == "7,7"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/tiles/tile0.txt")
			&& matches!(
				reference.key.as_str(),
				"num_sea_provinces" | "num_land_provinces" | "weight" | "continent"
			)
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RandomLandNames.txt")
			&& reference.key == "random_name_table"
			&& reference.value == "random_land_names"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RandomLandNames.txt")
			&& reference.key == "random_name_token"
			&& reference.value == "p_tumbletown"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RandomLandNames.txt")
			&& reference.key == "random_name_token"
			&& reference.value == "p_chugwater"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RandomLandNames.txt")
			&& reference.key == "random_name_category"
			&& reference.value == "river"
	}));
}

#[test]
fn random_map_scenarios_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("map").join("random")).expect("create random map root");
	fs::write(
		mod_root.join("map").join("random").join("RNWScenarios.txt"),
		r#"
scenario_animism_tribes = {
religion = animism
technology_group = south_american
government = native
graphical_culture = northamericagfx
names = {
	rnw_arauluche
	rnw_namuncurcha
}
}
"#,
	)
	.expect("write scenarios file");

	let files = vec![
		parse_script_file(
			"1016",
			&mod_root,
			&mod_root.join("map").join("random").join("RNWScenarios.txt"),
		)
		.expect("parsed scenarios file"),
	];

	let index = build_semantic_index(&files);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RNWScenarios.txt")
			&& reference.key == "random_map_scenario"
			&& reference.value == "scenario_animism_tribes"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RNWScenarios.txt")
			&& reference.key == "religion"
			&& reference.value == "animism"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RNWScenarios.txt")
			&& reference.key == "technology_group"
			&& reference.value == "south_american"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RNWScenarios.txt")
			&& reference.key == "government"
			&& reference.value == "native"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RNWScenarios.txt")
			&& reference.key == "graphical_culture"
			&& reference.value == "northamericagfx"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("map/random/RNWScenarios.txt")
			&& reference.key == "scenario_name_key"
			&& reference.value == "rnw_arauluche"
	}));
}

#[test]
fn technology_files_record_real_syntax_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("technologies")).expect("create technologies");
	fs::write(
		mod_root.join("common").join("technologies").join("adm.txt"),
		r#"
monarch_power = ADM
ahead_of_time = {
adm_tech_cost_modifier = 0.2
}
technology = {
year = 1444
expects_institution = {
	feudalism = 0.5
}
temple = yes
}
technology = {
year = 1466
expects_institution = {
	renaissance = 0.15
}
courthouse = yes
may_force_march = yes
}
"#,
	)
	.expect("write technologies");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root.join("common").join("technologies").join("adm.txt"),
	)
	.expect("parsed technologies");
	let index = build_semantic_index(&[parsed]);
	assert!(index.key_usages.iter().any(|usage| {
		usage.path == Path::new("common/technologies/adm.txt")
			&& usage.key == "adm_tech_cost_modifier"
			&& scope_kind(&index, usage.scope_id) == ScopeKind::Block
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "monarch_power"
			&& reference.value == "ADM"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "technology_definition"
			&& reference.value == "adm_tech_0"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "technology_definition"
			&& reference.value == "adm_tech_1"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "expects_institution"
			&& reference.value == "feudalism"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "expects_institution"
			&& reference.value == "renaissance"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "enable"
			&& reference.value == "temple"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "enable"
			&& reference.value == "courthouse"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "enable"
			&& reference.value == "may_force_march"
	}));
}

#[test]
fn technology_files_reset_state_per_overlay_contributor() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_a_root = tmp.path().join("mod_a");
	let mod_b_root = tmp.path().join("mod_b");
	fs::create_dir_all(mod_a_root.join("common").join("technologies"))
		.expect("create mod a technologies");
	fs::create_dir_all(mod_b_root.join("common").join("technologies"))
		.expect("create mod b technologies");
	fs::write(
		mod_a_root
			.join("common")
			.join("technologies")
			.join("adm.txt"),
		r#"
monarch_power = ADM
technology = {
year = 1444
temple = yes
}
"#,
	)
	.expect("write mod a technologies");
	fs::write(
		mod_b_root
			.join("common")
			.join("technologies")
			.join("adm.txt"),
		r#"
monarch_power = DIP
technology = {
year = 1444
marketplace = yes
}
"#,
	)
	.expect("write mod b technologies");

	let files = vec![
		parse_script_file(
			"mod-a",
			&mod_a_root,
			&mod_a_root
				.join("common")
				.join("technologies")
				.join("adm.txt"),
		)
		.expect("parsed mod a technologies"),
		parse_script_file(
			"mod-b",
			&mod_b_root,
			&mod_b_root
				.join("common")
				.join("technologies")
				.join("adm.txt"),
		)
		.expect("parsed mod b technologies"),
	];

	let index = build_semantic_index(&files);
	assert!(index.resource_references.iter().any(|reference| {
		reference.mod_id == "mod-a"
			&& reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "technology_definition"
			&& reference.value == "adm_tech_0"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.mod_id == "mod-b"
			&& reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "technology_definition"
			&& reference.value == "dip_tech_0"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.mod_id == "mod-b"
			&& reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "monarch_power"
			&& reference.value == "DIP"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.mod_id == "mod-b"
			&& reference.path == Path::new("common/technologies/adm.txt")
			&& reference.key == "technology_definition"
			&& reference.value == "adm_tech_1"
	}));
}

#[test]
fn technology_groups_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common")).expect("create common");
	fs::write(
		mod_root.join("common").join("technology.txt"),
		r#"
groups = {
western = {
	start_level = 3
	start_cost_modifier = 0.0
	nation_designer_unit_type = western
	nation_designer_trigger = {
		has_dlc = "Conquest of Paradise"
	}
	nation_designer_cost = {
		trigger = { is_free_or_tributary_trigger = yes }
		value = 25
	}
}
eastern = {
	start_level = 2
	start_cost_modifier = 0.1
	nation_designer_unit_type = eastern
}
}
"#,
	)
	.expect("write technology groups");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root.join("common").join("technology.txt"),
	)
	.expect("parsed technology groups");
	let index = build_semantic_index(&[parsed]);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technology.txt")
			&& reference.key == "technology_group"
			&& reference.value == "western"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technology.txt")
			&& reference.key == "nation_designer_unit_type"
			&& reference.value == "western"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/technology.txt")
			&& reference.key == "nation_designer_cost_value"
			&& reference.value == "25"
	}));
}

#[test]
fn index_builds_event_and_scope_types() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("events")).expect("create dir");
	fs::write(
		mod_root.join("events").join("x.txt"),
		r#"
namespace = test
country_event = {
id = test.1
option = {
	every_owned_province = {
		ROOT = { }
		province_event = { id = test.2 }
	}
}
}
province_event = {
id = test.2
trigger = {
	FROM = {
		has_country_flag = seen_city
	}
}
immediate = {
	owner = {
		country_event = { id = test.1 }
	}
}
}
"#,
	)
	.expect("write file");

	let parsed = parse_script_file("1000", &mod_root, &mod_root.join("events").join("x.txt"))
		.expect("parsed script");

	let index = build_semantic_index(&[parsed]);
	assert!(
		index
			.definitions
			.iter()
			.any(|item| item.kind == SymbolKind::Event && item.name == "test.1")
	);
	assert!(
		index
			.definitions
			.iter()
			.any(|item| item.kind == SymbolKind::Event && item.name == "test.2")
	);
	assert!(
		index
			.references
			.iter()
			.any(|item| item.kind == SymbolKind::Event && item.name == "test.2")
	);
	assert!(
		index
			.scopes
			.iter()
			.any(|scope| scope.this_type == ScopeType::Province)
	);
	assert!(
		!index.key_usages.iter().any(|usage| {
			(usage.key == "country_event" || usage.key == "province_event")
				&& scope_kind(&index, usage.scope_id) == ScopeKind::File
		}),
		"top-level event definitions should not be recorded as plain key usage"
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001" && finding.path == Some("events/x.txt".into())
		}),
		"typed event roots should not stay in Unknown scope"
	);
}

#[test]
fn achievements_builtin_blocks_do_not_become_scripted_effect_calls() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common")).expect("create dir");
	fs::write(
		mod_root.join("common").join("achievements.txt"),
		r#"
achievement_example = {
possible = {
	capital_scope = {
		all_core_province = {
			region = north_america
		}
	}
}
visible = {
	capital_scope = {
		region = japan_region
	}
}
happened = {
	all_core_province = {
		is_core = ROOT
	}
}
provinces_to_highlight = {
	all_core_province = {
		region = china_region
	}
}
}
"#,
	)
	.expect("write file");

	let parsed = parse_script_file(
		"1001",
		&mod_root,
		&mod_root.join("common").join("achievements.txt"),
	)
	.expect("parsed script");

	let index = build_semantic_index(&[parsed]);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/achievements.txt")
			&& reference.key == "achievement_definition"
			&& reference.value == "achievement_example"
	}));
	for value in [
		"possible",
		"visible",
		"happened",
		"provinces_to_highlight",
		"capital_scope",
		"all_core_province",
	] {
		assert!(!index.resource_references.iter().any(|reference| {
			reference.key == "achievement_definition" && reference.value == value
		}));
	}
	for name in [
		"possible",
		"visible",
		"happened",
		"provinces_to_highlight",
		"capital_scope",
		"all_core_province",
	] {
		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect && reference.name == name
			}),
			"{name} should not be recorded as a scripted effect reference"
		);
	}
	assert!(index.scopes.iter().any(|scope| {
		scope.kind == ScopeKind::AliasBlock && scope.this_type == ScopeType::Province
	}));
	assert!(
		index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Province)
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for name in [
		"possible",
		"visible",
		"happened",
		"provinces_to_highlight",
		"capital_scope",
		"all_core_province",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
			"{name} should not produce S002"
		);
	}
	assert!(
		!diagnostics
			.advisory
			.iter()
			.any(|finding| finding.rule_id == "A001"
				&& finding.path == Some("common/achievements.txt".into())),
		"achievements root scope should no longer stay Unknown"
	);
}

#[test]
fn common_data_file_roots_do_not_become_scripted_effect_calls() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("ideas")).expect("create ideas");
	fs::create_dir_all(mod_root.join("common").join("ages")).expect("create ages");
	fs::create_dir_all(mod_root.join("common").join("buildings")).expect("create buildings");
	fs::create_dir_all(mod_root.join("common").join("great_projects")).expect("create monuments");
	fs::create_dir_all(mod_root.join("common").join("institutions")).expect("create institutions");
	fs::create_dir_all(mod_root.join("common").join("province_triggered_modifiers"))
		.expect("create province modifiers");
	fs::create_dir_all(mod_root.join("common").join("custom_gui")).expect("create custom gui");
	fs::create_dir_all(mod_root.join("common").join("government_names"))
		.expect("create government names");
	fs::create_dir_all(mod_root.join("customizable_localization")).expect("create custom loc");
	fs::create_dir_all(mod_root.join("interface")).expect("create interface");
	fs::write(
		mod_root.join("common").join("ideas").join("ideas.txt"),
		"my_ideas = { start = { add_prestige = 1 } }\n",
	)
	.expect("write ideas");
	fs::write(
		mod_root.join("common").join("ages").join("ages.txt"),
		"age_of_discovery = { objectives = { obj_one = { calc_true_if = { all_owned_province = { is_core = ROOT controlled_by = owner exclude_from_progress = { is_core = ROOT } } amount = 1 } } } }\n",
	)
	.expect("write ages");
	fs::write(
		mod_root.join("common").join("buildings").join("buildings.txt"),
		"marketplace = { on_built = { owner = { add_prestige = 1 } FROM = { add_stability_cost_modifier = -0.1 } } }\n",
	)
	.expect("write buildings");
	fs::write(
		mod_root
			.join("common")
			.join("great_projects")
			.join("projects.txt"),
		"project_alpha = { build_cost = 1000 }\n",
	)
	.expect("write monuments");
	fs::write(
		mod_root
			.join("common")
			.join("institutions")
			.join("institutions.txt"),
		r#"
printing_press = {
potential = {
	owner = {
		government = monarchy
	}
}
on_start = {
	add_base_tax = 1
}
}
"#,
	)
	.expect("write institutions");
	fs::write(
		mod_root
			.join("common")
			.join("province_triggered_modifiers")
			.join("modifiers.txt"),
		r#"
prosperous = {
trigger = {
	owner = {
		government = monarchy
	}
}
}
"#,
	)
	.expect("write province modifiers");
	fs::write(
		mod_root
			.join("common")
			.join("custom_gui")
			.join("advisor.txt"),
		"advisor_actions = { title = advisor_title }\n",
	)
	.expect("write custom gui");
	fs::write(
		mod_root
			.join("common")
			.join("government_names")
			.join("names.txt"),
		"czech_localisation = { trigger = { government = monarchy } }\n",
	)
	.expect("write government names");
	fs::write(
		mod_root
			.join("customizable_localization")
			.join("defined.txt"),
		"defined_text = { name = GetFoo text = { localisation_key = foo trigger = { always = yes } } }\n",
	)
	.expect("write custom loc");
	fs::write(
		mod_root.join("interface").join("main.gui"),
		"windowType = { name = main_window }\n",
	)
	.expect("write ui");

	let parsed = [
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root.join("common").join("ideas").join("ideas.txt"),
		)
		.expect("parsed ideas"),
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root.join("common").join("ages").join("ages.txt"),
		)
		.expect("parsed ages"),
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root
				.join("common")
				.join("buildings")
				.join("buildings.txt"),
		)
		.expect("parsed buildings"),
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root
				.join("common")
				.join("great_projects")
				.join("projects.txt"),
		)
		.expect("parsed monuments"),
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root
				.join("common")
				.join("institutions")
				.join("institutions.txt"),
		)
		.expect("parsed institutions"),
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root
				.join("common")
				.join("province_triggered_modifiers")
				.join("modifiers.txt"),
		)
		.expect("parsed province modifiers"),
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root
				.join("common")
				.join("custom_gui")
				.join("advisor.txt"),
		)
		.expect("parsed custom gui"),
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root
				.join("common")
				.join("government_names")
				.join("names.txt"),
		)
		.expect("parsed government names"),
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root
				.join("customizable_localization")
				.join("defined.txt"),
		)
		.expect("parsed custom loc"),
		parse_script_file(
			"1002",
			&mod_root,
			&mod_root.join("interface").join("main.gui"),
		)
		.expect("parsed ui"),
	];

	let index = build_semantic_index(&parsed);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for path in [
		"common/ideas/ideas.txt",
		"common/ages/ages.txt",
		"common/buildings/buildings.txt",
		"common/great_projects/projects.txt",
		"common/institutions/institutions.txt",
		"common/province_triggered_modifiers/modifiers.txt",
		"common/custom_gui/advisor.txt",
		"common/government_names/names.txt",
		"customizable_localization/defined.txt",
		"interface/main.gui",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.path == Some(path.into()) }),
			"{path} should not report top-level scripted effect fallback"
		);
	}
	for path in [
		"common/ages/ages.txt",
		"common/buildings/buildings.txt",
		"common/institutions/institutions.txt",
		"common/province_triggered_modifiers/modifiers.txt",
	] {
		assert!(
			!diagnostics
				.advisory
				.iter()
				.any(|finding| { finding.rule_id == "A001" && finding.path == Some(path.into()) }),
			"{path} should have a typed root scope"
		);
	}
}

#[test]
fn mislocated_dsl_paths_reuse_existing_semantics() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(
		mod_root
			.join("events")
			.join("common")
			.join("new_diplomatic_actions"),
	)
	.expect("create misplaced diplomatic actions");
	fs::create_dir_all(mod_root.join("events").join("decisions"))
		.expect("create misplaced decisions");
	fs::write(
		mod_root
			.join("events")
			.join("common")
			.join("new_diplomatic_actions")
			.join("actions.txt"),
		r#"
static_actions = {
royal_marriage = {
	alert_index = 1
}
}

sell_indulgence = {
is_visible = { always = yes }
on_accept = {
	missing_effect = { FLAG = TEST }
}
}
"#,
	)
	.expect("write misplaced diplomatic actions");
	fs::write(
		mod_root
			.join("events")
			.join("decisions")
			.join("decisions.txt"),
		r#"
country_decisions = {
test_decision = {
	potential = { always = yes }
	effect = {
		missing_decision_effect = { FLAG = TEST }
	}
}
}
"#,
	)
	.expect("write misplaced decisions");

	let parsed = [
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root
				.join("events")
				.join("common")
				.join("new_diplomatic_actions")
				.join("actions.txt"),
		)
		.expect("parsed misplaced diplomatic actions"),
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root
				.join("events")
				.join("decisions")
				.join("decisions.txt"),
		)
		.expect("parsed misplaced decisions"),
	];
	let index = build_semantic_index(&parsed);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for path in [
		"events/common/new_diplomatic_actions/actions.txt",
		"events/decisions/decisions.txt",
	] {
		assert!(
			!diagnostics
				.advisory
				.iter()
				.any(|finding| { finding.rule_id == "A001" && finding.path == Some(path.into()) }),
			"{path} should reuse typed DSL semantics"
		);
	}
	for name in [
		"static_actions",
		"royal_marriage",
		"is_visible",
		"country_decisions",
		"test_decision",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
			"{name} should not be treated as a scripted effect"
		);
	}
	assert!(diagnostics.strict.iter().any(|finding| {
		finding.rule_id == "S002" && finding.message.contains("missing_effect")
	}));
	assert!(diagnostics.strict.iter().any(|finding| {
		finding.rule_id == "S002" && finding.message.contains("missing_decision_effect")
	}));
}

#[test]
fn missions_and_map_groups_do_not_become_scripted_effect_calls() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("missions")).expect("create missions");
	fs::create_dir_all(mod_root.join("map")).expect("create map");
	fs::write(
		mod_root.join("map").join("area.txt"),
		"finland_area = { 1 }\n",
	)
	.expect("write area");
	fs::write(
		mod_root.join("map").join("region.txt"),
		"baltic_region = { areas = { finland_area } }\n",
	)
	.expect("write region");
	fs::write(
		mod_root.join("missions").join("missions.txt"),
		r#"
mos_rus_handle_succession = {
potential_on_load = {
	has_dlc = "Domination"
}
mos_rus_window_on_the_west = {
	required_missions = { mos_prev }
	trigger = {
		baltic_region = {
			type = all
			owned_by = ROOT
		}
	}
	effect = {
		finland_area = {
			add_prestige = 1
		}
		missing_effect = { FLAG = TEST }
	}
	ai_weight = {
		factor = 100
	}
}
}
"#,
	)
	.expect("write missions");

	let parsed = [
		parse_script_file("1004", &mod_root, &mod_root.join("map").join("area.txt"))
			.expect("parsed area"),
		parse_script_file("1004", &mod_root, &mod_root.join("map").join("region.txt"))
			.expect("parsed region"),
		parse_script_file(
			"1004",
			&mod_root,
			&mod_root.join("missions").join("missions.txt"),
		)
		.expect("parsed missions"),
	];
	let index = build_semantic_index(&parsed);
	for name in [
		"potential_on_load",
		"mos_rus_window_on_the_west",
		"required_missions",
		"baltic_region",
		"finland_area",
		"ai_weight",
	] {
		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect && reference.name == name
			}),
			"{name} should not be recorded as a scripted effect reference"
		);
	}
	assert!(
		index.scopes.iter().any(|scope| {
			scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Province
		})
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for name in [
		"potential_on_load",
		"mos_rus_window_on_the_west",
		"required_missions",
		"baltic_region",
		"finland_area",
		"ai_weight",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
			"{name} should not produce S002"
		);
	}
	assert!(diagnostics.strict.iter().any(|finding| {
		finding.rule_id == "S002" && finding.message.contains("missing_effect")
	}));
}

#[test]
fn mission_event_and_common_wrappers_do_not_become_scripted_effect_calls() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("missions")).expect("create missions");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::create_dir_all(mod_root.join("common").join("government_reforms"))
		.expect("create government reforms");
	fs::create_dir_all(mod_root.join("common").join("scripted_triggers"))
		.expect("create scripted triggers");
	fs::write(
		mod_root.join("missions").join("missions.txt"),
		r#"
mos_rus_window_on_the_west = {
ai_weight = {
	mission_weight_helper = { FLAG = TEST }
}
}
"#,
	)
	.expect("write missions");
	fs::write(
		mod_root.join("events").join("event.txt"),
		r#"
namespace = test
country_event = {
id = test.1
mean_time_to_happen = {
	event_weight_helper = { FLAG = TEST }
}
}
"#,
	)
	.expect("write event");
	fs::write(
		mod_root
			.join("common")
			.join("government_reforms")
			.join("reforms.txt"),
		r#"
test_reform = {
ai_will_do = {
	common_weight_helper = { FLAG = TEST }
}
}
"#,
	)
	.expect("write government reforms");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_triggers")
			.join("helpers.txt"),
		r#"
mission_weight_helper = { always = yes }
event_weight_helper = { always = yes }
common_weight_helper = { always = yes }
"#,
	)
	.expect("write scripted triggers");

	let parsed = [
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root.join("missions").join("missions.txt"),
		)
		.expect("parsed missions"),
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root.join("events").join("event.txt"),
		)
		.expect("parsed event"),
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root
				.join("common")
				.join("government_reforms")
				.join("reforms.txt"),
		)
		.expect("parsed government reforms"),
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_triggers")
				.join("helpers.txt"),
		)
		.expect("parsed scripted triggers"),
	];
	let index = build_semantic_index(&parsed);
	for name in [
		"ai_weight",
		"mission_weight_helper",
		"mean_time_to_happen",
		"event_weight_helper",
		"ai_will_do",
		"common_weight_helper",
	] {
		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect && reference.name == name
			}),
			"{name} should not be recorded as a scripted effect reference"
		);
	}

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for name in [
		"ai_weight",
		"mission_weight_helper",
		"mean_time_to_happen",
		"event_weight_helper",
		"ai_will_do",
		"common_weight_helper",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
			"{name} should not produce S002"
		);
	}
}

#[test]
fn new_diplomatic_actions_containers_preserve_nested_effect_calls() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("new_diplomatic_actions"))
		.expect("create new diplomatic actions");
	fs::write(
		mod_root
			.join("common")
			.join("new_diplomatic_actions")
			.join("actions.txt"),
		r#"
static_actions = {
royal_marriage = {
	alert_index = 1
}
}

sell_indulgence = {
is_visible = { always = yes }
is_allowed = { always = yes }
on_accept = {
	missing_effect = { FLAG = TEST }
}
on_decline = {}
ai_acceptance = {
	add_entry = {
		name = TRUST
		change_variable = { which = score value = 1 }
		missing_inner_effect = { FLAG = TEST }
	}
}
}
"#,
	)
	.expect("write actions");

	let parsed = [parse_script_file(
		"1005",
		&mod_root,
		&mod_root
			.join("common")
			.join("new_diplomatic_actions")
			.join("actions.txt"),
	)
	.expect("parsed actions")];
	let index = build_semantic_index(&parsed);
	assert!(index.definitions.iter().any(|definition| {
		definition.kind == SymbolKind::DiplomaticAction
			&& definition.local_name == "sell_indulgence"
	}));
	for name in [
		"static_actions",
		"is_visible",
		"is_allowed",
		"on_accept",
		"on_decline",
		"ai_acceptance",
		"add_entry",
		"royal_marriage",
	] {
		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect && reference.name == name
			}),
			"{name} should not be recorded as a scripted effect reference"
		);
	}

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for name in [
		"static_actions",
		"is_visible",
		"is_allowed",
		"on_accept",
		"on_decline",
		"ai_acceptance",
		"add_entry",
		"royal_marriage",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
			"{name} should not produce S002"
		);
	}
	for name in ["missing_effect", "missing_inner_effect"] {
		assert!(
			diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) })
		);
	}
}

#[test]
fn new_diplomatic_actions_if_blocks_keep_nested_effect_calls_in_effect_context() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("new_diplomatic_actions"))
		.expect("create new diplomatic actions");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::write(
		mod_root
			.join("common")
			.join("new_diplomatic_actions")
			.join("actions.txt"),
		r#"
request_general = {
on_accept = {
	if = {
		limit = { always = yes }
		create_general_from_country = { who = FROM }
	}
}
}
"#,
	)
	.expect("write actions");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		r#"
create_general_from_country {
$who$ = {
	trigger_switch = {
		on_trigger = army_tradition
		100 = {
			PREV = {
				create_general = {
					culture = PREV
					tradition = 100
				}
			}
		}
	}
}
}
"#,
	)
	.expect("write effects");

	let parsed = [
		parse_script_file(
			"1007",
			&mod_root,
			&mod_root
				.join("common")
				.join("new_diplomatic_actions")
				.join("actions.txt"),
		)
		.expect("parsed actions"),
		parse_script_file(
			"1007",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
		)
		.expect("parsed effects"),
	];
	let index = build_semantic_index(&parsed);
	assert!(index.references.iter().any(|reference| {
		reference.kind == SymbolKind::ScriptedEffect
			&& reference.name == "create_general_from_country"
	}));
	assert!(
		!index.references.iter().any(|reference| {
			reference.kind == SymbolKind::ScriptedTrigger
				&& reference.name == "create_general_from_country"
		}),
		"nested if effect calls should not become scripted trigger references"
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S002"
				&& finding
					.path
					.as_ref()
					.map(|path| path.ends_with("common/new_diplomatic_actions/actions.txt"))
					.unwrap_or(false)
		}),
		"nested if effect calls should resolve as scripted effects"
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001"
				&& finding
					.path
					.as_ref()
					.map(|path| path.ends_with("common/scripted_effects/effects.txt"))
					.unwrap_or(false)
		}),
		"param-driven scope inference should suppress PREV Unknown-scope noise"
	);
}

#[test]
fn ages_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("ages")).expect("create ages");
	fs::write(
		mod_root.join("common").join("ages").join("ages.txt"),
		r#"
age_of_discovery = {
objectives = {
	obj_one = {
		calc_true_if = { always = yes amount = 1 }
	}
}
}
"#,
	)
	.expect("write ages");

	let parsed = [parse_script_file(
		"1010",
		&mod_root,
		&mod_root.join("common").join("ages").join("ages.txt"),
	)
	.expect("parsed ages")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/ages/ages.txt")
			&& reference.key == "age_definition"
			&& reference.value == "age_of_discovery"
	}));
	assert!(
		!index
			.resource_references
			.iter()
			.any(|reference| { reference.key == "age_definition" && reference.value == "obj_one" })
	);
}

#[test]
fn institutions_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("institutions")).expect("create institutions");
	fs::write(
		mod_root
			.join("common")
			.join("institutions")
			.join("institutions.txt"),
		r#"
renaissance = {
can_embrace = { always = yes }
on_start = {
	add_prestige = 5
}
}
"#,
	)
	.expect("write institutions");

	let parsed = [parse_script_file(
		"1011",
		&mod_root,
		&mod_root
			.join("common")
			.join("institutions")
			.join("institutions.txt"),
	)
	.expect("parsed institutions")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/institutions/institutions.txt")
			&& reference.key == "institution_definition"
			&& reference.value == "renaissance"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "institution_definition" && reference.value == "can_embrace"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "institution_definition" && reference.value == "on_start"
	}));
}

#[test]
fn great_projects_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("great_projects"))
		.expect("create great projects");
	fs::write(
		mod_root
			.join("common")
			.join("great_projects")
			.join("projects.txt"),
		r#"
coverage_project = {
	build_trigger = {
		always = yes
	}
	on_built = {
		add_prestige = 1
	}
}
"#,
	)
	.expect("write great projects");

	let parsed = [parse_script_file(
		"1021",
		&mod_root,
		&mod_root
			.join("common")
			.join("great_projects")
			.join("projects.txt"),
	)
	.expect("parsed great projects")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/great_projects/projects.txt")
			&& reference.key == "great_project_definition"
			&& reference.value == "coverage_project"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "great_project_definition" && reference.value == "build_trigger"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "great_project_definition" && reference.value == "on_built"
	}));
}

#[test]
fn great_projects_real_shape_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("great_projects"))
		.expect("create great projects");
	fs::write(
		mod_root
			.join("common")
			.join("great_projects")
			.join("projects.txt"),
		r#"
coverage_canal = {
	start = 1775
	date = 1895.06.20
	time = {
		months = 120
	}
	build_cost = 10000
	can_be_moved = no
	starting_tier = 3
	type = canal
	build_trigger = {
		FROM = {
			owns_or_vassal_of = 1775
		}
	}
	on_built = {
		add_canal = coverage_canal
	}
	on_destroyed = {
		remove_canal = coverage_canal
	}
	can_use_modifiers_trigger = {
	}
	tier_0 = {
		upgrade_time = {
			months = 0
		}
		on_upgraded = {
		}
	}
}
"#,
	)
	.expect("write real-shaped great projects");

	let parsed = [parse_script_file(
		"1022",
		&mod_root,
		&mod_root
			.join("common")
			.join("great_projects")
			.join("projects.txt"),
	)
	.expect("parsed real-shaped great projects")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/great_projects/projects.txt")
			&& reference.key == "great_project_definition"
			&& reference.value == "coverage_canal"
	}));
	for value in [
		"build_trigger",
		"on_built",
		"on_destroyed",
		"can_use_modifiers_trigger",
		"tier_0",
	] {
		assert!(!index.resource_references.iter().any(|reference| {
			reference.key == "great_project_definition" && reference.value == value
		}));
	}
}

#[test]
fn ideas_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("ideas")).expect("create ideas");
	fs::write(
		mod_root.join("common").join("ideas").join("ideas.txt"),
		r#"
coverage_ideas = {
	start = {
		add_prestige = 1
	}
	bonus = {
		global_tax_modifier = 0.1
	}
	coverage_idea_1 = {
		inflation_reduction = 0.05
	}
}
"#,
	)
	.expect("write ideas");

	let parsed = [parse_script_file(
		"1012",
		&mod_root,
		&mod_root.join("common").join("ideas").join("ideas.txt"),
	)
	.expect("parsed ideas")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/ideas/ideas.txt")
			&& reference.key == "idea_group_definition"
			&& reference.value == "coverage_ideas"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "idea_group_definition" && reference.value == "start"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "idea_group_definition" && reference.value == "bonus"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "idea_group_definition" && reference.value == "coverage_idea_1"
	}));
}

#[test]
fn advisortypes_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("advisortypes")).expect("create advisortypes");
	fs::write(
		mod_root
			.join("common")
			.join("advisortypes")
			.join("advisors.txt"),
		r#"
coverage_advisor = {
	trigger = {
		always = yes
	}
}
"#,
	)
	.expect("write advisortypes");

	let parsed = [parse_script_file(
		"1013",
		&mod_root,
		&mod_root
			.join("common")
			.join("advisortypes")
			.join("advisors.txt"),
	)
	.expect("parsed advisortypes")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/advisortypes/advisors.txt")
			&& reference.key == "advisor_type_definition"
			&& reference.value == "coverage_advisor"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "advisor_type_definition" && reference.value == "trigger"
	}));
}

#[test]
fn custom_gui_emit_definition_resources_from_top_level_custom_blocks() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("custom_gui")).expect("create custom gui");
	fs::write(
		mod_root.join("common").join("custom_gui").join("gui.txt"),
		r#"custom_button = {
	name = coverage_window
	potential = {
		always = yes
	}
	frame = {
		number = 1
		trigger = {
			always = yes
		}
	}
}
"#,
	)
	.expect("write custom gui");

	let parsed = [parse_script_file(
		"1019",
		&mod_root,
		&mod_root.join("common").join("custom_gui").join("gui.txt"),
	)
	.expect("parsed custom gui")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/custom_gui/gui.txt")
			&& reference.key == "custom_gui_definition"
			&& reference.value == "coverage_window"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "custom_gui_definition" && reference.value == "custom_button"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "custom_gui_definition" && reference.value == "potential"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "custom_gui_definition" && reference.value == "frame"
	}));
}

#[test]
fn cultures_emit_nested_culture_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("cultures")).expect("create cultures");
	fs::write(
		mod_root
			.join("common")
			.join("cultures")
			.join("cultures.txt"),
		r#"
coverage_group = {
	coverage_culture = {
		primary = { 1 2 3 }
		male_names = { "Alex" }
		female_names = { "Alice" }
	}
}
"#,
	)
	.expect("write cultures");

	let parsed = [parse_script_file(
		"1020",
		&mod_root,
		&mod_root
			.join("common")
			.join("cultures")
			.join("cultures.txt"),
	)
	.expect("parsed cultures")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/cultures/cultures.txt")
			&& reference.key == "culture_definition"
			&& reference.value == "coverage_culture"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "culture_definition" && reference.value == "coverage_group"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "culture_definition" && reference.value == "primary"
	}));
}

#[test]
fn government_names_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("government_names"))
		.expect("create government names");
	fs::write(
		mod_root
			.join("common")
			.join("government_names")
			.join("names.txt"),
		r#"
coverage_government_names = {
	trigger = {
		always = yes
	}
}
"#,
	)
	.expect("write government names");

	let parsed = [parse_script_file(
		"1014",
		&mod_root,
		&mod_root
			.join("common")
			.join("government_names")
			.join("names.txt"),
	)
	.expect("parsed government names")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/government_names/names.txt")
			&& reference.key == "government_name_definition"
			&& reference.value == "coverage_government_names"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "government_name_definition" && reference.value == "trigger"
	}));
}

#[test]
fn event_modifiers_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("event_modifiers"))
		.expect("create event modifiers");
	fs::write(
		mod_root
			.join("common")
			.join("event_modifiers")
			.join("modifiers.txt"),
		r#"
coverage_event_modifier = {
	trigger = {
		always = yes
	}
}
"#,
	)
	.expect("write event modifiers");

	let parsed = [parse_script_file(
		"1015",
		&mod_root,
		&mod_root
			.join("common")
			.join("event_modifiers")
			.join("modifiers.txt"),
	)
	.expect("parsed event modifiers")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/event_modifiers/modifiers.txt")
			&& reference.key == "event_modifier_definition"
			&& reference.value == "coverage_event_modifier"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "event_modifier_definition" && reference.value == "trigger"
	}));
}

#[test]
fn province_triggered_modifiers_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("province_triggered_modifiers"))
		.expect("create province triggered modifiers");
	fs::write(
		mod_root
			.join("common")
			.join("province_triggered_modifiers")
			.join("modifiers.txt"),
		r#"
coverage_ptm = {
	potential = {
		owner = {
			government = monarchy
		}
	}
	on_activation = {
		add_base_tax = 1
	}
}
"#,
	)
	.expect("write province triggered modifiers");

	let parsed = [parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("common")
			.join("province_triggered_modifiers")
			.join("modifiers.txt"),
	)
	.expect("parsed province triggered modifiers")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/province_triggered_modifiers/modifiers.txt")
			&& reference.key == "province_triggered_modifier_definition"
			&& reference.value == "coverage_ptm"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "province_triggered_modifier_definition" && reference.value == "potential"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "province_triggered_modifier_definition"
			&& reference.value == "on_activation"
	}));
}

#[test]
fn cb_types_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("cb_types")).expect("create cb types");
	fs::write(
		mod_root.join("common").join("cb_types").join("cb.txt"),
		r#"
coverage_cb = {
	can_use = {
		always = yes
	}
	can_take_province = {
		always = yes
	}
}
"#,
	)
	.expect("write cb types");

	let parsed = [parse_script_file(
		"1017",
		&mod_root,
		&mod_root.join("common").join("cb_types").join("cb.txt"),
	)
	.expect("parsed cb types")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/cb_types/cb.txt")
			&& reference.key == "cb_type_definition"
			&& reference.value == "coverage_cb"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "cb_type_definition" && reference.value == "can_use"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "cb_type_definition" && reference.value == "can_take_province"
	}));
}

#[test]
fn government_reforms_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("government_reforms"))
		.expect("create government reforms");
	fs::write(
		mod_root
			.join("common")
			.join("government_reforms")
			.join("reforms.txt"),
		r#"
test_reform = {
	ai_will_do = {
		factor = 1
	}
	modifiers = {
		global_tax_modifier = 0.1
	}
}
"#,
	)
	.expect("write government reforms");

	let parsed = [parse_script_file(
		"1018",
		&mod_root,
		&mod_root
			.join("common")
			.join("government_reforms")
			.join("reforms.txt"),
	)
	.expect("parsed government reforms")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/government_reforms/reforms.txt")
			&& reference.key == "government_reform_definition"
			&& reference.value == "test_reform"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "government_reform_definition" && reference.value == "ai_will_do"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "government_reform_definition" && reference.value == "modifiers"
	}));
}

#[test]
fn scripted_triggers_emit_top_level_definition_resources() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_triggers"))
		.expect("create scripted triggers");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_triggers")
			.join("triggers.txt"),
		r#"
eu4_cov_country_trigger = {
has_country_flag = eu4_cov_enabled
}

eu4_cov_province_trigger = {
owner = {
	limit = { eu4_cov_country_trigger = yes }
}
}
"#,
	)
	.expect("write scripted triggers");

	let parsed = [parse_script_file(
		"1012",
		&mod_root,
		&mod_root
			.join("common")
			.join("scripted_triggers")
			.join("triggers.txt"),
	)
	.expect("parsed scripted triggers")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/scripted_triggers/triggers.txt")
			&& reference.key == "scripted_trigger_definition"
			&& reference.value == "eu4_cov_country_trigger"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/scripted_triggers/triggers.txt")
			&& reference.key == "scripted_trigger_definition"
			&& reference.value == "eu4_cov_province_trigger"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "scripted_trigger_definition" && reference.value == "limit"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "scripted_trigger_definition" && reference.value == "owner"
	}));
}

#[test]
fn new_diplomatic_actions_emit_top_level_definition_resources_without_recording_static_containers()
{
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("new_diplomatic_actions"))
		.expect("create new diplomatic actions");
	fs::write(
		mod_root
			.join("common")
			.join("new_diplomatic_actions")
			.join("actions.txt"),
		r#"
static_actions = {
royal_marriage = {
	alert_index = 1
}
}

request_condottieri = {
is_visible = { always = yes }
on_accept = {
	add_favors = { who = FROM amount = -10 }
}
}
"#,
	)
	.expect("write actions");

	let parsed = [parse_script_file(
		"1009",
		&mod_root,
		&mod_root
			.join("common")
			.join("new_diplomatic_actions")
			.join("actions.txt"),
	)
	.expect("parsed actions")];
	let index = build_semantic_index(&parsed);

	assert!(index.resource_references.iter().any(|reference| {
		reference.path == std::path::Path::new("common/new_diplomatic_actions/actions.txt")
			&& reference.key == "new_diplomatic_action_definition"
			&& reference.value == "request_condottieri"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.key == "new_diplomatic_action_definition" && reference.value == "static_actions"
	}));
}

#[test]
fn full_body_scripted_effect_scope_params_resolve_nested_prev_aliases() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("new_diplomatic_actions"))
		.expect("create new diplomatic actions");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::write(
		mod_root
			.join("common")
			.join("new_diplomatic_actions")
			.join("actions.txt"),
		r#"
request_general = {
on_accept = {
	if = {
		limit = {
			FROM = {
				army_tradition = 10
			}
		}
		add_favors = { who = FROM amount = -50 }
		create_general_from_country = { who = FROM }
		FROM = {
			add_army_tradition = -10
		}
	}
}
}
"#,
	)
	.expect("write actions");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		full_army_tradition_switch_effect("create_general_from_country"),
	)
	.expect("write effects");

	let parsed = [
		parse_script_file(
			"1008",
			&mod_root,
			&mod_root
				.join("common")
				.join("new_diplomatic_actions")
				.join("actions.txt"),
		)
		.expect("parsed actions"),
		parse_script_file(
			"1008",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
		)
		.expect("parsed effects"),
	];
	let index = build_semantic_index(&parsed);
	let callable_scope_map = build_inferred_callable_scope_map(&index);
	let inferred_masks = collect_inferred_callable_masks(&index);
	let nested_prev_usage = index
		.alias_usages
		.iter()
		.find(|usage| {
			usage.alias == "PREV"
				&& usage.path.ends_with("common/scripted_effects/effects.txt")
				&& usage.line > 4
		})
		.expect("nested PREV alias usage");
	assert_ne!(
		effective_alias_scope_mask_with_overrides(
			&index,
			&callable_scope_map,
			&inferred_masks,
			nested_prev_usage.scope_id,
			"PREV",
		),
		0,
		"nested PREV aliases should no longer resolve to Unknown scope"
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001"
				&& finding
					.path
					.as_ref()
					.map(|path| path.ends_with("common/scripted_effects/effects.txt"))
					.unwrap_or(false)
		}),
		"full-body scripted effects should suppress PREV Unknown-scope noise"
	);
}

#[test]
fn cb_types_seed_country_aliases_and_trigger_scopes() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("cb_types")).expect("create cb types");
	fs::write(
		mod_root.join("common").join("cb_types").join("cb.txt"),
		r#"
cb_restore = {
prerequisites_self = {
	capital_scope = {
		owner = {
			government = monarchy
		}
		is_core = ROOT
	}
}
prerequisites = {
	FROM = {
		government = monarchy
	}
}
can_use = {
	ROOT = {
		legitimacy = 50
	}
}
}
"#,
	)
	.expect("write cb types");

	let parsed = [parse_script_file(
		"1006",
		&mod_root,
		&mod_root.join("common").join("cb_types").join("cb.txt"),
	)
	.expect("parsed cb types")];
	let index = build_semantic_index(&parsed);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001" && finding.path == Some("common/cb_types/cb.txt".into())
		}),
		"cb types should no longer keep ROOT/FROM/owner/capital_scope under Unknown scope"
	);
	assert!(
		!diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S002" && finding.path == Some("common/cb_types/cb.txt".into())
		}),
		"cb type trigger containers should not become scripted effect calls"
	);
}

#[test]
fn effect_context_selectors_do_not_become_scripted_effect_calls() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::create_dir_all(mod_root.join("map")).expect("create map");
	fs::write(
		mod_root.join("map").join("region.txt"),
		"hudson_bay_region = { areas = { north_bay_area } }\n",
	)
	.expect("write region");
	fs::write(
		mod_root.join("events").join("selectors.txt"),
		r#"
namespace = test
country_event = {
id = test.1
immediate = {
	random_list = {
		50 = {
			missing_weight_effect = { amount = 1 }
		}
	}
	every_country = {
		HBC = {
			add_prestige = 1
		}
		2022 = {
			add_core = HBC
		}
		random_country = {
			add_legitimacy = 1
		}
		every_known_country = {
			add_prestige = 1
		}
		every_subject_country = {
			add_stability = 1
		}
		hudson_bay_region = {
			add_permanent_claim = ROOT
		}
		overlord = {
			add_stability = 1
		}
		missing_effect = { amount = 1 }
	}
	random_owned_province = {
		add_base_tax = 1
	}
	random_province = {
		add_base_production = 1
	}
	every_province = {
		add_base_manpower = 1
	}
	while = {
		limit = { always = yes }
		missing_loop_effect = { amount = 1 }
	}
	IF = {
		limit = { always = yes }
		missing_if_effect = { amount = 1 }
	}
	ELSE_IF = {
		limit = { always = yes }
		missing_else_if_effect = { amount = 1 }
	}
}
}
"#,
	)
	.expect("write selectors");

	let parsed = [
		parse_script_file("1007", &mod_root, &mod_root.join("map").join("region.txt"))
			.expect("parsed region"),
		parse_script_file(
			"1007",
			&mod_root,
			&mod_root.join("events").join("selectors.txt"),
		)
		.expect("parsed selectors"),
	];
	let index = build_semantic_index(&parsed);
	for name in [
		"random_list",
		"every_country",
		"random_country",
		"every_known_country",
		"every_subject_country",
		"random_owned_province",
		"random_province",
		"every_province",
		"while",
		"IF",
		"ELSE_IF",
		"HBC",
		"2022",
		"hudson_bay_region",
		"overlord",
	] {
		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect && reference.name == name
			}),
			"{name} should not be recorded as a scripted effect reference"
		);
	}

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for name in [
		"random_list",
		"every_country",
		"random_country",
		"every_known_country",
		"every_subject_country",
		"random_owned_province",
		"random_province",
		"every_province",
		"while",
		"IF",
		"ELSE_IF",
		"HBC",
		"2022",
		"hudson_bay_region",
		"overlord",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
			"{name} should not produce S002"
		);
	}
	for name in [
		"missing_weight_effect",
		"missing_effect",
		"missing_loop_effect",
		"missing_if_effect",
		"missing_else_if_effect",
	] {
		assert!(
			diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) })
		);
	}
}

#[test]
fn on_actions_callbacks_seed_scopes_and_do_not_start_unknown() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("on_actions")).expect("create on_actions");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root.join("events").join("events.txt"),
		r#"
namespace = test
country_event = { id = test.1 }
country_event = { id = test.2 }
"#,
	)
	.expect("write events");
	fs::write(
		mod_root
			.join("common")
			.join("on_actions")
			.join("callbacks.txt"),
		r#"
on_adm_development = {
owner = {
	country_event = { id = test.1 }
}
random_owned_province = {
	missing_province_effect = { amount = 1 }
}
}
on_startup = {
country_event = { id = test.2 }
while = {
	limit = { always = yes }
	every_subject_country = {
		missing_country_effect = { amount = 1 }
	}
}
}
"#,
	)
	.expect("write on_actions");

	let parsed = [
		parse_script_file(
			"1011",
			&mod_root,
			&mod_root.join("events").join("events.txt"),
		)
		.expect("parsed events"),
		parse_script_file(
			"1011",
			&mod_root,
			&mod_root
				.join("common")
				.join("on_actions")
				.join("callbacks.txt"),
		)
		.expect("parsed on_actions"),
	];
	let index = build_semantic_index(&parsed);
	for name in [
		"on_adm_development",
		"on_startup",
		"random_owned_province",
		"while",
		"every_subject_country",
	] {
		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect && reference.name == name
			}),
			"{name} should not be recorded as a scripted effect reference"
		);
	}
	assert!(
		index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Effect && scope.this_type == ScopeType::Province)
	);
	assert!(
		index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Effect && scope.this_type == ScopeType::Country)
	);
	assert!(
		index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::AliasBlock
				&& scope.this_type == ScopeType::Country)
	);
	assert!(
		index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Province)
	);
	assert!(
		index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Country)
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001"
				&& finding.path == Some("common/on_actions/callbacks.txt".into())
		}),
		"on_actions callbacks should no longer start from Unknown scope"
	);
	for name in [
		"random_owned_province",
		"while",
		"every_subject_country",
		"country_event",
	] {
		assert!(
			!diagnostics.strict.iter().any(|finding| {
				finding.rule_id == "S002"
					&& finding.path == Some("common/on_actions/callbacks.txt".into())
					&& finding.message.contains(name)
			}),
			"{name} should not produce S002 in on_actions callbacks"
		);
	}
	for name in ["missing_province_effect", "missing_country_effect"] {
		assert!(diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S002"
				&& finding.path == Some("common/on_actions/callbacks.txt".into())
				&& finding.message.contains(name)
		}));
	}
}

#[test]
fn scripted_effect_param_contracts_reduce_s004_noise() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("contracts.txt"),
		r#"
ME_give_claims = {
add_prestige = 1
}
add_prestige_or_monarch_power = {
add_prestige = 1
}
country_event_with_option_insight = {
add_stability = 1
}
create_or_add_center_of_trade_level = {
add_base_production = 1
}
"#,
	)
	.expect("write scripted effects");
	fs::write(
		mod_root.join("events").join("contracts.txt"),
		r#"
namespace = test
country_event = {
id = test.1
immediate = {
	ME_give_claims = {
		area = baltic_area
	}
	ME_give_claims = {
		hidden_effect = {
			region = finland_area
		}
	}
	add_prestige_or_monarch_power = {
		value = 10
	}
	country_event_with_option_insight = {
		id = test.2
		option_3 = some_option
	}
	create_or_add_center_of_trade_level = {
		level = 2
	}
}
}
"#,
	)
	.expect("write events");

	let parsed = [
		parse_script_file(
			"1010",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("contracts.txt"),
		)
		.expect("parsed scripted effects"),
		parse_script_file(
			"1010",
			&mod_root,
			&mod_root.join("events").join("contracts.txt"),
		)
		.expect("parsed events"),
	];
	let index = build_semantic_index(&parsed);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	assert!(
		!diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S004" && finding.message.contains("缺失 area")
		}),
		"one-of contract should not expand into per-parameter missing messages"
	);
	assert_eq!(
		diagnostics
			.strict
			.iter()
			.filter(|finding| {
				finding.rule_id == "S004"
					&& finding
						.message
						.contains("ME_give_claims 至少需要一个参数: area|region|province|id")
			})
			.count(),
		1,
		"missing one-of params should aggregate into a single message"
	);
	for name in [
		"add_prestige_or_monarch_power",
		"country_event_with_option_insight",
		"create_or_add_center_of_trade_level",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S004" && finding.message.contains(name) }),
			"{name} should satisfy its explicit param contract"
		);
	}
}

#[test]
fn complex_dynamic_effect_contracts_keep_optional_slots_optional() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::create_dir_all(mod_root.join("missions")).expect("create missions");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("contracts.txt"),
		r#"
complex_dynamic_effect = {
custom_tooltip = $first_custom_tooltip$
if = {
	limit = { $first_limit$ }
}
tooltip = {
	$first_effect$
}
[[third_custom_tooltip]
custom_tooltip = $third_custom_tooltip$
if = {
	limit = { $third_limit$ }
}
tooltip = {
	$third_effect$
}
]
hidden_effect = {
	[[eigth_custom_tooltip]
	else_if = {
		limit = { $eigth_limit$ }
		$eigth_effect$
	}
	]
}
}

complex_dynamic_effect_without_alternative = {
custom_tooltip = $first_custom_tooltip$
if = {
	limit = { $first_limit$ }
}
tooltip = {
	$first_effect$
}
[[second_custom_tooltip]
custom_tooltip = $second_custom_tooltip$
if = {
	limit = { $second_limit$ }
}
tooltip = {
	$second_effect$
}
]
[[third_custom_tooltip]
custom_tooltip = $third_custom_tooltip$
if = {
	limit = { $third_limit$ }
}
tooltip = {
	$third_effect$
}
]
[[combined_effect]
if = {
	limit = {
		$first_limit$
		$second_limit$
		$third_limit$
		$eigth_limit$
	}
	$combined_effect$
}
]
hidden_effect = {
	[[eigth_custom_tooltip]
	if = {
		limit = { $eigth_limit$ }
		$eigth_effect$
	}
	]
}
}
"#,
	)
	.expect("write scripted effects");
	fs::write(
		mod_root.join("missions").join("contracts.txt"),
		r#"
test_mission = {
icon = mission_conquer_1_province
position = 1
effect = {
	complex_dynamic_effect = {
		first_custom_tooltip = TEST_DYNAMIC_EFFECT_1
		first_limit = "
			always = yes
		"
		first_effect = "
			add_prestige = 5
		"
	}
	complex_dynamic_effect = {
		first_custom_tooltip = TEST_DYNAMIC_EFFECT_2
		first_limit = "
			always = yes
		"
		first_effect = "
			add_prestige = 10
		"
		third_custom_tooltip = TEST_DYNAMIC_EFFECT_3
		third_limit = "
			always = yes
		"
		third_effect = "
			add_stability = 1
		"
	}
	complex_dynamic_effect_without_alternative = {
		first_custom_tooltip = TEST_DYNAMIC_EFFECT_WITHOUT_ALT_1
		first_limit = "
			always = yes
		"
		first_effect = "
			add_legitimacy = 10
		"
	}
	complex_dynamic_effect_without_alternative = {
		first_custom_tooltip = TEST_DYNAMIC_EFFECT_WITHOUT_ALT_2
		first_limit = "
			always = yes
		"
		first_effect = "
			add_meritocracy = 5
		"
		third_custom_tooltip = TEST_DYNAMIC_EFFECT_WITHOUT_ALT_3
		third_limit = "
			always = yes
		"
		third_effect = "
			add_treasury = 50
		"
	}
}
}
"#,
	)
	.expect("write missions");

	let parsed = [
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("contracts.txt"),
		)
		.expect("parsed scripted effects"),
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root.join("missions").join("contracts.txt"),
		)
		.expect("parsed missions"),
	];
	let index = build_semantic_index(&parsed);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	for name in [
		"complex_dynamic_effect",
		"complex_dynamic_effect_without_alternative",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S004" && finding.message.contains(name) }),
			"{name} should treat later dynamic slots as optional"
		);
	}
}

#[test]
fn diplomatic_actions_emit_top_level_definition_resources_without_treating_conditions_as_defs() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("diplomatic_actions"))
		.expect("create diplomatic actions");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::write(
		mod_root
			.join("common")
			.join("diplomatic_actions")
			.join("actions.txt"),
		r#"
milaccess = {
condition = {
	potential = { always = yes }
	allow = { always = yes }
}
effect = {
	grant_free_access = { who = FROM }
}
}
"#,
	)
	.expect("write diplomatic actions");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		r#"
grant_free_access = {
$who$ = {
	add_opinion = { who = ROOT modifier = granted_military_access }
}
}
"#,
	)
	.expect("write scripted effects");

	let parsed = [
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root
				.join("common")
				.join("diplomatic_actions")
				.join("actions.txt"),
		)
		.expect("parsed diplomatic actions"),
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
		)
		.expect("parsed scripted effects"),
	];
	let index = build_semantic_index(&parsed);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/diplomatic_actions/actions.txt")
			&& reference.key == "diplomatic_action_definition"
			&& reference.value == "milaccess"
	}));
	assert!(!index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/diplomatic_actions/actions.txt")
			&& reference.key == "diplomatic_action_definition"
			&& reference.value == "condition"
	}));
	assert!(index.references.iter().any(|reference| {
		reference.kind == SymbolKind::ScriptedEffect && reference.name == "grant_free_access"
	}));
}

#[test]
fn scripted_effect_named_param_bindings_satisfy_required_params() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::create_dir_all(mod_root.join("common").join("buildings")).expect("create buildings");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		r#"
update_improved_military_buildings_modifier = {
if = {
	tooltip = {
		add_province_modifier = {
			name = wei_suo_system_reform_$building$_modifier
			duration = -1
		}
	}
}
}
"#,
	)
	.expect("write scripted effects");
	fs::write(
		mod_root
			.join("common")
			.join("buildings")
			.join("buildings.txt"),
		r#"
barracks = {
on_built = {
	update_improved_military_buildings_modifier = {
		building = barracks
	}
}
}
"#,
	)
	.expect("write buildings");

	let parsed = [
		parse_script_file(
			"1011",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
		)
		.expect("parsed scripted effects"),
		parse_script_file(
			"1011",
			&mod_root,
			&mod_root
				.join("common")
				.join("buildings")
				.join("buildings.txt"),
		)
		.expect("parsed buildings"),
	];
	let index = build_semantic_index(&parsed);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path == Path::new("common/buildings/buildings.txt")
			&& reference.key == "building_definition"
			&& reference.value == "barracks"
	}));
	let reference = index
		.references
		.iter()
		.find(|reference| {
			reference.kind == SymbolKind::ScriptedEffect
				&& reference.name == "update_improved_military_buildings_modifier"
		})
		.expect("scripted effect reference");
	assert!(
		reference
			.provided_params
			.iter()
			.any(|param| param == "building"),
		"named building binding should count as a provided param"
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S004"
				&& finding
					.message
					.contains("update_improved_military_buildings_modifier")
		}),
		"named building binding should satisfy $building$"
	);
}

#[test]
fn second_wave_param_contracts_and_named_param_collection_work() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("contracts.txt"),
		r#"
add_age_modifier = {
add_country_modifier = {
	name = $name$
	duration = $duration$
	desc = ME_until_the_end_of_$age$
}
else = { [[else] $else$ ] }
}
country_event_with_effect_insight = {
country_event = {
	id = $id$
	[[days] days = $days$]
	[[random] random = $random$]
	[[tooltip] tooltip = $tooltip$]
}
tooltip = {
	$effect$
}
}
ME_distribute_development = {
while = {
	limit = { always = yes }
	random_owned_province = {
		[[limit] limit = { $limit$ }]
		add_base_$type$ = 1
	}
}
custom_tooltip = $type$_$amount$
[[tooltip] custom_tooltip = $tooltip$ ]
}
pick_best_provinces = {
pick_best_provinces_2 = {
	scope = "$scope$"
	scale = "$scale$"
	event_target_name = "$event_target_name$"
	global_trigger = "$global_trigger$"
	1 = "$1$"
	10 = "$10$"
}
}
ME_overlord_effect = {
overlord = {
	$effect$
}
}
create_general_with_pips = {
create_general = {
	tradition = $tradition$
	[[add_fire] add_fire = $add_fire$ ]
	[[culture] culture = $culture$ ]
}
}
"#,
	)
	.expect("write scripted effects");
	fs::write(
		mod_root.join("events").join("contracts.txt"),
		r#"
namespace = test
country_event = {
id = test.1
immediate = {
	add_age_modifier = {
		age = age_of_discovery
		name = test_modifier
		duration = 365
	}
	country_event_with_effect_insight = {
		id = test.2
		effect = { add_stability = 1 }
	}
	ME_distribute_development = {
		type = production
		amount = 5
	}
	pick_best_provinces = {
		scale = base_tax
		event_target_name = best_province
		global_trigger = always
		1 = always
		10 = never
		effect = { culture = ROOT }
	}
	ME_overlord_effect = {
		effect = { add_prestige = 1 }
	}
	create_general_with_pips = {
		tradition = 40
	}
}
}
"#,
	)
	.expect("write events");

	let parsed = [
		parse_script_file(
			"1010",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("contracts.txt"),
		)
		.expect("parsed scripted effects"),
		parse_script_file(
			"1010",
			&mod_root,
			&mod_root.join("events").join("contracts.txt"),
		)
		.expect("parsed events"),
	];
	let index = build_semantic_index(&parsed);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	let contract_findings: Vec<String> = diagnostics
		.strict
		.iter()
		.filter(|finding| finding.rule_id == "S004")
		.map(|finding| finding.message.clone())
		.collect();
	for snippet in [
		"add_age_modifier 缺失 else",
		"country_event_with_effect_insight 缺失 days",
		"country_event_with_effect_insight 缺失 tooltip",
		"ME_distribute_development 缺失 limit",
		"ME_distribute_development 缺失 tooltip",
		"pick_best_provinces 缺失 scope",
		"create_general_with_pips 缺失 add_fire",
		"create_general_with_pips 缺失 culture",
	] {
		assert!(
			!contract_findings
				.iter()
				.any(|message| message.contains(snippet)),
			"{snippet} should be optional"
		);
	}

	let pick_best_reference = index
		.references
		.iter()
		.find(|reference| {
			reference.kind == SymbolKind::ScriptedEffect && reference.name == "pick_best_provinces"
		})
		.expect("pick_best_provinces reference");
	for expected in ["scale", "event_target_name", "global_trigger", "1", "10"] {
		assert!(
			pick_best_reference
				.provided_params
				.iter()
				.any(|item| item == expected),
			"missing collected param {expected}"
		);
	}
	assert!(
		!pick_best_reference
			.provided_params
			.iter()
			.any(|item| item == "culture"),
		"nested keys should not be collected as provided params"
	);
}

#[test]
fn third_wave_estate_param_contracts_cover_common_wrappers() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("estate_contracts.txt"),
		r#"
take_estate_land_share_massive = {
estate = $estate$
amount = $amount$
}
add_estate_loyalty = {
estate = $estate$
short = $short$
amount = $amount$
}
estate_loyalty = {
estate = $estate$
loyalty = $loyalty$
}
estate_influence = {
estate = $estate$
influence = $influence$
}
"#,
	)
	.expect("write scripted effects");
	fs::write(
		mod_root.join("events").join("estate_contracts.txt"),
		r#"
namespace = test
country_event = {
id = test.3
immediate = {
	take_estate_land_share_massive = {
		estate = all
	}
	add_estate_loyalty = {
		estate = all
		short = yes
	}
	estate_loyalty = {
		estate = all
		loyalty = 50
	}
	estate_influence = {
		estate = all
		influence = 1
	}
}
}
"#,
	)
	.expect("write events");

	let parsed = [
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("estate_contracts.txt"),
		)
		.expect("parsed scripted effects"),
		parse_script_file(
			"1012",
			&mod_root,
			&mod_root.join("events").join("estate_contracts.txt"),
		)
		.expect("parsed events"),
	];
	let index = build_semantic_index(&parsed);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	for name in [
		"take_estate_land_share_massive",
		"add_estate_loyalty",
		"estate_loyalty",
		"estate_influence",
	] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S004" && finding.message.contains(name) }),
			"{name} should not produce S004"
		);
	}
}

#[test]
fn fourth_wave_param_contracts_cover_real_corpus_hotspots() {
	let s004_messages = fourth_wave_s004_messages(
		&["events", "fourth_wave_contracts.txt"],
		r#"
namespace = test
country_event = {
id = test.4
immediate = {
	unlock_estate_privilege = {
		estate_privilege = estate_church_anti_heresy_act
	}
	HAB_change_habsburg_glory = {
		remove = 20
	}
	HAB_change_habsburg_glory = {
		amount = 15
	}
	add_legitimacy_or_reform_progress = {
		amount = 25
	}
	add_legitimacy_or_reform_progress = {
		value = 10
	}
	EE_change_variable = {
		which = papal_authority_value
		divide = 2
	}
	EE_change_variable = {
		which = papal_authority_value
		multiply = 3
	}
	ME_tim_add_spoils_of_war = {
		add = 2
	}
	ME_tim_add_spoils_of_war = {
		remove = 1
	}
	ME_add_power_projection = {
		amount = 25
	}
	ME_add_power_projection = {
		value = 10
	}
	create_general_scaling_with_tradition_and_pips = { }
	create_general_scaling_with_tradition_and_pips = {
		add_shock = 1
		add_manuever = 1
	}
	ME_automatic_colonization_effect_module = {
		target_region_effect = autonomous_colonist_region_north_africa_colonizing_effect
		superregion = africa_superregion
	}
	ME_automatic_colonization_effect_module = {
		target_region_effect = autonomous_colonist_region_mexico_colonizing_effect
		region = colonial_mexico
	}
	country_event_with_insight = {
		id = test.6
		insight_tooltip = INSIGHT_JUST_TOOLTIP
	}
	country_event_with_insight = {
		id = test.7
		insight_tooltip = ENG_we_will_be_able_to_form
		effect_tooltip = "
			add_stability = 1
		"
	}
	define_and_hire_grand_vizier = {
		type = artist
	}
	define_and_hire_grand_vizier = {
		type = inquisitor
		age = 45
		religion = catholic
	}
	ME_override_country_name = {
		string = NED_united_provinces_name
	}
	ME_override_country_name = {
		name = Ducal_PRU
	}
	persia_indian_hegemony_decision_march_effect = {
		province = 563
		tag_1 = BNG
		tag_2 = TRT
		tag_3 = MKP
		trade_company_region = trade_company_east_india
	}
	persia_indian_hegemony_decision_coup_effect = {
		province = 563
		tag_1 = BNG
		tag_2 = TRT
		tag_3 = MKP
	}
	build_as_many_as_possible = {
		new_building = naval_battery
		upgrade_target = coastal_defence
		pick_best_function = pick_best_navydef_province
		cost = 1
		speed = 1
	}
	give_claims = {
		area = austria_area
	}
	give_claims = {
		id = 134
	}
	pick_best_tags = {
		scale = total_development
		event_target_name = claim_target
		global_trigger = "tag = HAB"
	}
	pick_best_tags = {
		scope = every_country
		scale = total_development
		event_target_name = scoped_claim_target
		global_trigger = "tag = HAB"
		1 = yes
		2 = yes
	}
	ME_add_years_of_trade_income = {
		value = 1
	}
	ME_add_years_of_trade_income = {
		years = 5
	}
}
}
"#,
	);

	for name in [
		"unlock_estate_privilege",
		"HAB_change_habsburg_glory",
		"add_legitimacy_or_reform_progress",
		"EE_change_variable",
		"ME_tim_add_spoils_of_war",
		"ME_add_power_projection",
		"create_general_scaling_with_tradition_and_pips",
		"ME_automatic_colonization_effect_module",
		"country_event_with_insight",
		"define_and_hire_grand_vizier",
		"ME_override_country_name",
		"persia_indian_hegemony_decision_march_effect",
		"persia_indian_hegemony_decision_coup_effect",
		"build_as_many_as_possible",
		"give_claims",
		"pick_best_tags",
		"ME_add_years_of_trade_income",
	] {
		assert!(
			!s004_messages.iter().any(|message| message.contains(name)),
			"{name} should not produce S004: {s004_messages:?}"
		);
	}
}

#[test]
fn fourth_wave_param_contracts_preserve_required_and_one_of_constraints() {
	let s004_messages = fourth_wave_s004_messages(
		&["events", "fourth_wave_contract_failures.txt"],
		r#"
namespace = test
country_event = {
id = test.5
immediate = {
	unlock_estate_privilege = { }
	HAB_change_habsburg_glory = { }
	add_legitimacy_or_reform_progress = { }
	EE_change_variable = {
		which = papal_authority_value
	}
	EE_change_variable = {
		add = 5
	}
	ME_tim_add_spoils_of_war = { }
	ME_add_power_projection = { }
	ME_automatic_colonization_effect_module = {
		target_region_effect = autonomous_colonist_region_north_africa_colonizing_effect
	}
	ME_automatic_colonization_effect_module = {
		region = colonial_mexico
	}
	country_event_with_insight = {
		id = test.6
	}
	country_event_with_insight = {
		insight_tooltip = INSIGHT_JUST_TOOLTIP
	}
	define_and_hire_grand_vizier = { }
	ME_override_country_name = { }
	persia_indian_hegemony_decision_march_effect = {
		province = 563
		trade_company_region = trade_company_east_india
	}
	persia_indian_hegemony_decision_march_effect = {
		tag_1 = BNG
		trade_company_region = trade_company_east_india
	}
	persia_indian_hegemony_decision_march_effect = {
		province = 563
		tag_1 = BNG
	}
	persia_indian_hegemony_decision_coup_effect = {
		province = 563
	}
	persia_indian_hegemony_decision_coup_effect = {
		tag_1 = BNG
	}
	build_as_many_as_possible = {
		new_building = naval_battery
		upgrade_target = coastal_defence
		cost = 1
		speed = 1
	}
	give_claims = { }
	pick_best_tags = {
		event_target_name = claim_target
		global_trigger = "tag = HAB"
	}
	pick_best_tags = {
		scale = total_development
		global_trigger = "tag = HAB"
	}
	pick_best_tags = {
		scale = total_development
		event_target_name = claim_target
	}
	ME_add_years_of_trade_income = { }
}
}
"#,
	);

	for snippet in [
		"unlock_estate_privilege 缺失 estate_privilege",
		"HAB_change_habsburg_glory 至少需要一个参数: amount|remove",
		"add_legitimacy_or_reform_progress 至少需要一个参数: amount|value",
		"EE_change_variable 至少需要一个参数: add|subtract|divide|multiply",
		"EE_change_variable 缺失 which",
		"ME_tim_add_spoils_of_war 至少需要一个参数: add|remove",
		"ME_add_power_projection 至少需要一个参数: amount|value",
		"ME_automatic_colonization_effect_module 至少需要一个参数: region|superregion",
		"ME_automatic_colonization_effect_module 缺失 target_region_effect",
		"country_event_with_insight 缺失 insight_tooltip",
		"country_event_with_insight 缺失 id",
		"define_and_hire_grand_vizier 缺失 type",
		"ME_override_country_name 至少需要一个参数: country_name|name|country|value|string",
		"persia_indian_hegemony_decision_march_effect 缺失 province",
		"persia_indian_hegemony_decision_march_effect 缺失 tag_1",
		"persia_indian_hegemony_decision_march_effect 缺失 trade_company_region",
		"persia_indian_hegemony_decision_coup_effect 缺失 province",
		"persia_indian_hegemony_decision_coup_effect 缺失 tag_1",
		"build_as_many_as_possible 缺失 pick_best_function",
		"give_claims 至少需要一个参数: area|region|province|id",
		"pick_best_tags 缺失 scale",
		"pick_best_tags 缺失 event_target_name",
		"pick_best_tags 缺失 global_trigger",
		"ME_add_years_of_trade_income 至少需要一个参数: years|value|amount",
	] {
		assert!(
			s004_messages
				.iter()
				.any(|message| message.contains(snippet)),
			"missing expected S004 snippet {snippet}: {s004_messages:?}"
		);
	}
}

#[test]
fn for_control_flow_does_not_emit_s002_or_s004() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root.join("events").join("for_control.txt"),
		r#"
namespace = test
country_event = {
id = test.1
immediate = {
	for = {
		amount = 3
		effect = {
			missing_effect = { FLAG = TEST }
		}
	}
}
}
"#,
	)
	.expect("write events");

	let parsed = [parse_script_file(
		"1011",
		&mod_root,
		&mod_root.join("events").join("for_control.txt"),
	)
	.expect("parsed events")];
	let index = build_semantic_index(&parsed);
	assert!(
		!index.references.iter().any(|reference| {
			reference.kind == SymbolKind::ScriptedEffect && reference.name == "for"
		}),
		"for should not be recorded as a scripted effect reference"
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.strict.iter().any(|finding| {
			(finding.rule_id == "S002" || finding.rule_id == "S004")
				&& finding.message.contains("for")
		}),
		"for control flow should not produce S002 or S004"
	);
	assert!(diagnostics.strict.iter().any(|finding| {
		finding.rule_id == "S002" && finding.message.contains("missing_effect")
	}));
}

#[test]
fn wrapper_heavy_scripted_effects_infer_scope_without_callers() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("wrappers.txt"),
		r#"
country_wrapper = {
owner = {
	add_prestige = 1
}
}

province_wrapper = {
capital_scope = {
	add_base_tax = 1
}
}

shared_wrapper = {
owner = {
	add_prestige = 1
}
capital_scope = {
	add_base_tax = 1
}
}
"#,
	)
	.expect("write scripted effects");

	let parsed = [parse_script_file(
		"1010",
		&mod_root,
		&mod_root
			.join("common")
			.join("scripted_effects")
			.join("wrappers.txt"),
	)
	.expect("parsed scripted effects")];
	let index = build_semantic_index(&parsed);

	let mut inferred = std::collections::HashMap::new();
	let mut inferred_masks = std::collections::HashMap::new();
	for definition in &index.definitions {
		if definition.kind == SymbolKind::ScriptedEffect {
			inferred.insert(definition.local_name.clone(), definition.inferred_this_type);
			inferred_masks.insert(definition.local_name.clone(), definition.inferred_this_mask);
		}
	}

	assert_eq!(inferred.get("country_wrapper"), Some(&ScopeType::Country));
	assert_eq!(inferred.get("province_wrapper"), Some(&ScopeType::Province));
	assert_eq!(inferred.get("shared_wrapper"), Some(&ScopeType::Unknown));
	assert_eq!(inferred_masks.get("country_wrapper"), Some(&0b01));
	assert_eq!(inferred_masks.get("province_wrapper"), Some(&0b10));
	assert_eq!(inferred_masks.get("shared_wrapper"), Some(&0b11));

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001"
				&& finding.path == Some("common/scripted_effects/wrappers.txt".into())
		}),
		"wrapper-heavy scripted effects should not stay unknown"
	);
}

#[test]
fn province_id_selectors_seed_province_scope_in_trigger_contexts() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("province_ids.txt"),
		r#"
check_subject_monuments = {
hidden_effect = {
	if = {
		limit = {
			1775 = {
				owner = {
					has_country_flag = test_flag
				}
			}
		}
	}
}
}
"#,
	)
	.expect("write scripted effects");

	let parsed = [parse_script_file(
		"1009",
		&mod_root,
		&mod_root
			.join("common")
			.join("scripted_effects")
			.join("province_ids.txt"),
	)
	.expect("parsed scripted effects")];
	let index = build_semantic_index(&parsed);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001"
				&& finding.path == Some("common/scripted_effects/province_ids.txt".into())
		}),
		"province id selector should seed Province scope for nested owner blocks"
	);
}

#[test]
fn scripted_effect_scope_inference_reaches_fixpoint() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		r#"
country_wrapper = {
conflict = { FLAG = TEST }
}

province_wrapper = {
chain_a = { FLAG = TEST }
conflict = { FLAG = TEST }
}

chain_a = {
chain_b = { FLAG = TEST }
}

chain_b = {
owner = {
	add_prestige = 1
}
}

conflict = {
owner = {
	add_prestige = 1
}
capital_scope = {
	add_base_tax = 1
}
}
"#,
	)
	.expect("write scripted effects");
	fs::write(
		mod_root.join("events").join("event.txt"),
		r#"
namespace = test
country_event = {
id = test.1
immediate = {
	country_wrapper = { FLAG = TEST }
	capital_scope = {
		province_wrapper = { FLAG = TEST }
	}
}
}
"#,
	)
	.expect("write event");

	let parsed = [
		parse_script_file(
			"1008",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
		)
		.expect("parsed scripted effects"),
		parse_script_file(
			"1008",
			&mod_root,
			&mod_root.join("events").join("event.txt"),
		)
		.expect("parsed event"),
	];
	let index = build_semantic_index(&parsed);

	let mut inferred = std::collections::HashMap::new();
	let mut inferred_masks = std::collections::HashMap::new();
	for definition in &index.definitions {
		if definition.kind == SymbolKind::ScriptedEffect {
			inferred.insert(definition.local_name.clone(), definition.inferred_this_type);
			inferred_masks.insert(definition.local_name.clone(), definition.inferred_this_mask);
		}
	}
	assert_eq!(inferred.get("country_wrapper"), Some(&ScopeType::Country));
	assert_eq!(inferred.get("province_wrapper"), Some(&ScopeType::Province));
	assert_eq!(inferred.get("chain_a"), Some(&ScopeType::Province));
	assert_eq!(inferred.get("chain_b"), Some(&ScopeType::Province));
	assert_eq!(inferred.get("conflict"), Some(&ScopeType::Unknown));
	assert_eq!(inferred_masks.get("country_wrapper"), Some(&0b01));
	assert_eq!(inferred_masks.get("province_wrapper"), Some(&0b10));
	assert_eq!(inferred_masks.get("chain_a"), Some(&0b10));
	assert_eq!(inferred_masks.get("chain_b"), Some(&0b10));
	assert_eq!(inferred_masks.get("conflict"), Some(&0b11));

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001"
				&& finding.path == Some("common/scripted_effects/effects.txt".into())
				&& finding.line == Some(14)
		}),
		"chain_b owner scope should resolve to Province after fixpoint inference"
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001"
				&& finding.path == Some("common/scripted_effects/effects.txt".into())
		}),
		"mixed scripted effects should stay usable via mask-aware A001 checks"
	);
}

#[test]
fn scripted_triggers_build_definitions_and_propagate_scope_masks() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_triggers"))
		.expect("create scripted triggers");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_triggers")
			.join("triggers.txt"),
		r#"
province_only = {
owner = {
	has_country_flag = seen
}
}

mixed_trigger = {
owner = {
	has_country_flag = seen
}
capital_scope = {
	has_province_flag = seen
}
}
"#,
	)
	.expect("write scripted triggers");
	fs::write(
		mod_root.join("events").join("events.txt"),
		r#"
namespace = test
country_event = {
id = test.1
trigger = {
	mixed_trigger = yes
}
}

province_event = {
id = test.2
trigger = {
	mixed_trigger = yes
	province_only = yes
}
}
"#,
	)
	.expect("write events");

	let parsed = [
		parse_script_file(
			"1009",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_triggers")
				.join("triggers.txt"),
		)
		.expect("parsed scripted triggers"),
		parse_script_file(
			"1009",
			&mod_root,
			&mod_root.join("events").join("events.txt"),
		)
		.expect("parsed events"),
	];
	let index = build_semantic_index(&parsed);

	let scripted_trigger_defs: std::collections::HashMap<_, _> = index
		.definitions
		.iter()
		.filter(|definition| definition.kind == SymbolKind::ScriptedTrigger)
		.map(|definition| (definition.local_name.as_str(), definition))
		.collect();
	assert_eq!(
		scripted_trigger_defs
			.get("province_only")
			.map(|d| d.inferred_this_mask),
		Some(0b10)
	);
	assert_eq!(
		scripted_trigger_defs
			.get("province_only")
			.map(|d| d.inferred_this_type),
		Some(ScopeType::Province)
	);
	assert_eq!(
		scripted_trigger_defs
			.get("mixed_trigger")
			.map(|d| d.inferred_this_mask),
		Some(0b11)
	);
	assert_eq!(
		scripted_trigger_defs
			.get("mixed_trigger")
			.map(|d| d.inferred_this_type),
		Some(ScopeType::Unknown)
	);

	for name in ["province_only", "mixed_trigger"] {
		assert!(
			index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedTrigger && reference.name == name
			}),
			"{name} should be recorded as a scripted trigger reference"
		);
	}

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for name in ["province_only", "mixed_trigger"] {
		assert!(
			!diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
			"{name} should not participate in scripted-effect unresolved-call reporting"
		);
	}
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001"
				&& finding.path == Some("common/scripted_triggers/triggers.txt".into())
		}),
		"scripted triggers should use propagated masks for owner/capital_scope checks"
	);
}

#[test]
fn generic_fallback_requires_explicit_effect_scope() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root.join("events").join("fallback.txt"),
		r#"
namespace = test
country_event = {
id = test.1
stray_container = {
	missing_outer_effect = { amount = 1 }
}
immediate = {
	missing_inner_effect = { amount = 1 }
	hidden_effect = {
		missing_hidden_effect = { amount = 1 }
	}
}
option = {
	name = ok
	missing_option_effect = { amount = 1 }
}
}
"#,
	)
	.expect("write file");

	let parsed = parse_script_file(
		"1003",
		&mod_root,
		&mod_root.join("events").join("fallback.txt"),
	)
	.expect("parsed event");
	let index = build_semantic_index(&[parsed]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	assert!(
		!index.references.iter().any(|reference| {
			reference.kind == SymbolKind::ScriptedEffect && reference.name == "missing_outer_effect"
		}),
		"generic block inside an event should not trigger scripted-effect fallback"
	);
	for name in [
		"missing_inner_effect",
		"missing_hidden_effect",
		"missing_option_effect",
	] {
		assert!(
			index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect && reference.name == name
			}),
			"{name} should still be recorded inside an explicit effect-ish scope"
		);
		assert!(
			diagnostics
				.strict
				.iter()
				.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
			"{name} should still report unresolved scripted-effect usage"
		);
	}
	assert!(
		!diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S002" && finding.message.contains("missing_outer_effect")
		}),
		"generic block inside an event should not report S002"
	);
}

#[test]
fn extractor_for_returns_none_for_families_without_extractors() {
	use super::super::content_family::{
		ConflictPolicy, ContentFamilyCapabilities, ContentFamilyDescriptor,
		ContentFamilyPathMatcher, MergePolicies, ModuleNameRule,
	};
	use super::extractors;
	use foch_core::model::ScopeType;
	let descriptor = ContentFamilyDescriptor {
		id: "unregistered_test_family",
		matcher: ContentFamilyPathMatcher::Prefix("unregistered_test_family/"),
		script_file_kind: ScriptFileKind::Events,
		module_name_rule: ModuleNameRule::Static("events"),
		scope_policy: super::super::content_family::ContentFamilyScopePolicy {
			root_scope: ScopeType::Unknown,
			from_alias: None,
			dynamic_scope: false,
		},
		capabilities: ContentFamilyCapabilities::default(),
		extractor: super::super::content_family::ContentFamilyExtractor::None,
		merge_key_source: None,
		conflict_policy: ConflictPolicy::default(),
		merge_policies: MergePolicies::default(),
	};
	assert!(extractors::extractor_for(&descriptor).is_none());
}

#[test]
fn extractor_for_returns_some_for_registered_families() {
	use super::super::content_family::GameProfile;
	use super::super::eu4_profile::eu4_profile;
	use super::extractors;
	let profile = eu4_profile();
	let descriptor = profile
		.classify_content_family(std::path::Path::new("common/fervor/test.txt"))
		.expect("fervor family");
	assert!(extractors::extractor_for(descriptor).is_some());
}

#[test]
fn named_definition_table_extractor_emits_definition_reference() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("fervor")).expect("create fervor");
	fs::write(
		mod_root.join("common").join("fervor").join("test.txt"),
		"test_fervor_aspect = {\n\tgfx = test_gfx\n}\n",
	)
	.expect("write fervor");
	let parsed = [parse_script_file(
		"1000",
		&mod_root,
		&mod_root.join("common").join("fervor").join("test.txt"),
	)
	.expect("parsed")];
	let index = build_semantic_index(&parsed);
	assert!(
		index
			.resource_references
			.iter()
			.any(|r| r.key == "fervor_definition" && r.value == "test_fervor_aspect"),
		"should emit fervor_definition reference"
	);
	assert!(
		index
			.resource_references
			.iter()
			.any(|r| r.key == "gfx" && r.value == "test_gfx"),
		"should emit gfx scalar reference"
	);
}

#[test]
fn top_level_named_block_extractor_emits_localisation_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("church_aspects")).expect("create dir");
	fs::write(
		mod_root
			.join("common")
			.join("church_aspects")
			.join("test.txt"),
		"test_aspect = {\n\teffect = { add_stability = 1 }\n}\n",
	)
	.expect("write");
	let parsed = [parse_script_file(
		"1000",
		&mod_root,
		&mod_root
			.join("common")
			.join("church_aspects")
			.join("test.txt"),
	)
	.expect("parsed")];
	let index = build_semantic_index(&parsed);
	assert!(
		index
			.resource_references
			.iter()
			.any(|r| r.key == "localisation" && r.value == "test_aspect"),
		"should emit localisation key"
	);
	assert!(
		index
			.resource_references
			.iter()
			.any(|r| r.key == "localisation_desc" && r.value == "desc_test_aspect"),
		"should emit localisation_desc with prefix"
	);
	assert!(
		index
			.resource_references
			.iter()
			.any(|r| r.key == "localisation_modifier" && r.value == "test_aspect_modifier"),
		"should emit localisation_modifier with suffix"
	);
}

#[test]
fn triggered_modifiers_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("triggered_modifiers"))
		.expect("create triggered modifiers");
	fs::write(
		mod_root
			.join("common")
			.join("triggered_modifiers")
			.join("00_triggered_modifiers.txt"),
		r#"
legitimate_ruler = {
	potential = {
		has_dlc = "Res Publica"
		government = republic
		NOT = { has_reform = dutch_republic }
	}
	trigger = {
		republican_tradition = 90
	}
	legitimacy = 1
	republican_tradition = 0.5
}

march_modifier = {
	potential = {
		is_march = yes
	}
	trigger = {
		always = yes
	}
	land_morale = 0.15
	fort_defense = 0.15
	manpower_recovery_speed = 0.30
}
"#,
	)
	.expect("write triggered modifiers");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("common")
			.join("triggered_modifiers")
			.join("00_triggered_modifiers.txt"),
	)
	.expect("parsed triggered modifiers");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/triggered_modifiers/00_triggered_modifiers.txt")
				&& reference.key == "triggered_modifier_definition"
				&& reference.value == "legitimate_ruler"
		}),
		"should capture legitimate_ruler as a triggered modifier definition"
	);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/triggered_modifiers/00_triggered_modifiers.txt")
				&& reference.key == "triggered_modifier_definition"
				&& reference.value == "march_modifier"
		}),
		"should capture march_modifier as a triggered modifier definition"
	);
}

#[test]
fn scripted_effects_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
		.expect("create scripted effects");
	fs::write(
		mod_root
			.join("common")
			.join("scripted_effects")
			.join("00_scripted_effects.txt"),
		r#"
country_event_effect = {
	random_owned_province = {
		limit = {
			is_core = ROOT
		}
		add_base_tax = 1
	}
}

add_age_modifier_effect = {
	if = {
		limit = { has_dlc = "Mandate of Heaven" }
		add_age_modifier = {
			name = $name$
			duration = $duration$
		}
	}
}
"#,
	)
	.expect("write scripted effects");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("common")
			.join("scripted_effects")
			.join("00_scripted_effects.txt"),
	)
	.expect("parsed scripted effects");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/scripted_effects/00_scripted_effects.txt")
				&& reference.key == "scripted_effect_definition"
				&& reference.value == "country_event_effect"
		}),
		"should capture country_event_effect as a scripted effect definition"
	);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/scripted_effects/00_scripted_effects.txt")
				&& reference.key == "scripted_effect_definition"
				&& reference.value == "add_age_modifier_effect"
		}),
		"should capture add_age_modifier_effect as a scripted effect definition"
	);
}

#[test]
fn defines_have_no_extractor_and_no_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("defines")).expect("create defines");
	fs::write(
		mod_root
			.join("common")
			.join("defines")
			.join("00_defines.txt"),
		r#"
NCountry = {
	KARMA_INCREASE_PEACE = 1
	BASE_TAX_COST = 1
}
"#,
	)
	.expect("write defines");

	use super::super::content_family::GameProfile;
	use super::super::eu4_profile::eu4_profile;
	use super::extractors;
	let profile = eu4_profile();
	let descriptor = profile
		.classify_content_family(std::path::Path::new("common/defines/00_defines.txt"))
		.expect("defines family");
	assert!(
		extractors::extractor_for(descriptor).is_none(),
		"common/defines should have no registered extractor (files are .lua)"
	);

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("common")
			.join("defines")
			.join("00_defines.txt"),
	)
	.expect("parser still returns a result for .txt content");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index
			.resource_references
			.iter()
			.all(|r| r.key != "defines_definition"),
		"should not emit any defines_definition resource references"
	);
}

#[test]
fn customizable_localization_records_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("customizable_localization"))
		.expect("create customizable localization");
	fs::write(
		mod_root
			.join("customizable_localization")
			.join("00_custom_loc.txt"),
		r#"
defined_text = {
	name = get_advisor_title
	text = {
		trigger = {
			is_female = yes
		}
		localisation_key = advisor_female
	}
	text = {
		localisation_key = advisor_male
	}
}
"#,
	)
	.expect("write customizable localization");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("customizable_localization")
			.join("00_custom_loc.txt"),
	)
	.expect("parsed customizable localization");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("customizable_localization/00_custom_loc.txt")
				&& reference.key == "customizable_localization_definition"
				&& reference.value == "defined_text"
		}),
		"should capture defined_text as a customizable localization definition"
	);
}

#[test]
fn events_extractor_is_registered() {
	use super::super::content_family::GameProfile;
	use super::super::eu4_profile::eu4_profile;
	use super::extractors;
	let profile = eu4_profile();
	let descriptor = profile
		.classify_content_family(std::path::Path::new("events/FlavorFRA.txt"))
		.expect("events family");
	assert!(
		extractors::extractor_for(descriptor).is_some(),
		"events family should have a registered extractor"
	);
}

#[test]
fn events_decisions_extractor_is_registered() {
	use super::super::content_family::GameProfile;
	use super::super::eu4_profile::eu4_profile;
	use super::extractors;
	let profile = eu4_profile();
	let descriptor = profile
		.classify_content_family(std::path::Path::new("events/decisions/00_decisions.txt"))
		.expect("events/decisions family");
	assert_eq!(descriptor.id, "events/decisions");
	assert!(
		extractors::extractor_for(descriptor).is_some(),
		"events/decisions family should have a registered extractor"
	);
}

#[test]
fn decisions_extractor_is_registered() {
	use super::super::content_family::GameProfile;
	use super::super::eu4_profile::eu4_profile;
	use super::extractors;
	let profile = eu4_profile();
	let descriptor = profile
		.classify_content_family(std::path::Path::new("decisions/00_decisions.txt"))
		.expect("decisions family");
	assert_eq!(descriptor.id, "decisions");
	assert!(
		extractors::extractor_for(descriptor).is_some(),
		"decisions family should have a registered extractor"
	);
}

#[test]
fn events_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root.join("events").join("FlavorFRA.txt"),
		r#"
namespace = flavor_fra

country_event = {
	id = flavor_fra.9100
	title = "flavor_fra.EVTNAME9100"
	trigger = { tag = FRA }
	option = { name = "flavor_fra.EVTOPT9100A" }
}

province_event = {
	id = flavor_fra.9200
	trigger = { always = yes }
	option = { name = "OK" }
}
"#,
	)
	.expect("write events");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root.join("events").join("FlavorFRA.txt"),
	)
	.expect("parsed events");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|r| {
			r.path == Path::new("events/FlavorFRA.txt")
				&& r.key == "event_namespace"
				&& r.value == "flavor_fra"
		}),
		"should capture namespace as event_namespace"
	);
	assert!(
		index.resource_references.iter().any(|r| {
			r.path == Path::new("events/FlavorFRA.txt")
				&& r.key == "event_definition"
				&& r.value == "flavor_fra.9100"
		}),
		"should capture country_event id as event_definition"
	);
	assert!(
		index.resource_references.iter().any(|r| {
			r.path == Path::new("events/FlavorFRA.txt")
				&& r.key == "event_definition"
				&& r.value == "flavor_fra.9200"
		}),
		"should capture province_event id as event_definition"
	);
}

#[test]
fn decisions_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("decisions")).expect("create decisions");
	fs::write(
		mod_root.join("decisions").join("00_decisions.txt"),
		r#"
country_decisions = {
	restore_roman_empire = {
		major = yes
		potential = { tag = ROM }
		allow = { is_at_war = no }
		effect = { add_prestige = 100 }
	}
	form_prussia = {
		potential = { tag = BRA }
		allow = { is_at_war = no }
		effect = { change_tag = PRU }
	}
}
"#,
	)
	.expect("write decisions");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root.join("decisions").join("00_decisions.txt"),
	)
	.expect("parsed decisions");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|r| {
			r.path == Path::new("decisions/00_decisions.txt")
				&& r.key == "decision_definition"
				&& r.value == "restore_roman_empire"
		}),
		"should capture restore_roman_empire as decision_definition"
	);
	assert!(
		index.resource_references.iter().any(|r| {
			r.path == Path::new("decisions/00_decisions.txt")
				&& r.key == "decision_definition"
				&& r.value == "form_prussia"
		}),
		"should capture form_prussia as decision_definition"
	);
}

#[test]
fn missions_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("missions")).expect("create missions");
	fs::write(
		mod_root.join("missions").join("FRA_Missions.txt"),
		r#"
french_missions_1 = {
	slot = 1
	generic = no
	ai = yes
	potential = {
		tag = FRA
		NOT = { map_setup = map_setup_random }
	}

	french_grand_army = {
		icon = mission_assemble_an_army
		required_missions = { }
		trigger = {
			army_size = 80
		}
		effect = {
			add_country_modifier = {
				name = "grand_army"
				duration = 7300
			}
		}
	}
}

french_missions_2 = {
	slot = 2
	generic = no
	ai = yes
	potential = {
		tag = FRA
	}
}
"#,
	)
	.expect("write missions");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root.join("missions").join("FRA_Missions.txt"),
	)
	.expect("parsed missions");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("missions/FRA_Missions.txt")
				&& reference.key == "mission_definition"
				&& reference.value == "french_missions_1"
		}),
		"should capture french_missions_1 as a mission definition"
	);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("missions/FRA_Missions.txt")
				&& reference.key == "mission_definition"
				&& reference.value == "french_missions_2"
		}),
		"should capture french_missions_2 as a mission definition"
	);
}

#[test]
fn on_actions_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("on_actions")).expect("create on actions");
	fs::write(
		mod_root
			.join("common")
			.join("on_actions")
			.join("00_on_actions.txt"),
		r#"
on_battle_won = {
	random_events = {
		100 = battle_event.1
	}
	events = {
		flavor_fra.9401
	}
}

on_startup = {
	events = {
		startup.1
		startup.2
	}
}
"#,
	)
	.expect("write on actions");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("common")
			.join("on_actions")
			.join("00_on_actions.txt"),
	)
	.expect("parsed on actions");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/on_actions/00_on_actions.txt")
				&& reference.key == "on_action_definition"
				&& reference.value == "on_battle_won"
		}),
		"should capture on_battle_won as an on_action definition"
	);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/on_actions/00_on_actions.txt")
				&& reference.key == "on_action_definition"
				&& reference.value == "on_startup"
		}),
		"should capture on_startup as an on_action definition"
	);
}

#[test]
fn events_common_on_actions_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("events").join("common").join("on_actions"))
		.expect("create events/common/on_actions");
	fs::write(
		mod_root
			.join("events")
			.join("common")
			.join("on_actions")
			.join("00_on_actions.txt"),
		r#"
on_war_declared = {
	events = {
		flavor_war.1
	}
}

on_peace_signed = {
	events = {
		flavor_peace.1
	}
}
"#,
	)
	.expect("write events/common/on_actions");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("events")
			.join("common")
			.join("on_actions")
			.join("00_on_actions.txt"),
	)
	.expect("parsed events/common/on_actions");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("events/common/on_actions/00_on_actions.txt")
				&& reference.key == "on_action_definition"
				&& reference.value == "on_war_declared"
		}),
		"should capture on_war_declared via events/common/on_actions family"
	);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("events/common/on_actions/00_on_actions.txt")
				&& reference.key == "on_action_definition"
				&& reference.value == "on_peace_signed"
		}),
		"should capture on_peace_signed via events/common/on_actions family"
	);
}

#[test]
fn events_common_new_diplomatic_actions_record_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(
		mod_root
			.join("events")
			.join("common")
			.join("new_diplomatic_actions"),
	)
	.expect("create events/common/new_diplomatic_actions");
	fs::write(
		mod_root
			.join("events")
			.join("common")
			.join("new_diplomatic_actions")
			.join("01_diplomatic_actions.txt"),
		r#"
static_actions = {
	request_condottieri
	knowledge_sharing
}

request_condottieri = {
	category = 1
	require_acceptance = yes
	is_visible = {
		has_dlc = "Mare Nostrum"
	}
	on_accept = {
		add_trust = {
			who = FROM
			value = 10
		}
	}
}

knowledge_sharing = {
	category = 1
	require_acceptance = yes
	is_visible = {
		has_dlc = "Rule Britannia"
	}
}
"#,
	)
	.expect("write new diplomatic actions");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("events")
			.join("common")
			.join("new_diplomatic_actions")
			.join("01_diplomatic_actions.txt"),
	)
	.expect("parsed new diplomatic actions");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path
				== Path::new("events/common/new_diplomatic_actions/01_diplomatic_actions.txt")
				&& reference.key == "new_diplomatic_action_definition"
				&& reference.value == "request_condottieri"
		}),
		"should capture request_condottieri as a new diplomatic action"
	);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path
				== Path::new("events/common/new_diplomatic_actions/01_diplomatic_actions.txt")
				&& reference.key == "new_diplomatic_action_definition"
				&& reference.value == "knowledge_sharing"
		}),
		"should capture knowledge_sharing as a new diplomatic action"
	);
	assert!(
		!index
			.resource_references
			.iter()
			.any(|reference| { reference.value == "static_actions" }),
		"static_actions block should be excluded by NewDiplomaticActionsExtractor"
	);
}

#[test]
fn interface_records_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("interface")).expect("create interface");
	fs::write(
		mod_root.join("interface").join("core.gui"),
		r#"
guiTypes = {
	windowType = {
		name = "frontend_loading"
		backGround = ""
		position = { x = 0 y = 0 }
		size = { x = 640 y = 480 }
		dontRender = ""
		moveable = 0
	}
}
"#,
	)
	.expect("write interface");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root.join("interface").join("core.gui"),
	)
	.expect("parsed interface");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("interface/core.gui")
				&& reference.key == "interface_definition"
				&& reference.value == "guiTypes"
		}),
		"should capture guiTypes as an interface definition"
	);
}

#[test]
fn common_interface_records_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("common").join("interface")).expect("create common/interface");
	fs::write(
		mod_root
			.join("common")
			.join("interface")
			.join("country.gui"),
		r#"
guiTypes = {
	windowType = {
		name = "country_diplomacy"
		backGround = ""
		position = { x = 0 y = 0 }
		size = { x = 510 y = 680 }
		moveable = 1
	}
}
"#,
	)
	.expect("write common/interface");

	let parsed = parse_script_file(
		"1016",
		&mod_root,
		&mod_root
			.join("common")
			.join("interface")
			.join("country.gui"),
	)
	.expect("parsed common/interface");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/interface/country.gui")
				&& reference.key == "interface_definition"
				&& reference.value == "guiTypes"
		}),
		"should capture guiTypes as an interface definition via common/interface family"
	);
}

#[test]
fn gfx_records_resource_references() {
	let tmp = TempDir::new().expect("temp dir");
	let mod_root = tmp.path().join("mod");
	fs::create_dir_all(mod_root.join("gfx")).expect("create gfx");
	fs::write(
		mod_root.join("gfx").join("FX.gfx"),
		r#"
spriteTypes = {
	spriteType = {
		name = "GFX_advisor_theologian"
		texturefile = "gfx/interface/advisors/theologian.dds"
	}
	spriteType = {
		name = "GFX_advisor_artist"
		texturefile = "gfx/interface/advisors/artist.dds"
	}
}

objectTypes = {
	pdxmesh = {
		name = "western_galleon_mesh"
		file = "gfx/models/ships/western_galleon.mesh"
	}
}
"#,
	)
	.expect("write gfx");

	let parsed = parse_script_file("1016", &mod_root, &mod_root.join("gfx").join("FX.gfx"))
		.expect("parsed gfx");
	let index = build_semantic_index(&[parsed]);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("gfx/FX.gfx")
				&& reference.key == "gfx_definition"
				&& reference.value == "spriteTypes"
		}),
		"should capture spriteTypes as a gfx definition"
	);
	assert!(
		index.resource_references.iter().any(|reference| {
			reference.path == Path::new("gfx/FX.gfx")
				&& reference.key == "gfx_definition"
				&& reference.value == "objectTypes"
		}),
		"should capture objectTypes as a gfx definition"
	);
}

#[test]
fn batch_promoted_roots_emit_definition_references() {
	// (root_dir, def_key, block_name) — directory-based roots
	let dir_cases: &[(&str, &str, &str)] = &[
		("common/tradegoods", "tradegoods_definition", "grain"),
		(
			"common/colonial_regions",
			"colonial_regions_definition",
			"colonial_eastern_america",
		),
		(
			"common/static_modifiers",
			"static_modifiers_definition",
			"war",
		),
		(
			"common/wargoal_types",
			"wargoal_types_definition",
			"take_province",
		),
	];

	for &(root, def_key, block_name) in dir_cases {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		let dir = mod_root.join(root);
		fs::create_dir_all(&dir).expect("create dir");
		fs::write(
			dir.join("test.txt"),
			format!("{block_name} = {{\n\tsome_key = some_value\n}}\n"),
		)
		.expect("write file");
		let parsed = [parse_script_file("1000", &mod_root, &dir.join("test.txt")).expect("parsed")];
		let index = build_semantic_index(&parsed);
		assert!(
			index
				.resource_references
				.iter()
				.any(|r| r.key == def_key && r.value == block_name),
			"root {root} should emit {def_key} for '{block_name}'"
		);
	}

	// (exact_file, def_key, block_name) — single-file roots
	let exact_cases: &[(&str, &str, &str)] = &[
		("map/region.txt", "region_definition", "france_region"),
		("map/terrain.txt", "terrain_definition", "grasslands"),
	];

	for &(file_path, def_key, block_name) in exact_cases {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		let full_path = mod_root.join(file_path);
		fs::create_dir_all(full_path.parent().unwrap()).expect("create dir");
		fs::write(
			&full_path,
			format!("{block_name} = {{\n\tsome_key = some_value\n}}\n"),
		)
		.expect("write file");
		let parsed = [parse_script_file("1000", &mod_root, &full_path).expect("parsed")];
		let index = build_semantic_index(&parsed);
		assert!(
			index
				.resource_references
				.iter()
				.any(|r| r.key == def_key && r.value == block_name),
			"file {file_path} should emit {def_key} for '{block_name}'"
		);
	}
}

#[test]
fn batch_promoted_roots_have_registered_extractors() {
	use super::super::content_family::GameProfile;
	use super::super::eu4_profile::eu4_profile;
	use super::extractors;

	let roots: &[(&str, &str)] = &[
		("common/tradegoods/00_tradegoods.txt", "common/tradegoods"),
		(
			"common/colonial_regions/00_colonial_regions.txt",
			"common/colonial_regions",
		),
		(
			"common/opinion_modifiers/00_opinion_modifiers.txt",
			"common/opinion_modifiers",
		),
		("map/region.txt", "map/region"),
		("map/terrain.txt", "map/terrain"),
		("map/area.txt", "map/area"),
		("map/climate.txt", "map/climate"),
		("map/continent.txt", "map/continent"),
		("map/lakes.txt", "map/lakes"),
		("map/positions.txt", "map/positions"),
		("map/provincegroup.txt", "map/provincegroup"),
		("map/seasons.txt", "map/seasons"),
		("map/superregion.txt", "map/superregion"),
		("map/trade_winds.txt", "map/trade_winds"),
		("map/ambient_object.txt", "map/ambient_object"),
		(
			"common/graphicalculturetype.txt",
			"common/graphicalculturetype",
		),
		("music/songs.txt", "music"),
		("sound/sounds.txt", "sound"),
		("tutorial/00_tutorial.txt", "tutorial"),
		("userdir.txt", "userdir.txt"),
	];

	let profile = eu4_profile();
	for &(path, expected_id) in roots {
		let descriptor = profile
			.classify_content_family(std::path::Path::new(path))
			.unwrap_or_else(|| panic!("no descriptor for path {path}"));
		assert_eq!(descriptor.id, expected_id, "wrong id for {path}");
		assert!(
			extractors::extractor_for(descriptor).is_some(),
			"{expected_id} should have a registered extractor"
		);
	}
}
