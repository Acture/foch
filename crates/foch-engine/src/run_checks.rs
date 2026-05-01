use crate::merge::namespace::{build_family_key_index, detect_key_conflicts, group_by_family};
use crate::request::{CheckRequest, RunOptions};
use crate::runtime::{build_overlap_findings, build_runtime_state_from_workspace};
use crate::workspace::{
	LoadedModSnapshot, ResolvedFileContributor, WorkspaceResolveErrorKind, normalize_relative_path,
	resolve_workspace,
};
use foch_core::model::{
	AnalysisMeta, AnalysisMode, CheckContext, CheckResult, DocumentFamily, FamilyParseStats,
	Finding, FindingChannel, ParseFamilyStats, ParseIssueReportItem, SemanticIndex, Severity,
};
use foch_language::analyzer::analysis::{AnalyzeOptions, analyze_visibility};
use foch_language::analyzer::content_family::GameProfile;
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::rules::{
	check_dependency_misuse, check_duplicate_mod_identity, check_duplicate_scripted_effect,
	check_file_conflict, check_missing_dependency, check_missing_descriptor, check_required_fields,
	check_version_mismatch,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

/// Tracing target for `foch check` (and `foch merge`'s revalidation pass)
/// progress events. The CLI enables INFO-level output for this target on the
/// relevant commands so users see a stage-by-stage progress trail instead of
/// staring at a silent terminal during the 30-second-plus end-to-end run.
pub const CHECK_PROGRESS_TARGET: &str = "foch::check::progress";

fn run_progress_stage<T, F, S>(stage: &'static str, f: F, summarize: S) -> T
where
	F: FnOnce() -> T,
	S: FnOnce(&T) -> String,
{
	tracing::info!(target: CHECK_PROGRESS_TARGET, "check {stage}: start");
	let started = Instant::now();
	let value = f();
	let elapsed_ms = started.elapsed().as_millis() as u64;
	let summary = summarize(&value);
	if summary.is_empty() {
		tracing::info!(target: CHECK_PROGRESS_TARGET, elapsed_ms, "check {stage}: done");
	} else {
		tracing::info!(
			target: CHECK_PROGRESS_TARGET,
			elapsed_ms,
			summary = %summary,
			"check {stage}: done",
		);
	}
	value
}

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

	let resolved = run_progress_stage(
		"resolve workspace",
		|| resolve_workspace(&request, options.include_game_base),
		|res| match res {
			Ok(workspace) => format!(
				"mods={} inventory_files={}",
				workspace.mods.len(),
				workspace.file_inventory.len()
			),
			Err(_) => "failed".to_string(),
		},
	);
	let resolved = match resolved {
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
	let semantic_index = run_progress_stage(
		"merge semantic indexes",
		|| match base_semantic {
			Some(snapshot) => merge_semantic_indexes(snapshot.index, mod_semantic_index),
			None => mod_semantic_index,
		},
		|index| {
			format!(
				"definitions={} references={} scopes={}",
				index.definitions.len(),
				index.references.len(),
				index.scopes.len()
			)
		},
	);
	let game_version = resolved
		.installed_base_snapshot
		.as_ref()
		.map(|installed| installed.snapshot.game_version.clone());
	let runtime_overlap_findings = if options.analysis_mode == AnalysisMode::Semantic {
		run_progress_stage(
			"runtime overlap",
			|| {
				build_runtime_state_from_workspace(&resolved)
					.ok()
					.map(|state| build_overlap_findings(&state))
					.unwrap_or_default()
			},
			|findings| format!("findings={}", findings.len()),
		)
	} else {
		Vec::new()
	};
	let ctx = CheckContext {
		playlist_path: resolved.playlist_path.clone(),
		playlist: resolved.playlist,
		mods: resolved.mods,
		semantic_index,
	};

	run_progress_stage(
		"structural rules",
		|| {
			result.findings.extend(check_required_fields(&ctx));
			result.findings.extend(check_duplicate_mod_identity(&ctx));
			result.findings.extend(check_missing_descriptor(&ctx));
			result.findings.extend(check_file_conflict(&ctx));
			result.findings.extend(check_missing_dependency(&ctx));
			if let Some(game_version) = game_version.as_deref() {
				result
					.findings
					.extend(check_version_mismatch(&ctx, game_version));
			}
		},
		|_| String::new(),
	);

	if options.analysis_mode == AnalysisMode::Semantic {
		// Names already covered by the runtime overlap module (A003 mergeable /
		// discardable, S001 overshadow). R007 and N001 are redundant for these
		// names — the overlap finding is more specific. Collect the covered
		// names so we can suppress the duplicates downstream.
		let overlap_covered_names: HashSet<String> = runtime_overlap_findings
			.iter()
			.filter(|finding| finding.rule_id == "S001" || finding.rule_id == "A003")
			.filter_map(|finding| extract_overlap_symbol_name(&finding.message))
			.collect();

		let diagnostics = run_progress_stage(
			"semantic visibility",
			|| {
				analyze_visibility(
					&ctx.semantic_index,
					&AnalyzeOptions {
						mode: options.analysis_mode,
					},
				)
			},
			|d| format!("strict={} advisory={}", d.strict.len(), d.advisory.len()),
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
		result.findings.extend(check_dependency_misuse(&ctx));
		result.findings.extend(
			check_namespace_conflicts(&resolved.file_inventory, &ctx.mods)
				.into_iter()
				.filter(|finding| {
					!finding
						.evidence
						.as_deref()
						.and_then(extract_namespace_key)
						.is_some_and(|key| overlap_covered_names.contains(key))
				}),
		);
	} else {
		// Basic mode: no overlap module runs, so the heuristic R007 still
		// provides value for scripted-effect duplicates.
		result
			.findings
			.extend(check_duplicate_scripted_effect(&ctx));
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

/// Target content families for cross-file key conflict detection.
const NAMESPACE_CHECK_FAMILIES: &[&str] = &["common/scripted_effects", "common/scripted_triggers"];

/// Detect cross-file key conflicts in high-value content families.
///
/// Only checks `scripted_effects` and `scripted_triggers` — the families
/// where two mods silently redefining the same key is a common source of
/// broken gameplay.
fn check_namespace_conflicts(
	file_inventory: &BTreeMap<String, Vec<ResolvedFileContributor>>,
	mods: &[foch_core::model::ModCandidate],
) -> Vec<Finding> {
	let profile = eu4_profile();
	let families_by_id = group_by_family(file_inventory, profile);

	let mod_names: HashMap<&str, &str> = mods
		.iter()
		.filter_map(|m| {
			m.entry
				.display_name
				.as_deref()
				.map(|name| (m.mod_id.as_str(), name))
		})
		.collect();

	let mut findings = Vec::new();

	for family_id in NAMESPACE_CHECK_FAMILIES {
		let Some(family_files) = families_by_id.get(*family_id) else {
			continue;
		};
		let Some(descriptor) = profile.descriptor_for_root_family(family_id) else {
			continue;
		};
		let Some(merge_key_source) = descriptor.merge_key_source else {
			continue;
		};

		let index = build_family_key_index(family_id, merge_key_source, family_files, profile);
		let conflicts = detect_key_conflicts(&index);

		for conflict in &conflicts {
			let mut seen_mods = HashSet::new();
			let non_base: Vec<_> = conflict
				.contributors
				.iter()
				.filter(|c| !c.is_base_game)
				.filter(|c| seen_mods.insert(c.mod_id.as_str()))
				.collect();
			if non_base.len() < 2 {
				continue;
			}

			let primary = non_base[0];
			let participants: Vec<String> = non_base
				.iter()
				.map(|contributor| {
					let mod_name = mod_names
						.get(contributor.mod_id.as_str())
						.copied()
						.unwrap_or(&contributor.mod_id);
					format!("'{}' in {}", mod_name, contributor.file_path)
				})
				.collect();
			let evidence = format!(
				"key={}; mods=[{}]",
				conflict.key,
				non_base
					.iter()
					.map(|c| format!("{}:{}", c.mod_id, c.file_path))
					.collect::<Vec<_>>()
					.join(", ")
			);

			findings.push(Finding {
				rule_id: "N001".to_string(),
				severity: Severity::Warning,
				channel: FindingChannel::Advisory,
				message: format!(
					"Key '{}' is defined by {} mods: {}",
					conflict.key,
					non_base.len(),
					participants.join(", ")
				),
				mod_id: Some(primary.mod_id.clone()),
				path: Some(std::path::PathBuf::from(&primary.file_path)),
				evidence: Some(evidence),
				line: None,
				column: None,
				confidence: Some(0.9),
			});
		}
	}

	findings
}

/// Pull the symbol name out of an overlap finding message such as
/// `跨 Mod 重合定义会改变解析目标: scripted_effect foo_bar` or
/// `跨 Mod 重合定义可自动合并: scripted_trigger baz`.
fn extract_overlap_symbol_name(message: &str) -> Option<String> {
	let after_colon = message.rsplit_once(':')?.1.trim();
	let (_kind, name) = after_colon.split_once(' ')?;
	let name = name.trim();
	if name.is_empty() {
		None
	} else {
		Some(name.to_string())
	}
}

/// Pull the conflict key from an N001 evidence string of the form
/// `key=NAME; mods=[...]`.
fn extract_namespace_key(evidence: &str) -> Option<&str> {
	let rest = evidence.strip_prefix("key=")?;
	let end = rest.find(';').unwrap_or(rest.len());
	Some(rest[..end].trim())
}
