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
