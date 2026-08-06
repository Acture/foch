//! Explicit, repository-owned merge-quality maintenance workflows.
//!
//! Every state-changing test is ignored and additionally requires the matching
//! `FOCH_MERGE_QUALITY_MAINTENANCE_WORKFLOW` token. The Fish entrypoints under
//! `scripts/merge-quality/` provide both that token and an exact test filter.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use foch_merge_quality::archive;
use foch_merge_quality::common_probe::{
	COMMON_APPLICABILITY_UNIT_COUNT, CommonApplicabilityOptions, run_common_applicability_probe,
};
use foch_merge_quality::config::{self, DiscoveryOverrides, Eu4Discovery};
use foch_merge_quality::corpus::Corpus;
use foch_merge_quality::dataset::{
	DatasetPaths, EngineArtifactIdentity, FileResultRecord, MeasurementIdentityV2,
	MeasurementKernel, MeasurementRecord, MeasurementScope, SCORER_VERSION, TerminalStatus,
	read_jsonl,
};
use foch_merge_quality::lifecycle::{
	CollectOptions, DatasetExportProfile, ExportOptions, collect, executable_hash, export_dataset,
};
use foch_merge_quality::report::{
	MeasurementCohortRegistry, MeasurementCohortSelector, SelectedMeasurementCohort,
	committed_measurement_cohort_registry, measurement_cohort_descriptors,
	select_measurement_cohort,
};
use serde::Serialize;

const WORKFLOW_ENV: &str = "FOCH_MERGE_QUALITY_MAINTENANCE_WORKFLOW";
const STRUCTURED_ROLLOUT_CASES: usize = 23;

fn crate_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn corpus_path() -> PathBuf {
	crate_root().join("corpus.json")
}

fn dataset_root() -> PathBuf {
	crate_root().join("dataset")
}

fn maintenance_root(name: &str) -> PathBuf {
	dataset_root().join(".work/maintenance").join(name)
}

fn validate_workflow_invocation<S: AsRef<str>>(
	expected_workflow: &str,
	expected_test: &str,
	actual_workflow: Option<&str>,
	libtest_args: &[S],
) -> Result<(), String> {
	if actual_workflow != Some(expected_workflow) {
		return Err(format!(
			"refusing maintenance workflow: run scripts/merge-quality/{expected_workflow}.fish"
		));
	}
	if !libtest_args.iter().any(|arg| arg.as_ref() == "--ignored") {
		return Err("refusing maintenance workflow without libtest --ignored".to_string());
	}
	if !libtest_args.iter().any(|arg| arg.as_ref() == "--exact") {
		return Err("refusing maintenance workflow without libtest --exact".to_string());
	}
	if libtest_args.first().map(AsRef::as_ref) != Some(expected_test)
		|| libtest_args
			.iter()
			.skip(1)
			.any(|arg| !arg.as_ref().starts_with('-'))
	{
		return Err(format!(
			"refusing maintenance workflow without the exact `{expected_test}` test filter"
		));
	}
	Ok(())
}

fn require_workflow(expected_workflow: &str, expected_test: &str) -> Result<(), Box<dyn Error>> {
	let actual_workflow = std::env::var(WORKFLOW_ENV).ok();
	let libtest_args = std::env::args().skip(1).collect::<Vec<_>>();
	validate_workflow_invocation(
		expected_workflow,
		expected_test,
		actual_workflow.as_deref(),
		&libtest_args,
	)
	.map_err(Into::into)
}

#[test]
fn workflow_invocation_guard_accepts_exact_ignored_test() {
	let args = ["refresh_corpus", "--ignored", "--exact", "--nocapture"];
	assert_eq!(
		validate_workflow_invocation(
			"refresh-corpus",
			"refresh_corpus",
			Some("refresh-corpus"),
			&args,
		),
		Ok(())
	);
}

#[test]
fn workflow_invocation_guard_rejects_unguarded_or_ambiguous_invocations() {
	let missing_ignored = ["refresh_corpus", "--exact"];
	let missing_exact = ["refresh_corpus", "--ignored"];
	let wrong_filter = ["refresh_fixtures", "--ignored", "--exact"];
	let skip_value_only = ["--skip", "refresh_corpus", "--ignored", "--exact"];
	let extra_filter = ["refresh_corpus", "refresh_fixtures", "--ignored", "--exact"];
	for args in [
		missing_ignored.as_slice(),
		missing_exact.as_slice(),
		wrong_filter.as_slice(),
		skip_value_only.as_slice(),
		extra_filter.as_slice(),
	] {
		assert!(
			validate_workflow_invocation(
				"refresh-corpus",
				"refresh_corpus",
				Some("refresh-corpus"),
				args,
			)
			.is_err()
		);
	}
	let valid_args = ["refresh_corpus", "--ignored", "--exact"];
	assert!(
		validate_workflow_invocation("refresh-corpus", "refresh_corpus", None, &valid_args,)
			.is_err()
	);
}

fn require_fresh_output(path: &Path, label: &str) -> Result<(), Box<dyn Error>> {
	if path.exists() && fs::read_dir(path)?.next().is_some() {
		return Err(format!("{label} output already exists; archive it before retrying").into());
	}
	fs::create_dir_all(path)?;
	Ok(())
}

fn require_nonempty_file(path: &Path, description: &str) -> Result<(), Box<dyn Error>> {
	if !path.is_file() || fs::metadata(path)?.len() == 0 {
		return Err(format!("missing or empty {description}").into());
	}
	Ok(())
}

fn directory_contains_file(path: &Path) -> bool {
	path.is_dir()
		&& walkdir::WalkDir::new(path)
			.into_iter()
			.filter_map(Result::ok)
			.any(|entry| entry.file_type().is_file())
}

#[cfg(feature = "steam")]
fn command_on_path(name: &str) -> bool {
	std::env::var_os("PATH").is_some_and(|paths| {
		std::env::split_paths(&paths).any(|directory| directory.join(name).is_file())
	})
}

fn relative_files(root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
	let mut files = Vec::new();
	for entry in walkdir::WalkDir::new(root) {
		let entry = entry?;
		if entry.file_type().is_file() {
			files.push(entry.path().strip_prefix(root)?.to_path_buf());
		}
	}
	files.sort();
	Ok(files)
}

fn require_byte_identical_trees(first: &Path, second: &Path) -> Result<(), Box<dyn Error>> {
	let first_files = relative_files(first)?;
	let second_files = relative_files(second)?;
	if first_files.is_empty() || first_files != second_files {
		return Err("dataset exports contain different file sets".into());
	}
	for relative in first_files {
		if fs::read(first.join(&relative))? != fs::read(second.join(&relative))? {
			return Err(format!(
				"dataset export is not byte deterministic: {}",
				relative.display()
			)
			.into());
		}
	}
	Ok(())
}

fn discover_eu4() -> Result<Eu4Discovery, Box<dyn Error>> {
	config::discover_eu4(&DiscoveryOverrides::default()).map_err(Into::into)
}

fn primary_workshop(discovery: &Eu4Discovery) -> Result<&Path, Box<dyn Error>> {
	discovery
		.workshop
		.roots
		.first()
		.map(PathBuf::as_path)
		.ok_or_else(|| "no EU4 Workshop root discovered".into())
}

#[test]
#[ignore = "mutates the canonical local corpus dataset"]
fn refresh_corpus() -> Result<(), Box<dyn Error>> {
	require_workflow("refresh-corpus", "refresh_corpus")?;
	require_nonempty_file(&corpus_path(), "canonical corpus definition")?;
	let discovery = discover_eu4()?;
	let summary = collect(&CollectOptions {
		corpus: &corpus_path(),
		dataset_root: &dataset_root(),
		discovery: &discovery,
		limit: 0,
	})?;
	if summary.local_cases == 0
		|| summary.snapshots != summary.local_cases
		|| summary.unique_objects == 0
	{
		return Err(format!("invalid corpus refresh summary: {summary:?}").into());
	}
	for path in [
		dataset_root().join("dataset.json"),
		dataset_root().join("snapshots.jsonl"),
		dataset_root().join("object_records.jsonl"),
	] {
		require_nonempty_file(&path, "refreshed dataset artifact")?;
	}
	Ok(())
}

#[test]
#[ignore = "writes a deterministic local dataset export"]
fn export_dataset_metadata() -> Result<(), Box<dyn Error>> {
	require_workflow("export", "export_dataset_metadata")?;
	for path in [
		dataset_root().join("dataset.json"),
		dataset_root().join("measurements.jsonl"),
		dataset_root().join("file_results.jsonl"),
	] {
		require_nonempty_file(&path, "dataset export input")?;
	}
	let root = maintenance_root("export");
	require_fresh_output(&root, "dataset export")?;
	let first = root.join("metadata-a");
	let second = root.join("metadata-b");
	for output_dir in [&first, &second] {
		export_dataset(&ExportOptions {
			dataset_root: &dataset_root(),
			output_dir,
			profile: DatasetExportProfile::Metadata,
		})?;
	}
	for output in [&first, &second] {
		for relative in ["export.json", "checksums.txt"] {
			require_nonempty_file(&output.join(relative), "dataset export manifest")?;
		}
	}
	require_byte_identical_trees(&first, &second)?;
	Ok(())
}

#[test]
#[ignore = "reads the local Workshop and full EU4 installation"]
fn refresh_fixtures() -> Result<(), Box<dyn Error>> {
	require_workflow("refresh-fixtures", "refresh_fixtures")?;
	require_nonempty_file(&corpus_path(), "canonical corpus definition")?;
	let discovery = discover_eu4()?;
	let workshop = primary_workshop(&discovery)?;
	if !directory_contains_file(workshop) {
		return Err("discovered Workshop root contains no files".into());
	}
	let output = maintenance_root("fixture-refresh");
	require_fresh_output(&output, "fixture refresh")?;
	let scratch_root = dataset_root().join(".work");
	fs::create_dir_all(&scratch_root)?;
	let staging = tempfile::tempdir_in(&scratch_root)?;
	foch_merge_quality::fixtures::extract(
		&corpus_path(),
		workshop,
		&discovery.game_root,
		staging.path(),
		&[],
	)?;
	let extracted_corpus_path = staging.path().join("corpus.json");
	require_nonempty_file(&extracted_corpus_path, "extracted fixture corpus")?;
	let extracted_corpus = Corpus::from_json(&fs::read_to_string(&extracted_corpus_path)?)?;
	if extracted_corpus.cases.is_empty()
		|| !directory_contains_file(&staging.path().join("workshop"))
		|| !directory_contains_file(&staging.path().join("basegame"))
	{
		return Err("fixture extraction produced no complete local cases".into());
	}
	let base_manifest_path = staging.path().join("basegame-manifest.json");
	require_nonempty_file(&base_manifest_path, "base-game fixture manifest")?;
	let base_manifest: serde_json::Value = serde_json::from_slice(&fs::read(&base_manifest_path)?)?;
	let valid_base_manifest = base_manifest
		.get("file_count")
		.and_then(serde_json::Value::as_u64)
		.is_some_and(|count| count > 0)
		&& base_manifest
			.get("content_bytes")
			.and_then(serde_json::Value::as_u64)
			.is_some_and(|bytes| bytes > 0)
		&& base_manifest
			.get("game_version")
			.and_then(serde_json::Value::as_str)
			== Some(discovery.game_version.as_str())
		&& base_manifest
			.get("content_hash")
			.and_then(serde_json::Value::as_str)
			.is_some_and(|hash| hash.len() == 64);
	if !valid_base_manifest {
		return Err("fixture extraction produced an invalid base-game manifest".into());
	}
	let base_staging = tempfile::tempdir_in(&scratch_root)?;
	fs::rename(
		staging.path().join("basegame"),
		base_staging.path().join("basegame"),
	)?;
	fs::rename(
		staging.path().join("basegame-manifest.json"),
		base_staging.path().join("basegame-manifest.json"),
	)?;
	let corpus_archive = output.join("corpus.tar.gz");
	let base_archive = output.join("basegame-text.tar.gz");
	archive::pack_dir(staging.path(), &corpus_archive)?;
	archive::pack_dir(base_staging.path(), &base_archive)?;
	fs::copy(
		base_staging.path().join("basegame-manifest.json"),
		output.join("basegame-manifest.json"),
	)?;

	let corpus_check = tempfile::tempdir_in(&scratch_root)?;
	let base_check = tempfile::tempdir_in(&scratch_root)?;
	archive::unpack(&corpus_archive, corpus_check.path())?;
	archive::unpack(&base_archive, base_check.path())?;
	if !corpus_check.path().join("corpus.json").is_file()
		|| !directory_contains_file(&corpus_check.path().join("workshop"))
		|| !base_check.path().join("basegame-manifest.json").is_file()
		|| !directory_contains_file(&base_check.path().join("basegame"))
		|| fs::metadata(&corpus_archive)?.len() == 0
		|| fs::metadata(&base_archive)?.len() == 0
	{
		return Err("fixture refresh failed archive validation".into());
	}
	Ok(())
}

#[test]
#[ignore = "scans the full local Workshop corpus"]
fn symbol_evidence() -> Result<(), Box<dyn Error>> {
	require_workflow("symbol-evidence", "symbol_evidence")?;
	require_nonempty_file(&corpus_path(), "canonical corpus definition")?;
	let discovery = discover_eu4()?;
	let workshop = primary_workshop(&discovery)?;
	if !directory_contains_file(workshop) {
		return Err("discovered Workshop root contains no files".into());
	}
	let output = maintenance_root("symbol-evidence");
	require_fresh_output(&output, "symbol evidence")?;
	foch_merge_quality::symbols::run(&corpus_path(), workshop, &output, 0)?;
	let report: serde_json::Value =
		serde_json::from_slice(&fs::read(output.join("symbols.json"))?)?;
	let cases_seen = report
		.pointer("/totals/cases_seen")
		.and_then(serde_json::Value::as_u64)
		.ok_or("symbol evidence is missing cases_seen")?;
	let report_cases = report
		.get("cases")
		.and_then(serde_json::Value::as_array)
		.ok_or("symbol evidence is missing cases")?;
	if cases_seen == 0 || cases_seen as usize != report_cases.len() {
		return Err("symbol evidence has an invalid case denominator".into());
	}
	require_nonempty_file(&output.join("symbols.md"), "rendered symbol report")?;
	Ok(())
}

#[test]
#[ignore = "requires the private corpus CAS and installed EU4 snapshot"]
fn common_module_acceptance() -> Result<(), Box<dyn Error>> {
	require_workflow("common-module", "common_module_acceptance")?;
	for path in [
		dataset_root().join("dataset.json"),
		dataset_root().join("snapshots.jsonl"),
		dataset_root().join("measurements.jsonl"),
		dataset_root().join("file_results.jsonl"),
		crate_root().join("tests/fixtures/legacy-baseline.json"),
	] {
		require_nonempty_file(&path, "Common applicability input")?;
	}
	let discovery = discover_eu4()?;
	let output = maintenance_root("common-module");
	require_fresh_output(&output, "Common module acceptance")?;
	let evaluator_artifact = executable_hash(&std::env::current_exe()?)?;
	let report = run_common_applicability_probe(&CommonApplicabilityOptions {
		dataset_root: &dataset_root(),
		output_dir: &output,
		legacy_baseline: &crate_root().join("tests/fixtures/legacy-baseline.json"),
		game: &config::Eu4GameDiscovery {
			game_root: discovery.game_root,
			game_version: discovery.game_version,
			steam_build_id: discovery.steam_build_id,
			steam_root: discovery.steam_root,
		},
		evaluator_artifact_blake3: &evaluator_artifact,
		case_ids: &BTreeSet::new(),
		families: &BTreeSet::new(),
	})?;
	if report.summary.expected_units != COMMON_APPLICABILITY_UNIT_COUNT
		|| report.summary.classified_units != COMMON_APPLICABILITY_UNIT_COUNT
		|| !report.summary.full_denominator
		|| report.summary.failed != 0
		|| report.summary.gate_passed != Some(true)
	{
		return Err(format!("Common acceptance gate failed: {:?}", report.summary).into());
	}
	Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScoredUnit {
	accepted: bool,
	verdict: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct RolloutDelta {
	snapshot_id: String,
	relative_path: String,
	legacy_verdict: String,
	product_verdict: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct RolloutReport {
	schema: &'static str,
	legacy_cohort_id: String,
	product_cohort_id: String,
	terminal_cases: usize,
	units: usize,
	verdict_changes: usize,
	improvements: Vec<RolloutDelta>,
	regressions: Vec<RolloutDelta>,
}

fn cohort_is_complete(measurements: &[&MeasurementRecord]) -> bool {
	let snapshot_ids = measurements
		.iter()
		.map(|record| record.snapshot_id())
		.collect::<BTreeSet<_>>();
	measurements.len() == STRUCTURED_ROLLOUT_CASES
		&& snapshot_ids.len() == STRUCTURED_ROLLOUT_CASES
		&& measurements
			.iter()
			.all(|record| record.status() == TerminalStatus::Completed)
}

fn select_complete_product_cohort<'a>(
	measurements: &'a [MeasurementRecord],
	registry: &MeasurementCohortRegistry,
) -> Result<SelectedMeasurementCohort<'a>, Box<dyn Error>> {
	let descriptors = measurement_cohort_descriptors(measurements, registry)?;
	let mut complete = Vec::new();
	let mut candidate_summaries = Vec::new();
	for descriptor in descriptors.into_iter().filter(|descriptor| {
		descriptor.scorer_version == SCORER_VERSION
			&& descriptor.merge_kernel == Some(MeasurementKernel::SemanticTree)
			&& descriptor.identity.scope() == Some(MeasurementScope::FullProductMerge)
	}) {
		let cohort = select_measurement_cohort(
			measurements,
			registry,
			MeasurementCohortSelector::CohortId(&descriptor.cohort_id),
		)?;
		let completed = cohort
			.measurements
			.iter()
			.filter(|record| record.status() == TerminalStatus::Completed)
			.count();
		let snapshots = cohort
			.measurements
			.iter()
			.map(|record| record.snapshot_id())
			.collect::<BTreeSet<_>>()
			.len();
		candidate_summaries.push(format!(
			"{}={completed}/{} Completed across {snapshots} snapshots",
			descriptor.cohort_id,
			cohort.measurements.len()
		));
		if cohort_is_complete(&cohort.measurements) {
			complete.push(cohort);
		}
	}
	if complete.len() != 1 {
		return Err(format!(
			"expected one complete {STRUCTURED_ROLLOUT_CASES}-case product cohort, found {}; candidates: {}",
			complete.len(),
			candidate_summaries.join(", ")
		)
		.into());
	}
	Ok(complete.pop().expect("exactly one complete cohort"))
}

#[test]
#[ignore = "requires a complete V2 product cohort"]
fn structured_rollout_acceptance() -> Result<(), Box<dyn Error>> {
	require_workflow("structured-rollout", "structured_rollout_acceptance")?;
	let paths = DatasetPaths::new(dataset_root());
	for path in [&paths.measurements, &paths.file_results] {
		require_nonempty_file(path, "structured rollout input")?;
	}
	let output = maintenance_root("structured-rollout");
	require_fresh_output(&output, "structured rollout")?;
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements)?;
	let registry = committed_measurement_cohort_registry()?;
	let legacy = select_measurement_cohort(
		&measurements,
		&registry,
		MeasurementCohortSelector::ScorerVersion("1.3.0"),
	)?;
	if !cohort_is_complete(&legacy.measurements) {
		return Err(format!(
			"Legacy scorer 1.3 cohort must contain {STRUCTURED_ROLLOUT_CASES} unique Completed snapshots"
		)
		.into());
	}
	let product = select_complete_product_cohort(&measurements, &registry)?;

	let file_results = read_jsonl::<FileResultRecord>(&paths.file_results)?;
	let legacy_units = scored_units(&file_results, &legacy.measurements)?;
	let product_units = scored_units(&file_results, &product.measurements)?;
	if legacy_units.is_empty() {
		return Err("Legacy scorer 1.3 cohort contains no file-result units".into());
	}
	let legacy_unit_keys = legacy_units.keys().cloned().collect::<BTreeSet<_>>();
	let product_unit_keys = product_units.keys().cloned().collect::<BTreeSet<_>>();
	if legacy_unit_keys != product_unit_keys {
		let missing = legacy_unit_keys.difference(&product_unit_keys).count();
		let unexpected = product_unit_keys.difference(&legacy_unit_keys).count();
		return Err(format!(
			"product file-result unit set differs from Legacy scorer 1.3: {missing} missing, {unexpected} unexpected"
		)
		.into());
	}
	let mut improvements = Vec::new();
	let mut regressions = Vec::new();
	let mut verdict_changes = 0_usize;
	for ((snapshot_id, relative_path), legacy_score) in &legacy_units {
		let product_score = product_units
			.get(&(snapshot_id.clone(), relative_path.clone()))
			.expect("unit sets were checked");
		if legacy_score.verdict != product_score.verdict {
			verdict_changes += 1;
		}
		let delta = RolloutDelta {
			snapshot_id: snapshot_id.clone(),
			relative_path: relative_path.clone(),
			legacy_verdict: legacy_score.verdict.clone(),
			product_verdict: product_score.verdict.clone(),
		};
		match (legacy_score.accepted, product_score.accepted) {
			(false, true) => improvements.push(delta),
			(true, false) => regressions.push(delta),
			_ => {}
		}
	}
	let report = RolloutReport {
		schema: "1.0.0",
		legacy_cohort_id: legacy.descriptor.cohort_id,
		product_cohort_id: product.descriptor.cohort_id,
		terminal_cases: product.measurements.len(),
		units: product_units.len(),
		verdict_changes,
		improvements,
		regressions,
	};
	fs::write(
		output.join("structured-rollout.json"),
		format!("{}\n", serde_json::to_string_pretty(&report)?),
	)?;
	if !report.regressions.is_empty() {
		return Err(format!(
			"Structured product rollout lost {} Legacy-accepted units",
			report.regressions.len()
		)
		.into());
	}
	Ok(())
}

fn scored_units(
	file_results: &[FileResultRecord],
	measurements: &[&MeasurementRecord],
) -> Result<BTreeMap<(String, String), ScoredUnit>, Box<dyn Error>> {
	let snapshots_by_measurement = measurements
		.iter()
		.map(|record| {
			(
				record.measurement_id().to_string(),
				record.snapshot_id().to_string(),
			)
		})
		.collect::<BTreeMap<_, _>>();
	let mut units = BTreeMap::new();
	for file_result in file_results {
		let Some(snapshot_id) = snapshots_by_measurement.get(&file_result.measurement_id) else {
			continue;
		};
		let score = file_result
			.result
			.get("score")
			.ok_or("file result is missing score payload")?;
		let accepted = score
			.get("accepted_ok")
			.and_then(serde_json::Value::as_bool)
			.ok_or("file result is missing accepted_ok")?;
		let verdict = score
			.get("verdict")
			.and_then(serde_json::Value::as_str)
			.ok_or("file result is missing verdict")?
			.to_string();
		let key = (snapshot_id.clone(), file_result.relative_path.clone());
		if units
			.insert(key, ScoredUnit { accepted, verdict })
			.is_some()
		{
			return Err("duplicate scoring unit in cohort".into());
		}
	}
	Ok(units)
}

fn synthetic_product_cohort(
	artifact_hash: &str,
	case_count: usize,
	last_status: TerminalStatus,
) -> Vec<MeasurementRecord> {
	(0..case_count)
		.map(|index| {
			let status = if index + 1 == case_count {
				last_status
			} else {
				TerminalStatus::Completed
			};
			MeasurementRecord::new_v2(
				MeasurementIdentityV2 {
					snapshot_id: format!("snapshot-{index:02}"),
					engine_artifact: EngineArtifactIdentity::foch_executable_blake3(artifact_hash),
					runner_protocol_version: "1.0.0".to_string(),
					merge_kernel: MeasurementKernel::SemanticTree,
					scope: MeasurementScope::FullProductMerge,
					scorer_version: SCORER_VERSION.to_string(),
					scorer_config_hash: "fixed-product-config".to_string(),
				},
				"started".to_string(),
				"finished".to_string(),
				status,
				None,
				None,
				None,
			)
		})
		.collect()
}

#[test]
fn product_cohort_selection_ignores_partial_and_failed_artifacts() {
	let complete_hash = "a".repeat(64);
	let mut measurements = synthetic_product_cohort(
		&complete_hash,
		STRUCTURED_ROLLOUT_CASES,
		TerminalStatus::Completed,
	);
	let complete_cohort_id = measurements[0].cohort_id();
	measurements.extend(synthetic_product_cohort(
		&"b".repeat(64),
		STRUCTURED_ROLLOUT_CASES - 1,
		TerminalStatus::Completed,
	));
	measurements.extend(synthetic_product_cohort(
		&"c".repeat(64),
		STRUCTURED_ROLLOUT_CASES,
		TerminalStatus::Fatal,
	));

	let selected = select_complete_product_cohort(
		&measurements,
		&committed_measurement_cohort_registry().unwrap(),
	)
	.unwrap();

	assert_eq!(selected.descriptor.cohort_id, complete_cohort_id);
}

#[cfg(feature = "steam")]
#[test]
#[ignore = "performs Steam API discovery and SteamCMD acquisition"]
fn acquire_workshop_corpus() -> Result<(), Box<dyn Error>> {
	require_workflow("acquire", "acquire_workshop_corpus")?;
	if foch_merge_quality::secrets::steam_api_key().is_none() {
		return Err("Steam API key is not configured".into());
	}
	if foch_merge_quality::secrets::steam_username().is_none() {
		return Err("Steam username is not configured".into());
	}
	if !command_on_path("steamcmd") {
		return Err("steamcmd is not available on PATH".into());
	}
	let discovery = discover_eu4()?;
	let workshop = primary_workshop(&discovery)?;
	if !workshop.is_dir() {
		return Err("discovered Workshop root does not exist".into());
	}
	let output = maintenance_root("acquisition");
	require_fresh_output(&output, "Steam acquisition")?;
	let discovered_corpus = output.join("corpus.json");
	foch_merge_quality::steam::discover(&discovered_corpus, 300)?;
	require_nonempty_file(&discovered_corpus, "discovered Workshop corpus")?;
	let corpus_bytes = fs::read(&discovered_corpus)?;
	let plan = foch_merge_quality::fetch::plan_acquisition(&corpus_bytes, 15, 100)?;
	let outcome = foch_merge_quality::fetch::fetch(&plan, workshop)?;
	let manifest = foch_merge_quality::fetch::write_acquisition_integrity(
		&plan,
		&outcome,
		&discovered_corpus,
		workshop,
		&output,
	)?;
	let verified = foch_merge_quality::fetch::verify_acquisition_integrity(
		&plan,
		&outcome,
		&discovered_corpus,
		workshop,
		&output,
	)?;
	if verified != manifest {
		return Err("full Steam acquisition verification returned a different manifest".into());
	}
	require_nonempty_file(
		&output.join(foch_merge_quality::fetch::ACQUISITION_MANIFEST_FILE),
		"Steam acquisition manifest",
	)?;
	require_nonempty_file(
		&output.join(foch_merge_quality::fetch::ACQUISITION_CHECKSUMS_FILE),
		"Steam acquisition checksums",
	)?;
	eprintln!(
		"Steam acquisition integrity attested: {} cases, {} Workshop items; \
		 {} existing-local inputs, {} SteamCMD-confirmed downloads",
		manifest.selection.selected_case_ids.len(),
		manifest.workshop_items.len(),
		outcome.already_local_count(),
		outcome.downloaded_count(),
	);
	Ok(())
}
