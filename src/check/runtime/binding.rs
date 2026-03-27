use crate::check::analyzer::parser::{AstStatement, AstValue, SpanRange};
use crate::check::analyzer::semantic_index::{
	ParsedScriptFile, build_semantic_index, parse_script_file,
	resolve_scripted_effect_reference_targets, resolve_scripted_trigger_reference_targets,
};
use crate::check::base_data::base_game_mod_id;
use crate::check::merge::emit::emit_clausewitz_statements;
use crate::check::model::{CheckRequest, SymbolKind, SymbolReference};
use crate::check::runtime::overlap::{OverlapStatus, classify_definition_overlaps};
use crate::check::workspace::{ResolvedWorkspace, WorkspaceResolveErrorKind, resolve_workspace};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DependencyMatchKind {
	ModId,
	DescriptorName,
	None,
}

#[derive(Clone, Debug)]
pub(crate) struct DefinitionRecord {
	pub index: usize,
	pub kind: SymbolKind,
	pub name: String,
	pub local_name: String,
	pub mod_id: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub precedence: usize,
	pub root_mergeable: bool,
	pub normalized_statement: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeState {
	pub semantic_index: crate::check::model::SemanticIndex,
	pub definitions: Vec<DefinitionRecord>,
	pub overlap_status_by_def: HashMap<usize, OverlapStatus>,
	pub winner_by_symbol: HashMap<(SymbolKind, String), usize>,
	pub dependency_hints: HashMap<(String, String), DependencyMatchKind>,
	pub scope_definition_map: HashMap<usize, Vec<usize>>,
	pub enabled_mod_ids: HashSet<String>,
	pub base_game_mod_id: Option<String>,
}

pub(crate) fn build_runtime_state_for_request(
	request: &CheckRequest,
	include_game_base: bool,
) -> Result<RuntimeState, String> {
	let workspace = resolve_workspace(request, include_game_base).map_err(|err| {
		if err.kind == WorkspaceResolveErrorKind::PlaylistFormat {
			"无法解析 Playset JSON".to_string()
		} else {
			err.message
		}
	})?;
	build_runtime_state_from_workspace(&workspace)
}

pub(crate) fn build_runtime_state_from_workspace(
	workspace: &ResolvedWorkspace,
) -> Result<RuntimeState, String> {
	let enabled_mod_ids = workspace
		.mods
		.iter()
		.filter(|item| item.entry.enabled)
		.map(|item| item.mod_id.clone())
		.collect::<HashSet<_>>();
	let base_mod_id = workspace
		.installed_base_snapshot
		.as_ref()
		.map(|_| base_game_mod_id(workspace.playlist.game.key()));
	let parsed_scripts =
		collect_workspace_scripts(workspace, &enabled_mod_ids, base_mod_id.as_deref());
	let semantic_index = build_semantic_index(&parsed_scripts);
	let precedence_by_mod = build_precedence_map(workspace, base_mod_id.as_deref());
	let definitions =
		collect_definition_records(&semantic_index, &parsed_scripts, &precedence_by_mod)?;
	let overlap_status_by_def = classify_definition_overlaps(&definitions, base_mod_id.as_deref());
	let winner_by_symbol = build_winner_lookup(&definitions);
	let dependency_hints = build_dependency_hints(workspace, base_mod_id.as_deref());
	let mut scope_definition_map = HashMap::<usize, Vec<usize>>::new();
	for definition in &definitions {
		scope_definition_map
			.entry(
				semantic_index
					.definitions
					.get(definition.index)
					.map(|item| item.scope_id)
					.unwrap_or_default(),
			)
			.or_default()
			.push(definition.index);
	}

	Ok(RuntimeState {
		semantic_index,
		definitions,
		overlap_status_by_def,
		winner_by_symbol,
		dependency_hints,
		scope_definition_map,
		enabled_mod_ids,
		base_game_mod_id: base_mod_id,
	})
}

pub(crate) fn runtime_reference_target(
	state: &RuntimeState,
	reference: &SymbolReference,
) -> Option<usize> {
	match reference.kind {
		SymbolKind::Event => pick_highest_precedence(
			state,
			state
				.definitions
				.iter()
				.filter(|definition| {
					definition.kind == SymbolKind::Event && definition.name == reference.name
				})
				.map(|definition| definition.index)
				.collect(),
		),
		SymbolKind::ScriptedEffect => pick_highest_precedence(
			state,
			resolve_scripted_effect_reference_targets(&state.semantic_index, reference),
		),
		SymbolKind::ScriptedTrigger => pick_highest_precedence(
			state,
			resolve_scripted_trigger_reference_targets(&state.semantic_index, reference),
		),
		_ => None,
	}
}

pub(crate) fn nearest_enclosing_definition(
	state: &RuntimeState,
	mut scope_id: usize,
) -> Option<usize> {
	loop {
		if let Some(defs) = state.scope_definition_map.get(&scope_id)
			&& let Some(found) = defs.iter().copied().max()
		{
			return Some(found);
		}
		let parent = state
			.semantic_index
			.scopes
			.get(scope_id)
			.and_then(|scope| scope.parent)?;
		scope_id = parent;
	}
}

pub(crate) fn dependency_hint_for_edge(
	state: &RuntimeState,
	caller_mod_id: &str,
	callee_mod_id: &str,
) -> (bool, DependencyMatchKind) {
	if caller_mod_id == callee_mod_id {
		return (true, DependencyMatchKind::None);
	}
	if state
		.base_game_mod_id
		.as_deref()
		.is_some_and(|base| callee_mod_id == base)
	{
		return (true, DependencyMatchKind::None);
	}
	let hint = state
		.dependency_hints
		.get(&(caller_mod_id.to_string(), callee_mod_id.to_string()))
		.copied()
		.unwrap_or(DependencyMatchKind::None);
	(hint != DependencyMatchKind::None, hint)
}

fn collect_workspace_scripts(
	workspace: &ResolvedWorkspace,
	enabled_mod_ids: &HashSet<String>,
	base_mod_id: Option<&str>,
) -> Vec<ParsedScriptFile> {
	let mut seen = HashSet::new();
	let mut parsed = Vec::new();
	for contributors in workspace.file_inventory.values() {
		for contributor in contributors {
			if !(enabled_mod_ids.contains(&contributor.mod_id)
				|| base_mod_id.is_some_and(|base| contributor.mod_id == base))
			{
				continue;
			}
			let key = format!(
				"{}::{}",
				contributor.mod_id,
				contributor.absolute_path.to_string_lossy()
			);
			if !seen.insert(key) || !looks_like_clausewitz_path(&contributor.absolute_path) {
				continue;
			}
			if let Some(file) = parse_script_file(
				&contributor.mod_id,
				&contributor.root_path,
				&contributor.absolute_path,
			) {
				parsed.push(file);
			}
		}
	}
	parsed.sort_by(|lhs, rhs| {
		(lhs.mod_id.as_str(), lhs.relative_path.as_os_str())
			.cmp(&(rhs.mod_id.as_str(), rhs.relative_path.as_os_str()))
	});
	parsed
}

fn build_precedence_map(
	workspace: &ResolvedWorkspace,
	base_mod_id: Option<&str>,
) -> HashMap<String, usize> {
	let mut precedence = HashMap::new();
	let mut next = 0usize;
	if let Some(base) = base_mod_id {
		precedence.insert(base.to_string(), next);
		next += 1;
	}
	let mut mods = workspace
		.mods
		.iter()
		.filter(|item| item.entry.enabled)
		.collect::<Vec<_>>();
	mods.sort_by_key(|item| item.entry.position.unwrap_or(usize::MAX));
	for mod_item in mods {
		precedence.insert(mod_item.mod_id.clone(), next);
		next += 1;
	}
	precedence
}

fn collect_definition_records(
	index: &crate::check::model::SemanticIndex,
	parsed_scripts: &[ParsedScriptFile],
	precedence_by_mod: &HashMap<String, usize>,
) -> Result<Vec<DefinitionRecord>, String> {
	let mut by_path = HashMap::<(String, String), &ParsedScriptFile>::new();
	for parsed in parsed_scripts {
		by_path.insert(
			(
				parsed.mod_id.clone(),
				normalize_path(parsed.relative_path.as_path()),
			),
			parsed,
		);
	}

	let mut definitions = Vec::new();
	for (idx, definition) in index.definitions.iter().enumerate() {
		let key = (
			definition.mod_id.clone(),
			normalize_path(Path::new(&definition.path)),
		);
		let Some(parsed) = by_path.get(&key) else {
			continue;
		};
		let Some(statement) =
			find_statement_by_position(&parsed.ast.statements, definition.line, definition.column)
		else {
			continue;
		};
		let normalized_statement = emit_clausewitz_statements(std::slice::from_ref(&statement))
			.map_err(|err| {
				format!(
					"failed to normalize {} {}:{}: {err}",
					definition.mod_id,
					definition.path.display(),
					definition.line
				)
			})?;
		definitions.push(DefinitionRecord {
			index: idx,
			kind: definition.kind,
			name: definition.name.clone(),
			local_name: definition.local_name.clone(),
			mod_id: definition.mod_id.clone(),
			path: normalize_path(Path::new(&definition.path)),
			line: definition.line,
			column: definition.column,
			precedence: precedence_by_mod
				.get(&definition.mod_id)
				.copied()
				.unwrap_or_default(),
			root_mergeable: is_merge_candidate_path(Path::new(&definition.path)),
			normalized_statement,
		});
	}
	Ok(definitions)
}

fn build_winner_lookup(definitions: &[DefinitionRecord]) -> HashMap<(SymbolKind, String), usize> {
	let mut grouped = HashMap::<(SymbolKind, String), usize>::new();
	for definition in definitions {
		let key = (definition.kind, definition.name.clone());
		match grouped.get(&key).copied() {
			Some(existing) => {
				let current = definitions
					.iter()
					.find(|item| item.index == existing)
					.expect("winner index should exist");
				if (definition.precedence, definition.index) > (current.precedence, current.index) {
					grouped.insert(key, definition.index);
				}
			}
			None => {
				grouped.insert(key, definition.index);
			}
		}
	}
	grouped
}

fn build_dependency_hints(
	workspace: &ResolvedWorkspace,
	base_mod_id: Option<&str>,
) -> HashMap<(String, String), DependencyMatchKind> {
	let mut id_lookup = HashMap::<String, String>::new();
	let mut name_lookup = HashMap::<String, String>::new();
	for mod_item in workspace.mods.iter().filter(|item| item.entry.enabled) {
		id_lookup.insert(mod_item.mod_id.clone(), mod_item.mod_id.clone());
		if let Some(descriptor) = mod_item.descriptor.as_ref() {
			name_lookup.insert(descriptor.name.clone(), mod_item.mod_id.clone());
		}
	}
	if let Some(base) = base_mod_id {
		id_lookup.insert(base.to_string(), base.to_string());
	}

	let mut dependency_hints = HashMap::new();
	for mod_item in workspace.mods.iter().filter(|item| item.entry.enabled) {
		let Some(descriptor) = mod_item.descriptor.as_ref() else {
			continue;
		};
		for dependency in &descriptor.dependencies {
			if let Some(target) = id_lookup.get(dependency) {
				dependency_hints.insert(
					(mod_item.mod_id.clone(), target.clone()),
					DependencyMatchKind::ModId,
				);
				continue;
			}
			if let Some(target) = name_lookup.get(dependency) {
				dependency_hints.insert(
					(mod_item.mod_id.clone(), target.clone()),
					DependencyMatchKind::DescriptorName,
				);
			}
		}
	}
	dependency_hints
}

fn pick_highest_precedence(state: &RuntimeState, candidates: Vec<usize>) -> Option<usize> {
	candidates.into_iter().max_by_key(|idx| {
		state
			.definitions
			.iter()
			.find(|definition| definition.index == *idx)
			.map(|definition| (definition.precedence, definition.index))
			.unwrap_or_default()
	})
}

fn find_statement_by_position(
	statements: &[AstStatement],
	line: usize,
	column: usize,
) -> Option<AstStatement> {
	for statement in statements {
		match statement {
			AstStatement::Assignment {
				key_span,
				value,
				span,
				..
			} => {
				if key_span.start.line == line && key_span.start.column == column {
					return Some(statement.clone());
				}
				if let AstValue::Block { items, .. } = value
					&& let Some(found) = find_statement_by_position(items, line, column)
				{
					return Some(found);
				}
				if value.span().start.line == line && value.span().start.column == column {
					return Some(statement.clone());
				}
				if span_contains(span, line, column) {
					return Some(statement.clone());
				}
			}
			AstStatement::Item { value, span } => {
				if let AstValue::Block { items, .. } = value
					&& let Some(found) = find_statement_by_position(items, line, column)
				{
					return Some(found);
				}
				if span_contains(span, line, column) {
					return Some(statement.clone());
				}
			}
			AstStatement::Comment { .. } => {}
		}
	}
	None
}

fn span_contains(span: &SpanRange, line: usize, column: usize) -> bool {
	let starts_before = (span.start.line, span.start.column) <= (line, column);
	let ends_after = (line, column) <= (span.end.line, span.end.column);
	starts_before && ends_after
}

fn looks_like_clausewitz_path(path: &Path) -> bool {
	matches!(
		path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()),
		Some(ext) if matches!(ext.as_str(), "txt" | "lua" | "gfx" | "gui" | "asset")
	)
}

fn is_merge_candidate_path(path: &Path) -> bool {
	let normalized = normalize_path(path).to_ascii_lowercase();
	normalized.starts_with("events/")
		|| normalized.starts_with("decisions/")
		|| normalized.starts_with("common/scripted_effects/")
		|| normalized.starts_with("common/diplomatic_actions/")
		|| normalized.starts_with("common/triggered_modifiers/")
		|| normalized.starts_with("common/defines/")
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}
