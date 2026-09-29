use crate::check::runtime::overlap::{OverlapStatus, classify_definition_overlaps};
use crate::game::eu4::base::snapshot::base_game_mod_id;
use crate::game::eu4::script::emit::emit_clausewitz_statements;
use crate::game::eu4::script::parser::{AstStatement, AstValue, SpanRange};
use crate::game::eu4::script::{
	ParsedScriptFile, resolve_scripted_effect_reference_targets,
	resolve_scripted_trigger_reference_targets,
};
use crate::input::request::InputRequest;
use crate::input::{InputResolveErrorKind, ResolvedInput, ResolvedInputContributor, resolve_input};
use crate::model::{GamePath, GamePathBuf, SemanticIndex, SymbolKind, SymbolReference};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
	pub precedence: usize,
	pub root_mergeable: bool,
	pub normalized_statement: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeState {
	pub semantic_index: SemanticIndex,
	pub(crate) definitions: Vec<DefinitionRecord>,
	pub(crate) overlap_status_by_def: HashMap<usize, OverlapStatus>,
	pub(crate) winner_by_symbol: HashMap<(SymbolKind, String), usize>,
	pub(crate) dependency_hints: HashMap<(String, String), DependencyMatchKind>,
	pub(crate) scope_definition_map: HashMap<usize, Vec<usize>>,
	pub(crate) enabled_mod_ids: HashSet<String>,
	pub(crate) base_game_mod_id: Option<String>,
}

pub(crate) fn build_runtime_state_for_request(
	request: &InputRequest,
	include_game_base: bool,
) -> Result<RuntimeState, String> {
	let input = resolve_input(request, include_game_base).map_err(|err| {
		if err.kind == InputResolveErrorKind::PlaylistFormat {
			"failed to parse Playset JSON".to_string()
		} else {
			err.message
		}
	})?;
	build_runtime_state_from_input(&input)
}

pub(crate) fn build_runtime_state_from_input(
	input: &ResolvedInput,
) -> Result<RuntimeState, String> {
	let enabled_mod_ids = input
		.mods
		.iter()
		.filter(|item| item.entry.enabled)
		.map(|item| item.mod_id.clone())
		.collect::<HashSet<_>>();
	let base_mod_id = input
		.installed_base_snapshot
		.as_ref()
		.map(|_| base_game_mod_id(input.playlist.game.key()));
	let semantic_index =
		collect_input_semantic_index(input, &enabled_mod_ids, base_mod_id.as_deref());
	let parsed_scripts = collect_input_scripts(
		input,
		&enabled_mod_ids,
		base_mod_id.as_deref(),
		&semantic_index,
	)?;
	let precedence_by_mod = build_precedence_map(input, base_mod_id.as_deref());
	let definitions =
		collect_definition_records(&semantic_index, &parsed_scripts, &precedence_by_mod)?;
	let overlap_status_by_def = classify_definition_overlaps(&definitions, base_mod_id.as_deref());
	let winner_by_symbol = build_winner_lookup(&definitions);
	let dependency_hints = build_dependency_hints(input, base_mod_id.as_deref());
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

fn collect_input_semantic_index(
	input: &ResolvedInput,
	enabled_mod_ids: &HashSet<String>,
	base_mod_id: Option<&str>,
) -> SemanticIndex {
	let mut merged = SemanticIndex::default();
	if let (Some(base), Some(installed)) = (base_mod_id, input.installed_base_snapshot.as_ref()) {
		let mut base_index = installed.snapshot.to_semantic_index();
		for document in &mut base_index.documents {
			document.mod_id = base.to_string();
		}
		merged = merge_semantic_indexes(merged, base_index);
	}
	for (mod_item, snapshot) in input.mods.iter().zip(input.mod_snapshots.iter()) {
		if !enabled_mod_ids.contains(&mod_item.mod_id) {
			continue;
		}
		let Some(snapshot) = snapshot.as_ref() else {
			continue;
		};
		merged = merge_semantic_indexes(merged, snapshot.semantic_index.clone());
	}
	merged
}

fn collect_input_scripts(
	input: &ResolvedInput,
	enabled_mod_ids: &HashSet<String>,
	base_mod_id: Option<&str>,
	semantic_index: &SemanticIndex,
) -> Result<Vec<Arc<ParsedScriptFile>>, String> {
	let required = semantic_index
		.definitions
		.iter()
		.map(|definition| (definition.mod_id.as_str(), definition.path.as_game_path()))
		.collect::<HashSet<_>>();
	let mut parsed = input
		.script_cache
		.documents_for_mods(enabled_mod_ids, base_mod_id)?;
	parsed.retain(|document| {
		required.contains(&(
			document.mod_id.as_str(),
			document.relative_path.as_game_path(),
		))
	});
	let loaded = parsed
		.iter()
		.map(|document| (document.mod_id.clone(), document.relative_path.clone()))
		.collect::<HashSet<_>>();

	let contributors_by_key = runtime_contributors_by_key(
		input.file_inventory.values().flatten(),
		enabled_mod_ids,
		base_mod_id,
	)?;
	let mut required = required.into_iter().collect::<Vec<_>>();
	required.sort();
	for (mod_id, relative_path) in required {
		let key = (mod_id.to_string(), relative_path.to_owned());
		if loaded.contains(&key) {
			continue;
		}
		let contributor = contributors_by_key
			.get(&key)
			.ok_or_else(|| format!("missing runtime AST contributor {mod_id}::{relative_path}"))?;
		if !looks_like_clausewitz_path(&contributor.relative_path) {
			return Err(format!(
				"runtime definition {mod_id}::{relative_path} is not a Clausewitz script"
			));
		}
		parsed.push(input.script_cache.load(contributor)?);
	}
	parsed.sort_by(|lhs, rhs| {
		(lhs.mod_id.as_str(), &lhs.relative_path).cmp(&(rhs.mod_id.as_str(), &rhs.relative_path))
	});
	Ok(parsed)
}

/// The enabled contributors keyed by mod id and game path, the key semantic
/// definitions name their files by. Two contributors under one key from
/// different roots (two playset entries sharing a mod id) are an error,
/// because the key cannot say which file a definition was read from. The
/// same file listed twice, as a synthetic base copies its seed, is one file.
fn runtime_contributors_by_key<'a>(
	contributors: impl IntoIterator<Item = &'a ResolvedInputContributor>,
	enabled_mod_ids: &HashSet<String>,
	base_mod_id: Option<&str>,
) -> Result<HashMap<(String, GamePathBuf), &'a ResolvedInputContributor>, String> {
	let mut contributors_by_key = HashMap::new();
	for contributor in contributors {
		if !(enabled_mod_ids.contains(&contributor.mod_id)
			|| base_mod_id.is_some_and(|base| contributor.mod_id == base))
		{
			continue;
		}
		let key = (
			contributor.mod_id.clone(),
			contributor.relative_path.clone(),
		);
		match contributors_by_key.entry(key) {
			Entry::Vacant(slot) => {
				slot.insert(contributor);
			}
			Entry::Occupied(slot) if slot.get().root_path != contributor.root_path => {
				let (mod_id, relative_path) = slot.key();
				return Err(format!(
					"runtime contributors {mod_id}::{relative_path} name two files: {} and {}",
					slot.get().absolute_path().display(),
					contributor.absolute_path().display()
				));
			}
			Entry::Occupied(_) => {}
		}
	}
	Ok(contributors_by_key)
}

fn merge_semantic_indexes(mut base: SemanticIndex, mut overlay: SemanticIndex) -> SemanticIndex {
	let offset = base.scopes.len();
	for scope in &mut overlay.scopes {
		scope.id += offset;
		if let Some(parent) = scope.parent {
			scope.parent = Some(parent + offset);
		}
	}
	for definition in &mut overlay.definitions {
		definition.scope_id += offset;
	}
	for reference in &mut overlay.references {
		reference.scope_id += offset;
	}
	for alias in &mut overlay.alias_usages {
		alias.scope_id += offset;
	}
	for usage in &mut overlay.key_usages {
		usage.scope_id += offset;
	}
	for assignment in &mut overlay.scalar_assignments {
		assignment.scope_id += offset;
	}

	base.scopes.extend(overlay.scopes);
	base.definitions.extend(overlay.definitions);
	base.references.extend(overlay.references);
	base.alias_usages.extend(overlay.alias_usages);
	base.key_usages.extend(overlay.key_usages);
	base.scalar_assignments.extend(overlay.scalar_assignments);
	base.documents.extend(overlay.documents);
	base.localisation_definitions
		.extend(overlay.localisation_definitions);
	base.localisation_duplicates
		.extend(overlay.localisation_duplicates);
	base.ui_definitions.extend(overlay.ui_definitions);
	base.resource_references.extend(overlay.resource_references);
	base.csv_rows.extend(overlay.csv_rows);
	base.json_properties.extend(overlay.json_properties);
	base.parse_issues.extend(overlay.parse_issues);
	base
}

fn build_precedence_map(
	input: &ResolvedInput,
	base_mod_id: Option<&str>,
) -> HashMap<String, usize> {
	let mut precedence = HashMap::new();
	let mut next = 0usize;
	if let Some(base) = base_mod_id {
		precedence.insert(base.to_string(), next);
		next += 1;
	}
	let mut mods = input
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
	index: &SemanticIndex,
	parsed_scripts: &[Arc<ParsedScriptFile>],
	precedence_by_mod: &HashMap<String, usize>,
) -> Result<Vec<DefinitionRecord>, String> {
	let mut by_path = HashMap::<(&str, &GamePath), &ParsedScriptFile>::new();
	for parsed in parsed_scripts {
		by_path.insert(
			(parsed.mod_id.as_str(), parsed.relative_path.as_game_path()),
			parsed.as_ref(),
		);
	}

	let mut definitions = Vec::new();
	for (idx, definition) in index.definitions.iter().enumerate() {
		let key = (definition.mod_id.as_str(), definition.path.as_game_path());
		let parsed = by_path.get(&key).ok_or_else(|| {
			format!(
				"missing verified AST for runtime definition {} {}:{}",
				definition.mod_id, definition.path, definition.line
			)
		})?;
		let Some(statement) =
			find_statement_by_position(&parsed.ast.statements, definition.line, definition.column)
		else {
			return Err(format!(
				"runtime definition position is absent from verified AST for {} {}:{}:{}",
				definition.mod_id, definition.path, definition.line, definition.column
			));
		};
		let normalized_statement = emit_clausewitz_statements(std::slice::from_ref(&statement))
			.map_err(|err| {
				format!(
					"failed to normalize {} {}:{}: {err}",
					definition.mod_id, definition.path, definition.line
				)
			})?;
		definitions.push(DefinitionRecord {
			index: idx,
			kind: definition.kind,
			name: definition.name.clone(),
			local_name: definition.local_name.clone(),
			mod_id: definition.mod_id.clone(),
			path: definition.path.clone(),
			line: definition.line,
			column: definition.column,
			precedence: precedence_by_mod
				.get(&definition.mod_id)
				.copied()
				.unwrap_or_default(),
			root_mergeable: is_merge_candidate_path(&definition.path),
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
	input: &ResolvedInput,
	base_mod_id: Option<&str>,
) -> HashMap<(String, String), DependencyMatchKind> {
	let mut id_lookup = HashMap::<String, String>::new();
	let mut name_lookup = HashMap::<String, String>::new();
	for mod_item in input.mods.iter().filter(|item| item.entry.enabled) {
		id_lookup.insert(mod_item.mod_id.clone(), mod_item.mod_id.clone());
		if let Some(descriptor) = mod_item.descriptor.as_ref() {
			name_lookup.insert(descriptor.name.clone(), mod_item.mod_id.clone());
		}
	}
	if let Some(base) = base_mod_id {
		id_lookup.insert(base.to_string(), base.to_string());
	}

	let mut dependency_hints = HashMap::new();
	for mod_item in input.mods.iter().filter(|item| item.entry.enabled) {
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

fn looks_like_clausewitz_path(path: &GamePath) -> bool {
	matches!(
		path.extension().map(str::to_ascii_lowercase),
		Some(ext) if matches!(ext.as_str(), "txt" | "lua" | "gfx" | "gui" | "asset")
	)
}

/// Directories whose definitions merge structurally. Names compare ignoring
/// ASCII case.
const MERGE_CANDIDATE_DIRECTORIES: [&[&str]; 6] = [
	&["events"],
	&["decisions"],
	&["common", "scripted_effects"],
	&["common", "diplomatic_actions"],
	&["common", "triggered_modifiers"],
	&["common", "defines"],
];

fn is_merge_candidate_path(path: &GamePath) -> bool {
	MERGE_CANDIDATE_DIRECTORIES
		.iter()
		.any(|directory| path.is_inside(directory, str::eq_ignore_ascii_case))
}

#[cfg(test)]
mod tests {
	use super::{is_merge_candidate_path, runtime_contributors_by_key};
	use crate::input::ResolvedInputContributor;
	use crate::model::{GamePath, GamePathBuf};
	use std::collections::HashSet;

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("valid game path")
	}

	fn contributor(mod_id: &str, root: &str, relative: &str) -> ResolvedInputContributor {
		ResolvedInputContributor {
			mod_id: mod_id.to_string(),
			root_path: std::env::temp_dir().join(root),
			relative_path: game_path(relative),
			precedence: 1,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: None,
			mod_hash: None,
		}
	}

	#[test]
	fn contributor_keys_are_the_game_paths_definitions_carry() {
		let effects = contributor("a", "mod-a", "common/scripted_effects/x.txt");
		let disabled = contributor("b", "mod-b", "common/scripted_effects/x.txt");
		let enabled = HashSet::from(["a".to_string()]);

		let keys = runtime_contributors_by_key([&effects, &disabled], &enabled, None)
			.expect("one contributor per key");

		// A semantic definition names its file by mod id and game path.
		let key = ("a".to_string(), game_path("common/scripted_effects/x.txt"));
		assert_eq!(
			keys.get(&key).map(|found| found.absolute_path()),
			Some(effects.absolute_path())
		);
		assert_eq!(keys.len(), 1, "a disabled mod contributes no key");
	}

	/// The lossy key this replaced was taken from the physical file, so a
	/// literal backslash folded into a separator and `scripted_effects\x.txt`
	/// collided with the nested `scripted_effects/x.txt`. A contributor now
	/// carries the game path the inventory walker gave it; a name without one
	/// never becomes a contributor, and the physical spelling of the root
	/// never reaches the key.
	#[cfg(unix)]
	#[test]
	fn contributors_are_keyed_by_their_game_path_not_their_physical_spelling() {
		assert!(GamePathBuf::parse(r"common/scripted_effects\x.txt").is_err());
		let nested = contributor("a", r"mod\a", "common/scripted_effects/x.txt");
		let enabled = HashSet::from(["a".to_string()]);

		let keys = runtime_contributors_by_key([&nested], &enabled, None)
			.expect("one contributor per key");

		let key = ("a".to_string(), game_path("common/scripted_effects/x.txt"));
		assert_eq!(
			keys.get(&key).map(|found| found.absolute_path()),
			Some(nested.absolute_path())
		);
		assert!(nested.absolute_path().starts_with(&nested.root_path));
	}

	#[test]
	fn two_files_under_one_mod_id_and_game_path_are_an_error() {
		let first = contributor("a", "mod-a", "common/scripted_effects/x.txt");
		let second = contributor("a", "mod-a-copy", "common/scripted_effects/x.txt");
		let enabled = HashSet::from(["a".to_string()]);

		let error = runtime_contributors_by_key([&first, &second], &enabled, None)
			.expect_err("one key cannot name two files");
		for expected in [
			"a::common/scripted_effects/x.txt".to_string(),
			first.absolute_path().display().to_string(),
			second.absolute_path().display().to_string(),
		] {
			assert!(error.contains(&expected), "{expected} not in {error}");
		}
	}

	#[test]
	fn the_same_file_listed_twice_is_one_contributor() {
		let seed = contributor("a", "mod-a", "common/scripted_effects/x.txt");
		let mut synthetic_base = seed.clone();
		synthetic_base.is_synthetic_base = true;
		synthetic_base.precedence = 0;
		let enabled = HashSet::from(["a".to_string()]);

		let keys = runtime_contributors_by_key([&synthetic_base, &seed], &enabled, None)
			.expect("a synthetic base names its seed's file");
		assert_eq!(keys.len(), 1);
	}

	#[test]
	fn merge_candidate_directories_match_whole_components_ignoring_ascii_case() {
		for (path, candidate) in [
			("events/x.txt", true),
			("Events/x.txt", true),
			("common/scripted_effects/x.txt", true),
			("COMMON/Defines/x.lua", true),
			("eventsx/x.txt", false),
			("events", false),
			("common/scripted_effects_extra/x.txt", false),
			("common/ideas/x.txt", false),
		] {
			assert_eq!(
				is_merge_candidate_path(GamePath::new(path).expect("valid game path")),
				candidate,
				"{path}"
			);
		}
	}
}
