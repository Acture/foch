use super::content_family::{
	BlockMergePolicy, BlockPatchPolicy, BooleanMergePolicy, ConflictPolicy,
	ContentFamilyCapabilities, ContentFamilyDescriptor, ContentFamilyPathMatcher,
	ContentFamilyScopePolicy, ContentLoadPolicy, CwtType, DedupPolicy, DefinitionFileOrder,
	DefinitionKeyPolicy, DefinitionModuleOutput, DefinitionModulePolicy, DuplicateDefinitionPolicy,
	GameId, GameProfile, ListMergePolicy, MergeKeySource, ModuleNameRule, OneSidedRemovalPolicy,
	ScalarMergePolicy, ScalarReducerRule,
};
use super::eu4_builtin::builtin_base_scope_names;
use foch_core::model::{MaybeScope, ScopeType, base_scope};
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug)]
pub struct Eu4Profile;

static EU4_PROFILE: Eu4Profile = Eu4Profile;

const TRADEGOODS_SCALAR_REDUCER_RULES: &[ScalarReducerRule] = &[
	ScalarReducerRule::new(&["global_colonial_growth"], ScalarMergePolicy::Max),
	ScalarReducerRule::new(&["province_trade_power_modifier"], ScalarMergePolicy::Avg),
];

fn ensure_base_scopes_initialized() {
	if base_scope::is_initialized() {
		return;
	}
	let (country, province) = builtin_base_scope_names();
	base_scope::init_base_scopes(country, province);
}

const fn semantic_complete_and_merge_ready() -> ContentFamilyCapabilities {
	ContentFamilyCapabilities {
		semantic_complete: true,
		graph_ready: false,
		merge_ready: true,
		dedup_policy: DedupPolicy::None,
	}
}

const EU4_GOVERNMENTS_MODULE_POLICY: DefinitionModulePolicy = DefinitionModulePolicy {
	definition_key: DefinitionKeyPolicy::AssignmentKey,
	file_order: DefinitionFileOrder::NormalizedPathAscending,
	duplicate_definitions: DuplicateDefinitionPolicy::LaterDefinitionWins,
	output_path: "common/governments/zzz_foch_governments.txt",
	namespace_prefix: "common/governments",
	output_mode: DefinitionModuleOutput::ReplaceNamespace,
	policy_version: 1,
};

fn enable_common_definition_modules(families: &mut [ContentFamilyDescriptor]) {
	for descriptor in families {
		if descriptor.load_policy != ContentLoadPolicy::PerPath
			|| !matches!(
				descriptor.merge_key_source,
				Some(
					MergeKeySource::AssignmentKey
						| MergeKeySource::FieldValue(_)
						| MergeKeySource::ChildFieldValue { .. }
				)
			) {
			continue;
		}
		let ContentFamilyPathMatcher::Prefix(prefix) = descriptor.matcher else {
			continue;
		};
		if !prefix.starts_with("common/") || !prefix.ends_with('/') {
			continue;
		}
		if matches!(prefix, "common/countries/" | "common/units/") {
			continue;
		}

		let namespace_prefix = prefix.trim_end_matches('/');
		let module_slug = namespace_prefix.rsplit('/').next().unwrap_or("definitions");
		let output_path =
			Box::leak(format!("{namespace_prefix}/zzz_foch_{module_slug}.txt").into_boxed_str());
		descriptor.load_policy = ContentLoadPolicy::DefinitionModule(DefinitionModulePolicy {
			definition_key: DefinitionKeyPolicy::AssignmentKey,
			file_order: DefinitionFileOrder::NormalizedPathAscending,
			duplicate_definitions: DuplicateDefinitionPolicy::PreserveAll,
			output_path,
			namespace_prefix,
			output_mode: DefinitionModuleOutput::Overlay,
			policy_version: 2,
		});
	}
}

/// Safe for EU4 families whose definitions live in a global runtime namespace:
/// event ids, decision ids, scripted effect names, and scripted trigger names
/// are resolved by key, not by the file that provided an identical body.
const fn semantic_complete_merge_ready_cross_file_dedup_safe() -> ContentFamilyCapabilities {
	ContentFamilyCapabilities {
		semantic_complete: true,
		graph_ready: false,
		merge_ready: true,
		dedup_policy: DedupPolicy::CrossFileOnly,
	}
}

fn scope(root_scope: ScopeType) -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope: root_scope.into(),
		from_alias: None,
		dynamic_scope: false,
	}
}

fn unknown_scope() -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope: MaybeScope::Unknown,
		from_alias: None,
		dynamic_scope: false,
	}
}

fn country_from_scope(root_scope: ScopeType) -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope: root_scope.into(),
		from_alias: Some(base_scope::country()),
		dynamic_scope: false,
	}
}

/// Scope policy for content families whose top-level keys are not themselves
/// scoped (e.g. `common/religions` groups religions, religions in turn group
/// nested trigger/effect blocks). Within those nested blocks the engine pushes
/// the appropriate scope, but FROM is virtually always a country event source
/// (e.g. `on_convert`, `religious_schools/*/can_invite_scholar`).
fn country_from_only() -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope: MaybeScope::Unknown,
		from_alias: Some(base_scope::country()),
		dynamic_scope: false,
	}
}

/// Scope policy for content families whose implicit scope is supplied by the
/// runtime caller (callables, UI bindings, customizable localization,
/// on_actions callbacks). unknown-scope-type (uncertain-scope path) skips usages inside
/// these files because Unknown is the by-design state.
fn dynamic_scope_policy() -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope: MaybeScope::Unknown,
		from_alias: None,
		dynamic_scope: true,
	}
}

const COUNTRY_HISTORY_BLOCK_PATCH_POLICIES: &[(&str, BlockPatchPolicy)] = &[
	("monarch_names", BlockPatchPolicy::Union),
	("leader_names", BlockPatchPolicy::Union),
	("ship_names", BlockPatchPolicy::Union),
	("army_names", BlockPatchPolicy::Union),
];

const GUI_TYPES_NAMED_CHILD_TYPES: &[&str] = &[
	"windowType",
	"WindowType",
	"containerWindowType",
	"instantTextBoxType",
	"instantTextboxType",
	"iconType",
	"spriteType",
	"textSpriteType",
	"corneredTileSpriteType",
	"progressbartype",
	"frameAnimatedSpriteType",
	"maskedShieldType",
	"OverlappingElementsBoxType",
	"overlappingElementsBoxType",
	"lineChartType",
	"LineChartType",
	"PieChartType",
	"arrowType",
	"guiButtonType",
	"buttonType",
	"ButtonType",
	"editBoxType",
	"textBoxType",
	"scrollbarType",
	"extendedScrollbarType",
	"barType",
	"tabType",
	"positionType",
	"checkboxType",
	"listBoxType",
	"listboxType",
	"smoothListBoxType",
	"smoothListboxType",
	"gridBoxType",
	"eu3dialogtype",
	"bitmapfont",
	"objectType",
	"cursorType",
];

fn eu4_content_families() -> &'static [ContentFamilyDescriptor] {
	static EU4_CONTENT_FAMILIES: OnceLock<Box<[ContentFamilyDescriptor]>> = OnceLock::new();
	ensure_base_scopes_initialized();
	EU4_CONTENT_FAMILIES.get_or_init(|| {
		let mut families = vec![
			ContentFamilyDescriptor::prefix(
				"events/common/new_diplomatic_actions",
				"events/common/new_diplomatic_actions/",
			)
			.kind(CwtType::new("new_diplomatic_actions"))
			.module_name(ModuleNameRule::Tail {
				prefix_len: 2,
				fallback: "new_diplomatic_actions",
			})
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/on_actions", "common/on_actions/")
				.kind(CwtType::new("on_actions"))
				.module_name(ModuleNameRule::Static("on_actions"))
				.scope(dynamic_scope_policy())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"events/common/on_actions",
				"events/common/on_actions/",
			)
			.kind(CwtType::new("on_actions"))
			.module_name(ModuleNameRule::Static("on_actions"))
			.scope(dynamic_scope_policy())
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("events/decisions", "events/decisions/")
				.kind(CwtType::new("decisions"))
				.module_name(ModuleNameRule::Static("decisions"))
				.scope(scope(base_scope::country()))
				// Cross-file dedup is safe because decision ids are global. Per-entry
				// dedup is not: generated output reuses the source path and shadows vanilla.
				.capabilities(semantic_complete_merge_ready_cross_file_dedup_safe())
				.merge_key(MergeKeySource::ContainerChildKey)
				.scalar_policy(ScalarMergePolicy::LastWriter)
				.boolean_policy(BooleanMergePolicy::And)
				.build(),
			ContentFamilyDescriptor::prefix("events", "events/")
				.kind(CwtType::new("events"))
				.module_name(ModuleNameRule::Static("events"))
				.scope(unknown_scope())
				// Cross-file dedup is safe because event ids are global. Per-entry
				// dedup is not: generated output reuses the source path and shadows vanilla.
				.capabilities(semantic_complete_merge_ready_cross_file_dedup_safe())
				.merge_key(MergeKeySource::FieldValue("id"))
				.nested_merge_key(MergeKeySource::ChildFieldValue {
					child_key_field: "name",
					child_types: &["option"],
				})
				.edit_wins_over_remove()
				.scalar_policy(ScalarMergePolicy::LastWriter)
				.list_policy(ListMergePolicy::UnionWithRename)
				.one_sided_removal_policy(OneSidedRemovalPolicy::PreserveAdditiveStructure)
				.boolean_policy(BooleanMergePolicy::And)
				.build(),
			ContentFamilyDescriptor::prefix("decisions", "decisions/")
				.kind(CwtType::new("decisions"))
				.module_name(ModuleNameRule::Static("decisions"))
				.scope(scope(base_scope::country()))
				// Cross-file dedup is safe because decision ids are global. Per-entry
				// dedup is not: generated output reuses the source path and shadows vanilla.
				.capabilities(semantic_complete_merge_ready_cross_file_dedup_safe())
				.merge_key(MergeKeySource::ContainerChildKey)
				.scalar_policy(ScalarMergePolicy::LastWriter)
				.boolean_policy(BooleanMergePolicy::And)
				.build(),
			ContentFamilyDescriptor::prefix("common/scripted_effects", "common/scripted_effects/")
				.kind(CwtType::new("scripted_effects"))
				.module_name(ModuleNameRule::Tail {
					prefix_len: 2,
					fallback: "scripted_effects",
				})
				.scope(dynamic_scope_policy())
				// Safe: scripted effect names are global call targets across files;
				// omitting a vanilla-equivalent generated effect leaves the vanilla effect active.
				.capabilities(semantic_complete_merge_ready_cross_file_dedup_safe())
				.per_entry_dedup_safe()
				.merge_key(MergeKeySource::AssignmentKey)
				.block_patch_policy(BlockPatchPolicy::Union)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/scripted_triggers",
				"common/scripted_triggers/",
			)
			.kind(CwtType::new("scripted_triggers"))
			.module_name(ModuleNameRule::Tail {
				prefix_len: 2,
				fallback: "scripted_triggers",
			})
			.scope(dynamic_scope_policy())
			// Safe: scripted trigger names are global call targets across files;
			// omitting a vanilla-equivalent generated trigger leaves the vanilla trigger active.
			.capabilities(semantic_complete_merge_ready_cross_file_dedup_safe())
			.per_entry_dedup_safe()
			.merge_key(MergeKeySource::AssignmentKey)
			.conflict_policy(ConflictPolicy::BooleanOr)
			.block_patch_policy(BlockPatchPolicy::BooleanOr)
			.build(),
			ContentFamilyDescriptor::prefix(
				"common/triggered_modifiers",
				"common/triggered_modifiers/",
			)
			.kind(CwtType::new("triggered_modifiers"))
			.module_name(ModuleNameRule::Tail {
				prefix_len: 2,
				fallback: "triggered_modifiers",
			})
			.scope(scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/defines", "common/defines/")
				.kind(CwtType::new("defines"))
				.module_name(ModuleNameRule::Tail {
					prefix_len: 2,
					fallback: "defines",
				})
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::LeafPath)
				.conflict_policy(ConflictPolicy::MergeLeaf)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/diplomatic_actions",
				"common/diplomatic_actions/",
			)
			.kind(CwtType::new("diplomatic_actions"))
			.module_name(ModuleNameRule::Tail {
				prefix_len: 2,
				fallback: "diplomatic_actions",
			})
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix(
				"common/new_diplomatic_actions",
				"common/new_diplomatic_actions/",
			)
			.kind(CwtType::new("new_diplomatic_actions"))
			.module_name(ModuleNameRule::Tail {
				prefix_len: 2,
				fallback: "new_diplomatic_actions",
			})
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/country_tags", "common/country_tags/")
				.kind(CwtType::new("country_tags"))
				.module_name(ModuleNameRule::Static("country_tags"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/countries", "common/countries/")
				.kind(CwtType::new("countries"))
				.module_name(ModuleNameRule::Static("countries"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("history/countries", "history/countries/")
				.kind(CwtType::new("country_history"))
				.module_name(ModuleNameRule::Static("country_history"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.block_patch_policies(COUNTRY_HISTORY_BLOCK_PATCH_POLICIES)
				.build(),
			ContentFamilyDescriptor::prefix("history/provinces", "history/provinces/")
				.kind(CwtType::new("province_history"))
				.module_name(ModuleNameRule::Static("province_history"))
				.scope(scope(base_scope::province()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("history/diplomacy", "history/diplomacy/")
				.kind(CwtType::new("diplomacy_history"))
				.module_name(ModuleNameRule::Static("diplomacy_history"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("history/advisors", "history/advisors/")
				.kind(CwtType::new("advisor_history"))
				.module_name(ModuleNameRule::Static("advisor_history"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("history/wars", "history/wars/")
				.kind(CwtType::new("wars"))
				.module_name(ModuleNameRule::Static("wars"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/units", "common/units/")
				.kind(CwtType::new("units"))
				.module_name(ModuleNameRule::Static("units"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/religions", "common/religions/")
				.kind(CwtType::new("religions"))
				.module_name(ModuleNameRule::Static("religions"))
				.scope(country_from_only())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_policy(ScalarMergePolicy::LastWriter)
				.list_policy(ListMergePolicy::Replace)
				.build(),
			ContentFamilyDescriptor::prefix("common/subject_types", "common/subject_types/")
				.kind(CwtType::new("subject_types"))
				.module_name(ModuleNameRule::Static("subject_types"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/rebel_types", "common/rebel_types/")
				.kind(CwtType::new("rebel_types"))
				.module_name(ModuleNameRule::Static("rebel_types"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/disasters", "common/disasters/")
				.kind(CwtType::new("disasters"))
				.module_name(ModuleNameRule::Static("disasters"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_policy(ScalarMergePolicy::Sum)
				.boolean_policy(BooleanMergePolicy::And)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/government_mechanics",
				"common/government_mechanics/",
			)
			.kind(CwtType::new("government_mechanics"))
			.module_name(ModuleNameRule::Static("government_mechanics"))
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/church_aspects", "common/church_aspects/")
				.kind(CwtType::new("church_aspects"))
				.module_name(ModuleNameRule::Static("church_aspects"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/factions", "common/factions/")
				.kind(CwtType::new("factions"))
				.module_name(ModuleNameRule::Static("factions"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/hegemons", "common/hegemons/")
				.kind(CwtType::new("hegemons"))
				.module_name(ModuleNameRule::Static("hegemons"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/personal_deities", "common/personal_deities/")
				.kind(CwtType::new("personal_deities"))
				.module_name(ModuleNameRule::Static("personal_deities"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/fetishist_cults", "common/fetishist_cults/")
				.kind(CwtType::new("fetishist_cults"))
				.module_name(ModuleNameRule::Static("fetishist_cults"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/peace_treaties", "common/peace_treaties/")
				.kind(CwtType::new("peace_treaties"))
				.module_name(ModuleNameRule::Static("peace_treaties"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/bookmarks", "common/bookmarks/")
				.kind(CwtType::new("bookmarks"))
				.module_name(ModuleNameRule::Static("bookmarks"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/policies", "common/policies/")
				.kind(CwtType::new("policies"))
				.module_name(ModuleNameRule::Static("policies"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_policy(ScalarMergePolicy::Sum)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/mercenary_companies",
				"common/mercenary_companies/",
			)
			.kind(CwtType::new("mercenary_companies"))
			.module_name(ModuleNameRule::Static("mercenary_companies"))
			.scope(scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.scalar_policy(ScalarMergePolicy::Sum)
			.build(),
			ContentFamilyDescriptor::prefix("common/fervor", "common/fervor/")
				.kind(CwtType::new("fervor"))
				.module_name(ModuleNameRule::Static("fervor"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/decrees", "common/decrees/")
				.kind(CwtType::new("decrees"))
				.module_name(ModuleNameRule::Static("decrees"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/federation_advancements",
				"common/federation_advancements/",
			)
			.kind(CwtType::new("federation_advancements"))
			.module_name(ModuleNameRule::Static("federation_advancements"))
			.scope(scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/golden_bulls", "common/golden_bulls/")
				.kind(CwtType::new("golden_bulls"))
				.module_name(ModuleNameRule::Static("golden_bulls"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/flagship_modifications",
				"common/flagship_modifications/",
			)
			.kind(CwtType::new("flagship_modifications"))
			.module_name(ModuleNameRule::Static("flagship_modifications"))
			.scope(scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/holy_orders", "common/holy_orders/")
				.kind(CwtType::new("holy_orders"))
				.module_name(ModuleNameRule::Static("holy_orders"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_policy(ScalarMergePolicy::Sum)
				.build(),
			ContentFamilyDescriptor::prefix("common/naval_doctrines", "common/naval_doctrines/")
				.kind(CwtType::new("naval_doctrines"))
				.module_name(ModuleNameRule::Static("naval_doctrines"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/defender_of_faith",
				"common/defender_of_faith/",
			)
			.kind(CwtType::new("defender_of_faith"))
			.module_name(ModuleNameRule::Static("defender_of_faith"))
			.scope(scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/isolationism", "common/isolationism/")
				.kind(CwtType::new("isolationism"))
				.module_name(ModuleNameRule::Static("isolationism"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/professionalism", "common/professionalism/")
				.kind(CwtType::new("professionalism"))
				.module_name(ModuleNameRule::Static("professionalism"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/powerprojection", "common/powerprojection/")
				.kind(CwtType::new("powerprojection"))
				.module_name(ModuleNameRule::Static("powerprojection"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/subject_type_upgrades",
				"common/subject_type_upgrades/",
			)
			.kind(CwtType::new("subject_type_upgrades"))
			.module_name(ModuleNameRule::Static("subject_type_upgrades"))
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/government_ranks", "common/government_ranks/")
				.kind(CwtType::new("government_ranks"))
				.module_name(ModuleNameRule::Static("government_ranks"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/province_names", "common/province_names/")
				.kind(CwtType::new("province_names"))
				.module_name(ModuleNameRule::Static("province_names"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("map/random/tiles", "map/random/tiles/")
				.kind(CwtType::new("random_map_tiles"))
				.module_name(ModuleNameRule::Static("random_map_tiles"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/random_names", "map/random/RandomLandNames.txt")
				.kind(CwtType::new("random_map_names"))
				.module_name(ModuleNameRule::Static("random_map_names"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/random_names", "map/random/RandomSeaNames.txt")
				.kind(CwtType::new("random_map_names"))
				.module_name(ModuleNameRule::Static("random_map_names"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/random_names", "map/random/RandomLakeNames.txt")
				.kind(CwtType::new("random_map_names"))
				.module_name(ModuleNameRule::Static("random_map_names"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/random/scenarios", "map/random/RNWScenarios.txt")
				.kind(CwtType::new("random_map_scenarios"))
				.module_name(ModuleNameRule::Static("random_map_scenarios"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/technologies", "common/technologies/")
				.kind(CwtType::new("technologies"))
				.module_name(ModuleNameRule::Static("technologies"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("common/technology", "common/technology.txt")
				.kind(CwtType::new("technology_groups"))
				.module_name(ModuleNameRule::Static("technology_groups"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/estate_agendas", "common/estate_agendas/")
				.kind(CwtType::new("estate_agendas"))
				.module_name(ModuleNameRule::Static("estate_agendas"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/estate_privileges",
				"common/estate_privileges/",
			)
			.kind(CwtType::new("estate_privileges"))
			.module_name(ModuleNameRule::Static("estate_privileges"))
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.scalar_policy(ScalarMergePolicy::Sum)
			.boolean_policy(BooleanMergePolicy::And)
			.build(),
			ContentFamilyDescriptor::prefix("common/estate_action", "common/estate_action/")
				.module_name(ModuleNameRule::Static("estate_action"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/native_advancement",
				"common/native_advancement/",
			)
			.module_name(ModuleNameRule::Static("native_advancement"))
			.scope(scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/estates", "common/estates/")
				.kind(CwtType::new("estates"))
				.module_name(ModuleNameRule::Static("estates"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/parliament_bribes",
				"common/parliament_bribes/",
			)
			.kind(CwtType::new("parliament_bribes"))
			.module_name(ModuleNameRule::Static("parliament_bribes"))
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix(
				"common/parliament_issues",
				"common/parliament_issues/",
			)
			.kind(CwtType::new("parliament_issues"))
			.module_name(ModuleNameRule::Static("parliament_issues"))
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/state_edicts", "common/state_edicts/")
				.kind(CwtType::new("state_edicts"))
				.module_name(ModuleNameRule::Static("state_edicts"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("common/achievements", "common/achievements.txt")
				.kind(CwtType::new("achievements"))
				.module_name(ModuleNameRule::Static("achievements"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/ages", "common/ages/")
				.kind(CwtType::new("ages"))
				.module_name(ModuleNameRule::Static("ages"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.list_policy(ListMergePolicy::OrderedUnion)
				.build(),
			ContentFamilyDescriptor::prefix("common/buildings", "common/buildings/")
				.kind(CwtType::new("buildings"))
				.module_name(ModuleNameRule::Static("buildings"))
				.scope(country_from_scope(base_scope::province()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_policy(ScalarMergePolicy::LastWriter)
				.list_policy(ListMergePolicy::Replace)
				.one_sided_removal_policy(OneSidedRemovalPolicy::PreserveIfParentSurvives)
				.build(),
			ContentFamilyDescriptor::prefix("common/institutions", "common/institutions/")
				.kind(CwtType::new("institutions"))
				.module_name(ModuleNameRule::Static("institutions"))
				.scope(scope(base_scope::province()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_policy(ScalarMergePolicy::Sum)
				.one_sided_removal_policy(OneSidedRemovalPolicy::PreserveBooleanAlternatives)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/province_triggered_modifiers",
				"common/province_triggered_modifiers/",
			)
			.kind(CwtType::new("province_triggered_modifiers"))
			.module_name(ModuleNameRule::Static("province_triggered_modifiers"))
			.scope(scope(base_scope::province()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/ideas", "common/ideas/")
				.kind(CwtType::new("ideas"))
				.module_name(ModuleNameRule::Static("ideas"))
				.scope(scope(base_scope::country()))
				// Safe: idea group ids are global; omitting a vanilla-equivalent generated
				// group leaves the vanilla idea group active.
				.capabilities(semantic_complete_and_merge_ready())
				.per_entry_dedup_safe()
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_policy(ScalarMergePolicy::Sum)
				.build(),
			ContentFamilyDescriptor::prefix("common/great_projects", "common/great_projects/")
				.kind(CwtType::new("great_projects"))
				.module_name(ModuleNameRule::Static("great_projects"))
				.scope(country_from_scope(base_scope::province()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/government_reforms",
				"common/government_reforms/",
			)
			.kind(CwtType::new("government_reforms"))
			.module_name(ModuleNameRule::Static("government_reforms"))
			.scope(scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.list_policy(ListMergePolicy::Replace)
			.boolean_policy(BooleanMergePolicy::And)
			.build(),
			ContentFamilyDescriptor::prefix("common/cultures", "common/cultures/")
				.kind(CwtType::new("cultures"))
				.module_name(ModuleNameRule::Static("cultures"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/custom_gui", "common/custom_gui/")
				.kind(CwtType::new("custom_gui"))
				.module_name(ModuleNameRule::Static("custom_gui"))
				.scope(dynamic_scope_policy())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/advisortypes", "common/advisortypes/")
				.kind(CwtType::new("advisortypes"))
				.module_name(ModuleNameRule::Static("advisortypes"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/event_modifiers", "common/event_modifiers/")
				.kind(CwtType::new("event_modifiers"))
				.module_name(ModuleNameRule::Static("event_modifiers"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/cb_types", "common/cb_types/")
				.kind(CwtType::new("cb_types"))
				.module_name(ModuleNameRule::Static("cb_types"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.list_policy(ListMergePolicy::Replace)
				.boolean_policy(BooleanMergePolicy::And)
				.build(),
			ContentFamilyDescriptor::prefix("common/government_names", "common/government_names/")
				.kind(CwtType::new("government_names"))
				.module_name(ModuleNameRule::Static("government_names"))
				.scope(scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"customizable_localization",
				"customizable_localization/",
			)
			.kind(CwtType::new("customizable_localization"))
			.module_name(ModuleNameRule::Static("customizable_localization"))
			.scope(dynamic_scope_policy())
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("missions", "missions/")
				.kind(CwtType::new("missions"))
				.module_name(ModuleNameRule::Static("missions"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_policy(ScalarMergePolicy::LastWriter)
				.build(),
			// GUI files store repeated widget blocks under `guiTypes`; the listed
			// widget types are keyed by their inner `name` field so different widgets
			// in the same file merge independently. Unlisted children fall back to their
			// assignment key inside `guiTypes` rather than being dropped.
			ContentFamilyDescriptor::prefix("interface", "interface/")
				.kind(CwtType::new("ui"))
				.module_name(ModuleNameRule::Static("ui"))
				.scope(dynamic_scope_policy())
				.capabilities(semantic_complete_and_merge_ready())
				.edit_wins_over_remove()
				.merge_key(MergeKeySource::ContainerChildFieldValue {
					containers: &["guiTypes", "spriteTypes", "bitmapfonts", "objectTypes"],
					child_key_field: "name",
					child_types: GUI_TYPES_NAMED_CHILD_TYPES,
				})
				.scalar_policy(ScalarMergePolicy::GuiWidget)
				.build(),
			ContentFamilyDescriptor::prefix("common/interface", "common/interface/")
				.kind(CwtType::new("ui"))
				.module_name(ModuleNameRule::Static("ui"))
				.scope(dynamic_scope_policy())
				.capabilities(semantic_complete_and_merge_ready())
				.edit_wins_over_remove()
				.merge_key(MergeKeySource::ContainerChildFieldValue {
					containers: &["guiTypes", "spriteTypes", "bitmapfonts", "objectTypes"],
					child_key_field: "name",
					child_types: GUI_TYPES_NAMED_CHILD_TYPES,
				})
				.scalar_policy(ScalarMergePolicy::GuiWidget)
				.build(),
			ContentFamilyDescriptor::prefix("gfx", "gfx/")
				.kind(CwtType::new("ui"))
				.module_name(ModuleNameRule::Static("ui"))
				.scope(dynamic_scope_policy())
				.capabilities(semantic_complete_and_merge_ready())
				.edit_wins_over_remove()
				.merge_key(MergeKeySource::ContainerChildFieldValue {
					containers: &["guiTypes", "spriteTypes", "bitmapfonts", "objectTypes"],
					child_key_field: "name",
					child_types: GUI_TYPES_NAMED_CHILD_TYPES,
				})
				.scalar_policy(ScalarMergePolicy::GuiWidget)
				.build(),
			// ------------------------------------------------------------------
			// Batch-promoted parse_only → semantic_complete (59 roots)
			// ------------------------------------------------------------------
			// common/ roots (41)
			ContentFamilyDescriptor::prefix("common/ai_army", "common/ai_army/")
				.module_name(ModuleNameRule::Static("ai_army"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/ai_attitudes", "common/ai_attitudes/")
				.module_name(ModuleNameRule::Static("ai_attitudes"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/ai_personalities", "common/ai_personalities/")
				.module_name(ModuleNameRule::Static("ai_personalities"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/alerts", "common/alerts/")
				.module_name(ModuleNameRule::Static("alerts"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/ancestor_personalities",
				"common/ancestor_personalities/",
			)
			.module_name(ModuleNameRule::Static("ancestor_personalities"))
			.scope(unknown_scope())
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/centers_of_trade", "common/centers_of_trade/")
				.module_name(ModuleNameRule::Static("centers_of_trade"))
				.scope(scope(base_scope::province()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/client_states", "common/client_states/")
				.module_name(ModuleNameRule::Static("client_states"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/colonial_regions", "common/colonial_regions/")
				.module_name(ModuleNameRule::Static("colonial_regions"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/country_colors", "common/country_colors/")
				.module_name(ModuleNameRule::Static("country_colors"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/custom_country_colors",
				"common/custom_country_colors/",
			)
			.module_name(ModuleNameRule::Static("custom_country_colors"))
			.scope(unknown_scope())
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/custom_ideas", "common/custom_ideas/")
				.module_name(ModuleNameRule::Static("custom_ideas"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/dynasty_colors", "common/dynasty_colors/")
				.module_name(ModuleNameRule::Static("dynasty_colors"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/estate_crown_land",
				"common/estate_crown_land/",
			)
			.module_name(ModuleNameRule::Static("estate_crown_land"))
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/estates_preload", "common/estates_preload/")
				.module_name(ModuleNameRule::Static("estates_preload"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::ChildFieldValue {
					child_key_field: "key",
					child_types: &["modifier"],
				})
				.build(),
			ContentFamilyDescriptor::prefix("common/governments", "common/governments/")
				.module_name(ModuleNameRule::Static("governments"))
				.load_policy(ContentLoadPolicy::DefinitionModule(
					EU4_GOVERNMENTS_MODULE_POLICY,
				))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.list_policy(ListMergePolicy::OrderedUnion)
				.build(),
			ContentFamilyDescriptor::exact(
				"common/graphicalculturetype",
				"common/graphicalculturetype.txt",
			)
			.module_name(ModuleNameRule::Static("graphicalculturetype"))
			.scope(unknown_scope())
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::exact("common/historial_lucky", "common/historial_lucky.txt")
				.module_name(ModuleNameRule::Static("historial_lucky"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/imperial_incidents",
				"common/imperial_incidents/",
			)
			.module_name(ModuleNameRule::Static("imperial_incidents"))
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/imperial_reforms", "common/imperial_reforms/")
				.module_name(ModuleNameRule::Static("imperial_reforms"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/incidents", "common/incidents/")
				.module_name(ModuleNameRule::Static("incidents"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/insults", "common/insults/")
				.module_name(ModuleNameRule::Static("insults"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/leader_personalities",
				"common/leader_personalities/",
			)
			.module_name(ModuleNameRule::Static("leader_personalities"))
			.scope(unknown_scope())
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/natives", "common/natives/")
				.module_name(ModuleNameRule::Static("natives"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/opinion_modifiers",
				"common/opinion_modifiers/",
			)
			.module_name(ModuleNameRule::Static("opinion_modifiers"))
			.scope(unknown_scope())
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/prices", "common/prices/")
				.module_name(ModuleNameRule::Static("prices"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/region_colors", "common/region_colors/")
				.module_name(ModuleNameRule::Static("region_colors"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/religious_conversions",
				"common/religious_conversions/",
			)
			.module_name(ModuleNameRule::Static("religious_conversions"))
			.scope(country_from_scope(base_scope::province()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix(
				"common/religious_reforms",
				"common/religious_reforms/",
			)
			.module_name(ModuleNameRule::Static("religious_reforms"))
			.scope(country_from_scope(base_scope::country()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/revolt_triggers", "common/revolt_triggers/")
				.module_name(ModuleNameRule::Static("revolt_triggers"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/revolution", "common/revolution/")
				.module_name(ModuleNameRule::Static("revolution"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/ruler_personalities",
				"common/ruler_personalities/",
			)
			.module_name(ModuleNameRule::Static("ruler_personalities"))
			.scope(unknown_scope())
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix(
				"common/scripted_functions",
				"common/scripted_functions/",
			)
			.module_name(ModuleNameRule::Static("scripted_functions"))
			.scope(dynamic_scope_policy())
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/static_modifiers", "common/static_modifiers/")
				.module_name(ModuleNameRule::Static("static_modifiers"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_policy(ScalarMergePolicy::Sum)
				.build(),
			ContentFamilyDescriptor::prefix("common/timed_modifiers", "common/timed_modifiers/")
				.module_name(ModuleNameRule::Static("timed_modifiers"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/trade_companies", "common/trade_companies/")
				.module_name(ModuleNameRule::Static("trade_companies"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix(
				"common/tradecompany_investments",
				"common/tradecompany_investments/",
			)
			.module_name(ModuleNameRule::Static("tradecompany_investments"))
			.scope(country_from_scope(base_scope::province()))
			.capabilities(semantic_complete_and_merge_ready())
			.merge_key(MergeKeySource::AssignmentKey)
			.build(),
			ContentFamilyDescriptor::prefix("common/tradegoods", "common/tradegoods/")
				.module_name(ModuleNameRule::Static("tradegoods"))
				.scope(country_from_scope(base_scope::province()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.scalar_reducer_rules(TRADEGOODS_SCALAR_REDUCER_RULES)
				.list_policy(ListMergePolicy::Replace)
				.one_sided_removal_policy(OneSidedRemovalPolicy::PreserveIfParentSurvives)
				.build(),
			ContentFamilyDescriptor::prefix("common/tradenodes", "common/tradenodes/")
				.module_name(ModuleNameRule::Static("tradenodes"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.block_policy(BlockMergePolicy::Replace)
				.build(),
			ContentFamilyDescriptor::prefix("common/trading_policies", "common/trading_policies/")
				.module_name(ModuleNameRule::Static("trading_policies"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/units_display", "common/units_display/")
				.module_name(ModuleNameRule::Static("units_display"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("common/wargoal_types", "common/wargoal_types/")
				.module_name(ModuleNameRule::Static("wargoal_types"))
				.scope(country_from_scope(base_scope::country()))
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			// map/ roots (12, excluding map/random)
			ContentFamilyDescriptor::exact("map/ambient_object", "map/ambient_object.txt")
				.module_name(ModuleNameRule::Static("ambient_object"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/area", "map/area.txt")
				.module_name(ModuleNameRule::Static("area"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/climate", "map/climate.txt")
				.module_name(ModuleNameRule::Static("climate"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/continent", "map/continent.txt")
				.module_name(ModuleNameRule::Static("continent"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/lakes", "map/lakes.txt")
				.module_name(ModuleNameRule::Static("lakes"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/positions", "map/positions.txt")
				.module_name(ModuleNameRule::Static("positions"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.block_policy(BlockMergePolicy::Replace)
				.build(),
			ContentFamilyDescriptor::exact("map/provincegroup", "map/provincegroup.txt")
				.module_name(ModuleNameRule::Static("provincegroup"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/region", "map/region.txt")
				.module_name(ModuleNameRule::Static("region"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/seasons", "map/seasons.txt")
				.module_name(ModuleNameRule::Static("seasons"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/superregion", "map/superregion.txt")
				.module_name(ModuleNameRule::Static("superregion"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/terrain", "map/terrain.txt")
				.module_name(ModuleNameRule::Static("terrain"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("map/trade_winds", "map/trade_winds.txt")
				.module_name(ModuleNameRule::Static("trade_winds"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.block_policy(BlockMergePolicy::Replace)
				.build(),
			// misc roots (6)
			ContentFamilyDescriptor::prefix("music", "music/")
				.module_name(ModuleNameRule::Static("music"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::FieldValue("name"))
				.build(),
			ContentFamilyDescriptor::prefix("sound", "sound/")
				.module_name(ModuleNameRule::Static("sound"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::FieldValue("name"))
				.build(),
			ContentFamilyDescriptor::exact("trigger_profile.txt", "trigger_profile.txt")
				.module_name(ModuleNameRule::Static("trigger_profile"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::prefix("tutorial", "tutorial/")
				.module_name(ModuleNameRule::Static("tutorial"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::FieldValue("index"))
				.build(),
			ContentFamilyDescriptor::prefix("tweakergui_assets", "tweakergui_assets/")
				.module_name(ModuleNameRule::Static("tweakergui_assets"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
			ContentFamilyDescriptor::exact("userdir.txt", "userdir.txt")
				.module_name(ModuleNameRule::Static("userdir"))
				.scope(unknown_scope())
				.capabilities(semantic_complete_and_merge_ready())
				.merge_key(MergeKeySource::AssignmentKey)
				.build(),
		];
		enable_common_definition_modules(&mut families);
		families.into_boxed_slice()
	})
}

impl GameProfile for Eu4Profile {
	fn game_id(&self) -> GameId {
		GameId::Eu4
	}

	fn classify_content_family(&self, relative: &Path) -> Option<&'static ContentFamilyDescriptor> {
		let normalized = relative.to_string_lossy().replace('\\', "/");
		eu4_content_families()
			.iter()
			.find(|descriptor| match descriptor.matcher {
				ContentFamilyPathMatcher::Prefix(prefix) => normalized.starts_with(prefix),
				ContentFamilyPathMatcher::Exact(exact) => normalized == exact,
			})
	}

	fn descriptor_for_root_family(
		&self,
		root_family: &str,
	) -> Option<&'static ContentFamilyDescriptor> {
		eu4_content_families()
			.iter()
			.find(|descriptor| descriptor.id.as_str() == root_family)
	}
}

pub fn eu4_profile() -> &'static Eu4Profile {
	ensure_base_scopes_initialized();
	&EU4_PROFILE
}

pub fn eu4_content_family_for_root_family(
	root_family: &str,
) -> Option<&'static ContentFamilyDescriptor> {
	eu4_profile().descriptor_for_root_family(root_family)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn governments_use_a_complete_versioned_definition_module_policy() {
		let descriptor = eu4_profile()
			.classify_content_family(Path::new("common/governments/00_governments.txt"))
			.expect("governments descriptor");
		let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
			panic!("governments must use definition-module loading");
		};

		assert_eq!(policy.definition_key, DefinitionKeyPolicy::AssignmentKey);
		assert_eq!(
			policy.file_order,
			DefinitionFileOrder::NormalizedPathAscending
		);
		assert_eq!(
			policy.duplicate_definitions,
			DuplicateDefinitionPolicy::LaterDefinitionWins
		);
		assert_eq!(
			policy.output_path,
			"common/governments/zzz_foch_governments.txt"
		);
		assert_eq!(policy.namespace_prefix, "common/governments");
		assert_eq!(policy.output_mode, DefinitionModuleOutput::ReplaceNamespace);
		assert!(policy.policy_version > 0);
		assert!(
			policy
				.output_path
				.starts_with(&format!("{}/", policy.namespace_prefix))
		);
	}

	#[test]
	fn common_assignment_key_families_share_directory_namespaces() {
		let profile = eu4_profile();
		for path in [
			"common/institutions/00_Core.txt",
			"common/religions/00_religion.txt",
			"common/scripted_effects/example.txt",
		] {
			let descriptor = profile
				.classify_content_family(Path::new(path))
				.expect("common descriptor");
			let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
				panic!("{path} must use directory namespace loading");
			};
			assert_eq!(
				policy.output_mode,
				DefinitionModuleOutput::Overlay,
				"{path}"
			);
			assert!(policy.output_path.contains("/zzz_foch_"), "{path}");
		}
	}

	#[test]
	fn buildings_preserve_one_sided_children_when_the_definition_survives() {
		let descriptor = eu4_profile()
			.classify_content_family(Path::new("common/buildings/00_buildings.txt"))
			.expect("buildings descriptor");

		assert_eq!(
			descriptor.merge_policies.one_sided_removal,
			OneSidedRemovalPolicy::PreserveIfParentSurvives
		);
	}

	#[test]
	fn institutions_preserve_one_sided_or_alternatives() {
		let descriptor = eu4_profile()
			.classify_content_family(Path::new("common/institutions/00_Core.txt"))
			.expect("institutions descriptor");

		assert_eq!(
			descriptor.merge_policies.one_sided_removal,
			OneSidedRemovalPolicy::PreserveBooleanAlternatives
		);
	}

	#[test]
	fn tradegoods_use_only_path_scoped_numeric_reducers() {
		let descriptor = eu4_profile()
			.classify_content_family(Path::new("common/tradegoods/00_tradegoods.txt"))
			.expect("tradegoods descriptor");
		let policies = &descriptor.merge_policies;

		assert_eq!(policies.scalar, ScalarMergePolicy::Conflict);
		assert_eq!(
			policies.one_sided_removal,
			OneSidedRemovalPolicy::PreserveIfParentSurvives
		);
		assert_eq!(
			policies
				.scalar_reducer_rule_for_path(&["cloves", "global_colonial_growth"])
				.expect("growth reducer")
				.reducer,
			ScalarMergePolicy::Max
		);
		assert_eq!(
			policies
				.scalar_reducer_rule_for_path(&["cloves", "province_trade_power_modifier",])
				.expect("trade-power reducer")
				.reducer,
			ScalarMergePolicy::Avg
		);
		for protected in ["technology", "date", "id", "position"] {
			assert!(
				policies
					.scalar_reducer_rule_for_path(&["cloves", protected])
					.is_none(),
				"{protected} unexpectedly received a reducer"
			);
		}
	}

	#[test]
	fn ages_do_not_sum_every_scalar() {
		let descriptor = eu4_profile()
			.classify_content_family(Path::new("common/ages/00_default.txt"))
			.expect("ages descriptor");

		assert_eq!(
			descriptor.merge_policies.scalar,
			ScalarMergePolicy::Conflict
		);
		assert!(descriptor.merge_policies.scalar_reducer_rules.is_empty());
	}

	#[test]
	fn every_eligible_common_directory_uses_namespace_loading() {
		let mut eligible = 0;
		for descriptor in eu4_content_families() {
			let ContentFamilyPathMatcher::Prefix(prefix) = descriptor.matcher else {
				continue;
			};
			if !prefix.starts_with("common/")
				|| !matches!(
					descriptor.merge_key_source,
					Some(
						MergeKeySource::AssignmentKey
							| MergeKeySource::FieldValue(_)
							| MergeKeySource::ChildFieldValue { .. }
					)
				) || matches!(prefix, "common/countries/" | "common/units/")
			{
				continue;
			}
			eligible += 1;
			assert!(
				matches!(
					descriptor.load_policy,
					ContentLoadPolicy::DefinitionModule(_)
				),
				"{prefix}"
			);
		}
		assert!(eligible >= 70, "expected broad common namespace coverage");
	}

	#[test]
	fn filename_identity_common_families_stay_per_path() {
		let profile = eu4_profile();
		for path in [
			"common/countries/France.txt",
			"common/units/western_medieval_infantry.txt",
			"common/defines/00_test.lua",
			"common/interface/example.gui",
		] {
			let descriptor = profile
				.classify_content_family(Path::new(path))
				.expect("common descriptor");
			assert_eq!(descriptor.load_policy, ContentLoadPolicy::PerPath, "{path}");
		}
	}

	#[test]
	fn decision_and_mission_families_use_last_writer_scalar_policy() {
		let profile = eu4_profile();
		for path in [
			"events/decisions/00_decisions.txt",
			"decisions/Ottoman.txt",
			"missions/DOM_Ottoman_Missions.txt",
		] {
			let descriptor = profile
				.classify_content_family(Path::new(path))
				.expect("expected EU4 content family");
			assert_eq!(
				descriptor.merge_policies.scalar,
				ScalarMergePolicy::LastWriter,
				"{path}"
			);
		}
	}

	#[test]
	fn events_merge_named_options_by_name() {
		let descriptor = eu4_profile()
			.classify_content_family(Path::new("events/Elections.txt"))
			.expect("events descriptor");

		assert_eq!(
			descriptor.merge_policies.nested_merge_key_source,
			MergeKeySource::ChildFieldValue {
				child_key_field: "name",
				child_types: &["option"],
			}
		);
	}

	#[test]
	fn per_path_event_and_decision_outputs_keep_vanilla_entries() {
		let profile = eu4_profile();
		for path in [
			"events/Elections.txt",
			"events/decisions/00_decisions.txt",
			"decisions/Ottoman.txt",
		] {
			let descriptor = profile
				.classify_content_family(Path::new(path))
				.expect("expected EU4 content family");
			assert!(
				descriptor.capabilities.dedup_policy.cross_file_safe(),
				"{path}"
			);
			assert!(
				!descriptor.capabilities.dedup_policy.per_entry_safe(),
				"{path} shadows the lower-layer file"
			);
		}
	}

	#[test]
	fn ui_families_use_gui_widget_scalar_policy() {
		let profile = eu4_profile();
		for path in [
			"interface/provinceview.gui",
			"common/interface/example.gui",
			"gfx/interface/example.gfx",
		] {
			let descriptor = profile
				.classify_content_family(Path::new(path))
				.expect("expected EU4 UI content family");
			assert_eq!(
				descriptor.merge_policies.scalar,
				ScalarMergePolicy::GuiWidget,
				"{path}"
			);
		}
	}
}
