#![cfg(target_os = "macos")]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Mutex;

use foch_core::domain::game::Game;
use foch_engine::{BaseDataSource, FileFilter, build_base_snapshot, install_built_snapshot};
use foch_merge_quality::dataset::{
	DatasetPaths, FileResultRecord, MeasurementRecord, SnapshotRecord, TerminalStatus, read_jsonl,
};

static BASE_DATA_ENV_LOCK: Mutex<()> = Mutex::new(());

fn write_mod(root: &Path, content: &str) {
	fs::create_dir_all(root.join("common/governments")).unwrap();
	fs::write(root.join("descriptor.mod"), "name=\"fixture\"\n").unwrap();
	fs::write(root.join("common/governments/example.txt"), content).unwrap();
}

fn write_decision_mod(root: &Path, content: &str) {
	fs::create_dir_all(root.join("decisions")).unwrap();
	fs::write(root.join("descriptor.mod"), "name=\"fixture\"\n").unwrap();
	fs::write(root.join("decisions/example.txt"), content).unwrap();
}

fn install_test_base_data(game_root: &Path, data_root: &Path) {
	unsafe {
		std::env::set_var("FOCH_DATA_DIR", data_root);
	}
	let game = Game::EuropaUniversalis4;
	let filter = FileFilter::for_game(game.clone());
	let built = build_base_snapshot(&game, game_root, Some("1.37.5"), &filter).unwrap();
	install_built_snapshot(
		&built.encoded_snapshot,
		BaseDataSource::Build,
		Some(built.snapshot_asset_name),
		Some(built.snapshot_sha256),
	)
	.unwrap();
}

fn run(binary: &Path, args: &[&str]) -> Output {
	let output = Command::new(binary).args(args).output().unwrap();
	assert!(
		output.status.success(),
		"command failed: {}\nstdout:\n{}\nstderr:\n{}",
		args.join(" "),
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	output
}

#[test]
fn collect_measure_report_roundtrip() {
	let _guard = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let game = temp.path().join("game");
	let base_data = temp.path().join("base-data");
	let workshop = temp.path().join("workshop");
	let dataset = temp.path().join("dataset");
	let results = temp.path().join("results");
	fs::create_dir_all(&game).unwrap();
	fs::write(game.join("version.txt"), "1.37.5\n").unwrap();
	fs::create_dir_all(game.join("decisions")).unwrap();
	let vanilla = "country_decisions = { shared = { potential = { tag = FRA } allow = { stability = 1 } effect = { add_prestige = 1 } } }\n";
	fs::write(game.join("decisions/example.txt"), vanilla).unwrap();
	install_test_base_data(&game, &base_data);
	write_decision_mod(
		&workshop.join("1"),
		"country_decisions = { shared = { potential = { tag = ENG } allow = { stability = 1 } effect = { add_prestige = 1 } } }\n",
	);
	write_decision_mod(
		&workshop.join("2"),
		"country_decisions = { shared = { potential = { tag = FRA } allow = { stability = 1 } effect = { add_prestige = 2 } } }\n",
	);
	write_decision_mod(
		&workshop.join("100"),
		"country_decisions = { shared = { potential = { tag = ENG } allow = { stability = 1 } effect = { add_prestige = 2 } } }\n",
	);
	let corpus = temp.path().join("corpus.json");
	fs::write(
		&corpus,
		r#"{
	"schema": "1.0.0",
	"cases": [{
		"compatch_id": "100",
		"title": "Fixture compatch",
		"referenced_mods": ["1", "2"]
	}]
}"#,
	)
	.unwrap();
	let binary = Path::new(env!("CARGO_BIN_EXE_foch-mq"));
	let common = [
		"--corpus",
		corpus.to_str().unwrap(),
		"--dataset-root",
		dataset.to_str().unwrap(),
		"--game-root",
		game.to_str().unwrap(),
		"--workshop-dir",
		workshop.to_str().unwrap(),
		"--results-dir",
		results.to_str().unwrap(),
	];
	let mut collect = common.to_vec();
	collect.push("collect");
	run(binary, &collect);
	let paths = DatasetPaths::new(&dataset);
	let snapshot_id = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.unwrap()
		.pop()
		.unwrap()
		.snapshot_id;
	let wrong_output = temp.path().join("wrong-identity-output");
	let mut wrong_identity = common.to_vec();
	wrong_identity.extend([
		"measure-one",
		"--snapshot-id",
		&snapshot_id,
		"--output-dir",
		wrong_output.to_str().unwrap(),
		"--basegame-root",
		game.to_str().unwrap(),
		"--base-snapshot-identity",
		"sha256:not-the-parent-snapshot",
	]);
	let wrong_identity = run(binary, &wrong_identity);
	let worker: serde_json::Value = serde_json::from_slice(&wrong_identity.stdout).unwrap();
	assert_eq!(worker["status"], "merge_failed");
	assert!(
		worker["detail"]
			.as_str()
			.unwrap()
			.contains("installed base snapshot identity mismatch")
	);
	let mut measure = common.to_vec();
	measure.extend(["measure", "--timeout-secs", "30"]);
	run(binary, &measure);
	let mut report = common.to_vec();
	report.push("report");
	run(binary, &report);

	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements).unwrap();
	assert_eq!(measurements.len(), 1);
	assert_eq!(measurements[0].status, TerminalStatus::Completed);
	assert!(measurements[0].merged_output_hash.is_some());
	let file_results = read_jsonl::<FileResultRecord>(&paths.file_results).unwrap();
	assert_eq!(file_results.len(), 1);
	assert!(
		file_results[0].result["score"]["accepted_ok"]
			.as_bool()
			.unwrap(),
		"unexpected file result: {}",
		file_results[0].result
	);
	assert!(!file_results[0].result["human_resolution"].is_null());
	assert_eq!(
		file_results[0].result["human_resolution"]["contributors"]
			.as_array()
			.unwrap()
			.len(),
		2
	);
	let baseline: serde_json::Value =
		serde_json::from_str(&fs::read_to_string(results.join("baseline.json")).unwrap()).unwrap();
	assert_eq!(baseline["baseline_complete"], true);
	assert_eq!(baseline["terminal_cases"], 1);
	assert_eq!(baseline["completed_cases"], 1);
	unsafe {
		std::env::remove_var("FOCH_DATA_DIR");
	}
}

#[test]
fn corrupt_input_object_becomes_a_fatal_terminal_measurement() {
	let _guard = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let game = temp.path().join("game");
	let base_data = temp.path().join("base-data");
	let workshop = temp.path().join("workshop");
	let dataset = temp.path().join("dataset");
	let results = temp.path().join("results");
	fs::create_dir_all(&game).unwrap();
	fs::write(game.join("version.txt"), "1.37.5\n").unwrap();
	install_test_base_data(&game, &base_data);
	write_mod(&workshop.join("1"), "a = { value = 1 }\n");
	write_mod(&workshop.join("2"), "b = { value = 2 }\n");
	write_mod(
		&workshop.join("100"),
		"a = { value = 1 }\nb = { value = 2 }\n",
	);
	let corpus = temp.path().join("corpus.json");
	fs::write(
		&corpus,
		r#"{"schema":"1.0.0","cases":[{"compatch_id":"100","title":"Fixture Compatch","referenced_mods":["1","2"]}]}"#,
	)
	.unwrap();
	let binary = Path::new(env!("CARGO_BIN_EXE_foch-mq"));
	let common = [
		"--corpus",
		corpus.to_str().unwrap(),
		"--dataset-root",
		dataset.to_str().unwrap(),
		"--game-root",
		game.to_str().unwrap(),
		"--workshop-dir",
		workshop.to_str().unwrap(),
		"--results-dir",
		results.to_str().unwrap(),
	];
	let mut collect = common.to_vec();
	collect.push("collect");
	run(binary, &collect);

	let paths = DatasetPaths::new(&dataset);
	let snapshot = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.unwrap()
		.pop()
		.unwrap();
	let hash = &snapshot.source_mods[0].content_hash;
	fs::write(
		paths
			.objects
			.join(&hash[..2])
			.join(hash)
			.join("tree/common/governments/example.txt"),
		"corrupt\n",
	)
	.unwrap();

	let mut measure = common.to_vec();
	measure.extend(["measure", "--timeout-secs", "30"]);
	run(binary, &measure);
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements).unwrap();
	assert_eq!(measurements.len(), 1);
	assert_eq!(measurements[0].status, TerminalStatus::Fatal);
	assert!(
		measurements[0]
			.detail
			.as_deref()
			.unwrap()
			.contains("failed verification")
	);

	let mut report = common.to_vec();
	report.push("report");
	run(binary, &report);
	let baseline: serde_json::Value =
		serde_json::from_str(&fs::read_to_string(results.join("baseline.json")).unwrap()).unwrap();
	assert_eq!(baseline["baseline_complete"], true);
	assert_eq!(baseline["terminal_cases"], 1);
	assert_eq!(baseline["merge_failed_cases"], 1);
	unsafe {
		std::env::remove_var("FOCH_DATA_DIR");
	}
}
