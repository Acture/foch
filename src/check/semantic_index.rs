use crate::check::eu4_builtin::{
	is_builtin_effect, is_builtin_iterator, is_builtin_scope_changer, is_builtin_special_block,
	is_builtin_trigger, is_contextual_keyword, is_reserved_keyword,
};
use crate::check::localisation::collect_localisation_definitions_from_root;
use crate::check::model::{
	AliasUsage, DocumentFamily, DocumentRecord, KeyUsage, LocalisationDefinition, ParamBinding,
	ParseIssue, ResourceReference, ScalarAssignment, ScopeKind, ScopeNode, ScopeType,
	SemanticIndex, SourceSpan, SymbolDefinition, SymbolKind, SymbolReference, UiDefinition,
};
use crate::check::param_contracts::{
	apply_registered_param_contracts, explicit_contract_param_names, registered_param_contract,
};
use crate::check::parser::{AstFile, AstStatement, AstValue, SpanRange, parse_clausewitz_file};
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

const PARSE_CACHE_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ParseCacheEntry {
	version: u32,
	file_len: u64,
	modified_nanos: u128,
	result: crate::check::parser::ParseResult,
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
	} else if normalized.starts_with("common/diplomatic_actions/") {
		ScriptFileKind::DiplomaticActions
	} else if normalized.starts_with("common/new_diplomatic_actions/") {
		ScriptFileKind::NewDiplomaticActions
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
		ScriptFileKind::GovernmentReforms => {
			module_with_tail(&parts, 2, "government_reforms")
		}
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
			} = stmt
				&& !is_keyword(key)
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

fn parse_clausewitz_file_cached(path: &Path) -> (crate::check::parser::ParseResult, bool) {
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
							required_params,
							param_contract: registered_param_contract(key),
						});
					}

					if definition_kind.is_none()
						&& is_scripted_effect_call_candidate(ctx, ctx.file_kind, key, scope_id, index)
					{
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
			required_params: Vec::new(),
			param_contract: None,
		});
	}

	let mut child_ctx = BuildContext {
		mod_id: ctx.mod_id,
		path: ctx.path,
		file_kind: ctx.file_kind,
		module_name: ctx.module_name,
		map_groups: ctx.map_groups,
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
	match value {
		AstValue::Scalar { value, span } => {
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
		AstValue::Block { items, .. } => {
			for item in items {
				match item {
					AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
						record_alias_tokens_from_value(index, scope_id, ctx, value)
					}
					AstStatement::Comment { .. } => {}
				}
			}
		}
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
		| ScriptFileKind::CustomizableLocalization => ScopeType::Country,
		ScriptFileKind::Missions | ScriptFileKind::NewDiplomaticActions => ScopeType::Country,
		ScriptFileKind::Buildings
		| ScriptFileKind::GreatProjects
		| ScriptFileKind::Institutions
		| ScriptFileKind::ProvinceTriggeredModifiers => ScopeType::Province,
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

fn is_effect_like_scope(index: &SemanticIndex, scope_id: usize) -> bool {
	if scope_kind(index, scope_id) == ScopeKind::Trigger {
		return false;
	}
	!is_under_trigger_scope(index, scope_id)
}

fn allows_generic_scripted_effect_fallback(scope_kind: ScopeKind) -> bool {
	matches!(
		scope_kind,
		ScopeKind::Effect
			| ScopeKind::AliasBlock
			| ScopeKind::Loop
			| ScopeKind::ScriptedEffect
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
	if matches!(key, "for" | "while" | "IF" | "ELSE_IF" | "else_if" | "ELSE" | "else") {
		return Some(EffectContextScopeSemantics::EffectContainer);
	}
	if matches!(key, "every_country" | "every_subject_country" | "every_known_country" | "random_country") {
		return Some(EffectContextScopeSemantics::Iterator(ScopeType::Country));
	}
	if matches!(key, "random_owned_province" | "random_province" | "every_province") {
		return Some(EffectContextScopeSemantics::Iterator(ScopeType::Province));
	}
	if key == "overlord" {
		return Some(EffectContextScopeSemantics::ScopeChanger(ScopeType::Country));
	}
	if let Some(selector) = numeric_effect_context_semantics(key) {
		return Some(selector);
	}
	if is_country_tag_selector(key) {
		return Some(EffectContextScopeSemantics::ScopeChanger(ScopeType::Country));
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
		"on_adm_development" | "on_dip_development" | "on_mil_development" => {
			ScopeType::Province
		}
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
			| "after"
			| "hidden_effect"
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
	} else if is_province_id_selector(key) && scope_kind(index, parent_scope_id) != ScopeKind::File {
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
	} else if key == "FROM" {
		kind = ScopeKind::AliasBlock;
		this_type = aliases.get("FROM").copied().unwrap_or(ScopeType::Unknown);
		aliases.insert("THIS".to_string(), this_type);
	} else if let Some(event_this_type) = event_scope_type(key) {
		kind = ScopeKind::Event;
		this_type = event_this_type;
		aliases.insert("THIS".to_string(), event_this_type);
		aliases.insert("ROOT".to_string(), event_this_type);
		if let Some(from_type) = event_from_type(key) {
			aliases.insert("FROM".to_string(), from_type);
		}
	} else if ctx.file_kind == ScriptFileKind::ScriptedEffects
		&& scope_kind(index, parent_scope_id) == ScopeKind::File
		&& !is_keyword(key)
	{
		kind = ScopeKind::ScriptedEffect;
	}

	if key == "if" || key == "else" {
		if effect_context_semantics == Some(EffectContextScopeSemantics::EffectContainer) {
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

fn iterator_scope_type(key: &str) -> Option<ScopeType> {
	match key {
		"all_core_province" | "all_owned_province" | "any_owned_province" | "all_state_province" => {
			Some(ScopeType::Province)
		}
		"all_subject_country" => Some(ScopeType::Country),
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
	matches!(ctx.file_kind, ScriptFileKind::Missions | ScriptFileKind::CbTypes)
		&& scope_kind(index, scope_id) != ScopeKind::File
		&& looks_like_map_group_key(key)
}

fn looks_like_map_group_key(key: &str) -> bool {
	key.ends_with("_area")
		|| key.ends_with("_region")
		|| key.ends_with("_superregion")
		|| key.ends_with("_provincegroup")
}

fn is_country_tag_selector(key: &str) -> bool {
	key.len() == 3 && key.chars().all(|ch| ch.is_ascii_uppercase())
}

fn is_province_id_selector(key: &str) -> bool {
	key
		.parse::<u32>()
		.map(|value| value > 100)
		.unwrap_or(false)
}

fn numeric_effect_context_semantics(key: &str) -> Option<EffectContextScopeSemantics> {
	let value = key.parse::<u32>().ok()?;
	if value <= 100 {
		Some(EffectContextScopeSemantics::EffectContainer)
	} else {
		Some(EffectContextScopeSemantics::ScopeChanger(ScopeType::Province))
	}
}

fn file_kind_container_scope_kind(
	file_kind: ScriptFileKind,
	key: &str,
) -> Option<ScopeKind> {
	match file_kind {
		ScriptFileKind::Missions => match key {
			"potential_on_load"
			| "potential"
			| "trigger"
			| "provinces_to_highlight"
			| "completed_by" => Some(ScopeKind::Trigger),
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
					| "on_upgraded"
					| "on_downgraded"
					| "on_obtained"
					| "on_lost"
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
		"possible" | "visible" | "happened" | "provinces_to_highlight"
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
		if let AstStatement::Assignment { key, value, .. } = stmt
			&& (key
				.chars()
				.all(|ch| ch.is_ascii_uppercase() || ch == '_' || ch.is_ascii_digit())
				|| contract_names.contains(key.as_str()))
		{
			params.names.push(key.clone());
			if let Some(value) = scalar_text(value) {
				params.bindings.push(ParamBinding {
					name: key.clone(),
					value,
				});
			}
		}
	}
	params
}

fn is_alias_key(key: &str) -> bool {
	matches!(key, "ROOT" | "FROM" | "THIS" | "PREV")
}

fn is_decision_container_key(key: &str) -> bool {
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

pub fn resolve_scripted_effect_reference_targets(
	index: &SemanticIndex,
	reference: &SymbolReference,
) -> Vec<usize> {
	if reference.kind != SymbolKind::ScriptedEffect {
		return Vec::new();
	}

	let mut exact = Vec::new();
	for (idx, def) in index.definitions.iter().enumerate() {
		if def.kind != SymbolKind::ScriptedEffect {
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
		if def.kind != SymbolKind::ScriptedEffect {
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

fn infer_definition_scope_from_references(index: &mut SemanticIndex) {
	use std::collections::HashMap;

	let scripted_effect_scope_map: HashMap<usize, usize> = index
		.definitions
		.iter()
		.enumerate()
		.filter_map(|(idx, definition)| {
			(definition.kind == SymbolKind::ScriptedEffect).then_some((definition.scope_id, idx))
		})
		.collect();

	let mut observed_masks: Vec<u8> = index
		.definitions
		.iter()
		.map(|definition| {
			if definition.kind == SymbolKind::ScriptedEffect {
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
			if reference.kind != SymbolKind::ScriptedEffect {
				continue;
			}
			let caller_type = effective_scope_this_type_with_overrides(
				index,
				&scripted_effect_scope_map,
				&observed_masks,
				reference.scope_id,
			);
			if caller_type == ScopeType::Unknown {
				continue;
			}
			for def_idx in resolve_scripted_effect_reference_targets(index, reference) {
				let merged = observed_masks[def_idx] | scope_type_mask(caller_type);
				if merged != observed_masks[def_idx] {
					observed_masks[def_idx] = merged;
					changed = true;
				}
			}
		}
	}

	for (idx, definition) in index.definitions.iter_mut().enumerate() {
		if definition.kind != SymbolKind::ScriptedEffect {
			continue;
		}
		definition.inferred_this_type = scope_type_from_mask(observed_masks[idx]);
	}
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

fn effective_scope_this_type_with_overrides(
	index: &SemanticIndex,
	scripted_effect_scope_map: &HashMap<usize, usize>,
	observed_masks: &[u8],
	mut scope_id: usize,
) -> ScopeType {
	loop {
		let Some(scope) = index.scopes.get(scope_id) else {
			return ScopeType::Unknown;
		};
		if scope.this_type != ScopeType::Unknown {
			return scope.this_type;
		}
		if let Some(def_idx) = scripted_effect_scope_map.get(&scope_id) {
			let inferred = observed_masks
				.get(*def_idx)
				.copied()
				.map(scope_type_from_mask)
				.unwrap_or(ScopeType::Unknown);
			if inferred != ScopeType::Unknown {
				return inferred;
			}
		}
		let Some(parent) = scope.parent else {
			return ScopeType::Unknown;
		};
		scope_id = parent;
	}
}

#[cfg(test)]
mod tests {
	use super::{
		ScriptFileKind, build_semantic_index, classify_script_file, parse_script_file, scope_kind,
	};
	use crate::check::analysis::{AnalyzeOptions, analyze_visibility};
	use crate::check::model::{AnalysisMode, ScopeKind, ScopeType, SymbolKind};
	use std::fs;
	use tempfile::TempDir;

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
			classify_script_file(std::path::Path::new("common/great_projects/01_monuments.txt")),
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
			classify_script_file(std::path::Path::new("common/custom_gui/AdvisorActionsGui.txt")),
			ScriptFileKind::CustomGui
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/advisortypes/00_advisortypes.txt")),
			ScriptFileKind::AdvisorTypes
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("common/event_modifiers/00_modifiers.txt")),
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
		assert!(index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Province));

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
				!diagnostics.strict.iter().any(|finding| {
					finding.rule_id == "S002" && finding.message.contains(name)
				}),
				"{name} should not produce S002"
			);
		}
		assert!(
			!diagnostics
				.advisory
				.iter()
				.any(|finding| finding.rule_id == "A001" && finding.path == Some("common/achievements.txt".into())),
			"achievements root scope should no longer stay Unknown"
		);
	}

	#[test]
	fn common_data_file_roots_do_not_become_scripted_effect_calls() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("common").join("ideas")).expect("create ideas");
		fs::create_dir_all(mod_root.join("common").join("ages")).expect("create ages");
		fs::create_dir_all(mod_root.join("common").join("buildings"))
			.expect("create buildings");
		fs::create_dir_all(mod_root.join("common").join("great_projects"))
			.expect("create monuments");
		fs::create_dir_all(mod_root.join("common").join("institutions"))
			.expect("create institutions");
		fs::create_dir_all(mod_root.join("common").join("province_triggered_modifiers"))
			.expect("create province modifiers");
		fs::create_dir_all(mod_root.join("common").join("custom_gui"))
			.expect("create custom gui");
		fs::create_dir_all(mod_root.join("common").join("government_names"))
			.expect("create government names");
		fs::create_dir_all(mod_root.join("customizable_localization"))
			.expect("create custom loc");
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
				&mod_root.join("common").join("buildings").join("buildings.txt"),
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
		fs::create_dir_all(mod_root.join("events").join("common").join("new_diplomatic_actions"))
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
				!diagnostics.strict.iter().any(|finding| {
					finding.rule_id == "S002" && finding.message.contains(name)
				}),
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
				!diagnostics.strict.iter().any(|finding| {
					finding.rule_id == "S002" && finding.message.contains(name)
				}),
				"{name} should not produce S002"
			);
		}
		assert!(diagnostics.strict.iter().any(|finding| {
			finding.rule_id == "S002" && finding.message.contains("missing_effect")
		}));
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
			definition.kind == SymbolKind::DiplomaticAction && definition.local_name == "sell_indulgence"
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
				!diagnostics.strict.iter().any(|finding| {
					finding.rule_id == "S002" && finding.message.contains(name)
				}),
				"{name} should not produce S002"
			);
		}
		for name in ["missing_effect", "missing_inner_effect"] {
			assert!(diagnostics.strict.iter().any(|finding| {
				finding.rule_id == "S002" && finding.message.contains(name)
			}));
		}
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
				!diagnostics.strict.iter().any(|finding| {
					finding.rule_id == "S002" && finding.message.contains(name)
				}),
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
			assert!(diagnostics.strict.iter().any(|finding| {
				finding.rule_id == "S002" && finding.message.contains(name)
			}));
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
			parse_script_file("1011", &mod_root, &mod_root.join("events").join("events.txt"))
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
		assert!(index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Effect && scope.this_type == ScopeType::Province));
		assert!(index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Effect && scope.this_type == ScopeType::Country));
		assert!(index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::AliasBlock && scope.this_type == ScopeType::Country));
		assert!(index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Province));
		assert!(index
			.scopes
			.iter()
			.any(|scope| scope.kind == ScopeKind::Loop && scope.this_type == ScopeType::Country));

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
			parse_script_file("1010", &mod_root, &mod_root.join("events").join("contracts.txt"))
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
				!diagnostics.strict.iter().any(|finding| {
					finding.rule_id == "S004" && finding.message.contains(name)
				}),
				"{name} should satisfy its explicit param contract"
			);
		}
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
			parse_script_file("1010", &mod_root, &mod_root.join("events").join("contracts.txt"))
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
				!contract_findings.iter().any(|message| message.contains(snippet)),
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
			parse_script_file("1008", &mod_root, &mod_root.join("events").join("event.txt"))
				.expect("parsed event"),
		];
		let index = build_semantic_index(&parsed);

		let mut inferred = std::collections::HashMap::new();
		for definition in &index.definitions {
			if definition.kind == SymbolKind::ScriptedEffect {
				inferred.insert(definition.local_name.clone(), definition.inferred_this_type);
			}
		}
		assert_eq!(inferred.get("country_wrapper"), Some(&ScopeType::Country));
		assert_eq!(inferred.get("province_wrapper"), Some(&ScopeType::Province));
		assert_eq!(inferred.get("chain_a"), Some(&ScopeType::Province));
		assert_eq!(inferred.get("chain_b"), Some(&ScopeType::Province));
		assert_eq!(inferred.get("conflict"), Some(&ScopeType::Unknown));

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
		assert!(diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A001"
				&& finding.path == Some("common/scripted_effects/effects.txt".into())
		}));
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
				diagnostics.strict.iter().any(|finding| {
					finding.rule_id == "S002" && finding.message.contains(name)
				}),
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
