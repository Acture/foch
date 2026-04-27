use foch_core::model::{ConditionalParamRule, ParamContract, SemanticIndex, SymbolKind};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

type ParamContractRegistry = BTreeMap<&'static str, ParamContract>;

fn complex_dynamic_effect_optional_params(include_combined_effect: bool) -> Vec<String> {
	let mut optional = Vec::new();
	for prefix in [
		"second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "nineth", "tenth",
	] {
		for suffix in ["custom_tooltip", "limit", "effect"] {
			optional.push(format!("{prefix}_{suffix}"));
		}
	}
	for suffix in ["custom_tooltip", "limit", "effect"] {
		optional.push(format!("eigth_{suffix}"));
	}
	if include_combined_effect {
		optional.push("combined_effect".to_string());
	}
	optional
}

fn complex_dynamic_effect_contract(include_combined_effect: bool) -> ParamContract {
	ParamContract {
		required_all: vec![
			"first_custom_tooltip".to_string(),
			"first_limit".to_string(),
			"first_effect".to_string(),
		],
		optional: complex_dynamic_effect_optional_params(include_combined_effect),
		one_of_groups: Vec::new(),
		conditional_required: Vec::new(),
	}
}

fn registered_param_contract_registry() -> &'static ParamContractRegistry {
	static REGISTRY: OnceLock<ParamContractRegistry> = OnceLock::new();
	REGISTRY.get_or_init(|| {
		let mut registry = ParamContractRegistry::new();
		registry.insert(
			"complex_dynamic_effect",
			complex_dynamic_effect_contract(false),
		);
		registry.insert(
			"complex_dynamic_effect_without_alternative",
			complex_dynamic_effect_contract(true),
		);
		registry.insert(
			"ME_give_claims",
			ParamContract {
				required_all: Vec::new(),
				optional: Vec::new(),
				one_of_groups: vec![vec![
					"area".to_string(),
					"region".to_string(),
					"province".to_string(),
					"id".to_string(),
				]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"give_claims",
			ParamContract {
				required_all: Vec::new(),
				optional: Vec::new(),
				one_of_groups: vec![vec![
					"area".to_string(),
					"region".to_string(),
					"province".to_string(),
					"id".to_string(),
				]],
				conditional_required: Vec::new(),
			},
		);
		for local_name in [
			"add_prestige_or_monarch_power",
			"add_army_tradition_or_mil_power",
		] {
			registry.insert(
				local_name,
				ParamContract {
					required_all: Vec::new(),
					optional: Vec::new(),
					one_of_groups: vec![vec!["amount".to_string(), "value".to_string()]],
					conditional_required: Vec::new(),
				},
			);
		}
		registry.insert(
			"country_event_with_option_insight",
			ParamContract {
				required_all: vec!["id".to_string()],
				optional: vec![
					"days".to_string(),
					"random".to_string(),
					"tooltip".to_string(),
					"option_1".to_string(),
					"option_2".to_string(),
					"option_3".to_string(),
					"option_4".to_string(),
					"option_5".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"add_age_modifier",
			ParamContract {
				required_all: vec![
					"age".to_string(),
					"name".to_string(),
					"duration".to_string(),
				],
				optional: vec!["else".to_string()],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"change_asha_vahishta",
			ParamContract {
				required_all: vec!["value".to_string()],
				optional: vec!["custom_tooltip".to_string()],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"se_md_refund_splendor",
			ParamContract {
				required_all: vec!["flagName".to_string(), "flagCategory".to_string()],
				optional: Vec::new(),
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		for local_name in [
			"se_md_refund_splendor_bonus_progress",
			"se_md_refund_splendor_bonus_complete",
		] {
			registry.insert(
				local_name,
				ParamContract {
					required_all: vec!["flagName".to_string(), "bonusName".to_string()],
					optional: Vec::new(),
					one_of_groups: Vec::new(),
					conditional_required: Vec::new(),
				},
			);
		}
		registry.insert(
			"se_md_add_or_upgrade_bonus",
			ParamContract {
				required_all: vec![
					"abilityName".to_string(),
					"bonusName".to_string(),
					"flagName".to_string(),
				],
				optional: vec!["showTooltip".to_string()],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"country_event_with_effect_insight",
			ParamContract {
				required_all: vec!["id".to_string(), "effect".to_string()],
				optional: vec![
					"days".to_string(),
					"random".to_string(),
					"tooltip".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"country_event_with_insight",
			ParamContract {
				required_all: vec!["id".to_string(), "insight_tooltip".to_string()],
				optional: vec![
					"days".to_string(),
					"random".to_string(),
					"tooltip".to_string(),
					"effect_tooltip".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"ME_distribute_development",
			ParamContract {
				required_all: vec!["type".to_string(), "amount".to_string()],
				optional: vec!["limit".to_string(), "tooltip".to_string()],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"define_and_hire_grand_vizier",
			ParamContract {
				required_all: vec!["type".to_string()],
				optional: vec![
					"skill".to_string(),
					"culture".to_string(),
					"religion".to_string(),
					"female".to_string(),
					"age".to_string(),
					"max_age".to_string(),
					"min_age".to_string(),
					"location".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"pick_best_provinces",
			ParamContract {
				required_all: vec![
					"scale".to_string(),
					"event_target_name".to_string(),
					"global_trigger".to_string(),
				],
				optional: vec![
					"scope".to_string(),
					"1".to_string(),
					"2".to_string(),
					"3".to_string(),
					"4".to_string(),
					"5".to_string(),
					"10".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"pick_best_tags",
			ParamContract {
				required_all: vec![
					"scale".to_string(),
					"event_target_name".to_string(),
					"global_trigger".to_string(),
				],
				optional: vec![
					"scope".to_string(),
					"1".to_string(),
					"2".to_string(),
					"3".to_string(),
					"4".to_string(),
					"5".to_string(),
					"10".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"ME_add_years_of_trade_income",
			ParamContract {
				required_all: Vec::new(),
				optional: Vec::new(),
				one_of_groups: vec![vec![
					"years".to_string(),
					"value".to_string(),
					"amount".to_string(),
				]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"ME_overlord_effect",
			ParamContract {
				required_all: vec!["effect".to_string()],
				optional: Vec::new(),
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"create_general_with_pips",
			ParamContract {
				required_all: vec!["tradition".to_string()],
				optional: vec![
					"add_fire".to_string(),
					"add_shock".to_string(),
					"add_manuever".to_string(),
					"add_siege".to_string(),
					"name".to_string(),
					"culture".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"create_or_add_center_of_trade_level",
			ParamContract {
				required_all: vec!["level".to_string()],
				optional: Vec::new(),
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"take_estate_land_share_massive",
			ParamContract {
				required_all: vec!["estate".to_string()],
				optional: vec!["amount".to_string()],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"add_estate_loyalty",
			ParamContract {
				required_all: vec!["estate".to_string()],
				optional: vec!["short".to_string(), "amount".to_string()],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"estate_loyalty",
			ParamContract {
				required_all: vec!["estate".to_string(), "loyalty".to_string()],
				optional: Vec::new(),
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"estate_influence",
			ParamContract {
				required_all: vec!["estate".to_string(), "influence".to_string()],
				optional: Vec::new(),
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"unlock_estate_privilege",
			ParamContract {
				required_all: vec!["estate_privilege".to_string()],
				optional: vec!["modifier_tooltip".to_string(), "effect_tooltip".to_string()],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"HAB_change_habsburg_glory",
			ParamContract {
				required_all: Vec::new(),
				optional: Vec::new(),
				one_of_groups: vec![vec!["amount".to_string(), "remove".to_string()]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"add_legitimacy_or_reform_progress",
			ParamContract {
				required_all: Vec::new(),
				optional: Vec::new(),
				one_of_groups: vec![vec!["amount".to_string(), "value".to_string()]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"EE_change_variable",
			ParamContract {
				required_all: vec!["which".to_string()],
				optional: Vec::new(),
				one_of_groups: vec![vec![
					"add".to_string(),
					"subtract".to_string(),
					"divide".to_string(),
					"multiply".to_string(),
				]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"build_as_many_as_possible",
			ParamContract {
				required_all: vec![
					"pick_best_function".to_string(),
					"new_building".to_string(),
					"cost".to_string(),
					"speed".to_string(),
				],
				optional: vec!["all_prior_trig".to_string()],
				one_of_groups: vec![vec![
					"upgrade_target".to_string(),
					"construct_new".to_string(),
				]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"ME_tim_add_spoils_of_war",
			ParamContract {
				required_all: Vec::new(),
				optional: Vec::new(),
				one_of_groups: vec![vec!["add".to_string(), "remove".to_string()]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"ME_add_power_projection",
			ParamContract {
				required_all: Vec::new(),
				optional: Vec::new(),
				one_of_groups: vec![vec!["amount".to_string(), "value".to_string()]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"create_general_scaling_with_tradition_and_pips",
			ParamContract {
				required_all: Vec::new(),
				optional: vec![
					"add_fire".to_string(),
					"add_shock".to_string(),
					"add_manuever".to_string(),
					"add_siege".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"ME_automatic_colonization_effect_module",
			ParamContract {
				required_all: vec!["target_region_effect".to_string()],
				optional: Vec::new(),
				one_of_groups: vec![vec!["region".to_string(), "superregion".to_string()]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"ME_override_country_name",
			ParamContract {
				required_all: Vec::new(),
				optional: Vec::new(),
				one_of_groups: vec![vec![
					"country_name".to_string(),
					"name".to_string(),
					"country".to_string(),
					"value".to_string(),
					"string".to_string(),
				]],
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"persia_indian_hegemony_decision_march_effect",
			ParamContract {
				required_all: vec![
					"province".to_string(),
					"tag_1".to_string(),
					"trade_company_region".to_string(),
				],
				optional: vec![
					"tag_2".to_string(),
					"tag_3".to_string(),
					"tag_4".to_string(),
					"tag_5".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"persia_indian_hegemony_decision_coup_effect",
			ParamContract {
				required_all: vec!["province".to_string(), "tag_1".to_string()],
				optional: vec![
					"tag_2".to_string(),
					"tag_3".to_string(),
					"tag_4".to_string(),
					"tag_5".to_string(),
				],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry.insert(
			"for",
			ParamContract {
				required_all: vec!["amount".to_string(), "effect".to_string()],
				optional: Vec::new(),
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			},
		);
		registry
	})
}

pub(crate) fn registered_param_contracts_hash() -> &'static str {
	static HASH: OnceLock<String> = OnceLock::new();
	HASH.get_or_init(|| {
		let encoded = serde_json::to_vec(registered_param_contract_registry())
			.expect("serialize registered param contract registry");
		let digest = Sha256::digest(&encoded);
		format!("{digest:x}")[..16].to_string()
	})
}

pub(crate) fn registered_param_contract(local_name: &str) -> Option<ParamContract> {
	registered_param_contract_registry()
		.get(local_name)
		.cloned()
}

pub fn apply_registered_param_contracts(index: &mut SemanticIndex) {
	for definition in &mut index.definitions {
		if definition.kind != SymbolKind::ScriptedEffect {
			definition.param_contract = None;
			continue;
		}
		if definition.param_contract.is_none() {
			definition.param_contract = registered_param_contract(&definition.local_name);
		}
	}
}

pub(crate) fn explicit_contract_param_names(local_name: &str) -> HashSet<String> {
	let Some(contract) = registered_param_contract(local_name) else {
		return HashSet::new();
	};
	let mut names = HashSet::new();
	names.extend(contract.required_all);
	names.extend(contract.optional);
	for group in contract.one_of_groups {
		names.extend(group);
	}
	for rule in contract.conditional_required {
		names.insert(rule.when_present);
		names.extend(rule.requires_any_of);
	}
	names
}

pub(crate) fn evaluate_param_contract(
	contract: &ParamContract,
	name: &str,
	provided: &HashSet<&str>,
) -> Vec<String> {
	let mut messages = Vec::new();

	for required in &contract.required_all {
		if !provided.contains(required.as_str()) {
			messages.push(format!("参数未绑定: {name} 缺失 {required}"));
		}
	}

	for group in &contract.one_of_groups {
		if group
			.iter()
			.any(|candidate| provided.contains(candidate.as_str()))
		{
			continue;
		}
		messages.push(format!(
			"参数未绑定: {name} 至少需要一个参数: {}",
			group.join("|")
		));
	}

	for ConditionalParamRule {
		when_present,
		requires_any_of,
	} in &contract.conditional_required
	{
		if !provided.contains(when_present.as_str()) {
			continue;
		}
		if requires_any_of
			.iter()
			.any(|candidate| provided.contains(candidate.as_str()))
		{
			continue;
		}
		messages.push(format!(
			"参数未绑定: {name} 在提供 {when_present} 时至少需要一个参数: {}",
			requires_any_of.join("|")
		));
	}

	messages
}

#[cfg(test)]
mod tests {
	use super::{apply_registered_param_contracts, registered_param_contract};
	use foch_core::model::{ParamContract, ScopeType, SemanticIndex, SymbolDefinition, SymbolKind};
	use std::path::PathBuf;

	#[test]
	fn apply_registered_param_contracts_preserves_existing_contracts() {
		let mut index = SemanticIndex::default();
		index.definitions.push(SymbolDefinition {
			kind: SymbolKind::ScriptedEffect,
			name: "custom.effect".to_string(),
			module: "test".to_string(),
			local_name: "ME_give_claims".to_string(),
			mod_id: "1000".to_string(),
			path: PathBuf::from("common/scripted_effects/test.txt"),
			line: 1,
			column: 1,
			scope_id: 0,
			declared_this_type: ScopeType::Country,
			inferred_this_type: ScopeType::Country,
			inferred_this_mask: 0b01,
			inferred_from_mask: 0,
			inferred_root_mask: 0,
			required_params: Vec::new(),
			param_contract: Some(ParamContract {
				required_all: vec!["custom".to_string()],
				optional: Vec::new(),
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			}),
			optional_params: Vec::new(),
			scope_param_names: Vec::new(),
		});

		apply_registered_param_contracts(&mut index);

		let definition = index.definitions.first().expect("definition");
		assert_eq!(
			definition.param_contract.as_ref().expect("param contract"),
			&ParamContract {
				required_all: vec!["custom".to_string()],
				optional: Vec::new(),
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			}
		);
		assert!(registered_param_contract("ME_give_claims").is_some());
		assert!(registered_param_contract("give_claims").is_some());
		assert!(registered_param_contract("pick_best_tags").is_some());
		assert!(registered_param_contract("ME_add_years_of_trade_income").is_some());
	}
}
