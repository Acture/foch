use std::path::PathBuf;

use foch_language::analyzer::parser::{AstFile, parse_clausewitz_content};
use foch_merge_kernel::ConflictKind;

use crate::emit::emit_clausewitz_statements;

use super::ast_adapter::{denormalize_ast, normalize_ast};
use super::merge_event_files;
use super::policy::EventTreePolicy;

fn parse(source: &str) -> AstFile {
	let parsed = parse_clausewitz_content(PathBuf::from("events/test.txt"), source);
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	parsed.ast
}

fn emit(file: &AstFile) -> String {
	emit_clausewitz_statements(&file.statements).expect("emit Clausewitz AST")
}

#[test]
fn event_adapter_round_trips_ast_content_and_scalar_variants() {
	let ast = parse(
		"# retained comment\n\
		namespace = demo\n\
		country_event = {\n\
		\tid = demo.1\n\
		\thidden = yes\n\
		\ttitle = \"demo.title\"\n\
		\tweight = 1.25\n\
		\toption = { name = demo.accept }\n\
		}\n",
	);
	let normalized = normalize_ast(&ast, &EventTreePolicy).expect("normalize AST");
	let rebuilt = denormalize_ast(ast.path.clone(), &normalized).expect("rebuild AST");

	assert_eq!(emit(&rebuilt), emit(&ast));
}

#[test]
fn event_and_option_use_semantic_identity_but_control_flow_does_not() {
	let ast = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\tif = { limit = { always = yes } }\n\
		\tif = { limit = { always = no } }\n\
		\toption = { name = demo.accept }\n\
		}\n",
	);
	let tree = normalize_ast(&ast, &EventTreePolicy).expect("normalize AST");
	let anchors = tree
		.nodes()
		.filter_map(|(_, node)| node.anchor.as_ref())
		.filter(|anchor| anchor.namespace == "clausewitz.assignment.identity")
		.map(|anchor| anchor.value.as_str())
		.collect::<Vec<_>>();
	let if_nodes = tree
		.nodes()
		.filter(|(_, node)| node.value.as_deref() == Some("if"))
		.map(|(_, node)| node)
		.collect::<Vec<_>>();

	assert_eq!(anchors, vec!["country_event:demo.1", "option:demo.accept"]);
	assert_eq!(if_nodes.len(), 2);
	assert!(if_nodes.iter().all(|node| node.anchor.is_none()));
}

#[test]
fn event_merge_amalgamates_independent_ordered_insertions() {
	let base = parse(
		"namespace = demo\n\
		country_event = {\n\
		\tid = demo.1\n\
		\ttitle = demo.title\n\
		\toption = {\n\
		\t\tname = demo.accept\n\
		\t\tadd_prestige = 1\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"namespace = demo\n\
		country_event = {\n\
		\tid = demo.1\n\
		\ttitle = demo.title\n\
		\ttrigger = { has_country_flag = from_left }\n\
		\toption = {\n\
		\t\tname = demo.accept\n\
		\t\tadd_prestige = 1\n\
		\t}\n\
		}\n",
	);
	let right = parse(
		"namespace = demo\n\
		country_event = {\n\
		\tid = demo.1\n\
		\ttitle = demo.title\n\
		\toption = {\n\
		\t\tname = demo.accept\n\
		\t\tadd_prestige = 1\n\
		\t}\n\
		\toption = {\n\
		\t\tname = demo.reject\n\
		\t\tadd_stability = -1\n\
		\t}\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right).expect("merge event files");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	assert_eq!(
		emit(outcome.resolved_ast().expect("conflict-free AST")),
		"namespace = demo\n\
		country_event = {\n\
		\tid = demo.1\n\
		\ttitle = demo.title\n\
		\ttrigger = {\n\
		\t\thas_country_flag = from_left\n\
		\t}\n\
		\toption = {\n\
		\t\tname = demo.accept\n\
		\t\tadd_prestige = 1\n\
		\t}\n\
		\toption = {\n\
		\t\tname = demo.reject\n\
		\t\tadd_stability = -1\n\
		\t}\n\
		}\n"
	);
	assert!(!outcome.kernel().provenance.is_empty());
}

#[test]
fn event_merge_never_exposes_a_conflicted_tree_as_resolved() {
	let base = parse("country_event = { id = demo.1 title = old }\n");
	let left = parse("country_event = { id = demo.1 title = left }\n");
	let right = parse("country_event = { id = demo.1 title = right }\n");

	let outcome = merge_event_files(&base, &left, &right).expect("merge event files");

	assert!(
		outcome
			.conflicts()
			.iter()
			.any(|conflict| conflict.kind == ConflictKind::Policy),
		"{:#?}",
		outcome.conflicts()
	);
	assert!(outcome.resolved_ast().is_none());
	assert!(!outcome.tentative_ast().statements.is_empty());
}
