use crate::check::model::CheckRequest;
use crate::check::merge::emit::emit_clausewitz_statements;
use crate::check::parser::{AstStatement, AstValue};
use crate::check::runtime::{OverlapStatus, build_runtime_state_from_workspace};
use crate::check::semantic_index::parse_script_file;
use crate::check::simplify::model::{
	SimplifyKeptItem, SimplifyOptions, SimplifyRemovedItem, SimplifyReport, SimplifySummary,
};
use crate::check::workspace::resolve_workspace;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

pub fn run_simplify_with_options(
	request: CheckRequest,
	options: SimplifyOptions,
) -> Result<SimplifySummary, Box<dyn std::error::Error>> {
	let workspace = resolve_workspace(&request, options.include_game_base).map_err(|err| err.message)?;
	let runtime = build_runtime_state_from_workspace(&workspace)?;
	let target = workspace
		.mods
		.iter()
		.find(|item| item.mod_id == options.target_mod_id)
		.ok_or_else(|| format!("unknown mod target {}", options.target_mod_id))?;
	let source_root = target
		.root_path
		.as_ref()
		.ok_or_else(|| format!("target mod {} has no root path", options.target_mod_id))?;
	let destination_root = if options.in_place {
		source_root.clone()
	} else {
		let out_dir = options
			.out_dir
			.as_ref()
			.ok_or_else(|| "--out is required unless --in-place is set".to_string())?;
		copy_directory(source_root, out_dir)?;
		out_dir.clone()
	};

	let mut removals_by_path = BTreeMap::<String, Vec<(usize, usize)>>::new();
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

	let mut removed_file_count = 0usize;
	for (path, positions) in removals_by_path {
		let absolute = destination_root.join(&path);
		if !absolute.exists() {
			continue;
		}
		let Some(mut parsed) = parse_script_file(&options.target_mod_id, &destination_root, &absolute) else {
			continue;
		};
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

	let report_path = destination_root.join("simplify-report.json");
	fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;

	Ok(SimplifySummary {
		report_path,
		removed_definition_count: report.removed.len(),
		removed_file_count,
		target_root: destination_root,
	})
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

fn symbol_kind_text(kind: crate::check::model::SymbolKind) -> &'static str {
	match kind {
		crate::check::model::SymbolKind::ScriptedEffect => "scripted_effect",
		crate::check::model::SymbolKind::ScriptedTrigger => "scripted_trigger",
		crate::check::model::SymbolKind::Event => "event",
		crate::check::model::SymbolKind::Decision => "decision",
		crate::check::model::SymbolKind::DiplomaticAction => "diplomatic_action",
		crate::check::model::SymbolKind::TriggeredModifier => "triggered_modifier",
	}
}
