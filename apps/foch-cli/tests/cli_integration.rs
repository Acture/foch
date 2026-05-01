use foch_core::model::{
	MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH, MERGED_MOD_DESCRIPTOR_PATH,
};
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tempfile::TempDir;

fn write_dlc_load(path: &Path, mods: &[(&str, &str)]) {
	let parent = path.parent().expect("playset path has parent");
	fs::create_dir_all(parent.join("mod")).expect("create mod metadata dir");
	let enabled_mods: Vec<String> = mods
		.iter()
		.map(|(steam_id, _)| format!("mod/ugc_{steam_id}.mod"))
		.collect();
	let dlc_load = json!({
		"enabled_mods": enabled_mods,
		"disabled_dlcs": Vec::<String>::new(),
	});
	fs::write(
		path,
		serde_json::to_string_pretty(&dlc_load).expect("serialize dlc_load"),
	)
	.expect("write dlc_load.json");
	for (steam_id, display_name) in mods {
		let mod_root = parent.join(steam_id);
		let body = format!(
			"name=\"{display_name}\"\npath=\"{}\"\nremote_file_id=\"{steam_id}\"\n",
			mod_root.display()
		);
		fs::write(parent.join("mod").join(format!("ugc_{steam_id}.mod")), body)
			.expect("write ugc descriptor");
	}
}

fn write_descriptor(mod_root: &Path, name: &str) {
	write_descriptor_with_dependencies(mod_root, name, &[]);
}

fn write_descriptor_with_dependencies(mod_root: &Path, name: &str, dependencies: &[&str]) {
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

fn write_script_file(mod_root: &Path, relative_path: &str, content: &str) {
	let path = mod_root.join(relative_path);
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).expect("create parent");
	}
	fs::write(path, content).expect("write script file");
}

/// Stage a structural-merge conflict: both mods contribute the same scripted
/// triggers file, but mod_b's content is malformed Clausewitz so
/// validate_structural_merge_inputs flags it and the merge plan downgrades the
/// path to MergePlanStrategy::ManualConflict.
const STRUCTURAL_CONFLICT_PATH: &str = "common/scripted_triggers/conflict.txt";

fn stage_structural_manual_conflict(mod_a: &Path, mod_b: &Path) {
	let dir_a = mod_a.join("common").join("scripted_triggers");
	let dir_b = mod_b.join("common").join("scripted_triggers");
	fs::create_dir_all(&dir_a).expect("create scripted_triggers dir (a)");
	fs::create_dir_all(&dir_b).expect("create scripted_triggers dir (b)");
	fs::write(
		dir_a.join("conflict.txt"),
		"my_trigger = { always = yes }\n",
	)
	.expect("write valid scripted_trigger");
	// Malformed Clausewitz: produces a parse diagnostic ("无法解析的语句起始 token"),
	// which downgrades the structural merge to ManualConflict.
	fs::write(
		dir_b.join("conflict.txt"),
		"name { = invalid syntax with unclosed\nbraces\n",
	)
	.expect("write malformed scripted_trigger");
}

const DAG_FALLBACK_PATH: &str = "common/ideas/fallback.txt";

fn idea_file(cost: &str) -> String {
	format!("group = {{\n\tidea = {{\n\t\tcost = {cost}\n\t}}\n}}\n")
}

#[allow(dead_code)]
fn stage_dag_fallback_conflict(
	playlist_path: &Path,
	mod_base: &Path,
	mod_a: &Path,
	mod_b: &Path,
	mod_c: &Path,
) {
	write_dlc_load(
		playlist_path,
		&[
			("9101", "Base"),
			("9102", "A"),
			("9103", "B"),
			("9104", "C"),
		],
	);
	write_descriptor(mod_base, "fallback-base");
	write_descriptor_with_dependencies(mod_a, "fallback-a", &["fallback-base"]);
	write_descriptor_with_dependencies(mod_b, "fallback-b", &["fallback-base"]);
	write_descriptor_with_dependencies(mod_c, "fallback-c", &["fallback-a", "fallback-b"]);
	write_script_file(mod_base, DAG_FALLBACK_PATH, &idea_file("old"));
	write_script_file(mod_a, DAG_FALLBACK_PATH, &idea_file("alpha"));
	write_script_file(mod_b, DAG_FALLBACK_PATH, &idea_file("beta"));
	write_script_file(mod_c, DAG_FALLBACK_PATH, &idea_file("gamma"));
}

/// Genuine sibling-overwrite conflict between A and B with no downstream
/// resolver. The DAG topo walk cannot auto-resolve this; the fallback path
/// is the only way to produce output.
fn stage_dag_genuine_conflict(playlist_path: &Path, mod_base: &Path, mod_a: &Path, mod_b: &Path) {
	write_dlc_load(
		playlist_path,
		&[("9101", "Base"), ("9102", "A"), ("9103", "B")],
	);
	write_descriptor(mod_base, "fallback-base");
	write_descriptor_with_dependencies(mod_a, "fallback-a", &["fallback-base"]);
	write_descriptor_with_dependencies(mod_b, "fallback-b", &["fallback-base"]);
	write_script_file(mod_base, DAG_FALLBACK_PATH, &idea_file("old"));
	write_script_file(mod_a, DAG_FALLBACK_PATH, &idea_file("alpha"));
	write_script_file(mod_b, DAG_FALLBACK_PATH, &idea_file("beta"));
}

fn write_config(path: &Path, content: &str) {
	fs::write(path.join("config.toml"), content).expect("write config");
}

fn write_game_version(game_root: &Path, version: &str) {
	fs::create_dir_all(game_root).expect("create game root");
	fs::write(
		game_root.join("launcher-settings.json"),
		format!(r#"{{ "rawVersion": "{version}" }}"#),
	)
	.expect("write launcher settings");
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

fn build_base_data_install(config_dir: &Path, game_root: &Path) {
	let game_root_str = game_root.display().to_string();
	let (code, _stdout, stderr) = run_foch(
		&[
			"data",
			"build",
			"eu4",
			"--from-game-path",
			game_root_str.as_str(),
			"--game-version",
			"auto",
			"--install",
		],
		config_dir,
	);
	assert_eq!(code, 0, "stderr: {stderr}");
}

fn build_release_assets(config_dir: &Path, game_root: &Path, output_dir: &Path) {
	let game_root_str = game_root.display().to_string();
	let output_dir_str = output_dir.display().to_string();
	let (code, _stdout, stderr) = run_foch(
		&[
			"data",
			"build",
			"eu4",
			"--from-game-path",
			game_root_str.as_str(),
			"--game-version",
			"auto",
			"--output-dir",
			output_dir_str.as_str(),
			"--release-asset",
		],
		config_dir,
	);
	assert_eq!(code, 0, "stderr: {stderr}");
}

struct StaticServer {
	base_url: String,
	stop_tx: mpsc::Sender<()>,
	handle: Option<JoinHandle<()>>,
}

impl Drop for StaticServer {
	fn drop(&mut self) {
		let _ = self.stop_tx.send(());
		if let Some(handle) = self.handle.take() {
			let _ = handle.join();
		}
	}
}

fn serve_directory(root: &Path) -> StaticServer {
	let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
	let addr = listener.local_addr().expect("server addr");
	listener
		.set_nonblocking(true)
		.expect("set nonblocking listener");
	let root = root.to_path_buf();
	let (stop_tx, stop_rx) = mpsc::channel::<()>();
	let handle = thread::spawn(move || {
		loop {
			if stop_rx.try_recv().is_ok() {
				break;
			}
			match listener.accept() {
				Ok((mut stream, _addr)) => {
					let mut request_line = String::new();
					let mut reader = BufReader::new(
						stream.try_clone().expect("clone stream for request reader"),
					);
					if reader.read_line(&mut request_line).is_err() {
						continue;
					}
					let path = request_line
						.split_whitespace()
						.nth(1)
						.unwrap_or("/")
						.trim_start_matches('/');
					let full_path = root.join(path);
					if let Ok(bytes) = fs::read(&full_path) {
						let header = format!(
							"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
							bytes.len()
						);
						let _ = stream.write_all(header.as_bytes());
						let _ = stream.write_all(&bytes);
					} else {
						let body = b"not found";
						let header = format!(
							"HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
							body.len()
						);
						let _ = stream.write_all(header.as_bytes());
						let _ = stream.write_all(body);
					}
				}
				Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
					thread::sleep(Duration::from_millis(25));
				}
				Err(_err) => break,
			}
		}
	});

	StaticServer {
		base_url: format!("http://127.0.0.1:{}", addr.port()),
		stop_tx,
		handle: Some(handle),
	}
}

fn collect_gzip_files(root: &Path) -> Vec<std::path::PathBuf> {
	let mut files = Vec::new();
	if !root.exists() {
		return files;
	}
	for entry in walkdir::WalkDir::new(root)
		.into_iter()
		.filter_map(Result::ok)
	{
		if entry.file_type().is_file()
			&& entry.path().extension().and_then(|value| value.to_str()) == Some("gz")
		{
			files.push(entry.path().to_path_buf());
		}
	}
	files.sort();
	files
}

fn read_json_file(path: &Path) -> serde_json::Value {
	let content = fs::read_to_string(path).expect("read json file");
	serde_json::from_str(&content).expect("parse json file")
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

	write_dlc_load(&playlist_path, &[("4001", "A"), ("4001", "B")]);
	write_descriptor(&tmp.path().join("4001"), "mod-a");

	let playlist_str = playlist_path.display().to_string();
	let args = ["check", playlist_str.as_str(), "--strict", "--no-game-base"];
	let (code, stdout, _stderr) = run_foch(&args, tmp.path());

	assert_eq!(code, 2);
	assert!(stdout.contains("R003"));
}

#[test]
fn check_json_output_can_be_deserialized() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("result.json");

	write_dlc_load(&playlist_path, &[("5001", "A")]);
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
		"--no-game-base",
	];

	let (code, _stdout, _stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0);

	let content = fs::read_to_string(output_path).expect("read json output");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("deserialize result");
	assert!(parsed.get("findings").is_some());
}

#[test]
fn check_rejects_removed_graph_flags() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	write_dlc_load(&playlist_path, &[]);

	let playlist_str = playlist_path.display().to_string();
	let args = ["check", playlist_str.as_str(), "--graph-out", "graph.json"];

	let (code, _stdout, stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 2);
	assert!(stderr.contains("--graph-out"));
}

#[test]
fn graph_command_resolves_runtime_calls_even_without_declared_dependency() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("graphs");
	let mod_a = tmp.path().join("9001");
	let mod_b = tmp.path().join("9002");

	write_dlc_load(&playlist_path, &[("9001", "A"), ("9002", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor(&mod_b, "mod-b");
	fs::create_dir_all(mod_a.join("events")).expect("create events dir");
	fs::create_dir_all(mod_b.join("common").join("scripted_effects")).expect("create effects dir");
	fs::write(
		mod_a.join("events").join("ref.txt"),
		"namespace = test\ncountry_event = { id = test.1 immediate = { shared_effect = { } } }\n",
	)
	.expect("write ref event");
	fs::write(
		mod_b
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = provider }\n",
	)
	.expect("write effect");

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, _stdout, stderr) = run_foch(
		&[
			"graph",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--scope",
			"mods",
			"--format",
			"json",
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stderr: {stderr}");

	let calls = read_json_file(&out_dir.join("mods/9001/calls.json"));
	let nodes = calls["nodes"].as_array().expect("calls nodes");
	let provider = nodes
		.iter()
		.find(|node| {
			node["mod_id"] == "9002"
				&& node["kind"] == "definition"
				&& node["name"]
					.as_str()
					.is_some_and(|name| name.ends_with("::shared_effect"))
		})
		.expect("provider node");
	let provider_id = provider["id"].as_str().expect("provider id");
	let edges = calls["edges"].as_array().expect("calls edges");
	let call_edge = edges
		.iter()
		.find(|edge| edge["kind"] == "calls" && edge["to"] == provider_id)
		.expect("runtime call edge");
	assert_eq!(call_edge["declared_dependency"], false);
	assert_eq!(call_edge["dependency_match_kind"], "none");
	let hint_edge = edges
		.iter()
		.find(|edge| edge["kind"] == "declared_dependency_hint" && edge["to"] == provider_id)
		.expect("dependency hint edge");
	assert_eq!(hint_edge["declared_dependency"], false);
	assert_eq!(hint_edge["dependency_match_kind"], "none");

	let deps = read_json_file(&out_dir.join("mods/9001/mod-deps.json"));
	assert!(deps["edges"].as_array().expect("deps edges").is_empty());
}

#[test]
fn graph_command_exports_declared_dependency_and_symbol_tree() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("graphs");
	let mod_a = tmp.path().join("9011");
	let mod_b = tmp.path().join("9012");

	write_dlc_load(&playlist_path, &[("9011", "A"), ("9012", "B")]);
	write_descriptor_with_dependencies(&mod_a, "mod-a", &["mod-b"]);
	write_descriptor(&mod_b, "mod-b");
	fs::create_dir_all(mod_a.join("events")).expect("create events dir");
	fs::create_dir_all(mod_b.join("common").join("scripted_effects")).expect("create effects dir");
	fs::write(
		mod_a.join("events").join("ref.txt"),
		"namespace = test\ncountry_event = { id = test.1 immediate = { shared_effect = { } } }\n",
	)
	.expect("write ref event");
	fs::write(
		mod_b
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = provider }\n",
	)
	.expect("write effect");

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, _stdout, stderr) = run_foch(
		&[
			"graph",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--format",
			"both",
			"--root",
			"scripted_effect:shared_effect",
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stderr: {stderr}");

	let calls = read_json_file(&out_dir.join("mods/9011/calls.json"));
	let nodes = calls["nodes"].as_array().expect("calls nodes");
	let provider = nodes
		.iter()
		.find(|node| {
			node["mod_id"] == "9012"
				&& node["name"]
					.as_str()
					.is_some_and(|name| name.ends_with("::shared_effect"))
		})
		.expect("provider node");
	let provider_id = provider["id"].as_str().expect("provider id");
	let call_edge = calls["edges"]
		.as_array()
		.expect("calls edges")
		.iter()
		.find(|edge| edge["kind"] == "calls" && edge["to"] == provider_id)
		.expect("runtime call edge");
	assert_eq!(call_edge["declared_dependency"], true);
	assert_eq!(call_edge["dependency_match_kind"], "descriptor_name");

	let deps = read_json_file(&out_dir.join("workspace/mod-deps.json"));
	let dep_edge = deps["edges"]
		.as_array()
		.expect("deps edges")
		.iter()
		.find(|edge| edge["from"] == "9011" && edge["to"] == "9012")
		.expect("dependency edge");
	assert_eq!(dep_edge["match_kind"], "descriptor_name");

	assert!(
		out_dir
			.join("trees/scripted_effect-shared_effect.json")
			.exists()
	);
	assert!(
		out_dir
			.join("trees/scripted_effect-shared_effect.dot")
			.exists()
	);
}

#[test]
fn semantic_graph_requires_family_argument() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("graphs");
	write_dlc_load(&playlist_path, &[]);

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, _stdout, stderr) = run_foch(
		&[
			"graph",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--mode",
			"semantic",
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_ne!(code, 0);
	assert!(stderr.contains("--family"), "stderr: {stderr}");
}

#[test]
fn semantic_graph_writes_family_json_and_html() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("graphs");
	let mod_a = tmp.path().join("9101");

	write_dlc_load(&playlist_path, &[("9101", "A")]);
	write_descriptor(&mod_a, "mod-a");
	fs::create_dir_all(mod_a.join("common").join("holy_orders")).expect("create holy orders dir");
	fs::write(
		mod_a.join("common").join("holy_orders").join("orders.txt"),
		concat!(
			"order_alpha = {\n",
			"\ticon = order_icon\n",
			"\tregion = europe_region\n",
			"\tcustom_tooltip = HOLY_ORDER_TOOLTIP\n",
			"\tmodifier = { manpower_recovery_speed = 0.1 }\n",
			"}\n",
		),
	)
	.expect("write holy order");

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, _stdout, stderr) = run_foch(
		&[
			"graph",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--mode",
			"semantic",
			"--family",
			"common/holy_orders",
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stderr: {stderr}");

	let graph_path = out_dir
		.join("semantic")
		.join("common/holy_orders")
		.join("semantic-graph.json");
	let html_path = out_dir
		.join("semantic")
		.join("common/holy_orders")
		.join("index.html");
	assert!(graph_path.exists());
	assert!(html_path.exists());

	let graph = read_json_file(&graph_path);
	assert_eq!(graph["family_id"], "common/holy_orders");
	assert!(
		graph["nodes"]
			.as_array()
			.expect("nodes")
			.iter()
			.any(|node| {
				node["kind"] == "definition"
					&& node["definition_key"] == "holy_order_definition"
					&& node["definition_value"] == "order_alpha"
			})
	);
	assert!(
		graph["edges"]
			.as_array()
			.expect("edges")
			.iter()
			.any(|edge| edge["kind"] == "references_external")
	);

	let html = fs::read_to_string(html_path).expect("read html");
	assert!(html.contains("Semantic Graph"));
	assert!(html.contains("common/holy_orders"));
}

#[test]
fn semantic_graph_real_minimized_playlist_emits_progress_and_real_nodes() {
	let tmp = TempDir::new().expect("temp dir");
	let out_dir = tmp.path().join("graphs");
	let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("../..")
		.canonicalize()
		.expect("repo root");
	let playlist_path = repo_root
		.join("tests")
		.join("corpus")
		.join("eu4_real_minimized")
		.join("playlist.json");

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, _stdout, stderr) = run_foch(
		&[
			"graph",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--mode",
			"semantic",
			"--family",
			"common/scripted_effects",
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(
		stderr.contains("semantic graph resolve workspace: start"),
		"stderr: {stderr}"
	);
	assert!(
		stderr.contains("semantic graph build runtime state: done"),
		"stderr: {stderr}"
	);
	assert!(
		stderr.contains("semantic graph build semantic artifact: done"),
		"stderr: {stderr}"
	);

	let graph_path = out_dir
		.join("semantic")
		.join("common/scripted_effects")
		.join("semantic-graph.json");
	let html_path = out_dir
		.join("semantic")
		.join("common/scripted_effects")
		.join("index.html");
	assert!(graph_path.exists());
	assert!(html_path.exists());

	let graph = read_json_file(&graph_path);
	assert_eq!(graph["family_id"], "common/scripted_effects");
	assert!(
		graph["nodes"]
			.as_array()
			.expect("nodes")
			.iter()
			.any(|node| {
				node["kind"] == "definition"
					&& node["definition_key"] == "symbol:scripted_effect"
					&& node["definition_value"]
						== "eu4::scripted_effects::se_md_add_or_upgrade_bonus"
			})
	);
	assert!(
		graph["nodes"]
			.as_array()
			.expect("nodes")
			.iter()
			.any(|node| {
				node["kind"] == "definition"
					&& node["definition_key"] == "symbol:scripted_effect"
					&& node["definition_value"]
						== "eu4::scripted_effects::complex_dynamic_effect_without_alternative"
			})
	);

	let html = fs::read_to_string(html_path).expect("read html");
	assert!(html.contains("Semantic Graph"));
	assert!(html.contains("common/scripted_effects"));
}

#[test]
fn simplify_command_out_removes_base_equivalent_definitions_and_reports_merge_candidates() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let game_root = tmp.path().join("eu4-game");
	let out_dir = tmp.path().join("simplified-mod");
	let mod_a = tmp.path().join("9021");
	let mod_b = tmp.path().join("9022");

	write_dlc_load(&playlist_path, &[("9021", "A"), ("9022", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor(&mod_b, "mod-b");
	write_game_version(&game_root, "12.1.0-test");
	fs::create_dir_all(game_root.join("common").join("scripted_effects"))
		.expect("create base effects dir");
	fs::create_dir_all(mod_a.join("common").join("scripted_effects"))
		.expect("create mod effects dir");
	fs::create_dir_all(mod_b.join("common").join("scripted_effects"))
		.expect("create mod effects dir");
	fs::write(
		game_root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = base }\n",
	)
	.expect("write base effect");
	fs::write(
		mod_a
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		concat!(
			"shared_effect = { log = base }\n",
			"merge_me = { log = a }\n",
			"local_effect = { log = keep }\n"
		),
	)
	.expect("write mod a effects");
	fs::write(
		mod_b
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"merge_me = { log = b }\n",
	)
	.expect("write mod b effects");
	write_config(
		tmp.path(),
		format!("[game_path]\neu4 = \"{}\"\n", game_root.display()).as_str(),
	);
	build_base_data_install(tmp.path(), &game_root);

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, stderr) = run_foch(
		&[
			"simplify",
			playlist_str.as_str(),
			"--target",
			"9021",
			"--out",
			out_str.as_str(),
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(stdout.contains("removed_definitions=1"));

	let simplified = fs::read_to_string(
		out_dir
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
	)
	.expect("read simplified file");
	assert!(!simplified.contains("shared_effect"));
	assert!(simplified.contains("merge_me"));
	assert!(simplified.contains("local_effect"));

	let report = read_json_file(&out_dir.join("simplify-report.json"));
	assert_eq!(report["target_mod_id"], "9021");
	assert_eq!(report["removed"][0]["name"], "shared_effect");
	assert_eq!(report["merge_candidates"][0]["name"], "merge_me");
}

#[test]
fn simplify_command_in_place_removes_empty_files() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let game_root = tmp.path().join("eu4-game");
	let mod_a = tmp.path().join("9031");
	let target_file = mod_a
		.join("common")
		.join("scripted_effects")
		.join("effects.txt");

	write_dlc_load(&playlist_path, &[("9031", "A")]);
	write_descriptor(&mod_a, "mod-a");
	write_game_version(&game_root, "12.2.0-test");
	fs::create_dir_all(game_root.join("common").join("scripted_effects"))
		.expect("create base effects dir");
	fs::create_dir_all(mod_a.join("common").join("scripted_effects"))
		.expect("create mod effects dir");
	fs::write(
		game_root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = base }\n",
	)
	.expect("write base effect");
	fs::write(&target_file, "shared_effect = { log = base }\n").expect("write mod effect");
	write_config(
		tmp.path(),
		format!("[game_path]\neu4 = \"{}\"\n", game_root.display()).as_str(),
	);
	build_base_data_install(tmp.path(), &game_root);

	let playlist_str = playlist_path.display().to_string();
	let (code, stdout, stderr) = run_foch(
		&[
			"simplify",
			playlist_str.as_str(),
			"--target",
			"9031",
			"--in-place",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(stdout.contains("removed_definitions=1"));
	assert!(!target_file.exists());
	assert!(mod_a.join("simplify-report.json").exists());
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

	write_dlc_load(&playlist_path, &[("7101", "A"), ("7102", "B")]);
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
		"--no-game-base",
	];

	let (code, _stdout, _stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0);

	let content = fs::read_to_string(output_path).expect("read merge plan output");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("deserialize merge plan");
	assert!(
		parsed
			.get("generated_at")
			.and_then(|value| value.as_str())
			.is_some()
	);
	assert!(parsed.get("strategies").is_some());
	assert!(parsed.get("paths").is_some());
	assert!(parsed.get("entries").is_none());
	assert!(parsed.get("summary").is_none());
}

#[test]
fn merge_plan_returns_exit_2_when_manual_conflict_exists() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");

	write_dlc_load(&playlist_path, &[("7201", "A"), ("7202", "B")]);
	write_descriptor(&tmp.path().join("7201"), "mod-a");
	write_descriptor(&tmp.path().join("7202"), "mod-b");
	stage_structural_manual_conflict(&tmp.path().join("7201"), &tmp.path().join("7202"));

	let playlist_str = playlist_path.display().to_string();
	let (code, stdout, _stderr) = run_foch(
		&["merge-plan", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
	);
	assert_eq!(code, 2);
	assert!(stdout.contains("MANUAL_CONFLICT"));
	assert!(stdout.contains("generated=false"));
}

#[test]
fn merge_plan_returns_exit_0_when_no_manual_conflict_exists() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");

	write_dlc_load(&playlist_path, &[("7301", "A"), ("7302", "B")]);
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
	let (code, stdout, _stderr) = run_foch(
		&["merge-plan", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
	);
	assert_eq!(code, 0);
	assert!(stdout.contains("structural_merge: 1"));
}

#[test]
fn merge_plan_json_output_contains_strategy_contributors_and_winner() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("plan.json");

	write_dlc_load(&playlist_path, &[("7401", "A"), ("7402", "B")]);
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
		"--no-game-base",
	];

	let (code, _stdout, _stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0);

	let content = fs::read_to_string(output_path).expect("read merge plan");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse merge plan");
	assert!(parsed["generated_at"].as_str().is_some());
	assert_eq!(parsed["strategies"]["localisation_merge"], 1);
	let entry = parsed["paths"]
		.as_array()
		.expect("paths array")
		.iter()
		.find(|item| item["path"] == "localisation/english/test_l_english.yml")
		.expect("matching entry");
	assert_eq!(entry["strategy"], "localisation_merge");
	assert!(entry["contributors"].is_array());
	assert_eq!(entry["winner"]["mod_id"], "7402");
	assert_eq!(entry["generated"], false);
	assert_eq!(entry["notes"], json!([]));
}

#[test]
fn merge_plan_json_output_uses_null_winner_for_manual_conflicts() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("plan.json");

	write_dlc_load(&playlist_path, &[("7411", "A"), ("7412", "B")]);
	write_descriptor(&tmp.path().join("7411"), "mod-a");
	write_descriptor(&tmp.path().join("7412"), "mod-b");
	stage_structural_manual_conflict(&tmp.path().join("7411"), &tmp.path().join("7412"));

	let playlist_str = playlist_path.display().to_string();
	let output_str = output_path.display().to_string();
	let (code, _stdout, _stderr) = run_foch(
		&[
			"merge-plan",
			playlist_str.as_str(),
			"--format",
			"json",
			"--output",
			output_str.as_str(),
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 2);

	let content = fs::read_to_string(output_path).expect("read merge plan");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse merge plan");
	let entry = parsed["paths"]
		.as_array()
		.expect("paths array")
		.iter()
		.find(|item| item["path"] == STRUCTURAL_CONFLICT_PATH)
		.expect("matching entry");
	assert_eq!(entry["strategy"], "manual_conflict");
	assert!(entry["winner"].is_null());
	assert_eq!(entry["generated"], false);
	assert!(
		entry["notes"]
			.as_array()
			.is_some_and(|items| !items.is_empty())
	);
}

#[test]
fn merge_plan_json_output_marks_non_normalizable_defines_as_manual_conflict() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("plan.json");

	write_dlc_load(&playlist_path, &[("7421", "A"), ("7422", "B")]);
	write_descriptor(&tmp.path().join("7421"), "mod-a");
	write_descriptor(&tmp.path().join("7422"), "mod-b");
	fs::create_dir_all(tmp.path().join("7421").join("common").join("defines"))
		.expect("create defines dir");
	fs::create_dir_all(tmp.path().join("7422").join("common").join("defines"))
		.expect("create defines dir");
	fs::write(
		tmp.path()
			.join("7421")
			.join("common")
			.join("defines")
			.join("test.txt"),
		"NGame = {\n\tSTART_YEAR = 1444\n}\n",
	)
	.expect("write defines");
	fs::write(
		tmp.path()
			.join("7422")
			.join("common")
			.join("defines")
			.join("test.txt"),
		"NGame = {\n\t1445\n}\n",
	)
	.expect("write defines");

	let playlist_str = playlist_path.display().to_string();
	let output_str = output_path.display().to_string();
	let (code, _stdout, _stderr) = run_foch(
		&[
			"merge-plan",
			playlist_str.as_str(),
			"--format",
			"json",
			"--output",
			output_str.as_str(),
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 2);

	let content = fs::read_to_string(output_path).expect("read merge plan");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse merge plan");
	let entry = parsed["paths"]
		.as_array()
		.expect("paths array")
		.iter()
		.find(|item| item["path"] == "common/defines/test.txt")
		.expect("matching entry");
	assert_eq!(entry["strategy"], "manual_conflict");
	assert!(entry["winner"].is_null());
	assert_eq!(entry["generated"], false);
	assert!(entry["notes"].as_array().is_some_and(|notes| {
		notes.iter().any(|note| {
			note.as_str()
				.is_some_and(|text| text.contains("non-normalizable defines"))
		})
	}));
}

#[test]
fn merge_plan_include_game_base_changes_contributor_ordering() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("plan.json");
	let game_root = tmp.path().join("eu4-game");

	write_dlc_load(&playlist_path, &[("7501", "A")]);
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
	write_game_version(&game_root, "7.5.0-test");
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
	build_base_data_install(tmp.path(), &game_root);

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
	let entry = parsed["paths"]
		.as_array()
		.expect("paths array")
		.iter()
		.find(|item| item["path"] == "common/scripted_effects/effects.txt")
		.expect("matching entry");
	assert_eq!(entry["contributors"][0]["is_base_game"], true);
	assert_eq!(entry["winner"]["mod_id"], "7501");
	assert_eq!(entry["generated"], false);
}

#[test]
fn merge_command_generates_output_tree_and_returns_exit_0_for_clean_playset() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	let mod_root = tmp.path().join("7805");

	write_dlc_load(&playlist_path, &[("7805", "A")]);
	write_descriptor(&mod_root, "mod-a");
	fs::create_dir_all(mod_root.join("common")).expect("create common dir");
	fs::write(mod_root.join("common").join("only.txt"), "from-a\n").expect("write file");

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 0);
	assert!(stdout.contains("status: READY"));
	assert!(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
	assert!(out_dir.join(MERGE_PLAN_ARTIFACT_PATH).exists());
	assert!(out_dir.join(MERGE_REPORT_ARTIFACT_PATH).exists());
	assert_eq!(
		fs::read_to_string(out_dir.join("common/only.txt")).expect("read copied file"),
		"from-a\n"
	);

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "ready");
	assert_eq!(report["copied_file_count"], 1);
	assert_eq!(report["overlay_file_count"], 0);
	assert_eq!(report["generated_file_count"], 0);
	assert_eq!(report["validation"]["fatal_errors"], 0);
	assert_eq!(report["validation"]["strict_findings"], 0);
	assert_eq!(report["validation"]["parse_errors"], 0);
}

#[test]
fn merge_command_ignore_dep_drops_declared_edge_and_reports_override() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	let mod_a = tmp.path().join("7901");
	let mod_b = tmp.path().join("7902");
	let relative_path = "common/scripted_effects/effects.txt";

	write_dlc_load(&playlist_path, &[("7901", "A"), ("7902", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor_with_dependencies(&mod_b, "mod-b", &["mod-a"]);
	write_script_file(&mod_a, relative_path, "effect_a = { log = a }\n");
	write_script_file(&mod_b, relative_path, "effect_b = { log = b }\n");

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--ignore-dep",
			"7902:7901",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
	assert!(stdout.contains("status: READY"));
	let output = fs::read_to_string(out_dir.join(relative_path)).expect("read merged output");
	assert!(output.contains("effect_a"), "output: {output}");
	assert!(output.contains("effect_b"), "output: {output}");

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["dep_overrides_applied"][0]["mod_id"], "7902");
	assert_eq!(report["dep_overrides_applied"][0]["dep_id"], "7901");
	assert_eq!(report["dep_overrides_applied"][0]["source"], "cli");
}

#[test]
fn merge_command_skips_unresolved_dag_conflict_without_fallback() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 2);
	assert!(stdout.contains("status: BLOCKED"));
	assert!(stdout.contains("fallback_resolved_count: 0"));
	assert!(!out_dir.join(DAG_FALLBACK_PATH).exists());

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "blocked");
	assert_eq!(report["manual_conflict_count"], 1);
	assert_eq!(report["fallback_resolved_count"], 0);
	assert_eq!(report["generated_file_count"], 0);
	assert_eq!(
		report["conflict_resolutions"][0]["kind"],
		"true_conflict_skipped"
	);
}

#[test]
fn merge_command_fallback_writes_last_writer_marker() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--fallback",
		],
		tmp.path(),
	);
	assert_eq!(code, 0);
	assert!(stdout.contains("status: READY"));
	assert!(stdout.contains("fallback_resolved_count: 1"));
	let output = fs::read_to_string(out_dir.join(DAG_FALLBACK_PATH)).expect("read fallback");
	assert!(output.starts_with("# foch:conflict reason=\"patch merge failed:"));
	assert!(output.contains("resolved=\"last-writer:9103:1.0.0\""));
	assert!(output.ends_with(&idea_file("beta")));

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "ready");
	assert_eq!(report["manual_conflict_count"], 0);
	assert_eq!(report["fallback_resolved_count"], 1);
	assert_eq!(report["generated_file_count"], 1);
	assert_eq!(
		report["conflict_resolutions"][0]["kind"],
		"last_writer_fallback"
	);
	assert_eq!(
		report["conflict_resolutions"][0]["winning_mod"],
		"9103:1.0.0"
	);
	assert_eq!(report["conflict_resolutions"][0]["marker_written"], true);
}

#[test]
fn merge_command_default_unresolved_conflict_prints_fallback_tip_to_stderr() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 2, "stdout: {stdout}\nstderr: {stderr}");
	assert!(!stdout.contains("Tip:"), "stdout: {stdout}");
	assert!(
		stderr.contains("Tip: 1 unresolved merge conflict"),
		"stderr: {stderr}"
	);
	assert!(stderr.contains("SKIPPED"), "stderr: {stderr}");
	assert!(stderr.contains("not written"), "stderr: {stderr}");
	assert!(
		stderr.contains(".foch/foch-merge-report.json"),
		"stderr: {stderr}"
	);
	assert!(stderr.contains("--fallback"), "stderr: {stderr}");
	assert!(stderr.contains("last-writer"), "stderr: {stderr}");
	assert!(stderr.contains("conflict markers"), "stderr: {stderr}");
	assert!(stderr.contains("Foch kept"), "stderr: {stderr}");
}

#[test]
fn merge_command_fallback_unresolved_scenario_suppresses_fallback_tip() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--fallback",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
	assert!(stdout.contains("fallback_resolved_count: 1"));
	assert!(!stderr.contains("Tip:"), "stderr: {stderr}");
	assert!(
		!stderr.contains("unresolved merge conflict"),
		"stderr: {stderr}"
	);
	assert!(
		!stderr.contains("--fallback to materialize"),
		"stderr: {stderr}"
	);
}

#[test]
fn merge_command_returns_exit_2_and_writes_only_sidecars_when_manual_conflict_blocks() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	let mod_a = tmp.path().join("7811");
	let mod_b = tmp.path().join("7812");

	write_dlc_load(&playlist_path, &[("7811", "A"), ("7812", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor(&mod_b, "mod-b");
	stage_structural_manual_conflict(&mod_a, &mod_b);

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 2);
	assert!(stdout.contains("status: BLOCKED"));
	assert!(!out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
	assert!(!out_dir.join(STRUCTURAL_CONFLICT_PATH).exists());
	assert!(out_dir.join(MERGE_PLAN_ARTIFACT_PATH).exists());
	assert!(out_dir.join(MERGE_REPORT_ARTIFACT_PATH).exists());

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "blocked");
	assert_eq!(report["manual_conflict_count"], 1);
}

#[test]
fn merge_command_force_mode_returns_exit_3_and_keeps_placeholder_behavior() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	let mod_a = tmp.path().join("7821");
	let mod_b = tmp.path().join("7822");

	write_dlc_load(&playlist_path, &[("7821", "A"), ("7822", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor(&mod_b, "mod-b");
	stage_structural_manual_conflict(&mod_a, &mod_b);
	fs::create_dir_all(mod_b.join("common")).expect("create common dir");
	fs::write(mod_b.join("common").join("safe.txt"), "safe\n").expect("write safe file");

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--force",
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 0);
	assert!(stdout.contains("status: PARTIAL_SUCCESS"));
	assert!(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
	assert_eq!(
		fs::read_to_string(out_dir.join("common/safe.txt")).expect("read copied safe file"),
		"safe\n"
	);
	// Manual-conflict structural file resolved by --force placeholder
	assert!(out_dir.join(STRUCTURAL_CONFLICT_PATH).exists());

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "partial_success");
	assert_eq!(report["manual_conflict_count"], 1);
	assert_eq!(report["generated_file_count"], 1);
	assert_eq!(report["copied_file_count"], 1);
}

#[test]
fn merge_command_revalidates_generated_output_and_backfills_validation_buckets() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	let mod_a = tmp.path().join("7831");
	let mod_b = tmp.path().join("7832");

	write_dlc_load(&playlist_path, &[("7831", "A"), ("7832", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor(&mod_b, "mod-b");
	fs::create_dir_all(mod_a.join("events")).expect("create events dir");
	fs::create_dir_all(mod_b.join("events")).expect("create events dir");
	fs::create_dir_all(mod_a.join("localisation").join("english"))
		.expect("create localisation dir");
	fs::write(
		mod_a.join("events").join("shared.txt"),
		"namespace = test\ncountry_event = {\n\tid = test.1\n\ttitle = missing_title\n\ttrigger = {\n\t\thas_global_flag = missing_flag\n\t}\n\timmediate = {\n\t\tmissing_effect = { }\n\t}\n}\n",
	)
	.expect("write events a");
	fs::write(
		mod_b.join("events").join("shared.txt"),
		"namespace = test\ncountry_event = {\n\tid = test.2\n\ttitle = missing_title\n\ttrigger = {\n\t\thas_global_flag = missing_flag\n\t}\n\timmediate = {\n\t\tmissing_effect = { }\n\t}\n}\ncountry_event = {\n\tid = test.3\n\ttitle = known_title\n}\n",
	)
	.expect("write events b");
	fs::write(
		mod_a
			.join("localisation")
			.join("english")
			.join("test_l_english.yml"),
		"l_english:\n known_title:0 \"Known\"\n",
	)
	.expect("write localisation");

	let playlist_str = playlist_path.display().to_string();
	let out_str = out_dir.display().to_string();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 3);
	assert!(stdout.contains("status: FATAL"));
	assert!(out_dir.join("events/shared.txt").exists());

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "fatal");
	assert_eq!(report["manual_conflict_count"], 0);
	assert_eq!(report["generated_file_count"], 1);
	assert_eq!(report["validation"]["fatal_errors"], 0);
	assert_eq!(report["validation"]["strict_findings"], 2);
	assert_eq!(report["validation"]["parse_errors"], 0);
	assert_eq!(report["validation"]["unresolved_references"], 2);
	assert_eq!(report["validation"]["missing_localisation"], 2);
	assert!(
		report["validation"]["advisory_findings"]
			.as_u64()
			.is_some_and(|count| count >= 1)
	);
}

#[test]
fn default_base_game_mode_fails_when_game_root_is_missing() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	write_dlc_load(&playlist_path, &[("7601", "A")]);
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
	write_dlc_load(&playlist_path, &[("7701", "A")]);
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
fn check_parse_issue_report_writes_family_annotated_json() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let mod_root = tmp.path().join("7705");
	let report_path = tmp.path().join("parse-issues.json");

	write_dlc_load(&playlist_path, &[("7705", "A")]);
	write_descriptor(&mod_root, "mod-a");
	fs::create_dir_all(mod_root.join("localisation")).expect("create localisation dir");
	fs::write(
		mod_root.join("localisation").join("broken_l_english.yml"),
		"l_english:\nbroken.key:0 Missing quotes\n",
	)
	.expect("write broken localisation");

	let playlist_str = playlist_path.display().to_string();
	let report_str = report_path.display().to_string();
	let (code, _stdout, stderr) = run_foch(
		&[
			"check",
			playlist_str.as_str(),
			"--no-game-base",
			"--parse-issue-report",
			report_str.as_str(),
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stderr: {stderr}");

	let content = fs::read_to_string(report_path).expect("read parse issue report");
	let parsed: serde_json::Value =
		serde_json::from_str(&content).expect("parse parse issue report");
	let items = parsed.as_array().expect("parse issue report array");
	assert!(!items.is_empty());
	assert!(items.iter().any(|item| {
		item["family"] == "localisation" && item["path"] == "localisation/broken_l_english.yml"
	}));
}

#[test]
fn no_game_base_without_detectable_version_skips_mod_snapshot_cache() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let mod_root = tmp.path().join("7706");
	let cache_dir = tmp.path().join("mod-cache");

	write_dlc_load(&playlist_path, &[("7706", "A")]);
	write_descriptor(&mod_root, "mod-a");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root.join("events").join("event.txt"),
		"namespace = test\ncountry_event = { id = test.1 }\n",
	)
	.expect("write event");

	let playlist_str = playlist_path.display().to_string();
	let cache_dir_str = cache_dir.display().to_string();
	let (code, _stdout, stderr) = run_foch_with_env(
		&["check", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
		&[("FOCH_MOD_SNAPSHOT_CACHE_DIR", cache_dir_str.as_str())],
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(collect_gzip_files(&cache_dir).is_empty());
}

#[test]
fn check_no_game_base_builds_and_reuses_mod_snapshot_cache() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let mod_root = tmp.path().join("7711");
	let cache_dir = tmp.path().join("mod-cache");
	write_game_version(&tmp.path().join("eu4-game"), "11.0.0-test");

	write_dlc_load(&playlist_path, &[("7711", "A")]);
	write_descriptor(&mod_root, "mod-a");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root.join("events").join("event.txt"),
		"namespace = test\ncountry_event = { id = test.1 }\n",
	)
	.expect("write event");

	let playlist_str = playlist_path.display().to_string();
	let cache_dir_str = cache_dir.display().to_string();
	let envs = [("FOCH_MOD_SNAPSHOT_CACHE_DIR", cache_dir_str.as_str())];

	let (code, _stdout, stderr) = run_foch_with_env(
		&["check", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
		&envs,
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	let first_files = collect_gzip_files(&cache_dir);
	assert_eq!(first_files.len(), 1);
	assert!(first_files[0].to_string_lossy().contains("rules-v"));

	let (code, _stdout, stderr) = run_foch_with_env(
		&["check", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
		&envs,
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	let second_files = collect_gzip_files(&cache_dir);
	assert_eq!(second_files.len(), 1);

	fs::write(
		mod_root.join("events").join("event.txt"),
		"namespace = test\ncountry_event = { id = test.2 }\n",
	)
	.expect("rewrite event");

	let (code, _stdout, stderr) = run_foch_with_env(
		&["check", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
		&envs,
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	let third_files = collect_gzip_files(&cache_dir);
	assert_eq!(third_files.len(), 2);
}

#[test]
fn merge_plan_no_game_base_populates_mod_snapshot_cache() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let cache_dir = tmp.path().join("mod-cache");
	write_game_version(&tmp.path().join("eu4-game"), "11.1.0-test");

	write_dlc_load(&playlist_path, &[("7721", "A"), ("7722", "B")]);
	write_descriptor(&tmp.path().join("7721"), "mod-a");
	write_descriptor(&tmp.path().join("7722"), "mod-b");
	fs::create_dir_all(
		tmp.path()
			.join("7721")
			.join("common")
			.join("scripted_effects"),
	)
	.expect("create effects dir");
	fs::create_dir_all(
		tmp.path()
			.join("7722")
			.join("common")
			.join("scripted_effects"),
	)
	.expect("create effects dir");
	fs::write(
		tmp.path()
			.join("7721")
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = a }\n",
	)
	.expect("write effect");
	fs::write(
		tmp.path()
			.join("7722")
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = b }\n",
	)
	.expect("write effect");

	let playlist_str = playlist_path.display().to_string();
	let cache_dir_str = cache_dir.display().to_string();
	let (code, _stdout, stderr) = run_foch_with_env(
		&["merge-plan", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
		&[("FOCH_MOD_SNAPSHOT_CACHE_DIR", cache_dir_str.as_str())],
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	assert_eq!(collect_gzip_files(&cache_dir).len(), 2);
}

#[test]
fn data_build_install_and_list_round_trip() {
	let tmp = TempDir::new().expect("temp dir");
	let game_root = tmp.path().join("eu4-game");
	write_game_version(&game_root, "8.1.0-test");
	fs::create_dir_all(game_root.join("events")).expect("create events");
	fs::write(
		game_root.join("events").join("base.txt"),
		"namespace = base\ncountry_event = { id = base.1 }\n",
	)
	.expect("write base event");

	build_base_data_install(tmp.path(), &game_root);

	let (code, stdout, _stderr) = run_foch(&["data", "list", "--json"], tmp.path());
	assert_eq!(code, 0);
	let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse data list");
	let entry = parsed
		.as_array()
		.expect("list array")
		.iter()
		.find(|item| item["game"] == "eu4" && item["game_version"] == "8.1.0-test")
		.expect("installed entry");
	assert_eq!(entry["source"], "build");
	assert!(entry["install_path"].as_str().is_some());
	assert!(
		entry["analysis_rules_version"]
			.as_str()
			.unwrap_or("")
			.starts_with("rules-v")
	);
}

#[test]
fn check_uses_installed_base_data_to_resolve_base_symbols() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let game_root = tmp.path().join("eu4-game");
	let mod_root = tmp.path().join("7801");
	let output_path = tmp.path().join("result.json");

	write_dlc_load(&playlist_path, &[("7801", "A")]);
	write_descriptor(&mod_root, "mod-a");
	fs::create_dir_all(game_root.join("events")).expect("create events");
	fs::write(
		game_root.join("events").join("base.txt"),
		"namespace = base\ncountry_event = { id = base.1 option = { name = ok } }\n",
	)
	.expect("write base event");
	fs::create_dir_all(mod_root.join("events")).expect("create mod events");
	fs::write(
		mod_root.join("events").join("ref.txt"),
		"namespace = test\ncountry_event = { id = test.1 option = { country_event = { id = base.1 } } }\n",
	)
	.expect("write mod event");
	write_game_version(&game_root, "8.2.0-test");
	write_config(
		tmp.path(),
		format!("[game_path]\neu4 = \"{}\"\n", game_root.display()).as_str(),
	);
	build_base_data_install(tmp.path(), &game_root);

	let playlist_str = playlist_path.display().to_string();
	let output_str = output_path.display().to_string();
	let (code, _stdout, _stderr) = run_foch(
		&[
			"check",
			playlist_str.as_str(),
			"--format",
			"json",
			"--output",
			output_str.as_str(),
		],
		tmp.path(),
	);
	assert_eq!(code, 0);

	let content = fs::read_to_string(output_path).expect("read result");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse result");
	let findings = parsed["findings"].as_array().expect("findings array");
	assert!(
		!findings.iter().any(|item| {
			item["rule_id"] == "S002" && item["message"].as_str().unwrap_or("").contains("base.1")
		}),
		"base event reference should resolve through installed snapshot"
	);
}

#[test]
fn data_install_downloads_release_asset_from_manifest() {
	let tmp = TempDir::new().expect("temp dir");
	let game_root = tmp.path().join("eu4-game");
	let release_dir = tmp.path().join("release-data");
	write_game_version(&game_root, "9.1.0-test");
	fs::create_dir_all(game_root.join("events")).expect("create events");
	fs::write(
		game_root.join("events").join("base.txt"),
		"namespace = base\ncountry_event = { id = base.1 }\n",
	)
	.expect("write base event");
	write_config(
		tmp.path(),
		format!("[game_path]\neu4 = \"{}\"\n", game_root.display()).as_str(),
	);
	build_release_assets(tmp.path(), &game_root, &release_dir);

	let server = serve_directory(&release_dir);
	let (code, _stdout, stderr) = run_foch_with_env(
		&["data", "install", "eu4", "--game-version", "auto"],
		tmp.path(),
		&[("FOCH_DATA_RELEASE_BASE_URL", server.base_url.as_str())],
	);
	assert_eq!(code, 0, "stderr: {stderr}");

	let (code, stdout, _stderr) = run_foch(&["data", "list", "--json"], tmp.path());
	assert_eq!(code, 0);
	let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse data list");
	let entry = parsed
		.as_array()
		.expect("list array")
		.iter()
		.find(|item| item["game"] == "eu4" && item["game_version"] == "9.1.0-test")
		.expect("downloaded entry");
	assert_eq!(entry["source"], "download");
}

#[test]
fn data_build_emits_progress_and_profile_output() {
	let tmp = TempDir::new().expect("temp dir");
	let game_root = tmp.path().join("eu4-game");
	let output_dir = tmp.path().join("bundle");
	let profile_path = tmp.path().join("build-profile.json");
	write_game_version(&game_root, "10.1.0-test");
	fs::create_dir_all(game_root.join("events")).expect("create events");
	fs::create_dir_all(game_root.join("localisation")).expect("create localisation");
	fs::write(
		game_root.join("events").join("base.txt"),
		"namespace = base\ncountry_event = { id = base.1 }\n",
	)
	.expect("write base event");
	fs::write(
		game_root.join("localisation").join("base_l_english.yml"),
		"l_english:\n base.1.t:0 \"Base\"\n",
	)
	.expect("write localisation");

	let game_root_str = game_root.display().to_string();
	let output_dir_str = output_dir.display().to_string();
	let profile_str = profile_path.display().to_string();
	let (code, _stdout, stderr) = run_foch(
		&[
			"data",
			"build",
			"eu4",
			"--from-game-path",
			game_root_str.as_str(),
			"--game-version",
			"auto",
			"--output-dir",
			output_dir_str.as_str(),
			"--profile-out",
			profile_str.as_str(),
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(stderr.contains("[data build] detect_version: start"));
	assert!(stderr.contains("[data build] encode_snapshot: done"));
	assert!(stderr.contains("[data build] write_outputs: done"));

	let profile_raw = fs::read_to_string(&profile_path).expect("read profile");
	let profile: serde_json::Value = serde_json::from_str(&profile_raw).expect("parse profile");
	let stages = profile["stages"].as_array().expect("stages array");
	for name in [
		"detect_version",
		"collect_inventory",
		"discover_documents",
		"parse_documents",
		"build_semantic_index",
		"materialize_snapshot",
		"encode_snapshot",
		"write_outputs",
	] {
		assert!(
			stages.iter().any(|stage| stage["name"] == name),
			"missing stage {name}: {profile_raw}"
		);
	}
	assert!(profile["encoded_size_bytes"].as_u64().unwrap_or(0) > 0);
	assert_eq!(profile["inventory_file_count"], 2);
	assert_eq!(profile["document_count"], 2);
	assert_eq!(
		profile["parse_stats"]["clausewitz_mainline"]["documents"],
		1
	);
	assert_eq!(profile["parse_stats"]["localisation"]["documents"], 1);
	assert_eq!(profile["parse_stats"]["csv"]["documents"], 0);
	assert_eq!(profile["parse_stats"]["json"]["documents"], 0);
	assert_eq!(
		profile["encoded_sections"]
			.as_array()
			.expect("encoded sections array")
			.len(),
		5
	);
}
