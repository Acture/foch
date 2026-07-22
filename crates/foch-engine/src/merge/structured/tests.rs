use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use foch_language::analyzer::content_family::{
	BlockPatchPolicy, MergePolicies, OneSidedRemovalPolicy, ScalarMergePolicy, ScalarReducerRule,
};
use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue, parse_clausewitz_content};
use foch_merge_kernel::{ConflictKind, SemanticKeyScope};

use crate::emit::emit_clausewitz_statements;

use super::ast_adapter::{denormalize_ast, normalize_ast};
use super::policy::DefaultClausewitzTreePolicy;
use super::{merge_clausewitz_files, merge_event_files};

fn parse(source: &str) -> AstFile {
	let parsed = parse_clausewitz_content(PathBuf::from("events/test.txt"), source);
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	parsed.ast
}

fn emit(file: &AstFile) -> String {
	emit_clausewitz_statements(&file.statements).expect("emit Clausewitz AST")
}

fn repeated_block_keys_by_identity(
	file: &AstFile,
	repeated_key: &str,
) -> BTreeMap<String, BTreeSet<String>> {
	let mut result = BTreeMap::new();
	let Some(AstStatement::Assignment {
		value: AstValue::Block { items, .. },
		..
	}) = file.statements.first()
	else {
		return result;
	};
	for statement in items {
		let AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} = statement
		else {
			continue;
		};
		if key != repeated_key {
			continue;
		}
		let identity = items.iter().find_map(|item| match item {
			AstStatement::Assignment {
				key,
				value: AstValue::Scalar { value, .. },
				..
			} if key == "identity" => Some(value.as_text()),
			_ => None,
		});
		let Some(identity) = identity else {
			continue;
		};
		result.insert(
			identity,
			items
				.iter()
				.filter_map(|item| match item {
					AstStatement::Assignment { key, .. } => Some(key.clone()),
					AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
				})
				.collect(),
		);
	}
	result
}

fn scalar_items_for_definition(file: &AstFile, definition: &str, field: &str) -> Vec<String> {
	file.statements
		.iter()
		.find_map(|statement| match statement {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} if key == definition => Some(items),
			_ => None,
		})
		.and_then(|items| {
			items.iter().find_map(|statement| match statement {
				AstStatement::Assignment {
					key,
					value: AstValue::Block { items, .. },
					..
				} if key == field => Some(items),
				_ => None,
			})
		})
		.into_iter()
		.flatten()
		.filter_map(|statement| match statement {
			AstStatement::Item {
				value: AstValue::Scalar { value, .. },
				..
			} => Some(value.as_text()),
			_ => None,
		})
		.collect()
}

fn event_policies() -> MergePolicies {
	MergePolicies {
		scalar: ScalarMergePolicy::LastWriter,
		one_sided_removal: OneSidedRemovalPolicy::PreserveAdditiveStructure,
		edit_wins_over_remove: true,
		..MergePolicies::default()
	}
}

fn boolean_or_policies() -> MergePolicies {
	MergePolicies {
		block_patch: BlockPatchPolicy::BooleanOr,
		..MergePolicies::default()
	}
}

fn preserve_one_sided_policies() -> MergePolicies {
	MergePolicies {
		one_sided_removal: OneSidedRemovalPolicy::PreserveIfParentSurvives,
		..MergePolicies::default()
	}
}

#[test]
fn structured_boolean_or_flattens_and_deduplicates_disjuncts() {
	let base = parse("");
	let left = parse(
		"is_expanded_mod_active = {\n\
		\tOR = {\n\
		\t\thas_global_flag = $mod$_expanded_mod_active\n\
		\t\thas_global_flag = $mod$_expaned_mod_active\n\
		\t}\n\
		}\n",
	);
	let right = parse(
		"is_expanded_mod_active = {\n\
		\thas_global_flag = $mod$_expanded_mod_active\n\
		}\n",
	);

	let outcome = merge_clausewitz_files(&base, &left, &right, &boolean_or_policies())
		.expect("merge BooleanOr definition");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert_eq!(output.matches("OR = {").count(), 1, "{output}");
	assert_eq!(
		output
			.matches("has_global_flag = $mod$_expanded_mod_active")
			.count(),
		1,
		"{output}"
	);
	assert_eq!(
		output
			.matches("has_global_flag = $mod$_expaned_mod_active")
			.count(),
		1,
		"{output}"
	);
}

#[test]
fn structured_merge_matches_reordered_repeated_blocks_by_content() {
	let base = parse(
		"institution = {\n\
		\tmodifier = { identity = a base_a = yes }\n\
		\tmodifier = { identity = b base_b = yes }\n\
		}\n",
	);
	let left = parse(
		"institution = {\n\
		\tmodifier = { identity = a base_a = yes left_a = yes }\n\
		\tmodifier = { identity = b base_b = yes left_b = yes }\n\
		}\n",
	);
	let right = parse(
		"institution = {\n\
		\tmodifier = { identity = b base_b = yes right_b = yes }\n\
		\tmodifier = { identity = a base_a = yes right_a = yes }\n\
		}\n",
	);

	let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default())
		.expect("merge repeated blocks");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let keys = repeated_block_keys_by_identity(
		outcome.resolved_ast().expect("conflict-free AST"),
		"modifier",
	);
	assert_eq!(
		keys.get("a"),
		Some(&BTreeSet::from([
			"identity".to_string(),
			"base_a".to_string(),
			"left_a".to_string(),
			"right_a".to_string(),
		]))
	);
	assert_eq!(
		keys.get("b"),
		Some(&BTreeSet::from([
			"identity".to_string(),
			"base_b".to_string(),
			"left_b".to_string(),
			"right_b".to_string(),
		]))
	);
}

#[test]
fn structured_merge_matches_repeated_modifiers_by_tooltip_identity() {
	let base = parse(
		"manufactories = { embracement_speed = {
			modifier = { potential = { OR = { trade_goods = coffee } } custom_trigger_tooltip = { tooltip = plantations_on } }
			modifier = { potential = { OR = { trade_goods = spices trade_goods = cloves } } custom_trigger_tooltip = { tooltip = tradecompany_on } }
			modifier = { potential = { OR = { trade_goods = cocoa } } custom_trigger_tooltip = { tooltip = plantations_off } }
			modifier = { potential = { OR = { trade_goods = ivory trade_goods = cloves } } custom_trigger_tooltip = { tooltip = tradecompany_off } }
		} }\n",
	);
	let left = parse(
		"manufactories = { embracement_speed = {
			modifier = { potential = { OR = { trade_goods = coffee } } custom_trigger_tooltip = { tooltip = plantations_on } }
			modifier = { potential = { OR = { trade_goods = spices trade_goods = cloves trade_goods = incense } } custom_trigger_tooltip = { tooltip = tradecompany_on } }
			modifier = { potential = { OR = { trade_goods = cocoa } } custom_trigger_tooltip = { tooltip = plantations_off } }
			modifier = { potential = { OR = { trade_goods = ivory trade_goods = cloves trade_goods = incense } } custom_trigger_tooltip = { tooltip = tradecompany_off } }
		} }\n",
	);
	let right = parse(
		"manufactories = { embracement_speed = {
			modifier = { potential = { OR = { trade_goods = ivory trade_goods = fur } } custom_trigger_tooltip = { tooltip = tradecompany_off } }
			modifier = { potential = { OR = { trade_goods = cocoa trade_goods = cloves } } custom_trigger_tooltip = { tooltip = plantations_off } }
			modifier = { potential = { OR = { trade_goods = spices trade_goods = cloves trade_goods = fur } } custom_trigger_tooltip = { tooltip = tradecompany_on } }
			modifier = { potential = { OR = { trade_goods = coffee trade_goods = cloves } } custom_trigger_tooltip = { tooltip = plantations_on } }
		} }\n",
	);
	let policies = MergePolicies {
		one_sided_removal: OneSidedRemovalPolicy::PreserveBooleanAlternatives,
		..MergePolicies::default()
	};

	let outcome = merge_clausewitz_files(&base, &left, &right, &policies)
		.expect("merge tooltip-identified modifiers");
	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("publishable modifier merge"));
	for trade_good in ["incense", "fur"] {
		assert!(
			output.contains(&format!("trade_goods = {trade_good}")),
			"{output}"
		);
	}
	assert_eq!(
		output.matches("trade_goods = cloves").count(),
		4,
		"{output}"
	);
}

#[test]
fn structured_merge_keeps_numeric_tuple_items_with_their_parent() {
	let base = parse(
		"rebel_a = { color = { 1 2 3 } }\n\
		rebel_b = { color = { 1 2 3 } }\n",
	);
	let left = parse(
		"rebel_a = { color = { 10 2 3 } }\n\
		rebel_b = { color = { 1 2 3 } }\n",
	);
	let right = parse(
		"rebel_a = { color = { 1 2 3 } }\n\
		rebel_b = { color = { 1 20 3 } }\n",
	);

	let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default())
		.expect("merge numeric tuples");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let ast = outcome.resolved_ast().expect("conflict-free AST");
	assert_eq!(
		scalar_items_for_definition(ast, "rebel_a", "color"),
		vec!["10", "2", "3"]
	);
	assert_eq!(
		scalar_items_for_definition(ast, "rebel_b", "color"),
		vec!["1", "20", "3"]
	);
}

#[test]
fn structured_merge_does_not_match_tuple_items_across_distinct_insertions() {
	let base = parse("");
	let left = parse("ita_rebels = { color = { 1 2 3 } }\n");
	let right = parse("fee_ita_rebels = { color = { 1 2 3 } }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default())
		.expect("merge independent tuple-bearing definitions");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let ast = outcome.resolved_ast().expect("conflict-free AST");
	assert_eq!(
		scalar_items_for_definition(ast, "ita_rebels", "color"),
		vec!["1", "2", "3"]
	);
	assert_eq!(
		scalar_items_for_definition(ast, "fee_ita_rebels", "color"),
		vec!["1", "2", "3"]
	);
}

#[test]
fn structured_preserve_policy_keeps_unchanged_child_when_parent_survives() {
	let base = parse("building = { cost = 100 sailors = 1 }\n");
	let left = parse("building = { cost = 100 }\n");
	let right = parse("building = { cost = 100 sailors = 1 tax = 1 }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &preserve_one_sided_policies())
		.expect("merge one-sided omission");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(output.contains("sailors = 1"), "{output}");
	assert!(output.contains("tax = 1"), "{output}");
}

#[test]
fn structured_preserve_policy_keeps_unchanged_repeated_block_among_insertions() {
	let base = parse(
		"gems = {\n\
		\tchance = {\n\
		\t\tmodifier = { factor = 2.0 FROM = { has_country_flag = encourage_cash_crops_flag } }\n\
		\t\tmodifier = { factor = 2 FROM = { OR = { has_increased_trade_goods_discovery = { trade_goods = gems } colonial_parent = { has_increased_trade_goods_discovery = { trade_goods = gems } } } } }\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"gems = {\n\
		\tchance = {\n\
		\t\tmodifier = { factor = 2.0 FROM = { has_country_flag = encourage_cash_crops_flag } }\n\
		\t}\n\
		}\n",
	);
	let right = parse(
		"gems = {\n\
		\tchance = {\n\
		\t\tmodifier = { factor = 2.0 FROM = { has_country_flag = encourage_cash_crops_flag } }\n\
		\t\tmodifier = { factor = 2 FROM = { OR = { has_increased_trade_goods_discovery = { trade_goods = gems } colonial_parent = { has_increased_trade_goods_discovery = { trade_goods = gems } } } } }\n\
		\t\tmodifier = { factor = 1.2 FROM = { has_country_flag = gems_chance_flag_low } }\n\
		\t\tmodifier = { factor = 1.4 FROM = { has_country_flag = gems_chance_flag_medium } }\n\
		\t\tmodifier = { factor = 1.6 FROM = { has_country_flag = gems_chance_flag_high } }\n\
		\t}\n\
		}\n",
	);

	let outcome = merge_clausewitz_files(&base, &left, &right, &preserve_one_sided_policies())
		.expect("merge one-sided repeated block omission");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(
		output.contains("has_increased_trade_goods_discovery"),
		"{output}"
	);
	assert!(output.contains("gems_chance_flag_low"), "{output}");
	assert!(output.contains("gems_chance_flag_medium"), "{output}");
	assert!(output.contains("gems_chance_flag_high"), "{output}");
}

#[test]
fn structured_boolean_alternative_policy_preserves_one_sided_or_members() {
	let base = parse(
		"institution = {\n\
		\tembracement_speed = {\n\
		\t\tmodifier = {\n\
		\t\t\tfactor = 0.2\n\
		\t\t\tpotential = { OR = { trade_goods = ivory trade_goods = cloves } }\n\
		\t\t\tcustom_trigger_tooltip = { tooltip = tradecompany has_building = tradecompany }\n\
		\t\t}\n\
		\t}\n\
		}\n",
	);
	let left = base.clone();
	let right = parse(
		"institution = {\n\
		\tembracement_speed = {\n\
		\t\tmodifier = {\n\
		\t\t\tfactor = 0.2\n\
		\t\t\tpotential = { OR = { trade_goods = ivory trade_goods = fur } }\n\
		\t\t\tcustom_trigger_tooltip = { tooltip = tradecompany has_building = tradecompany }\n\
		\t\t}\n\
		\t}\n\
		}\n",
	);
	let policies = MergePolicies {
		one_sided_removal: OneSidedRemovalPolicy::PreserveBooleanAlternatives,
		..MergePolicies::default()
	};

	let outcome = merge_clausewitz_files(&base, &left, &right, &policies)
		.expect("merge additive Boolean predicate deletion");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(output.contains("trade_goods = cloves"), "{output}");
	assert!(output.contains("trade_goods = fur"), "{output}");
}

#[test]
fn structured_default_policy_keeps_delete_wins_semantics() {
	let base = parse("building = { cost = 100 sailors = 1 }\n");
	let left = parse("building = { cost = 100 }\n");
	let right = parse("building = { cost = 100 sailors = 1 tax = 1 }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default())
		.expect("merge one-sided omission conservatively");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(!output.contains("sailors = 1"), "{output}");
	assert!(output.contains("tax = 1"), "{output}");
}

#[test]
fn structured_preserve_policy_still_honors_two_sided_deletion() {
	let base = parse("building = { cost = 100 sailors = 1 }\n");
	let left = parse("building = { cost = 100 }\n");
	let right = left.clone();

	let outcome = merge_clausewitz_files(&base, &left, &right, &preserve_one_sided_policies())
		.expect("merge two-sided deletion");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(!output.contains("sailors = 1"), "{output}");
}

#[test]
fn structured_merge_preserves_orphan_control_flow_but_withholds_publication() {
	let source = parse(
		"scripted_effect = {\n\
		\telse_if = { limit = { always = yes } add_prestige = 1 }\n\
		}\n",
	);

	let outcome = merge_clausewitz_files(&source, &source, &source, &MergePolicies::default())
		.expect("retain orphan control flow as structured AST");

	assert!(outcome.resolved_ast().is_none());
	assert!(
		outcome.conflicts().iter().any(|conflict| {
			conflict.kind == ConflictKind::Policy
				&& conflict
					.detail
					.contains("control-flow finding(s) require review")
				&& conflict.detail.contains("else_if")
		}),
		"{:?}",
		outcome.conflicts()
	);
	assert!(emit(outcome.tentative_ast()).contains("else_if ="));
}

#[test]
fn structured_preserve_policy_does_not_hide_delete_modify_conflict() {
	let base = parse("building = { cost = 100 sailors = 1 }\n");
	let left = parse("building = { cost = 100 }\n");
	let right = parse("building = { cost = 100 sailors = 2 }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &preserve_one_sided_policies())
		.expect("merge delete against modification");

	assert!(
		outcome
			.conflicts()
			.iter()
			.any(|conflict| conflict.kind == ConflictKind::DeleteModify),
		"{:?}",
		outcome.conflicts()
	);
}

#[test]
fn generic_clausewitz_merge_combines_independent_definition_edits() {
	let base = parse("temple = { cost = 100 }\n");
	let left = parse("temple = { cost = 100 manpower = 1 }\n");
	let right = parse("temple = { cost = 100 tax = 1 }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default())
		.expect("merge generic Clausewitz definitions");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(output.contains("manpower = 1"), "{output}");
	assert!(output.contains("tax = 1"), "{output}");
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
	let normalized = normalize_ast(&ast, &DefaultClausewitzTreePolicy).expect("normalize AST");
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
	let tree = normalize_ast(&ast, &DefaultClausewitzTreePolicy).expect("normalize AST");
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
	let tree = normalize_ast(&ast, &DefaultClausewitzTreePolicy).expect("normalize AST");
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

#[test]
fn structured_merge_applies_path_scoped_numeric_reducers_with_provenance() {
	const RULES: &[ScalarReducerRule] = &[
		ScalarReducerRule::new(&["global_colonial_growth"], ScalarMergePolicy::Max),
		ScalarReducerRule::new(&["province_trade_power_modifier"], ScalarMergePolicy::Avg),
	];
	let policies = MergePolicies {
		scalar_reducer_rules: RULES,
		..MergePolicies::default()
	};
	let base =
		parse("cloves = { global_colonial_growth = .05 province_trade_power_modifier = .05 }\n");
	let left =
		parse("cloves = { global_colonial_growth = .2 province_trade_power_modifier = .2 }\n");
	let right =
		parse("cloves = { global_colonial_growth = .1 province_trade_power_modifier = .1 }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &policies).unwrap();

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("numeric reducers resolve"));
	assert!(output.contains("global_colonial_growth = .2"), "{output}");
	assert!(
		output.contains("province_trade_power_modifier = .15"),
		"{output}"
	);
	let reductions = outcome.scalar_reductions();
	assert_eq!(reductions.len(), 2);
	assert!(reductions.iter().any(|reduction| {
		reduction
			.path
			.ends_with(&["cloves".to_string(), "global_colonial_growth".to_string()])
			&& reduction.output == ".2"
			&& reduction.inputs.len() == 2
	}));
	assert!(reductions.iter().any(|reduction| {
		reduction.path.ends_with(&[
			"cloves".to_string(),
			"province_trade_power_modifier".to_string(),
		]) && reduction.output == ".15"
	}));
}

#[test]
fn structured_merge_keeps_unruled_numeric_divergence_as_a_conflict() {
	const RULES: &[ScalarReducerRule] = &[ScalarReducerRule::new(
		&["province_trade_power_modifier"],
		ScalarMergePolicy::Avg,
	)];
	let policies = MergePolicies {
		scalar_reducer_rules: RULES,
		..MergePolicies::default()
	};
	let base = parse("cloves = { technology = 1 }\n");
	let left = parse("cloves = { technology = 2 }\n");
	let right = parse("cloves = { technology = 3 }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &policies).unwrap();

	assert!(outcome.resolved_ast().is_none());
	assert!(outcome.scalar_reductions().is_empty());
}

#[test]
fn structured_merge_preserves_distinct_comments_without_semantic_conflicts() {
	let base = parse("# base\nvalue = { amount = 1 }\n");
	let left = parse("# base\n# left\nvalue = { amount = 1 left = yes }\n");
	let right = parse("# base\n# right\nvalue = { amount = 1 right = yes }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default()).unwrap();

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(
		outcome
			.resolved_ast()
			.expect("comment-only divergence resolves"),
	);
	for comment in ["# base", "# left", "# right"] {
		assert!(output.contains(comment), "missing {comment}:\n{output}");
	}
}
