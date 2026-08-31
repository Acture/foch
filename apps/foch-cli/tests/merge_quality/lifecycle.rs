use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use foch::model::{
	MergeBackendId, MergeReport, MergeReportStatus, PRODUCT_INPUT_PROFILE, ProductInputManifest,
};
use foch::playset::steam::{SteamId, WorkshopInstallIdentity};
use serde::{Deserialize, Serialize};

use crate::merge_quality::config::Eu4Discovery;
use crate::merge_quality::corpus::{Case, ORACLE_POLICY_VERSION, assess_oracle_candidate};
use crate::merge_quality::dataset::{
	DatasetPaths, EngineArtifactIdentity, FileResultRecord, InputVersionRecord,
	MeasurementIdentityV2, MeasurementRecord, MeasurementScope, MeasurementSummary, SCORER_VERSION,
	TerminalStatus, WorkshopItemObservationV2, WorkshopObservationRecordV2, append_unique,
	append_unique_many, append_workshop_observation_v2, now_rfc3339, read_jsonl, stable_id,
};
use crate::merge_quality::evidence_store::{
	EvidenceBundleInput, EvidenceEntryInput, EvidenceEntryKind, EvidenceStore,
	read_stable_source_file,
};
use crate::merge_quality::orchestrate::{
	CaseResult, ExistingOutputScore, FileRecord, ScoreExistingOutputRequest,
	score_existing_output_with_cache,
};
use crate::merge_quality::report::{
	MeasurementCohortSelector, WorkshopMeasurementReport, WorkshopReportCase, WorkshopReportCohort,
	WorkshopReportRequest, build_workshop_measurement_report,
	committed_measurement_cohort_registry,
};
use crate::merge_quality::score::{
	ScoreCache, reference_output_files, scoring_evidence_files,
	scoring_evidence_path_belongs_to_unit, scoring_reference_units,
};
use crate::merge_quality::workshop_inputs::{
	ResolvedWorkshopCase, WorkshopCaseManifest, resolve_workshop_cases,
	validate_workshop_cases_unchanged,
};

#[derive(Clone, Debug)]
pub struct WorkshopMeasureOptions<'a> {
	pub case_manifest: &'a Path,
	pub dataset_root: &'a Path,
	pub discovery: &'a Eu4Discovery,
	pub timeout: Duration,
	pub basegame_root: &'a Path,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopMeasureRunSummary {
	pub selected: usize,
	pub cached: usize,
	pub measured: usize,
	pub failed: usize,
	pub cohort_id: String,
	pub product_hash: String,
	pub scorer_config_hash: String,
	pub input_version_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct WorkshopReportOptions<'a> {
	pub case_manifest: &'a Path,
	pub dataset_root: &'a Path,
	pub discovery: &'a Eu4Discovery,
	pub output_dir: &'a Path,
	pub cohort_id: &'a str,
	pub cohort: WorkshopReportCohort,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MeasurementRunnerIdentity {
	pub engine_artifact: EngineArtifactIdentity,
	pub runner_protocol_version: String,
	pub backend: MergeBackendId,
	pub scope: MeasurementScope,
}

#[derive(Clone, Debug)]
pub struct MeasurementRequest {
	pub input_version_id: String,
	pub case: Case,
	pub compatch_dir: PathBuf,
	pub source_dirs: Vec<PathBuf>,
	/// Ordered ACF identities for product inputs. Real Workshop measurements
	/// require this; synthetic local fixtures may omit it.
	pub source_manifest: Option<ProductInputManifest>,
	pub output_dir: PathBuf,
	pub basegame_root: PathBuf,
	pub expected_base_snapshot_identity: String,
	pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub enum TerminalMerge {
	Completed {
		report: Box<MergeReport>,
		merge_ms: u64,
	},
	MergeFailed {
		detail: String,
	},
	Crashed {
		detail: Option<String>,
	},
	TimedOut {
		detail: Option<String>,
	},
	Fatal {
		detail: String,
	},
}

pub trait MeasurementRunner {
	fn identity(&self) -> &MeasurementRunnerIdentity;

	/// Validate mutable runner prerequisites, including the executable bound by
	/// [`Self::identity`], before any measurement is resumed or executed.
	fn preflight(&self) -> Result<(), String>;

	fn run(&mut self, request: &MeasurementRequest) -> TerminalMerge;
}

struct CapturedScoringInputs {
	_root: tempfile::TempDir,
	compatch_dir: PathBuf,
	source_dirs: Vec<PathBuf>,
	output_dir: PathBuf,
	basegame_root: PathBuf,
	scorer_closure: ScorerCaseClosure,
}

struct CompletedMeasurement {
	score: ExistingOutputScore,
	inputs: CapturedScoringInputs,
}

struct PreparedWorkshopMeasurement {
	resolved: ResolvedWorkshopCase,
	input_version: InputVersionRecord,
	identity: MeasurementIdentityV2,
	measurement_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ScorerCaseClosure {
	/// First-run scorer inputs and compact evidence only. Workshop ACF identity
	/// is the authoritative installation version, so this closure is not part of
	/// input or cohort identity and is never rebuilt for cached/report reads.
	scoring_units: Vec<String>,
	base_scoring_closure_digest: String,
}

struct CachedWorkshopValidation<'a> {
	evidence_store: &'a EvidenceStore,
	input_version: &'a InputVersionRecord,
	resolved: &'a ResolvedWorkshopCase,
	runner_identity: &'a MeasurementRunnerIdentity,
	scorer_config_hash: &'a str,
	timeout: Duration,
	base_snapshot_identity: &'a str,
}

struct WorkshopEvidenceRequest<'a> {
	input_version: &'a InputVersionRecord,
	resolved: &'a ResolvedWorkshopCase,
	measurement: &'a MeasurementRequest,
	report: &'a MergeReport,
	file_results: &'a [FileResultRecord],
	runner_identity: &'a MeasurementRunnerIdentity,
	scorer_config_hash: &'a str,
	captured: &'a CapturedScoringInputs,
}

struct WorkshopReportEvidenceValidation<'a> {
	cases: &'a [WorkshopReportCase],
	resolved_cases: &'a [ResolvedWorkshopCase],
	measurements: &'a [MeasurementRecord],
	file_results: &'a [FileResultRecord],
	evidence_store: &'a EvidenceStore,
}

type TerminalClassification = (TerminalStatus, Option<String>, Option<CompletedMeasurement>);

const SCORER_EVIDENCE_INDEX_SCHEMA: &str = "1.0.0";

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkshopProductInputEvidence {
	input_version: InputVersionRecord,
	compatch: WorkshopInstallIdentity,
	source_manifest: ProductInputManifest,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScorerEvidenceReference {
	kind: EvidenceEntryKind,
	relative_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScorerEvidenceUnit {
	relative_path: String,
	entries: Vec<ScorerEvidenceReference>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScorerEvidenceIndex {
	schema: String,
	units: Vec<ScorerEvidenceUnit>,
}

/// Measure the fixed logical cohort directly from read-only Steam Workshop
/// inputs. This path never constructs or opens the legacy input CAS.
pub fn measure_workshop_with_runner(
	options: &WorkshopMeasureOptions<'_>,
	runner: &mut dyn MeasurementRunner,
) -> Result<WorkshopMeasureRunSummary, Box<dyn std::error::Error>> {
	let paths = DatasetPaths::new(options.dataset_root);
	paths.ensure_workshop_layout()?;
	let case_manifest = WorkshopCaseManifest::from_path(options.case_manifest)?;
	if case_manifest.app_id != crate::merge_quality::config::EU4_APPID.to_string() {
		return Err(format!(
			"Workshop case manifest app id {} does not match EU4 {}",
			case_manifest.app_id,
			crate::merge_quality::config::EU4_APPID
		)
		.into());
	}
	if options.discovery.workshop.app_id() != crate::merge_quality::config::EU4_APPID {
		return Err("Workshop catalog belongs to the wrong Steam app".into());
	}
	runner.preflight().map_err(|error| {
		format!(
			"measurement runner preflight failed: {}",
			redact_remaining_absolute_paths(&error)
		)
	})?;
	let runner_identity = runner.identity().clone();
	validate_runner_identity(&runner_identity)?;

	let local_game = crate::merge_quality::config::discover_eu4_game(
		&crate::merge_quality::config::DiscoveryOverrides {
			game_root: Some(options.basegame_root.to_path_buf()),
			steam_root: options.discovery.steam_root.clone(),
			..crate::merge_quality::config::DiscoveryOverrides::default()
		},
	)
	.map_err(|error| {
		format!(
			"failed to identify the Workshop measurement base game: {}",
			redact_remaining_absolute_paths(&error)
		)
	})?;
	if local_game.game_version != options.discovery.game_version
		|| local_game.steam_build_id != options.discovery.steam_build_id
	{
		return Err("Workshop discovery and measurement base-game identity differ".into());
	}
	let steam_build_id = local_game.steam_build_id.ok_or(
		"full-product Workshop measurement requires a Steam build id from appmanifest_236850.acf",
	)?;
	let base_snapshot = foch::game::eu4::base::snapshot::installed_base_snapshot_identity(
		"eu4",
		&local_game.game_version,
	)?
	.ok_or_else(|| {
		format!(
			"no installed base snapshot for eu4@{}",
			local_game.game_version
		)
	})?;
	let base_snapshot_label = base_snapshot.as_label();
	let preflight_started = Instant::now();
	let resolved_cases = resolve_workshop_cases(
		&options.discovery.workshop,
		&case_manifest.cases,
		|position, total, workshop_id| {
			eprintln!(
				"[workshop-preflight] item {position}/{total} {workshop_id} ({})",
				progress(position, total, preflight_started)
			);
		},
	)
	.map_err(|error| {
		format!(
			"Workshop input preflight failed: {}",
			redact_remaining_absolute_paths(&error)
		)
	})?;
	let scorer_config_hash = workshop_scorer_config_hash(
		options.timeout,
		&local_game.game_version,
		steam_build_id,
		&base_snapshot_label,
	);

	let mut cohort_id = None;
	let mut input_version_ids = Vec::with_capacity(resolved_cases.len());
	let mut prepared = Vec::with_capacity(resolved_cases.len());
	for resolved in resolved_cases {
		let input_version =
			resolved.input_version(&local_game.game_version, Some(steam_build_id))?;
		if !input_version.identity_is_valid() {
			return Err(format!(
				"Workshop input version identity is invalid for case {}",
				resolved.definition.case_id
			)
			.into());
		}
		let identity = MeasurementIdentityV2 {
			input_version_id: input_version.input_version_id.clone(),
			engine_artifact: runner_identity.engine_artifact.clone(),
			runner_protocol_version: runner_identity.runner_protocol_version.clone(),
			backend: runner_identity.backend,
			scope: runner_identity.scope,
			scorer_version: SCORER_VERSION.to_string(),
			scorer_config_hash: scorer_config_hash.clone(),
		};
		let current_cohort_id = identity.cohort_id();
		if let Some(expected) = cohort_id.as_deref() {
			if expected != current_cohort_id {
				return Err("Workshop cases resolved to multiple measurement cohorts".into());
			}
		} else {
			cohort_id = Some(current_cohort_id);
		}
		let measurement_id = identity.measurement_id();
		input_version_ids.push(input_version.input_version_id.clone());
		prepared.push(PreparedWorkshopMeasurement {
			resolved,
			input_version,
			identity,
			measurement_id,
		});
	}

	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements)?;
	let measurements_by_id = index_measurements(&measurements)?;
	let file_results = read_jsonl::<FileResultRecord>(&paths.file_results)?;
	let measurement_ids = measurements
		.iter()
		.map(|measurement| measurement.measurement_id().to_string())
		.collect::<HashSet<_>>();
	let recoverable_measurement_ids = prepared
		.iter()
		.map(|measurement| measurement.measurement_id.clone())
		.collect::<HashSet<_>>();
	let mut v2_measurement_ids = measurements
		.iter()
		.filter(|measurement| matches!(measurement, MeasurementRecord::V2 { .. }))
		.map(|measurement| measurement.measurement_id().to_string())
		.collect::<HashSet<_>>();
	v2_measurement_ids.extend(recoverable_measurement_ids.iter().cloned());
	validate_file_result_foreign_keys(
		&file_results,
		&measurement_ids,
		&v2_measurement_ids,
		&recoverable_measurement_ids,
	)?;

	let evidence_store = EvidenceStore::for_dataset(&paths);
	let mut score_cache = ScoreCache::new();
	let mut cached = 0_usize;
	let mut measured = 0_usize;
	let mut failed = 0_usize;
	let started = Instant::now();

	for (index, prepared_measurement) in prepared.into_iter().enumerate() {
		let PreparedWorkshopMeasurement {
			resolved,
			input_version,
			identity,
			measurement_id,
		} = prepared_measurement;
		let definition = &resolved.definition;
		eprintln!(
			"[workshop-measure] case {}/{} {} ({})",
			index + 1,
			case_manifest.cases.len(),
			definition.case_id,
			progress(index + 1, case_manifest.cases.len(), started)
		);
		if let Some(record) = measurements_by_id.get(measurement_id.as_str()) {
			resolved.validate_unchanged(&options.discovery.workshop)?;
			validate_cached_workshop_measurement(
				record,
				&file_results,
				&CachedWorkshopValidation {
					evidence_store: &evidence_store,
					input_version: &input_version,
					resolved: &resolved,
					runner_identity: &runner_identity,
					scorer_config_hash: &scorer_config_hash,
					timeout: options.timeout,
					base_snapshot_identity: &base_snapshot_label,
				},
			)?;
			cached += 1;
			if record.status().counts_as_merge_failed() {
				failed += 1;
			}
			continue;
		}
		let work = tempfile::Builder::new()
			.prefix("workshop-measurement-")
			.tempdir_in(&paths.evidence_work)?;
		let output_dir = work.path().join("output");
		fs::create_dir(&output_dir)?;
		let request = MeasurementRequest {
			input_version_id: input_version.input_version_id.clone(),
			case: resolved.case(),
			compatch_dir: resolved.compatch.install.content_path.clone(),
			source_dirs: resolved.source_dirs(),
			source_manifest: Some(resolved.product_manifest.clone()),
			output_dir,
			basegame_root: options.basegame_root.to_path_buf(),
			expected_base_snapshot_identity: base_snapshot_label.clone(),
			timeout: options.timeout,
		};
		let started_at = now_rfc3339();
		let terminal = runner.run(&request);
		let completed_report = match &terminal {
			TerminalMerge::Completed { report, .. } => Some((**report).clone()),
			_ => None,
		};
		if let Some(report) = completed_report
			.as_ref()
			.filter(|report| report.status != MergeReportStatus::Fatal)
			&& report.input.as_ref() != Some(&resolved.product_manifest.attestation())
		{
			return Err(format!(
				"product merge report input does not match prepared Workshop case {}",
				definition.case_id
			)
			.into());
		}
		let current_base = foch::game::eu4::base::snapshot::installed_base_snapshot_identity(
			"eu4",
			&local_game.game_version,
		)?
		.ok_or_else(|| "installed base snapshot disappeared during measurement".to_string())?;
		if current_base.as_label() != base_snapshot_label {
			return Err(format!(
				"installed base snapshot changed during case {}",
				definition.case_id
			)
			.into());
		}
		validate_live_game_identity(&local_game, options.basegame_root, "Workshop measurement")?;
		let (status, detail, mut completed) =
			classify_terminal_merge(terminal, &request, &mut score_cache)?;
		resolved.validate_unchanged(&options.discovery.workshop)?;
		let detail = detail.map(|detail| redact_persisted_detail(&detail, &request));
		let summary = completed
			.as_ref()
			.map(|completed| measurement_summary(&completed.score.result));
		let file_evidence = completed.as_mut().map_or_else(Vec::new, |completed| {
			std::mem::take(&mut completed.score.result.files)
				.into_iter()
				.map(|file| {
					let relative_path = file.rel.clone();
					let resolution = completed.score.resolutions.remove(&relative_path);
					FileResultRecord::new(
						measurement_id.clone(),
						relative_path,
						serde_json::json!({
							"score": file,
							"human_resolution": resolution
						}),
					)
				})
				.collect::<Vec<_>>()
		});
		let has_recoverable_file_results = file_results
			.iter()
			.any(|record| record.measurement_id == measurement_id);
		if summary.is_none() && has_recoverable_file_results {
			return Err(format!(
				"case {} has recoverable completed file results, but replay did not complete",
				definition.case_id
			)
			.into());
		}
		if let Some(summary) = &summary {
			validate_replayed_file_results(
				&measurement_id,
				summary,
				&file_results,
				&file_evidence,
			)?;
		}
		let evidence_bundle_hash = if status == TerminalStatus::Completed {
			let report = completed_report
				.as_ref()
				.ok_or("completed Workshop measurement has no merge report")?;
			let entries = workshop_evidence_entries(&WorkshopEvidenceRequest {
				input_version: &input_version,
				resolved: &resolved,
				measurement: &request,
				report,
				file_results: &file_evidence,
				runner_identity: &runner_identity,
				scorer_config_hash: &scorer_config_hash,
				captured: &completed
					.as_ref()
					.ok_or("completed Workshop measurement has no captured scorer inputs")?
					.inputs,
			})?;
			Some(
				evidence_store
					.store(EvidenceBundleInput {
						measurement_id: measurement_id.clone(),
						input_version_id: input_version.input_version_id.clone(),
						entries,
					})?
					.hash,
			)
		} else {
			None
		};
		resolved.validate_unchanged(&options.discovery.workshop)?;
		validate_live_game_identity(&local_game, options.basegame_root, "Workshop measurement")?;

		let observation = workshop_observation(&input_version, &resolved, now_rfc3339())?;
		if !observation.matches_input_version(&input_version) {
			return Err("Workshop ACF observation does not match its input version".into());
		}
		append_unique(&paths.input_versions, &input_version)?;
		append_workshop_observation_v2(&paths.observations, &observation)?;
		if summary.is_some() {
			append_unique_many(&paths.file_results, &file_evidence)?;
		}
		let record = MeasurementRecord::new_v2(
			identity,
			started_at,
			now_rfc3339(),
			status,
			detail,
			evidence_bundle_hash,
			summary,
		);
		if !record.evidence_reference_is_valid() {
			return Err("Workshop measurement has an invalid evidence reference".into());
		}
		append_unique(&paths.measurements, &record)?;
		measured += 1;
		if status.counts_as_merge_failed() {
			failed += 1;
		}
	}

	Ok(WorkshopMeasureRunSummary {
		selected: case_manifest.cases.len(),
		cached,
		measured,
		failed,
		cohort_id: cohort_id.ok_or("Workshop cohort contains no cases")?,
		product_hash: runner_identity.engine_artifact.hash,
		scorer_config_hash,
		input_version_ids,
	})
}

fn validate_live_game_identity(
	expected: &crate::merge_quality::config::Eu4GameDiscovery,
	game_root: &Path,
	operation: &str,
) -> Result<(), Box<dyn std::error::Error>> {
	let observed = crate::merge_quality::config::discover_eu4_game(
		&crate::merge_quality::config::DiscoveryOverrides {
			game_root: Some(game_root.to_path_buf()),
			steam_root: expected.steam_root.clone(),
			..crate::merge_quality::config::DiscoveryOverrides::default()
		},
	)?;
	if observed.game_root != expected.game_root
		|| observed.game_version != expected.game_version
		|| observed.steam_build_id != expected.steam_build_id
	{
		return Err(format!("EU4 installation identity changed during {operation}").into());
	}
	Ok(())
}

fn workshop_scorer_config_hash(
	timeout: Duration,
	game_version: &str,
	steam_build_id: u64,
	base_snapshot_identity: &str,
) -> String {
	// This hash identifies scorer policy, not case data. Ordered Workshop ACF
	// revisions live in input_version_id; per-case scoring closures are captured
	// only when a new measurement actually runs.
	let config = serde_json::json!({
		"scorer_version": SCORER_VERSION,
		"oracle_policy_version": ORACLE_POLICY_VERSION,
		"scope": MeasurementScope::FullProductMerge.as_str(),
		"backend": MergeBackendId::GumtreePcsNway.as_str(),
		"public_command": "foch_merge_confirm_non_interactive",
		"force": false,
		"retained_paths": "all",
		"include_game_base": true,
		"input_source": "steam_workshop_acf_read_only_v1",
		"product_input_profile": PRODUCT_INPUT_PROFILE,
		"timeout_nanos": timeout.as_nanos().to_string(),
		"ordering": "gui_sensitive_else_insensitive",
		"basegame_subtraction": "semantic_atoms_v1",
		"game_version": game_version,
		"steam_build_id": steam_build_id.to_string(),
		"base_snapshot_identity": base_snapshot_identity,
		"multi_source": "all_sources_v1"
	});
	stable_id(
		"workshop-scorer-config-v4",
		&[config.to_string().as_bytes()],
	)
}

fn workshop_product_input_evidence(
	input_version: &InputVersionRecord,
	resolved: &ResolvedWorkshopCase,
) -> WorkshopProductInputEvidence {
	WorkshopProductInputEvidence {
		input_version: input_version.clone(),
		compatch: resolved.compatch.install.identity.clone(),
		source_manifest: resolved.product_manifest.clone(),
	}
}

fn workshop_scorer_evidence(
	runner_identity: &MeasurementRunnerIdentity,
	scorer_config_hash: &str,
	timeout: Duration,
	base_snapshot_identity: &str,
	base_scoring_closure_digest: &str,
	scoring_units: &[String],
) -> serde_json::Value {
	serde_json::json!({
		"scorer_version": SCORER_VERSION,
		"oracle_policy_version": ORACLE_POLICY_VERSION,
		"scorer_config_hash": scorer_config_hash,
		"runner": runner_identity,
		"timeout_nanos": timeout.as_nanos().to_string(),
		"base_snapshot_identity": base_snapshot_identity,
		"base_scoring_closure_digest": base_scoring_closure_digest,
		"scoring_units": scoring_units,
	})
}

fn stored_scorer_closure(
	evidence: &serde_json::Value,
) -> Result<ScorerCaseClosure, Box<dyn std::error::Error>> {
	let scoring_units = evidence
		.get("scoring_units")
		.cloned()
		.ok_or_else(|| "scorer evidence has no scoring units".to_string())
		.and_then(|value| {
			serde_json::from_value::<Vec<String>>(value)
				.map_err(|error| format!("invalid scorer evidence units: {error}"))
		})?;
	let canonical_units = scoring_units.iter().cloned().collect::<BTreeSet<_>>();
	if scoring_units.is_empty()
		|| canonical_units.len() != scoring_units.len()
		|| canonical_units.iter().cloned().collect::<Vec<_>>() != scoring_units
		|| scoring_units.iter().any(|unit| {
			unit.is_empty()
				|| Path::new(unit).is_absolute()
				|| Path::new(unit).components().any(|component| {
					matches!(
						component,
						Component::Prefix(_)
							| Component::RootDir | Component::ParentDir
							| Component::CurDir
					)
				})
		}) {
		return Err("scorer evidence units are unsafe, empty, duplicated, or unsorted".into());
	}
	let base_scoring_closure_digest = evidence
		.get("base_scoring_closure_digest")
		.and_then(serde_json::Value::as_str)
		.ok_or("scorer evidence has no base scoring closure digest")?
		.to_string();
	if base_scoring_closure_digest.len() != 64
		|| !base_scoring_closure_digest
			.bytes()
			.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
	{
		return Err("scorer evidence has an invalid base scoring closure digest".into());
	}
	Ok(ScorerCaseClosure {
		scoring_units,
		base_scoring_closure_digest,
	})
}

/// Write a schema-3 Workshop report from the current read-only ACF cohort and
/// compact V2 measurement evidence. Legacy snapshots and object CAS are never
/// opened by this path.
pub fn report_workshop(
	options: &WorkshopReportOptions<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
	let manifest = WorkshopCaseManifest::from_path(options.case_manifest)?;
	if manifest.app_id != crate::merge_quality::config::EU4_APPID.to_string() {
		return Err("Workshop product report requires an EU4 case manifest".into());
	}
	if options.discovery.workshop.app_id() != crate::merge_quality::config::EU4_APPID {
		return Err("Workshop report catalog belongs to the wrong Steam app".into());
	}
	let local_game = crate::merge_quality::config::discover_eu4_game(
		&crate::merge_quality::config::DiscoveryOverrides {
			game_root: Some(options.discovery.game_root.clone()),
			steam_root: options.discovery.steam_root.clone(),
			..crate::merge_quality::config::DiscoveryOverrides::default()
		},
	)
	.map_err(|error| {
		format!(
			"failed to identify the Workshop report base game: {}",
			redact_remaining_absolute_paths(&error)
		)
	})?;
	if local_game.game_root != options.discovery.game_root
		|| local_game.game_version != options.discovery.game_version
		|| local_game.steam_build_id != options.discovery.steam_build_id
	{
		return Err("Workshop discovery and report base-game identity differ".into());
	}
	let steam_build_id = local_game.steam_build_id.ok_or(
		"full-product Workshop report requires a Steam build id from appmanifest_236850.acf",
	)?;
	let resolve_started = Instant::now();
	let resolved_cases = resolve_workshop_cases(
		&options.discovery.workshop,
		&manifest.cases,
		|position, total, workshop_id| {
			eprintln!(
				"[workshop-report] resolve item {position}/{total} {workshop_id} ({})",
				progress(position, total, resolve_started)
			);
		},
	)?;
	let mut cases = Vec::with_capacity(resolved_cases.len());
	for resolved in &resolved_cases {
		let input_version =
			resolved.input_version(&local_game.game_version, Some(steam_build_id))?;
		let title = resolved.case().title;
		let oracle = assess_oracle_candidate(&title, resolved.sources.len(), false);
		cases.push(WorkshopReportCase::new(input_version, title, oracle));
	}
	let paths = DatasetPaths::new(options.dataset_root);
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements)?;
	let file_results = read_jsonl::<FileResultRecord>(&paths.file_results)?;
	let registry = committed_measurement_cohort_registry()?;
	let generated_at = now_rfc3339();
	let report = build_workshop_measurement_report(WorkshopReportRequest {
		generated_at: &generated_at,
		cases: &cases,
		measurements: &measurements,
		registry: &registry,
		selector: MeasurementCohortSelector::CohortId(options.cohort_id),
		cohort: options.cohort,
	})?;

	let evidence_store = EvidenceStore::for_dataset(&paths);
	validate_workshop_report_evidence(
		&report,
		&WorkshopReportEvidenceValidation {
			cases: &cases,
			resolved_cases: &resolved_cases,
			measurements: &measurements,
			file_results: &file_results,
			evidence_store: &evidence_store,
		},
	)?;
	let validation_started = Instant::now();
	validate_workshop_cases_unchanged(
		&options.discovery.workshop,
		&resolved_cases,
		|position, total, workshop_id| {
			eprintln!(
				"[workshop-report] validate item {position}/{total} {workshop_id} ({})",
				progress(position, total, validation_started)
			);
		},
	)?;
	validate_live_game_identity(&local_game, &local_game.game_root, "Workshop report")?;

	fs::create_dir_all(options.output_dir)?;
	fs::write(
		options.output_dir.join("baseline.json"),
		format!("{}\n", serde_json::to_string_pretty(&report)?),
	)?;
	Ok(())
}

fn validate_workshop_report_evidence(
	report: &WorkshopMeasurementReport,
	validation: &WorkshopReportEvidenceValidation<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
	for reported_case in &report.cases {
		let Some(measurement_id) = reported_case.measurement_id.as_deref() else {
			continue;
		};
		let measurement = validation
			.measurements
			.iter()
			.find(|measurement| measurement.measurement_id() == measurement_id)
			.ok_or_else(|| format!("reported measurement {measurement_id} is missing"))?;
		if measurement.status() != TerminalStatus::Completed {
			continue;
		}
		let case_index = validation
			.cases
			.iter()
			.position(|case| case.input_version.input_version_id == reported_case.input_version_id)
			.ok_or_else(|| {
				format!(
					"reported input version {} is not in the live Workshop cohort",
					reported_case.input_version_id
				)
			})?;
		let case = &validation.cases[case_index];
		let resolved = &validation.resolved_cases[case_index];
		if resolved.definition.case_id != reported_case.case_id {
			return Err(format!(
				"reported case {} does not match its live Workshop input",
				reported_case.case_id
			)
			.into());
		}
		let evidence_hash = reported_case
			.evidence_bundle_hash
			.as_deref()
			.ok_or_else(|| {
				format!("completed measurement {measurement_id} has no evidence bundle")
			})?;
		let bundle = validation.evidence_store.open(evidence_hash)?;
		if bundle.manifest.input_version_id != reported_case.input_version_id
			|| bundle.manifest.measurement_id != measurement_id
		{
			return Err(format!(
				"evidence bundle {evidence_hash} does not match reported case {}",
				reported_case.case_id
			)
			.into());
		}

		let measurement_file_results = validation
			.file_results
			.iter()
			.filter(|record| record.measurement_id == measurement_id)
			.collect::<Vec<_>>();
		let summary = measurement
			.summary()
			.ok_or_else(|| format!("completed measurement {measurement_id} has no summary"))?;
		validate_cached_file_results(measurement_id, summary, &measurement_file_results)?;
		let stored_product_input = read_bundle_json::<WorkshopProductInputEvidence>(
			&bundle,
			"metadata/product-input.json",
		)?;
		if stored_product_input != workshop_product_input_evidence(&case.input_version, resolved) {
			return Err(format!(
				"reported measurement {measurement_id} has stale product-input evidence"
			)
			.into());
		}
		let stored_file_results =
			read_bundle_json::<Vec<FileResultRecord>>(&bundle, "metadata/file-results.json")?;
		let expected_file_results = measurement_file_results
			.iter()
			.map(|record| (*record).clone())
			.collect::<Vec<_>>();
		if canonical_file_results(stored_file_results)
			!= canonical_file_results(expected_file_results)
		{
			return Err(format!(
				"reported measurement {measurement_id} has stale file-result evidence"
			)
			.into());
		}
		let merge_report = read_bundle_json::<MergeReport>(&bundle, "metadata/merge-report.json")?;
		if merge_report.input.as_ref() != Some(&resolved.product_manifest.attestation()) {
			return Err(format!(
				"reported measurement {measurement_id} has stale merge-report input"
			)
			.into());
		}

		let scorer = read_bundle_json::<serde_json::Value>(&bundle, "metadata/scorer-config.json")?;
		let scorer_closure = stored_scorer_closure(&scorer)?;
		if scorer
			.get("scorer_config_hash")
			.and_then(serde_json::Value::as_str)
			!= Some(measurement.config_hash())
			|| scorer
				.get("scorer_version")
				.and_then(serde_json::Value::as_str)
				!= Some(measurement.scorer_version())
		{
			return Err(
				format!("reported measurement {measurement_id} has stale scorer evidence").into(),
			);
		}
		validate_scorer_evidence_index(
			&bundle,
			&measurement_file_results,
			&resolved.product_manifest,
			&scorer_closure.scoring_units,
		)?;
	}
	Ok(())
}

fn workshop_observation(
	input_version: &InputVersionRecord,
	resolved: &ResolvedWorkshopCase,
	observed_at: String,
) -> Result<WorkshopObservationRecordV2, Box<dyn std::error::Error>> {
	fn item(
		resolved: &crate::merge_quality::workshop_inputs::ResolvedWorkshopItem,
	) -> Result<WorkshopItemObservationV2, String> {
		Ok(WorkshopItemObservationV2 {
			workshop_id: resolved.install.identity.workshop_id.clone(),
			manifest_id: resolved.install.identity.manifest_id.clone(),
			time_updated: resolved.install.time_updated,
			size_bytes: resolved.install.size_bytes,
			ugc_handle: resolved
				.install
				.ugc_handle
				.as_deref()
				.map(str::parse::<SteamId>)
				.transpose()?,
		})
	}

	Ok(WorkshopObservationRecordV2::new(
		input_version.input_version_id.clone(),
		observed_at,
		item(&resolved.compatch)?,
		resolved
			.sources
			.iter()
			.map(item)
			.collect::<Result<Vec<_>, _>>()?,
	))
}

fn validate_cached_workshop_measurement(
	measurement: &MeasurementRecord,
	file_results: &[FileResultRecord],
	validation: &CachedWorkshopValidation<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
	let measurement_id = measurement.measurement_id();
	if measurement.input_version_id() != Some(validation.input_version.input_version_id.as_str()) {
		return Err(format!(
			"cached Workshop measurement {measurement_id} belongs to a different input version"
		)
		.into());
	}
	if !measurement.identity_is_valid() || !measurement.evidence_reference_is_valid() {
		return Err(format!(
			"cached Workshop measurement {measurement_id} has an invalid identity or evidence reference"
		)
		.into());
	}
	let measurement_file_results = file_results
		.iter()
		.filter(|record| record.measurement_id == measurement_id)
		.collect::<Vec<_>>();
	if measurement.status() != TerminalStatus::Completed {
		if measurement.summary().is_some() || !measurement_file_results.is_empty() {
			return Err(format!(
				"non-completed cached Workshop measurement {measurement_id} contains completed evidence"
			)
			.into());
		}
		return Ok(());
	}

	let evidence_hash = measurement.evidence_bundle_hash().ok_or_else(|| {
		format!("completed cached Workshop measurement {measurement_id} has no evidence bundle")
	})?;
	let bundle = validation.evidence_store.open(evidence_hash)?;
	if bundle.manifest.measurement_id != measurement_id
		|| bundle.manifest.input_version_id != validation.input_version.input_version_id
	{
		return Err(format!(
			"evidence bundle {evidence_hash} does not belong to Workshop measurement {measurement_id}"
		)
		.into());
	}
	let summary = measurement.summary().ok_or_else(|| {
		format!("completed cached Workshop measurement {measurement_id} has no score summary")
	})?;
	validate_cached_file_results(measurement_id, summary, &measurement_file_results)?;

	let expected_product_input =
		workshop_product_input_evidence(validation.input_version, validation.resolved);
	let stored_product_input =
		read_bundle_json::<WorkshopProductInputEvidence>(&bundle, "metadata/product-input.json")?;
	if stored_product_input != expected_product_input {
		return Err(format!(
			"cached Workshop measurement {measurement_id} product-input evidence is stale"
		)
		.into());
	}
	let stored_scorer =
		read_bundle_json::<serde_json::Value>(&bundle, "metadata/scorer-config.json")?;
	let scorer_closure = stored_scorer_closure(&stored_scorer)?;
	let expected_scorer = workshop_scorer_evidence(
		validation.runner_identity,
		validation.scorer_config_hash,
		validation.timeout,
		validation.base_snapshot_identity,
		&scorer_closure.base_scoring_closure_digest,
		&scorer_closure.scoring_units,
	);
	if stored_scorer != expected_scorer {
		return Err(format!(
			"cached Workshop measurement {measurement_id} scorer evidence is stale"
		)
		.into());
	}
	let stored_file_results =
		read_bundle_json::<Vec<FileResultRecord>>(&bundle, "metadata/file-results.json")?;
	let expected_file_results = measurement_file_results
		.iter()
		.map(|record| (*record).clone())
		.collect::<Vec<_>>();
	if canonical_file_results(stored_file_results) != canonical_file_results(expected_file_results)
	{
		return Err(format!(
			"cached Workshop measurement {measurement_id} file-result evidence is stale"
		)
		.into());
	}
	let report = read_bundle_json::<MergeReport>(&bundle, "metadata/merge-report.json")?;
	if report.input.as_ref() != Some(&validation.resolved.product_manifest.attestation()) {
		return Err(format!(
			"cached Workshop measurement {measurement_id} merge report input is stale"
		)
		.into());
	}
	validate_scorer_evidence_index(
		&bundle,
		&measurement_file_results,
		&validation.resolved.product_manifest,
		&scorer_closure.scoring_units,
	)?;
	Ok(())
}

fn read_bundle_json<T>(
	bundle: &crate::merge_quality::evidence_store::StoredEvidenceBundle,
	relative_path: &str,
) -> Result<T, Box<dyn std::error::Error>>
where
	T: serde::de::DeserializeOwned,
{
	serde_json::from_slice(&bundle.read_entry(relative_path)?)
		.map_err(|error| format!("invalid cached evidence {}: {error}", relative_path).into())
}

fn canonical_file_results(mut records: Vec<FileResultRecord>) -> Vec<FileResultRecord> {
	records.sort_by(|left, right| left.file_result_id.cmp(&right.file_result_id));
	records
}

fn validate_scorer_evidence_index(
	bundle: &crate::merge_quality::evidence_store::StoredEvidenceBundle,
	file_results: &[&FileResultRecord],
	source_manifest: &ProductInputManifest,
	expected_scoring_units: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
	if !source_manifest.digest_is_valid() {
		return Err("scorer evidence uses an invalid ACF source manifest".into());
	}
	let index = read_bundle_json::<ScorerEvidenceIndex>(bundle, "metadata/evidence-index.json")?;
	if index.schema != SCORER_EVIDENCE_INDEX_SCHEMA {
		return Err(format!("unsupported scorer evidence index schema {}", index.schema).into());
	}
	let expected_units = file_results
		.iter()
		.map(|record| record.relative_path.as_str())
		.collect::<BTreeSet<_>>();
	let actual_units = index
		.units
		.iter()
		.map(|unit| unit.relative_path.as_str())
		.collect::<BTreeSet<_>>();
	let current_units = expected_scoring_units
		.iter()
		.map(String::as_str)
		.collect::<BTreeSet<_>>();
	if current_units.len() != expected_scoring_units.len()
		|| actual_units.len() != index.units.len()
		|| actual_units != expected_units
		|| actual_units != current_units
	{
		return Err("scorer evidence index does not cover the exact scoring units".into());
	}

	let metadata = BTreeSet::from([
		ScorerEvidenceReference {
			kind: EvidenceEntryKind::ProductInputManifest,
			relative_path: "metadata/product-input.json".to_string(),
		},
		ScorerEvidenceReference {
			kind: EvidenceEntryKind::MergeReport,
			relative_path: "metadata/merge-report.json".to_string(),
		},
		ScorerEvidenceReference {
			kind: EvidenceEntryKind::ScorerConfig,
			relative_path: "metadata/scorer-config.json".to_string(),
		},
		ScorerEvidenceReference {
			kind: EvidenceEntryKind::FileResult,
			relative_path: "metadata/file-results.json".to_string(),
		},
		ScorerEvidenceReference {
			kind: EvidenceEntryKind::ScorerEvidenceIndex,
			relative_path: "metadata/evidence-index.json".to_string(),
		},
	]);
	let mut indexed_files = BTreeSet::new();
	let source_prefixes = source_manifest
		.mods
		.iter()
		.enumerate()
		.map(|(index, source)| format!("sources/{:02}-{}/", index + 1, source.mod_id))
		.collect::<Vec<_>>();
	for unit in &index.units {
		let canonical = unit.entries.iter().cloned().collect::<BTreeSet<_>>();
		if canonical.len() != unit.entries.len()
			|| canonical.iter().cloned().collect::<Vec<_>>() != unit.entries
		{
			return Err(format!(
				"scorer evidence for {} is not canonical",
				unit.relative_path
			)
			.into());
		}
		if !canonical.iter().any(|entry| {
			entry.kind == EvidenceEntryKind::CompatchInput
				&& entry.relative_path != "compatch/descriptor.mod"
		}) {
			return Err(format!(
				"scorer evidence for {} has no compatch scoring input",
				unit.relative_path
			)
			.into());
		}
		let file_result = file_results
			.iter()
			.find(|record| record.relative_path == unit.relative_path)
			.expect("validated scoring-unit coverage");
		let score = file_result
			.result
			.get("score")
			.cloned()
			.ok_or_else(|| "file result is missing score evidence".to_string())
			.and_then(|value| {
				serde_json::from_value::<FileRecord>(value)
					.map_err(|error| format!("invalid score evidence: {error}"))
			})?;
		let output_entries = canonical
			.iter()
			.filter(|entry| entry.kind == EvidenceEntryKind::MergedOutput)
			.collect::<Vec<_>>();
		for entry in &output_entries {
			let relative_path = entry
				.relative_path
				.strip_prefix("output/")
				.ok_or("merged-output evidence has an invalid prefix")?;
			if !scoring_evidence_path_belongs_to_unit(&unit.relative_path, relative_path) {
				return Err(format!(
					"scorer evidence for {} contains unrelated merged output {}",
					unit.relative_path, entry.relative_path
				)
				.into());
			}
		}
		let expected_output = format!("output/{}", unit.relative_path);
		let captured_output_exists = output_entries
			.iter()
			.any(|entry| entry.relative_path == expected_output);
		if captured_output_exists != score.foch_emitted {
			return Err(format!(
				"scorer evidence for {} disagrees with the recorded output presence",
				unit.relative_path
			)
			.into());
		}
		for entry in canonical {
			let valid_prefix = match entry.kind {
				EvidenceEntryKind::CompatchInput => entry.relative_path.starts_with("compatch/"),
				EvidenceEntryKind::SourceInput => source_prefixes
					.iter()
					.any(|prefix| entry.relative_path.starts_with(prefix)),
				EvidenceEntryKind::MergedOutput => entry.relative_path.starts_with("output/"),
				EvidenceEntryKind::BaseInput => entry.relative_path.starts_with("base/"),
				_ => false,
			};
			if !valid_prefix {
				return Err(format!(
					"scorer evidence for {} contains an invalid reference",
					unit.relative_path
				)
				.into());
			}
			indexed_files.insert(entry);
		}
	}

	let bundle_files = bundle
		.manifest
		.entries
		.iter()
		.map(|entry| ScorerEvidenceReference {
			kind: entry.kind,
			relative_path: entry.relative_path.clone(),
		})
		.collect::<BTreeSet<_>>();
	let expected_bundle_files = metadata
		.into_iter()
		.chain(indexed_files)
		.collect::<BTreeSet<_>>();
	if bundle_files.len() != bundle.manifest.entries.len() || bundle_files != expected_bundle_files
	{
		return Err("evidence bundle does not match its scorer-read closure".into());
	}
	Ok(())
}

fn workshop_evidence_entries(
	request: &WorkshopEvidenceRequest<'_>,
) -> Result<Vec<EvidenceEntryInput>, Box<dyn std::error::Error>> {
	let mut entries = vec![
		EvidenceEntryInput::bytes(
			EvidenceEntryKind::ProductInputManifest,
			"metadata/product-input.json",
			serde_json::to_vec_pretty(&workshop_product_input_evidence(
				request.input_version,
				request.resolved,
			))?,
		),
		EvidenceEntryInput::bytes(
			EvidenceEntryKind::MergeReport,
			"metadata/merge-report.json",
			serde_json::to_vec_pretty(request.report)?,
		),
		EvidenceEntryInput::bytes(
			EvidenceEntryKind::ScorerConfig,
			"metadata/scorer-config.json",
			serde_json::to_vec_pretty(&workshop_scorer_evidence(
				request.runner_identity,
				request.scorer_config_hash,
				request.measurement.timeout,
				&request.measurement.expected_base_snapshot_identity,
				&request.captured.scorer_closure.base_scoring_closure_digest,
				&request.captured.scorer_closure.scoring_units,
			))?,
		),
		EvidenceEntryInput::bytes(
			EvidenceEntryKind::FileResult,
			"metadata/file-results.json",
			serde_json::to_vec_pretty(request.file_results)?,
		),
	];
	let mut destinations = entries
		.iter()
		.map(|entry| entry.relative_path.clone())
		.collect::<BTreeSet<_>>();
	let mut units = Vec::with_capacity(request.file_results.len());

	for file_result in request.file_results {
		let mut unit_entries = BTreeSet::new();
		let compatch_files =
			scoring_evidence_files(&request.captured.compatch_dir, &file_result.relative_path)?;
		if !compatch_files.iter().any(|path| path != "descriptor.mod") {
			return Err(format!(
				"scoring unit {} has no regular compatch evidence",
				file_result.relative_path
			)
			.into());
		}
		push_evidence_paths(
			&mut entries,
			&mut destinations,
			&mut unit_entries,
			EvidenceEntryKind::CompatchInput,
			"compatch",
			&request.captured.compatch_dir,
			compatch_files,
		)?;
		for (index, (source, captured_source)) in request
			.resolved
			.sources
			.iter()
			.zip(&request.captured.source_dirs)
			.enumerate()
		{
			let prefix = format!(
				"sources/{:02}-{}",
				index + 1,
				source.install.identity.workshop_id
			);
			push_evidence_paths(
				&mut entries,
				&mut destinations,
				&mut unit_entries,
				EvidenceEntryKind::SourceInput,
				&prefix,
				captured_source,
				scoring_evidence_files(captured_source, &file_result.relative_path)?,
			)?;
		}
		push_evidence_paths(
			&mut entries,
			&mut destinations,
			&mut unit_entries,
			EvidenceEntryKind::MergedOutput,
			"output",
			&request.captured.output_dir,
			scoring_evidence_files(&request.captured.output_dir, &file_result.relative_path)?,
		)?;
		push_evidence_paths(
			&mut entries,
			&mut destinations,
			&mut unit_entries,
			EvidenceEntryKind::BaseInput,
			"base",
			&request.captured.basegame_root,
			scoring_evidence_files(&request.captured.basegame_root, &file_result.relative_path)?,
		)?;
		units.push(ScorerEvidenceUnit {
			relative_path: file_result.relative_path.clone(),
			entries: unit_entries.into_iter().collect(),
		});
	}
	units.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
	entries.push(EvidenceEntryInput::bytes(
		EvidenceEntryKind::ScorerEvidenceIndex,
		"metadata/evidence-index.json",
		serde_json::to_vec_pretty(&ScorerEvidenceIndex {
			schema: SCORER_EVIDENCE_INDEX_SCHEMA.to_string(),
			units,
		})?,
	));
	Ok(entries)
}

fn push_evidence_paths(
	entries: &mut Vec<EvidenceEntryInput>,
	destinations: &mut BTreeSet<String>,
	unit_entries: &mut BTreeSet<ScorerEvidenceReference>,
	kind: EvidenceEntryKind,
	prefix: &str,
	root: &Path,
	relative_paths: Vec<String>,
) -> io::Result<()> {
	for relative_path in relative_paths {
		let destination = format!("{prefix}/{relative_path}");
		unit_entries.insert(ScorerEvidenceReference {
			kind,
			relative_path: destination.clone(),
		});
		if destinations.insert(destination.clone()) {
			entries.push(EvidenceEntryInput::source_file(
				kind,
				destination,
				root.join(relative_path),
			));
		}
	}
	Ok(())
}

fn validate_runner_identity(
	identity: &MeasurementRunnerIdentity,
) -> Result<(), Box<dyn std::error::Error>> {
	let hash = &identity.engine_artifact.hash;
	if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
		return Err("measurement runner artifact must have a 64-character BLAKE3 hash".into());
	}
	if identity.runner_protocol_version.trim().is_empty() {
		return Err("measurement runner protocol version must not be empty".into());
	}
	if identity.backend != MergeBackendId::GumtreePcsNway {
		return Err("new product measurements require the gumtree-pcs-nway backend".into());
	}
	Ok(())
}
pub fn executable_hash(path: &Path) -> io::Result<String> {
	digest_file(path)
}
fn index_measurements(
	measurements: &[MeasurementRecord],
) -> Result<HashMap<&str, &MeasurementRecord>, Box<dyn std::error::Error>> {
	let mut by_id = HashMap::with_capacity(measurements.len());
	for measurement in measurements {
		if !measurement.identity_is_valid() {
			return Err(format!(
				"measurement {} has an invalid identity",
				measurement.measurement_id()
			)
			.into());
		}
		if by_id
			.insert(measurement.measurement_id(), measurement)
			.is_some()
		{
			return Err(format!(
				"measurement {} occurs more than once",
				measurement.measurement_id()
			)
			.into());
		}
	}
	Ok(by_id)
}

fn validate_file_result_foreign_keys(
	file_results: &[FileResultRecord],
	measurement_ids: &HashSet<String>,
	v2_measurement_ids: &HashSet<String>,
	recoverable_measurement_ids: &HashSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
	let mut result_ids = HashSet::with_capacity(file_results.len());
	for file_result in file_results {
		if !measurement_ids.contains(&file_result.measurement_id)
			&& !recoverable_measurement_ids.contains(&file_result.measurement_id)
		{
			return Err(format!(
				"file result {} references a missing or non-recoverable measurement {}",
				file_result.file_result_id, file_result.measurement_id
			)
			.into());
		}
		let expected = if v2_measurement_ids.contains(&file_result.measurement_id) {
			FileResultRecord::new_v2(
				file_result.measurement_id.clone(),
				file_result.relative_path.clone(),
				file_result.result.clone(),
			)
		} else {
			FileResultRecord::new_v1(
				file_result.measurement_id.clone(),
				file_result.relative_path.clone(),
				file_result.result.clone(),
			)
		};
		if expected != *file_result {
			return Err(format!(
				"file result {} has an invalid identity",
				file_result.file_result_id
			)
			.into());
		}
		if !result_ids.insert(file_result.file_result_id.as_str()) {
			return Err(format!(
				"file result {} occurs more than once",
				file_result.file_result_id
			)
			.into());
		}
	}
	Ok(())
}
fn validate_replayed_file_results(
	measurement_id: &str,
	summary: &MeasurementSummary,
	persisted_file_results: &[FileResultRecord],
	replayed_file_results: &[FileResultRecord],
) -> Result<(), Box<dyn std::error::Error>> {
	let persisted = persisted_file_results
		.iter()
		.filter(|record| record.measurement_id == measurement_id)
		.collect::<Vec<_>>();
	let persisted_by_path = persisted
		.iter()
		.map(|record| (record.relative_path.as_str(), *record))
		.collect::<HashMap<_, _>>();
	let mut replayed_ids = HashSet::with_capacity(replayed_file_results.len());
	let mut combined = persisted;
	for replayed in replayed_file_results {
		if replayed.measurement_id != measurement_id {
			return Err(format!(
				"replayed file result {} belongs to the wrong measurement",
				replayed.file_result_id
			)
			.into());
		}
		let expected = FileResultRecord::new(
			replayed.measurement_id.clone(),
			replayed.relative_path.clone(),
			replayed.result.clone(),
		);
		if expected != *replayed {
			return Err(format!(
				"replayed file result {} has an invalid identity",
				replayed.file_result_id
			)
			.into());
		}
		if !replayed_ids.insert(replayed.file_result_id.as_str()) {
			return Err(format!(
				"replay produced duplicate file result {}",
				replayed.file_result_id
			)
			.into());
		}
		if let Some(persisted) = persisted_by_path.get(replayed.relative_path.as_str()) {
			if *persisted != replayed {
				return Err(format!(
					"recoverable file result {} differs from replay",
					replayed.file_result_id
				)
				.into());
			}
		} else {
			combined.push(replayed);
		}
	}
	validate_cached_file_results(measurement_id, summary, &combined)
}

fn validate_cached_file_results(
	measurement_id: &str,
	summary: &MeasurementSummary,
	file_results: &[&FileResultRecord],
) -> Result<(), Box<dyn std::error::Error>> {
	if file_results.len() != summary.ground_truth_files {
		return Err(format!(
			"completed cached measurement {measurement_id} declares {} scored files but stores {} file results",
			summary.ground_truth_files,
			file_results.len()
		)
		.into());
	}
	let merge_status = summary.merge_status.as_deref().ok_or_else(|| {
		format!("completed cached measurement {measurement_id} has no merge status")
	})?;
	serde_json::from_value::<MergeReportStatus>(serde_json::Value::String(
		merge_status.to_string(),
	))
	.map_err(|error| {
		format!("completed cached measurement {measurement_id} has invalid merge status: {error}")
	})?;
	if summary.total_ms
		!= summary
			.setup_ms
			.saturating_add(summary.merge_ms)
			.saturating_add(summary.scoring_ms)
	{
		return Err(format!(
			"completed cached measurement {measurement_id} has inconsistent timing totals"
		)
		.into());
	}

	let mut relative_paths = HashSet::with_capacity(file_results.len());
	let mut all_verdicts = BTreeMap::new();
	let mut multi_verdicts = BTreeMap::new();
	let mut multi_source_files = 0_usize;
	let mut accepted_files = 0_usize;
	let mut accepted_multi_source_files = 0_usize;
	for file_result in file_results {
		if !relative_paths.insert(file_result.relative_path.as_str()) {
			return Err(format!(
				"completed cached measurement {measurement_id} has duplicate file result {}",
				file_result.relative_path
			)
			.into());
		}
		if file_result.result.get("human_resolution").is_none() {
			return Err(format!(
				"file result {} is missing human_resolution evidence",
				file_result.file_result_id
			)
			.into());
		}
		let score = file_result
			.result
			.get("score")
			.ok_or_else(|| {
				format!(
					"file result {} is missing score evidence",
					file_result.file_result_id
				)
			})
			.and_then(|value| {
				serde_json::from_value::<FileRecord>(value.clone()).map_err(|error| {
					format!(
						"file result {} has invalid score evidence: {error}",
						file_result.file_result_id
					)
				})
			})?;
		if score.rel != file_result.relative_path {
			return Err(format!(
				"file result {} path does not match its score payload",
				file_result.file_result_id
			)
			.into());
		}
		*all_verdicts.entry(score.verdict.clone()).or_default() += 1;
		if score.accepted_ok {
			accepted_files += 1;
		}
		if score.multi_source {
			multi_source_files += 1;
			*multi_verdicts.entry(score.verdict.clone()).or_default() += 1;
			if score.accepted_ok {
				accepted_multi_source_files += 1;
			}
		}
	}
	if multi_source_files != summary.multi_source_files
		|| accepted_files != summary.accepted_ground_truth_files
		|| accepted_multi_source_files != summary.accepted_multi_source_files
		|| all_verdicts != summary.all_ground_truth_verdicts
		|| multi_verdicts != summary.multi_source_verdicts
	{
		return Err(format!(
			"completed cached measurement {measurement_id} summary does not match file results"
		)
		.into());
	}
	Ok(())
}

fn capture_scoring_inputs(
	request: &MeasurementRequest,
) -> Result<CapturedScoringInputs, Box<dyn std::error::Error>> {
	let scoring_units = scoring_reference_units(&reference_output_files(&request.compatch_dir)?);
	if scoring_units.is_empty() {
		return Err(format!(
			"Workshop case {} contains no scorer reference units",
			request.case.compatch_id
		)
		.into());
	}
	let work_parent = request
		.output_dir
		.parent()
		.ok_or("measurement output has no work parent")?;
	let root = tempfile::Builder::new()
		.prefix("scorer-inputs-")
		.tempdir_in(work_parent)?;
	let compatch_dir = root.path().join("compatch");
	let output_dir = root.path().join("output");
	let basegame_root = root.path().join("base");
	fs::create_dir(&compatch_dir)?;
	fs::create_dir(&output_dir)?;
	fs::create_dir(&basegame_root)?;
	capture_scoring_layer(&request.compatch_dir, &compatch_dir, &scoring_units)?;
	capture_scoring_layer(&request.output_dir, &output_dir, &scoring_units)?;
	capture_scoring_layer(&request.basegame_root, &basegame_root, &scoring_units)?;
	let mut source_dirs = Vec::with_capacity(request.source_dirs.len());
	for (index, source) in request.source_dirs.iter().enumerate() {
		let destination = root.path().join(format!("source-{:02}", index + 1));
		fs::create_dir(&destination)?;
		capture_scoring_layer(source, &destination, &scoring_units)?;
		source_dirs.push(destination);
	}
	let captured_units = scoring_reference_units(&reference_output_files(&compatch_dir)?);
	if captured_units != scoring_units {
		return Err("captured compatch does not preserve the exact scoring units".into());
	}
	let base_scoring_closure_digest = scoring_closure_digest(&basegame_root, &scoring_units)?;
	Ok(CapturedScoringInputs {
		_root: root,
		compatch_dir,
		source_dirs,
		output_dir,
		basegame_root,
		scorer_closure: ScorerCaseClosure {
			scoring_units,
			base_scoring_closure_digest,
		},
	})
}

fn scoring_closure_digest(
	root: &Path,
	scoring_units: &[String],
) -> Result<String, Box<dyn std::error::Error>> {
	let before = scoring_units
		.iter()
		.map(|unit| scoring_evidence_files(root, unit))
		.collect::<Result<Vec<_>, _>>()?
		.into_iter()
		.flatten()
		.collect::<BTreeSet<_>>();
	let mut hasher = blake3::Hasher::new();
	update_closure_field(&mut hasher, b"foch-scorer-closure-v1");
	hasher.update(&(before.len() as u64).to_le_bytes());
	for relative_path in &before {
		let content = read_stable_source_file(root, Path::new(relative_path))?;
		update_closure_field(&mut hasher, relative_path.as_bytes());
		update_closure_field(&mut hasher, &content);
	}
	let after = scoring_units
		.iter()
		.map(|unit| scoring_evidence_files(root, unit))
		.collect::<Result<Vec<_>, _>>()?
		.into_iter()
		.flatten()
		.collect::<BTreeSet<_>>();
	if after != before {
		return Err(format!("scorer closure changed while reading {}", root.display()).into());
	}
	Ok(hasher.finalize().to_hex().to_string())
}

fn update_closure_field(hasher: &mut blake3::Hasher, field: &[u8]) {
	hasher.update(&(field.len() as u64).to_le_bytes());
	hasher.update(field);
}

fn capture_scoring_layer(
	source_root: &Path,
	destination_root: &Path,
	scoring_units: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
	let before = scoring_units
		.iter()
		.map(|unit| scoring_evidence_files(source_root, unit))
		.collect::<Result<Vec<_>, _>>()?
		.into_iter()
		.flatten()
		.collect::<BTreeSet<_>>();
	for relative_path in &before {
		let content = read_stable_source_file(source_root, Path::new(relative_path))?;
		let destination = destination_root.join(relative_path);
		fs::create_dir_all(destination.parent().expect("captured file has parent"))?;
		fs::write(destination, content)?;
	}
	let after = scoring_units
		.iter()
		.map(|unit| scoring_evidence_files(source_root, unit))
		.collect::<Result<Vec<_>, _>>()?
		.into_iter()
		.flatten()
		.collect::<BTreeSet<_>>();
	if after != before {
		return Err(format!(
			"scorer input file set changed while capturing {}",
			source_root.display()
		)
		.into());
	}
	Ok(())
}

fn classify_terminal_merge(
	terminal: TerminalMerge,
	request: &MeasurementRequest,
	score_cache: &mut ScoreCache,
) -> Result<TerminalClassification, Box<dyn std::error::Error>> {
	match terminal {
		TerminalMerge::Completed { report, merge_ms } => {
			if report.status == MergeReportStatus::Fatal {
				let detail = report
					.fatal_reason
					.clone()
					.unwrap_or_else(|| "merge report status is fatal".to_string());
				return Ok((TerminalStatus::Fatal, Some(detail), None));
			}
			let captured = capture_scoring_inputs(request)?;
			let completed = score_existing_output_with_cache(
				&ScoreExistingOutputRequest {
					case: &request.case,
					compatch_dir: &captured.compatch_dir,
					source_dirs: &captured.source_dirs,
					output_dir: &captured.output_dir,
					report: &report,
					basegame_root: Some(&captured.basegame_root),
					merge_ms,
				},
				score_cache,
			)
			.map_err(|error| format!("failed to score completed merge: {error}"))?;
			Ok((
				TerminalStatus::Completed,
				None,
				Some(CompletedMeasurement {
					score: completed,
					inputs: captured,
				}),
			))
		}
		TerminalMerge::MergeFailed { detail } => {
			Ok((TerminalStatus::MergeFailed, Some(detail), None))
		}
		TerminalMerge::Crashed { detail } => Ok((TerminalStatus::Crashed, detail, None)),
		TerminalMerge::TimedOut { detail } => Ok((TerminalStatus::TimedOut, detail, None)),
		TerminalMerge::Fatal { detail } => Ok((TerminalStatus::Fatal, Some(detail), None)),
	}
}

fn redact_persisted_detail(detail: &str, request: &MeasurementRequest) -> String {
	let mut roots = vec![
		(&request.output_dir, "<output-root>"),
		(&request.compatch_dir, "<compatch-root>"),
		(&request.basegame_root, "<basegame-root>"),
	];
	roots.extend(
		request
			.source_dirs
			.iter()
			.map(|path| (path, "<source-root>")),
	);
	roots.sort_by_key(|(path, _)| std::cmp::Reverse(path.as_os_str().len()));
	let mut redacted = detail.to_string();
	for (path, replacement) in roots {
		let displayed = path.to_string_lossy();
		if !displayed.is_empty() {
			redacted = redacted.replace(displayed.as_ref(), replacement);
			let normalized = displayed.replace('\\', "/");
			if normalized != displayed {
				redacted = redacted.replace(&normalized, replacement);
			}
		}
	}
	redact_remaining_absolute_paths(&redacted)
}

fn redact_remaining_absolute_paths(detail: &str) -> String {
	let mut output = String::with_capacity(detail.len());
	let mut cursor = 0_usize;
	while let Some(relative_start) = detail[cursor..].find('/') {
		let start = cursor + relative_start;
		let prefix = &detail[..start];
		let previous = prefix.chars().next_back();
		let starts_absolute = previous.is_none_or(|character| {
			character.is_whitespace()
				|| matches!(
					character,
					'=' | ':' | '\'' | '"' | '`' | '(' | '[' | '{' | '<'
				)
		});
		if !starts_absolute {
			output.push_str(&detail[cursor..=start]);
			cursor = start + 1;
			continue;
		}
		output.push_str(&detail[cursor..start]);
		output.push_str("<absolute-path>");
		let end = detail[start..]
			.char_indices()
			.find_map(|(offset, character)| {
				(offset > 0
					&& (character.is_whitespace()
						|| matches!(
							character,
							'\'' | '"' | '`' | ')' | ']' | '}' | '>' | ',' | ';'
						)))
				.then_some(start + offset)
			})
			.unwrap_or(detail.len());
		cursor = end;
	}
	output.push_str(&detail[cursor..]);
	output
}
fn measurement_summary(result: &CaseResult) -> MeasurementSummary {
	MeasurementSummary {
		merge_status: result.merge_status.clone(),
		ground_truth_files: result.ground_truth_files,
		multi_source_files: result.multi_source_files,
		accepted_ground_truth_files: result.accepted_ground_truth_files,
		accepted_multi_source_files: result.accepted_multi_source_files,
		all_ground_truth_verdicts: result.all_ground_truth_verdicts.clone(),
		multi_source_verdicts: result.multi_source_verdicts.clone(),
		setup_ms: result.timings.setup_ms,
		merge_ms: result.timings.merge_ms,
		scoring_ms: result.timings.scoring_ms,
		total_ms: result.timings.total_ms,
	}
}

fn progress(position: usize, total: usize, started: Instant) -> String {
	let elapsed = started.elapsed().as_secs_f64();
	if position <= 1 || total <= 1 {
		return format!("elapsed={elapsed:.1}s eta=unknown");
	}
	let completed = position - 1;
	let remaining = total.saturating_sub(completed);
	let eta = elapsed / completed as f64 * remaining as f64;
	format!("elapsed={elapsed:.1}s eta={eta:.1}s")
}

fn digest_file(path: &Path) -> io::Result<String> {
	let mut hasher = blake3::Hasher::new();
	let mut file = File::open(path)?;
	let mut buffer = vec![0_u8; 1024 * 1024];
	loop {
		let read = file.read(&mut buffer)?;
		if read == 0 {
			break;
		}
		hasher.update(&buffer[..read]);
	}
	Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn absolute_path_redaction_handles_markup_delimiters() {
		assert_eq!(
			redact_remaining_absolute_paths("failed at `/private/sentinel` and </Users/me/file>"),
			"failed at `<absolute-path>` and <<absolute-path>>"
		);
	}

	#[test]
	fn scorer_closure_binds_live_bytes_but_config_identity_is_policy_only() {
		let base = tempfile::tempdir().unwrap();
		fs::create_dir_all(base.path().join("interface")).unwrap();
		fs::write(base.path().join("interface/reference.gui"), "one\n").unwrap();
		let units = vec!["interface/reference.gui".to_string()];
		let first_digest = scoring_closure_digest(base.path(), &units).unwrap();
		fs::write(base.path().join("interface/reference.gui"), "two\n").unwrap();
		let second_digest = scoring_closure_digest(base.path(), &units).unwrap();
		assert_ne!(first_digest, second_digest);

		let first =
			workshop_scorer_config_hash(Duration::from_secs(1), "1.37.5", 4242, "base-snapshot");
		let changed_timeout =
			workshop_scorer_config_hash(Duration::from_secs(2), "1.37.5", 4242, "base-snapshot");
		let changed_base =
			workshop_scorer_config_hash(Duration::from_secs(1), "1.37.5", 4242, "other-base");
		assert_ne!(first, changed_timeout);
		assert_ne!(first, changed_base);
	}

	#[cfg(unix)]
	#[test]
	fn scorer_closure_rejects_intermediate_symlinks() {
		use std::os::unix::fs::symlink;

		let root = tempfile::tempdir().unwrap();
		let outside = tempfile::tempdir().unwrap();
		fs::write(outside.path().join("reference.gui"), "outside\n").unwrap();
		symlink(outside.path(), root.path().join("interface")).unwrap();

		let error = scoring_closure_digest(root.path(), &["interface/reference.gui".to_string()])
			.expect_err("intermediate symlink must fail closed");
		assert!(
			error.to_string().contains("not a directory") || error.to_string().contains("symlink")
		);
	}
}
