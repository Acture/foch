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

fn run_foch(args: &[&str], config_dir: &Path) -> (i32, String, String) {
	let output = Command::new(env!("CARGO_BIN_EXE_foch"))
		.env("FOCH_CONFIG_DIR", config_dir)
		.args(args)
		.output()
		.expect("failed to run foch");

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
