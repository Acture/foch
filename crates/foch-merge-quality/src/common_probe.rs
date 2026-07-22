use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use std::time::Instant;

use foch_engine::merge_clausewitz_definition_module;
use foch_language::analyzer::content_family::{GameProfile, MergeKeySource, MergePolicies};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstFile, AstStatement};
use serde::{Deserialize, Serialize};

use crate::common_module::{
	CommonModuleDiagnostic, CommonModuleViewBuilder, normalize_module_comparison,
};
use crate::config::Eu4GameDiscovery;
use crate::corpus_shadow::{
	LoadedSnapshot, latest_snapshots, load_snapshot, validate_snapshot_game,
};
use crate::dataset::{DatasetPaths, now_rfc3339};
use crate::object_store::ObjectStore;
use crate::orchestrate::FileRecord;
use crate::score::{
	SemanticAtomDiff, semantic_ast_content_id, semantic_atom_diff_ast,
	semantic_atom_diff_statements,
};

pub const COMMON_APPLICABILITY_SCHEMA: &str = "2.0.0";
pub const COMMON_APPLICABILITY_UNIT_COUNT: usize = 12;

pub struct CommonApplicabilityOptions<'a> {
	pub dataset_root: &'a Path,
	pub output_dir: &'a Path,
	pub legacy_baseline: &'a Path,
	pub game: &'a Eu4GameDiscovery,
	pub case_ids: &'a BTreeSet<String>,
	pub families: &'a BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommonProbeStatus {
	AcceptedEquivalent,
	ManualResolutionRequired,
	SemanticMismatch,
	InputFailure,
	ParseFailure,
	ConfigurationFailure,
	AdapterFailure,
}

impl CommonProbeStatus {
	fn is_accepted(self) -> bool {
		matches!(self, Self::AcceptedEquivalent)
	}

	fn is_failure(self) -> bool {
		matches!(
			self,
			Self::InputFailure
				| Self::ParseFailure
				| Self::ConfigurationFailure
				| Self::AdapterFailure
		)
	}

	fn as_str(self) -> &'static str {
		match self {
			Self::AcceptedEquivalent => "accepted_equivalent",
			Self::ManualResolutionRequired => "manual_resolution_required",
			Self::SemanticMismatch => "semantic_mismatch",
			Self::InputFailure => "input_failure",
			Self::ParseFailure => "parse_failure",
			Self::ConfigurationFailure => "configuration_failure",
			Self::AdapterFailure => "adapter_failure",
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CommonProbeDiagnostic {
	pub phase: String,
	pub path: Option<String>,
	pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CommonProbeConflict {
	pub kind: String,
	pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CommonProbeScalarReduction {
	pub path: Vec<String>,
	pub inputs: Vec<(u16, String)>,
	pub output: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CommonProbeTimings {
	pub view_ms: u64,
	pub merge_ms: u64,
	pub comparison_ms: u64,
	pub copy_through_definitions: usize,
	pub structured_definitions: usize,
	pub comparison_reused_definitions: usize,
	pub comparison_normalized_definitions: usize,
	pub matcher_ns: u64,
	pub pcs_ns: u64,
	pub policy_ns: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CommonProbeViewIdentities {
	pub base: String,
	pub left: String,
	pub right: String,
	pub human: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CommonProbeUnit {
	pub case_id: String,
	pub snapshot_id: Option<String>,
	pub relative_path: String,
	pub module_prefix: String,
	pub content_family: Option<String>,
	pub source_mod_ids: Vec<String>,
	pub legacy_verdict: String,
	pub legacy_accepted: bool,
	pub status: CommonProbeStatus,
	pub publishable: bool,
	pub policies: Option<MergePolicies>,
	pub view_content_ids: Option<CommonProbeViewIdentities>,
	pub candidate_semantic_content_id: Option<String>,
	pub base_definitions: usize,
	pub active_definitions: usize,
	pub missing_top_level_keys: Vec<String>,
	pub extra_top_level_keys: Vec<String>,
	pub semantic_diff_from_human: Option<SemanticAtomDiff>,
	pub conflicts: Vec<CommonProbeConflict>,
	pub scalar_reductions: Vec<CommonProbeScalarReduction>,
	pub diagnostics: Vec<CommonProbeDiagnostic>,
	pub timings: CommonProbeTimings,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CommonProbeSummary {
	pub expected_units: usize,
	pub classified_units: usize,
	pub full_denominator: bool,
	pub accepted_equivalent: usize,
	pub manual_resolution_required: usize,
	pub semantic_mismatch: usize,
	pub failed: usize,
	pub legacy_accepted: usize,
	pub legacy_accepted_preserved: usize,
	pub review_required: usize,
	pub gate_passed: Option<bool>,
	pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CommonProbeFamilySummary {
	pub content_family: String,
	pub units: usize,
	pub accepted_equivalent: usize,
	pub review_required: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CommonApplicabilityReport {
	pub schema: String,
	pub generated_at: String,
	pub hypothesis: String,
	pub game_version: String,
	pub steam_build_id: Option<u64>,
	pub executable_blake3: String,
	pub legacy_baseline_blake3: String,
	pub summary: CommonProbeSummary,
	pub families: Vec<CommonProbeFamilySummary>,
	pub units: Vec<CommonProbeUnit>,
}

#[derive(Deserialize)]
struct LegacyBaseline {
	units: Vec<LegacyBaselineUnit>,
}

#[derive(Clone, Deserialize)]
struct LegacyBaselineUnit {
	case_id: String,
	score: FileRecord,
}

struct ComparisonNormalization {
	candidate: AstFile,
	human: AstFile,
	conflicts: Vec<CommonProbeConflict>,
	reused_definitions: usize,
	normalized_definitions: usize,
}

pub fn run_common_applicability_probe(
	options: &CommonApplicabilityOptions<'_>,
) -> Result<CommonApplicabilityReport, Box<dyn std::error::Error>> {
	let started = Instant::now();
	let all_targets = load_targets(options.legacy_baseline)?;
	if all_targets.len() != COMMON_APPLICABILITY_UNIT_COUNT {
		return Err(format!(
			"common applicability denominator drifted: expected {}, found {}",
			COMMON_APPLICABILITY_UNIT_COUNT,
			all_targets.len()
		)
		.into());
	}
	let full_denominator = options.case_ids.is_empty() && options.families.is_empty();
	let targets = select_targets(&all_targets, options.case_ids, options.families)?;

	let paths = DatasetPaths::new(options.dataset_root);
	let snapshots = latest_snapshots(&paths)?
		.into_iter()
		.map(|snapshot| (snapshot.case_id.clone(), snapshot))
		.collect::<BTreeMap<_, _>>();
	let store = ObjectStore::new(&paths.objects, &paths.work);
	let mut verified = HashSet::new();
	let mut loaded = BTreeMap::<String, LoadedSnapshot>::new();
	let mut load_errors = BTreeMap::<String, String>::new();
	for case_id in targets
		.iter()
		.map(|target| target.case_id.as_str())
		.collect::<BTreeSet<_>>()
	{
		let Some(snapshot) = snapshots.get(case_id).cloned() else {
			load_errors.insert(
				case_id.to_string(),
				"latest snapshot is missing".to_string(),
			);
			continue;
		};
		if let Err(error) = validate_snapshot_game(&snapshot, options.game) {
			load_errors.insert(case_id.to_string(), error.to_string());
			continue;
		}
		match load_snapshot(&store, snapshot, &mut verified) {
			Ok(snapshot) => {
				loaded.insert(case_id.to_string(), snapshot);
			}
			Err(error) => {
				load_errors.insert(case_id.to_string(), error.to_string());
			}
		}
	}

	let mut cache = CommonModuleViewBuilder::default();
	let mut units = Vec::with_capacity(targets.len());
	for (index, target) in targets.iter().enumerate() {
		let unit_started = Instant::now();
		let unit = if let Some(error) = load_errors.get(&target.case_id) {
			failed_unit(
				target,
				CommonProbeStatus::InputFailure,
				"snapshot",
				error.clone(),
			)
		} else if let Some(snapshot) = loaded.get(&target.case_id) {
			evaluate_unit(options, target, snapshot, &mut cache)
		} else {
			failed_unit(
				target,
				CommonProbeStatus::InputFailure,
				"snapshot",
				"snapshot was not loaded".to_string(),
			)
		};
		let elapsed_ms = duration_ms(unit_started.elapsed());
		eprintln!(
			"[common-probe] {}/{} {}/{} status={} elapsed_ms={elapsed_ms}",
			index + 1,
			targets.len(),
			target.case_id,
			target.score.rel,
			unit.status.as_str(),
		);
		units.push(unit);
	}

	let mut report = CommonApplicabilityReport {
		schema: COMMON_APPLICABILITY_SCHEMA.to_string(),
		generated_at: now_rfc3339(),
		hypothesis: "common/<folder> is a provisional semantic merge unit".to_string(),
		game_version: options.game.game_version.clone(),
		steam_build_id: options.game.steam_build_id,
		executable_blake3: blake3::hash(&fs::read(std::env::current_exe()?)?)
			.to_hex()
			.to_string(),
		legacy_baseline_blake3: blake3::hash(&fs::read(options.legacy_baseline)?)
			.to_hex()
			.to_string(),
		summary: summarize(&units, full_denominator, duration_ms(started.elapsed())),
		families: summarize_families(&units),
		units,
	};
	report.summary.gate_passed = common_gate_result(&report.summary);
	write_report(options.output_dir, &report)?;
	Ok(report)
}

fn load_targets(path: &Path) -> Result<Vec<LegacyBaselineUnit>, Box<dyn std::error::Error>> {
	let baseline: LegacyBaseline = serde_json::from_slice(&fs::read(path)?)?;
	let mut targets = baseline
		.units
		.into_iter()
		.filter(|unit| unit.score.rel.starts_with("common/"))
		.collect::<Vec<_>>();
	targets.sort_by(|left, right| {
		(&left.case_id, &left.score.rel).cmp(&(&right.case_id, &right.score.rel))
	});
	let unique = targets
		.iter()
		.map(|target| (&target.case_id, &target.score.rel))
		.collect::<BTreeSet<_>>();
	if unique.len() != targets.len() {
		return Err("common applicability baseline contains duplicate units".into());
	}
	Ok(targets)
}

fn select_targets(
	targets: &[LegacyBaselineUnit],
	case_ids: &BTreeSet<String>,
	families: &BTreeSet<String>,
) -> Result<Vec<LegacyBaselineUnit>, Box<dyn std::error::Error>> {
	let available_cases = targets
		.iter()
		.map(|target| target.case_id.as_str())
		.collect::<BTreeSet<_>>();
	let unknown_cases = case_ids
		.iter()
		.filter(|case_id| !available_cases.contains(case_id.as_str()))
		.cloned()
		.collect::<Vec<_>>();
	if !unknown_cases.is_empty() {
		return Err(format!("unknown common probe cases: {}", unknown_cases.join(", ")).into());
	}

	let profile = eu4_profile();
	let target_families = targets
		.iter()
		.filter_map(|target| {
			profile
				.classify_content_family(Path::new(&target.score.rel))
				.map(|descriptor| descriptor.id.as_str().to_string())
		})
		.collect::<BTreeSet<_>>();
	let unknown_families = families
		.iter()
		.filter(|family| !target_families.contains(family.as_str()))
		.cloned()
		.collect::<Vec<_>>();
	if !unknown_families.is_empty() {
		return Err(format!(
			"unknown common probe families: {}",
			unknown_families.join(", ")
		)
		.into());
	}

	let selected = targets
		.iter()
		.filter(|target| case_ids.is_empty() || case_ids.contains(&target.case_id))
		.filter(|target| {
			families.is_empty()
				|| profile
					.classify_content_family(Path::new(&target.score.rel))
					.is_some_and(|descriptor| families.contains(descriptor.id.as_str()))
		})
		.cloned()
		.collect::<Vec<_>>();
	if selected.is_empty() {
		return Err("common probe selection matched no units".into());
	}
	Ok(selected)
}

fn evaluate_unit(
	options: &CommonApplicabilityOptions<'_>,
	target: &LegacyBaselineUnit,
	snapshot: &LoadedSnapshot,
	cache: &mut CommonModuleViewBuilder,
) -> CommonProbeUnit {
	let Some(module_prefix) = common_folder_prefix(&target.score.rel) else {
		return failed_unit(
			target,
			CommonProbeStatus::ConfigurationFailure,
			"module_boundary",
			"path does not identify a common/<folder> module".to_string(),
		);
	};
	let Some(descriptor) = eu4_profile().classify_content_family(Path::new(&target.score.rel))
	else {
		return failed_unit_with_prefix(
			target,
			module_prefix,
			CommonProbeStatus::ConfigurationFailure,
			"content_family",
			"path has no ContentFamily descriptor".to_string(),
		);
	};
	if descriptor.merge_key_source != Some(MergeKeySource::AssignmentKey) {
		return failed_unit_with_family(
			target,
			module_prefix,
			descriptor.id.as_str(),
			CommonProbeStatus::ConfigurationFailure,
			"merge_key",
			"folder probe currently requires AssignmentKey module definitions".to_string(),
		);
	}
	if snapshot.source_dirs.len() != 2 {
		return failed_unit_with_family(
			target,
			module_prefix,
			descriptor.id.as_str(),
			CommonProbeStatus::ConfigurationFailure,
			"source_arity",
			format!(
				"structured three-way probe requires exactly two source mods, found {}",
				snapshot.source_dirs.len()
			),
		);
	}

	let view_started = Instant::now();
	let base_roots = [options.game.game_root.as_path()];
	let left_roots = [
		options.game.game_root.as_path(),
		snapshot.source_dirs[0].as_path(),
	];
	let right_roots = [
		options.game.game_root.as_path(),
		snapshot.source_dirs[1].as_path(),
	];
	let human_roots = [
		options.game.game_root.as_path(),
		snapshot.source_dirs[0].as_path(),
		snapshot.source_dirs[1].as_path(),
		snapshot.compatch.as_path(),
	];
	let base = cache.view(&base_roots, &module_prefix);
	let left = cache.view(&left_roots, &module_prefix);
	let right = cache.view(&right_roots, &module_prefix);
	let human = cache.view(&human_roots, &module_prefix);
	let view_ms = duration_ms(view_started.elapsed());
	let views = [base, left, right, human];
	if views.iter().any(Result::is_err) {
		let diagnostics = views
			.into_iter()
			.filter_map(Result::err)
			.flatten()
			.map(common_module_diagnostic)
			.collect::<Vec<_>>();
		return CommonProbeUnit {
			case_id: target.case_id.clone(),
			snapshot_id: Some(snapshot.snapshot.snapshot_id.clone()),
			relative_path: target.score.rel.clone(),
			module_prefix,
			content_family: Some(descriptor.id.as_str().to_string()),
			source_mod_ids: snapshot_source_ids(snapshot),
			legacy_verdict: target.score.verdict.clone(),
			legacy_accepted: target.score.accepted_ok,
			status: CommonProbeStatus::ParseFailure,
			publishable: false,
			policies: Some(descriptor.merge_policies),
			view_content_ids: None,
			candidate_semantic_content_id: None,
			base_definitions: 0,
			active_definitions: 0,
			missing_top_level_keys: Vec::new(),
			extra_top_level_keys: Vec::new(),
			semantic_diff_from_human: None,
			conflicts: Vec::new(),
			scalar_reductions: Vec::new(),
			diagnostics,
			timings: CommonProbeTimings {
				view_ms,
				..CommonProbeTimings::default()
			},
		};
	}
	let [Ok(base), Ok(left), Ok(right), Ok(human)] = views else {
		unreachable!("view errors returned above");
	};
	let view_content_ids = CommonProbeViewIdentities {
		base: semantic_ast_content_id(&base),
		left: semantic_ast_content_id(&left),
		right: semantic_ast_content_id(&right),
		human: semantic_ast_content_id(&human),
	};
	let merge_started = Instant::now();
	let outcome = match merge_clausewitz_definition_module(
		&base,
		&left,
		&right,
		&descriptor.merge_policies,
	) {
		Ok(outcome) => outcome,
		Err(error) => {
			return CommonProbeUnit {
				case_id: target.case_id.clone(),
				snapshot_id: Some(snapshot.snapshot.snapshot_id.clone()),
				relative_path: target.score.rel.clone(),
				module_prefix,
				content_family: Some(descriptor.id.as_str().to_string()),
				source_mod_ids: snapshot_source_ids(snapshot),
				legacy_verdict: target.score.verdict.clone(),
				legacy_accepted: target.score.accepted_ok,
				status: CommonProbeStatus::AdapterFailure,
				publishable: false,
				policies: Some(descriptor.merge_policies),
				view_content_ids: Some(view_content_ids.clone()),
				candidate_semantic_content_id: None,
				base_definitions: top_level_assignment_keys(&base).len(),
				active_definitions: changed_top_level_keys(&base, &left, &right).len(),
				missing_top_level_keys: Vec::new(),
				extra_top_level_keys: Vec::new(),
				semantic_diff_from_human: None,
				conflicts: Vec::new(),
				scalar_reductions: Vec::new(),
				diagnostics: vec![CommonProbeDiagnostic {
					phase: "structured_adapter".to_string(),
					path: Some(target.score.rel.clone()),
					message: error.to_string(),
				}],
				timings: CommonProbeTimings {
					view_ms,
					merge_ms: duration_ms(merge_started.elapsed()),
					..CommonProbeTimings::default()
				},
			};
		}
	};
	let merge_ms = duration_ms(merge_started.elapsed());
	let candidate = outcome.tentative_ast().clone();
	let comparison_started = Instant::now();
	let comparison = canonicalize_comparison_pair(
		&candidate,
		&human,
		&descriptor.merge_policies,
		&module_prefix,
	);
	let comparison_ms = duration_ms(comparison_started.elapsed());
	let diff = semantic_atom_diff_ast(&comparison.candidate, &comparison.human);
	let candidate_keys = top_level_assignment_keys(&candidate);
	let human_keys = top_level_assignment_keys(&human);
	let missing_top_level_keys = human_keys.difference(&candidate_keys).cloned().collect();
	let extra_top_level_keys = candidate_keys.difference(&human_keys).cloned().collect();
	let mut conflicts = outcome
		.conflicts()
		.iter()
		.map(|conflict| CommonProbeConflict {
			kind: conflict.kind.to_string(),
			detail: conflict.detail.clone(),
		})
		.collect::<Vec<_>>();
	conflicts.extend(comparison.conflicts);
	let scalar_reductions = outcome
		.scalar_reductions()
		.iter()
		.map(|reduction| CommonProbeScalarReduction {
			path: reduction.path.clone(),
			inputs: reduction
				.inputs
				.iter()
				.map(|(revision, value)| (revision.get(), value.clone()))
				.collect(),
			output: reduction.output.clone(),
		})
		.collect();
	let equivalent = diff.left_only.is_empty() && diff.right_only.is_empty();
	let status = if conflicts.is_empty() && equivalent {
		CommonProbeStatus::AcceptedEquivalent
	} else if !conflicts.is_empty() {
		CommonProbeStatus::ManualResolutionRequired
	} else {
		CommonProbeStatus::SemanticMismatch
	};
	CommonProbeUnit {
		case_id: target.case_id.clone(),
		snapshot_id: Some(snapshot.snapshot.snapshot_id.clone()),
		relative_path: target.score.rel.clone(),
		module_prefix,
		content_family: Some(descriptor.id.as_str().to_string()),
		source_mod_ids: snapshot_source_ids(snapshot),
		legacy_verdict: target.score.verdict.clone(),
		legacy_accepted: target.score.accepted_ok,
		status,
		publishable: status.is_accepted(),
		policies: Some(descriptor.merge_policies),
		view_content_ids: Some(view_content_ids),
		candidate_semantic_content_id: Some(semantic_ast_content_id(&comparison.candidate)),
		base_definitions: outcome.base_definitions(),
		active_definitions: outcome.active_definitions(),
		missing_top_level_keys,
		extra_top_level_keys,
		semantic_diff_from_human: Some(diff),
		conflicts,
		scalar_reductions,
		diagnostics: Vec::new(),
		timings: CommonProbeTimings {
			view_ms,
			merge_ms,
			comparison_ms,
			copy_through_definitions: outcome.copy_through_definitions(),
			structured_definitions: outcome.structured_definitions(),
			comparison_reused_definitions: comparison.reused_definitions,
			comparison_normalized_definitions: comparison.normalized_definitions,
			matcher_ns: outcome.timings().matcher_ns,
			pcs_ns: outcome.timings().pcs_ns,
			policy_ns: outcome.timings().policy_ns,
		},
	}
}
fn canonicalize_comparison_pair(
	candidate: &AstFile,
	human: &AstFile,
	policies: &MergePolicies,
	module_prefix: &str,
) -> ComparisonNormalization {
	let comparison = normalize_module_comparison(candidate, human, policies, module_prefix);
	let conflicts = comparison
		.diagnostics
		.into_iter()
		.map(|diagnostic| CommonProbeConflict {
			kind: diagnostic.phase,
			detail: match diagnostic.path {
				Some(path) => format!("{path}: {}", diagnostic.message),
				None => diagnostic.message,
			},
		})
		.collect();
	ComparisonNormalization {
		candidate: comparison.candidate,
		human: comparison.human,
		conflicts,
		reused_definitions: comparison.reused_definitions,
		normalized_definitions: comparison.normalized_definitions,
	}
}

fn definition_map(ast: &AstFile) -> BTreeMap<&str, &AstStatement> {
	ast.statements
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment { key, .. } => Some((key.as_str(), statement)),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
		})
		.collect()
}

fn optional_statements_equivalent(
	left: Option<&&AstStatement>,
	right: Option<&&AstStatement>,
) -> bool {
	match (left, right) {
		(Some(left), Some(right)) => {
			let diff = semantic_atom_diff_statements(
				std::slice::from_ref(*left),
				std::slice::from_ref(*right),
			);
			diff.left_only.is_empty() && diff.right_only.is_empty()
		}
		(None, None) => true,
		(Some(_), None) | (None, Some(_)) => false,
	}
}

fn changed_top_level_keys(base: &AstFile, left: &AstFile, right: &AstFile) -> BTreeSet<String> {
	let base_map = definition_map(base);
	let left_map = definition_map(left);
	let right_map = definition_map(right);
	base_map
		.keys()
		.chain(left_map.keys())
		.chain(right_map.keys())
		.map(|key| (*key).to_string())
		.collect::<BTreeSet<_>>()
		.into_iter()
		.filter(|key| {
			!optional_statements_equivalent(base_map.get(key.as_str()), left_map.get(key.as_str()))
				|| !optional_statements_equivalent(
					base_map.get(key.as_str()),
					right_map.get(key.as_str()),
				)
		})
		.collect()
}

fn common_module_diagnostic(diagnostic: CommonModuleDiagnostic) -> CommonProbeDiagnostic {
	CommonProbeDiagnostic {
		phase: diagnostic.phase,
		path: diagnostic.path,
		message: diagnostic.message,
	}
}

fn common_folder_prefix(relative_path: &str) -> Option<String> {
	let mut components = relative_path.split('/');
	let common = components.next()?;
	let folder = components.next()?;
	(common == "common" && !folder.is_empty()).then(|| format!("common/{folder}"))
}

fn top_level_assignment_keys(ast: &AstFile) -> BTreeSet<String> {
	ast.statements
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment { key, .. } => Some(key.clone()),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
		})
		.collect()
}

fn snapshot_source_ids(snapshot: &LoadedSnapshot) -> Vec<String> {
	snapshot
		.snapshot
		.source_mods
		.iter()
		.map(|source| source.workshop_id.clone())
		.collect()
}

fn failed_unit(
	target: &LegacyBaselineUnit,
	status: CommonProbeStatus,
	phase: &str,
	message: String,
) -> CommonProbeUnit {
	failed_unit_with_prefix(
		target,
		common_folder_prefix(&target.score.rel).unwrap_or_else(|| "common/?".to_string()),
		status,
		phase,
		message,
	)
}

fn failed_unit_with_prefix(
	target: &LegacyBaselineUnit,
	module_prefix: String,
	status: CommonProbeStatus,
	phase: &str,
	message: String,
) -> CommonProbeUnit {
	CommonProbeUnit {
		case_id: target.case_id.clone(),
		snapshot_id: None,
		relative_path: target.score.rel.clone(),
		module_prefix,
		content_family: None,
		source_mod_ids: target.score.source_mod_ids.clone(),
		legacy_verdict: target.score.verdict.clone(),
		legacy_accepted: target.score.accepted_ok,
		status,
		publishable: false,
		policies: None,
		view_content_ids: None,
		candidate_semantic_content_id: None,
		base_definitions: 0,
		active_definitions: 0,
		missing_top_level_keys: Vec::new(),
		extra_top_level_keys: Vec::new(),
		semantic_diff_from_human: None,
		conflicts: Vec::new(),
		scalar_reductions: Vec::new(),
		diagnostics: vec![CommonProbeDiagnostic {
			phase: phase.to_string(),
			path: Some(target.score.rel.clone()),
			message,
		}],
		timings: CommonProbeTimings::default(),
	}
}

fn failed_unit_with_family(
	target: &LegacyBaselineUnit,
	module_prefix: String,
	content_family: &str,
	status: CommonProbeStatus,
	phase: &str,
	message: String,
) -> CommonProbeUnit {
	let mut unit = failed_unit_with_prefix(target, module_prefix, status, phase, message);
	unit.content_family = Some(content_family.to_string());
	unit
}

fn summarize(
	units: &[CommonProbeUnit],
	full_denominator: bool,
	elapsed_ms: u64,
) -> CommonProbeSummary {
	let mut summary = CommonProbeSummary {
		expected_units: COMMON_APPLICABILITY_UNIT_COUNT,
		classified_units: units.len(),
		full_denominator,
		elapsed_ms,
		..CommonProbeSummary::default()
	};
	for unit in units {
		match unit.status {
			CommonProbeStatus::AcceptedEquivalent => summary.accepted_equivalent += 1,
			CommonProbeStatus::ManualResolutionRequired => {
				summary.manual_resolution_required += 1;
				summary.review_required += 1;
			}
			CommonProbeStatus::SemanticMismatch => {
				summary.semantic_mismatch += 1;
				summary.review_required += 1;
			}
			status if status.is_failure() => summary.failed += 1,
			_ => {}
		}
		if unit.legacy_accepted {
			summary.legacy_accepted += 1;
			summary.legacy_accepted_preserved += usize::from(unit.publishable);
		}
	}
	summary
}

fn common_gate_passed(summary: &CommonProbeSummary) -> bool {
	summary.classified_units == summary.expected_units
		&& summary.failed == 0
		&& summary.legacy_accepted_preserved == summary.legacy_accepted
}

fn common_gate_result(summary: &CommonProbeSummary) -> Option<bool> {
	summary
		.full_denominator
		.then(|| common_gate_passed(summary))
}

fn summarize_families(units: &[CommonProbeUnit]) -> Vec<CommonProbeFamilySummary> {
	let mut families = BTreeMap::<String, CommonProbeFamilySummary>::new();
	for unit in units {
		let family = unit
			.content_family
			.clone()
			.unwrap_or_else(|| "unclassified".to_string());
		let summary = families
			.entry(family.clone())
			.or_insert(CommonProbeFamilySummary {
				content_family: family,
				units: 0,
				accepted_equivalent: 0,
				review_required: false,
			});
		summary.units += 1;
		summary.accepted_equivalent += usize::from(unit.status.is_accepted());
		summary.review_required |= !unit.status.is_accepted();
	}
	families.into_values().collect()
}

fn write_report(
	output_dir: &Path,
	report: &CommonApplicabilityReport,
) -> Result<(), Box<dyn std::error::Error>> {
	fs::create_dir_all(output_dir)?;
	fs::write(
		output_dir.join("common-applicability.json"),
		serde_json::to_vec_pretty(report)?,
	)?;
	fs::write(
		output_dir.join("common-applicability.md"),
		render_markdown(report),
	)?;
	Ok(())
}

fn render_markdown(report: &CommonApplicabilityReport) -> String {
	let summary = &report.summary;
	let gate = match summary.gate_passed {
		Some(true) => "passed",
		Some(false) => "failed",
		None => "not evaluated (filtered run)",
	};
	let mut output = format!(
		"# Common applicability probe\n\nGate: **{}** | classified: {}/{} | accepted equivalent: {} | manual resolution: {} | semantic mismatch: {} | failed: {} | elapsed: {} ms\n\n",
		gate,
		summary.classified_units,
		summary.expected_units,
		summary.accepted_equivalent,
		summary.manual_resolution_required,
		summary.semantic_mismatch,
		summary.failed,
		summary.elapsed_ms,
	);
	output.push_str(
		"| Case | Module | Family | Legacy | Structured | Active/base | Missing keys | Extra keys | Conflicts |\n",
	);
	output.push_str("| --- | --- | --- | --- | --- | ---: | ---: | ---: | --- |\n");
	for unit in &report.units {
		let conflict_kinds = unit
			.conflicts
			.iter()
			.map(|conflict| conflict.kind.as_str())
			.collect::<BTreeSet<_>>()
			.into_iter()
			.collect::<Vec<_>>()
			.join(", ");
		output.push_str(&format!(
			"| {} | `{}` | `{}` | {} | {} | {}/{} | {} | {} | {} |\n",
			unit.case_id,
			unit.module_prefix,
			unit.content_family.as_deref().unwrap_or("unclassified"),
			unit.legacy_verdict,
			unit.status.as_str(),
			unit.active_definitions,
			unit.base_definitions,
			unit.missing_top_level_keys.len(),
			unit.extra_top_level_keys.len(),
			conflict_kinds,
		));
	}
	output
}

fn duration_ms(duration: std::time::Duration) -> u64 {
	u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::content_family::{MergePolicies, OneSidedRemovalPolicy};
	use foch_language::analyzer::parser::parse_clausewitz_content;
	use std::path::PathBuf;

	fn parse(source: &str) -> AstFile {
		let parsed = parse_clausewitz_content(PathBuf::from("common/test.txt"), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast
	}

	fn write_file(root: &Path, relative: &str, contents: &str) {
		let path = root.join(relative);
		fs::create_dir_all(path.parent().expect("fixture path has parent")).unwrap();
		fs::write(path, contents).unwrap();
	}

	#[test]
	fn folder_module_view_merges_definitions_across_file_names() {
		let temp = tempfile::tempdir().unwrap();
		let base = temp.path().join("base");
		let left = temp.path().join("left");
		let right = temp.path().join("right");
		let human_root = temp.path().join("human");
		write_file(
			&base,
			"common/buildings/00_base.txt",
			"temple = { cost = 100 }\nmarket = { cost = 50 }\n",
		);
		write_file(
			&left,
			"common/buildings/left.txt",
			"temple = { cost = 100 manpower = 1 }\n",
		);
		write_file(
			&right,
			"common/buildings/right.txt",
			"temple = { cost = 100 tax = 1 }\n",
		);
		write_file(
			&human_root,
			"common/buildings/zzz_compatch.txt",
			"temple = { cost = 100 manpower = 1 tax = 1 }\n",
		);

		let mut cache = CommonModuleViewBuilder::default();
		let base_view = cache.view(&[&base], "common/buildings").unwrap();
		let left_view = cache.view(&[&base, &left], "common/buildings").unwrap();
		let right_view = cache.view(&[&base, &right], "common/buildings").unwrap();
		assert_eq!(
			changed_top_level_keys(&base_view, &left_view, &right_view),
			BTreeSet::from(["temple".to_string()])
		);
		let merged = merge_clausewitz_definition_module(
			&base_view,
			&left_view,
			&right_view,
			&MergePolicies::default(),
		)
		.unwrap();
		assert_eq!(merged.base_definitions(), 2);
		assert!(merged.conflicts().is_empty(), "{:?}", merged.conflicts());
		let candidate = merged.tentative_ast();
		let human = cache
			.view(&[&base, &left, &right, &human_root], "common/buildings")
			.unwrap();
		let diff = semantic_atom_diff_ast(candidate, &human);

		assert!(diff.left_only.is_empty(), "{:?}", diff.left_only);
		assert!(diff.right_only.is_empty(), "{:?}", diff.right_only);
	}

	#[test]
	fn shared_module_merge_honors_nonstandard_one_sided_removal_policy() {
		let base = parse(
			"institution = { potential = { OR = { trade_goods = ivory trade_goods = cloves } } }\n",
		);
		let right = parse(
			"institution = { potential = { OR = { trade_goods = ivory trade_goods = fur } } }\n",
		);
		let policies = MergePolicies {
			one_sided_removal: OneSidedRemovalPolicy::PreserveBooleanAlternatives,
			..MergePolicies::default()
		};

		let outcome = merge_clausewitz_definition_module(&base, &base, &right, &policies)
			.expect("policy-aware partitioned merge");
		assert_eq!(outcome.copy_through_definitions(), 0);
		assert_eq!(outcome.structured_definitions(), 1);
		let diff = semantic_atom_diff_ast(
			outcome.tentative_ast(),
			&parse(
				"institution = { potential = { OR = { trade_goods = ivory trade_goods = cloves trade_goods = fur } } }\n",
			),
		);
		assert!(diff.left_only.is_empty(), "{:?}", diff.left_only);
		assert!(diff.right_only.is_empty(), "{:?}", diff.right_only);
	}

	#[test]
	fn comparison_reuses_equal_definitions_and_normalizes_changed_control_flow() {
		let candidate = parse(
			"same = { value = 1 }\nflow = { if = { limit = { flag = yes } value = 1 } else = { value = 2 } }\n",
		);
		let human = parse(
			"same = { value = 1 }\nflow = { if = { limit = { NOT = { flag = yes } } value = 2 } else = { value = 1 } }\n",
		);
		let comparison = canonicalize_comparison_pair(
			&candidate,
			&human,
			&MergePolicies::default(),
			"common/test",
		);

		assert_eq!(comparison.reused_definitions, 1);
		assert_eq!(comparison.normalized_definitions, 1);
		assert!(
			comparison.conflicts.is_empty(),
			"{:?}",
			comparison.conflicts
		);
		let diff = semantic_atom_diff_ast(&comparison.candidate, &comparison.human);
		assert!(diff.left_only.is_empty(), "{:?}", diff.left_only);
		assert!(diff.right_only.is_empty(), "{:?}", diff.right_only);
	}

	#[test]
	fn covering_replace_path_clears_earlier_module_files() {
		let temp = tempfile::tempdir().unwrap();
		let base = temp.path().join("base");
		let replacement = temp.path().join("replacement");
		write_file(&base, "common/religions/base.txt", "christian = { }\n");
		write_file(
			&replacement,
			"descriptor.mod",
			"name = \"replacement\"\nreplace_path = \"common/religions\"\n",
		);
		write_file(
			&replacement,
			"common/religions/replacement.txt",
			"muslim = { }\n",
		);

		let view = CommonModuleViewBuilder::default()
			.view(&[&base, &replacement], "common/religions")
			.unwrap();
		assert_eq!(
			top_level_assignment_keys(&view),
			BTreeSet::from(["muslim".to_string()])
		);
	}

	#[test]
	fn filtered_summary_never_reports_the_full_gate() {
		let filtered = summarize(&[], false, 0);
		assert_eq!(filtered.expected_units, COMMON_APPLICABILITY_UNIT_COUNT);
		assert_eq!(filtered.classified_units, 0);
		assert_eq!(common_gate_result(&filtered), None);

		let full = summarize(&[], true, 0);
		assert_eq!(common_gate_result(&full), Some(false));
	}
}
