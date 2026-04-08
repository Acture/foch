use super::super::content_family::ContentFamilyDescriptor;
use super::super::parser::{AstValue, SpanRange};
use super::{
	BuildContext, extract_assignment_scalar, extract_block_scalar_items,
	extract_named_block_member_keys, extract_named_block_scalar_items,
	extract_nested_assignment_scalar, extract_yes_assignment_keys, is_country_file_reference,
	is_country_tag_text, is_named_block_in_top_level_block, is_province_id_text,
	is_top_level_named_block, monarch_power_prefix, province_name_table_id,
	push_resource_reference, random_map_tile_id, random_name_table_id, scalar_text, scope_kind,
};
use foch_core::model::{ScopeKind, SemanticIndex};

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

pub(super) trait ResourceExtractor: Send + Sync {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	);
}

// ---------------------------------------------------------------------------
// Shared: NamedDefinitionTable
// ---------------------------------------------------------------------------

struct NamedDefinitionTable {
	definition_key: &'static str,
	scalar_reference_keys: &'static [&'static str],
	block_reference_keys: &'static [&'static str],
}

impl ResourceExtractor for NamedDefinitionTable {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if is_top_level_named_block(index, scope_id, key, value) {
			push_resource_reference(index, ctx, key_span, self.definition_key, key);
		}
		if let Some(text) = scalar_text(value)
			&& self.scalar_reference_keys.contains(&key)
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
		if self.block_reference_keys.contains(&key) {
			for item in extract_block_scalar_items(value) {
				push_resource_reference(index, ctx, key_span, key, item.as_str());
			}
		}
	}
}

// ---------------------------------------------------------------------------
// Shared: ScalarRefExtractor
// ---------------------------------------------------------------------------

struct ScalarRefExtractor {
	check: fn(&str) -> bool,
}

impl ResourceExtractor for ScalarRefExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		_scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		let Some(text) = scalar_text(value) else {
			return;
		};
		if (self.check)(key) {
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
	}
}

// ---------------------------------------------------------------------------
// Shared: ScalarBlockRefExtractor
// ---------------------------------------------------------------------------

struct ScalarBlockRefExtractor {
	scalar_check: fn(&str) -> bool,
	block_ref_key: fn(&str) -> Option<&'static str>,
}

impl ResourceExtractor for ScalarBlockRefExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		_scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if let Some(text) = scalar_text(value)
			&& (self.scalar_check)(key)
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
		let Some(reference_key) = (self.block_ref_key)(key) else {
			return;
		};
		for item in extract_block_scalar_items(value) {
			push_resource_reference(index, ctx, key_span, reference_key, item.as_str());
		}
	}
}

// ---------------------------------------------------------------------------
// Shared: TopLevelNamedBlockExtractor
// ---------------------------------------------------------------------------

struct LocalisationSuffix {
	ref_key: &'static str,
	format: LocalisationFormat,
}

enum LocalisationFormat {
	Key,
	Prefix(&'static str),
	Suffix(&'static str),
}

struct TopLevelNamedBlockExtractor {
	localisation: &'static [LocalisationSuffix],
	extra_scalar_keys: &'static [&'static str],
}

impl ResourceExtractor for TopLevelNamedBlockExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if is_top_level_named_block(index, scope_id, key, value) {
			for spec in self.localisation {
				let value_str = match spec.format {
					LocalisationFormat::Key => key.to_string(),
					LocalisationFormat::Prefix(p) => format!("{p}{key}"),
					LocalisationFormat::Suffix(s) => format!("{key}{s}"),
				};
				push_resource_reference(index, ctx, key_span, spec.ref_key, &value_str);
			}
			return;
		}
		if !self.extra_scalar_keys.is_empty()
			&& let Some(text) = scalar_text(value)
			&& self.extra_scalar_keys.contains(&key)
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
	}
}

// ---------------------------------------------------------------------------
// Custom extractors
// ---------------------------------------------------------------------------

struct CountryTagsExtractor;
impl ResourceExtractor for CountryTagsExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		let Some(text) = scalar_text(value) else {
			return;
		};
		if scope_kind(index, scope_id) != ScopeKind::File
			|| !is_country_tag_selector(key)
			|| !is_country_file_reference(&text)
		{
			return;
		}
		push_resource_reference(
			index,
			ctx,
			key_span,
			&format!("country_tag:{key}"),
			text.as_str(),
		);
	}
}

struct CountryHistoryExtractor;
impl ResourceExtractor for CountryHistoryExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		_scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		let Some(text) = scalar_text(value) else {
			return;
		};
		if (is_country_history_province_reference_key(key) && is_province_id_text(&text))
			|| (is_country_history_country_reference_key(key) && is_country_tag_text(&text))
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
	}
}

struct ProvinceHistoryExtractor;
impl ResourceExtractor for ProvinceHistoryExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		_scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		let Some(text) = scalar_text(value) else {
			return;
		};
		if is_province_history_country_reference_key(key) && is_country_tag_text(&text) {
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
	}
}

struct WarsExtractor;
impl ResourceExtractor for WarsExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		_scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		let Some(text) = scalar_text(value) else {
			return;
		};
		if (is_war_history_country_reference_key(key) && is_country_tag_text(&text))
			|| (is_war_history_province_reference_key(key) && is_province_id_text(&text))
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
	}
}

struct CountriesExtractor;
impl ResourceExtractor for CountriesExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if scope_kind(index, scope_id) != ScopeKind::File {
			return;
		}
		if let Some(text) = scalar_text(value)
			&& is_country_metadata_scalar_reference_key(key)
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
		let Some(reference_key) = country_metadata_block_reference_key(key) else {
			return;
		};
		for item in extract_block_scalar_items(value) {
			push_resource_reference(index, ctx, key_span, reference_key, item.as_str());
		}
	}
}

struct ReligionsExtractor;
impl ResourceExtractor for ReligionsExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		_scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if let Some(text) = scalar_text(value)
			&& ((key == "center_of_religion" && is_province_id_text(&text))
				|| (key == "papal_tag" && is_country_tag_text(&text)))
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
		let Some(reference_key) = religion_block_reference_key(key) else {
			return;
		};
		for item in extract_block_scalar_items(value) {
			push_resource_reference(index, ctx, key_span, reference_key, item.as_str());
		}
	}
}

struct GovernmentMechanicsExtractor;
impl ResourceExtractor for GovernmentMechanicsExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		_scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if let Some(text) = scalar_text(value)
			&& is_government_mechanic_scalar_reference_key(key)
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
		if key != "country_event" {
			return;
		}
		for item in extract_named_block_scalar_items(value, "id") {
			push_resource_reference(index, ctx, key_span, key, item.as_str());
		}
	}
}

struct EstatePrivilegesExtractor;
impl ResourceExtractor for EstatePrivilegesExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		_scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if let Some(text) = scalar_text(value)
			&& is_estate_privilege_scalar_reference_key(key)
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
		if key != "mechanics" {
			return;
		}
		for item in extract_block_scalar_items(value) {
			push_resource_reference(index, ctx, key_span, key, item.as_str());
		}
	}
}

struct PeaceTreatiesExtractor;
impl ResourceExtractor for PeaceTreatiesExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if is_top_level_named_block(index, scope_id, key, value) {
			push_resource_reference(
				index,
				ctx,
				key_span,
				"localisation_desc",
				&format!("{key}_desc"),
			);
			push_resource_reference(
				index,
				ctx,
				key_span,
				"localisation_cb_allowed",
				&format!("CB_ALLOWED_{key}"),
			);
			push_resource_reference(
				index,
				ctx,
				key_span,
				"localisation_peace",
				&format!("PEACE_{key}"),
			);
		}
		let Some(text) = scalar_text(value) else {
			return;
		};
		if is_peace_treaty_scalar_reference_key(key) {
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
	}
}

struct BookmarksExtractor;
impl ResourceExtractor for BookmarksExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		_scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		let Some(text) = scalar_text(value) else {
			return;
		};
		if is_bookmark_localisation_reference_key(key)
			|| (key == "country" && is_country_tag_text(&text))
			|| (key == "center" && is_province_id_text(&text))
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
	}
}

struct MercenaryCompaniesExtractor;
impl ResourceExtractor for MercenaryCompaniesExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if is_top_level_named_block(index, scope_id, key, value) {
			push_resource_reference(index, ctx, key_span, "localisation", key);
			return;
		}
		if let Some(text) = scalar_text(value)
			&& is_mercenary_company_scalar_reference_key(key, text.as_str())
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
		if key != "sprites" {
			return;
		}
		for item in extract_block_scalar_items(value) {
			push_resource_reference(index, ctx, key_span, key, item.as_str());
		}
	}
}

struct TechnologiesExtractor;
impl ResourceExtractor for TechnologiesExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if scope_kind(index, scope_id) == ScopeKind::File && key == "monarch_power" {
			if let Some(text) = scalar_text(value) {
				ctx.technology_monarch_power = Some(text.clone());
				push_resource_reference(index, ctx, key_span, key, text.as_str());
			}
			return;
		}
		if scope_kind(index, scope_id) != ScopeKind::File || key != "technology" {
			return;
		}
		let Some(prefix) = ctx
			.technology_monarch_power
			.as_deref()
			.and_then(monarch_power_prefix)
		else {
			return;
		};
		let definition_key = format!("{prefix}_tech_{}", ctx.technology_definition_ordinal);
		ctx.technology_definition_ordinal += 1;
		push_resource_reference(
			index,
			ctx,
			key_span,
			"technology_definition",
			definition_key.as_str(),
		);
		for year in extract_named_block_scalar_items(value, "year") {
			push_resource_reference(index, ctx, key_span, "year", year.as_str());
		}
		for institution in extract_named_block_member_keys(value, "expects_institution") {
			push_resource_reference(
				index,
				ctx,
				key_span,
				"expects_institution",
				institution.as_str(),
			);
		}
		for enable in extract_yes_assignment_keys(value) {
			push_resource_reference(index, ctx, key_span, "enable", enable.as_str());
		}
	}
}

struct TechnologyGroupsExtractor;
impl ResourceExtractor for TechnologyGroupsExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if !is_named_block_in_top_level_block(index, scope_id, key, value) {
			return;
		}
		push_resource_reference(index, ctx, key_span, "technology_group", key);
		let AstValue::Block { items, .. } = value else {
			return;
		};
		for field in [
			"start_level",
			"start_cost_modifier",
			"nation_designer_unit_type",
		] {
			if let Some(text) = extract_assignment_scalar(items, field) {
				push_resource_reference(index, ctx, key_span, field, text.as_str());
			}
		}
		if let Some(cost_value) =
			extract_nested_assignment_scalar(items, "nation_designer_cost", "value")
		{
			push_resource_reference(
				index,
				ctx,
				key_span,
				"nation_designer_cost_value",
				cost_value.as_str(),
			);
		}
	}
}

struct DiplomacyHistoryExtractor;
impl ResourceExtractor for DiplomacyHistoryExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if scope_kind(index, scope_id) != ScopeKind::File {
			return;
		}
		if is_diplomacy_relation_block(key, value) {
			push_resource_reference(index, ctx, key_span, "relation_type", key);
			let AstValue::Block { items, .. } = value else {
				return;
			};
			for field in ["first", "second"] {
				let Some(text) = extract_assignment_scalar(items, field) else {
					continue;
				};
				if is_country_tag_text(&text) {
					push_resource_reference(index, ctx, key_span, field, text.as_str());
				}
			}
			return;
		}
		if !is_diplomacy_timeline_block(key, value) {
			return;
		}
		let AstValue::Block { items, .. } = value else {
			return;
		};
		for field in ["emperor", "celestial_emperor"] {
			let Some(text) = extract_assignment_scalar(items, field) else {
				continue;
			};
			if is_country_tag_text(&text) {
				push_resource_reference(index, ctx, key_span, field, text.as_str());
			}
		}
	}
}

struct AdvisorHistoryExtractor;
impl ResourceExtractor for AdvisorHistoryExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if scope_kind(index, scope_id) != ScopeKind::File || key != "advisor" {
			return;
		}
		let AstValue::Block { items, .. } = value else {
			return;
		};
		let Some(advisor_id) = extract_assignment_scalar(items, "advisor_id") else {
			return;
		};
		push_resource_reference(
			index,
			ctx,
			key_span,
			"advisor_definition",
			&format!("advisor_{advisor_id}"),
		);
		push_resource_reference(index, ctx, key_span, "advisor_id", advisor_id.as_str());
		if let Some(location) = extract_assignment_scalar(items, "location")
			&& is_province_id_text(&location)
		{
			push_resource_reference(index, ctx, key_span, "location", location.as_str());
		}
		if let Some(advisor_type) = extract_assignment_scalar(items, "type") {
			push_resource_reference(index, ctx, key_span, "type", advisor_type.as_str());
		}
		if let Some(culture) = extract_assignment_scalar(items, "culture") {
			push_resource_reference(index, ctx, key_span, "culture", culture.as_str());
		}
		if let Some(religion) = extract_assignment_scalar(items, "religion") {
			push_resource_reference(index, ctx, key_span, "religion", religion.as_str());
		}
	}
}

struct ProvinceNamesExtractor;
impl ResourceExtractor for ProvinceNamesExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if scope_kind(index, scope_id) != ScopeKind::File || !is_province_id_text(key) {
			return;
		}
		let Some(name) = scalar_text(value) else {
			return;
		};
		let Some(table) = province_name_table_id(ctx.path) else {
			return;
		};
		push_resource_reference(index, ctx, key_span, "province_name_table", table.as_str());
		push_resource_reference(index, ctx, key_span, "province_id", key);
		push_resource_reference(index, ctx, key_span, "province_name_literal", name.as_str());
	}
}

struct RandomMapTilesExtractor;
impl ResourceExtractor for RandomMapTilesExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if scope_kind(index, scope_id) != ScopeKind::File {
			return;
		}
		let Some(tile) = random_map_tile_id(ctx.path) else {
			return;
		};
		if !ctx.random_map_tile_emitted {
			push_resource_reference(index, ctx, key_span, "tile_definition", tile.as_str());
			ctx.random_map_tile_emitted = true;
		}
		if key == "size" {
			let values = extract_block_scalar_items(value);
			if !values.is_empty() {
				push_resource_reference(index, ctx, key_span, "tile_size", &values.join(","));
			}
			return;
		}
		let values = extract_block_scalar_items(value);
		if values.len() == 3 && values.iter().all(|item| item.parse::<u16>().is_ok()) {
			push_resource_reference(index, ctx, key_span, "tile_color_group", key);
			push_resource_reference(index, ctx, key_span, "tile_color_rgb", &values.join(","));
		}
	}
}

struct RandomMapNamesExtractor;
impl ResourceExtractor for RandomMapNamesExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if scope_kind(index, scope_id) != ScopeKind::File || key != "random_names" {
			return;
		}
		let Some(table) = random_name_table_id(ctx.path) else {
			return;
		};
		if !ctx.random_name_table_emitted {
			push_resource_reference(index, ctx, key_span, "random_name_table", table.as_str());
			ctx.random_name_table_emitted = true;
		}
		for entry in extract_block_scalar_items(value) {
			let (token, category) = entry
				.split_once(':')
				.map_or((entry.as_str(), None), |(token, category)| {
					(token, Some(category))
				});
			push_resource_reference(index, ctx, key_span, "random_name_token", token);
			if let Some(category) = category {
				push_resource_reference(index, ctx, key_span, "random_name_category", category);
			}
		}
	}
}

struct RandomMapScenariosExtractor;
impl ResourceExtractor for RandomMapScenariosExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if scope_kind(index, scope_id) != ScopeKind::File {
			return;
		}
		let AstValue::Block { items, .. } = value else {
			return;
		};
		push_resource_reference(index, ctx, key_span, "random_map_scenario", key);
		for field in [
			"culture_group",
			"religion",
			"technology_group",
			"government",
			"graphical_culture",
		] {
			let Some(text) = extract_assignment_scalar(items, field) else {
				continue;
			};
			push_resource_reference(index, ctx, key_span, field, text.as_str());
		}
		for name in extract_named_block_scalar_items(value, "names") {
			push_resource_reference(index, ctx, key_span, "scenario_name_key", name.as_str());
		}
	}
}

struct NewDiplomaticActionsExtractor;
impl ResourceExtractor for NewDiplomaticActionsExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if key == "static_actions" {
			return;
		}
		if is_top_level_named_block(index, scope_id, key, value) {
			push_resource_reference(
				index,
				ctx,
				key_span,
				"new_diplomatic_action_definition",
				key,
			);
		}
	}
}

struct CustomGuiExtractor;
impl ResourceExtractor for CustomGuiExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		if scope_kind(index, scope_id) != ScopeKind::File || !key.starts_with("custom_") {
			return;
		}
		let AstValue::Block { items, .. } = value else {
			return;
		};
		let Some(name) = extract_assignment_scalar(items, "name") else {
			return;
		};
		if !name.is_empty() {
			push_resource_reference(index, ctx, key_span, "custom_gui_definition", name.as_str());
		}
	}
}

struct FederationAdvancementsExtractor;
impl ResourceExtractor for FederationAdvancementsExtractor {
	fn extract(
		&self,
		index: &mut SemanticIndex,
		scope_id: usize,
		ctx: &mut BuildContext<'_>,
		key: &str,
		key_span: &SpanRange,
		value: &AstValue,
	) {
		static INNER: NamedDefinitionTable = NamedDefinitionTable {
			definition_key: "federation_advancement_definition",
			scalar_reference_keys: &[
				"cost_type",
				"gfx",
				"graphical_culture",
				"government",
				"icon",
				"localization",
				"religion",
				"technology_group",
				"tooltip",
				"custom_tooltip",
			],
			block_reference_keys: &["names"],
		};
		INNER.extract(index, scope_id, ctx, key, key_span, value);
		if let Some(text) = scalar_text(value)
			&& key == "tag"
			&& is_country_tag_text(&text)
		{
			push_resource_reference(index, ctx, key_span, key, text.as_str());
		}
	}
}

// ---------------------------------------------------------------------------
// Reference key helpers (moved from semantic_index)
// ---------------------------------------------------------------------------

fn is_country_tag_selector(key: &str) -> bool {
	key.len() == 3 && key.chars().all(|ch| ch.is_ascii_uppercase())
}

fn is_country_history_province_reference_key(key: &str) -> bool {
	matches!(key, "capital")
}

fn is_country_history_country_reference_key(key: &str) -> bool {
	matches!(key, "country_of_origin")
}

fn is_province_history_country_reference_key(key: &str) -> bool {
	matches!(key, "add_core" | "owner" | "controller")
}

fn is_war_history_country_reference_key(key: &str) -> bool {
	matches!(
		key,
		"add_attacker" | "add_defender" | "rem_attacker" | "rem_defender" | "country"
	)
}

fn is_war_history_province_reference_key(key: &str) -> bool {
	matches!(key, "location")
}

fn is_country_metadata_scalar_reference_key(key: &str) -> bool {
	matches!(
		key,
		"graphical_culture" | "second_graphical_culture" | "preferred_religion"
	)
}

fn country_metadata_block_reference_key(key: &str) -> Option<&'static str> {
	match key {
		"historical_idea_groups" => Some("historical_idea_groups"),
		"historical_units" => Some("historical_units"),
		_ => None,
	}
}

fn is_unit_definition_reference_key(key: &str) -> bool {
	matches!(key, "type" | "unit_type")
}

fn religion_block_reference_key(key: &str) -> Option<&'static str> {
	match key {
		"allowed_conversion" => Some("allowed_conversion"),
		"heretic" => Some("heretic"),
		_ => None,
	}
}

fn is_subject_type_reference_key(key: &str) -> bool {
	matches!(
		key,
		"copy_from"
			| "sprite"
			| "diplomacy_overlord_sprite"
			| "diplomacy_subject_sprite"
			| "overlord_opinion_modifier"
			| "subject_opinion_modifier"
	)
}

fn is_rebel_type_reference_key(key: &str) -> bool {
	matches!(key, "gfx_type" | "demands_description")
}

fn is_disaster_scalar_reference_key(key: &str) -> bool {
	matches!(key, "on_start" | "on_end" | "has_disaster")
}

fn disaster_block_reference_key(key: &str) -> Option<&'static str> {
	match key {
		"events" => Some("event"),
		"random_events" => Some("event"),
		_ => None,
	}
}

fn is_government_mechanic_scalar_reference_key(key: &str) -> bool {
	matches!(
		key,
		"gui" | "mechanic_type" | "power_type" | "custom_tooltip"
	)
}

fn is_peace_treaty_scalar_reference_key(key: &str) -> bool {
	matches!(key, "power_projection")
}

fn is_bookmark_localisation_reference_key(key: &str) -> bool {
	matches!(key, "name" | "desc")
}

fn is_mercenary_company_scalar_reference_key(key: &str, value: &str) -> bool {
	match key {
		"home_province" => is_province_id_text(value),
		"mercenary_desc_key" => true,
		"tag" => is_country_tag_text(value),
		_ => false,
	}
}

fn is_estate_agenda_scalar_reference_key(key: &str) -> bool {
	matches!(key, "estate" | "custom_tooltip" | "tooltip")
}

fn is_estate_privilege_scalar_reference_key(key: &str) -> bool {
	matches!(key, "icon" | "custom_tooltip" | "estate")
}

fn is_estate_scalar_reference_key(key: &str) -> bool {
	matches!(
		key,
		"custom_name" | "custom_desc" | "starting_reform" | "independence_government"
	)
}

fn estate_block_reference_key(key: &str) -> Option<&'static str> {
	match key {
		"privileges" => Some("privileges"),
		"agendas" => Some("agendas"),
		_ => None,
	}
}

fn is_parliament_bribe_scalar_reference_key(key: &str) -> bool {
	matches!(
		key,
		"name" | "estate" | "mechanic_type" | "power_type" | "type"
	)
}

fn is_parliament_issue_scalar_reference_key(key: &str) -> bool {
	matches!(
		key,
		"parliament_action" | "issue" | "estate" | "custom_tooltip"
	)
}

fn is_state_edict_scalar_reference_key(key: &str) -> bool {
	matches!(
		key,
		"tooltip" | "custom_trigger_tooltip" | "has_state_edict"
	)
}

fn is_diplomacy_relation_block(key: &str, value: &AstValue) -> bool {
	matches!(
		key,
		"alliance" | "vassal" | "royal_marriage" | "union" | "dependency" | "guarantee" | "march"
	) && matches!(value, AstValue::Block { .. })
}

fn is_diplomacy_timeline_block(key: &str, value: &AstValue) -> bool {
	matches!(value, AstValue::Block { .. }) && is_clausewitz_date_key(key)
}

fn is_clausewitz_date_key(key: &str) -> bool {
	let mut parts = key.split('.');
	let Some(year) = parts.next() else {
		return false;
	};
	let Some(month) = parts.next() else {
		return false;
	};
	let Some(day) = parts.next() else {
		return false;
	};
	if parts.next().is_some() {
		return false;
	}
	year.parse::<u32>().is_ok()
		&& month
			.parse::<u32>()
			.is_ok_and(|value| (1..=12).contains(&value))
		&& day
			.parse::<u32>()
			.is_ok_and(|value| (1..=31).contains(&value))
}

// ---------------------------------------------------------------------------
// Static extractor instances
// ---------------------------------------------------------------------------

static FERVOR: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "fervor_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &[],
};

static DECREES: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "decree_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &[],
};

static GOLDEN_BULLS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "golden_bull_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &["mechanics"],
};

static FLAGSHIP_MODIFICATIONS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "flagship_modification_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &[],
};

static HOLY_ORDERS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "holy_order_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &[],
};

static DIPLOMATIC_ACTIONS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "diplomatic_action_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static SCRIPTED_TRIGGERS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "scripted_trigger_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static AGES: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "age_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static BUILDINGS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "building_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static INSTITUTIONS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "institution_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static ADVISOR_TYPES: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "advisor_type_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static GOVERNMENT_NAMES: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "government_name_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static EVENT_MODIFIERS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "event_modifier_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static PROVINCE_TRIGGERED_MODIFIERS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "province_triggered_modifier_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static CB_TYPES: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "cb_type_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static IDEAS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "idea_group_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static GOVERNMENT_REFORMS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "government_reform_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static NAVAL_DOCTRINES: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "naval_doctrine_definition",
	scalar_reference_keys: &[
		"button_gfx",
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &[],
};

static DEFENDER_OF_FAITH: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "defender_of_faith_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &[],
};

static ISOLATIONISM: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "isolationism_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &[],
};

static PROFESSIONALISM: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "professionalism_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"marker_sprite",
		"tooltip",
		"custom_tooltip",
		"unit_sprite_start",
	],
	block_reference_keys: &[],
};

static POWERPROJECTION: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "powerprojection_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &[],
};

static SUBJECT_TYPE_UPGRADES: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "subject_type_upgrade_definition",
	scalar_reference_keys: &[
		"cost_type",
		"gfx",
		"icon",
		"localization",
		"tooltip",
		"custom_tooltip",
	],
	block_reference_keys: &[],
};

static GOVERNMENT_RANKS: NamedDefinitionTable = NamedDefinitionTable {
	definition_key: "government_rank_definition",
	scalar_reference_keys: &[],
	block_reference_keys: &[],
};

static UNITS: ScalarRefExtractor = ScalarRefExtractor {
	check: is_unit_definition_reference_key,
};

static SUBJECT_TYPES: ScalarRefExtractor = ScalarRefExtractor {
	check: is_subject_type_reference_key,
};

static REBEL_TYPES: ScalarRefExtractor = ScalarRefExtractor {
	check: is_rebel_type_reference_key,
};

static ESTATE_AGENDAS: ScalarRefExtractor = ScalarRefExtractor {
	check: is_estate_agenda_scalar_reference_key,
};

static PARLIAMENT_BRIBES: ScalarRefExtractor = ScalarRefExtractor {
	check: is_parliament_bribe_scalar_reference_key,
};

static PARLIAMENT_ISSUES: ScalarRefExtractor = ScalarRefExtractor {
	check: is_parliament_issue_scalar_reference_key,
};

static STATE_EDICTS: ScalarRefExtractor = ScalarRefExtractor {
	check: is_state_edict_scalar_reference_key,
};

static DISASTERS: ScalarBlockRefExtractor = ScalarBlockRefExtractor {
	scalar_check: is_disaster_scalar_reference_key,
	block_ref_key: disaster_block_reference_key,
};

static ESTATES: ScalarBlockRefExtractor = ScalarBlockRefExtractor {
	scalar_check: is_estate_scalar_reference_key,
	block_ref_key: estate_block_reference_key,
};

static CHURCH_ASPECTS: TopLevelNamedBlockExtractor = TopLevelNamedBlockExtractor {
	localisation: &[
		LocalisationSuffix {
			ref_key: "localisation",
			format: LocalisationFormat::Key,
		},
		LocalisationSuffix {
			ref_key: "localisation_desc",
			format: LocalisationFormat::Prefix("desc_"),
		},
		LocalisationSuffix {
			ref_key: "localisation_modifier",
			format: LocalisationFormat::Suffix("_modifier"),
		},
	],
	extra_scalar_keys: &[],
};

static HEGEMONS: TopLevelNamedBlockExtractor = TopLevelNamedBlockExtractor {
	localisation: &[LocalisationSuffix {
		ref_key: "localisation",
		format: LocalisationFormat::Key,
	}],
	extra_scalar_keys: &[],
};

static PERSONAL_DEITIES: TopLevelNamedBlockExtractor = TopLevelNamedBlockExtractor {
	localisation: &[
		LocalisationSuffix {
			ref_key: "localisation",
			format: LocalisationFormat::Key,
		},
		LocalisationSuffix {
			ref_key: "localisation_desc",
			format: LocalisationFormat::Suffix("_desc"),
		},
	],
	extra_scalar_keys: &[],
};

static FETISHIST_CULTS: TopLevelNamedBlockExtractor = TopLevelNamedBlockExtractor {
	localisation: &[
		LocalisationSuffix {
			ref_key: "localisation",
			format: LocalisationFormat::Key,
		},
		LocalisationSuffix {
			ref_key: "localisation_desc",
			format: LocalisationFormat::Suffix("_desc"),
		},
	],
	extra_scalar_keys: &[],
};

static FACTIONS: TopLevelNamedBlockExtractor = TopLevelNamedBlockExtractor {
	localisation: &[
		LocalisationSuffix {
			ref_key: "localisation",
			format: LocalisationFormat::Key,
		},
		LocalisationSuffix {
			ref_key: "localisation_influence",
			format: LocalisationFormat::Suffix("_influence"),
		},
	],
	extra_scalar_keys: &["monarch_power"],
};

static POLICIES: TopLevelNamedBlockExtractor = TopLevelNamedBlockExtractor {
	localisation: &[LocalisationSuffix {
		ref_key: "localisation",
		format: LocalisationFormat::Key,
	}],
	extra_scalar_keys: &["monarch_power"],
};

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

pub(super) fn extractor_for(
	descriptor: &ContentFamilyDescriptor,
) -> Option<&'static dyn ResourceExtractor> {
	match descriptor.id {
		// NamedDefinitionTable
		"common/fervor" => Some(&FERVOR),
		"common/decrees" => Some(&DECREES),
		"common/golden_bulls" => Some(&GOLDEN_BULLS),
		"common/flagship_modifications" => Some(&FLAGSHIP_MODIFICATIONS),
		"common/holy_orders" => Some(&HOLY_ORDERS),
		"common/diplomatic_actions" => Some(&DIPLOMATIC_ACTIONS),
		"common/scripted_triggers" => Some(&SCRIPTED_TRIGGERS),
		"common/ages" => Some(&AGES),
		"common/buildings" => Some(&BUILDINGS),
		"common/institutions" => Some(&INSTITUTIONS),
		"common/advisortypes" => Some(&ADVISOR_TYPES),
		"common/government_names" => Some(&GOVERNMENT_NAMES),
		"common/custom_gui" => Some(&CustomGuiExtractor),
		"common/event_modifiers" => Some(&EVENT_MODIFIERS),
		"common/province_triggered_modifiers" => Some(&PROVINCE_TRIGGERED_MODIFIERS),
		"common/cb_types" => Some(&CB_TYPES),
		"common/ideas" => Some(&IDEAS),
		"common/government_reforms" => Some(&GOVERNMENT_REFORMS),
		"common/naval_doctrines" => Some(&NAVAL_DOCTRINES),
		"common/defender_of_faith" => Some(&DEFENDER_OF_FAITH),
		"common/isolationism" => Some(&ISOLATIONISM),
		"common/professionalism" => Some(&PROFESSIONALISM),
		"common/powerprojection" => Some(&POWERPROJECTION),
		"common/subject_type_upgrades" => Some(&SUBJECT_TYPE_UPGRADES),
		"common/government_ranks" => Some(&GOVERNMENT_RANKS),
		"common/new_diplomatic_actions" => Some(&NewDiplomaticActionsExtractor),
		"common/federation_advancements" => Some(&FederationAdvancementsExtractor),

		// ScalarRefExtractor
		"common/units" => Some(&UNITS),
		"common/subject_types" => Some(&SUBJECT_TYPES),
		"common/rebel_types" => Some(&REBEL_TYPES),
		"common/estate_agendas" => Some(&ESTATE_AGENDAS),
		"common/parliament_bribes" => Some(&PARLIAMENT_BRIBES),
		"common/parliament_issues" => Some(&PARLIAMENT_ISSUES),
		"common/state_edicts" => Some(&STATE_EDICTS),

		// ScalarBlockRefExtractor
		"common/disasters" => Some(&DISASTERS),
		"common/estates" => Some(&ESTATES),

		// TopLevelNamedBlockExtractor
		"common/church_aspects" => Some(&CHURCH_ASPECTS),
		"common/hegemons" => Some(&HEGEMONS),
		"common/personal_deities" => Some(&PERSONAL_DEITIES),
		"common/fetishist_cults" => Some(&FETISHIST_CULTS),
		"common/factions" => Some(&FACTIONS),
		"common/policies" => Some(&POLICIES),

		// Custom extractors
		"common/country_tags" => Some(&CountryTagsExtractor),
		"common/countries" => Some(&CountriesExtractor),
		"history/countries" => Some(&CountryHistoryExtractor),
		"history/provinces" => Some(&ProvinceHistoryExtractor),
		"history/wars" => Some(&WarsExtractor),
		"common/religions" => Some(&ReligionsExtractor),
		"common/government_mechanics" => Some(&GovernmentMechanicsExtractor),
		"common/estate_privileges" => Some(&EstatePrivilegesExtractor),
		"common/peace_treaties" => Some(&PeaceTreatiesExtractor),
		"common/bookmarks" => Some(&BookmarksExtractor),
		"common/mercenary_companies" => Some(&MercenaryCompaniesExtractor),
		"common/technologies" => Some(&TechnologiesExtractor),
		"common/technology" => Some(&TechnologyGroupsExtractor),
		"history/diplomacy" => Some(&DiplomacyHistoryExtractor),
		"history/advisors" => Some(&AdvisorHistoryExtractor),
		"common/province_names" => Some(&ProvinceNamesExtractor),
		"map/random/tiles" => Some(&RandomMapTilesExtractor),
		"map/random_names" => Some(&RandomMapNamesExtractor),
		"map/random/scenarios" => Some(&RandomMapScenariosExtractor),

		_ => None,
	}
}
