//! Fail-closed workspace rename for mod-authored EU4 symbols.
//!
//! Indexed locations point at a 1-based byte column and, for values such as
//! event ids and flags, at the assignment key rather than the identifier. A
//! rename is emitted only after every occurrence is re-read and the bytes at
//! that span are exactly the old name.

use super::{
	OpenDocument, ScanTarget, TargetRole, WorkspaceSnapshot, assignment_context_at_cursor,
	extract_token_at_cursor, flag_value_kind, is_flag_definition_key, is_identifier_char,
	is_localisation_reference_key, match_scan_target, scan_target_mod_id,
	symbol_target_indices_at_cursor,
};
use foch::game::eu4::script::resolve_symbol_reference_targets;
use foch::model::{GamePath, SymbolDefinition, SymbolKind as FochSymbolKind};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use tower_lsp::jsonrpc::{self, ErrorCode};
use tower_lsp::lsp_types::{Position, Range, TextEdit, Url, WorkspaceEdit};

pub(super) const SAVE_BEFORE_RENAME: &str = "save open files before renaming across the workspace";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RenameBlock {
	NoWorkspace,
	NotRenameable,
	BaseGame,
	OutsideMod,
	Stale,
	UnverifiedSpan,
	InvalidName,
	Unchanged,
	Collision,
}

#[derive(Clone, Debug)]
pub(super) struct PlannedRename {
	pub(super) old_name: String,
	pub(super) cursor: Range,
	pub(super) edit: WorkspaceEdit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RenameClass {
	Key(FochSymbolKind),
	Event,
	Flag(&'static str),
	Localisation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpanStyle {
	/// The indexed column is the first byte of the identifier.
	/// `colon_boundary` is set for localisation keys, which are followed by `:`.
	Key { colon_boundary: bool },
	/// The indexed column is the assignment key; the identifier is the value.
	Value,
}

#[derive(Clone, Debug)]
struct Site {
	path: PathBuf,
	line: usize,
	column: usize,
	style: SpanStyle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Occurrence {
	path: PathBuf,
	range: Range,
}

enum HashCheck {
	Match,
	Drift,
	Missing,
}

pub(super) fn open_buffers(docs: &HashMap<Url, OpenDocument>) -> HashMap<PathBuf, Option<String>> {
	let mut opens = HashMap::new();
	for (uri, document) in docs {
		if let Ok(path) = uri.to_file_path() {
			opens.insert(path, document.text.clone());
		}
	}
	opens
}

pub(super) fn rename_error(block: RenameBlock, new_name: &str) -> jsonrpc::Error {
	match block {
		RenameBlock::Stale => request_failed(SAVE_BEFORE_RENAME),
		RenameBlock::BaseGame => request_failed("base-game symbols cannot be renamed"),
		RenameBlock::OutsideMod => request_failed("symbols outside mod roots cannot be renamed"),
		RenameBlock::UnverifiedSpan => request_failed(
			"rename refused because an occurrence does not match the symbol name exactly",
		),
		RenameBlock::InvalidName => jsonrpc::Error::invalid_params(
			"new name must be a non-empty EU4 identifier ([A-Za-z_][A-Za-z0-9_.-]*) without spaces, quotes, braces, or '='",
		),
		RenameBlock::Unchanged => {
			jsonrpc::Error::invalid_params("new name matches the current name")
		}
		RenameBlock::Collision => request_failed(format!(
			"`{new_name}` already exists and cannot be renamed over"
		)),
		RenameBlock::NoWorkspace => jsonrpc::Error::invalid_params("workspace is not indexed"),
		RenameBlock::NotRenameable => {
			jsonrpc::Error::invalid_params("this symbol cannot be renamed")
		}
	}
}

pub(super) fn plan_symbol_rename(
	snapshot: &WorkspaceSnapshot,
	targets: &[ScanTarget],
	opens: &HashMap<PathBuf, Option<String>>,
	path: &Path,
	position: Position,
	new_name: Option<&str>,
) -> Result<PlannedRename, RenameBlock> {
	if snapshot.session.is_none() {
		return Err(RenameBlock::NoWorkspace);
	}
	let text = load_text(snapshot, opens, path)?;
	let (class, old_name) = classify(snapshot, targets, path, &text, position)?;
	let sites = collect_sites(snapshot, targets, class, &old_name)?;
	let mut cache = HashMap::<PathBuf, String>::new();
	cache.insert(path.to_path_buf(), text);
	let occurrences = verify_sites(snapshot, opens, &sites, &old_name, &mut cache)?;
	let cursor =
		cursor_occurrence(&occurrences, path, position).ok_or(RenameBlock::UnverifiedSpan)?;
	let edit = match new_name {
		None => WorkspaceEdit::default(),
		Some(new_name) => {
			validate_new_name(snapshot, class, &old_name, new_name)?;
			workspace_edit(&occurrences, new_name)?
		}
	};
	Ok(PlannedRename {
		old_name,
		cursor,
		edit,
	})
}

fn request_failed(message: impl Into<std::borrow::Cow<'static, str>>) -> jsonrpc::Error {
	jsonrpc::Error {
		code: ErrorCode::ServerError(-32803),
		message: message.into(),
		data: None,
	}
}

fn classify(
	snapshot: &WorkspaceSnapshot,
	targets: &[ScanTarget],
	path: &Path,
	text: &str,
	position: Position,
) -> Result<(RenameClass, String), RenameBlock> {
	let session = snapshot.session.as_ref().ok_or(RenameBlock::NoWorkspace)?;
	let line = nth_line(text, position.line as usize).ok_or(RenameBlock::NotRenameable)?;
	let cursor = position.character as usize;
	let (token, _, _) = extract_token_at_cursor(line, cursor).ok_or(RenameBlock::NotRenameable)?;
	let assignment = assignment_context_at_cursor(line, cursor);
	if let Some((key, key_start, _, true)) = assignment.as_ref() {
		if let Some(flag_kind) = flag_value_kind(key)
			&& flag_defined(session, flag_kind, &token)
		{
			return Ok((RenameClass::Flag(flag_kind), token));
		}
		if is_event_value(
			snapshot,
			targets,
			path,
			position,
			key,
			key_start + 1,
			&token,
		) {
			return Ok((RenameClass::Event, token));
		}
		if is_localisation_reference_key(key, &token) && localisation_defined(session, &token) {
			return Ok((RenameClass::Localisation, token));
		}
		return Err(RenameBlock::NotRenameable);
	}

	if let Some(name) = localisation_key_at(snapshot, targets, path, position, &token) {
		return Ok((RenameClass::Localisation, name));
	}

	let uri = Url::from_file_path(path).map_err(|_| RenameBlock::NotRenameable)?;
	let (target, Ok(relative_path)) =
		match_scan_target(targets, path).ok_or(RenameBlock::NotRenameable)?
	else {
		return Err(RenameBlock::NotRenameable);
	};
	let mod_id = scan_target_mod_id(
		targets
			.iter()
			.position(|candidate| std::ptr::eq(candidate, target))
			.ok_or(RenameBlock::NotRenameable)?,
		target,
	);
	let Some(indices) = symbol_target_indices_at_cursor(
		session,
		&uri,
		&mod_id,
		&relative_path,
		position,
		&token,
		assignment.as_ref(),
	) else {
		return Err(RenameBlock::NotRenameable);
	};
	let mut kind = None;
	let mut name = None;
	for index in indices {
		let Some(definition) = session.index.definitions.get(index) else {
			return Err(RenameBlock::NotRenameable);
		};
		if !supported_key_kind(definition.kind) || definition.local_name != token {
			return Err(RenameBlock::NotRenameable);
		}
		if kind.get_or_insert(definition.kind) != &definition.kind {
			return Err(RenameBlock::NotRenameable);
		}
		if *name.get_or_insert(definition.local_name.as_str()) != definition.local_name.as_str() {
			return Err(RenameBlock::NotRenameable);
		}
	}
	match (kind, name) {
		(Some(kind), Some(name)) => Ok((RenameClass::Key(kind), name.to_string())),
		_ => Err(RenameBlock::NotRenameable),
	}
}

fn supported_key_kind(kind: FochSymbolKind) -> bool {
	matches!(
		kind,
		FochSymbolKind::ScriptedEffect
			| FochSymbolKind::ScriptedTrigger
			| FochSymbolKind::Decision
			| FochSymbolKind::DiplomaticAction
			| FochSymbolKind::TriggeredModifier
	)
}

fn is_event_value(
	snapshot: &WorkspaceSnapshot,
	targets: &[ScanTarget],
	path: &Path,
	position: Position,
	key: &str,
	key_column: usize,
	token: &str,
) -> bool {
	let Some(session) = snapshot.session.as_ref() else {
		return false;
	};
	if !event_defined_exactly(session, token) {
		return false;
	}
	if key == "id" {
		return true;
	}
	let Some((target, Ok(relative))) = match_scan_target(targets, path) else {
		return false;
	};
	let Some(ordinal) = targets
		.iter()
		.position(|candidate| std::ptr::eq(candidate, target))
	else {
		return false;
	};
	let mod_id = scan_target_mod_id(ordinal, target);
	let line = position.line as usize + 1;
	session.index.references.iter().any(|reference| {
		reference.kind == FochSymbolKind::Event
			&& reference.name == token
			&& reference.mod_id == mod_id
			&& *reference.path == *relative
			&& reference.line == line
			&& reference.column == key_column
	})
}

fn event_defined_exactly(
	session: &foch::game::eu4::editor::workspace::WorkspaceSession,
	name: &str,
) -> bool {
	session
		.index
		.definitions
		.iter()
		.any(|definition| definition.kind == FochSymbolKind::Event && definition.name == name)
}

fn flag_defined(
	session: &foch::game::eu4::editor::workspace::WorkspaceSession,
	kind: &str,
	name: &str,
) -> bool {
	session.index.scalar_assignments.iter().any(|usage| {
		usage.value == name
			&& flag_value_kind(usage.key.as_str()) == Some(kind)
			&& is_flag_definition_key(usage.key.as_str())
	})
}

fn localisation_defined(
	session: &foch::game::eu4::editor::workspace::WorkspaceSession,
	name: &str,
) -> bool {
	session
		.index
		.localisation_definitions
		.iter()
		.any(|definition| definition.key == name)
}

fn localisation_key_at(
	snapshot: &WorkspaceSnapshot,
	targets: &[ScanTarget],
	path: &Path,
	position: Position,
	token: &str,
) -> Option<String> {
	let session = snapshot.session.as_ref()?;
	let (target, Ok(relative)) = match_scan_target(targets, path)? else {
		return None;
	};
	let ordinal = targets
		.iter()
		.position(|candidate| std::ptr::eq(candidate, target))?;
	let mod_id = scan_target_mod_id(ordinal, target);
	let line = position.line as usize + 1;
	session
		.index
		.localisation_definitions
		.iter()
		.find(|definition| {
			localisation_token_matches(&definition.key, token)
				&& definition.mod_id == mod_id
				&& definition.path == relative
				&& definition.line == line
				&& column_covers(definition.column, &definition.key, position.character)
		})
		.map(|definition| definition.key.clone())
}

/// A localisation file spells `KEY:0`. The cursor token includes the version
/// suffix because `:` is an identifier character; the key itself does not.
fn localisation_token_matches(key: &str, token: &str) -> bool {
	token == key
		|| token
			.strip_prefix(key)
			.is_some_and(|rest| rest.starts_with(':'))
}

fn column_covers(column: usize, name: &str, character: u32) -> bool {
	let start = u32::try_from(column.saturating_sub(1)).unwrap_or(u32::MAX);
	let end = start.saturating_add(u32::try_from(name.len()).unwrap_or(u32::MAX));
	character >= start && character <= end
}

fn collect_sites(
	snapshot: &WorkspaceSnapshot,
	targets: &[ScanTarget],
	class: RenameClass,
	old_name: &str,
) -> Result<Vec<Site>, RenameBlock> {
	let session = snapshot.session.as_ref().ok_or(RenameBlock::NoWorkspace)?;
	match class {
		RenameClass::Key(kind) => collect_key_sites(session, targets, kind, old_name),
		RenameClass::Event => collect_event_sites(session, targets, old_name),
		RenameClass::Flag(kind) => collect_flag_sites(session, targets, kind, old_name),
		RenameClass::Localisation => collect_localisation_sites(session, targets, old_name),
	}
}

fn collect_key_sites(
	session: &foch::game::eu4::editor::workspace::WorkspaceSession,
	targets: &[ScanTarget],
	kind: FochSymbolKind,
	old_name: &str,
) -> Result<Vec<Site>, RenameBlock> {
	let mut definitions = HashSet::new();
	let mut sites = Vec::new();
	for (index, definition) in session.index.definitions.iter().enumerate() {
		if definition.kind != kind || definition.local_name != old_name {
			continue;
		}
		definitions.insert(index);
		sites.push(site_for(
			session,
			targets,
			&definition.mod_id,
			&definition.path,
			definition.line,
			definition.column,
			SpanStyle::Key {
				colon_boundary: false,
			},
		)?);
	}
	if definitions.is_empty() {
		return Err(RenameBlock::NotRenameable);
	}
	for reference in &session.index.references {
		if reference.kind != kind {
			continue;
		}
		let resolved = resolve_symbol_reference_targets(&session.index, reference);
		let hits = resolved.iter().any(|index| definitions.contains(index));
		if reference.name != old_name {
			if hits {
				return Err(RenameBlock::NotRenameable);
			}
			continue;
		}
		sites.push(site_for(
			session,
			targets,
			&reference.mod_id,
			&reference.path,
			reference.line,
			reference.column,
			SpanStyle::Key {
				colon_boundary: false,
			},
		)?);
	}
	Ok(sites)
}

fn collect_event_sites(
	session: &foch::game::eu4::editor::workspace::WorkspaceSession,
	targets: &[ScanTarget],
	old_name: &str,
) -> Result<Vec<Site>, RenameBlock> {
	let mut definitions = HashSet::new();
	let mut definition_scopes = HashSet::new();
	for (index, definition) in session.index.definitions.iter().enumerate() {
		if definition.kind != FochSymbolKind::Event || definition.name != old_name {
			continue;
		}
		// The stored column is the block brace, not the id token. The id
		// scalar in that scope is the occurrence the definition contributes.
		require_mod_file(session, targets, &definition.mod_id, &definition.path)?;
		definitions.insert(index);
		definition_scopes.insert(definition.scope_id);
	}
	if definitions.is_empty() {
		return Err(RenameBlock::NotRenameable);
	}
	let mut sites = Vec::new();
	let mut covered_scopes = HashSet::new();
	for usage in &session.index.scalar_assignments {
		if usage.key != "id"
			|| usage.value != old_name
			|| !definition_scopes.contains(&usage.scope_id)
		{
			continue;
		}
		covered_scopes.insert(usage.scope_id);
		sites.push(site_for(
			session,
			targets,
			&usage.mod_id,
			&usage.path,
			usage.line,
			usage.column,
			SpanStyle::Value,
		)?);
	}
	if covered_scopes != definition_scopes {
		return Err(RenameBlock::UnverifiedSpan);
	}
	for reference in &session.index.references {
		if reference.kind != FochSymbolKind::Event {
			continue;
		}
		let resolved = resolve_symbol_reference_targets(&session.index, reference);
		let hits = resolved.iter().any(|index| definitions.contains(index));
		if reference.name != old_name && !hits {
			continue;
		}
		if reference.name != old_name {
			return Err(RenameBlock::NotRenameable);
		}
		let resolved_set: HashSet<usize> = resolved.into_iter().collect();
		if !resolved_set.is_empty() && resolved_set != definitions {
			return Err(RenameBlock::NotRenameable);
		}
		sites.push(site_for(
			session,
			targets,
			&reference.mod_id,
			&reference.path,
			reference.line,
			reference.column,
			SpanStyle::Value,
		)?);
	}
	if sites.is_empty() {
		return Err(RenameBlock::UnverifiedSpan);
	}
	Ok(sites)
}

fn collect_flag_sites(
	session: &foch::game::eu4::editor::workspace::WorkspaceSession,
	targets: &[ScanTarget],
	kind: &str,
	old_name: &str,
) -> Result<Vec<Site>, RenameBlock> {
	let mut sites = Vec::new();
	let mut saw_setter = false;
	for usage in &session.index.scalar_assignments {
		if usage.value != old_name || flag_value_kind(usage.key.as_str()) != Some(kind) {
			continue;
		}
		if is_flag_definition_key(usage.key.as_str()) {
			saw_setter = true;
		}
		sites.push(site_for(
			session,
			targets,
			&usage.mod_id,
			&usage.path,
			usage.line,
			usage.column,
			SpanStyle::Value,
		)?);
	}
	if sites.is_empty() || !saw_setter {
		return Err(RenameBlock::NotRenameable);
	}
	Ok(sites)
}

fn collect_localisation_sites(
	session: &foch::game::eu4::editor::workspace::WorkspaceSession,
	targets: &[ScanTarget],
	old_name: &str,
) -> Result<Vec<Site>, RenameBlock> {
	let mut sites = Vec::new();
	let mut saw_definition = false;
	for definition in &session.index.localisation_definitions {
		if definition.key != old_name {
			continue;
		}
		saw_definition = true;
		sites.push(site_for(
			session,
			targets,
			&definition.mod_id,
			&definition.path,
			definition.line,
			definition.column,
			SpanStyle::Key {
				colon_boundary: true,
			},
		)?);
	}
	if !saw_definition {
		return Err(RenameBlock::NotRenameable);
	}
	for usage in &session.index.scalar_assignments {
		if usage.value != old_name || !is_localisation_reference_key(&usage.key, old_name) {
			continue;
		}
		sites.push(site_for(
			session,
			targets,
			&usage.mod_id,
			&usage.path,
			usage.line,
			usage.column,
			SpanStyle::Value,
		)?);
	}
	for reference in &session.index.resource_references {
		if reference.key != "localisation_reference" || reference.value != old_name {
			continue;
		}
		sites.push(site_for(
			session,
			targets,
			&reference.mod_id,
			&reference.path,
			reference.line,
			reference.column,
			SpanStyle::Value,
		)?);
	}
	Ok(sites)
}

fn site_for(
	session: &foch::game::eu4::editor::workspace::WorkspaceSession,
	targets: &[ScanTarget],
	mod_id: &str,
	relative: &GamePath,
	line: usize,
	column: usize,
	style: SpanStyle,
) -> Result<Site, RenameBlock> {
	if line == 0 || column == 0 {
		return Err(RenameBlock::UnverifiedSpan);
	}
	let path = require_mod_file(session, targets, mod_id, relative)?;
	Ok(Site {
		path,
		line,
		column,
		style,
	})
}

fn require_mod_file(
	session: &foch::game::eu4::editor::workspace::WorkspaceSession,
	targets: &[ScanTarget],
	mod_id: &str,
	relative: &GamePath,
) -> Result<PathBuf, RenameBlock> {
	let path = session
		.resolve_path(mod_id, relative)
		.ok_or(RenameBlock::UnverifiedSpan)?
		.to_path_buf();
	match match_scan_target(targets, &path) {
		Some((target, Ok(_))) if target.role == TargetRole::Mod => Ok(path),
		Some((target, _)) if target.role == TargetRole::Game => Err(RenameBlock::BaseGame),
		_ => Err(RenameBlock::OutsideMod),
	}
}

fn verify_sites(
	snapshot: &WorkspaceSnapshot,
	opens: &HashMap<PathBuf, Option<String>>,
	sites: &[Site],
	old_name: &str,
	cache: &mut HashMap<PathBuf, String>,
) -> Result<Vec<Occurrence>, RenameBlock> {
	let mut occurrences = Vec::new();
	let mut seen = BTreeSet::new();
	for site in sites {
		let text = cached_text(snapshot, opens, &site.path, cache)?;
		let line = nth_line(&text, site.line - 1).ok_or(RenameBlock::UnverifiedSpan)?;
		let (start, end) = match site.style {
			SpanStyle::Key { colon_boundary } => {
				identifier_at(line, site.column - 1, old_name, colon_boundary)
			}
			SpanStyle::Value => value_identifier_at(line, site.column - 1, old_name),
		}
		.ok_or(RenameBlock::UnverifiedSpan)?;
		let range = range_in_line((site.line - 1) as u32, line, start, end)
			.ok_or(RenameBlock::UnverifiedSpan)?;
		let key = (
			site.path.to_string_lossy().into_owned(),
			range.start.line,
			range.start.character,
			range.end.character,
		);
		if seen.insert(key) {
			occurrences.push(Occurrence {
				path: site.path.clone(),
				range,
			});
		}
	}
	if occurrences.is_empty() || ranges_overlap(&occurrences) {
		return Err(RenameBlock::UnverifiedSpan);
	}
	Ok(occurrences)
}

fn ranges_overlap(occurrences: &[Occurrence]) -> bool {
	let mut by_file: BTreeMap<&Path, Vec<&Range>> = BTreeMap::new();
	for occurrence in occurrences {
		by_file
			.entry(&occurrence.path)
			.or_default()
			.push(&occurrence.range);
	}
	for ranges in by_file.values_mut() {
		ranges.sort_by_key(|range| (range.start.line, range.start.character, range.end.character));
		for pair in ranges.windows(2) {
			if range_starts_before(pair[1], &pair[0].end) {
				return true;
			}
		}
	}
	false
}

fn range_starts_before(range: &Range, position: &Position) -> bool {
	range.start.line < position.line
		|| (range.start.line == position.line && range.start.character < position.character)
}

fn identifier_at(
	line: &str,
	start_byte: usize,
	name: &str,
	colon_boundary: bool,
) -> Option<(usize, usize)> {
	let end = start_byte.checked_add(name.len())?;
	if line.get(start_byte..end)? != name {
		return None;
	}
	if start_byte > 0 {
		let previous = line.get(..start_byte)?.chars().next_back()?;
		if is_identifier_char(previous) {
			return None;
		}
	}
	if let Some(next) = line.get(end..)?.chars().next()
		&& is_identifier_char(next)
		&& !(colon_boundary && next == ':')
	{
		return None;
	}
	Some((start_byte, end))
}

fn value_identifier_at(line: &str, key_byte: usize, name: &str) -> Option<(usize, usize)> {
	let bytes = line.as_bytes();
	if key_byte >= bytes.len() || !is_identifier_byte(bytes[key_byte]) {
		return None;
	}
	let mut index = key_byte;
	while index < bytes.len() && is_identifier_byte(bytes[index]) {
		index += 1;
	}
	while index < bytes.len() && bytes[index].is_ascii_whitespace() {
		index += 1;
	}
	if bytes.get(index) != Some(&b'=') {
		return None;
	}
	index += 1;
	while index < bytes.len() && bytes[index].is_ascii_whitespace() {
		index += 1;
	}
	identifier_at(line, index, name, false)
}

fn is_identifier_byte(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'$' | b'@' | b'-')
}

fn range_in_line(line_index: u32, line: &str, start_byte: usize, end_byte: usize) -> Option<Range> {
	let start = utf16_len(line.get(..start_byte)?)?;
	let end = start.checked_add(utf16_len(line.get(start_byte..end_byte)?)?)?;
	Some(Range {
		start: Position {
			line: line_index,
			character: start,
		},
		end: Position {
			line: line_index,
			character: end,
		},
	})
}

fn utf16_len(text: &str) -> Option<u32> {
	u32::try_from(text.encode_utf16().count()).ok()
}

fn nth_line(text: &str, line_index: usize) -> Option<&str> {
	let line = text.split('\n').nth(line_index)?;
	Some(line.strip_suffix('\r').unwrap_or(line))
}

fn cursor_occurrence(occurrences: &[Occurrence], path: &Path, position: Position) -> Option<Range> {
	let mut found = None;
	for occurrence in occurrences {
		if same_file(&occurrence.path, path) && position_in_range(position, &occurrence.range) {
			if found.is_some() {
				return None;
			}
			found = Some(occurrence.range);
		}
	}
	found
}

fn position_in_range(position: Position, range: &Range) -> bool {
	at_or_after(position, range.start) && at_or_before(position, range.end)
}

fn at_or_after(position: Position, start: Position) -> bool {
	position.line > start.line
		|| (position.line == start.line && position.character >= start.character)
}

fn at_or_before(position: Position, end: Position) -> bool {
	position.line < end.line || (position.line == end.line && position.character <= end.character)
}

fn same_file(left: &Path, right: &Path) -> bool {
	if left == right {
		return true;
	}
	match (left.canonicalize(), right.canonicalize()) {
		(Ok(left), Ok(right)) => left == right,
		_ => false,
	}
}

fn validate_new_name(
	snapshot: &WorkspaceSnapshot,
	class: RenameClass,
	old_name: &str,
	new_name: &str,
) -> Result<(), RenameBlock> {
	if new_name == old_name {
		return Err(RenameBlock::Unchanged);
	}
	if !is_valid_new_name(new_name) {
		return Err(RenameBlock::InvalidName);
	}
	let session = snapshot.session.as_ref().ok_or(RenameBlock::NoWorkspace)?;
	let collides = match class {
		RenameClass::Key(kind) => session
			.index
			.definitions
			.iter()
			.any(|definition| definition.kind == kind && symbol_key(definition) == new_name),
		RenameClass::Event => session.index.definitions.iter().any(|definition| {
			definition.kind == FochSymbolKind::Event && definition.name == new_name
		}),
		RenameClass::Flag(kind) => session.index.scalar_assignments.iter().any(|usage| {
			usage.value == new_name && flag_value_kind(usage.key.as_str()) == Some(kind)
		}),
		RenameClass::Localisation => session
			.index
			.localisation_definitions
			.iter()
			.any(|definition| definition.key == new_name),
	};
	if collides {
		Err(RenameBlock::Collision)
	} else {
		Ok(())
	}
}

fn symbol_key(definition: &SymbolDefinition) -> &str {
	if definition.kind == FochSymbolKind::Event {
		definition.name.as_str()
	} else {
		definition.local_name.as_str()
	}
}

fn is_valid_new_name(name: &str) -> bool {
	let mut chars = name.chars();
	let Some(first) = chars.next() else {
		return false;
	};
	matches!(first, 'A'..='Z' | 'a'..='z' | '_')
		&& chars.all(|ch| matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '.' | '-'))
}

fn workspace_edit(
	occurrences: &[Occurrence],
	new_name: &str,
) -> Result<WorkspaceEdit, RenameBlock> {
	let mut ordered: BTreeMap<String, (Url, Vec<TextEdit>)> = BTreeMap::new();
	for occurrence in occurrences {
		let uri = Url::from_file_path(&occurrence.path).map_err(|_| RenameBlock::UnverifiedSpan)?;
		let key = uri.to_string();
		let (_, edits) = ordered.entry(key).or_insert_with(|| (uri, Vec::new()));
		edits.push(TextEdit {
			range: occurrence.range,
			new_text: new_name.to_string(),
		});
	}
	let mut changes = HashMap::new();
	for (_, (uri, mut edits)) in ordered {
		edits.sort_by(|left, right| {
			(right.range.start.line, right.range.start.character)
				.cmp(&(left.range.start.line, left.range.start.character))
		});
		changes.insert(uri, edits);
	}
	Ok(WorkspaceEdit {
		changes: Some(changes),
		..WorkspaceEdit::default()
	})
}

fn cached_text(
	snapshot: &WorkspaceSnapshot,
	opens: &HashMap<PathBuf, Option<String>>,
	path: &Path,
	cache: &mut HashMap<PathBuf, String>,
) -> Result<String, RenameBlock> {
	if let Some(text) = cache.get(path) {
		return Ok(text.clone());
	}
	let text = load_text(snapshot, opens, path)?;
	cache.insert(path.to_path_buf(), text.clone());
	Ok(text)
}

fn load_text(
	snapshot: &WorkspaceSnapshot,
	opens: &HashMap<PathBuf, Option<String>>,
	path: &Path,
) -> Result<String, RenameBlock> {
	if let Some(open) = lookup_open(opens, path) {
		let Some(text) = open else {
			return Err(RenameBlock::Stale);
		};
		return match hash_check(snapshot, path, text) {
			HashCheck::Match => Ok(text.to_string()),
			HashCheck::Drift | HashCheck::Missing => Err(RenameBlock::Stale),
		};
	}
	let text = std::fs::read_to_string(path).map_err(|_| RenameBlock::UnverifiedSpan)?;
	match hash_check(snapshot, path, &text) {
		HashCheck::Match | HashCheck::Missing => Ok(text),
		HashCheck::Drift => Err(RenameBlock::Stale),
	}
}

fn lookup_open<'a>(
	opens: &'a HashMap<PathBuf, Option<String>>,
	path: &Path,
) -> Option<&'a Option<String>> {
	if let Some(open) = opens.get(path) {
		return Some(open);
	}
	let canonical = path.canonicalize().ok()?;
	opens.iter().find_map(|(open_path, text)| {
		(open_path == &canonical || open_path.canonicalize().ok().as_ref() == Some(&canonical))
			.then_some(text)
	})
}

fn hash_check(snapshot: &WorkspaceSnapshot, path: &Path, text: &str) -> HashCheck {
	let actual = blake3::hash(text.as_bytes());
	if let Some(expected) = snapshot.source_hashes.get(path) {
		return if expected == &actual {
			HashCheck::Match
		} else {
			HashCheck::Drift
		};
	}
	let Ok(canonical) = path.canonicalize() else {
		return HashCheck::Missing;
	};
	for (stored, expected) in &snapshot.source_hashes {
		let same = stored == &canonical || stored.canonicalize().ok().as_ref() == Some(&canonical);
		if same {
			return if expected == &actual {
				HashCheck::Match
			} else {
				HashCheck::Drift
			};
		}
	}
	HashCheck::Missing
}
