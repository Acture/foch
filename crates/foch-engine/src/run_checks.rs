use crate::request::{CheckRequest, RunOptions};
use crate::runtime::{build_overlap_findings, build_runtime_state_from_workspace};
use crate::workspace::{
	LoadedModSnapshot, WorkspaceResolveErrorKind, normalize_relative_path, resolve_workspace,
};
use foch_core::model::{
	AnalysisMeta, AnalysisMode, CheckContext, CheckResult, DocumentFamily, FamilyParseStats,
	Finding, FindingChannel, ParseFamilyStats, ParseIssueReportItem, SemanticIndex, Severity,
};
use foch_language::analyzer::analysis::{AnalyzeOptions, analyze_visibility};
use foch_language::analyzer::rules::{
	check_duplicate_mod_identity, check_duplicate_scripted_effect, check_file_conflict,
	check_missing_dependency, check_missing_descriptor, check_required_fields,
};

#[derive(Clone, Debug)]
struct GameBaseSemanticSnapshot {
	index: SemanticIndex,
	parsed_files: usize,
	parse_error_count: usize,
	parse_stats: ParseFamilyStats,
}

pub fn run_checks(request: CheckRequest) -> CheckResult {
	run_checks_with_options(request, RunOptions::default())
}

pub fn run_checks_with_options(request: CheckRequest, options: RunOptions) -> CheckResult {
	let mut result = CheckResult::default();

	let resolved = match resolve_workspace(&request, options.include_game_base) {
		Ok(workspace) => workspace,
		Err(err) => {
			if err.kind == WorkspaceResolveErrorKind::PlaylistFormat {
				result.findings.push(Finding {
					rule_id: "R001".to_string(),
					severity: Severity::Error,
					channel: FindingChannel::Strict,
					message: "Playset JSON 无法解析".to_string(),
					mod_id: None,
					path: Some(err.path),
					evidence: Some(err.message),
					line: None,
					column: None,
					confidence: Some(1.0),
				});
				result.recompute_channels();
				return result;
			}
			result.push_fatal_error(err.message);
			return result;
		}
	};

	let base_semantic =
		resolved
			.installed_base_snapshot
			.as_ref()
			.map(|installed| GameBaseSemanticSnapshot {
				index: installed.snapshot.to_semantic_index(),
				parsed_files: installed.snapshot.parsed_files,
				parse_error_count: installed.snapshot.parse_error_count,
				parse_stats: installed.snapshot.parse_stats.clone(),
			});
	let mod_parsed_files_count: usize = resolved
		.mod_snapshots
		.iter()
		.flatten()
		.map(|snapshot| snapshot.parsed_files)
		.sum();
	let mod_parse_error_count: usize = resolved
		.mod_snapshots
		.iter()
		.flatten()
		.map(|snapshot| snapshot.parse_error_count)
		.sum();
	let mod_parse_stats = resolved
		.mod_snapshots
		.iter()
		.flatten()
		.fold(ParseFamilyStats::default(), |acc, snapshot| {
			sum_parse_family_stats(acc, snapshot.parse_stats.clone())
		});
	let mod_semantic_index = merge_mod_snapshots(&resolved.mod_snapshots);
	let parsed_files_count = mod_parsed_files_count
		+ base_semantic
			.as_ref()
			.map_or(0, |snapshot| snapshot.parsed_files);
	let base_parse_error_count = base_semantic
		.as_ref()
		.map_or(0, |snapshot| snapshot.parse_error_count);
	let total_parse_stats = base_semantic
		.as_ref()
		.map_or(mod_parse_stats.clone(), |snapshot| {
			sum_parse_family_stats(snapshot.parse_stats.clone(), mod_parse_stats.clone())
		});
	let semantic_index = match base_semantic {
		Some(snapshot) => merge_semantic_indexes(snapshot.index, mod_semantic_index),
		None => mod_semantic_index,
	};
	let runtime_overlap_findings = if options.analysis_mode == AnalysisMode::Semantic {
		build_runtime_state_from_workspace(&resolved)
			.ok()
			.map(|state| build_overlap_findings(&state))
			.unwrap_or_default()
	} else {
		Vec::new()
	};
	let ctx = CheckContext {
		playlist_path: resolved.playlist_path.clone(),
		playlist: resolved.playlist,
		mods: resolved.mods,
		semantic_index,
	};

	result.findings.extend(check_required_fields(&ctx));
	result.findings.extend(check_duplicate_mod_identity(&ctx));
	result.findings.extend(check_missing_descriptor(&ctx));
	result.findings.extend(check_file_conflict(&ctx));
	result.findings.extend(check_missing_dependency(&ctx));
	result
		.findings
		.extend(check_duplicate_scripted_effect(&ctx));

	if options.analysis_mode == AnalysisMode::Semantic {
		let diagnostics = analyze_visibility(
			&ctx.semantic_index,
			&AnalyzeOptions {
				mode: options.analysis_mode,
			},
		);
		result.findings.extend(
			diagnostics
				.strict
				.into_iter()
				.filter(|finding| finding.rule_id != "S001"),
		);
		result.findings.extend(
			diagnostics
				.advisory
				.into_iter()
				.filter(|finding| finding.rule_id != "A003"),
		);
		result.findings.extend(runtime_overlap_findings);
	}

	result.analysis_meta = AnalysisMeta {
		text_documents: ctx.semantic_index.documents.len(),
		parsed_files: parsed_files_count,
		parse_errors: mod_parse_error_count + base_parse_error_count,
		parse_stats: total_parse_stats,
		scopes: ctx.semantic_index.scopes.len(),
		symbol_definitions: ctx.semantic_index.definitions.len(),
		symbol_references: ctx.semantic_index.references.len(),
		alias_usages: ctx.semantic_index.alias_usages.len(),
	};
	result.parse_issue_report = build_parse_issue_report(&ctx.semantic_index);

	result.recompute_channels();
	result
}

fn merge_mod_snapshots(snapshots: &[Option<LoadedModSnapshot>]) -> SemanticIndex {
	let mut merged = SemanticIndex::default();
	for snapshot in snapshots.iter().flatten() {
		merged = merge_semantic_indexes(merged, snapshot.semantic_index.clone());
	}
	merged
}

fn sum_parse_family_stats(lhs: ParseFamilyStats, rhs: ParseFamilyStats) -> ParseFamilyStats {
	ParseFamilyStats {
		clausewitz_mainline: sum_family_parse_stats(
			lhs.clausewitz_mainline,
			rhs.clausewitz_mainline,
		),
		localisation: sum_family_parse_stats(lhs.localisation, rhs.localisation),
		csv: sum_family_parse_stats(lhs.csv, rhs.csv),
		json: sum_family_parse_stats(lhs.json, rhs.json),
	}
}

fn sum_family_parse_stats(lhs: FamilyParseStats, rhs: FamilyParseStats) -> FamilyParseStats {
	FamilyParseStats {
		documents: lhs.documents + rhs.documents,
		parse_failed_documents: lhs.parse_failed_documents + rhs.parse_failed_documents,
		parse_issue_count: lhs.parse_issue_count + rhs.parse_issue_count,
	}
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

fn build_parse_issue_report(index: &SemanticIndex) -> Vec<ParseIssueReportItem> {
	let family_lookup = index
		.documents
		.iter()
		.map(|item| {
			(
				(item.mod_id.clone(), normalize_relative_path(&item.path)),
				item.family,
			)
		})
		.collect::<std::collections::HashMap<_, _>>();
	let mut items: Vec<ParseIssueReportItem> = index
		.parse_issues
		.iter()
		.map(|issue| ParseIssueReportItem {
			family: family_lookup
				.get(&(issue.mod_id.clone(), normalize_relative_path(&issue.path)))
				.copied()
				.unwrap_or(DocumentFamily::Clausewitz),
			mod_id: issue.mod_id.clone(),
			path: issue.path.clone(),
			line: issue.line,
			column: issue.column,
			message: issue.message.clone(),
		})
		.collect();
	items.sort_by(|lhs, rhs| {
		(
			format!("{:?}", lhs.family),
			lhs.mod_id.as_str(),
			lhs.path.as_os_str(),
			lhs.line,
			lhs.column,
			lhs.message.as_str(),
		)
			.cmp(&(
				format!("{:?}", rhs.family),
				rhs.mod_id.as_str(),
				rhs.path.as_os_str(),
				rhs.line,
				rhs.column,
				rhs.message.as_str(),
			))
	});
	items
}
