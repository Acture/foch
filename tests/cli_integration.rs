use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn write_playlist(path: &Path, mods: serde_json::Value) {
	let playlist = json!({
		"game": "eu4",
		"name": "cli-playset",
		"mods": mods,
	});
	fs::write(
		path,
		serde_json::to_string_pretty(&playlist).expect("serialize playlist"),
	)
	.expect("write playlist");
}

fn write_descriptor(mod_root: &Path, name: &str) {
	fs::create_dir_all(mod_root).expect("create mod root");
	fs::write(
		mod_root.join("descriptor.mod"),
		format!("name=\"{name}\"\nversion=\"1.0.0\"\n"),
	)
	.expect("write descriptor");
}

fn write_config(path: &Path, content: &str) {
	fs::write(path.join("config.toml"), content).expect("write config");
}

fn ensure_default_game_config(config_dir: &Path) {
	let config_file = config_dir.join("config.toml");
	if config_file.exists() {
		return;
	}
	let game_root = config_dir.join("eu4-game");
	fs::create_dir_all(&game_root).expect("create default game root");
	write_config(
		config_dir,
		format!("[game_path]\neu4 = \"{}\"\n", game_root.display()).as_str(),
	);
}

fn run_foch(args: &[&str], config_dir: &Path) -> (i32, String, String) {
	ensure_default_game_config(config_dir);
	run_foch_with_env(args, config_dir, &[])
}

fn run_foch_with_env(
	args: &[&str],
	config_dir: &Path,
	envs: &[(&str, &str)],
) -> (i32, String, String) {
	ensure_default_game_config(config_dir);
	let home_dir = config_dir.join(".home");
	let xdg_data_home = config_dir.join(".xdg-data");
	fs::create_dir_all(&home_dir).expect("create isolated home");
	fs::create_dir_all(&xdg_data_home).expect("create isolated xdg data");
	let mut command = Command::new(env!("CARGO_BIN_EXE_foch"));
	command
		.env("FOCH_CONFIG_DIR", config_dir)
		.env("HOME", &home_dir)
		.env("XDG_DATA_HOME", &xdg_data_home);
	for (key, value) in envs {
		command.env(key, value);
	}
	let output = command.args(args).output().expect("failed to run foch");

	(
		output.status.code().unwrap_or(-1),
		String::from_utf8(output.stdout).expect("stdout utf8"),
		String::from_utf8(output.stderr).expect("stderr utf8"),
	)
}

#[test]
fn missing_playset_path_returns_exit_1() {
	let tmp = TempDir::new().expect("temp dir");
	let missing = tmp.path().join("missing.json");
	let missing_string = missing.display().to_string();
	let args = ["check", missing_string.as_str()];

	let (code, stdout, _stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 1);
	assert!(stdout.contains("fatal_errors: 1"));
}

#[test]
fn strict_mode_returns_exit_2_when_findings_exist() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"4001"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"4001"}
		]),
	);
	write_descriptor(&tmp.path().join("4001"), "mod-a");

	let playlist_str = playlist_path.display().to_string();
	let args = ["check", playlist_str.as_str(), "--strict"];
	let (code, stdout, _stderr) = run_foch(&args, tmp.path());

	assert_eq!(code, 2);
	assert!(stdout.contains("R003"));
}

#[test]
fn check_json_output_can_be_deserialized() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("result.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"5001"}
		]),
	);
	write_descriptor(&tmp.path().join("5001"), "mod-a");

	let playlist_str = playlist_path.display().to_string();
	let output_str = output_path.display().to_string();
	let args = [
		"check",
		playlist_str.as_str(),
		"--format",
		"json",
		"--output",
		output_str.as_str(),
	];

	let (code, _stdout, _stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0);

	let content = fs::read_to_string(output_path).expect("read json output");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("deserialize result");
	assert!(parsed.get("findings").is_some());
}

#[test]
fn check_can_export_graph_json() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let graph_path = tmp.path().join("graph.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"6001"}
		]),
	);
	let mod_root = tmp.path().join("6001");
	write_descriptor(&mod_root, "mod-a");
	fs::create_dir_all(mod_root.join("events")).expect("create events dir");
	fs::write(
		mod_root.join("events").join("a.txt"),
		"namespace = test\ncountry_event = { id = test.1 }\n",
	)
	.expect("write event file");

	let playlist_str = playlist_path.display().to_string();
	let graph_str = graph_path.display().to_string();
	let args = [
		"check",
		playlist_str.as_str(),
		"--graph-out",
		graph_str.as_str(),
		"--graph-format",
		"json",
	];

	let (code, _stdout, _stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0);

	let content = fs::read_to_string(graph_path).expect("read graph json");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("graph output json");
	assert!(parsed.get("scopes").is_some());
}

#[test]
fn config_validate_reports_invalid_paths() {
	let tmp = TempDir::new().expect("temp dir");
	let cfg_file = tmp.path().join("config.toml");
	fs::write(
		cfg_file,
		"steam_root_path = \"/definitely/not-exist\"\nparadox_data_path = \"/still/not-exist\"\n",
	)
	.expect("write config");

	let (code, stdout, _stderr) = run_foch(&["config", "validate"], tmp.path());
	assert_eq!(code, 0);
	assert!(stdout.contains("[ERROR] steam_root_path"));
}

#[test]
fn merge_plan_json_output_can_be_deserialized() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("plan.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7101"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"7102"}
		]),
	);
	write_descriptor(&tmp.path().join("7101"), "mod-a");
	write_descriptor(&tmp.path().join("7102"), "mod-b");
	fs::create_dir_all(
		tmp.path()
			.join("7101")
			.join("common")
			.join("scripted_effects"),
	)
	.expect("create effects dir");
	fs::create_dir_all(
		tmp.path()
			.join("7102")
			.join("common")
			.join("scripted_effects"),
	)
	.expect("create effects dir");
	fs::write(
		tmp.path()
			.join("7101")
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = a }\n",
	)
	.expect("write effect");
	fs::write(
		tmp.path()
			.join("7102")
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = b }\n",
	)
	.expect("write effect");

	let playlist_str = playlist_path.display().to_string();
	let output_str = output_path.display().to_string();
	let args = [
		"merge-plan",
		playlist_str.as_str(),
		"--format",
		"json",
		"--output",
		output_str.as_str(),
	];

	let (code, _stdout, _stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0);

	let content = fs::read_to_string(output_path).expect("read merge plan output");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("deserialize merge plan");
	assert!(parsed.get("entries").is_some());
	assert!(parsed.get("summary").is_some());
}

#[test]
fn merge_plan_returns_exit_2_when_manual_conflict_exists() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7201"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"7202"}
		]),
	);
	write_descriptor(&tmp.path().join("7201"), "mod-a");
	write_descriptor(&tmp.path().join("7202"), "mod-b");
	fs::create_dir_all(tmp.path().join("7201").join("interface")).expect("create interface dir");
	fs::create_dir_all(tmp.path().join("7202").join("interface")).expect("create interface dir");
	fs::write(
		tmp.path().join("7201").join("interface").join("main.gui"),
		"windowType = { name = a }\n",
	)
	.expect("write gui");
	fs::write(
		tmp.path().join("7202").join("interface").join("main.gui"),
		"windowType = { name = b }\n",
	)
	.expect("write gui");

	let playlist_str = playlist_path.display().to_string();
	let (code, stdout, _stderr) = run_foch(&["merge-plan", playlist_str.as_str()], tmp.path());
	assert_eq!(code, 2);
	assert!(stdout.contains("MANUAL_CONFLICT"));
}

#[test]
fn merge_plan_returns_exit_0_when_no_manual_conflict_exists() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7301"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"7302"}
		]),
	);
	write_descriptor(&tmp.path().join("7301"), "mod-a");
	write_descriptor(&tmp.path().join("7302"), "mod-b");
	fs::create_dir_all(
		tmp.path()
			.join("7301")
			.join("common")
			.join("scripted_effects"),
	)
	.expect("create effects dir");
	fs::create_dir_all(
		tmp.path()
			.join("7302")
			.join("common")
			.join("scripted_effects"),
	)
	.expect("create effects dir");
	fs::write(
		tmp.path()
			.join("7301")
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = a }\n",
	)
	.expect("write effect");
	fs::write(
		tmp.path()
			.join("7302")
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = b }\n",
	)
	.expect("write effect");

	let playlist_str = playlist_path.display().to_string();
	let (code, stdout, _stderr) = run_foch(&["merge-plan", playlist_str.as_str()], tmp.path());
	assert_eq!(code, 0);
	assert!(stdout.contains("structural_merge: 1"));
}

#[test]
fn merge_plan_json_output_contains_strategy_contributors_and_winner() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("plan.json");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7401"},
			{"displayName":"B", "enabled": true, "position": 1, "steamId":"7402"}
		]),
	);
	write_descriptor(&tmp.path().join("7401"), "mod-a");
	write_descriptor(&tmp.path().join("7402"), "mod-b");
	fs::create_dir_all(tmp.path().join("7401").join("localisation").join("english"))
		.expect("create localisation dir");
	fs::create_dir_all(tmp.path().join("7402").join("localisation").join("english"))
		.expect("create localisation dir");
	fs::write(
		tmp.path()
			.join("7401")
			.join("localisation")
			.join("english")
			.join("test_l_english.yml"),
		"l_english:\n test:0 \"A\"\n",
	)
	.expect("write localisation");
	fs::write(
		tmp.path()
			.join("7402")
			.join("localisation")
			.join("english")
			.join("test_l_english.yml"),
		"l_english:\n test:0 \"B\"\n",
	)
	.expect("write localisation");

	let playlist_str = playlist_path.display().to_string();
	let output_str = output_path.display().to_string();
	let args = [
		"merge-plan",
		playlist_str.as_str(),
		"--format",
		"json",
		"--output",
		output_str.as_str(),
	];

	let (code, _stdout, _stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0);

	let content = fs::read_to_string(output_path).expect("read merge plan");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse merge plan");
	let entry = parsed["entries"]
		.as_array()
		.expect("entries array")
		.iter()
		.find(|item| item["path"] == "localisation/english/test_l_english.yml")
		.expect("matching entry");
	assert_eq!(entry["strategy"], "last_writer_overlay");
	assert!(entry["contributors"].is_array());
	assert_eq!(entry["winner"]["mod_id"], "7402");
}

#[test]
fn merge_plan_include_game_base_changes_contributor_ordering() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("plan.json");
	let game_root = tmp.path().join("eu4-game");

	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7501"}
		]),
	);
	write_descriptor(&tmp.path().join("7501"), "mod-a");
	fs::create_dir_all(game_root.join("common").join("scripted_effects")).expect("create effects");
	fs::create_dir_all(
		tmp.path()
			.join("7501")
			.join("common")
			.join("scripted_effects"),
	)
	.expect("create effects");
	fs::write(
		game_root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = base }\n",
	)
	.expect("write base effect");
	fs::write(
		tmp.path()
			.join("7501")
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = mod }\n",
	)
	.expect("write mod effect");
	write_config(
		tmp.path(),
		format!("[game_path]\neu4 = \"{}\"\n", game_root.display()).as_str(),
	);

	let playlist_str = playlist_path.display().to_string();
	let output_str = output_path.display().to_string();
	let args = [
		"merge-plan",
		playlist_str.as_str(),
		"--format",
		"json",
		"--output",
		output_str.as_str(),
	];

	let (code, _stdout, _stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0);

	let content = fs::read_to_string(output_path).expect("read merge plan");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse merge plan");
	let entry = parsed["entries"]
		.as_array()
		.expect("entries array")
		.iter()
		.find(|item| item["path"] == "common/scripted_effects/effects.txt")
		.expect("matching entry");
	assert_eq!(entry["contributors"][0]["is_base_game"], true);
	assert_eq!(entry["winner"]["mod_id"], "7501");
}

#[test]
fn default_base_game_mode_fails_when_game_root_is_missing() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7601"}
		]),
	);
	write_descriptor(&tmp.path().join("7601"), "mod-a");

	let config_dir = tmp.path().join("config-missing-game");
	fs::create_dir_all(&config_dir).expect("create config dir");
	write_config(&config_dir, "");

	let playlist_str = playlist_path.display().to_string();
	let (code, stdout, _stderr) =
		run_foch_with_env(&["check", playlist_str.as_str()], &config_dir, &[]);
	assert_eq!(code, 1);
	assert!(stdout.contains("fatal_errors: 1"));
}

#[test]
fn no_game_base_opt_out_allows_check_without_game_root() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7701"}
		]),
	);
	write_descriptor(&tmp.path().join("7701"), "mod-a");

	let config_dir = tmp.path().join("config-no-game");
	fs::create_dir_all(&config_dir).expect("create config dir");
	write_config(&config_dir, "");

	let playlist_str = playlist_path.display().to_string();
	let (code, stdout, _stderr) = run_foch_with_env(
		&["check", playlist_str.as_str(), "--no-game-base"],
		&config_dir,
		&[],
	);
	assert_eq!(code, 0);
	assert!(stdout.contains("fatal_errors: 0"));
}

#[test]
fn builtin_base_snapshot_bootstraps_local_snapshot_cache() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let game_root = tmp.path().join("eu4-game");
	let snapshot_dir = tmp.path().join("snapshots");
	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7801"}
		]),
	);
	write_descriptor(&tmp.path().join("7801"), "mod-a");
	fs::create_dir_all(game_root.join("events")).expect("create events");
	fs::write(
		game_root.join("events").join("base.txt"),
		"namespace = base\ncountry_event = { id = base.1 }\n",
	)
	.expect("write base event");
	fs::write(
		game_root.join("launcher-settings.json"),
		r#"{ "rawVersion": "builtin-test-1.0.0" }"#,
	)
	.expect("write launcher settings");
	write_config(
		tmp.path(),
		format!("[game_path]\neu4 = \"{}\"\n", game_root.display()).as_str(),
	);

	let playlist_str = playlist_path.display().to_string();
	let snapshot_dir_str = snapshot_dir.display().to_string();
	let (code, stdout, _stderr) = run_foch_with_env(
		&["check", playlist_str.as_str()],
		tmp.path(),
		&[("FOCH_BASE_SNAPSHOT_DIR", snapshot_dir_str.as_str())],
	);
	assert_eq!(code, 0);
	assert!(stdout.contains("fatal_errors: 0"));
	assert!(
		fs::read_dir(&snapshot_dir)
			.expect("snapshot dir exists")
			.flatten()
			.next()
			.is_some()
	);
}

#[test]
fn local_base_snapshot_rebuilds_when_manifest_changes() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let game_root = tmp.path().join("eu4-game");
	let snapshot_dir = tmp.path().join("snapshots");
	write_playlist(
		&playlist_path,
		json!([
			{"displayName":"A", "enabled": true, "position": 0, "steamId":"7901"}
		]),
	);
	write_descriptor(&tmp.path().join("7901"), "mod-a");
	fs::create_dir_all(game_root.join("events")).expect("create events");
	fs::write(
		game_root.join("launcher-settings.json"),
		r#"{ "rawVersion": "9.9.9-test" }"#,
	)
	.expect("write launcher settings");
	fs::write(
		game_root.join("events").join("base.txt"),
		"namespace = base\ncountry_event = { id = base.1 }\n",
	)
	.expect("write base event");
	write_config(
		tmp.path(),
		format!("[game_path]\neu4 = \"{}\"\n", game_root.display()).as_str(),
	);

	let playlist_str = playlist_path.display().to_string();
	let snapshot_dir_str = snapshot_dir.display().to_string();
	let envs = [("FOCH_BASE_SNAPSHOT_DIR", snapshot_dir_str.as_str())];
	let (code, _stdout, _stderr) =
		run_foch_with_env(&["check", playlist_str.as_str()], tmp.path(), &envs);
	assert_eq!(code, 0);
	let first_count = collect_json_files(&snapshot_dir).len();
	assert!(first_count >= 1);

	fs::write(
		game_root.join("events").join("base.txt"),
		"namespace = base\ncountry_event = { id = base.2 }\n",
	)
	.expect("rewrite base event");

	let (code, _stdout, _stderr) =
		run_foch_with_env(&["check", playlist_str.as_str()], tmp.path(), &envs);
	assert_eq!(code, 0);
	let second_count = collect_json_files(&snapshot_dir).len();
	assert!(second_count > first_count);
}

fn collect_json_files(root: &Path) -> Vec<std::path::PathBuf> {
	let mut files = Vec::new();
	if !root.exists() {
		return files;
	}
	for entry in walkdir::WalkDir::new(root)
		.into_iter()
		.filter_map(Result::ok)
	{
		if entry.file_type().is_file()
			&& entry.path().extension().and_then(|value| value.to_str()) == Some("json")
		{
			files.push(entry.path().to_path_buf());
		}
	}
	files.sort();
	files
}
