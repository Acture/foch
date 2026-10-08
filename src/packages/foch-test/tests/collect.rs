//! Collection of inline `#test` annotations, carried over from the first
//! framework's regression suite.

use foch_test::model::{CaseName, Step, TestCase};
use foch_test::{Collection, RelPath, SourceFile, SourceKind, collect};

fn event(annotation: &str) -> String {
	format!(
		"namespace = demo\n{annotation}\ncountry_event = {{\n id = demo.1\n hidden = yes\n is_triggered_only = yes\n immediate = {{ set_country_flag = tested }}\n option = {{ name = OK }}\n}}\n"
	)
}

fn collected(source: &str) -> Collection {
	collect(&[SourceFile {
		mod_name: "demo".into(),
		path: RelPath::new("events/demo.txt").unwrap(),
		kind: SourceKind::Inline,
		text: source.into(),
	}])
}

fn expect_text(case: &TestCase) -> Vec<&str> {
	case.steps
		.iter()
		.flat_map(|step| match step {
			Step::Expect { clauses } => clauses.iter().map(|clause| clause.text.as_str()).collect(),
			_ => Vec::new(),
		})
		.collect()
}

#[test]
fn collects_smoke_and_preserves_original_event_and_location() {
	let source = event("#test(time=1444.11.11, tag=SWE)");
	let result = collected(&source);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let [case] = result.cases.as_slice() else {
		panic!("one case");
	};
	assert!(case.is_smoke());
	assert_eq!(case.node.target.as_ref().unwrap().as_str(), "demo.1");
	assert_eq!(case.node.name, CaseName::Index(0));
	assert_eq!(case.start.date.to_string(), "1444.11.11");
	assert_eq!(case.start.tag.as_str(), "SWE");
	assert_eq!(case.origin.start.line, 2);
	assert_eq!(case.origin.start.column, 1);
	let parsed = foch::game::eu4::script::parser::parse_clausewitz_content(
		foch::model::GamePath::new("events/demo.txt").unwrap(),
		&source,
	);
	assert!(
		parsed.diagnostics.is_empty(),
		"annotated file stays valid script"
	);
}

#[test]
fn parses_multiline_arguments_without_splitting_nested_quoted_values() {
	let source = event(
		"#test(\n# time=1444.11.11, tag=SWE, name=delayed, advance_days=2,\n# effect={ log = \"value,)#test(\" set_country_flag = ready },\n# expect={ has_country_flag = ready NOT = { always = no } }\n#)",
	);
	let result = collected(&source);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let case = &result.cases[0];
	assert_eq!(case.node.name, CaseName::Named("delayed".into()));
	let Step::Effect { block } = &case.steps[0] else {
		panic!("effect first");
	};
	assert!(block.text.contains("value,)#test("));
	assert!(matches!(case.steps[2], Step::AdvanceDays { days: 2, .. }));
	assert_eq!(
		expect_text(case),
		["has_country_flag = ready", "NOT = { always = no }"]
	);
	assert_eq!(case.origin.end.line, 6);
}

#[test]
fn ignores_lookalikes_and_rejects_orphan_or_nested_annotations() {
	let result = collected("text = \"#test(time=1444.11.11, tag=SWE)\"\n#testing(example)\n");
	assert!(result.cases.is_empty());
	assert!(!result.has_errors());
	for source in [
		"#test(time=1444.11.11, tag=SWE)",
		"#test(time=1444.11.11, tag=SWE)\nnamespace = demo",
		"country_event = {\n#test(time=1444.11.11, tag=SWE)\n id = demo.1\n}",
	] {
		let result = collected(source);
		assert!(result.cases.is_empty());
		assert!(result.has_errors(), "{source}");
	}
	// An end-of-line comment is not an annotation, so nothing is collected.
	assert!(
		collected("namespace = demo #test(time=1444.11.11, tag=SWE)")
			.cases
			.is_empty()
	);
}

#[test]
fn rejects_bad_arguments_and_reports_original_annotation_line() {
	for annotation in [
		"#test(tag=SWE)",
		"#test(time=1444.11.11)",
		"#test(time=1444.2.30, tag=SWE)",
		"#test(time=1444.11.11, tag=sweden)",
		"#test(time=1444.11.11, tag=SWE, tag=FRA)",
		"#test(time=1444.11.11, tag=SWE, unknown=yes)",
		"#test(time=1444.11.11, tag=SWE, advance_days=-1)",
		"#test(time=1444.11.11, tag=SWE, expect=yes)",
		"#test(time=1444.11.11, tag=SWE, expect={})",
		"#test(time=1444.11.11, tag=SWE) trailing",
		"#test(time=1444.11.11, tag=SWE",
		"#test(time=1444.11.11, tag=SWE, ai=maybe)",
	] {
		let result = collected(&event(annotation));
		assert!(result.cases.is_empty(), "{annotation}");
		assert!(result.has_errors(), "{annotation}");
		assert_eq!(
			result.diagnostics[0].span.as_ref().unwrap().start.line,
			2,
			"{annotation}"
		);
	}
}

#[test]
fn rejects_unsupported_targets_and_duplicate_names() {
	let original = event("#test(time=1444.11.11, tag=SWE)");
	for source in [
		original.replace("country_event", "province_event"),
		original.replace("hidden = yes", "hidden = no"),
		original.replace("is_triggered_only = yes", "is_triggered_only = no"),
		original.replace("id = demo.1", "id = \"bad\\\"id\""),
		original.replace("id = demo.1", "id = demo.bad"),
	] {
		let result = collected(&source);
		assert!(result.cases.is_empty());
		assert!(result.has_errors(), "{source}");
	}
	let duplicate =
		event("#test(time=1444.11.11, tag=SWE, name=a)\n#test(time=1444.11.11, tag=DAN, name=a)");
	assert!(collected(&duplicate).has_errors());
	// Unnamed cases are addressed by position, like pytest's generated IDs.
	let unnamed = collected(&event(
		"#test(time=1444.11.11, tag=SWE)\n#test(time=1444.11.11, tag=SWE)",
	));
	assert!(unnamed.diagnostics.is_empty(), "{:?}", unnamed.diagnostics);
	assert_ne!(unnamed.cases[0].node, unnamed.cases[1].node);
}

#[test]
fn normalizes_calendar_dates_and_rejects_leap_day_and_oversized_delays() {
	let result = collected(&event("#test(time=1444.01.02, tag=SWE)"));
	assert!(result.diagnostics.is_empty());
	assert_eq!(result.cases[0].start.date.to_string(), "1444.1.2");
	for annotation in [
		"#test(time=1444.2.29, tag=SWE)",
		"#test(time=1444.11.11, tag=SWE, advance_days=36501)",
	] {
		assert!(collected(&event(annotation)).has_errors(), "{annotation}");
	}
}

#[test]
fn closing_a_multiline_annotation_does_not_consume_the_following_annotation() {
	let result = collected(&event(
		"#test(\n# time=1444.11.11, tag=SWE, name=first\n#)\n# ordinary comment\n#test(time=1444.11.11, tag=SWE, name=second)",
	));
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	assert_eq!(result.cases.len(), 2);
	assert_eq!(result.cases[1].origin.start.line, 6);
}

#[test]
fn accepts_human_readable_case_names_without_using_them_as_paths() {
	let result = collected(&event(
		"#test(time=1444.11.11, tag=SWE, name=\"改革 flag check\")",
	));
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	assert_eq!(
		result.cases[0].node.name,
		CaseName::Named("改革 flag check".into())
	);
}

#[test]
fn example_fixture_collects_its_four_cases() {
	let text = include_str!("fixtures/runtime-tests/events/annotated.txt");
	let result = collect(&[SourceFile {
		mod_name: "example".into(),
		path: RelPath::new("events/annotated.txt").unwrap(),
		kind: SourceKind::Inline,
		text: text.into(),
	}]);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let names: Vec<_> = result
		.cases
		.iter()
		.map(|case| case.node.name.to_string())
		.collect();
	assert_eq!(
		names,
		[
			"smoke",
			"flag_is_set",
			"delayed_flag",
			"intentional_failure"
		]
	);
}
