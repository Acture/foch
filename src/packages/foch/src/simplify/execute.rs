use super::model::{
	SimplifyKeptItem, SimplifyOptions, SimplifyRemovedItem, SimplifyReport, SimplifySummary,
};
use crate::check::runtime::{OverlapStatus, build_runtime_state_from_input};
use crate::game::eu4::script::emit::emit_clausewitz_statements;
use crate::game::eu4::script::parse_script_file;
use crate::game::eu4::script::parser::{AstStatement, AstValue};
use crate::input::request::InputRequest;
use crate::input::resolve_input;
use crate::model::{GamePathBuf, SymbolKind};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

pub fn run_simplify_with_options(
	request: InputRequest,
	options: SimplifyOptions,
) -> Result<SimplifySummary, Box<dyn std::error::Error>> {
	let input = resolve_input(&request, options.include_game_base).map_err(|err| err.message)?;
	let runtime = build_runtime_state_from_input(&input)?;
	let target = input
		.mods
		.iter()
		.find(|item| item.mod_id == options.target_mod_id)
		.ok_or_else(|| format!("unknown mod target {}", options.target_mod_id))?;
	let source_root = target
		.root_path
		.as_ref()
		.ok_or_else(|| format!("target mod {} has no root path", options.target_mod_id))?;
	let destination_root = options.out_dir.clone();
	ensure_output_is_separate(source_root, &destination_root)?;
	copy_directory(source_root, &destination_root)?;

	let mut removals_by_path = BTreeMap::<GamePathBuf, Vec<(usize, usize)>>::new();
	let mut report = SimplifyReport {
		target_mod_id: options.target_mod_id.clone(),
		..SimplifyReport::default()
	};

	for definition in runtime
		.definitions
		.iter()
		.filter(|definition| definition.mod_id == options.target_mod_id)
	{
		match runtime
			.overlap_status_by_def
			.get(&definition.index)
			.copied()
			.unwrap_or(OverlapStatus::None)
		{
			OverlapStatus::DiscardableBaseCopy => {
				report.removed.push(SimplifyRemovedItem {
					symbol_kind: symbol_kind_text(definition.kind).to_string(),
					name: definition.local_name.clone(),
					path: definition.path.clone(),
					line: definition.line,
					column: definition.column,
				});
				removals_by_path
					.entry(definition.path.clone())
					.or_default()
					.push((definition.line, definition.column));
			}
			OverlapStatus::MergeCandidate => report.merge_candidates.push(SimplifyKeptItem {
				symbol_kind: symbol_kind_text(definition.kind).to_string(),
				name: definition.local_name.clone(),
				path: definition.path.clone(),
				line: definition.line,
				column: definition.column,
				reason: "merge_candidate".to_string(),
			}),
			OverlapStatus::OvershadowConflict => report.conflicts.push(SimplifyKeptItem {
				symbol_kind: symbol_kind_text(definition.kind).to_string(),
				name: definition.local_name.clone(),
				path: definition.path.clone(),
				line: definition.line,
				column: definition.column,
				reason: "overshadow_conflict".to_string(),
			}),
			OverlapStatus::None => report.kept.push(SimplifyKeptItem {
				symbol_kind: symbol_kind_text(definition.kind).to_string(),
				name: definition.local_name.clone(),
				path: definition.path.clone(),
				line: definition.line,
				column: definition.column,
				reason: "kept".to_string(),
			}),
		}
	}

	let removed_file_count =
		apply_removals(&options.target_mod_id, &destination_root, removals_by_path)?;

	let report_path = destination_root.join("simplify-report.json");
	fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;

	Ok(SimplifySummary {
		report_path,
		removed_definition_count: report.removed.len(),
		removed_file_count,
		target_root: destination_root,
	})
}

/// Removes the statements at each file's positions from the copy of the mod
/// under `destination_root`, deleting a file left empty. Each file is the
/// game path joined onto that root, and a file absent there is skipped.
/// Returns how many files were deleted.
fn apply_removals(
	mod_id: &str,
	destination_root: &Path,
	removals_by_path: BTreeMap<GamePathBuf, Vec<(usize, usize)>>,
) -> Result<usize, Box<dyn std::error::Error>> {
	let mut removed_file_count = 0usize;
	for (relative, positions) in removals_by_path {
		let absolute = relative.to_path(destination_root);
		if !absolute.exists() {
			continue;
		}
		let mut parsed = parse_script_file(mod_id, destination_root, &relative);
		let positions = positions.into_iter().collect::<HashSet<_>>();
		remove_matching_statements(&mut parsed.ast.statements, &positions);
		if parsed.ast.statements.is_empty() {
			fs::remove_file(&absolute)?;
			removed_file_count += 1;
			continue;
		}
		let rendered = emit_clausewitz_statements(&parsed.ast.statements)?;
		fs::write(&absolute, rendered)?;
	}
	Ok(removed_file_count)
}

/// Refuses an output directory that is, contains, or lies inside the source
/// mod root. Copying replaces the output directory, so any overlap would
/// rewrite or delete the read-only source mod.
fn ensure_output_is_separate(source: &Path, destination: &Path) -> Result<(), String> {
	let source = fs::canonicalize(source).map_err(|err| {
		format!(
			"failed to resolve target mod root {}: {err}",
			source.display()
		)
	})?;
	let destination = canonicalize_existing_prefix(destination).map_err(|err| {
		format!(
			"failed to resolve simplify output {}: {err}",
			destination.display()
		)
	})?;
	if destination.starts_with(&source) || source.starts_with(&destination) {
		return Err(format!(
			"simplify output {} overlaps the target mod root {}; source mods are read-only, choose a separate --out directory",
			destination.display(),
			source.display()
		));
	}
	Ok(())
}

/// Canonicalizes the longest existing ancestor of `path` and re-appends the
/// components that do not exist yet.
fn canonicalize_existing_prefix(path: &Path) -> std::io::Result<std::path::PathBuf> {
	let absolute = std::path::absolute(path)?;
	let mut existing = absolute.as_path();
	let mut missing = Vec::new();
	loop {
		match fs::canonicalize(existing) {
			Ok(mut resolved) => {
				for component in missing.into_iter().rev() {
					resolved.push(component);
				}
				return Ok(resolved);
			}
			Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
				let Some(name) = existing.file_name() else {
					return Err(err);
				};
				missing.push(name.to_os_string());
				existing = existing.parent().ok_or(err)?;
			}
			Err(err) => return Err(err),
		}
	}
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
	if destination.exists() {
		fs::remove_dir_all(destination)?;
	}
	for entry in WalkDir::new(source).into_iter().filter_map(Result::ok) {
		let relative = entry.path().strip_prefix(source)?;
		let target = destination.join(relative);
		if entry.file_type().is_dir() {
			fs::create_dir_all(&target)?;
		} else {
			if let Some(parent) = target.parent() {
				fs::create_dir_all(parent)?;
			}
			fs::copy(entry.path(), &target)?;
		}
	}
	Ok(())
}

fn remove_matching_statements(
	statements: &mut Vec<AstStatement>,
	positions: &HashSet<(usize, usize)>,
) {
	let mut retained = Vec::new();
	for mut statement in std::mem::take(statements) {
		let remove_here = match &statement {
			AstStatement::Assignment { key_span, .. } => {
				positions.contains(&(key_span.start.line, key_span.start.column))
			}
			_ => false,
		};
		if remove_here {
			continue;
		}
		match &mut statement {
			AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
				if let AstValue::Block { items, .. } = value {
					remove_matching_statements(items, positions);
				}
			}
			AstStatement::Comment { .. } => {}
		}
		let keep_statement = match &statement {
			AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} => !items.is_empty(),
			_ => true,
		};
		if keep_statement {
			retained.push(statement);
		}
	}
	*statements = retained;
}

fn symbol_kind_text(kind: SymbolKind) -> &'static str {
	match kind {
		SymbolKind::ScriptedEffect => "scripted_effect",
		SymbolKind::ScriptedTrigger => "scripted_trigger",
		SymbolKind::Event => "event",
		SymbolKind::Decision => "decision",
		SymbolKind::DiplomaticAction => "diplomatic_action",
		SymbolKind::TriggeredModifier => "triggered_modifier",
	}
}

#[cfg(test)]
mod tests {
	use super::apply_removals;
	use crate::game::eu4::script::parser::{AstStatement, parse_clausewitz_content};
	use crate::model::GamePathBuf;
	use std::collections::BTreeMap;
	use std::fs;
	use std::path::Path;

	const TWO_DEFINITIONS: &str = "keep = { a = 1 }\ndrop = { b = 2 }\n";
	const ONLY_DROP: &str = "drop = { b = 2 }\n";

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("valid game path")
	}

	fn write(root: &Path, path: &GamePathBuf, source: &str) {
		let physical = path.to_path(root);
		fs::create_dir_all(physical.parent().expect("file has a parent")).expect("create dir");
		fs::write(physical, source).expect("write fixture");
	}

	fn drop_position(path: &GamePathBuf, source: &str) -> (usize, usize) {
		parse_clausewitz_content(path, source)
			.ast
			.statements
			.iter()
			.find_map(|statement| match statement {
				AstStatement::Assignment { key, key_span, .. } if key == "drop" => {
					Some((key_span.start.line, key_span.start.column))
				}
				_ => None,
			})
			.expect("fixture defines `drop`")
	}

	#[test]
	fn removals_rewrite_exactly_the_file_their_game_path_names_under_the_destination() {
		let root = tempfile::tempdir().expect("create destination");
		let nested = game_path("common/scripted_effects/a/b.txt");
		// Sorts directly before `a/b.txt` in byte order and must stay untouched.
		let neighbour = game_path("common/scripted_effects/a-b.txt");
		let spaced = game_path("common/scripted_effects/only drop.txt");
		write(root.path(), &nested, TWO_DEFINITIONS);
		write(root.path(), &neighbour, TWO_DEFINITIONS);
		write(root.path(), &spaced, ONLY_DROP);

		let removals = BTreeMap::from([
			(
				nested.clone(),
				vec![drop_position(&nested, TWO_DEFINITIONS)],
			),
			(spaced.clone(), vec![drop_position(&spaced, ONLY_DROP)]),
		]);
		let deleted = apply_removals("mod", root.path(), removals).expect("apply removals");

		assert_eq!(deleted, 1);
		assert!(
			!root
				.path()
				.join("common/scripted_effects/only drop.txt")
				.exists()
		);
		let rewritten = fs::read_to_string(
			root.path()
				.join("common")
				.join("scripted_effects")
				.join("a")
				.join("b.txt"),
		)
		.expect("read rewritten file");
		assert!(rewritten.contains("keep"), "{rewritten}");
		assert!(!rewritten.contains("drop"), "{rewritten}");
		assert_eq!(
			fs::read_to_string(neighbour.to_path(root.path())).expect("read neighbour"),
			TWO_DEFINITIONS
		);
	}

	/// The lossy path this replaced folded a literal backslash into a
	/// separator, so a file named `a\b.txt` and the nested `a/b.txt` were one
	/// removal target. Only the file the game path names is rewritten.
	#[cfg(unix)]
	#[test]
	fn a_file_named_with_a_literal_backslash_is_not_the_nested_file() {
		let root = tempfile::tempdir().expect("create destination");
		let nested = game_path("common/scripted_effects/a/b.txt");
		write(root.path(), &nested, TWO_DEFINITIONS);
		let literal = root
			.path()
			.join("common")
			.join("scripted_effects")
			.join(r"a\b.txt");
		fs::write(&literal, TWO_DEFINITIONS).expect("write literal-backslash sibling");

		let removals = BTreeMap::from([(
			nested.clone(),
			vec![drop_position(&nested, TWO_DEFINITIONS)],
		)]);
		apply_removals("mod", root.path(), removals).expect("apply removals");

		let rewritten = fs::read_to_string(nested.to_path(root.path())).expect("read nested");
		assert!(!rewritten.contains("drop"), "{rewritten}");
		assert_eq!(
			fs::read_to_string(&literal).expect("read literal-backslash sibling"),
			TWO_DEFINITIONS
		);
	}

	#[test]
	fn a_removal_whose_file_is_absent_from_the_destination_is_skipped() {
		let root = tempfile::tempdir().expect("create destination");
		let missing = game_path("common/scripted_effects/missing.txt");
		let removals = BTreeMap::from([(missing.clone(), vec![(1, 1)])]);

		assert_eq!(
			apply_removals("mod", root.path(), removals).expect("apply removals"),
			0
		);
		assert!(!missing.to_path(root.path()).exists());
	}
}
