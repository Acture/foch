#![cfg(target_os = "macos")]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use foch_merge_quality::dataset::{
	DatasetPaths, FileResultRecord, MeasurementRecord, SnapshotRecord, TerminalStatus, read_jsonl,
};

fn write_mod(root: &Path, content: &str) {
	fs::create_dir_all(root.join("common/governments")).unwrap();
	fs::write(root.join("descriptor.mod"), "name=\"fixture\"\n").unwrap();
	fs::write(root.join("common/governments/example.txt"), content).unwrap();
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
	let temp = tempfile::tempdir().unwrap();
	let game = temp.path().join("game");
	let workshop = temp.path().join("workshop");
	let dataset = temp.path().join("dataset");
	let results = temp.path().join("results");
	fs::create_dir_all(&game).unwrap();
	fs::write(game.join("version.txt"), "1.37.5\n").unwrap();
	write_mod(&workshop.join("1"), "government_a = { rank = 1 }\n");
	write_mod(&workshop.join("2"), "government_b = { rank = 2 }\n");
	write_mod(
		&workshop.join("100"),
		"government_a = { rank = 1 }\ngovernment_b = { rank = 2 }\n",
	);
	let corpus = temp.path().join("corpus.json");
	fs::write(
		&corpus,
		r#"{
	"cases": [{
		"compatch_id": "100",
		"title": "Fixture compatch",
		"patched": ["1", "2"]
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
	let mut measure = common.to_vec();
	measure.extend(["measure", "--timeout-secs", "30"]);
	run(binary, &measure);
	let mut report = common.to_vec();
	report.push("report");
	run(binary, &report);

	let paths = DatasetPaths::new(&dataset);
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements).unwrap();
	assert_eq!(measurements.len(), 1);
	assert_eq!(measurements[0].status, TerminalStatus::Completed);
	assert!(measurements[0].merged_output_hash.is_some());
	let file_results = read_jsonl::<FileResultRecord>(&paths.file_results).unwrap();
	assert_eq!(file_results.len(), 1);
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
}

#[test]
fn corrupt_input_object_becomes_a_fatal_terminal_measurement() {
	let temp = tempfile::tempdir().unwrap();
	let game = temp.path().join("game");
	let workshop = temp.path().join("workshop");
	let dataset = temp.path().join("dataset");
	let results = temp.path().join("results");
	fs::create_dir_all(&game).unwrap();
	fs::write(game.join("version.txt"), "1.37.5\n").unwrap();
	write_mod(&workshop.join("1"), "a = { value = 1 }\n");
	write_mod(&workshop.join("2"), "b = { value = 2 }\n");
	write_mod(
		&workshop.join("100"),
		"a = { value = 1 }\nb = { value = 2 }\n",
	);
	let corpus = temp.path().join("corpus.json");
	fs::write(
		&corpus,
		r#"{"cases":[{"compatch_id":"100","title":"Fixture","patched":["1","2"]}]}"#,
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
}
