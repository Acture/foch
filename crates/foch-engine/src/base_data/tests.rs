use super::{
	BASE_DATA_DIR_ENV, BASE_DATA_ENV_LOCK, BASE_DATA_SCHEMA_VERSION, BaseAnalysisSnapshot,
	BaseDataSource, BaseSymbolDefinition, CoverageClass, INSTALLED_COVERAGE_FILE_NAME,
	INSTALLED_SNAPSHOT_FILE_NAME, InstalledBaseDataMetadata, build_coverage_report,
	clear_cached_loaded_base_snapshot, decode_cached_base_snapshot, decode_snapshot_from_bytes,
	encode_snapshot_to_bytes, install_installed_snapshot_decode_gate,
	installed_base_snapshot_identity, installed_snapshot_cold_decode_count,
	installed_snapshot_current_digest_count, installed_snapshot_current_validation_count,
	installed_snapshot_file_read_count, load_installed_base_snapshot,
	loaded_base_snapshot_cache_completed_count, lock_and_validate_installed_base_snapshot_identity,
	lock_installed_base_snapshot_for_install, reset_installed_snapshot_test_counters, sha256_hex,
	stale_installed_base_data_message, validate_installed_base_snapshot_identity,
	verified_snapshot_content_matches, write_release_artifacts, write_snapshot_bundle,
	write_test_installed_snapshot,
};
use filetime::{FileTime, set_file_mtime};
use foch_core::domain::game::Game;
use foch_core::model::{
	DocumentFamily, DocumentRecord, LocalisationDefinition, MaybeScope, ParamContract,
	ResourceReference, ScopeSet, SemanticIndex, SymbolDefinition, SymbolKind, base_scope,
	test_support,
};
use foch_language::analysis_version::analysis_rules_version;
use foch_language::analyzer::content_family::CwtType;
use foch_language::analyzer::parser::parse_clausewitz_content;
use foch_language::analyzer::semantic_index::ParsedScriptFile;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

fn tamper_snapshot_preserving_len_and_mtime(path: &std::path::Path) {
	let metadata = std::fs::metadata(path).expect("snapshot metadata");
	let original_len = metadata.len();
	let original_mtime = FileTime::from_last_modification_time(&metadata);
	let mut bytes = std::fs::read(path).expect("read snapshot");
	let last = bytes.last_mut().expect("non-empty snapshot");
	*last ^= 0xff;
	assert!(
		decode_snapshot_from_bytes(&bytes).is_err(),
		"tamper fixture must corrupt the encoded snapshot"
	);
	std::fs::write(path, bytes).expect("tamper snapshot");
	set_file_mtime(path, original_mtime).expect("restore snapshot mtime");

	let tampered_metadata = std::fs::metadata(path).expect("tampered snapshot metadata");
	assert_eq!(tampered_metadata.len(), original_len);
	assert_eq!(
		FileTime::from_last_modification_time(&tampered_metadata),
		original_mtime
	);
}

fn country_mask() -> ScopeSet {
	base_scope::country().into()
}

fn sample_snapshot_with_contract() -> BaseAnalysisSnapshot {
	test_support::install_defaults();
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
		declared_this_type: MaybeScope::Known(base_scope::country()),
		inferred_this_type: MaybeScope::Known(base_scope::country()),
		inferred_this_mask: country_mask(),
		inferred_from_mask: ScopeSet::EMPTY,
		inferred_root_mask: ScopeSet::EMPTY,
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

fn alternate_valid_snapshot() -> BaseAnalysisSnapshot {
	let mut snapshot = sample_snapshot_with_contract();
	snapshot
		.inventory_paths
		.push("common/scripted_effects/alternate.txt".to_string());
	snapshot
}

fn metadata_for_test_snapshot(
	snapshot: &BaseAnalysisSnapshot,
	encoded_snapshot: &[u8],
) -> InstalledBaseDataMetadata {
	InstalledBaseDataMetadata {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: snapshot.analysis_rules_version.clone(),
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
		source: BaseDataSource::Build,
		asset_name: None,
		sha256: Some(sha256_hex(encoded_snapshot)),
		vocabulary_manifest_sha256: None,
	}
}

#[test]
fn base_snapshot_roundtrips_parsed_scripts_section() {
	test_support::install_defaults();
	let temp = TempDir::new().expect("temp dir");
	let relative_path = PathBuf::from("common/scripted_effects/test.txt");
	let absolute_path = temp.path().join(&relative_path);
	let source = "test_effect = { add_prestige = 1 }\n";
	let parsed = parse_clausewitz_content(absolute_path.clone(), source);
	let parsed_script = ParsedScriptFile {
		mod_id: "__game__eu4".to_string(),
		path: absolute_path,
		relative_path: relative_path.clone(),
		content_family: None,
		file_kind: CwtType::new("scripted_effects"),
		module_name: "scripted_effects".to_string(),
		ast: parsed.ast,
		source: source.to_string(),
		parse_issues: Vec::new(),
		parse_cache_hit: false,
	};
	let parsed_scripts = crate::cache::parsed_scripts::encode_parsed_documents(&[parsed_script])
		.expect("encode parsed script");
	let mut index = SemanticIndex::default();
	index.documents.push(DocumentRecord {
		mod_id: "__game__eu4".to_string(),
		path: relative_path.clone(),
		family: DocumentFamily::Clausewitz,
		parse_ok: true,
	});
	let snapshot = BaseAnalysisSnapshot::from_semantic_index_with_parsed_scripts(
		&Game::EuropaUniversalis4,
		"parsed-script-test",
		vec![relative_path.to_string_lossy().to_string()],
		&index,
		Default::default(),
		parsed_scripts,
	);

	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let decoded = decode_snapshot_from_bytes(&encoded.bytes).expect("decode snapshot");
	let decoded_scripts = decoded
		.parsed_script_files(temp.path())
		.expect("decode parsed scripts");

	assert_eq!(decoded_scripts.len(), 1);
	assert_eq!(decoded_scripts[0].mod_id, "__game__eu4");
	assert_eq!(decoded_scripts[0].relative_path, relative_path);
	assert_eq!(
		decoded_scripts[0].path,
		temp.path().join("common/scripted_effects/test.txt")
	);
	assert_eq!(decoded_scripts[0].source, source);
	assert_eq!(decoded_scripts[0].ast.statements.len(), 1);
	assert!(decoded_scripts[0].parse_cache_hit);
}

fn sample_coverage_snapshot() -> BaseAnalysisSnapshot {
	test_support::install_defaults();
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
		declared_this_type: MaybeScope::Known(base_scope::country()),
		inferred_this_type: MaybeScope::Known(base_scope::country()),
		inferred_this_mask: country_mask(),
		inferred_from_mask: ScopeSet::EMPTY,
		inferred_root_mask: ScopeSet::EMPTY,
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
		Some(country_mask())
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
		Some(country_mask())
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
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	let snapshot = sample_coverage_snapshot();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let bundle = write_snapshot_bundle(
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
fn write_snapshot_bundle_emits_vocabulary_manifest() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	let snapshot = sample_snapshot_with_contract();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	write_snapshot_bundle(
		&encoded.bytes,
		temp.path(),
		BaseDataSource::Build,
		None,
		None,
	)
	.expect("write bundle");

	let manifest_path = temp
		.path()
		.join(super::INSTALLED_VOCABULARY_MANIFEST_FILE_NAME);
	assert!(
		manifest_path.is_file(),
		"vocabulary manifest sidecar written"
	);
	let raw = std::fs::read_to_string(&manifest_path).expect("read manifest");
	let manifest: super::VocabularyManifest = serde_json::from_str(&raw).expect("parse manifest");
	assert!(
		manifest
			.entries
			.iter()
			.any(|entry| entry.name == "test.effect"),
		"manifest lists the defined symbol"
	);

	// Deterministic: rebuilding the manifest reproduces byte-identical output.
	let rebuilt = serde_json::to_string_pretty(&super::build_vocabulary_manifest(&snapshot))
		.expect("serialize manifest");
	assert_eq!(raw, rebuilt);

	// The metadata records the sidecar digest for drift provenance.
	let metadata_raw =
		std::fs::read_to_string(temp.path().join(super::INSTALLED_METADATA_FILE_NAME))
			.expect("read metadata");
	let metadata: InstalledBaseDataMetadata =
		serde_json::from_str(&metadata_raw).expect("parse metadata");
	assert!(metadata.vocabulary_manifest_sha256.is_some());
}

#[test]
fn snapshot_export_sidecars_are_derived_from_encoded_snapshot() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	let snapshot = alternate_valid_snapshot();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let expected_sha256 = sha256_hex(&encoded.bytes);

	let bundle = write_snapshot_bundle(
		&encoded.bytes,
		&temp.path().join("bundle"),
		BaseDataSource::Build,
		Some("payload-asset.bin".to_string()),
		Some(expected_sha256.clone()),
	)
	.expect("write bundle from encoded snapshot");
	let metadata: InstalledBaseDataMetadata = serde_json::from_str(
		&std::fs::read_to_string(&bundle.metadata_path).expect("read bundle metadata"),
	)
	.expect("parse bundle metadata");
	assert_eq!(metadata.game, snapshot.game);
	assert_eq!(metadata.game_version, snapshot.game_version);
	assert_eq!(metadata.sha256.as_deref(), Some(expected_sha256.as_str()));

	let release = write_release_artifacts(&encoded.bytes, &temp.path().join("release"), "v-test")
		.expect("write release from encoded snapshot");
	let manifest: super::ReleaseDataManifest = serde_json::from_str(
		&std::fs::read_to_string(&release.manifest_path).expect("read release manifest"),
	)
	.expect("parse release manifest");
	assert_eq!(manifest.assets[0].game, snapshot.game);
	assert_eq!(manifest.assets[0].game_version, snapshot.game_version);
	assert_eq!(manifest.assets[0].sha256, expected_sha256);
}

#[test]
fn snapshot_export_rejects_invalid_encoded_bytes_before_writing_sidecars() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	let bundle_dir = temp.path().join("bundle");
	let release_dir = temp.path().join("release");

	let bundle_err = write_snapshot_bundle(
		b"not a snapshot",
		&bundle_dir,
		BaseDataSource::Build,
		None,
		None,
	)
	.expect_err("invalid bundle payload must be rejected");
	let release_err = write_release_artifacts(b"not a snapshot", &release_dir, "v-test")
		.expect_err("invalid release payload must be rejected");

	assert!(bundle_err.contains("failed to verify encoded base data snapshot"));
	assert!(release_err.contains("failed to verify encoded base data snapshot"));
	assert!(!bundle_dir.exists());
	assert!(!release_dir.exists());
}

#[test]
fn encoded_snapshot_outputs_share_one_verified_decode() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let snapshot = alternate_valid_snapshot();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	clear_cached_loaded_base_snapshot(temp.path());
	reset_installed_snapshot_test_counters();

	let installed = super::install_built_snapshot(
		&encoded.bytes,
		BaseDataSource::Build,
		Some("base.bin".to_string()),
		Some(sha256_hex(&encoded.bytes)),
	)
	.expect("install snapshot");
	write_release_artifacts(&encoded.bytes, &temp.path().join("release"), "v-test")
		.expect("write release");
	write_snapshot_bundle(
		&encoded.bytes,
		&temp.path().join("bundle"),
		BaseDataSource::Build,
		Some("base.bin".to_string()),
		Some(sha256_hex(&encoded.bytes)),
	)
	.expect("write bundle");

	assert_eq!(installed_snapshot_cold_decode_count(), 1);
	let loaded = load_installed_base_snapshot("eu4", "schema-test", None)
		.expect("load installed snapshot")
		.expect("snapshot exists");
	assert!(Arc::ptr_eq(&installed.snapshot, &loaded.snapshot));
	assert_eq!(installed_snapshot_cold_decode_count(), 1);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn installed_snapshot_writer_blocks_while_publication_guard_is_held() {
	use std::sync::mpsc::{self, RecvTimeoutError};
	use std::thread;
	use std::time::Duration;

	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let original = sample_snapshot_with_contract();
	let original_encoded = encode_snapshot_to_bytes(&original).expect("encode original snapshot");
	let original_metadata = metadata_for_test_snapshot(&original, &original_encoded.bytes);
	write_test_installed_snapshot(&original_metadata, &original_encoded.bytes)
		.expect("install original snapshot");
	let identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read original identity")
		.expect("original identity exists");
	let publication_guard =
		lock_and_validate_installed_base_snapshot_identity("eu4", "schema-test", &identity)
			.expect("lock original snapshot for publication");

	let replacement = alternate_valid_snapshot();
	let replacement_encoded =
		encode_snapshot_to_bytes(&replacement).expect("encode replacement snapshot");
	let replacement_metadata = metadata_for_test_snapshot(&replacement, &replacement_encoded.bytes);
	let started = Arc::new(Barrier::new(2));
	let worker_started = Arc::clone(&started);
	let (completed_tx, completed_rx) = mpsc::channel();
	let writer = thread::spawn(move || {
		worker_started.wait();
		let result =
			write_test_installed_snapshot(&replacement_metadata, &replacement_encoded.bytes)
				.map(|_| ());
		completed_tx.send(result).expect("report writer result");
	});

	started.wait();
	assert!(
		matches!(
			completed_rx.recv_timeout(Duration::from_millis(100)),
			Err(RecvTimeoutError::Timeout)
		),
		"installer completed while the publication guard held a shared lock"
	);
	drop(publication_guard);
	completed_rx
		.recv_timeout(Duration::from_secs(2))
		.expect("installer should resume after publication guard release")
		.expect("install replacement snapshot");
	writer.join().expect("join snapshot writer");

	let replacement_identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read replacement identity")
		.expect("replacement identity exists");
	assert_ne!(identity.sha256(), replacement_identity.sha256());

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn installed_snapshot_writes_are_serialized() {
	use std::sync::mpsc::{self, RecvTimeoutError};
	use std::thread;
	use std::time::Duration;

	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let first_writer_guard = lock_installed_base_snapshot_for_install("eu4", "schema-test")
		.expect("lock first snapshot installation");
	let snapshot = sample_snapshot_with_contract();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let metadata = metadata_for_test_snapshot(&snapshot, &encoded.bytes);
	let started = Arc::new(Barrier::new(2));
	let worker_started = Arc::clone(&started);
	let (completed_tx, completed_rx) = mpsc::channel();
	let writer = thread::spawn(move || {
		worker_started.wait();
		let result = write_test_installed_snapshot(&metadata, &encoded.bytes).map(|_| ());
		completed_tx.send(result).expect("report writer result");
	});

	started.wait();
	assert!(
		matches!(
			completed_rx.recv_timeout(Duration::from_millis(100)),
			Err(RecvTimeoutError::Timeout)
		),
		"second installer completed while the first held the exclusive lock"
	);
	drop(first_writer_guard);
	completed_rx
		.recv_timeout(Duration::from_secs(2))
		.expect("second installer should resume after exclusive lock release")
		.expect("install snapshot");
	writer.join().expect("join snapshot writer");

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
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

	let err = load_installed_base_snapshot("eu4", "schema-test", None)
		.expect_err("old schema should be rejected");
	assert!(err.contains("base data schema mismatch"));

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn load_installed_base_snapshot_reuses_decoded_snapshot() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let first = load_installed_base_snapshot("eu4", "schema-test", None)
		.expect("load snapshot")
		.expect("snapshot exists");
	let second = load_installed_base_snapshot("eu4", "schema-test", None)
		.expect("load snapshot")
		.expect("snapshot exists");
	assert!(Arc::ptr_eq(&installed.snapshot, &first.snapshot));
	assert!(Arc::ptr_eq(&first.snapshot, &second.snapshot));

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn installed_base_snapshot_identity_supplies_verified_bytes_to_load() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	reset_installed_snapshot_test_counters();

	let identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read snapshot identity")
		.expect("snapshot identity exists");
	assert_eq!(identity.verified.bytes.len(), encoded.bytes.len());
	assert_eq!(installed_snapshot_current_digest_count(), 0);
	assert_eq!(installed_snapshot_current_validation_count(), 0);
	let loaded = load_installed_base_snapshot("eu4", "schema-test", Some(&identity))
		.expect("load snapshot")
		.expect("snapshot exists");

	assert_eq!(
		identity.to_string(),
		format!("sha256:{}", sha256_hex(&encoded.bytes))
	);
	assert_eq!(installed_snapshot_file_read_count(), 1);
	#[cfg(unix)]
	assert_eq!(installed_snapshot_current_digest_count(), 0);
	#[cfg(not(unix))]
	assert_eq!(installed_snapshot_current_digest_count(), 1);
	assert_eq!(installed_snapshot_current_validation_count(), 1);
	assert!(Arc::ptr_eq(&installed.snapshot, &loaded.snapshot));

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn explicit_snapshot_identity_transfers_across_threads_without_rereading() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	clear_cached_loaded_base_snapshot(&snapshot_path);
	reset_installed_snapshot_test_counters();

	let identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read snapshot identity")
		.expect("snapshot identity exists");
	let expected_label = identity.to_string();
	let loaded = std::thread::spawn(move || {
		load_installed_base_snapshot("eu4", "schema-test", Some(&identity))
			.expect("load snapshot on another thread")
			.expect("snapshot exists")
	})
	.join()
	.expect("identity transfer worker");

	assert_eq!(
		expected_label,
		format!("sha256:{}", sha256_hex(&encoded.bytes))
	);
	assert_eq!(installed_snapshot_file_read_count(), 1);
	assert_eq!(installed_snapshot_cold_decode_count(), 1);
	assert_eq!(loaded.snapshot.inventory_paths, snapshot.inventory_paths);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn explicit_snapshot_identity_rejects_current_to_legacy_path_switch() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read snapshot identity")
		.expect("snapshot identity exists");
	reset_installed_snapshot_test_counters();
	let current_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	let legacy_path = installed.install_dir.join("snapshot.bin.gz");
	std::fs::rename(&current_path, &legacy_path).expect("switch to legacy snapshot path");

	let err = load_installed_base_snapshot("eu4", "schema-test", Some(&identity))
		.expect_err("path switch must invalidate the staged identity");
	assert!(err.contains("changed after identity verification"), "{err}");
	assert!(err.contains(&identity.to_string()), "{err}");
	assert_eq!(installed_snapshot_cold_decode_count(), 0);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn decoded_snapshot_cache_is_keyed_by_content_sha_across_install_paths() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	let root_a = temp.path().join("a");
	let root_b = temp.path().join("b");
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
		vocabulary_manifest_sha256: None,
	};

	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, &root_a);
	}
	let installed_a =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot A");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, &root_b);
	}
	write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot B");
	clear_cached_loaded_base_snapshot(&installed_a.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME));
	reset_installed_snapshot_test_counters();

	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, &root_a);
	}
	let identity_a = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read identity A")
		.expect("identity A exists");
	let loaded_a = load_installed_base_snapshot("eu4", "schema-test", Some(&identity_a))
		.expect("load snapshot A")
		.expect("snapshot A exists");

	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, &root_b);
	}
	let identity_b = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read identity B")
		.expect("identity B exists");
	let loaded_b = load_installed_base_snapshot("eu4", "schema-test", Some(&identity_b))
		.expect("load snapshot B")
		.expect("snapshot B exists");

	assert_eq!(identity_a.to_string(), identity_b.to_string());
	assert!(Arc::ptr_eq(&loaded_a.snapshot, &loaded_b.snapshot));
	assert_eq!(installed_snapshot_cold_decode_count(), 1);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn explicit_identity_rejects_snapshot_changed_before_load() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	reset_installed_snapshot_test_counters();
	let identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read snapshot identity")
		.expect("snapshot identity exists");

	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	tamper_snapshot_preserving_len_and_mtime(&snapshot_path);

	let err = load_installed_base_snapshot("eu4", "schema-test", Some(&identity))
		.expect_err("changed snapshot must invalidate the explicit identity");
	assert!(err.contains("changed after identity verification"), "{err}");
	assert!(err.contains("retry"), "{err}");
	assert_eq!(installed_snapshot_file_read_count(), 1);
	assert_eq!(installed_snapshot_cold_decode_count(), 0);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn current_content_digest_rejects_same_file_same_len_and_mtime_replacement() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read snapshot identity")
		.expect("snapshot identity exists");
	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	tamper_snapshot_preserving_len_and_mtime(&snapshot_path);
	reset_installed_snapshot_test_counters();

	assert!(
		!verified_snapshot_content_matches(&snapshot_path, &identity.verified)
			.expect("compare current snapshot digest")
	);
	assert_eq!(installed_snapshot_current_digest_count(), 1);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn public_identity_validation_rejects_changed_content() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read snapshot identity")
		.expect("snapshot identity exists");
	validate_installed_base_snapshot_identity("eu4", "schema-test", &identity)
		.expect("identity must initially be current");

	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	tamper_snapshot_preserving_len_and_mtime(&snapshot_path);
	let err = validate_installed_base_snapshot_identity("eu4", "schema-test", &identity)
		.expect_err("changed content must invalidate identity");
	assert!(err.contains("changed after identity verification"), "{err}");
	assert!(err.contains(&identity.to_string()), "{err}");

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn load_rejects_valid_snapshot_replacement_after_identity() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let original = sample_snapshot_with_contract();
	let original_encoded = encode_snapshot_to_bytes(&original).expect("encode original snapshot");
	let replacement = alternate_valid_snapshot();
	let replacement_encoded =
		encode_snapshot_to_bytes(&replacement).expect("encode replacement snapshot");
	let metadata = InstalledBaseDataMetadata {
		schema_version: BASE_DATA_SCHEMA_VERSION,
		game: original.game.clone(),
		game_version: original.game_version.clone(),
		analysis_rules_version: analysis_rules_version().to_string(),
		generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
		source: BaseDataSource::Build,
		asset_name: None,
		sha256: None,
		vocabulary_manifest_sha256: None,
	};
	let installed = write_test_installed_snapshot(&metadata, &original_encoded.bytes)
		.expect("install original snapshot");
	reset_installed_snapshot_test_counters();
	let original_identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read original identity")
		.expect("original identity exists");
	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	std::fs::write(&snapshot_path, &replacement_encoded.bytes)
		.expect("replace snapshot with valid bytes");

	let err = load_installed_base_snapshot("eu4", "schema-test", Some(&original_identity))
		.expect_err("load must not return replacement under original identity");
	assert!(err.contains("changed after identity verification"), "{err}");
	assert!(err.contains(&original_identity.to_string()), "{err}");
	assert!(err.contains("retry"), "{err}");
	assert_eq!(installed_snapshot_file_read_count(), 1);
	assert_eq!(installed_snapshot_cold_decode_count(), 0);

	let replacement_identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read replacement identity")
		.expect("replacement identity exists");
	assert_ne!(original_identity, replacement_identity);
	let loaded = load_installed_base_snapshot("eu4", "schema-test", Some(&replacement_identity))
		.expect("load replacement after rebuilding identity")
		.expect("replacement exists");
	assert_eq!(loaded.snapshot.inventory_paths, replacement.inventory_paths);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn explicit_identity_rejects_valid_replacement_during_decode() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let original = sample_snapshot_with_contract();
	let original_encoded = encode_snapshot_to_bytes(&original).expect("encode original snapshot");
	let replacement = alternate_valid_snapshot();
	let replacement_encoded =
		encode_snapshot_to_bytes(&replacement).expect("encode replacement snapshot");
	let metadata = InstalledBaseDataMetadata {
		schema_version: BASE_DATA_SCHEMA_VERSION,
		game: original.game.clone(),
		game_version: original.game_version.clone(),
		analysis_rules_version: analysis_rules_version().to_string(),
		generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
		source: BaseDataSource::Build,
		asset_name: None,
		sha256: None,
		vocabulary_manifest_sha256: None,
	};
	let installed = write_test_installed_snapshot(&metadata, &original_encoded.bytes)
		.expect("install original snapshot");
	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	clear_cached_loaded_base_snapshot(&snapshot_path);
	reset_installed_snapshot_test_counters();
	let original_identity = format!("sha256:{}", sha256_hex(&original_encoded.bytes));
	let decode_gate = install_installed_snapshot_decode_gate();

	let worker = std::thread::spawn(|| {
		let identity = installed_base_snapshot_identity("eu4", "schema-test")
			.expect("read original identity")
			.expect("original identity exists");
		let result = load_installed_base_snapshot("eu4", "schema-test", Some(&identity));
		(identity, result)
	});
	decode_gate.wait_until_entered(1);
	assert_eq!(installed_snapshot_cold_decode_count(), 1);
	std::fs::write(&snapshot_path, &replacement_encoded.bytes)
		.expect("replace snapshot while original is decoding");
	decode_gate.release();

	let (worker_identity, result) = worker.join().expect("load worker");
	assert_eq!(worker_identity.to_string(), original_identity);
	let err = result.expect_err("load must not consume replacement after decoding original");
	assert!(err.contains("changed after identity verification"), "{err}");
	assert!(err.contains(&original_identity), "{err}");
	assert_eq!(installed_snapshot_file_read_count(), 1);
	assert_eq!(installed_snapshot_cold_decode_count(), 1);

	let replacement_identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read replacement identity")
		.expect("replacement identity exists");
	assert_ne!(original_identity, replacement_identity.to_string());
	let loaded = load_installed_base_snapshot("eu4", "schema-test", Some(&replacement_identity))
		.expect("load replacement after retry")
		.expect("replacement exists");
	assert_eq!(loaded.snapshot.inventory_paths, replacement.inventory_paths);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn explicit_snapshot_identities_do_not_cross_between_threads() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let original = sample_snapshot_with_contract();
	let original_encoded = encode_snapshot_to_bytes(&original).expect("encode original snapshot");
	let replacement = alternate_valid_snapshot();
	let replacement_encoded =
		encode_snapshot_to_bytes(&replacement).expect("encode replacement snapshot");
	let metadata = InstalledBaseDataMetadata {
		schema_version: BASE_DATA_SCHEMA_VERSION,
		game: original.game.clone(),
		game_version: original.game_version.clone(),
		analysis_rules_version: analysis_rules_version().to_string(),
		generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
		source: BaseDataSource::Build,
		asset_name: None,
		sha256: None,
		vocabulary_manifest_sha256: None,
	};
	let installed = write_test_installed_snapshot(&metadata, &original_encoded.bytes)
		.expect("install original snapshot");
	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	clear_cached_loaded_base_snapshot(&snapshot_path);
	reset_installed_snapshot_test_counters();

	let (original_ready_tx, original_ready_rx) = std::sync::mpsc::channel();
	let (load_original_tx, load_original_rx) = std::sync::mpsc::channel();
	let original_worker = std::thread::spawn(move || {
		let identity = installed_base_snapshot_identity("eu4", "schema-test")
			.expect("read original identity")
			.expect("original identity exists");
		original_ready_tx
			.send(identity.clone())
			.expect("signal original identity");
		load_original_rx.recv().expect("wait to load original");
		let result = load_installed_base_snapshot("eu4", "schema-test", Some(&identity));
		(identity, result)
	});
	let original_identity = original_ready_rx.recv().expect("receive original identity");
	std::fs::write(&snapshot_path, &replacement_encoded.bytes)
		.expect("replace snapshot with valid bytes");
	let replacement_identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read replacement identity")
		.expect("replacement identity exists");
	assert_ne!(original_identity, replacement_identity);
	load_original_tx.send(()).expect("start original load");

	let (worker_identity, original_result) = original_worker.join().expect("original worker");
	assert_eq!(worker_identity, original_identity);
	let err = original_result.expect_err("original load must not accept replacement identity");
	assert!(err.contains("changed after identity verification"), "{err}");
	assert!(err.contains(&original_identity.to_string()), "{err}");
	let loaded = load_installed_base_snapshot("eu4", "schema-test", Some(&replacement_identity))
		.expect("load replacement identity")
		.expect("replacement exists");
	assert_eq!(loaded.snapshot.inventory_paths, replacement.inventory_paths);
	assert_eq!(installed_snapshot_file_read_count(), 2);
	// Each captured digest is decoded once; the stale original is rejected by
	// its single final current-content check rather than a pre-decode hash.
	assert_eq!(installed_snapshot_cold_decode_count(), 2);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn install_built_snapshot_uses_encoded_bytes_as_the_single_source_of_truth() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let encoded_snapshot = alternate_valid_snapshot();
	let encoded = encode_snapshot_to_bytes(&encoded_snapshot).expect("encode alternate snapshot");
	let installed =
		super::install_built_snapshot(&encoded.bytes, BaseDataSource::Build, None, None)
			.expect("install encoded snapshot");
	let identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read installed identity")
		.expect("installed identity exists");
	let loaded = load_installed_base_snapshot("eu4", "schema-test", Some(&identity))
		.expect("load installed alternate snapshot")
		.expect("alternate snapshot exists");
	assert!(Arc::ptr_eq(&installed.snapshot, &loaded.snapshot));
	assert_eq!(
		loaded.snapshot.inventory_paths,
		encoded_snapshot.inventory_paths
	);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn encoded_snapshot_installer_validates_release_contract() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let snapshot = sample_snapshot_with_contract();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let sha256 = sha256_hex(&encoded.bytes);
	let expected = super::EncodedSnapshotExpectations {
		game: Some("eu4"),
		game_version: Some("schema-test"),
		analysis_rules_version: Some(analysis_rules_version()),
		sha256: Some(&sha256),
	};
	for invalid in [
		super::EncodedSnapshotExpectations {
			game: Some("ck3"),
			..expected
		},
		super::EncodedSnapshotExpectations {
			game_version: Some("wrong-version"),
			..expected
		},
		super::EncodedSnapshotExpectations {
			analysis_rules_version: Some("wrong-rules"),
			..expected
		},
		super::EncodedSnapshotExpectations {
			sha256: Some("wrong-sha256"),
			..expected
		},
	] {
		super::install_encoded_snapshot(
			&encoded.bytes,
			BaseDataSource::Download,
			Some("release.bin".to_string()),
			invalid,
		)
		.expect_err("invalid release contract must fail");
	}

	let mut old_schema = snapshot;
	old_schema.schema_version = BASE_DATA_SCHEMA_VERSION - 1;
	let old_schema = encode_snapshot_to_bytes(&old_schema).expect("encode old schema snapshot");
	let schema_err = super::install_encoded_snapshot(
		&old_schema.bytes,
		BaseDataSource::Download,
		Some("release.bin".to_string()),
		super::EncodedSnapshotExpectations {
			sha256: Some(&sha256_hex(&old_schema.bytes)),
			..expected
		},
	)
	.expect_err("old schema must fail");
	assert!(schema_err.contains("schema mismatch"), "{schema_err}");

	let installed = super::install_encoded_snapshot(
		&encoded.bytes,
		BaseDataSource::Download,
		Some("release.bin".to_string()),
		expected,
	)
	.expect("install validated release bytes");
	assert_eq!(installed.metadata.source, BaseDataSource::Download);
	assert_eq!(installed.metadata.sha256.as_deref(), Some(sha256.as_str()));

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn load_installed_base_snapshot_caches_concurrent_decode_error() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	clear_cached_loaded_base_snapshot(&snapshot_path);
	std::fs::write(&snapshot_path, b"not-a-valid-base-snapshot")
		.expect("write invalid snapshot bytes");
	reset_installed_snapshot_test_counters();
	let decode_gate = install_installed_snapshot_decode_gate();

	let barrier = Arc::new(Barrier::new(3));
	let mut workers = Vec::new();
	for _ in 0..2 {
		let barrier = Arc::clone(&barrier);
		workers.push(std::thread::spawn(move || {
			barrier.wait();
			load_installed_base_snapshot("eu4", "schema-test", None)
				.expect_err("invalid snapshot must fail")
		}));
	}
	barrier.wait();
	decode_gate.wait_until_entered(1);
	assert_eq!(installed_snapshot_cold_decode_count(), 1);
	decode_gate.release();
	let first = workers.remove(0).join().expect("first worker");
	let second = workers.remove(0).join().expect("second worker");

	assert_eq!(first, second);
	assert!(first.contains("failed to parse base data snapshot"));
	assert_eq!(installed_snapshot_cold_decode_count(), 1);
	let third = load_installed_base_snapshot("eu4", "schema-test", None)
		.expect_err("cached invalid snapshot must still fail");
	assert_eq!(third, first);
	assert_eq!(installed_snapshot_cold_decode_count(), 1);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn decoded_snapshot_cache_never_evicts_in_flight_cells() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	clear_cached_loaded_base_snapshot(&temp.path().join("clear-all"));
	reset_installed_snapshot_test_counters();
	let decode_gate = install_installed_snapshot_decode_gate();
	let start = Arc::new(Barrier::new(3));
	let bytes_a = Arc::<[u8]>::from(&b"invalid-a"[..]);
	let bytes_b = Arc::<[u8]>::from(&b"invalid-b"[..]);
	let sha_a = sha256_hex(&bytes_a);
	let sha_b = sha256_hex(&bytes_b);

	let first_a = {
		let start = Arc::clone(&start);
		let bytes = Arc::clone(&bytes_a);
		let sha = sha_a.clone();
		std::thread::spawn(move || {
			start.wait();
			decode_cached_base_snapshot(&sha, &bytes)
		})
	};
	let first_b = {
		let start = Arc::clone(&start);
		let bytes = Arc::clone(&bytes_b);
		let sha = sha_b.clone();
		std::thread::spawn(move || {
			start.wait();
			decode_cached_base_snapshot(&sha, &bytes)
		})
	};
	start.wait();
	decode_gate.wait_until_entered(2);

	let (started_tx, started_rx) = std::sync::mpsc::channel();
	let second_a = {
		let bytes = Arc::clone(&bytes_a);
		let sha = sha_a.clone();
		std::thread::spawn(move || {
			started_tx.send(()).expect("signal second A");
			decode_cached_base_snapshot(&sha, &bytes)
		})
	};
	started_rx.recv().expect("second A started");
	decode_gate.release();

	let first_a = first_a.join().expect("first A worker");
	let first_b = first_b.join().expect("first B worker");
	let second_a = second_a.join().expect("second A worker");
	let first_a = first_a.expect_err("invalid A must fail");
	assert!(first_b.is_err());
	let second_a = second_a.expect_err("second invalid A must fail");
	assert_eq!(first_a, second_a);
	assert_eq!(installed_snapshot_cold_decode_count(), 2);
}

#[test]
fn decoded_snapshot_cache_bounds_completed_entries() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	clear_cached_loaded_base_snapshot(&snapshot_path);

	for bytes in [b"invalid-one".as_slice(), b"invalid-two", b"invalid-three"] {
		std::fs::write(&snapshot_path, bytes).expect("replace invalid snapshot bytes");
		load_installed_base_snapshot("eu4", "schema-test", None)
			.expect_err("invalid snapshot must fail");
		assert!(loaded_base_snapshot_cache_completed_count() <= 1);
	}

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn installed_base_snapshot_identity_reports_stale_metadata() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let metadata_path = installed
		.install_dir
		.join(super::INSTALLED_METADATA_FILE_NAME);

	let old_schema = InstalledBaseDataMetadata {
		schema_version: BASE_DATA_SCHEMA_VERSION - 1,
		..metadata.clone()
	};
	std::fs::write(
		&metadata_path,
		serde_json::to_string_pretty(&old_schema).expect("serialize old schema metadata"),
	)
	.expect("write old schema metadata");
	let schema_err = installed_base_snapshot_identity("eu4", "schema-test")
		.expect_err("stale schema must be reported");
	assert_eq!(
		schema_err,
		stale_installed_base_data_message(
			"eu4",
			"schema-test",
			&format!(
				"base data schema mismatch: expected {}, found {}",
				BASE_DATA_SCHEMA_VERSION,
				BASE_DATA_SCHEMA_VERSION - 1
			),
		)
	);

	let stale_rules = "stale-analysis-rules";
	let old_rules = InstalledBaseDataMetadata {
		analysis_rules_version: stale_rules.to_string(),
		..metadata
	};
	std::fs::write(
		&metadata_path,
		serde_json::to_string_pretty(&old_rules).expect("serialize old rules metadata"),
	)
	.expect("write old rules metadata");
	let rules_err = installed_base_snapshot_identity("eu4", "schema-test")
		.expect_err("stale analysis rules must be reported");
	assert_eq!(
		rules_err,
		stale_installed_base_data_message(
			"eu4",
			"schema-test",
			&format!(
				"base data analysis rules version mismatch: expected {}, found {}",
				analysis_rules_version(),
				stale_rules
			),
		)
	);

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn load_installed_base_snapshot_decodes_cold_content_once() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	clear_cached_loaded_base_snapshot(&snapshot_path);
	reset_installed_snapshot_test_counters();
	let decode_gate = install_installed_snapshot_decode_gate();

	let barrier = Arc::new(Barrier::new(3));
	let mut workers = Vec::new();
	for _ in 0..2 {
		let barrier = Arc::clone(&barrier);
		workers.push(std::thread::spawn(move || {
			barrier.wait();
			load_installed_base_snapshot("eu4", "schema-test", None)
				.expect("load snapshot")
				.expect("snapshot exists")
		}));
	}
	barrier.wait();
	decode_gate.wait_until_entered(1);
	assert_eq!(installed_snapshot_cold_decode_count(), 1);
	decode_gate.release();
	let first = workers.remove(0).join().expect("first worker");
	let second = workers.remove(0).join().expect("second worker");

	assert_eq!(installed_snapshot_cold_decode_count(), 1);
	assert!(Arc::ptr_eq(&first.snapshot, &second.snapshot));

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn installed_base_snapshot_identity_tracks_bytes_and_invalidates_decoded_cache() {
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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let original_identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read snapshot identity")
		.expect("snapshot identity exists");
	let loaded = load_installed_base_snapshot("eu4", "schema-test", Some(&original_identity))
		.expect("load snapshot")
		.expect("snapshot exists");
	assert!(Arc::ptr_eq(&installed.snapshot, &loaded.snapshot));

	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	tamper_snapshot_preserving_len_and_mtime(&snapshot_path);

	let tampered_identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read tampered snapshot identity")
		.expect("tampered snapshot identity exists");
	assert_ne!(original_identity, tampered_identity);
	let err = load_installed_base_snapshot("eu4", "schema-test", Some(&tampered_identity))
		.expect_err("tampered bytes must not reuse decoded snapshot");
	assert!(err.contains("failed to parse base data snapshot"));

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn installed_base_snapshot_identity_verifies_metadata_sha256() {
	let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
	let temp = TempDir::new().expect("temp dir");
	unsafe {
		std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
	}

	let snapshot = sample_snapshot_with_contract();
	let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
	let expected_sha256 = sha256_hex(&encoded.bytes);
	let metadata = InstalledBaseDataMetadata {
		schema_version: BASE_DATA_SCHEMA_VERSION,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: analysis_rules_version().to_string(),
		generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
		source: BaseDataSource::Download,
		asset_name: Some("snapshot.bin".to_string()),
		sha256: Some(expected_sha256.clone()),
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
	let identity = installed_base_snapshot_identity("eu4", "schema-test")
		.expect("read snapshot identity")
		.expect("snapshot identity exists");
	assert_eq!(identity.to_string(), format!("sha256:{expected_sha256}"));
	load_installed_base_snapshot("eu4", "schema-test", Some(&identity))
		.expect("load verified snapshot identity")
		.expect("snapshot exists");

	let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	tamper_snapshot_preserving_len_and_mtime(&snapshot_path);

	let identity_err = installed_base_snapshot_identity("eu4", "schema-test")
		.expect_err("identity must verify metadata SHA-256");
	assert!(identity_err.contains("SHA256 verification failed"));
	let load_err = load_installed_base_snapshot("eu4", "schema-test", None)
		.expect_err("load must verify metadata SHA-256 before cache reuse");
	assert!(load_err.contains("SHA256 verification failed"));

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
		vocabulary_manifest_sha256: None,
	};
	let installed =
		write_test_installed_snapshot(&metadata, &encoded.bytes).expect("install snapshot");
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

	let err = load_installed_base_snapshot("eu4", "schema-test", None)
		.expect_err("stale metadata should short-circuit before decode");
	assert!(err.contains("base data schema mismatch"));
	assert!(!err.contains("failed to parse base data snapshot"));

	unsafe {
		std::env::remove_var(BASE_DATA_DIR_ENV);
	}
}

#[test]
fn base_symbol_definition_defaults_missing_param_contract() {
	test_support::install_defaults();
	let raw = serde_json::json!({
		"kind": "ScriptedEffect",
		"name": "test.effect",
		"module": "test",
		"local_name": "test_effect",
		"path": "common/scripted_effects/test.txt",
		"line": 1,
		"column": 1,
		"scope_id": 0,
		"declared_this_type": MaybeScope::Known(base_scope::country()),
		"inferred_this_type": MaybeScope::Known(base_scope::country()),
		"inferred_this_mask": country_mask(),
		"required_params": []
	});
	let decoded: BaseSymbolDefinition =
		serde_json::from_value(raw).expect("deserialize base symbol definition");
	assert!(decoded.param_contract.is_none());
	assert_eq!(decoded.inferred_this_mask, country_mask());
}
