use std::path::PathBuf;

use foch_language::analyzer::content_family::{
	MergePolicies, OneSidedRemovalPolicy, ScalarMergePolicy,
};
use foch_language::analyzer::parser::{AstFile, parse_clausewitz_content};
use foch_merge_kernel::{ConflictKind, SemanticKeyScope};

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
		one_sided_removal: OneSidedRemovalPolicy::PreserveIfParentSurvives,
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
fn event_option_and_control_flow_use_their_intended_identity_scope() {
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
		.map(|anchor| (anchor.value.as_str(), &anchor.scope))
		.collect::<Vec<_>>();
	let if_nodes = tree
		.nodes()
		.filter(|(_, node)| {
			node.kind
				.starts_with("clausewitz.control_flow.guarded_branch:")
		})
		.map(|(_, node)| node)
		.collect::<Vec<_>>();
	let control_chains = tree
		.nodes()
		.filter(|(_, node)| node.kind.starts_with("clausewitz.control_flow.chain:"))
		.map(|(_, node)| node)
		.collect::<Vec<_>>();

	assert_eq!(
		anchors,
		vec![
			("country_event:demo.1", &SemanticKeyScope::Global),
			("option:demo.accept", &SemanticKeyScope::Parent),
		]
	);
	assert_eq!(if_nodes.len(), 2);
	assert!(if_nodes.iter().all(|node| {
		node.anchor
			.as_ref()
			.is_some_and(|anchor| anchor.scope == SemanticKeyScope::Parent)
	}));
	assert!(if_nodes.iter().all(|node| node.signature.is_some()));
	assert_eq!(
		control_chains.len(),
		2,
		"adjacent ifs are independent chains"
	);
	assert!(control_chains.iter().all(|node| {
		node.anchor
			.as_ref()
			.is_some_and(|anchor| anchor.scope == SemanticKeyScope::Parent)
	}));
}

#[test]
fn event_adapter_groups_else_branches_and_comments_into_one_chain() {
	let ast = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\toption = {\n\
		\t\tname = demo.accept\n\
		\t\tif = { limit = { has_country_flag = first } add_prestige = 1 }\n\
		\t\t# branch note\n\
		\t\telse_if = { limit = { has_country_flag = second } add_stability = 1 }\n\
		\t\telse = { add_legitimacy = 1 }\n\
		\t}\n\
		}\n",
	);
	let tree = normalize_ast(&ast, &EventTreePolicy).expect("normalize AST");
	let chains = tree
		.nodes()
		.filter(|(_, node)| node.kind.starts_with("clausewitz.control_flow.chain:"))
		.map(|(_, node)| node)
		.collect::<Vec<_>>();

	assert_eq!(chains.len(), 1);
	assert!(chains[0].signature.is_some());
	assert_eq!(
		chains[0]
			.children
			.iter()
			.filter_map(|child| {
				let node = tree.node(*child).unwrap();
				if node
					.kind
					.starts_with("clausewitz.control_flow.guarded_branch:")
				{
					Some("guarded")
				} else if node.kind == "clausewitz.control_flow.else_branch" {
					Some("else")
				} else if node.kind == "clausewitz.comment" {
					None
				} else {
					node.value.as_deref()
				}
			})
			.collect::<Vec<_>>(),
		vec!["guarded", "guarded", "else"]
	);
	let rebuilt = denormalize_ast(ast.path.clone(), &tree).expect("rebuild AST");
	assert_eq!(emit(&rebuilt), emit(&ast));
}

#[test]
fn event_merge_recognizes_an_if_demoted_by_a_new_leading_branch() {
	let base = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\tif = { limit = { has_country_flag = old } add_prestige = 1 }\n\
		\telse = { add_stability = -1 }\n\
		}\n",
	);
	let left = base.clone();
	let right = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\tif = { limit = { has_country_flag = new } add_legitimacy = 1 }\n\
		\telse_if = { limit = { has_country_flag = old } add_prestige = 1 }\n\
		\telse = { add_stability = -1 }\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge branch insertion and demotion");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	assert_eq!(emit(outcome.resolved_ast().unwrap()), emit(&right));
}

#[test]
fn event_merge_treats_guard_signatures_as_soft_correspondence() {
	let base = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\tif = { limit = { always = yes } add_prestige = 1 }\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\tif = { limit = { always = yes has_country_flag = from_left } add_prestige = 1 }\n\
		}\n",
	);
	let right = parse(
		"country_event = {\n\
		\tid = demo.1\n\
		\tif = { limit = { always = yes has_ruler_flag = from_right } add_prestige = 1 }\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge disjoint guard edits");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(output.contains("has_country_flag = from_left"));
	assert!(output.contains("has_ruler_flag = from_right"));
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
fn event_merge_edit_wins_does_not_restore_unchanged_deleted_descendants() {
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
fn event_merge_combines_hooks_boolean_replacements_and_union_safe_chains() {
	let base = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\timmediate = { hidden_effect = { pre_select_possible_ruler_focus = yes } }\n\
		\tdesc = {\n\
		\t\ttrigger = { NOT = { has_government_attribute = has_dutch_election } }\n\
		\t\tdesc = elections.720.db\n\
		\t}\n\
		\toption = {\n\
		\t\tname = elections.720.a\n\
		\t\tif = {\n\
		\t\t\tlimit = { has_government_attribute = republican_virtues }\n\
		\t\t\tdefine_ruler = { change_adm = 1 change_dip = 1 change_mil = 1 }\n\
		\t\t}\n\
		\t\telse = { define_ruler = {} }\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\tdesc = { trigger = { NOT = { has_reform = dutch_republic } } desc = elections.720.db }\n\
		\toption = {\n\
		\t\tname = elections.720.a\n\
		\t\tif = {\n\
		\t\t\tlimit = { has_country_flag = NED_upgrade_statist_candidate_1 }\n\
		\t\t\tdefine_ruler = { change_mil = 1 }\n\
		\t\t}\n\
		\t\telse = { define_ruler = {} }\n\
		\t}\n\
		}\n",
	);
	let right = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\timmediate = {\n\
		\t\thidden_effect = {\n\
		\t\t\tpre_select_possible_ruler_focus = yes\n\
		\t\t\tpre_election_set_factional_veche = yes\n\
		\t\t}\n\
		\t}\n\
		\tdesc = {\n\
		\t\ttrigger = {\n\
		\t\t\tNOT = {\n\
		\t\t\t\thas_government_attribute = has_dutch_election\n\
		\t\t\t\thas_reform = crown_of_saint_wenceslaus\n\
		\t\t\t}\n\
		\t\t}\n\
		\t\tdesc = elections.720.db\n\
		\t}\n\
		\toption = {\n\
		\t\tname = elections.720.a\n\
		\t\tif = {\n\
		\t\t\tlimit = { has_government_attribute = republican_virtues }\n\
		\t\t\tdefine_ruler = { change_adm = 1 change_dip = 1 change_mil = 1 }\n\
		\t\t}\n\
		\t\telse = { define_ruler = {} }\n\
		\t}\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge one-sided omissions");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	for retained in [
		"pre_select_possible_ruler_focus = yes",
		"pre_election_set_factional_veche = yes",
		"has_government_attribute = has_dutch_election",
		"has_reform = crown_of_saint_wenceslaus",
		"has_government_attribute = republican_virtues",
		"has_country_flag = NED_upgrade_statist_candidate_1",
		"change_adm = 1",
		"change_dip = 1",
		"change_mil = 1",
	] {
		assert!(output.contains(retained), "missing `{retained}`:\n{output}");
	}
	assert!(!output.contains("has_reform = dutch_republic"), "{output}");
	assert_eq!(
		output.matches("\t\telse = {").count(),
		1,
		"only one empty constructor fallback should remain:\n{output}"
	);
}

#[test]
fn event_merge_does_not_union_exclusive_constructor_chains() {
	let base = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\toption = {\n\
		\t\tname = elections.720.a\n\
		\t\tif = {\n\
		\t\t\tlimit = { has_country_flag = original_candidate }\n\
		\t\t\tdefine_ruler = { dynasty = original_dynasty }\n\
		\t\t}\n\
		\t\telse = { define_ruler = { dynasty = original_fallback } }\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\toption = {\n\
		\t\tname = elections.720.a\n\
		\t\tif = {\n\
		\t\t\tlimit = { has_country_flag = replacement_candidate }\n\
		\t\t\tdefine_ruler = { dynasty = replacement_dynasty }\n\
		\t\t}\n\
		\t\telse = { define_ruler = { dynasty = replacement_fallback } }\n\
		\t}\n\
		}\n",
	);
	let right = base.clone();

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge exclusive constructor replacement");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(output.contains("replacement_candidate"), "{output}");
	assert!(output.contains("replacement_dynasty"), "{output}");
	assert!(output.contains("replacement_fallback"), "{output}");
	assert!(!output.contains("original_candidate"), "{output}");
	assert!(!output.contains("original_dynasty"), "{output}");
	assert!(!output.contains("original_fallback"), "{output}");
}

#[test]
fn event_merge_combines_presence_and_last_writer_policies() {
	let base = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\tdesc = { trigger = { NOT = { has_government_attribute = has_dutch_election } } desc = elections.720.db }\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\tdesc = { trigger = { NOT = { has_reform = dutch_republic } } desc = elections.720.db }\n\
		}\n",
	);
	let right = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\tdesc = { trigger = { NOT = { has_government_attribute = has_dutch_election has_reform = crown_of_saint_wenceslaus } } desc = elections.720.db }\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge divergent inserted scalar");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(output.contains("has_government_attribute = has_dutch_election"));
	assert!(output.contains("has_reform = crown_of_saint_wenceslaus"));
	assert!(!output.contains("has_reform = dutch_republic"), "{output}");
}
