use foch::check::model::{
	CheckRequest, MergePlanEntry, MergePlanOptions, MergePlanStrategy, MergeReport,
	MergeReportStatus, MergeReportValidation, RunOptions,
};
use foch::check::{run_checks, run_checks_with_options, run_merge_plan_with_options};
use foch::cli::config::Config;
use serde_json::json;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn write_playlist(path: &Path, mods: serde_json::Value) {
	let playlist = json!({
		"game": "eu4",
		"name": "test-playset",
		"mods": mods,
	});
	fs::write(
		path,
		serde_json::to_string_pretty(&playlist).expect("serialize playlist"),
	)
	.expect("write playlist");
}

fn write_descriptor(mod_root: &Path, name: &str, dependencies: &[&str]) {
	fs::create_dir_all(mod_root).expect("create mod root");
	let mut descriptor = format!("name=\"{name}\"\nversion=\"1.0.0\"\n");
	if !dependencies.is_empty() {
		descriptor.push_str("dependencies={\n");
		for dependency in dependencies {
			descriptor.push_str(&format!("\t\"{dependency}\"\n"));
		}
		descriptor.push_str("}\n");
	}
	fs::write(mod_root.join("descriptor.mod"), descriptor).expect("write descriptor");
}

fn write_script_file(mod_root: &Path, relative: &str, content: &str) {
	let script_path = mod_root.join(relative);
	if let Some(parent) = script_path.parent() {
		fs::create_dir_all(parent).expect("create script parent");
	}
	fs::write(script_path, content).expect("write script file");
}

fn write_ugc_metadata(paradox_game_dir: &Path, steam_id: &str, target_path: &Path) {
	let mod_dir = paradox_game_dir.join("mod");
	fs::create_dir_all(&mod_dir).expect("create mod metadata dir");
	let content = format!(
		"name=\"ugc-{steam_id}\"\npath=\"{}\"\nremote_file_id=\"{steam_id}\"\n",
		target_path.display()
	);
	fs::write(mod_dir.join(format!("ugc_{steam_id}.mod")), content).expect("write ugc metadata");
}

fn request_for(playlist_path: &Path) -> CheckRequest {
	let game_root = playlist_path
		.parent()
		.expect("playlist parent")
		.join("eu4-game");
	fs::create_dir_all(&game_root).expect("create default game root");
	let mut game_path = std::collections::HashMap::new();
	game_path.insert("eu4".to_string(), game_root);
	CheckRequest {
		playset_path: playlist_path.to_path_buf(),
		config: Config {
			steam_root_path: None,
			paradox_data_path: None,
			game_path,
		},
	}
}

fn request_with_config(playlist_path: &Path, config: Config) -> CheckRequest {
	CheckRequest {
		playset_path: playlist_path.to_path_buf(),
		config,
	}
}

fn plan_entry_for<'a>(result: &'a foch::check::MergePlanResult, path: &str) -> &'a MergePlanEntry {
	result
		.paths
		.iter()
		.find(|entry| entry.path == path)
		.expect("merge plan entry exists")
}

fn run_checks_no_base(request: CheckRequest) -> foch::check::CheckResult {
	run_checks_with_options(
		request,
		RunOptions {
			include_game_base: false,
			..RunOptions::default()
		},
	)
}

fn run_merge_plan_no_base(request: CheckRequest) -> foch::check::MergePlanResult {
	run_merge_plan_with_options(
		request,
		MergePlanOptions {
			include_game_base: false,
		},
	)
}

#[test]
fn invalid_json_creates_r001() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	fs::write(&playlist_path, "{broken").expect("write broken json");

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| f.rule_id == "R001"));
}

#[test]
fn duplicate_steam_id_creates_r003() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"1001"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"1001"}
		]),
	);

	write_descriptor(&temp.path().join("1001"), "mod-a", &[]);

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| f.rule_id == "R003"));
}

#[test]
fn missing_descriptor_creates_r004() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"1002"}
		]),
	);
	fs::create_dir_all(temp.path().join("1002")).expect("create mod dir");

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| f.rule_id == "R004"));
}

#[test]
fn file_conflict_creates_r005() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"2001"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"2002"}
		]),
	);

	let mod_a = temp.path().join("2001");
	write_descriptor(&mod_a, "mod-a", &[]);
	fs::create_dir_all(mod_a.join("common")).expect("create dir");
	fs::write(mod_a.join("common").join("shared.txt"), "from-a").expect("write file");

	let mod_b = temp.path().join("2002");
	write_descriptor(&mod_b, "mod-b", &[]);
	fs::create_dir_all(mod_b.join("common")).expect("create dir");
	fs::write(mod_b.join("common").join("shared.txt"), "from-b").expect("write file");

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| f.rule_id == "R005"));
}

#[test]
fn missing_dependency_creates_r006() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"3001"}
		]),
	);

	let mod_a = temp.path().join("3001");
	write_descriptor(&mod_a, "mod-a", &["mod-b"]);

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| f.rule_id == "R006"));
}

#[test]
fn duplicate_scripted_effect_creates_r007() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7001"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"7002"}
		]),
	);

	let mod_a = temp.path().join("7001");
	write_descriptor(&mod_a, "mod-a", &[]);
	write_script_file(
		&mod_a,
		"common/scripted_effects/effects.txt",
		"shared_effect = {\n\tif = { limit = { always = yes } }\n}\n",
	);

	let mod_b = temp.path().join("7002");
	write_descriptor(&mod_b, "mod-b", &[]);
	write_script_file(
		&mod_b,
		"common/scripted_effects/effects.txt",
		"shared_effect = {\n\thidden_effect = { }\n}\n",
	);

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| f.rule_id == "R007"));
}

#[test]
fn mergeable_cross_mod_overlap_reports_a003_without_s001() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7011"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"7012"}
		]),
	);

	let mod_a = temp.path().join("7011");
	write_descriptor(&mod_a, "mod-a", &[]);
	write_script_file(
		&mod_a,
		"common/scripted_effects/effects.txt",
		"shared_effect = { log = a }\n",
	);

	let mod_b = temp.path().join("7012");
	write_descriptor(&mod_b, "mod-b", &[]);
	write_script_file(
		&mod_b,
		"common/scripted_effects/effects.txt",
		"shared_effect = { log = b }\n",
	);

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| {
		f.rule_id == "A003" && f.message.contains("可自动合并")
	}));
	assert!(!result.findings.iter().any(|f| f.rule_id == "S001"));
}

#[test]
fn non_mergeable_cross_mod_overlap_reports_s001() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7021"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"7022"}
		]),
	);

	let mod_a = temp.path().join("7021");
	write_descriptor(&mod_a, "mod-a", &[]);
	write_script_file(
		&mod_a,
		"common/odd/event_defs.txt",
		"country_event = { id = odd.1 title = odd_title }\n",
	);

	let mod_b = temp.path().join("7022");
	write_descriptor(&mod_b, "mod-b", &[]);
	write_script_file(
		&mod_b,
		"common/odd/event_defs.txt",
		"country_event = { id = odd.1 title = other_title }\n",
	);

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| {
		f.rule_id == "S001" && f.message.contains("跨 Mod 重合定义")
	}));
}

#[test]
fn unresolved_scripted_effect_reports_only_s002() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"8001"}
		]),
	);

	let mod_a = temp.path().join("8001");
	write_descriptor(&mod_a, "mod-a", &[]);
	write_script_file(
		&mod_a,
		"events/events.txt",
		"country_event = {\n\tid = test.1\n\timmediate = {\n\t\tmissing_effect = { FLAG = TEST }\n\t}\n}\n",
	);

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| f.rule_id == "S002"));
	assert!(!result.findings.iter().any(|f| f.rule_id == "R008"));
}

#[test]
fn resolves_mod_root_from_ugc_metadata_when_paradox_root_is_configured() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9101"}
		]),
	);

	let paradox_root = temp.path().join("Paradox Interactive");
	let paradox_game_dir = paradox_root.join("Europa Universalis IV");
	let mod_root = temp.path().join("real-mod-9101");
	write_descriptor(&mod_root, "mod-a", &[]);
	write_ugc_metadata(&paradox_game_dir, "9101", &mod_root);

	let config = Config {
		steam_root_path: None,
		paradox_data_path: Some(paradox_root),
		game_path: std::collections::HashMap::new(),
	};

	let result = run_checks_no_base(request_with_config(&playlist_path, config));
	assert!(
		!result.findings.iter().any(|f| f.rule_id == "R004"),
		"should resolve descriptor.mod through ugc metadata"
	);
}

#[test]
fn resolves_mod_root_from_non_default_steam_library_folder() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9201"}
		]),
	);

	let steam_root = temp.path().join("Steam");
	let lib2 = temp.path().join("SteamLibrary2");
	fs::create_dir_all(steam_root.join("steamapps")).expect("create steamapps");
	fs::write(
		steam_root.join("steamapps").join("libraryfolders.vdf"),
		format!(
			r#""libraryfolders"
{{
	"0"
	{{
		"path"		"{}"
	}}
	"1"
	{{
		"path"		"{}"
	}}
}}"#,
			steam_root.display(),
			lib2.display()
		),
	)
	.expect("write libraryfolders");

	let workshop_mod_root = lib2
		.join("steamapps")
		.join("workshop")
		.join("content")
		.join("236850")
		.join("9201");
	write_descriptor(&workshop_mod_root, "mod-a", &[]);

	let config = Config {
		steam_root_path: Some(steam_root),
		paradox_data_path: None,
		game_path: std::collections::HashMap::new(),
	};

	let result = run_checks_no_base(request_with_config(&playlist_path, config));
	assert!(
		!result.findings.iter().any(|f| f.rule_id == "R004"),
		"should resolve descriptor.mod from steam libraryfolders path"
	);
}

#[test]
fn merge_plan_marks_single_contributor_path_as_copy_through() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	let mod_root = temp.path().join("9401");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9401"}
		]),
	);
	write_descriptor(&mod_root, "mod-a", &[]);
	write_script_file(
		&mod_root,
		"events/a.txt",
		"namespace = test\ncountry_event = { id = test.1 }\n",
	);

	let result = run_merge_plan_no_base(request_for(&playlist_path));
	let entry = plan_entry_for(&result, "events/a.txt");
	assert!(!result.generated_at.is_empty());
	assert_eq!(result.strategies.total_paths, 1);
	assert_eq!(result.strategies.copy_through, 1);
	assert_eq!(entry.strategy, MergePlanStrategy::CopyThrough);
	assert!(!entry.generated);
	assert!(entry.notes.is_empty());
}

#[test]
fn merge_plan_marks_valid_scripted_effect_overlap_as_structural_merge() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	let mod_a = temp.path().join("9501");
	let mod_b = temp.path().join("9502");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9501"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"9502"}
		]),
	);
	write_descriptor(&mod_a, "mod-a", &[]);
	write_descriptor(&mod_b, "mod-b", &[]);
	write_script_file(
		&mod_a,
		"common/scripted_effects/effects.txt",
		"shared_effect = {\n\tif = { limit = { always = yes } }\n}\n",
	);
	write_script_file(
		&mod_b,
		"common/scripted_effects/effects.txt",
		"shared_effect = {\n\thidden_effect = { }\n}\n",
	);

	let result = run_merge_plan_no_base(request_for(&playlist_path));
	let entry = plan_entry_for(&result, "common/scripted_effects/effects.txt");
	assert_eq!(result.strategies.structural_merge, 1);
	assert_eq!(entry.strategy, MergePlanStrategy::StructuralMerge);
	assert_eq!(
		entry.winner.as_ref().expect("winner").mod_id,
		"9502",
		"highest-precedence mod should win ties"
	);
	assert!(!entry.generated);
	assert!(entry.notes.is_empty());
}

#[test]
fn merge_plan_marks_non_normalizable_defines_overlap_as_manual_conflict() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	let mod_a = temp.path().join("9551");
	let mod_b = temp.path().join("9552");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9551"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"9552"}
		]),
	);
	write_descriptor(&mod_a, "mod-a", &[]);
	write_descriptor(&mod_b, "mod-b", &[]);
	write_script_file(
		&mod_a,
		"common/defines/test.txt",
		"NGame = {\n\tSTART_YEAR = 1444\n}\n",
	);
	write_script_file(&mod_b, "common/defines/test.txt", "NGame = {\n\t1445\n}\n");

	let result = run_merge_plan_no_base(request_for(&playlist_path));
	let entry = plan_entry_for(&result, "common/defines/test.txt");
	assert_eq!(result.strategies.manual_conflict, 1);
	assert_eq!(entry.strategy, MergePlanStrategy::ManualConflict);
	assert!(entry.winner.is_none());
	assert!(!entry.generated);
	assert!(
		entry
			.notes
			.iter()
			.any(|note| note.contains("non-normalizable defines"))
	);
}

#[test]
fn merge_plan_marks_invalid_structural_overlap_as_manual_conflict() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	let mod_a = temp.path().join("9601");
	let mod_b = temp.path().join("9602");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9601"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"9602"}
		]),
	);
	write_descriptor(&mod_a, "mod-a", &[]);
	write_descriptor(&mod_b, "mod-b", &[]);
	write_script_file(
		&mod_a,
		"events/shared.txt",
		"namespace = test\ncountry_event = { id = test.1 }\n",
	);
	write_script_file(
		&mod_b,
		"events/shared.txt",
		"namespace = broken\ncountry_event = =\n",
	);

	let result = run_merge_plan_no_base(request_for(&playlist_path));
	let entry = plan_entry_for(&result, "events/shared.txt");
	assert_eq!(result.strategies.manual_conflict, 1);
	assert_eq!(entry.strategy, MergePlanStrategy::ManualConflict);
	assert!(entry.winner.is_none());
	assert!(!entry.generated);
	assert!(!entry.notes.is_empty());
}

#[test]
fn merge_plan_marks_ui_overlap_as_manual_conflict() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	let mod_a = temp.path().join("9701");
	let mod_b = temp.path().join("9702");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9701"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"9702"}
		]),
	);
	write_descriptor(&mod_a, "mod-a", &[]);
	write_descriptor(&mod_b, "mod-b", &[]);
	write_script_file(&mod_a, "interface/main.gui", "windowType = { name = x }\n");
	write_script_file(&mod_b, "interface/main.gui", "windowType = { name = y }\n");

	let result = run_merge_plan_no_base(request_for(&playlist_path));
	let entry = plan_entry_for(&result, "interface/main.gui");
	assert_eq!(entry.strategy, MergePlanStrategy::ManualConflict);
	assert!(entry.winner.is_none());
	assert!(!entry.generated);
	assert!(!entry.notes.is_empty());
}

#[test]
fn merge_plan_marks_binary_overlap_as_manual_conflict() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	let mod_a = temp.path().join("9801");
	let mod_b = temp.path().join("9802");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9801"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"9802"}
		]),
	);
	write_descriptor(&mod_a, "mod-a", &[]);
	write_descriptor(&mod_b, "mod-b", &[]);
	write_script_file(&mod_a, "gfx/flags/test.dds", "binary-a");
	write_script_file(&mod_b, "gfx/flags/test.dds", "binary-b");

	let result = run_merge_plan_no_base(request_for(&playlist_path));
	let entry = plan_entry_for(&result, "gfx/flags/test.dds");
	assert_eq!(entry.strategy, MergePlanStrategy::ManualConflict);
	assert!(entry.winner.is_none());
	assert!(!entry.generated);
	assert!(!entry.notes.is_empty());
}

#[test]
fn merge_plan_marks_non_structural_text_overlap_as_last_writer_overlay() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	let mod_a = temp.path().join("9901");
	let mod_b = temp.path().join("9902");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9901"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"9902"}
		]),
	);
	write_descriptor(&mod_a, "mod-a", &[]);
	write_descriptor(&mod_b, "mod-b", &[]);
	write_script_file(
		&mod_a,
		"localisation/english/test_l_english.yml",
		"l_english:\n test:0 \"A\"\n",
	);
	write_script_file(
		&mod_b,
		"localisation/english/test_l_english.yml",
		"l_english:\n test:0 \"B\"\n",
	);

	let result = run_merge_plan_no_base(request_for(&playlist_path));
	let entry = plan_entry_for(&result, "localisation/english/test_l_english.yml");
	assert_eq!(result.strategies.last_writer_overlay, 1);
	assert_eq!(entry.strategy, MergePlanStrategy::LastWriterOverlay);
	assert_eq!(entry.winner.as_ref().expect("winner").mod_id, "9902");
	assert!(!entry.generated);
	assert!(entry.notes.is_empty());
}

#[test]
fn merge_report_serializes_frozen_contract_buckets() {
	let report = MergeReport {
		status: MergeReportStatus::Blocked,
		manual_conflict_count: 2,
		generated_file_count: 0,
		copied_file_count: 3,
		overlay_file_count: 1,
		validation: MergeReportValidation {
			fatal_errors: 0,
			strict_findings: 4,
			advisory_findings: 7,
			parse_errors: 1,
			unresolved_references: 2,
			missing_localisation: 5,
		},
	};

	let value = serde_json::to_value(&report).expect("serialize merge report");
	assert_eq!(value["status"], "blocked");
	assert_eq!(value["manual_conflict_count"], 2);
	assert_eq!(value["generated_file_count"], 0);
	assert_eq!(value["copied_file_count"], 3);
	assert_eq!(value["overlay_file_count"], 1);
	assert_eq!(value["validation"]["fatal_errors"], 0);
	assert_eq!(value["validation"]["strict_findings"], 4);
	assert_eq!(value["validation"]["advisory_findings"], 7);
	assert_eq!(value["validation"]["parse_errors"], 1);
	assert_eq!(value["validation"]["unresolved_references"], 2);
	assert_eq!(value["validation"]["missing_localisation"], 5);
}

#[test]
fn check_defaults_to_base_game_and_fails_when_missing() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9921"}
		]),
	);
	write_descriptor(&temp.path().join("9921"), "mod-a", &[]);

	let result = run_checks(request_with_config(&playlist_path, Config::default()));
	assert!(result.has_fatal_errors());
	assert!(
		result
			.fatal_errors
			.iter()
			.any(|message| message.contains("基础游戏目录"))
	);
}

#[test]
fn whole_tree_documents_feed_ui_localisation_csv_and_json_analysis() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	let mod_root = temp.path().join("9931");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9931"}
		]),
	);
	write_descriptor(&mod_root, "mod-a", &[]);
	write_script_file(
		&mod_root,
		"interface/main.gui",
		"windowType = { name = TEST_WINDOW tooltip = TEST_WINDOW_TT texturefile = \"gfx/interface/main.dds\" }\n",
	);
	write_script_file(
		&mod_root,
		"localisation/english/test_l_english.yml",
		"l_english:\n TEST_WINDOW_TT:0 \"Tooltip\"\n TEST_WINDOW_TT:0 \"Duplicate\"\n",
	);
	write_script_file(
		&mod_root,
		"common/data/table.csv",
		"key,value\nalpha,1\nbeta\n",
	);
	write_script_file(&mod_root, "common/data/settings.json", "{ invalid json }\n");

	let result = run_checks_no_base(request_for(&playlist_path));
	assert!(result.analysis_meta.text_documents >= 5);
	assert_eq!(result.analysis_meta.parse_errors, 0);
	assert_eq!(
		result
			.analysis_meta
			.parse_stats
			.localisation
			.parse_issue_count,
		0
	);
	assert!(result.analysis_meta.parse_stats.csv.parse_issue_count >= 1);
	assert!(result.analysis_meta.parse_stats.json.parse_issue_count >= 1);
	assert!(
		result
			.findings
			.iter()
			.any(|finding| finding.rule_id == "A006" && finding.message.contains("TEST_WINDOW_TT"))
	);
	assert!(
		!result
			.findings
			.iter()
			.any(|finding| finding.rule_id == "A005" && finding.message.contains("TEST_WINDOW_TT"))
	);
}
