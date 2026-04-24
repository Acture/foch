use super::content_family::{
	ContentFamilyCapabilities, ContentFamilyDescriptor, ContentFamilyExtractor,
	ContentFamilyPathMatcher, ContentFamilyScopePolicy, GameId, GameProfile, ModuleNameRule,
	ScriptFileKind,
};
use foch_core::model::ScopeType;
use std::path::Path;

#[derive(Debug)]
pub struct Eu4Profile;

static EU4_PROFILE: Eu4Profile = Eu4Profile;

const fn semantic_complete() -> ContentFamilyCapabilities {
	ContentFamilyCapabilities {
		semantic_complete: true,
		graph_ready: false,
		merge_ready: false,
	}
}

const fn semantic_complete_and_merge_ready() -> ContentFamilyCapabilities {
	ContentFamilyCapabilities {
		semantic_complete: true,
		graph_ready: false,
		merge_ready: true,
	}
}

const fn merge_ready() -> ContentFamilyCapabilities {
	ContentFamilyCapabilities {
		semantic_complete: false,
		graph_ready: false,
		merge_ready: true,
	}
}

const fn scope(root_scope: ScopeType) -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope,
		from_alias: None,
	}
}

const fn country_from_scope(root_scope: ScopeType) -> ContentFamilyScopePolicy {
	ContentFamilyScopePolicy {
		root_scope,
		from_alias: Some(ScopeType::Country),
	}
}

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
	.scope(scope(ScopeType::Country))
	.capabilities(semantic_complete())
	.build(),
	ContentFamilyDescriptor::prefix("common/on_actions", "common/on_actions/")
		.kind(ScriptFileKind::OnActions)
		.module_name(ModuleNameRule::Static("on_actions"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.build(),
	ContentFamilyDescriptor::prefix("events/common/on_actions", "events/common/on_actions/")
		.kind(ScriptFileKind::OnActions)
		.module_name(ModuleNameRule::Static("on_actions"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.build(),
	ContentFamilyDescriptor::prefix("events/decisions", "events/decisions/")
		.kind(ScriptFileKind::Decisions)
		.module_name(ModuleNameRule::Static("decisions"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.build(),
	ContentFamilyDescriptor::prefix("events", "events/")
		.kind(ScriptFileKind::Events)
		.module_name(ModuleNameRule::Static("events"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.build(),
	ContentFamilyDescriptor::prefix("decisions", "decisions/")
		.kind(ScriptFileKind::Decisions)
		.module_name(ModuleNameRule::Static("decisions"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
		.build(),
	ContentFamilyDescriptor::prefix("common/scripted_effects", "common/scripted_effects/")
		.kind(ScriptFileKind::ScriptedEffects)
		.module_name(ModuleNameRule::Tail {
			prefix_len: 2,
			fallback: "scripted_effects",
		})
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete_and_merge_ready())
		.build(),
	ContentFamilyDescriptor::prefix("common/scripted_triggers", "common/scripted_triggers/")
		.kind(ScriptFileKind::ScriptedTriggers)
		.module_name(ModuleNameRule::Tail {
			prefix_len: 2,
			fallback: "scripted_triggers",
		})
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
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
		.build(),
	ContentFamilyDescriptor::prefix("common/defines", "common/defines/")
		.kind(ScriptFileKind::Defines)
		.module_name(ModuleNameRule::Tail {
			prefix_len: 2,
			fallback: "defines",
		})
		.scope(scope(ScopeType::Unknown))
		.capabilities(merge_ready())
		.build(),
	ContentFamilyDescriptor::prefix("common/diplomatic_actions", "common/diplomatic_actions/")
		.kind(ScriptFileKind::DiplomaticActions)
		.module_name(ModuleNameRule::Tail {
			prefix_len: 2,
			fallback: "diplomatic_actions",
		})
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete_and_merge_ready())
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
	.capabilities(semantic_complete())
	.extractor(ContentFamilyExtractor::NewDiplomaticActions)
	.build(),
	ContentFamilyDescriptor::prefix("common/country_tags", "common/country_tags/")
		.kind(ScriptFileKind::CountryTags)
		.module_name(ModuleNameRule::Static("country_tags"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::CountryTags)
		.build(),
	ContentFamilyDescriptor::prefix("common/countries", "common/countries/")
		.kind(ScriptFileKind::Countries)
		.module_name(ModuleNameRule::Static("countries"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Countries)
		.build(),
	ContentFamilyDescriptor::prefix("history/countries", "history/countries/")
		.kind(ScriptFileKind::CountryHistory)
		.module_name(ModuleNameRule::Static("country_history"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::CountryHistory)
		.build(),
	ContentFamilyDescriptor::prefix("history/provinces", "history/provinces/")
		.kind(ScriptFileKind::ProvinceHistory)
		.module_name(ModuleNameRule::Static("province_history"))
		.scope(scope(ScopeType::Province))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::ProvinceHistory)
		.build(),
	ContentFamilyDescriptor::prefix("history/diplomacy", "history/diplomacy/")
		.kind(ScriptFileKind::DiplomacyHistory)
		.module_name(ModuleNameRule::Static("diplomacy_history"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::DiplomacyHistory)
		.build(),
	ContentFamilyDescriptor::prefix("history/advisors", "history/advisors/")
		.kind(ScriptFileKind::AdvisorHistory)
		.module_name(ModuleNameRule::Static("advisor_history"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::AdvisorHistory)
		.build(),
	ContentFamilyDescriptor::prefix("history/wars", "history/wars/")
		.kind(ScriptFileKind::Wars)
		.module_name(ModuleNameRule::Static("wars"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Wars)
		.build(),
	ContentFamilyDescriptor::prefix("common/units", "common/units/")
		.kind(ScriptFileKind::Units)
		.module_name(ModuleNameRule::Static("units"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Units)
		.build(),
	ContentFamilyDescriptor::prefix("common/religions", "common/religions/")
		.kind(ScriptFileKind::Religions)
		.module_name(ModuleNameRule::Static("religions"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Religions)
		.build(),
	ContentFamilyDescriptor::prefix("common/subject_types", "common/subject_types/")
		.kind(ScriptFileKind::SubjectTypes)
		.module_name(ModuleNameRule::Static("subject_types"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::SubjectTypes)
		.build(),
	ContentFamilyDescriptor::prefix("common/rebel_types", "common/rebel_types/")
		.kind(ScriptFileKind::RebelTypes)
		.module_name(ModuleNameRule::Static("rebel_types"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::RebelTypes)
		.build(),
	ContentFamilyDescriptor::prefix("common/disasters", "common/disasters/")
		.kind(ScriptFileKind::Disasters)
		.module_name(ModuleNameRule::Static("disasters"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Disasters)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/government_mechanics",
		"common/government_mechanics/",
	)
	.kind(ScriptFileKind::GovernmentMechanics)
	.module_name(ModuleNameRule::Static("government_mechanics"))
	.scope(scope(ScopeType::Unknown))
	.capabilities(semantic_complete())
	.extractor(ContentFamilyExtractor::GovernmentMechanics)
	.build(),
	ContentFamilyDescriptor::prefix("common/church_aspects", "common/church_aspects/")
		.kind(ScriptFileKind::ChurchAspects)
		.module_name(ModuleNameRule::Static("church_aspects"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::ChurchAspects)
		.build(),
	ContentFamilyDescriptor::prefix("common/factions", "common/factions/")
		.kind(ScriptFileKind::Factions)
		.module_name(ModuleNameRule::Static("factions"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Factions)
		.build(),
	ContentFamilyDescriptor::prefix("common/hegemons", "common/hegemons/")
		.kind(ScriptFileKind::Hegemons)
		.module_name(ModuleNameRule::Static("hegemons"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Hegemons)
		.build(),
	ContentFamilyDescriptor::prefix("common/personal_deities", "common/personal_deities/")
		.kind(ScriptFileKind::PersonalDeities)
		.module_name(ModuleNameRule::Static("personal_deities"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::PersonalDeities)
		.build(),
	ContentFamilyDescriptor::prefix("common/fetishist_cults", "common/fetishist_cults/")
		.kind(ScriptFileKind::FetishistCults)
		.module_name(ModuleNameRule::Static("fetishist_cults"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::FetishistCults)
		.build(),
	ContentFamilyDescriptor::prefix("common/peace_treaties", "common/peace_treaties/")
		.kind(ScriptFileKind::PeaceTreaties)
		.module_name(ModuleNameRule::Static("peace_treaties"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::PeaceTreaties)
		.build(),
	ContentFamilyDescriptor::prefix("common/bookmarks", "common/bookmarks/")
		.kind(ScriptFileKind::Bookmarks)
		.module_name(ModuleNameRule::Static("bookmarks"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Bookmarks)
		.build(),
	ContentFamilyDescriptor::prefix("common/policies", "common/policies/")
		.kind(ScriptFileKind::Policies)
		.module_name(ModuleNameRule::Static("policies"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Policies)
		.build(),
	ContentFamilyDescriptor::prefix("common/mercenary_companies", "common/mercenary_companies/")
		.kind(ScriptFileKind::MercenaryCompanies)
		.module_name(ModuleNameRule::Static("mercenary_companies"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::MercenaryCompanies)
		.build(),
	ContentFamilyDescriptor::prefix("common/fervor", "common/fervor/")
		.kind(ScriptFileKind::Fervor)
		.module_name(ModuleNameRule::Static("fervor"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Fervor)
		.build(),
	ContentFamilyDescriptor::prefix("common/decrees", "common/decrees/")
		.kind(ScriptFileKind::Decrees)
		.module_name(ModuleNameRule::Static("decrees"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Decrees)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/federation_advancements",
		"common/federation_advancements/",
	)
	.kind(ScriptFileKind::FederationAdvancements)
	.module_name(ModuleNameRule::Static("federation_advancements"))
	.scope(scope(ScopeType::Country))
	.capabilities(semantic_complete())
	.extractor(ContentFamilyExtractor::FederationAdvancements)
	.build(),
	ContentFamilyDescriptor::prefix("common/golden_bulls", "common/golden_bulls/")
		.kind(ScriptFileKind::GoldenBulls)
		.module_name(ModuleNameRule::Static("golden_bulls"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::GoldenBulls)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/flagship_modifications",
		"common/flagship_modifications/",
	)
	.kind(ScriptFileKind::FlagshipModifications)
	.module_name(ModuleNameRule::Static("flagship_modifications"))
	.scope(scope(ScopeType::Country))
	.capabilities(semantic_complete())
	.extractor(ContentFamilyExtractor::FlagshipModifications)
	.build(),
	ContentFamilyDescriptor::prefix("common/holy_orders", "common/holy_orders/")
		.kind(ScriptFileKind::HolyOrders)
		.module_name(ModuleNameRule::Static("holy_orders"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::HolyOrders)
		.build(),
	ContentFamilyDescriptor::prefix("common/naval_doctrines", "common/naval_doctrines/")
		.kind(ScriptFileKind::NavalDoctrines)
		.module_name(ModuleNameRule::Static("naval_doctrines"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::NavalDoctrines)
		.build(),
	ContentFamilyDescriptor::prefix("common/defender_of_faith", "common/defender_of_faith/")
		.kind(ScriptFileKind::DefenderOfFaith)
		.module_name(ModuleNameRule::Static("defender_of_faith"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::DefenderOfFaith)
		.build(),
	ContentFamilyDescriptor::prefix("common/isolationism", "common/isolationism/")
		.kind(ScriptFileKind::Isolationism)
		.module_name(ModuleNameRule::Static("isolationism"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Isolationism)
		.build(),
	ContentFamilyDescriptor::prefix("common/professionalism", "common/professionalism/")
		.kind(ScriptFileKind::Professionalism)
		.module_name(ModuleNameRule::Static("professionalism"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Professionalism)
		.build(),
	ContentFamilyDescriptor::prefix("common/powerprojection", "common/powerprojection/")
		.kind(ScriptFileKind::PowerProjection)
		.module_name(ModuleNameRule::Static("powerprojection"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::PowerProjection)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/subject_type_upgrades",
		"common/subject_type_upgrades/",
	)
	.kind(ScriptFileKind::SubjectTypeUpgrades)
	.module_name(ModuleNameRule::Static("subject_type_upgrades"))
	.scope(scope(ScopeType::Country))
	.capabilities(semantic_complete())
	.extractor(ContentFamilyExtractor::SubjectTypeUpgrades)
	.build(),
	ContentFamilyDescriptor::prefix("common/government_ranks", "common/government_ranks/")
		.kind(ScriptFileKind::GovernmentRanks)
		.module_name(ModuleNameRule::Static("government_ranks"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::GovernmentRanks)
		.build(),
	ContentFamilyDescriptor::prefix("common/province_names", "common/province_names/")
		.kind(ScriptFileKind::ProvinceNames)
		.module_name(ModuleNameRule::Static("province_names"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::ProvinceNames)
		.build(),
	ContentFamilyDescriptor::prefix("map/random/tiles", "map/random/tiles/")
		.kind(ScriptFileKind::RandomMapTiles)
		.module_name(ModuleNameRule::Static("random_map_tiles"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::RandomMapTiles)
		.build(),
	ContentFamilyDescriptor::exact("map/random_names", "map/random/RandomLandNames.txt")
		.kind(ScriptFileKind::RandomMapNames)
		.module_name(ModuleNameRule::Static("random_map_names"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::RandomMapNames)
		.build(),
	ContentFamilyDescriptor::exact("map/random_names", "map/random/RandomSeaNames.txt")
		.kind(ScriptFileKind::RandomMapNames)
		.module_name(ModuleNameRule::Static("random_map_names"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::RandomMapNames)
		.build(),
	ContentFamilyDescriptor::exact("map/random_names", "map/random/RandomLakeNames.txt")
		.kind(ScriptFileKind::RandomMapNames)
		.module_name(ModuleNameRule::Static("random_map_names"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::RandomMapNames)
		.build(),
	ContentFamilyDescriptor::exact("map/random/scenarios", "map/random/RNWScenarios.txt")
		.kind(ScriptFileKind::RandomMapScenarios)
		.module_name(ModuleNameRule::Static("random_map_scenarios"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::RandomMapScenarios)
		.build(),
	ContentFamilyDescriptor::prefix("common/technologies", "common/technologies/")
		.kind(ScriptFileKind::Technologies)
		.module_name(ModuleNameRule::Static("technologies"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Technologies)
		.build(),
	ContentFamilyDescriptor::exact("common/technology", "common/technology.txt")
		.kind(ScriptFileKind::TechnologyGroups)
		.module_name(ModuleNameRule::Static("technology_groups"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::TechnologyGroups)
		.build(),
	ContentFamilyDescriptor::prefix("common/estate_agendas", "common/estate_agendas/")
		.kind(ScriptFileKind::EstateAgendas)
		.module_name(ModuleNameRule::Static("estate_agendas"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::EstateAgendas)
		.build(),
	ContentFamilyDescriptor::prefix("common/estate_privileges", "common/estate_privileges/")
		.kind(ScriptFileKind::EstatePrivileges)
		.module_name(ModuleNameRule::Static("estate_privileges"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::EstatePrivileges)
		.build(),
	ContentFamilyDescriptor::prefix("common/estates", "common/estates/")
		.kind(ScriptFileKind::Estates)
		.module_name(ModuleNameRule::Static("estates"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Estates)
		.build(),
	ContentFamilyDescriptor::prefix("common/parliament_bribes", "common/parliament_bribes/")
		.kind(ScriptFileKind::ParliamentBribes)
		.module_name(ModuleNameRule::Static("parliament_bribes"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::ParliamentBribes)
		.build(),
	ContentFamilyDescriptor::prefix("common/parliament_issues", "common/parliament_issues/")
		.kind(ScriptFileKind::ParliamentIssues)
		.module_name(ModuleNameRule::Static("parliament_issues"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::ParliamentIssues)
		.build(),
	ContentFamilyDescriptor::prefix("common/state_edicts", "common/state_edicts/")
		.kind(ScriptFileKind::StateEdicts)
		.module_name(ModuleNameRule::Static("state_edicts"))
		.scope(scope(ScopeType::Province))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::StateEdicts)
		.build(),
	ContentFamilyDescriptor::exact("common/achievements", "common/achievements.txt")
		.kind(ScriptFileKind::Achievements)
		.module_name(ModuleNameRule::Static("achievements"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Achievements)
		.build(),
	ContentFamilyDescriptor::prefix("common/ages", "common/ages/")
		.kind(ScriptFileKind::Ages)
		.module_name(ModuleNameRule::Static("ages"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Ages)
		.build(),
	ContentFamilyDescriptor::prefix("common/buildings", "common/buildings/")
		.kind(ScriptFileKind::Buildings)
		.module_name(ModuleNameRule::Static("buildings"))
		.scope(country_from_scope(ScopeType::Province))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Buildings)
		.build(),
	ContentFamilyDescriptor::prefix("common/institutions", "common/institutions/")
		.kind(ScriptFileKind::Institutions)
		.module_name(ModuleNameRule::Static("institutions"))
		.scope(scope(ScopeType::Province))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Institutions)
		.build(),
	ContentFamilyDescriptor::prefix(
		"common/province_triggered_modifiers",
		"common/province_triggered_modifiers/",
	)
	.kind(ScriptFileKind::ProvinceTriggeredModifiers)
	.module_name(ModuleNameRule::Static("province_triggered_modifiers"))
	.scope(scope(ScopeType::Province))
	.capabilities(semantic_complete())
	.extractor(ContentFamilyExtractor::ProvinceTriggeredModifiers)
	.build(),
	ContentFamilyDescriptor::prefix("common/ideas", "common/ideas/")
		.kind(ScriptFileKind::Ideas)
		.module_name(ModuleNameRule::Static("ideas"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Ideas)
		.build(),
	ContentFamilyDescriptor::prefix("common/great_projects", "common/great_projects/")
		.kind(ScriptFileKind::GreatProjects)
		.module_name(ModuleNameRule::Static("great_projects"))
		.scope(scope(ScopeType::Province))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::GreatProjects)
		.build(),
	ContentFamilyDescriptor::prefix("common/government_reforms", "common/government_reforms/")
		.kind(ScriptFileKind::GovernmentReforms)
		.module_name(ModuleNameRule::Static("government_reforms"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::GovernmentReforms)
		.build(),
	ContentFamilyDescriptor::prefix("common/cultures", "common/cultures/")
		.kind(ScriptFileKind::Cultures)
		.module_name(ModuleNameRule::Static("cultures"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::Cultures)
		.build(),
	ContentFamilyDescriptor::prefix("common/custom_gui", "common/custom_gui/")
		.kind(ScriptFileKind::CustomGui)
		.module_name(ModuleNameRule::Static("custom_gui"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::CustomGui)
		.build(),
	ContentFamilyDescriptor::prefix("common/advisortypes", "common/advisortypes/")
		.kind(ScriptFileKind::AdvisorTypes)
		.module_name(ModuleNameRule::Static("advisortypes"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::AdvisorTypes)
		.build(),
	ContentFamilyDescriptor::prefix("common/event_modifiers", "common/event_modifiers/")
		.kind(ScriptFileKind::EventModifiers)
		.module_name(ModuleNameRule::Static("event_modifiers"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::EventModifiers)
		.build(),
	ContentFamilyDescriptor::prefix("common/cb_types", "common/cb_types/")
		.kind(ScriptFileKind::CbTypes)
		.module_name(ModuleNameRule::Static("cb_types"))
		.scope(country_from_scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::CbTypes)
		.build(),
	ContentFamilyDescriptor::prefix("common/government_names", "common/government_names/")
		.kind(ScriptFileKind::GovernmentNames)
		.module_name(ModuleNameRule::Static("government_names"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.extractor(ContentFamilyExtractor::GovernmentNames)
		.build(),
	ContentFamilyDescriptor::prefix("customizable_localization", "customizable_localization/")
		.kind(ScriptFileKind::CustomizableLocalization)
		.module_name(ModuleNameRule::Static("customizable_localization"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.build(),
	ContentFamilyDescriptor::prefix("missions", "missions/")
		.kind(ScriptFileKind::Missions)
		.module_name(ModuleNameRule::Static("missions"))
		.scope(scope(ScopeType::Country))
		.capabilities(semantic_complete())
		.build(),
	ContentFamilyDescriptor::prefix("interface", "interface/")
		.kind(ScriptFileKind::Ui)
		.module_name(ModuleNameRule::Static("ui"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.build(),
	ContentFamilyDescriptor::prefix("common/interface", "common/interface/")
		.kind(ScriptFileKind::Ui)
		.module_name(ModuleNameRule::Static("ui"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
		.build(),
	ContentFamilyDescriptor::prefix("gfx", "gfx/")
		.kind(ScriptFileKind::Ui)
		.module_name(ModuleNameRule::Static("ui"))
		.scope(scope(ScopeType::Unknown))
		.capabilities(semantic_complete())
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
