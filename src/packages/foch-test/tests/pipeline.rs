//! Collect → expand → lint → group → compile → judge → report, with logs
//! written the way the generated script would make the engine write them.

use foch_test::compile::{CheckKind, PlannedCheck};
use foch_test::model::{AiMode, CaseIndex, RunId, SessionRequest, Step};
use foch_test::plan::{AloneReason, Isolation, NotRunReason};
use foch_test::report::EnvironmentRecord;
use foch_test::{
	Bundle, DiagnosticCode, ExpandOptions, GroupOptions, Plan, ProjectFacts, RelPath, Report,
	RunArtifacts, RunContext, RunnerCapabilities, RunnerExit, Selector, SourceFile, SourceKind,
	Status, Timing, check_capabilities, collect, compile, expand, group, judge, lint,
};

const EVENTS: &str = r#"namespace = reforms

#mark(reform)
#test(time=1444.11.11, tag=SWE, name=grants_flag,
#     effect={ clr_country_flag = reform_done },
#     expect={
#         has_country_flag = reform_done
#         NOT = { has_country_flag = never_set }
#     })
#xfail(reason="known bug", strict=yes)
#test(time=1444.11.11, tag=SWE, name=known_bug, advance_days=2, expect={ always = no })
#test(time=1444.11.11, tag=SWE, ai=on)
#skip(reason="needs war")
#test(time=1444.11.11, tag=SWE, name=needs_war, expect={ is_at_war = yes })
country_event = {
	id = reforms.1
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { set_country_flag = reform_done }
	option = { name = OK }
}
"#;

const FIXTURES: &str = r#"
fixture = {
	name = prepared
	use = { base }
	effect = { set_country_flag = prepared }
	check = { has_country_flag = prepared }
}
fixture = {
	name = base
	effect = { set_country_flag = base }
}
"#;

const SCENARIO: &str = r#"
test = {
	name = multi_step
	time = 1444.11.11
	tag = DAN
	ai = off
	use = { prepared }
	fire = reforms.1
	expect = { has_country_flag = reform_done }
	advance_days = 30
	effect = { clr_country_flag = reform_done }
	expect = { NOT = { has_country_flag = reform_done } has_country_flag = base }
}
test = {
	name = other_country
	time = 1444.11.11
	tag = NOR
	fire = reforms.1
	expect = { has_country_flag = reform_done }
}
"#;

fn file(path: &str, kind: SourceKind, text: &str) -> SourceFile {
	SourceFile {
		mod_name: "Reform Mod".into(),
		path: RelPath::new(path).unwrap(),
		kind,
		text: text.into(),
	}
}

fn sources() -> Vec<SourceFile> {
	vec![
		file("events/reforms.txt", SourceKind::Inline, EVENTS),
		file("tests/fixtures.txt", SourceKind::TestsDir, FIXTURES),
		file("tests/scenario.txt", SourceKind::TestsDir, SCENARIO),
	]
}

fn plan(shared: bool) -> Plan {
	let collection = collect(&sources());
	assert!(
		collection.diagnostics.is_empty(),
		"{:#?}",
		collection.diagnostics
	);
	let expanded = expand(&collection, &ExpandOptions::default()).unwrap();
	let lint = lint(&expanded, &ProjectFacts::default());
	group(
		expanded,
		&lint,
		&GroupOptions {
			runner_shared_sessions: shared,
			..GroupOptions::default()
		},
	)
}

fn context() -> RunContext {
	RunContext {
		run_id: RunId::parse("run-1").unwrap(),
		mod_name: "Reform Mod".into(),
	}
}

fn log_line(bundle: &Bundle, case: CaseIndex, date: &str, record: &str) -> String {
	format!(
		"[effectimplementation.cpp:1]: EVENT [{date}]:{} {record}\n",
		bundle.case_prefix(case)
	)
}

/// The log the engine writes when every check reports `outcome(check)`.
fn simulated_log(bundle: &Bundle, outcome: &dyn Fn(&PlannedCheck) -> bool) -> String {
	let mut log = String::from("unrelated engine output\n");
	for window in &bundle.windows {
		log += &log_line(bundle, window.case, &window.begin.to_string(), "BEGIN");
		for check in bundle
			.checks
			.iter()
			.filter(|check| check.case == window.case)
		{
			let verdict = if outcome(check) { "PASS" } else { "FAIL" };
			log += &log_line(
				bundle,
				check.case,
				&check.date.to_string(),
				&format!("{verdict} {}", check.kind.label()),
			);
		}
		log += &log_line(bundle, window.case, &window.end.to_string(), "END");
	}
	log
}

fn artifacts(log: &str) -> RunArtifacts<'_> {
	RunArtifacts {
		game_log: Some(log),
		error_log: Some(""),
		exit: RunnerExit::Success,
		timing: Timing {
			wall_ms: 60_000,
			..Timing::default()
		},
		profile_untouched: true,
	}
}

fn run(plan: &Plan, outcome: &dyn Fn(&PlannedCheck) -> bool) -> Vec<foch_test::CaseResult> {
	plan.sessions
		.iter()
		.flat_map(|session| {
			let bundle = compile(plan, session.id, &context()).unwrap();
			let log = simulated_log(&bundle, outcome);
			judge(&bundle, plan, &artifacts(&log))
		})
		.collect()
}

/// known_bug's assertion fails after two days and grants_flag's second
/// clause fails; everything else holds.
fn realistic(check: &PlannedCheck) -> bool {
	let known_bug = check.kind == (CheckKind::Expect { step: 2, clause: 0 })
		&& check.date.to_string() == "1444.11.13";
	let grants_second = check.kind == (CheckKind::Expect { step: 2, clause: 1 });
	!known_bug && !grants_second
}

fn status(results: &[foch_test::CaseResult], name: &str) -> Status {
	results
		.iter()
		.find(|result| result.node.name.to_string() == name)
		.unwrap_or_else(|| panic!("no result for {name}"))
		.status
}

#[test]
fn collects_inline_annotations_tests_dir_blocks_and_fixtures() {
	let collection = collect(&sources());
	let names: Vec<_> = collection
		.cases
		.iter()
		.map(|case| case.node.to_string())
		.collect();
	assert_eq!(
		names,
		[
			"Reform Mod::events/reforms.txt::reforms.1::grants_flag",
			"Reform Mod::events/reforms.txt::reforms.1::known_bug",
			"Reform Mod::events/reforms.txt::reforms.1::#2",
			"Reform Mod::events/reforms.txt::reforms.1::needs_war",
			"Reform Mod::tests/scenario.txt::multi_step",
			"Reform Mod::tests/scenario.txt::other_country",
		]
	);
	let grants = &collection.cases[0];
	assert!(grants.marks.labels.contains("reform"));
	let Step::Expect { clauses } = grants.steps.last().unwrap() else {
		panic!("expect is the last inline step");
	};
	assert_eq!(clauses[0].text, "has_country_flag = reform_done");
	assert_eq!(clauses[1].text, "NOT = { has_country_flag = never_set }");
	assert_eq!(
		clauses[1].span.start.line, 8,
		"clauses point at their source lines"
	);
	assert!(collection.cases[1].marks.xfail.as_ref().unwrap().strict);
	assert!(collection.cases[2].is_smoke());
	assert!(collection.cases[3].marks.skip.is_some());
	let scenario = &collection.cases[4];
	assert_eq!(scenario.ai, AiMode::Off);
	assert_eq!(scenario.steps.len(), 5);
	assert_eq!(collection.fixtures.len(), 2);
	assert_eq!(collection.events.len(), 1);
	assert!(collection.events[0].invocable);
}

#[test]
fn content_identity_ignores_marks_and_line_moves() {
	let original = collect(&sources()).cases[0].content_id.clone();
	let moved = EVENTS.replace("#mark(reform)\n", "\n\n#mark(reform)\n#ignore()\n");
	let mut files = sources();
	files[0].text = moved;
	let collection = collect(&files);
	assert_eq!(collection.cases[0].content_id, original);
	let changed = EVENTS.replace(
		"clr_country_flag = reform_done }",
		"clr_country_flag = other }",
	);
	files[0].text = changed;
	assert_ne!(collect(&files).cases[0].content_id, original);
}

#[test]
fn malformed_declarations_are_all_reported_with_positions() {
	let text = r#"
#skip(reason="orphan")
namespace = x
#test(time=1444.11.11, tag=swe)
#tset(time=1444.11.11)
#xfail()
#test(time=1444.11.11, tag=SWE, expect={ always = no })
#xfail(reason="no expectation")
#test(time=1444.11.11, tag=SWE, name=smoke_xfail)
#test(time=1444.11.11, tag=SWE, bogus=1)
country_event = {
	id = x.1
	hidden = yes
	is_triggered_only = yes
	#test(time=1444.11.11, tag=SWE)
	option = { name = OK }
}
#test(time=1444.11.11, tag=SWE)
country_event = { id = x.2 hidden = no is_triggered_only = yes option = { name = OK } }
"#;
	let collection = collect(&[file("events/x.txt", SourceKind::Inline, text)]);
	let codes: Vec<_> = collection
		.diagnostics
		.iter()
		.map(|diagnostic| {
			(
				diagnostic.code,
				diagnostic.span.as_ref().unwrap().start.line,
			)
		})
		.collect();
	assert!(
		codes.contains(&(DiagnosticCode::OrphanAnnotation, 2)),
		"{codes:?}"
	);
	assert!(
		codes.contains(&(DiagnosticCode::Annotation, 4)),
		"bad tag: {codes:?}"
	);
	assert!(
		codes.contains(&(DiagnosticCode::UnknownAnnotation, 5)),
		"{codes:?}"
	);
	assert!(
		codes.contains(&(DiagnosticCode::Annotation, 9)),
		"xfail on smoke: {codes:?}"
	);
	assert!(
		codes.contains(&(DiagnosticCode::Annotation, 10)),
		"unknown parameter: {codes:?}"
	);
	assert!(
		codes.contains(&(DiagnosticCode::NestedAnnotation, 15)),
		"{codes:?}"
	);
	assert!(
		codes.contains(&(DiagnosticCode::UnsupportedTarget, 18)),
		"visible event: {codes:?}"
	);
	assert_eq!(
		collection.cases.len(),
		1,
		"the valid xfail case is still collected"
	);
}

#[test]
fn selection_marks_and_fixture_resolution() {
	let collection = collect(&sources());
	let options = ExpandOptions {
		selector: Selector {
			marks: Some(foch_test::select::Expr::parse("reform").unwrap()),
			..Selector::default()
		},
		..ExpandOptions::default()
	};
	let expanded = expand(&collection, &options).unwrap();
	assert_eq!(expanded.cases.len(), 1);
	assert_eq!(expanded.cases[0].case.node.name.to_string(), "grants_flag");

	let expanded = expand(&collection, &ExpandOptions::default()).unwrap();
	let skipped = expanded
		.not_run
		.iter()
		.find(|not_run| matches!(not_run.reason, NotRunReason::Skipped { .. }))
		.unwrap();
	assert_eq!(skipped.node.name.to_string(), "needs_war");
	let multi = expanded
		.cases
		.iter()
		.find(|planned| planned.case.node.name.to_string() == "multi_step")
		.unwrap();
	let order: Vec<_> = multi
		.fixtures
		.iter()
		.map(|fixture| fixture.name.to_string())
		.collect();
	assert_eq!(order, ["base", "prepared"], "dependencies run first");

	let budget = ExpandOptions {
		max_cases: 2,
		..ExpandOptions::default()
	};
	let errors = expand(&collection, &budget).unwrap_err();
	assert_eq!(errors[0].code, DiagnosticCode::TooManyCases);

	let cyclic = FIXTURES.replace("name = base\n", "name = base\n\tuse = { prepared }\n");
	let mut files = sources();
	files[1].text = cyclic;
	let errors = expand(&collect(&files), &ExpandOptions::default()).unwrap_err();
	assert!(
		errors
			.iter()
			.any(|error| error.code == DiagnosticCode::FixtureCycle)
	);
}

#[test]
fn grouping_shares_only_compatible_cases_and_explains_the_rest() {
	let alone = plan(false);
	assert!(
		alone
			.sessions
			.iter()
			.all(|session| session.isolation == Isolation::Dedicated)
	);
	assert!(
		alone
			.alone_reasons
			.values()
			.all(|reason| *reason == AloneReason::RunnerUnsupported)
	);

	let shared = plan(true);
	let shared_sessions: Vec<_> = shared
		.sessions
		.iter()
		.filter(|session| session.isolation == Isolation::Shared)
		.collect();
	assert_eq!(shared_sessions.len(), 1);
	let members: Vec<_> = shared_sessions[0]
		.cases
		.iter()
		.map(|&index| shared.case(index).case.node.name.to_string())
		.collect();
	// AI is off by default, so different countries share one launch.
	assert_eq!(members, ["grants_flag", "multi_step", "other_country"]);
	let reason = |name: &str| {
		let planned = shared
			.cases
			.iter()
			.find(|planned| planned.case.node.name.to_string() == name)
			.unwrap();
		shared.alone_reasons[&planned.index].clone()
	};
	assert_eq!(reason("#2"), AloneReason::AiEnabled, "ai=on is explicit");
	assert!(matches!(reason("known_bug"), AloneReason::SameTag { .. }));

	let isolate = {
		let collection = collect(&sources());
		let expanded = expand(&collection, &ExpandOptions::default()).unwrap();
		let lint = lint(&expanded, &ProjectFacts::default());
		group(
			expanded,
			&lint,
			&GroupOptions {
				runner_shared_sessions: true,
				default_session: SessionRequest::Alone,
				..GroupOptions::default()
			},
		)
	};
	assert!(
		isolate
			.sessions
			.iter()
			.all(|session| session.cases.len() == 1)
	);
}

#[test]
fn compiled_layer_is_valid_script_with_readable_names() {
	let plan = plan(true);
	for session in &plan.sessions {
		let bundle = compile(&plan, session.id, &context()).unwrap();
		assert!(bundle.namespace.starts_with("foch_test_reform_mod_"));
		assert!(bundle.test_mod_name.starts_with("Reform Mod tests ["));
		for (path, content) in &bundle.files {
			assert!(!path.contains("..") && !path.starts_with('/'));
			if path != "commands.txt" {
				let parsed = foch::game::eu4::script::parser::parse_clausewitz_content(
					foch::model::GamePath::new(path).unwrap(),
					content,
				);
				assert!(parsed.diagnostics.is_empty(), "{path}: {content}");
			}
		}
		assert_eq!(
			compile(&plan, session.id, &context()).unwrap(),
			bundle,
			"deterministic"
		);
	}
	let shared = plan
		.sessions
		.iter()
		.find(|session| session.isolation == Isolation::Shared)
		.unwrap();
	let bundle = compile(&plan, shared.id, &context()).unwrap();
	let start = &bundle.files[&format!("{}_start.txt", bundle.namespace)];
	assert!(start.contains("DAN = {") && start.contains("NOR = {"));
	assert_eq!(bundle.launch.end_date.to_string(), "1444.12.11");
	let multi = *bundle
		.cases
		.iter()
		.find(|&&index| plan.case(index).case.node.name.to_string() == "multi_step")
		.unwrap();
	let labels: Vec<_> = bundle
		.checks
		.iter()
		.filter(|check| check.case == multi)
		.map(|check| (check.kind.label(), check.date.to_string()))
		.collect();
	assert_eq!(
		labels,
		[
			("scope".into(), "1444.11.11".into()),
			("fixture.prepared.0".into(), "1444.11.11".into()),
			("invoked.0".into(), "1444.11.11".into()),
			("expect.1.0".into(), "1444.11.11".into()),
			("expect.4.0".into(), "1444.12.11".into()),
			("expect.4.1".into(), "1444.12.11".into()),
		]
	);
	let events = &bundle.files[&format!("test_mod/events/{}.txt", bundle.namespace)];
	assert!(events.contains("days = 30"));
	assert!(
		events.find("set_country_flag = base").unwrap()
			< events.find("set_country_flag = prepared").unwrap()
	);

	let error = check_capabilities(&bundle, &RunnerCapabilities::default()).unwrap_err();
	assert_eq!(error.reasons.len(), 3, "{error}");
	check_capabilities(
		&bundle,
		&RunnerCapabilities {
			start_dates: vec![bundle.launch.start_date],
			ai_off: true,
			shared_sessions: true,
		},
	)
	.unwrap();
}

#[test]
fn judge_reports_each_status_once_in_one_place() {
	let plan = plan(true);
	let all_pass = run(&plan, &|_| true);
	assert_eq!(status(&all_pass, "grants_flag"), Status::Passed);
	assert_eq!(
		status(&all_pass, "known_bug"),
		Status::XPassed { strict: true }
	);
	assert_eq!(status(&all_pass, "#2"), Status::Smoke);
	assert_eq!(status(&all_pass, "multi_step"), Status::Passed);

	let realistic = run(&plan, &realistic);
	assert_eq!(status(&realistic, "known_bug"), Status::XFailed);
	let grants = realistic
		.iter()
		.find(|result| result.node.name.to_string() == "grants_flag")
		.unwrap();
	assert_eq!(grants.status, Status::Failed);
	assert_eq!(grants.failures.len(), 1);
	assert_eq!(grants.failures[0].check, "expect.2.1");
	assert_eq!(grants.failures[0].span.as_ref().unwrap().start.line, 8);

	let setup = run(&plan, &|check| {
		!matches!(check.kind, CheckKind::Fixture { .. })
	});
	assert_eq!(status(&setup, "multi_step"), Status::SetupError);
	assert_eq!(status(&setup, "other_country"), Status::Passed);
}

#[test]
fn runtime_problems_are_never_absorbed_by_xfail() {
	let plan = plan(false);
	let known_bug = plan
		.sessions
		.iter()
		.find(|session| plan.case(session.cases[0]).case.node.name.to_string() == "known_bug")
		.unwrap();
	let bundle = compile(&plan, known_bug.id, &context()).unwrap();
	let log = simulated_log(&bundle, &|check| {
		!matches!(check.kind, CheckKind::Expect { .. })
	});
	let judged = |artifacts: RunArtifacts| judge(&bundle, &plan, &artifacts)[0].status;
	assert_eq!(judged(artifacts(&log)), Status::XFailed);
	assert_eq!(
		judged(RunArtifacts {
			exit: RunnerExit::TimedOut { seconds: 180 },
			..artifacts(&log)
		}),
		Status::RuntimeError
	);
	assert_eq!(
		judged(RunArtifacts {
			error_log: Some("[pdx_d3d9]: device lost"),
			..artifacts(&log)
		}),
		Status::RuntimeError
	);
	assert_eq!(
		judged(RunArtifacts {
			profile_untouched: false,
			..artifacts(&log)
		}),
		Status::RuntimeError
	);
	let truncated: String = log
		.lines()
		.take(4)
		.map(|line| format!("{line}\n"))
		.collect();
	assert_eq!(judged(artifacts(&truncated)), Status::Incomplete);
}

#[test]
fn forged_or_foreign_records_are_protocol_errors() {
	let plan = plan(false);
	let session = &plan.sessions[0];
	let bundle = compile(&plan, session.id, &context()).unwrap();
	let log = simulated_log(&bundle, &|_| true);
	let judged = |bundle: &Bundle, log: &str| judge(bundle, &plan, &artifacts(log))[0].status;
	assert!(judged(&bundle, &log).is_success());

	let other_run = compile(
		&plan,
		session.id,
		&RunContext {
			run_id: RunId::parse("run-2").unwrap(),
			..context()
		},
	)
	.unwrap();
	assert_eq!(
		judged(&bundle, &simulated_log(&other_run, &|_| true)),
		Status::ProtocolError
	);
	assert_eq!(
		judged(&bundle, &(log.clone() + &log)),
		Status::ProtocolError
	);
	assert_eq!(
		judged(&bundle, &log.replace("1444.11.11", "1444.11.12")),
		Status::ProtocolError
	);
	let mut forged = bundle.clone();
	forged.checks.clear();
	assert_eq!(judged(&forged, &log), Status::ProtocolError);
}

#[test]
fn isolated_rerun_decides_and_flags_interference() {
	let plan = plan(true);
	let shared = plan
		.sessions
		.iter()
		.find(|session| session.isolation == Isolation::Shared)
		.unwrap();
	let bundle = compile(&plan, shared.id, &context()).unwrap();
	let failing = judge(
		&bundle,
		&plan,
		&artifacts(&simulated_log(&bundle, &|check| {
			!matches!(check.kind, CheckKind::Expect { .. })
		})),
	);
	assert!(failing.iter().all(|result| result.status == Status::Failed));
	let rerun = plan.isolated_rerun(&bundle.cases);
	assert!(
		rerun
			.sessions
			.iter()
			.all(|session| session.isolation == Isolation::Dedicated)
	);
	let alone = run(&rerun, &|_| true);
	let reconciled = foch_test::reconcile(&failing[0], alone[0].clone());
	assert_eq!(reconciled.status, Status::Passed);
	assert!(
		reconciled
			.notes
			.iter()
			.any(|note| note.contains("interfered"))
	);
}

#[test]
fn report_summarizes_exit_code_and_ci_formats() {
	let plan = plan(true);
	let results = run(&plan, &realistic);
	let report = Report::new(
		RunId::parse("run-1").unwrap(),
		results,
		plan.not_run.clone(),
		EnvironmentRecord::default(),
	);
	let summary = report.summary();
	assert_eq!(summary.failed, 1);
	assert_eq!(summary.xpassed, 0);
	assert_eq!(summary.xfailed, 1);
	assert_eq!(summary.skipped, 1);
	assert_eq!(
		summary.wall_ms,
		60_000 * plan.sessions.len() as u64,
		"shared timing counted once"
	);
	assert_eq!(report.exit_code(), 1);
	assert_eq!(
		report.failed_nodes().into_iter().collect::<Vec<_>>(),
		["Reform Mod::events/reforms.txt::reforms.1::grants_flag"]
	);
	let junit = report.to_junit();
	assert!(junit.contains("failures=\"1\""));
	assert!(junit.contains("<skipped message=\"needs war\"/>"));
	assert!(junit.contains("events/reforms.txt:8:"));
	let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
	let grants = json["results"]
		.as_array()
		.unwrap()
		.iter()
		.find(|result| result["node"]["name"]["named"] == "grants_flag")
		.unwrap();
	assert_eq!(grants["status"], "failed");
	assert_eq!(grants["failures"][0]["check"], "expect.2.1");
}
