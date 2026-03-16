use crate::check::engine::{ResolvedFileContributor, WorkspaceResolveErrorKind, resolve_workspace};
use crate::check::model::{
	CheckRequest, MergePlanContributor, MergePlanEntry, MergePlanOptions, MergePlanResult,
	MergePlanStrategy, MergePlanSummary,
};
use crate::check::semantic_index::parse_script_file;

pub fn run_merge_plan(request: CheckRequest) -> MergePlanResult {
	run_merge_plan_with_options(request, MergePlanOptions::default())
}

pub fn run_merge_plan_with_options(
	request: CheckRequest,
	options: MergePlanOptions,
) -> MergePlanResult {
	let mut result = MergePlanResult {
		include_game_base: options.include_game_base,
		..MergePlanResult::default()
	};

	let workspace = match resolve_workspace(&request, options.include_game_base) {
		Ok(workspace) => workspace,
		Err(err) => {
			if err.kind == WorkspaceResolveErrorKind::PlaylistFormat {
				result.push_fatal_error("无法解析 Playset JSON");
			} else {
				result.push_fatal_error(err.message);
			}
			return result;
		}
	};

	result.game = workspace.playlist.game.key().to_string();
	result.playset_name = workspace.playlist.name.clone();

	result.entries = workspace
		.file_inventory
		.iter()
		.map(|(path, contributors)| classify_entry(path, contributors))
		.collect();
	result.summary = summarize_entries(&result.entries);
	result
}

fn classify_entry(path: &str, contributors: &[ResolvedFileContributor]) -> MergePlanEntry {
	let contributors_out: Vec<MergePlanContributor> =
		contributors.iter().map(to_merge_contributor).collect();
	let winner = contributors_out.last().cloned();
	let mut notes = Vec::new();

	let strategy = if contributors.len() == 1 {
		notes.push("single contributor".to_string());
		MergePlanStrategy::CopyThrough
	} else if is_ui_conflict_path(path) {
		notes.push("ui path overlap is not rewritten in v1".to_string());
		MergePlanStrategy::ManualConflict
	} else if is_structural_merge_path(path) {
		match validate_structural_merge_inputs(contributors) {
			Ok(()) => {
				notes.push("all contributors parsed successfully".to_string());
				MergePlanStrategy::StructuralMerge
			}
			Err(message) => {
				notes.push(message);
				MergePlanStrategy::ManualConflict
			}
		}
	} else if is_text_like_overlay_path(path) {
		notes.push("resolved with last-writer overlay".to_string());
		MergePlanStrategy::LastWriterOverlay
	} else {
		notes.push(
			"overlapping binary or unknown-format path requires manual resolution".to_string(),
		);
		MergePlanStrategy::ManualConflict
	};

	MergePlanEntry {
		path: path.to_string(),
		strategy,
		contributors: contributors_out,
		winner,
		notes,
	}
}

fn to_merge_contributor(contributor: &ResolvedFileContributor) -> MergePlanContributor {
	MergePlanContributor {
		mod_id: contributor.mod_id.clone(),
		source_path: contributor
			.absolute_path
			.to_string_lossy()
			.replace('\\', "/"),
		precedence: contributor.precedence,
		is_base_game: contributor.is_base_game,
	}
}

fn summarize_entries(entries: &[MergePlanEntry]) -> MergePlanSummary {
	let mut summary = MergePlanSummary {
		total_paths: entries.len(),
		..MergePlanSummary::default()
	};

	for entry in entries {
		match entry.strategy {
			MergePlanStrategy::CopyThrough => summary.copy_through += 1,
			MergePlanStrategy::LastWriterOverlay => summary.last_writer_overlay += 1,
			MergePlanStrategy::StructuralMerge => summary.structural_merge += 1,
			MergePlanStrategy::ManualConflict => summary.manual_conflict += 1,
		}
	}

	summary
}

fn validate_structural_merge_inputs(
	contributors: &[ResolvedFileContributor],
) -> Result<(), String> {
	let mut failures = Vec::new();

	for contributor in contributors {
		if let Some(parse_ok) = contributor.parse_ok_hint {
			if parse_ok {
				continue;
			}
			if contributor.is_base_game {
				failures.push(format!("base game parse issues in {}", contributor.mod_id));
			} else {
				failures.push(format!("cached parse issues in {}", contributor.mod_id));
			}
			continue;
		}

		match parse_script_file(
			&contributor.mod_id,
			&contributor.root_path,
			&contributor.absolute_path,
		) {
			Some(parsed) if parsed.parse_issues.is_empty() => {}
			Some(parsed) => failures.push(format!(
				"{} parse issues in {}",
				parsed.parse_issues.len(),
				contributor.mod_id
			)),
			None => failures.push(format!("unable to parse {}", contributor.mod_id)),
		}
	}

	if failures.is_empty() {
		Ok(())
	} else {
		Err(format!(
			"structural merge blocked by parse failures: {}",
			failures.join(", ")
		))
	}
}

fn is_structural_merge_path(path: &str) -> bool {
	let normalized = path.to_ascii_lowercase();
	normalized.starts_with("events/")
		|| normalized.starts_with("decisions/")
		|| normalized.starts_with("common/scripted_effects/")
		|| normalized.starts_with("common/diplomatic_actions/")
		|| normalized.starts_with("common/triggered_modifiers/")
		|| normalized.starts_with("common/defines/")
}

fn is_ui_conflict_path(path: &str) -> bool {
	let normalized = path.to_ascii_lowercase();
	normalized.starts_with("interface/")
		|| normalized.starts_with("common/interface/")
		|| normalized.starts_with("gfx/")
}

fn is_text_like_overlay_path(path: &str) -> bool {
	let normalized = path.to_ascii_lowercase();
	let Some(ext) = normalized.rsplit('.').next() else {
		return false;
	};

	matches!(
		ext,
		"txt" | "lua" | "yml" | "yaml" | "csv" | "json" | "asset" | "gui" | "gfx" | "mod"
	)
}
