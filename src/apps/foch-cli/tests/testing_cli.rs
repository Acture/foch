use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

const CASES: &str = r#"namespace = fixture
#test(time=1444.11.11, tag=SWE, name="smoke")
country_event = { id = fixture.1 hidden = yes is_triggered_only = yes immediate = {} }
#test(time=1444.11.11, tag=SWE, name="flag check", expect={ has_country_flag = fixture_flag })
country_event = { id = fixture.2 hidden = yes is_triggered_only = yes immediate = { set_country_flag = fixture_flag } }
"#;

fn fixture() -> TempDir {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/events/nested")).unwrap();
	fs::write(dir.path().join("mod/events/nested/cases.txt"), CASES).unwrap();
	dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
	Command::new(env!("CARGO_BIN_EXE_foch"))
		.current_dir(dir)
		.env("FOCH_DATA_DIR", dir.join("global-data"))
		.args(args)
		.output()
		.unwrap()
}

fn json(output: &Output) -> Value {
	serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
		panic!(
			"{err}: stdout={} stderr={}",
			String::from_utf8_lossy(&output.stdout),
			String::from_utf8_lossy(&output.stderr)
		)
	})
}

#[test]
fn collect_distinguishes_smoke_and_assertion_without_global_initialization() {
	let dir = fixture();
	let output = run(
		dir.path(),
		&["test", "mod", "--collect-only", "--format", "json"],
	);
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	let report = json(&output);
	assert_eq!(report["cases"].as_array().unwrap().len(), 2);
	assert_eq!(report["cases"][0]["kind"], "smoke");
	assert_eq!(report["cases"][1]["kind"], "assertion");
	assert_eq!(
		report["cases"][0]["origin"]["path"],
		"events/nested/cases.txt"
	);
	assert!(!dir.path().join("global-data").exists());
	assert_eq!(
		fs::read_to_string(dir.path().join("mod/events/nested/cases.txt")).unwrap(),
		CASES
	);
}

#[test]
fn static_layer_flags_an_effect_used_as_a_condition() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/events")).unwrap();
	// set_country_flag is an effect; using it inside expect is a mistake the
	// static layer now catches offline, from the embedded builtin catalog.
	let cases = "namespace = fixture\n#test(time=1444.11.11, tag=SWE, name=\"bad\", expect={ set_country_flag = oops })\ncountry_event = { id = fixture.1 hidden = yes is_triggered_only = yes immediate = {} }\n";
	fs::write(dir.path().join("mod/events/cases.txt"), cases).unwrap();
	let output = run(
		dir.path(),
		&["test", "mod", "--collect-only", "--format", "json"],
	);
	let report = json(&output);
	let diagnostics = report["diagnostics"].as_array().unwrap();
	assert!(
		diagnostics.iter().any(|diagnostic| diagnostic["message"]
			.as_str()
			.unwrap_or_default()
			.contains("is an effect, but this block is a condition")),
		"{report}"
	);
	// The check reads the embedded catalog, so it builds no base snapshot.
	assert!(!dir.path().join("global-data").exists());
}

#[test]
fn discover_only_event_text_files_and_allow_explicit_files() {
	let dir = fixture();
	fs::create_dir_all(dir.path().join("mod/common")).unwrap();
	fs::write(dir.path().join("mod/common/ignored.txt"), "#test(invalid)").unwrap();
	fs::write(dir.path().join("mod/events/ignored.log"), "#test(invalid)").unwrap();
	let root = run(
		dir.path(),
		&["test", "mod", "--collect-only", "--format", "json"],
	);
	assert!(root.status.success());
	assert_eq!(json(&root)["cases"].as_array().unwrap().len(), 2);
	let explicit = run(
		dir.path(),
		&[
			"test",
			"mod/events/nested/cases.txt",
			"--collect-only",
			"--format",
			"json",
		],
	);
	assert!(explicit.status.success());
	assert_eq!(json(&root)["cases"], json(&explicit)["cases"]);
}

#[test]
fn default_path_and_filter_are_supported_and_empty_selection_fails() {
	let dir = fixture();
	let output = run(
		&dir.path().join("mod"),
		&[
			"test",
			"--collect-only",
			"-k",
			"flag and check",
			"--format",
			"json",
		],
	);
	assert!(output.status.success());
	assert_eq!(json(&output)["cases"].as_array().unwrap().len(), 1);
	let empty = run(
		dir.path(),
		&[
			"test",
			"mod",
			"--collect-only",
			"-k",
			"absent",
			"--format",
			"json",
		],
	);
	assert!(!empty.status.success());
	assert!(json(&empty)["cases"].as_array().unwrap().is_empty());
}

#[test]
fn invalid_annotation_produces_source_diagnostic_and_failure() {
	let dir = fixture();
	fs::write(dir.path().join("mod/events/bad.txt"), "#test(time=invalid, tag=SWE)\ncountry_event = { id = fixture.3 hidden=yes is_triggered_only=yes }\n").unwrap();
	let output = run(
		dir.path(),
		&["test", "mod", "--collect-only", "--format", "json"],
	);
	assert!(!output.status.success());
	let report = json(&output);
	assert!(!report["diagnostics"].as_array().unwrap().is_empty());
	assert_eq!(report["diagnostics"][0]["span"]["path"], "events/bad.txt");
}

#[test]
fn text_collection_and_diagnostics_report_annotation_line_and_column() {
	let dir = fixture();
	let collected = run(dir.path(), &["test", "mod", "--collect-only"]);
	assert!(collected.status.success());
	assert!(String::from_utf8_lossy(&collected.stdout).contains("events/nested/cases.txt:2:1"));
	fs::write(dir.path().join("mod/events/bad.txt"), "#test(time=invalid, tag=SWE)\ncountry_event = { id = fixture.3 hidden=yes is_triggered_only=yes }\n").unwrap();
	let invalid = run(dir.path(), &["test", "mod", "--collect-only"]);
	assert!(!invalid.status.success());
	assert!(String::from_utf8_lossy(&invalid.stderr).contains("events/bad.txt:1:"));
}

#[test]
fn test_help_explains_annotation_placement_and_smoke_scope() {
	let dir = tempfile::tempdir().unwrap();
	let output = run(dir.path(), &["test", "--help"]);
	assert!(output.status.success());
	let help = String::from_utf8_lossy(&output.stdout)
		.split_whitespace()
		.collect::<Vec<_>>()
		.join(" ");
	for required in [
		"next top-level country_event",
		"hidden=yes",
		"is_triggered_only=yes",
		"Without expect",
		"smoke",
		"invocation completion only",
	] {
		assert!(
			help.contains(required),
			"help must describe {required}: {help}"
		);
	}
}

#[test]
fn api_exposes_the_same_annotation_contract_as_help() {
	let dir = tempfile::tempdir().unwrap();
	let output = run(dir.path(), &["test", "--api", "--format", "json"]);
	assert!(output.status.success());
	assert_eq!(
		json(&output)["annotations"][0]["description"],
		foch_annotation::builtin::TEST_DESCRIPTION
	);
	let text = run(dir.path(), &["test", "--api"]);
	assert!(text.status.success());
	assert!(
		String::from_utf8_lossy(&text.stdout).contains(foch_annotation::builtin::TEST_DESCRIPTION)
	);
}

#[test]
fn api_metadata_is_available_offline_and_unknown_names_fail() {
	let dir = tempfile::tempdir().unwrap();
	let output = run(dir.path(), &["test", "--api", "--format", "json"]);
	assert!(output.status.success());
	let all = json(&output);
	let names: Vec<_> = all["annotations"]
		.as_array()
		.unwrap()
		.iter()
		.map(|annotation| annotation["name"].as_str().unwrap())
		.collect();
	assert_eq!(
		names,
		["test", "skip", "ignore", "xfail", "mark", "parametrize"]
	);
	assert!(
		all["annotations"][0]["params"]
			.as_array()
			.unwrap()
			.iter()
			.any(|p| p["name"] == "expect")
	);
	let output = run(dir.path(), &["test", "--api", "expect", "--format", "json"]);
	assert!(output.status.success());
	assert_eq!(json(&output)["annotations"].as_array().unwrap().len(), 1);
	assert!(
		!run(dir.path(), &["test", "--api", "missing"])
			.status
			.success()
	);
	assert!(!dir.path().join("global-data").exists());
}

#[test]
fn no_run_writes_separate_session_bundles_without_launching() {
	let dir = fixture();
	assert!(
		!run(
			dir.path(),
			&["compile", "--tests", "mod", "--out", "compiled"]
		)
		.status
		.success(),
		"there is no separate compile command for tests"
	);
	let output = run(
		dir.path(),
		&[
			"test", "mod", "--no-run", "--out", "compiled", "--format", "json",
		],
	);
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	let report = json(&output);
	let bundles = report["bundles"].as_array().unwrap();
	assert_eq!(bundles.len(), 2);
	for path in bundles {
		let path = Path::new(path.as_str().unwrap());
		assert!(path.is_absolute());
		let bundle: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
		assert!(Path::new(bundle["source_root"].as_str().unwrap()).is_absolute());
		for (relative, content) in bundle["files"].as_object().unwrap() {
			assert_eq!(
				fs::read_to_string(path.parent().unwrap().join(relative)).unwrap(),
				content.as_str().unwrap()
			);
		}
	}
	assert!(
		!dir.path().join("compiled/result.json").exists(),
		"nothing ran"
	);
	assert_eq!(
		fs::read_to_string(dir.path().join("mod/events/nested/cases.txt")).unwrap(),
		CASES
	);
	assert!(!dir.path().join("global-data").exists());
}

#[test]
fn no_run_refuses_nonempty_output_and_any_source_subdirectory() {
	let dir = fixture();
	fs::create_dir(dir.path().join("existing")).unwrap();
	fs::write(dir.path().join("existing/sentinel"), "retain").unwrap();
	for output in ["existing", "mod", "mod/new-output"] {
		let result = run(dir.path(), &["test", "mod", "--no-run", "--out", output]);
		assert!(!result.status.success(), "must reject {output}");
	}
	assert_eq!(
		fs::read_to_string(dir.path().join("existing/sentinel")).unwrap(),
		"retain"
	);
	assert!(!dir.path().join("mod/new-output").exists());
}

#[test]
fn case_outside_a_supported_start_date_is_refused() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/events")).unwrap();
	fs::write(
		dir.path().join("mod/events/future.txt"),
		"namespace = future
#test(time=1500.1.1, tag=SWE, name=smoke)
country_event = { id = future.1 hidden = yes is_triggered_only = yes }
",
	)
	.unwrap();
	let output = run(dir.path(), &["test", "mod"]);
	assert!(!output.status.success());
	assert!(
		String::from_utf8_lossy(&output.stderr).contains("start date"),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
}

#[cfg(unix)]
#[test]
fn input_symlinks_are_never_followed() {
	use std::os::unix::fs::symlink;
	let dir = fixture();
	fs::write(dir.path().join("external.txt"), "#test(invalid)").unwrap();
	symlink(
		dir.path().join("external.txt"),
		dir.path().join("mod/events/linked.txt"),
	)
	.unwrap();
	assert!(
		run(dir.path(), &["test", "mod", "--collect-only"])
			.status
			.success()
	);
	assert!(
		!run(
			dir.path(),
			&["test", "mod/events/linked.txt", "--collect-only"]
		)
		.status
		.success()
	);
	symlink(dir.path().join("mod"), dir.path().join("linked-mod")).unwrap();
	assert!(
		!run(
			dir.path(),
			&[
				"test",
				"linked-mod/events/nested/cases.txt",
				"--collect-only"
			]
		)
		.status
		.success()
	);
}
