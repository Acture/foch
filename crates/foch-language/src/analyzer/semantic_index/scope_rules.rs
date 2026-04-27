use super::super::content_family::ScriptFileKind;
use foch_core::model::{ScopeKind, ScopeType};

pub fn iterator_scope_type(key: &str) -> Option<ScopeType> {
	match key {
		// Province iterators (every_/any_/all_/random_<...>_province)
		"all_core_province"
		| "all_neighbor_province"
		| "all_owned_province"
		| "all_owned_province_cumulative"
		| "all_province"
		| "all_province_in_state"
		| "all_state_province"
		| "all_trade_node_member_province"
		| "any_core_province"
		| "any_empty_neighbor_province"
		| "any_friendly_coast_border_province"
		| "any_heretic_province"
		| "any_owned_province"
		| "any_province"
		| "any_province_in_state"
		| "any_trade_node_member_province"
		| "every_claimed_province"
		| "every_core_province"
		| "every_empty_neighbor_province"
		| "every_heretic_province"
		| "every_neighbor_province"
		| "every_owned_province"
		| "every_province"
		| "every_province_in_state"
		| "every_trade_node_member_province"
		| "every_tribal_land_province"
		| "random_area_province"
		| "random_core_province"
		| "random_empty_neighbor_province"
		| "random_heretic_province"
		| "random_neighbor_province"
		| "random_owned_province"
		| "random_province"
		| "random_province_in_state"
		| "random_trade_node_member_province"
		// Sea zones are provinces in EU4
		| "every_neighbor_sea_zone"
		// Tribal land iterates over provinces
		| "any_tribal_land" => Some(ScopeType::Province),
		// Country iterators
		"all_ally"
		| "all_countries_including_self"
		| "all_country"
		| "all_elector"
		| "all_federation_members"
		| "all_known_country"
		| "all_neighbor_country"
		| "all_rival_country"
		| "all_subject_country"
		| "all_trade_node_member_country"
		| "all_war_enemy_countries"
		| "any_ally"
		| "any_core_country"
		| "any_country"
		| "any_country_active_in_node"
		| "any_elector"
		| "any_enemy_country"
		| "any_great_power"
		| "any_hired_mercenary_company"
		| "any_known_country"
		| "any_neighbor_country"
		| "any_other_great_power"
		| "any_privateering_country"
		| "any_rival_country"
		| "any_trade_node_member_country"
		| "any_war_enemy_country"
		| "every_ally"
		| "every_core_country"
		| "every_country"
		| "every_country_including_inactive"
		| "every_elector"
		| "every_enemy_country"
		| "every_federation_member"
		| "every_known_country"
		| "every_neighbor_country"
		| "every_rival_country"
		| "every_subject_country"
		| "every_trade_node_member_country"
		| "every_war_enemy_country"
		| "random_ally"
		| "random_core_country"
		| "random_country"
		| "random_elector"
		| "random_enemy_country"
		| "random_hired_mercenary_company"
		| "random_known_country"
		| "random_neighbor_country"
		| "random_privateering_country"
		| "random_rival_country"
		| "random_subject_country"
		| "random_war_enemy_country" => Some(ScopeType::Country),
		_ => None,
	}
}

pub fn file_kind_container_scope_kind(file_kind: ScriptFileKind, key: &str) -> Option<ScopeKind> {
	match file_kind {
		ScriptFileKind::Missions => match key {
			"potential_on_load"
			| "potential"
			| "trigger"
			| "provinces_to_highlight"
			| "completed_by"
			| "ai_weight" => Some(ScopeKind::Trigger),
			"effect" | "on_completed" | "on_cancelled" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::NewDiplomaticActions => match key {
			"is_visible" | "is_allowed" | "ai_will_do" => Some(ScopeKind::Trigger),
			"on_accept" | "on_decline" | "add_entry" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::Events => match key {
			"mean_time_to_happen" => Some(ScopeKind::Trigger),
			_ => None,
		},
		ScriptFileKind::Ages => match key {
			"can_start" | "custom_trigger_tooltip" | "calc_true_if" | "ai_will_do" => {
				Some(ScopeKind::Trigger)
			}
			"effect" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::Buildings => match key {
			"ai_will_do" => Some(ScopeKind::Trigger),
			"on_built"
			| "on_destroyed"
			| "on_construction_started"
			| "on_construction_canceled"
			| "on_obsolete" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::Institutions => match key {
			"history" | "can_embrace" | "potential" | "custom_trigger_tooltip" => {
				Some(ScopeKind::Trigger)
			}
			"on_start" => Some(ScopeKind::Effect),
			"embracement_speed" | "modifier" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::ProvinceTriggeredModifiers => match key {
			"potential" | "trigger" => Some(ScopeKind::Trigger),
			"on_activation" | "on_deactivation" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::TriggeredModifiers => match key {
			"potential" | "trigger" => Some(ScopeKind::Trigger),
			"on_activation" | "on_deactivation" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::ScriptedTriggers => match key {
			"trigger" | "limit" | "custom_trigger_tooltip" => Some(ScopeKind::Trigger),
			_ => None,
		},
		ScriptFileKind::Ideas => match key {
			"start" | "bonus" => Some(ScopeKind::Effect),
			"trigger" | "ai_will_do" => Some(ScopeKind::Trigger),
			_ => None,
		},
		ScriptFileKind::GreatProjects => {
			if key.ends_with("_trigger") {
				Some(ScopeKind::Trigger)
			} else if matches!(
				key,
				"on_built"
					| "on_destroyed"
					| "on_upgraded" | "on_downgraded"
					| "on_obtained" | "on_lost"
			) {
				Some(ScopeKind::Effect)
			} else {
				None
			}
		}
		ScriptFileKind::GovernmentReforms => match key {
			"on_enabled" | "on_disabled" | "on_enacted" | "on_removed" | "removed_effect" => {
				Some(ScopeKind::Effect)
			}
			"ai_will_do" => Some(ScopeKind::Trigger),
			_ => None,
		},
		ScriptFileKind::CbTypes => match key {
			"prerequisites_self" | "prerequisites" | "can_use" | "can_take_province" => {
				Some(ScopeKind::Trigger)
			}
			_ => None,
		},
		ScriptFileKind::GovernmentNames | ScriptFileKind::CustomizableLocalization => match key {
			"trigger" => Some(ScopeKind::Trigger),
			_ => None,
		},
		ScriptFileKind::Religions => match key {
			"potential" | "allow" | "ai_will_do" => Some(ScopeKind::Trigger),
			"effect" | "on_convert" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::SubjectTypes => match key {
			"is_potential_overlord" | "can_fight" | "can_rival" | "can_ally" | "can_marry" => {
				Some(ScopeKind::Trigger)
			}
			"modifier_subject" | "modifier_overlord" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::RebelTypes => match key {
			"spawn_chance"
			| "movement_evaluation"
			| "can_negotiate_trigger"
			| "can_enforce_trigger" => Some(ScopeKind::Trigger),
			"siege_won_effect" | "demands_enforced_effect" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::Disasters => match key {
			"potential" | "can_start" | "can_stop" | "can_end" => Some(ScopeKind::Trigger),
			"on_start" | "on_end" | "on_monthly" => Some(ScopeKind::Effect),
			"progress" | "modifier" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::GovernmentMechanics => match key {
			"available" | "trigger" => Some(ScopeKind::Trigger),
			"on_max_reached" | "on_min_reached" => Some(ScopeKind::Effect),
			"powers" | "scaled_modifier" | "reverse_scaled_modifier" | "modifier" => {
				Some(ScopeKind::Block)
			}
			_ => None,
		},
		ScriptFileKind::ChurchAspects => match key {
			"potential" | "trigger" | "ai_will_do" => Some(ScopeKind::Trigger),
			"effect" => Some(ScopeKind::Effect),
			"modifier" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::Factions => match key {
			"allow" => Some(ScopeKind::Trigger),
			"modifier" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::Hegemons => match key {
			"allow" => Some(ScopeKind::Trigger),
			"base" | "scale" | "max" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::PersonalDeities => match key {
			"potential" | "trigger" | "ai_will_do" => Some(ScopeKind::Trigger),
			"effect" | "removed_effect" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::FetishistCults => match key {
			"allow" | "ai_will_do" => Some(ScopeKind::Trigger),
			_ => None,
		},
		ScriptFileKind::PeaceTreaties => match key {
			"is_visible" | "is_allowed" | "ai_weight" => Some(ScopeKind::Trigger),
			"effect" => Some(ScopeKind::Effect),
			"warscore_cost" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::Policies => match key {
			"potential" | "allow" | "ai_will_do" => Some(ScopeKind::Trigger),
			"effect" | "removed_effect" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::MercenaryCompanies => match key {
			"trigger" => Some(ScopeKind::Trigger),
			"modifier" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::EstateAgendas => match key {
			"can_select"
			| "task_requirements"
			| "fail_if"
			| "invalid_trigger"
			| "provinces_to_highlight"
			| "selection_weight" => Some(ScopeKind::Trigger),
			"pre_effect" | "immediate_effect" | "on_invalid" | "task_completed_effect" => {
				Some(ScopeKind::Effect)
			}
			_ => None,
		},
		ScriptFileKind::EstatePrivileges => match key {
			"is_valid" | "can_select" | "can_revoke" | "ai_will_do" => Some(ScopeKind::Trigger),
			"on_granted"
			| "on_revoked"
			| "on_invalid"
			| "on_granted_province"
			| "on_revoked_province"
			| "on_invalid_province"
			| "on_cooldown_expires" => Some(ScopeKind::Effect),
			"benefits"
			| "penalties"
			| "modifier_by_land_ownership"
			| "mechanics"
			| "conditional_modifier"
			| "influence_scaled_conditional_modifier"
			| "loyalty_scaled_conditional_modifier" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::Estates => match key {
			"trigger" => Some(ScopeKind::Trigger),
			"country_modifier_happy"
			| "country_modifier_neutral"
			| "country_modifier_angry"
			| "land_ownership_modifier"
			| "province_independence_weight"
			| "influence_modifier"
			| "loyalty_modifier"
			| "influence_from_dev_modifier" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::ParliamentBribes => match key {
			"trigger" | "chance" | "ai_will_do" => Some(ScopeKind::Trigger),
			"effect" => Some(ScopeKind::Effect),
			_ => None,
		},
		ScriptFileKind::ParliamentIssues => match key {
			"allow" | "chance" | "ai_will_do" => Some(ScopeKind::Trigger),
			"effect" | "on_issue_taken" => Some(ScopeKind::Effect),
			"modifier" | "influence_scaled_modifier" => Some(ScopeKind::Block),
			_ => None,
		},
		ScriptFileKind::StateEdicts => match key {
			"potential" | "allow" | "notify_trigger" | "ai_will_do" => Some(ScopeKind::Trigger),
			"modifier" => Some(ScopeKind::Block),
			_ => None,
		},
		_ => None,
	}
}

pub fn scope_changer_target_type(key: &str) -> Option<ScopeType> {
	match key {
		"capital_scope" => Some(ScopeType::Province),
		"owner" => Some(ScopeType::Country),
		_ => None,
	}
}

pub fn special_block_scope_kind(key: &str) -> ScopeKind {
	match key {
		"possible"
		| "visible"
		| "happened"
		| "provinces_to_highlight"
		| "exclude_from_progress" => ScopeKind::Trigger,
		_ => ScopeKind::Block,
	}
}

pub fn is_country_file_reference(value: &str) -> bool {
	value.starts_with("countries/") && value.ends_with(".txt")
}

pub fn is_country_tag_text(value: &str) -> bool {
	is_country_tag_token(value)
}

pub fn is_country_tag_selector(key: &str) -> bool {
	is_country_tag_token(key)
}

/// EU4 country tags are exactly three ASCII characters: the first is an
/// uppercase letter and the remaining characters are uppercase letters or
/// ASCII digits. This matches both vanilla tags (FRA, ENG, KOR) and the
/// digit-bearing tags commonly used by mods (X3E, Y1D, K01).
fn is_country_tag_token(token: &str) -> bool {
	if token.len() != 3 {
		return false;
	}
	let mut chars = token.chars();
	let first = chars.next().unwrap();
	if !first.is_ascii_uppercase() {
		return false;
	}
	chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

pub fn is_province_id_text(value: &str) -> bool {
	value.parse::<u32>().is_ok_and(|id| id > 0)
}

pub fn is_province_id_selector(key: &str) -> bool {
	key.parse::<u32>().map(|value| value > 100).unwrap_or(false)
}

pub fn is_dynamic_scope_reference_key(key: &str) -> bool {
	key.starts_with("event_target:")
}

pub fn looks_like_map_group_key(key: &str) -> bool {
	key.ends_with("_area")
		|| key.ends_with("_region")
		|| key.ends_with("_superregion")
		|| key.ends_with("_provincegroup")
}
