use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use foch_core::model::{MergeReport, MergeReportStatus};
use serde::Serialize;
use walkdir::WalkDir;

use crate::config::{Eu4Discovery, WorkshopCatalog};
use crate::corpus::{
	Case, Corpus, ORACLE_POLICY_VERSION, OracleAssessment, OracleStatus, assess_oracle_candidate,
};
use crate::dataset::{
	DatasetPaths, EngineArtifactIdentity, FileResultRecord, GameIdentity, MeasurementCohortKey,
	MeasurementIdentityV2, MeasurementKernel, MeasurementRecord, MeasurementScope,
	MeasurementSummary, ObjectKind, ObjectRecord, ObservationRecord, SCHEMA, SCORER_VERSION,
	SnapshotObjectRef, SnapshotRecord, TerminalStatus, WorkshopObservation, append_unique,
	append_unique_many, now_rfc3339, read_jsonl, stable_id,
};
use crate::object_store::{ExportProfile, ObjectStore, StoredObject};
use crate::orchestrate::{CaseResult, FileRecord, score_existing_output_with_cache};
use crate::report::{
	MEASUREMENT_REPORT_SCHEMA, MeasurementCohortSelector, committed_measurement_cohort_registry,
	select_measurement_cohort,
};
use crate::score::{Resolution, ScoreCache, SourceMod, classify_resolution};

#[derive(Clone, Debug)]
pub struct CollectOptions<'a> {
	pub corpus: &'a Path,
	pub dataset_root: &'a Path,
	pub discovery: &'a Eu4Discovery,
	pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectSummary {
	pub local_cases: usize,
	pub snapshots: usize,
	pub unique_objects: usize,
	pub logical_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct MeasureOptions<'a> {
	pub dataset_root: &'a Path,
	pub timeout: Duration,
	pub limit: usize,
	pub basegame_root: &'a Path,
	/// Exact immutable snapshots to measure, in caller-supplied order.
	///
	/// Exact selection and `limit` are mutually exclusive: an exact selector is
	/// itself the complete denominator and must not be silently truncated.
	pub snapshot_ids: Option<&'a [String]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasureRunSummary {
	pub selected: usize,
	pub cached: usize,
	pub measured: usize,
	pub failed: usize,
	pub product_hash: String,
	pub scorer_config_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasurementRunnerIdentity {
	pub engine_artifact: EngineArtifactIdentity,
	pub worker_protocol_version: String,
	pub merge_kernel: MeasurementKernel,
	pub scope: MeasurementScope,
}

#[derive(Clone, Debug)]
pub struct MeasurementRequest {
	pub snapshot_id: String,
	pub case: Case,
	pub compatch_dir: PathBuf,
	pub source_dirs: Vec<PathBuf>,
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

#[derive(Clone, Debug)]
pub struct ReportOptions<'a> {
	pub dataset_root: &'a Path,
	pub output_dir: &'a Path,
	pub cohort_id: Option<&'a str>,
	pub scorer_version: Option<&'a str>,
	pub cohort: ReportCohort,
	pub limit: usize,
	/// Exact immutable snapshots to report, in caller-supplied order.
	pub snapshot_ids: Option<&'a [String]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportCohort {
	Scorable,
	AllCandidates,
}

impl ReportCohort {
	fn includes(self, assessment: &OracleAssessment) -> bool {
		match self {
			Self::Scorable => assessment.is_scorable(),
			Self::AllCandidates => true,
		}
	}

	fn as_str(self) -> &'static str {
		match self {
			Self::Scorable => "scorable",
			Self::AllCandidates => "all_candidates",
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatasetExportProfile {
	Metadata,
	Semantic,
	Full,
}

#[derive(Clone, Debug)]
pub struct ExportOptions<'a> {
	pub dataset_root: &'a Path,
	pub output_dir: &'a Path,
	pub profile: DatasetExportProfile,
}

struct CompletedMeasurement {
	result: CaseResult,
	resolutions: BTreeMap<String, Resolution>,
}

type TerminalClassification = (TerminalStatus, Option<String>, Option<CompletedMeasurement>);

struct PreparedMeasurement {
	identity: MeasurementIdentityV2,
	case_id: String,
	request: MeasurementRequest,
	_work: tempfile::TempDir,
}

#[derive(Clone, Copy, Serialize)]
struct ProductBaseIdentity<'a> {
	game: &'static str,
	game_version: &'a str,
	steam_build_id: Option<u64>,
	installed_snapshot_sha256: &'a str,
	pinned_product_base_blake3: &'a str,
}

#[derive(Clone, Debug, Serialize)]
struct BaselineReport {
	schema: String,
	generated_at: String,
	measurement_cohort_id: String,
	measurement_cohort_label: String,
	measurement_identity: MeasurementCohortKey,
	merge_kernel: MeasurementKernel,
	scorer_version: String,
	oracle_policy_version: String,
	cohort: String,
	candidate_cases: usize,
	scorable_cases: usize,
	excluded_cases: usize,
	baseline_complete: bool,
	total_cases: usize,
	terminal_cases: usize,
	completed_cases: usize,
	merge_failed_cases: usize,
	status_counts: BTreeMap<String, usize>,
	reference_output: QualityAggregate,
	multi_source: QualityAggregate,
	cases: Vec<BaselineCase>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct QualityAggregate {
	accepted: usize,
	total: usize,
	verdicts: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Serialize)]
struct BaselineCase {
	case_id: String,
	snapshot_id: String,
	title: String,
	oracle: OracleAssessment,
	status: String,
	measurement_id: Option<String>,
	detail: Option<String>,
	summary: Option<MeasurementSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct ExportManifest {
	schema: String,
	profile: String,
	objects: Vec<ExportManifestObject>,
}

#[derive(Clone, Debug, Serialize)]
struct ExportManifestObject {
	content_hash: String,
	archive: String,
	archive_hash: String,
	archive_bytes: u64,
}

pub fn collect(options: &CollectOptions<'_>) -> Result<CollectSummary, Box<dyn std::error::Error>> {
	let paths = DatasetPaths::new(options.dataset_root);
	paths.ensure_layout()?;
	let corpus = Corpus::from_json(&fs::read_to_string(options.corpus)?)?;
	let local: Vec<(&Case, PathBuf, Vec<PathBuf>)> = corpus
		.cases
		.iter()
		.filter(|case| case.referenced_mods.len() >= 2)
		.filter_map(|case| resolve_case_paths(case, &options.discovery.workshop))
		.take(if options.limit == 0 {
			usize::MAX
		} else {
			options.limit
		})
		.collect();

	let store = ObjectStore::new(&paths.objects, &paths.work);
	let mut cache: HashMap<PathBuf, StoredObject> = HashMap::new();
	let total_input_paths = local
		.iter()
		.flat_map(|(_, compatch, sources)| std::iter::once(compatch).chain(sources.iter()))
		.collect::<HashSet<_>>()
		.len();
	let mut input_position = 0_usize;
	let observed_at = now_rfc3339();
	let started = Instant::now();
	for (index, (case, compatch_path, source_paths)) in local.iter().enumerate() {
		eprintln!(
			"[collect] case {}/{} {} ({})",
			index + 1,
			local.len(),
			case.compatch_id,
			progress(index + 1, local.len(), started)
		);
		if !cache.contains_key(compatch_path) {
			input_position += 1;
			eprintln!(
				"[collect] object {}/{} compatch:{} ({})",
				input_position,
				total_input_paths,
				case.compatch_id,
				progress(input_position, total_input_paths, started)
			);
		}
		let compatch = snapshot_cached(&store, &mut cache, compatch_path)?;
		append_unique(
			&paths.object_records,
			&ObjectRecord::new(
				ObjectKind::Compatch,
				compatch.hash.clone(),
				Some(case.compatch_id.clone()),
				compatch.stats.clone(),
			),
		)?;

		let mut source_refs = Vec::with_capacity(source_paths.len());
		for (source_id, source_path) in case.referenced_mods.iter().zip(source_paths) {
			if !cache.contains_key(source_path) {
				input_position += 1;
				eprintln!(
					"[collect] object {}/{} source:{} ({})",
					input_position,
					total_input_paths,
					source_id,
					progress(input_position, total_input_paths, started)
				);
			}
			let source = snapshot_cached(&store, &mut cache, source_path)?;
			append_unique(
				&paths.object_records,
				&ObjectRecord::new(
					ObjectKind::SourceMod,
					source.hash.clone(),
					Some(source_id.clone()),
					source.stats.clone(),
				),
			)?;
			source_refs.push(SnapshotObjectRef {
				workshop_id: source_id.clone(),
				content_hash: source.hash,
			});
		}

		let snapshot = SnapshotRecord::new(
			case.compatch_id.clone(),
			GameIdentity {
				app_id: crate::config::EU4_APPID,
				version: options.discovery.game_version.clone(),
				steam_build_id: options.discovery.steam_build_id,
			},
			SnapshotObjectRef {
				workshop_id: case.compatch_id.clone(),
				content_hash: compatch.hash,
			},
			source_refs,
		);
		append_unique(&paths.snapshots, &snapshot)?;
		append_unique(
			&paths.observations,
			&observation_for_case(case, &snapshot.snapshot_id, &observed_at),
		)?;
	}

	let unique_objects: HashMap<&str, u64> = cache
		.values()
		.map(|object| (object.hash.as_str(), object.stats.bytes))
		.collect();
	let logical_bytes = unique_objects.values().sum();
	Ok(CollectSummary {
		local_cases: local.len(),
		snapshots: local.len(),
		unique_objects: unique_objects.len(),
		logical_bytes,
	})
}

pub fn measure_with_runner(
	options: &MeasureOptions<'_>,
	runner: &mut dyn MeasurementRunner,
) -> Result<MeasureRunSummary, Box<dyn std::error::Error>> {
	let paths = DatasetPaths::new(options.dataset_root);
	paths.ensure_layout()?;
	runner.preflight().map_err(|error| {
		format!(
			"measurement runner preflight failed: {}",
			redact_remaining_absolute_paths(&error)
		)
	})?;
	let observations = read_jsonl::<ObservationRecord>(&paths.observations)?;
	let snapshots = select_snapshots(
		read_jsonl::<SnapshotRecord>(&paths.snapshots)?,
		&observations,
		options.snapshot_ids,
		options.limit,
	)?;
	let runner_identity = runner.identity().clone();
	validate_runner_identity(&runner_identity)?;
	let local_game = crate::config::discover_eu4_game(&crate::config::DiscoveryOverrides {
		game_root: Some(options.basegame_root.to_path_buf()),
		..crate::config::DiscoveryOverrides::default()
	})
	.map_err(|error| {
		format!(
			"failed to identify the measurement base game: {}",
			redact_remaining_absolute_paths(&error)
		)
	})?;
	for snapshot in &snapshots {
		validate_measurement_game(snapshot, &local_game)?;
	}
	let basegame_version = local_game.game_version;
	let basegame_snapshot =
		foch_engine::installed_base_snapshot_identity("eu4", &basegame_version)?
			.ok_or_else(|| format!("no installed base snapshot for eu4@{basegame_version}"))?;
	let basegame_snapshot_label = basegame_snapshot.as_label();
	let loaded_base = foch_engine::load_installed_base_snapshot(
		"eu4",
		&basegame_version,
		Some(&basegame_snapshot),
	)?
	.ok_or_else(|| format!("no installed base snapshot for eu4@{basegame_version}"))?;
	let pinned_base_paths =
		product_base_inventory_paths(options.basegame_root, &loaded_base.snapshot.inventory_paths)?;
	let (pinned_base_view, pinned_product_base_blake3) =
		materialize_base_inventory_view(options.basegame_root, &pinned_base_paths, &paths.work)?;
	let pinned_version = crate::config::detect_game_version(pinned_base_view.path())
		.ok_or("pinned product base does not contain detectable version metadata")?;
	if pinned_version != basegame_version {
		return Err(format!(
			"pinned product base version mismatch: expected {basegame_version}, found {pinned_version}"
		)
		.into());
	}
	let basegame_identity = ProductBaseIdentity {
		game: "eu4",
		game_version: &basegame_version,
		steam_build_id: local_game.steam_build_id,
		installed_snapshot_sha256: basegame_snapshot.sha256(),
		pinned_product_base_blake3: &pinned_product_base_blake3,
	};
	let scorer_config_hash = product_scorer_config_hash(options.timeout, &basegame_identity);
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements)?;
	let measurements_by_id = index_measurements(&measurements)?;
	let mut measurement_ids = measurements_by_id
		.keys()
		.map(|measurement_id| (*measurement_id).to_string())
		.collect::<HashSet<_>>();
	let mut v2_measurement_ids = measurements
		.iter()
		.filter(|measurement| matches!(measurement, MeasurementRecord::V2 { .. }))
		.map(|measurement| measurement.measurement_id().to_string())
		.collect::<HashSet<_>>();
	let selected_measurements = snapshots
		.iter()
		.map(|snapshot| {
			(
				snapshot,
				measurement_identity(snapshot, &runner_identity, &scorer_config_hash),
			)
		})
		.collect::<Vec<_>>();
	let mut recoverable_measurement_ids = HashSet::with_capacity(selected_measurements.len());
	for (_, identity) in &selected_measurements {
		let measurement_id = identity.measurement_id();
		if !measurements_by_id.contains_key(measurement_id.as_str())
			&& !recoverable_measurement_ids.insert(measurement_id)
		{
			return Err("selected snapshots resolve to duplicate measurement identities".into());
		}
		v2_measurement_ids.insert(identity.measurement_id());
	}
	let file_results = read_jsonl::<FileResultRecord>(&paths.file_results)?;
	validate_file_result_foreign_keys(
		&file_results,
		&measurement_ids,
		&v2_measurement_ids,
		&recoverable_measurement_ids,
	)?;
	let object_records = read_jsonl::<ObjectRecord>(&paths.object_records)?;
	let store = ObjectStore::new(&paths.objects, &paths.work);
	let started = Instant::now();
	let mut input_hashes = HashSet::new();
	for snapshot in &snapshots {
		input_hashes.insert(snapshot.compatch.content_hash.clone());
		input_hashes.extend(
			snapshot
				.source_mods
				.iter()
				.map(|source| source.content_hash.clone()),
		);
	}
	let mut input_hashes: Vec<String> = input_hashes.into_iter().collect();
	input_hashes.sort();
	for (index, hash) in input_hashes.iter().enumerate() {
		eprintln!(
			"[measure] inspect object {}/{} {hash} ({})",
			index + 1,
			input_hashes.len(),
			progress(index + 1, input_hashes.len(), started)
		);
		store.open_object_guarded(hash).map_err(|error| {
			format!(
				"measurement CAS preflight failed for object {hash}: {:?}",
				error.kind()
			)
		})?;
	}

	let mut verified_outputs = HashMap::new();
	let mut cached = 0_usize;
	let mut pending = Vec::new();
	for (snapshot, identity) in selected_measurements {
		let measurement_id = identity.measurement_id();
		if let Some(record) = measurements_by_id.get(measurement_id.as_str()) {
			validate_cached_measurement(
				record,
				&file_results,
				&object_records,
				&store,
				&mut verified_outputs,
			)?;
			cached += 1;
		} else {
			pending.push((snapshot, identity));
		}
	}

	// Resolve every immutable input and allocate every work directory before the
	// first product invocation. A missing CAS object or local setup error can
	// therefore never poison an immutable measurement identity with `fatal`.
	let mut prepared = Vec::with_capacity(pending.len());
	for (snapshot, identity) in pending {
		let work = tempfile::Builder::new()
			.prefix("measurement-")
			.tempdir_in(&paths.work)?;
		let output_dir = work.path().join("output");
		fs::create_dir(&output_dir)?;
		let request = measurement_request(
			snapshot,
			&observations,
			&store,
			output_dir,
			options,
			&basegame_snapshot_label,
			pinned_base_view.path(),
		)
		.map_err(|error| {
			format!(
				"measurement input preflight failed for snapshot {}: {:?}",
				snapshot.snapshot_id,
				error.kind()
			)
		})?;
		prepared.push(PreparedMeasurement {
			identity,
			case_id: snapshot.case_id.clone(),
			request,
			_work: work,
		});
	}

	let mut measured = 0_usize;
	let mut failed = 0_usize;
	let case_started = Instant::now();
	let mut score_cache = ScoreCache::new();
	let pending_count = prepared.len();

	for (index, prepared) in prepared.into_iter().enumerate() {
		eprintln!(
			"[measure] case {}/{} {} ({})",
			index + 1,
			pending_count,
			prepared.case_id,
			progress(index + 1, pending_count, case_started)
		);
		let started_at = now_rfc3339();
		let terminal = runner.run(&prepared.request);
		validate_base_inventory_view(
			pinned_base_view.path(),
			&pinned_base_paths,
			&pinned_product_base_blake3,
		)
		.map_err(|error| {
			format!(
				"pinned product base changed during case {}: {:?}",
				prepared.case_id,
				error.kind()
			)
		})?;
		let (status, detail, completed) =
			classify_terminal_merge(terminal, &prepared.request, &mut score_cache)?;
		let detail = detail.map(|detail| redact_persisted_detail(&detail, &prepared.request));
		let measurement_id = prepared.identity.measurement_id();
		let preexisting_file_results = file_results
			.iter()
			.filter(|record| record.measurement_id == measurement_id)
			.count();
		let summary = completed
			.as_ref()
			.map(|completed| measurement_summary(&completed.result));
		let completed_output = completed.is_some();
		let replayed_file_results = completed.map_or_else(Vec::new, |mut completed| {
			completed
				.result
				.files
				.into_iter()
				.map(|file| {
					let relative_path = file.rel.clone();
					let resolution = completed.resolutions.remove(&relative_path);
					let result = serde_json::json!({
						"score": file,
						"human_resolution": resolution
					});
					FileResultRecord::new(measurement_id.clone(), relative_path, result)
				})
				.collect::<Vec<_>>()
		});
		if let Some(summary) = &summary {
			validate_replayed_file_results(
				&measurement_id,
				summary,
				&file_results,
				&replayed_file_results,
			)?;
		} else if preexisting_file_results != 0 {
			return Err(format!(
				"recoverable orphan measurement {measurement_id} has file evidence but its replay did not complete"
			)
			.into());
		}
		let merged_output_hash = if completed_output {
			Some(
				archive_output_tree(&store, &paths, &prepared.request.output_dir)?
					.ok_or("completed merge output disappeared before archival")?,
			)
		} else {
			None
		};
		if let Some(summary) = &summary {
			append_unique_many(&paths.file_results, &replayed_file_results)?;
			let persisted_file_results = read_jsonl::<FileResultRecord>(&paths.file_results)?;
			validate_file_result_foreign_keys(
				&persisted_file_results,
				&measurement_ids,
				&v2_measurement_ids,
				&recoverable_measurement_ids,
			)?;
			validate_replayed_file_results(&measurement_id, summary, &persisted_file_results, &[])?;
		}
		let record = MeasurementRecord::new_v2(
			prepared.identity,
			started_at,
			now_rfc3339(),
			status,
			detail,
			merged_output_hash,
			summary,
		);
		// Commit the terminal record last. If the process is interrupted while
		// persisting file results, the next run sees no terminal measurement and
		// safely replays the idempotent file-result writes before committing it.
		append_unique(&paths.measurements, &record)?;
		measurement_ids.insert(measurement_id.clone());
		recoverable_measurement_ids.remove(&measurement_id);
		measured += 1;
		if status.counts_as_merge_failed() {
			failed += 1;
		}
	}

	Ok(MeasureRunSummary {
		selected: snapshots.len(),
		cached,
		measured,
		failed,
		product_hash: runner_identity.engine_artifact.hash,
		scorer_config_hash,
	})
}

pub fn report(options: &ReportOptions<'_>) -> Result<(), Box<dyn std::error::Error>> {
	let paths = DatasetPaths::new(options.dataset_root);
	let observations = read_jsonl::<ObservationRecord>(&paths.observations)?;
	let candidate_snapshots = select_snapshots(
		read_jsonl::<SnapshotRecord>(&paths.snapshots)?,
		&observations,
		options.snapshot_ids,
		options.limit,
	)?;
	let candidate_cases = candidate_snapshots.len();
	let mut snapshots: Vec<(SnapshotRecord, String, OracleAssessment)> = candidate_snapshots
		.into_iter()
		.map(|snapshot| {
			let observation = latest_observation(&observations, &snapshot.snapshot_id);
			let title = observation
				.map(|record| record.compatch.title.clone())
				.unwrap_or_else(|| snapshot.case_id.clone());
			let oracle = assess_oracle_candidate(
				&title,
				snapshot.source_mods.len(),
				observation.is_some_and(|record| record.mod_churned),
			);
			(snapshot, title, oracle)
		})
		.collect();
	let scorable_cases = snapshots
		.iter()
		.filter(|(_, _, assessment)| assessment.is_scorable())
		.count();
	snapshots.retain(|(_, _, assessment)| options.cohort.includes(assessment));
	let report_snapshot_ids: HashSet<&str> = snapshots
		.iter()
		.map(|(snapshot, _, _)| snapshot.snapshot_id.as_str())
		.collect();
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements)?;
	let selector = match (options.cohort_id, options.scorer_version) {
		(Some(cohort_id), None) => MeasurementCohortSelector::CohortId(cohort_id),
		(None, Some(scorer_version)) => MeasurementCohortSelector::ScorerVersion(scorer_version),
		(None, None) => MeasurementCohortSelector::ScorerVersion(SCORER_VERSION),
		(Some(_), Some(_)) => {
			return Err("report accepts either cohort_id or scorer_version, not both".into());
		}
	};
	let registry = committed_measurement_cohort_registry()?;
	let selected_cohort = select_measurement_cohort(&measurements, &registry, selector)?;
	let descriptor = selected_cohort.descriptor;
	let merge_kernel = descriptor.merge_kernel.ok_or_else(|| {
		format!(
			"measurement cohort {} has no registered merge kernel",
			descriptor.cohort_id
		)
	})?;
	let mut selected: HashMap<String, &MeasurementRecord> = HashMap::new();
	for measurement in selected_cohort.measurements {
		if !report_snapshot_ids.contains(measurement.snapshot_id()) {
			continue;
		}
		selected
			.entry(measurement.snapshot_id().to_string())
			.and_modify(|current| {
				if measurement.finished_at() > current.finished_at() {
					*current = measurement;
				}
			})
			.or_insert(measurement);
	}

	let mut cases = Vec::with_capacity(snapshots.len());
	let mut status_counts = BTreeMap::new();
	let mut terminal_cases = 0_usize;
	let mut completed_cases = 0_usize;
	let mut reference_output = QualityAggregate::default();
	let mut multi_source = QualityAggregate::default();
	for (snapshot, title, oracle) in &snapshots {
		let measurement = selected.get(&snapshot.snapshot_id).copied();
		let (status, measurement_id, detail, summary) = match measurement {
			Some(measurement) => {
				terminal_cases += 1;
				let status = terminal_status_name(measurement.status()).to_string();
				if measurement.status() == TerminalStatus::Completed {
					completed_cases += 1;
					if let Some(summary) = measurement.summary() {
						reference_output.accepted += summary.accepted_ground_truth_files;
						reference_output.total += summary.ground_truth_files;
						merge_counts(
							&mut reference_output.verdicts,
							&summary.all_ground_truth_verdicts,
						);
						multi_source.accepted += summary.accepted_multi_source_files;
						multi_source.total += summary.multi_source_files;
						merge_counts(&mut multi_source.verdicts, &summary.multi_source_verdicts);
					}
				}
				(
					status,
					Some(measurement.measurement_id().to_string()),
					measurement.detail().map(str::to_string),
					measurement.summary().cloned(),
				)
			}
			None => ("missing".to_string(), None, None, None),
		};
		*status_counts.entry(status.clone()).or_default() += 1;
		cases.push(BaselineCase {
			case_id: snapshot.case_id.clone(),
			snapshot_id: snapshot.snapshot_id.clone(),
			title: title.clone(),
			oracle: oracle.clone(),
			status,
			measurement_id,
			detail,
			summary,
		});
	}
	let report = BaselineReport {
		schema: MEASUREMENT_REPORT_SCHEMA.to_string(),
		generated_at: now_rfc3339(),
		measurement_cohort_id: descriptor.cohort_id,
		measurement_cohort_label: descriptor.label,
		measurement_identity: descriptor.identity,
		merge_kernel,
		scorer_version: descriptor.scorer_version,
		oracle_policy_version: ORACLE_POLICY_VERSION.to_string(),
		cohort: options.cohort.as_str().to_string(),
		candidate_cases,
		scorable_cases,
		excluded_cases: candidate_cases.saturating_sub(scorable_cases),
		baseline_complete: !snapshots.is_empty() && terminal_cases == snapshots.len(),
		total_cases: snapshots.len(),
		terminal_cases,
		completed_cases,
		merge_failed_cases: terminal_cases.saturating_sub(completed_cases),
		status_counts,
		reference_output,
		multi_source,
		cases,
	};
	fs::create_dir_all(options.output_dir)?;
	fs::write(
		options.output_dir.join("baseline.json"),
		format!("{}\n", serde_json::to_string_pretty(&report)?),
	)?;
	fs::write(
		options.output_dir.join("report.md"),
		render_baseline_report(&report),
	)?;
	Ok(())
}

pub fn export_dataset(options: &ExportOptions<'_>) -> Result<(), Box<dyn std::error::Error>> {
	let paths = DatasetPaths::new(options.dataset_root);
	if options.output_dir.is_dir() && fs::read_dir(options.output_dir)?.next().is_some() {
		return Err(format!(
			"export output directory must be empty: {}",
			options.output_dir.display()
		)
		.into());
	}
	fs::create_dir_all(options.output_dir)?;
	for source in [
		&paths.manifest,
		&paths.object_records,
		&paths.snapshots,
		&paths.observations,
		&paths.measurements,
		&paths.file_results,
		&paths.shadow_measurements,
		&paths.annotations,
	] {
		if source.is_file() {
			fs::write(
				options.output_dir.join(
					source
						.file_name()
						.expect("dataset metadata paths have file names"),
				),
				fs::read(source)?,
			)?;
		}
	}

	let mut exported = Vec::new();
	if options.profile != DatasetExportProfile::Metadata {
		let store = ObjectStore::new(&paths.objects, &paths.work);
		let records = read_jsonl::<ObjectRecord>(&paths.object_records)?;
		let mut archives: Vec<(String, String)> = records
			.iter()
			.map(|record| {
				let directory = if record.kind == ObjectKind::MergedOutput {
					"outputs"
				} else {
					"objects"
				};
				(directory.to_string(), record.content_hash.clone())
			})
			.collect::<HashSet<_>>()
			.into_iter()
			.collect();
		archives.sort();
		let profile = match options.profile {
			DatasetExportProfile::Semantic => ExportProfile::Semantic,
			DatasetExportProfile::Full => ExportProfile::Full,
			DatasetExportProfile::Metadata => unreachable!("handled above"),
		};
		for (index, (directory, hash)) in archives.iter().enumerate() {
			eprintln!("[export] object {}/{} {hash}", index + 1, archives.len());
			fs::create_dir_all(options.output_dir.join(directory))?;
			let relative = format!("{directory}/{hash}.tar.zst");
			let archive =
				store.export_object(hash, &options.output_dir.join(&relative), profile)?;
			exported.push(ExportManifestObject {
				content_hash: hash.clone(),
				archive: relative,
				archive_hash: archive.hash,
				archive_bytes: archive.bytes,
			});
		}
	}
	let profile = match options.profile {
		DatasetExportProfile::Metadata => "metadata",
		DatasetExportProfile::Semantic => "semantic",
		DatasetExportProfile::Full => "full",
	};
	let manifest = ExportManifest {
		schema: SCHEMA.to_string(),
		profile: profile.to_string(),
		objects: exported,
	};
	fs::write(
		options.output_dir.join("export.json"),
		format!("{}\n", serde_json::to_string_pretty(&manifest)?),
	)?;
	write_export_checksums(options.output_dir)?;
	Ok(())
}

fn write_export_checksums(output_dir: &Path) -> io::Result<()> {
	let mut files: Vec<PathBuf> = walkdir::WalkDir::new(output_dir)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.filter(|entry| entry.file_name() != "checksums.txt")
		.map(|entry| entry.into_path())
		.collect();
	files.sort();
	let mut checksums = String::new();
	for file in files {
		let relative = file
			.strip_prefix(output_dir)
			.expect("export files remain under output directory")
			.to_string_lossy()
			.replace('\\', "/");
		checksums.push_str(&format!("{}  {relative}\n", digest_file(&file)?));
	}
	fs::write(output_dir.join("checksums.txt"), checksums)
}

fn product_scorer_config_hash(
	timeout: Duration,
	basegame_identity: &ProductBaseIdentity<'_>,
) -> String {
	let config = serde_json::json!({
		"scorer_version": SCORER_VERSION,
		"scope": MeasurementScope::FullProductMerge.as_str(),
		"merge_kernel": MeasurementKernel::SemanticTree.as_str(),
		"public_command": "foch_merge_non_interactive",
		"force": false,
		"retained_paths": "all",
		"retained_path_filter": null,
		"include_game_base": true,
		"timeout_nanos": timeout.as_nanos().to_string(),
		"ordering": "gui_sensitive_else_insensitive",
		"basegame_subtraction": "semantic_atoms_v1",
		"basegame_identity": basegame_identity,
		"multi_source": "all_sources_v1"
	});
	stable_id("scorer-config", &[config.to_string().as_bytes()])
}

fn product_base_inventory_paths(
	game_root: &Path,
	installed_inventory_paths: &[String],
) -> io::Result<Vec<String>> {
	let mut relative_paths = validated_base_inventory_paths(installed_inventory_paths)?;
	for version_path in [
		"launcher-settings.json",
		"launcher/launcher-settings.json",
		"version.txt",
	] {
		if game_root.join(version_path).is_file()
			&& !relative_paths.iter().any(|path| path == version_path)
		{
			relative_paths.push(version_path.to_string());
		}
	}
	relative_paths.sort();
	Ok(relative_paths)
}

fn materialize_base_inventory_view(
	game_root: &Path,
	inventory_paths: &[String],
	work_root: &Path,
) -> io::Result<(tempfile::TempDir, String)> {
	let relative_paths = validated_base_inventory_paths(inventory_paths)?;
	let view = tempfile::Builder::new()
		.prefix("scoring-base-")
		.tempdir_in(work_root)?;
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-product-base-inventory-v1");
	let mut buffer = vec![0_u8; 1024 * 1024];
	let total = relative_paths.len();
	let progress_interval = (total / 10).max(1_000);
	let started = Instant::now();
	for (index, relative_path) in relative_paths.into_iter().enumerate() {
		let source_path = game_root.join(&relative_path);
		let source_link_metadata = fs::symlink_metadata(&source_path).map_err(|error| {
			io::Error::new(
				error.kind(),
				format!("failed to inspect base inventory file {relative_path:?}: {error}"),
			)
		})?;
		if !source_link_metadata.file_type().is_file() {
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				format!("base inventory entry is not a regular file: {relative_path:?}"),
			));
		}
		let destination = view.path().join(&relative_path);
		if let Some(parent) = destination.parent() {
			fs::create_dir_all(parent)?;
		}
		let mut source = File::open(&source_path)?;
		let source_metadata_before = source.metadata()?;
		let mut target = File::create(&destination)?;
		hasher.update(&(relative_path.len() as u64).to_le_bytes());
		hasher.update(relative_path.as_bytes());
		hasher.update(&source_metadata_before.len().to_le_bytes());
		let mut copied = 0_u64;
		loop {
			let read = source.read(&mut buffer)?;
			if read == 0 {
				break;
			}
			target.write_all(&buffer[..read])?;
			hasher.update(&buffer[..read]);
			copied = copied.saturating_add(read as u64);
		}
		drop(target);
		let source_metadata_after = source.metadata()?;
		if copied != source_metadata_before.len()
			|| source_metadata_before.len() != source_metadata_after.len()
			|| source_metadata_before.modified().ok() != source_metadata_after.modified().ok()
		{
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				format!("base inventory file changed while copying: {relative_path:?}"),
			));
		}
		let mut permissions = fs::metadata(&destination)?.permissions();
		permissions.set_readonly(true);
		fs::set_permissions(&destination, permissions)?;

		let position = index + 1;
		if position == total || position % progress_interval == 0 {
			eprintln!(
				"[measure] pin product base {position}/{total} (elapsed={:.1}s)",
				started.elapsed().as_secs_f64()
			);
		}
	}
	Ok((view, hasher.finalize().to_hex().to_string()))
}

fn validated_base_inventory_paths(inventory_paths: &[String]) -> io::Result<Vec<String>> {
	let mut relative_paths = inventory_paths.to_vec();
	relative_paths.sort();
	if relative_paths.windows(2).any(|pair| pair[0] == pair[1]) {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"installed base snapshot inventory contains duplicate paths",
		));
	}
	for relative_path in &relative_paths {
		let relative = Path::new(relative_path);
		if relative_path.is_empty()
			|| relative.is_absolute()
			|| relative.components().any(|component| {
				matches!(
					component,
					Component::Prefix(_) | Component::RootDir | Component::ParentDir
				)
			}) {
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				format!("invalid base inventory path: {relative_path:?}"),
			));
		}
	}
	Ok(relative_paths)
}

fn validate_base_inventory_view(
	view_root: &Path,
	inventory_paths: &[String],
	expected_digest: &str,
) -> io::Result<()> {
	let expected_paths = validated_base_inventory_paths(inventory_paths)?
		.into_iter()
		.collect::<HashSet<_>>();
	let mut actual_paths = HashSet::with_capacity(expected_paths.len());
	for entry in WalkDir::new(view_root).follow_links(false) {
		let entry = entry.map_err(io::Error::other)?;
		if entry.path() == view_root || entry.file_type().is_dir() {
			continue;
		}
		if !entry.file_type().is_file() {
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				"pinned product base contains a non-regular entry",
			));
		}
		let relative = entry
			.path()
			.strip_prefix(view_root)
			.expect("walked base entry remains under its root")
			.to_string_lossy()
			.replace('\\', "/");
		actual_paths.insert(relative);
	}
	if actual_paths != expected_paths {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"pinned product base file set changed",
		));
	}
	let actual_digest = base_inventory_digest(view_root, inventory_paths)?;
	if actual_digest != expected_digest {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"pinned product base content changed",
		));
	}
	Ok(())
}

fn base_inventory_digest(game_root: &Path, inventory_paths: &[String]) -> io::Result<String> {
	let relative_paths = validated_base_inventory_paths(inventory_paths)?;

	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-product-base-inventory-v1");
	let mut buffer = vec![0_u8; 1024 * 1024];
	let total = relative_paths.len();
	let progress_interval = (total / 10).max(1_000);
	let started = Instant::now();
	for (index, relative_path) in relative_paths.into_iter().enumerate() {
		let relative = Path::new(&relative_path);
		let mut file = File::open(game_root.join(relative)).map_err(|error| {
			io::Error::new(
				error.kind(),
				format!("failed to read base inventory file {relative_path:?}: {error}"),
			)
		})?;
		let metadata_before = file.metadata()?;
		if !metadata_before.is_file() {
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				format!("base inventory entry is not a file: {relative_path:?}"),
			));
		}
		hasher.update(&(relative_path.len() as u64).to_le_bytes());
		hasher.update(relative_path.as_bytes());
		hasher.update(&metadata_before.len().to_le_bytes());
		loop {
			let read = file.read(&mut buffer)?;
			if read == 0 {
				break;
			}
			hasher.update(&buffer[..read]);
		}
		let metadata_after = file.metadata()?;
		if metadata_before.len() != metadata_after.len()
			|| metadata_before.modified().ok() != metadata_after.modified().ok()
		{
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				format!("base inventory file changed while hashing: {relative_path:?}"),
			));
		}
		let position = index + 1;
		if position == total || position % progress_interval == 0 {
			eprintln!(
				"[measure] hash base inventory {position}/{total} (elapsed={:.1}s)",
				started.elapsed().as_secs_f64()
			);
		}
	}
	Ok(hasher.finalize().to_hex().to_string())
}

fn validate_runner_identity(
	identity: &MeasurementRunnerIdentity,
) -> Result<(), Box<dyn std::error::Error>> {
	let hash = &identity.engine_artifact.hash;
	if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
		return Err("measurement runner artifact must have a 64-character BLAKE3 hash".into());
	}
	if identity.worker_protocol_version.trim().is_empty() {
		return Err("measurement runner protocol version must not be empty".into());
	}
	if identity.merge_kernel != MeasurementKernel::SemanticTree {
		return Err("new product measurements require the semantic_tree kernel".into());
	}
	Ok(())
}

fn validate_measurement_game(
	snapshot: &SnapshotRecord,
	local: &crate::config::Eu4GameDiscovery,
) -> Result<(), Box<dyn std::error::Error>> {
	if snapshot.game.app_id != crate::config::EU4_APPID {
		return Err(format!(
			"snapshot {} has unexpected app ID {}",
			snapshot.snapshot_id, snapshot.game.app_id
		)
		.into());
	}
	if snapshot.game.version != local.game_version {
		return Err(format!(
			"base-game version mismatch for snapshot {}: snapshot={} local={}",
			snapshot.snapshot_id, snapshot.game.version, local.game_version
		)
		.into());
	}
	if snapshot.game.steam_build_id != local.steam_build_id {
		return Err(format!(
			"Steam build mismatch for snapshot {}: snapshot={:?} local={:?}",
			snapshot.snapshot_id, snapshot.game.steam_build_id, local.steam_build_id
		)
		.into());
	}
	Ok(())
}

pub fn executable_hash(path: &Path) -> io::Result<String> {
	digest_file(path)
}

fn resolve_case_paths<'a>(
	case: &'a Case,
	catalog: &WorkshopCatalog,
) -> Option<(&'a Case, PathBuf, Vec<PathBuf>)> {
	let compatch = catalog.resolve(&case.compatch_id)?;
	let sources = case
		.referenced_mods
		.iter()
		.map(|id| catalog.resolve(id))
		.collect::<Option<Vec<_>>>()?;
	Some((case, compatch, sources))
}

fn snapshot_cached(
	store: &ObjectStore,
	cache: &mut HashMap<PathBuf, StoredObject>,
	path: &Path,
) -> io::Result<StoredObject> {
	if let Some(object) = cache.get(path) {
		return Ok(object.clone());
	}
	let object = store.snapshot_tree(path)?;
	cache.insert(path.to_path_buf(), object.clone());
	Ok(object)
}

fn observation_for_case(case: &Case, snapshot_id: &str, observed_at: &str) -> ObservationRecord {
	let source_mods = case
		.referenced_mods
		.iter()
		.map(|id| {
			let meta = case.referenced_mod_meta.get(id);
			WorkshopObservation {
				workshop_id: id.clone(),
				title: meta
					.map(|meta| meta.title.clone())
					.unwrap_or_else(|| id.clone()),
				time_created: meta.map_or(0, |meta| meta.time_created),
				time_updated: meta.map_or(0, |meta| meta.time_updated),
				provenance: meta.map(|meta| meta.workshop.clone()).unwrap_or_default(),
			}
		})
		.collect();
	ObservationRecord::new(
		snapshot_id.to_string(),
		observed_at.to_string(),
		WorkshopObservation {
			workshop_id: case.compatch_id.clone(),
			title: case.title.clone(),
			time_created: case.time_created,
			time_updated: case.time_updated,
			provenance: case.workshop.clone(),
		},
		source_mods,
		case.subscriptions,
		case.mod_churned(),
	)
}

fn select_snapshots(
	snapshots: Vec<SnapshotRecord>,
	observations: &[ObservationRecord],
	exact_ids: Option<&[String]>,
	limit: usize,
) -> Result<Vec<SnapshotRecord>, Box<dyn std::error::Error>> {
	let mut snapshot_ids = HashSet::with_capacity(snapshots.len());
	for snapshot in &snapshots {
		if !snapshot.identity_is_valid() {
			return Err(
				format!("snapshot {} has an invalid identity", snapshot.snapshot_id).into(),
			);
		}
		if !snapshot_ids.insert(snapshot.snapshot_id.as_str()) {
			return Err(format!("snapshot {} occurs more than once", snapshot.snapshot_id).into());
		}
	}
	let Some(exact_ids) = exact_ids else {
		let mut selected = latest_snapshots(snapshots, observations);
		if limit > 0 {
			selected.truncate(limit);
		}
		return Ok(selected);
	};
	if exact_ids.is_empty() {
		return Err("exact snapshot selection must contain at least one ID".into());
	}
	if limit != 0 {
		return Err("exact snapshot selection cannot be combined with limit".into());
	}
	let requested = exact_ids.iter().map(String::as_str).collect::<HashSet<_>>();
	if requested.len() != exact_ids.len() {
		return Err("exact snapshot selection contains duplicate IDs".into());
	}
	let mut matches: HashMap<String, Vec<SnapshotRecord>> = HashMap::new();
	for snapshot in snapshots {
		if requested.contains(snapshot.snapshot_id.as_str()) {
			matches
				.entry(snapshot.snapshot_id.clone())
				.or_default()
				.push(snapshot);
		}
	}
	let mut selected = Vec::with_capacity(exact_ids.len());
	for snapshot_id in exact_ids {
		let found = matches.remove(snapshot_id).unwrap_or_default();
		if found.len() != 1 {
			return Err(format!(
				"exact snapshot ID {snapshot_id} must exist exactly once; found {} records",
				found.len()
			)
			.into());
		}
		selected.push(found.into_iter().next().expect("one exact snapshot"));
	}
	Ok(selected)
}

fn latest_snapshots(
	snapshots: Vec<SnapshotRecord>,
	observations: &[ObservationRecord],
) -> Vec<SnapshotRecord> {
	let mut observed_at: HashMap<&str, &str> = HashMap::new();
	for observation in observations {
		observed_at
			.entry(observation.snapshot_id.as_str())
			.and_modify(|timestamp| {
				if observation.observed_at.as_str() > *timestamp {
					*timestamp = observation.observed_at.as_str();
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
	latest.into_values().map(|(_, snapshot)| snapshot).collect()
}

fn latest_observation<'a>(
	observations: &'a [ObservationRecord],
	snapshot_id: &str,
) -> Option<&'a ObservationRecord> {
	observations
		.iter()
		.filter(|observation| observation.snapshot_id == snapshot_id)
		.max_by(|left, right| left.observed_at.cmp(&right.observed_at))
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

fn validate_cached_measurement(
	measurement: &MeasurementRecord,
	file_results: &[FileResultRecord],
	object_records: &[ObjectRecord],
	store: &ObjectStore,
	verified_outputs: &mut HashMap<String, crate::object_store::TreeStats>,
) -> Result<(), Box<dyn std::error::Error>> {
	let measurement_id = measurement.measurement_id();
	let measurement_file_results = file_results
		.iter()
		.filter(|record| record.measurement_id == measurement_id)
		.collect::<Vec<_>>();
	if measurement.status() != TerminalStatus::Completed {
		if measurement.merged_output_hash().is_some()
			|| measurement.summary().is_some()
			|| !measurement_file_results.is_empty()
		{
			return Err(format!(
				"non-completed cached measurement {measurement_id} contains completed evidence"
			)
			.into());
		}
		return Ok(());
	}

	let output_hash = measurement.merged_output_hash().ok_or_else(|| {
		format!("completed cached measurement {measurement_id} has no output CAS hash")
	})?;
	let summary = measurement.summary().ok_or_else(|| {
		format!("completed cached measurement {measurement_id} has no score summary")
	})?;
	let matching_objects = object_records
		.iter()
		.filter(|record| {
			record.kind == ObjectKind::MergedOutput && record.content_hash == output_hash
		})
		.collect::<Vec<_>>();
	if matching_objects.len() != 1 {
		return Err(format!(
			"completed cached measurement {measurement_id} requires exactly one merged-output object record for {output_hash}; found {}",
			matching_objects.len()
		)
		.into());
	}
	let object_record = matching_objects[0];
	let expected_object = ObjectRecord::new(
		ObjectKind::MergedOutput,
		output_hash.to_string(),
		None,
		object_record.stats.clone(),
	);
	if expected_object != *object_record {
		return Err(format!(
			"merged-output object record for {output_hash} has invalid identity or metadata"
		)
		.into());
	}
	let verified_stats = if let Some(stats) = verified_outputs.get(output_hash) {
		stats.clone()
	} else {
		let object = store.verify_object(output_hash).map_err(|error| {
			format!(
				"completed cached output {output_hash} failed CAS verification: {:?}",
				error.kind()
			)
		})?;
		verified_outputs.insert(output_hash.to_string(), object.stats.clone());
		object.stats
	};
	if verified_stats != object_record.stats {
		return Err(format!(
			"merged-output object record for {output_hash} has stale tree statistics"
		)
		.into());
	}
	validate_cached_file_results(measurement_id, summary, &measurement_file_results)
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

fn measurement_identity(
	snapshot: &SnapshotRecord,
	runner: &MeasurementRunnerIdentity,
	scorer_config_hash: &str,
) -> MeasurementIdentityV2 {
	MeasurementIdentityV2 {
		snapshot_id: snapshot.snapshot_id.clone(),
		engine_artifact: runner.engine_artifact.clone(),
		worker_protocol_version: runner.worker_protocol_version.clone(),
		merge_kernel: runner.merge_kernel,
		scope: runner.scope,
		scorer_version: SCORER_VERSION.to_string(),
		scorer_config_hash: scorer_config_hash.to_string(),
	}
}

fn measurement_request(
	snapshot: &SnapshotRecord,
	observations: &[ObservationRecord],
	store: &ObjectStore,
	output_dir: PathBuf,
	options: &MeasureOptions<'_>,
	expected_base_snapshot_identity: &str,
	pinned_basegame_root: &Path,
) -> io::Result<MeasurementRequest> {
	let observation = latest_observation(observations, &snapshot.snapshot_id);
	let compatch_dir = store.open_object(&snapshot.compatch.content_hash)?.tree;
	let source_dirs = snapshot
		.source_mods
		.iter()
		.map(|source| {
			store
				.open_object(&source.content_hash)
				.map(|object| object.tree)
		})
		.collect::<io::Result<Vec<_>>>()?;
	let case = Case {
		compatch_id: snapshot.compatch.workshop_id.clone(),
		title: observation
			.map(|record| record.compatch.title.clone())
			.unwrap_or_else(|| snapshot.case_id.clone()),
		referenced_mods: snapshot
			.source_mods
			.iter()
			.map(|source| source.workshop_id.clone())
			.collect(),
		..Case::default()
	};
	Ok(MeasurementRequest {
		snapshot_id: snapshot.snapshot_id.clone(),
		case,
		compatch_dir,
		source_dirs,
		output_dir,
		basegame_root: pinned_basegame_root.to_path_buf(),
		expected_base_snapshot_identity: expected_base_snapshot_identity.to_string(),
		timeout: options.timeout,
	})
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
			let result = score_existing_output_with_cache(
				&request.case,
				&request.compatch_dir,
				&request.source_dirs,
				&request.output_dir,
				&report,
				Some(&request.basegame_root),
				merge_ms,
				score_cache,
			)
			.map_err(|error| format!("failed to score completed merge: {error}"))?;
			let source_mods = request
				.case
				.referenced_mods
				.iter()
				.zip(&request.source_dirs)
				.map(|(id, root)| SourceMod { id, root })
				.collect::<Vec<_>>();
			let resolutions = result
				.files
				.iter()
				.filter(|file| file.multi_source)
				.filter_map(|file| {
					classify_resolution(
						&file.rel,
						&source_mods,
						&request.compatch_dir,
						Some(&request.basegame_root),
					)
					.map(|resolution| (file.rel.clone(), resolution))
				})
				.collect();
			Ok((
				TerminalStatus::Completed,
				None,
				Some(CompletedMeasurement {
					result,
					resolutions,
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

pub fn archive_output_tree(
	store: &ObjectStore,
	paths: &DatasetPaths,
	output_dir: &Path,
) -> io::Result<Option<String>> {
	if !output_dir.is_dir() {
		return Ok(None);
	}
	let object = store.snapshot_tree(output_dir)?;
	append_unique(
		&paths.object_records,
		&ObjectRecord::new(
			ObjectKind::MergedOutput,
			object.hash.clone(),
			None,
			object.stats,
		),
	)?;
	Ok(Some(object.hash))
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

fn terminal_status_name(status: TerminalStatus) -> &'static str {
	match status {
		TerminalStatus::Completed => "completed",
		TerminalStatus::MergeFailed => "merge_failed",
		TerminalStatus::Crashed => "crashed",
		TerminalStatus::TimedOut => "timed_out",
		TerminalStatus::Fatal => "fatal",
	}
}

fn oracle_status_name(status: OracleStatus) -> &'static str {
	match status {
		OracleStatus::Accepted => "accepted",
		OracleStatus::Proposed => "proposed",
		OracleStatus::Excluded => "excluded",
	}
}

fn merge_counts(target: &mut BTreeMap<String, usize>, source: &BTreeMap<String, usize>) {
	for (key, count) in source {
		*target.entry(key.clone()).or_default() += count;
	}
}

fn render_baseline_report(report: &BaselineReport) -> String {
	let mut lines = vec![
		"# foch merge-quality baseline".to_string(),
		String::new(),
		format!(
			"Measurement cohort: **{}** (`{}`)",
			report.measurement_cohort_label, report.measurement_cohort_id
		),
		measurement_identity_summary(report),
		format!(
			"Oracle cohort: **{}** (policy `{}`) · candidates: **{}** · scorable: **{}** · excluded: **{}**",
			report.cohort,
			report.oracle_policy_version,
			report.candidate_cases,
			report.scorable_cases,
			report.excluded_cases
		),
		"The scorable cohort combines manually accepted and automatically proposed cases; proposed cases remain provisional oracle evidence.".to_string(),
		format!(
			"Baseline complete: **{}** · terminal cases: **{}/{}** · completed merges: **{}/{}**",
			report.baseline_complete,
			report.terminal_cases,
			report.total_cases,
			report.completed_cases,
			report.total_cases
		),
		format!(
			"Reference-output accepted: **{}/{}** · multi-source accepted: **{}/{}**",
			report.reference_output.accepted,
			report.reference_output.total,
			report.multi_source.accepted,
			report.multi_source.total
		),
		String::new(),
		"## Outcomes".to_string(),
		String::new(),
		"| status | cases |".to_string(),
		"|---|---:|".to_string(),
	];
	for (status, count) in &report.status_counts {
		lines.push(format!("| `{status}` | {count} |"));
	}
	lines.extend([
		String::new(),
		"## Cases".to_string(),
		String::new(),
		"| case | snapshot | oracle | status | multi-source accepted |".to_string(),
		"|---|---|---|---|---:|".to_string(),
	]);
	for case in &report.cases {
		let accepted = case.summary.as_ref().map_or_else(
			|| "n/a".to_string(),
			|summary| {
				format!(
					"{}/{}",
					summary.accepted_multi_source_files, summary.multi_source_files
				)
			},
		);
		lines.push(format!(
			"| {} (`{}`) | `{}` | `{}` | `{}` | {} |",
			case.title,
			case.case_id,
			case.snapshot_id,
			oracle_status_name(case.oracle.status),
			case.status,
			accepted
		));
	}
	lines.push(String::new());
	lines.join("\n")
}

fn measurement_identity_summary(report: &BaselineReport) -> String {
	match &report.measurement_identity {
		MeasurementCohortKey::OrchestratorBoundV1 {
			executable_hash,
			scorer_version,
			config_hash,
		} => format!(
			"Identity: `orchestrator_bound_v1` · artifact BLAKE3 `{executable_hash}` · scorer `{scorer_version}` · config `{config_hash}` · kernel `{}`",
			report.merge_kernel.as_str()
		),
		MeasurementCohortKey::EngineArtifactV2 {
			engine_artifact,
			worker_protocol_version,
			merge_kernel,
			scope,
			scorer_version,
			scorer_config_hash,
		} => format!(
			"Identity: `engine_artifact_v2` · artifact `{}` `{}` `{}` · worker protocol `{worker_protocol_version}` · scope `{}` · scorer `{scorer_version}` · config `{scorer_config_hash}` · kernel `{}`",
			engine_artifact.kind.as_str(),
			engine_artifact.hash_algorithm.as_str(),
			engine_artifact.hash,
			scope.as_str(),
			merge_kernel.as_str()
		),
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

	fn snapshot(case_id: &str, hash_seed: &str) -> SnapshotRecord {
		SnapshotRecord::new(
			case_id.to_string(),
			GameIdentity {
				app_id: 236850,
				version: "1.37.5".to_string(),
				steam_build_id: Some(42),
			},
			SnapshotObjectRef {
				workshop_id: case_id.to_string(),
				content_hash: hash_seed.repeat(64),
			},
			vec![
				SnapshotObjectRef {
					workshop_id: "a".to_string(),
					content_hash: "a".repeat(64),
				},
				SnapshotObjectRef {
					workshop_id: "b".to_string(),
					content_hash: "b".repeat(64),
				},
			],
		)
	}

	fn observation(snapshot: &SnapshotRecord, observed_at: &str) -> ObservationRecord {
		observation_with_title(snapshot, observed_at, &snapshot.case_id)
	}

	fn observation_with_title(
		snapshot: &SnapshotRecord,
		observed_at: &str,
		title: &str,
	) -> ObservationRecord {
		ObservationRecord::new(
			snapshot.snapshot_id.clone(),
			observed_at.to_string(),
			WorkshopObservation {
				workshop_id: snapshot.case_id.clone(),
				title: title.to_string(),
				time_created: 0,
				time_updated: 0,
				provenance: Default::default(),
			},
			Vec::new(),
			0,
			false,
		)
	}

	#[test]
	fn report_scorable_cohort_excludes_broad_search_false_positives() {
		let temp = tempfile::tempdir().unwrap();
		let paths = DatasetPaths::new(temp.path().join("dataset"));
		paths.ensure_layout().unwrap();
		let excluded = snapshot("excluded", "c");
		let proposed = snapshot("proposed", "d");
		for snapshot in [&excluded, &proposed] {
			append_unique(&paths.snapshots, snapshot).unwrap();
		}
		append_unique(
			&paths.observations,
			&observation_with_title(
				&excluded,
				"2026-07-12T00:00:00Z",
				"Elder Scrolls Universalis",
			),
		)
		.unwrap();
		append_unique(
			&paths.observations,
			&observation_with_title(&proposed, "2026-07-12T00:00:00Z", "Actual Compatch"),
		)
		.unwrap();
		let proposed_identity = MeasurementIdentityV2 {
			snapshot_id: proposed.snapshot_id.clone(),
			engine_artifact: EngineArtifactIdentity::foch_executable_blake3("a".repeat(64)),
			worker_protocol_version: "test-runner-v1".to_string(),
			merge_kernel: MeasurementKernel::SemanticTree,
			scope: MeasurementScope::FullProductMerge,
			scorer_version: SCORER_VERSION.to_string(),
			scorer_config_hash: "b".repeat(64),
		};
		let proposed_cohort_id = proposed_identity.cohort_id();
		append_unique(
			&paths.measurements,
			&MeasurementRecord::new_v2(
				proposed_identity,
				"2026-07-12T00:00:00Z".to_string(),
				"2026-07-12T00:01:00Z".to_string(),
				TerminalStatus::Crashed,
				Some("signal".to_string()),
				None,
				None,
			),
		)
		.unwrap();
		append_unique(
			&paths.measurements,
			&MeasurementRecord::new_v2(
				MeasurementIdentityV2 {
					snapshot_id: excluded.snapshot_id.clone(),
					engine_artifact: EngineArtifactIdentity::foch_executable_blake3("c".repeat(64)),
					worker_protocol_version: "test-runner-v1".to_string(),
					merge_kernel: MeasurementKernel::SemanticTree,
					scope: MeasurementScope::FullProductMerge,
					scorer_version: SCORER_VERSION.to_string(),
					scorer_config_hash: "d".repeat(64),
				},
				"2026-07-13T00:00:00Z".to_string(),
				"2026-07-13T00:01:00Z".to_string(),
				TerminalStatus::Crashed,
				Some("excluded candidate".to_string()),
				None,
				None,
			),
		)
		.unwrap();

		let output = temp.path().join("report");
		report(&ReportOptions {
			dataset_root: &paths.root,
			output_dir: &output,
			cohort_id: Some(&proposed_cohort_id),
			scorer_version: None,
			cohort: ReportCohort::Scorable,
			limit: 0,
			snapshot_ids: None,
		})
		.unwrap();
		let json: serde_json::Value =
			serde_json::from_str(&fs::read_to_string(output.join("baseline.json")).unwrap())
				.unwrap();
		assert_eq!(json["candidate_cases"], 2);
		assert_eq!(json["scorable_cases"], 1);
		assert_eq!(json["excluded_cases"], 1);
		assert_eq!(json["total_cases"], 1);
		assert_eq!(json["baseline_complete"], true);
		assert_eq!(json["measurement_cohort_id"], proposed_cohort_id);
		assert_eq!(
			json["measurement_identity"]["identity_kind"],
			"engine_artifact_v2"
		);
		assert_eq!(json["cases"][0]["case_id"], "proposed");
		assert_eq!(json["cases"][0]["oracle"]["status"], "proposed");
	}

	#[test]
	fn latest_snapshot_uses_observation_time_per_case() {
		let old = snapshot("case", "c");
		let new = snapshot("case", "d");
		let observations = vec![
			observation(&old, "2026-07-11T00:00:00Z"),
			observation(&new, "2026-07-12T00:00:00Z"),
		];
		assert_eq!(
			latest_snapshots(vec![old, new.clone()], &observations),
			vec![new]
		);
	}

	#[test]
	fn measurement_game_contract_binds_the_exact_steam_build() {
		let snapshot = snapshot("case", "c");
		let local = crate::config::Eu4GameDiscovery {
			game_root: PathBuf::from("/unused"),
			game_version: snapshot.game.version.clone(),
			steam_build_id: None,
			steam_root: None,
		};
		let error = validate_measurement_game(&snapshot, &local).unwrap_err();
		assert!(error.to_string().contains("Steam build mismatch"));
	}

	#[test]
	fn product_scorer_config_identity_binds_execution_scope_and_timeout() {
		let base_a = ProductBaseIdentity {
			game: "eu4",
			game_version: "1",
			steam_build_id: Some(1),
			installed_snapshot_sha256: "base-a",
			pinned_product_base_blake3: "raw-a",
		};
		let base_b = ProductBaseIdentity {
			installed_snapshot_sha256: "base-b",
			..base_a
		};
		let raw_b = ProductBaseIdentity {
			pinned_product_base_blake3: "raw-b",
			..base_a
		};
		assert_eq!(
			product_scorer_config_hash(Duration::from_secs(600), &base_a),
			product_scorer_config_hash(Duration::from_secs(600), &base_a)
		);
		assert_ne!(
			product_scorer_config_hash(Duration::from_secs(600), &base_a),
			product_scorer_config_hash(Duration::from_secs(600) + Duration::from_nanos(1), &base_a)
		);
		assert_ne!(
			product_scorer_config_hash(Duration::from_secs(600), &base_a),
			product_scorer_config_hash(Duration::from_secs(600), &base_b)
		);
		assert_ne!(
			product_scorer_config_hash(Duration::from_secs(600), &base_a),
			product_scorer_config_hash(Duration::from_secs(600), &raw_b)
		);
	}

	#[test]
	fn materialized_base_inventory_excludes_unbound_files_and_freezes_scoring_bytes() {
		let temp = tempfile::tempdir().unwrap();
		let game = temp.path().join("game");
		let work = temp.path().join("work");
		fs::create_dir_all(game.join("decisions")).unwrap();
		fs::create_dir(&work).unwrap();
		fs::write(game.join("decisions/a.txt"), "a").unwrap();
		fs::write(game.join("ignored.txt"), "ignored-a").unwrap();
		let inventory = vec!["decisions/a.txt".to_string()];
		let (view, first) = materialize_base_inventory_view(&game, &inventory, &work).unwrap();
		assert!(!view.path().join("ignored.txt").exists());

		fs::write(game.join("ignored.txt"), "ignored-b").unwrap();
		fs::write(game.join("decisions/a.txt"), "b").unwrap();
		assert_eq!(
			fs::read_to_string(view.path().join("decisions/a.txt")).unwrap(),
			"a"
		);
		assert_eq!(
			first,
			base_inventory_digest(view.path(), &inventory).unwrap()
		);

		let (changed_view, changed_digest) =
			materialize_base_inventory_view(&game, &inventory, &work).unwrap();
		assert_ne!(first, changed_digest);
		assert_eq!(
			changed_digest,
			base_inventory_digest(changed_view.path(), &inventory).unwrap()
		);
	}

	#[test]
	fn pinned_base_validation_rejects_added_missing_and_changed_files() {
		let temp = tempfile::tempdir().unwrap();
		let game = temp.path().join("game");
		let work = temp.path().join("work");
		fs::create_dir_all(game.join("decisions")).unwrap();
		fs::create_dir(&work).unwrap();
		fs::write(game.join("decisions/a.txt"), "a").unwrap();
		let inventory = vec!["decisions/a.txt".to_string()];

		let (added_view, added_digest) =
			materialize_base_inventory_view(&game, &inventory, &work).unwrap();
		fs::write(added_view.path().join("extra.txt"), "extra").unwrap();
		assert!(
			validate_base_inventory_view(added_view.path(), &inventory, &added_digest).is_err()
		);

		let (missing_view, missing_digest) =
			materialize_base_inventory_view(&game, &inventory, &work).unwrap();
		fs::remove_file(missing_view.path().join("decisions/a.txt")).unwrap();
		assert!(
			validate_base_inventory_view(missing_view.path(), &inventory, &missing_digest).is_err()
		);

		let (changed_view, changed_digest) =
			materialize_base_inventory_view(&game, &inventory, &work).unwrap();
		let changed_path = changed_view.path().join("decisions/a.txt");
		let mut permissions = fs::metadata(&changed_path).unwrap().permissions();
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			permissions.set_mode(permissions.mode() | 0o200);
		}
		#[cfg(not(unix))]
		permissions.set_readonly(false);
		fs::set_permissions(&changed_path, permissions).unwrap();
		fs::write(changed_path, "b").unwrap();
		assert!(
			validate_base_inventory_view(changed_view.path(), &inventory, &changed_digest).is_err()
		);
	}

	#[test]
	fn report_requires_a_terminal_outcome_for_every_case() {
		let temp = tempfile::tempdir().unwrap();
		let paths = DatasetPaths::new(temp.path().join("dataset"));
		paths.ensure_layout().unwrap();
		let first = snapshot("first", "c");
		let second = snapshot("second", "d");
		append_unique(&paths.snapshots, &first).unwrap();
		append_unique(&paths.snapshots, &second).unwrap();
		append_unique(
			&paths.observations,
			&observation(&first, "2026-07-12T00:00:00Z"),
		)
		.unwrap();
		append_unique(
			&paths.observations,
			&observation(&second, "2026-07-12T00:00:00Z"),
		)
		.unwrap();
		let identity = MeasurementIdentityV2 {
			snapshot_id: first.snapshot_id.clone(),
			engine_artifact: EngineArtifactIdentity::foch_executable_blake3("e".repeat(64)),
			worker_protocol_version: "test-runner-v1".to_string(),
			merge_kernel: MeasurementKernel::SemanticTree,
			scope: MeasurementScope::FullProductMerge,
			scorer_version: SCORER_VERSION.to_string(),
			scorer_config_hash: "f".repeat(64),
		};
		let cohort_id = identity.cohort_id();
		let measurement = MeasurementRecord::new_v2(
			identity,
			"start".to_string(),
			"finish".to_string(),
			TerminalStatus::Crashed,
			Some("signal".to_string()),
			None,
			None,
		);
		append_unique(&paths.measurements, &measurement).unwrap();
		let output = temp.path().join("report");
		report(&ReportOptions {
			dataset_root: &paths.root,
			output_dir: &output,
			cohort_id: Some(&cohort_id),
			scorer_version: None,
			cohort: ReportCohort::AllCandidates,
			limit: 0,
			snapshot_ids: None,
		})
		.unwrap();
		let json: serde_json::Value =
			serde_json::from_str(&fs::read_to_string(output.join("baseline.json")).unwrap())
				.unwrap();
		assert_eq!(json["baseline_complete"], false);
		assert_eq!(json["terminal_cases"], 1);
		assert_eq!(json["merge_failed_cases"], 1);
	}

	#[test]
	fn report_rejects_a_missing_requested_scorer_cohort() {
		let temp = tempfile::tempdir().unwrap();
		let paths = DatasetPaths::new(temp.path().join("dataset"));
		paths.ensure_layout().unwrap();
		let snapshot = snapshot("case", "c");
		append_unique(&paths.snapshots, &snapshot).unwrap();
		append_unique(
			&paths.observations,
			&observation(&snapshot, "2026-07-12T00:00:00Z"),
		)
		.unwrap();
		append_unique(
			&paths.measurements,
			&MeasurementRecord::new_v2(
				MeasurementIdentityV2 {
					snapshot_id: snapshot.snapshot_id.clone(),
					engine_artifact: EngineArtifactIdentity::foch_executable_blake3("1".repeat(64)),
					worker_protocol_version: "test-runner-v1".to_string(),
					merge_kernel: MeasurementKernel::SemanticTree,
					scope: MeasurementScope::FullProductMerge,
					scorer_version: "0.9.0".to_string(),
					scorer_config_hash: "2".repeat(64),
				},
				"start".to_string(),
				"finish".to_string(),
				TerminalStatus::Completed,
				None,
				None,
				None,
			),
		)
		.unwrap();

		let error = report(&ReportOptions {
			dataset_root: &paths.root,
			output_dir: &temp.path().join("report"),
			cohort_id: None,
			scorer_version: Some(SCORER_VERSION),
			cohort: ReportCohort::AllCandidates,
			limit: 0,
			snapshot_ids: None,
		})
		.unwrap_err();
		assert!(error.to_string().contains("no measurement cohort matches"));
	}
}
