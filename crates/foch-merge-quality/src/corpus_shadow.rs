use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use foch_engine::installed_base_snapshot_identity;
use foch_language::analyzer::content_family::GameProfile;
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue, parse_clausewitz_file};
use serde::{Deserialize, Serialize};

use crate::common_module::{CommonModuleViewBuilder, normalize_module_comparison};
use crate::config::Eu4GameDiscovery;
use crate::corpus::{OracleAssessment, assess_oracle_candidate};
use crate::dataset::{
	DatasetPaths, IdentifiedRecord, ObservationRecord, SCORER_VERSION, SnapshotRecord,
	append_unique_many, now_rfc3339, read_jsonl, stable_id,
};
use crate::lifecycle::{executable_hash, scorer_config_hash};
use crate::object_store::ObjectStore;
use crate::orchestrate::FileRecord;
use crate::score::{
	AdjudicationBinding, Adjudications, Resolution, ScoreCache, ScoreFileRequest, SemanticAtomDiff,
	SourceMod, classify_resolution, definition_module_policy_for_path, reference_output_files,
	score_file_with_cache_and_basegame, scoring_reference_units, semantic_atom_diff,
	semantic_atom_diff_ast, write_playset,
};
use crate::shadow::{
	ShadowCompareRequest, ShadowComparisonReport, ShadowDiagnostic, ShadowDiagnosticKind,
	ShadowRunRecord, output_content_hash, run_shadow_comparison,
};

pub const CORPUS_SHADOW_SCHEMA: &str = "1.0.0";
pub const CORPUS_SHADOW_REPORT_SCHEMA: &str = "3.0.0";

pub struct CorpusShadowOptions<'a> {
	pub dataset_root: &'a Path,
	pub output_dir: &'a Path,
	pub game: &'a Eu4GameDiscovery,
	pub executable: &'a Path,
	pub timeout: Duration,
	pub force: bool,
	pub record: bool,
}

pub struct CorpusShadowCorpusOptions<'a> {
	pub shadow: CorpusShadowOptions<'a>,
	pub legacy_baseline: &'a Path,
	pub expected_verdicts: &'a Path,
	pub candidates: &'a BTreeSet<CorpusShadowSelection>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct CorpusShadowSelection {
	pub case_id: String,
	pub relative_path: String,
}

impl FromStr for CorpusShadowSelection {
	type Err = String;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let (case_id, relative_path) = value
			.split_once(':')
			.ok_or_else(|| "candidate must use CASE_ID:RELATIVE_PATH".to_string())?;
		if case_id.is_empty() || relative_path.is_empty() {
			return Err("candidate must use non-empty CASE_ID:RELATIVE_PATH".to_string());
		}
		Ok(Self {
			case_id: case_id.to_string(),
			relative_path: relative_path.replace('\\', "/"),
		})
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CorpusShadowTarget {
	pub schema: String,
	pub unit_id: String,
	pub snapshot_id: String,
	pub case_id: String,
	pub relative_path: String,
	pub source_mod_ids: Vec<String>,
	pub game_version: String,
	pub steam_build_id: Option<u64>,
	pub base_snapshot_identity: String,
	pub executable_hash: String,
	pub scorer_config_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventSafetyAssessment {
	pub parse_ok: bool,
	pub diagnostic_count: usize,
	pub namespaces: Vec<String>,
	pub event_ids: Vec<String>,
	pub option_ids: Vec<String>,
	pub duplicate_event_ids: Vec<String>,
	pub duplicate_option_ids: Vec<String>,
	pub orphan_control_flow_paths: Vec<String>,
	pub control_flow_shape: Vec<String>,
	pub human_parse_ok: bool,
	pub namespaces_include_human: Option<bool>,
	pub event_ids_include_human: Option<bool>,
	pub option_ids_include_human: Option<bool>,
	pub control_flow_matches_human: Option<bool>,
}

impl EventSafetyAssessment {
	fn passed(&self) -> bool {
		self.parse_ok
			&& self.duplicate_event_ids.is_empty()
			&& self.duplicate_option_ids.is_empty()
			&& self.orphan_control_flow_paths.is_empty()
			&& self.human_parse_ok
			&& self.namespaces_include_human != Some(false)
			&& self.event_ids_include_human != Some(false)
			&& self.option_ids_include_human != Some(false)
			&& self.control_flow_matches_human != Some(false)
	}
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShadowArmAssessment {
	pub kernel: String,
	pub status: String,
	pub output_valid: bool,
	pub elapsed_ms: u64,
	pub output_hash: Option<String>,
	pub score: Option<FileRecord>,
	pub semantic_diff_from_human: Option<SemanticAtomDiff>,
	pub semantic_diff_from_sources: BTreeMap<String, SemanticAtomDiff>,
	pub semantic_diff_from_base: Option<SemanticAtomDiff>,
	pub event_safety: Option<EventSafetyAssessment>,
	pub diagnostics: Vec<ShadowDiagnostic>,
	pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusShadowOutcome {
	Improved,
	Regressed,
	UnchangedAccepted,
	UnchangedRejected,
	NeedsReview,
	SafetyFailed,
	StructuredUnsupported,
	StructuredConflict,
	Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusShadowDisposition {
	LegacyRetained,
	CandidateEvaluated,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorpusShadowUnitRecord {
	pub schema: String,
	pub record_id: String,
	pub target: CorpusShadowTarget,
	pub comparison_id: String,
	pub observed_at: String,
	pub human_resolution: Option<Resolution>,
	pub legacy: ShadowArmAssessment,
	pub structured: ShadowArmAssessment,
	pub structured_vs_legacy: Option<SemanticAtomDiff>,
	pub outcome: CorpusShadowOutcome,
}

impl IdentifiedRecord for CorpusShadowUnitRecord {
	fn record_id(&self) -> &str {
		&self.record_id
	}
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CorpusShadowSummary {
	pub total_units: usize,
	pub completed_units: usize,
	pub legacy_retained: usize,
	pub candidate_evaluated: usize,
	pub non_gui_units: usize,
	pub improved: usize,
	pub regressed: usize,
	pub unchanged_accepted: usize,
	pub unchanged_rejected: usize,
	pub needs_review: usize,
	pub safety_failed: usize,
	pub structured_unsupported: usize,
	pub structured_conflict: usize,
	pub failed: usize,
	pub legacy_strict_accepted: usize,
	pub legacy_adjudicated_accepted: usize,
	pub candidate_strict_accepted: usize,
	pub candidate_adjudicated_accepted: usize,
	pub projected_strict_accepted: usize,
	pub projected_adjudicated_accepted: usize,
	pub legacy_strict_accepted_lost: usize,
	pub legacy_adjudicated_accepted_lost: usize,
	pub legacy_strict_accepted_non_gui: usize,
	pub legacy_adjudicated_accepted_non_gui: usize,
	pub candidate_strict_accepted_non_gui: usize,
	pub candidate_adjudicated_accepted_non_gui: usize,
	pub projected_strict_accepted_non_gui: usize,
	pub projected_adjudicated_accepted_non_gui: usize,
	pub legacy_strict_accepted_non_gui_lost: usize,
	pub legacy_adjudicated_accepted_non_gui_lost: usize,
	pub legacy_elapsed_ms: u64,
	pub structured_elapsed_ms: u64,
	pub supported_runtime_ratio_milli: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LegacyBaselineIdentity {
	pub schema: String,
	pub scorer_version: String,
	pub baseline_content_id: String,
	pub expected_content_id: String,
	pub unit_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct LegacyBaselineArtifact {
	schema: String,
	scorer_version: String,
	expected_content_id: String,
	units: Vec<LegacyBaselineUnit>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct LegacyBaselineUnit {
	case_id: String,
	score: FileRecord,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorpusShadowProjectionUnit {
	pub target: CorpusShadowTarget,
	pub disposition: CorpusShadowDisposition,
	pub legacy_baseline: FileRecord,
	pub candidate: Option<CorpusShadowUnitRecord>,
	pub projected_strict_accepted: bool,
	pub projected_adjudicated_accepted: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorpusShadowReport {
	pub schema: String,
	pub generated_at: String,
	pub legacy_baseline: LegacyBaselineIdentity,
	pub targets: Vec<CorpusShadowTarget>,
	pub units: Vec<CorpusShadowProjectionUnit>,
	pub summary: CorpusShadowSummary,
}

pub(crate) struct LoadedSnapshot {
	pub(crate) snapshot: SnapshotRecord,
	pub(crate) compatch: PathBuf,
	pub(crate) source_dirs: Vec<PathBuf>,
}

struct TargetIdentity<'a> {
	snapshot: &'a SnapshotRecord,
	relative_path: &'a str,
	source_mod_ids: &'a [String],
	base_snapshot_identity: &'a str,
	executable_hash: &'a str,
	scorer_config_hash: &'a str,
}

pub fn run_case(
	options: &CorpusShadowOptions<'_>,
	case_id: &str,
	retained_path: &str,
) -> Result<CorpusShadowUnitRecord, Box<dyn std::error::Error>> {
	validate_options(options)?;
	let paths = DatasetPaths::new(options.dataset_root);
	let snapshots = latest_snapshots(&paths)?;
	let snapshot = snapshots
		.into_iter()
		.find(|snapshot| snapshot.case_id == case_id)
		.ok_or_else(|| format!("no latest snapshot for case {case_id}"))?;
	validate_snapshot_game(&snapshot, options.game)?;
	let store = ObjectStore::new(&paths.objects, &paths.work);
	let mut verified = HashSet::new();
	let loaded = load_snapshot(&store, snapshot, &mut verified)?;
	let identity = run_identity(options)?;
	let mut score_cache = ScoreCache::new();
	let normalized = retained_path.replace('\\', "/");
	let targets = discover_targets(
		&loaded,
		options.game,
		&identity,
		&mut score_cache,
		Some(&normalized),
	)?;
	let target = targets
		.into_iter()
		.find(|target| target.relative_path == normalized)
		.ok_or_else(|| {
			format!("{case_id}/{normalized} is not a multi-source corpus scoring unit")
		})?;
	fs::create_dir_all(options.output_dir)?;
	let record = run_target(options, &loaded, target, &mut score_cache)?;
	write_single_report(options.output_dir, &record)?;
	if options.record {
		append_unique_many(&paths.shadow_measurements, std::slice::from_ref(&record))?;
	}
	Ok(record)
}

pub fn run_corpus(
	options: &CorpusShadowCorpusOptions<'_>,
	expect_multi_source_units: Option<usize>,
) -> Result<CorpusShadowReport, Box<dyn std::error::Error>> {
	validate_options(&options.shadow)?;
	if options.candidates.is_empty() {
		return Err("shadow-corpus requires at least one explicit --candidate".into());
	}
	let paths = DatasetPaths::new(options.shadow.dataset_root);
	let observations = read_jsonl::<ObservationRecord>(&paths.observations)?;
	let snapshots = latest_snapshots(&paths)?;
	let store = ObjectStore::new(&paths.objects, &paths.work);
	let identity = run_identity(&options.shadow)?;
	let mut verified = HashSet::new();
	let mut score_cache = ScoreCache::new();
	let mut loaded_by_snapshot = HashMap::new();
	let mut targets = Vec::new();
	for snapshot in snapshots {
		let Some(assessment) = oracle_assessment(&snapshot, &observations) else {
			continue;
		};
		if !assessment.is_scorable() {
			continue;
		}
		validate_snapshot_game(&snapshot, options.shadow.game)?;
		let loaded = load_snapshot(&store, snapshot, &mut verified)?;
		targets.extend(discover_targets(
			&loaded,
			options.shadow.game,
			&identity,
			&mut score_cache,
			None,
		)?);
		loaded_by_snapshot.insert(loaded.snapshot.snapshot_id.clone(), loaded);
	}
	targets.sort_by(|left, right| {
		(&left.case_id, &left.relative_path).cmp(&(&right.case_id, &right.relative_path))
	});
	if let Some(expected) = expect_multi_source_units
		&& targets.len() != expected
	{
		return Err(format!(
			"multi-source denominator drifted: expected {expected}, discovered {}",
			targets.len()
		)
		.into());
	}
	let discovered = targets
		.iter()
		.map(|target| CorpusShadowSelection {
			case_id: target.case_id.clone(),
			relative_path: target.relative_path.clone(),
		})
		.collect::<BTreeSet<_>>();
	let missing_candidates = options
		.candidates
		.difference(&discovered)
		.map(|selection| format!("{}:{}", selection.case_id, selection.relative_path))
		.collect::<Vec<_>>();
	if !missing_candidates.is_empty() {
		return Err(format!(
			"candidate selection is not in the corpus denominator: {}",
			missing_candidates.join(", ")
		)
		.into());
	}
	let (legacy_baseline, baseline_scores) =
		load_legacy_baseline(options.legacy_baseline, options.expected_verdicts, &targets)?;
	fs::create_dir_all(options.shadow.output_dir)?;
	fs::write(
		options.shadow.output_dir.join("shadow-targets.json"),
		serde_json::to_vec_pretty(&targets)?,
	)?;

	let started = Instant::now();
	let mut units = Vec::with_capacity(targets.len());
	let mut candidate_records = Vec::with_capacity(options.candidates.len());
	let mut candidate_index = 0_usize;
	for target in targets.iter().cloned() {
		let selection = CorpusShadowSelection {
			case_id: target.case_id.clone(),
			relative_path: target.relative_path.clone(),
		};
		let baseline = baseline_scores
			.get(&selection)
			.ok_or_else(|| {
				format!(
					"Legacy baseline is missing {}/{}",
					target.case_id, target.relative_path
				)
			})?
			.clone();
		if !options.candidates.contains(&selection) {
			units.push(CorpusShadowProjectionUnit {
				target,
				disposition: CorpusShadowDisposition::LegacyRetained,
				projected_strict_accepted: strict_record_accepted(&baseline),
				projected_adjudicated_accepted: baseline.accepted_ok,
				legacy_baseline: baseline,
				candidate: None,
			});
			continue;
		}
		candidate_index += 1;
		let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
		let eta_ms = if candidate_index == 1 {
			0
		} else {
			elapsed_ms
				.checked_div((candidate_index - 1) as u64)
				.unwrap_or(0)
				.saturating_mul((options.candidates.len() - candidate_index + 1) as u64)
		};
		eprintln!(
			"[shadow-corpus] candidate {}/{} {}/{} elapsed_ms={} eta_ms={eta_ms}",
			candidate_index,
			options.candidates.len(),
			target.case_id,
			target.relative_path,
			elapsed_ms
		);
		let loaded = loaded_by_snapshot
			.get(&target.snapshot_id)
			.ok_or_else(|| format!("snapshot {} was not loaded", target.snapshot_id))?;
		let candidate = run_target(&options.shadow, loaded, target.clone(), &mut score_cache)?;
		verify_candidate_legacy_baseline(&candidate, &baseline)?;
		let projected_strict_accepted = candidate_projected_strict_accepted(&candidate);
		let projected_adjudicated_accepted = candidate_projected_adjudicated_accepted(&candidate);
		candidate_records.push(candidate.clone());
		units.push(CorpusShadowProjectionUnit {
			target,
			disposition: CorpusShadowDisposition::CandidateEvaluated,
			legacy_baseline: baseline,
			candidate: Some(candidate),
			projected_strict_accepted,
			projected_adjudicated_accepted,
		});
	}
	eprintln!(
		"[shadow-corpus] assembled {} units: candidates={} legacy_retained={} elapsed_ms={}",
		units.len(),
		candidate_records.len(),
		units.len().saturating_sub(candidate_records.len()),
		u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
	);
	let report = CorpusShadowReport {
		schema: CORPUS_SHADOW_REPORT_SCHEMA.to_string(),
		generated_at: now_rfc3339(),
		legacy_baseline,
		targets,
		summary: summarize(&units),
		units,
	};
	write_corpus_report(options.shadow.output_dir, &report)?;
	if options.shadow.record {
		append_unique_many(&paths.shadow_measurements, &candidate_records)?;
	}
	Ok(report)
}

fn load_legacy_baseline(
	baseline_path: &Path,
	expected_path: &Path,
	targets: &[CorpusShadowTarget],
) -> Result<
	(
		LegacyBaselineIdentity,
		BTreeMap<CorpusShadowSelection, FileRecord>,
	),
	Box<dyn std::error::Error>,
> {
	let baseline_bytes = fs::read(baseline_path)?;
	let expected_bytes = fs::read(expected_path)?;
	let baseline = serde_json::from_slice::<LegacyBaselineArtifact>(&baseline_bytes)?;
	let expected =
		serde_json::from_slice::<BTreeMap<String, BTreeMap<String, usize>>>(&expected_bytes)?;
	let expected_content_id = stable_id("legacy-expected-v1", &[&expected_bytes]);
	if baseline.schema != "1.0.0" {
		return Err(format!(
			"unsupported Legacy baseline schema {}; expected 1.0.0",
			baseline.schema
		)
		.into());
	}
	if baseline.scorer_version != SCORER_VERSION {
		return Err(format!(
			"Legacy baseline scorer version is {}; expected {SCORER_VERSION}",
			baseline.scorer_version
		)
		.into());
	}
	if baseline.expected_content_id != expected_content_id {
		return Err("Legacy baseline is not bound to the supplied expected verdicts".into());
	}
	let mut actual = BTreeMap::<String, BTreeMap<String, usize>>::new();
	for unit in &baseline.units {
		*actual
			.entry(unit.case_id.clone())
			.or_default()
			.entry(unit.score.verdict.clone())
			.or_default() += 1;
	}
	if actual != expected {
		return Err(format!(
			"Legacy baseline does not reproduce {}",
			expected_path.display()
		)
		.into());
	}

	let mut scores = BTreeMap::new();
	for unit in baseline.units {
		if !unit.score.multi_source {
			return Err(format!(
				"Legacy baseline unit {}:{} is not multi-source",
				unit.case_id, unit.score.rel
			)
			.into());
		}
		let selection = CorpusShadowSelection {
			case_id: unit.case_id,
			relative_path: unit.score.rel.clone(),
		};
		if scores.insert(selection.clone(), unit.score).is_some() {
			return Err(format!(
				"duplicate Legacy baseline unit {}:{}",
				selection.case_id, selection.relative_path
			)
			.into());
		}
	}
	let target_keys = targets
		.iter()
		.map(|target| CorpusShadowSelection {
			case_id: target.case_id.clone(),
			relative_path: target.relative_path.clone(),
		})
		.collect::<BTreeSet<_>>();
	let score_keys = scores.keys().cloned().collect::<BTreeSet<_>>();
	if score_keys != target_keys {
		let missing = target_keys
			.difference(&score_keys)
			.map(selection_name)
			.collect::<Vec<_>>();
		let extra = score_keys
			.difference(&target_keys)
			.map(selection_name)
			.collect::<Vec<_>>();
		return Err(format!(
			"Legacy baseline denominator mismatch: missing=[{}] extra=[{}]",
			missing.join(", "),
			extra.join(", ")
		)
		.into());
	}
	for target in targets {
		let selection = CorpusShadowSelection {
			case_id: target.case_id.clone(),
			relative_path: target.relative_path.clone(),
		};
		let score = scores
			.get(&selection)
			.expect("matching key sets guarantee a baseline score");
		if score.source_mod_ids != target.source_mod_ids {
			return Err(format!(
				"Legacy baseline source identity mismatch for {}:{}",
				selection.case_id, selection.relative_path
			)
			.into());
		}
	}

	Ok((
		LegacyBaselineIdentity {
			schema: "1.0.0".to_string(),
			scorer_version: SCORER_VERSION.to_string(),
			baseline_content_id: stable_id("legacy-baseline-v1", &[&baseline_bytes]),
			expected_content_id,
			unit_count: scores.len(),
		},
		scores,
	))
}

fn selection_name(selection: &CorpusShadowSelection) -> String {
	format!("{}:{}", selection.case_id, selection.relative_path)
}

fn verify_candidate_legacy_baseline(
	candidate: &CorpusShadowUnitRecord,
	baseline: &FileRecord,
) -> Result<(), Box<dyn std::error::Error>> {
	let observed = candidate
		.legacy
		.score
		.as_ref()
		.ok_or("candidate comparison did not produce a Legacy score")?;
	if observed != baseline {
		return Err(format!(
			"candidate Legacy arm drifted from the fixed baseline for {}:{}: baseline={} observed={}",
			candidate.target.case_id,
			candidate.target.relative_path,
			baseline.verdict,
			observed.verdict
		)
		.into());
	}
	Ok(())
}

fn candidate_is_publishable(candidate: &CorpusShadowUnitRecord) -> bool {
	matches!(
		candidate.outcome,
		CorpusShadowOutcome::Improved | CorpusShadowOutcome::UnchangedAccepted
	)
}

fn candidate_projected_strict_accepted(candidate: &CorpusShadowUnitRecord) -> bool {
	candidate_is_publishable(candidate)
		&& candidate
			.structured
			.score
			.as_ref()
			.is_some_and(strict_record_accepted)
}

fn candidate_projected_adjudicated_accepted(candidate: &CorpusShadowUnitRecord) -> bool {
	candidate_is_publishable(candidate)
		&& candidate
			.structured
			.score
			.as_ref()
			.is_some_and(|score| score.accepted_ok)
}

fn strict_record_accepted(score: &FileRecord) -> bool {
	matches!(
		score.verdict.as_str(),
		"matches_human" | "matches_ast" | "accepted_equivalent"
	)
}

struct RunIdentity {
	base_snapshot_identity: String,
	executable_hash: String,
	scorer_config_hash: String,
}

fn run_identity(
	options: &CorpusShadowOptions<'_>,
) -> Result<RunIdentity, Box<dyn std::error::Error>> {
	let base = installed_base_snapshot_identity("eu4", &options.game.game_version)?
		.ok_or_else(|| {
			format!(
				"no installed base snapshot for eu4@{}",
				options.game.game_version
			)
		})?
		.as_label();
	let executable_hash = executable_hash(options.executable)?;
	let base_identity = format!("eu4@{}/{}", options.game.game_version, base);
	let base_scorer_config = scorer_config_hash(options.timeout, Some(&base_identity));
	let force = [u8::from(options.force)];
	Ok(RunIdentity {
		base_snapshot_identity: base,
		executable_hash,
		scorer_config_hash: stable_id(
			"corpus-shadow-config-v1",
			&[base_scorer_config.as_bytes(), &force],
		),
	})
}

fn validate_options(options: &CorpusShadowOptions<'_>) -> Result<(), Box<dyn std::error::Error>> {
	if options.timeout.is_zero() {
		return Err("shadow timeout must be greater than zero".into());
	}
	if !options.executable.is_file() {
		return Err(format!(
			"shadow executable does not exist: {}",
			options.executable.display()
		)
		.into());
	}
	Ok(())
}

pub(crate) fn validate_snapshot_game(
	snapshot: &SnapshotRecord,
	game: &Eu4GameDiscovery,
) -> Result<(), Box<dyn std::error::Error>> {
	if snapshot.game.version != game.game_version {
		return Err(format!(
			"base-game version mismatch for {}: snapshot={} local={}",
			snapshot.case_id, snapshot.game.version, game.game_version
		)
		.into());
	}
	if snapshot.game.steam_build_id != game.steam_build_id {
		return Err(format!(
			"Steam build mismatch for {}: snapshot={:?} local={:?}",
			snapshot.case_id, snapshot.game.steam_build_id, game.steam_build_id
		)
		.into());
	}
	Ok(())
}

pub(crate) fn load_snapshot(
	store: &ObjectStore,
	snapshot: SnapshotRecord,
	verified: &mut HashSet<String>,
) -> io::Result<LoadedSnapshot> {
	verify_once(store, &snapshot.compatch.content_hash, verified)?;
	for source in &snapshot.source_mods {
		verify_once(store, &source.content_hash, verified)?;
	}
	let compatch = store.open_object(&snapshot.compatch.content_hash)?.tree;
	let source_dirs = snapshot
		.source_mods
		.iter()
		.map(|source| {
			store
				.open_object(&source.content_hash)
				.map(|object| object.tree)
		})
		.collect::<io::Result<Vec<_>>>()?;
	Ok(LoadedSnapshot {
		snapshot,
		compatch,
		source_dirs,
	})
}

fn verify_once(store: &ObjectStore, hash: &str, verified: &mut HashSet<String>) -> io::Result<()> {
	if verified.insert(hash.to_string()) {
		store.verify_object(hash)?;
	}
	Ok(())
}

fn discover_targets(
	loaded: &LoadedSnapshot,
	game: &Eu4GameDiscovery,
	identity: &RunIdentity,
	cache: &mut ScoreCache,
	retained_path: Option<&str>,
) -> Result<Vec<CorpusShadowTarget>, Box<dyn std::error::Error>> {
	let references = reference_output_files(&loaded.compatch);
	let units = scoring_reference_units(&references);
	let source_mods = source_mods(loaded);
	let conflicts = HashSet::new();
	let empty_output = tempfile::tempdir()?;
	let mut targets = Vec::new();
	for relative_path in units {
		if retained_path.is_some_and(|retained| retained != relative_path) {
			continue;
		}
		let score = score_file_with_cache_and_basegame(
			&ScoreFileRequest {
				rel: &relative_path,
				source_mods: &source_mods,
				compatch: &loaded.compatch,
				out_dir: empty_output.path(),
				conflict_paths: &conflicts,
			},
			cache,
			Some(&game.game_root),
		);
		if !score.multi_source {
			continue;
		}
		targets.push(make_target(
			TargetIdentity {
				snapshot: &loaded.snapshot,
				relative_path: &score.rel,
				source_mod_ids: &score.source_mod_ids,
				base_snapshot_identity: &identity.base_snapshot_identity,
				executable_hash: &identity.executable_hash,
				scorer_config_hash: &identity.scorer_config_hash,
			},
			game,
		));
	}
	Ok(targets)
}

fn make_target(identity: TargetIdentity<'_>, game: &Eu4GameDiscovery) -> CorpusShadowTarget {
	let encoded = serde_json::to_vec(&(
		&identity.snapshot.snapshot_id,
		identity.relative_path,
		identity.source_mod_ids,
		&game.game_version,
		game.steam_build_id,
		identity.base_snapshot_identity,
		identity.executable_hash,
		identity.scorer_config_hash,
	))
	.expect("corpus shadow target identity serializes");
	CorpusShadowTarget {
		schema: CORPUS_SHADOW_SCHEMA.to_string(),
		unit_id: stable_id("corpus-shadow-unit-v1", &[&encoded]),
		snapshot_id: identity.snapshot.snapshot_id.clone(),
		case_id: identity.snapshot.case_id.clone(),
		relative_path: identity.relative_path.to_string(),
		source_mod_ids: identity.source_mod_ids.to_vec(),
		game_version: game.game_version.clone(),
		steam_build_id: game.steam_build_id,
		base_snapshot_identity: identity.base_snapshot_identity.to_string(),
		executable_hash: identity.executable_hash.to_string(),
		scorer_config_hash: identity.scorer_config_hash.to_string(),
	}
}

fn run_target(
	options: &CorpusShadowOptions<'_>,
	loaded: &LoadedSnapshot,
	target: CorpusShadowTarget,
	cache: &mut ScoreCache,
) -> Result<CorpusShadowUnitRecord, Box<dyn std::error::Error>> {
	let unit_dir = options.output_dir.join("units").join(&target.unit_id);
	let assessment_path = unit_dir.join("assessment.json");
	if let Some(existing) = resume_record(&unit_dir, &target, &options.game.game_root) {
		return Ok(existing);
	}
	fs::create_dir_all(&unit_dir)?;
	match fs::remove_file(&assessment_path) {
		Ok(()) => {}
		Err(error) if error.kind() == io::ErrorKind::NotFound => {}
		Err(error) => return Err(error.into()),
	}
	let playset_root = tempfile::Builder::new()
		.prefix("corpus-shadow-playset-")
		.tempdir()?;
	let mods = loaded
		.snapshot
		.source_mods
		.iter()
		.zip(&loaded.source_dirs)
		.map(|(source, root)| (source.workshop_id.clone(), root.clone()))
		.collect::<Vec<_>>();
	let playset = write_playset(playset_root.path(), &mods)?;
	let comparison = run_shadow_comparison(ShadowCompareRequest {
		playset: &playset,
		output_dir: &unit_dir,
		game_root: &options.game.game_root,
		game_version: &options.game.game_version,
		retained_paths: BTreeSet::from([target.relative_path.clone()]),
		expected_base_snapshot_identity: Some(&target.base_snapshot_identity),
		force: options.force,
		executable: options.executable,
		timeout: options.timeout,
	})?;
	let record = assess_comparison(options, loaded, target, comparison, cache)?;
	fs::write(&assessment_path, serde_json::to_vec_pretty(&record)?)?;
	Ok(record)
}

fn resume_record(
	unit_dir: &Path,
	target: &CorpusShadowTarget,
	game_root: &Path,
) -> Option<CorpusShadowUnitRecord> {
	let record = serde_json::from_slice::<CorpusShadowUnitRecord>(
		&fs::read(unit_dir.join("assessment.json")).ok()?,
	)
	.ok()?;
	if record.schema != CORPUS_SHADOW_SCHEMA || &record.target != target {
		return None;
	}
	let comparison = serde_json::from_slice::<ShadowComparisonReport>(
		&fs::read(unit_dir.join("shadow-compare.json")).ok()?,
	)
	.ok()?;
	if comparison.comparison_id != record.comparison_id
		|| comparison.inputs.executable_hash != target.executable_hash
		|| comparison.inputs.game_version != target.game_version
		|| comparison.inputs.base_snapshot_identity != target.base_snapshot_identity
		|| comparison.inputs.retained_paths != [target.relative_path.clone()]
		|| !crate::shadow::corpus_resume_environment_matches(&comparison.inputs, game_root).ok()?
		|| !arm_artifact_matches(unit_dir, "legacy", &record.legacy, &comparison.legacy)
		|| !arm_artifact_matches(
			unit_dir,
			"structured",
			&record.structured,
			&comparison.structured,
		) {
		return None;
	}
	Some(record)
}

fn arm_artifact_matches(
	unit_dir: &Path,
	kernel: &str,
	assessment: &ShadowArmAssessment,
	run: &ShadowRunRecord,
) -> bool {
	if assessment.kernel != kernel
		|| run.kernel != kernel
		|| assessment.status != run.status
		|| assessment.output_valid != run.output_valid
	{
		return false;
	}
	let output_dir = unit_dir.join(kernel);
	if !assessment.output_valid {
		return assessment.output_hash.is_none() && !output_dir.exists();
	}
	output_content_hash(&output_dir).ok().flatten() == assessment.output_hash
}

fn assess_comparison(
	options: &CorpusShadowOptions<'_>,
	loaded: &LoadedSnapshot,
	target: CorpusShadowTarget,
	comparison: ShadowComparisonReport,
	cache: &mut ScoreCache,
) -> Result<CorpusShadowUnitRecord, Box<dyn std::error::Error>> {
	let source_mods = source_mods(loaded);
	let human_resolution = classify_resolution(
		&target.relative_path,
		&source_mods,
		&loaded.compatch,
		Some(&options.game.game_root),
	);
	let human_path = loaded.compatch.join(&target.relative_path);
	let legacy = assess_arm(
		&comparison.legacy,
		&target,
		loaded,
		options,
		&human_path,
		cache,
	)?;
	let structured = assess_arm(
		&comparison.structured,
		&target,
		loaded,
		options,
		&human_path,
		cache,
	)?;
	let structured_vs_legacy = semantic_diff_if_files(
		&target.relative_path,
		&comparison.structured.output_dir.join(&target.relative_path),
		&comparison.legacy.output_dir.join(&target.relative_path),
	);
	let outcome = classify_outcome(&legacy, &structured);
	let observed_at = now_rfc3339();
	let record_id = record_id(&target, &legacy, &structured, outcome, &observed_at);
	Ok(CorpusShadowUnitRecord {
		schema: CORPUS_SHADOW_SCHEMA.to_string(),
		record_id,
		target,
		comparison_id: comparison.comparison_id,
		observed_at,
		human_resolution,
		legacy,
		structured,
		structured_vs_legacy,
		outcome,
	})
}

fn assess_arm(
	run: &ShadowRunRecord,
	target: &CorpusShadowTarget,
	loaded: &LoadedSnapshot,
	options: &CorpusShadowOptions<'_>,
	human_path: &Path,
	cache: &mut ScoreCache,
) -> Result<ShadowArmAssessment, Box<dyn std::error::Error>> {
	let output_path = run.output_dir.join(&target.relative_path);
	let output_hash = output_content_hash(&run.output_dir)?;
	let mut assessment_diagnostics = run.diagnostics.clone();
	let score = if run.output_valid {
		let conflicts = run
			.diagnostics
			.iter()
			.filter(|diagnostic| diagnostic.kind == ShadowDiagnosticKind::Conflict)
			.filter_map(|diagnostic| diagnostic.path.clone())
			.collect::<HashSet<_>>();
		let source_mods = source_mods(loaded);
		let mut score = FileRecord::from_score(score_file_with_cache_and_basegame(
			&ScoreFileRequest {
				rel: &target.relative_path,
				source_mods: &source_mods,
				compatch: &loaded.compatch,
				out_dir: &run.output_dir,
				conflict_paths: &conflicts,
			},
			cache,
			Some(&options.game.game_root),
		));
		if !score.accepted_ok
			&& let Some((candidate, human)) = adjudication_ast_pair(
				&target.relative_path,
				loaded,
				&options.game.game_root,
				&run.output_dir,
			) {
			if run.kernel == "structured"
				&& let Some(module_policy) =
					definition_module_policy_for_path(&target.relative_path)
				&& let Some(descriptor) =
					eu4_profile().classify_content_family(Path::new(&target.relative_path))
			{
				let comparison = normalize_module_comparison(
					&candidate,
					&human,
					&descriptor.merge_policies,
					module_policy.namespace_prefix,
				);
				let equivalent = if comparison.diagnostics.is_empty() {
					let diff = semantic_atom_diff_ast(&comparison.candidate, &comparison.human);
					let equivalent = diff.left_only.is_empty() && diff.right_only.is_empty();
					if !equivalent {
						let residual_keys = semantic_diff_top_level_keys(&diff);
						assessment_diagnostics.push(ShadowDiagnostic {
							kind: ShadowDiagnosticKind::Warning,
							path: Some(target.relative_path.clone()),
							message: format!(
								"structured module normalization retained {} differing top-level definition(s): {}",
								residual_keys.len(),
								residual_keys.join(", "),
							),
						});
					}
					equivalent
				} else {
					for diagnostic in comparison.diagnostics {
						assessment_diagnostics.push(ShadowDiagnostic {
							kind: ShadowDiagnosticKind::Warning,
							path: diagnostic.path,
							message: format!("{}: {}", diagnostic.phase, diagnostic.message),
						});
					}
					false
				};
				if equivalent {
					score.verdict = "accepted_equivalent".to_string();
					score.accepted_ok = true;
					score.acceptance_reason =
						Some("structured_module_semantic_equivalent".to_string());
				}
			}
			if !score.accepted_ok
				&& let Some(reason) = Adjudications::built_in().accepted_better_reason(
					&AdjudicationBinding {
						compatch_id: &loaded.snapshot.compatch.workshop_id,
						relative_path: &target.relative_path,
						snapshot_id: &target.snapshot_id,
						scoring_unit_id: &target.unit_id,
					},
					&candidate,
					&human,
				) {
				score.verdict = "accepted_better".to_string();
				score.accepted_ok = true;
				score.acceptance_reason = Some(reason);
			}
		}
		Some(score)
	} else {
		None
	};
	let semantic_diff_from_human =
		semantic_diff_if_files(&target.relative_path, &output_path, human_path);
	let semantic_diff_from_sources = loaded
		.snapshot
		.source_mods
		.iter()
		.zip(&loaded.source_dirs)
		.filter_map(|(source, root)| {
			semantic_diff_if_files(
				&target.relative_path,
				&output_path,
				&root.join(&target.relative_path),
			)
			.map(|diff| (source.workshop_id.clone(), diff))
		})
		.collect();
	let semantic_diff_from_base = semantic_diff_if_files(
		&target.relative_path,
		&output_path,
		&options.game.game_root.join(&target.relative_path),
	);
	let event_safety =
		is_event_path(&target.relative_path).then(|| assess_event_safety(&output_path, human_path));
	Ok(ShadowArmAssessment {
		kernel: run.kernel.clone(),
		status: run.status.clone(),
		output_valid: run.output_valid,
		elapsed_ms: run.elapsed_ms,
		output_hash,
		score,
		semantic_diff_from_human,
		semantic_diff_from_sources,
		semantic_diff_from_base,
		event_safety,
		diagnostics: assessment_diagnostics,
		error: run.error.clone(),
	})
}

fn adjudication_ast_pair(
	relative_path: &str,
	loaded: &LoadedSnapshot,
	game_root: &Path,
	output_dir: &Path,
) -> Option<(AstFile, AstFile)> {
	if let Some(policy) = definition_module_policy_for_path(relative_path) {
		let mut builder = CommonModuleViewBuilder::default();
		let mut candidate_roots = Vec::with_capacity(loaded.source_dirs.len() + 2);
		candidate_roots.push(game_root);
		candidate_roots.extend(loaded.source_dirs.iter().map(PathBuf::as_path));
		candidate_roots.push(output_dir);
		let mut human_roots = Vec::with_capacity(loaded.source_dirs.len() + 2);
		human_roots.push(game_root);
		human_roots.extend(loaded.source_dirs.iter().map(PathBuf::as_path));
		human_roots.push(loaded.compatch.as_path());
		let candidate = builder
			.view(&candidate_roots, policy.namespace_prefix)
			.ok()?;
		let human = builder.view(&human_roots, policy.namespace_prefix).ok()?;
		return Some((candidate.as_ref().clone(), human.as_ref().clone()));
	}

	let candidate = parse_clausewitz_file(&output_dir.join(relative_path));
	let human = parse_clausewitz_file(&loaded.compatch.join(relative_path));
	(candidate.diagnostics.is_empty() && human.diagnostics.is_empty())
		.then_some((candidate.ast, human.ast))
}

fn source_mods(loaded: &LoadedSnapshot) -> Vec<SourceMod<'_>> {
	loaded
		.snapshot
		.source_mods
		.iter()
		.zip(&loaded.source_dirs)
		.map(|(source, root)| SourceMod {
			id: &source.workshop_id,
			root,
		})
		.collect()
}

fn semantic_diff_if_files(rel: &str, left: &Path, right: &Path) -> Option<SemanticAtomDiff> {
	if !left.is_file() || !right.is_file() {
		return None;
	}
	semantic_atom_diff(rel, left, right, !is_gui_path(rel))
}

fn semantic_diff_top_level_keys(diff: &SemanticAtomDiff) -> Vec<String> {
	diff.left_only
		.keys()
		.chain(diff.right_only.keys())
		.filter_map(|atom| atom.strip_prefix("assignment:"))
		.filter_map(|atom| atom.split('/').next())
		.map(str::to_string)
		.collect::<BTreeSet<_>>()
		.into_iter()
		.collect()
}

fn is_gui_path(rel: &str) -> bool {
	rel.to_ascii_lowercase().ends_with(".gui")
}

fn is_event_path(rel: &str) -> bool {
	rel.to_ascii_lowercase().starts_with("events/") && rel.to_ascii_lowercase().ends_with(".txt")
}

fn classify_outcome(
	legacy: &ShadowArmAssessment,
	structured: &ShadowArmAssessment,
) -> CorpusShadowOutcome {
	if !legacy.output_valid {
		return CorpusShadowOutcome::Failed;
	}
	if structured_unsupported(structured) {
		return CorpusShadowOutcome::StructuredUnsupported;
	}
	if structured.status == "blocked"
		|| structured
			.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.kind == ShadowDiagnosticKind::Conflict)
	{
		return CorpusShadowOutcome::StructuredConflict;
	}
	if !structured.output_valid {
		return CorpusShadowOutcome::Failed;
	}
	if structured
		.event_safety
		.as_ref()
		.is_some_and(|safety| !safety.passed())
	{
		return CorpusShadowOutcome::SafetyFailed;
	}
	let legacy_accepted = legacy.score.as_ref().is_some_and(|score| score.accepted_ok);
	let structured_accepted = structured
		.score
		.as_ref()
		.is_some_and(|score| score.accepted_ok);
	match (legacy_accepted, structured_accepted) {
		(false, true) => CorpusShadowOutcome::Improved,
		(true, false) => CorpusShadowOutcome::Regressed,
		(true, true) => CorpusShadowOutcome::UnchangedAccepted,
		(false, false) if legacy.output_hash == structured.output_hash => {
			CorpusShadowOutcome::UnchangedRejected
		}
		(false, false) => CorpusShadowOutcome::NeedsReview,
	}
}

fn structured_unsupported(assessment: &ShadowArmAssessment) -> bool {
	assessment
		.error
		.as_deref()
		.is_some_and(|error| error.contains("structured merge unsupported"))
		|| assessment
			.diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("structured merge unsupported"))
}

fn record_id(
	target: &CorpusShadowTarget,
	legacy: &ShadowArmAssessment,
	structured: &ShadowArmAssessment,
	outcome: CorpusShadowOutcome,
	observed_at: &str,
) -> String {
	let payload = serde_json::to_vec(&(
		&target.unit_id,
		&legacy.status,
		&legacy.output_hash,
		legacy.elapsed_ms,
		&structured.status,
		&structured.output_hash,
		structured.elapsed_ms,
		outcome,
		observed_at,
	))
	.expect("corpus shadow record identity serializes");
	stable_id("corpus-shadow-record-v1", &[&payload])
}

fn summarize(units: &[CorpusShadowProjectionUnit]) -> CorpusShadowSummary {
	let mut summary = CorpusShadowSummary {
		total_units: units.len(),
		completed_units: units.len(),
		..CorpusShadowSummary::default()
	};
	let mut supported_legacy_ms = 0_u64;
	let mut supported_structured_ms = 0_u64;
	for unit in units {
		let legacy_strict_accepted = strict_record_accepted(&unit.legacy_baseline);
		let legacy_adjudicated_accepted = unit.legacy_baseline.accepted_ok;
		let non_gui = !is_gui_path(&unit.target.relative_path);
		summary.legacy_strict_accepted += usize::from(legacy_strict_accepted);
		summary.legacy_adjudicated_accepted += usize::from(legacy_adjudicated_accepted);
		let candidate_score = unit
			.candidate
			.as_ref()
			.and_then(|candidate| candidate.structured.score.as_ref());
		let candidate_strict_accepted = candidate_score.is_some_and(strict_record_accepted);
		let candidate_adjudicated_accepted = candidate_score.is_some_and(|score| score.accepted_ok);
		summary.candidate_strict_accepted += usize::from(candidate_strict_accepted);
		summary.candidate_adjudicated_accepted += usize::from(candidate_adjudicated_accepted);
		match &unit.candidate {
			None => summary.legacy_retained += 1,
			Some(candidate) => {
				summary.candidate_evaluated += 1;
				summary.legacy_elapsed_ms = summary
					.legacy_elapsed_ms
					.saturating_add(candidate.legacy.elapsed_ms);
				summary.structured_elapsed_ms = summary
					.structured_elapsed_ms
					.saturating_add(candidate.structured.elapsed_ms);
				match candidate.outcome {
					CorpusShadowOutcome::Improved => summary.improved += 1,
					CorpusShadowOutcome::Regressed => summary.regressed += 1,
					CorpusShadowOutcome::UnchangedAccepted => summary.unchanged_accepted += 1,
					CorpusShadowOutcome::UnchangedRejected => summary.unchanged_rejected += 1,
					CorpusShadowOutcome::NeedsReview => summary.needs_review += 1,
					CorpusShadowOutcome::SafetyFailed => summary.safety_failed += 1,
					CorpusShadowOutcome::StructuredUnsupported => {
						summary.structured_unsupported += 1
					}
					CorpusShadowOutcome::StructuredConflict => summary.structured_conflict += 1,
					CorpusShadowOutcome::Failed => summary.failed += 1,
				}
				if candidate.structured.output_valid
					&& !matches!(
						candidate.outcome,
						CorpusShadowOutcome::StructuredUnsupported
							| CorpusShadowOutcome::StructuredConflict
							| CorpusShadowOutcome::Failed
					) {
					supported_legacy_ms =
						supported_legacy_ms.saturating_add(candidate.legacy.elapsed_ms);
					supported_structured_ms =
						supported_structured_ms.saturating_add(candidate.structured.elapsed_ms);
				}
			}
		}
		summary.projected_strict_accepted += usize::from(unit.projected_strict_accepted);
		summary.projected_adjudicated_accepted += usize::from(unit.projected_adjudicated_accepted);
		summary.legacy_strict_accepted_lost +=
			usize::from(legacy_strict_accepted && !unit.projected_strict_accepted);
		summary.legacy_adjudicated_accepted_lost +=
			usize::from(legacy_adjudicated_accepted && !unit.projected_adjudicated_accepted);
		if non_gui {
			summary.non_gui_units += 1;
			summary.legacy_strict_accepted_non_gui += usize::from(legacy_strict_accepted);
			summary.legacy_adjudicated_accepted_non_gui += usize::from(legacy_adjudicated_accepted);
			summary.candidate_strict_accepted_non_gui += usize::from(candidate_strict_accepted);
			summary.candidate_adjudicated_accepted_non_gui +=
				usize::from(candidate_adjudicated_accepted);
			summary.projected_strict_accepted_non_gui +=
				usize::from(unit.projected_strict_accepted);
			summary.projected_adjudicated_accepted_non_gui +=
				usize::from(unit.projected_adjudicated_accepted);
			summary.legacy_strict_accepted_non_gui_lost +=
				usize::from(legacy_strict_accepted && !unit.projected_strict_accepted);
			summary.legacy_adjudicated_accepted_non_gui_lost +=
				usize::from(legacy_adjudicated_accepted && !unit.projected_adjudicated_accepted);
		}
	}
	if supported_legacy_ms > 0 {
		summary.supported_runtime_ratio_milli = Some(
			supported_structured_ms
				.saturating_mul(1_000)
				.checked_div(supported_legacy_ms)
				.unwrap_or(u64::MAX),
		);
	}
	summary
}

fn write_single_report(output_dir: &Path, record: &CorpusShadowUnitRecord) -> io::Result<()> {
	fs::write(
		output_dir.join("shadow-case.json"),
		serde_json::to_vec_pretty(record).map_err(io::Error::other)?,
	)?;
	fs::write(
		output_dir.join("report.md"),
		render_candidate_report(record),
	)
}

fn write_corpus_report(output_dir: &Path, report: &CorpusShadowReport) -> io::Result<()> {
	fs::write(
		output_dir.join("shadow-corpus.json"),
		serde_json::to_vec_pretty(report).map_err(io::Error::other)?,
	)?;
	fs::write(
		output_dir.join("report.md"),
		render_projection_report(report),
	)
}

fn render_candidate_report(unit: &CorpusShadowUnitRecord) -> String {
	let mut out = String::from("# Structured Merge Shadow Report\n\n");
	out.push_str("| Case | Unit | Legacy | Structured | Outcome |\n");
	out.push_str("| --- | --- | --- | --- | --- |\n");
	append_candidate_row(&mut out, unit);
	append_candidate_details(&mut out, unit);
	out
}

fn render_projection_report(report: &CorpusShadowReport) -> String {
	let summary = &report.summary;
	let mut out = String::from("# Structured Merge Rollout Projection\n\n");
	out.push_str(&format!(
		"Units: {} | candidate evaluated: {} | Legacy retained: {}\n\n",
		summary.total_units, summary.candidate_evaluated, summary.legacy_retained
	));
	out.push_str(&format!(
		"Strict: Legacy accepted {}/{} | projected accepted {}/{} | Legacy accepted lost {}\n\n",
		summary.legacy_strict_accepted,
		summary.total_units,
		summary.projected_strict_accepted,
		summary.total_units,
		summary.legacy_strict_accepted_lost
	));
	out.push_str(&format!(
		"Adjudicated: Legacy accepted {}/{} | projected accepted {}/{} | Legacy accepted lost {}\n\n",
		summary.legacy_adjudicated_accepted,
		summary.total_units,
		summary.projected_adjudicated_accepted,
		summary.total_units,
		summary.legacy_adjudicated_accepted_lost
	));
	out.push_str(&format!(
		"Non-GUI strict: Legacy accepted {}/{} | projected accepted {}/{}\n\n",
		summary.legacy_strict_accepted_non_gui,
		summary.non_gui_units,
		summary.projected_strict_accepted_non_gui,
		summary.non_gui_units
	));
	out.push_str(&format!(
		"Non-GUI adjudicated: Legacy accepted {}/{} | projected accepted {}/{}\n\n",
		summary.legacy_adjudicated_accepted_non_gui,
		summary.non_gui_units,
		summary.projected_adjudicated_accepted_non_gui,
		summary.non_gui_units
	));
	out.push_str(&format!(
		"Candidate outcomes: improved={} regressed={} unchanged_accepted={} unchanged_rejected={} review={} safety_failed={} unsupported={} conflict={} failed={}\n\n",
		summary.improved,
		summary.regressed,
		summary.unchanged_accepted,
		summary.unchanged_rejected,
		summary.needs_review,
		summary.safety_failed,
		summary.structured_unsupported,
		summary.structured_conflict,
		summary.failed
	));
	if let Some(ratio_milli) = summary.supported_runtime_ratio_milli {
		out.push_str(&format!(
			"Candidate runtime ratio: {}.{:03}x Legacy\n\n",
			ratio_milli / 1_000,
			ratio_milli % 1_000
		));
	}
	out.push_str(&format!(
		"Legacy baseline: scorer `{}`; baseline `{}`; expected `{}`\n\n",
		report.legacy_baseline.scorer_version,
		report.legacy_baseline.baseline_content_id,
		report.legacy_baseline.expected_content_id
	));
	out.push_str(
		"| Case | Unit | Legacy baseline | Candidate | Disposition | Strict | Adjudicated |\n",
	);
	out.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
	for unit in &report.units {
		let (candidate, disposition) =
			unit.candidate
				.as_ref()
				.map_or(("not run", "legacy_retained"), |candidate| {
					(
						candidate
							.structured
							.score
							.as_ref()
							.map_or(candidate.structured.status.as_str(), |score| {
								score.verdict.as_str()
							}),
						outcome_name(candidate.outcome),
					)
				});
		out.push_str(&format!(
			"| {} | `{}` | {} | {} | {} | {} | {} |\n",
			unit.target.case_id,
			unit.target.relative_path,
			unit.legacy_baseline.verdict,
			candidate,
			disposition,
			if unit.projected_strict_accepted {
				"accepted"
			} else {
				"rejected"
			},
			if unit.projected_adjudicated_accepted {
				"accepted"
			} else {
				"rejected"
			}
		));
	}
	for unit in &report.units {
		if let Some(candidate) = &unit.candidate {
			append_candidate_details(&mut out, candidate);
		}
	}
	out
}

fn append_candidate_row(out: &mut String, unit: &CorpusShadowUnitRecord) {
	let legacy = unit
		.legacy
		.score
		.as_ref()
		.map_or(unit.legacy.status.as_str(), |score| score.verdict.as_str());
	let structured = unit
		.structured
		.score
		.as_ref()
		.map_or(unit.structured.status.as_str(), |score| {
			score.verdict.as_str()
		});
	out.push_str(&format!(
		"| {} | `{}` | {} | {} | {} |\n",
		unit.target.case_id,
		unit.target.relative_path,
		legacy,
		structured,
		outcome_name(unit.outcome)
	));
}

fn append_candidate_details(out: &mut String, unit: &CorpusShadowUnitRecord) {
	out.push_str(&format!(
		"\n## {}/{}\n\n- Outcome: `{}`\n- Timing: Legacy {} ms; Structured {} ms\n",
		unit.target.case_id,
		unit.target.relative_path,
		outcome_name(unit.outcome),
		unit.legacy.elapsed_ms,
		unit.structured.elapsed_ms
	));
	if let Some(diff) = &unit.legacy.semantic_diff_from_human {
		out.push_str(&format!(
			"- Legacy versus human: {} only / {} only / {} shared atoms\n",
			diff.left_only.values().sum::<usize>(),
			diff.right_only.values().sum::<usize>(),
			diff.shared_atoms
		));
	}
	if let Some(diff) = &unit.structured.semantic_diff_from_human {
		out.push_str(&format!(
			"- Structured versus human: {} only / {} only / {} shared atoms\n",
			diff.left_only.values().sum::<usize>(),
			diff.right_only.values().sum::<usize>(),
			diff.shared_atoms
		));
	}
	if let Some(safety) = &unit.structured.event_safety {
		out.push_str(&format!(
			"- Event safety: parse={} human_parse={} duplicate_events={} duplicate_options={} orphan_control={} control_shape_matches_human={}\n",
			safety.parse_ok,
			safety.human_parse_ok,
			safety.duplicate_event_ids.len(),
			safety.duplicate_option_ids.len(),
			safety.orphan_control_flow_paths.len(),
			safety.control_flow_matches_human.unwrap_or(false)
		));
	}
}

fn outcome_name(outcome: CorpusShadowOutcome) -> &'static str {
	match outcome {
		CorpusShadowOutcome::Improved => "improved",
		CorpusShadowOutcome::Regressed => "regressed",
		CorpusShadowOutcome::UnchangedAccepted => "unchanged_accepted",
		CorpusShadowOutcome::UnchangedRejected => "unchanged_rejected",
		CorpusShadowOutcome::NeedsReview => "needs_review",
		CorpusShadowOutcome::SafetyFailed => "safety_failed",
		CorpusShadowOutcome::StructuredUnsupported => "structured_unsupported",
		CorpusShadowOutcome::StructuredConflict => "structured_conflict",
		CorpusShadowOutcome::Failed => "failed",
	}
}

pub(crate) fn latest_snapshots(paths: &DatasetPaths) -> io::Result<Vec<SnapshotRecord>> {
	let snapshots = read_jsonl::<SnapshotRecord>(&paths.snapshots)?;
	let observations = read_jsonl::<ObservationRecord>(&paths.observations)?;
	let mut observed_at = HashMap::new();
	for observation in &observations {
		observed_at
			.entry(observation.snapshot_id.as_str())
			.and_modify(|current: &mut &str| {
				if observation.observed_at.as_str() > *current {
					*current = observation.observed_at.as_str();
				}
			})
			.or_insert(observation.observed_at.as_str());
	}
	let mut latest: BTreeMap<String, (String, SnapshotRecord)> = BTreeMap::new();
	for snapshot in snapshots {
		let timestamp = observed_at
			.get(snapshot.snapshot_id.as_str())
			.copied()
			.unwrap_or("")
			.to_string();
		latest
			.entry(snapshot.case_id.clone())
			.and_modify(|current| {
				if (timestamp.as_str(), snapshot.snapshot_id.as_str())
					> (current.0.as_str(), current.1.snapshot_id.as_str())
				{
					*current = (timestamp.clone(), snapshot.clone());
				}
			})
			.or_insert((timestamp, snapshot));
	}
	Ok(latest.into_values().map(|(_, snapshot)| snapshot).collect())
}

fn oracle_assessment(
	snapshot: &SnapshotRecord,
	observations: &[ObservationRecord],
) -> Option<OracleAssessment> {
	let observation = observations
		.iter()
		.filter(|observation| observation.snapshot_id == snapshot.snapshot_id)
		.max_by(|left, right| left.observed_at.cmp(&right.observed_at))?;
	Some(assess_oracle_candidate(
		&observation.compatch.title,
		snapshot.source_mods.len(),
		observation.mod_churned,
	))
}

#[derive(Default)]
struct EventFacts {
	namespaces: Vec<String>,
	event_ids: Vec<String>,
	option_ids: Vec<String>,
	orphan_control_flow_paths: Vec<String>,
	control_flow_shape: Vec<String>,
}

fn assess_event_safety(output: &Path, human: &Path) -> EventSafetyAssessment {
	let output_parsed = parse_clausewitz_file(output);
	let human_parsed = parse_clausewitz_file(human);
	let mut output_facts = EventFacts::default();
	let mut human_facts = EventFacts::default();
	if output_parsed.diagnostics.is_empty() {
		collect_event_facts(&output_parsed.ast.statements, &mut output_facts);
	}
	if human_parsed.diagnostics.is_empty() {
		collect_event_facts(&human_parsed.ast.statements, &mut human_facts);
	}
	normalize_event_facts(&mut output_facts);
	normalize_event_facts(&mut human_facts);
	let parse_ok = output.is_file() && output_parsed.diagnostics.is_empty();
	let human_parse_ok = human.is_file() && human_parsed.diagnostics.is_empty();
	let comparable = parse_ok && human_parse_ok;
	EventSafetyAssessment {
		parse_ok,
		diagnostic_count: output_parsed.diagnostics.len(),
		duplicate_event_ids: duplicates(&output_facts.event_ids),
		duplicate_option_ids: duplicates(&output_facts.option_ids),
		human_parse_ok,
		namespaces_include_human: comparable.then_some(includes_all(
			&output_facts.namespaces,
			&human_facts.namespaces,
		)),
		event_ids_include_human: comparable.then_some(includes_all(
			&output_facts.event_ids,
			&human_facts.event_ids,
		)),
		option_ids_include_human: comparable.then_some(includes_all(
			&output_facts.option_ids,
			&human_facts.option_ids,
		)),
		control_flow_matches_human: comparable
			.then_some(output_facts.control_flow_shape == human_facts.control_flow_shape),
		namespaces: output_facts.namespaces,
		event_ids: output_facts.event_ids,
		option_ids: output_facts.option_ids,
		orphan_control_flow_paths: output_facts.orphan_control_flow_paths,
		control_flow_shape: output_facts.control_flow_shape,
	}
}

fn collect_event_facts(statements: &[AstStatement], facts: &mut EventFacts) {
	for statement in statements {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		if key == "namespace"
			&& let Some(value) = scalar_text(value)
		{
			facts.namespaces.push(value);
		}
		if is_event_key(key)
			&& let AstValue::Block { items, .. } = value
		{
			let event_id = child_scalar(items, "id").unwrap_or_else(|| "<missing-id>".to_string());
			facts.event_ids.push(event_id.clone());
			let mut option_index = 0_usize;
			for item in items {
				if let AstStatement::Assignment {
					key,
					value: AstValue::Block { items, .. },
					..
				} = item && key == "option"
				{
					option_index += 1;
					let option = child_scalar(items, "name")
						.unwrap_or_else(|| format!("<unnamed-{option_index}>"));
					facts.option_ids.push(format!("{event_id}::{option}"));
				}
			}
			collect_control_flow(items, &format!("event:{event_id}"), facts);
		}
	}
}

fn collect_control_flow(statements: &[AstStatement], path: &str, facts: &mut EventFacts) {
	let mut previous_control: Option<&str> = None;
	let mut key_counts = HashMap::new();
	for statement in statements {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		let index = key_counts.entry(key.as_str()).or_insert(0_usize);
		let current_path = format!("{path}/{key}[{index}]");
		*index += 1;
		match key.as_str() {
			"if" => {
				facts.control_flow_shape.push(format!("{path}/if"));
				previous_control = Some("if");
			}
			"else_if" => {
				facts.control_flow_shape.push(format!("{path}/else_if"));
				if !matches!(previous_control, Some("if" | "else_if")) {
					facts.orphan_control_flow_paths.push(current_path.clone());
				}
				previous_control = Some("else_if");
			}
			"else" => {
				facts.control_flow_shape.push(format!("{path}/else"));
				if !matches!(previous_control, Some("if" | "else_if")) {
					facts.orphan_control_flow_paths.push(current_path.clone());
				}
				previous_control = None;
			}
			_ => previous_control = None,
		}
		if let AstValue::Block { items, .. } = value {
			collect_control_flow(items, &current_path, facts);
		}
	}
}

fn child_scalar(statements: &[AstStatement], expected: &str) -> Option<String> {
	statements.iter().find_map(|statement| match statement {
		AstStatement::Assignment { key, value, .. } if key == expected => scalar_text(value),
		_ => None,
	})
}

fn scalar_text(value: &AstValue) -> Option<String> {
	match value {
		AstValue::Scalar { value, .. } => Some(value.as_text()),
		AstValue::Block { .. } => None,
	}
}

fn is_event_key(key: &str) -> bool {
	matches!(
		key,
		"country_event" | "province_event" | "unit_event" | "news_event"
	) || key.ends_with("_event")
}

fn normalize_event_facts(facts: &mut EventFacts) {
	facts.namespaces.sort();
	facts.event_ids.sort();
	facts.option_ids.sort();
	facts.orphan_control_flow_paths.sort();
	facts.control_flow_shape.sort();
}

fn duplicates(values: &[String]) -> Vec<String> {
	let mut counts = BTreeMap::new();
	for value in values {
		*counts.entry(value.clone()).or_insert(0_usize) += 1;
	}
	counts
		.into_iter()
		.filter_map(|(value, count)| (count > 1).then_some(value))
		.collect()
}

fn includes_all(actual: &[String], expected: &[String]) -> bool {
	expected
		.iter()
		.all(|value| actual.binary_search(value).is_ok())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::dataset::{GameIdentity, SnapshotObjectRef};

	fn write(path: &Path, content: &str) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, content).unwrap();
	}

	fn snapshot() -> SnapshotRecord {
		SnapshotRecord::new(
			"case-a".to_string(),
			GameIdentity {
				app_id: 236850,
				version: "v1.37.5.0".to_string(),
				steam_build_id: Some(1),
			},
			SnapshotObjectRef {
				workshop_id: "compatch".to_string(),
				content_hash: "a".repeat(64),
			},
			vec![
				SnapshotObjectRef {
					workshop_id: "left".to_string(),
					content_hash: "b".repeat(64),
				},
				SnapshotObjectRef {
					workshop_id: "right".to_string(),
					content_hash: "c".repeat(64),
				},
			],
		)
	}

	fn game(root: &Path) -> Eu4GameDiscovery {
		Eu4GameDiscovery {
			game_root: root.to_path_buf(),
			game_version: "v1.37.5.0".to_string(),
			steam_build_id: Some(1),
			steam_root: None,
		}
	}

	fn score(accepted: bool) -> FileRecord {
		FileRecord {
			rel: "events/a.txt".to_string(),
			source_mod_ids: vec!["left".to_string(), "right".to_string()],
			source_count: 2,
			multi_source: true,
			foch_emitted: true,
			foch_conflict: false,
			similarity: None,
			keys_match: Some(true),
			ast_match: Some(accepted),
			dropped_keys: Vec::new(),
			verdict: if accepted {
				"matches_ast".to_string()
			} else {
				"diverges_ast".to_string()
			},
			accepted_ok: accepted,
			acceptance_reason: None,
		}
	}

	fn arm(kernel: &str, accepted: bool) -> ShadowArmAssessment {
		ShadowArmAssessment {
			kernel: kernel.to_string(),
			status: "ready".to_string(),
			output_valid: true,
			elapsed_ms: 10,
			output_hash: Some(format!("{kernel}-hash")),
			score: Some(score(accepted)),
			semantic_diff_from_human: None,
			semantic_diff_from_sources: BTreeMap::new(),
			semantic_diff_from_base: None,
			event_safety: None,
			diagnostics: Vec::new(),
			error: None,
		}
	}

	fn baseline_artifact(score: FileRecord, expected_content_id: String) -> LegacyBaselineArtifact {
		LegacyBaselineArtifact {
			schema: "1.0.0".to_string(),
			scorer_version: SCORER_VERSION.to_string(),
			expected_content_id,
			units: vec![LegacyBaselineUnit {
				case_id: "case-a".to_string(),
				score,
			}],
		}
	}

	#[test]
	fn candidate_selection_requires_an_explicit_case_and_path() {
		assert_eq!(
			"case-a:events/a.txt".parse::<CorpusShadowSelection>(),
			Ok(CorpusShadowSelection {
				case_id: "case-a".to_string(),
				relative_path: "events/a.txt".to_string(),
			})
		);
		assert!("case-a".parse::<CorpusShadowSelection>().is_err());
		assert!(":events/a.txt".parse::<CorpusShadowSelection>().is_err());
	}

	#[test]
	fn legacy_baseline_must_cover_targets_and_match_expected_verdicts() {
		let temp = tempfile::tempdir().unwrap();
		let baseline_path = temp.path().join("legacy-baseline.json");
		let expected_path = temp.path().join("expected.json");
		let baseline_score = score(true);
		let expected_bytes = serde_json::to_vec(&BTreeMap::from([(
			"case-a".to_string(),
			BTreeMap::from([("matches_ast".to_string(), 1)]),
		)]))
		.unwrap();
		fs::write(&expected_path, &expected_bytes).unwrap();
		fs::write(
			&baseline_path,
			serde_json::to_vec(&baseline_artifact(
				baseline_score.clone(),
				stable_id("legacy-expected-v1", &[&expected_bytes]),
			))
			.unwrap(),
		)
		.unwrap();
		let target = make_target(
			TargetIdentity {
				snapshot: &snapshot(),
				relative_path: "events/a.txt",
				source_mod_ids: &["left".to_string(), "right".to_string()],
				base_snapshot_identity: "base",
				executable_hash: "exe",
				scorer_config_hash: "config",
			},
			&game(Path::new("/game")),
		);

		let (identity, scores) = load_legacy_baseline(
			&baseline_path,
			&expected_path,
			std::slice::from_ref(&target),
		)
		.unwrap();

		assert_eq!(identity.unit_count, 1);
		assert_eq!(scores.len(), 1);
		assert_eq!(scores.values().next(), Some(&baseline_score));

		fs::write(
			&expected_path,
			serde_json::to_vec(&BTreeMap::from([(
				"case-a".to_string(),
				BTreeMap::from([("diverges_ast".to_string(), 1)]),
			)]))
			.unwrap(),
		)
		.unwrap();
		let error = load_legacy_baseline(&baseline_path, &expected_path, &[target]).unwrap_err();
		assert!(error.to_string().contains("not bound"));
	}

	#[test]
	fn target_identity_is_content_based_and_deterministic() {
		let temp = tempfile::tempdir().unwrap();
		let snapshot = snapshot();
		let game = game(temp.path());
		let source_ids = vec!["left".to_string(), "right".to_string()];
		let make = || {
			make_target(
				TargetIdentity {
					snapshot: &snapshot,
					relative_path: "events/a.txt",
					source_mod_ids: &source_ids,
					base_snapshot_identity: "base",
					executable_hash: "exe",
					scorer_config_hash: "config",
				},
				&game,
			)
		};

		assert_eq!(make().unit_id, make().unit_id);
		assert!(
			!make()
				.unit_id
				.contains(temp.path().to_string_lossy().as_ref())
		);
		let changed_config = make_target(
			TargetIdentity {
				snapshot: &snapshot,
				relative_path: "events/a.txt",
				source_mod_ids: &source_ids,
				base_snapshot_identity: "base",
				executable_hash: "exe",
				scorer_config_hash: "force-enabled-config",
			},
			&game,
		);
		assert_ne!(make().unit_id, changed_config.unit_id);
	}

	#[test]
	fn snapshot_game_validation_rejects_steam_build_drift() {
		let temp = tempfile::tempdir().unwrap();
		let snapshot = snapshot();
		let mut game = game(temp.path());
		game.steam_build_id = Some(2);

		let error = validate_snapshot_game(&snapshot, &game).unwrap_err();

		assert!(error.to_string().contains("Steam build mismatch"));
	}

	#[test]
	fn discovers_only_multi_source_scoring_units() {
		let temp = tempfile::tempdir().unwrap();
		let compatch = temp.path().join("compatch");
		let left = temp.path().join("left");
		let right = temp.path().join("right");
		let game_root = temp.path().join("game");
		let event = "namespace = test\ncountry_event = { id = test.1 option = { name = ok } }\n";
		for root in [&compatch, &left, &right, &game_root] {
			write(&root.join("events/a.txt"), event);
		}
		write(&compatch.join("events/left-only.txt"), event);
		write(&left.join("events/left-only.txt"), event);
		let loaded = LoadedSnapshot {
			snapshot: snapshot(),
			compatch,
			source_dirs: vec![left, right],
		};
		let identity = RunIdentity {
			base_snapshot_identity: "base".to_string(),
			executable_hash: "exe".to_string(),
			scorer_config_hash: "config".to_string(),
		};
		let targets = discover_targets(
			&loaded,
			&game(&game_root),
			&identity,
			&mut ScoreCache::new(),
			None,
		)
		.unwrap();

		assert_eq!(targets.len(), 1);
		assert_eq!(targets[0].relative_path, "events/a.txt");
		assert_eq!(targets[0].source_mod_ids, ["left", "right"]);

		let filtered = discover_targets(
			&loaded,
			&game(&game_root),
			&identity,
			&mut ScoreCache::new(),
			Some("events/left-only.txt"),
		)
		.unwrap();
		assert!(filtered.is_empty());
	}

	#[test]
	fn valid_event_control_flow_passes_and_ignores_unrelated_sibling_offsets() {
		let temp = tempfile::tempdir().unwrap();
		let human = temp.path().join("human.txt");
		let output = temp.path().join("output.txt");
		write(
			&human,
			"namespace = test\ncountry_event = { id = test.1 option = { name = ok if = { limit = { always = yes } } else = { add_stability = 1 } } }\n",
		);
		write(
			&output,
			"namespace = test\ncountry_event = { id = test.1 title = test_title option = { name = ok custom_tooltip = tip if = { limit = { always = yes } } else = { add_stability = 1 } } }\n",
		);

		let safety = assess_event_safety(&output, &human);

		assert!(safety.parse_ok);
		assert!(safety.orphan_control_flow_paths.is_empty());
		assert_eq!(safety.namespaces_include_human, Some(true));
		assert_eq!(safety.event_ids_include_human, Some(true));
		assert_eq!(safety.option_ids_include_human, Some(true));
		assert_eq!(safety.control_flow_matches_human, Some(true));
		assert!(safety.passed());
	}

	#[test]
	fn valid_event_control_flow_ignores_top_level_event_order() {
		let temp = tempfile::tempdir().unwrap();
		let human = temp.path().join("human.txt");
		let output = temp.path().join("output.txt");
		let first = "country_event = { id = test.1 option = { name = first if = { always = yes } else = { always = no } } }";
		let second =
			"country_event = { id = test.2 option = { name = second if = { always = yes } } }";
		write(&human, &format!("namespace = test\n{first}\n{second}\n"));
		write(&output, &format!("namespace = test\n{second}\n{first}\n"));

		let safety = assess_event_safety(&output, &human);

		assert_eq!(safety.control_flow_matches_human, Some(true));
		assert!(safety.passed());
	}

	#[test]
	fn event_safety_rejects_orphan_else_and_duplicate_anchors() {
		let temp = tempfile::tempdir().unwrap();
		let human = temp.path().join("human.txt");
		let output = temp.path().join("output.txt");
		write(
			&human,
			"namespace = test\ncountry_event = { id = test.1 option = { name = ok if = { always = yes } else = { always = no } } }\n",
		);
		write(
			&output,
			"namespace = test\ncountry_event = { id = test.1 option = { name = ok else = { always = no } } }\ncountry_event = { id = test.1 option = { name = ok } }\n",
		);

		let safety = assess_event_safety(&output, &human);

		assert_eq!(safety.duplicate_event_ids, ["test.1"]);
		assert_eq!(safety.duplicate_option_ids, ["test.1::ok"]);
		assert_eq!(safety.orphan_control_flow_paths.len(), 1);
		assert!(!safety.passed());
	}

	#[test]
	fn event_safety_rejects_control_flow_shape_loss() {
		let temp = tempfile::tempdir().unwrap();
		let human = temp.path().join("human.txt");
		let output = temp.path().join("output.txt");
		write(
			&human,
			"namespace = test\ncountry_event = { id = test.1 option = { name = ok if = { always = yes } else = { always = no } } }\n",
		);
		write(
			&output,
			"namespace = test\ncountry_event = { id = test.1 option = { name = ok } }\n",
		);

		let safety = assess_event_safety(&output, &human);

		assert_eq!(safety.control_flow_matches_human, Some(false));
		assert!(!safety.passed());
	}

	#[test]
	fn outcome_preserves_unsupported_and_quality_deltas() {
		let legacy = arm("legacy", false);
		let structured = arm("structured", true);
		assert_eq!(
			classify_outcome(&legacy, &structured),
			CorpusShadowOutcome::Improved
		);

		let mut unsupported = arm("structured", false);
		unsupported.output_valid = false;
		unsupported.error = Some("structured merge unsupported: events only".to_string());
		assert_eq!(
			classify_outcome(&legacy, &unsupported),
			CorpusShadowOutcome::StructuredUnsupported
		);

		let mut failed_legacy = legacy;
		failed_legacy.output_valid = false;
		failed_legacy.status = "timed_out".to_string();
		assert_eq!(
			classify_outcome(&failed_legacy, &unsupported),
			CorpusShadowOutcome::Failed
		);
	}

	#[test]
	fn summary_retains_legacy_for_unselected_units_without_running_structured() {
		let target = make_target(
			TargetIdentity {
				snapshot: &snapshot(),
				relative_path: "events/a.txt",
				source_mod_ids: &["left".to_string(), "right".to_string()],
				base_snapshot_identity: "base",
				executable_hash: "exe",
				scorer_config_hash: "config",
			},
			&game(Path::new("/game")),
		);
		let record = CorpusShadowProjectionUnit {
			target,
			disposition: CorpusShadowDisposition::LegacyRetained,
			legacy_baseline: score(true),
			candidate: None,
			projected_strict_accepted: true,
			projected_adjudicated_accepted: true,
		};

		let summary = summarize(&[record]);

		assert_eq!(summary.legacy_strict_accepted, 1);
		assert_eq!(summary.legacy_adjudicated_accepted, 1);
		assert_eq!(summary.candidate_strict_accepted, 0);
		assert_eq!(summary.candidate_adjudicated_accepted, 0);
		assert_eq!(summary.projected_strict_accepted, 1);
		assert_eq!(summary.projected_adjudicated_accepted, 1);
		assert_eq!(summary.legacy_strict_accepted_lost, 0);
		assert_eq!(summary.legacy_adjudicated_accepted_lost, 0);
		assert_eq!(summary.legacy_retained, 1);
		assert_eq!(summary.candidate_evaluated, 0);
		assert_eq!(summary.structured_unsupported, 0);
		assert_eq!(summary.non_gui_units, 1);
		assert_eq!(summary.legacy_strict_accepted_non_gui, 1);
		assert_eq!(summary.legacy_adjudicated_accepted_non_gui, 1);
		assert_eq!(summary.candidate_strict_accepted_non_gui, 0);
		assert_eq!(summary.candidate_adjudicated_accepted_non_gui, 0);
		assert_eq!(summary.projected_strict_accepted_non_gui, 1);
		assert_eq!(summary.projected_adjudicated_accepted_non_gui, 1);
		assert_eq!(summary.legacy_strict_accepted_non_gui_lost, 0);
		assert_eq!(summary.legacy_adjudicated_accepted_non_gui_lost, 0);
	}

	#[test]
	fn summary_never_projects_a_safety_failed_arm_as_accepted() {
		let target = make_target(
			TargetIdentity {
				snapshot: &snapshot(),
				relative_path: "events/a.txt",
				source_mod_ids: &["left".to_string(), "right".to_string()],
				base_snapshot_identity: "base",
				executable_hash: "exe",
				scorer_config_hash: "config",
			},
			&game(Path::new("/game")),
		);
		let mut structured = arm("structured", true);
		structured.event_safety = Some(EventSafetyAssessment {
			parse_ok: true,
			diagnostic_count: 0,
			namespaces: vec!["test".to_string()],
			event_ids: vec!["test.1".to_string()],
			option_ids: vec!["test.1::ok".to_string()],
			duplicate_event_ids: Vec::new(),
			duplicate_option_ids: Vec::new(),
			orphan_control_flow_paths: Vec::new(),
			control_flow_shape: Vec::new(),
			human_parse_ok: true,
			namespaces_include_human: Some(true),
			event_ids_include_human: Some(true),
			option_ids_include_human: Some(true),
			control_flow_matches_human: Some(false),
		});
		let candidate = CorpusShadowUnitRecord {
			schema: CORPUS_SHADOW_SCHEMA.to_string(),
			record_id: "record".to_string(),
			target: target.clone(),
			comparison_id: "comparison".to_string(),
			observed_at: "now".to_string(),
			human_resolution: None,
			legacy: arm("legacy", true),
			structured,
			structured_vs_legacy: None,
			outcome: CorpusShadowOutcome::SafetyFailed,
		};
		let record = CorpusShadowProjectionUnit {
			target,
			disposition: CorpusShadowDisposition::CandidateEvaluated,
			legacy_baseline: score(true),
			candidate: Some(candidate),
			projected_strict_accepted: false,
			projected_adjudicated_accepted: false,
		};

		let summary = summarize(&[record]);

		assert_eq!(summary.candidate_strict_accepted, 1);
		assert_eq!(summary.candidate_adjudicated_accepted, 1);
		assert_eq!(summary.projected_strict_accepted, 0);
		assert_eq!(summary.projected_adjudicated_accepted, 0);
		assert_eq!(summary.legacy_strict_accepted_lost, 1);
		assert_eq!(summary.legacy_adjudicated_accepted_lost, 1);
		assert_eq!(summary.safety_failed, 1);
		assert_eq!(summary.non_gui_units, 1);
		assert_eq!(summary.legacy_strict_accepted_non_gui, 1);
		assert_eq!(summary.legacy_adjudicated_accepted_non_gui, 1);
		assert_eq!(summary.candidate_strict_accepted_non_gui, 1);
		assert_eq!(summary.candidate_adjudicated_accepted_non_gui, 1);
		assert_eq!(summary.projected_strict_accepted_non_gui, 0);
		assert_eq!(summary.projected_adjudicated_accepted_non_gui, 0);
		assert_eq!(summary.legacy_strict_accepted_non_gui_lost, 1);
		assert_eq!(summary.legacy_adjudicated_accepted_non_gui_lost, 1);
	}

	#[test]
	fn summary_excludes_gui_units_from_rollout_quality_counts() {
		let target = make_target(
			TargetIdentity {
				snapshot: &snapshot(),
				relative_path: "interface/a.gui",
				source_mod_ids: &["left".to_string(), "right".to_string()],
				base_snapshot_identity: "base",
				executable_hash: "exe",
				scorer_config_hash: "config",
			},
			&game(Path::new("/game")),
		);
		let candidate = CorpusShadowUnitRecord {
			schema: CORPUS_SHADOW_SCHEMA.to_string(),
			record_id: "record".to_string(),
			target: target.clone(),
			comparison_id: "comparison".to_string(),
			observed_at: "now".to_string(),
			human_resolution: None,
			legacy: arm("legacy", true),
			structured: arm("structured", false),
			structured_vs_legacy: None,
			outcome: CorpusShadowOutcome::Regressed,
		};
		let record = CorpusShadowProjectionUnit {
			target,
			disposition: CorpusShadowDisposition::CandidateEvaluated,
			legacy_baseline: score(true),
			candidate: Some(candidate),
			projected_strict_accepted: false,
			projected_adjudicated_accepted: false,
		};

		let summary = summarize(&[record]);

		assert_eq!(summary.total_units, 1);
		assert_eq!(summary.legacy_strict_accepted, 1);
		assert_eq!(summary.legacy_adjudicated_accepted, 1);
		assert_eq!(summary.legacy_strict_accepted_lost, 1);
		assert_eq!(summary.legacy_adjudicated_accepted_lost, 1);
		assert_eq!(summary.non_gui_units, 0);
		assert_eq!(summary.legacy_strict_accepted_non_gui, 0);
		assert_eq!(summary.legacy_adjudicated_accepted_non_gui, 0);
		assert_eq!(summary.candidate_strict_accepted_non_gui, 0);
		assert_eq!(summary.candidate_adjudicated_accepted_non_gui, 0);
		assert_eq!(summary.projected_strict_accepted_non_gui, 0);
		assert_eq!(summary.projected_adjudicated_accepted_non_gui, 0);
		assert_eq!(summary.legacy_strict_accepted_non_gui_lost, 0);
		assert_eq!(summary.legacy_adjudicated_accepted_non_gui_lost, 0);
	}

	#[test]
	fn gfx_units_remain_in_the_non_gui_rollout_denominator() {
		assert!(!is_gui_path("interface/000_expanded_mod_family.gfx"));
		assert!(is_gui_path("interface/frontend.gui"));
	}

	#[test]
	fn report_renders_non_gui_rollout_evidence() {
		let summary = CorpusShadowSummary {
			total_units: 36,
			legacy_retained: 35,
			candidate_evaluated: 1,
			non_gui_units: 21,
			legacy_strict_accepted: 7,
			legacy_adjudicated_accepted: 7,
			projected_strict_accepted: 10,
			projected_adjudicated_accepted: 11,
			legacy_strict_accepted_lost: 2,
			legacy_adjudicated_accepted_lost: 3,
			legacy_strict_accepted_non_gui: 7,
			legacy_adjudicated_accepted_non_gui: 7,
			candidate_strict_accepted_non_gui: 3,
			candidate_adjudicated_accepted_non_gui: 4,
			projected_strict_accepted_non_gui: 10,
			projected_adjudicated_accepted_non_gui: 11,
			legacy_strict_accepted_non_gui_lost: 0,
			legacy_adjudicated_accepted_non_gui_lost: 0,
			supported_runtime_ratio_milli: Some(1_086),
			..CorpusShadowSummary::default()
		};
		let report = render_projection_report(&CorpusShadowReport {
			schema: CORPUS_SHADOW_REPORT_SCHEMA.to_string(),
			generated_at: "now".to_string(),
			legacy_baseline: LegacyBaselineIdentity {
				schema: "1.0.0".to_string(),
				scorer_version: SCORER_VERSION.to_string(),
				baseline_content_id: "baseline".to_string(),
				expected_content_id: "expected".to_string(),
				unit_count: 36,
			},
			targets: Vec::new(),
			units: Vec::new(),
			summary,
		});

		assert!(report.contains("Units: 36 | candidate evaluated: 1 | Legacy retained: 35"));
		assert!(report.contains(
			"Strict: Legacy accepted 7/36 | projected accepted 10/36 | Legacy accepted lost 2"
		));
		assert!(report.contains(
			"Adjudicated: Legacy accepted 7/36 | projected accepted 11/36 | Legacy accepted lost 3"
		));
		assert!(report.contains("Non-GUI strict: Legacy accepted 7/21 | projected accepted 10/21"));
		assert!(
			report.contains("Non-GUI adjudicated: Legacy accepted 7/21 | projected accepted 11/21")
		);
		assert!(report.contains("Candidate runtime ratio: 1.086x Legacy"));
	}

	#[test]
	fn resume_rejects_an_assessment_without_its_comparison_evidence() {
		let temp = tempfile::tempdir().unwrap();
		let source_ids = vec!["left".to_string(), "right".to_string()];
		let target = make_target(
			TargetIdentity {
				snapshot: &snapshot(),
				relative_path: "events/a.txt",
				source_mod_ids: &source_ids,
				base_snapshot_identity: "base",
				executable_hash: "exe",
				scorer_config_hash: "config",
			},
			&game(Path::new("/game")),
		);
		let record = CorpusShadowUnitRecord {
			schema: CORPUS_SHADOW_SCHEMA.to_string(),
			record_id: "record".to_string(),
			target: target.clone(),
			comparison_id: "comparison".to_string(),
			observed_at: "now".to_string(),
			human_resolution: None,
			legacy: arm("legacy", true),
			structured: arm("structured", true),
			structured_vs_legacy: None,
			outcome: CorpusShadowOutcome::UnchangedAccepted,
		};
		fs::write(
			temp.path().join("assessment.json"),
			serde_json::to_vec(&record).unwrap(),
		)
		.unwrap();

		assert!(resume_record(temp.path(), &target, Path::new("/game")).is_none());
	}

	#[test]
	fn invalid_arm_resume_rejects_leftover_output() {
		let temp = tempfile::tempdir().unwrap();
		let mut assessment = arm("structured", false);
		assessment.status = "crashed".to_string();
		assessment.output_valid = false;
		assessment.output_hash = None;
		assessment.score = None;
		assessment.error = Some("failed".to_string());
		let run = ShadowRunRecord {
			schema: crate::shadow::SHADOW_COMPARE_SCHEMA.to_string(),
			comparison_id: "comparison".to_string(),
			kernel: "structured".to_string(),
			output_dir: temp.path().join("structured"),
			output_valid: false,
			elapsed_ms: 10,
			status: "crashed".to_string(),
			exit_code: None,
			manual_conflict_count: None,
			handler_resolution_count: None,
			generated_file_count: None,
			fatal_reason: None,
			error: Some("failed".to_string()),
			diagnostics: Vec::new(),
		};
		write(
			&temp.path().join("structured/events/partial.txt"),
			"partial",
		);

		assert!(!arm_artifact_matches(
			temp.path(),
			"structured",
			&assessment,
			&run,
		));
	}
}
