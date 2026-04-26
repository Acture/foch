mod extractors;
mod parse_cache;
mod scope_rules;

use super::content_family::{
	ContentFamilyDescriptor, ContentFamilyScopePolicy, GameProfile, ScriptFileKind,
	module_name_for_descriptor,
};
use super::eu4_builtin::{
	is_builtin_effect, is_builtin_iterator, is_builtin_scope_changer, is_builtin_special_block,
	is_builtin_trigger, is_contextual_keyword, is_game_only_candidate, is_reserved_keyword,
};
use super::eu4_profile::eu4_profile;
use super::localisation::collect_localisation_definitions_from_root;
use super::param_contracts::{
	apply_registered_param_contracts, explicit_contract_param_names, registered_param_contract,
};
use super::parser::{AstFile, AstStatement, AstValue, SpanRange};
use foch_core::model::{
	AliasUsage, DocumentFamily, DocumentRecord, KeyUsage, LocalisationDefinition, ParamBinding,
	ParseIssue, ResourceReference, ScalarAssignment, ScopeKind, ScopeNode, ScopeType,
	SemanticIndex, SourceSpan, SymbolDefinition, SymbolKind, SymbolReference, UiDefinition,
};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct ParsedScriptFile {
	pub mod_id: String,
	pub path: PathBuf,
	pub relative_path: PathBuf,
	pub content_family: Option<&'static ContentFamilyDescriptor>,
	pub file_kind: ScriptFileKind,
	pub module_name: String,
	pub ast: AstFile,
	pub parse_issues: Vec<ParseIssue>,
	pub parse_cache_hit: bool,
}

use parse_cache::parse_clausewitz_file_cached;
use scope_rules::{
	file_kind_container_scope_kind, is_country_file_reference, is_country_tag_selector,
	is_country_tag_text, is_dynamic_scope_reference_key, is_province_id_selector,
	is_province_id_text, iterator_scope_type, looks_like_map_group_key, scope_changer_target_type,
	special_block_scope_kind,
};

pub fn classify_script_file(relative: &Path) -> ScriptFileKind {
	eu4_profile()
		.classify_content_family(relative)
		.map_or(ScriptFileKind::Other, |descriptor| {
			descriptor.script_file_kind
		})
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

fn fallback_module_name(parts: &[&str]) -> String {
	if parts.len() <= 1 {
		return "other".to_string();
	}
	parts[..parts.len() - 1].join(".")
}

fn fallback_module_name_from_relative(relative: &Path) -> String {
	let normalized = relative.to_string_lossy().replace('\\', "/");
	let parts: Vec<&str> = normalized.split('/').collect();
	fallback_module_name(&parts)
}

fn qualify_symbol_name(module: &str, local: &str) -> String {
	format!("eu4::{module}::{local}")
}

pub fn parse_script_file(mod_id: &str, root: &Path, file: &Path) -> Option<ParsedScriptFile> {
	parse_script_file_with_profile(mod_id, root, file, eu4_profile())
}

pub fn parse_script_file_with_profile(
	mod_id: &str,
	root: &Path,
	file: &Path,
	profile: &dyn GameProfile,
) -> Option<ParsedScriptFile> {
	let relative = file.strip_prefix(root).ok()?.to_path_buf();
	let content_family = profile.classify_content_family(&relative);
	let file_kind = content_family.map_or(ScriptFileKind::Other, |descriptor| {
		descriptor.script_file_kind
	});
	let module_name = content_family.map_or_else(
		|| fallback_module_name_from_relative(&relative),
		|descriptor| module_name_for_descriptor(&relative, descriptor).replace('-', "_"),
	);
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
		content_family,
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

pub fn build_semantic_index(files: &[ParsedScriptFile]) -> SemanticIndex {
	build_semantic_index_with_profile(files, eu4_profile())
}

pub fn build_semantic_index_with_profile(
	files: &[ParsedScriptFile],
	_profile: &dyn GameProfile,
) -> SemanticIndex {
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
	let scope_policy = file
		.content_family
		.map_or(ContentFamilyScopePolicy::default(), |descriptor| {
			descriptor.scope_policy
		});
	let root_this_type = scope_policy.root_scope;
	aliases.insert("THIS".to_string(), root_this_type);
	aliases.insert("ROOT".to_string(), root_this_type);
	if let Some(from_alias) = scope_policy.from_alias {
		aliases.insert("FROM".to_string(), from_alias);
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
		content_family: file.content_family,
		file_kind: file.file_kind,
		module_name: &file.module_name,
		map_groups,
		technology_monarch_power: None,
		technology_definition_ordinal: 0,
		random_map_tile_emitted: false,
		random_name_table_emitted: false,
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
	content_family: Option<&'static ContentFamilyDescriptor>,
	file_kind: ScriptFileKind,
	module_name: &'a str,
	map_groups: &'a MapGroupLookup,
	technology_monarch_power: Option<String>,
	technology_definition_ordinal: usize,
	random_map_tile_emitted: bool,
	random_name_table_emitted: bool,
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
					record_foundation_resource_semantics(
						index, scope_id, ctx, key, key_span, value,
					);
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
		content_family: ctx.content_family,
		file_kind: ctx.file_kind,
		module_name: ctx.module_name,
		map_groups: ctx.map_groups,
		technology_monarch_power: ctx.technology_monarch_power.clone(),
		technology_definition_ordinal: ctx.technology_definition_ordinal,
		random_map_tile_emitted: ctx.random_map_tile_emitted,
		random_name_table_emitted: ctx.random_name_table_emitted,
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
	let Some(descriptor) = ctx.content_family else {
		return;
	};
	if let Some(extractor) = extractors::extractor_for(descriptor) {
		extractor.extract(index, scope_id, ctx, key, key_span, value);
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
		if key != key_name {
			continue;
		}
		if let Some(text) = scalar_text(value) {
			values.push(text);
			continue;
		}
		values.extend(extract_block_scalar_items(value));
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
			if !is_keyword(key) && !matches!(key, "static_actions") =>
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
		|| is_game_only_candidate(key)
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
		|| is_game_only_candidate(key)
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

fn province_name_table_id(path: &Path) -> Option<String> {
	path.file_stem()
		.and_then(|stem| stem.to_str())
		.map(std::string::ToString::to_string)
}

fn random_map_tile_id(path: &Path) -> Option<String> {
	path.file_stem()
		.and_then(|stem| stem.to_str())
		.map(std::string::ToString::to_string)
}

fn random_name_table_id(path: &Path) -> Option<String> {
	match path.file_stem().and_then(|stem| stem.to_str()) {
		Some("RandomLandNames") => Some("random_land_names".to_string()),
		Some("RandomSeaNames") => Some("random_sea_names".to_string()),
		Some("RandomLakeNames") => Some("random_lake_names".to_string()),
		_ => None,
	}
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

pub fn is_decision_container_key(key: &str) -> bool {
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
	// Scripted effects/triggers are global — if any definition matches by
	// name+kind, the reference is resolved (EU4 uses last-loaded-wins for
	// duplicate names, so having 1+ matches means the key exists).
	if !by_local.is_empty() {
		return by_local;
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
mod tests;
