use foch::model::{
	MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH, MERGED_MOD_DESCRIPTOR_PATH,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tempfile::TempDir;

#[path = "../../../packages/foch/tests/support/static_modifiers.rs"]
mod static_modifiers;

#[test]
fn static_modifiers_cli_preserves_contributions_and_defers_final_disagreement() {
	let scratch: TempDir = TempDir::new().expect("test scratch");
	let fixture: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../packages/foch/tests/fixtures/static_modifiers")
		.canonicalize()
		.expect("fixture root");
	let original: std::collections::BTreeMap<PathBuf, Vec<u8>> =
		static_modifiers::source_bytes(&fixture);
	write_game_path_config(scratch.path(), &fixture.join("base"));
	build_base_data_install(scratch.path(), &fixture.join("base"));
	for case in static_modifiers::CASES {
		let manifest: PathBuf =
			static_modifiers::write_manifest(&fixture, &scratch.path().join(case.name), case);
		for run in 0..2 {
			let out: PathBuf = scratch.path().join(case.name).join(format!("out-{run}"));
			let (code, stdout, stderr): (i32, String, String) = run_foch(
				&[
					"merge",
					manifest.to_str().unwrap(),
					"--out",
					out.to_str().unwrap(),
					"--non-interactive",
					"--provenance",
				],
				scratch.path(),
			);
			assert_eq!(code, 0, "{}: {stdout}\n{stderr}", case.name);
			assert!(!out.exists(), "preview must not create output");
			let (code, stdout, stderr): (i32, String, String) = run_foch(
				&[
					"merge",
					manifest.to_str().unwrap(),
					"--out",
					out.to_str().unwrap(),
					"--non-interactive",
					"--confirm",
					"--provenance",
				],
				scratch.path(),
			);
			assert_eq!(code, 0, "{}: {stdout}\n{stderr}", case.name);
			let report: foch::model::MergeReport = serde_json::from_slice(
				&fs::read(out.join(MERGE_REPORT_ARTIFACT_PATH)).expect("read report"),
			)
			.expect("decode report");
			static_modifiers::assert_output(case, &out, &report);
			if run == 1 && case.expected.is_some() {
				assert_eq!(
					fs::read(out.join(static_modifiers::OUTPUT)).unwrap(),
					fs::read(
						scratch
							.path()
							.join(case.name)
							.join("out-0")
							.join(static_modifiers::OUTPUT)
					)
					.unwrap(),
					"repeatable CLI output"
				);
			}
		}
	}
	assert_eq!(
		static_modifiers::source_bytes(&fixture),
		original,
		"CLI must preserve source bytes"
	);
}

#[test]
fn culture_cli_repairs_and_adapts_reviewed_sources_without_mutating_them() {
	use foch::project::{CultureRenameEntry, CultureRepairEntry, Project, SourceEdit};
	use sha2::{Digest, Sha256};
	let scratch = TempDir::new().unwrap();
	let game = scratch.path().join("game");
	let rename = scratch.path().join("rename");
	let bonus = scratch.path().join("bonus");
	let culture_path = "common/cultures/base.txt";
	let base = "g = { old = { primary = AAA } }";
	let broken = "g renamed = { primary = AAA male_names = { NewName } } }";
	write_game_version(&game, "culture-cli-1.0");
	write_script_file(&game, culture_path, base);
	write_script_file(
		&game,
		"common/scripted_effects/base.txt",
		"base_effect = { add_prestige = 1 }",
	);
	write_script_file(
		&game,
		"common/scripted_triggers/base.txt",
		"base_check = { primary_culture = old }",
	);
	write_descriptor(&rename, "Rename");
	write_script_file(&rename, culture_path, broken);
	write_descriptor(&bonus, "Bonus");
	write_script_file(
		&bonus,
		culture_path,
		"g = { old = { primary = AAA country = { discipline = 0.1 } } }",
	);
	write_script_file(
		&bonus,
		"common/scripted_effects/mechanic.txt",
		"mechanic = { change_culture = old set_country_flag = old }",
	);
	write_game_path_config(scratch.path(), &game);
	build_base_data_install(scratch.path(), &game);
	let mut project: Project = toml::from_str("[project]\ngame='eu4'\n[[project.mods]]\nid='rename'\npath='rename'\n[[project.mods]]\nid='bonus'\npath='bonus'\n").unwrap();
	let hash = format!("{:x}", Sha256::digest(broken.as_bytes()));
	project.cultures.renames.push(CultureRenameEntry {
		from: "old".into(),
		to: "renamed".into(),
		mod_id: "rename".into(),
		file: culture_path.into(),
		sha256: hash.clone(),
	});
	project.cultures.repairs.push(CultureRepairEntry {
		mod_id: "rename".into(),
		file: culture_path.into(),
		sha256: hash,
		edits: vec![SourceEdit {
			start: 1,
			end: 1,
			expected: "".into(),
			replacement: " = {".into(),
		}],
	});
	let manifest = scratch.path().join("foch.toml");
	fs::write(&manifest, toml::to_string(&project).unwrap()).unwrap();
	let out = scratch.path().join("out");
	let args = [
		"merge",
		manifest.to_str().unwrap(),
		"--out",
		out.to_str().unwrap(),
		"--non-interactive",
	];
	let (code, stdout, stderr) = run_foch(&args, scratch.path());
	assert_eq!(code, 0, "{stdout}\n{stderr}");
	assert!(!out.exists());
	let mut commit = args.to_vec();
	commit.push("--confirm");
	let (code, stdout, stderr) = run_foch(&commit, scratch.path());
	assert_eq!(code, 0, "{stdout}\n{stderr}");
	let report: foch::model::MergeReport =
		serde_json::from_slice(&fs::read(out.join(MERGE_REPORT_ARTIFACT_PATH)).unwrap()).unwrap();
	assert_eq!(
		report.status,
		foch::model::MergeReportStatus::Ready,
		"{report:#?}"
	);
	assert!(
		report.stale_vanilla_targets.is_empty(),
		"{:#?}",
		report.stale_vanilla_targets
	);
	let cultures = fs::read_to_string(out.join("common/cultures/zzz_foch_cultures.txt")).unwrap();
	assert!(
		cultures.contains("renamed =")
			&& cultures.contains("discipline = 0.1")
			&& cultures.contains("NewName"),
		"{cultures}"
	);
	let effect =
		fs::read_to_string(out.join("common/scripted_effects/zzz_foch_scripted_effects.txt"))
			.unwrap();
	assert!(
		effect.contains("change_culture = renamed") && effect.contains("set_country_flag = old")
	);
	assert!(
		fs::read_to_string(out.join("common/scripted_triggers/base.txt"))
			.unwrap()
			.contains("primary_culture = renamed")
	);
	assert_eq!(
		fs::read_to_string(rename.join(culture_path)).unwrap(),
		broken
	);
	assert_eq!(fs::read_to_string(game.join(culture_path)).unwrap(), base);
	write_script_file(&rename, culture_path, &format!("{broken}\n# Author update"));
	let stale_out = scratch.path().join("stale-out");
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			manifest.to_str().unwrap(),
			"--out",
			stale_out.to_str().unwrap(),
			"--non-interactive",
			"--confirm",
		],
		scratch.path(),
	);
	assert_ne!(code, 0, "{stdout}\n{stderr}");
	assert!(stderr.contains("stale culture decision"), "{stderr}");
	assert!(!stale_out.exists());
}

/// A test path as an argument, environment or configuration value. Test
/// directories are UTF-8; one that is not fails the test instead of being
/// rendered as some other path.
fn path_text(path: &Path) -> &str {
	path.to_str().expect("UTF-8 test path")
}

/// The text between the quotes of a descriptor `path` value, written by the
/// descriptor format's own encoder.
fn descriptor_path_value(path: &Path) -> String {
	foch::playset::descriptor::descriptor_path_text(path)
		.expect("a fixture directory has descriptor text")
}

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
			descriptor_path_value(&mod_root)
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

/// Stage a structural-merge conflict: both mods contribute the same event
/// file, but mod_b's content is malformed Clausewitz so
/// validate_structural_merge_inputs flags it and merge analysis leaves the path
/// for manual review.
const STRUCTURAL_CONFLICT_PATH: &str = "events/conflict.txt";

fn stage_structural_manual_conflict(mod_a: &Path, mod_b: &Path) {
	write_script_file(
		mod_a,
		STRUCTURAL_CONFLICT_PATH,
		"country_event = { id = test.1 }\n",
	);
	// Malformed Clausewitz: produces a parse diagnostic ("无法解析的语句起始 token"),
	// which downgrades the structural merge to ManualConflict.
	write_script_file(
		mod_b,
		STRUCTURAL_CONFLICT_PATH,
		"name { = invalid syntax with unclosed\nbraces\n",
	);
}

const DAG_CONFLICT_PATH: &str = "history/countries/conflict.txt";

fn idea_file(cost: &str) -> String {
	format!("group = {{\n\tidea = {{\n\t\tcost = {cost}\n\t}}\n}}\n")
}

#[allow(dead_code)]
fn stage_dag_downstream_conflict(
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
	write_descriptor(mod_base, "conflict-base");
	write_descriptor_with_dependencies(mod_a, "conflict-a", &["conflict-base"]);
	write_descriptor_with_dependencies(mod_b, "conflict-b", &["conflict-base"]);
	write_descriptor_with_dependencies(mod_c, "conflict-c", &["conflict-a", "conflict-b"]);
	write_script_file(mod_base, DAG_CONFLICT_PATH, &idea_file("old"));
	write_script_file(mod_a, DAG_CONFLICT_PATH, &idea_file("alpha"));
	write_script_file(mod_b, DAG_CONFLICT_PATH, &idea_file("beta"));
	write_script_file(mod_c, DAG_CONFLICT_PATH, &idea_file("gamma"));
}

/// Genuine sibling-overwrite conflict between A and B with no downstream
/// resolver. The DAG topo walk cannot auto-resolve this; an explicit
/// resolution is required to produce merged output.
fn stage_dag_genuine_conflict(playlist_path: &Path, mod_base: &Path, mod_a: &Path, mod_b: &Path) {
	write_dlc_load(
		playlist_path,
		&[("9101", "Base"), ("9102", "A"), ("9103", "B")],
	);
	write_descriptor(mod_base, "conflict-base");
	write_descriptor_with_dependencies(mod_a, "conflict-a", &["conflict-base"]);
	write_descriptor_with_dependencies(mod_b, "conflict-b", &["conflict-base"]);
	write_script_file(mod_base, DAG_CONFLICT_PATH, &idea_file("old"));
	write_script_file(mod_a, DAG_CONFLICT_PATH, &idea_file("alpha"));
	write_script_file(mod_b, DAG_CONFLICT_PATH, &idea_file("beta"));
}

fn write_config(path: &Path, content: &str) {
	fs::write(path.join("config.toml"), content).expect("write config");
}

fn game_path_config(game_root: &Path) -> String {
	let mut config = toml::Table::new();
	let mut game_path = toml::Table::new();
	game_path.insert(
		"eu4".into(),
		toml::Value::String(path_text(game_root).to_owned()),
	);
	config.insert("game_path".into(), toml::Value::Table(game_path));
	toml::to_string(&config).expect("serialize game path config")
}

fn write_game_path_config(config_dir: &Path, game_root: &Path) {
	write_config(config_dir, &game_path_config(game_root));
}

fn steam_root_config(steam_root: &Path) -> String {
	let mut config = toml::Table::new();
	config.insert(
		"steam_root_path".into(),
		toml::Value::String(path_text(steam_root).to_owned()),
	);
	toml::to_string(&config).expect("serialize steam root config")
}

fn write_no_auto_detect_config(config_dir: &Path) {
	write_config(
		config_dir,
		&steam_root_config(&config_dir.join("missing-steam")),
	);
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
	write_game_path_config(config_dir, &game_root);
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
	let cache_root = config_dir.join(".foch-cache");
	fs::create_dir_all(&home_dir).expect("create isolated home");
	fs::create_dir_all(&xdg_data_home).expect("create isolated xdg data");
	let mut command = Command::new(env!("CARGO_BIN_EXE_foch"));
	command
		.env("FOCH_CONFIG_DIR", config_dir)
		.env("FOCH_DATA_DIR", config_dir.join(".foch-data"))
		.env("FOCH_CACHE_ROOT", &cache_root)
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
fn plugin_plan_agrees_with_launch_resolution_for_an_undeclared_entry_digest() {
	use foch::plugin::{deployment, planner, selection, store};
	for identical in [true, false] {
		let scratch = TempDir::new().unwrap();
		let game = scratch.path().join("game");
		write_game_version(&game, "1.37.5.0");
		let mut dll = vec![0u8; 0x80];
		dll[..2].copy_from_slice(b"MZ");
		dll[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
		dll[0x40..0x44].copy_from_slice(b"PE\0\0");
		dll[0x44..0x46].copy_from_slice(&store::MACHINE_AMD64.to_le_bytes());
		let mut dependency = dll.clone();
		if !identical {
			dependency[0x70] = 1;
		}
		let manifest = format!(
			r#"schema = 1
[plugin]
id = "dev.foch.sample"
name = "Sample"
version = "1.0.0"
[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "*"
abi_major = 1
[entry]
kind = "native"
path = "sample.dll"
phase = "deferred"
[[files]]
path = "dependency/sample.dll"
sha256 = "{}"
"#,
			deployment::hash(&dependency)
		);
		let package = store::validate(vec![
			store::ArchiveEntry {
				path: "foch-plugin.toml".into(),
				data: manifest.into_bytes(),
			},
			store::ArchiveEntry {
				path: "sample.dll".into(),
				data: dll,
			},
			store::ArchiveEntry {
				path: "dependency/sample.dll".into(),
				data: dependency,
			},
		])
		.unwrap();
		let store_root = scratch.path().join(".foch-data/plugins/store");
		store::install(&store_root, &package).unwrap();
		let choices = selection::PlaysetSelections {
			plugins: BTreeMap::from([(
				"dev.foch.sample".into(),
				selection::Choice {
					version: "1.0.0".parse().unwrap(),
					enabled: true,
					config: BTreeMap::new(),
				},
			)]),
		};
		let selected = choices.to_selections();
		let mut selections = selection::Selections::default();
		selections.set_playset("test", choices);
		selections
			.save(&scratch.path().join("plugins/selections.toml"))
			.unwrap();
		let launchable = deployment::resolve(
			&planner::GameIdentity {
				game: "eu4".into(),
				version: "1.37.5".parse().unwrap(),
				platform: planner::WINDOWS_X64.into(),
			},
			&store_root,
			&selected,
		)
		.is_ok();
		assert_eq!(launchable, identical);
		let (code, stdout, stderr) = run_foch(
			&[
				"plugin",
				"plan",
				"--playset",
				"test",
				"--game-path",
				game.to_str().unwrap(),
				"--format",
				"json",
			],
			scratch.path(),
		);
		assert_eq!(code, i32::from(!launchable), "{stdout}\n{stderr}");
		let output: serde_json::Value = serde_json::from_str(&stdout).unwrap();
		assert_eq!(output["launchable"], launchable);
	}
}

#[test]
fn top_level_help_exposes_only_current_merge_commands() {
	let tmp = TempDir::new().expect("temp dir");
	let (help_code, stdout, help_stderr) = run_foch(&["--help"], tmp.path());

	assert_eq!(help_code, 0, "stderr: {help_stderr}");
	assert!(
		stdout
			.lines()
			.any(|line| line.trim_start().starts_with("merge ")),
		"stdout: {stdout}"
	);
	assert!(
		stdout
			.lines()
			.any(|line| line.trim_start().starts_with("input ")),
		"stdout: {stdout}"
	);
	assert!(!stdout.contains("merge-plan"), "stdout: {stdout}");

	let (rejected_code, _stdout, rejected_stderr) = run_foch(&["merge-plan"], tmp.path());
	assert_eq!(rejected_code, 2, "stderr: {rejected_stderr}");
	assert!(
		rejected_stderr.contains("unrecognized subcommand 'merge-plan'"),
		"stderr: {rejected_stderr}"
	);
}

#[test]
fn bare_foch_needs_a_terminal_and_initializes_nothing() {
	let tmp = TempDir::new().expect("temp dir");
	let config_dir = tmp.path().join("absent-config");
	let output = Command::new(env!("CARGO_BIN_EXE_foch"))
		.env("FOCH_CONFIG_DIR", &config_dir)
		.env("HOME", tmp.path().join("home"))
		.env("FOCH_CACHE_ROOT", tmp.path().join("cache"))
		.output()
		.expect("run bare foch");

	let stderr = String::from_utf8_lossy(&output.stderr);
	assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
	assert!(stderr.contains("needs a TTY"), "stderr: {stderr}");
	assert!(output.stdout.is_empty());
	assert!(
		!config_dir.exists(),
		"bare foch must not initialize configuration"
	);
}

/// Without INPUT_SOURCE, `foch input inspect` reports the current EU4 input
/// bare `foch` shows. What it finds depends on the machine, so only the
/// shape is checked.
#[test]
fn input_inspect_without_a_source_describes_the_current_input_as_json() {
	let tmp = TempDir::new().expect("temp dir");
	let config_dir = tmp.path().join("absent-config");
	let output = Command::new(env!("CARGO_BIN_EXE_foch"))
		.env("FOCH_CONFIG_DIR", &config_dir)
		.env("HOME", tmp.path().join("home"))
		.env("FOCH_CACHE_ROOT", tmp.path().join("cache"))
		.args(["input", "inspect", "--format", "json"])
		.output()
		.expect("run input inspect");

	let stdout = String::from_utf8_lossy(&output.stdout);
	assert!(
		matches!(output.status.code(), Some(0 | 2)),
		"stderr: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	let input: serde_json::Value = serde_json::from_str(&stdout).expect("JSON input");
	assert!(input.get("readiness").is_some(), "{stdout}");
	assert!(input.get("issues").is_some(), "{stdout}");
	assert!(
		!config_dir.exists(),
		"inspection must not create configuration"
	);
}

#[test]
fn exclusions_and_json_apply_only_to_the_current_input() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist = tmp.path().join("playlist.json");
	write_dlc_load(&playlist, &[("7251", "A")]);
	write_descriptor(&tmp.path().join("7251"), "mod-a");
	let playlist = path_text(&playlist).to_owned();
	let out = path_text(&tmp.path().join("out")).to_owned();

	let (code, _stdout, stderr) = run_foch(
		&[
			"merge",
			playlist.as_str(),
			"--out",
			out.as_str(),
			"--non-interactive",
			"--exclude",
			"7251",
		],
		tmp.path(),
	);
	assert_eq!(code, 1, "stderr: {stderr}");
	assert!(stderr.contains("omit INPUT_SOURCE"), "stderr: {stderr}");

	let (code, _stdout, stderr) = run_foch(
		&["input", "inspect", playlist.as_str(), "--format", "json"],
		tmp.path(),
	);
	assert_eq!(code, 1, "stderr: {stderr}");
	assert!(stderr.contains("omit INPUT_SOURCE"), "stderr: {stderr}");
}

#[test]
fn version_names_the_embedded_cwt_schema_and_any_override() {
	let version = |override_dir: Option<&str>| {
		let mut command = Command::new(env!("CARGO_BIN_EXE_foch"));
		command
			.arg("--version")
			.env_remove("FOCH_CWTOOLS_SCHEMA_DIR");
		if let Some(dir) = override_dir {
			command.env("FOCH_CWTOOLS_SCHEMA_DIR", dir);
		}
		let output = command.output().expect("run foch --version");
		assert!(output.status.success(), "{output:?}");
		String::from_utf8(output.stdout).expect("stdout utf8")
	};
	let embedded = format!(
		"cwt-schema {} (embedded)",
		foch::game::eu4::EMBEDDED_CWT_SCHEMA_ID
	);

	let plain = version(None);
	assert!(plain.contains(&embedded), "stdout: {plain}");
	assert!(!plain.contains("overridden"), "stdout: {plain}");

	let overridden = version(Some("/maintainer/cwt"));
	assert!(overridden.contains(&embedded), "stdout: {overridden}");
	assert!(
		overridden.contains("cwt-schema overridden by FOCH_CWTOOLS_SCHEMA_DIR=/maintainer/cwt"),
		"stdout: {overridden}"
	);
}

#[test]
fn input_inspect_reads_manifest_path_mod() {
	let tmp = TempDir::new().expect("tempdir");
	let mod_root = tmp.path().join("local-mod");
	write_descriptor(&mod_root, "Local Mod");
	let manifest = tmp.path().join("foch.toml");
	fs::write(
		&manifest,
		r#"
[project]
game = "eu4"

[[project.mods]]
id = "local_mod"
path = "local-mod"
"#,
	)
	.expect("write manifest");

	let manifest_arg = path_text(&manifest).to_owned();
	let (code, stdout, stderr) = run_foch(&["input", "inspect", manifest_arg.as_str()], tmp.path());

	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(stdout.contains("input:"));
	assert!(stdout.contains("game: eu4"));
	assert!(stdout.contains("id=local_mod"));
	assert!(stdout.contains("path="));
}

#[test]
fn merge_rejects_missing_enabled_inputs_before_base_loading_or_export() {
	let tmp: TempDir = TempDir::new().unwrap();
	let present: PathBuf = tmp.path().join("present");
	write_descriptor(&present, "Present");
	write_script_file(
		&present,
		"common/scripted_effects/test.txt",
		"present = { add_prestige = 1 }",
	);
	let manifest: PathBuf = tmp.path().join("foch.toml");
	for partial in [false, true] {
		let present_entry: &str = if partial {
			"[[project.mods]]\npath = 'present'\n"
		} else {
			""
		};
		fs::write(&manifest, format!("[project]\ngame = 'eu4'\n{present_entry}[[project.mods]]\nid = 'missing-local'\npath = 'missing'\n[[project.mods]]\nsteam_id = '999999999999999999'\n")).unwrap();
		for no_base in [false, true] {
			for confirm in [false, true] {
				let out: PathBuf = tmp
					.path()
					.join(format!("out-{partial}-{no_base}-{confirm}"));
				let mut args: Vec<&str> = vec![
					"merge",
					manifest.to_str().unwrap(),
					"--out",
					out.to_str().unwrap(),
					"--non-interactive",
					"--force",
				];
				if no_base {
					args.push("--no-game-base");
				}
				if confirm {
					args.push("--confirm");
				}
				let (code, stdout, stderr): (i32, String, String) = run_foch(&args, tmp.path());
				assert_eq!(code, 1, "{stdout}\n{stderr}");
				assert!(
					stderr.contains("missing-local") && stderr.contains("999999999999999999"),
					"{stderr}"
				);
				assert!(stderr.contains("unavailable enabled mod"), "{stderr}");
				assert!(!out.exists(), "missing input must not publish output");
			}
		}
	}
	fs::write(&manifest, "[project]\ngame = 'eu4'\n[[project.mods]]\npath = 'present'\n[[project.mods]]\npath = 'missing'\nenabled = false\n").unwrap();
	let out: PathBuf = tmp.path().join("disabled-out");
	let (code, stdout, stderr): (i32, String, String) = run_foch(
		&[
			"merge",
			manifest.to_str().unwrap(),
			"--out",
			out.to_str().unwrap(),
			"--no-game-base",
			"--confirm",
			"--non-interactive",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "{stdout}\n{stderr}");
}

/// A mod holding both `common/a/b.txt` and a file literally named
/// `common/a\b.txt` stops the merge before anything is written: the second
/// name has no portable game path, and folding it onto the first would merge
/// two files as one. Only a Unix host can create the second name.
#[cfg(unix)]
#[test]
fn merge_stops_on_a_mod_file_whose_name_has_no_game_path() {
	let tmp: TempDir = TempDir::new().expect("temp dir");
	let mod_root: PathBuf = tmp.path().join("alias_mod");
	write_descriptor(&mod_root, "Alias Mod");
	write_script_file(&mod_root, "common/a/b.txt", "nested = yes\n");
	let literal: PathBuf = mod_root.join("common").join(r"a\b.txt");
	fs::write(&literal, "literal = yes\n").expect("write literal-backslash file");
	let manifest: PathBuf = tmp.path().join("foch.toml");
	// The id differs from the directory name, so only the inventory walker's
	// own diagnostic can name `alias_id`.
	fs::write(
		&manifest,
		"[project]\ngame = 'eu4'\n[[project.mods]]\nid = 'alias_id'\npath = 'alias_mod'\n",
	)
	.expect("write manifest");

	for confirm in [false, true] {
		let out: PathBuf = tmp.path().join(format!("out-{confirm}"));
		let mut args: Vec<&str> = vec![
			"merge",
			path_text(&manifest),
			"--out",
			path_text(&out),
			"--no-game-base",
			"--non-interactive",
		];
		if confirm {
			args.push("--confirm");
		}
		let (code, stdout, stderr): (i32, String, String) = run_foch(&args, tmp.path());
		assert_eq!(code, 1, "{stdout}\n{stderr}");
		for expected in [
			"mod alias_id: ".to_string(),
			literal.display().to_string(),
			"no portable game path".to_string(),
		] {
			assert!(stderr.contains(&expected), "{expected} not in {stderr}");
		}
		assert!(!out.exists(), "an unportable input must not publish output");
	}
}

/// The merged descriptor's `path` is read back without escapes, so an output
/// directory whose name holds a `"` has no spelling there: the merge stops
/// before publishing instead of writing a descriptor that names another
/// directory. A clean single-mod merge, so nothing else stops it.
#[cfg(unix)]
#[test]
fn merge_stops_on_an_output_directory_a_descriptor_cannot_name() {
	let tmp: TempDir = TempDir::new().expect("temp dir");
	let mod_root: PathBuf = tmp.path().join("present");
	write_descriptor(&mod_root, "Present");
	write_script_file(
		&mod_root,
		"common/scripted_effects/test.txt",
		"present = { add_prestige = 1 }\n",
	);
	let manifest: PathBuf = tmp.path().join("foch.toml");
	fs::write(
		&manifest,
		"[project]\ngame = 'eu4'\n[[project.mods]]\npath = 'present'\n",
	)
	.expect("write manifest");

	for confirm in [false, true] {
		let out: PathBuf = tmp.path().join(format!("merged \"{confirm}\""));
		let mut args: Vec<&str> = vec![
			"merge",
			path_text(&manifest),
			"--out",
			path_text(&out),
			"--no-game-base",
			"--non-interactive",
		];
		if confirm {
			args.push("--confirm");
		}
		let (code, stdout, stderr): (i32, String, String) = run_foch(&args, tmp.path());
		assert_eq!(code, 1, "{stdout}\n{stderr}");
		assert!(
			stderr.contains("cannot be named in a descriptor"),
			"{stderr}"
		);
		assert!(!out.exists(), "an unnameable output must not be published");
	}
	// The same merge into a nameable directory publishes it.
	let out: PathBuf = tmp.path().join("merged");
	let (code, stdout, stderr): (i32, String, String) = run_foch(
		&[
			"merge",
			path_text(&manifest),
			"--out",
			path_text(&out),
			"--no-game-base",
			"--non-interactive",
			"--confirm",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "{stdout}\n{stderr}");
	assert!(out.join("descriptor.mod").is_file());
}

#[test]
fn input_inspect_does_not_initialize_configuration() {
	let tmp = TempDir::new().expect("tempdir");
	let config_dir = tmp.path().join("absent-config");
	let mod_root = tmp.path().join("local-mod");
	write_descriptor(&mod_root, "Local Mod");
	let project = tmp.path().join("foch.toml");
	fs::write(
		&project,
		r#"
[project]
game = "eu4"

[[project.mods]]
id = "local_mod"
path = "local-mod"
"#,
	)
	.expect("write project");
	let output = Command::new(env!("CARGO_BIN_EXE_foch"))
		.env("FOCH_CONFIG_DIR", &config_dir)
		.env("HOME", tmp.path().join("home"))
		.env("FOCH_CACHE_ROOT", tmp.path().join("cache"))
		.args(["input", "inspect", path_text(&project)])
		.output()
		.expect("run input inspect");

	assert!(
		output.status.success(),
		"stderr: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert!(
		!config_dir.exists(),
		"input inspection must not create configuration"
	);
}

fn build_base_data_install(config_dir: &Path, game_root: &Path) {
	let game_root_str = path_text(game_root).to_owned();
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
	let game_root_str = path_text(game_root).to_owned();
	let output_dir_str = path_text(output_dir).to_owned();
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

#[test]
fn static_server_waits_for_complete_request_headers() {
	use std::io::Read;
	use std::net::TcpStream;
	use std::time::Instant;
	let root = TempDir::new().unwrap();
	fs::write(root.path().join("asset.bin"), b"fixture asset").unwrap();
	let server = serve_directory(root.path());
	let mut stream = TcpStream::connect(server.base_url.trim_start_matches("http://")).unwrap();
	stream.write_all(b"GET /asset.bin HTTP/1.1\r\n").unwrap();
	stream.set_nonblocking(true).unwrap();
	let mut buffer = [0; 512];
	// A timed-out blocking receive can invalidate a Windows socket. Peek
	// without consuming data while leaving this connection usable.
	let deadline = Instant::now() + Duration::from_millis(200);
	loop {
		let early_response = stream.peek(&mut buffer);
		assert!(
			matches!(early_response, Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock),
			"server replied before the request headers were complete: {early_response:?}"
		);
		if Instant::now() >= deadline {
			break;
		}
		thread::sleep(Duration::from_millis(5));
	}
	stream.set_nonblocking(false).unwrap();
	stream
		.set_read_timeout(Some(Duration::from_secs(5)))
		.unwrap();
	stream
		.write_all(b"Host: localhost\r\nConnection: close\r\n\r\n")
		.unwrap();
	// Content-Length frames this response; a graceful socket EOF is not
	// required to finish the HTTP transfer.
	let expected =
		b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\nfixture asset";
	let mut response = vec![0; expected.len()];
	stream.read_exact(&mut response).unwrap();
	assert_eq!(response, expected);
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
					// Windows accepts inherit the listener's nonblocking mode.
					stream.set_nonblocking(false).expect("set blocking request");
					stream
						.set_read_timeout(Some(Duration::from_secs(5)))
						.expect("set request timeout");
					stream
						.set_write_timeout(Some(Duration::from_secs(5)))
						.expect("set response timeout");
					let mut request_line = String::new();
					let mut reader = BufReader::new(
						stream.try_clone().expect("clone stream for request reader"),
					);
					if reader.read_line(&mut request_line).is_err() {
						continue;
					}
					// Consume the complete request before replying and closing.
					// Unread socket data can reset the connection on Windows.
					let mut header = String::new();
					let complete = loop {
						header.clear();
						match reader.read_line(&mut header) {
							Ok(0) | Err(_) => break false,
							Ok(_) if header == "\r\n" || header == "\n" => break true,
							Ok(_) => {}
						}
					};
					if !complete {
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
						if stream.write_all(header.as_bytes()).is_err() {
							continue;
						}
						let _ = stream.write_all(&bytes);
					} else {
						let body = b"not found";
						let header = format!(
							"HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
							body.len()
						);
						if stream.write_all(header.as_bytes()).is_err() {
							continue;
						}
						let _ = stream.write_all(body);
					}
				}
				Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
					thread::sleep(Duration::from_millis(25));
				}
				Err(_err) => {
					thread::sleep(Duration::from_millis(25));
				}
			}
		}
	});

	StaticServer {
		base_url: format!("http://127.0.0.1:{}", addr.port()),
		stop_tx,
		handle: Some(handle),
	}
}

fn collect_mod_snapshot_files(root: &Path) -> Vec<std::path::PathBuf> {
	let mut files = Vec::new();
	if !root.exists() {
		return files;
	}
	for entry in walkdir::WalkDir::new(root)
		.into_iter()
		.filter_map(Result::ok)
	{
		if entry.file_type().is_file()
			&& entry.path().extension().and_then(|value| value.to_str()) == Some("rkyv")
		{
			files.push(entry.path().to_path_buf());
		}
	}
	files.sort();
	files
}

struct CacheLayerFixture {
	mods: PathBuf,
	diffs: PathBuf,
	dag_base: PathBuf,
	cwt_rules: PathBuf,
	parse: PathBuf,
	parse_legacy: PathBuf,
}

fn target_temp_dir() -> TempDir {
	let root = Path::new(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.and_then(Path::parent)
		.and_then(Path::parent)
		.expect("repo root")
		.join("target")
		.join("cli-integration-temp");
	fs::create_dir_all(&root).expect("create target temp root");
	TempDir::new_in(root).expect("temp dir in target")
}

fn seed_cache_layers(root: &Path) -> CacheLayerFixture {
	let fixture = CacheLayerFixture {
		mods: root.join("mods").join("mods-entry.rkyv"),
		diffs: root.join("diffs").join("v6.0.0").join("diffs-entry.bin"),
		dag_base: root
			.join("dag-base")
			.join("v12.0.0")
			.join("dag-base-entry.bin"),
		cwt_rules: root.join("cwt-rules").join("v0.12.0").join("cwt-entry.bin"),
		parse: root
			.join("parse")
			.join("v14.0.0")
			.join("aa")
			.join("bb")
			.join("parse-entry.bin"),
		parse_legacy: root.join("parse_cache").join("legacy-entry.bin"),
	};
	for path in [
		&fixture.mods,
		&fixture.diffs,
		&fixture.dag_base,
		&fixture.cwt_rules,
		&fixture.parse,
		&fixture.parse_legacy,
	] {
		fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
		fs::write(path, b"cache-entry").expect("write cache entry");
	}
	fixture
}

fn cache_env_values(root: &Path) -> Vec<(String, String)> {
	vec![("FOCH_CACHE_ROOT".to_string(), path_text(root).to_owned())]
}

fn read_json_file(path: &Path) -> serde_json::Value {
	let content = fs::read_to_string(path).expect("read json file");
	serde_json::from_str(&content).expect("parse json file")
}

#[test]
fn cache_commands_stats_where_and_clean_noop() {
	let tmp = TempDir::new().expect("temp dir");

	let (where_code, where_stdout, where_stderr) = run_foch(&["cache", "where"], tmp.path());
	assert_eq!(where_code, 0, "stderr: {where_stderr}");
	assert!(where_stdout.contains("foch"), "stdout: {where_stdout}");

	let (stats_code, stats_stdout, stats_stderr) = run_foch(&["cache", "stats"], tmp.path());
	assert_eq!(stats_code, 0, "stderr: {stats_stderr}");
	assert!(stats_stdout.contains("cache root:"));
	assert!(stats_stdout.contains("mods"));
	assert!(stats_stdout.contains("diffs"));
	assert!(stats_stdout.contains("dag-base"));
	assert!(stats_stdout.contains("cwt-rules"));
	assert!(stats_stdout.contains("parse"));
	assert!(!stats_stdout.contains("input-digests"));

	let (clean_code, clean_stdout, clean_stderr) =
		run_foch(&["cache", "clean", "--older-than", "9999"], tmp.path());
	assert_eq!(clean_code, 0, "stderr: {clean_stderr}");
	assert!(clean_stdout.contains("removed: 0 entries"));
}

#[test]
fn missing_playset_path_returns_exit_1() {
	let tmp = TempDir::new().expect("temp dir");
	let missing = tmp.path().join("missing.json");
	let missing_string = path_text(&missing).to_owned();
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

	let playlist_str = path_text(&playlist_path).to_owned();
	let args = ["check", playlist_str.as_str(), "--strict", "--no-game-base"];
	let (code, stdout, _stderr) = run_foch(&args, tmp.path());

	assert_eq!(code, 2);
	assert!(stdout.contains("duplicate-playset-entry"));
}

#[test]
fn check_json_output_can_be_deserialized() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("result.json");

	write_dlc_load(&playlist_path, &[("5001", "A")]);
	write_descriptor(&tmp.path().join("5001"), "mod-a");

	let playlist_str = path_text(&playlist_path).to_owned();
	let output_str = path_text(&output_path).to_owned();
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

/// A finding about a script names its file by game path under `path`, as it
/// always has. A finding about an input file (here a descriptor) leaves
/// `path` null and names the physical file under `source_file`.
#[test]
fn check_json_findings_keep_game_paths_and_input_files_apart() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let output_path = tmp.path().join("result.json");

	write_dlc_load(&playlist_path, &[("5101", "A"), ("5102", "B")]);
	let mod_a = tmp.path().join("5101");
	let mod_b = tmp.path().join("5102");
	write_descriptor(&mod_a, "mod-a");
	write_descriptor_with_dependencies(&mod_b, "mod-b", &["absent-mod"]);
	write_script_file(&mod_a, "common/shared.txt", "a = 1\n");
	write_script_file(&mod_b, "common/shared.txt", "a = 2\n");

	let playlist_str = path_text(&playlist_path).to_owned();
	let output_str = path_text(&output_path).to_owned();
	let args = [
		"check",
		playlist_str.as_str(),
		"--format",
		"json",
		"--output",
		output_str.as_str(),
		"--no-game-base",
	];
	let (code, _stdout, stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0, "{stderr}");

	let content = fs::read_to_string(output_path).expect("read json output");
	let parsed: serde_json::Value = serde_json::from_str(&content).expect("deserialize result");
	let findings = parsed["findings"].as_array().expect("findings array");
	let finding = |rule_id: &str| {
		findings
			.iter()
			.find(|finding| finding["rule_id"] == rule_id)
			.unwrap_or_else(|| panic!("no {rule_id} finding in {content}"))
	};

	let conflict = finding("file-overwrite-conflict");
	assert_eq!(conflict["path"], "common/shared.txt");
	assert!(conflict.get("source_file").is_none(), "{conflict}");

	let dependency = finding("missing-mod-dependency");
	assert!(dependency["path"].is_null(), "{dependency}");
	let source_file = Path::new(
		dependency["source_file"]
			.as_str()
			.expect("the descriptor is named"),
	);
	assert!(
		source_file.ends_with(Path::new("5102").join("descriptor.mod")),
		"{}",
		source_file.display()
	);
}

#[test]
fn check_rejects_removed_graph_flags() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	write_dlc_load(&playlist_path, &[]);

	let playlist_str = path_text(&playlist_path).to_owned();
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

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
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

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
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

	let deps = read_json_file(&out_dir.join("input/mod-deps.json"));
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
fn graph_modules_command_writes_module_report() {
	let tmp = target_temp_dir();
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("graphs");
	let mod_a = tmp.path().join("9021");
	let mod_b = tmp.path().join("9022");

	write_dlc_load(&playlist_path, &[("9021", "A"), ("9022", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor(&mod_b, "mod-b");
	write_script_file(
		&mod_a,
		"events/ref.txt",
		"namespace = test\ncountry_event = { id = test.1 immediate = { shared_effect = { } } }\n",
	);
	write_script_file(
		&mod_b,
		"common/scripted_effects/effects.txt",
		"shared_effect = { log = provider }\n",
	);

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let report_path = out_dir.join(".foch").join("module-report.json");
	let (code, stdout, stderr) = run_foch(
		&[
			"graph",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--modules",
			"--mode",
			"semantic",
			"--no-game-base",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(
		stdout.contains(&format!(
			"module report written to {}",
			report_path.display()
		)),
		"stdout: {stdout}"
	);

	let report = read_json_file(&report_path);
	assert!(report["module_count"].as_u64().is_some());
	assert!(report["node_count"].as_u64().is_some());
	assert!(report["mods"].as_array().is_some());
}

#[test]
fn semantic_graph_requires_family_argument() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("graphs");
	write_dlc_load(&playlist_path, &[]);

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
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

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
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
		.join("../../packages/foch")
		.canonicalize()
		.expect("repo root");
	let playlist_path = repo_root
		.join("tests")
		.join("fixtures")
		.join("eu4_real_minimized")
		.join("playlist.json");

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
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
		stderr.contains("semantic graph resolve input: start"),
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
	write_game_path_config(tmp.path(), &game_root);
	build_base_data_install(tmp.path(), &game_root);

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
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
fn simplify_command_refuses_to_write_into_the_source_mod() {
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
	write_game_path_config(tmp.path(), &game_root);
	build_base_data_install(tmp.path(), &game_root);

	let playlist_str = path_text(&playlist_path).to_owned();
	let (code, _stdout, stderr) = run_foch(
		&[
			"simplify",
			playlist_str.as_str(),
			"--target",
			"9031",
			"--in-place",
		],
		tmp.path(),
	);
	assert_ne!(code, 0, "--in-place must no longer be accepted");
	assert!(stderr.contains("--in-place"), "stderr: {stderr}");

	let nested_out = mod_a.join("clean");
	for out in [mod_a.as_path(), nested_out.as_path(), tmp.path()] {
		let out_str = path_text(out).to_owned();
		let (code, _stdout, stderr) = run_foch(
			&[
				"simplify",
				playlist_str.as_str(),
				"--target",
				"9031",
				"--out",
				out_str.as_str(),
			],
			tmp.path(),
		);
		assert_ne!(code, 0, "output {out_str} overlaps the source mod");
		assert!(
			stderr.contains("overlaps the root of input mod 9031"),
			"stderr: {stderr}"
		);
	}
	assert_eq!(
		fs::read_to_string(&target_file).expect("source effect is untouched"),
		"shared_effect = { log = base }
"
	);
	assert!(!mod_a.join("simplify-report.json").exists());
	assert!(!nested_out.exists());
}

#[test]
fn simplify_command_refuses_an_output_inside_another_input_mod() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let game_root = tmp.path().join("eu4-game");
	let mod_a = tmp.path().join("9031");
	let mod_b = tmp.path().join("9032");
	let other_file = mod_b
		.join("common")
		.join("scripted_effects")
		.join("other.txt");

	write_dlc_load(&playlist_path, &[("9031", "A"), ("9032", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor(&mod_b, "mod-b");
	write_game_version(&game_root, "12.2.0-test");
	fs::create_dir_all(game_root.join("common").join("scripted_effects"))
		.expect("create base effects dir");
	fs::create_dir_all(mod_a.join("common").join("scripted_effects"))
		.expect("create mod a effects dir");
	fs::create_dir_all(mod_b.join("common").join("scripted_effects"))
		.expect("create mod b effects dir");
	fs::write(
		mod_a
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"shared_effect = { log = a }
",
	)
	.expect("write mod a effect");
	fs::write(
		&other_file,
		"other_effect = { log = b }
",
	)
	.expect("write mod b effect");
	write_game_path_config(tmp.path(), &game_root);
	build_base_data_install(tmp.path(), &game_root);

	let playlist_str = path_text(&playlist_path).to_owned();
	let nested_out = mod_b.join("clean");
	for out in [mod_b.as_path(), nested_out.as_path()] {
		let out_str = path_text(out).to_owned();
		let (code, _stdout, stderr) = run_foch(
			&[
				"simplify",
				playlist_str.as_str(),
				"--target",
				"9031",
				"--out",
				out_str.as_str(),
			],
			tmp.path(),
		);
		assert_ne!(code, 0, "output {out_str} overlaps another input mod");
		assert!(
			stderr.contains("overlaps the root of input mod 9032"),
			"stderr: {stderr}"
		);
	}
	assert_eq!(
		fs::read_to_string(&other_file).expect("other input mod is untouched"),
		"other_effect = { log = b }
"
	);
	assert!(!mod_b.join("simplify-report.json").exists());
	assert!(!nested_out.exists());
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
fn merge_preview_returns_exit_0_when_plan_has_manual_conflicts() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");

	write_dlc_load(&playlist_path, &[("7251", "A"), ("7252", "B")]);
	write_descriptor(&tmp.path().join("7251"), "mod-a");
	write_descriptor(&tmp.path().join("7252"), "mod-b");
	stage_structural_manual_conflict(&tmp.path().join("7251"), &tmp.path().join("7252"));

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--non-interactive",
		],
		tmp.path(),
	);

	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
	assert!(stdout.contains("Foch Merge Review"), "stdout: {stdout}");
	assert!(
		stdout.contains("[unsupported_input] events/conflict.txt"),
		"stdout: {stdout}"
	);
	assert!(!out_dir.exists(), "preview must not create --out");
}

#[test]
fn merge_preview_limits_each_disposition_and_can_show_every_unit() {
	let tmp: TempDir = TempDir::new().expect("temp dir");
	let playlist_path: PathBuf = tmp.path().join("playlist.json");
	let out_dir: PathBuf = tmp.path().join("merged-out");
	let mod_a: PathBuf = tmp.path().join("7251");
	let mod_b: PathBuf = tmp.path().join("7252");
	write_dlc_load(&playlist_path, &[("7251", "A"), ("7252", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor(&mod_b, "mod-b");
	stage_structural_manual_conflict(&mod_a, &mod_b);
	fs::create_dir_all(mod_a.join("gfx")).expect("create gfx dir");
	for index in 0..25 {
		fs::write(mod_a.join(format!("gfx/copy-{index:02}.dds")), [0, 1, 2])
			.expect("write copied asset");
	}
	for mod_root in [&mod_a, &mod_b] {
		for index in 0..24 {
			fs::copy(
				mod_root.join("events/conflict.txt"),
				mod_root.join(format!("events/z-conflict-{index:02}.txt")),
			)
			.expect("write additional unsupported unit");
		}
	}
	let mut args: Vec<&str> = vec![
		"merge",
		path_text(&playlist_path),
		"--out",
		path_text(&out_dir),
		"--no-game-base",
		"--non-interactive",
	];
	let (code, stdout, stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
	assert_eq!(stdout.matches("- [copy]").count(), 20, "{stdout}");
	assert_eq!(
		stdout.matches("- [unsupported_input]").count(),
		20,
		"{stdout}"
	);
	assert!(stdout.contains("  copy: 25"), "{stdout}");
	assert!(stdout.contains("  unsupported_input: 25"), "{stdout}");
	assert!(
		stdout.contains("[unsupported_input] events/conflict.txt"),
		"{stdout}"
	);
	assert!(stdout.contains("5 more copy units"), "{stdout}");
	assert!(
		stdout.contains("5 more unsupported_input units"),
		"{stdout}"
	);
	assert!(stdout.contains("--review-all"), "{stdout}");
	assert!(!stdout.contains("gfx/copy-24.dds"), "{stdout}");
	assert!(!out_dir.exists(), "preview must not create --out");

	args.push("--review-all");
	let (code, stdout, stderr) = run_foch(&args, tmp.path());
	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
	assert_eq!(stdout.matches("- [copy]").count(), 25, "{stdout}");
	assert_eq!(
		stdout.matches("- [unsupported_input]").count(),
		25,
		"{stdout}"
	);
	assert!(stdout.contains("gfx/copy-24.dds"), "{stdout}");
	assert!(
		stdout.contains("[unsupported_input] events/conflict.txt"),
		"{stdout}"
	);
	assert!(!stdout.contains("more copy units"), "{stdout}");
	assert!(!out_dir.exists(), "full review must not create --out");
}

#[test]
fn merge_command_defaults_to_plan_without_writing_output() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	let mod_root = tmp.path().join("7804");

	write_dlc_load(&playlist_path, &[("7804", "A")]);
	write_descriptor(&mod_root, "mod-a");
	fs::create_dir_all(mod_root.join("common")).expect("create common dir");
	fs::write(mod_root.join("common").join("only.txt"), "from-a\n").expect("write file");

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
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

	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
	assert!(stdout.contains("Foch Merge Review"), "stdout: {stdout}");
	assert!(
		stdout.contains("[copy] common/only.txt"),
		"stdout: {stdout}"
	);
	assert!(!out_dir.exists(), "preview must not create --out");
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

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--confirm",
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

	let (repeat_code, repeat_stdout, repeat_stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--confirm",
			"--non-interactive",
		],
		tmp.path(),
	);
	assert_eq!(
		repeat_code, 1,
		"stdout: {repeat_stdout}\nstderr: {repeat_stderr}"
	);
	assert!(repeat_stdout.contains("Foch Merge Review"));
	assert!(
		repeat_stderr.contains("separate interactive confirmation"),
		"stderr: {repeat_stderr}"
	);
	assert_eq!(
		fs::read_to_string(out_dir.join("common/only.txt")).expect("read preserved output"),
		"from-a\n"
	);
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

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--ignore-dep",
			"7902:7901",
			"--confirm",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
	assert!(stdout.contains("status: READY"));
	let output =
		fs::read_to_string(out_dir.join("common/scripted_effects/zzz_foch_scripted_effects.txt"))
			.expect("read merged output");
	assert!(output.contains("effect_a"), "output: {output}");
	assert!(output.contains("effect_b"), "output: {output}");

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["dep_overrides_applied"][0]["mod_id"], "7902");
	assert_eq!(report["dep_overrides_applied"][0]["dep_id"], "7901");
	assert_eq!(report["dep_overrides_applied"][0]["source"], "cli");
}

#[test]
fn merge_command_skips_unresolved_dag_conflict_by_default() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--confirm",
		],
		tmp.path(),
	);
	assert_eq!(code, 0);
	assert!(stdout.contains("status: PARTIAL_SUCCESS"));
	assert!(!out_dir.join(DAG_CONFLICT_PATH).exists());
	assert!(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "partial_success");
	assert_eq!(report["manual_conflict_count"], 1);
	assert_eq!(report["generated_file_count"], 0);
	assert!(
		report["conflict_resolutions"][0]["leaf_conflicts"]
			.as_array()
			.is_some_and(|items| !items.is_empty())
	);
}

/// `--review-json` writes the review before confirmation: an analysis
/// without `--confirm` leaves the output absent but the review written, with
/// the dependency edges, the conflict's candidates and its decision point.
#[test]
fn merge_review_json_is_written_without_committing() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	let review_path = tmp.path().join("merge-review.json");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			path_text(&playlist_path),
			"--out",
			path_text(&out_dir),
			"--no-game-base",
			"--non-interactive",
			"--review-json",
			path_text(&review_path),
		],
		tmp.path(),
	);
	assert_eq!(
		code, 0,
		"{stdout}
{stderr}"
	);
	assert!(!out_dir.exists());
	let review = read_json_file(&review_path);
	assert_eq!(review["schema"], "foch.merge_review.v1");
	let edges = review["dependencies"].as_array().expect("dependency edges");
	for child in ["9102", "9103"] {
		assert!(
			edges.iter().any(|edge| edge["child"] == child
				&& edge["parent"] == "9101"
				&& edge["status"] == "active"),
			"{edges:#?}"
		);
	}
	let leaf = review["conflicts"][0]["nodes"]
		.as_array()
		.expect("address nodes")
		.iter()
		.flat_map(|node| node["conflicts"].as_array().cloned().unwrap_or_default())
		.next()
		.expect("a conflict leaf");
	let rendered = leaf["candidates"]
		.as_array()
		.expect("candidates")
		.iter()
		.map(|candidate| {
			candidate["rendered"]
				.as_str()
				.unwrap_or_default()
				.to_owned()
		})
		.collect::<Vec<_>>();
	assert!(
		rendered.iter().any(|text| text.contains("alpha"))
			&& rendered.iter().any(|text| text.contains("beta")),
		"{leaf:#}"
	);
	let point = &review["decisions"][0];
	assert_eq!(point["conflict_id"], leaf["conflict_id"]);
	let conflict_scope = &point["options"][0]["scopes"][0];
	assert_eq!(conflict_scope["scope"], "conflict");
	assert_eq!(
		conflict_scope["decision"]["conflict_id"],
		leaf["conflict_id"]
	);
	assert!(
		conflict_scope.get("resolution").is_none(),
		"{conflict_scope:#}"
	);
	assert_eq!(point["options"][0]["scopes"][1]["scope"], "file");
}

/// `--review-json` refuses, before analysis, to overwrite a file that is not
/// an earlier review, such as the playset, and to write into `--out`. A
/// repeated run may replace its own earlier review.
#[test]
fn merge_review_json_never_overwrites_an_input_or_writes_into_the_output() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);
	let playlist = fs::read(&playlist_path).expect("read playlist");
	let run = |review: &Path| {
		run_foch(
			&[
				"merge",
				path_text(&playlist_path),
				"--out",
				path_text(&out_dir),
				"--no-game-base",
				"--non-interactive",
				"--review-json",
				path_text(review),
			],
			tmp.path(),
		)
	};

	let (code, stdout, stderr) = run(&playlist_path);
	assert_ne!(
		code, 0,
		"{stdout}
{stderr}"
	);
	assert!(stderr.contains("not a foch merge review"), "{stderr}");
	assert!(
		!stdout.contains("Foch Merge Review"),
		"refused before analysis: {stdout}"
	);
	assert_eq!(fs::read(&playlist_path).expect("read playlist"), playlist);

	let (code, stdout, stderr) = run(&out_dir.join("review.json"));
	assert_ne!(
		code, 0,
		"{stdout}
{stderr}"
	);
	assert!(stderr.contains("inside --out"), "{stderr}");
	assert!(!out_dir.exists());

	// `..` is applied where a write would apply it, whether or not `--out`
	// exists yet.
	let (code, stdout, stderr) = run(&out_dir.join("..").join("outside.json"));
	assert_eq!(code, 0, "{stdout}\n{stderr}");
	assert!(tmp.path().join("outside.json").is_file());
	fs::create_dir_all(out_dir.join("sub")).expect("create output subdirectory");
	let (code, stdout, stderr) = run(&out_dir.join("sub").join("..").join("review.json"));
	assert_ne!(code, 0, "{stdout}\n{stderr}");
	assert!(stderr.contains("inside --out"), "{stderr}");
	assert!(!out_dir.join("review.json").exists());
	fs::remove_dir_all(&out_dir).expect("remove output");

	// A source mod is a read-only input: the review is refused there too.
	let in_mod = tmp.path().join("9102").join("review.json");
	let (code, stdout, stderr) = run(&in_mod);
	assert_ne!(code, 0, "{stdout}\n{stderr}");
	assert!(stderr.contains("read-only input"), "{stderr}");
	assert!(!in_mod.exists());

	let review_path = tmp.path().join("review.json");
	for _ in 0..2 {
		let (code, stdout, stderr) = run(&review_path);
		assert_eq!(
			code, 0,
			"{stdout}
{stderr}"
		);
		assert_eq!(
			read_json_file(&review_path)["schema"],
			"foch.merge_review.v1"
		);
	}
}

#[test]
fn merge_command_force_writes_placeholder_only_for_genuine_user_choice() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--force",
			"--no-game-base",
			"--confirm",
		],
		tmp.path(),
	);

	assert_eq!(code, 0);
	assert!(stdout.contains("status: PARTIAL_SUCCESS"));
	assert!(out_dir.join(DAG_CONFLICT_PATH).exists());

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "partial_success");
	assert_eq!(report["manual_conflict_count"], 1);
	assert_eq!(report["unsupported_input_count"], 0);
	assert_eq!(report["engine_failure_count"], 0);
	assert_eq!(report["generated_file_count"], 1);
}

#[test]
fn merge_command_non_interactive_does_not_enable_tui_prompting() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--non-interactive",
		],
		tmp.path(),
	);

	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
	assert!(stdout.contains("Foch Merge Review"), "stdout: {stdout}");
	assert!(!stderr.contains("interactive mode:"), "stderr: {stderr}");
	assert!(!stderr.contains("interactive TUI"), "stderr: {stderr}");
	assert!(
		!out_dir.exists(),
		"--non-interactive must not imply --confirm"
	);
}

#[test]
fn merge_command_cli_prompt_selects_simple_prompt_handler() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--cli-prompt",
			"--confirm",
		],
		tmp.path(),
	);

	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
	assert!(
		stderr.contains("interactive mode: simple prompt"),
		"stderr: {stderr}"
	);
	assert!(
		stderr.contains("stdin/stderr is not a TTY"),
		"stderr: {stderr}"
	);
	assert!(!stderr.contains("ratatui UI"), "stderr: {stderr}");
	assert!(!stderr.contains("interactive TUI"), "stderr: {stderr}");
}

#[test]
fn merge_command_default_unresolved_conflict_prints_resolution_tip_to_stderr() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	stage_dag_genuine_conflict(
		&playlist_path,
		&tmp.path().join("9101"),
		&tmp.path().join("9102"),
		&tmp.path().join("9103"),
	);

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--confirm",
		],
		tmp.path(),
	);
	assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
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
	assert!(stderr.contains("foch.toml"), "stderr: {stderr}");
	assert!(
		stderr.contains("handler = \"last_writer\""),
		"stderr: {stderr}"
	);
	assert!(
		stderr.contains("Foch committed the safe units"),
		"stderr: {stderr}"
	);
}

#[test]
fn merge_command_defers_unsupported_input_and_exports_safe_files() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let out_dir = tmp.path().join("merged-out");
	let mod_a = tmp.path().join("7811");
	let mod_b = tmp.path().join("7812");

	write_dlc_load(&playlist_path, &[("7811", "A"), ("7812", "B")]);
	write_descriptor(&mod_a, "mod-a");
	write_descriptor(&mod_b, "mod-b");
	stage_structural_manual_conflict(&mod_a, &mod_b);
	fs::create_dir_all(mod_b.join("common")).expect("create common dir");
	fs::write(mod_b.join("common").join("safe.txt"), "safe\n").expect("write safe file");

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--confirm",
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
	assert!(!out_dir.join(STRUCTURAL_CONFLICT_PATH).exists());
	assert!(out_dir.join(MERGE_PLAN_ARTIFACT_PATH).exists());
	assert!(out_dir.join(MERGE_REPORT_ARTIFACT_PATH).exists());

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "partial_success");
	assert_eq!(report["manual_conflict_count"], 0);
	assert_eq!(report["unsupported_input_count"], 1);
	assert_eq!(report["engine_failure_count"], 0);
	assert_eq!(report["copied_file_count"], 1);
}

#[test]
fn merge_command_force_mode_does_not_override_unsupported_input() {
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

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--force",
			"--no-game-base",
			"--confirm",
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
	// Malformed input is unsupported, not a user-choice conflict; --force must
	// not commit a placeholder for it.
	assert!(!out_dir.join(STRUCTURAL_CONFLICT_PATH).exists());

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "partial_success");
	assert_eq!(report["manual_conflict_count"], 0);
	assert_eq!(report["unsupported_input_count"], 1);
	assert_eq!(report["engine_failure_count"], 0);
	assert_eq!(report["generated_file_count"], 0);
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

	let playlist_str = path_text(&playlist_path).to_owned();
	let out_str = path_text(&out_dir).to_owned();
	let (code, stdout, _stderr) = run_foch(
		&[
			"merge",
			playlist_str.as_str(),
			"--out",
			out_str.as_str(),
			"--no-game-base",
			"--confirm",
		],
		tmp.path(),
	);
	assert_eq!(code, 0);
	assert!(stdout.contains("status: READY"));
	assert!(out_dir.join("events/shared.txt").exists());

	let report = read_json_file(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH));
	assert_eq!(report["status"], "ready");
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
	write_no_auto_detect_config(&config_dir);

	let playlist_str = path_text(&playlist_path).to_owned();
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
	write_no_auto_detect_config(&config_dir);

	let playlist_str = path_text(&playlist_path).to_owned();
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

	let playlist_str = path_text(&playlist_path).to_owned();
	let report_str = path_text(&report_path).to_owned();
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
	let cache_root = tmp.path().join("cache");

	write_dlc_load(&playlist_path, &[("7706", "A")]);
	write_descriptor(&mod_root, "mod-a");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root.join("events").join("event.txt"),
		"namespace = test\ncountry_event = { id = test.1 }\n",
	)
	.expect("write event");

	let playlist_str = path_text(&playlist_path).to_owned();
	let cache_root_str = path_text(&cache_root).to_owned();
	let (code, _stdout, stderr) = run_foch_with_env(
		&["check", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
		&[("FOCH_CACHE_ROOT", cache_root_str.as_str())],
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(collect_mod_snapshot_files(&cache_root).is_empty());
}

#[test]
fn check_no_game_base_does_not_persist_unversioned_mod_snapshot_cache() {
	let tmp = TempDir::new().expect("temp dir");
	let playlist_path = tmp.path().join("playlist.json");
	let mod_root = tmp.path().join("7711");
	let cache_root = tmp.path().join("cache");

	write_dlc_load(&playlist_path, &[("7711", "A")]);
	write_descriptor(&mod_root, "mod-a");
	fs::create_dir_all(mod_root.join("events")).expect("create events");
	fs::write(
		mod_root.join("events").join("event.txt"),
		"namespace = test\ncountry_event = { id = test.1 }\n",
	)
	.expect("write event");

	let playlist_str = path_text(&playlist_path).to_owned();
	let cache_root_str = path_text(&cache_root).to_owned();
	let envs = [("FOCH_CACHE_ROOT", cache_root_str.as_str())];

	let (code, _stdout, stderr) = run_foch_with_env(
		&["check", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
		&envs,
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	let first_files = collect_mod_snapshot_files(&cache_root);
	assert!(first_files.is_empty());

	let (code, _stdout, stderr) = run_foch_with_env(
		&["check", playlist_str.as_str(), "--no-game-base"],
		tmp.path(),
		&envs,
	);
	assert_eq!(code, 0, "stderr: {stderr}");
	let second_files = collect_mod_snapshot_files(&cache_root);
	assert!(second_files.is_empty());

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
	let third_files = collect_mod_snapshot_files(&cache_root);
	assert!(third_files.is_empty());
}

#[test]
fn cache_clean_layer_filter_targets_only_specified_layer() {
	let tmp = target_temp_dir();
	let cache_root = tmp.path().join("cache");
	let fixture = seed_cache_layers(&cache_root);
	thread::sleep(Duration::from_millis(20));
	let env_values = cache_env_values(&cache_root);
	let env_refs = env_values
		.iter()
		.map(|(key, value)| (key.as_str(), value.as_str()))
		.collect::<Vec<_>>();

	let (code, stdout, stderr) = run_foch_with_env(
		&["cache", "clean", "--layer", "mods", "--older-than", "0"],
		tmp.path(),
		&env_refs,
	);

	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(stdout.contains("removed:"));
	assert!(!fixture.mods.exists());
	assert!(fixture.diffs.exists());
	assert!(fixture.dag_base.exists());
	assert!(fixture.cwt_rules.exists());
	assert!(fixture.parse.exists());
	assert!(fixture.parse_legacy.exists());
}

#[test]
fn cache_clean_parse_includes_the_legacy_parse_root() {
	let tmp = target_temp_dir();
	let cache_root = tmp.path().join("cache");
	let fixture = seed_cache_layers(&cache_root);
	let env_values = cache_env_values(&cache_root);
	let env_refs = env_values
		.iter()
		.map(|(key, value)| (key.as_str(), value.as_str()))
		.collect::<Vec<_>>();

	let (code, _stdout, stderr) = run_foch_with_env(
		&["cache", "clean", "--layer", "parse", "--older-than", "9999"],
		tmp.path(),
		&env_refs,
	);

	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(fixture.parse.exists());
	assert!(!fixture.parse_legacy.exists());
}

#[test]
fn cache_clear_all_wipes_every_layer() {
	let tmp = target_temp_dir();
	let cache_root = tmp.path().join("cache");
	let fixture = seed_cache_layers(&cache_root);
	let env_values = cache_env_values(&cache_root);
	let env_refs = env_values
		.iter()
		.map(|(key, value)| (key.as_str(), value.as_str()))
		.collect::<Vec<_>>();

	let (code, stdout, stderr) = run_foch_with_env(
		&["cache", "clear", "--layer", "all", "--yes"],
		tmp.path(),
		&env_refs,
	);

	assert_eq!(code, 0, "stderr: {stderr}");
	assert!(stdout.contains("removed:"));
	assert!(!fixture.mods.exists());
	assert!(!fixture.diffs.exists());
	assert!(!fixture.dag_base.exists());
	assert!(!fixture.cwt_rules.exists());
	assert!(!fixture.parse.exists());
	assert!(!fixture.parse_legacy.exists());
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
	write_game_path_config(tmp.path(), &game_root);
	build_base_data_install(tmp.path(), &game_root);

	let playlist_str = path_text(&playlist_path).to_owned();
	let output_str = path_text(&output_path).to_owned();
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
			item["rule_id"] == "unresolved-call-target"
				&& item["message"].as_str().unwrap_or("").contains("base.1")
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
	write_game_path_config(tmp.path(), &game_root);
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

	let game_root_str = path_text(&game_root).to_owned();
	let output_dir_str = path_text(&output_dir).to_owned();
	let profile_str = path_text(&profile_path).to_owned();
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
	let encoded_sections = profile["encoded_sections"]
		.as_array()
		.expect("encoded sections array");
	assert_eq!(encoded_sections.len(), 6);
	assert!(
		encoded_sections
			.iter()
			.any(|section| section["name"] == "parsed_scripts"),
		"missing parsed_scripts section: {profile_raw}"
	);
}

const JOBS_EVENT_FILE_COUNT: usize = 4;
const JOBS_OPINION_PATH: &str = "common/opinion_modifiers/00_opinion_modifiers.txt";
const JOBS_EFFECTS_PATH: &str = "common/scripted_effects/p609_effects.txt";
const JOBS_HISTORY_PATH: &str = "history/countries/P09 - P609.txt";
const JOBS_FLAG_PATH: &str = "gfx/flags/P09.tga";

fn jobs_event_file(index: usize, first_title: &str, second_title: &str) -> String {
	format!(
		"namespace = p609\ncountry_event = {{\n\tid = p609.{index}1\n\ttitle = {first_title}\n\tis_triggered_only = yes\n}}\ncountry_event = {{\n\tid = p609.{index}2\n\ttitle = {second_title}\n\tis_triggered_only = yes\n}}\n"
	)
}

/// Stage a two-mod playset over a synthetic EU4 base that yields every kind of
/// merge unit: event files where each mod edits a different event, one
/// static-modifier database module spanning two directories, a safe and a
/// conflicting definition module, a conflicting history file, a localisation
/// merge, a single-contributor copy and a binary last-writer overlay.
///
/// The base reports 1.37.5, the version with load rules, so `static_modifiers`
/// and `event_modifiers` compose one database module. The base also ships an
/// event modifier: without one that directory has no verified ancestor and the
/// module fails instead of merging.
fn stage_jobs_playset(root: &Path) -> (PathBuf, PathBuf) {
	let game_root: PathBuf = root.join("eu4-base");
	let (game, mod_a, mod_b): (&Path, &Path, &Path) = (
		&game_root,
		&root.join("mods").join("a"),
		&root.join("mods").join("b"),
	);
	write_descriptor(mod_a, "p609-a");
	write_descriptor(mod_b, "p609-b");
	let static_modifier_file = |tax: &str, morale: &str| -> String {
		format!("shared = {{\n\tglobal_tax_modifier = {tax}\n\tland_morale = {morale}\n}}\n")
	};
	let opinion =
		|value: u32| -> String { format!("p609_opinion = {{\n\topinion = {value}\n}}\n") };
	let country =
		|capital: u32| -> String { format!("government = monarchy\ncapital = {capital}\n") };
	let base_effect: &str = "p609_effect = { add_prestige = 1 }\n";
	let mut files: Vec<(&Path, String, String)> = vec![
		(game, "version.txt".into(), "1.37.5\n".into()),
		(
			game,
			static_modifiers::SOURCE.into(),
			static_modifier_file("0.10", "0.10"),
		),
		(
			mod_a,
			static_modifiers::SOURCE.into(),
			static_modifier_file("0.15", "0.10"),
		),
		(
			mod_b,
			static_modifiers::SOURCE.into(),
			static_modifier_file("0.10", "0.20"),
		),
		(
			game,
			"common/event_modifiers/00_event_modifiers.txt".into(),
			"p609_base_event_modifier = {\n\tglobal_tax_modifier = 0.01\n}\n".into(),
		),
		(
			mod_b,
			"common/event_modifiers/p609_event_modifiers.txt".into(),
			"p609_event_modifier = {\n\tglobal_tax_modifier = 0.05\n}\n".into(),
		),
		(game, JOBS_OPINION_PATH.into(), opinion(10)),
		(mod_a, JOBS_OPINION_PATH.into(), opinion(20)),
		(mod_b, JOBS_OPINION_PATH.into(), opinion(30)),
		(game, JOBS_EFFECTS_PATH.into(), base_effect.into()),
		(
			mod_a,
			JOBS_EFFECTS_PATH.into(),
			format!("{base_effect}p609_effect_a = {{ add_prestige = 2 }}\n"),
		),
		(
			mod_b,
			JOBS_EFFECTS_PATH.into(),
			format!("{base_effect}p609_effect_b = {{ add_legitimacy = 3 }}\n"),
		),
		(game, JOBS_HISTORY_PATH.into(), country(1)),
		(mod_a, JOBS_HISTORY_PATH.into(), country(2)),
		(mod_b, JOBS_HISTORY_PATH.into(), country(3)),
		(
			mod_a,
			"localisation/p609_l_english.yml".into(),
			"l_english:\n p609_from_a:0 \"From A\"\n".into(),
		),
		(
			mod_b,
			"localisation/p609_l_english.yml".into(),
			"l_english:\n p609_from_b:0 \"From B\"\n".into(),
		),
		(
			mod_a,
			"localisation/p609_only_l_english.yml".into(),
			"l_english:\n p609_only_a:0 \"Only A\"\n".into(),
		),
	];
	for index in 0..JOBS_EVENT_FILE_COUNT {
		let path: String = format!("events/p609_events_{index}.txt");
		let base_first: String = format!("p609_{index}1_t");
		let base_second: String = format!("p609_{index}2_t");
		files.extend([
			(
				game,
				path.clone(),
				jobs_event_file(index, &base_first, &base_second),
			),
			(
				mod_a,
				path.clone(),
				jobs_event_file(index, &format!("p609_{index}1_from_a"), &base_second),
			),
			(
				mod_b,
				path,
				jobs_event_file(index, &base_first, &format!("p609_{index}2_from_b")),
			),
		]);
	}
	for (dir, path, content) in &files {
		write_script_file(dir, path, content);
	}
	for (dir, bytes) in [(mod_a, b"flag-a\x00\x01"), (mod_b, b"flag-b\x00\x02")] {
		let path: PathBuf = dir.join(JOBS_FLAG_PATH);
		fs::create_dir_all(path.parent().expect("flag parent")).expect("create flag dir");
		fs::write(path, bytes).expect("write binary flag");
	}
	let manifest: PathBuf = root.join("foch.toml");
	fs::write(
		&manifest,
		"[project]\ngame = 'eu4'\n[[project.mods]]\nid = 'p609-a'\npath = 'mods/a'\n[[project.mods]]\nid = 'p609-b'\npath = 'mods/b'\n",
	)
	.expect("write manifest");
	(game_root, manifest)
}

/// Commit the playset into a fresh output directory with `--jobs <jobs>`,
/// returning the output directory and stdout.
fn commit_merge_with_jobs(scratch: &Path, manifest: &Path, jobs: &str) -> (PathBuf, String) {
	let out: PathBuf = scratch.join(format!("out-jobs-{jobs}"));
	// A cache of its own keeps one run from reusing the other's analysis.
	let cache: PathBuf = scratch.join(format!("cache-jobs-{jobs}"));
	assert!(!out.exists() && !cache.exists(), "each run starts fresh");
	let (code, stdout, stderr): (i32, String, String) = run_foch_with_env(
		&[
			"merge",
			manifest.to_str().unwrap(),
			"--out",
			out.to_str().unwrap(),
			"--confirm",
			"--non-interactive",
			"--provenance",
			"--jobs",
			jobs,
		],
		scratch,
		&[("FOCH_CACHE_ROOT", cache.to_str().unwrap())],
	);
	assert_eq!(code, 0, "--jobs {jobs}\nstdout: {stdout}\nstderr: {stderr}");
	assert!(
		stderr.contains(&format!(" workers={jobs} ")),
		"--jobs {jobs} must reach materialization: {stderr}"
	);
	(out, stdout)
}

/// Decode a JSON artifact, check that `field` holds the expected kind of
/// value, and replace it with null.
fn mask_json_field(
	tree: &mut BTreeMap<PathBuf, Vec<u8>>,
	path: &str,
	field: &str,
	expected: fn(&serde_json::Value) -> bool,
) {
	let bytes: &mut Vec<u8> = tree
		.get_mut(Path::new(path))
		.unwrap_or_else(|| panic!("missing {path}"));
	let mut value: serde_json::Value =
		serde_json::from_slice(bytes).unwrap_or_else(|err| panic!("decode {path}: {err}"));
	let slot: &mut serde_json::Value = value
		.get_mut(field)
		.unwrap_or_else(|| panic!("{path} has no {field}"));
	assert!(expected(slot), "{path}: unexpected {field}: {slot}");
	*slot = serde_json::Value::Null;
	*bytes = serde_json::to_vec_pretty(&value).expect("encode masked JSON");
}

/// Every file of a committed merge, masking only what differs between runs by
/// design: the output directory in descriptor.mod, the plan's creation time
/// and the wall-clock time spent on definition modules.
fn normalized_merge_tree(out: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
	let mut tree: BTreeMap<PathBuf, Vec<u8>> = static_modifiers::source_bytes(out);
	let descriptor: &mut Vec<u8> = tree
		.get_mut(Path::new(MERGED_MOD_DESCRIPTOR_PATH))
		.expect("merged descriptor");
	let mut text: String = String::from_utf8(std::mem::take(descriptor)).expect("UTF-8 descriptor");
	// The canonical form first: on macOS it contains the given path.
	for form in [
		out.canonicalize().expect("canonical out"),
		out.to_path_buf(),
	] {
		text = text.replace(&descriptor_path_value(&form), "<out>");
	}
	*descriptor = text.into_bytes();
	mask_json_field(
		&mut tree,
		MERGE_PLAN_ARTIFACT_PATH,
		"generated_at",
		serde_json::Value::is_string,
	);
	mask_json_field(
		&mut tree,
		MERGE_REPORT_ARTIFACT_PATH,
		"definition_module_elapsed_ms",
		serde_json::Value::is_u64,
	);
	tree
}

#[test]
fn merge_output_is_identical_for_one_and_many_jobs() {
	let scratch: TempDir = TempDir::new().expect("test scratch");
	let (game, manifest): (PathBuf, PathBuf) = stage_jobs_playset(scratch.path());
	write_game_path_config(scratch.path(), &game);
	build_base_data_install(scratch.path(), &game);
	let (serial, serial_stdout): (PathBuf, String) =
		commit_merge_with_jobs(scratch.path(), &manifest, "1");
	let (parallel, parallel_stdout): (PathBuf, String) =
		commit_merge_with_jobs(scratch.path(), &manifest, "4");

	// Equality proves nothing unless the playset reaches every unit kind.
	let report: foch::model::MergeReport = serde_json::from_slice(
		&fs::read(serial.join(MERGE_REPORT_ARTIFACT_PATH)).expect("read report"),
	)
	.expect("decode report");
	assert_eq!(
		report.status,
		foch::model::MergeReportStatus::PartialSuccess,
		"{report:#?}"
	);
	assert_eq!(report.unsupported_input_count, 0, "{report:#?}");
	assert_eq!(report.engine_failure_count, 0, "{report:#?}");
	assert!(
		report.generated_file_count > 0
			&& report.copied_file_count > 0
			&& report.overlay_file_count > 0,
		"{report:#?}"
	);
	let deferred: Vec<(&str, foch::model::DeferredUnitReason)> = report
		.conflict_resolutions
		.iter()
		.map(|conflict| (conflict.path.as_str(), conflict.deferred_reason))
		.collect();
	let opinion_output: &str = "common/opinion_modifiers/zzz_foch_opinion_modifiers.txt";
	assert_eq!(
		deferred,
		[
			(
				opinion_output,
				foch::model::DeferredUnitReason::NeedsUserChoice
			),
			(
				JOBS_HISTORY_PATH,
				foch::model::DeferredUnitReason::NeedsUserChoice
			),
		]
	);
	for path in [opinion_output, JOBS_HISTORY_PATH] {
		assert!(!serial.join(path).exists(), "deferred {path} is withheld");
	}
	for index in 0..JOBS_EVENT_FILE_COUNT {
		let merged: String =
			fs::read_to_string(serial.join(format!("events/p609_events_{index}.txt")))
				.expect("read merged events");
		assert!(
			merged.contains(&format!("p609_{index}1_from_a"))
				&& merged.contains(&format!("p609_{index}2_from_b")),
			"{merged}"
		);
	}
	// One database unit, analyzed per namespace, writes both directories.
	assert!(
		serial_stdout.contains("id: module:CStaticModifierDataBase/CStaticModifierDataBase"),
		"{serial_stdout}"
	);
	let merged_static: String =
		fs::read_to_string(serial.join(static_modifiers::OUTPUT)).expect("read static modifiers");
	assert!(
		merged_static.contains("global_tax_modifier = 0.15")
			&& merged_static.contains("land_morale = 0.20"),
		"{merged_static}"
	);
	let merged_event: String =
		fs::read_to_string(serial.join("common/event_modifiers/zzz_foch_event_modifiers.txt"))
			.expect("read event modifiers");
	assert!(
		merged_event.contains("p609_event_modifier"),
		"{merged_event}"
	);
	let merged_effects: String =
		fs::read_to_string(serial.join("common/scripted_effects/zzz_foch_scripted_effects.txt"))
			.expect("read scripted effects");
	assert!(
		merged_effects.contains("p609_effect_a") && merged_effects.contains("p609_effect_b"),
		"{merged_effects}"
	);
	let localisation: String = fs::read_to_string(serial.join("localisation/p609_l_english.yml"))
		.expect("read merged localisation");
	assert!(
		localisation.contains("p609_from_a") && localisation.contains("p609_from_b"),
		"{localisation}"
	);
	assert_eq!(
		fs::read(serial.join("localisation/p609_only_l_english.yml")).expect("read copy"),
		b"l_english:\n p609_only_a:0 \"Only A\"\n"
	);
	assert_eq!(
		fs::read(serial.join(JOBS_FLAG_PATH)).expect("read overlay"),
		b"flag-b\x00\x02"
	);

	assert_eq!(serial_stdout, parallel_stdout, "review and report listings");
	let expected: BTreeMap<PathBuf, Vec<u8>> = normalized_merge_tree(&serial);
	let actual: BTreeMap<PathBuf, Vec<u8>> = normalized_merge_tree(&parallel);
	assert_eq!(
		expected.keys().collect::<Vec<&PathBuf>>(),
		actual.keys().collect::<Vec<&PathBuf>>(),
		"output file sets"
	);
	for (path, bytes) in &expected {
		assert!(
			*bytes == actual[path],
			"{} differs\n--- --jobs 1\n{}\n--- --jobs 4\n{}",
			path.display(),
			String::from_utf8_lossy(bytes),
			String::from_utf8_lossy(&actual[path])
		);
	}
}

#[test]
fn merge_rejects_zero_jobs() {
	let tmp: TempDir = TempDir::new().expect("temp dir");
	let playlist_path: PathBuf = tmp.path().join("playlist.json");
	let out_dir: PathBuf = tmp.path().join("merged-out");
	let mod_root: PathBuf = tmp.path().join("7861");
	write_dlc_load(&playlist_path, &[("7861", "A")]);
	write_descriptor(&mod_root, "mod-a");
	write_script_file(&mod_root, "common/only.txt", "from-a\n");

	let (code, stdout, stderr): (i32, String, String) = run_foch(
		&[
			"merge",
			playlist_path.to_str().unwrap(),
			"--out",
			out_dir.to_str().unwrap(),
			"--no-game-base",
			"--confirm",
			"--non-interactive",
			"--jobs",
			"0",
		],
		tmp.path(),
	);
	assert_eq!(code, 2, "stdout: {stdout}\nstderr: {stderr}");
	assert!(
		stderr.contains("invalid value '0' for '--jobs"),
		"stderr: {stderr}"
	);
	assert!(!stdout.contains("Foch Merge Review"), "stdout: {stdout}");
	assert!(
		!out_dir.exists(),
		"a rejected job count must not write --out"
	);
}

/// P-695: two mods writing one value differently must merge, and the merged
/// mod must carry the value in EU4's own representation.
///
/// This is the end-to-end proof that the schema binding reaches the merge in
/// the real pipeline: the canonicalization keys off the game-relative path,
/// which only the production plumbing supplies.
#[test]
fn merge_reads_one_value_written_two_ways_as_one_value() {
	let scratch: TempDir = TempDir::new().expect("test scratch");
	let root: &Path = scratch.path();
	let game: PathBuf = root.join("game");
	// `all_estate_loyalty_equilibrium` is one of the modifiers the vendored CWT
	// config actually declares (`alias[modifier:...] = float`), unlike the
	// registry-only keys the other fixtures use.
	write_fixture_mod(
		&game,
		None,
		"shared = {\n\tall_estate_loyalty_equilibrium = 0.1\n}\n",
	);
	fs::write(game.join("version.txt"), "p695-1.0").expect("write game version");
	write_fixture_mod(
		&root.join("mods/spelled_short"),
		Some("spelled_short"),
		"shared = {\n\tall_estate_loyalty_equilibrium = 0.5\n}\n",
	);
	write_fixture_mod(
		&root.join("mods/spelled_long"),
		Some("spelled_long"),
		"shared = {\n\tall_estate_loyalty_equilibrium = 0.50\n}\n",
	);
	write_game_path_config(root, &game);
	build_base_data_install(root, &game);

	let manifest: PathBuf = root.join("foch.toml");
	fs::write(
		&manifest,
		format!(
			"[project]\ngame = \"eu4\"\n\n[[project.mods]]\nid = \"spelled_short\"\npath = {}\n\n[[project.mods]]\nid = \"spelled_long\"\npath = {}\n",
			serde_json::to_string(&root.join("mods/spelled_short")).unwrap(),
			serde_json::to_string(&root.join("mods/spelled_long")).unwrap(),
		),
	)
	.expect("write manifest");

	let out: PathBuf = root.join("out");
	let (code, stdout, stderr) = run_foch(
		&[
			"merge",
			manifest.to_str().unwrap(),
			"--out",
			out.to_str().unwrap(),
			"--non-interactive",
			"--confirm",
		],
		root,
	);
	assert_eq!(code, 0, "{stdout}\n{stderr}");

	let report: foch::model::MergeReport = serde_json::from_slice(
		&fs::read(out.join(MERGE_REPORT_ARTIFACT_PATH)).expect("read report"),
	)
	.expect("decode report");
	assert_eq!(
		report.status,
		foch::model::MergeReportStatus::Ready,
		"one value written two ways is not a conflict: {report:#?}"
	);

	let merged: String = fs::read_dir(out.join("common/static_modifiers"))
		.expect("read merged directory")
		.filter_map(|entry| {
			let path = entry.expect("merged entry").path();
			(path.extension()? == "txt").then(|| fs::read_to_string(&path).expect("read merged"))
		})
		.collect();
	// `CFixedPoint` holds thousandths, so this is the value the game keeps.
	assert!(
		merged.contains("all_estate_loyalty_equilibrium = 0.500"),
		"merged output must use the engine's representation: {merged}"
	);
}

fn write_fixture_mod(root: &Path, name: Option<&str>, modifiers: &str) {
	let directory: PathBuf = root.join("common/static_modifiers");
	fs::create_dir_all(&directory).expect("create fixture directory");
	fs::write(directory.join("00_static_modifiers.txt"), modifiers).expect("write fixture script");
	if let Some(name) = name {
		fs::write(
			root.join("descriptor.mod"),
			format!("name=\"{name}\"\nversion=\"1.0.0\"\n"),
		)
		.expect("write fixture descriptor");
	}
}
