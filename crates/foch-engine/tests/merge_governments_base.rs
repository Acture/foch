//! Hermetic base-aware definition-module merge coverage.
//!
//! This is a separate integration-test process because it overrides the base
//! data and modset cache roots for a synthetic EU4 installation.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use foch_core::domain::game::Game;
use foch_core::model::MergeReportStatus;
use foch_engine::{
	BaseDataSource, CheckRequest, Config, FileFilter, MergeExecuteOptions, build_base_snapshot,
	install_built_snapshot, run_merge_with_options,
};
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::parse_script_file;
use tempfile::TempDir;

fn write_file(root: &Path, relative: &str, content: &str) {
	let path = root.join(relative);
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).expect("create fixture parent");
	}
	fs::write(path, content).expect("write fixture file");
}

fn write_mod(playset_root: &Path, id: &str, name: &str, relative: &str, content: &str) {
	let mod_root = playset_root.join("mods").join(id);
	write_file(
		&mod_root,
		"descriptor.mod",
		&format!("name=\"{name}\"\nversion=\"1.0.0\"\n"),
	);
	write_file(&mod_root, relative, content);
	write_file(
		playset_root,
		&format!("mod/ugc_{id}.mod"),
		&format!("name=\"{name}\"\npath=\"mods/{id}\"\nremote_file_id=\"{id}\"\n"),
	);
}

fn government_reforms(path: &Path, root: &Path) -> BTreeMap<String, String> {
	let parsed = parse_script_file("generated", root, path).expect("parse generated module");
	parsed
		.ast
		.statements
		.iter()
		.filter_map(|statement| {
			let AstStatement::Assignment { key, value, .. } = statement else {
				return None;
			};
			let AstValue::Block { items, .. } = value else {
				return None;
			};
			let reform = items.iter().find_map(|item| match item {
				AstStatement::Assignment {
					key,
					value: AstValue::Scalar { value, .. },
					..
				} if key == "basic_reform" => Some(value.as_text()),
				_ => None,
			})?;
			Some((key.clone(), reform))
		})
		.collect()
}

#[test]
fn retained_governments_merge_includes_complete_version_bound_base_module() {
	let temp = TempDir::new().expect("temp dir");
	let data_root = temp.path().join("base-data");
	let cache_root = temp.path().join("modset-cache");
	let game_root = temp.path().join("eu4-game");
	let playset_root = temp.path().join("playset");
	let out_dir = temp.path().join("out");
	fs::create_dir_all(&game_root).expect("create game root");
	fs::create_dir_all(&playset_root).expect("create playset root");
	write_file(&game_root, "version.txt", "test-1.0\n");
	write_file(
		&game_root,
		"common/governments/00_vanilla.txt",
		concat!(
			"vanilla_only = { basic_reform = vanilla_reform }\n",
			"shared = { basic_reform = vanilla_early_reform }\n",
		),
	);
	write_file(
		&game_root,
		"common/governments/50_vanilla.txt",
		"shared = { basic_reform = vanilla_late_reform }\n",
	);
	write_mod(
		&playset_root,
		"1001",
		"Override",
		"common/governments/zzz_10_override.txt",
		concat!(
			"shared = { basic_reform = mod_override_reform }\n",
			"mod_only = { basic_reform = mod_only_reform }\n",
		),
	);
	write_mod(
		&playset_root,
		"1002",
		"Sibling",
		"common/governments/zzz_20_sibling.txt",
		"sibling_only = { basic_reform = sibling_reform }\n",
	);
	write_file(
		&playset_root,
		"dlc_load.json",
		"{\n\t\"enabled_mods\": [\"mod/ugc_1001.mod\", \"mod/ugc_1002.mod\"],\n\t\"disabled_dlcs\": []\n}\n",
	);

	unsafe {
		std::env::set_var("FOCH_DATA_DIR", &data_root);
		std::env::set_var("FOCH_MODSET_CACHE_DIR", &cache_root);
	}
	let game = Game::EuropaUniversalis4;
	let built = build_base_snapshot(
		&game,
		&game_root,
		Some("test-1.0"),
		&FileFilter::for_game(game.clone()),
	)
	.expect("build synthetic base snapshot");
	install_built_snapshot(
		&built.encoded_snapshot,
		BaseDataSource::Build,
		Some(built.snapshot_asset_name),
		Some(built.snapshot_sha256),
	)
	.expect("install synthetic base snapshot");

	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_root.clone());
	let request = CheckRequest::from_playset_path(
		playset_root.join("dlc_load.json"),
		Config {
			steam_root_path: None,
			paradox_data_path: None,
			game_path,
			extra_ignore_patterns: Vec::new(),
		},
	);
	let result = run_merge_with_options(
		request,
		MergeExecuteOptions {
			out_dir: out_dir.clone(),
			include_game_base: true,
			include_base: false,
			gui_scroll_merge: false,
			force: false,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_config_path: None,
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			playset_fingerprint: None,
			provenance: false,
			retained_paths: Some(BTreeSet::from([
				"common/governments/zzz_10_override.txt".to_string()
			])),
		},
	)
	.expect("merge synthetic base-aware module");

	assert_eq!(result.report.status, MergeReportStatus::Ready);
	assert_eq!(result.report.cache_source, None);
	assert_eq!(result.report.definition_module_count, 1);
	assert_eq!(result.report.definition_module_generated_count, 1);
	let merged_path = out_dir.join("common/governments/zzz_foch_governments.txt");
	assert_eq!(
		government_reforms(&merged_path, &out_dir),
		BTreeMap::from([
			("mod_only".to_string(), "mod_only_reform".to_string()),
			("shared".to_string(), "mod_override_reform".to_string()),
			("sibling_only".to_string(), "sibling_reform".to_string()),
			("vanilla_only".to_string(), "vanilla_reform".to_string()),
		])
	);
	let emitted_files = fs::read_dir(out_dir.join("common/governments"))
		.expect("read module output directory")
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
		.collect::<Vec<_>>();
	assert_eq!(emitted_files.len(), 1);
	let descriptor = fs::read_to_string(out_dir.join("descriptor.mod")).expect("read descriptor");
	assert!(descriptor.contains("replace_path=\"common/governments\""));

	unsafe {
		std::env::remove_var("FOCH_DATA_DIR");
		std::env::remove_var("FOCH_MODSET_CACHE_DIR");
	}
}
