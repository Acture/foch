use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use foch_core::domain::descriptor::load_descriptor;
use foch_engine::merge_clausewitz_files;
use foch_language::analyzer::content_family::{GameProfile, MergeKeySource, MergePolicies};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstFile, AstStatement, parse_clausewitz_file};
use serde::{Deserialize, Serialize};

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

pub const COMMON_APPLICABILITY_SCHEMA: &str = "1.0.0";
pub const COMMON_APPLICABILITY_UNIT_COUNT: usize = 12;

pub struct CommonApplicabilityOptions<'a> {
	pub dataset_root: &'a Path,
	pub output_dir: &'a Path,
	pub legacy_baseline: &'a Path,
	pub game: &'a Eu4GameDiscovery,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CommonProbeTimings {
	pub view_ms: u64,
	pub merge_ms: u64,
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
	pub base_definitions: usize,
	pub active_definitions: usize,
	pub missing_top_level_keys: Vec<String>,
	pub extra_top_level_keys: Vec<String>,
	pub semantic_diff_from_human: Option<SemanticAtomDiff>,
	pub conflicts: Vec<CommonProbeConflict>,
	pub diagnostics: Vec<CommonProbeDiagnostic>,
	pub timings: CommonProbeTimings,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CommonProbeSummary {
	pub expected_units: usize,
	pub classified_units: usize,
	pub accepted_equivalent: usize,
	pub manual_resolution_required: usize,
	pub semantic_mismatch: usize,
	pub failed: usize,
	pub legacy_accepted: usize,
	pub legacy_accepted_preserved: usize,
	pub review_required: usize,
	pub gate_passed: bool,
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

#[derive(Clone, Debug)]
struct ParsedModuleFile {
	statements: Vec<AstStatement>,
	diagnostics: Vec<CommonProbeDiagnostic>,
}

#[derive(Clone, Debug)]
struct ParsedModuleLayer {
	replace_namespace: bool,
	files: BTreeMap<String, Arc<ParsedModuleFile>>,
}

struct PreparedMergeInputs {
	base: AstFile,
	left: AstFile,
	right: AstFile,
	active_keys: BTreeSet<String>,
	base_definitions: usize,
	whole_module: bool,
}

#[derive(Default)]
struct FolderModuleCache {
	layers: BTreeMap<(PathBuf, String), Arc<ParsedModuleLayer>>,
	views: BTreeMap<(Vec<PathBuf>, String), Arc<AstFile>>,
}

pub fn run_common_applicability_probe(
	options: &CommonApplicabilityOptions<'_>,
) -> Result<CommonApplicabilityReport, Box<dyn std::error::Error>> {
	let started = Instant::now();
	let targets = load_targets(options.legacy_baseline)?;
	if targets.len() != COMMON_APPLICABILITY_UNIT_COUNT {
		return Err(format!(
			"common applicability denominator drifted: expected {}, found {}",
			COMMON_APPLICABILITY_UNIT_COUNT,
			targets.len()
		)
		.into());
	}

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

	let mut cache = FolderModuleCache::default();
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
		summary: summarize(&units, duration_ms(started.elapsed())),
		families: summarize_families(&units),
		units,
	};
	report.summary.gate_passed = common_gate_passed(&report.summary);
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

fn evaluate_unit(
	options: &CommonApplicabilityOptions<'_>,
	target: &LegacyBaselineUnit,
	snapshot: &LoadedSnapshot,
	cache: &mut FolderModuleCache,
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
			base_definitions: 0,
			active_definitions: 0,
			missing_top_level_keys: Vec::new(),
			extra_top_level_keys: Vec::new(),
			semantic_diff_from_human: None,
			conflicts: Vec::new(),
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
	let prepared = prepare_merge_inputs(&base, &left, &right);

	let merge_started = Instant::now();
	let outcome = match merge_clausewitz_files(
		&prepared.base,
		&prepared.left,
		&prepared.right,
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
				base_definitions: prepared.base_definitions,
				active_definitions: prepared.active_keys.len(),
				missing_top_level_keys: Vec::new(),
				extra_top_level_keys: Vec::new(),
				semantic_diff_from_human: None,
				conflicts: Vec::new(),
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
	let candidate = compose_module_candidate(&base, &prepared, outcome.tentative_ast());
	let diff = semantic_atom_diff_ast(&candidate, &human);
	let candidate_keys = top_level_assignment_keys(&candidate);
	let human_keys = top_level_assignment_keys(&human);
	let missing_top_level_keys = human_keys.difference(&candidate_keys).cloned().collect();
	let extra_top_level_keys = candidate_keys.difference(&human_keys).cloned().collect();
	let conflicts = outcome
		.conflict_summaries()
		.into_iter()
		.map(|conflict| CommonProbeConflict {
			kind: conflict.kind.to_string(),
			detail: conflict.detail,
		})
		.collect::<Vec<_>>();
	let equivalent = diff.left_only.is_empty() && diff.right_only.is_empty();
	let status = if conflicts.is_empty() && equivalent {
		CommonProbeStatus::AcceptedEquivalent
	} else if !conflicts.is_empty() {
		CommonProbeStatus::ManualResolutionRequired
	} else {
		CommonProbeStatus::SemanticMismatch
	};
	let timings = outcome.timings();
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
		base_definitions: prepared.base_definitions,
		active_definitions: prepared.active_keys.len(),
		missing_top_level_keys,
		extra_top_level_keys,
		semantic_diff_from_human: Some(diff),
		conflicts,
		diagnostics: Vec::new(),
		timings: CommonProbeTimings {
			view_ms,
			merge_ms,
			matcher_ns: timings.matcher_ns,
			pcs_ns: timings.pcs_ns,
			policy_ns: timings.policy_ns,
		},
	}
}

fn prepare_merge_inputs(base: &AstFile, left: &AstFile, right: &AstFile) -> PreparedMergeInputs {
	let base_definitions = top_level_assignment_keys(base).len();
	let base_map = definition_map(base);
	let left_map = definition_map(left);
	let right_map = definition_map(right);
	let active_keys = base_map
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
		.collect::<BTreeSet<_>>();
	let whole_module = [base, left, right].iter().any(|ast| {
		ast.statements
			.iter()
			.any(|statement| matches!(statement, AstStatement::Item { .. }))
	});
	if whole_module {
		return PreparedMergeInputs {
			base: base.clone(),
			left: left.clone(),
			right: right.clone(),
			active_keys: base_map
				.keys()
				.chain(left_map.keys())
				.chain(right_map.keys())
				.map(|key| (*key).to_string())
				.collect(),
			base_definitions,
			whole_module,
		};
	}
	PreparedMergeInputs {
		base: select_definitions(base, &active_keys),
		left: select_definitions(left, &active_keys),
		right: select_definitions(right, &active_keys),
		active_keys,
		base_definitions,
		whole_module,
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

fn select_definitions(ast: &AstFile, keys: &BTreeSet<String>) -> AstFile {
	AstFile {
		path: ast.path.clone(),
		statements: ast
			.statements
			.iter()
			.filter(|statement| match statement {
				AstStatement::Assignment { key, .. } => keys.contains(key),
				AstStatement::Item { .. } => true,
				AstStatement::Comment { .. } => false,
			})
			.cloned()
			.collect(),
	}
}

fn compose_module_candidate(
	base: &AstFile,
	prepared: &PreparedMergeInputs,
	merged_active: &AstFile,
) -> AstFile {
	if prepared.whole_module {
		return merged_active.clone();
	}
	let mut statements = base
		.statements
		.iter()
		.filter(|statement| match statement {
			AstStatement::Assignment { key, .. } => !prepared.active_keys.contains(key),
			AstStatement::Item { .. } => false,
			AstStatement::Comment { .. } => false,
		})
		.cloned()
		.collect::<Vec<_>>();
	statements.extend(merged_active.statements.iter().cloned());
	statements.sort_by(compare_top_level_statements);
	AstFile {
		path: base.path.clone(),
		statements,
	}
}

fn compare_top_level_statements(left: &AstStatement, right: &AstStatement) -> std::cmp::Ordering {
	match (left, right) {
		(
			AstStatement::Assignment { key: left, .. },
			AstStatement::Assignment { key: right, .. },
		) => left.cmp(right),
		(AstStatement::Assignment { .. }, _) => std::cmp::Ordering::Less,
		(_, AstStatement::Assignment { .. }) => std::cmp::Ordering::Greater,
		(AstStatement::Item { .. }, AstStatement::Comment { .. }) => std::cmp::Ordering::Less,
		(AstStatement::Comment { .. }, AstStatement::Item { .. }) => std::cmp::Ordering::Greater,
		_ => std::cmp::Ordering::Equal,
	}
}

impl FolderModuleCache {
	fn view(
		&mut self,
		roots: &[&Path],
		module_prefix: &str,
	) -> Result<Arc<AstFile>, Vec<CommonProbeDiagnostic>> {
		let key = (
			roots.iter().map(|root| root.to_path_buf()).collect(),
			module_prefix.to_string(),
		);
		if let Some(view) = self.views.get(&key) {
			return Ok(Arc::clone(view));
		}

		let mut visible = BTreeMap::<String, Arc<ParsedModuleFile>>::new();
		for root in roots {
			let layer = self.layer(root, module_prefix)?;
			if layer.replace_namespace {
				visible.clear();
			}
			for (relative, file) in &layer.files {
				visible.insert(relative.clone(), Arc::clone(file));
			}
		}

		let diagnostics = visible
			.values()
			.flat_map(|file| file.diagnostics.iter().cloned())
			.collect::<Vec<_>>();
		if !diagnostics.is_empty() {
			return Err(diagnostics);
		}

		let mut definitions = BTreeMap::<String, AstStatement>::new();
		let mut items = Vec::new();
		for file in visible.values() {
			for statement in &file.statements {
				match statement {
					AstStatement::Assignment { key, .. } => {
						definitions.insert(key.clone(), statement.clone());
					}
					AstStatement::Item { .. } => items.push(statement.clone()),
					AstStatement::Comment { .. } => {}
				}
			}
		}
		let mut statements = definitions.into_values().collect::<Vec<_>>();
		statements.extend(items);
		let view = Arc::new(AstFile {
			path: PathBuf::from(format!("{module_prefix}/__foch_common_probe__.txt")),
			statements,
		});
		self.views.insert(key, Arc::clone(&view));
		Ok(view)
	}

	fn layer(
		&mut self,
		root: &Path,
		module_prefix: &str,
	) -> Result<Arc<ParsedModuleLayer>, Vec<CommonProbeDiagnostic>> {
		let key = (root.to_path_buf(), module_prefix.to_string());
		if let Some(layer) = self.layers.get(&key) {
			return Ok(Arc::clone(layer));
		}
		let layer = Arc::new(parse_module_layer(root, module_prefix)?);
		self.layers.insert(key, Arc::clone(&layer));
		Ok(layer)
	}
}

fn parse_module_layer(
	root: &Path,
	module_prefix: &str,
) -> Result<ParsedModuleLayer, Vec<CommonProbeDiagnostic>> {
	let replace_namespace = match layer_replaces_module(root, module_prefix) {
		Ok(replace) => replace,
		Err(diagnostic) => return Err(vec![diagnostic]),
	};
	let directory = root.join(module_prefix);
	if !directory.exists() {
		return Ok(ParsedModuleLayer {
			replace_namespace,
			files: BTreeMap::new(),
		});
	}
	if !directory.is_dir() {
		return Err(vec![CommonProbeDiagnostic {
			phase: "module_input".to_string(),
			path: Some(directory.display().to_string()),
			message: "module prefix is not a directory".to_string(),
		}]);
	}

	let mut files = BTreeMap::new();
	for entry in walkdir::WalkDir::new(&directory) {
		let entry = match entry {
			Ok(entry) => entry,
			Err(error) => {
				return Err(vec![CommonProbeDiagnostic {
					phase: "module_input".to_string(),
					path: error.path().map(|path| path.display().to_string()),
					message: error.to_string(),
				}]);
			}
		};
		if !entry.file_type().is_file()
			|| entry
				.path()
				.extension()
				.and_then(|extension| extension.to_str())
				.is_none_or(|extension| !extension.eq_ignore_ascii_case("txt"))
		{
			continue;
		}
		let path = entry.into_path();
		let relative = relative_path(root, &path);
		let parsed = parse_clausewitz_file(&path);
		let diagnostics = parsed
			.diagnostics
			.into_iter()
			.map(|diagnostic| CommonProbeDiagnostic {
				phase: "parse".to_string(),
				path: Some(relative.clone()),
				message: diagnostic.message,
			})
			.collect();
		files.insert(
			relative,
			Arc::new(ParsedModuleFile {
				statements: parsed.ast.statements,
				diagnostics,
			}),
		);
	}
	Ok(ParsedModuleLayer {
		replace_namespace,
		files,
	})
}

fn layer_replaces_module(root: &Path, module_prefix: &str) -> Result<bool, CommonProbeDiagnostic> {
	let descriptor_path = root.join("descriptor.mod");
	if !descriptor_path.is_file() {
		return Ok(false);
	}
	let descriptor = load_descriptor(&descriptor_path).map_err(|error| CommonProbeDiagnostic {
		phase: "descriptor".to_string(),
		path: Some(descriptor_path.display().to_string()),
		message: error.to_string(),
	})?;
	Ok(descriptor
		.replace_path
		.iter()
		.any(|replace_path| replace_path_covers_prefix(replace_path, module_prefix)))
}

fn replace_path_covers_prefix(replace_path: &str, module_prefix: &str) -> bool {
	let normalized = replace_path.trim().replace('\\', "/");
	let replace_path = normalized.trim_matches('/');
	let module_prefix = module_prefix.trim_matches('/');
	!replace_path.is_empty()
		&& (replace_path == module_prefix
			|| module_prefix
				.strip_prefix(replace_path)
				.is_some_and(|suffix| suffix.starts_with('/')))
}

fn common_folder_prefix(relative_path: &str) -> Option<String> {
	let mut components = relative_path.split('/');
	let common = components.next()?;
	let folder = components.next()?;
	(common == "common" && !folder.is_empty()).then(|| format!("common/{folder}"))
}

fn relative_path(root: &Path, path: &Path) -> String {
	path.strip_prefix(root)
		.unwrap_or(path)
		.to_string_lossy()
		.replace('\\', "/")
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
		base_definitions: 0,
		active_definitions: 0,
		missing_top_level_keys: Vec::new(),
		extra_top_level_keys: Vec::new(),
		semantic_diff_from_human: None,
		conflicts: Vec::new(),
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

fn summarize(units: &[CommonProbeUnit], elapsed_ms: u64) -> CommonProbeSummary {
	let mut summary = CommonProbeSummary {
		expected_units: COMMON_APPLICABILITY_UNIT_COUNT,
		classified_units: units.len(),
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
	let mut output = format!(
		"# Common applicability probe\n\nGate: **{}** | classified: {}/{} | accepted equivalent: {} | manual resolution: {} | semantic mismatch: {} | failed: {} | elapsed: {} ms\n\n",
		if summary.gate_passed {
			"passed"
		} else {
			"failed"
		},
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
	use foch_language::analyzer::content_family::MergePolicies;

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

		let mut cache = FolderModuleCache::default();
		let base_view = cache.view(&[&base], "common/buildings").unwrap();
		let left_view = cache.view(&[&base, &left], "common/buildings").unwrap();
		let right_view = cache.view(&[&base, &right], "common/buildings").unwrap();
		let prepared = prepare_merge_inputs(&base_view, &left_view, &right_view);
		assert_eq!(prepared.base_definitions, 2);
		assert_eq!(prepared.active_keys, BTreeSet::from(["temple".to_string()]));
		let merged = merge_clausewitz_files(
			&prepared.base,
			&prepared.left,
			&prepared.right,
			&MergePolicies::default(),
		)
		.unwrap();
		let candidate = compose_module_candidate(
			&base_view,
			&prepared,
			merged.resolved_ast().expect("independent edits resolve"),
		);
		let human = cache
			.view(&[&base, &left, &right, &human_root], "common/buildings")
			.unwrap();
		let diff = semantic_atom_diff_ast(&candidate, &human);

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

		let view = FolderModuleCache::default()
			.view(&[&base, &replacement], "common/religions")
			.unwrap();
		assert_eq!(
			top_level_assignment_keys(&view),
			BTreeSet::from(["muslim".to_string()])
		);
	}
}
