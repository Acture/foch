use foch_test::model::RunId;
use foch_test::{
	ExpandOptions, GroupOptions, ProjectFacts, RelPath, RunContext, SourceFile, SourceKind,
	collect, compile, expand, group, lint,
};

#[test]
fn native_war_fixtures_compile_as_isolated_foch_sessions_with_real_assertions() {
	let path =
		std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/runtime/events/pacts.txt");
	let text = std::fs::read_to_string(path).unwrap();
	let source = SourceFile {
		mod_name: "Directed Pact Fixtures".into(),
		path: RelPath::new("events/pacts.txt").unwrap(),
		kind: SourceKind::Inline,
		text,
	};
	let collected = collect(&[source]);
	assert!(!collected.has_errors(), "{:?}", collected.diagnostics);
	assert_eq!(collected.cases.len(), 8);
	let expanded = expand(&collected, &ExpandOptions::default()).unwrap();
	let linted = lint(&expanded, &ProjectFacts::default());
	let plan = group(expanded, &linted, &GroupOptions::default());
	assert_eq!(plan.sessions.len(), 8);
	let context = RunContext {
		run_id: RunId::parse("directed-pacts-fixture").unwrap(),
		mod_name: "Directed Pact Fixtures".into(),
	};
	for session in &plan.sessions {
		let bundle = compile(&plan, session.id, &context).unwrap();
		assert!(
			bundle
				.checks
				.iter()
				.any(|check| matches!(check.kind, foch_test::compile::CheckKind::Expect { .. }))
		);
		assert!(!bundle.requires.shared_session);
		assert!(
			bundle
				.files
				.values()
				.any(|text| text.contains("FOCH_TEST_V2"))
		);
	}
}

#[test]
fn read_probe_fixture_preserves_both_war_assertions_and_the_game_flag() {
	let source = SourceFile {
		mod_name: "Directed Pact Read Probe".into(),
		path: RelPath::new("tests/probe.txt").unwrap(),
		kind: SourceKind::TestsDir,
		text: std::fs::read_to_string(
			std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/probe/tests/probe.txt"),
		)
		.unwrap(),
	};
	let event = SourceFile {
		mod_name: source.mod_name.clone(),
		path: RelPath::new("events/rosters.txt").unwrap(),
		kind: SourceKind::Inline,
		text: std::fs::read_to_string(
			std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
				.join("fixtures/probe/events/rosters.txt"),
		)
		.unwrap(),
	};
	let collected = collect(&[source, event]);
	assert!(!collected.has_errors(), "{:?}", collected.diagnostics);
	assert_eq!(collected.cases.len(), 1);
	let receipts: Vec<_> = collected.cases[0]
		.steps
		.iter()
		.filter_map(|step| {
			if let foch_test::model::Step::ExpectReceipt { plugin, fields, .. } = step {
				assert_eq!(plugin, foch_plugin_directed_pacts::ROSTER_PROBE_PLUGIN_ID);
				let get = |key| {
					fields
						.iter()
						.find(|(field, _)| field == key)
						.unwrap()
						.1
						.clone()
				};
				assert_eq!(get("case"), "two_rosters");
				assert_eq!(get("status"), "observed");
				assert_eq!(get("native_calls"), "0");
				Some((get("request"), get("type")))
			} else {
				None
			}
		})
		.collect();
	assert_eq!(
		receipts,
		vec![
			("1".into(), "war_roster".into()),
			("2".into(), "war_roster".into()),
			("3".into(), "country_flag".into())
		]
	);
	assert!(
		collected.cases[0]
			.steps
			.iter()
			.any(|step| matches!(step, foch_test::model::Step::AdvanceDays { days: 35, .. }))
	);
	let expanded = expand(&collected, &ExpandOptions::default()).unwrap();
	let linted = lint(&expanded, &ProjectFacts::default());
	let plan = group(expanded, &linted, &GroupOptions::default());
	assert_eq!(plan.sessions.len(), 1);
	let bundle = compile(
		&plan,
		plan.sessions[0].id,
		&RunContext {
			run_id: RunId::parse("pact-read-probe").unwrap(),
			mod_name: "Directed Pact Read Probe".into(),
		},
	)
	.unwrap();
	assert!(!bundle.requires.shared_session);
	assert_eq!(
		bundle
			.checks
			.iter()
			.filter(|check| matches!(check.kind, foch_test::compile::CheckKind::Expect { .. }))
			.count(),
		3
	);
	assert!(
		bundle
			.files
			.values()
			.any(|text| text.contains("has_country_flag = foch_dp_roster_flag"))
	);
	assert!(
		bundle
			.files
			.values()
			.all(|text| !text.contains("expect_receipt"))
	);
}
