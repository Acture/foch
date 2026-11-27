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
fn collects_test_blocks_and_helpers_under_the_tests_directory() {
	let dir = fixture();
	fs::create_dir_all(dir.path().join("mod/tests/events")).unwrap();
	// The game does not load tests/, so this file holds a test block.
	fs::write(
		dir.path().join("mod/tests/war.txt"),
		"test = {\n name = declare\n time = 1444.11.11\n tag = SWE\n fire = helper.1\n}\n",
	)
	.unwrap();
	// A helper event that only the generated test layer uses.
	fs::write(
		dir.path().join("mod/tests/events/helper.txt"),
		"namespace = helper\ncountry_event = { id = helper.1 hidden = yes is_triggered_only = yes immediate = {} }\n",
	)
	.unwrap();
	let output = run(
		dir.path(),
		&["test", "mod", "--collect-only", "--format", "json"],
	);
	let report = json(&output);
	let cases = report["cases"].as_array().unwrap();
	// The two inline cases plus the tests/ block, with no fire-target errors.
	assert_eq!(cases.len(), 3, "{report}");
	assert!(
		cases.iter().any(|case| case["node_id"]
			.as_str()
			.unwrap_or_default()
			.contains("tests/war.txt::declare")),
		"{report}"
	);
	assert!(
		report["diagnostics"].as_array().unwrap().is_empty(),
		"{report}"
	);
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

#[test]
fn offline_interprets_without_a_game_installation() {
	let dir = fixture();
	let out = dir.path().join("offline-results");
	let output = run(
		dir.path(),
		&[
			"test",
			"mod",
			"--offline",
			"--format",
			"json",
			"--out",
			out.to_str().unwrap(),
		],
	);
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	let report = json(&output);
	assert_eq!(report["layer"], "offline");
	assert_eq!(report["evidence"], "unpinned");
	let outcomes: Vec<_> = report["results"]
		.as_array()
		.unwrap()
		.iter()
		.map(|result| result["outcome"].as_str().unwrap())
		.collect();
	assert_eq!(outcomes, ["smoke", "pass"]);
	let results =
		serde_json::from_str::<Value>(&fs::read_to_string(out.join("result.json")).unwrap())
			.unwrap();
	assert_eq!(results["layer"], "offline");
	let junit = fs::read_to_string(out.join("junit.xml")).unwrap();
	assert!(
		junit.contains("<testsuite name=\"foch-offline\""),
		"{junit}"
	);
	// The offline layer never initializes Foch's global data.
	assert!(!dir.path().join("global-data").exists());
}

#[test]
fn offline_failures_report_modeled_observations() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/events")).unwrap();
	fs::write(
		dir.path().join("mod/events/cases.txt"),
		"namespace = offline\n#test(time=1444.11.11, tag=SWE, name=\"contradiction\", expect={ always = no })\ncountry_event = { id = offline.1 hidden = yes is_triggered_only = yes immediate = {} }\n",
	)
	.unwrap();
	let output = run(
		dir.path(),
		&[
			"test",
			"mod",
			"--offline",
			"--out",
			dir.path().join("results").to_str().unwrap(),
		],
	);
	assert!(
		!output.status.success(),
		"a failed model expectation is a failing run"
	);
	let text = String::from_utf8_lossy(&output.stdout);
	assert!(text.contains("offline layer"), "{text}");
	assert!(text.contains("FAIL"), "{text}");
	assert!(text.contains("always = no"), "{text}");
}

#[test]
fn offline_is_not_bound_to_the_runners_start_dates() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/events")).unwrap();
	// A start date the built-in runner refuses online; the model has no
	// launch and no start-date capability to check.
	fs::write(
		dir.path().join("mod/events/future.txt"),
		"namespace = future\n#test(time=1500.1.1, tag=SWE, name=smoke)\ncountry_event = { id = future.1 hidden = yes is_triggered_only = yes }\n",
	)
	.unwrap();
	let output = run(dir.path(), &["test", "mod", "--offline"]);
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
}

#[test]
fn inline_test_annotations_accept_expect_receipt() {
	// The peace line's repro: an inline #test block with expect_receipt was
	// rejected at collection ("unknown #test parameter expect_receipt"),
	// blocking every annotated receipt fixture before the game. The schema
	// declares it and the annotated case collects and compiles, numeric
	// range included.
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/events")).unwrap();
	fs::write(
		dir.path().join("mod/events/cases.txt"),
		r#"namespace = cases
#test(time=1444.11.11, tag=SWE, name=rosetta, expect={ has_country_flag = ready })
country_event = { id = cases.1 hidden = yes is_triggered_only = yes immediate = { set_country_flag = ready } option = { name = OK } }
#test(time=1444.11.11, tag=SWE, name=paid_up, expect_receipt={ plugin = "dev.foch.x" case = paid_up request = r1 status = paid numeric = { gold = { min = 100 max = 500 } } })
country_event = { id = cases.2 hidden = yes is_triggered_only = yes immediate = { set_country_flag = staged } option = { name = OK } }"#,
	)
	.unwrap();
	let output = run(
		dir.path(),
		&["test", "mod", "--collect-only", "--format", "json"],
	);
	assert!(
		output.status.success(),
		"{}{}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	let report = json(&output);
	let nodes: Vec<_> = report["cases"]
		.as_array()
		.unwrap()
		.iter()
		.map(|case| case["node_id"].as_str().unwrap_or_default())
		.collect();
	assert!(
		nodes.iter().any(|node| node.contains("rosetta")),
		"{nodes:?}"
	);
	assert!(
		nodes.iter().any(|node| node.contains("paid_up")),
		"{nodes:?}"
	);
	// The receipt step rides the plan: bundles compile without a game.
	let out = dir.path().join("annotated-out");
	let compiled = run(
		dir.path(),
		&[
			"test",
			"mod",
			"--no-run",
			"--format",
			"json",
			"--out",
			out.to_str().unwrap(),
		],
	);
	assert!(
		compiled.status.success(),
		"{}{}",
		String::from_utf8_lossy(&compiled.stdout),
		String::from_utf8_lossy(&compiled.stderr)
	);
}

#[test]
fn console_steps_ride_the_command_channel_in_order() {
	// task_a7a3061a7932: cheat / save / reload steps are console steps; the
	// bundle emits them, in step order, on the verified startup channel
	// before the test script runs.
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/tests")).unwrap();
	fs::write(
		dir.path().join("mod/tests/a.txt"),
		r#"test = {
	name = cycle
	time = 1444.11.11
	tag = SWE
	cheat = "cash 500"
	save = "slot_a"
	effect = { set_country_flag = staged }
	reload = "slot_a"
	advance_days = 1
	expect = { has_country_flag = staged }
}"#,
	)
	.unwrap();
	let out = dir.path().join("cycle-out");
	let output = run(
		dir.path(),
		&[
			"test",
			"mod",
			"--no-run",
			"--format",
			"json",
			"--out",
			out.to_str().unwrap(),
		],
	);
	assert!(
		output.status.success(),
		"{}{}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	let report = json(&output);
	let bundles = report["bundles"].as_array().unwrap();
	assert_eq!(bundles.len(), 1);
	let bundle: Value =
		serde_json::from_slice(&fs::read(bundles[0].as_str().unwrap()).unwrap()).unwrap();
	let commands = bundle["files"]["commands.txt"].as_str().unwrap();
	assert!(
		commands.starts_with("speed 0\ncash 500\nsave_game slot_a\nload slot_a\nrun "),
		"console steps keep their step order on the startup channel: {commands:?}"
	);
	assert!(commands.ends_with("_start.txt\nspeed 5\n"), "{commands:?}");
}

#[test]
fn a_save_slot_must_be_one_console_token() {
	// The slot becomes one console line, so path separators, quotes and
	// whitespace are refused at collection instead of riding the console.
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/tests")).unwrap();
	fs::write(
		dir.path().join("mod/tests/a.txt"),
		r#"test = { name = inject time = 1444.11.11 tag = SWE save = "../evil; save_game x" expect = { always = yes } }"#,
	)
	.unwrap();
	let output = run(dir.path(), &["test", "mod", "--no-run"]);
	assert!(!output.status.success());
	let text = format!(
		"{}{}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	assert!(text.contains("letters, digits"), "{text}");
	assert!(!text.contains("panicked"), "{text}");
}

#[test]
fn offline_conflicts_with_plugins_as_a_flag_error() {
	// --offline silently ignored --with-plugins and interpreted the run; the
	// combination must be a clap conflict before any playset or host is
	// resolved, in either flag order.
	let dir = fixture();
	for args in [
		vec![
			"test",
			"mod",
			"--offline",
			"--with-plugins",
			"nonexistent_playset",
		],
		vec![
			"test",
			"mod",
			"--with-plugins",
			"nonexistent_playset",
			"--offline",
		],
	] {
		let output = run(dir.path(), &args);
		assert!(
			!output.status.success(),
			"{args:?} must not run: {}",
			String::from_utf8_lossy(&output.stdout)
		);
		let text = format!(
			"{}{}",
			String::from_utf8_lossy(&output.stdout),
			String::from_utf8_lossy(&output.stderr)
		);
		assert!(
			text.contains("--offline") && text.contains("--with-plugins"),
			"the conflict names both flags: {text}"
		);
	}
}

#[test]
fn unmapped_step_kinds_are_rejected_cleanly_at_collection() {
	// The review repro: cheat / take_decision in a tests/ block must never
	// panic the compile path. This branch collects both as steps, so the
	// collection-level refusal lives on the merged core; here the invariant
	// is the one that survived the review: the online runner stops with a
	// clean CompileError — take_decision names --offline — and nothing
	// panics.
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/tests")).unwrap();
	fs::write(
		dir.path().join("mod/tests/a.txt"),
		r#"test = { name = cheaty time = 1444.11.11 tag = SWE cheat = "cash 500" expect = { always = yes } }"#,
	)
	.unwrap();
	fs::write(
		dir.path().join("mod/tests/b.txt"),
		r#"test = { name = decided time = 1444.11.11 tag = SWE take_decision = my_decision expect = { always = yes } }"#,
	)
	.unwrap();
	let output = run(dir.path(), &["test", "mod", "--no-run"]);
	assert!(
		!output.status.success(),
		"the decision step stops the online run: {}",
		String::from_utf8_lossy(&output.stdout)
	);
	let text = format!(
		"{}{}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	assert!(
		text.contains("cannot take decisions yet") && text.contains("--offline"),
		"{text}"
	);
	assert!(!text.contains("panicked"), "{text}");
}

#[test]
fn offline_refuses_plugin_receipt_steps() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/tests")).unwrap();
	fs::write(
		dir.path().join("mod/tests/a.txt"),
		r#"test = { name = rosy time = 1444.11.11 tag = SWE effect = { set_country_flag = done } expect_receipt = { plugin = dev.foch.x case = rosy request = 1 status = passed } }"#,
	)
	.unwrap();
	let output = run(
		dir.path(),
		&["test", "mod", "--offline", "--format", "json"],
	);
	assert!(
		!output.status.success(),
		"an all-unsupported offline run exits nonzero: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	let text = format!(
		"{}{}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	assert!(text.contains("receipts need a real game session"), "{text}");
	assert!(!text.contains("panicked"), "{text}");
}

#[test]
fn offline_refuses_runner_only_flags() {
	let dir = fixture();
	let output = run(dir.path(), &["test", "mod", "--offline", "--no-run"]);
	assert!(!output.status.success());
	let output = run(
		dir.path(),
		&["test", "mod", "--offline", "--with-plugins", "example"],
	);
	assert!(!output.status.success());
}

#[test]
fn offline_takes_a_nested_country_decision() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/decisions")).unwrap();
	fs::create_dir_all(dir.path().join("mod/tests")).unwrap();
	fs::write(
		dir.path().join("mod/decisions/00_decisions.txt"),
		"country_decisions = {\n\tmy_decision = {\n\t\tpotential = { tag = SWE }\n\t\tallow = { always = yes }\n\t\teffect = { set_country_flag = decided }\n\t}\n}\n",
	)
	.unwrap();
	fs::write(
		dir.path().join("mod/tests/case.txt"),
		"test = { name = decided time = 1444.11.11 tag = SWE take_decision = my_decision expect = { has_country_flag = decided } }\n",
	)
	.unwrap();
	let output = run(
		dir.path(),
		&["test", "mod", "--offline", "--format", "json"],
	);
	assert!(
		output.status.success(),
		"stdout={} stderr={}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(json(&output)["results"][0]["outcome"], "pass");
}

#[test]
fn offline_folds_mod_history_at_the_case_date() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/history/countries")).unwrap();
	fs::create_dir_all(dir.path().join("mod/tests")).unwrap();
	fs::write(
		dir.path().join("mod/history/countries/SWE - Sweden.txt"),
		"religion = catholic\n1500.1.1 = { religion = protestant }\n",
	)
	.unwrap();
	fs::write(
		dir.path().join("mod/history/countries/Q01 - Custom.txt"),
		"religion = norse\n",
	)
	.unwrap();
	fs::write(
		dir.path().join("mod/tests/case.txt"),
		"test = { name = early time = 1444.11.11 tag = SWE expect = { religion = catholic } }\n\
		 test = { name = later time = 1500.1.1 tag = SWE expect = { religion = protestant } }\n\
		 test = { name = custom time = 1444.11.11 tag = Q01 expect = { religion = norse } }\n",
	)
	.unwrap();
	let output = run(
		dir.path(),
		&["test", "mod", "--offline", "--format", "json"],
	);
	assert!(
		output.status.success(),
		"stdout={} stderr={}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	let report = json(&output);
	let outcomes: Vec<_> = report["results"]
		.as_array()
		.unwrap()
		.iter()
		.map(|result| result["outcome"].as_str().unwrap())
		.collect();
	assert_eq!(outcomes.len(), 3, "{outcomes:?}");
	assert!(
		outcomes.iter().all(|outcome| *outcome == "pass"),
		"{outcomes:?}"
	);
}

#[test]
fn offline_resolves_a_mod_subject_type() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/common/subject_types")).unwrap();
	fs::create_dir_all(dir.path().join("mod/tests")).unwrap();
	fs::write(
		dir.path()
			.join("mod/common/subject_types/00_subject_types.txt"),
		"default = {}\nee_tributary = { copy_from = default }\n",
	)
	.unwrap();
	fs::write(
		dir.path().join("mod/tests/case.txt"),
		"test = {\n\tname = subject\n\ttime = 1444.11.11\n\ttag = SWE\n\teffect = { create_subject = { subject_type = ee_tributary subject = NOR } }\n\texpect = { NOR = { is_subject_of = SWE is_subject_of_type = ee_tributary } }\n}\n",
	)
	.unwrap();
	let output = run(
		dir.path(),
		&["test", "mod", "--offline", "--format", "json"],
	);
	assert!(
		output.status.success(),
		"stdout={} stderr={}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(json(&output)["results"][0]["outcome"], "pass");
}

#[test]
fn offline_province_scopes_follow_mod_history() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	fs::create_dir_all(dir.path().join("mod/history/countries")).unwrap();
	fs::create_dir_all(dir.path().join("mod/history/provinces")).unwrap();
	fs::create_dir_all(dir.path().join("mod/events")).unwrap();
	fs::create_dir_all(dir.path().join("mod/tests")).unwrap();
	fs::write(
		dir.path().join("mod/history/countries/SWE - Sweden.txt"),
		"capital = 1\n",
	)
	.unwrap();
	fs::write(
		dir.path().join("mod/history/provinces/1-Stockholm.txt"),
		"owner = SWE\ncontroller = SWE\nbase_tax = 5\nbase_production = 4\nbase_manpower = 3\nadd_core = SWE\n",
	)
	.unwrap();
	fs::write(
		dir.path().join("mod/events/provinces.txt"),
		"namespace = cases\nprovince_event = {\n\tid = cases.2\n\thidden = yes\n\tis_triggered_only = yes\n\timmediate = { set_province_flag = from_event }\n}\n",
	)
	.unwrap();
	fs::write(
		dir.path().join("mod/tests/case.txt"),
		"test = {\n\
		 \tname = stockholm\n\
		 \ttime = 1444.11.11\n\
		 \ttag = SWE\n\
		 \teffect = {\n\
		 \t\tcapital_scope = { set_province_flag = capital_seen }\n\
		 \t\t1 = { owner = { PREV = { set_province_flag = round_trip } } province_event = { id = cases.2 } }\n\
		 \t\tevery_owned_province = { limit = { owned_by = SWE } set_province_flag = visited }\n\
		 \t}\n\
		 \texpect = {\n\
		 \t\towns = 1\n\
		 \t\towns_core_province = 1\n\
		 \t\t1 = { development = 12 has_province_flag = capital_seen has_province_flag = round_trip has_province_flag = from_event has_province_flag = visited }\n\
		 \t}\n\
		 }\n\
		 test = {\n\
		 \tname = refuses\n\
		 \ttime = 1444.11.11\n\
		 \ttag = SWE\n\
		 \texpect = { base_tax = 1 }\n\
		 }\n",
	)
	.unwrap();
	let output = run(
		dir.path(),
		&["test", "mod", "--offline", "--format", "json"],
	);
	let report = json(&output);
	let results = report["results"].as_array().unwrap();
	assert_eq!(results.len(), 2, "{report}");
	assert_eq!(results[0]["outcome"], "pass", "{report}");
	assert_eq!(results[1]["outcome"], "unsupported", "{report}");
	let reasons = results[1]["unsupported"].to_string();
	assert!(
		reasons.contains("base_tax is a province trigger"),
		"{reasons}"
	);
}

#[test]
fn companion_plugins_do_not_interfere_with_offline_tests() {
	// A mod may declare `[[plugins]]` in its own foch.toml next to descriptor.mod
	// and annotated events. Collection, the offline interpreter, and bundle
	// compilation read the script and the descriptor name; the plugin table is
	// not a test input and must not change those results.
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	let root = dir.path().join("mod");
	fs::create_dir_all(root.join("events")).unwrap();
	fs::write(
		root.join("descriptor.mod"),
		"name=\"Companion Mod\"\nsupported_version=\"1.37.*\"\n",
	)
	.unwrap();
	let manifest = "[[plugins]]\nid = \"dev.foch.eu4-dynamic-trade-blackhole\"\nrepo = \"Acture/eu4-dynamic-trade-blackhole\"\ngame_versions = \"=1.37.5\"\nrequired = true\nmin_version = \"0.1.0\"\n[[plugins]]\nid = \"dev.foch.sample\"\nrepo = \"Acture/sample\"\nrequired = false\n";
	fs::write(root.join("foch.toml"), manifest).unwrap();
	fs::write(
		root.join("events/companion.txt"),
		"namespace = companion\n#test(time=1444.11.11, tag=SWE, name=smoke)\ncountry_event = { id = companion.1 hidden = yes is_triggered_only = yes immediate = { set_country_flag = companion_ready } }\n#test(time=1444.11.11, tag=SWE, name=flag, expect={ has_country_flag = companion_ready })\ncountry_event = { id = companion.2 hidden = yes is_triggered_only = yes immediate = { set_country_flag = companion_ready } }\n",
	)
	.unwrap();

	let collected = run(
		dir.path(),
		&["test", "mod", "--collect-only", "--format", "json"],
	);
	assert!(
		collected.status.success(),
		"{}{}",
		String::from_utf8_lossy(&collected.stdout),
		String::from_utf8_lossy(&collected.stderr)
	);
	let report = json(&collected);
	assert!(
		report["diagnostics"].as_array().unwrap().is_empty(),
		"{report}"
	);
	let cases = report["cases"].as_array().unwrap();
	assert_eq!(cases.len(), 2, "{report}");
	assert_eq!(cases[0]["kind"], "smoke");
	assert_eq!(cases[1]["kind"], "assertion");
	assert!(
		cases.iter().all(|case| case["node_id"]
			.as_str()
			.unwrap()
			.starts_with("Companion Mod::events/companion.txt::")),
		"{report}"
	);

	let offline = run(
		dir.path(),
		&["test", "mod", "--offline", "--format", "json"],
	);
	assert!(
		offline.status.success(),
		"{}{}",
		String::from_utf8_lossy(&offline.stdout),
		String::from_utf8_lossy(&offline.stderr)
	);
	let offline_report = json(&offline);
	let outcomes: Vec<_> = offline_report["results"]
		.as_array()
		.unwrap()
		.iter()
		.map(|result| result["outcome"].as_str().unwrap())
		.collect();
	assert_eq!(outcomes, ["smoke", "pass"]);

	let out = dir.path().join("compiled");
	let compiled = run(
		dir.path(),
		&[
			"test",
			"mod",
			"--no-run",
			"--format",
			"json",
			"--out",
			out.to_str().unwrap(),
		],
	);
	assert!(
		compiled.status.success(),
		"{}{}",
		String::from_utf8_lossy(&compiled.stdout),
		String::from_utf8_lossy(&compiled.stderr)
	);
	let compiled_report = json(&compiled);
	let bundles = compiled_report["bundles"].as_array().unwrap();
	assert!(!bundles.is_empty(), "{compiled_report}");
	let mut compiled_cases = 0;
	for path in bundles {
		let bundle: Value =
			serde_json::from_slice(&fs::read(path.as_str().unwrap()).unwrap()).unwrap();
		assert_eq!(bundle["mod_name"], "Companion Mod");
		compiled_cases += bundle["cases"].as_array().unwrap().len();
	}
	assert_eq!(compiled_cases, 2, "{compiled_report}");
	assert_eq!(
		fs::read_to_string(root.join("foch.toml")).unwrap(),
		manifest
	);
}

const SCOPE_MISSING: &str = "Country tags and event definitions were not checked because no matching base was loaded. A vanilla tag or event is not reported as unknown.";
const SCOPE_MATCHING: &str = "Country tags and event definitions were checked from the ordered playset and the matching base. A tag or event in neither is unknown. An event that is defined there but was not collected from the mod under test cannot be checked as hidden and triggered-only.";

fn write_text(path: &std::path::Path, text: &str) {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).unwrap();
	}
	fs::write(path, text).unwrap();
}

fn write_named_mod(root: &std::path::Path, name: &str, dependencies: &str) {
	let deps = if dependencies.is_empty() {
		String::new()
	} else {
		format!("dependencies={{\n{dependencies}\n}}\n")
	};
	write_text(
		&root.join("descriptor.mod"),
		&format!("name=\"{name}\"\n{deps}"),
	);
	write_text(
		&root.join("events/cases.txt"),
		&format!(
			"namespace = {name}\n#test(time=1444.11.11, tag=SWE, name=\"smoke\")\ncountry_event = {{ id = {name}.1 hidden = yes is_triggered_only = yes immediate = {{}} }}\n"
		),
	);
}

fn forward_path(path: &std::path::Path) -> String {
	path.display().to_string().replace('\\', "/")
}

fn messages(report: &Value) -> Vec<&str> {
	report["diagnostics"]
		.as_array()
		.unwrap()
		.iter()
		.map(|diagnostic| diagnostic["message"].as_str().unwrap())
		.collect()
}

fn codes(report: &Value) -> Vec<&str> {
	report["diagnostics"]
		.as_array()
		.unwrap()
		.iter()
		.map(|diagnostic| diagnostic["code"].as_str().unwrap())
		.collect()
}

#[test]
fn a_missing_base_does_not_call_a_vanilla_tag_unknown() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	write_named_mod(&dir.path().join("mod"), "fixture", "");
	write_text(
		&dir.path().join("mod/common/country_tags/00.txt"),
		"ZZZ = \"countries/Custom.txt\"\n",
	);
	let collected = run(
		dir.path(),
		&["test", "mod", "--collect-only", "--format", "json"],
	);
	assert!(
		collected.status.success(),
		"{}",
		String::from_utf8_lossy(&collected.stderr)
	);
	let report = json(&collected);
	assert_eq!(report["check_scope"]["base"], "missing");
	assert_eq!(report["check_scope"]["tags"], "not_checked");
	assert_eq!(report["check_scope"]["events"], "not_checked");
	assert_eq!(report["check_scope"]["detail"], SCOPE_MISSING);
	assert!(!codes(&report).contains(&"unknown_tag"), "{report}");
	assert!(!dir.path().join("global-data").exists());

	let offline = run(
		dir.path(),
		&["test", "mod", "--offline", "--format", "json"],
	);
	assert!(
		offline.status.success(),
		"{}",
		String::from_utf8_lossy(&offline.stderr)
	);
	let report = json(&offline);
	assert_eq!(report["check_scope"]["detail"], SCOPE_MISSING);
	assert_eq!(report["results"][0]["outcome"], "smoke");
	assert!(!dir.path().join("global-data").exists());
}

#[test]
fn a_matching_base_checks_tags_from_the_same_ordered_input() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	let base = dir.path().join("base");
	write_text(
		&base.join("common/country_tags/00_countries.txt"),
		"SWE = \"countries/Sweden.txt\"\n",
	);
	write_text(
		&dir.path().join("mod/common/country_tags/01.txt"),
		"ZZZ = \"countries/Custom.txt\"\n",
	);
	write_text(
		&dir.path().join("mod/events/cases.txt"),
		"namespace = fixture\n#test(time=1444.11.11, tag=SWE, name=\"sweden\")\ncountry_event = { id = fixture.1 hidden = yes is_triggered_only = yes immediate = {} }\n#test(time=1444.11.11, tag=ZZZ, name=\"mod\")\ncountry_event = { id = fixture.2 hidden = yes is_triggered_only = yes immediate = {} }\n#test(time=1444.11.11, tag=QQQ, name=\"missing\")\ncountry_event = { id = fixture.3 hidden = yes is_triggered_only = yes immediate = {} }\n",
	);
	let output = run(
		dir.path(),
		&[
			"test",
			"mod",
			"--collect-only",
			"--format",
			"json",
			"--game-path",
			base.to_str().unwrap(),
		],
	);
	let report = json(&output);
	assert_eq!(report["check_scope"]["base"], "matching");
	assert_eq!(report["check_scope"]["tags"], "checked");
	assert_eq!(report["check_scope"]["events"], "checked");
	assert_eq!(report["check_scope"]["detail"], SCOPE_MATCHING);
	let text = messages(&report).join("\n");
	assert!(text.contains("QQQ"), "{report}");
	assert!(!text.contains("SWE"), "{report}");
	assert!(!text.contains("ZZZ"), "{report}");
	assert!(codes(&report).contains(&"unknown_tag"), "{report}");
	assert!(!dir.path().join("global-data").exists());
}

#[test]
fn an_empty_matching_base_makes_an_undefined_tag_unknown() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	let base = dir.path().join("base");
	fs::create_dir_all(&base).unwrap();
	write_named_mod(&dir.path().join("mod"), "fixture", "");
	let output = run(
		dir.path(),
		&[
			"test",
			"mod",
			"--collect-only",
			"--format",
			"json",
			"--game-path",
			base.to_str().unwrap(),
		],
	);
	let report = json(&output);
	assert_eq!(report["check_scope"]["base"], "matching");
	let text = messages(&report).join("\n");
	assert!(text.contains("SWE"), "{report}");
	assert!(codes(&report).contains(&"unknown_tag"), "{report}");
}

#[test]
fn event_categories_follow_the_loaded_base() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	write_text(
		&dir.path().join("mod/events/own.txt"),
		"namespace = fixture\ncountry_event = { id = fixture.1 hidden = yes is_triggered_only = yes immediate = {} }\n",
	);
	write_text(
		&dir.path().join("mod/tests/fires.txt"),
		"test = { name = own time = 1444.11.11 tag = SWE fire = fixture.1 }\ntest = { name = vanilla time = 1444.11.11 tag = SWE fire = vanilla.1 }\ntest = { name = missing time = 1444.11.11 tag = SWE fire = missing.1 }\n",
	);
	let without_base = run(
		dir.path(),
		&["test", "mod", "--collect-only", "--format", "json"],
	);
	let report = json(&without_base);
	assert!(!codes(&report).contains(&"unknown_event"), "{report}");
	assert!(!codes(&report).contains(&"unknown_tag"), "{report}");
	let text = messages(&report).join("\n");
	assert!(
		text.contains("vanilla.1") && text.contains("not collected"),
		"{report}"
	);
	assert!(
		text.contains("missing.1") && text.contains("cannot verify"),
		"{report}"
	);
	assert!(!text.contains("fixture.1"), "{report}");

	let base = dir.path().join("base");
	write_text(
		&base.join("common/country_tags/00_countries.txt"),
		"SWE = \"countries/Sweden.txt\"\n",
	);
	write_text(
		&base.join("events/vanilla.txt"),
		"namespace = vanilla\ncountry_event = { id = vanilla.1 hidden = yes is_triggered_only = yes }\n",
	);
	let with_base = run(
		dir.path(),
		&[
			"test",
			"mod",
			"--collect-only",
			"--format",
			"json",
			"--game-path",
			base.to_str().unwrap(),
		],
	);
	let report = json(&with_base);
	assert_eq!(report["check_scope"]["events"], "checked");
	let text = messages(&report).join("\n");
	assert!(
		codes(&report).contains(&"unknown_event") && text.contains("missing.1"),
		"{report}"
	);
	assert!(
		codes(&report).contains(&"unsupported_target")
			&& text.contains("vanilla.1")
			&& text.contains("not collected"),
		"{report}"
	);
	assert!(!text.contains("fixture.1"), "{report}");
	assert!(!text.contains("SWE"), "{report}");
}

#[test]
fn an_explicit_playset_keeps_author_order_and_inserts_missing_dependencies() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	write_named_mod(&dir.path().join("mods/zed"), "Zed", "");
	write_named_mod(&dir.path().join("mods/amy"), "Amy", "");
	write_text(
		&dir.path().join("foch.toml"),
		"[project]\n\n[[project.mods]]\npath = \"mods/zed\"\n\n[[project.mods]]\npath = \"mods/amy\"\n",
	);
	let output = run(
		dir.path(),
		&[
			"test",
			"mods/zed",
			"--playset",
			"foch.toml",
			"--collect-only",
			"--format",
			"json",
		],
	);
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	let report = json(&output);
	let names: Vec<_> = report["playset"]
		.as_array()
		.unwrap()
		.iter()
		.map(|name| name.as_str().unwrap())
		.collect();
	assert_eq!(names, ["Zed", "Amy"]);

	write_text(
		&dir.path().join("positions.toml"),
		"[project]\n\n[[project.mods]]\npath = \"mods/amy\"\nposition = 1\n\n[[project.mods]]\npath = \"mods/zed\"\nposition = 0\n",
	);
	let positioned = run(
		dir.path(),
		&[
			"test",
			"mods/amy",
			"--playset",
			"positions.toml",
			"--collect-only",
			"--format",
			"json",
		],
	);
	assert!(
		positioned.status.success(),
		"{}",
		String::from_utf8_lossy(&positioned.stderr)
	);
	let report = json(&positioned);
	let names: Vec<_> = report["playset"]
		.as_array()
		.unwrap()
		.iter()
		.map(|name| name.as_str().unwrap())
		.collect();
	assert_eq!(names, ["Zed", "Amy"]);

	write_named_mod(&dir.path().join("mods/other"), "Other", "");
	write_named_mod(&dir.path().join("mods/leaf"), "Leaf", "\"Base\"");
	write_named_mod(&dir.path().join("mods/base"), "Base", "");
	write_text(
		&dir.path().join("mod/base.mod"),
		&format!(
			"name=\"Base\"\npath=\"{}\"\n",
			forward_path(&dir.path().join("mods/base"))
		),
	);
	write_text(
		&dir.path().join("deps.toml"),
		"[project]\n\n[[project.mods]]\npath = \"mods/other\"\n\n[[project.mods]]\npath = \"mods/leaf\"\n",
	);
	let inserted = run(
		dir.path(),
		&[
			"test",
			"mods/leaf",
			"--playset",
			"deps.toml",
			"--collect-only",
			"--format",
			"json",
		],
	);
	assert!(
		inserted.status.success(),
		"{}",
		String::from_utf8_lossy(&inserted.stderr)
	);
	let report = json(&inserted);
	let names: Vec<_> = report["playset"]
		.as_array()
		.unwrap()
		.iter()
		.map(|name| name.as_str().unwrap())
		.collect();
	assert_eq!(names, ["Other", "Base", "Leaf"]);

	write_named_mod(&dir.path().join("mods/mid"), "Mid", "\"Base\"");
	write_named_mod(&dir.path().join("mods/chain"), "Leaf", "\"Mid\"");
	write_text(
		&dir.path().join("mod/mid.mod"),
		&format!(
			"name=\"Mid\"\npath=\"{}\"\n",
			forward_path(&dir.path().join("mods/mid"))
		),
	);
	write_text(
		&dir.path().join("chain.toml"),
		"[project]\n\n[[project.mods]]\npath = \"mods/chain\"\n",
	);
	let chain = run(
		dir.path(),
		&[
			"test",
			"mods/chain",
			"--playset",
			"chain.toml",
			"--collect-only",
			"--format",
			"json",
		],
	);
	assert!(
		chain.status.success(),
		"{}",
		String::from_utf8_lossy(&chain.stderr)
	);
	let report = json(&chain);
	let names: Vec<_> = report["playset"]
		.as_array()
		.unwrap()
		.iter()
		.map(|name| name.as_str().unwrap())
		.collect();
	assert_eq!(names, ["Base", "Mid", "Leaf"]);
}

#[test]
fn a_playset_base_must_match_game_path() {
	let dir = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
	let base_a = dir.path().join("base-a");
	let base_b = dir.path().join("base-b");
	fs::create_dir_all(&base_a).unwrap();
	fs::create_dir_all(&base_b).unwrap();
	write_named_mod(&dir.path().join("mods/src"), "Src", "");
	write_text(
		&dir.path().join("foch.toml"),
		"[project]\ngame_path = \"base-a\"\n\n[[project.mods]]\npath = \"mods/src\"\n",
	);
	let output = run(
		dir.path(),
		&[
			"test",
			"mods/src",
			"--playset",
			"foch.toml",
			"--game-path",
			base_b.to_str().unwrap(),
			"--collect-only",
		],
	);
	assert!(!output.status.success());
	let text = format!(
		"{}{}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	assert!(
		text.contains("the --game-path base does not match the playset's game_path"),
		"{text}"
	);
}
