use foch::check::model::CheckRequest;
use foch::check::run_checks;
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

fn request_for(playlist_path: &Path) -> CheckRequest {
	CheckRequest {
		playset_path: playlist_path.to_path_buf(),
		config: Config::default(),
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
