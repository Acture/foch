use super::{
	BASE_DATA_DIR_ENV, BASE_DATA_SCHEMA_VERSION, BaseAnalysisSnapshot, BaseDataSource,
	BaseSymbolDefinition, CoverageClass, INSTALLED_COVERAGE_FILE_NAME,
	INSTALLED_SNAPSHOT_FILE_NAME, InstalledBaseDataMetadata, build_coverage_report,
	decode_snapshot_from_bytes, encode_snapshot_to_bytes, load_installed_base_snapshot,
	write_installed_snapshot, write_snapshot_bundle,
};
use foch_core::domain::game::Game;
use foch_core::model::{
	DocumentFamily, DocumentRecord, LocalisationDefinition, ParamContract, ResourceReference,
	ScopeType, SemanticIndex, SymbolDefinition, SymbolKind,
};
use foch_language::analysis_version::analysis_rules_version;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;

static BASE_DATA_ENV_LOCK: Mutex<()> = Mutex::new(());

fn sample_snapshot_with_contract() -> BaseAnalysisSnapshot {
	let mut index = SemanticIndex::default();
	index.definitions.push(SymbolDefinition {
		kind: SymbolKind::ScriptedEffect,
		name: "test.effect".to_string(),
		module: "test".to_string(),
		local_name: "add_age_modifier".to_string(),
		mod_id: "__game__eu4".to_string(),
		path: PathBuf::from("common/scripted_effects/test.txt"),
		line: 1,
		column: 1,
		scope_id: 0,
		declared_this_type: ScopeType::Country,
		inferred_this_type: ScopeType::Country,
		inferred_this_mask: 0b01,
		inferred_from_mask: 0,
		inferred_root_mask: 0,
		required_params: vec![
			"age".to_string(),
			"name".to_string(),
			"duration".to_string(),
		],
		param_contract: Some(ParamContract {
			required_all: vec![
				"age".to_string(),
				"name".to_string(),
				"duration".to_string(),
			],
			optional: vec!["else".to_string()],
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		optional_params: Vec::new(),
		scope_param_names: Vec::new(),
	});
	BaseAnalysisSnapshot::from_semantic_index(
		&Game::EuropaUniversalis4,
		"schema-test",
		vec!["common/scripted_effects/test.txt".to_string()],
		&index,
		Default::default(),
	)
}

fn sample_coverage_snapshot() -> BaseAnalysisSnapshot {
	let mod_id = "__game__eu4".to_string();
	let mut index = SemanticIndex {
		documents: vec![
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/country_tags/00_countries.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/countries/Sweden.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/units/swedish_tercio.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/religions/00_religion.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/subject_types/00_subject_types.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/rebel_types/independence_rebels.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/disasters/civil_war.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/government_mechanics/18_parliament_vs_monarchy.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/peace_treaties/00_peace_treaties.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/bookmarks/a_new_world.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/policies/00_adm.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/mercenary_companies/00_mercenaries.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/fervor/00_fervor.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/decrees/00_china.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/federation_advancements/00_default.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/golden_bulls/00_golden_bulls.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/flagship_modifications/00_flagship_modifications.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/holy_orders/00_holy_orders.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/naval_doctrines/00_naval_doctrines.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/defender_of_faith/00_defender_of_faith.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/isolationism/00_shinto.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/professionalism/00_modifiers.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/powerprojection/00_static.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/subject_type_upgrades/00_subject_type_upgrades.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/government_ranks/00_government_ranks.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/achievements.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/ages/00_ages.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/scripted_triggers/00_triggers.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/diplomatic_actions/00_actions.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/new_diplomatic_actions/00_actions.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/buildings/buildings.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/institutions/institutions.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/great_projects/00_coverage_projects.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/advisortypes/00_advisortypes.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/government_names/00_coverage_government_names.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/custom_gui/00_coverage_gui.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/cultures/00_coverage_cultures.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/event_modifiers/00_modifiers.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/province_triggered_modifiers/00_modifiers.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/cb_types/00_cb.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/ideas/00_ideas.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/technologies/adm.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/technology.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/estate_agendas/00_generic_agendas.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/estate_privileges/01_church_privileges.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/estates/01_church.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/parliament_bribes/administrative_support.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/parliament_issues/00_adm_parliament_issues.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/state_edicts/edict_of_governance.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/church_aspects/00_church_aspects.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/factions/00_factions.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/hegemons/0_economic_hegemon.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/personal_deities/00_hindu_deities.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/fetishist_cults/00_fetishist_cults.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/scripted_effects/test.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("history/countries/SWE - Sweden.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("history/provinces/1 - Stockholm.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("common/province_names/sorbian.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("map/random/tiles/tile0.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("map/random/RandomLandNames.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("map/random/RNWScenarios.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("history/diplomacy/hre.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("history/advisors/00_england.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("history/wars/sample.txt"),
				family: DocumentFamily::Clausewitz,
				parse_ok: true,
			},
			DocumentRecord {
				mod_id: mod_id.clone(),
				path: PathBuf::from("localisation/english/test_l_english.yml"),
				family: DocumentFamily::Localisation,
				parse_ok: true,
			},
		],
		..Default::default()
	};
	index.definitions.push(SymbolDefinition {
		kind: SymbolKind::ScriptedEffect,
		name: "test.effect".to_string(),
		module: "test".to_string(),
		local_name: "test_effect".to_string(),
		mod_id: mod_id.clone(),
		path: PathBuf::from("common/scripted_effects/test.txt"),
		line: 1,
		column: 1,
		scope_id: 0,
		declared_this_type: ScopeType::Country,
		inferred_this_type: ScopeType::Country,
		inferred_this_mask: 0b01,
		inferred_from_mask: 0,
		inferred_root_mask: 0,
		required_params: vec!["value".to_string()],
		param_contract: None,
		optional_params: Vec::new(),
		scope_param_names: Vec::new(),
	});
	index.localisation_definitions.push(LocalisationDefinition {
		key: "test_key".to_string(),
		mod_id,
		path: PathBuf::from("localisation/english/test_l_english.yml"),
		line: 1,
		column: 1,
	});
	index.resource_references.extend([
		ResourceReference {
			key: "country_tag:SWE".to_string(),
			value: "countries/Sweden.txt".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/country_tags/00_countries.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "graphical_culture".to_string(),
			value: "scandinaviangfx".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/countries/Sweden.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "historical_units".to_string(),
			value: "western_medieval_infantry".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/countries/Sweden.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "capital".to_string(),
			value: "1".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/countries/SWE - Sweden.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "owner".to_string(),
			value: "SWE".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/provinces/1 - Stockholm.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "province_name_table".to_string(),
			value: "sorbian".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/province_names/sorbian.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "province_id".to_string(),
			value: "4778".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/province_names/sorbian.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "province_name_literal".to_string(),
			value: "Zhorjelc".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/province_names/sorbian.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "tile_definition".to_string(),
			value: "tile0".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/tiles/tile0.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "tile_color_group".to_string(),
			value: "sea_province".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/tiles/tile0.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "tile_color_rgb".to_string(),
			value: "93,164,236".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/tiles/tile0.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "tile_size".to_string(),
			value: "7,7".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/tiles/tile0.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "weight".to_string(),
			value: "130".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/tiles/tile0.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "random_name_table".to_string(),
			value: "random_land_names".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RandomLandNames.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "random_name_token".to_string(),
			value: "p_tumbletown".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RandomLandNames.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "random_name_token".to_string(),
			value: "p_chugwater".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RandomLandNames.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "random_name_category".to_string(),
			value: "river".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RandomLandNames.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "random_map_scenario".to_string(),
			value: "scenario_animism_tribes".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RNWScenarios.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "religion".to_string(),
			value: "animism".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RNWScenarios.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "technology_group".to_string(),
			value: "south_american".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RNWScenarios.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "government".to_string(),
			value: "native".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RNWScenarios.txt"),
			line: 4,
			column: 1,
		},
		ResourceReference {
			key: "graphical_culture".to_string(),
			value: "northamericagfx".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RNWScenarios.txt"),
			line: 5,
			column: 1,
		},
		ResourceReference {
			key: "scenario_name_key".to_string(),
			value: "rnw_arauluche".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("map/random/RNWScenarios.txt"),
			line: 6,
			column: 1,
		},
		ResourceReference {
			key: "relation_type".to_string(),
			value: "alliance".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/diplomacy/hre.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "first".to_string(),
			value: "FRA".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/diplomacy/hre.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "second".to_string(),
			value: "SCO".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/diplomacy/hre.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "emperor".to_string(),
			value: "BOH".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/diplomacy/hre.txt"),
			line: 4,
			column: 1,
		},
		ResourceReference {
			key: "advisor_definition".to_string(),
			value: "advisor_216".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/advisors/00_england.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "location".to_string(),
			value: "236".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/advisors/00_england.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "type".to_string(),
			value: "theologian".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/advisors/00_england.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "unit_type".to_string(),
			value: "western".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/units/swedish_tercio.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "center_of_religion".to_string(),
			value: "118".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/religions/00_religion.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "copy_from".to_string(),
			value: "default".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/subject_types/00_subject_types.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "demands_description".to_string(),
			value: "independence_rebels_demands".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/rebel_types/independence_rebels.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "on_start".to_string(),
			value: "civil_war.1".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/disasters/civil_war.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "gui".to_string(),
			value: "parliament_vs_monarchy_gov_mech".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/government_mechanics/18_parliament_vs_monarchy.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "localisation_desc".to_string(),
			value: "spread_dynasty_desc".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/peace_treaties/00_peace_treaties.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "country".to_string(),
			value: "CAS".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/bookmarks/a_new_world.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "localisation".to_string(),
			value: "the_combination_act".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/policies/00_adm.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "monarch_power".to_string(),
			value: "ADM".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/policies/00_adm.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "localisation".to_string(),
			value: "merc_black_army".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/mercenary_companies/00_mercenaries.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "mercenary_desc_key".to_string(),
			value: "FREE_OF_ARMY_PROFESSIONALISM_COST".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/mercenary_companies/00_mercenaries.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "fervor_definition".to_string(),
			value: "fervor_trade".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/fervor/00_fervor.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "cost_type".to_string(),
			value: "fervor".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/fervor/00_fervor.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "decree_definition".to_string(),
			value: "expand_bureaucracy_decree".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/decrees/00_china.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "icon".to_string(),
			value: "decree_expand_bureaucracy".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/decrees/00_china.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "federation_advancement_definition".to_string(),
			value: "federal_constitution".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/federation_advancements/00_default.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "gfx".to_string(),
			value: "federation_constitution".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/federation_advancements/00_default.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "government".to_string(),
			value: "federal_republic".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/federation_advancements/00_default.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "golden_bull_definition".to_string(),
			value: "golden_bull_treasury".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/golden_bulls/00_golden_bulls.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "mechanics".to_string(),
			value: "curia_treasury".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/golden_bulls/00_golden_bulls.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "flagship_modification_definition".to_string(),
			value: "extra_cannons".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/flagship_modifications/00_flagship_modifications.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "cost_type".to_string(),
			value: "sailors".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/flagship_modifications/00_flagship_modifications.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "holy_order_definition".to_string(),
			value: "benedictines".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/holy_orders/00_holy_orders.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "cost_type".to_string(),
			value: "adm_power".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/holy_orders/00_holy_orders.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "naval_doctrine_definition".to_string(),
			value: "fleet_in_being".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/naval_doctrines/00_naval_doctrines.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "button_gfx".to_string(),
			value: "1".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/naval_doctrines/00_naval_doctrines.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "defender_of_faith_definition".to_string(),
			value: "defender_of_faith_1".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/defender_of_faith/00_defender_of_faith.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "isolationism_definition".to_string(),
			value: "open_doors_isolation".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/isolationism/00_shinto.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "professionalism_definition".to_string(),
			value: "nothingness_modifier".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/professionalism/00_modifiers.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "marker_sprite".to_string(),
			value: "GFX_pa_rank_0".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/professionalism/00_modifiers.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "powerprojection_definition".to_string(),
			value: "great_power_1".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/powerprojection/00_static.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "subject_type_upgrade_definition".to_string(),
			value: "increase_force_limit_from_colony".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/subject_type_upgrades/00_subject_type_upgrades.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "government_rank_definition".to_string(),
			value: "2".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/government_ranks/00_government_ranks.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "achievement_definition".to_string(),
			value: "coverage_achievement".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/achievements.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "age_definition".to_string(),
			value: "age_of_discovery".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/ages/00_ages.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "scripted_trigger_definition".to_string(),
			value: "eu4_cov_country_trigger".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/scripted_triggers/00_triggers.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "diplomatic_action_definition".to_string(),
			value: "milaccess".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/diplomatic_actions/00_actions.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "new_diplomatic_action_definition".to_string(),
			value: "request_condottieri".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/new_diplomatic_actions/00_actions.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "building_definition".to_string(),
			value: "marketplace".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/buildings/buildings.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "institution_definition".to_string(),
			value: "feudalism".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/institutions/institutions.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "great_project_definition".to_string(),
			value: "coverage_project".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/great_projects/00_coverage_projects.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "advisor_type_definition".to_string(),
			value: "coverage_advisor".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/advisortypes/00_advisortypes.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "government_name_definition".to_string(),
			value: "coverage_government_names".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/government_names/00_coverage_government_names.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "custom_gui_definition".to_string(),
			value: "coverage_window".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/custom_gui/00_coverage_gui.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "culture_definition".to_string(),
			value: "coverage_culture".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/cultures/00_coverage_cultures.txt"),
			line: 2,
			column: 2,
		},
		ResourceReference {
			key: "event_modifier_definition".to_string(),
			value: "coverage_event_modifier".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/event_modifiers/00_modifiers.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "province_triggered_modifier_definition".to_string(),
			value: "coverage_ptm".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/province_triggered_modifiers/00_modifiers.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "cb_type_definition".to_string(),
			value: "coverage_cb".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/cb_types/00_cb.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "idea_group_definition".to_string(),
			value: "coverage_ideas".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/ideas/00_ideas.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "monarch_power".to_string(),
			value: "ADM".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/technologies/adm.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "technology_definition".to_string(),
			value: "adm_tech_0".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/technologies/adm.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "expects_institution".to_string(),
			value: "feudalism".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/technologies/adm.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "enable".to_string(),
			value: "temple".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/technologies/adm.txt"),
			line: 4,
			column: 1,
		},
		ResourceReference {
			key: "technology_group".to_string(),
			value: "western".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/technology.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "nation_designer_unit_type".to_string(),
			value: "western".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/technology.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "nation_designer_cost_value".to_string(),
			value: "25".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/technology.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "estate".to_string(),
			value: "clergy".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/estate_agendas/00_generic_agendas.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "custom_tooltip".to_string(),
			value: "agenda_done_tt".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/estate_agendas/00_generic_agendas.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "icon".to_string(),
			value: "privilege_religious_diplomats".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/estate_privileges/01_church_privileges.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "mechanics".to_string(),
			value: "papal_influence".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/estate_privileges/01_church_privileges.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "custom_name".to_string(),
			value: "estate_clergy_custom_name".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/estates/01_church.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "privileges".to_string(),
			value: "religious_diplomats".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/estates/01_church.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "mechanic_type".to_string(),
			value: "parliament_vs_monarchy_mechanic".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/parliament_bribes/administrative_support.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "parliament_action".to_string(),
			value: "strengthen_government".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/parliament_issues/00_adm_parliament_issues.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "tooltip".to_string(),
			value: "edict_of_governance_tt".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/state_edicts/edict_of_governance.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "has_state_edict".to_string(),
			value: "encourage_development_edict".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/state_edicts/edict_of_governance.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "localisation".to_string(),
			value: "organised_through_bishops_aspect".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/church_aspects/00_church_aspects.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "localisation_desc".to_string(),
			value: "desc_organised_through_bishops_aspect".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/church_aspects/00_church_aspects.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "localisation_modifier".to_string(),
			value: "organised_through_bishops_aspect_modifier".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/church_aspects/00_church_aspects.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "localisation".to_string(),
			value: "rr_jacobins".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/factions/00_factions.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "localisation_influence".to_string(),
			value: "rr_jacobins_influence".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/factions/00_factions.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "monarch_power".to_string(),
			value: "ADM".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/factions/00_factions.txt"),
			line: 3,
			column: 1,
		},
		ResourceReference {
			key: "localisation".to_string(),
			value: "economic_hegemon".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/hegemons/0_economic_hegemon.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "localisation".to_string(),
			value: "shiva".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/personal_deities/00_hindu_deities.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "localisation_desc".to_string(),
			value: "shiva_desc".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/personal_deities/00_hindu_deities.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "localisation".to_string(),
			value: "yemoja_cult".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/fetishist_cults/00_fetishist_cults.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "localisation_desc".to_string(),
			value: "yemoja_cult_desc".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/fetishist_cults/00_fetishist_cults.txt"),
			line: 2,
			column: 1,
		},
		ResourceReference {
			key: "add_attacker".to_string(),
			value: "SWE".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("history/wars/sample.txt"),
			line: 1,
			column: 1,
		},
		ResourceReference {
			key: "scripted_effect_definition".to_string(),
			value: "test_effect".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/scripted_effects/test.txt"),
			line: 1,
			column: 1,
		},
	]);
	BaseAnalysisSnapshot::from_semantic_index(
		&Game::EuropaUniversalis4,
		"coverage-test",
		vec![
			"common/country_tags/00_countries.txt".to_string(),
			"common/countries/Sweden.txt".to_string(),
			"common/units/swedish_tercio.txt".to_string(),
			"common/religions/00_religion.txt".to_string(),
			"common/subject_types/00_subject_types.txt".to_string(),
			"common/rebel_types/independence_rebels.txt".to_string(),
			"common/disasters/civil_war.txt".to_string(),
			"common/government_mechanics/18_parliament_vs_monarchy.txt".to_string(),
			"common/peace_treaties/00_peace_treaties.txt".to_string(),
			"common/bookmarks/a_new_world.txt".to_string(),
			"common/policies/00_adm.txt".to_string(),
			"common/mercenary_companies/00_mercenaries.txt".to_string(),
			"common/fervor/00_fervor.txt".to_string(),
			"common/decrees/00_china.txt".to_string(),
			"common/federation_advancements/00_default.txt".to_string(),
			"common/golden_bulls/00_golden_bulls.txt".to_string(),
			"common/flagship_modifications/00_flagship_modifications.txt".to_string(),
			"common/holy_orders/00_holy_orders.txt".to_string(),
			"common/naval_doctrines/00_naval_doctrines.txt".to_string(),
			"common/defender_of_faith/00_defender_of_faith.txt".to_string(),
			"common/isolationism/00_shinto.txt".to_string(),
			"common/professionalism/00_modifiers.txt".to_string(),
			"common/powerprojection/00_static.txt".to_string(),
			"common/subject_type_upgrades/00_subject_type_upgrades.txt".to_string(),
			"common/ages/00_ages.txt".to_string(),
			"common/scripted_triggers/00_triggers.txt".to_string(),
			"common/diplomatic_actions/00_actions.txt".to_string(),
			"common/new_diplomatic_actions/00_actions.txt".to_string(),
			"common/buildings/buildings.txt".to_string(),
			"common/institutions/institutions.txt".to_string(),
			"common/great_projects/00_coverage_projects.txt".to_string(),
			"common/technologies/adm.txt".to_string(),
			"common/technology.txt".to_string(),
			"common/estate_agendas/00_generic_agendas.txt".to_string(),
			"common/estate_privileges/01_church_privileges.txt".to_string(),
			"common/estates/01_church.txt".to_string(),
			"common/parliament_bribes/administrative_support.txt".to_string(),
			"common/parliament_issues/00_adm_parliament_issues.txt".to_string(),
			"common/state_edicts/edict_of_governance.txt".to_string(),
			"common/achievements.txt".to_string(),
			"common/church_aspects/00_church_aspects.txt".to_string(),
			"common/factions/00_factions.txt".to_string(),
			"common/hegemons/0_economic_hegemon.txt".to_string(),
			"common/personal_deities/00_hindu_deities.txt".to_string(),
			"common/fetishist_cults/00_fetishist_cults.txt".to_string(),
			"common/scripted_effects/test.txt".to_string(),
			"history/countries/SWE - Sweden.txt".to_string(),
			"history/provinces/1 - Stockholm.txt".to_string(),
			"common/province_names/sorbian.txt".to_string(),
			"map/random/tiles/tile0.txt".to_string(),
			"map/random/RandomLandNames.txt".to_string(),
			"map/random/RNWScenarios.txt".to_string(),
			"history/diplomacy/hre.txt".to_string(),
			"history/advisors/00_england.txt".to_string(),
			"history/wars/sample.txt".to_string(),
			"localisation/english/test_l_english.yml".to_string(),
			"patchnotes/1_36.txt".to_string(),
			"builtin_dlc/builtin_dlc.txt".to_string(),
			"checksum_manifest.txt".to_string(),
		],
		&index,
		Default::default(),
	)
}

#[test]
fn base_snapshot_round_trip_preserves_param_contracts() {
	let snapshot = sample_snapshot_with_contract();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let decoded = decode_snapshot_from_bytes(&encoded.bytes).expect("decode snapshot");
	let contract = decoded
		.symbol_definitions
		.first()
		.and_then(|definition| definition.param_contract.as_ref())
		.expect("serialized param contract");
	assert_eq!(contract.required_all, vec!["age", "name", "duration"]);
	assert_eq!(contract.optional, vec!["else"]);
	assert_eq!(
		decoded
			.symbol_definitions
			.first()
			.map(|definition| definition.inferred_this_mask),
		Some(0b01)
	);

	let rehydrated = decoded.to_semantic_index();
	let contract = rehydrated
		.definitions
		.first()
		.and_then(|definition| definition.param_contract.as_ref())
		.expect("rehydrated param contract");
	assert_eq!(contract.required_all, vec!["age", "name", "duration"]);
	assert_eq!(contract.optional, vec!["else"]);
	assert_eq!(
		rehydrated
			.definitions
			.first()
			.map(|definition| definition.inferred_this_mask),
		Some(0b01)
	);
}

#[test]
fn build_coverage_report_classifies_foundation_and_excluded_roots() {
	let snapshot = sample_coverage_snapshot();
	let report = build_coverage_report(&snapshot);
	let scripted_effects = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/scripted_effects")
		.expect("scripted effects coverage");
	assert_eq!(
		scripted_effects.coverage_class,
		CoverageClass::SemanticComplete
	);

	let provinces = report
		.roots
		.iter()
		.find(|item| item.root_family == "history/provinces")
		.expect("province history coverage");
	assert_eq!(provinces.coverage_class, CoverageClass::SemanticComplete);

	let province_names = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/province_names")
		.expect("province names coverage");
	assert_eq!(
		province_names.coverage_class,
		CoverageClass::SemanticComplete
	);

	let random_map_tiles = report
		.roots
		.iter()
		.find(|item| item.root_family == "map/random/tiles")
		.expect("random map tiles coverage");
	assert_eq!(
		random_map_tiles.coverage_class,
		CoverageClass::SemanticComplete
	);

	let random_map_names = report
		.roots
		.iter()
		.find(|item| item.root_family == "map/random_names")
		.expect("random map names coverage");
	assert_eq!(
		random_map_names.coverage_class,
		CoverageClass::SemanticComplete
	);

	let random_map_scenarios = report
		.roots
		.iter()
		.find(|item| item.root_family == "map/random/scenarios")
		.expect("random map scenarios coverage");
	assert_eq!(
		random_map_scenarios.coverage_class,
		CoverageClass::SemanticComplete
	);

	let diplomacy = report
		.roots
		.iter()
		.find(|item| item.root_family == "history/diplomacy")
		.expect("diplomacy history coverage");
	assert_eq!(diplomacy.coverage_class, CoverageClass::SemanticComplete);

	let great_projects = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/great_projects")
		.expect("great projects coverage");
	assert_eq!(
		great_projects.coverage_class,
		CoverageClass::SemanticComplete
	);

	let advisors = report
		.roots
		.iter()
		.find(|item| item.root_family == "history/advisors")
		.expect("advisor history coverage");
	assert_eq!(advisors.coverage_class, CoverageClass::SemanticComplete);

	let fervor = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/fervor")
		.expect("fervor coverage");
	assert_eq!(fervor.coverage_class, CoverageClass::SemanticComplete);

	let decrees = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/decrees")
		.expect("decrees coverage");
	assert_eq!(decrees.coverage_class, CoverageClass::SemanticComplete);

	let federation_advancements = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/federation_advancements")
		.expect("federation advancements coverage");
	assert_eq!(
		federation_advancements.coverage_class,
		CoverageClass::SemanticComplete
	);

	let golden_bulls = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/golden_bulls")
		.expect("golden bulls coverage");
	assert_eq!(golden_bulls.coverage_class, CoverageClass::SemanticComplete);

	let flagship_modifications = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/flagship_modifications")
		.expect("flagship modifications coverage");
	assert_eq!(
		flagship_modifications.coverage_class,
		CoverageClass::SemanticComplete
	);

	let holy_orders = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/holy_orders")
		.expect("holy orders coverage");
	assert_eq!(holy_orders.coverage_class, CoverageClass::SemanticComplete);

	let naval_doctrines = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/naval_doctrines")
		.expect("naval doctrines coverage");
	assert_eq!(
		naval_doctrines.coverage_class,
		CoverageClass::SemanticComplete
	);

	let defender_of_faith = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/defender_of_faith")
		.expect("defender of faith coverage");
	assert_eq!(
		defender_of_faith.coverage_class,
		CoverageClass::SemanticComplete
	);

	let isolationism = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/isolationism")
		.expect("isolationism coverage");
	assert_eq!(isolationism.coverage_class, CoverageClass::SemanticComplete);

	let professionalism = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/professionalism")
		.expect("professionalism coverage");
	assert_eq!(
		professionalism.coverage_class,
		CoverageClass::SemanticComplete
	);

	let powerprojection = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/powerprojection")
		.expect("powerprojection coverage");
	assert_eq!(
		powerprojection.coverage_class,
		CoverageClass::SemanticComplete
	);

	let subject_type_upgrades = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/subject_type_upgrades")
		.expect("subject type upgrades coverage");
	assert_eq!(
		subject_type_upgrades.coverage_class,
		CoverageClass::SemanticComplete
	);

	let government_ranks = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/government_ranks")
		.expect("government ranks coverage");
	assert_eq!(
		government_ranks.coverage_class,
		CoverageClass::SemanticComplete
	);

	let ages = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/ages")
		.expect("ages coverage");
	assert_eq!(ages.coverage_class, CoverageClass::SemanticComplete);

	let scripted_triggers = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/scripted_triggers")
		.expect("scripted triggers coverage");
	assert_eq!(
		scripted_triggers.coverage_class,
		CoverageClass::SemanticComplete
	);

	let diplomatic_actions = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/diplomatic_actions")
		.expect("diplomatic actions coverage");
	assert_eq!(
		diplomatic_actions.coverage_class,
		CoverageClass::SemanticComplete
	);

	let new_diplomatic_actions = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/new_diplomatic_actions")
		.expect("new diplomatic actions coverage");
	assert_eq!(
		new_diplomatic_actions.coverage_class,
		CoverageClass::SemanticComplete
	);

	let buildings = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/buildings")
		.expect("buildings coverage");
	assert_eq!(buildings.coverage_class, CoverageClass::SemanticComplete);

	let institutions = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/institutions")
		.expect("institutions coverage");
	assert_eq!(institutions.coverage_class, CoverageClass::SemanticComplete);

	let advisortypes = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/advisortypes")
		.expect("advisortypes coverage");
	assert_eq!(advisortypes.coverage_class, CoverageClass::SemanticComplete);

	let government_names = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/government_names")
		.expect("government names coverage");
	assert_eq!(
		government_names.coverage_class,
		CoverageClass::SemanticComplete
	);

	let custom_gui = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/custom_gui")
		.expect("custom gui coverage");
	assert_eq!(custom_gui.coverage_class, CoverageClass::SemanticComplete);

	let cultures = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/cultures")
		.expect("cultures coverage");
	assert_eq!(cultures.coverage_class, CoverageClass::SemanticComplete);

	let event_modifiers = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/event_modifiers")
		.expect("event modifiers coverage");
	assert_eq!(
		event_modifiers.coverage_class,
		CoverageClass::SemanticComplete
	);

	let province_triggered_modifiers = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/province_triggered_modifiers")
		.expect("province triggered modifiers coverage");
	assert_eq!(
		province_triggered_modifiers.coverage_class,
		CoverageClass::SemanticComplete
	);

	let cb_types = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/cb_types")
		.expect("cb types coverage");
	assert_eq!(cb_types.coverage_class, CoverageClass::SemanticComplete);

	let ideas = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/ideas")
		.expect("ideas coverage");
	assert_eq!(ideas.coverage_class, CoverageClass::SemanticComplete);

	let country_tags = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/country_tags")
		.expect("country tags coverage");
	assert_eq!(country_tags.coverage_class, CoverageClass::SemanticComplete);

	let countries = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/countries")
		.expect("countries coverage");
	assert_eq!(countries.coverage_class, CoverageClass::SemanticComplete);

	let units = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/units")
		.expect("units coverage");
	assert_eq!(units.coverage_class, CoverageClass::SemanticComplete);

	let religions = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/religions")
		.expect("religions coverage");
	assert_eq!(religions.coverage_class, CoverageClass::SemanticComplete);

	let subject_types = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/subject_types")
		.expect("subject types coverage");
	assert_eq!(
		subject_types.coverage_class,
		CoverageClass::SemanticComplete
	);

	let rebel_types = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/rebel_types")
		.expect("rebel types coverage");
	assert_eq!(rebel_types.coverage_class, CoverageClass::SemanticComplete);

	let disasters = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/disasters")
		.expect("disasters coverage");
	assert_eq!(disasters.coverage_class, CoverageClass::SemanticComplete);

	let government_mechanics = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/government_mechanics")
		.expect("government mechanics coverage");
	assert_eq!(
		government_mechanics.coverage_class,
		CoverageClass::SemanticComplete
	);

	let peace_treaties = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/peace_treaties")
		.expect("peace treaties coverage");
	assert_eq!(
		peace_treaties.coverage_class,
		CoverageClass::SemanticComplete
	);

	let bookmarks = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/bookmarks")
		.expect("bookmarks coverage");
	assert_eq!(bookmarks.coverage_class, CoverageClass::SemanticComplete);

	let policies = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/policies")
		.expect("policies coverage");
	assert_eq!(policies.coverage_class, CoverageClass::SemanticComplete);

	let mercenary_companies = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/mercenary_companies")
		.expect("mercenary companies coverage");
	assert_eq!(
		mercenary_companies.coverage_class,
		CoverageClass::SemanticComplete
	);

	let technologies = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/technologies")
		.expect("technologies coverage");
	assert_eq!(technologies.coverage_class, CoverageClass::SemanticComplete);

	let technology_groups = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/technology")
		.expect("technology groups coverage");
	assert_eq!(
		technology_groups.coverage_class,
		CoverageClass::SemanticComplete
	);

	let estate_agendas = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/estate_agendas")
		.expect("estate agendas coverage");
	assert_eq!(
		estate_agendas.coverage_class,
		CoverageClass::SemanticComplete
	);

	let estate_privileges = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/estate_privileges")
		.expect("estate privileges coverage");
	assert_eq!(
		estate_privileges.coverage_class,
		CoverageClass::SemanticComplete
	);

	let estates = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/estates")
		.expect("estates coverage");
	assert_eq!(estates.coverage_class, CoverageClass::SemanticComplete);

	let parliament_bribes = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/parliament_bribes")
		.expect("parliament bribes coverage");
	assert_eq!(
		parliament_bribes.coverage_class,
		CoverageClass::SemanticComplete
	);

	let parliament_issues = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/parliament_issues")
		.expect("parliament issues coverage");
	assert_eq!(
		parliament_issues.coverage_class,
		CoverageClass::SemanticComplete
	);

	let state_edicts = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/state_edicts")
		.expect("state edicts coverage");
	assert_eq!(state_edicts.coverage_class, CoverageClass::SemanticComplete);

	let church_aspects = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/church_aspects")
		.expect("church aspects coverage");
	assert_eq!(
		church_aspects.coverage_class,
		CoverageClass::SemanticComplete
	);

	let factions = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/factions")
		.expect("factions coverage");
	assert_eq!(factions.coverage_class, CoverageClass::SemanticComplete);

	let hegemons = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/hegemons")
		.expect("hegemons coverage");
	assert_eq!(hegemons.coverage_class, CoverageClass::SemanticComplete);

	let personal_deities = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/personal_deities")
		.expect("personal deities coverage");
	assert_eq!(
		personal_deities.coverage_class,
		CoverageClass::SemanticComplete
	);

	let fetishist_cults = report
		.roots
		.iter()
		.find(|item| item.root_family == "common/fetishist_cults")
		.expect("fetishist cults coverage");
	assert_eq!(
		fetishist_cults.coverage_class,
		CoverageClass::SemanticComplete
	);

	let localisation = report
		.roots
		.iter()
		.find(|item| item.root_family == "localisation")
		.expect("localisation coverage");
	assert_eq!(localisation.coverage_class, CoverageClass::SemanticComplete);

	let patchnotes = report
		.roots
		.iter()
		.find(|item| item.root_family == "patchnotes")
		.expect("patchnotes coverage");
	assert_eq!(
		patchnotes.coverage_class,
		CoverageClass::ExcludedNonGameplay
	);

	let builtin_dlc = report
		.roots
		.iter()
		.find(|item| item.root_family == "builtin_dlc")
		.expect("builtin dlc coverage");
	assert_eq!(
		builtin_dlc.coverage_class,
		CoverageClass::ExcludedNonGameplay
	);

	let checksum_manifest = report
		.roots
		.iter()
		.find(|item| item.root_family == "checksum_manifest.txt")
		.expect("checksum manifest coverage");
	assert_eq!(
		checksum_manifest.coverage_class,
		CoverageClass::ExcludedNonGameplay
	);
}

#[test]
fn write_snapshot_bundle_emits_coverage_report() {
	let temp = TempDir::new().expect("temp dir");
	let snapshot = sample_coverage_snapshot();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let bundle = write_snapshot_bundle(
		&snapshot,
		&encoded.bytes,
		temp.path(),
		BaseDataSource::Build,
		None,
		None,
	)
	.expect("write bundle");
	assert!(bundle.coverage_path.is_file());
	let coverage_path = temp.path().join(INSTALLED_COVERAGE_FILE_NAME);
	assert_eq!(bundle.coverage_path, coverage_path);
}

#[test]
fn load_installed_base_snapshot_rejects_old_schema_version() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let snapshot = sample_snapshot_with_contract();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let metadata = InstalledBaseDataMetadata {
		schema_version: BASE_DATA_SCHEMA_VERSION,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: analysis_rules_version().to_string(),
		generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
		source: BaseDataSource::Build,
		asset_name: None,
		sha256: None,
	};
	let installed =
		write_installed_snapshot(&snapshot, &metadata, &encoded.bytes).expect("install snapshot");
	assert!(
		installed
			.install_dir
			.join(INSTALLED_COVERAGE_FILE_NAME)
			.is_file()
	);
	let metadata_path = installed
		.install_dir
		.join(super::INSTALLED_METADATA_FILE_NAME);
	let old_metadata = InstalledBaseDataMetadata {
		schema_version: BASE_DATA_SCHEMA_VERSION - 1,
		..metadata
	};
	std::fs::write(
		&metadata_path,
		serde_json::to_string_pretty(&old_metadata).expect("serialize metadata"),
	)
	.expect("write metadata");

	let err = load_installed_base_snapshot("eu4", "schema-test")
		.expect_err("old schema should be rejected");
	assert!(err.contains("基础数据 schema 不匹配"));

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn load_installed_base_snapshot_rejects_stale_metadata_before_decoding_snapshot() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let snapshot = sample_snapshot_with_contract();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let metadata = InstalledBaseDataMetadata {
		schema_version: BASE_DATA_SCHEMA_VERSION,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: analysis_rules_version().to_string(),
		generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
		source: BaseDataSource::Build,
		asset_name: None,
		sha256: None,
	};
	let installed =
		write_installed_snapshot(&snapshot, &metadata, &encoded.bytes).expect("install snapshot");
	assert!(
		installed
			.install_dir
			.join(INSTALLED_COVERAGE_FILE_NAME)
			.is_file()
	);
	let metadata_path = installed
		.install_dir
		.join(super::INSTALLED_METADATA_FILE_NAME);
	let old_metadata = InstalledBaseDataMetadata {
		schema_version: BASE_DATA_SCHEMA_VERSION - 1,
		..metadata
	};
	std::fs::write(
		&metadata_path,
		serde_json::to_string_pretty(&old_metadata).expect("serialize metadata"),
	)
	.expect("write metadata");
	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	std::fs::write(&snapshot_path, b"definitely-not-a-valid-snapshot")
		.expect("write corrupt snapshot");

	let err = load_installed_base_snapshot("eu4", "schema-test")
		.expect_err("stale metadata should short-circuit before decode");
	assert!(err.contains("基础数据 schema 不匹配"));
	assert!(!err.contains("无法解析基础数据 snapshot"));

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn base_symbol_definition_defaults_missing_param_contract() {
	let raw = serde_json::json!({
		"kind": "ScriptedEffect",
		"name": "test.effect",
		"module": "test",
		"local_name": "test_effect",
		"path": "common/scripted_effects/test.txt",
		"line": 1,
		"column": 1,
		"scope_id": 0,
		"declared_this_type": "Country",
		"inferred_this_type": "Country",
		"inferred_this_mask": 1,
		"required_params": []
	});
	let decoded: BaseSymbolDefinition =
		serde_json::from_value(raw).expect("deserialize legacy base symbol definition");
	assert!(decoded.param_contract.is_none());
	assert_eq!(decoded.inferred_this_mask, 1);
}
