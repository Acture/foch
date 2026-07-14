use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::{Eu4Discovery, WorkshopCatalog};
use crate::corpus::{
	Case, Corpus, ORACLE_POLICY_VERSION, OracleAssessment, OracleStatus, assess_oracle_candidate,
};
use crate::dataset::{
	DatasetPaths, FileResultRecord, GameIdentity, MeasurementIdentity, MeasurementRecord,
	MeasurementSummary, ObjectKind, ObjectRecord, ObservationRecord, SCHEMA, SCORER_VERSION,
	SnapshotObjectRef, SnapshotRecord, TerminalStatus, WorkshopObservation, append_unique,
	append_unique_many, now_rfc3339, read_jsonl, stable_id,
};
use crate::object_store::{ExportProfile, ObjectStore, StoredObject};
use crate::orchestrate::{BaseGameMode, CaseResult, score_case_from_paths_with_cache};
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
	pub executable: &'a Path,
	pub basegame_root: &'a Path,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasureRunSummary {
	pub selected: usize,
	pub cached: usize,
	pub measured: usize,
	pub failed: usize,
	pub executable_hash: String,
	pub config_hash: String,
}

#[derive(Clone, Debug)]
pub struct ReportOptions<'a> {
	pub dataset_root: &'a Path,
	pub output_dir: &'a Path,
	pub executable_hash: Option<&'a str>,
	pub config_hash: Option<&'a str>,
	pub cohort: ReportCohort,
	pub limit: usize,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerOutput {
	Completed {
		result: Box<CaseResult>,
		resolutions: BTreeMap<String, Resolution>,
	},
	MergeFailed {
		detail: String,
	},
	Fatal {
		detail: String,
	},
}

struct CompletedMeasurement {
	result: CaseResult,
	resolutions: BTreeMap<String, Resolution>,
}

#[derive(Clone, Debug, Serialize)]
struct BaselineReport {
	schema: String,
	generated_at: String,
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
	executable_hash: Option<String>,
	config_hash: Option<String>,
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

pub fn measure(
	options: &MeasureOptions<'_>,
) -> Result<MeasureRunSummary, Box<dyn std::error::Error>> {
	let paths = DatasetPaths::new(options.dataset_root);
	paths.ensure_layout()?;
	let observations = read_jsonl::<ObservationRecord>(&paths.observations)?;
	let mut snapshots = latest_snapshots(
		read_jsonl::<SnapshotRecord>(&paths.snapshots)?,
		&observations,
	);
	if options.limit > 0 {
		snapshots.truncate(options.limit);
	}
	let executable_hash = digest_file(options.executable)?;
	let basegame_version = crate::config::detect_game_version(options.basegame_root)
		.ok_or("failed to detect base-game version for scorer identity")?;
	let basegame_snapshot =
		foch_engine::installed_base_snapshot_identity("eu4", &basegame_version)?
			.ok_or_else(|| format!("no installed base snapshot for eu4@{basegame_version}"))?;
	let basegame_snapshot_label = basegame_snapshot.as_label();
	let basegame_identity = format!("eu4@{basegame_version}/{basegame_snapshot_label}");
	let config_hash = scorer_config_hash(options.timeout, Some(&basegame_identity));
	let existing: HashSet<String> = read_jsonl::<MeasurementRecord>(&paths.measurements)?
		.into_iter()
		.map(|measurement| measurement.measurement_id)
		.collect();
	let store = ObjectStore::new(&paths.objects, &paths.work);
	let started = Instant::now();
	let mut pending_hashes = HashSet::new();
	for snapshot in &snapshots {
		let identity = measurement_identity(snapshot, &executable_hash, &config_hash);
		if existing.contains(&measurement_id(&identity)) {
			continue;
		}
		pending_hashes.insert(snapshot.compatch.content_hash.clone());
		pending_hashes.extend(
			snapshot
				.source_mods
				.iter()
				.map(|source| source.content_hash.clone()),
		);
	}
	let mut pending_hashes: Vec<String> = pending_hashes.into_iter().collect();
	pending_hashes.sort();
	let mut verification_errors = HashMap::new();
	for (index, hash) in pending_hashes.iter().enumerate() {
		eprintln!(
			"[measure] verify object {}/{} {hash} ({})",
			index + 1,
			pending_hashes.len(),
			progress(index + 1, pending_hashes.len(), started)
		);
		if let Err(err) = store.verify_object(hash) {
			verification_errors.insert(hash.clone(), err.to_string());
		}
	}
	let mut cached = 0_usize;
	let mut measured = 0_usize;
	let mut failed = 0_usize;
	let case_started = Instant::now();

	for (index, snapshot) in snapshots.iter().enumerate() {
		let identity = measurement_identity(snapshot, &executable_hash, &config_hash);
		let measurement_id = measurement_id(&identity);
		if existing.contains(&measurement_id) {
			cached += 1;
			continue;
		}
		if let Some(detail) = snapshot_verification_error(snapshot, &verification_errors) {
			let timestamp = now_rfc3339();
			append_unique(
				&paths.measurements,
				&MeasurementRecord::new(
					identity,
					timestamp.clone(),
					timestamp,
					TerminalStatus::Fatal,
					Some(detail),
					None,
					None,
				),
			)?;
			measured += 1;
			failed += 1;
			continue;
		}
		eprintln!(
			"[measure] case {}/{} {} ({})",
			index + 1,
			snapshots.len(),
			snapshot.case_id,
			progress(index + 1, snapshots.len(), case_started)
		);
		let started_at = now_rfc3339();
		let work = tempfile::Builder::new()
			.prefix("measurement-")
			.tempdir_in(&paths.work)?;
		let output_dir = work.path().join("output");
		fs::create_dir(&output_dir)?;
		let stdout_path = work.path().join("stdout.json");
		let stderr_path = work.path().join("stderr.log");

		let child = run_measurement_child(
			options.executable,
			options.dataset_root,
			&snapshot.snapshot_id,
			&output_dir,
			&stdout_path,
			&stderr_path,
			options.timeout,
			options.basegame_root,
			&basegame_snapshot_label,
		);
		let (mut status, mut detail, completed) = classify_child(child, &stdout_path, &stderr_path);

		let merged_output_hash = match archive_output(&store, &paths, &output_dir) {
			Ok(hash) => hash,
			Err(err) => {
				status = TerminalStatus::Fatal;
				detail = Some(format!("failed to archive merged output: {err}"));
				None
			}
		};
		let summary = completed
			.as_ref()
			.map(|completed| measurement_summary(&completed.result));
		let record = MeasurementRecord::new(
			identity,
			started_at,
			now_rfc3339(),
			status,
			detail,
			merged_output_hash,
			summary,
		);
		append_unique(&paths.measurements, &record)?;
		if let Some(mut completed) = completed {
			let file_results: Vec<FileResultRecord> = completed
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
					FileResultRecord::new(record.measurement_id.clone(), relative_path, result)
				})
				.collect();
			append_unique_many(&paths.file_results, &file_results)?;
		}
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
		executable_hash,
		config_hash,
	})
}

pub fn measure_one(
	dataset_root: &Path,
	snapshot_id: &str,
	output_dir: &Path,
	basegame_root: &Path,
	expected_base_snapshot_identity: &str,
) -> Result<(), Box<dyn std::error::Error>> {
	let paths = DatasetPaths::new(dataset_root);
	let snapshot = read_jsonl::<SnapshotRecord>(&paths.snapshots)?
		.into_iter()
		.find(|snapshot| snapshot.snapshot_id == snapshot_id)
		.ok_or_else(|| format!("snapshot {snapshot_id} not found"))?;
	let observations = read_jsonl::<ObservationRecord>(&paths.observations)?;
	let observation = latest_observation(&observations, snapshot_id);
	let store = ObjectStore::new(&paths.objects, &paths.work);
	let actual_version = crate::config::detect_game_version(basegame_root);
	if actual_version.as_deref() != Some(snapshot.game.version.as_str()) {
		let output = WorkerOutput::Fatal {
			detail: format!(
				"base-game version mismatch: snapshot={} local={}",
				snapshot.game.version,
				actual_version.as_deref().unwrap_or("unknown")
			),
		};
		println!("{}", serde_json::to_string(&output)?);
		return Ok(());
	}
	let compatch = store.open_object(&snapshot.compatch.content_hash)?.tree;
	let source_dirs: Vec<PathBuf> = snapshot
		.source_mods
		.iter()
		.map(|source| {
			store
				.open_object(&source.content_hash)
				.map(|object| object.tree)
		})
		.collect::<io::Result<_>>()?;
	let case = Case {
		compatch_id: snapshot.compatch.workshop_id.clone(),
		title: observation
			.map(|observation| observation.compatch.title.clone())
			.unwrap_or_else(|| snapshot.case_id.clone()),
		referenced_mods: snapshot
			.source_mods
			.iter()
			.map(|source| source.workshop_id.clone())
			.collect(),
		..Case::default()
	};
	let mut cache = ScoreCache::new();
	let output = match score_case_from_paths_with_cache(
		&case,
		&compatch,
		&source_dirs,
		output_dir,
		BaseGameMode::Path(basegame_root),
		Some(expected_base_snapshot_identity),
		&mut cache,
	) {
		Ok(result) => {
			let source_mods: Vec<SourceMod<'_>> = case
				.referenced_mods
				.iter()
				.zip(&source_dirs)
				.map(|(id, root)| SourceMod { id, root })
				.collect();
			let resolutions = result
				.files
				.iter()
				.filter(|file| file.multi_source)
				.filter_map(|file| {
					classify_resolution(&file.rel, &source_mods, &compatch, Some(basegame_root))
						.map(|resolution| (file.rel.clone(), resolution))
				})
				.collect();
			WorkerOutput::Completed {
				result: Box::new(result),
				resolutions,
			}
		}
		Err(err) => WorkerOutput::MergeFailed {
			detail: err.to_string(),
		},
	};
	println!("{}", serde_json::to_string(&output)?);
	Ok(())
}

pub fn report(options: &ReportOptions<'_>) -> Result<(), Box<dyn std::error::Error>> {
	let paths = DatasetPaths::new(options.dataset_root);
	let observations = read_jsonl::<ObservationRecord>(&paths.observations)?;
	let candidate_snapshots = latest_snapshots(
		read_jsonl::<SnapshotRecord>(&paths.snapshots)?,
		&observations,
	);
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
	if options.limit > 0 {
		snapshots.truncate(options.limit);
	}
	let report_snapshot_ids: HashSet<&str> = snapshots
		.iter()
		.map(|(snapshot, _, _)| snapshot.snapshot_id.as_str())
		.collect();
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements)?;
	let cohort = measurements
		.iter()
		.filter(|measurement| {
			report_snapshot_ids.contains(measurement.snapshot_id.as_str())
				&& measurement.scorer_version == SCORER_VERSION
				&& options
					.executable_hash
					.is_none_or(|hash| measurement.executable_hash == hash)
				&& options
					.config_hash
					.is_none_or(|hash| measurement.config_hash == hash)
		})
		.max_by(|left, right| left.finished_at.cmp(&right.finished_at));
	let executable_hash = options
		.executable_hash
		.map(str::to_string)
		.or_else(|| cohort.map(|measurement| measurement.executable_hash.clone()));
	let config_hash = options
		.config_hash
		.map(str::to_string)
		.or_else(|| cohort.map(|measurement| measurement.config_hash.clone()));
	let mut selected: HashMap<String, &MeasurementRecord> = HashMap::new();
	for measurement in &measurements {
		if measurement.scorer_version != SCORER_VERSION
			|| executable_hash
				.as_deref()
				.is_some_and(|hash| measurement.executable_hash != hash)
			|| config_hash
				.as_deref()
				.is_some_and(|hash| measurement.config_hash != hash)
		{
			continue;
		}
		selected
			.entry(measurement.snapshot_id.clone())
			.and_modify(|current| {
				if measurement.finished_at > current.finished_at {
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
				let status = terminal_status_name(measurement.status).to_string();
				if measurement.status == TerminalStatus::Completed {
					completed_cases += 1;
					if let Some(summary) = &measurement.summary {
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
					Some(measurement.measurement_id.clone()),
					measurement.detail.clone(),
					measurement.summary.clone(),
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
		schema: SCHEMA.to_string(),
		generated_at: now_rfc3339(),
		scorer_version: SCORER_VERSION.to_string(),
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
		executable_hash,
		config_hash,
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

pub fn scorer_config_hash(timeout: Duration, basegame_identity: Option<&str>) -> String {
	let config = serde_json::json!({
		"scorer_version": SCORER_VERSION,
		"timeout_secs": timeout.as_secs(),
		"ordering": "gui_sensitive_else_insensitive",
		"basegame_subtraction": "semantic_atoms_v1",
		"basegame_identity": basegame_identity,
		"multi_source": "all_sources_v1"
	});
	stable_id("scorer-config", &[config.to_string().as_bytes()])
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

fn measurement_identity(
	snapshot: &SnapshotRecord,
	executable_hash: &str,
	config_hash: &str,
) -> MeasurementIdentity {
	MeasurementIdentity {
		snapshot_id: snapshot.snapshot_id.clone(),
		executable_hash: executable_hash.to_string(),
		scorer_version: SCORER_VERSION.to_string(),
		config_hash: config_hash.to_string(),
	}
}

fn snapshot_verification_error(
	snapshot: &SnapshotRecord,
	errors: &HashMap<String, String>,
) -> Option<String> {
	std::iter::once(&snapshot.compatch)
		.chain(&snapshot.source_mods)
		.find_map(|object| {
			errors.get(&object.content_hash).map(|error| {
				format!(
					"object {} ({}) failed verification: {error}",
					object.workshop_id, object.content_hash
				)
			})
		})
}

fn measurement_id(identity: &MeasurementIdentity) -> String {
	stable_id(
		"measurement",
		&[
			identity.snapshot_id.as_bytes(),
			identity.executable_hash.as_bytes(),
			identity.scorer_version.as_bytes(),
			identity.config_hash.as_bytes(),
		],
	)
}

struct ChildOutcome {
	status: ExitStatus,
	timed_out: bool,
}

#[allow(clippy::too_many_arguments)]
fn run_measurement_child(
	executable: &Path,
	dataset_root: &Path,
	snapshot_id: &str,
	output_dir: &Path,
	stdout_path: &Path,
	stderr_path: &Path,
	timeout: Duration,
	basegame_root: &Path,
	base_snapshot_identity: &str,
) -> io::Result<ChildOutcome> {
	let mut command = measurement_child_command(
		executable,
		dataset_root,
		snapshot_id,
		output_dir,
		basegame_root,
		base_snapshot_identity,
	);
	let mut child = command
		.stdout(Stdio::from(File::create(stdout_path)?))
		.stderr(Stdio::from(File::create(stderr_path)?))
		.spawn()?;
	let started = Instant::now();
	loop {
		if let Some(status) = child.try_wait()? {
			return Ok(ChildOutcome {
				status,
				timed_out: false,
			});
		}
		if started.elapsed() >= timeout {
			if let Err(kill_error) = child.kill() {
				if let Some(status) = child.try_wait()? {
					return Ok(ChildOutcome {
						status,
						timed_out: false,
					});
				}
				return Err(kill_error);
			}
			return Ok(ChildOutcome {
				status: child.wait()?,
				timed_out: true,
			});
		}
		thread::sleep(Duration::from_millis(100));
	}
}

fn measurement_child_command(
	executable: &Path,
	dataset_root: &Path,
	snapshot_id: &str,
	output_dir: &Path,
	basegame_root: &Path,
	base_snapshot_identity: &str,
) -> Command {
	let mut command = Command::new(executable);
	command
		.arg("--dataset-root")
		.arg(dataset_root)
		.arg("measure-one")
		.arg("--snapshot-id")
		.arg(snapshot_id)
		.arg("--output-dir")
		.arg(output_dir)
		.arg("--basegame-root")
		.arg(basegame_root)
		.arg("--base-snapshot-identity")
		.arg(base_snapshot_identity);
	command
}

fn classify_child(
	child: io::Result<ChildOutcome>,
	stdout_path: &Path,
	stderr_path: &Path,
) -> (TerminalStatus, Option<String>, Option<CompletedMeasurement>) {
	let stderr = read_tail(stderr_path, 8 * 1024)
		.ok()
		.filter(|text| !text.is_empty());
	let outcome = match child {
		Ok(outcome) => outcome,
		Err(err) => return (TerminalStatus::Fatal, Some(err.to_string()), None),
	};
	if outcome.timed_out {
		return (TerminalStatus::TimedOut, stderr, None);
	}
	if !outcome.status.success() {
		let status = if exit_was_signal(&outcome.status) {
			TerminalStatus::Crashed
		} else {
			TerminalStatus::Fatal
		};
		let detail = stderr.or_else(|| Some(format!("worker exited with {}", outcome.status)));
		return (status, detail, None);
	}
	let bytes = match fs::read(stdout_path) {
		Ok(bytes) => bytes,
		Err(err) => return (TerminalStatus::Fatal, Some(err.to_string()), None),
	};
	match serde_json::from_slice::<WorkerOutput>(&bytes) {
		Ok(WorkerOutput::Completed {
			result,
			resolutions,
		}) => (
			TerminalStatus::Completed,
			None,
			Some(CompletedMeasurement {
				result: *result,
				resolutions,
			}),
		),
		Ok(WorkerOutput::MergeFailed { detail }) => {
			(TerminalStatus::MergeFailed, Some(detail), None)
		}
		Ok(WorkerOutput::Fatal { detail }) => (TerminalStatus::Fatal, Some(detail), None),
		Err(err) => (
			TerminalStatus::Fatal,
			Some(format!("invalid worker output: {err}")),
			None,
		),
	}
}

fn archive_output(
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
			"Cohort: **{}** (scorer `{}`, oracle policy `{}`) · candidates: **{}** · scorable: **{}** · excluded: **{}**",
			report.cohort,
			report.scorer_version,
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

fn read_tail(path: &Path, max_bytes: usize) -> io::Result<String> {
	let bytes = fs::read(path)?;
	let start = bytes.len().saturating_sub(max_bytes);
	Ok(String::from_utf8_lossy(&bytes[start..]).trim().to_string())
}

#[cfg(unix)]
fn exit_was_signal(status: &ExitStatus) -> bool {
	use std::os::unix::process::ExitStatusExt;
	status.signal().is_some()
}

#[cfg(not(unix))]
fn exit_was_signal(_status: &ExitStatus) -> bool {
	false
}

#[cfg(test)]
mod tests {
	use super::*;

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
		append_unique(
			&paths.measurements,
			&MeasurementRecord::new(
				MeasurementIdentity {
					snapshot_id: proposed.snapshot_id.clone(),
					executable_hash: "exe".to_string(),
					scorer_version: SCORER_VERSION.to_string(),
					config_hash: "config".to_string(),
				},
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
			&MeasurementRecord::new(
				MeasurementIdentity {
					snapshot_id: excluded.snapshot_id.clone(),
					executable_hash: "excluded-exe".to_string(),
					scorer_version: SCORER_VERSION.to_string(),
					config_hash: "excluded-config".to_string(),
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
			executable_hash: None,
			config_hash: None,
			cohort: ReportCohort::Scorable,
			limit: 0,
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
		assert_eq!(json["executable_hash"], "exe");
		assert_eq!(json["config_hash"], "config");
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
	fn scorer_config_identity_changes_with_timeout() {
		assert_eq!(
			scorer_config_hash(Duration::from_secs(600), Some("eu4@1/base-a")),
			scorer_config_hash(Duration::from_secs(600), Some("eu4@1/base-a"))
		);
		assert_ne!(
			scorer_config_hash(Duration::from_secs(600), Some("eu4@1/base-a")),
			scorer_config_hash(Duration::from_secs(601), Some("eu4@1/base-a"))
		);
		assert_ne!(
			scorer_config_hash(Duration::from_secs(600), Some("eu4@1/base-a")),
			scorer_config_hash(Duration::from_secs(600), Some("eu4@1/base-b"))
		);
		assert_ne!(
			scorer_config_hash(Duration::from_secs(600), Some("eu4@1/base-a")),
			scorer_config_hash(Duration::from_secs(600), None)
		);
	}

	#[test]
	fn measurement_child_command_propagates_exact_base_snapshot_identity() {
		let command = measurement_child_command(
			Path::new("/tmp/foch-mq"),
			Path::new("/tmp/dataset"),
			"snapshot-1",
			Path::new("/tmp/output"),
			Path::new("/tmp/eu4"),
			"sha256:parent-snapshot",
		);
		let args: Vec<String> = command
			.get_args()
			.map(|arg| arg.to_string_lossy().into_owned())
			.collect();
		assert_eq!(
			args,
			[
				"--dataset-root",
				"/tmp/dataset",
				"measure-one",
				"--snapshot-id",
				"snapshot-1",
				"--output-dir",
				"/tmp/output",
				"--basegame-root",
				"/tmp/eu4",
				"--base-snapshot-identity",
				"sha256:parent-snapshot",
			]
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
		let measurement = MeasurementRecord::new(
			MeasurementIdentity {
				snapshot_id: first.snapshot_id.clone(),
				executable_hash: "exe".to_string(),
				scorer_version: SCORER_VERSION.to_string(),
				config_hash: "config".to_string(),
			},
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
			executable_hash: Some("exe"),
			config_hash: Some("config"),
			cohort: ReportCohort::AllCandidates,
			limit: 0,
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
	fn report_does_not_relabel_stale_scorer_measurements() {
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
			&MeasurementRecord::new(
				MeasurementIdentity {
					snapshot_id: snapshot.snapshot_id.clone(),
					executable_hash: "exe".to_string(),
					scorer_version: "0.9.0".to_string(),
					config_hash: "config".to_string(),
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

		let output = temp.path().join("report");
		report(&ReportOptions {
			dataset_root: &paths.root,
			output_dir: &output,
			executable_hash: Some("exe"),
			config_hash: Some("config"),
			cohort: ReportCohort::AllCandidates,
			limit: 0,
		})
		.unwrap();
		let json: serde_json::Value =
			serde_json::from_str(&fs::read_to_string(output.join("baseline.json")).unwrap())
				.unwrap();
		assert_eq!(json["scorer_version"], SCORER_VERSION);
		assert_eq!(json["baseline_complete"], false);
		assert_eq!(json["terminal_cases"], 0);
		assert_eq!(json["cases"][0]["status"], "missing");
	}
}
