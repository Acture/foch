use crate::check::model::{ConditionalParamRule, ParamContract, SemanticIndex, SymbolKind};
use std::collections::HashSet;

pub(crate) fn registered_param_contract(local_name: &str) -> Option<ParamContract> {
	match local_name {
		"ME_give_claims" => Some(ParamContract {
			required_all: Vec::new(),
			optional: Vec::new(),
			one_of_groups: vec![vec![
				"area".to_string(),
				"region".to_string(),
				"province".to_string(),
				"id".to_string(),
			]],
			conditional_required: Vec::new(),
		}),
		"add_prestige_or_monarch_power" | "add_army_tradition_or_mil_power" => {
			Some(ParamContract {
				required_all: Vec::new(),
				optional: Vec::new(),
				one_of_groups: vec![vec!["amount".to_string(), "value".to_string()]],
				conditional_required: Vec::new(),
			})
		}
		"country_event_with_option_insight" => Some(ParamContract {
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
		}),
		"add_age_modifier" => Some(ParamContract {
			required_all: vec![
				"age".to_string(),
				"name".to_string(),
				"duration".to_string(),
			],
			optional: vec!["else".to_string()],
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		"country_event_with_effect_insight" => Some(ParamContract {
			required_all: vec!["id".to_string(), "effect".to_string()],
			optional: vec![
				"days".to_string(),
				"random".to_string(),
				"tooltip".to_string(),
			],
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		"ME_distribute_development" => Some(ParamContract {
			required_all: vec!["type".to_string(), "amount".to_string()],
			optional: vec!["limit".to_string(), "tooltip".to_string()],
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		"pick_best_provinces" => Some(ParamContract {
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
		}),
		"ME_overlord_effect" => Some(ParamContract {
			required_all: vec!["effect".to_string()],
			optional: Vec::new(),
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		"create_general_with_pips" => Some(ParamContract {
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
		}),
		"create_or_add_center_of_trade_level" => Some(ParamContract {
			required_all: vec!["level".to_string()],
			optional: Vec::new(),
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		"take_estate_land_share_massive" => Some(ParamContract {
			required_all: vec!["estate".to_string()],
			optional: vec!["amount".to_string()],
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		"add_estate_loyalty" => Some(ParamContract {
			required_all: vec!["estate".to_string()],
			optional: vec!["short".to_string(), "amount".to_string()],
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		"estate_loyalty" => Some(ParamContract {
			required_all: vec!["estate".to_string(), "loyalty".to_string()],
			optional: Vec::new(),
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		"estate_influence" => Some(ParamContract {
			required_all: vec!["estate".to_string(), "influence".to_string()],
			optional: Vec::new(),
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		"for" => Some(ParamContract {
			required_all: vec!["amount".to_string(), "effect".to_string()],
			optional: Vec::new(),
			one_of_groups: Vec::new(),
			conditional_required: Vec::new(),
		}),
		_ => None,
	}
}

pub(crate) fn apply_registered_param_contracts(index: &mut SemanticIndex) {
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
		if group.iter().any(|candidate| provided.contains(candidate.as_str())) {
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
	use crate::check::model::{ParamContract, SemanticIndex, SymbolDefinition, SymbolKind};
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
			declared_this_type: crate::check::model::ScopeType::Country,
			inferred_this_type: crate::check::model::ScopeType::Country,
			inferred_this_mask: 0b01,
			required_params: Vec::new(),
			param_contract: Some(ParamContract {
				required_all: vec!["custom".to_string()],
				optional: Vec::new(),
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			}),
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
	}
}
