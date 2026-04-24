use foch_core::model::ScopeType;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptFileKind {
	Events,
	OnActions,
	Decisions,
	ScriptedEffects,
	ScriptedTriggers,
	DiplomaticActions,
	TriggeredModifiers,
	Defines,
	Achievements,
	Ages,
	Buildings,
	Institutions,
	ProvinceTriggeredModifiers,
	Ideas,
	GreatProjects,
	GovernmentReforms,
	Cultures,
	CustomGui,
	AdvisorTypes,
	GovernmentNames,
	EventModifiers,
	CbTypes,
	CustomizableLocalization,
	Missions,
	NewDiplomaticActions,
	CountryTags,
	Countries,
	CountryHistory,
	ProvinceHistory,
	ProvinceNames,
	RandomMapTiles,
	RandomMapNames,
	RandomMapScenarios,
	RandomMapTweaks,
	DiplomacyHistory,
	AdvisorHistory,
	Wars,
	Fervor,
	Decrees,
	FederationAdvancements,
	GoldenBulls,
	FlagshipModifications,
	HolyOrders,
	NavalDoctrines,
	DefenderOfFaith,
	Isolationism,
	Professionalism,
	PowerProjection,
	SubjectTypeUpgrades,
	GovernmentRanks,
	Units,
	Religions,
	SubjectTypes,
	RebelTypes,
	Disasters,
	GovernmentMechanics,
	ChurchAspects,
	Factions,
	Hegemons,
	PersonalDeities,
	FetishistCults,
	PeaceTreaties,
	Bookmarks,
	Policies,
	MercenaryCompanies,
	Technologies,
	TechnologyGroups,
	EstateAgendas,
	EstatePrivileges,
	Estates,
	ParliamentBribes,
	ParliamentIssues,
	StateEdicts,
	Ui,
	Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameId {
	Eu4,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContentFamilyCapabilities {
	pub semantic_complete: bool,
	pub graph_ready: bool,
	pub merge_ready: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentFamilyScopePolicy {
	pub root_scope: ScopeType,
	pub from_alias: Option<ScopeType>,
}

impl Default for ContentFamilyScopePolicy {
	fn default() -> Self {
		Self {
			root_scope: ScopeType::Unknown,
			from_alias: None,
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleNameRule {
	Static(&'static str),
	Tail {
		prefix_len: usize,
		fallback: &'static str,
	},
	FallbackParent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentFamilyExtractor {
	None,
	CountryTags,
	Countries,
	CountryHistory,
	ProvinceHistory,
	ProvinceNames,
	RandomMapTiles,
	RandomMapNames,
	RandomMapScenarios,
	DiplomacyHistory,
	AdvisorHistory,
	Wars,
	Fervor,
	Decrees,
	FederationAdvancements,
	GoldenBulls,
	FlagshipModifications,
	HolyOrders,
	NavalDoctrines,
	DefenderOfFaith,
	Isolationism,
	Professionalism,
	PowerProjection,
	SubjectTypeUpgrades,
	GovernmentRanks,
	Units,
	Religions,
	SubjectTypes,
	RebelTypes,
	Disasters,
	GovernmentMechanics,
	ChurchAspects,
	Factions,
	Hegemons,
	PersonalDeities,
	FetishistCults,
	PeaceTreaties,
	Bookmarks,
	Policies,
	MercenaryCompanies,
	Technologies,
	TechnologyGroups,
	EstateAgendas,
	EstatePrivileges,
	Estates,
	ParliamentBribes,
	ParliamentIssues,
	StateEdicts,
	Achievements,
	Ages,
	Institutions,
	Cultures,
	AdvisorTypes,
	GovernmentNames,
	CustomGui,
	EventModifiers,
	ProvinceTriggeredModifiers,
	CbTypes,
	Ideas,
	GreatProjects,
	GovernmentReforms,
	DiplomaticActions,
	ScriptedTriggers,
	NewDiplomaticActions,
	Buildings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentFamilyPathMatcher {
	Prefix(&'static str),
	Exact(&'static str),
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentFamilyDescriptor {
	pub id: &'static str,
	pub matcher: ContentFamilyPathMatcher,
	pub script_file_kind: ScriptFileKind,
	pub module_name_rule: ModuleNameRule,
	pub scope_policy: ContentFamilyScopePolicy,
	pub capabilities: ContentFamilyCapabilities,
	pub extractor: ContentFamilyExtractor,
}

#[derive(Clone, Copy, Debug)]
pub struct ContentFamilyDescriptorBuilder {
	id: &'static str,
	matcher: ContentFamilyPathMatcher,
	script_file_kind: ScriptFileKind,
	module_name_rule: ModuleNameRule,
	scope_policy: ContentFamilyScopePolicy,
	capabilities: ContentFamilyCapabilities,
	extractor: ContentFamilyExtractor,
}

impl ContentFamilyDescriptorBuilder {
	pub const fn kind(mut self, kind: ScriptFileKind) -> Self {
		self.script_file_kind = kind;
		self
	}
	pub const fn module_name(mut self, rule: ModuleNameRule) -> Self {
		self.module_name_rule = rule;
		self
	}
	pub const fn scope(mut self, policy: ContentFamilyScopePolicy) -> Self {
		self.scope_policy = policy;
		self
	}
	pub const fn capabilities(mut self, caps: ContentFamilyCapabilities) -> Self {
		self.capabilities = caps;
		self
	}
	pub const fn extractor(mut self, ext: ContentFamilyExtractor) -> Self {
		self.extractor = ext;
		self
	}
	pub const fn build(self) -> ContentFamilyDescriptor {
		ContentFamilyDescriptor {
			id: self.id,
			matcher: self.matcher,
			script_file_kind: self.script_file_kind,
			module_name_rule: self.module_name_rule,
			scope_policy: self.scope_policy,
			capabilities: self.capabilities,
			extractor: self.extractor,
		}
	}
}

impl ContentFamilyDescriptor {
	pub const fn prefix(id: &'static str, prefix: &'static str) -> ContentFamilyDescriptorBuilder {
		ContentFamilyDescriptorBuilder {
			id,
			matcher: ContentFamilyPathMatcher::Prefix(prefix),
			script_file_kind: ScriptFileKind::Other,
			module_name_rule: ModuleNameRule::FallbackParent,
			scope_policy: ContentFamilyScopePolicy {
				root_scope: ScopeType::Unknown,
				from_alias: None,
			},
			capabilities: ContentFamilyCapabilities {
				semantic_complete: false,
				graph_ready: false,
				merge_ready: false,
			},
			extractor: ContentFamilyExtractor::None,
		}
	}

	pub const fn exact(
		id: &'static str,
		exact_path: &'static str,
	) -> ContentFamilyDescriptorBuilder {
		ContentFamilyDescriptorBuilder {
			id,
			matcher: ContentFamilyPathMatcher::Exact(exact_path),
			script_file_kind: ScriptFileKind::Other,
			module_name_rule: ModuleNameRule::FallbackParent,
			scope_policy: ContentFamilyScopePolicy {
				root_scope: ScopeType::Unknown,
				from_alias: None,
			},
			capabilities: ContentFamilyCapabilities {
				semantic_complete: false,
				graph_ready: false,
				merge_ready: false,
			},
			extractor: ContentFamilyExtractor::None,
		}
	}
}

pub trait GameProfile: std::fmt::Debug + Send + Sync {
	fn game_id(&self) -> GameId;
	fn classify_content_family(&self, relative: &Path) -> Option<&'static ContentFamilyDescriptor>;
	fn descriptor_for_root_family(
		&self,
		root_family: &str,
	) -> Option<&'static ContentFamilyDescriptor>;

	/// Get the content family ID for a relative path.
	fn family_id_for(&self, relative: &Path) -> Option<&'static str> {
		self.classify_content_family(relative).map(|d| d.id)
	}

	/// Get the capabilities for a root family name.
	fn capabilities_for_root(&self, root_family: &str) -> Option<ContentFamilyCapabilities> {
		self.descriptor_for_root_family(root_family)
			.map(|d| d.capabilities)
	}

	/// Get the content family ID for a root family name.
	fn family_id_for_root(&self, root_family: &str) -> Option<&'static str> {
		self.descriptor_for_root_family(root_family).map(|d| d.id)
	}
}

pub fn module_name_for_descriptor(relative: &Path, descriptor: &ContentFamilyDescriptor) -> String {
	let normalized = relative.to_string_lossy().replace('\\', "/");
	let parts: Vec<&str> = normalized.split('/').collect();
	match descriptor.module_name_rule {
		ModuleNameRule::Static(value) => value.to_string(),
		ModuleNameRule::Tail {
			prefix_len,
			fallback,
		} => module_with_tail(&parts, prefix_len, fallback),
		ModuleNameRule::FallbackParent => fallback_module_name(&parts),
	}
}

fn module_with_tail(parts: &[&str], prefix_len: usize, fallback: &str) -> String {
	if parts.len() <= prefix_len + 1 {
		return fallback.to_string();
	}
	let mut name = fallback.to_string();
	for part in &parts[prefix_len + 1..parts.len() - 1] {
		name.push('.');
		name.push_str(part);
	}
	name
}

fn fallback_module_name(parts: &[&str]) -> String {
	if parts.len() <= 1 {
		return "other".to_string();
	}
	parts[..parts.len() - 1].join(".")
}
