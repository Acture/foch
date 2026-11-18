//! Differential calibration of the offline layer against the recorded
//! in-game evidence for the runtime-tests fixture.
//!
//! The fixture's four cases ran against real EU4 1.37.5 on 2026-10-02
//! (see `src/apps/foch-cli/tests/probes/eu4-runtime/foch-runtime-tests-2026-10-02.json`):
//! smoke, flag_is_set and delayed_flag passed; intentional_failure failed.
//! When the offline model interprets a case, its verdict must agree. This is
//! the seed of the calibration corpus, not a proof that offline results
//! predict the game in general.

use foch_test::model::RunId;
use foch_test::source::{RelPath, SourceFile, SourceKind};
use foch_test::{
	ExpandOptions, GroupOptions, Outcome, ProjectFacts, SemanticsCatalog, collect, expand, group,
	interpret, lint,
};

const FIXTURE: &str = include_str!("fixtures/runtime-tests/events/annotated.txt");

fn interpret_fixture() -> foch_test::Interpretation {
	let sources = [SourceFile {
		mod_name: "foch-framework-example".into(),
		path: RelPath::new("events/annotated.txt").unwrap(),
		kind: SourceKind::Inline,
		text: FIXTURE.into(),
	}];
	let collection = collect(&sources);
	assert!(
		collection.diagnostics.is_empty(),
		"{:#?}",
		collection.diagnostics
	);
	let expanded = expand(&collection, &ExpandOptions::default()).unwrap();
	let linted = lint(&expanded, &ProjectFacts::default());
	let plan = group(
		expanded,
		&linted,
		&GroupOptions {
			runner_shared_sessions: false,
			..GroupOptions::default()
		},
	);
	interpret(
		&plan,
		&ProjectFacts::default(),
		&SemanticsCatalog::embedded(),
		&foch_test::InterpretOptions {
			run_id: RunId::parse("run-1").unwrap(),
			mod_name: "foch-framework-example".into(),
			observe: false,
		},
	)
}

fn outcome_of(result: &foch_test::Interpretation, name: &str) -> Outcome {
	result
		.results
		.iter()
		.find(|case| case.node.name.to_string().contains(name))
		.unwrap_or_else(|| panic!("no case named {name}"))
		.outcome
}

#[test]
fn offline_verdicts_agree_with_the_recorded_game_run() {
	let result = interpret_fixture();
	// Every case of this fixture is interpretable: it only uses flags.
	assert_eq!(result.summary().unsupported, 0, "{:#?}", result.results);
	assert_eq!(outcome_of(&result, "smoke"), Outcome::Smoke);
	assert_eq!(outcome_of(&result, "flag_is_set"), Outcome::Pass);
	// Flags are tick-insensitive, so the delayed read is decidable offline.
	assert_eq!(outcome_of(&result, "delayed_flag"), Outcome::Pass);
	// The intentional failure fails for the same reason it failed in game.
	assert_eq!(outcome_of(&result, "intentional_failure"), Outcome::Fail);
	assert_eq!(result.exit_code(), 1);
}

#[test]
fn the_failure_points_at_the_failing_clause() {
	let result = interpret_fixture();
	let failure = result
		.results
		.iter()
		.find(|case| case.node.name.to_string().contains("intentional_failure"))
		.unwrap();
	let failure = &failure.failures[0];
	assert_eq!(failure.check, "expect.1.0");
	assert_eq!(failure.message, "always = no");
	assert_eq!(failure.span.path.as_str(), "events/annotated.txt");
}

/// The second calibration set: the same sources the online pipeline tests
/// fabricate engine logs for (`tests/pipeline.rs`). The offline verdicts
/// must agree with those asserted online semantics: fixtures prepare in
/// dependency order, inline fires, delayed reads, cross-country scopes and
/// multi-step cleanup.
const EVENTS: &str = r#"namespace = reforms

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

fn interpret_corpus() -> foch_test::Interpretation {
	let sources = [
		SourceFile {
			mod_name: "Reform Mod".into(),
			path: RelPath::new("events/reforms.txt").unwrap(),
			kind: SourceKind::Inline,
			text: EVENTS.into(),
		},
		SourceFile {
			mod_name: "Reform Mod".into(),
			path: RelPath::new("tests/fixtures.txt").unwrap(),
			kind: SourceKind::TestsDir,
			text: FIXTURES.into(),
		},
		SourceFile {
			mod_name: "Reform Mod".into(),
			path: RelPath::new("tests/scenario.txt").unwrap(),
			kind: SourceKind::TestsDir,
			text: SCENARIO.into(),
		},
	];
	let collection = collect(&sources);
	assert!(
		collection.diagnostics.is_empty(),
		"{:#?}",
		collection.diagnostics
	);
	let expanded = expand(&collection, &ExpandOptions::default()).unwrap();
	let linted = lint(&expanded, &ProjectFacts::default());
	let plan = group(
		expanded,
		&linted,
		&GroupOptions {
			runner_shared_sessions: false,
			..GroupOptions::default()
		},
	);
	interpret(
		&plan,
		&ProjectFacts::default(),
		&SemanticsCatalog::embedded(),
		&foch_test::InterpretOptions {
			run_id: RunId::parse("run-1").unwrap(),
			mod_name: "Reform Mod".into(),
			observe: false,
		},
	)
}

#[test]
fn offline_verdicts_agree_with_the_online_pipeline_semantics() {
	let result = interpret_corpus();
	// The inline case passes exactly as its fabricated engine log asserts.
	assert_eq!(outcome_of(&result, "grants_flag"), Outcome::Pass);
	// xfail makes no offline claim: the raw modeled verdict is a failure.
	let known_bug = result
		.results
		.iter()
		.find(|case| case.node.name.to_string().contains("known_bug"))
		.unwrap();
	assert_eq!(known_bug.outcome, Outcome::Fail);
	assert!(
		known_bug.notes.iter().any(|note| note.contains("xfail")),
		"{:#?}",
		known_bug.notes
	);
	// ai = on is a runner concern the model cannot see: the case interprets
	// with a note, and nothing in this corpus is refused.
	let unsupported = result
		.results
		.iter()
		.filter(|case| case.outcome == Outcome::Unsupported)
		.count();
	assert_eq!(unsupported, 0, "{:#?}", result.results);
	let ai_case = result
		.results
		.iter()
		.find(|case| case.node.name.to_string().starts_with('#'))
		.expect("the unnamed ai=on case collects");
	assert_eq!(ai_case.outcome, Outcome::Smoke);
	assert!(
		ai_case.notes.iter().any(|note| note.contains("ai = on")),
		"{:#?}",
		ai_case.notes
	);
	// The skipped case passes through untouched.
	assert_eq!(result.summary().skipped, 1);
	// The multi-step tests/ block: fixture order, inline fire, delayed
	// cleanup and a second country's own flags.
	assert_eq!(outcome_of(&result, "multi_step"), Outcome::Pass);
	assert_eq!(outcome_of(&result, "other_country"), Outcome::Pass);
}
