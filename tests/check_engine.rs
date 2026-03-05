use foch::check::model::{CheckRequest, RunOptions};
use foch::check::{run_checks, run_checks_with_options};
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
	CheckRequest {
		playset_path: playlist_path.to_path_buf(),
		config: Config::default(),
	}
}

fn request_with_config(playlist_path: &Path, config: Config) -> CheckRequest {
	CheckRequest {
		playset_path: playlist_path.to_path_buf(),
		config,
	}
}

#[test]
fn invalid_json_creates_r001() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	fs::write(&playlist_path, "{broken").expect("write broken json");

	let result = run_checks(request_for(&playlist_path));
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

	let result = run_checks(request_for(&playlist_path));
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

	let result = run_checks(request_for(&playlist_path));
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

	let result = run_checks(request_for(&playlist_path));
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

	let result = run_checks(request_for(&playlist_path));
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

	let result = run_checks(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| f.rule_id == "R007"));
}

#[test]
fn unresolved_scripted_effect_creates_r008() {
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
		"country_event = {\n\tid = test.1\n\tmissing_effect = { FLAG = TEST }\n}\n",
	);

	let result = run_checks(request_for(&playlist_path));
	assert!(result.findings.iter().any(|f| f.rule_id == "R008"));
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

	let result = run_checks(request_with_config(&playlist_path, config));
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

	let result = run_checks(request_with_config(&playlist_path, config));
	assert!(
		!result.findings.iter().any(|f| f.rule_id == "R004"),
		"should resolve descriptor.mod from steam libraryfolders path"
	);
}

#[test]
fn include_game_base_resolves_event_reference_from_base_game_symbols() {
	let temp = TempDir::new().expect("temp dir");
	let playlist_path = temp.path().join("playlist.json");
	let mod_root = temp.path().join("9301");
	let game_root = temp.path().join("eu4-game");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"9301"}
		]),
	);
	write_descriptor(&mod_root, "mod-a", &[]);
	write_script_file(
		&mod_root,
		"events/ref.txt",
		"namespace = test\ncountry_event = { id = test.1 option = { country_event = { id = base.1 } } }\n",
	);
	write_script_file(
		&game_root,
		"events/base.txt",
		"namespace = base\ncountry_event = { id = base.1 option = { name = ok } }\n",
	);

	let mut game_path = std::collections::HashMap::new();
	game_path.insert("eu4".to_string(), game_root);
	let config = Config {
		steam_root_path: None,
		paradox_data_path: None,
		game_path,
	};

	let without_game = run_checks_with_options(
		request_with_config(&playlist_path, config.clone()),
		RunOptions::default(),
	);
	assert!(
		without_game
			.findings
			.iter()
			.any(|f| { f.rule_id == "S002" && f.message.contains("event base.1") })
	);

	let with_game = run_checks_with_options(
		request_with_config(&playlist_path, config),
		RunOptions {
			include_game_base: true,
			..RunOptions::default()
		},
	);
	assert!(
		!with_game
			.findings
			.iter()
			.any(|f| { f.rule_id == "S002" && f.message.contains("event base.1") })
	);
}
