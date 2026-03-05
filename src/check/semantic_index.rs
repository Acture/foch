use crate::check::model::{
	AliasUsage, KeyUsage, ParseIssue, ScopeKind, ScopeNode, ScopeType, SemanticIndex, SourceSpan,
	SymbolDefinition, SymbolKind, SymbolReference,
};
use crate::check::parser::{AstFile, AstStatement, AstValue, SpanRange, parse_clausewitz_file};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptFileKind {
	Events,
	Decisions,
	ScriptedEffects,
	DiplomaticActions,
	TriggeredModifiers,
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
}

pub fn classify_script_file(relative: &Path) -> ScriptFileKind {
	let normalized = relative.to_string_lossy().replace('\\', "/");
	if normalized.starts_with("events/") {
		ScriptFileKind::Events
	} else if normalized.starts_with("decisions/") {
		ScriptFileKind::Decisions
	} else if normalized.starts_with("common/scripted_effects/") {
		ScriptFileKind::ScriptedEffects
	} else if normalized.starts_with("common/diplomatic_actions/") {
		ScriptFileKind::DiplomaticActions
	} else if normalized.starts_with("common/triggered_modifiers/") {
		ScriptFileKind::TriggeredModifiers
	} else {
		ScriptFileKind::Other
	}
}

fn module_name_from_relative(relative: &Path, kind: ScriptFileKind) -> String {
	let normalized = relative.to_string_lossy().replace('\\', "/");
	let parts: Vec<&str> = normalized.split('/').collect();
	let module = match kind {
		ScriptFileKind::Events => "events".to_string(),
		ScriptFileKind::Decisions => "decisions".to_string(),
		ScriptFileKind::ScriptedEffects => module_with_tail(&parts, 2, "scripted_effects"),
		ScriptFileKind::DiplomaticActions => module_with_tail(&parts, 2, "diplomatic_actions"),
		ScriptFileKind::TriggeredModifiers => module_with_tail(&parts, 2, "triggered_modifiers"),
		ScriptFileKind::Other => fallback_module_name(&parts),
	};
	module.replace('-', "_")
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
	let parsed = parse_clausewitz_file(file);

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
	})
}

pub fn build_semantic_index(files: &[ParsedScriptFile]) -> SemanticIndex {
	let mut index = SemanticIndex::default();
	for file in files {
		index.parse_issues.extend(file.parse_issues.clone());
		build_file_index(file, &mut index);
	}
	infer_definition_scope_from_references(&mut index);
	index
}

fn build_file_index(file: &ParsedScriptFile, index: &mut SemanticIndex) {
	let mut aliases = HashMap::new();
	match file.file_kind {
		ScriptFileKind::DiplomaticActions => {
			aliases.insert("THIS".to_string(), ScopeType::Country);
			aliases.insert("ROOT".to_string(), ScopeType::Country);
			aliases.insert("FROM".to_string(), ScopeType::Country);
		}
		ScriptFileKind::Decisions => {
			aliases.insert("THIS".to_string(), ScopeType::Country);
			aliases.insert("ROOT".to_string(), ScopeType::Country);
		}
		_ => {
			aliases.insert("THIS".to_string(), ScopeType::Unknown);
			aliases.insert("ROOT".to_string(), ScopeType::Unknown);
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

struct BuildContext<'a> {
	mod_id: &'a str,
	path: &'a Path,
	file_kind: ScriptFileKind,
	module_name: &'a str,
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
				record_key_usage(index, scope_id, ctx, key, key_span);

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

				if key == "country_event" && scope_kind(index, scope_id) == ScopeKind::File {
					handle_country_event_block(
						index,
						scope_id,
						ctx,
						value,
						current_namespace.clone(),
					);
					continue;
				}

				if is_country_event_call(key, value)
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
					});
				}

				if let AstValue::Block { items, span } = value {
					let definition_kind =
						symbol_definition_kind(ctx.file_kind, key, scope_id, index);
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
							scope_id,
							declared_this_type: scope_this_type(index, scope_id),
							inferred_this_type: ScopeType::Unknown,
							required_params,
						});
					}

					if definition_kind.is_none()
						&& is_scripted_effect_call_candidate(ctx.file_kind, key, scope_id, index)
					{
						let mut provided = collect_provided_params(items);
						provided.sort();
						provided.dedup();
						index.references.push(SymbolReference {
							kind: SymbolKind::ScriptedEffect,
							name: key.clone(),
							module: ctx.module_name.to_string(),
							mod_id: ctx.mod_id.to_string(),
							path: ctx.path.to_path_buf(),
							line: key_span.start.line,
							column: key_span.start.column,
							scope_id,
							provided_params: provided,
						});
					}

					let child_scope = create_child_scope(index, scope_id, ctx, key, span, items);
					let next_scripted_effect = if key == "country_event" {
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

fn handle_country_event_block(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	value: &AstValue,
	namespace: Option<String>,
) {
	let AstValue::Block { items, span } = value else {
		return;
	};

	let mut aliases = scope_aliases(index, scope_id);
	aliases.insert("THIS".to_string(), ScopeType::Country);
	aliases.insert("ROOT".to_string(), ScopeType::Country);
	aliases.insert("PREV".to_string(), scope_this_type(index, scope_id));
	let event_scope = push_scope(
		index,
		ScopeKind::Event,
		Some(scope_id),
		ScopeType::Country,
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
			local_name: "country_event".to_string(),
			mod_id: ctx.mod_id.to_string(),
			path: ctx.path.to_path_buf(),
			line: span.start.line,
			column: span.start.column,
			scope_id: event_scope,
			declared_this_type: ScopeType::Country,
			inferred_this_type: ScopeType::Country,
			required_params: Vec::new(),
		});
	}

	let mut child_ctx = BuildContext {
		mod_id: ctx.mod_id,
		path: ctx.path,
		file_kind: ctx.file_kind,
		module_name: ctx.module_name,
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
	let aliases_re = Regex::new(r"\b(ROOT|FROM|THIS|PREV)\b").expect("valid alias regex");
	match value {
		AstValue::Scalar { value, span } => {
			let text = value.as_text();
			for cap in aliases_re.captures_iter(&text) {
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

	let param_re = Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)\$").expect("valid param regex");
	match value {
		AstValue::Scalar { value, .. } => {
			let text = value.as_text();
			for cap in param_re.captures_iter(&text) {
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
	file_kind: ScriptFileKind,
	key: &str,
	scope_id: usize,
	index: &SemanticIndex,
) -> bool {
	if is_keyword(key) || is_alias_key(key) {
		return false;
	}
	if scope_kind(index, scope_id) == ScopeKind::Trigger {
		return false;
	}
	if is_under_trigger_scope(index, scope_id) {
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

	if key == "trigger"
		|| key == "limit"
		|| key == "potential"
		|| key == "allow"
		|| key == "condition"
		|| key == "hidden_trigger"
	{
		kind = ScopeKind::Trigger;
	} else if key == "effect" || key == "after" {
		kind = ScopeKind::Effect;
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
	} else if key == "country_event" {
		kind = ScopeKind::Event;
		this_type = ScopeType::Country;
		aliases.insert("THIS".to_string(), ScopeType::Country);
		aliases.insert("ROOT".to_string(), ScopeType::Country);
	} else if ctx.file_kind == ScriptFileKind::ScriptedEffects
		&& scope_kind(index, parent_scope_id) == ScopeKind::File
		&& !is_keyword(key)
	{
		kind = ScopeKind::ScriptedEffect;
	}

	if key == "if" || key == "else" || key == "NOT" || key == "OR" || key == "AND" {
		kind = ScopeKind::Trigger;
	}

	if key == "option" {
		kind = ScopeKind::Effect;
	}

	if key == "country_event" && !items.is_empty() {
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

fn is_country_event_call(key: &str, value: &AstValue) -> bool {
	key == "country_event" && matches!(value, AstValue::Block { .. })
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

fn collect_provided_params(items: &[AstStatement]) -> Vec<String> {
	let mut params = Vec::new();
	for stmt in items {
		if let AstStatement::Assignment { key, .. } = stmt
			&& key
				.chars()
				.all(|ch| ch.is_ascii_uppercase() || ch == '_' || ch.is_ascii_digit())
		{
			params.push(key.clone());
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
	matches!(
		key,
		"if" | "else"
			| "limit" | "trigger"
			| "effect"
			| "option"
			| "after" | "NOT"
			| "OR" | "AND"
			| "allow" | "potential"
			| "condition"
			| "modifier"
			| "ai_chance"
			| "hidden_effect"
			| "custom_tooltip"
			| "hidden_trigger"
			| "ai_will_do"
			| "else_if"
			| "chance"
			| "base" | "mean_time_to_happen"
			| "immediate"
			| "on_add"
			| "on_remove"
			| "active"
			| "country_decisions"
			| "province_decisions"
			| "religion_decisions"
			| "government_decisions"
			| "from" | "every_owned_province"
			| "country_event"
			| "province_event"
			| "namespace"
			| "id" | "title"
			| "desc" | "name"
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
	use std::collections::{HashMap, HashSet};

	let mut observed: HashMap<usize, HashSet<ScopeType>> = HashMap::new();
	for reference in &index.references {
		if reference.kind != SymbolKind::ScriptedEffect {
			continue;
		}
		let caller_type = scope_this_type(index, reference.scope_id);
		if caller_type == ScopeType::Unknown {
			continue;
		}
		for def_idx in resolve_scripted_effect_reference_targets(index, reference) {
			observed.entry(def_idx).or_default().insert(caller_type);
		}
	}

	for (idx, definition) in index.definitions.iter_mut().enumerate() {
		if definition.kind != SymbolKind::ScriptedEffect {
			continue;
		}
		let inferred = observed.get(&idx).map_or(ScopeType::Unknown, |set| {
			let has_country = set.contains(&ScopeType::Country);
			let has_province = set.contains(&ScopeType::Province);
			match (has_country, has_province) {
				(true, false) => ScopeType::Country,
				(false, true) => ScopeType::Province,
				(true, true) => ScopeType::Unknown,
				(false, false) => ScopeType::Unknown,
			}
		});
		definition.inferred_this_type = if inferred == ScopeType::Unknown {
			definition.declared_this_type
		} else {
			inferred
		};
	}
}

#[cfg(test)]
mod tests {
	use super::{ScriptFileKind, build_semantic_index, classify_script_file, parse_script_file};
	use crate::check::model::{ScopeType, SymbolKind};
	use std::fs;
	use tempfile::TempDir;

	#[test]
	fn classify_paths() {
		assert_eq!(
			classify_script_file(std::path::Path::new("common/scripted_effects/a.txt")),
			ScriptFileKind::ScriptedEffects
		);
		assert_eq!(
			classify_script_file(std::path::Path::new("events/a.txt")),
			ScriptFileKind::Events
		);
	}

	#[test]
	fn index_builds_event_and_scope_types() {
		let tmp = TempDir::new().expect("temp dir");
		let mod_root = tmp.path().join("mod");
		fs::create_dir_all(mod_root.join("events")).expect("create dir");
		fs::write(
			mod_root.join("events").join("x.txt"),
			"namespace = test\ncountry_event = { id = test.1 option = { every_owned_province = { ROOT = { } } } }\n",
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
				.scopes
				.iter()
				.any(|scope| scope.this_type == ScopeType::Province)
		);
	}
}
