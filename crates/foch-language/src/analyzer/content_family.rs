use foch_core::model::ScopeType;
use serde::{Deserialize, Serialize};
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

/// How to resolve scalar conflicts during deep merge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalarMergePolicy {
	/// Overlay value replaces base (default).
	#[default]
	LastWriter,
	/// Parse both as f64 and sum them.
	Sum,
	/// Parse both as f64 and average them.
	Avg,
	/// Parse both as f64 and take the maximum.
	Max,
	/// Parse both as f64 and take the minimum.
	Min,
}

/// How to merge bare list items (entries without assignment keys).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListMergePolicy {
	/// Append unique items from overlay, dedup by value (default).
	#[default]
	Union,
	/// Append all items; rename duplicates to `{item}_{mod_name}`.
	UnionWithRename,
	/// Preserve base order, append overlay's new items at end.
	OrderedUnion,
	/// Overlay's list replaces base entirely.
	Replace,
}

/// How to merge child blocks that share the same key.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockMergePolicy {
	/// Deep merge child blocks by key (default).
	#[default]
	Recursive,
	/// Overlay's block replaces base entirely.
	Replace,
}

/// How to merge boolean condition blocks (triggers, potentials, etc.)
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BooleanMergePolicy {
	/// Combine conditions with AND semantics (both must hold).
	#[default]
	And,
	/// Combine conditions with OR semantics (either holds).
	Or,
	/// Overlay replaces base entirely.
	Replace,
}

/// Bundle of policies that control how `deep_merge` resolves conflicts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergePolicies {
	pub scalar: ScalarMergePolicy,
	pub list: ListMergePolicy,
	pub block: BlockMergePolicy,
	pub boolean: BooleanMergePolicy,
}

/// How to handle conflicts when two unrelated mods define the same merge key.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
	/// Rename conflicting definitions to `{key}_{mod_name}` (default for most families).
	#[default]
	Rename,
	/// Merge leaf values (last-writer per leaf) — for defines-style nested config.
	MergeLeaf,
	/// Last writer wins silently — for families where override is expected.
	LastWriter,
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
	/// True when this content family has no statically determinable implicit
	/// scope at runtime (callables, UI bindings, customizable localization,
	/// on_actions callbacks, etc.). Analyses that warn about Unknown scope
	/// (A001) should suppress findings inside such files because the implicit
	/// scope is supplied by the caller, not the file.
	pub dynamic_scope: bool,
}

impl Default for ContentFamilyScopePolicy {
	fn default() -> Self {
		Self {
			root_scope: ScopeType::Unknown,
			from_alias: None,
			dynamic_scope: false,
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
pub enum MergeKeySource {
	/// Top-level named assignments are the merge units (e.g. `effect_name = { ... }`).
	AssignmentKey,
	/// Merge key is extracted from an inner field value (e.g. `id` inside event blocks).
	FieldValue(&'static str),
	/// Merge units are children of a known container block (e.g. decisions).
	ContainerChildKey,
	/// Leaf-level defines paths (e.g. `NGame.START_YEAR`).
	LeafPath,
}

impl Serialize for MergeKeySource {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self {
			MergeKeySource::AssignmentKey => serializer.serialize_str("assignment_key"),
			MergeKeySource::FieldValue(field) => {
				use serde::ser::SerializeMap;
				let mut map = serializer.serialize_map(Some(1))?;
				map.serialize_entry("field_value", field)?;
				map.end()
			}
			MergeKeySource::ContainerChildKey => serializer.serialize_str("container_child_key"),
			MergeKeySource::LeafPath => serializer.serialize_str("leaf_path"),
		}
	}
}

impl<'de> Deserialize<'de> for MergeKeySource {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		use serde::de;

		struct MergeKeySourceVisitor;
		impl<'de> de::Visitor<'de> for MergeKeySourceVisitor {
			type Value = MergeKeySource;
			fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
				write!(f, "a MergeKeySource string or map")
			}
			fn visit_str<E: de::Error>(self, v: &str) -> Result<MergeKeySource, E> {
				match v {
					"assignment_key" => Ok(MergeKeySource::AssignmentKey),
					"container_child_key" => Ok(MergeKeySource::ContainerChildKey),
					"leaf_path" => Ok(MergeKeySource::LeafPath),
					_ => Err(E::unknown_variant(
						v,
						&["assignment_key", "container_child_key", "leaf_path"],
					)),
				}
			}
			fn visit_map<A: de::MapAccess<'de>>(
				self,
				mut map: A,
			) -> Result<MergeKeySource, A::Error> {
				let key: String = map
					.next_key()?
					.ok_or_else(|| de::Error::custom("expected field_value key"))?;
				if key != "field_value" {
					return Err(de::Error::unknown_field(&key, &["field_value"]));
				}
				let value: String = map.next_value()?;
				let leaked: &'static str = Box::leak(value.into_boxed_str());
				Ok(MergeKeySource::FieldValue(leaked))
			}
		}
		deserializer.deserialize_any(MergeKeySourceVisitor)
	}
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
	pub merge_key_source: Option<MergeKeySource>,
	pub conflict_policy: ConflictPolicy,
	pub merge_policies: MergePolicies,
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
	merge_key_source: Option<MergeKeySource>,
	conflict_policy: ConflictPolicy,
	merge_policies: MergePolicies,
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
	pub const fn merge_key(mut self, source: MergeKeySource) -> Self {
		self.merge_key_source = Some(source);
		self
	}
	pub const fn conflict_policy(mut self, policy: ConflictPolicy) -> Self {
		self.conflict_policy = policy;
		self
	}
	pub const fn merge_policies(mut self, policies: MergePolicies) -> Self {
		self.merge_policies = policies;
		self
	}
	pub const fn scalar_policy(mut self, policy: ScalarMergePolicy) -> Self {
		self.merge_policies.scalar = policy;
		self
	}
	pub const fn list_policy(mut self, policy: ListMergePolicy) -> Self {
		self.merge_policies.list = policy;
		self
	}
	pub const fn block_policy(mut self, policy: BlockMergePolicy) -> Self {
		self.merge_policies.block = policy;
		self
	}
	pub const fn boolean_policy(mut self, policy: BooleanMergePolicy) -> Self {
		self.merge_policies.boolean = policy;
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
			merge_key_source: self.merge_key_source,
			conflict_policy: self.conflict_policy,
			merge_policies: self.merge_policies,
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
				dynamic_scope: false,
			},
			capabilities: ContentFamilyCapabilities {
				semantic_complete: false,
				graph_ready: false,
				merge_ready: false,
			},
			extractor: ContentFamilyExtractor::None,
			merge_key_source: None,
			conflict_policy: ConflictPolicy::Rename,
			merge_policies: MergePolicies {
				scalar: ScalarMergePolicy::LastWriter,
				list: ListMergePolicy::Union,
				block: BlockMergePolicy::Recursive,
				boolean: BooleanMergePolicy::And,
			},
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
				dynamic_scope: false,
			},
			capabilities: ContentFamilyCapabilities {
				semantic_complete: false,
				graph_ready: false,
				merge_ready: false,
			},
			extractor: ContentFamilyExtractor::None,
			merge_key_source: None,
			conflict_policy: ConflictPolicy::Rename,
			merge_policies: MergePolicies {
				scalar: ScalarMergePolicy::LastWriter,
				list: ListMergePolicy::Union,
				block: BlockMergePolicy::Recursive,
				boolean: BooleanMergePolicy::And,
			},
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
