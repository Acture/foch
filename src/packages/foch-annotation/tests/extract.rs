use foch_annotation::builtin::REGISTRY;
use foch_annotation::schema::{AnnotationKind, AnnotationSchema, ParamSchema, Registry};
use foch_annotation::source::RelPath;
use foch_annotation::value::{Value, ValueType};
use foch_annotation::{Code, Severity, extract};

const EVENT: &str = "country_event = {\n\tid = a.1\n\thidden = yes\n\tis_triggered_only = yes\n\toption = { name = OK }\n}\n";

fn path() -> RelPath {
	RelPath::new("events/a.txt").unwrap()
}

fn codes(text: &str) -> Vec<(Code, usize)> {
	extract(&path(), text, &REGISTRY)
		.diagnostics
		.iter()
		.map(|diagnostic| (diagnostic.code, diagnostic.span.start.line))
		.collect()
}

#[test]
fn attaches_typed_arguments_and_modifiers_to_the_target() {
	let text = format!(
		"#mark(war, slow)\n#xfail(reason=\"bug\", strict=yes)\n#test(time=1444.11.11, tag=SWE,\n#     expect={{\n#         has_country_flag = a\n#         NOT = {{ has_country_flag = b }}\n#     }})\n#test(time=1444.11.11, tag=DAN)\n{EVENT}"
	);
	let extraction = extract(&path(), &text, &REGISTRY);
	assert!(
		extraction.diagnostics.is_empty(),
		"{:#?}",
		extraction.diagnostics
	);
	let [attachment] = extraction.attachments.as_slice() else {
		panic!("one target");
	};
	assert_eq!(attachment.target.key, "country_event");
	assert_eq!(attachment.target.id.as_deref(), Some("a.1"));
	assert_eq!(attachment.annotations.len(), 2);
	let first = &attachment.annotations[0];
	let names: Vec<_> = first
		.modifiers
		.iter()
		.map(|modifier| modifier.name())
		.collect();
	assert_eq!(names, ["mark", "xfail"]);
	assert_eq!(
		first.modifiers[0]
			.positional
			.iter()
			.map(|(label, _)| label.as_str())
			.collect::<Vec<_>>(),
		["war", "slow"]
	);
	assert_eq!(first.modifiers[1].get("strict"), Some(&Value::Bool(true)));
	let Some(Value::Conditions(clauses)) = first.annotation.get("expect") else {
		panic!("expect is typed as conditions");
	};
	assert_eq!(clauses[1].text, "NOT = { has_country_flag = b }");
	assert_eq!(clauses[1].span.start.line, 6);
	assert!(
		attachment.annotations[1].modifiers.is_empty(),
		"modifiers apply to the next #test only"
	);
}

#[test]
fn every_problem_is_reported_at_its_position() {
	let text = format!(
		"#skip()\nnamespace = a\n#test(time=1444.2.29, tag=SWE)\n#test(tag=SWE, bogus=1)\n#skip()\n#skip()\n#test(time=1444.11.11, tag=SWE)\n#tset(x=1)\n{EVENT}#test(time=1444.11.11, tag=SWE)\ncountry_event = {{ id = a.2 hidden = no is_triggered_only = yes }}\nother = {{\n\t#test(time=1444.11.11, tag=SWE)\n}}\n#mark(1bad)\n#test(time=1444.11.11 tag=SWE\n"
	);
	let codes = codes(&text);
	for expected in [
		(Code::Orphan, 1),
		(Code::InvalidValue, 3),
		(Code::UnknownParameter, 4),
		(Code::DuplicateModifier, 6),
		(Code::UnknownAnnotation, 8),
		(Code::UnsupportedTarget, 15),
		(Code::Nested, 18),
		(Code::InvalidValue, 20),
		(Code::Malformed, 21),
	] {
		assert!(
			codes.contains(&expected),
			"{expected:?} missing from {codes:?}"
		);
	}
	let warnings = extract(&path(), &text, &REGISTRY)
		.diagnostics
		.into_iter()
		.filter(|diagnostic| diagnostic.severity == Severity::Warning)
		.count();
	assert_eq!(warnings, 1, "only the unknown annotation is a warning");
}

#[test]
fn a_missing_required_parameter_is_reported() {
	// The builtin #test has no unconditionally required parameter (tag and
	// time may come from #parametrize), so the required-parameter mechanism is
	// exercised here with a small synthetic registry.
	const SCHEMAS: &[AnnotationSchema] = &[AnnotationSchema {
		name: "need",
		kind: AnnotationKind::Primary,
		description: "",
		params: &[ParamSchema {
			name: "x",
			value_type: ValueType::Text,
			required: true,
			description: "",
			example: "1",
		}],
		positional: None,
		target: None,
	}];
	const REG: Registry = Registry { schemas: SCHEMAS };
	let codes: Vec<_> = extract(&path(), "#need(y=1)\nfoo = { }\n", &REG)
		.diagnostics
		.iter()
		.map(|diagnostic| diagnostic.code)
		.collect();
	assert!(codes.contains(&Code::MissingParameter), "{codes:?}");
	assert!(codes.contains(&Code::UnknownParameter), "{codes:?}");
}

#[test]
fn ordinary_comments_and_lookalikes_are_not_annotations() {
	for text in [
		"# just a note about tests\nx = 1\n",
		"# testing(1)\nx = 1\n",
		"name = \"#test(time=1444.11.11)\"\n",
		"x = 1 #test(time=1444.11.11, tag=SWE)\n",
	] {
		let extraction = extract(&path(), text, &REGISTRY);
		assert!(extraction.attachments.is_empty(), "{text}");
		assert!(
			extraction
				.diagnostics
				.iter()
				.all(|diagnostic| diagnostic.code != Code::Orphan),
			"{text}: {:?}",
			extraction.diagnostics
		);
	}
	assert_eq!(
		codes("#test\nx = 1\n"),
		[(Code::Malformed, 1)],
		"bare #test is reported"
	);
}
