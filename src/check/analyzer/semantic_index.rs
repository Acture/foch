use super::eu4_builtin::{
	is_builtin_effect, is_builtin_iterator, is_builtin_scope_changer, is_builtin_special_block,
	is_builtin_trigger, is_contextual_keyword, is_reserved_keyword,
};
use super::localisation::collect_localisation_definitions_from_root;
use super::param_contracts::{
	apply_registered_param_contracts, explicit_contract_param_names, registered_param_contract,
};
use super::parser::{
	AstFile, AstStatement, AstValue, ParseResult, SpanRange, parse_clausewitz_file,
};
use crate::check::model::{
	AliasUsage, DocumentFamily, DocumentRecord, KeyUsage, LocalisationDefinition, ParamBinding,
	ParseIssue, ResourceReference, ScalarAssignment, ScopeKind, ScopeNode, ScopeType,
	SemanticIndex, SourceSpan, SymbolDefinition, SymbolKind, SymbolReference, UiDefinition,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

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
	EventModifiers,
	CbTypes,
	GovernmentNames,
	CustomizableLocalization,
	Missions,
	NewDiplomaticActions,
	CountryTags,
	Countries,
	CountryHistory,
	ProvinceHistory,
	Wars,
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

#[derive(Clone, Debug)]
pub struct ParsedScriptFile {
	pub mod_id: String,
	pub path: PathBuf,
	pub relative_path: PathBuf,
	pub file_kind: ScriptFileKind,
	pub module_name: String,
	pub ast: AstFile,
	pub parse_issues: Vec<ParseIssue>,
	pub parse_cache_hit: bool,
}

const PARSE_CACHE_VERSION: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ParseCacheEntry {
	version: u32,
	file_len: u64,
	modified_nanos: u128,
	result: ParseResult,
}

pub fn classify_script_file(relative: &Path) -> ScriptFileKind {
	let normalized = relative.to_string_lossy().replace('\\', "/");
	if normalized.starts_with("events/common/new_diplomatic_actions/") {
		ScriptFileKind::NewDiplomaticActions
	} else if normalized.starts_with("common/on_actions/")
		|| normalized.starts_with("events/common/on_actions/")
	{
		ScriptFileKind::OnActions
	} else if normalized.starts_with("events/decisions/") {
		ScriptFileKind::Decisions
	} else if normalized.starts_with("events/") {
		ScriptFileKind::Events
	} else if normalized.starts_with("decisions/") {
		ScriptFileKind::Decisions
	} else if normalized.starts_with("common/scripted_effects/") {
		ScriptFileKind::ScriptedEffects
	} else if normalized.starts_with("common/scripted_triggers/") {
		ScriptFileKind::ScriptedTriggers
	} else if normalized.starts_with("common/diplomatic_actions/") {
		ScriptFileKind::DiplomaticActions
	} else if normalized.starts_with("common/new_diplomatic_actions/") {
		ScriptFileKind::NewDiplomaticActions
	} else if normalized.starts_with("common/country_tags/") {
		ScriptFileKind::CountryTags
	} else if normalized.starts_with("common/countries/") {
		ScriptFileKind::Countries
	} else if normalized.starts_with("history/countries/") {
		ScriptFileKind::CountryHistory
	} else if normalized.starts_with("history/provinces/") {
		ScriptFileKind::ProvinceHistory
	} else if normalized.starts_with("history/wars/") {
		ScriptFileKind::Wars
	} else if normalized.starts_with("common/units/") {
		ScriptFileKind::Units
	} else if normalized.starts_with("common/religions/") {
		ScriptFileKind::Religions
	} else if normalized.starts_with("common/subject_types/") {
		ScriptFileKind::SubjectTypes
	} else if normalized.starts_with("common/rebel_types/") {
		ScriptFileKind::RebelTypes
	} else if normalized.starts_with("common/disasters/") {
		ScriptFileKind::Disasters
	} else if normalized.starts_with("common/government_mechanics/") {
		ScriptFileKind::GovernmentMechanics
	} else if normalized.starts_with("common/church_aspects/") {
		ScriptFileKind::ChurchAspects
	} else if normalized.starts_with("common/factions/") {
		ScriptFileKind::Factions
	} else if normalized.starts_with("common/hegemons/") {
		ScriptFileKind::Hegemons
	} else if normalized.starts_with("common/personal_deities/") {
		ScriptFileKind::PersonalDeities
	} else if normalized.starts_with("common/fetishist_cults/") {
		ScriptFileKind::FetishistCults
	} else if normalized.starts_with("common/peace_treaties/") {
		ScriptFileKind::PeaceTreaties
	} else if normalized.starts_with("common/bookmarks/") {
		ScriptFileKind::Bookmarks
	} else if normalized.starts_with("common/policies/") {
		ScriptFileKind::Policies
	} else if normalized.starts_with("common/mercenary_companies/") {
		ScriptFileKind::MercenaryCompanies
	} else if normalized.starts_with("common/technologies/") {
		ScriptFileKind::Technologies
	} else if normalized == "common/technology.txt" {
		ScriptFileKind::TechnologyGroups
	} else if normalized.starts_with("common/estate_agendas/") {
		ScriptFileKind::EstateAgendas
	} else if normalized.starts_with("common/estate_privileges/") {
		ScriptFileKind::EstatePrivileges
	} else if normalized.starts_with("common/estates/") {
		ScriptFileKind::Estates
	} else if normalized.starts_with("common/parliament_bribes/") {
		ScriptFileKind::ParliamentBribes
	} else if normalized.starts_with("common/parliament_issues/") {
		ScriptFileKind::ParliamentIssues
	} else if normalized.starts_with("common/state_edicts/") {
		ScriptFileKind::StateEdicts
	} else if normalized.starts_with("common/triggered_modifiers/") {
		ScriptFileKind::TriggeredModifiers
	} else if normalized.starts_with("common/defines/") {
		ScriptFileKind::Defines
	} else if normalized == "common/achievements.txt" {
		ScriptFileKind::Achievements
	} else if normalized.starts_with("common/ages/") {
		ScriptFileKind::Ages
	} else if normalized.starts_with("common/buildings/") {
		ScriptFileKind::Buildings
	} else if normalized.starts_with("common/institutions/") {
		ScriptFileKind::Institutions
	} else if normalized.starts_with("common/province_triggered_modifiers/") {
		ScriptFileKind::ProvinceTriggeredModifiers
	} else if normalized.starts_with("common/ideas/") {
		ScriptFileKind::Ideas
	} else if normalized.starts_with("common/great_projects/") {
		ScriptFileKind::GreatProjects
	} else if normalized.starts_with("common/government_reforms/") {
		ScriptFileKind::GovernmentReforms
	} else if normalized.starts_with("common/cultures/") {
		ScriptFileKind::Cultures
	} else if normalized.starts_with("common/custom_gui/") {
		ScriptFileKind::CustomGui
	} else if normalized.starts_with("common/advisortypes/") {
		ScriptFileKind::AdvisorTypes
	} else if normalized.starts_with("common/event_modifiers/") {
		ScriptFileKind::EventModifiers
	} else if normalized.starts_with("common/cb_types/") {
		ScriptFileKind::CbTypes
	} else if normalized.starts_with("common/government_names/") {
		ScriptFileKind::GovernmentNames
	} else if normalized.starts_with("customizable_localization/") {
		ScriptFileKind::CustomizableLocalization
	} else if normalized.starts_with("missions/") {
		ScriptFileKind::Missions
	} else if normalized.starts_with("interface/")
		|| normalized.starts_with("common/interface/")
		|| normalized.starts_with("gfx/")
	{
		ScriptFileKind::Ui
	} else {
		ScriptFileKind::Other
	}
}

fn module_name_from_relative(relative: &Path, kind: ScriptFileKind) -> String {
	let normalized = relative.to_string_lossy().replace('\\', "/");
	let parts: Vec<&str> = normalized.split('/').collect();
	let module = match kind {
		ScriptFileKind::Events => "events".to_string(),
		ScriptFileKind::OnActions => "on_actions".to_string(),
		ScriptFileKind::Decisions => "decisions".to_string(),
		ScriptFileKind::ScriptedEffects => module_with_tail(&parts, 2, "scripted_effects"),
		ScriptFileKind::ScriptedTriggers => module_with_tail(&parts, 2, "scripted_triggers"),
		ScriptFileKind::DiplomaticActions => module_with_tail(&parts, 2, "diplomatic_actions"),
		ScriptFileKind::NewDiplomaticActions => {
			module_with_tail(&parts, 2, "new_diplomatic_actions")
		}
		ScriptFileKind::TriggeredModifiers => module_with_tail(&parts, 2, "triggered_modifiers"),
		ScriptFileKind::Defines => module_with_tail(&parts, 2, "defines"),
		ScriptFileKind::Achievements => "achievements".to_string(),
		ScriptFileKind::Ages => module_with_tail(&parts, 2, "ages"),
		ScriptFileKind::Buildings => module_with_tail(&parts, 2, "buildings"),
		ScriptFileKind::Institutions => module_with_tail(&parts, 2, "institutions"),
		ScriptFileKind::ProvinceTriggeredModifiers => {
			module_with_tail(&parts, 2, "province_triggered_modifiers")
		}
		ScriptFileKind::Ideas => module_with_tail(&parts, 2, "ideas"),
		ScriptFileKind::GreatProjects => module_with_tail(&parts, 2, "great_projects"),
		ScriptFileKind::GovernmentReforms => module_with_tail(&parts, 2, "government_reforms"),
		ScriptFileKind::Cultures => module_with_tail(&parts, 2, "cultures"),
		ScriptFileKind::CustomGui => module_with_tail(&parts, 2, "custom_gui"),
		ScriptFileKind::AdvisorTypes => module_with_tail(&parts, 2, "advisortypes"),
		ScriptFileKind::EventModifiers => module_with_tail(&parts, 2, "event_modifiers"),
		ScriptFileKind::CbTypes => module_with_tail(&parts, 2, "cb_types"),
		ScriptFileKind::GovernmentNames => module_with_tail(&parts, 2, "government_names"),
		ScriptFileKind::CustomizableLocalization => {
			module_with_tail(&parts, 1, "customizable_localization")
		}
		ScriptFileKind::Missions => "missions".to_string(),
		ScriptFileKind::CountryTags => module_with_tail(&parts, 2, "country_tags"),
		ScriptFileKind::Countries => module_with_tail(&parts, 2, "countries"),
		ScriptFileKind::CountryHistory => module_with_tail(&parts, 2, "history_countries"),
		ScriptFileKind::ProvinceHistory => module_with_tail(&parts, 2, "history_provinces"),
		ScriptFileKind::Wars => module_with_tail(&parts, 2, "history_wars"),
		ScriptFileKind::Units => module_with_tail(&parts, 2, "units"),
		ScriptFileKind::Religions => module_with_tail(&parts, 2, "religions"),
		ScriptFileKind::SubjectTypes => module_with_tail(&parts, 2, "subject_types"),
		ScriptFileKind::RebelTypes => module_with_tail(&parts, 2, "rebel_types"),
		ScriptFileKind::Disasters => module_with_tail(&parts, 2, "disasters"),
		ScriptFileKind::GovernmentMechanics => module_with_tail(&parts, 2, "government_mechanics"),
		ScriptFileKind::ChurchAspects => module_with_tail(&parts, 2, "church_aspects"),
		ScriptFileKind::Factions => module_with_tail(&parts, 2, "factions"),
		ScriptFileKind::Hegemons => module_with_tail(&parts, 2, "hegemons"),
		ScriptFileKind::PersonalDeities => module_with_tail(&parts, 2, "personal_deities"),
		ScriptFileKind::FetishistCults => module_with_tail(&parts, 2, "fetishist_cults"),
		ScriptFileKind::PeaceTreaties => module_with_tail(&parts, 2, "peace_treaties"),
		ScriptFileKind::Bookmarks => module_with_tail(&parts, 2, "bookmarks"),
		ScriptFileKind::Policies => module_with_tail(&parts, 2, "policies"),
		ScriptFileKind::MercenaryCompanies => module_with_tail(&parts, 2, "mercenary_companies"),
		ScriptFileKind::Technologies => module_with_tail(&parts, 2, "technologies"),
		ScriptFileKind::TechnologyGroups => module_with_tail(&parts, 1, "technology_groups"),
		ScriptFileKind::EstateAgendas => module_with_tail(&parts, 2, "estate_agendas"),
		ScriptFileKind::EstatePrivileges => module_with_tail(&parts, 2, "estate_privileges"),
		ScriptFileKind::Estates => module_with_tail(&parts, 2, "estates"),
		ScriptFileKind::ParliamentBribes => module_with_tail(&parts, 2, "parliament_bribes"),
		ScriptFileKind::ParliamentIssues => module_with_tail(&parts, 2, "parliament_issues"),
		ScriptFileKind::StateEdicts => module_with_tail(&parts, 2, "state_edicts"),
		ScriptFileKind::Ui => module_with_tail(&parts, 1, "ui"),
		ScriptFileKind::Other => fallback_module_name(&parts),
	};
	module.replace('-', "_")
}

#[derive(Default)]
struct MapGroupLookup {
	province_sets: HashSet<String>,
}

impl MapGroupLookup {
	fn contains(&self, key: &str) -> bool {
		self.province_sets.contains(key)
	}
}

fn collect_map_groups(files: &[ParsedScriptFile]) -> MapGroupLookup {
	let mut lookup = MapGroupLookup::default();
	for file in files {
		if !is_map_group_file(&file.relative_path) {
			continue;
		}
		for stmt in &file.ast.statements {
			if let AstStatement::Assignment {
				key,
				value: AstValue::Block { .. },
				..
			} = stmt && !is_keyword(key)
			{
				lookup.province_sets.insert(key.clone());
			}
		}
	}
	lookup
}

fn is_map_group_file(relative_path: &Path) -> bool {
	matches!(
		relative_path.to_string_lossy().replace('\\', "/").as_str(),
		"map/area.txt"
			| "map/region.txt"
			| "map/superregion.txt"
			| "map/continent.txt"
			| "map/provincegroup.txt"
	)
}

fn module_with_tail(parts: &[&str], prefix_len: usize, base: &str) -> String {
	if parts.len() <= prefix_len + 1 {
		return base.to_string();
	}
	let mut name = base.to_string();
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

fn qualify_symbol_name(module: &str, local: &str) -> String {
	format!("eu4::{module}::{local}")
}

pub fn parse_script_file(mod_id: &str, root: &Path, file: &Path) -> Option<ParsedScriptFile> {
	let relative = file.strip_prefix(root).ok()?.to_path_buf();
	let file_kind = classify_script_file(&relative);
	let module_name = module_name_from_relative(&relative, file_kind);
	let (parsed, parse_cache_hit) = parse_clausewitz_file_cached(file);

	let parse_issues = parsed
		.diagnostics
		.into_iter()
		.map(|item| ParseIssue {
			mod_id: mod_id.to_string(),
			path: relative.clone(),
			line: item.span.start.line,
			column: item.span.start.column,
			message: item.message,
		})
		.collect();

	Some(ParsedScriptFile {
		mod_id: mod_id.to_string(),
		path: file.to_path_buf(),
		relative_path: relative,
		file_kind,
		module_name,
		ast: parsed.ast,
		parse_issues,
		parse_cache_hit,
	})
}

pub fn collect_localisation_definitions(mod_id: &str, root: &Path) -> Vec<LocalisationDefinition> {
	collect_localisation_definitions_from_root(mod_id, root)
}

fn parse_clausewitz_file_cached(path: &Path) -> (ParseResult, bool) {
	let signature = file_signature(path);
	let cache_path = parser_cache_file(path);

	if let Some((file_len, modified_nanos)) = signature
		&& let Ok(raw) = fs::read_to_string(&cache_path)
		&& let Ok(entry) = serde_json::from_str::<ParseCacheEntry>(&raw)
		&& entry.version == PARSE_CACHE_VERSION
		&& entry.file_len == file_len
		&& entry.modified_nanos == modified_nanos
	{
		return (entry.result, true);
	}

	let parsed = parse_clausewitz_file(path);

	if let Some((file_len, modified_nanos)) = signature {
		let entry = ParseCacheEntry {
			version: PARSE_CACHE_VERSION,
			file_len,
			modified_nanos,
			result: parsed.clone(),
		};
		store_parse_cache_entry(&cache_path, &entry);
	}

	(parsed, false)
}

fn file_signature(path: &Path) -> Option<(u64, u128)> {
	let metadata = fs::metadata(path).ok()?;
	let modified = metadata
		.modified()
		.ok()
		.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
		.map_or(0, |duration| duration.as_nanos());
	Some((metadata.len(), modified))
}

fn parser_cache_root() -> PathBuf {
	if let Ok(override_dir) = std::env::var("FOCH_PARSE_CACHE_DIR") {
		return PathBuf::from(override_dir);
	}
	dirs::cache_dir()
		.unwrap_or_else(std::env::temp_dir)
		.join("foch")
		.join("parse_cache")
}

fn parser_cache_file(path: &Path) -> PathBuf {
	let normalized = path.to_string_lossy().replace('\\', "/");
	let mut hasher = DefaultHasher::new();
	normalized.hash(&mut hasher);
	let key = format!("{:016x}", hasher.finish());
	parser_cache_root().join(format!("{key}.json"))
}

fn store_parse_cache_entry(path: &Path, entry: &ParseCacheEntry) {
	let Some(parent) = path.parent() else {
		return;
	};
	if fs::create_dir_all(parent).is_err() {
		return;
	}
	let Ok(raw) = serde_json::to_string(entry) else {
		return;
	};
	let tmp = path.with_extension("json.tmp");
	if fs::write(&tmp, raw).is_err() {
		return;
	}
	let _ = fs::rename(tmp, path);
}

pub fn build_semantic_index(files: &[ParsedScriptFile]) -> SemanticIndex {
	let mut index = SemanticIndex::default();
	let map_groups = collect_map_groups(files);
	for file in files {
		index.documents.push(DocumentRecord {
			mod_id: file.mod_id.clone(),
			path: file.relative_path.clone(),
			family: DocumentFamily::Clausewitz,
			parse_ok: file.parse_issues.is_empty(),
		});
		index.parse_issues.extend(file.parse_issues.clone());
		build_file_index(file, &map_groups, &mut index);
	}
	infer_definition_scope_from_references(&mut index);
	apply_registered_param_contracts(&mut index);
	index
}

fn build_file_index(
	file: &ParsedScriptFile,
	map_groups: &MapGroupLookup,
	index: &mut SemanticIndex,
) {
	let mut aliases = HashMap::new();
	let root_this_type = root_scope_type_for_file_kind(file.file_kind);
	match file.file_kind {
		ScriptFileKind::DiplomaticActions
		| ScriptFileKind::NewDiplomaticActions
		| ScriptFileKind::Buildings
		| ScriptFileKind::ChurchAspects
		| ScriptFileKind::Factions
		| ScriptFileKind::Hegemons
		| ScriptFileKind::PersonalDeities
		| ScriptFileKind::FetishistCults
		| ScriptFileKind::PeaceTreaties
		| ScriptFileKind::CbTypes => {
			aliases.insert("THIS".to_string(), root_this_type);
			aliases.insert("ROOT".to_string(), root_this_type);
			aliases.insert("FROM".to_string(), ScopeType::Country);
		}
		_ => {
			aliases.insert("THIS".to_string(), root_this_type);
			aliases.insert("ROOT".to_string(), root_this_type);
		}
	}

	let root_scope = push_scope(
		index,
		ScopeKind::File,
		None,
		aliases.get("THIS").copied().unwrap_or(ScopeType::Unknown),
		aliases,
		&file.mod_id,
		&file.relative_path,
		line_from_stmt(file.ast.statements.first()),
	);

	let mut ctx = BuildContext {
		mod_id: &file.mod_id,
		path: &file.relative_path,
		file_kind: file.file_kind,
		module_name: &file.module_name,
		map_groups,
		technology_monarch_power: None,
		technology_definition_ordinal: 0,
	};

	walk_statements(
		&file.ast.statements,
		index,
		root_scope,
		&mut ctx,
		None,
		None,
	);
}

fn event_scope_type(key: &str) -> Option<ScopeType> {
	match key {
		"country_event" => Some(ScopeType::Country),
		"province_event" => Some(ScopeType::Province),
		_ => None,
	}
}

fn event_from_type(key: &str) -> Option<ScopeType> {
	match key {
		"country_event" | "province_event" => Some(ScopeType::Country),
		_ => None,
	}
}

fn is_top_level_event_definition(
	index: &SemanticIndex,
	scope_id: usize,
	key: &str,
	value: &AstValue,
) -> bool {
	scope_kind(index, scope_id) == ScopeKind::File
		&& event_scope_type(key).is_some()
		&& matches!(value, AstValue::Block { .. })
}

struct BuildContext<'a> {
	mod_id: &'a str,
	path: &'a Path,
	file_kind: ScriptFileKind,
	module_name: &'a str,
	map_groups: &'a MapGroupLookup,
	technology_monarch_power: Option<String>,
	technology_definition_ordinal: usize,
}

fn walk_statements(
	statements: &[AstStatement],
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &mut BuildContext<'_>,
	active_scripted_effect: Option<usize>,
	namespace: Option<String>,
) {
	let mut current_namespace = namespace;

	for stmt in statements {
		match stmt {
			AstStatement::Assignment {
				key,
				key_span,
				value,
				..
			} => {
				if is_top_level_event_definition(index, scope_id, key, value) {
					handle_event_block(index, scope_id, ctx, key, value, current_namespace.clone());
					continue;
				}

				record_key_usage(index, scope_id, ctx, key, key_span);
				record_scalar_assignment(index, scope_id, ctx, key, key_span, value);
				record_ui_scalar_semantics(index, ctx, key, key_span, value);
				record_foundation_resource_semantics(index, scope_id, ctx, key, key_span, value);

				if key == "namespace"
					&& let Some(value_text) = scalar_text(value)
				{
					current_namespace = Some(value_text);
				}

				if is_alias_key(key) {
					record_alias_usage(index, scope_id, ctx, key, key_span);
				}

				record_alias_tokens_from_value(index, scope_id, ctx, value);
				record_param_tokens(index, active_scripted_effect, value);

				if is_event_call(key, value)
					&& let Some(event_id) = extract_event_call_id(value)
				{
					index.references.push(SymbolReference {
						kind: SymbolKind::Event,
						name: event_id,
						module: ctx.module_name.to_string(),
						mod_id: ctx.mod_id.to_string(),
						path: ctx.path.to_path_buf(),
						line: key_span.start.line,
						column: key_span.start.column,
						scope_id,
						provided_params: Vec::new(),
						param_bindings: Vec::new(),
					});
				}

				if is_scripted_trigger_call_candidate(ctx, ctx.file_kind, key, scope_id, index) {
					index.references.push(SymbolReference {
						kind: SymbolKind::ScriptedTrigger,
						name: key.clone(),
						module: ctx.module_name.to_string(),
						mod_id: ctx.mod_id.to_string(),
						path: ctx.path.to_path_buf(),
						line: key_span.start.line,
						column: key_span.start.column,
						scope_id,
						provided_params: Vec::new(),
						param_bindings: Vec::new(),
					});
				}

				if let AstValue::Block { items, span } = value {
					record_ui_block_semantics(index, ctx, key, key_span, items);
					let definition_kind =
						symbol_definition_kind(ctx.file_kind, key, scope_id, index);
					let child_scope = create_child_scope(index, scope_id, ctx, key, span, items);
					if let Some(def_kind) = definition_kind {
						let mut required_params = collect_required_params(items);
						required_params.sort();
						required_params.dedup();

						index.definitions.push(SymbolDefinition {
							kind: def_kind,
							name: qualify_symbol_name(ctx.module_name, key),
							module: ctx.module_name.to_string(),
							local_name: key.clone(),
							mod_id: ctx.mod_id.to_string(),
							path: ctx.path.to_path_buf(),
							line: key_span.start.line,
							column: key_span.start.column,
							scope_id: child_scope,
							declared_this_type: scope_this_type(index, child_scope),
							inferred_this_type: ScopeType::Unknown,
							inferred_this_mask: 0,
							required_params,
							param_contract: registered_param_contract(key),
							scope_param_names: collect_scope_param_names(items),
						});
					}

					if definition_kind.is_none()
						&& is_scripted_effect_call_candidate(
							ctx,
							ctx.file_kind,
							key,
							scope_id,
							index,
						) {
						let mut provided = collect_provided_params(key, items);
						provided.names.sort();
						provided.names.dedup();
						provided.bindings.sort_by(|lhs, rhs| {
							(lhs.name.as_str(), lhs.value.as_str())
								.cmp(&(rhs.name.as_str(), rhs.value.as_str()))
						});
						provided
							.bindings
							.dedup_by(|lhs, rhs| lhs.name == rhs.name && lhs.value == rhs.value);
						index.references.push(SymbolReference {
							kind: SymbolKind::ScriptedEffect,
							name: key.clone(),
							module: ctx.module_name.to_string(),
							mod_id: ctx.mod_id.to_string(),
							path: ctx.path.to_path_buf(),
							line: key_span.start.line,
							column: key_span.start.column,
							scope_id,
							provided_params: provided.names,
							param_bindings: provided.bindings,
						});
					}

					let next_scripted_effect = if event_scope_type(key).is_some() {
						None
					} else if ctx.file_kind == ScriptFileKind::ScriptedEffects
						&& scope_kind(index, scope_id) == ScopeKind::File
					{
						find_scripted_effect_definition(index, ctx.mod_id, ctx.path, key)
					} else {
						active_scripted_effect
					};

					walk_statements(
						items,
						index,
						child_scope,
						ctx,
						next_scripted_effect,
						current_namespace.clone(),
					);
				}
			}
			AstStatement::Item { value, .. } => {
				record_alias_tokens_from_value(index, scope_id, ctx, value);
				record_param_tokens(index, active_scripted_effect, value);
				if let AstValue::Block { items, span } = value {
					let child_scope = push_scope(
						index,
						ScopeKind::Block,
						Some(scope_id),
						scope_this_type(index, scope_id),
						scope_aliases(index, scope_id),
						ctx.mod_id,
						ctx.path,
						span.start.line,
					);
					walk_statements(
						items,
						index,
						child_scope,
						ctx,
						active_scripted_effect,
						current_namespace.clone(),
					);
				}
			}
			AstStatement::Comment { .. } => {}
		}
	}
}

fn handle_event_block(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	value: &AstValue,
	namespace: Option<String>,
) {
	let AstValue::Block { items, span } = value else {
		return;
	};
	let Some(this_type) = event_scope_type(key) else {
		return;
	};
	let from_type = event_from_type(key).unwrap_or(ScopeType::Unknown);

	let mut aliases = scope_aliases(index, scope_id);
	aliases.insert("THIS".to_string(), this_type);
	aliases.insert("ROOT".to_string(), this_type);
	aliases.insert("FROM".to_string(), from_type);
	aliases.insert("PREV".to_string(), scope_this_type(index, scope_id));
	let event_scope = push_scope(
		index,
		ScopeKind::Event,
		Some(scope_id),
		this_type,
		aliases,
		ctx.mod_id,
		ctx.path,
		span.start.line,
	);

	if let Some(id) = extract_assignment_scalar(items, "id") {
		let full_id = if id.contains('.') {
			id
		} else if let Some(ns) = namespace.as_ref() {
			format!("{ns}.{id}")
		} else {
			id
		};

		index.definitions.push(SymbolDefinition {
			kind: SymbolKind::Event,
			name: full_id,
			module: ctx.module_name.to_string(),
			local_name: key.to_string(),
			mod_id: ctx.mod_id.to_string(),
			path: ctx.path.to_path_buf(),
			line: span.start.line,
			column: span.start.column,
			scope_id: event_scope,
			declared_this_type: this_type,
			inferred_this_type: this_type,
			inferred_this_mask: scope_type_mask(this_type),
			required_params: Vec::new(),
			param_contract: None,
			scope_param_names: Vec::new(),
		});
	}

	let mut child_ctx = BuildContext {
		mod_id: ctx.mod_id,
		path: ctx.path,
		file_kind: ctx.file_kind,
		module_name: ctx.module_name,
		map_groups: ctx.map_groups,
		technology_monarch_power: ctx.technology_monarch_power.clone(),
		technology_definition_ordinal: ctx.technology_definition_ordinal,
	};
	walk_statements(items, index, event_scope, &mut child_ctx, None, namespace);
}

fn record_key_usage(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
) {
	index.key_usages.push(KeyUsage {
		key: key.to_string(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_path_buf(),
		line: key_span.start.line,
		column: key_span.start.column,
		scope_id,
		this_type: scope_this_type(index, scope_id),
	});
}

fn record_scalar_assignment(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let AstValue::Scalar { value, .. } = value else {
		return;
	};

	index.scalar_assignments.push(ScalarAssignment {
		key: key.to_string(),
		value: value.as_text(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_path_buf(),
		line: key_span.start.line,
		column: key_span.start.column,
		scope_id,
	});
}

fn record_ui_scalar_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if ctx.file_kind != ScriptFileKind::Ui {
		return;
	}
	let Some(text) = scalar_text(value) else {
		return;
	};

	if key == "name" && is_ui_identifier_candidate(&text) {
		index.ui_definitions.push(UiDefinition {
			name: text.clone(),
			mod_id: ctx.mod_id.to_string(),
			path: ctx.path.to_path_buf(),
			line: key_span.start.line,
			column: key_span.start.column,
		});
	}

	if is_ui_resource_key(key) {
		index.resource_references.push(ResourceReference {
			key: key.to_string(),
			value: text,
			mod_id: ctx.mod_id.to_string(),
			path: ctx.path.to_path_buf(),
			line: key_span.start.line,
			column: key_span.start.column,
		});
	}
}

fn record_foundation_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &mut BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	match ctx.file_kind {
		ScriptFileKind::CountryTags => {
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
		ScriptFileKind::Countries => {
			if scope_kind(index, scope_id) != ScopeKind::File {
				return;
			}
			record_country_metadata_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::CountryHistory => {
			let Some(text) = scalar_text(value) else {
				return;
			};
			if (is_country_history_province_reference_key(key) && is_province_id_text(&text))
				|| (is_country_history_country_reference_key(key) && is_country_tag_text(&text))
			{
				push_resource_reference(index, ctx, key_span, key, text.as_str());
			}
		}
		ScriptFileKind::ProvinceHistory => {
			let Some(text) = scalar_text(value) else {
				return;
			};
			if is_province_history_country_reference_key(key) && is_country_tag_text(&text) {
				push_resource_reference(index, ctx, key_span, key, text.as_str());
			}
		}
		ScriptFileKind::Wars => {
			let Some(text) = scalar_text(value) else {
				return;
			};
			if (is_war_history_country_reference_key(key) && is_country_tag_text(&text))
				|| (is_war_history_province_reference_key(key) && is_province_id_text(&text))
			{
				push_resource_reference(index, ctx, key_span, key, text.as_str());
			}
		}
		ScriptFileKind::Units => {
			if scope_kind(index, scope_id) != ScopeKind::File {
				return;
			}
			record_unit_definition_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::Religions => {
			record_religion_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::SubjectTypes => {
			record_subject_type_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::RebelTypes => {
			record_rebel_type_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::Disasters => {
			record_disaster_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::GovernmentMechanics => {
			record_government_mechanic_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::ChurchAspects => {
			record_church_aspect_resource_semantics(index, scope_id, ctx, key, key_span, value);
		}
		ScriptFileKind::Factions => {
			record_faction_resource_semantics(index, scope_id, ctx, key, key_span, value);
		}
		ScriptFileKind::Hegemons => {
			record_hegemon_resource_semantics(index, scope_id, ctx, key, key_span, value);
		}
		ScriptFileKind::PersonalDeities => {
			record_personal_deity_resource_semantics(index, scope_id, ctx, key, key_span, value);
		}
		ScriptFileKind::FetishistCults => {
			record_fetishist_cult_resource_semantics(index, scope_id, ctx, key, key_span, value);
		}
		ScriptFileKind::PeaceTreaties => {
			record_peace_treaty_resource_semantics(index, scope_id, ctx, key, key_span, value);
		}
		ScriptFileKind::Bookmarks => {
			record_bookmark_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::Policies => {
			record_policy_resource_semantics(index, scope_id, ctx, key, key_span, value);
		}
		ScriptFileKind::MercenaryCompanies => {
			record_mercenary_company_resource_semantics(index, scope_id, ctx, key, key_span, value);
		}
		ScriptFileKind::Technologies => {
			record_technology_definition_resource_semantics(
				index, scope_id, ctx, key, key_span, value,
			);
		}
		ScriptFileKind::TechnologyGroups => {
			record_technology_group_resource_semantics(index, scope_id, ctx, key, key_span, value);
		}
		ScriptFileKind::EstateAgendas => {
			record_estate_agenda_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::EstatePrivileges => {
			record_estate_privilege_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::Estates => {
			record_estate_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::ParliamentBribes => {
			record_parliament_bribe_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::ParliamentIssues => {
			record_parliament_issue_resource_semantics(index, ctx, key, key_span, value);
		}
		ScriptFileKind::StateEdicts => {
			record_state_edict_resource_semantics(index, ctx, key, key_span, value);
		}
		_ => {}
	}
}

fn push_resource_reference(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key_span: &SpanRange,
	key: &str,
	value: &str,
) {
	index.resource_references.push(ResourceReference {
		key: key.to_string(),
		value: value.to_string(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_path_buf(),
		line: key_span.start.line,
		column: key_span.start.column,
	});
}

fn record_country_metadata_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
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

fn record_unit_definition_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let Some(text) = scalar_text(value) else {
		return;
	};
	if is_unit_definition_reference_key(key) {
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}
}

fn record_religion_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
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

fn record_subject_type_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let Some(text) = scalar_text(value) else {
		return;
	};
	if is_subject_type_reference_key(key) {
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}
}

fn record_rebel_type_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let Some(text) = scalar_text(value) else {
		return;
	};
	if is_rebel_type_reference_key(key) {
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}
}

fn record_disaster_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if let Some(text) = scalar_text(value)
		&& is_disaster_scalar_reference_key(key)
	{
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}

	let Some(reference_key) = disaster_block_reference_key(key) else {
		return;
	};
	for item in extract_block_scalar_items(value) {
		push_resource_reference(index, ctx, key_span, reference_key, item.as_str());
	}
}

fn record_government_mechanic_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
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

fn record_church_aspect_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if !is_top_level_named_block(index, scope_id, key, value) {
		return;
	}
	push_resource_reference(index, ctx, key_span, "localisation", key);
	push_resource_reference(
		index,
		ctx,
		key_span,
		"localisation_desc",
		&format!("desc_{key}"),
	);
	push_resource_reference(
		index,
		ctx,
		key_span,
		"localisation_modifier",
		&format!("{key}_modifier"),
	);
}

fn record_faction_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if is_top_level_named_block(index, scope_id, key, value) {
		push_resource_reference(index, ctx, key_span, "localisation", key);
		push_resource_reference(
			index,
			ctx,
			key_span,
			"localisation_influence",
			&format!("{key}_influence"),
		);
		return;
	}
	let Some(text) = scalar_text(value) else {
		return;
	};
	if key == "monarch_power" {
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}
}

fn record_hegemon_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if is_top_level_named_block(index, scope_id, key, value) {
		push_resource_reference(index, ctx, key_span, "localisation", key);
	}
}

fn record_personal_deity_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if !is_top_level_named_block(index, scope_id, key, value) {
		return;
	}
	push_resource_reference(index, ctx, key_span, "localisation", key);
	push_resource_reference(
		index,
		ctx,
		key_span,
		"localisation_desc",
		&format!("{key}_desc"),
	);
}

fn record_fetishist_cult_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if !is_top_level_named_block(index, scope_id, key, value) {
		return;
	}
	push_resource_reference(index, ctx, key_span, "localisation", key);
	push_resource_reference(
		index,
		ctx,
		key_span,
		"localisation_desc",
		&format!("{key}_desc"),
	);
}

fn record_peace_treaty_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
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

fn is_top_level_named_block(
	index: &SemanticIndex,
	scope_id: usize,
	key: &str,
	value: &AstValue,
) -> bool {
	scope_kind(index, scope_id) == ScopeKind::File
		&& matches!(value, AstValue::Block { .. })
		&& !is_keyword(key)
}

fn is_named_block_in_top_level_block(
	index: &SemanticIndex,
	scope_id: usize,
	key: &str,
	value: &AstValue,
) -> bool {
	scope_kind(index, scope_id) == ScopeKind::Block
		&& scope_parent_kind(index, scope_id) == Some(ScopeKind::File)
		&& matches!(value, AstValue::Block { .. })
		&& !is_keyword(key)
}

fn scope_parent_kind(index: &SemanticIndex, scope_id: usize) -> Option<ScopeKind> {
	let parent = index.scopes.get(scope_id)?.parent?;
	Some(scope_kind(index, parent))
}

fn monarch_power_prefix(value: &str) -> Option<&'static str> {
	match value {
		"ADM" => Some("adm"),
		"DIP" => Some("dip"),
		"MIL" => Some("mil"),
		_ => None,
	}
}

fn record_bookmark_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
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

fn record_technology_definition_resource_semantics(
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

fn record_policy_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if is_top_level_named_block(index, scope_id, key, value) {
		push_resource_reference(index, ctx, key_span, "localisation", key);
		return;
	}
	let Some(text) = scalar_text(value) else {
		return;
	};
	if key == "monarch_power" {
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}
}

fn record_technology_group_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
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

fn record_mercenary_company_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
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

fn record_estate_agenda_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let Some(text) = scalar_text(value) else {
		return;
	};
	if is_estate_agenda_scalar_reference_key(key) {
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}
}

fn record_estate_privilege_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
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

fn record_estate_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if let Some(text) = scalar_text(value)
		&& is_estate_scalar_reference_key(key)
	{
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}

	let Some(reference_key) = estate_block_reference_key(key) else {
		return;
	};
	for item in extract_block_scalar_items(value) {
		push_resource_reference(index, ctx, key_span, reference_key, item.as_str());
	}
}

fn record_parliament_bribe_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let Some(text) = scalar_text(value) else {
		return;
	};
	if is_parliament_bribe_scalar_reference_key(key) {
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}
}

fn record_parliament_issue_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let Some(text) = scalar_text(value) else {
		return;
	};
	if is_parliament_issue_scalar_reference_key(key) {
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}
}

fn record_state_edict_resource_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let Some(text) = scalar_text(value) else {
		return;
	};
	if is_state_edict_scalar_reference_key(key) {
		push_resource_reference(index, ctx, key_span, key, text.as_str());
	}
}

fn extract_block_scalar_items(value: &AstValue) -> Vec<String> {
	let AstValue::Block { items, .. } = value else {
		return Vec::new();
	};
	let mut values: Vec<String> = Vec::new();
	for item in items {
		match item {
			AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
				if let Some(text) = scalar_text(value) {
					values.push(text);
				}
			}
			AstStatement::Comment { .. } => {}
		}
	}
	values
}

fn extract_named_block_scalar_items(value: &AstValue, key_name: &str) -> Vec<String> {
	let AstValue::Block { items, .. } = value else {
		return Vec::new();
	};
	let mut values: Vec<String> = Vec::new();
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key == key_name
			&& let Some(text) = scalar_text(value)
		{
			values.push(text);
		}
	}
	values
}

fn extract_named_block_member_keys(value: &AstValue, key_name: &str) -> Vec<String> {
	let AstValue::Block { items, .. } = value else {
		return Vec::new();
	};
	let mut keys: Vec<String> = Vec::new();
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != key_name {
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		for nested in items {
			let AstStatement::Assignment { key, .. } = nested else {
				continue;
			};
			keys.push(key.clone());
		}
	}
	keys
}

fn extract_yes_assignment_keys(value: &AstValue) -> Vec<String> {
	let AstValue::Block { items, .. } = value else {
		return Vec::new();
	};
	let mut keys: Vec<String> = Vec::new();
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		let Some(text) = scalar_text(value) else {
			continue;
		};
		if text.eq_ignore_ascii_case("yes") {
			keys.push(key.clone());
		}
	}
	keys
}

fn extract_nested_assignment_scalar(
	items: &[AstStatement],
	block_name: &str,
	field_name: &str,
) -> Option<String> {
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != block_name {
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		if let Some(text) = extract_assignment_scalar(items, field_name) {
			return Some(text);
		}
	}
	None
}

fn record_ui_block_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	items: &[AstStatement],
) {
	if ctx.file_kind != ScriptFileKind::Ui || !looks_like_ui_container_key(key) {
		return;
	}
	let Some(name) = extract_assignment_scalar(items, "name") else {
		return;
	};
	if !is_ui_identifier_candidate(&name) {
		return;
	}
	index.ui_definitions.push(UiDefinition {
		name,
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_path_buf(),
		line: key_span.start.line,
		column: key_span.start.column,
	});
}

fn record_alias_usage(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	alias: &str,
	span: &SpanRange,
) {
	index.alias_usages.push(AliasUsage {
		alias: alias.to_string(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_path_buf(),
		line: span.start.line,
		column: span.start.column,
		scope_id,
	});
}

fn record_alias_tokens_from_value(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	value: &AstValue,
) {
	let AstValue::Scalar { value, span } = value else {
		return;
	};
	let text = value.as_text();
	for cap in alias_capture_regex().captures_iter(&text) {
		let Some(alias) = cap.get(1) else {
			continue;
		};
		index.alias_usages.push(AliasUsage {
			alias: alias.as_str().to_string(),
			mod_id: ctx.mod_id.to_string(),
			path: ctx.path.to_path_buf(),
			line: span.start.line,
			column: span.start.column,
			scope_id,
		});
	}
}

fn record_param_tokens(index: &mut SemanticIndex, def_idx: Option<usize>, value: &AstValue) {
	let Some(def_idx) = def_idx else {
		return;
	};

	match value {
		AstValue::Scalar { value, .. } => {
			let text = value.as_text();
			for cap in param_capture_regex().captures_iter(&text) {
				let Some(param) = cap.get(1) else {
					continue;
				};
				let param = param.as_str().to_string();
				if let Some(def) = index.definitions.get_mut(def_idx)
					&& !def.required_params.contains(&param)
				{
					def.required_params.push(param);
				}
			}
		}
		AstValue::Block { items, .. } => {
			for item in items {
				match item {
					AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
						record_param_tokens(index, Some(def_idx), value)
					}
					AstStatement::Comment { .. } => {}
				}
			}
		}
	}
}

fn alias_capture_regex() -> &'static Regex {
	static REGEX: OnceLock<Regex> = OnceLock::new();
	REGEX.get_or_init(|| Regex::new(r"\b(ROOT|FROM|THIS|PREV)\b").expect("valid alias regex"))
}

fn param_capture_regex() -> &'static Regex {
	static REGEX: OnceLock<Regex> = OnceLock::new();
	REGEX.get_or_init(|| Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)\$").expect("valid param regex"))
}

fn top_level_symbol_kind(
	file_kind: ScriptFileKind,
	key: &str,
	scope_id: usize,
	index: &SemanticIndex,
) -> Option<SymbolKind> {
	if scope_kind(index, scope_id) != ScopeKind::File {
		return None;
	}
	match file_kind {
		ScriptFileKind::ScriptedEffects if !is_keyword(key) => Some(SymbolKind::ScriptedEffect),
		ScriptFileKind::ScriptedTriggers if !is_keyword(key) => Some(SymbolKind::ScriptedTrigger),
		ScriptFileKind::Decisions if !is_keyword(key) && !is_decision_container_key(key) => {
			Some(SymbolKind::Decision)
		}
		ScriptFileKind::DiplomaticActions if !is_keyword(key) => Some(SymbolKind::DiplomaticAction),
		ScriptFileKind::NewDiplomaticActions
			if !is_keyword(key) && !is_new_diplomatic_actions_container_key(key) =>
		{
			Some(SymbolKind::DiplomaticAction)
		}
		ScriptFileKind::TriggeredModifiers if !is_keyword(key) => {
			Some(SymbolKind::TriggeredModifier)
		}
		_ => None,
	}
}

fn symbol_definition_kind(
	file_kind: ScriptFileKind,
	key: &str,
	scope_id: usize,
	index: &SemanticIndex,
) -> Option<SymbolKind> {
	if let Some(kind) = top_level_symbol_kind(file_kind, key, scope_id, index) {
		return Some(kind);
	}
	if file_kind == ScriptFileKind::Decisions
		&& is_decision_entry_scope(index, scope_id)
		&& !is_keyword(key)
	{
		return Some(SymbolKind::Decision);
	}
	None
}

fn root_scope_type_for_file_kind(file_kind: ScriptFileKind) -> ScopeType {
	match file_kind {
		ScriptFileKind::OnActions => ScopeType::Unknown,
		ScriptFileKind::Decisions
		| ScriptFileKind::DiplomaticActions
		| ScriptFileKind::Achievements
		| ScriptFileKind::Ages
		| ScriptFileKind::Ideas
		| ScriptFileKind::GovernmentReforms
		| ScriptFileKind::CustomGui
		| ScriptFileKind::AdvisorTypes
		| ScriptFileKind::EventModifiers
		| ScriptFileKind::CbTypes
		| ScriptFileKind::GovernmentNames
		| ScriptFileKind::CustomizableLocalization
		| ScriptFileKind::CountryTags
		| ScriptFileKind::Countries
		| ScriptFileKind::CountryHistory
		| ScriptFileKind::ChurchAspects
		| ScriptFileKind::Factions
		| ScriptFileKind::Hegemons
		| ScriptFileKind::PersonalDeities
		| ScriptFileKind::FetishistCults
		| ScriptFileKind::PeaceTreaties
		| ScriptFileKind::Policies
		| ScriptFileKind::MercenaryCompanies
		| ScriptFileKind::Technologies
		| ScriptFileKind::TechnologyGroups
		| ScriptFileKind::EstateAgendas
		| ScriptFileKind::EstatePrivileges
		| ScriptFileKind::Estates
		| ScriptFileKind::ParliamentBribes
		| ScriptFileKind::ParliamentIssues => ScopeType::Country,
		ScriptFileKind::Missions | ScriptFileKind::NewDiplomaticActions => ScopeType::Country,
		ScriptFileKind::Buildings
		| ScriptFileKind::GreatProjects
		| ScriptFileKind::Institutions
		| ScriptFileKind::ProvinceTriggeredModifiers
		| ScriptFileKind::ProvinceHistory
		| ScriptFileKind::StateEdicts => ScopeType::Province,
		ScriptFileKind::TriggeredModifiers => ScopeType::Country,
		_ => ScopeType::Unknown,
	}
}

fn is_new_diplomatic_actions_container_key(key: &str) -> bool {
	matches!(key, "static_actions")
}

fn is_decision_entry_scope(index: &SemanticIndex, scope_id: usize) -> bool {
	let Some(scope) = index.scopes.get(scope_id) else {
		return false;
	};
	if scope.kind != ScopeKind::Block {
		return false;
	}
	let Some(parent_scope_id) = scope.parent else {
		return false;
	};
	scope_kind(index, parent_scope_id) == ScopeKind::File
}

fn is_scripted_effect_call_candidate(
	ctx: &BuildContext<'_>,
	file_kind: ScriptFileKind,
	key: &str,
	scope_id: usize,
	index: &SemanticIndex,
) -> bool {
	if is_keyword(key) || is_alias_key(key) {
		return false;
	}
	if is_template_param_placeholder_key(key) {
		return false;
	}
	if is_dynamic_scope_reference_key(key) {
		return false;
	}
	if scope_kind(index, scope_id) == ScopeKind::File {
		return false;
	}
	if is_province_id_selector(key) {
		return false;
	}
	if effect_context_scope_semantics(ctx, key, scope_id, index).is_some() {
		return false;
	}
	if is_map_group_scope_key(ctx, key, scope_id, index) {
		return false;
	}
	if is_builtin_effect(key)
		|| is_builtin_trigger(key)
		|| is_builtin_scope_changer(key)
		|| is_builtin_iterator(key)
		|| is_builtin_special_block(key)
	{
		return false;
	}
	if !is_effect_like_scope(index, scope_id) {
		return false;
	}
	if !allows_generic_scripted_effect_fallback(scope_kind(index, scope_id)) {
		return false;
	}
	if file_kind == ScriptFileKind::Decisions && is_decision_entry_scope(index, scope_id) {
		return false;
	}
	if file_kind == ScriptFileKind::ScriptedEffects
		&& scope_kind(index, scope_id) == ScopeKind::File
	{
		return false;
	}
	true
}

fn is_scripted_trigger_call_candidate(
	ctx: &BuildContext<'_>,
	file_kind: ScriptFileKind,
	key: &str,
	scope_id: usize,
	index: &SemanticIndex,
) -> bool {
	if is_keyword(key) || is_alias_key(key) {
		return false;
	}
	if is_template_param_placeholder_key(key) {
		return false;
	}
	if is_dynamic_scope_reference_key(key) {
		return false;
	}
	if scope_kind(index, scope_id) == ScopeKind::File {
		return false;
	}
	if is_province_id_selector(key) || is_country_tag_selector(key) {
		return false;
	}
	if event_scope_type(key).is_some() {
		return false;
	}
	if effect_context_scope_semantics(ctx, key, scope_id, index).is_some() {
		return false;
	}
	if is_map_group_scope_key(ctx, key, scope_id, index) {
		return false;
	}
	if is_builtin_effect(key)
		|| is_builtin_trigger(key)
		|| is_builtin_scope_changer(key)
		|| is_builtin_iterator(key)
		|| is_builtin_special_block(key)
	{
		return false;
	}
	if !is_trigger_like_scope(index, scope_id) {
		return false;
	}
	if file_kind == ScriptFileKind::ScriptedTriggers
		&& scope_kind(index, scope_id) == ScopeKind::File
	{
		return false;
	}
	true
}

fn is_effect_like_scope(index: &SemanticIndex, scope_id: usize) -> bool {
	if scope_kind(index, scope_id) == ScopeKind::Trigger {
		return false;
	}
	!is_under_trigger_scope(index, scope_id)
}

fn is_trigger_like_scope(index: &SemanticIndex, scope_id: usize) -> bool {
	scope_kind(index, scope_id) == ScopeKind::Trigger || is_under_trigger_scope(index, scope_id)
}

fn allows_generic_scripted_effect_fallback(scope_kind: ScopeKind) -> bool {
	matches!(
		scope_kind,
		ScopeKind::Effect | ScopeKind::AliasBlock | ScopeKind::Loop | ScopeKind::ScriptedEffect
	)
}

fn is_under_trigger_scope(index: &SemanticIndex, mut scope_id: usize) -> bool {
	loop {
		let Some(scope) = index.scopes.get(scope_id) else {
			return false;
		};
		if scope.kind == ScopeKind::Trigger {
			return true;
		}
		let Some(parent) = scope.parent else {
			return false;
		};
		scope_id = parent;
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EffectContextScopeSemantics {
	EffectContainer,
	Iterator(ScopeType),
	ScopeChanger(ScopeType),
}

fn is_explicit_effect_context_scope(index: &SemanticIndex, scope_id: usize) -> bool {
	is_effect_like_scope(index, scope_id)
		&& allows_generic_scripted_effect_fallback(scope_kind(index, scope_id))
}

fn effect_context_scope_semantics(
	ctx: &BuildContext<'_>,
	key: &str,
	scope_id: usize,
	index: &SemanticIndex,
) -> Option<EffectContextScopeSemantics> {
	if !is_explicit_effect_context_scope(index, scope_id) {
		return None;
	}

	if key == "random_list" {
		return Some(EffectContextScopeSemantics::EffectContainer);
	}
	if matches!(
		key,
		"for" | "while" | "IF" | "ELSE_IF" | "else_if" | "ELSE" | "else"
	) {
		return Some(EffectContextScopeSemantics::EffectContainer);
	}
	if matches!(
		key,
		"every_country" | "every_subject_country" | "every_known_country" | "random_country"
	) {
		return Some(EffectContextScopeSemantics::Iterator(ScopeType::Country));
	}
	if matches!(
		key,
		"random_owned_province" | "random_province" | "every_province"
	) {
		return Some(EffectContextScopeSemantics::Iterator(ScopeType::Province));
	}
	if key == "overlord" {
		return Some(EffectContextScopeSemantics::ScopeChanger(
			ScopeType::Country,
		));
	}
	if let Some(selector) = numeric_effect_context_semantics(key) {
		return Some(selector);
	}
	if is_country_tag_selector(key) {
		return Some(EffectContextScopeSemantics::ScopeChanger(
			ScopeType::Country,
		));
	}
	if is_map_group_scope_key(ctx, key, scope_id, index) {
		return Some(EffectContextScopeSemantics::Iterator(ScopeType::Province));
	}

	None
}

fn is_on_actions_callback_root(
	file_kind: ScriptFileKind,
	parent_scope_id: usize,
	index: &SemanticIndex,
	key: &str,
) -> bool {
	file_kind == ScriptFileKind::OnActions
		&& scope_kind(index, parent_scope_id) == ScopeKind::File
		&& key.starts_with("on_")
}

fn on_actions_callback_this_type(key: &str) -> ScopeType {
	match key {
		"on_adm_development" | "on_dip_development" | "on_mil_development" => ScopeType::Province,
		_ => ScopeType::Country,
	}
}

fn on_actions_callback_from_type(_key: &str) -> ScopeType {
	ScopeType::Country
}

fn create_child_scope(
	index: &mut SemanticIndex,
	parent_scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	span: &SpanRange,
	items: &[AstStatement],
) -> usize {
	let mut aliases = scope_aliases(index, parent_scope_id);
	aliases.insert("PREV".to_string(), scope_this_type(index, parent_scope_id));
	let mut this_type = scope_this_type(index, parent_scope_id);
	let mut kind = ScopeKind::Block;
	let enclosing_conditional_context = nearest_conditional_context_kind(index, parent_scope_id);
	let effect_context_semantics = effect_context_scope_semantics(ctx, key, parent_scope_id, index);

	if is_on_actions_callback_root(ctx.file_kind, parent_scope_id, index, key) {
		kind = ScopeKind::Effect;
		this_type = on_actions_callback_this_type(key);
		aliases.insert("THIS".to_string(), this_type);
		aliases.insert("ROOT".to_string(), this_type);
		aliases.insert("FROM".to_string(), on_actions_callback_from_type(key));
	} else if key == "trigger"
		|| key == "limit"
		|| key == "potential"
		|| key == "allow"
		|| key == "condition"
		|| key == "hidden_trigger"
	{
		kind = ScopeKind::Trigger;
	} else if matches!(
		key,
		"effect"
			| "after" | "hidden_effect"
			| "immediate"
			| "on_add"
			| "on_remove"
			| "on_start"
			| "on_end"
			| "on_monthly"
	) {
		kind = ScopeKind::Effect;
	} else if let Some(file_kind_scope_kind) = file_kind_container_scope_kind(ctx.file_kind, key) {
		kind = file_kind_scope_kind;
	} else if let Some(semantics) = effect_context_semantics {
		match semantics {
			EffectContextScopeSemantics::EffectContainer => {
				kind = ScopeKind::Effect;
			}
			EffectContextScopeSemantics::Iterator(target) => {
				kind = ScopeKind::Loop;
				this_type = target;
				aliases.insert("THIS".to_string(), target);
			}
			EffectContextScopeSemantics::ScopeChanger(target) => {
				kind = ScopeKind::AliasBlock;
				this_type = target;
				aliases.insert("THIS".to_string(), target);
			}
		}
	} else if is_province_id_selector(key) && scope_kind(index, parent_scope_id) != ScopeKind::File
	{
		kind = ScopeKind::AliasBlock;
		this_type = ScopeType::Province;
		aliases.insert("THIS".to_string(), ScopeType::Province);
	} else if is_map_group_scope_key(ctx, key, parent_scope_id, index) {
		kind = ScopeKind::Loop;
		this_type = ScopeType::Province;
		aliases.insert("THIS".to_string(), ScopeType::Province);
	} else if is_builtin_special_block(key) {
		kind = special_block_scope_kind(key);
	} else if is_builtin_iterator(key) {
		kind = ScopeKind::Loop;
		this_type = iterator_scope_type(key).unwrap_or(this_type);
		aliases.insert("THIS".to_string(), this_type);
	} else if is_builtin_scope_changer(key) {
		kind = ScopeKind::AliasBlock;
		this_type = scope_changer_target_type(key).unwrap_or(this_type);
		aliases.insert("THIS".to_string(), this_type);
	} else if key == "every_owned_province" {
		kind = ScopeKind::Loop;
		this_type = ScopeType::Province;
		aliases.insert("THIS".to_string(), ScopeType::Province);
	} else if key == "ROOT" {
		kind = ScopeKind::AliasBlock;
		this_type = aliases.get("ROOT").copied().unwrap_or(ScopeType::Unknown);
		aliases.insert("THIS".to_string(), this_type);
	} else if key == "THIS" {
		kind = ScopeKind::AliasBlock;
		this_type = aliases.get("THIS").copied().unwrap_or(ScopeType::Unknown);
		aliases.insert("THIS".to_string(), this_type);
	} else if key == "FROM" {
		kind = ScopeKind::AliasBlock;
		this_type = aliases.get("FROM").copied().unwrap_or(ScopeType::Unknown);
		aliases.insert("THIS".to_string(), this_type);
	} else if key == "PREV" {
		kind = ScopeKind::AliasBlock;
		this_type = aliases.get("PREV").copied().unwrap_or(ScopeType::Unknown);
		aliases.insert("THIS".to_string(), this_type);
	} else if let Some(event_this_type) = event_scope_type(key) {
		kind = ScopeKind::Event;
		this_type = event_this_type;
		aliases.insert("THIS".to_string(), event_this_type);
		aliases.insert("ROOT".to_string(), event_this_type);
		if let Some(from_type) = event_from_type(key) {
			aliases.insert("FROM".to_string(), from_type);
		}
	} else if ctx.file_kind == ScriptFileKind::ScriptedTriggers
		&& scope_kind(index, parent_scope_id) == ScopeKind::File
		&& !is_keyword(key)
	{
		kind = ScopeKind::Trigger;
	} else if ctx.file_kind == ScriptFileKind::ScriptedEffects
		&& scope_kind(index, parent_scope_id) == ScopeKind::File
		&& !is_keyword(key)
	{
		kind = ScopeKind::ScriptedEffect;
	}

	if key == "if" || key == "else" {
		if matches!(
			enclosing_conditional_context,
			Some(ScopeKind::Effect | ScopeKind::ScriptedEffect)
		) || effect_context_semantics == Some(EffectContextScopeSemantics::EffectContainer)
		{
			kind = ScopeKind::Effect;
		} else {
			kind = ScopeKind::Trigger;
		}
	}

	if key == "NOT" || key == "OR" || key == "AND" {
		kind = ScopeKind::Trigger;
	}

	if key == "option" {
		kind = ScopeKind::Effect;
	}

	if event_scope_type(key).is_some() && !items.is_empty() {
		kind = ScopeKind::Event;
	}

	push_scope(
		index,
		kind,
		Some(parent_scope_id),
		this_type,
		aliases,
		ctx.mod_id,
		ctx.path,
		span.start.line,
	)
}

fn nearest_conditional_context_kind(
	index: &SemanticIndex,
	mut scope_id: usize,
) -> Option<ScopeKind> {
	loop {
		let scope = index.scopes.get(scope_id)?;
		match scope.kind {
			ScopeKind::Trigger | ScopeKind::Effect | ScopeKind::ScriptedEffect => {
				return Some(scope.kind);
			}
			_ => {}
		}
		scope_id = scope.parent?;
	}
}

fn iterator_scope_type(key: &str) -> Option<ScopeType> {
	match key {
		"all_core_province"
		| "all_owned_province"
		| "any_owned_province"
		| "all_state_province"
		| "every_province"
		| "random_owned_province"
		| "random_province" => Some(ScopeType::Province),
		"all_subject_country"
		| "any_country"
		| "every_country"
		| "every_known_country"
		| "every_subject_country"
		| "random_country" => Some(ScopeType::Country),
		_ => None,
	}
}

fn is_map_group_scope_key(
	ctx: &BuildContext<'_>,
	key: &str,
	scope_id: usize,
	index: &SemanticIndex,
) -> bool {
	if ctx.map_groups.contains(key) {
		return scope_kind(index, scope_id) != ScopeKind::File;
	}
	if is_explicit_effect_context_scope(index, scope_id) {
		return looks_like_map_group_key(key);
	}
	matches!(
		ctx.file_kind,
		ScriptFileKind::Missions | ScriptFileKind::CbTypes
	) && scope_kind(index, scope_id) != ScopeKind::File
		&& looks_like_map_group_key(key)
}

fn looks_like_map_group_key(key: &str) -> bool {
	key.ends_with("_area")
		|| key.ends_with("_region")
		|| key.ends_with("_superregion")
		|| key.ends_with("_provincegroup")
}

fn is_country_file_reference(value: &str) -> bool {
	value.starts_with("countries/") && value.ends_with(".txt")
}

fn is_country_tag_text(value: &str) -> bool {
	value.len() == 3 && value.chars().all(|ch| ch.is_ascii_uppercase())
}

fn is_province_id_text(value: &str) -> bool {
	value.parse::<u32>().is_ok_and(|id| id > 0)
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

fn is_country_tag_selector(key: &str) -> bool {
	key.len() == 3 && key.chars().all(|ch| ch.is_ascii_uppercase())
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

fn is_province_id_selector(key: &str) -> bool {
	key.parse::<u32>().map(|value| value > 100).unwrap_or(false)
}

fn is_dynamic_scope_reference_key(key: &str) -> bool {
	key.starts_with("event_target:")
}

fn is_template_param_placeholder_key(key: &str) -> bool {
	extract_template_param_name(key).is_some()
}

fn numeric_effect_context_semantics(key: &str) -> Option<EffectContextScopeSemantics> {
	let value = key.parse::<u32>().ok()?;
	if value <= 100 {
		Some(EffectContextScopeSemantics::EffectContainer)
	} else {
		Some(EffectContextScopeSemantics::ScopeChanger(
			ScopeType::Province,
		))
	}
}

fn file_kind_container_scope_kind(file_kind: ScriptFileKind, key: &str) -> Option<ScopeKind> {
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

fn scope_changer_target_type(key: &str) -> Option<ScopeType> {
	match key {
		"capital_scope" => Some(ScopeType::Province),
		"owner" => Some(ScopeType::Country),
		_ => None,
	}
}

fn special_block_scope_kind(key: &str) -> ScopeKind {
	match key {
		"possible"
		| "visible"
		| "happened"
		| "provinces_to_highlight"
		| "exclude_from_progress" => ScopeKind::Trigger,
		_ => ScopeKind::Block,
	}
}

#[allow(clippy::too_many_arguments)]
fn push_scope(
	index: &mut SemanticIndex,
	kind: ScopeKind,
	parent: Option<usize>,
	this_type: ScopeType,
	aliases: HashMap<String, ScopeType>,
	mod_id: &str,
	path: &Path,
	line: usize,
) -> usize {
	let id = index.scopes.len();
	index.scopes.push(ScopeNode {
		id,
		kind,
		parent,
		this_type,
		aliases,
		mod_id: mod_id.to_string(),
		path: path.to_path_buf(),
		span: SourceSpan { line, column: 1 },
	});
	id
}

fn scope_kind(index: &SemanticIndex, scope_id: usize) -> ScopeKind {
	index
		.scopes
		.get(scope_id)
		.map(|scope| scope.kind)
		.unwrap_or(ScopeKind::Block)
}

fn scope_this_type(index: &SemanticIndex, scope_id: usize) -> ScopeType {
	index
		.scopes
		.get(scope_id)
		.map(|scope| scope.this_type)
		.unwrap_or(ScopeType::Unknown)
}

fn scope_aliases(index: &SemanticIndex, scope_id: usize) -> HashMap<String, ScopeType> {
	index
		.scopes
		.get(scope_id)
		.map(|scope| scope.aliases.clone())
		.unwrap_or_default()
}

fn line_from_stmt(stmt: Option<&AstStatement>) -> usize {
	stmt.map(|item| match item {
		AstStatement::Assignment { span, .. } => span.start.line,
		AstStatement::Item { span, .. } => span.start.line,
		AstStatement::Comment { span, .. } => span.start.line,
	})
	.unwrap_or(1)
}

fn find_scripted_effect_definition(
	index: &SemanticIndex,
	mod_id: &str,
	path: &Path,
	name: &str,
) -> Option<usize> {
	index.definitions.iter().position(|item| {
		item.kind == SymbolKind::ScriptedEffect
			&& item.mod_id == mod_id
			&& item.path == path
			&& item.local_name == name
	})
}

fn is_event_call(key: &str, value: &AstValue) -> bool {
	event_scope_type(key).is_some() && matches!(value, AstValue::Block { .. })
}

fn extract_event_call_id(value: &AstValue) -> Option<String> {
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	extract_assignment_scalar(items, "id")
}

fn extract_assignment_scalar(items: &[AstStatement], name: &str) -> Option<String> {
	for item in items {
		if let AstStatement::Assignment { key, value, .. } = item
			&& key == name
			&& let Some(text) = scalar_text(value)
		{
			return Some(text);
		}
	}
	None
}

fn scalar_text(value: &AstValue) -> Option<String> {
	let AstValue::Scalar { value, .. } = value else {
		return None;
	};
	Some(value.as_text())
}

fn collect_required_params(items: &[AstStatement]) -> Vec<String> {
	let param_re = Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)\$").expect("valid param regex");
	let mut params = Vec::new();
	for stmt in items {
		match stmt {
			AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
				collect_params_from_value(value, &param_re, &mut params)
			}
			AstStatement::Comment { .. } => {}
		}
	}
	params
}

fn collect_params_from_value(value: &AstValue, re: &Regex, out: &mut Vec<String>) {
	match value {
		AstValue::Scalar { value, .. } => {
			let text = value.as_text();
			for cap in re.captures_iter(&text) {
				if let Some(param) = cap.get(1) {
					out.push(param.as_str().to_string());
				}
			}
		}
		AstValue::Block { items, .. } => {
			for stmt in items {
				match stmt {
					AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
						collect_params_from_value(value, re, out)
					}
					AstStatement::Comment { .. } => {}
				}
			}
		}
	}
}

#[derive(Default)]
struct ProvidedParams {
	names: Vec<String>,
	bindings: Vec<ParamBinding>,
}

fn collect_provided_params(local_name: &str, items: &[AstStatement]) -> ProvidedParams {
	let mut params = ProvidedParams::default();
	let contract_names = explicit_contract_param_names(local_name);
	for stmt in items {
		if let AstStatement::Assignment { key, value, .. } = stmt {
			let is_explicit_param_name = key
				.chars()
				.all(|ch| ch.is_ascii_uppercase() || ch == '_' || ch.is_ascii_digit())
				|| contract_names.contains(key.as_str());
			let is_named_param_binding = !key.is_empty()
				&& key
					.chars()
					.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
			if is_explicit_param_name || is_named_param_binding {
				params.names.push(key.clone());
			}
			if is_named_param_binding && let Some(value) = scalar_text(value) {
				params.bindings.push(ParamBinding {
					name: key.clone(),
					value,
				});
			}
		}
	}
	params
}

fn collect_scope_param_names(items: &[AstStatement]) -> Vec<String> {
	let mut names = HashSet::new();
	collect_scope_param_names_from_statements(items, &mut names);
	let mut collected: Vec<String> = names.into_iter().collect();
	collected.sort();
	collected
}

fn extract_template_param_name(value: &str) -> Option<&str> {
	let trimmed = value.trim();
	if !(trimmed.starts_with('$') && trimmed.ends_with('$') && trimmed.len() > 2) {
		return None;
	}
	let param = &trimmed[1..trimmed.len() - 1];
	if param
		.chars()
		.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
	{
		Some(param)
	} else {
		None
	}
}

fn collect_scope_param_names_from_statements(items: &[AstStatement], names: &mut HashSet<String>) {
	for stmt in items {
		if let AstStatement::Assignment { key, value, .. } = stmt {
			if let Some(param_name) = extract_template_param_name(key)
				&& matches!(value, AstValue::Block { .. })
			{
				names.insert(param_name.to_string());
			}
			if let AstValue::Block { items, .. } = value {
				collect_scope_param_names_from_statements(items, names);
			}
		}
	}
}

fn is_alias_key(key: &str) -> bool {
	matches!(key, "ROOT" | "FROM" | "THIS" | "PREV")
}

pub(crate) fn is_decision_container_key(key: &str) -> bool {
	matches!(
		key,
		"country_decisions" | "province_decisions" | "religion_decisions" | "government_decisions"
	)
}

fn is_keyword(key: &str) -> bool {
	if is_reserved_keyword(key) || is_contextual_keyword(key) {
		return true;
	}
	matches!(key, "condition" | "from")
}

fn looks_like_ui_container_key(key: &str) -> bool {
	key.ends_with("Type")
		|| matches!(
			key,
			"containerWindow" | "window" | "button" | "icon" | "sprite" | "shield"
		)
}

fn is_ui_identifier_candidate(value: &str) -> bool {
	!value.is_empty()
		&& value.len() <= 128
		&& value
			.chars()
			.all(|ch| matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '.' | '-' | ':'))
}

fn is_ui_resource_key(key: &str) -> bool {
	matches!(
		key,
		"spriteType"
			| "buttonSprite"
			| "quadTextureSprite"
			| "texturefile"
			| "icon" | "iconType"
			| "pdxmesh"
			| "mesh"
	)
}

fn is_inferred_callable_kind(kind: SymbolKind) -> bool {
	matches!(
		kind,
		SymbolKind::ScriptedEffect | SymbolKind::ScriptedTrigger
	)
}

fn resolve_reference_targets_for_kind(
	index: &SemanticIndex,
	reference: &SymbolReference,
	kind: SymbolKind,
) -> Vec<usize> {
	if reference.kind != kind {
		return Vec::new();
	}

	let mut exact = Vec::new();
	for (idx, def) in index.definitions.iter().enumerate() {
		if def.kind != kind {
			continue;
		}
		if def.module == reference.module && def.local_name == reference.name {
			exact.push(idx);
		}
	}
	if !exact.is_empty() {
		return exact;
	}

	let mut by_local = Vec::new();
	for (idx, def) in index.definitions.iter().enumerate() {
		if def.kind != kind {
			continue;
		}
		if def.local_name == reference.name {
			by_local.push(idx);
		}
	}
	if by_local.len() == 1 {
		return by_local;
	}

	let by_mod: Vec<usize> = by_local
		.into_iter()
		.filter(|idx| {
			index
				.definitions
				.get(*idx)
				.map(|def| def.mod_id == reference.mod_id)
				.unwrap_or(false)
		})
		.collect();
	if by_mod.len() == 1 {
		return by_mod;
	}

	Vec::new()
}

pub fn resolve_scripted_effect_reference_targets(
	index: &SemanticIndex,
	reference: &SymbolReference,
) -> Vec<usize> {
	resolve_reference_targets_for_kind(index, reference, SymbolKind::ScriptedEffect)
}

pub fn resolve_scripted_trigger_reference_targets(
	index: &SemanticIndex,
	reference: &SymbolReference,
) -> Vec<usize> {
	resolve_reference_targets_for_kind(index, reference, SymbolKind::ScriptedTrigger)
}

fn infer_definition_scope_from_references(index: &mut SemanticIndex) {
	let callable_scope_map = build_inferred_callable_scope_map(index);

	let mut observed_masks: Vec<u8> = index
		.definitions
		.iter()
		.map(|definition| {
			if is_inferred_callable_kind(definition.kind) {
				scope_type_mask(definition.declared_this_type)
			} else {
				0
			}
		})
		.collect();

	let mut changed = true;
	while changed {
		changed = false;
		for reference in &index.references {
			if !is_inferred_callable_kind(reference.kind) {
				continue;
			}
			let caller_mask = effective_scope_mask_with_overrides(
				index,
				&callable_scope_map,
				&observed_masks,
				reference.scope_id,
			);
			if caller_mask == 0 {
				continue;
			}
			let target_defs = match reference.kind {
				SymbolKind::ScriptedEffect => {
					resolve_scripted_effect_reference_targets(index, reference)
				}
				SymbolKind::ScriptedTrigger => {
					resolve_scripted_trigger_reference_targets(index, reference)
				}
				_ => Vec::new(),
			};
			for def_idx in target_defs {
				let mut merged = observed_masks[def_idx] | caller_mask;
				if let Some(definition) = index.definitions.get(def_idx)
					&& definition.kind == SymbolKind::ScriptedEffect
				{
					for binding in &reference.param_bindings {
						if !definition
							.scope_param_names
							.iter()
							.any(|name| name == &binding.name)
						{
							continue;
						}
						merged |= binding_value_scope_mask(
							index,
							&callable_scope_map,
							&observed_masks,
							reference.scope_id,
							&binding.value,
						);
					}
				}
				if merged != observed_masks[def_idx] {
					observed_masks[def_idx] = merged;
					changed = true;
				}
			}
		}
	}

	let backfill_candidates: Vec<bool> = index
		.definitions
		.iter()
		.enumerate()
		.map(|(idx, definition)| {
			definition.kind == SymbolKind::ScriptedEffect && observed_masks[idx] == 0
		})
		.collect();
	for scope_id in 0..index.scopes.len() {
		let scope_mask = effective_scope_mask_with_overrides(
			index,
			&callable_scope_map,
			&observed_masks,
			scope_id,
		);
		if scope_mask == 0 {
			continue;
		}
		let Some(def_idx) = nearest_enclosing_scripted_effect_definition_index(
			index,
			&callable_scope_map,
			scope_id,
		) else {
			continue;
		};
		if !backfill_candidates[def_idx] {
			continue;
		}
		observed_masks[def_idx] |= scope_mask;
	}

	for usage in &index.key_usages {
		if usage.key != "capital_scope" {
			continue;
		}
		let Some(def_idx) =
			nearest_enclosing_callable_definition_index(index, &callable_scope_map, usage.scope_id)
		else {
			continue;
		};
		let Some(definition) = index.definitions.get(def_idx) else {
			continue;
		};
		if definition.kind != SymbolKind::ScriptedTrigger {
			continue;
		}
		observed_masks[def_idx] |= scope_type_mask(ScopeType::Country);
	}

	for (idx, definition) in index.definitions.iter_mut().enumerate() {
		if !is_inferred_callable_kind(definition.kind) {
			continue;
		}
		definition.inferred_this_mask = observed_masks[idx];
		definition.inferred_this_type = scope_type_from_mask(observed_masks[idx]);
	}
}

pub(crate) fn build_inferred_callable_scope_map(index: &SemanticIndex) -> HashMap<usize, usize> {
	index
		.definitions
		.iter()
		.enumerate()
		.filter_map(|(idx, definition)| {
			is_inferred_callable_kind(definition.kind).then_some((definition.scope_id, idx))
		})
		.collect()
}

pub(crate) fn collect_inferred_callable_masks(index: &SemanticIndex) -> Vec<u8> {
	index
		.definitions
		.iter()
		.map(|definition| definition.inferred_this_mask)
		.collect()
}

fn binding_value_scope_mask(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	observed_masks: &[u8],
	scope_id: usize,
	value: &str,
) -> u8 {
	let trimmed = value.trim();
	if trimmed.is_empty() {
		return 0;
	}
	match trimmed {
		"THIS" | "ROOT" | "FROM" | "PREV" => {
			return effective_alias_scope_mask_with_overrides(
				index,
				callable_scope_map,
				observed_masks,
				scope_id,
				trimmed,
			);
		}
		"owner" => return scope_type_mask(ScopeType::Country),
		"capital_scope" => return scope_type_mask(ScopeType::Province),
		_ => {}
	}
	if is_country_tag_selector(trimmed) {
		return scope_type_mask(ScopeType::Country);
	}
	if is_province_id_selector(trimmed) {
		return scope_type_mask(ScopeType::Province);
	}
	0
}

pub(crate) fn effective_alias_scope_mask_with_overrides(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	observed_masks: &[u8],
	mut scope_id: usize,
	alias: &str,
) -> u8 {
	let mut fallback_mask = 0;
	loop {
		let Some(scope) = index.scopes.get(scope_id) else {
			return fallback_mask;
		};
		if alias == "THIS" {
			let this_mask = scope_type_mask(scope.this_type);
			if this_mask != 0 {
				return this_mask;
			}
			if let Some(def_idx) = callable_scope_map.get(&scope_id) {
				let inferred_mask = observed_masks.get(*def_idx).copied().unwrap_or(0);
				if inferred_mask != 0 {
					return inferred_mask;
				}
			}
		}
		if let Some(alias_type) = scope.aliases.get(alias) {
			let alias_mask = scope_type_mask(*alias_type);
			if alias_mask != 0 {
				return alias_mask;
			}
		}
		if fallback_mask == 0
			&& let Some(def_idx) = callable_scope_map.get(&scope_id)
		{
			let inferred_mask = observed_masks.get(*def_idx).copied().unwrap_or(0);
			if inferred_mask != 0 {
				fallback_mask = inferred_mask;
			}
		}
		let Some(parent) = scope.parent else {
			return fallback_mask;
		};
		scope_id = parent;
	}
}

fn nearest_enclosing_callable_definition_index(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	mut scope_id: usize,
) -> Option<usize> {
	loop {
		if let Some(def_idx) = callable_scope_map.get(&scope_id)
			&& index
				.definitions
				.get(*def_idx)
				.is_some_and(|definition| is_inferred_callable_kind(definition.kind))
		{
			return Some(*def_idx);
		}
		let parent = index.scopes.get(scope_id).and_then(|scope| scope.parent)?;
		scope_id = parent;
	}
}

fn nearest_enclosing_scripted_effect_definition_index(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	scope_id: usize,
) -> Option<usize> {
	let def_idx = nearest_enclosing_callable_definition_index(index, callable_scope_map, scope_id)?;
	index
		.definitions
		.get(def_idx)
		.filter(|definition| definition.kind == SymbolKind::ScriptedEffect)
		.map(|_| def_idx)
}

fn scope_type_mask(scope_type: ScopeType) -> u8 {
	match scope_type {
		ScopeType::Country => 0b01,
		ScopeType::Province => 0b10,
		ScopeType::Unknown => 0,
	}
}

fn scope_type_from_mask(mask: u8) -> ScopeType {
	match mask {
		0b01 => ScopeType::Country,
		0b10 => ScopeType::Province,
		_ => ScopeType::Unknown,
	}
}

pub(crate) fn effective_scope_mask_with_overrides(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	observed_masks: &[u8],
	mut scope_id: usize,
) -> u8 {
	loop {
		let Some(scope) = index.scopes.get(scope_id) else {
			return 0;
		};
		if scope.this_type != ScopeType::Unknown {
			return scope_type_mask(scope.this_type);
		}
		if let Some(def_idx) = callable_scope_map.get(&scope_id)
			&& let Some(inferred_mask) = observed_masks.get(*def_idx).copied()
			&& inferred_mask != 0
		{
			return inferred_mask;
		}
		let Some(parent) = scope.parent else {
			return 0;
		};
		scope_id = parent;
	}
}

#[cfg(test)]
mod tests {
	use super::{
		ScriptFileKind, build_inferred_callable_scope_map, build_semantic_index,
		classify_script_file, collect_inferred_callable_masks,
		effective_alias_scope_mask_with_overrides, parse_script_file, scope_kind,
	};
	use crate::check::analyzer::analysis::{AnalyzeOptions, analyze_visibility};
	use crate::check::model::{AnalysisMode, ScopeKind, ScopeType, SymbolKind};
	use std::fs;
	use std::path::Path;
	use tempfile::TempDir;

	fn full_army_tradition_switch_effect(name: &str) -> String {
		let branches: String = (0..=100)
			.rev()
			.map(|tradition| {
				format!(
					"\t\t\t{tradition} = {{\n\t\t\t\tPREV = {{\n\t\t\t\t\tcreate_general = {{\n\t\t\t\t\t\tculture = PREV\n\t\t\t\t\t\ttradition = {tradition}\n\t\t\t\t\t}}\n\t\t\t\t}}\n\t\t\t}}\n"
				)
			})
			.collect();
		format!(
			"{name} {{\n\t$who$ = {{\n\t\ttrigger_switch = {{\n\t\t\ton_trigger = army_tradition\n{branches}\t\t}}\n\t}}\n}}\n"
		)
	}

	fn fourth_wave_contract_definitions() -> &'static str {
		r#"
unlock_estate_privilege = {
	custom_tooltip = unlock_privilege_$estate_privilege$_tt
	hidden_effect = {
		set_country_flag = unlocked_privilege_$estate_privilege$
	}
	[[modifier_tooltip]
		custom_tooltip = unlock_estate_privilege_modifier_tooltip_tt
		tooltip = {
			add_country_modifier = {
				name = $modifier_tooltip$
				duration = -1
				desc = UNTIL_PRIVILEGE_REVOKED
			}
		}
	]
	[[effect_tooltip]
		custom_tooltip = unlock_estate_privilege_effect_tooltip_tt
		tooltip = {
			$effect_tooltip$
		}
	]
}

HAB_change_habsburg_glory = {
	[[remove]
		add_government_power = {
			value = -$remove$
		}
	]
	[[amount]
		add_government_power = {
			value = $amount$
		}
	]
}

add_legitimacy_or_reform_progress = {
	[[amount]
		tooltip = {
			add_legitimacy_equivalent = { amount = $amount$ }
		}
	]
	[[value]
		tooltip = {
			add_legitimacy_equivalent = { amount = $value$ }
		}
	]
}

EE_change_variable = {
	[[add]
		change_variable = {
			which = $which$
			value = $add$
		}
	]
	[[subtract]
		subtract_variable = {
			which = $which$
			value = $subtract$
		}
	]
	[[divide]
		divide_variable = {
			which = $which$
			value = $divide$
		}
	]
	[[multiply]
		multiply_variable = {
			which = $which$
			value = $multiply$
		}
	]
}

build_as_many_as_possible = {
	[[upgrade_target]$pick_best_function$ = {
		scope = every_owned_province
		trigger = "$all_prior_trig$
			can_build = $new_building$"
	}]
	[[construct_new]$pick_best_function$ = {
		scope = every_owned_province
		trigger = "can_build = $new_building$"
	}]
	event_target:highest_score_trade = {
		add_building_construction = {
			building = $new_building$
			speed = $speed$
			cost = $cost$
		}
	}
}

give_claims = {
	[[province] custom_tooltip = $province$ ]
	[[id] custom_tooltip = $id$ ]
	[[area] custom_tooltip = $area$ ]
	[[region] custom_tooltip = $region$ ]
}

pick_best_tags = {
	[[scope] custom_tooltip = $scope$ ]
	custom_tooltip = $scale$
	custom_tooltip = $event_target_name$
	custom_tooltip = "$global_trigger$"
	[[1] custom_tooltip = "$1$" ]
	[[2] custom_tooltip = "$2$" ]
	[[3] custom_tooltip = "$3$" ]
	[[4] custom_tooltip = "$4$" ]
	[[5] custom_tooltip = "$5$" ]
	[[10] custom_tooltip = "$10$" ]
}

ME_add_years_of_trade_income = {
	[[years] add_years_of_trade_income = { years = $years$ } ]
	[[value] add_years_of_trade_income = { years = $value$ } ]
	[[amount] add_years_of_trade_income = { years = $amount$ } ]
}

ME_tim_add_spoils_of_war = {
	[[add]
		add_government_power = {
			value = $add$
		}
	]
	[[remove]
		add_government_power = {
			value = -$remove$
		}
	]
}

ME_add_power_projection = {
	[[amount]
		add_power_projection = {
			amount = $amount$
		}
	]
	[[value]
		add_power_projection = {
			amount = $value$
		}
	]
}

create_general_scaling_with_tradition_and_pips = {
	create_general_with_pips = {
		tradition = 100
		[[add_fire] add_fire = $add_fire$ ]
		[[add_shock] add_shock = $add_shock$ ]
		[[add_manuever] add_manuever = $add_manuever$ ]
		[[add_siege] add_siege = $add_siege$ ]
	}
}

ME_automatic_colonization_effect_module = {
	any_province = {
		OR = {
			[[superregion]
				superregion = $superregion$
			]
			[[region]
				colonial_region = $region$
			]
		}
	}
	$target_region_effect$ = yes
}

country_event_with_insight = {
	country_event = {
		id = $id$
		[[days] days = $days$]
		[[random] random = $random$]
		[[tooltip] tooltip = $tooltip$]
	}
	custom_tooltip = EVENT_INSIGHT_INTRO
	custom_tooltip = $insight_tooltip$
	[[effect_tooltip] tooltip = { $effect_tooltip$ }]
}

define_and_hire_grand_vizier = {
	hire_advisor = {
		type = $type$
		[[skill] skill = $skill$]
		[[culture] culture = $culture$]
		[[religion] religion = $religion$]
		[[female] female = $female$]
		[[age] age = $age$]
		[[max_age] max_age = $max_age$]
		[[min_age] min_age = $min_age$]
		[[location] location = $location$]
	}
	add_country_modifier = {
		name = grand_vizier_$type$
		duration = -1
		desc = UNTIL_ADVISOR_REMOVAL
	}
}

ME_override_country_name = {
	[[country_name] override_country_name = $country_name$ ]
	[[name] override_country_name = $name$ ]
	[[country] override_country_name = $country$ ]
	[[value] override_country_name = $value$ ]
	[[string] override_country_name = $string$ ]
	hidden_effect = {
		set_country_flag = ME_overrid_country_name
	}
}

persia_indian_hegemony_decision_march_effect = {
	$tag_1$ = {
		custom_tooltip = persia_indian_hegemony_decision_march_$province$_tt_release_march
	}
	[[tag_2] $tag_2$ = { }]
	[[tag_3] $tag_3$ = { }]
	[[tag_4] $tag_4$ = { }]
	[[tag_5] $tag_5$ = { }]
	$trade_company_region$ = {
		add_permanent_claim = event_target:persia_march_target
	}
	$province$ = {
		owner = {
			save_event_target_as = persia_march_target
		}
	}
}

persia_indian_hegemony_decision_coup_effect = {
	$province$ = {
		owner = {
			save_event_target_as = persia_coup_target
		}
	}
	$tag_1$ = {
		custom_tooltip = persia_indian_hegemony_decision_coup_$province$_tt_independence
	}
	[[tag_2] $tag_2$ = { }]
	[[tag_3] $tag_3$ = { }]
	[[tag_4] $tag_4$ = { }]
	[[tag_5] $tag_5$ = { }]
}
"#
	}

	fn fourth_wave_s004_messages(call_rel_path: &[&str], call_source: &str) -> Vec<String> {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		let scripted_effects_dir = mod_root.join("common").join("scripted_effects");
		fs::create_dir_all(&scripted_effects_dir).expect("create scripted effects");
		fs::write(
			scripted_effects_dir.join("fourth_wave_contracts.txt"),
			fourth_wave_contract_definitions(),
		)
		.expect("write scripted effects");

		let mut call_path = mod_root.clone();
		for component in call_rel_path {
			call_path = call_path.join(component);
		}
		let call_parent = call_path.parent().expect("call parent");
		fs::create_dir_all(call_parent).expect("create call dir");
		fs::write(&call_path, call_source).expect("write call source");

		let parsed = [
			parse_script_file(
				"1013",
				&mod_root,
				&scripted_effects_dir.join("fourth_wave_contracts.txt"),
			)
			.expect("parsed scripted effects"),
			parse_script_file("1013", &mod_root, &call_path).expect("parsed call source"),
		];
		let index = build_semantic_index(&parsed);
		analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		)
		.strict
		.into_iter()
		.filter(|finding| finding.rule_id == "S004")
		.map(|finding| finding.message)
		.collect()
	}

	#[test]
	fn classify_paths() {
		assert_eq!(
			classify_script_file(std::path::Path::new("common/on_actions/00_on_actions.txt")),
			ScriptFileKind::OnActions
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("events/common/on_actions/foo.txt")),
			ScriptFileKind::OnActions
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/scripted_effects/a.txt")),
			ScriptFileKind::ScriptedEffects
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("events/a.txt")),
			ScriptFileKind::Events
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("interface/a.gui")),
			ScriptFileKind::Ui
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/achievements.txt")),
			ScriptFileKind::Achievements
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/ages/00_default.txt")),
			ScriptFileKind::Ages
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/buildings/00_buildings.txt")),
			ScriptFileKind::Buildings
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/ideas/00_country_ideas.txt")),
			ScriptFileKind::Ideas
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/great_projects/01_monuments.txt"
			)),
			ScriptFileKind::GreatProjects
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/government_reforms/01_government_reforms.txt"
			)),
			ScriptFileKind::GovernmentReforms
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/cultures/00_cultures.txt")),
			ScriptFileKind::Cultures
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/custom_gui/AdvisorActionsGui.txt"
			)),
			ScriptFileKind::CustomGui
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/advisortypes/00_advisortypes.txt"
			)),
			ScriptFileKind::AdvisorTypes
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/event_modifiers/00_modifiers.txt"
			)),
			ScriptFileKind::EventModifiers
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/cb_types/00_cb_types.txt")),
			ScriptFileKind::CbTypes
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/government_names/00_names.txt")),
			ScriptFileKind::GovernmentNames
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"customizable_localization/00_customizable_localization.txt"
			)),
			ScriptFileKind::CustomizableLocalization
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/new_diplomatic_actions/00_actions.txt"
			)),
			ScriptFileKind::NewDiplomaticActions
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"events/common/new_diplomatic_actions/00_actions.txt"
			)),
			ScriptFileKind::NewDiplomaticActions
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("missions/example.txt")),
			ScriptFileKind::Missions
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("events/decisions/example.txt")),
			ScriptFileKind::Decisions
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/institutions/00.txt")),
			ScriptFileKind::Institutions
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/province_triggered_modifiers/00.txt"
			)),
			ScriptFileKind::ProvinceTriggeredModifiers
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/scripted_triggers/00_triggers.txt"
			)),
			ScriptFileKind::ScriptedTriggers
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/country_tags/00_countries.txt")),
			ScriptFileKind::CountryTags
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/countries/00_countries.txt")),
			ScriptFileKind::Countries
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("history/countries/FRA - France.txt")),
			ScriptFileKind::CountryHistory
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("history/provinces/1 - Stockholm.txt")),
			ScriptFileKind::ProvinceHistory
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("history/wars/100yearswar.txt")),
			ScriptFileKind::Wars
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/units/00_units.txt")),
			ScriptFileKind::Units
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/religions/00_religion.txt")),
			ScriptFileKind::Religions
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/subject_types/00_subject_types.txt"
			)),
			ScriptFileKind::SubjectTypes
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/rebel_types/independence_rebels.txt"
			)),
			ScriptFileKind::RebelTypes
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/disasters/civil_war.txt")),
			ScriptFileKind::Disasters
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/government_mechanics/18_parliament_vs_monarchy.txt"
			)),
			ScriptFileKind::GovernmentMechanics
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/church_aspects/00_church_aspects.txt"
			)),
			ScriptFileKind::ChurchAspects
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/factions/00_factions.txt")),
			ScriptFileKind::Factions
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/hegemons/0_economic_hegemon.txt"
			)),
			ScriptFileKind::Hegemons
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/personal_deities/00_hindu_deities.txt"
			)),
			ScriptFileKind::PersonalDeities
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/fetishist_cults/00_fetishist_cults.txt"
			)),
			ScriptFileKind::FetishistCults
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/estate_agendas/00_generic_agendas.txt"
			)),
			ScriptFileKind::EstateAgendas
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/estate_privileges/01_church_privileges.txt"
			)),
			ScriptFileKind::EstatePrivileges
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/estates/01_church.txt")),
			ScriptFileKind::Estates
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/parliament_bribes/administrative_support.txt"
			)),
			ScriptFileKind::ParliamentBribes
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/parliament_issues/00_adm_parliament_issues.txt"
			)),
			ScriptFileKind::ParliamentIssues
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/state_edicts/edict_of_governance.txt"
			)),
			ScriptFileKind::StateEdicts
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/peace_treaties/00_peace_treaties.txt"
			)),
			ScriptFileKind::PeaceTreaties
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/bookmarks/a_new_world.txt")),
			ScriptFileKind::Bookmarks
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/policies/00_adm.txt")),
			ScriptFileKind::Policies
		);
		assert_eq!(
			classify_script_file(std::path::Path::new(
				"common/mercenary_companies/00_mercenaries.txt"
			)),
			ScriptFileKind::MercenaryCompanies
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/technologies/adm.txt")),
			ScriptFileKind::Technologies
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/technology.txt")),
			ScriptFileKind::TechnologyGroups
		);
	}

	#[test]
	fn foundation_roots_record_resource_references() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("country_tags"))
			.expect("create country tags");
		fs::create_dir_all(mod_root.join("common").join("countries")).expect("create countries");
		fs::create_dir_all(mod_root.join("common").join("units")).expect("create units");
		fs::create_dir_all(mod_root.join("history").join("countries"))
			.expect("create country history");
		fs::create_dir_all(mod_root.join("history").join("provinces"))
			.expect("create province history");
		fs::create_dir_all(mod_root.join("history").join("wars")).expect("create wars");
		fs::write(
			mod_root
				.join("common")
				.join("country_tags")
				.join("00_countries.txt"),
			"SWE = \"countries/Sweden.txt\"\n",
		)
		.expect("write country tags");
		fs::write(
			mod_root.join("common").join("countries").join("Sweden.txt"),
			r#"
graphical_culture = scandinaviangfx
preferred_religion = protestant
historical_idea_groups = {
	quality_ideas
	offensive_ideas
}
historical_units = {
	western_medieval_infantry
}
"#,
		)
		.expect("write countries");
		fs::write(
			mod_root
				.join("common")
				.join("units")
				.join("swedish_tercio.txt"),
			"type = infantry
unit_type = western
offensive_fire = 2
defensive_shock = 1
",
		)
		.expect("write units");
		fs::write(
			mod_root
				.join("history")
				.join("countries")
				.join("SWE - Sweden.txt"),
			r#"
capital = 1
1448.6.20 = {
	queen = {
		country_of_origin = SWE
	}
}
"#,
		)
		.expect("write country history");
		fs::write(
			mod_root
				.join("history")
				.join("provinces")
				.join("1-Uppland.txt"),
			"add_core = SWE\nowner = SWE\ncontroller = SWE\n",
		)
		.expect("write province history");
		fs::write(
			mod_root
				.join("history")
				.join("wars")
				.join("afghan_maratha.txt"),
			r#"
1758.1.1 = {
	add_attacker = AFG
	add_defender = MAR
}
1761.1.14 = {
	battle = {
		location = 521
		attacker = { country = AFG }
		defender = { country = MAR }
	}
}
"#,
		)
		.expect("write war history");

		let files = vec![
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("country_tags")
					.join("00_countries.txt"),
			)
			.expect("parsed country tags"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root.join("common").join("countries").join("Sweden.txt"),
			)
			.expect("parsed countries"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("units")
					.join("swedish_tercio.txt"),
			)
			.expect("parsed units"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("history")
					.join("countries")
					.join("SWE - Sweden.txt"),
			)
			.expect("parsed country history"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("history")
					.join("provinces")
					.join("1-Uppland.txt"),
			)
			.expect("parsed province history"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("history")
					.join("wars")
					.join("afghan_maratha.txt"),
			)
			.expect("parsed war history"),
		];

		let index = build_semantic_index(&files);
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/country_tags/00_countries.txt")
				&& reference.key == "country_tag:SWE"
				&& reference.value == "countries/Sweden.txt"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/countries/Sweden.txt")
				&& reference.key == "graphical_culture"
				&& reference.value == "scandinaviangfx"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/countries/Sweden.txt")
				&& reference.key == "preferred_religion"
				&& reference.value == "protestant"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/countries/Sweden.txt")
				&& reference.key == "historical_idea_groups"
				&& reference.value == "quality_ideas"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/countries/Sweden.txt")
				&& reference.key == "historical_units"
				&& reference.value == "western_medieval_infantry"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("history/countries/SWE - Sweden.txt")
				&& reference.key == "capital"
				&& reference.value == "1"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("history/countries/SWE - Sweden.txt")
				&& reference.key == "country_of_origin"
				&& reference.value == "SWE"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("history/provinces/1-Uppland.txt")
				&& reference.key == "owner"
				&& reference.value == "SWE"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("history/wars/afghan_maratha.txt")
				&& reference.key == "add_attacker"
				&& reference.value == "AFG"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("history/wars/afghan_maratha.txt")
				&& reference.key == "location"
				&& reference.value == "521"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/units/swedish_tercio.txt")
				&& reference.key == "type"
				&& reference.value == "infantry"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/units/swedish_tercio.txt")
				&& reference.key == "unit_type"
				&& reference.value == "western"
		}));
	}

	#[test]
	fn common_data_roots_record_resource_references() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("religions")).expect("create religions");
		fs::create_dir_all(mod_root.join("common").join("subject_types"))
			.expect("create subject types");
		fs::create_dir_all(mod_root.join("common").join("rebel_types"))
			.expect("create rebel types");
		fs::create_dir_all(mod_root.join("common").join("disasters")).expect("create disasters");
		fs::create_dir_all(mod_root.join("common").join("government_mechanics"))
			.expect("create government mechanics");
		fs::write(
			mod_root
				.join("common")
				.join("religions")
				.join("00_religion.txt"),
			r#"
christian = {
	center_of_religion = 118
	catholic = {
		allowed_conversion = {
			protestant
		}
		heretic = { hussite }
		papacy = {
			papal_tag = PAP
		}
	}
"#,
		)
		.expect("write religions");
		fs::write(
			mod_root
				.join("common")
				.join("subject_types")
				.join("00_subject_types.txt"),
			r#"
default = {
	sprite = GFX_icon_vassal
	diplomacy_overlord_sprite = GFX_diplomacy_leadvassal
}
march = {
	copy_from = default
	subject_opinion_modifier = march_subject
}
"#,
		)
		.expect("write subject types");
		fs::write(
			mod_root
				.join("common")
				.join("rebel_types")
				.join("independence_rebels.txt"),
			r#"
independence_rebels = {
	gfx_type = culture_province
	demands_description = "independence_rebels_demands"
}
"#,
		)
		.expect("write rebel types");
		fs::write(
			mod_root
				.join("common")
				.join("disasters")
				.join("civil_war.txt"),
			r#"
civil_war = {
	on_start = civil_war.1
	on_end = civil_war.100
	on_monthly = {
		events = {
			civil_war.2
		}
		random_events = {
			100 = civil_war.3
		}
	}
	can_start = {
		NOT = { has_disaster = court_and_country }
	}
}
"#,
		)
		.expect("write disasters");
		fs::write(
			mod_root
				.join("common")
				.join("government_mechanics")
				.join("18_parliament_vs_monarchy.txt"),
			r#"
parliament_vs_monarchy_mechanic = {
	available = {
		has_dlc = "Domination"
	}
	powers = {
		governmental_power = {
			gui = parliament_vs_monarchy_gov_mech
			scaled_modifier = {
				trigger = {
					has_government_power = {
						mechanic_type = parliament_vs_monarchy_mechanic
						power_type = governmental_power
					}
				}
			}
			on_max_reached = {
				custom_tooltip = parliament_vs_monarchy_mechanic_at
				hidden_effect = {
					country_event = {
						id = flavor_gbr.113
					}
				}
			}
		}
	}
}
"#,
		)
		.expect("write government mechanics");

		let files = vec![
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("religions")
					.join("00_religion.txt"),
			)
			.expect("parsed religions"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("subject_types")
					.join("00_subject_types.txt"),
			)
			.expect("parsed subject types"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("rebel_types")
					.join("independence_rebels.txt"),
			)
			.expect("parsed rebel types"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("disasters")
					.join("civil_war.txt"),
			)
			.expect("parsed disasters"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("government_mechanics")
					.join("18_parliament_vs_monarchy.txt"),
			)
			.expect("parsed government mechanics"),
		];

		let index = build_semantic_index(&files);
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/religions/00_religion.txt")
				&& reference.key == "center_of_religion"
				&& reference.value == "118"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/religions/00_religion.txt")
				&& reference.key == "allowed_conversion"
				&& reference.value == "protestant"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/religions/00_religion.txt")
				&& reference.key == "papal_tag"
				&& reference.value == "PAP"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/subject_types/00_subject_types.txt")
				&& reference.key == "copy_from"
				&& reference.value == "default"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/subject_types/00_subject_types.txt")
				&& reference.key == "sprite"
				&& reference.value == "GFX_icon_vassal"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/rebel_types/independence_rebels.txt")
				&& reference.key == "demands_description"
				&& reference.value == "independence_rebels_demands"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/disasters/civil_war.txt")
				&& reference.key == "on_start"
				&& reference.value == "civil_war.1"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/disasters/civil_war.txt")
				&& reference.key == "event"
				&& reference.value == "civil_war.3"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/government_mechanics/18_parliament_vs_monarchy.txt")
				&& reference.key == "gui"
				&& reference.value == "parliament_vs_monarchy_gov_mech"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/government_mechanics/18_parliament_vs_monarchy.txt")
				&& reference.key == "country_event"
				&& reference.value == "flavor_gbr.113"
		}));
	}

	#[test]
	fn governance_roots_record_resource_references() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		for root in [
			"common/estate_agendas",
			"common/estate_privileges",
			"common/estates",
			"common/parliament_bribes",
			"common/parliament_issues",
			"common/state_edicts",
		] {
			fs::create_dir_all(mod_root.join(root)).expect("create governance root");
		}
		fs::write(
			mod_root
				.join("common")
				.join("estate_agendas")
				.join("00_generic_agendas.txt"),
			r#"
church_diplomatic_consultation_agenda = {
	estate = clergy
	can_select = {
		custom_tooltip = agenda_can_select_tt
	}
	task_requirements = {
		estate = clergy
	}
	pre_effect = {
		custom_tooltip = agenda_pre_tt
	}
	task_completed_effect = {
		custom_tooltip = agenda_done_tt
	}
}
"#,
		)
		.expect("write estate agendas");
		fs::write(
			mod_root
				.join("common")
				.join("estate_privileges")
				.join("01_church_privileges.txt"),
			r#"
religious_diplomats = {
	icon = privilege_religious_diplomats
	estate = clergy
	mechanics = {
		devotion
		papal_influence
	}
	benefits = {
		custom_tooltip = privilege_benefits_tt
	}
}
"#,
		)
		.expect("write estate privileges");
		fs::write(
			mod_root
				.join("common")
				.join("estates")
				.join("01_church.txt"),
			r#"
clergy = {
	custom_name = estate_clergy_custom_name
	custom_desc = estate_clergy_custom_desc
	privileges = {
		religious_diplomats
	}
	agendas = {
		church_diplomatic_consultation_agenda
	}
	starting_reform = monarchy_reform
	independence_government = theocracy
	trigger = {
		has_dlc = "Domination"
	}
}
"#,
		)
		.expect("write estates");
		fs::write(
			mod_root
				.join("common")
				.join("parliament_bribes")
				.join("administrative_support.txt"),
			r#"
administrative_support = {
	name = parliament_bribe_admin_support
	estate = clergy
	mechanic_type = parliament_vs_monarchy_mechanic
	power_type = governmental_power
	type = monarch_power
	effect = {
		add_adm_power = 50
	}
}
"#,
		)
		.expect("write parliament bribes");
		fs::write(
			mod_root
				.join("common")
				.join("parliament_issues")
				.join("00_adm_parliament_issues.txt"),
			r#"
expand_bureaucracy_issue = {
	parliament_action = strengthen_government
	issue = expand_bureaucracy_issue
	custom_tooltip = parliament_issue_tt
	effect = {
		custom_tooltip = parliament_issue_effect_tt
	}
	influence_scaled_modifier = {
		estate = clergy
	}
}
"#,
		)
		.expect("write parliament issues");
		fs::write(
			mod_root
				.join("common")
				.join("state_edicts")
				.join("edict_of_governance.txt"),
			r#"
edict_of_governance = {
	tooltip = edict_of_governance_tt
	allow = {
		custom_trigger_tooltip = state_edict_allow_tt
		has_state_edict = encourage_development_edict
	}
	modifier = {
		state_maintenance_modifier = -0.1
	}
}
"#,
		)
		.expect("write state edicts");

		let files = vec![
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("estate_agendas")
					.join("00_generic_agendas.txt"),
			)
			.expect("parsed estate agendas"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("estate_privileges")
					.join("01_church_privileges.txt"),
			)
			.expect("parsed estate privileges"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("estates")
					.join("01_church.txt"),
			)
			.expect("parsed estates"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("parliament_bribes")
					.join("administrative_support.txt"),
			)
			.expect("parsed parliament bribes"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("parliament_issues")
					.join("00_adm_parliament_issues.txt"),
			)
			.expect("parsed parliament issues"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("state_edicts")
					.join("edict_of_governance.txt"),
			)
			.expect("parsed state edicts"),
		];

		let index = build_semantic_index(&files);
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/estate_agendas/00_generic_agendas.txt")
				&& reference.key == "estate"
				&& reference.value == "clergy"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/estate_agendas/00_generic_agendas.txt")
				&& reference.key == "custom_tooltip"
				&& reference.value == "agenda_done_tt"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/estate_privileges/01_church_privileges.txt")
				&& reference.key == "icon"
				&& reference.value == "privilege_religious_diplomats"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/estate_privileges/01_church_privileges.txt")
				&& reference.key == "mechanics"
				&& reference.value == "papal_influence"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/estates/01_church.txt")
				&& reference.key == "custom_name"
				&& reference.value == "estate_clergy_custom_name"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/estates/01_church.txt")
				&& reference.key == "privileges"
				&& reference.value == "religious_diplomats"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/parliament_bribes/administrative_support.txt")
				&& reference.key == "mechanic_type"
				&& reference.value == "parliament_vs_monarchy_mechanic"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/parliament_issues/00_adm_parliament_issues.txt")
				&& reference.key == "parliament_action"
				&& reference.value == "strengthen_government"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/parliament_issues/00_adm_parliament_issues.txt")
				&& reference.key == "estate"
				&& reference.value == "clergy"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/state_edicts/edict_of_governance.txt")
				&& reference.key == "tooltip"
				&& reference.value == "edict_of_governance_tt"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/state_edicts/edict_of_governance.txt")
				&& reference.key == "has_state_edict"
				&& reference.value == "encourage_development_edict"
		}));
	}

	#[test]
	fn peace_treaties_and_bookmarks_record_resource_references() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("peace_treaties"))
			.expect("create peace treaties");
		fs::create_dir_all(mod_root.join("common").join("bookmarks")).expect("create bookmarks");
		fs::write(
			mod_root
				.join("common")
				.join("peace_treaties")
				.join("00_peace_treaties.txt"),
			r#"
spread_dynasty = {
	power_projection = vassalized_rival
	is_visible = { religion_group = christian }
	is_allowed = { religion = catholic }
	warscore_cost = { no_provinces = 20.0 }
	effect = { add_prestige = 5 }
	ai_weight = {
		export_to_variable = {
			variable_name = ai_value
			value = 50
		}
	}
}
"#,
		)
		.expect("write peace treaties");
		fs::write(
			mod_root
				.join("common")
				.join("bookmarks")
				.join("a_new_world.txt"),
			r#"
bookmark = {
	name = "NEWWORLD_NAME"
	desc = "NEWWORLD_DESC"
	date = 1492.1.1
	center = 2133
	country = CAS
	country = ENG
}
"#,
		)
		.expect("write bookmarks");

		let files = vec![
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("peace_treaties")
					.join("00_peace_treaties.txt"),
			)
			.expect("parsed peace treaties"),
			parse_script_file(
				"1000",
				&mod_root,
				&mod_root
					.join("common")
					.join("bookmarks")
					.join("a_new_world.txt"),
			)
			.expect("parsed bookmarks"),
		];

		let index = build_semantic_index(&files);
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/peace_treaties/00_peace_treaties.txt")
				&& reference.key == "localisation_desc"
				&& reference.value == "spread_dynasty_desc"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/peace_treaties/00_peace_treaties.txt")
				&& reference.key == "localisation_cb_allowed"
				&& reference.value == "CB_ALLOWED_spread_dynasty"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/peace_treaties/00_peace_treaties.txt")
				&& reference.key == "localisation_peace"
				&& reference.value == "PEACE_spread_dynasty"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/peace_treaties/00_peace_treaties.txt")
				&& reference.key == "power_projection"
				&& reference.value == "vassalized_rival"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/bookmarks/a_new_world.txt")
				&& reference.key == "name"
				&& reference.value == "NEWWORLD_NAME"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/bookmarks/a_new_world.txt")
				&& reference.key == "desc"
				&& reference.value == "NEWWORLD_DESC"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/bookmarks/a_new_world.txt")
				&& reference.key == "center"
				&& reference.value == "2133"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/bookmarks/a_new_world.txt")
				&& reference.key == "country"
				&& reference.value == "CAS"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/bookmarks/a_new_world.txt")
				&& reference.key == "country"
				&& reference.value == "ENG"
		}));
	}

	#[test]
	fn low_risk_definition_roots_record_resource_references() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("church_aspects"))
			.expect("create church aspects");
		fs::create_dir_all(mod_root.join("common").join("factions")).expect("create factions");
		fs::create_dir_all(mod_root.join("common").join("hegemons")).expect("create hegemons");
		fs::create_dir_all(mod_root.join("common").join("personal_deities"))
			.expect("create personal deities");
		fs::create_dir_all(mod_root.join("common").join("fetishist_cults"))
			.expect("create fetishist cults");

		fs::write(
			mod_root
				.join("common")
				.join("church_aspects")
				.join("00_church_aspects.txt"),
			r#"
organised_through_bishops_aspect = {
	cost = 100
	potential = { religion = protestant }
	trigger = { has_church_power = yes }
	effect = { add_stability = 1 }
	modifier = { development_cost = -0.05 }
	ai_will_do = { factor = 1 }
}
"#,
		)
		.expect("write church aspects");
		fs::write(
			mod_root
				.join("common")
				.join("factions")
				.join("00_factions.txt"),
			r#"
rr_jacobins = {
	allow = { has_dlc = "Rights of Man" }
	monarch_power = ADM
	always = yes
	modifier = { global_unrest = -2 }
}
"#,
		)
		.expect("write factions");
		fs::write(
			mod_root
				.join("common")
				.join("hegemons")
				.join("0_economic_hegemon.txt"),
			r#"
economic_hegemon = {
	allow = { is_great_power = yes }
	base = { war_exhaustion = -0.1 }
	scale = { mercenary_discipline = 0.10 }
	max = { governing_capacity_modifier = 0.20 }
}
"#,
		)
		.expect("write hegemons");
		fs::write(
			mod_root
				.join("common")
				.join("personal_deities")
				.join("00_hindu_deities.txt"),
			r#"
shiva = {
	sprite = 1
	potential = { religion = hinduism }
	trigger = { religion = hinduism }
	effect = { add_prestige = 1 }
	removed_effect = { add_prestige = -1 }
	ai_will_do = { factor = 1 }
}
"#,
		)
		.expect("write personal deities");
		fs::write(
			mod_root
				.join("common")
				.join("fetishist_cults")
				.join("00_fetishist_cults.txt"),
			r#"
yemoja_cult = {
	allow = { religion = shamanism }
	sprite = 1
	ai_will_do = { factor = 1 }
}
"#,
		)
		.expect("write fetishist cults");

		let files = vec![
			parse_script_file(
				"1014",
				&mod_root,
				&mod_root
					.join("common")
					.join("church_aspects")
					.join("00_church_aspects.txt"),
			)
			.expect("parsed church aspects"),
			parse_script_file(
				"1014",
				&mod_root,
				&mod_root
					.join("common")
					.join("factions")
					.join("00_factions.txt"),
			)
			.expect("parsed factions"),
			parse_script_file(
				"1014",
				&mod_root,
				&mod_root
					.join("common")
					.join("hegemons")
					.join("0_economic_hegemon.txt"),
			)
			.expect("parsed hegemons"),
			parse_script_file(
				"1014",
				&mod_root,
				&mod_root
					.join("common")
					.join("personal_deities")
					.join("00_hindu_deities.txt"),
			)
			.expect("parsed personal deities"),
			parse_script_file(
				"1014",
				&mod_root,
				&mod_root
					.join("common")
					.join("fetishist_cults")
					.join("00_fetishist_cults.txt"),
			)
			.expect("parsed fetishist cults"),
		];

		let index = build_semantic_index(&files);
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/church_aspects/00_church_aspects.txt")
				&& usage.key == "religion"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
		}));
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/church_aspects/00_church_aspects.txt")
				&& usage.key == "add_stability"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Effect
		}));
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/factions/00_factions.txt")
				&& usage.key == "has_dlc"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
		}));
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/hegemons/0_economic_hegemon.txt")
				&& usage.key == "war_exhaustion"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Block
		}));
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/personal_deities/00_hindu_deities.txt")
				&& usage.key == "add_prestige"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Effect
		}));
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/fetishist_cults/00_fetishist_cults.txt")
				&& usage.key == "religion"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/church_aspects/00_church_aspects.txt")
				&& reference.key == "localisation"
				&& reference.value == "organised_through_bishops_aspect"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/church_aspects/00_church_aspects.txt")
				&& reference.key == "localisation_desc"
				&& reference.value == "desc_organised_through_bishops_aspect"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/church_aspects/00_church_aspects.txt")
				&& reference.key == "localisation_modifier"
				&& reference.value == "organised_through_bishops_aspect_modifier"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/factions/00_factions.txt")
				&& reference.key == "localisation"
				&& reference.value == "rr_jacobins"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/factions/00_factions.txt")
				&& reference.key == "localisation_influence"
				&& reference.value == "rr_jacobins_influence"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/factions/00_factions.txt")
				&& reference.key == "monarch_power"
				&& reference.value == "ADM"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/hegemons/0_economic_hegemon.txt")
				&& reference.key == "localisation"
				&& reference.value == "economic_hegemon"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/personal_deities/00_hindu_deities.txt")
				&& reference.key == "localisation"
				&& reference.value == "shiva"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/personal_deities/00_hindu_deities.txt")
				&& reference.key == "localisation_desc"
				&& reference.value == "shiva_desc"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/fetishist_cults/00_fetishist_cults.txt")
				&& reference.key == "localisation"
				&& reference.value == "yemoja_cult"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/fetishist_cults/00_fetishist_cults.txt")
				&& reference.key == "localisation_desc"
				&& reference.value == "yemoja_cult_desc"
		}));
	}

	#[test]
	fn policies_and_mercenary_companies_record_resource_references() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("policies")).expect("create policies");
		fs::create_dir_all(mod_root.join("common").join("mercenary_companies"))
			.expect("create mercenary companies");
		fs::write(
			mod_root.join("common").join("policies").join("00_adm.txt"),
			r#"
the_combination_act = {
	monarch_power = ADM
	potential = { has_idea_group = aristocracy_ideas }
	allow = { full_idea_group = aristocracy_ideas }
	effect = { add_prestige = 1 }
	removed_effect = { add_prestige = -1 }
	ai_will_do = { factor = 1 }
}
"#,
		)
		.expect("write policies");
		fs::write(
			mod_root
				.join("common")
				.join("mercenary_companies")
				.join("00_mercenaries.txt"),
			r#"
merc_black_army = {
	mercenary_desc_key = FREE_OF_ARMY_PROFESSIONALISM_COST
	home_province = 153
	sprites = { dlc102_hun_sprite_pack easterngfx_sprite_pack }
	trigger = {
		tag = HUN
	}
	modifier = {
		discipline = 0.05
	}
}
"#,
		)
		.expect("write mercenary companies");

		let files = vec![
			parse_script_file(
				"1015",
				&mod_root,
				&mod_root.join("common").join("policies").join("00_adm.txt"),
			)
			.expect("parsed policies"),
			parse_script_file(
				"1015",
				&mod_root,
				&mod_root
					.join("common")
					.join("mercenary_companies")
					.join("00_mercenaries.txt"),
			)
			.expect("parsed mercenary companies"),
		];

		let index = build_semantic_index(&files);
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/policies/00_adm.txt")
				&& usage.key == "has_idea_group"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
		}));
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/policies/00_adm.txt")
				&& usage.key == "add_prestige"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Effect
		}));
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
				&& usage.key == "tag"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Trigger
		}));
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
				&& usage.key == "discipline"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Block
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/policies/00_adm.txt")
				&& reference.key == "localisation"
				&& reference.value == "the_combination_act"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/policies/00_adm.txt")
				&& reference.key == "monarch_power"
				&& reference.value == "ADM"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
				&& reference.key == "localisation"
				&& reference.value == "merc_black_army"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
				&& reference.key == "mercenary_desc_key"
				&& reference.value == "FREE_OF_ARMY_PROFESSIONALISM_COST"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
				&& reference.key == "home_province"
				&& reference.value == "153"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/mercenary_companies/00_mercenaries.txt")
				&& reference.key == "sprites"
				&& reference.value == "dlc102_hun_sprite_pack"
		}));
	}

	#[test]
	fn technology_files_record_real_syntax_resource_references() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("technologies"))
			.expect("create technologies");
		fs::write(
			mod_root.join("common").join("technologies").join("adm.txt"),
			r#"
monarch_power = ADM
ahead_of_time = {
	adm_tech_cost_modifier = 0.2
}
technology = {
	year = 1444
	expects_institution = {
		feudalism = 0.5
	}
	temple = yes
}
technology = {
	year = 1466
	expects_institution = {
		renaissance = 0.15
	}
	courthouse = yes
	may_force_march = yes
}
"#,
		)
		.expect("write technologies");

		let parsed = parse_script_file(
			"1016",
			&mod_root,
			&mod_root.join("common").join("technologies").join("adm.txt"),
		)
		.expect("parsed technologies");
		let index = build_semantic_index(&[parsed]);
		assert!(index.key_usages.iter().any(|usage| {
			usage.path == Path::new("common/technologies/adm.txt")
				&& usage.key == "adm_tech_cost_modifier"
				&& scope_kind(&index, usage.scope_id) == ScopeKind::Block
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "monarch_power"
				&& reference.value == "ADM"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "technology_definition"
				&& reference.value == "adm_tech_0"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "technology_definition"
				&& reference.value == "adm_tech_1"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "expects_institution"
				&& reference.value == "feudalism"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "expects_institution"
				&& reference.value == "renaissance"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "enable"
				&& reference.value == "temple"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "enable"
				&& reference.value == "courthouse"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "enable"
				&& reference.value == "may_force_march"
		}));
	}

	#[test]
	fn technology_files_reset_state_per_overlay_contributor() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_a_root = tmp.path().join("mod_a");
		let mod_b_root = tmp.path().join("mod_b");
		fs::create_dir_all(mod_a_root.join("common").join("technologies"))
			.expect("create mod a technologies");
		fs::create_dir_all(mod_b_root.join("common").join("technologies"))
			.expect("create mod b technologies");
		fs::write(
			mod_a_root
				.join("common")
				.join("technologies")
				.join("adm.txt"),
			r#"
monarch_power = ADM
technology = {
	year = 1444
	temple = yes
}
"#,
		)
		.expect("write mod a technologies");
		fs::write(
			mod_b_root
				.join("common")
				.join("technologies")
				.join("adm.txt"),
			r#"
monarch_power = DIP
technology = {
	year = 1444
	marketplace = yes
}
"#,
		)
		.expect("write mod b technologies");

		let files = vec![
			parse_script_file(
				"mod-a",
				&mod_a_root,
				&mod_a_root
					.join("common")
					.join("technologies")
					.join("adm.txt"),
			)
			.expect("parsed mod a technologies"),
			parse_script_file(
				"mod-b",
				&mod_b_root,
				&mod_b_root
					.join("common")
					.join("technologies")
					.join("adm.txt"),
			)
			.expect("parsed mod b technologies"),
		];

		let index = build_semantic_index(&files);
		assert!(index.resource_references.iter().any(|reference| {
			reference.mod_id == "mod-a"
				&& reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "technology_definition"
				&& reference.value == "adm_tech_0"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.mod_id == "mod-b"
				&& reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "technology_definition"
				&& reference.value == "dip_tech_0"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.mod_id == "mod-b"
				&& reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "monarch_power"
				&& reference.value == "DIP"
		}));
		assert!(!index.resource_references.iter().any(|reference| {
			reference.mod_id == "mod-b"
				&& reference.path == Path::new("common/technologies/adm.txt")
				&& reference.key == "technology_definition"
				&& reference.value == "adm_tech_1"
		}));
	}

	#[test]
	fn technology_groups_record_resource_references() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common")).expect("create common");
		fs::write(
			mod_root.join("common").join("technology.txt"),
			r#"
groups = {
	western = {
		start_level = 3
		start_cost_modifier = 0.0
		nation_designer_unit_type = western
		nation_designer_trigger = {
			has_dlc = "Conquest of Paradise"
		}
		nation_designer_cost = {
			trigger = { is_free_or_tributary_trigger = yes }
			value = 25
		}
	}
	eastern = {
		start_level = 2
		start_cost_modifier = 0.1
		nation_designer_unit_type = eastern
	}
}
"#,
		)
		.expect("write technology groups");

		let parsed = parse_script_file(
			"1016",
			&mod_root,
			&mod_root.join("common").join("technology.txt"),
		)
		.expect("parsed technology groups");
		let index = build_semantic_index(&[parsed]);
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technology.txt")
				&& reference.key == "technology_group"
				&& reference.value == "western"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technology.txt")
				&& reference.key == "nation_designer_unit_type"
				&& reference.value == "western"
		}));
		assert!(index.resource_references.iter().any(|reference| {
			reference.path == Path::new("common/technology.txt")
				&& reference.key == "nation_designer_cost_value"
				&& reference.value == "25"
		}));
	}

	#[test]
	fn index_builds_event_and_scope_types() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("events")).expect("create dir");
		fs::write(
			mod_root.join("events").join("x.txt"),
			r#"
namespace = test
country_event = {
	id = test.1
	option = {
		every_owned_province = {
			ROOT = { }
			province_event = { id = test.2 }
		}
	}
}
province_event = {
	id = test.2
	trigger = {
		FROM = {
			has_country_flag = seen_city
		}
	}
	immediate = {
		owner = {
			country_event = { id = test.1 }
		}
	}
}
"#,
		)
		.expect("write file");

		let parsed = parse_script_file("1000", &mod_root, &mod_root.join("events").join("x.txt"))
			.expect("parsed script");

		let index = build_semantic_index(&[parsed]);
		assert!(
			index
				.definitions
				.iter()
				.any(|item| item.kind == SymbolKind::Event && item.name == "test.1")
		);
		assert!(
			index
				.definitions
				.iter()
				.any(|item| item.kind == SymbolKind::Event && item.name == "test.2")
		);
		assert!(
			index
				.references
				.iter()
				.any(|item| item.kind == SymbolKind::Event && item.name == "test.2")
		);
		assert!(
			index
				.scopes
				.iter()
				.any(|scope| scope.this_type == ScopeType::Province)
		);
		assert!(
			!index.key_usages.iter().any(|usage| {
				(usage.key == "country_event" || usage.key == "province_event")
					&& scope_kind(&index, usage.scope_id) == ScopeKind::File
			}),
			"top-level event definitions should not be recorded as plain key usage"
		);

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001" && finding.path == Some("events/x.txt".into())
			}),
			"typed event roots should not stay in Unknown scope"
		);
	}

	#[test]
	fn achievements_builtin_blocks_do_not_become_scripted_effect_calls() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common")).expect("create dir");
		fs::write(
			mod_root.join("common").join("achievements.txt"),
			r#"
achievement_example = {
	possible = {
		capital_scope = {
			all_core_province = {
				region = north_america
			}
		}
	}
	visible = {
		capital_scope = {
			region = japan_region
		}
	}
	happened = {
		all_core_province = {
			is_core = ROOT
		}
	}
	provinces_to_highlight = {
		all_core_province = {
			region = china_region
		}
	}
}
"#,
		)
		.expect("write file");

		let parsed = parse_script_file(
			"1001",
			&mod_root,
			&mod_root.join("common").join("achievements.txt"),
		)
		.expect("parsed script");

		let index = build_semantic_index(&[parsed]);
		for name in [
			"possible",
			"visible",
			"happened",
			"provinces_to_highlight",
			"capital_scope",
			"all_core_province",
		] {
			assert!(
				!index.references.iter().any(|reference| {
					reference.kind == SymbolKind::ScriptedEffect && reference.name == name
				}),
				"{name} should not be recorded as a scripted effect reference"
			);
		}
		assert!(index.scopes.iter().any(|scope| {
			scope.kind == ScopeKind::AliasBlock && scope.this_type == ScopeType::Province
		}));
		assert!(
			index.scopes.iter().any(
				|scope| scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Province
			)
		);

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		for name in [
			"possible",
			"visible",
			"happened",
			"provinces_to_highlight",
			"capital_scope",
			"all_core_province",
		] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
				"{name} should not produce S002"
			);
		}
		assert!(
			!diagnostics
				.advisory
				.iter()
				.any(|finding| finding.rule_id == "A001"
					&& finding.path == Some("common/achievements.txt".into())),
			"achievements root scope should no longer stay Unknown"
		);
	}

	#[test]
	fn common_data_file_roots_do_not_become_scripted_effect_calls() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("ideas")).expect("create ideas");
		fs::create_dir_all(mod_root.join("common").join("ages")).expect("create ages");
		fs::create_dir_all(mod_root.join("common").join("buildings")).expect("create buildings");
		fs::create_dir_all(mod_root.join("common").join("great_projects"))
			.expect("create monuments");
		fs::create_dir_all(mod_root.join("common").join("institutions"))
			.expect("create institutions");
		fs::create_dir_all(mod_root.join("common").join("province_triggered_modifiers"))
			.expect("create province modifiers");
		fs::create_dir_all(mod_root.join("common").join("custom_gui")).expect("create custom gui");
		fs::create_dir_all(mod_root.join("common").join("government_names"))
			.expect("create government names");
		fs::create_dir_all(mod_root.join("customizable_localization")).expect("create custom loc");
		fs::create_dir_all(mod_root.join("interface")).expect("create interface");
		fs::write(
			mod_root.join("common").join("ideas").join("ideas.txt"),
			"my_ideas = { start = { add_prestige = 1 } }\n",
		)
		.expect("write ideas");
		fs::write(
			mod_root.join("common").join("ages").join("ages.txt"),
			"age_of_discovery = { objectives = { obj_one = { calc_true_if = { all_owned_province = { is_core = ROOT controlled_by = owner exclude_from_progress = { is_core = ROOT } } amount = 1 } } } }\n",
		)
		.expect("write ages");
		fs::write(
			mod_root.join("common").join("buildings").join("buildings.txt"),
			"marketplace = { on_built = { owner = { add_prestige = 1 } FROM = { add_stability_cost_modifier = -0.1 } } }\n",
		)
		.expect("write buildings");
		fs::write(
			mod_root
				.join("common")
				.join("great_projects")
				.join("projects.txt"),
			"project_alpha = { build_cost = 1000 }\n",
		)
		.expect("write monuments");
		fs::write(
			mod_root
				.join("common")
				.join("institutions")
				.join("institutions.txt"),
			r#"
printing_press = {
	potential = {
		owner = {
			government = monarchy
		}
	}
	on_start = {
		add_base_tax = 1
	}
}
"#,
		)
		.expect("write institutions");
		fs::write(
			mod_root
				.join("common")
				.join("province_triggered_modifiers")
				.join("modifiers.txt"),
			r#"
prosperous = {
	trigger = {
		owner = {
			government = monarchy
		}
	}
}
"#,
		)
		.expect("write province modifiers");
		fs::write(
			mod_root
				.join("common")
				.join("custom_gui")
				.join("advisor.txt"),
			"advisor_actions = { title = advisor_title }\n",
		)
		.expect("write custom gui");
		fs::write(
			mod_root
				.join("common")
				.join("government_names")
				.join("names.txt"),
			"czech_localisation = { trigger = { government = monarchy } }\n",
		)
		.expect("write government names");
		fs::write(
			mod_root
				.join("customizable_localization")
				.join("defined.txt"),
			"defined_text = { name = GetFoo text = { localisation_key = foo trigger = { always = yes } } }\n",
		)
		.expect("write custom loc");
		fs::write(
			mod_root.join("interface").join("main.gui"),
			"windowType = { name = main_window }\n",
		)
		.expect("write ui");

		let parsed = [
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root.join("common").join("ideas").join("ideas.txt"),
			)
			.expect("parsed ideas"),
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root.join("common").join("ages").join("ages.txt"),
			)
			.expect("parsed ages"),
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root
					.join("common")
					.join("buildings")
					.join("buildings.txt"),
			)
			.expect("parsed buildings"),
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root
					.join("common")
					.join("great_projects")
					.join("projects.txt"),
			)
			.expect("parsed monuments"),
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root
					.join("common")
					.join("institutions")
					.join("institutions.txt"),
			)
			.expect("parsed institutions"),
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root
					.join("common")
					.join("province_triggered_modifiers")
					.join("modifiers.txt"),
			)
			.expect("parsed province modifiers"),
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root
					.join("common")
					.join("custom_gui")
					.join("advisor.txt"),
			)
			.expect("parsed custom gui"),
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root
					.join("common")
					.join("government_names")
					.join("names.txt"),
			)
			.expect("parsed government names"),
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root
					.join("customizable_localization")
					.join("defined.txt"),
			)
			.expect("parsed custom loc"),
			parse_script_file(
				"1002",
				&mod_root,
				&mod_root.join("interface").join("main.gui"),
			)
			.expect("parsed ui"),
		];

		let index = build_semantic_index(&parsed);
		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		for path in [
			"common/ideas/ideas.txt",
			"common/ages/ages.txt",
			"common/buildings/buildings.txt",
			"common/great_projects/projects.txt",
			"common/institutions/institutions.txt",
			"common/province_triggered_modifiers/modifiers.txt",
			"common/custom_gui/advisor.txt",
			"common/government_names/names.txt",
			"customizable_localization/defined.txt",
			"interface/main.gui",
		] {
			assert!(
				!diagnostics.strict.iter().any(|finding| {
					finding.rule_id == "S002" && finding.path == Some(path.into())
				}),
				"{path} should not report top-level scripted effect fallback"
			);
		}
		for path in [
			"common/ages/ages.txt",
			"common/buildings/buildings.txt",
			"common/institutions/institutions.txt",
			"common/province_triggered_modifiers/modifiers.txt",
		] {
			assert!(
				!diagnostics.advisory.iter().any(|finding| {
					finding.rule_id == "A001" && finding.path == Some(path.into())
				}),
				"{path} should have a typed root scope"
			);
		}
	}

	#[test]
	fn mislocated_dsl_paths_reuse_existing_semantics() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(
			mod_root
				.join("events")
				.join("common")
				.join("new_diplomatic_actions"),
		)
		.expect("create misplaced diplomatic actions");
		fs::create_dir_all(mod_root.join("events").join("decisions"))
			.expect("create misplaced decisions");
		fs::write(
			mod_root
				.join("events")
				.join("common")
				.join("new_diplomatic_actions")
				.join("actions.txt"),
			r#"
static_actions = {
	royal_marriage = {
		alert_index = 1
	}
}

sell_indulgence = {
	is_visible = { always = yes }
	on_accept = {
		missing_effect = { FLAG = TEST }
	}
}
"#,
		)
		.expect("write misplaced diplomatic actions");
		fs::write(
			mod_root
				.join("events")
				.join("decisions")
				.join("decisions.txt"),
			r#"
country_decisions = {
	test_decision = {
		potential = { always = yes }
		effect = {
			missing_decision_effect = { FLAG = TEST }
		}
	}
}
"#,
		)
		.expect("write misplaced decisions");

		let parsed = [
			parse_script_file(
				"1012",
				&mod_root,
				&mod_root
					.join("events")
					.join("common")
					.join("new_diplomatic_actions")
					.join("actions.txt"),
			)
			.expect("parsed misplaced diplomatic actions"),
			parse_script_file(
				"1012",
				&mod_root,
				&mod_root
					.join("events")
					.join("decisions")
					.join("decisions.txt"),
			)
			.expect("parsed misplaced decisions"),
		];
		let index = build_semantic_index(&parsed);
		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		for path in [
			"events/common/new_diplomatic_actions/actions.txt",
			"events/decisions/decisions.txt",
		] {
			assert!(
				!diagnostics.advisory.iter().any(|finding| {
					finding.rule_id == "A001" && finding.path == Some(path.into())
				}),
				"{path} should reuse typed DSL semantics"
			);
		}
		for name in [
			"static_actions",
			"royal_marriage",
			"is_visible",
			"country_decisions",
			"test_decision",
		] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
				"{name} should not be treated as a scripted effect"
			);
		}
		assert!(diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S002" && finding.message.contains("missing_effect")
		}));
		assert!(diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S002" && finding.message.contains("missing_decision_effect")
		}));
	}

	#[test]
	fn missions_and_map_groups_do_not_become_scripted_effect_calls() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("missions")).expect("create missions");
		fs::create_dir_all(mod_root.join("map")).expect("create map");
		fs::write(
			mod_root.join("map").join("area.txt"),
			"finland_area = { 1 }\n",
		)
		.expect("write area");
		fs::write(
			mod_root.join("map").join("region.txt"),
			"baltic_region = { areas = { finland_area } }\n",
		)
		.expect("write region");
		fs::write(
			mod_root.join("missions").join("missions.txt"),
			r#"
mos_rus_handle_succession = {
	potential_on_load = {
		has_dlc = "Domination"
	}
	mos_rus_window_on_the_west = {
		required_missions = { mos_prev }
		trigger = {
			baltic_region = {
				type = all
				owned_by = ROOT
			}
		}
		effect = {
			finland_area = {
				add_prestige = 1
			}
			missing_effect = { FLAG = TEST }
		}
		ai_weight = {
			factor = 100
		}
	}
}
"#,
		)
		.expect("write missions");

		let parsed = [
			parse_script_file("1004", &mod_root, &mod_root.join("map").join("area.txt"))
				.expect("parsed area"),
			parse_script_file("1004", &mod_root, &mod_root.join("map").join("region.txt"))
				.expect("parsed region"),
			parse_script_file(
				"1004",
				&mod_root,
				&mod_root.join("missions").join("missions.txt"),
			)
			.expect("parsed missions"),
		];
		let index = build_semantic_index(&parsed);
		for name in [
			"potential_on_load",
			"mos_rus_window_on_the_west",
			"required_missions",
			"baltic_region",
			"finland_area",
			"ai_weight",
		] {
			assert!(
				!index.references.iter().any(|reference| {
					reference.kind == SymbolKind::ScriptedEffect && reference.name == name
				}),
				"{name} should not be recorded as a scripted effect reference"
			);
		}
		assert!(index.scopes.iter().any(|scope| {
			scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Province
		}));

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		for name in [
			"potential_on_load",
			"mos_rus_window_on_the_west",
			"required_missions",
			"baltic_region",
			"finland_area",
			"ai_weight",
		] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
				"{name} should not produce S002"
			);
		}
		assert!(diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S002" && finding.message.contains("missing_effect")
		}));
	}

	#[test]
	fn mission_event_and_common_wrappers_do_not_become_scripted_effect_calls() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("missions")).expect("create missions");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::create_dir_all(mod_root.join("common").join("government_reforms"))
			.expect("create government reforms");
		fs::write(
			mod_root.join("missions").join("missions.txt"),
			r#"
mos_rus_window_on_the_west = {
	ai_weight = {
		mission_weight_helper = { FLAG = TEST }
	}
}
"#,
		)
		.expect("write missions");
		fs::write(
			mod_root.join("events").join("event.txt"),
			r#"
namespace = test
country_event = {
	id = test.1
	mean_time_to_happen = {
		event_weight_helper = { FLAG = TEST }
	}
}
"#,
		)
		.expect("write event");
		fs::write(
			mod_root
				.join("common")
				.join("government_reforms")
				.join("reforms.txt"),
			r#"
test_reform = {
	ai_will_do = {
		common_weight_helper = { FLAG = TEST }
	}
}
"#,
		)
		.expect("write government reforms");

		let parsed = [
			parse_script_file(
				"1012",
				&mod_root,
				&mod_root.join("missions").join("missions.txt"),
			)
			.expect("parsed missions"),
			parse_script_file(
				"1012",
				&mod_root,
				&mod_root.join("events").join("event.txt"),
			)
			.expect("parsed event"),
			parse_script_file(
				"1012",
				&mod_root,
				&mod_root
					.join("common")
					.join("government_reforms")
					.join("reforms.txt"),
			)
			.expect("parsed government reforms"),
		];
		let index = build_semantic_index(&parsed);
		for name in [
			"ai_weight",
			"mission_weight_helper",
			"mean_time_to_happen",
			"event_weight_helper",
			"ai_will_do",
			"common_weight_helper",
		] {
			assert!(
				!index.references.iter().any(|reference| {
					reference.kind == SymbolKind::ScriptedEffect && reference.name == name
				}),
				"{name} should not be recorded as a scripted effect reference"
			);
		}

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		for name in [
			"ai_weight",
			"mission_weight_helper",
			"mean_time_to_happen",
			"event_weight_helper",
			"ai_will_do",
			"common_weight_helper",
		] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
				"{name} should not produce S002"
			);
		}
	}

	#[test]
	fn new_diplomatic_actions_containers_preserve_nested_effect_calls() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("new_diplomatic_actions"))
			.expect("create new diplomatic actions");
		fs::write(
			mod_root
				.join("common")
				.join("new_diplomatic_actions")
				.join("actions.txt"),
			r#"
static_actions = {
	royal_marriage = {
		alert_index = 1
	}
}

sell_indulgence = {
	is_visible = { always = yes }
	is_allowed = { always = yes }
	on_accept = {
		missing_effect = { FLAG = TEST }
	}
	on_decline = {}
	ai_acceptance = {
		add_entry = {
			name = TRUST
			change_variable = { which = score value = 1 }
			missing_inner_effect = { FLAG = TEST }
		}
	}
}
"#,
		)
		.expect("write actions");

		let parsed = [parse_script_file(
			"1005",
			&mod_root,
			&mod_root
				.join("common")
				.join("new_diplomatic_actions")
				.join("actions.txt"),
		)
		.expect("parsed actions")];
		let index = build_semantic_index(&parsed);
		assert!(index.definitions.iter().any(|definition| {
			definition.kind == SymbolKind::DiplomaticAction
				&& definition.local_name == "sell_indulgence"
		}));
		for name in [
			"static_actions",
			"is_visible",
			"is_allowed",
			"on_accept",
			"on_decline",
			"ai_acceptance",
			"add_entry",
			"royal_marriage",
		] {
			assert!(
				!index.references.iter().any(|reference| {
					reference.kind == SymbolKind::ScriptedEffect && reference.name == name
				}),
				"{name} should not be recorded as a scripted effect reference"
			);
		}

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		for name in [
			"static_actions",
			"is_visible",
			"is_allowed",
			"on_accept",
			"on_decline",
			"ai_acceptance",
			"add_entry",
			"royal_marriage",
		] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
				"{name} should not produce S002"
			);
		}
		for name in ["missing_effect", "missing_inner_effect"] {
			assert!(
				diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) })
			);
		}
	}

	#[test]
	fn new_diplomatic_actions_if_blocks_keep_nested_effect_calls_in_effect_context() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("new_diplomatic_actions"))
			.expect("create new diplomatic actions");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::write(
			mod_root
				.join("common")
				.join("new_diplomatic_actions")
				.join("actions.txt"),
			r#"
request_general = {
	on_accept = {
		if = {
			limit = { always = yes }
			create_general_from_country = { who = FROM }
		}
	}
}
"#,
		)
		.expect("write actions");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
			r#"
create_general_from_country {
	$who$ = {
		trigger_switch = {
			on_trigger = army_tradition
			100 = {
				PREV = {
					create_general = {
						culture = PREV
						tradition = 100
					}
				}
			}
		}
	}
}
"#,
		)
		.expect("write effects");

		let parsed = [
			parse_script_file(
				"1007",
				&mod_root,
				&mod_root
					.join("common")
					.join("new_diplomatic_actions")
					.join("actions.txt"),
			)
			.expect("parsed actions"),
			parse_script_file(
				"1007",
				&mod_root,
				&mod_root
					.join("common")
					.join("scripted_effects")
					.join("effects.txt"),
			)
			.expect("parsed effects"),
		];
		let index = build_semantic_index(&parsed);
		assert!(index.references.iter().any(|reference| {
			reference.kind == SymbolKind::ScriptedEffect
				&& reference.name == "create_general_from_country"
		}));
		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedTrigger
					&& reference.name == "create_general_from_country"
			}),
			"nested if effect calls should not become scripted trigger references"
		);

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.strict.iter().any(|finding| {
				finding.rule_id == "S002"
					&& finding
						.path
						.as_ref()
						.map(|path| path.ends_with("common/new_diplomatic_actions/actions.txt"))
						.unwrap_or(false)
			}),
			"nested if effect calls should resolve as scripted effects"
		);
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001"
					&& finding
						.path
						.as_ref()
						.map(|path| path.ends_with("common/scripted_effects/effects.txt"))
						.unwrap_or(false)
			}),
			"param-driven scope inference should suppress PREV Unknown-scope noise"
		);
	}

	#[test]
	fn full_body_scripted_effect_scope_params_resolve_nested_prev_aliases() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("new_diplomatic_actions"))
			.expect("create new diplomatic actions");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::write(
			mod_root
				.join("common")
				.join("new_diplomatic_actions")
				.join("actions.txt"),
			r#"
request_general = {
	on_accept = {
		if = {
			limit = {
				FROM = {
					army_tradition = 10
				}
			}
			add_favors = { who = FROM amount = -50 }
			create_general_from_country = { who = FROM }
			FROM = {
				add_army_tradition = -10
			}
		}
	}
}
"#,
		)
		.expect("write actions");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
			full_army_tradition_switch_effect("create_general_from_country"),
		)
		.expect("write effects");

		let parsed = [
			parse_script_file(
				"1008",
				&mod_root,
				&mod_root
					.join("common")
					.join("new_diplomatic_actions")
					.join("actions.txt"),
			)
			.expect("parsed actions"),
			parse_script_file(
				"1008",
				&mod_root,
				&mod_root
					.join("common")
					.join("scripted_effects")
					.join("effects.txt"),
			)
			.expect("parsed effects"),
		];
		let index = build_semantic_index(&parsed);
		let callable_scope_map = build_inferred_callable_scope_map(&index);
		let inferred_masks = collect_inferred_callable_masks(&index);
		let nested_prev_usage = index
			.alias_usages
			.iter()
			.find(|usage| {
				usage.alias == "PREV"
					&& usage.path.ends_with("common/scripted_effects/effects.txt")
					&& usage.line > 4
			})
			.expect("nested PREV alias usage");
		assert_ne!(
			effective_alias_scope_mask_with_overrides(
				&index,
				&callable_scope_map,
				&inferred_masks,
				nested_prev_usage.scope_id,
				"PREV",
			),
			0,
			"nested PREV aliases should no longer resolve to Unknown scope"
		);

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001"
					&& finding
						.path
						.as_ref()
						.map(|path| path.ends_with("common/scripted_effects/effects.txt"))
						.unwrap_or(false)
			}),
			"full-body scripted effects should suppress PREV Unknown-scope noise"
		);
	}

	#[test]
	fn cb_types_seed_country_aliases_and_trigger_scopes() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("cb_types")).expect("create cb types");
		fs::write(
			mod_root.join("common").join("cb_types").join("cb.txt"),
			r#"
cb_restore = {
	prerequisites_self = {
		capital_scope = {
			owner = {
				government = monarchy
			}
			is_core = ROOT
		}
	}
	prerequisites = {
		FROM = {
			government = monarchy
		}
	}
	can_use = {
		ROOT = {
			legitimacy = 50
		}
	}
}
"#,
		)
		.expect("write cb types");

		let parsed = [parse_script_file(
			"1006",
			&mod_root,
			&mod_root.join("common").join("cb_types").join("cb.txt"),
		)
		.expect("parsed cb types")];
		let index = build_semantic_index(&parsed);
		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001" && finding.path == Some("common/cb_types/cb.txt".into())
			}),
			"cb types should no longer keep ROOT/FROM/owner/capital_scope under Unknown scope"
		);
		assert!(
			!diagnostics.strict.iter().any(|finding| {
				finding.rule_id == "S002" && finding.path == Some("common/cb_types/cb.txt".into())
			}),
			"cb type trigger containers should not become scripted effect calls"
		);
	}

	#[test]
	fn effect_context_selectors_do_not_become_scripted_effect_calls() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::create_dir_all(mod_root.join("map")).expect("create map");
		fs::write(
			mod_root.join("map").join("region.txt"),
			"hudson_bay_region = { areas = { north_bay_area } }\n",
		)
		.expect("write region");
		fs::write(
			mod_root.join("events").join("selectors.txt"),
			r#"
namespace = test
country_event = {
	id = test.1
	immediate = {
		random_list = {
			50 = {
				missing_weight_effect = { amount = 1 }
			}
		}
		every_country = {
			HBC = {
				add_prestige = 1
			}
			2022 = {
				add_core = HBC
			}
			random_country = {
				add_legitimacy = 1
			}
			every_known_country = {
				add_prestige = 1
			}
			every_subject_country = {
				add_stability = 1
			}
			hudson_bay_region = {
				add_permanent_claim = ROOT
			}
			overlord = {
				add_stability = 1
			}
			missing_effect = { amount = 1 }
		}
		random_owned_province = {
			add_base_tax = 1
		}
		random_province = {
			add_base_production = 1
		}
		every_province = {
			add_base_manpower = 1
		}
		while = {
			limit = { always = yes }
			missing_loop_effect = { amount = 1 }
		}
		IF = {
			limit = { always = yes }
			missing_if_effect = { amount = 1 }
		}
		ELSE_IF = {
			limit = { always = yes }
			missing_else_if_effect = { amount = 1 }
		}
	}
}
"#,
		)
		.expect("write selectors");

		let parsed = [
			parse_script_file("1007", &mod_root, &mod_root.join("map").join("region.txt"))
				.expect("parsed region"),
			parse_script_file(
				"1007",
				&mod_root,
				&mod_root.join("events").join("selectors.txt"),
			)
			.expect("parsed selectors"),
		];
		let index = build_semantic_index(&parsed);
		for name in [
			"random_list",
			"every_country",
			"random_country",
			"every_known_country",
			"every_subject_country",
			"random_owned_province",
			"random_province",
			"every_province",
			"while",
			"IF",
			"ELSE_IF",
			"HBC",
			"2022",
			"hudson_bay_region",
			"overlord",
		] {
			assert!(
				!index.references.iter().any(|reference| {
					reference.kind == SymbolKind::ScriptedEffect && reference.name == name
				}),
				"{name} should not be recorded as a scripted effect reference"
			);
		}

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		for name in [
			"random_list",
			"every_country",
			"random_country",
			"every_known_country",
			"every_subject_country",
			"random_owned_province",
			"random_province",
			"every_province",
			"while",
			"IF",
			"ELSE_IF",
			"HBC",
			"2022",
			"hudson_bay_region",
			"overlord",
		] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
				"{name} should not produce S002"
			);
		}
		for name in [
			"missing_weight_effect",
			"missing_effect",
			"missing_loop_effect",
			"missing_if_effect",
			"missing_else_if_effect",
		] {
			assert!(
				diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) })
			);
		}
	}

	#[test]
	fn on_actions_callbacks_seed_scopes_and_do_not_start_unknown() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("on_actions")).expect("create on_actions");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::write(
			mod_root.join("events").join("events.txt"),
			r#"
namespace = test
country_event = { id = test.1 }
country_event = { id = test.2 }
"#,
		)
		.expect("write events");
		fs::write(
			mod_root
				.join("common")
				.join("on_actions")
				.join("callbacks.txt"),
			r#"
on_adm_development = {
	owner = {
		country_event = { id = test.1 }
	}
	random_owned_province = {
		missing_province_effect = { amount = 1 }
	}
}
on_startup = {
	country_event = { id = test.2 }
	while = {
		limit = { always = yes }
		every_subject_country = {
			missing_country_effect = { amount = 1 }
		}
	}
}
"#,
		)
		.expect("write on_actions");

		let parsed = [
			parse_script_file(
				"1011",
				&mod_root,
				&mod_root.join("events").join("events.txt"),
			)
			.expect("parsed events"),
			parse_script_file(
				"1011",
				&mod_root,
				&mod_root
					.join("common")
					.join("on_actions")
					.join("callbacks.txt"),
			)
			.expect("parsed on_actions"),
		];
		let index = build_semantic_index(&parsed);
		for name in [
			"on_adm_development",
			"on_startup",
			"random_owned_province",
			"while",
			"every_subject_country",
		] {
			assert!(
				!index.references.iter().any(|reference| {
					reference.kind == SymbolKind::ScriptedEffect && reference.name == name
				}),
				"{name} should not be recorded as a scripted effect reference"
			);
		}
		assert!(
			index
				.scopes
				.iter()
				.any(|scope| scope.kind == ScopeKind::Effect
					&& scope.this_type == ScopeType::Province)
		);
		assert!(
			index
				.scopes
				.iter()
				.any(|scope| scope.kind == ScopeKind::Effect
					&& scope.this_type == ScopeType::Country)
		);
		assert!(
			index
				.scopes
				.iter()
				.any(|scope| scope.kind == ScopeKind::AliasBlock
					&& scope.this_type == ScopeType::Country)
		);
		assert!(
			index.scopes.iter().any(
				|scope| scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Province
			)
		);
		assert!(
			index
				.scopes
				.iter()
				.any(|scope| scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Country)
		);

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001"
					&& finding.path == Some("common/on_actions/callbacks.txt".into())
			}),
			"on_actions callbacks should no longer start from Unknown scope"
		);
		for name in [
			"random_owned_province",
			"while",
			"every_subject_country",
			"country_event",
		] {
			assert!(
				!diagnostics.strict.iter().any(|finding| {
					finding.rule_id == "S002"
						&& finding.path == Some("common/on_actions/callbacks.txt".into())
						&& finding.message.contains(name)
				}),
				"{name} should not produce S002 in on_actions callbacks"
			);
		}
		for name in ["missing_province_effect", "missing_country_effect"] {
			assert!(diagnostics.strict.iter().any(|finding| {
				finding.rule_id == "S002"
					&& finding.path == Some("common/on_actions/callbacks.txt".into())
					&& finding.message.contains(name)
			}));
		}
	}

	#[test]
	fn scripted_effect_param_contracts_reduce_s004_noise() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("contracts.txt"),
			r#"
ME_give_claims = {
	add_prestige = 1
}
add_prestige_or_monarch_power = {
	add_prestige = 1
}
country_event_with_option_insight = {
	add_stability = 1
}
create_or_add_center_of_trade_level = {
	add_base_production = 1
}
"#,
		)
		.expect("write scripted effects");
		fs::write(
			mod_root.join("events").join("contracts.txt"),
			r#"
namespace = test
country_event = {
	id = test.1
	immediate = {
		ME_give_claims = {
			area = baltic_area
		}
		ME_give_claims = {
			hidden_effect = {
				region = finland_area
			}
		}
		add_prestige_or_monarch_power = {
			value = 10
		}
		country_event_with_option_insight = {
			id = test.2
			option_3 = some_option
		}
		create_or_add_center_of_trade_level = {
			level = 2
		}
	}
}
"#,
		)
		.expect("write events");

		let parsed = [
			parse_script_file(
				"1010",
				&mod_root,
				&mod_root
					.join("common")
					.join("scripted_effects")
					.join("contracts.txt"),
			)
			.expect("parsed scripted effects"),
			parse_script_file(
				"1010",
				&mod_root,
				&mod_root.join("events").join("contracts.txt"),
			)
			.expect("parsed events"),
		];
		let index = build_semantic_index(&parsed);
		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);

		assert!(
			!diagnostics.strict.iter().any(|finding| {
				finding.rule_id == "S004" && finding.message.contains("缺失 area")
			}),
			"one-of contract should not expand into per-parameter missing messages"
		);
		assert_eq!(
			diagnostics
				.strict
				.iter()
				.filter(|finding| {
					finding.rule_id == "S004"
						&& finding
							.message
							.contains("ME_give_claims 至少需要一个参数: area|region|province|id")
				})
				.count(),
			1,
			"missing one-of params should aggregate into a single message"
		);
		for name in [
			"add_prestige_or_monarch_power",
			"country_event_with_option_insight",
			"create_or_add_center_of_trade_level",
		] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S004" && finding.message.contains(name) }),
				"{name} should satisfy its explicit param contract"
			);
		}
	}

	#[test]
	fn complex_dynamic_effect_contracts_keep_optional_slots_optional() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::create_dir_all(mod_root.join("missions")).expect("create missions");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("contracts.txt"),
			r#"
complex_dynamic_effect = {
	custom_tooltip = $first_custom_tooltip$
	if = {
		limit = { $first_limit$ }
	}
	tooltip = {
		$first_effect$
	}
	[[third_custom_tooltip]
	custom_tooltip = $third_custom_tooltip$
	if = {
		limit = { $third_limit$ }
	}
	tooltip = {
		$third_effect$
	}
	]
	hidden_effect = {
		[[eigth_custom_tooltip]
		else_if = {
			limit = { $eigth_limit$ }
			$eigth_effect$
		}
		]
	}
}

complex_dynamic_effect_without_alternative = {
	custom_tooltip = $first_custom_tooltip$
	if = {
		limit = { $first_limit$ }
	}
	tooltip = {
		$first_effect$
	}
	[[second_custom_tooltip]
	custom_tooltip = $second_custom_tooltip$
	if = {
		limit = { $second_limit$ }
	}
	tooltip = {
		$second_effect$
	}
	]
	[[third_custom_tooltip]
	custom_tooltip = $third_custom_tooltip$
	if = {
		limit = { $third_limit$ }
	}
	tooltip = {
		$third_effect$
	}
	]
	[[combined_effect]
	if = {
		limit = {
			$first_limit$
			$second_limit$
			$third_limit$
			$eigth_limit$
		}
		$combined_effect$
	}
	]
	hidden_effect = {
		[[eigth_custom_tooltip]
		if = {
			limit = { $eigth_limit$ }
			$eigth_effect$
		}
		]
	}
}
"#,
		)
		.expect("write scripted effects");
		fs::write(
			mod_root.join("missions").join("contracts.txt"),
			r#"
test_mission = {
	icon = mission_conquer_1_province
	position = 1
	effect = {
		complex_dynamic_effect = {
			first_custom_tooltip = TEST_DYNAMIC_EFFECT_1
			first_limit = "
				always = yes
			"
			first_effect = "
				add_prestige = 5
			"
		}
		complex_dynamic_effect = {
			first_custom_tooltip = TEST_DYNAMIC_EFFECT_2
			first_limit = "
				always = yes
			"
			first_effect = "
				add_prestige = 10
			"
			third_custom_tooltip = TEST_DYNAMIC_EFFECT_3
			third_limit = "
				always = yes
			"
			third_effect = "
				add_stability = 1
			"
		}
		complex_dynamic_effect_without_alternative = {
			first_custom_tooltip = TEST_DYNAMIC_EFFECT_WITHOUT_ALT_1
			first_limit = "
				always = yes
			"
			first_effect = "
				add_legitimacy = 10
			"
		}
		complex_dynamic_effect_without_alternative = {
			first_custom_tooltip = TEST_DYNAMIC_EFFECT_WITHOUT_ALT_2
			first_limit = "
				always = yes
			"
			first_effect = "
				add_meritocracy = 5
			"
			third_custom_tooltip = TEST_DYNAMIC_EFFECT_WITHOUT_ALT_3
			third_limit = "
				always = yes
			"
			third_effect = "
				add_treasury = 50
			"
		}
	}
}
"#,
		)
		.expect("write missions");

		let parsed = [
			parse_script_file(
				"1012",
				&mod_root,
				&mod_root
					.join("common")
					.join("scripted_effects")
					.join("contracts.txt"),
			)
			.expect("parsed scripted effects"),
			parse_script_file(
				"1012",
				&mod_root,
				&mod_root.join("missions").join("contracts.txt"),
			)
			.expect("parsed missions"),
		];
		let index = build_semantic_index(&parsed);
		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);

		for name in [
			"complex_dynamic_effect",
			"complex_dynamic_effect_without_alternative",
		] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S004" && finding.message.contains(name) }),
				"{name} should treat later dynamic slots as optional"
			);
		}
	}

	#[test]
	fn scripted_effect_named_param_bindings_satisfy_required_params() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::create_dir_all(mod_root.join("common").join("buildings")).expect("create buildings");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
			r#"
update_improved_military_buildings_modifier = {
	if = {
		tooltip = {
			add_province_modifier = {
				name = wei_suo_system_reform_$building$_modifier
				duration = -1
			}
		}
	}
}
"#,
		)
		.expect("write scripted effects");
		fs::write(
			mod_root
				.join("common")
				.join("buildings")
				.join("buildings.txt"),
			r#"
barracks = {
	on_built = {
		update_improved_military_buildings_modifier = {
			building = barracks
		}
	}
}
"#,
		)
		.expect("write buildings");

		let parsed = [
			parse_script_file(
				"1011",
				&mod_root,
				&mod_root
					.join("common")
					.join("scripted_effects")
					.join("effects.txt"),
			)
			.expect("parsed scripted effects"),
			parse_script_file(
				"1011",
				&mod_root,
				&mod_root
					.join("common")
					.join("buildings")
					.join("buildings.txt"),
			)
			.expect("parsed buildings"),
		];
		let index = build_semantic_index(&parsed);
		let reference = index
			.references
			.iter()
			.find(|reference| {
				reference.kind == SymbolKind::ScriptedEffect
					&& reference.name == "update_improved_military_buildings_modifier"
			})
			.expect("scripted effect reference");
		assert!(
			reference
				.provided_params
				.iter()
				.any(|param| param == "building"),
			"named building binding should count as a provided param"
		);

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.strict.iter().any(|finding| {
				finding.rule_id == "S004"
					&& finding
						.message
						.contains("update_improved_military_buildings_modifier")
			}),
			"named building binding should satisfy $building$"
		);
	}

	#[test]
	fn second_wave_param_contracts_and_named_param_collection_work() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("contracts.txt"),
			r#"
add_age_modifier = {
	add_country_modifier = {
		name = $name$
		duration = $duration$
		desc = ME_until_the_end_of_$age$
	}
	else = { [[else] $else$ ] }
}
country_event_with_effect_insight = {
	country_event = {
		id = $id$
		[[days] days = $days$]
		[[random] random = $random$]
		[[tooltip] tooltip = $tooltip$]
	}
	tooltip = {
		$effect$
	}
}
ME_distribute_development = {
	while = {
		limit = { always = yes }
		random_owned_province = {
			[[limit] limit = { $limit$ }]
			add_base_$type$ = 1
		}
	}
	custom_tooltip = $type$_$amount$
	[[tooltip] custom_tooltip = $tooltip$ ]
}
pick_best_provinces = {
	pick_best_provinces_2 = {
		scope = "$scope$"
		scale = "$scale$"
		event_target_name = "$event_target_name$"
		global_trigger = "$global_trigger$"
		1 = "$1$"
		10 = "$10$"
	}
}
ME_overlord_effect = {
	overlord = {
		$effect$
	}
}
create_general_with_pips = {
	create_general = {
		tradition = $tradition$
		[[add_fire] add_fire = $add_fire$ ]
		[[culture] culture = $culture$ ]
	}
}
"#,
		)
		.expect("write scripted effects");
		fs::write(
			mod_root.join("events").join("contracts.txt"),
			r#"
namespace = test
country_event = {
	id = test.1
	immediate = {
		add_age_modifier = {
			age = age_of_discovery
			name = test_modifier
			duration = 365
		}
		country_event_with_effect_insight = {
			id = test.2
			effect = { add_stability = 1 }
		}
		ME_distribute_development = {
			type = production
			amount = 5
		}
		pick_best_provinces = {
			scale = base_tax
			event_target_name = best_province
			global_trigger = always
			1 = always
			10 = never
			effect = { culture = ROOT }
		}
		ME_overlord_effect = {
			effect = { add_prestige = 1 }
		}
		create_general_with_pips = {
			tradition = 40
		}
	}
}
"#,
		)
		.expect("write events");

		let parsed = [
			parse_script_file(
				"1010",
				&mod_root,
				&mod_root
					.join("common")
					.join("scripted_effects")
					.join("contracts.txt"),
			)
			.expect("parsed scripted effects"),
			parse_script_file(
				"1010",
				&mod_root,
				&mod_root.join("events").join("contracts.txt"),
			)
			.expect("parsed events"),
		];
		let index = build_semantic_index(&parsed);
		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		let contract_findings: Vec<String> = diagnostics
			.strict
			.iter()
			.filter(|finding| finding.rule_id == "S004")
			.map(|finding| finding.message.clone())
			.collect();
		for snippet in [
			"add_age_modifier 缺失 else",
			"country_event_with_effect_insight 缺失 days",
			"country_event_with_effect_insight 缺失 tooltip",
			"ME_distribute_development 缺失 limit",
			"ME_distribute_development 缺失 tooltip",
			"pick_best_provinces 缺失 scope",
			"create_general_with_pips 缺失 add_fire",
			"create_general_with_pips 缺失 culture",
		] {
			assert!(
				!contract_findings
					.iter()
					.any(|message| message.contains(snippet)),
				"{snippet} should be optional"
			);
		}

		let pick_best_reference = index
			.references
			.iter()
			.find(|reference| {
				reference.kind == SymbolKind::ScriptedEffect
					&& reference.name == "pick_best_provinces"
			})
			.expect("pick_best_provinces reference");
		for expected in ["scale", "event_target_name", "global_trigger", "1", "10"] {
			assert!(
				pick_best_reference
					.provided_params
					.iter()
					.any(|item| item == expected),
				"missing collected param {expected}"
			);
		}
		assert!(
			!pick_best_reference
				.provided_params
				.iter()
				.any(|item| item == "culture"),
			"nested keys should not be collected as provided params"
		);
	}

	#[test]
	fn third_wave_estate_param_contracts_cover_common_wrappers() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("estate_contracts.txt"),
			r#"
take_estate_land_share_massive = {
	estate = $estate$
	amount = $amount$
}
add_estate_loyalty = {
	estate = $estate$
	short = $short$
	amount = $amount$
}
estate_loyalty = {
	estate = $estate$
	loyalty = $loyalty$
}
estate_influence = {
	estate = $estate$
	influence = $influence$
}
"#,
		)
		.expect("write scripted effects");
		fs::write(
			mod_root.join("events").join("estate_contracts.txt"),
			r#"
namespace = test
country_event = {
	id = test.3
	immediate = {
		take_estate_land_share_massive = {
			estate = all
		}
		add_estate_loyalty = {
			estate = all
			short = yes
		}
		estate_loyalty = {
			estate = all
			loyalty = 50
		}
		estate_influence = {
			estate = all
			influence = 1
		}
	}
}
"#,
		)
		.expect("write events");

		let parsed = [
			parse_script_file(
				"1012",
				&mod_root,
				&mod_root
					.join("common")
					.join("scripted_effects")
					.join("estate_contracts.txt"),
			)
			.expect("parsed scripted effects"),
			parse_script_file(
				"1012",
				&mod_root,
				&mod_root.join("events").join("estate_contracts.txt"),
			)
			.expect("parsed events"),
		];
		let index = build_semantic_index(&parsed);
		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);

		for name in [
			"take_estate_land_share_massive",
			"add_estate_loyalty",
			"estate_loyalty",
			"estate_influence",
		] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S004" && finding.message.contains(name) }),
				"{name} should not produce S004"
			);
		}
	}

	#[test]
	fn fourth_wave_param_contracts_cover_real_corpus_hotspots() {
		let s004_messages = fourth_wave_s004_messages(
			&["events", "fourth_wave_contracts.txt"],
			r#"
namespace = test
country_event = {
	id = test.4
	immediate = {
		unlock_estate_privilege = {
			estate_privilege = estate_church_anti_heresy_act
		}
		HAB_change_habsburg_glory = {
			remove = 20
		}
		HAB_change_habsburg_glory = {
			amount = 15
		}
		add_legitimacy_or_reform_progress = {
			amount = 25
		}
		add_legitimacy_or_reform_progress = {
			value = 10
		}
		EE_change_variable = {
			which = papal_authority_value
			divide = 2
		}
		EE_change_variable = {
			which = papal_authority_value
			multiply = 3
		}
		ME_tim_add_spoils_of_war = {
			add = 2
		}
		ME_tim_add_spoils_of_war = {
			remove = 1
		}
		ME_add_power_projection = {
			amount = 25
		}
		ME_add_power_projection = {
			value = 10
		}
		create_general_scaling_with_tradition_and_pips = { }
		create_general_scaling_with_tradition_and_pips = {
			add_shock = 1
			add_manuever = 1
		}
		ME_automatic_colonization_effect_module = {
			target_region_effect = autonomous_colonist_region_north_africa_colonizing_effect
			superregion = africa_superregion
		}
		ME_automatic_colonization_effect_module = {
			target_region_effect = autonomous_colonist_region_mexico_colonizing_effect
			region = colonial_mexico
		}
		country_event_with_insight = {
			id = test.6
			insight_tooltip = INSIGHT_JUST_TOOLTIP
		}
		country_event_with_insight = {
			id = test.7
			insight_tooltip = ENG_we_will_be_able_to_form
			effect_tooltip = "
				add_stability = 1
			"
		}
		define_and_hire_grand_vizier = {
			type = artist
		}
		define_and_hire_grand_vizier = {
			type = inquisitor
			age = 45
			religion = catholic
		}
		ME_override_country_name = {
			string = NED_united_provinces_name
		}
		ME_override_country_name = {
			name = Ducal_PRU
		}
		persia_indian_hegemony_decision_march_effect = {
			province = 563
			tag_1 = BNG
			tag_2 = TRT
			tag_3 = MKP
			trade_company_region = trade_company_east_india
		}
		persia_indian_hegemony_decision_coup_effect = {
			province = 563
			tag_1 = BNG
			tag_2 = TRT
			tag_3 = MKP
		}
		build_as_many_as_possible = {
			new_building = naval_battery
			upgrade_target = coastal_defence
			pick_best_function = pick_best_navydef_province
			cost = 1
			speed = 1
		}
		give_claims = {
			area = austria_area
		}
		give_claims = {
			id = 134
		}
		pick_best_tags = {
			scale = total_development
			event_target_name = claim_target
			global_trigger = "tag = HAB"
		}
		pick_best_tags = {
			scope = every_country
			scale = total_development
			event_target_name = scoped_claim_target
			global_trigger = "tag = HAB"
			1 = yes
			2 = yes
		}
		ME_add_years_of_trade_income = {
			value = 1
		}
		ME_add_years_of_trade_income = {
			years = 5
		}
	}
}
"#,
		);

		for name in [
			"unlock_estate_privilege",
			"HAB_change_habsburg_glory",
			"add_legitimacy_or_reform_progress",
			"EE_change_variable",
			"ME_tim_add_spoils_of_war",
			"ME_add_power_projection",
			"create_general_scaling_with_tradition_and_pips",
			"ME_automatic_colonization_effect_module",
			"country_event_with_insight",
			"define_and_hire_grand_vizier",
			"ME_override_country_name",
			"persia_indian_hegemony_decision_march_effect",
			"persia_indian_hegemony_decision_coup_effect",
			"build_as_many_as_possible",
			"give_claims",
			"pick_best_tags",
			"ME_add_years_of_trade_income",
		] {
			assert!(
				!s004_messages.iter().any(|message| message.contains(name)),
				"{name} should not produce S004: {s004_messages:?}"
			);
		}
	}

	#[test]
	fn fourth_wave_param_contracts_preserve_required_and_one_of_constraints() {
		let s004_messages = fourth_wave_s004_messages(
			&["events", "fourth_wave_contract_failures.txt"],
			r#"
namespace = test
country_event = {
	id = test.5
	immediate = {
		unlock_estate_privilege = { }
		HAB_change_habsburg_glory = { }
		add_legitimacy_or_reform_progress = { }
		EE_change_variable = {
			which = papal_authority_value
		}
		EE_change_variable = {
			add = 5
		}
		ME_tim_add_spoils_of_war = { }
		ME_add_power_projection = { }
		ME_automatic_colonization_effect_module = {
			target_region_effect = autonomous_colonist_region_north_africa_colonizing_effect
		}
		ME_automatic_colonization_effect_module = {
			region = colonial_mexico
		}
		country_event_with_insight = {
			id = test.6
		}
		country_event_with_insight = {
			insight_tooltip = INSIGHT_JUST_TOOLTIP
		}
		define_and_hire_grand_vizier = { }
		ME_override_country_name = { }
		persia_indian_hegemony_decision_march_effect = {
			province = 563
			trade_company_region = trade_company_east_india
		}
		persia_indian_hegemony_decision_march_effect = {
			tag_1 = BNG
			trade_company_region = trade_company_east_india
		}
		persia_indian_hegemony_decision_march_effect = {
			province = 563
			tag_1 = BNG
		}
		persia_indian_hegemony_decision_coup_effect = {
			province = 563
		}
		persia_indian_hegemony_decision_coup_effect = {
			tag_1 = BNG
		}
		build_as_many_as_possible = {
			new_building = naval_battery
			upgrade_target = coastal_defence
			cost = 1
			speed = 1
		}
		give_claims = { }
		pick_best_tags = {
			event_target_name = claim_target
			global_trigger = "tag = HAB"
		}
		pick_best_tags = {
			scale = total_development
			global_trigger = "tag = HAB"
		}
		pick_best_tags = {
			scale = total_development
			event_target_name = claim_target
		}
		ME_add_years_of_trade_income = { }
	}
}
"#,
		);

		for snippet in [
			"unlock_estate_privilege 缺失 estate_privilege",
			"HAB_change_habsburg_glory 至少需要一个参数: amount|remove",
			"add_legitimacy_or_reform_progress 至少需要一个参数: amount|value",
			"EE_change_variable 至少需要一个参数: add|subtract|divide|multiply",
			"EE_change_variable 缺失 which",
			"ME_tim_add_spoils_of_war 至少需要一个参数: add|remove",
			"ME_add_power_projection 至少需要一个参数: amount|value",
			"ME_automatic_colonization_effect_module 至少需要一个参数: region|superregion",
			"ME_automatic_colonization_effect_module 缺失 target_region_effect",
			"country_event_with_insight 缺失 insight_tooltip",
			"country_event_with_insight 缺失 id",
			"define_and_hire_grand_vizier 缺失 type",
			"ME_override_country_name 至少需要一个参数: country_name|name|country|value|string",
			"persia_indian_hegemony_decision_march_effect 缺失 province",
			"persia_indian_hegemony_decision_march_effect 缺失 tag_1",
			"persia_indian_hegemony_decision_march_effect 缺失 trade_company_region",
			"persia_indian_hegemony_decision_coup_effect 缺失 province",
			"persia_indian_hegemony_decision_coup_effect 缺失 tag_1",
			"build_as_many_as_possible 缺失 pick_best_function",
			"give_claims 至少需要一个参数: area|region|province|id",
			"pick_best_tags 缺失 scale",
			"pick_best_tags 缺失 event_target_name",
			"pick_best_tags 缺失 global_trigger",
			"ME_add_years_of_trade_income 至少需要一个参数: years|value|amount",
		] {
			assert!(
				s004_messages
					.iter()
					.any(|message| message.contains(snippet)),
				"missing expected S004 snippet {snippet}: {s004_messages:?}"
			);
		}
	}

	#[test]
	fn for_control_flow_does_not_emit_s002_or_s004() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::write(
			mod_root.join("events").join("for_control.txt"),
			r#"
namespace = test
country_event = {
	id = test.1
	immediate = {
		for = {
			amount = 3
			effect = {
				missing_effect = { FLAG = TEST }
			}
		}
	}
}
"#,
		)
		.expect("write events");

		let parsed = [parse_script_file(
			"1011",
			&mod_root,
			&mod_root.join("events").join("for_control.txt"),
		)
		.expect("parsed events")];
		let index = build_semantic_index(&parsed);
		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect && reference.name == "for"
			}),
			"for should not be recorded as a scripted effect reference"
		);

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.strict.iter().any(|finding| {
				(finding.rule_id == "S002" || finding.rule_id == "S004")
					&& finding.message.contains("for")
			}),
			"for control flow should not produce S002 or S004"
		);
		assert!(diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S002" && finding.message.contains("missing_effect")
		}));
	}

	#[test]
	fn wrapper_heavy_scripted_effects_infer_scope_without_callers() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("wrappers.txt"),
			r#"
country_wrapper = {
	owner = {
		add_prestige = 1
	}
}

province_wrapper = {
	capital_scope = {
		add_base_tax = 1
	}
}

shared_wrapper = {
	owner = {
		add_prestige = 1
	}
	capital_scope = {
		add_base_tax = 1
	}
}
"#,
		)
		.expect("write scripted effects");

		let parsed = [parse_script_file(
			"1010",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("wrappers.txt"),
		)
		.expect("parsed scripted effects")];
		let index = build_semantic_index(&parsed);

		let mut inferred = std::collections::HashMap::new();
		let mut inferred_masks = std::collections::HashMap::new();
		for definition in &index.definitions {
			if definition.kind == SymbolKind::ScriptedEffect {
				inferred.insert(definition.local_name.clone(), definition.inferred_this_type);
				inferred_masks.insert(definition.local_name.clone(), definition.inferred_this_mask);
			}
		}

		assert_eq!(inferred.get("country_wrapper"), Some(&ScopeType::Country));
		assert_eq!(inferred.get("province_wrapper"), Some(&ScopeType::Province));
		assert_eq!(inferred.get("shared_wrapper"), Some(&ScopeType::Unknown));
		assert_eq!(inferred_masks.get("country_wrapper"), Some(&0b01));
		assert_eq!(inferred_masks.get("province_wrapper"), Some(&0b10));
		assert_eq!(inferred_masks.get("shared_wrapper"), Some(&0b11));

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001"
					&& finding.path == Some("common/scripted_effects/wrappers.txt".into())
			}),
			"wrapper-heavy scripted effects should not stay unknown"
		);
	}

	#[test]
	fn province_id_selectors_seed_province_scope_in_trigger_contexts() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("province_ids.txt"),
			r#"
check_subject_monuments = {
	hidden_effect = {
		if = {
			limit = {
				1775 = {
					owner = {
						has_country_flag = test_flag
					}
				}
			}
		}
	}
}
"#,
		)
		.expect("write scripted effects");

		let parsed = [parse_script_file(
			"1009",
			&mod_root,
			&mod_root
				.join("common")
				.join("scripted_effects")
				.join("province_ids.txt"),
		)
		.expect("parsed scripted effects")];
		let index = build_semantic_index(&parsed);
		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001"
					&& finding.path == Some("common/scripted_effects/province_ids.txt".into())
			}),
			"province id selector should seed Province scope for nested owner blocks"
		);
	}

	#[test]
	fn scripted_effect_scope_inference_reaches_fixpoint() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
			r#"
country_wrapper = {
	conflict = { FLAG = TEST }
}

province_wrapper = {
	chain_a = { FLAG = TEST }
	conflict = { FLAG = TEST }
}

chain_a = {
	chain_b = { FLAG = TEST }
}

chain_b = {
	owner = {
		add_prestige = 1
	}
}

conflict = {
	owner = {
		add_prestige = 1
	}
	capital_scope = {
		add_base_tax = 1
	}
}
"#,
		)
		.expect("write scripted effects");
		fs::write(
			mod_root.join("events").join("event.txt"),
			r#"
namespace = test
country_event = {
	id = test.1
	immediate = {
		country_wrapper = { FLAG = TEST }
		capital_scope = {
			province_wrapper = { FLAG = TEST }
		}
	}
}
"#,
		)
		.expect("write event");

		let parsed = [
			parse_script_file(
				"1008",
				&mod_root,
				&mod_root
					.join("common")
					.join("scripted_effects")
					.join("effects.txt"),
			)
			.expect("parsed scripted effects"),
			parse_script_file(
				"1008",
				&mod_root,
				&mod_root.join("events").join("event.txt"),
			)
			.expect("parsed event"),
		];
		let index = build_semantic_index(&parsed);

		let mut inferred = std::collections::HashMap::new();
		let mut inferred_masks = std::collections::HashMap::new();
		for definition in &index.definitions {
			if definition.kind == SymbolKind::ScriptedEffect {
				inferred.insert(definition.local_name.clone(), definition.inferred_this_type);
				inferred_masks.insert(definition.local_name.clone(), definition.inferred_this_mask);
			}
		}
		assert_eq!(inferred.get("country_wrapper"), Some(&ScopeType::Country));
		assert_eq!(inferred.get("province_wrapper"), Some(&ScopeType::Province));
		assert_eq!(inferred.get("chain_a"), Some(&ScopeType::Province));
		assert_eq!(inferred.get("chain_b"), Some(&ScopeType::Province));
		assert_eq!(inferred.get("conflict"), Some(&ScopeType::Unknown));
		assert_eq!(inferred_masks.get("country_wrapper"), Some(&0b01));
		assert_eq!(inferred_masks.get("province_wrapper"), Some(&0b10));
		assert_eq!(inferred_masks.get("chain_a"), Some(&0b10));
		assert_eq!(inferred_masks.get("chain_b"), Some(&0b10));
		assert_eq!(inferred_masks.get("conflict"), Some(&0b11));

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001"
					&& finding.path == Some("common/scripted_effects/effects.txt".into())
					&& finding.line == Some(14)
			}),
			"chain_b owner scope should resolve to Province after fixpoint inference"
		);
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001"
					&& finding.path == Some("common/scripted_effects/effects.txt".into())
			}),
			"mixed scripted effects should stay usable via mask-aware A001 checks"
		);
	}

	#[test]
	fn scripted_triggers_build_definitions_and_propagate_scope_masks() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("scripted_triggers"))
			.expect("create scripted triggers");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_triggers")
				.join("triggers.txt"),
			r#"
province_only = {
	owner = {
		has_country_flag = seen
	}
}

mixed_trigger = {
	owner = {
		has_country_flag = seen
	}
	capital_scope = {
		has_province_flag = seen
	}
}
"#,
		)
		.expect("write scripted triggers");
		fs::write(
			mod_root.join("events").join("events.txt"),
			r#"
namespace = test
country_event = {
	id = test.1
	trigger = {
		mixed_trigger = yes
	}
}

province_event = {
	id = test.2
	trigger = {
		mixed_trigger = yes
		province_only = yes
	}
}
"#,
		)
		.expect("write events");

		let parsed = [
			parse_script_file(
				"1009",
				&mod_root,
				&mod_root
					.join("common")
					.join("scripted_triggers")
					.join("triggers.txt"),
			)
			.expect("parsed scripted triggers"),
			parse_script_file(
				"1009",
				&mod_root,
				&mod_root.join("events").join("events.txt"),
			)
			.expect("parsed events"),
		];
		let index = build_semantic_index(&parsed);

		let scripted_trigger_defs: std::collections::HashMap<_, _> = index
			.definitions
			.iter()
			.filter(|definition| definition.kind == SymbolKind::ScriptedTrigger)
			.map(|definition| (definition.local_name.as_str(), definition))
			.collect();
		assert_eq!(
			scripted_trigger_defs
				.get("province_only")
				.map(|d| d.inferred_this_mask),
			Some(0b10)
		);
		assert_eq!(
			scripted_trigger_defs
				.get("province_only")
				.map(|d| d.inferred_this_type),
			Some(ScopeType::Province)
		);
		assert_eq!(
			scripted_trigger_defs
				.get("mixed_trigger")
				.map(|d| d.inferred_this_mask),
			Some(0b11)
		);
		assert_eq!(
			scripted_trigger_defs
				.get("mixed_trigger")
				.map(|d| d.inferred_this_type),
			Some(ScopeType::Unknown)
		);

		for name in ["province_only", "mixed_trigger"] {
			assert!(
				index.references.iter().any(|reference| {
					reference.kind == SymbolKind::ScriptedTrigger && reference.name == name
				}),
				"{name} should be recorded as a scripted trigger reference"
			);
		}

		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);
		for name in ["province_only", "mixed_trigger"] {
			assert!(
				!diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
				"{name} should not participate in scripted-effect unresolved-call reporting"
			);
		}
		assert!(
			!diagnostics.advisory.iter().any(|finding| {
				finding.rule_id == "A001"
					&& finding.path == Some("common/scripted_triggers/triggers.txt".into())
			}),
			"scripted triggers should use propagated masks for owner/capital_scope checks"
		);
	}

	#[test]
	fn generic_fallback_requires_explicit_effect_scope() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("events")).expect("create events");
		fs::write(
			mod_root.join("events").join("fallback.txt"),
			r#"
namespace = test
country_event = {
	id = test.1
	stray_container = {
		missing_outer_effect = { amount = 1 }
	}
	immediate = {
		missing_inner_effect = { amount = 1 }
		hidden_effect = {
			missing_hidden_effect = { amount = 1 }
		}
	}
	option = {
		name = ok
		missing_option_effect = { amount = 1 }
	}
}
"#,
		)
		.expect("write file");

		let parsed = parse_script_file(
			"1003",
			&mod_root,
			&mod_root.join("events").join("fallback.txt"),
		)
		.expect("parsed event");
		let index = build_semantic_index(&[parsed]);
		let diagnostics = analyze_visibility(
			&index,
			&AnalyzeOptions {
				mode: AnalysisMode::Semantic,
			},
		);

		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect
					&& reference.name == "missing_outer_effect"
			}),
			"generic block inside an event should not trigger scripted-effect fallback"
		);
		for name in [
			"missing_inner_effect",
			"missing_hidden_effect",
			"missing_option_effect",
		] {
			assert!(
				index.references.iter().any(|reference| {
					reference.kind == SymbolKind::ScriptedEffect && reference.name == name
				}),
				"{name} should still be recorded inside an explicit effect-ish scope"
			);
			assert!(
				diagnostics
					.strict
					.iter()
					.any(|finding| { finding.rule_id == "S002" && finding.message.contains(name) }),
				"{name} should still report unresolved scripted-effect usage"
			);
		}
		assert!(
			!diagnostics.strict.iter().any(|finding| {
				finding.rule_id == "S002" && finding.message.contains("missing_outer_effect")
			}),
			"generic block inside an event should not report S002"
		);
	}
}
