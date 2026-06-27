use super::super::content_family::CwtType;
use super::super::eu4_builtin::is_builtin_effect;
use foch_core::model::{ScopeKind, ScopeType, base_scope};
use foch_cwt::{
	AliasCategory, BindContext, CwtAlias, CwtRuleField, CwtRuleValue, CwtSchemaGraph, CwtTypeDef,
};

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
		| "any_tribal_land" => Some(base_scope::province()),
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
		| "random_war_enemy_country" => Some(base_scope::country()),
		_ => None,
	}
}

pub fn cwt_iterator_scope_type(graph: &CwtSchemaGraph, key: &str) -> Option<ScopeType> {
	cwt_alias_push_scope(graph, key)
		.and_then(scope_name_to_base_scope)
		.or_else(|| cwt_field_push_scope_type(graph, key))
}

pub fn cwt_scope_changer_target_type(graph: &CwtSchemaGraph, key: &str) -> Option<ScopeType> {
	cwt_alias_push_scope(graph, key)
		.and_then(scope_name_to_base_scope)
		.or_else(|| cwt_direct_scope_marker_type(graph, key))
		.or_else(|| scope_changer_target_type_fallback(key))
}

pub fn cwt_special_block_scope_kind(graph: &CwtSchemaGraph, key: &str) -> ScopeKind {
	let mut has_trigger = false;
	let mut has_effect = false;
	for alias in graph.aliases.values() {
		if alias.name != key {
			continue;
		}
		match alias_rules_scope_kind(alias) {
			Some(ScopeKind::Trigger) => has_trigger = true,
			Some(ScopeKind::Effect) => has_effect = true,
			_ => {}
		}
	}
	visit_graph_rule_fields(graph, &mut |field| {
		if field.key != key {
			return;
		}
		match rule_field_alias_scope_kind(field) {
			Some(ScopeKind::Trigger) => has_trigger = true,
			Some(ScopeKind::Effect) => has_effect = true,
			_ => {}
		}
	});
	if has_effect {
		ScopeKind::Effect
	} else if has_trigger {
		ScopeKind::Trigger
	} else {
		ScopeKind::Block
	}
}

fn cwt_alias_push_scope<'a>(graph: &'a CwtSchemaGraph, key: &str) -> Option<&'a str> {
	[AliasCategory::Effect, AliasCategory::Trigger]
		.into_iter()
		.find_map(|category| {
			graph
				.aliases
				.get(&(category, key.to_string()))
				.and_then(alias_push_scope)
		})
}

fn alias_push_scope(alias: &CwtAlias) -> Option<&str> {
	alias.attributes.push_scope.as_deref().or_else(|| {
		alias
			.rules
			.first()
			.and_then(|field| field.attributes.push_scope.as_deref())
	})
}

fn scope_name_to_base_scope(scope_name: &str) -> Option<ScopeType> {
	if scope_name.eq_ignore_ascii_case("country") {
		Some(base_scope::country())
	} else if scope_name.eq_ignore_ascii_case("province") {
		Some(base_scope::province())
	} else {
		None
	}
}

fn cwt_field_push_scope_type(graph: &CwtSchemaGraph, key: &str) -> Option<ScopeType> {
	let mut matched = None;
	let mut ambiguous = false;
	visit_graph_rule_fields(graph, &mut |field| {
		if ambiguous || field.key != key {
			return;
		}
		let Some(scope_type) = field
			.attributes
			.push_scope
			.as_deref()
			.and_then(scope_name_to_base_scope)
		else {
			return;
		};
		match matched {
			Some(existing) if existing != scope_type => ambiguous = true,
			Some(_) => {}
			None => matched = Some(scope_type),
		}
	});
	if ambiguous { None } else { matched }
}

fn cwt_direct_scope_marker_type(graph: &CwtSchemaGraph, key: &str) -> Option<ScopeType> {
	let mut matched = None;
	let mut ambiguous = false;
	visit_graph_rule_fields(graph, &mut |field| {
		if ambiguous || field.key != key {
			return;
		}
		let Some(scope_type) = rule_field_scope_marker_type(field) else {
			return;
		};
		match matched {
			Some(existing) if existing != scope_type => ambiguous = true,
			Some(_) => {}
			None => matched = Some(scope_type),
		}
	});
	if ambiguous { None } else { matched }
}

fn rule_field_scope_marker_type(field: &CwtRuleField) -> Option<ScopeType> {
	match &field.value {
		CwtRuleValue::Marker(marker) | CwtRuleValue::Scalar(marker) => marker_scope_type(marker),
		CwtRuleValue::Block(_) => None,
	}
}

fn marker_scope_type(marker: &str) -> Option<ScopeType> {
	let (head, payload) = cwt_marker_parts(marker)?;
	(head == "scope")
		.then(|| scope_name_to_base_scope(payload))
		.flatten()
}

fn cwt_marker_parts(text: &str) -> Option<(&str, &str)> {
	let (head, rest) = text.split_once('[')?;
	Some((head, rest.strip_suffix(']')?))
}

fn scope_changer_target_type_fallback(key: &str) -> Option<ScopeType> {
	match key {
		// `scope_links.cwt:385-387` comments out `alias[effect:owner]`, and
		// `scope_links.cwt:1114-1116` comments out `alias[trigger:owner]`.
		"owner" => Some(base_scope::country()),
		// `scope_links.cwt:392-394` comments out `alias[effect:controller]`, and
		// `scope_links.cwt:1121-1123` comments out `alias[trigger:controller]`.
		"controller" => Some(base_scope::country()),
		_ => None,
	}
}

fn alias_rules_scope_kind(alias: &CwtAlias) -> Option<ScopeKind> {
	rules_alias_scope_kind(&alias.rules)
}

fn rule_field_alias_scope_kind(field: &CwtRuleField) -> Option<ScopeKind> {
	let CwtRuleValue::Block(fields) = &field.value else {
		return None;
	};
	rules_alias_scope_kind(fields)
}

fn rules_alias_scope_kind(fields: &[CwtRuleField]) -> Option<ScopeKind> {
	if fields.iter().any(|child| child.key == "alias_name[effect]") {
		Some(ScopeKind::Effect)
	} else if fields
		.iter()
		.any(|child| child.key == "alias_name[trigger]")
	{
		Some(ScopeKind::Trigger)
	} else {
		None
	}
}

fn visit_graph_rule_fields<F>(graph: &CwtSchemaGraph, visit: &mut F)
where
	F: FnMut(&CwtRuleField),
{
	for definition in graph.types.values() {
		visit_rule_fields(&definition.rules, visit);
		for subtype in &definition.subtypes {
			visit_rule_fields(&subtype.rules, visit);
		}
	}
	for alias in graph.aliases.values() {
		visit_rule_fields(&alias.rules, visit);
	}
}

fn visit_rule_fields<F>(fields: &[CwtRuleField], visit: &mut F)
where
	F: FnMut(&CwtRuleField),
{
	for field in fields {
		visit(field);
		if let CwtRuleValue::Block(children) = &field.value {
			visit_rule_fields(children, visit);
		}
	}
}

pub fn cwt_file_kind_container_scope_kind(
	graph: &CwtSchemaGraph,
	file_kind: CwtType,
	key: &str,
) -> Option<ScopeKind> {
	if !is_legacy_container_scope_key(file_kind.as_str(), key) {
		return None;
	}
	if let Some(kind) = hand_container_scope_fallback(file_kind.clone(), key) {
		return Some(kind);
	}
	cwt_derived_container_scope_kind(graph, file_kind, key)
}

fn is_legacy_container_scope_key(file_kind: &str, key: &str) -> bool {
	match file_kind {
		"missions" => matches!(
			key,
			"potential_on_load"
				| "potential"
				| "trigger" | "provinces_to_highlight"
				| "completed_by"
				| "ai_weight"
				| "effect" | "on_completed"
				| "on_cancelled"
		),
		"new_diplomatic_actions" => {
			matches!(
				key,
				"is_visible"
					| "is_allowed" | "ai_will_do"
					| "on_accept" | "on_decline"
					| "add_entry"
			)
		}
		"events" => matches!(key, "mean_time_to_happen"),
		"ages" => matches!(
			key,
			"can_start" | "custom_trigger_tooltip" | "calc_true_if" | "ai_will_do" | "effect"
		),
		"buildings" => matches!(
			key,
			"ai_will_do"
				| "on_built" | "on_destroyed"
				| "on_construction_started"
				| "on_construction_canceled"
				| "on_obsolete"
		),
		"institutions" => matches!(
			key,
			"history"
				| "can_embrace"
				| "potential"
				| "custom_trigger_tooltip"
				| "on_start" | "embracement_speed"
				| "modifier"
		),
		"province_triggered_modifiers" => {
			matches!(
				key,
				"potential" | "trigger" | "on_activation" | "on_deactivation"
			)
		}
		"triggered_modifiers" => {
			matches!(
				key,
				"potential" | "trigger" | "on_activation" | "on_deactivation"
			)
		}
		"scripted_triggers" => matches!(key, "trigger" | "limit" | "custom_trigger_tooltip"),
		"ideas" => matches!(key, "start" | "bonus" | "trigger" | "ai_will_do"),
		"great_projects" => {
			matches!(
				key,
				"build_trigger"
					| "can_use_modifiers_trigger"
					| "can_upgrade_trigger"
					| "keep_trigger"
					| "on_built" | "on_destroyed"
					| "on_upgraded" | "on_downgraded"
					| "on_obtained" | "on_lost"
			)
		}
		"government_reforms" => {
			matches!(
				key,
				"on_enabled"
					| "on_disabled" | "on_enacted"
					| "on_removed" | "removed_effect"
					| "ai_will_do"
			)
		}
		"cb_types" => {
			matches!(
				key,
				"prerequisites_self" | "prerequisites" | "can_use" | "can_take_province"
			)
		}
		"government_names" => matches!(key, "trigger"),
		"customizable_localization" => matches!(key, "trigger"),
		"religions" => matches!(
			key,
			"potential" | "allow" | "ai_will_do" | "effect" | "on_convert"
		),
		"subject_types" => matches!(
			key,
			"is_potential_overlord"
				| "can_fight"
				| "can_rival"
				| "can_ally" | "can_marry"
				| "modifier_subject"
				| "modifier_overlord"
		),
		"rebel_types" => matches!(
			key,
			"spawn_chance"
				| "movement_evaluation"
				| "can_negotiate_trigger"
				| "can_enforce_trigger"
				| "siege_won_effect"
				| "demands_enforced_effect"
		),
		"disasters" => matches!(
			key,
			"potential"
				| "can_start"
				| "can_stop" | "can_end"
				| "on_start" | "on_end"
				| "on_monthly"
				| "progress" | "modifier"
		),
		"government_mechanics" => matches!(
			key,
			"available"
				| "trigger" | "on_max_reached"
				| "on_min_reached"
				| "powers" | "scaled_modifier"
				| "reverse_scaled_modifier"
				| "modifier"
		),
		"church_aspects" => matches!(
			key,
			"potential" | "trigger" | "ai_will_do" | "effect" | "modifier"
		),
		"factions" => matches!(key, "allow" | "modifier"),
		"hegemons" => matches!(key, "allow" | "base" | "scale" | "max"),
		"personal_deities" => matches!(
			key,
			"potential" | "trigger" | "ai_will_do" | "effect" | "removed_effect"
		),
		"fetishist_cults" => matches!(key, "allow" | "ai_will_do"),
		"peace_treaties" => matches!(
			key,
			"is_visible" | "is_allowed" | "ai_weight" | "effect" | "warscore_cost"
		),
		"policies" => matches!(
			key,
			"potential" | "allow" | "ai_will_do" | "effect" | "removed_effect"
		),
		"mercenary_companies" => matches!(key, "trigger" | "modifier"),
		"estate_agendas" => {
			matches!(
				key,
				"can_select"
					| "task_requirements"
					| "fail_if" | "invalid_trigger"
					| "provinces_to_highlight"
					| "selection_weight"
					| "pre_effect" | "immediate_effect"
					| "on_invalid" | "task_completed_effect"
			)
		}
		"estate_privileges" => {
			matches!(
				key,
				"is_valid"
					| "can_select" | "can_revoke"
					| "ai_will_do" | "on_granted"
					| "on_revoked" | "on_invalid"
					| "on_granted_province"
					| "on_revoked_province"
					| "on_invalid_province"
					| "on_cooldown_expires"
					| "benefits" | "penalties"
					| "modifier_by_land_ownership"
					| "mechanics" | "conditional_modifier"
					| "influence_scaled_conditional_modifier"
					| "loyalty_scaled_conditional_modifier"
			)
		}
		"estates" => matches!(
			key,
			"trigger"
				| "country_modifier_happy"
				| "country_modifier_neutral"
				| "country_modifier_angry"
				| "land_ownership_modifier"
				| "province_independence_weight"
				| "influence_modifier"
				| "loyalty_modifier"
				| "influence_from_dev_modifier"
		),
		"parliament_bribes" => matches!(key, "trigger" | "chance" | "ai_will_do" | "effect"),
		"parliament_issues" => matches!(
			key,
			"allow"
				| "chance" | "ai_will_do"
				| "effect" | "on_issue_taken"
				| "modifier" | "influence_scaled_modifier"
		),
		"state_edicts" => matches!(
			key,
			"potential" | "allow" | "notify_trigger" | "ai_will_do" | "modifier"
		),
		_ => false,
	}
}

fn cwt_derived_container_scope_kind(
	graph: &CwtSchemaGraph,
	file_kind: CwtType,
	key: &str,
) -> Option<ScopeKind> {
	let mut has_trigger = false;
	let mut has_effect = false;
	let mut has_block = false;
	for definition in file_kind_root_types(graph, &file_kind) {
		for field in file_kind_container_fields(graph, definition, key) {
			match cwt_container_field_scope_kind(graph, field) {
				Some(ScopeKind::Effect) => has_effect = true,
				Some(ScopeKind::Trigger) => has_trigger = true,
				Some(ScopeKind::Block) => has_block = true,
				_ => {}
			}
		}
	}
	if has_effect {
		Some(ScopeKind::Effect)
	} else if has_trigger {
		Some(ScopeKind::Trigger)
	} else if has_block {
		Some(ScopeKind::Block)
	} else {
		None
	}
}

fn file_kind_root_types<'g>(graph: &'g CwtSchemaGraph, file_kind: &CwtType) -> Vec<&'g CwtTypeDef> {
	let kind = file_kind.as_str();
	let mut matches = graph
		.types
		.values()
		.filter(|definition| {
			definition.name.as_str() == kind
				|| definition
					.path
					.as_deref()
					.is_some_and(|path| schema_path_matches_file_kind(path, kind))
		})
		.collect::<Vec<_>>();
	matches.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));
	matches
}

fn schema_path_matches_file_kind(path: &str, file_kind: &str) -> bool {
	let normalized = path
		.trim_start_matches("game/")
		.trim_matches('/')
		.to_ascii_lowercase();
	normalized == file_kind || normalized.rsplit('/').next() == Some(file_kind)
}

fn file_kind_container_fields<'g>(
	graph: &'g CwtSchemaGraph,
	definition: &'g CwtTypeDef,
	key: &str,
) -> Vec<&'g CwtRuleField> {
	let mut matches = Vec::new();
	collect_rule_set_matches(
		graph,
		BindContext::RootType(definition),
		definition.rules.as_slice(),
		key,
		&mut matches,
	);
	for subtype in &definition.subtypes {
		collect_rule_set_matches(
			graph,
			BindContext::AliasRules(subtype.rules.as_slice()),
			subtype.rules.as_slice(),
			key,
			&mut matches,
		);
	}
	matches
}

fn collect_rule_set_matches<'g>(
	graph: &'g CwtSchemaGraph,
	parent: BindContext<'g>,
	rules: &'g [CwtRuleField],
	key: &str,
	matches: &mut Vec<&'g CwtRuleField>,
) {
	matches.extend(graph.bind_fields(parent, key));
	for field in rules {
		let CwtRuleValue::Block(children) = &field.value else {
			continue;
		};
		collect_rule_set_matches(
			graph,
			BindContext::RuleField(field),
			children.as_slice(),
			key,
			matches,
		);
	}
}

fn cwt_container_field_scope_kind(
	graph: &CwtSchemaGraph,
	field: &CwtRuleField,
) -> Option<ScopeKind> {
	match &field.value {
		CwtRuleValue::Block(fields) => cwt_container_block_scope_kind(graph, fields),
		CwtRuleValue::Scalar(value) | CwtRuleValue::Marker(value) => {
			cwt_container_scalar_scope_kind(graph, value)
		}
	}
}

fn cwt_container_block_scope_kind(
	graph: &CwtSchemaGraph,
	fields: &[CwtRuleField],
) -> Option<ScopeKind> {
	let mut has_trigger = false;
	let mut has_effect = false;
	let mut has_block = false;
	for child in fields {
		match cwt_direct_child_scope_kind(graph, child) {
			Some(ScopeKind::Effect) => has_effect = true,
			Some(ScopeKind::Trigger) => has_trigger = true,
			Some(ScopeKind::Block) => has_block = true,
			_ => {}
		}
		if is_effect_like_child_key(&child.key) {
			has_effect = true;
		}
	}
	if has_effect {
		Some(ScopeKind::Effect)
	} else if has_trigger {
		Some(ScopeKind::Trigger)
	} else if has_block || !fields.is_empty() {
		Some(ScopeKind::Block)
	} else {
		None
	}
}

fn cwt_direct_child_scope_kind(graph: &CwtSchemaGraph, field: &CwtRuleField) -> Option<ScopeKind> {
	cwt_alias_marker_scope_kind(graph, &field.key)
}

fn cwt_container_scalar_scope_kind(graph: &CwtSchemaGraph, value: &str) -> Option<ScopeKind> {
	if is_event_like_reference(value) {
		return Some(ScopeKind::Effect);
	}
	if let Some(scope_kind) = cwt_alias_marker_scope_kind(graph, value) {
		return Some(scope_kind);
	}
	graph
		.types
		.get(&foch_cwt::CwtType::new(value))
		.and_then(|definition| cwt_container_block_scope_kind(graph, &definition.rules))
}

fn cwt_alias_marker_scope_kind(graph: &CwtSchemaGraph, marker: &str) -> Option<ScopeKind> {
	let (head, payload) = cwt_marker_parts(marker)?;
	match head {
		"alias_name" | "alias" => {
			let category = payload
				.split_once(':')
				.map_or(payload, |(category, _)| category);
			cwt_alias_category_scope_kind(graph, &AliasCategory::from_name(category))
		}
		_ => None,
	}
}

fn cwt_alias_category_scope_kind(
	graph: &CwtSchemaGraph,
	category: &AliasCategory,
) -> Option<ScopeKind> {
	match category {
		AliasCategory::Effect => Some(ScopeKind::Effect),
		AliasCategory::Trigger => Some(ScopeKind::Trigger),
		AliasCategory::Modifier => Some(ScopeKind::Block),
		AliasCategory::Link => None,
		AliasCategory::Other(_) => {
			let mut has_trigger = false;
			let mut has_effect = false;
			let mut has_block = false;
			for alias in graph
				.aliases
				.values()
				.filter(|alias| &alias.category == category)
			{
				match cwt_container_block_scope_kind(graph, &alias.rules) {
					Some(ScopeKind::Effect) => has_effect = true,
					Some(ScopeKind::Trigger) => has_trigger = true,
					Some(ScopeKind::Block) => has_block = true,
					_ => {}
				}
			}
			if has_effect {
				Some(ScopeKind::Effect)
			} else if has_trigger {
				Some(ScopeKind::Trigger)
			} else if has_block {
				Some(ScopeKind::Block)
			} else {
				None
			}
		}
	}
}

fn is_event_like_reference(value: &str) -> bool {
	value.starts_with("<event.") && value.ends_with('>')
}

fn is_effect_like_child_key(key: &str) -> bool {
	matches!(
		key,
		"effect"
			| "removed_effect"
			| "hidden_effect"
			| "immediate"
			| "after" | "on_add"
			| "on_remove"
			| "on_start"
			| "on_end"
			| "on_monthly"
	) || key.starts_with("on_")
		|| key.ends_with("_effect")
		|| is_builtin_effect(key)
}

/// Hand-maintained fallbacks for cwtools-eu4-config schema gaps.
///
/// These entries are the exact cases where the hybrid diff test found that
/// cwtools-eu4-config either lacks the relevant rule entirely or projects it in
/// a way that changes the legacy scope kind. To remove an entry from this list,
/// contribute the corresponding rule to cwtools-eu4-config upstream and bump the
/// vendored schema.
pub(super) fn hand_container_scope_fallback(file_kind: CwtType, key: &str) -> Option<ScopeKind> {
	match (file_kind.as_str(), key) {
		// cwtools-eu4-config gap: missions/missions.cwt declares `completed_by = date_field`
		// without enough schema signal to preserve the legacy trigger classification.
		("missions", "completed_by") => Some(ScopeKind::Trigger),
		// cwtools-eu4-config gap: missions/missions.cwt does not declare these mission
		// lifecycle effect containers.
		("missions", "on_completed" | "on_cancelled") => Some(ScopeKind::Effect),
		// cwtools-eu4-config gap: common/ages.cwt does not expose explicit fields for
		// these trigger containers.
		("ages", "custom_trigger_tooltip" | "calc_true_if") => Some(ScopeKind::Trigger),
		// cwtools-eu4-config gap: common/institutions.cwt only exposes nested trigger-like
		// content under `embracement_speed.modifier`, but legacy behavior treats the
		// outer `modifier` container as a plain block.
		("institutions", "modifier") => Some(ScopeKind::Block),
		// cwtools-eu4-config gap: common/00_small_types_consolidated.cwt does not declare
		// these top-level triggered modifier lifecycle effects for the `triggered_modifiers`
		// root family.
		("triggered_modifiers", "on_activation" | "on_deactivation") => Some(ScopeKind::Effect),
		// cwtools-eu4-config gap: common/scripted_triggers_and_effects.cwt does not model
		// these legacy scripted trigger containers directly.
		("scripted_triggers", "trigger" | "limit" | "custom_trigger_tooltip") => {
			Some(ScopeKind::Trigger)
		}
		// cwtools-eu4-config gap: common/ideas_and_native_advancements.cwt models `start`
		// as a modifier/ability block, but legacy semantics treat it as an effect container.
		("ideas", "start") => Some(ScopeKind::Effect),
		// cwtools-eu4-config gap: common/greatprojects.cwt does not declare these optional
		// lifecycle effect hooks.
		("great_projects", "on_downgraded" | "on_obtained" | "on_lost") => Some(ScopeKind::Effect),
		// cwtools-eu4-config gap: common/governments_and_reforms.cwt is missing these
		// reform lifecycle hooks and the legacy ai_will_do trigger container.
		("government_reforms", "on_enabled" | "on_disabled" | "on_enacted" | "on_removed") => {
			Some(ScopeKind::Effect)
		}
		("government_reforms", "ai_will_do") => Some(ScopeKind::Trigger),
		// cwtools-eu4-config gap: common/casus_belli_and_war_goals.cwt does not expose
		// these casus belli trigger containers.
		("cb_types", "can_use" | "can_take_province") => Some(ScopeKind::Trigger),
		// cwtools-eu4-config gap: common/subject_types.cwt models these relation checks as
		// structural blocks, but legacy semantics treat them as trigger containers.
		("subject_types", "can_fight" | "can_rival" | "can_ally" | "can_marry") => {
			Some(ScopeKind::Trigger)
		}
		// cwtools-eu4-config gap: common/disasters.cwt only surfaces a modifier_rule-style
		// block for `progress`, but legacy behavior keeps it as a plain block container.
		("disasters", "progress") => Some(ScopeKind::Block),
		// cwtools-eu4-config gap: common/peace_treaties.cwt models `ai_weight` as an AI
		// score block, but legacy behavior classifies it as a trigger container.
		("peace_treaties", "ai_weight") => Some(ScopeKind::Trigger),
		// cwtools-eu4-config gap: common/estates.cwt leaves `mechanics` empty, so schema
		// traversal cannot distinguish it from an untyped block without this fallback.
		("estate_privileges", "mechanics") => Some(ScopeKind::Block),
		// cwtools-eu4-config gap: common/estates.cwt projects these estate modifiers as a
		// modifier_rule block or scalar, but legacy behavior keeps them as plain blocks.
		("estates", "province_independence_weight" | "influence_from_dev_modifier") => {
			Some(ScopeKind::Block)
		}
		_ => None,
	}
}

pub fn scope_changer_target_type(key: &str) -> Option<ScopeType> {
	match key {
		"capital_scope" | "sea_zone" | "area_for_scope_province" | "region_for_scope_province" => {
			Some(base_scope::province())
		}
		"owner"
		| "controller"
		| "attacker_leader"
		| "defender_leader"
		| "emperor"
		| "colonial_parent"
		| "other_overlord"
		| "same_overlord"
		| "strongest_trade_power"
		| "unit_owner" => Some(base_scope::country()),
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
		|| key.starts_with("trade_company_")
}

#[cfg(test)]
mod tests {
	use super::iterator_scope_type;
	use foch_core::model::base_scope;

	#[test]
	fn iterator_scope_type_classifies_known_iterators() {
		for key in [
			"all_core_province",
			"any_owned_province",
			"every_neighbor_province",
			"random_province",
			"every_neighbor_sea_zone",
			"any_tribal_land",
		] {
			assert_eq!(
				iterator_scope_type(key),
				Some(base_scope::province()),
				"{key}"
			);
		}
		for key in [
			"all_ally",
			"any_country",
			"every_subject_country",
			"random_war_enemy_country",
			"any_hired_mercenary_company",
		] {
			assert_eq!(
				iterator_scope_type(key),
				Some(base_scope::country()),
				"{key}"
			);
		}
		for key in ["", "not_an_iterator", "owner", "any_known_country_xyz"] {
			assert_eq!(iterator_scope_type(key), None, "{key}");
		}
	}
}
