//! Receipt adjudication: `expect_receipt` steps judged against a fabricated
//! host event stream, the way `--with-plugins` sessions preserve one.
//! These prove the framework's judging, not any plugin's behavior.

use foch_test::model::RunId;
use foch_test::source::{RelPath, SourceFile, SourceKind};
use foch_test::{
	Bundle, ExpandOptions, GroupOptions, Plan, ProjectFacts, RunArtifacts, RunContext, RunnerExit,
	Status, Timing, collect, expand, group, judge, lint,
};
use std::collections::BTreeMap;

const CASES: &str = r#"test = {
	name = join_check
	time = 1444.11.11
	tag = SWE
	effect = { set_country_flag = ready }
	expect_receipt = {
		plugin = dev.foch.directed-pacts
		type = join
		case = join_check
		request = r1
		status = joined
	}
	expect = { has_country_flag = ready }
}
test = {
	name = refused_check
	time = 1444.11.11
	tag = SWE
	expect_receipt = {
		plugin = dev.foch.directed-pacts
		type = join
		case = refused_check
		request = r2
		status = refused_already_enemy
	}
}
test = {
	name = wrong_request
	time = 1444.11.11
	tag = SWE
	expect_receipt = {
		plugin = dev.foch.directed-pacts
		type = join
		case = wrong_request
		request = r3
		status = joined
	}
}
test = {
	name = two_steps
	time = 1444.11.11
	tag = SWE
	expect_receipt = {
		plugin = dev.foch.directed-pacts
		type = join
		case = two_steps
		request = a
		status = joined
	}
	expect_receipt = {
		plugin = dev.foch.directed-pacts
		type = join
		case = two_steps
		request = b
		status = already_fulfilled
	}
}
test = {
	name = native_only
	time = 1444.11.11
	tag = SWE
	expect = { always = yes }
}"#;

fn plan() -> Plan {
	plan_from(CASES, false)
}

fn plan_from(cases: &str, shared: bool) -> Plan {
	let sources = vec![SourceFile {
		mod_name: "Receipt Mod".into(),
		path: RelPath::new("tests/scenario.txt").unwrap(),
		kind: SourceKind::TestsDir,
		text: cases.into(),
	}];
	let collection = collect(&sources);
	assert!(
		collection.diagnostics.is_empty(),
		"{:#?}",
		collection.diagnostics
	);
	let expanded = expand(&collection, &ExpandOptions::default()).unwrap();
	let linted = lint(&expanded, &ProjectFacts::default());
	group(
		expanded,
		&linted,
		&GroupOptions {
			runner_shared_sessions: shared,
			..GroupOptions::default()
		},
	)
}

fn judge_with(stream: Option<&str>) -> BTreeMap<String, Status> {
	judge_with_launch(stream, None)
}

/// Judge each session against its own launch identity — the shape
/// production has once the CLI passes `host_launch_id` through.
fn judge_launches(stream: &str) -> BTreeMap<String, Status> {
	let plan = plan();
	let context = RunContext {
		run_id: RunId::parse("run-1").unwrap(),
		mod_name: "Receipt Mod".into(),
	};
	let mut results = BTreeMap::new();
	for (position, session) in plan.sessions.iter().enumerate() {
		let bundle = foch_test::compile(&plan, session.id, &context).unwrap();
		let launch = format!("{}_launch{position}", bundle.namespace);
		let log = simulated_log(&bundle);
		let artifacts = RunArtifacts {
			game_log: Some(&log),
			error_log: Some(""),
			exit: RunnerExit::Success,
			timing: Timing::default(),
			profile_untouched: true,
			host_events: Some(stream),
			host_launch_id: Some(&launch),
		};
		for result in judge(&bundle, &plan, &artifacts) {
			results.insert(result.node.name.to_string(), result.status);
		}
	}
	results
}

/// `launch` is the host identity the runner stamped; `None` is the legacy
/// caller that has not been moved to pass one.
fn judge_with_launch(stream: Option<&str>, launch: Option<&str>) -> BTreeMap<String, Status> {
	let plan = plan();
	let context = RunContext {
		run_id: RunId::parse("run-1").unwrap(),
		mod_name: "Receipt Mod".into(),
	};
	// Every case sits in its own session; judge them all, like the CLI does.
	let mut results = BTreeMap::new();
	for session in &plan.sessions {
		let bundle = foch_test::compile(&plan, session.id, &context).unwrap();
		let log = simulated_log(&bundle);
		let artifacts = RunArtifacts {
			game_log: Some(&log),
			error_log: Some(""),
			exit: RunnerExit::Success,
			timing: Timing::default(),
			profile_untouched: true,
			host_events: stream,
			host_launch_id: launch,
		};
		for result in judge(&bundle, &plan, &artifacts) {
			results.insert(result.node.name.to_string(), result.status);
		}
	}
	results
}

/// Deterministic stand-ins for the runner's fresh launch IDs. These fixtures
/// exercise judging; production creates the ID when staging the actual launch.
fn launch_identities() -> Vec<String> {
	let plan = plan();
	let context = RunContext {
		run_id: RunId::parse("run-1").unwrap(),
		mod_name: "Receipt Mod".into(),
	};
	plan.sessions
		.iter()
		.enumerate()
		.map(|(position, session)| {
			let bundle = foch_test::compile(&plan, session.id, &context).unwrap();
			format!("{}_launch{position}", bundle.namespace)
		})
		.collect()
}

/// The log the engine writes when every native check passes.
fn simulated_log(bundle: &Bundle) -> String {
	use std::fmt::Write;
	let mut log = String::from("unrelated engine output\n");
	for window in &bundle.windows {
		let prefix = format!("{} c{}", bundle.marker_prefix, window.case.0);
		let _ = writeln!(
			log,
			"[effectimplementation.cpp:1]: EVENT [{}]:{} BEGIN",
			window.begin, prefix
		);
		for check in bundle
			.checks
			.iter()
			.filter(|check| check.case == window.case)
		{
			let _ = writeln!(
				log,
				"[effectimplementation.cpp:1]: EVENT [{}]:{} PASS {}",
				check.date,
				prefix,
				check.kind.label()
			);
		}
		let _ = writeln!(
			log,
			"[effectimplementation.cpp:1]: EVENT [{}]:{} END",
			window.end, prefix
		);
	}
	log
}

fn receipt_line(plugin: &str, launch: &str, payload: &str) -> String {
	// The host serializes the message as a real JSON string, so the payload
	// lands escaped; build the line the same way instead of hand-formatting.
	serde_json::json!({
		"format": 1,
		"run_id": launch,
		"plugin_id": plugin,
		"version": "1.0.0",
		"phase": "entry",
		"pid": 1234,
		"timestamp_ms": 1700000000000_u64,
		"log_path": "fixture/plugin.log",
		"event": "log",
		"message": format!("FOCH_RECEIPT/1 {payload}"),
	})
	.to_string()
}

fn state_line(plugin: &str, state: &str, launch: &str) -> String {
	serde_json::json!({
		"format": 1,
		"run_id": launch,
		"plugin_id": plugin,
		"version": "1.0.0",
		"phase": "entry",
		"pid": 1234,
		"timestamp_ms": 1700000000000_u64,
		"log_path": "fixture/plugin.log",
		"event": "state",
		"state": state,
	})
	.to_string()
}

fn full_stream() -> String {
	let mut lines: Vec<String> = Vec::new();
	for run in launch_identities() {
		lines.extend([
			state_line("dev.foch.directed-pacts", "active", &run),
			receipt_line(
				"dev.foch.directed-pacts",
				&run,
				"{\"type\":\"join\",\"case\":\"join_check\",\"request\":\"r1\",\"status\":\"joined\"}",
			),
			receipt_line(
				"dev.foch.directed-pacts",
				&run,
				"{\"type\":\"join\",\"case\":\"refused_check\",\"request\":\"r2\",\"status\":\"refused_already_enemy\"}",
			),
			receipt_line(
				"dev.foch.directed-pacts",
				&run,
				"{\"type\":\"join\",\"case\":\"two_steps\",\"request\":\"a\",\"status\":\"joined\"}",
			),
			receipt_line(
				"dev.foch.directed-pacts",
				&run,
				"{\"type\":\"join\",\"case\":\"two_steps\",\"request\":\"b\",\"status\":\"already_fulfilled\"}",
			),
			// An unrelated plugin's records are ignored, not corruption —
			// even when their payload breaks this contract.
			state_line("dev.foch.other-plugin", "active", &run),
			receipt_line(
				"dev.foch.other-plugin",
				&run,
				"{\"type\":\"join\",\"case\":\"join_check\",\"request\":\"r1\"}",
			),
			receipt_line("dev.foch.other-plugin", &run, "{broken"),
		]);
	}
	// Records of other launches never satisfy or poison this session: a
	// receipt for wrong_request and a terminal failed state, both stale.
	let stale = launch_identities()[0].clone();
	lines.push(receipt_line(
		"dev.foch.directed-pacts",
		"an-old-launch",
		"{\"type\":\"join\",\"case\":\"wrong_request\",\"request\":\"r3\",\"status\":\"joined\"}",
	));
	lines.push(state_line(
		"dev.foch.directed-pacts",
		"failed",
		"an-old-launch",
	));
	lines.push(receipt_line(
		"dev.foch.directed-pacts",
		&format!("{stale}_old"),
		"{\"type\":\"join\",\"case\":\"join_check\",\"request\":\"r1\",\"status\":\"joined\"}",
	));
	lines.join(
		"
",
	)
}

#[test]
fn numeric_ranges_match_payload_values() {
	// Receipts serialize every payload as a string; the judge parses the
	// constrained fields and compares inclusive bounds. In-range and
	// negative bounds pass; below-min and non-numeric payloads fail.
	let cases = r#"
test = { name = gold_in time = 1444.11.11 tag = SWE expect_receipt = {
	plugin = dev.foch.directed-pacts case = gold_in request = n1
	status = paid numeric = { gold = { min = 100 max = 500 } } } }
test = { name = gold_low time = 1444.11.11 tag = SWE expect_receipt = {
	plugin = dev.foch.directed-pacts case = gold_low request = n2
	status = paid numeric = { gold = { min = 1000 } } } }
test = { name = gold_words time = 1444.11.11 tag = SWE expect_receipt = {
	plugin = dev.foch.directed-pacts case = gold_words request = n3
	status = paid numeric = { gold = { max = 500 } } } }
test = { name = stable_min time = 1444.11.11 tag = SWE expect_receipt = {
	plugin = dev.foch.directed-pacts case = stable_min request = n4
	status = paid numeric = { stability = { min = -3 } } } }"#;
	let plan = plan_from(cases, false);
	let context = RunContext {
		run_id: RunId::parse("run-1").unwrap(),
		mod_name: "Receipt Mod".into(),
	};
	let mut results = BTreeMap::new();
	for (position, session) in plan.sessions.iter().enumerate() {
		let bundle = foch_test::compile(&plan, session.id, &context).unwrap();
		let launch = format!("{}_launch{position}", bundle.namespace);
		let log = simulated_log(&bundle);
		let payload = |case: &str, request: &str, extra: &str| {
			format!(
				"{{\"type\":\"pay\",\"case\":\"{case}\",\"request\":\"{request}\",\"status\":\"paid\",{extra}}}"
			)
		};
		let stream = [
			state_line("dev.foch.directed-pacts", "active", &launch),
			receipt_line(
				"dev.foch.directed-pacts",
				&launch,
				&payload("gold_in", "n1", "\"gold\":\"250\""),
			),
			receipt_line(
				"dev.foch.directed-pacts",
				&launch,
				&payload("gold_low", "n2", "\"gold\":\"250\""),
			),
			receipt_line(
				"dev.foch.directed-pacts",
				&launch,
				&payload("gold_words", "n3", "\"gold\":\"rich\""),
			),
			receipt_line(
				"dev.foch.directed-pacts",
				&launch,
				&payload("stable_min", "n4", "\"stability\":\"-1\""),
			),
		]
		.join(
			"
",
		);
		let artifacts = RunArtifacts {
			game_log: Some(&log),
			error_log: Some(""),
			exit: RunnerExit::Success,
			timing: Timing::default(),
			profile_untouched: true,
			host_events: Some(&stream),
			host_launch_id: Some(&launch),
		};
		for result in judge(&bundle, &plan, &artifacts) {
			results.insert(result.node.name.to_string(), result.status);
		}
	}
	assert_eq!(results["gold_in"], Status::Passed);
	assert_eq!(results["stable_min"], Status::Passed);
	assert_eq!(results["gold_low"], Status::Failed);
	assert_eq!(results["gold_words"], Status::Failed);
}

#[test]
fn numeric_constraints_are_guarded_at_collection() {
	let bad = |numeric: &str| {
		let cases = format!(
			"test = {{ name = g time = 1444.11.11 tag = SWE expect_receipt = {{ 				plugin = p case = g request = r numeric = {{ {numeric} }} }} }}"
		);
		let sources = [SourceFile {
			mod_name: "Receipt Mod".into(),
			path: RelPath::new("tests/a.txt").unwrap(),
			kind: SourceKind::TestsDir,
			text: cases,
		}];
		collect(&sources).diagnostics
	};
	assert!(
		bad("gold = { min = 1 } gold = { max = 2 }")
			.iter()
			.any(|d| d.message.contains("constrains gold twice"))
	);
	assert!(
		bad("gold = { }")
			.iter()
			.any(|d| d.message.contains("at least one of min or max"))
	);
	assert!(
		bad("gold = { min = 1e999 }")
			.iter()
			.any(|d| d.message.contains("finite"))
	);
	// One field constrained both as an exact string and numerically is a
	// conflict, not a conjunction.
	assert!(
		bad("case = { min = 1 }")
			.iter()
			.any(|d| d.message.contains("constrained twice"))
	);
}

#[test]
fn matched_receipts_pass_and_correlation_gates_fail() {
	let results = judge_launches(&full_stream());
	assert_eq!(results["join_check"], Status::Passed);
	assert_eq!(
		results["refused_check"],
		Status::Passed,
		"an expected refusal is a matched payload, not an error"
	);
	assert_eq!(
		results["wrong_request"],
		Status::Failed,
		"a receipt from another run must not satisfy this session"
	);
	assert_eq!(
		results["two_steps"],
		Status::Passed,
		"two expectations with distinct requests each bind to their own receipt"
	);
	assert_eq!(results["native_only"], Status::Passed);
}

#[test]
fn a_missing_stream_with_receipt_expectations_is_an_infrastructure_error() {
	let results = judge_with(None);
	assert_eq!(results["join_check"], Status::RuntimeError);
	assert_eq!(results["native_only"], Status::Passed);
}

#[test]
fn a_plugin_that_did_not_stay_active_is_an_infrastructure_error() {
	let stream = [
		state_line("dev.foch.directed-pacts", "active", &launch_identities()[0]),
		receipt_line(
			"dev.foch.directed-pacts",
			&launch_identities()[0],
			"{\"type\":\"join\",\"case\":\"join_check\",\"request\":\"r1\",\"status\":\"joined\"}",
		),
		state_line("dev.foch.directed-pacts", "failed", &launch_identities()[0]),
	]
	.join("\n");
	let results = judge_launches(&stream);
	assert_eq!(
		results["join_check"],
		Status::RuntimeError,
		"active-then-failed must not be masked by the earlier receipt"
	);
}

#[test]
fn corrupt_convention_lines_fail_the_whole_receipt_evidence() {
	for stream in [
		// Bad JSON in the payload.
		format!(
			"{}\n{}",
			state_line("dev.foch.directed-pacts", "active", &launch_identities()[0]),
			receipt_line(
				"dev.foch.directed-pacts",
				&launch_identities()[0],
				"{broken",
			)
		),
		// A future version of the prefix is not this contract.
		format!(
			"{}\n{}",
			state_line("dev.foch.directed-pacts", "active", &launch_identities()[0]),
			serde_json::json!({
				"format": 1,
				"run_id": launch_identities()[0],
				"plugin_id": "dev.foch.directed-pacts",
				"event": "log",
				"message": "FOCH_RECEIPT/2 {}",
			})
		),
		// A payload trying to override the envelope identity.
		format!(
			"{}\n{}",
			state_line("dev.foch.directed-pacts", "active", &launch_identities()[0]),
			receipt_line(
				"dev.foch.directed-pacts",
				&launch_identities()[0],
				"{\"run_id\":\"run-9\",\"case\":\"join_check\",\"request\":\"r1\"}"
			),
		),
		// A line that is not JSON at all.
		format!(
			"{}\nnot json",
			state_line("dev.foch.directed-pacts", "active", &launch_identities()[0])
		),
	] {
		let results = judge_launches(&stream);
		assert_eq!(
			results["join_check"],
			Status::RuntimeError,
			"corrupt receipt evidence must not be judged as a domain outcome"
		);
		assert_eq!(results["native_only"], Status::Passed);
	}
}

#[test]
fn non_string_payload_values_never_match_by_coercion() {
	let stream = [
		state_line("dev.foch.directed-pacts", "active", &launch_identities()[0]),
		receipt_line(
			"dev.foch.directed-pacts",
			&launch_identities()[0],
			"{\"type\":\"join\",\"case\":\"join_check\",\"request\":\"r1\",\"status\":5}",
		),
	]
	.join("\n");
	let results = judge_launches(&stream);
	assert_eq!(
		results["join_check"],
		Status::RuntimeError,
		"a numeric payload is corrupt evidence, not a domain refusal"
	);
}

#[test]
fn missing_correlation_fields_are_a_collection_error() {
	let sources = vec![SourceFile {
		mod_name: "Receipt Mod".into(),
		path: RelPath::new("tests/scenario.txt").unwrap(),
		kind: SourceKind::TestsDir,
		text: r#"test = {
	name = uncorrelated
	time = 1444.11.11
	tag = SWE
	expect_receipt = { plugin = dev.foch.directed-pacts type = join }
}"#
		.into(),
	}];
	let collection = collect(&sources);
	assert!(
		collection
			.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("needs a case")),
		"{:#?}",
		collection.diagnostics
	);
}

#[test]
fn a_stream_stamped_with_the_run_id_never_matches() {
	// The whole test-run ID is not the actual launch ID, even when every
	// other field and receipt matches.
	let plan = plan();
	let context = RunContext {
		run_id: RunId::parse("run-1").unwrap(),
		mod_name: "Receipt Mod".into(),
	};
	let mut results = BTreeMap::new();
	for session in &plan.sessions {
		let bundle = foch_test::compile(&plan, session.id, &context).unwrap();
		let stream = format!(
			"{}
{}",
			state_line(
				"dev.foch.directed-pacts",
				"active",
				&bundle.run_id.to_string()
			),
			receipt_line(
				"dev.foch.directed-pacts",
				&bundle.run_id.to_string(),
				"{\"type\":\"join\",\"case\":\"join_check\",\"request\":\"r1\",\"status\":\"joined\"}",
			),
		);
		let artifacts = RunArtifacts {
			game_log: Some(&simulated_log(&bundle)),
			error_log: Some(""),
			exit: RunnerExit::Success,
			timing: Timing::default(),
			profile_untouched: true,
			host_events: Some(&stream),
			host_launch_id: Some("actual-current-launch"),
		};
		for result in judge(&bundle, &plan, &artifacts) {
			results.insert(result.node.name.to_string(), result.status);
		}
	}
	// Under the old comparison against the run id this stream matched and
	// passed; any non-success verdict proves the identity fix, whether the
	// missing evidence reads as a domain failure or an infrastructure error.
	assert!(
		!results["join_check"].is_success(),
		"a run-id-stamped stream is another launch; it must never pass (got {:?})",
		results["join_check"]
	);
}

#[test]
fn an_old_launch_neither_backs_nor_poisons_this_one() {
	let run = launch_identities()[0].clone();
	let stream = format!(
		"{}
{}
{}
{}",
		// This launch: the plugin is active and the receipt matches.
		state_line("dev.foch.directed-pacts", "active", &run),
		receipt_line(
			"dev.foch.directed-pacts",
			&run,
			"{\"type\":\"join\",\"case\":\"join_check\",\"request\":\"r1\",\"status\":\"joined\"}",
		),
		// An old launch of the same plugin ended failed and carries a
		// broken receipt: neither may touch this launch's verdict.
		state_line("dev.foch.directed-pacts", "failed", "an-old-launch"),
		receipt_line("dev.foch.directed-pacts", "an-old-launch", "{broken"),
	);
	let results = judge_launches(&stream);
	assert_eq!(results["join_check"], Status::Passed);
}

#[test]
fn receipts_do_not_mask_a_native_failure() {
	let plan = plan();
	let context = RunContext {
		run_id: RunId::parse("run-1").unwrap(),
		mod_name: "Receipt Mod".into(),
	};
	let session = plan.sessions[0].id;
	let bundle = foch_test::compile(&plan, session, &context).unwrap();
	// BEGIN/END with no check lines: every native check goes missing.
	let mut log = String::new();
	for window in &bundle.windows {
		use std::fmt::Write;
		let prefix = format!("{} c{}", bundle.marker_prefix, window.case.0);
		let _ = writeln!(
			log,
			"[effectimplementation.cpp:1]: EVENT [{}]:{} BEGIN",
			window.begin, prefix
		);
		let _ = writeln!(
			log,
			"[effectimplementation.cpp:1]: EVENT [{}]:{} END",
			window.end, prefix
		);
	}
	let artifacts = RunArtifacts {
		game_log: Some(&log),
		error_log: Some(""),
		exit: RunnerExit::Success,
		timing: Timing::default(),
		profile_untouched: true,
		host_events: Some(&full_stream()),
		host_launch_id: Some(&launch_identities()[0]),
	};
	let results = judge(&bundle, &plan, &artifacts);
	let join_check = results
		.iter()
		.find(|result| result.node.name.to_string().contains("join_check"))
		.unwrap();
	eprintln!(
		"status={:?} notes={:#?} failures={:#?}",
		join_check.status, join_check.notes, join_check.failures
	);
	assert_eq!(join_check.status, Status::Incomplete);
}

#[test]
fn the_collector_rejects_dropped_constraints() {
	for (label, text) in [
		(
			"bare item",
			r#"test = { name = t time = 1444.11.11 tag = SWE
	expect_receipt = { plugin = p case = t request = r bare_item } }"#,
		),
		(
			"plugin twice",
			r#"test = { name = t time = 1444.11.11 tag = SWE
	expect_receipt = { plugin = p plugin = q case = t request = r } }"#,
		),
		(
			"field twice",
			r#"test = { name = t time = 1444.11.11 tag = SWE
	expect_receipt = { plugin = p case = t request = r status = joined status = refused } }"#,
		),
		(
			"block field",
			r#"test = { name = t time = 1444.11.11 tag = SWE
	expect_receipt = { plugin = p case = t request = r nested = { x } } }"#,
		),
	] {
		let sources = vec![SourceFile {
			mod_name: "Receipt Mod".into(),
			path: RelPath::new("tests/scenario.txt").unwrap(),
			kind: SourceKind::TestsDir,
			text: text.into(),
		}];
		let collection = collect(&sources);
		assert!(
			!collection.diagnostics.is_empty(),
			"{label} must be a collection error"
		);
	}
}

#[test]
fn a_legacy_caller_without_a_launch_id_never_judges_receipts() {
	// No host_launch_id: the fallback identity is deterministic per bundle,
	// so receipt evidence cannot be trusted to belong to this launch.
	let results = judge_with(Some(&full_stream()));
	assert_eq!(results["join_check"], Status::RuntimeError);
	assert!(matches!(results["native_only"], Status::Passed));
}

#[test]
fn a_shared_triple_across_steps_is_rejected_at_collection() {
	// Identical plugin/case/request on two steps would let one receipt
	// satisfy both — the reviewer's exact counterexample. Collection now
	// refuses the triple instead of trusting field differences.
	let sources = vec![SourceFile {
		mod_name: "Receipt Mod".into(),
		path: RelPath::new("tests/scenario.txt").unwrap(),
		kind: SourceKind::TestsDir,
		text: r#"test = { name = cloned time = 1444.11.11 tag = SWE
	expect_receipt = { plugin = p case = cloned request = same status = joined }
	expect_receipt = { plugin = p case = cloned request = same status = joined } }"#
			.into(),
	}];
	let collection = collect(&sources);
	assert!(
		collection
			.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("own request token")),
		"{:#?}",
		collection.diagnostics
	);
}

#[test]
fn distinct_requests_across_steps_still_collect() {
	let sources = vec![SourceFile {
		mod_name: "Receipt Mod".into(),
		path: RelPath::new("tests/scenario.txt").unwrap(),
		kind: SourceKind::TestsDir,
		text: r#"test = { name = distinct time = 1444.11.11 tag = SWE
	expect_receipt = { plugin = p case = distinct request = a status = joined }
	expect_receipt = { plugin = p case = distinct request = b status = already_fulfilled } }"#
			.into(),
	}];
	let collection = collect(&sources);
	assert!(
		collection.diagnostics.is_empty(),
		"{:#?}",
		collection.diagnostics
	);
}

#[test]
fn corrupt_receipts_of_another_cases_plugin_do_not_poison_this_case() {
	let cases = r#"test = {
		name = first time = 1444.11.11 tag = SWE
		expect_receipt = { plugin = p case = first request = r status = joined }
	}
	test = {
		name = second time = 1444.11.11 tag = FRA
		expect_receipt = { plugin = q case = second request = r status = joined }
	}"#;
	for shared in [false, true] {
		let plan = plan_from(cases, shared);
		assert_eq!(plan.sessions.len(), if shared { 1 } else { 2 });
		let context = RunContext {
			run_id: RunId::parse("run-1").unwrap(),
			mod_name: "Receipt Mod".into(),
		};
		let mut results = BTreeMap::new();
		for session in &plan.sessions {
			let bundle = foch_test::compile(&plan, session.id, &context).unwrap();
			let log = simulated_log(&bundle);
			let stream = [
				state_line("p", "active", "launch"),
				receipt_line(
					"p",
					"launch",
					r#"{"case":"first","request":"r","status":"joined"}"#,
				),
				state_line("q", "active", "launch"),
				receipt_line("q", "launch", "{broken"),
			]
			.join("\n");
			let artifacts = RunArtifacts {
				game_log: Some(&log),
				error_log: Some(""),
				exit: RunnerExit::Success,
				timing: Timing::default(),
				profile_untouched: true,
				host_events: Some(&stream),
				host_launch_id: Some("launch"),
			};
			for result in judge(&bundle, &plan, &artifacts) {
				results.insert(result.node.name.to_string(), result.status);
			}
		}
		assert_eq!(results["first"], Status::Passed, "shared={shared}");
		assert_eq!(results["second"], Status::RuntimeError, "shared={shared}");
	}
}

#[test]
fn malformed_relevant_state_or_log_cannot_reuse_earlier_active_evidence() {
	let launch = launch_identities()[0].clone();
	for (event, field) in [("state", "state"), ("log", "message")] {
		for invalid in [
			None,
			Some(serde_json::Value::Null),
			Some(serde_json::json!(42)),
		] {
			let mut bad = serde_json::json!({
				"format": 1, "plugin_id": "dev.foch.directed-pacts",
				"run_id": launch, "event": event,
			});
			if let Some(value) = invalid {
				bad[field] = value;
			}
			let stream = format!("{}\n{bad}", full_stream());
			assert_eq!(
				judge_launches(&stream)["join_check"],
				Status::RuntimeError,
				"malformed {event} must invalidate prior evidence: {bad}"
			);
		}
	}
}

#[test]
fn unrelated_or_stale_envelope_versions_do_not_poison_current_receipts() {
	let launch = launch_identities()[0].clone();
	for (plugin, run) in [
		("dev.foch.other-plugin", launch.as_str()),
		("dev.foch.directed-pacts", "an-old-launch"),
	] {
		let foreign = serde_json::json!({
			"format": 2, "plugin_id": plugin, "run_id": run,
			"event": "log", "message": "FOCH_RECEIPT/2 {broken",
		});
		let stream = format!("{}\n{foreign}", full_stream());
		assert_eq!(judge_launches(&stream)["join_check"], Status::Passed);
	}
}
