use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use foch::merge::kernel::{ConflictKind, SemanticKeyScope};
use foch_language::analyzer::content_family::{
	BlockMergePolicy, DivergentBlockPolicy, GameProfile, MergePolicies, OneSidedRemovalPolicy,
	ScalarMergePolicy, ScalarReducerRule,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue, parse_clausewitz_content};

use crate::emit::emit_clausewitz_statements;

use super::ast_adapter::{denormalize_ast, normalize_ast};
use super::policy::ContentFamilyMergePolicy;
use super::{
	ClausewitzScalarReduction, merge_clausewitz_files, merge_clausewitz_files_n_way,
	merge_event_files,
};

fn parse(source: &str) -> AstFile {
	let parsed = parse_clausewitz_content(PathBuf::from("events/test.txt"), source);
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	parsed.ast
}

fn parse_at(path: &str, source: &str) -> AstFile {
	let parsed = parse_clausewitz_content(PathBuf::from(path), source);
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
		divergent_block: DivergentBlockPolicy::BooleanOr,
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
fn structured_union_keeps_distinct_scalar_assignments() {
	const UNION_RULES: &[(&str, DivergentBlockPolicy)] =
		&[("monarch_names", DivergentBlockPolicy::Union)];
	let policies = MergePolicies {
		divergent_block_rules: UNION_RULES,
		..MergePolicies::default()
	};
	let base = parse("");
	let left = parse("monarch_names = \"Aldus\"\n");
	let middle = parse("monarch_names = \"Berta\"\n");
	let right = parse("monarch_names = \"Cedric\"\n");

	let outcome = merge_clausewitz_files_n_way(&base, &[&left, &middle, &right], &policies)
		.expect("merge scalar union");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	for monarch_name in ["Aldus", "Berta", "Cedric"] {
		assert_eq!(
			output
				.matches(&format!("monarch_names = \"{monarch_name}\""))
				.count(),
			1,
			"{output}"
		);
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
fn structured_trigger_containers_or_complete_revision_predicates() {
	let base = parse_at(
		"common/ages/00_default.txt",
		"age_of_reformation = {\n\
		\tobjectives = {\n\
		\t\tobj_colonial_empire = {\n\
		\t\t\tcalc_true_if = {\n\
		\t\t\t\tamount = 5\n\
		\t\t\t\tall_subject_country = { is_colonial_nation = yes }\n\
		\t\t\t}\n\
		\t\t}\n\
		\t}\n\
		}\n",
	);
	let left = parse_at(
		"common/ages/00_default.txt",
		"age_of_reformation = {\n\
		\tobjectives = {\n\
		\t\tobj_colonial_empire = {\n\
		\t\t\tif = {\n\
		\t\t\t\tlimit = { is_expanded_mod_active = { mod = subjects } }\n\
		\t\t\t\tcalc_true_if = {\n\
		\t\t\t\t\tamount = 5\n\
		\t\t\t\t\tall_subject_country = { is_colonial_nation = yes }\n\
		\t\t\t\t}\n\
		\t\t\t}\n\
		\t\t\telse = { colony = 5 }\n\
		\t\t}\n\
		\t}\n\
		}\n",
	);
	let right = parse_at(
		"common/ages/00_default.txt",
		"age_of_reformation = {\n\
		\tobjectives = {\n\
		\t\tobj_colonial_empire = { colony = 5 }\n\
		\t}\n\
		}\n",
	);

	let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default()).unwrap();

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("trigger merge resolves"));
	assert!(
		output.contains("obj_colonial_empire = {\n\t\t\tOR = {"),
		"{output}"
	);
	assert!(output.contains("AND = {"), "{output}");
	assert!(output.contains("is_expanded_mod_active = {"), "{output}");
	assert!(output.contains("calc_true_if = {"), "{output}");
	assert!(!output.contains("\tif = {"), "{output}");
	assert!(!output.contains("\telse = {"), "{output}");
	assert_eq!(output.matches("colony = 5").count(), 1, "{output}");
}

#[test]
fn structured_trigger_simplifier_preserves_open_conditionals() {
	let base = parse_at(
		"common/ages/00_default.txt",
		"age = { objectives = { objective = { always = yes } } }\n",
	);
	let left = parse_at(
		"common/ages/00_default.txt",
		"age = {\n\
		\tobjectives = {\n\
		\t\tobjective = {\n\
		\t\t\tif = {\n\
		\t\t\t\tlimit = { has_dlc = demo }\n\
		\t\t\t\thas_country_flag = left\n\
		\t\t\t}\n\
		\t\t}\n\
		\t}\n\
		}\n",
	);
	let right = parse_at(
		"common/ages/00_default.txt",
		"age = { objectives = { objective = { colony = 5 } } }\n",
	);

	let outcome = merge_clausewitz_files(&base, &left, &right, &MergePolicies::default()).unwrap();

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("trigger merge resolves"));
	assert!(output.contains("OR = {"), "{output}");
	assert!(output.contains("\t\t\t\tif = {"), "{output}");
	assert!(output.contains("colony = 5"), "{output}");
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
fn scalar_control_flow_named_assignment_remains_ordinary_data() {
	let base = parse(
		"scripted_effect = {\n\
		\tadd_age_modifier = {\n\
		\t\tname = early_modifier\n\
		\t\telse = \"add_prestige = 50\"\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"scripted_effect = {\n\
		\tadd_age_modifier = {\n\
		\t\tname = early_modifier\n\
		\t\telse = \"add_prestige = 50\"\n\
		\t}\n\
		\tset_country_flag = left_marker\n\
		}\n",
	);

	let outcome = merge_clausewitz_files(&base, &left, &base, &MergePolicies::default())
		.expect("merge scalar assignment named like control flow");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("conflict-free AST"));
	assert!(output.contains("else = \"add_prestige = 50\""), "{output}");
	assert!(
		output.contains("set_country_flag = left_marker"),
		"{output}"
	);
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
	let policies = MergePolicies::default();
	let policy = ContentFamilyMergePolicy::new(&policies);
	let normalized = normalize_ast(&ast, &policy).expect("normalize AST");
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
	let policies = MergePolicies::default();
	let policy = ContentFamilyMergePolicy::new(&policies);
	let tree = normalize_ast(&ast, &policy).expect("normalize AST");
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
	let policies = MergePolicies::default();
	let policy = ContentFamilyMergePolicy::new(&policies);
	let tree = normalize_ast(&ast, &policy).expect("normalize AST");
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
		\t\tOR = {\n\
		\t\t\tleft_only = yes\n\
		\t\t\tright_only = yes\n\
		\t\t}\n\
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
fn event_merge_keeps_distinct_complete_constructor_chains_independent() {
	let base = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\toption = {\n\
		\t\tname = elections.720.a\n\
		\t\tif = { limit = { has_government_attribute = republican_virtues } define_ruler = { change_adm = 1 change_dip = 1 change_mil = 1 } }\n\
		\t\telse = { define_ruler = {} }\n\
		\t\tif = { limit = { has_country_flag = obsolete_bonus } add_estate_loyalty = 5 }\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = elections.720\n\
		\toption = {\n\
		\t\tname = elections.720.a\n\
		\t\tif = { limit = { has_country_flag = upgraded_candidate } define_ruler = { change_mil = 1 } }\n\
		\t\telse = { define_ruler = {} }\n\
		\t\tif = { limit = { has_saved_event_target = spread_target } add_province_modifier = support }\n\
		\t}\n\
		}\n",
	);
	let right = base.clone();

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge distinct complete constructor chains");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let resolved = outcome.resolved_ast().expect("publishable event AST");
	let option_items = resolved
		.statements
		.iter()
		.find_map(|statement| match statement {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} if key == "country_event" => items.iter().find_map(|item| match item {
				AstStatement::Assignment {
					key,
					value: AstValue::Block { items, .. },
					..
				} if key == "option" => Some(items),
				_ => None,
			}),
			_ => None,
		})
		.expect("merged event retains its option");
	let control_flow = option_items
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment { key, .. }
				if matches!(key.as_str(), "if" | "else_if" | "else") =>
			{
				Some(key.as_str())
			}
			_ => None,
		})
		.collect::<Vec<_>>();
	assert_eq!(control_flow, ["if", "else", "if", "if"]);
	let output = emit(resolved);
	for retained in ["republican_virtues", "upgraded_candidate", "spread_target"] {
		assert!(output.contains(retained), "missing `{retained}`:\n{output}");
	}
	assert!(!output.contains("obsolete_bonus"), "{output}");
}

#[test]
fn event_merge_matches_replaced_open_chains_by_sequence_context() {
	let base = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_government_attribute = republican_virtues } define_ruler = { change_adm = 1 } }\n\
		\t\telse = { define_ruler = {} }\n\
		\t\tif = { limit = { has_country_flag = old_candidate_flag } add_estate_loyalty = 5 }\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tge_marker = yes\n\
		\t\tif = { limit = { has_government_attribute = republican_virtues } define_ruler = { change_adm = 1 } }\n\
		\t\telse = { define_ruler = {} }\n\
		\t\tif = { limit = { has_country_flag = old_candidate_flag } add_estate_loyalty = 5 }\n\
		\t}\n\
		}\n",
	);
	let right = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_country_flag = upgraded_candidate_flag } define_ruler = { change_mil = 1 } }\n\
		\t\telse = { define_ruler = {} }\n\
		\t\tif = { limit = { has_saved_event_target = spread_target } add_province_modifier = support }\n\
		\t}\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge corresponding open-chain sequence slots");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("publishable event AST"));
	for retained in [
		"ge_marker = yes",
		"has_government_attribute = republican_virtues",
		"has_country_flag = upgraded_candidate_flag",
		"has_saved_event_target = spread_target",
	] {
		assert!(output.contains(retained), "missing `{retained}`:\n{output}");
	}
	assert!(!output.contains("old_candidate_flag"), "{output}");
}

#[test]
fn event_merge_aligns_open_chains_after_an_insertion() {
	let base = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_country_flag = candidate_a } add_prestige = 1 }\n\
		\t\tif = { limit = { has_country_flag = candidate_b } add_legitimacy = 1 }\n\
		\t\tif = { limit = { has_country_flag = candidate_c } add_stability = 1 }\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_country_flag = candidate_x } add_treasury = 10 }\n\
		\t\tif = { limit = { has_country_flag = candidate_a } add_prestige = 1 }\n\
		\t\tif = { limit = { has_country_flag = candidate_b } add_legitimacy = 1 }\n\
		\t\tif = { limit = { has_country_flag = candidate_c } add_stability = 1 }\n\
		\t}\n\
		}\n",
	);
	let right = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_country_flag = candidate_a } add_prestige = 1 }\n\
		\t\tif = { limit = { has_country_flag = candidate_b } add_legitimacy = 2 }\n\
		\t\tif = { limit = { has_country_flag = candidate_c } add_stability = 1 }\n\
		\t}\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("merge insertion-shifted open chains");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("publishable event AST"));
	for retained in [
		"has_country_flag = candidate_x",
		"add_treasury = 10",
		"has_country_flag = candidate_a",
		"has_country_flag = candidate_b",
		"add_legitimacy = 2",
		"has_country_flag = candidate_c",
	] {
		assert!(output.contains(retained), "missing `{retained}`:\n{output}");
	}
	assert!(!output.contains("add_legitimacy = 1"), "{output}");
}

#[test]
fn event_merge_keeps_open_chain_insertions_around_a_closed_chain() {
	let base = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_country_flag = ruler_candidate } define_ruler = { adm = 4 } }\n\
		\t\telse = { define_ruler = { adm = 3 } }\n\
		\t\tif = { limit = { has_country_flag = shared_effect } add_prestige = 1 }\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_reform = left_only } add_adm_power = 50 }\n\
		\t\tif = { limit = { has_country_flag = ruler_candidate } define_ruler = { adm = 4 } }\n\
		\t\telse = { define_ruler = { adm = 3 } }\n\
		\t\tif = { limit = { has_country_flag = shared_effect } add_prestige = 1 }\n\
		\t}\n\
		}\n",
	);
	let right = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_country_flag = ruler_candidate } define_ruler = { adm = 4 } }\n\
		\t\telse = { define_ruler = { adm = 3 } }\n\
		\t\tif = { limit = { has_government_attribute = right_only } define_advisor = { skill = 2 } }\n\
		\t\tif = { limit = { has_country_flag = shared_effect } add_prestige = 1 }\n\
		\t}\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("retain independent insertions on both sides of a closed chain");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("publishable event AST"));
	let left_position = output.find("left_only").expect("retain left insertion");
	let ruler_position = output
		.find("ruler_candidate")
		.expect("retain closed ruler chain");
	let right_position = output.find("right_only").expect("retain right insertion");
	assert!(
		left_position < ruler_position && ruler_position < right_position,
		"independent insertions must remain on their respective sides:\n{output}"
	);
	assert!(output.contains("shared_effect"), "{output}");
}

#[test]
fn event_merge_honors_one_sided_trailing_chain_deletions() {
	let base = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_country_flag = ruler_candidate } define_ruler = { adm = 4 } }\n\
		\t\telse = { define_ruler = { adm = 3 } }\n\
		\t\tif = { limit = { has_country_flag = shared_effect } add_prestige = 1 }\n\
		\t\tif = { limit = { has_government_attribute = has_limited_terms } set_variable = election_term }\n\
		\t\tif = { limit = { has_government_attribute = has_candidate_bonus } assign_ruler_focus = adm }\n\
		\t}\n\
		}\n",
	);
	let left = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_reform = left_only } add_adm_power = 50 }\n\
		\t\tif = { limit = { has_country_flag = ruler_candidate } define_ruler = { adm = 4 } }\n\
		\t\telse = { define_ruler = { adm = 3 } }\n\
		\t\tif = { limit = { has_country_flag = shared_effect } add_prestige = 1 }\n\
		\t}\n\
		}\n",
	);
	let right = parse(
		"country_event = {\n\
		\tid = test.1\n\
		\toption = {\n\
		\t\tname = test.1.a\n\
		\t\tif = { limit = { has_country_flag = ruler_candidate } define_ruler = { adm = 4 } }\n\
		\t\telse = { define_ruler = { adm = 3 } }\n\
		\t\tif = { limit = { has_government_attribute = right_only } define_advisor = { skill = 2 } }\n\
		\t\tif = { limit = { has_country_flag = shared_effect } add_prestige = 1 }\n\
		\t\tif = { limit = { has_government_attribute = has_limited_terms } set_variable = election_term }\n\
		\t\tif = { limit = { has_government_attribute = has_candidate_bonus } assign_ruler_focus = adm }\n\
		\t}\n\
		}\n",
	);

	let outcome = merge_event_files(&base, &left, &right, &event_policies())
		.expect("honor one-sided deletion outside an insertion gap");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("publishable event AST"));
	for retained in [
		"left_only",
		"ruler_candidate",
		"right_only",
		"shared_effect",
	] {
		assert!(output.contains(retained), "missing `{retained}`:\n{output}");
	}
	assert!(!output.contains("has_limited_terms"), "{output}");
	assert!(!output.contains("has_candidate_bonus"), "{output}");
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
fn structured_merge_applies_the_content_family_numeric_reducer() {
	let policies = MergePolicies {
		scalar: ScalarMergePolicy::Sum,
		..MergePolicies::default()
	};
	let base = parse("estate = { loyalty = 10 }\n");
	let left = parse("estate = { loyalty = 15 }\n");
	let right = parse("estate = { loyalty = 20 }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &policies).unwrap();

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("numeric reducer resolves"));
	assert!(output.contains("loyalty = 35"), "{output}");
}

#[test]
fn structured_merge_applies_atomic_block_replacement_policy() {
	let policies = MergePolicies {
		block: BlockMergePolicy::Replace,
		..MergePolicies::default()
	};
	let base = parse("node = { value = 1 base_only = yes }\n");
	let left = parse("node = { value = 2 left_only = yes }\n");
	let right = parse("node = { value = 3 right_only = yes }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &policies).unwrap();

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("replacement resolves"));
	assert!(output.contains("value = 3"), "{output}");
	assert!(output.contains("right_only = yes"), "{output}");
	assert!(!output.contains("left_only"), "{output}");
	assert!(!output.contains("base_only"), "{output}");
}

#[test]
fn structured_union_blocks_match_children_by_content() {
	let policies = MergePolicies {
		divergent_block: DivergentBlockPolicy::Union,
		..MergePolicies::default()
	};
	let base = parse("names = { tag = Base }\n");
	let left = parse("names = { tag = Base tag = Left }\n");
	let right = parse("names = { tag = Base tag = Right }\n");

	let outcome = merge_clausewitz_files(&base, &left, &right, &policies).unwrap();

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("union resolves"));
	for value in ["Base", "Left", "Right"] {
		assert_eq!(
			output.matches(&format!("tag = {value}")).count(),
			1,
			"{output}"
		);
	}
}

#[test]
fn eu4_ages_reducer_retains_the_stronger_value_against_a_one_sided_change() {
	let source = |value: &str| {
		parse_at(
			"common/ages/00_default.txt",
			&format!(
				"age_of_discovery = {{\n\
				\tabilities = {{\n\
				\t\tab_portugal_colonial_growth = {{\n\
				\t\t\tmodifier = {{ global_colonial_growth = {value} }}\n\
				\t\t}}\n\
				\t}}\n\
				}}\n"
			),
		)
	};
	let base = source("50");
	let left = source("50");
	let right = source("35");
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from("common/ages/00_default.txt").as_path())
		.expect("ages descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies).unwrap();

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("numeric reducer resolves"));
	assert!(output.contains("global_colonial_growth = 50"), "{output}");
	assert!(!output.contains("global_colonial_growth = 35"), "{output}");
	assert_eq!(
		outcome.scalar_reductions(),
		vec![ClausewitzScalarReduction {
			path: vec![
				"age_of_discovery".to_string(),
				"abilities".to_string(),
				"ab_portugal_colonial_growth".to_string(),
				"modifier".to_string(),
				"global_colonial_growth".to_string(),
			],
			inputs: vec![
				(foch::merge::kernel::RevisionId::LEFT, "50".to_string()),
				(foch::merge::kernel::RevisionId::RIGHT, "35".to_string()),
			],
			output: "50".to_string(),
		}]
	);
}

#[test]
fn eu4_diplomatic_actions_keep_distinct_tooltip_conditions_independent() {
	let path = "common/diplomatic_actions/zzz_foch_diplomatic_actions.txt";
	let base = parse_at(path, "");
	let left = parse_at(
		path,
		"requestpeace = {\n\
		\tcondition = {\n\
		\t\ttooltip = EE_PEACE_BLOCK\n\
		\t\tpotential = { has_country_flag = ee_war }\n\
		\t\tallow = { always = no }\n\
		\t}\n\
		}\n",
	);
	let right = parse_at(
		path,
		"requestpeace = {\n\
		\tcondition = {\n\
		\t\ttooltip = ICE_PEACE_BLOCK\n\
		\t\tpotential = { has_country_flag = ice_war }\n\
		\t\tallow = { always = no }\n\
		\t}\n\
		}\n",
	);
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("diplomatic actions descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge independent diplomatic-action conditions");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(
		outcome
			.resolved_ast()
			.expect("conflict-free condition merge"),
	);
	assert_eq!(output.matches("condition = {").count(), 2, "{output}");
	for identity in ["EE_PEACE_BLOCK", "ICE_PEACE_BLOCK"] {
		assert_eq!(output.matches(identity).count(), 1, "{output}");
	}
}

#[test]
fn eu4_diplomatic_actions_keep_missing_and_keyed_conditions_independent() {
	let path = "common/diplomatic_actions/zzz_foch_diplomatic_actions.txt";
	let base = parse_at(path, "");
	let left = parse_at(
		path,
		"requestpeace = {\n\
		\tcondition = {\n\
		\t\tpotential = { has_country_flag = shared_war }\n\
		\t\tallow = { always = no }\n\
		\t}\n\
		}\n",
	);
	let right = parse_at(
		path,
		"requestpeace = {\n\
		\tcondition = {\n\
		\t\ttooltip = KEYED_PEACE_BLOCK\n\
		\t\tpotential = { has_country_flag = shared_war }\n\
		\t\tallow = { always = no }\n\
		\t}\n\
		}\n",
	);
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("diplomatic actions descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge identity-less and keyed diplomatic-action conditions");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(
		outcome
			.resolved_ast()
			.expect("conflict-free condition merge"),
	);
	assert_eq!(output.matches("condition = {").count(), 2, "{output}");
	assert_eq!(output.matches("KEYED_PEACE_BLOCK").count(), 1, "{output}");
}

#[test]
fn eu4_diplomatic_actions_preserve_duplicate_blank_tooltip_cardinality() {
	let path = "common/diplomatic_actions/zzz_foch_diplomatic_actions.txt";
	let conditions = |first_allow: &str, third_allow: &str| {
		parse_at(
			path,
			&format!(
				"annexationaction = {{\n\
				\tcondition = {{ tooltip = \" \" potential = {{ tag = AAA }} allow = {{ {first_allow} }} }}\n\
				\tcondition = {{ tooltip = \" \" potential = {{ tag = BBB }} allow = {{ always = no }} }}\n\
				\tcondition = {{ tooltip = \" \" potential = {{ tag = CCC }} allow = {{ {third_allow} }} }}\n\
				}}\n"
			),
		)
	};
	let base = conditions("always = no", "always = no");
	let left = conditions("has_country_flag = left_annexation", "always = no");
	let right = conditions("always = no", "has_country_flag = right_annexation");
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("diplomatic actions descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge duplicate placeholder-tooltip conditions");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(
		outcome
			.resolved_ast()
			.expect("conflict-free placeholder condition merge"),
	);
	assert_eq!(output.matches("condition = {").count(), 3, "{output}");
	assert_eq!(output.matches("tooltip = \" \"").count(), 3, "{output}");
	for value in ["AAA", "BBB", "CCC", "left_annexation", "right_annexation"] {
		assert_eq!(output.matches(value).count(), 1, "{output}");
	}
}

#[test]
fn eu4_diplomatic_actions_keep_same_tooltip_additions_source_isolated() {
	let path = "common/diplomatic_actions/zzz_foch_diplomatic_actions.txt";
	let base = parse_at(path, "");
	let condition = |flag: &str| {
		parse_at(
			path,
			&format!(
				"requestpeace = {{\n\
				\tcondition = {{\n\
				\t\ttooltip = SHARED_PEACE_BLOCK\n\
				\t\tpotential = {{ has_country_flag = {flag} }}\n\
				\t\tallow = {{ always = no }}\n\
				\t}}\n\
				}}\n"
			),
		)
	};
	let left = condition("ee_war");
	let right = condition("ice_war");
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("diplomatic actions descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge source-isolated diplomatic-action conditions");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(
		outcome
			.resolved_ast()
			.expect("conflict-free source-isolated condition merge"),
	);
	assert_eq!(output.matches("condition = {").count(), 2, "{output}");
	assert_eq!(output.matches("SHARED_PEACE_BLOCK").count(), 2, "{output}");
	let left_offset = output.find("ee_war").expect("left condition emitted");
	let right_offset = output.find("ice_war").expect("right condition emitted");
	assert!(left_offset < right_offset, "{output}");
}

#[test]
fn eu4_diplomatic_actions_defer_divergent_edits_to_the_same_base_condition() {
	let path = "common/diplomatic_actions/zzz_foch_diplomatic_actions.txt";
	let condition = |flag: &str| {
		parse_at(
			path,
			&format!(
				"requestpeace = {{\n\
				\tcondition = {{\n\
				\t\ttooltip = SHARED_PEACE_BLOCK\n\
				\t\tpotential = {{ has_country_flag = {flag} }}\n\
				\t\tallow = {{ always = no }}\n\
				\t}}\n\
				}}\n"
			),
		)
	};
	let base = condition("base_war");
	let left = condition("ee_war");
	let right = condition("ice_war");
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("diplomatic actions descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge divergent edits to one ancestral condition");

	assert!(outcome.resolved_ast().is_none());
	assert!(!outcome.conflicts().is_empty());
	let tentative = emit(outcome.tentative_ast());
	assert_eq!(tentative.matches("condition = {").count(), 1, "{tentative}");
}

#[test]
fn eu4_diplomatic_actions_do_not_duplicate_unchanged_base_conditions() {
	let path = "common/diplomatic_actions/zzz_foch_diplomatic_actions.txt";
	let base = parse_at(
		path,
		"requestpeace = {\n\
		\tcondition = {\n\
		\t\ttooltip = BASE_PEACE_BLOCK\n\
		\t\tpotential = { has_country_flag = base_war }\n\
		\t\tallow = { always = no }\n\
		\t}\n\
		}\n",
	);
	let left = base.clone();
	let right = base.clone();
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("diplomatic actions descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge unchanged ancestral condition copies");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("unchanged copies resolve"));
	assert_eq!(output.matches("condition = {").count(), 1, "{output}");
	assert_eq!(output.matches("BASE_PEACE_BLOCK").count(), 1, "{output}");
	assert_eq!(output.matches("base_war").count(), 1, "{output}");
}

#[test]
fn eu4_diplomatic_actions_preserve_one_mods_duplicate_condition_order() {
	let path = "common/diplomatic_actions/zzz_foch_diplomatic_actions.txt";
	let base = parse_at(path, "");
	let left = parse_at(
		path,
		"requestpeace = {\n\
		\tcondition = { tooltip = SHARED potential = { tag = AAA } allow = { always = no } }\n\
		\tcondition = { tooltip = SHARED potential = { tag = BBB } allow = { always = no } }\n\
		\tcondition = { tooltip = \" \" potential = { tag = CCC } allow = { always = no } }\n\
		\tcondition = { tooltip = \" \" potential = { tag = DDD } allow = { always = no } }\n\
		}\n",
	);
	let right = parse_at(
		path,
		"requestpeace = {\n\
		\tcondition = { tooltip = SHARED potential = { tag = EEE } allow = { always = no } }\n\
		}\n",
	);
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("diplomatic actions descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge duplicate source conditions");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(
		outcome
			.resolved_ast()
			.expect("duplicate conditions resolve"),
	);
	assert_eq!(output.matches("condition = {").count(), 5, "{output}");
	assert_eq!(output.matches("tooltip = SHARED").count(), 3, "{output}");
	assert_eq!(output.matches("tooltip = \" \"").count(), 2, "{output}");
	let offsets = ["AAA", "BBB", "CCC", "DDD", "EEE"].map(|tag| {
		output
			.find(tag)
			.unwrap_or_else(|| panic!("missing {tag}: {output}"))
	});
	assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]), "{output}");
}

#[test]
fn eu4_diplomatic_actions_append_source_isolated_conditions_in_nway_order() {
	let path = "common/diplomatic_actions/zzz_foch_diplomatic_actions.txt";
	let condition = |flag: &str| {
		parse_at(
			path,
			&format!(
				"requestpeace = {{\n\
				\tcondition = {{\n\
				\t\ttooltip = SHARED_PEACE_BLOCK\n\
				\t\tpotential = {{ has_country_flag = {flag} }}\n\
				\t\tallow = {{ always = no }}\n\
				\t}}\n\
				}}\n"
			),
		)
	};
	let base = parse_at(path, "");
	let first = condition("first_war");
	let second = condition("second_war");
	let third = condition("third_war");
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("diplomatic actions descriptor");

	let outcome = merge_clausewitz_files_n_way(
		&base,
		&[&first, &second, &third],
		&descriptor.merge_policies,
	)
	.expect("merge source-isolated N-way conditions");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(outcome.resolved_ast().expect("N-way conditions resolve"));
	assert_eq!(output.matches("condition = {").count(), 3, "{output}");
	assert_eq!(output.matches("SHARED_PEACE_BLOCK").count(), 3, "{output}");
	let offsets = ["first_war", "second_war", "third_war"].map(|flag| {
		output
			.find(flag)
			.unwrap_or_else(|| panic!("missing {flag}: {output}"))
	});
	assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]), "{output}");
}

#[test]
fn eu4_subject_types_keep_distinct_modifier_subject_entries_independent() {
	let path = "common/subject_types/zzz_foch_subject_types.txt";
	let subject_type = |additional_modifier: Option<&str>| {
		let additional = additional_modifier.map_or_else(String::new, |modifier| {
			format!(
				"\tmodifier_subject = {{\n\
				\t\tmodifier = {modifier}\n\
				\t\ttrigger = {{ overlord = {{ has_country_flag = {modifier}_flag }} }}\n\
				\t}}\n"
			)
		});
		parse_at(
			path,
			&format!(
				"colony = {{\n\
				\tmodifier_subject = {{\n\
				\t\tmodifier = vanilla_colony_modifier\n\
				\t\ttrigger = {{ num_of_cities = 10 }}\n\
				\t}}\n\
				{additional}\
				}}\n"
			),
		)
	};
	let base = subject_type(None);
	let left = subject_type(Some("ee_colony_modifier"));
	let right = subject_type(Some("ice_colony_modifier"));
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("subject types descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge independent subject modifier entries");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(
		outcome
			.resolved_ast()
			.expect("conflict-free subject modifier merge"),
	);
	assert_eq!(
		output.matches("modifier_subject = {").count(),
		3,
		"{output}"
	);
	for modifier in [
		"vanilla_colony_modifier",
		"ee_colony_modifier",
		"ice_colony_modifier",
	] {
		assert_eq!(
			output.matches(&format!("modifier = {modifier}")).count(),
			1,
			"{output}"
		);
	}
}

#[test]
fn eu4_subject_types_key_modifier_overlord_entries_by_modifier() {
	let path = "common/subject_types/zzz_foch_subject_types.txt";
	let base = parse_at(path, "");
	let subject_type = |modifier: &str| {
		parse_at(
			path,
			&format!(
				"colony = {{\n\
				\tmodifier_overlord = {{\n\
				\t\tmodifier = {modifier}\n\
				\t\ttrigger = {{ has_country_flag = {modifier}_flag }}\n\
				\t}}\n\
				}}\n"
			),
		)
	};
	let left = subject_type("ee_overlord_modifier");
	let right = subject_type("ice_overlord_modifier");
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("subject types descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge independent overlord modifier entries");

	assert!(outcome.conflicts().is_empty(), "{:?}", outcome.conflicts());
	let output = emit(
		outcome
			.resolved_ast()
			.expect("conflict-free overlord modifier merge"),
	);
	assert_eq!(
		output.matches("modifier_overlord = {").count(),
		2,
		"{output}"
	);
	for modifier in ["ee_overlord_modifier", "ice_overlord_modifier"] {
		assert_eq!(
			output.matches(&format!("modifier = {modifier}")).count(),
			1,
			"{output}"
		);
	}
}

#[test]
fn eu4_subject_types_still_conflict_on_same_modifier_subject_identity() {
	let path = "common/subject_types/zzz_foch_subject_types.txt";
	let base = parse_at(path, "");
	let subject_type = |flag: &str| {
		parse_at(
			path,
			&format!(
				"colony = {{\n\
				\tmodifier_subject = {{\n\
				\t\tmodifier = shared_colony_modifier\n\
				\t\ttrigger = {{ overlord = {{ has_country_flag = {flag} }} }}\n\
				\t}}\n\
				}}\n"
			),
		)
	};
	let left = subject_type("ee_flag");
	let right = subject_type("ice_flag");
	let descriptor = eu4_profile()
		.classify_content_family(PathBuf::from(path).as_path())
		.expect("subject types descriptor");

	let outcome = merge_clausewitz_files(&base, &left, &right, &descriptor.merge_policies)
		.expect("merge same-identity subject modifier entries");

	assert!(outcome.resolved_ast().is_none());
	assert!(
		outcome
			.conflicts()
			.iter()
			.any(|conflict| conflict.kind == ConflictKind::InsertInsert),
		"{:?}",
		outcome.conflicts()
	);
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
