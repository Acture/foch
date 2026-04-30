use super::content_family::{
	BlockMergePolicy, BlockPatchPolicy, BooleanMergePolicy, ConflictPolicy,
	ContentFamilyCapabilities, ContentFamilyDescriptor, ContentFamilyExtractor,
	ContentFamilyPathMatcher, ContentFamilyScopePolicy, GameId, GameProfile, ListMergePolicy,
	MergeKeySource, ModuleNameRule, ScalarMergePolicy, ScriptFileKind,
};
use foch_core::model::ScopeType;
use std::path::Path;

#[derive(Debug)]
pub struct Eu4Profile;

static EU4_PROFILE: Eu4Profile = Eu4Profile;

const fn semantic_complete_and_merge_ready() -> ContentFamilyCapabilities {
	ContentFamilyCapabilities {
		semantic_complete: true,
		graph_ready: false,
		merge_ready: true,
	}
}

const fn scope(root_scope: ScopeType) -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope,
		from_alias: None,
		dynamic_scope: false,
	}
}

const fn country_from_scope(root_scope: ScopeType) -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope,
		from_alias: Some(ScopeType::Country),
		dynamic_scope: false,
	}
}

/// Scope policy for content families whose top-level keys are not themselves
/// scoped (e.g. `common/religions` groups religions, religions in turn group
/// nested trigger/effect blocks). Within those nested blocks the engine pushes
/// the appropriate scope, but FROM is virtually always a country event source
/// (e.g. `on_convert`, `religious_schools/*/can_invite_scholar`).
const fn country_from_only() -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope: ScopeType::Unknown,
		from_alias: Some(ScopeType::Country),
		dynamic_scope: false,
	}
}

/// Scope policy for content families whose implicit scope is supplied by the
/// runtime caller (callables, UI bindings, customizable localization,
/// on_actions callbacks). A001 (uncertain-scope path) skips usages inside
/// these files because Unknown is the by-design state.
const fn dynamic_scope_policy() -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope: ScopeType::Unknown,
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

static EU4_CONTENT_FAMILIES: &[ContentFamilyDescriptor] = &[
	ContentFamilyDescriptor::prefix(
		"events/common/new_diplomatic_actions",
		"events/common/new_diplomatic_actions/",
	)
	.kind(ScriptFileKind::NewDiplomaticActions)
	.module_name(ModuleNameRule::Tail {
		prefix_len: 2,
		fallback: "new_diplomatic_actions",
	})
	.scope(country_from_scope(ScopeType::Country))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.build(),
	ContentFamilyDescriptor::prefix("common/on_actions", "common/on_actions/")
		.kind(ScriptFileKind::OnActions)
		.module_name(ModuleNameRule::Static("on_actions"))
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("events/common/on_actions", "events/common/on_actions/")
		.kind(ScriptFileKind::OnActions)
		.module_name(ModuleNameRule::Static("on_actions"))
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("events/decisions", "events/decisions/")
		.kind(ScriptFileKind::Decisions)
		.module_name(ModuleNameRule::Static("decisions"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::ContainerChildKey)
		.boolean_policy(BooleanMergePolicy::And)
		.build(),
	ContentFamilyDescriptor::prefix("events", "events/")
		.kind(ScriptFileKind::Events)
		.module_name(ModuleNameRule::Static("events"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::FieldValue("id"))
		.list_policy(ListMergePolicy::UnionWithRename)
		.boolean_policy(BooleanMergePolicy::And)
		.build(),
	ContentFamilyDescriptor::prefix("decisions", "decisions/")
		.kind(ScriptFileKind::Decisions)
		.module_name(ModuleNameRule::Static("decisions"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::ContainerChildKey)
		.boolean_policy(BooleanMergePolicy::And)
		.build(),
	ContentFamilyDescriptor::prefix("common/scripted_effects", "common/scripted_effects/")
		.kind(ScriptFileKind::ScriptedEffects)
		.module_name(ModuleNameRule::Tail {
			prefix_len: 2,
			fallback: "scripted_effects",
		})
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.block_patch_policy(BlockPatchPolicy::BooleanOr)
		.build(),
	ContentFamilyDescriptor::prefix("common/scripted_triggers", "common/scripted_triggers/")
		.kind(ScriptFileKind::ScriptedTriggers)
		.module_name(ModuleNameRule::Tail {
			prefix_len: 2,
			fallback: "scripted_triggers",
		})
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.conflict_policy(ConflictPolicy::BooleanOr)
		.block_patch_policy(BlockPatchPolicy::BooleanOr)
		.extractor(ContentFamilyExtractor::ScriptedTriggers)
		.build(),
	ContentFamilyDescriptor::prefix("common/triggered_modifiers", "common/triggered_modifiers/")
		.kind(ScriptFileKind::TriggeredModifiers)
		.module_name(ModuleNameRule::Tail {
			prefix_len: 2,
			fallback: "triggered_modifiers",
		})
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/defines", "common/defines/")
		.kind(ScriptFileKind::Defines)
		.module_name(ModuleNameRule::Tail {
			prefix_len: 2,
			fallback: "defines",
		})
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::LeafPath)
		.conflict_policy(ConflictPolicy::MergeLeaf)
		.build(),
	ContentFamilyDescriptor::prefix("common/diplomatic_actions", "common/diplomatic_actions/")
		.kind(ScriptFileKind::DiplomaticActions)
		.module_name(ModuleNameRule::Tail {
			prefix_len: 2,
			fallback: "diplomatic_actions",
		})
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::DiplomaticActions)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/new_diplomatic_actions",
		"common/new_diplomatic_actions/",
	)
	.kind(ScriptFileKind::NewDiplomaticActions)
	.module_name(ModuleNameRule::Tail {
		prefix_len: 2,
		fallback: "new_diplomatic_actions",
	})
	.scope(country_from_scope(ScopeType::Country))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.extractor(ContentFamilyExtractor::NewDiplomaticActions)
	.build(),
	ContentFamilyDescriptor::prefix("common/country_tags", "common/country_tags/")
		.kind(ScriptFileKind::CountryTags)
		.module_name(ModuleNameRule::Static("country_tags"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::CountryTags)
		.build(),
	ContentFamilyDescriptor::prefix("common/countries", "common/countries/")
		.kind(ScriptFileKind::Countries)
		.module_name(ModuleNameRule::Static("countries"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Countries)
		.build(),
	ContentFamilyDescriptor::prefix("history/countries", "history/countries/")
		.kind(ScriptFileKind::CountryHistory)
		.module_name(ModuleNameRule::Static("country_history"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.block_patch_policy(BlockPatchPolicy::Recurse)
		.block_patch_policies(COUNTRY_HISTORY_BLOCK_PATCH_POLICIES)
		.extractor(ContentFamilyExtractor::CountryHistory)
		.build(),
	ContentFamilyDescriptor::prefix("history/provinces", "history/provinces/")
		.kind(ScriptFileKind::ProvinceHistory)
		.module_name(ModuleNameRule::Static("province_history"))
		.scope(scope(ScopeType::Province))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.block_patch_policy(BlockPatchPolicy::Recurse)
		.extractor(ContentFamilyExtractor::ProvinceHistory)
		.build(),
	ContentFamilyDescriptor::prefix("history/diplomacy", "history/diplomacy/")
		.kind(ScriptFileKind::DiplomacyHistory)
		.module_name(ModuleNameRule::Static("diplomacy_history"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::DiplomacyHistory)
		.build(),
	ContentFamilyDescriptor::prefix("history/advisors", "history/advisors/")
		.kind(ScriptFileKind::AdvisorHistory)
		.module_name(ModuleNameRule::Static("advisor_history"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::AdvisorHistory)
		.build(),
	ContentFamilyDescriptor::prefix("history/wars", "history/wars/")
		.kind(ScriptFileKind::Wars)
		.module_name(ModuleNameRule::Static("wars"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Wars)
		.build(),
	ContentFamilyDescriptor::prefix("common/units", "common/units/")
		.kind(ScriptFileKind::Units)
		.module_name(ModuleNameRule::Static("units"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Units)
		.build(),
	ContentFamilyDescriptor::prefix("common/religions", "common/religions/")
		.kind(ScriptFileKind::Religions)
		.module_name(ModuleNameRule::Static("religions"))
		.scope(country_from_only())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Religions)
		.list_policy(ListMergePolicy::Replace)
		.build(),
	ContentFamilyDescriptor::prefix("common/subject_types", "common/subject_types/")
		.kind(ScriptFileKind::SubjectTypes)
		.module_name(ModuleNameRule::Static("subject_types"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::SubjectTypes)
		.build(),
	ContentFamilyDescriptor::prefix("common/rebel_types", "common/rebel_types/")
		.kind(ScriptFileKind::RebelTypes)
		.module_name(ModuleNameRule::Static("rebel_types"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::RebelTypes)
		.build(),
	ContentFamilyDescriptor::prefix("common/disasters", "common/disasters/")
		.kind(ScriptFileKind::Disasters)
		.module_name(ModuleNameRule::Static("disasters"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Disasters)
		.scalar_policy(ScalarMergePolicy::Sum)
		.boolean_policy(BooleanMergePolicy::And)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/government_mechanics",
		"common/government_mechanics/",
	)
	.kind(ScriptFileKind::GovernmentMechanics)
	.module_name(ModuleNameRule::Static("government_mechanics"))
	.scope(country_from_scope(ScopeType::Country))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.extractor(ContentFamilyExtractor::GovernmentMechanics)
	.build(),
	ContentFamilyDescriptor::prefix("common/church_aspects", "common/church_aspects/")
		.kind(ScriptFileKind::ChurchAspects)
		.module_name(ModuleNameRule::Static("church_aspects"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::ChurchAspects)
		.build(),
	ContentFamilyDescriptor::prefix("common/factions", "common/factions/")
		.kind(ScriptFileKind::Factions)
		.module_name(ModuleNameRule::Static("factions"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Factions)
		.build(),
	ContentFamilyDescriptor::prefix("common/hegemons", "common/hegemons/")
		.kind(ScriptFileKind::Hegemons)
		.module_name(ModuleNameRule::Static("hegemons"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Hegemons)
		.build(),
	ContentFamilyDescriptor::prefix("common/personal_deities", "common/personal_deities/")
		.kind(ScriptFileKind::PersonalDeities)
		.module_name(ModuleNameRule::Static("personal_deities"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::PersonalDeities)
		.build(),
	ContentFamilyDescriptor::prefix("common/fetishist_cults", "common/fetishist_cults/")
		.kind(ScriptFileKind::FetishistCults)
		.module_name(ModuleNameRule::Static("fetishist_cults"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::FetishistCults)
		.build(),
	ContentFamilyDescriptor::prefix("common/peace_treaties", "common/peace_treaties/")
		.kind(ScriptFileKind::PeaceTreaties)
		.module_name(ModuleNameRule::Static("peace_treaties"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::PeaceTreaties)
		.build(),
	ContentFamilyDescriptor::prefix("common/bookmarks", "common/bookmarks/")
		.kind(ScriptFileKind::Bookmarks)
		.module_name(ModuleNameRule::Static("bookmarks"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Bookmarks)
		.build(),
	ContentFamilyDescriptor::prefix("common/policies", "common/policies/")
		.kind(ScriptFileKind::Policies)
		.module_name(ModuleNameRule::Static("policies"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Policies)
		.scalar_policy(ScalarMergePolicy::Sum)
		.build(),
	ContentFamilyDescriptor::prefix("common/mercenary_companies", "common/mercenary_companies/")
		.kind(ScriptFileKind::MercenaryCompanies)
		.module_name(ModuleNameRule::Static("mercenary_companies"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::MercenaryCompanies)
		.scalar_policy(ScalarMergePolicy::Sum)
		.build(),
	ContentFamilyDescriptor::prefix("common/fervor", "common/fervor/")
		.kind(ScriptFileKind::Fervor)
		.module_name(ModuleNameRule::Static("fervor"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Fervor)
		.build(),
	ContentFamilyDescriptor::prefix("common/decrees", "common/decrees/")
		.kind(ScriptFileKind::Decrees)
		.module_name(ModuleNameRule::Static("decrees"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Decrees)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/federation_advancements",
		"common/federation_advancements/",
	)
	.kind(ScriptFileKind::FederationAdvancements)
	.module_name(ModuleNameRule::Static("federation_advancements"))
	.scope(scope(ScopeType::Country))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.extractor(ContentFamilyExtractor::FederationAdvancements)
	.build(),
	ContentFamilyDescriptor::prefix("common/golden_bulls", "common/golden_bulls/")
		.kind(ScriptFileKind::GoldenBulls)
		.module_name(ModuleNameRule::Static("golden_bulls"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::GoldenBulls)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/flagship_modifications",
		"common/flagship_modifications/",
	)
	.kind(ScriptFileKind::FlagshipModifications)
	.module_name(ModuleNameRule::Static("flagship_modifications"))
	.scope(scope(ScopeType::Country))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.extractor(ContentFamilyExtractor::FlagshipModifications)
	.build(),
	ContentFamilyDescriptor::prefix("common/holy_orders", "common/holy_orders/")
		.kind(ScriptFileKind::HolyOrders)
		.module_name(ModuleNameRule::Static("holy_orders"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::HolyOrders)
		.scalar_policy(ScalarMergePolicy::Sum)
		.build(),
	ContentFamilyDescriptor::prefix("common/naval_doctrines", "common/naval_doctrines/")
		.kind(ScriptFileKind::NavalDoctrines)
		.module_name(ModuleNameRule::Static("naval_doctrines"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::NavalDoctrines)
		.build(),
	ContentFamilyDescriptor::prefix("common/defender_of_faith", "common/defender_of_faith/")
		.kind(ScriptFileKind::DefenderOfFaith)
		.module_name(ModuleNameRule::Static("defender_of_faith"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::DefenderOfFaith)
		.build(),
	ContentFamilyDescriptor::prefix("common/isolationism", "common/isolationism/")
		.kind(ScriptFileKind::Isolationism)
		.module_name(ModuleNameRule::Static("isolationism"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Isolationism)
		.build(),
	ContentFamilyDescriptor::prefix("common/professionalism", "common/professionalism/")
		.kind(ScriptFileKind::Professionalism)
		.module_name(ModuleNameRule::Static("professionalism"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Professionalism)
		.build(),
	ContentFamilyDescriptor::prefix("common/powerprojection", "common/powerprojection/")
		.kind(ScriptFileKind::PowerProjection)
		.module_name(ModuleNameRule::Static("powerprojection"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::PowerProjection)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/subject_type_upgrades",
		"common/subject_type_upgrades/",
	)
	.kind(ScriptFileKind::SubjectTypeUpgrades)
	.module_name(ModuleNameRule::Static("subject_type_upgrades"))
	.scope(country_from_scope(ScopeType::Country))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.extractor(ContentFamilyExtractor::SubjectTypeUpgrades)
	.build(),
	ContentFamilyDescriptor::prefix("common/government_ranks", "common/government_ranks/")
		.kind(ScriptFileKind::GovernmentRanks)
		.module_name(ModuleNameRule::Static("government_ranks"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::GovernmentRanks)
		.build(),
	ContentFamilyDescriptor::prefix("common/province_names", "common/province_names/")
		.kind(ScriptFileKind::ProvinceNames)
		.module_name(ModuleNameRule::Static("province_names"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::ProvinceNames)
		.build(),
	ContentFamilyDescriptor::prefix("map/random/tiles", "map/random/tiles/")
		.kind(ScriptFileKind::RandomMapTiles)
		.module_name(ModuleNameRule::Static("random_map_tiles"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::RandomMapTiles)
		.build(),
	ContentFamilyDescriptor::exact("map/random_names", "map/random/RandomLandNames.txt")
		.kind(ScriptFileKind::RandomMapNames)
		.module_name(ModuleNameRule::Static("random_map_names"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::RandomMapNames)
		.build(),
	ContentFamilyDescriptor::exact("map/random_names", "map/random/RandomSeaNames.txt")
		.kind(ScriptFileKind::RandomMapNames)
		.module_name(ModuleNameRule::Static("random_map_names"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::RandomMapNames)
		.build(),
	ContentFamilyDescriptor::exact("map/random_names", "map/random/RandomLakeNames.txt")
		.kind(ScriptFileKind::RandomMapNames)
		.module_name(ModuleNameRule::Static("random_map_names"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::RandomMapNames)
		.build(),
	ContentFamilyDescriptor::exact("map/random/scenarios", "map/random/RNWScenarios.txt")
		.kind(ScriptFileKind::RandomMapScenarios)
		.module_name(ModuleNameRule::Static("random_map_scenarios"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::RandomMapScenarios)
		.build(),
	ContentFamilyDescriptor::prefix("common/technologies", "common/technologies/")
		.kind(ScriptFileKind::Technologies)
		.module_name(ModuleNameRule::Static("technologies"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Technologies)
		.build(),
	ContentFamilyDescriptor::exact("common/technology", "common/technology.txt")
		.kind(ScriptFileKind::TechnologyGroups)
		.module_name(ModuleNameRule::Static("technology_groups"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::TechnologyGroups)
		.build(),
	ContentFamilyDescriptor::prefix("common/estate_agendas", "common/estate_agendas/")
		.kind(ScriptFileKind::EstateAgendas)
		.module_name(ModuleNameRule::Static("estate_agendas"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::EstateAgendas)
		.build(),
	ContentFamilyDescriptor::prefix("common/estate_privileges", "common/estate_privileges/")
		.kind(ScriptFileKind::EstatePrivileges)
		.module_name(ModuleNameRule::Static("estate_privileges"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::EstatePrivileges)
		.scalar_policy(ScalarMergePolicy::Sum)
		.boolean_policy(BooleanMergePolicy::And)
		.build(),
	ContentFamilyDescriptor::prefix("common/estate_action", "common/estate_action/")
		.module_name(ModuleNameRule::Static("estate_action"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/native_advancement", "common/native_advancement/")
		.module_name(ModuleNameRule::Static("native_advancement"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/estates", "common/estates/")
		.kind(ScriptFileKind::Estates)
		.module_name(ModuleNameRule::Static("estates"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Estates)
		.build(),
	ContentFamilyDescriptor::prefix("common/parliament_bribes", "common/parliament_bribes/")
		.kind(ScriptFileKind::ParliamentBribes)
		.module_name(ModuleNameRule::Static("parliament_bribes"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::ParliamentBribes)
		.build(),
	ContentFamilyDescriptor::prefix("common/parliament_issues", "common/parliament_issues/")
		.kind(ScriptFileKind::ParliamentIssues)
		.module_name(ModuleNameRule::Static("parliament_issues"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::ParliamentIssues)
		.build(),
	ContentFamilyDescriptor::prefix("common/state_edicts", "common/state_edicts/")
		.kind(ScriptFileKind::StateEdicts)
		.module_name(ModuleNameRule::Static("state_edicts"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::StateEdicts)
		.build(),
	ContentFamilyDescriptor::exact("common/achievements", "common/achievements.txt")
		.kind(ScriptFileKind::Achievements)
		.module_name(ModuleNameRule::Static("achievements"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Achievements)
		.build(),
	ContentFamilyDescriptor::prefix("common/ages", "common/ages/")
		.kind(ScriptFileKind::Ages)
		.module_name(ModuleNameRule::Static("ages"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Ages)
		.scalar_policy(ScalarMergePolicy::Sum)
		.list_policy(ListMergePolicy::OrderedUnion)
		.build(),
	ContentFamilyDescriptor::prefix("common/buildings", "common/buildings/")
		.kind(ScriptFileKind::Buildings)
		.module_name(ModuleNameRule::Static("buildings"))
		.scope(country_from_scope(ScopeType::Province))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Buildings)
		.list_policy(ListMergePolicy::Replace)
		.build(),
	ContentFamilyDescriptor::prefix("common/institutions", "common/institutions/")
		.kind(ScriptFileKind::Institutions)
		.module_name(ModuleNameRule::Static("institutions"))
		.scope(scope(ScopeType::Province))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Institutions)
		.scalar_policy(ScalarMergePolicy::Sum)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/province_triggered_modifiers",
		"common/province_triggered_modifiers/",
	)
	.kind(ScriptFileKind::ProvinceTriggeredModifiers)
	.module_name(ModuleNameRule::Static("province_triggered_modifiers"))
	.scope(scope(ScopeType::Province))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.extractor(ContentFamilyExtractor::ProvinceTriggeredModifiers)
	.build(),
	ContentFamilyDescriptor::prefix("common/ideas", "common/ideas/")
		.kind(ScriptFileKind::Ideas)
		.module_name(ModuleNameRule::Static("ideas"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Ideas)
		.scalar_policy(ScalarMergePolicy::Sum)
		.build(),
	ContentFamilyDescriptor::prefix("common/great_projects", "common/great_projects/")
		.kind(ScriptFileKind::GreatProjects)
		.module_name(ModuleNameRule::Static("great_projects"))
		.scope(country_from_scope(ScopeType::Province))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::GreatProjects)
		.build(),
	ContentFamilyDescriptor::prefix("common/government_reforms", "common/government_reforms/")
		.kind(ScriptFileKind::GovernmentReforms)
		.module_name(ModuleNameRule::Static("government_reforms"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::GovernmentReforms)
		.list_policy(ListMergePolicy::Replace)
		.boolean_policy(BooleanMergePolicy::And)
		.build(),
	ContentFamilyDescriptor::prefix("common/cultures", "common/cultures/")
		.kind(ScriptFileKind::Cultures)
		.module_name(ModuleNameRule::Static("cultures"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::Cultures)
		.build(),
	ContentFamilyDescriptor::prefix("common/custom_gui", "common/custom_gui/")
		.kind(ScriptFileKind::CustomGui)
		.module_name(ModuleNameRule::Static("custom_gui"))
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::CustomGui)
		.build(),
	ContentFamilyDescriptor::prefix("common/advisortypes", "common/advisortypes/")
		.kind(ScriptFileKind::AdvisorTypes)
		.module_name(ModuleNameRule::Static("advisortypes"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::AdvisorTypes)
		.build(),
	ContentFamilyDescriptor::prefix("common/event_modifiers", "common/event_modifiers/")
		.kind(ScriptFileKind::EventModifiers)
		.module_name(ModuleNameRule::Static("event_modifiers"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::EventModifiers)
		.build(),
	ContentFamilyDescriptor::prefix("common/cb_types", "common/cb_types/")
		.kind(ScriptFileKind::CbTypes)
		.module_name(ModuleNameRule::Static("cb_types"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::CbTypes)
		.list_policy(ListMergePolicy::Replace)
		.boolean_policy(BooleanMergePolicy::And)
		.build(),
	ContentFamilyDescriptor::prefix("common/government_names", "common/government_names/")
		.kind(ScriptFileKind::GovernmentNames)
		.module_name(ModuleNameRule::Static("government_names"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.extractor(ContentFamilyExtractor::GovernmentNames)
		.build(),
	ContentFamilyDescriptor::prefix("customizable_localization", "customizable_localization/")
		.kind(ScriptFileKind::CustomizableLocalization)
		.module_name(ModuleNameRule::Static("customizable_localization"))
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("missions", "missions/")
		.kind(ScriptFileKind::Missions)
		.module_name(ModuleNameRule::Static("missions"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("interface", "interface/")
		.kind(ScriptFileKind::Ui)
		.module_name(ModuleNameRule::Static("ui"))
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/interface", "common/interface/")
		.kind(ScriptFileKind::Ui)
		.module_name(ModuleNameRule::Static("ui"))
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("gfx", "gfx/")
		.kind(ScriptFileKind::Ui)
		.module_name(ModuleNameRule::Static("ui"))
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	// ------------------------------------------------------------------
	// Batch-promoted parse_only → semantic_complete (59 roots)
	// ------------------------------------------------------------------
	// common/ roots (41)
	ContentFamilyDescriptor::prefix("common/ai_army", "common/ai_army/")
		.module_name(ModuleNameRule::Static("ai_army"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/ai_attitudes", "common/ai_attitudes/")
		.module_name(ModuleNameRule::Static("ai_attitudes"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/ai_personalities", "common/ai_personalities/")
		.module_name(ModuleNameRule::Static("ai_personalities"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/alerts", "common/alerts/")
		.module_name(ModuleNameRule::Static("alerts"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/ancestor_personalities",
		"common/ancestor_personalities/",
	)
	.module_name(ModuleNameRule::Static("ancestor_personalities"))
	.scope(scope(ScopeType::Unknown))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.build(),
	ContentFamilyDescriptor::prefix("common/centers_of_trade", "common/centers_of_trade/")
		.module_name(ModuleNameRule::Static("centers_of_trade"))
		.scope(scope(ScopeType::Province))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/client_states", "common/client_states/")
		.module_name(ModuleNameRule::Static("client_states"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/colonial_regions", "common/colonial_regions/")
		.module_name(ModuleNameRule::Static("colonial_regions"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/country_colors", "common/country_colors/")
		.module_name(ModuleNameRule::Static("country_colors"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/custom_country_colors",
		"common/custom_country_colors/",
	)
	.module_name(ModuleNameRule::Static("custom_country_colors"))
	.scope(scope(ScopeType::Unknown))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.build(),
	ContentFamilyDescriptor::prefix("common/custom_ideas", "common/custom_ideas/")
		.module_name(ModuleNameRule::Static("custom_ideas"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/dynasty_colors", "common/dynasty_colors/")
		.module_name(ModuleNameRule::Static("dynasty_colors"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/estate_crown_land", "common/estate_crown_land/")
		.module_name(ModuleNameRule::Static("estate_crown_land"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/estates_preload", "common/estates_preload/")
		.module_name(ModuleNameRule::Static("estates_preload"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/governments", "common/governments/")
		.module_name(ModuleNameRule::Static("governments"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.list_policy(ListMergePolicy::OrderedUnion)
		.build(),
	ContentFamilyDescriptor::exact(
		"common/graphicalculturetype",
		"common/graphicalculturetype.txt",
	)
	.module_name(ModuleNameRule::Static("graphicalculturetype"))
	.scope(scope(ScopeType::Unknown))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.build(),
	ContentFamilyDescriptor::exact("common/historial_lucky", "common/historial_lucky.txt")
		.module_name(ModuleNameRule::Static("historial_lucky"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/imperial_incidents", "common/imperial_incidents/")
		.module_name(ModuleNameRule::Static("imperial_incidents"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/imperial_reforms", "common/imperial_reforms/")
		.module_name(ModuleNameRule::Static("imperial_reforms"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/incidents", "common/incidents/")
		.module_name(ModuleNameRule::Static("incidents"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/insults", "common/insults/")
		.module_name(ModuleNameRule::Static("insults"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/leader_personalities",
		"common/leader_personalities/",
	)
	.module_name(ModuleNameRule::Static("leader_personalities"))
	.scope(scope(ScopeType::Unknown))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.build(),
	ContentFamilyDescriptor::prefix("common/natives", "common/natives/")
		.module_name(ModuleNameRule::Static("natives"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/opinion_modifiers", "common/opinion_modifiers/")
		.module_name(ModuleNameRule::Static("opinion_modifiers"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/prices", "common/prices/")
		.module_name(ModuleNameRule::Static("prices"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/region_colors", "common/region_colors/")
		.module_name(ModuleNameRule::Static("region_colors"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/religious_conversions",
		"common/religious_conversions/",
	)
	.module_name(ModuleNameRule::Static("religious_conversions"))
	.scope(country_from_scope(ScopeType::Province))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.build(),
	ContentFamilyDescriptor::prefix("common/religious_reforms", "common/religious_reforms/")
		.module_name(ModuleNameRule::Static("religious_reforms"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/revolt_triggers", "common/revolt_triggers/")
		.module_name(ModuleNameRule::Static("revolt_triggers"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/revolution", "common/revolution/")
		.module_name(ModuleNameRule::Static("revolution"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/ruler_personalities", "common/ruler_personalities/")
		.module_name(ModuleNameRule::Static("ruler_personalities"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/scripted_functions", "common/scripted_functions/")
		.module_name(ModuleNameRule::Static("scripted_functions"))
		.scope(dynamic_scope_policy())
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/static_modifiers", "common/static_modifiers/")
		.module_name(ModuleNameRule::Static("static_modifiers"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.scalar_policy(ScalarMergePolicy::Sum)
		.build(),
	ContentFamilyDescriptor::prefix("common/timed_modifiers", "common/timed_modifiers/")
		.module_name(ModuleNameRule::Static("timed_modifiers"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/trade_companies", "common/trade_companies/")
		.module_name(ModuleNameRule::Static("trade_companies"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/tradecompany_investments",
		"common/tradecompany_investments/",
	)
	.module_name(ModuleNameRule::Static("tradecompany_investments"))
	.scope(country_from_scope(ScopeType::Province))
	.capabilities(semantic_complete_and_merge_ready())
	.merge_key(MergeKeySource::AssignmentKey)
	.build(),
	ContentFamilyDescriptor::prefix("common/tradegoods", "common/tradegoods/")
		.module_name(ModuleNameRule::Static("tradegoods"))
		.scope(country_from_scope(ScopeType::Province))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.list_policy(ListMergePolicy::Replace)
		.build(),
	ContentFamilyDescriptor::prefix("common/tradenodes", "common/tradenodes/")
		.module_name(ModuleNameRule::Static("tradenodes"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.block_policy(BlockMergePolicy::Replace)
		.build(),
	ContentFamilyDescriptor::prefix("common/trading_policies", "common/trading_policies/")
		.module_name(ModuleNameRule::Static("trading_policies"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/units_display", "common/units_display/")
		.module_name(ModuleNameRule::Static("units_display"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("common/wargoal_types", "common/wargoal_types/")
		.module_name(ModuleNameRule::Static("wargoal_types"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	// map/ roots (12, excluding map/random)
	ContentFamilyDescriptor::exact("map/ambient_object", "map/ambient_object.txt")
		.module_name(ModuleNameRule::Static("ambient_object"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/area", "map/area.txt")
		.module_name(ModuleNameRule::Static("area"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/climate", "map/climate.txt")
		.module_name(ModuleNameRule::Static("climate"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/continent", "map/continent.txt")
		.module_name(ModuleNameRule::Static("continent"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/lakes", "map/lakes.txt")
		.module_name(ModuleNameRule::Static("lakes"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/positions", "map/positions.txt")
		.module_name(ModuleNameRule::Static("positions"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.block_policy(BlockMergePolicy::Replace)
		.build(),
	ContentFamilyDescriptor::exact("map/provincegroup", "map/provincegroup.txt")
		.module_name(ModuleNameRule::Static("provincegroup"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/region", "map/region.txt")
		.module_name(ModuleNameRule::Static("region"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/seasons", "map/seasons.txt")
		.module_name(ModuleNameRule::Static("seasons"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/superregion", "map/superregion.txt")
		.module_name(ModuleNameRule::Static("superregion"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/terrain", "map/terrain.txt")
		.module_name(ModuleNameRule::Static("terrain"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("map/trade_winds", "map/trade_winds.txt")
		.module_name(ModuleNameRule::Static("trade_winds"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.block_policy(BlockMergePolicy::Replace)
		.build(),
	// misc roots (6)
	ContentFamilyDescriptor::prefix("music", "music/")
		.module_name(ModuleNameRule::Static("music"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::FieldValue("name"))
		.build(),
	ContentFamilyDescriptor::prefix("sound", "sound/")
		.module_name(ModuleNameRule::Static("sound"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::FieldValue("name"))
		.build(),
	ContentFamilyDescriptor::exact("trigger_profile.txt", "trigger_profile.txt")
		.module_name(ModuleNameRule::Static("trigger_profile"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::prefix("tutorial", "tutorial/")
		.module_name(ModuleNameRule::Static("tutorial"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::FieldValue("index"))
		.build(),
	ContentFamilyDescriptor::prefix("tweakergui_assets", "tweakergui_assets/")
		.module_name(ModuleNameRule::Static("tweakergui_assets"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
	ContentFamilyDescriptor::exact("userdir.txt", "userdir.txt")
		.module_name(ModuleNameRule::Static("userdir"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.merge_key(MergeKeySource::AssignmentKey)
		.build(),
];

impl GameProfile for Eu4Profile {
	fn game_id(&self) -> GameId {
		GameId::Eu4
	}

	fn classify_content_family(&self, relative: &Path) -> Option<&'static ContentFamilyDescriptor> {
		let normalized = relative.to_string_lossy().replace('\\', "/");
		EU4_CONTENT_FAMILIES
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
		EU4_CONTENT_FAMILIES
			.iter()
			.find(|descriptor| descriptor.id == root_family)
	}
}

pub fn eu4_profile() -> &'static Eu4Profile {
	&EU4_PROFILE
}

pub fn eu4_content_family_for_root_family(
	root_family: &str,
) -> Option<&'static ContentFamilyDescriptor> {
	eu4_profile().descriptor_for_root_family(root_family)
}
