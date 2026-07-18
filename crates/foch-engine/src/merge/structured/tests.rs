use std::path::PathBuf;

use foch_language::analyzer::content_family::{MergePolicies, ScalarMergePolicy};
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

fn event_policies() -> MergePolicies {
	MergePolicies {
		scalar: ScalarMergePolicy::LastWriter,
		edit_wins_over_remove: true,
		..MergePolicies::default()
	}
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

	let outcome =
		merge_event_files(&base, &left, &right, &event_policies()).expect("merge event files");

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
fn event_merge_preserves_the_assignment_value_slot_when_blocks_diverge() {
	let base = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\ttrigger = { base_only = yes }\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\ttrigger = { left_only = yes }\n\
		}\n",
	);
	let right = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\ttrigger = { right_only = yes }\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge divergent value blocks");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	assert_eq!(
		emit(outcome.resolved_ast().expect("conflict-free AST")),
		"country_event = {\n\
		\tid = demo.1\n\
		\ttrigger = {\n\
		\t\tleft_only = yes\n\
		\t\tright_only = yes\n\
		\t}\n\
		}\n"
	);
}

#[test]
fn event_merge_applies_a_one_sided_assignment_value_type_replacement() {
	let base = parse("country_event = { id = demo.1 payload = old }\n");
	let left = parse("country_event = { id = demo.1 payload = { replacement = yes } }\n");
	let right = base.clone();

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge value type replacement");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	assert_eq!(
		emit(outcome.resolved_ast().expect("conflict-free AST")),
		"country_event = {\n\
		\tid = demo.1\n\
		\tpayload = {\n\
		\t\treplacement = yes\n\
		\t}\n\
		}\n"
	);
}

#[test]
fn event_merge_reports_divergent_assignment_value_type_replacements() {
	let base = parse("country_event = { id = demo.1 payload = old }\n");
	let left = parse("country_event = { id = demo.1 payload = { replacement = yes } }\n");
	let right = parse("country_event = { id = demo.1 payload = 1 }\n");

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge conflicting replacements");

	assert!(
		outcome
			.conflicts()
			.iter()
			.any(|conflict| conflict.kind == ConflictKind::ValueSlot),
		"{:?}",
		outcome.conflicts()
	);
	assert!(outcome.resolved_ast().is_none());
	assert_eq!(
		emit(outcome.tentative_ast()),
		"country_event = {\n\
		\tid = demo.1\n\
		\tpayload = 1\n\
		}\n"
	);
}

#[test]
fn event_merge_never_exposes_a_conflicted_tree_as_resolved() {
	let base = parse("country_event = { id = demo.1 title = old }\n");
	let left = parse("country_event = { id = demo.1 title = left }\n");
	let right = parse("country_event = { id = demo.1 title = right }\n");

	let outcome = merge_event_files(&base, &left, &right, &MergePolicies::default())
		.expect("merge event files");

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

#[test]
fn event_merge_uses_edit_wins_for_delete_modify() {
	let base = parse(
		"country_event = {\n\
		\tid = 700\n\
		\timmediate = { hidden_effect = { old = yes } }\n\
		}\n",
	);
	let left = parse("country_event = { id = 700 }\n");
	let right = parse(
		"country_event = {\n\
		\tid = 700\n\
		\timmediate = { hidden_effect = { old = yes added = yes } }\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge edit against deletion");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	assert_eq!(
		emit(outcome.resolved_ast().expect("conflict-free AST")),
		"country_event = {\n\
		\tid = 700\n\
		\timmediate = {\n\
		\t\thidden_effect = {\n\
		\t\t\tadded = yes\n\
		\t\t}\n\
		\t}\n\
		}\n"
	);
}

#[test]
fn event_merge_uses_last_writer_for_divergent_inserted_scalar() {
	let base = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\tdesc = { trigger = { NOT = { has_government_attribute = has_dutch_election } } }\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\tdesc = { trigger = { NOT = { has_reform = dutch_republic } } }\n\
		}\n",
	);
	let right = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\tdesc = { trigger = { NOT = { has_government_attribute = has_dutch_election has_reform = crown_of_saint_wenceslaus } } }\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge divergent inserted scalar");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(!output.contains("has_government_attribute = has_dutch_election"));
	assert!(output.contains("has_reform = crown_of_saint_wenceslaus"));
	assert!(!output.contains("has_reform = dutch_republic"));
}
