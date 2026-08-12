//! Explicit, repository-owned merge-quality maintenance workflows.
//!
//! Every state-changing test is ignored and additionally requires the matching
//! `FOCH_MERGE_QUALITY_MAINTENANCE_WORKFLOW` token. The Fish entrypoints under
//! `scripts/merge-quality/` provide both that token and an exact test filter.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use foch_merge_quality::common_probe::{
	COMMON_APPLICABILITY_UNIT_COUNT, CommonApplicabilityOptions, run_common_applicability_probe,
};
use foch_merge_quality::config::{self, DiscoveryOverrides, Eu4Discovery};
use foch_merge_quality::dataset::{
	DatasetPaths, EngineArtifactIdentity, FileResultRecord, InputVersionRecord,
	MeasurementIdentityV2, MeasurementKernel, MeasurementRecord, MeasurementScope, SCORER_VERSION,
	SnapshotRecord, TerminalStatus, read_jsonl,
};
use foch_merge_quality::lifecycle::{
	MetadataExportOptions, executable_hash, export_dataset_metadata as write_metadata_export,
};
use foch_merge_quality::report::{
	MeasurementCohortRegistry, MeasurementCohortSelector, committed_measurement_cohort_registry,
	measurement_cohort_descriptors, select_measurement_cohort,
};
use foch_merge_quality::workshop_inputs::WorkshopCaseManifest;
use serde::Serialize;

const WORKFLOW_ENV: &str = "FOCH_MERGE_QUALITY_MAINTENANCE_WORKFLOW";

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
	dataset_root().join(".maintenance-work").join(name)
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
	let args = [
		"export_dataset_metadata",
		"--ignored",
		"--exact",
		"--nocapture",
	];
	assert_eq!(
		validate_workflow_invocation("export", "export_dataset_metadata", Some("export"), &args,),
		Ok(())
	);
}

#[test]
fn workflow_invocation_guard_rejects_unguarded_or_ambiguous_invocations() {
	let missing_ignored = ["export_dataset_metadata", "--exact"];
	let missing_exact = ["export_dataset_metadata", "--ignored"];
	let wrong_filter = ["symbol_evidence", "--ignored", "--exact"];
	let skip_value_only = ["--skip", "export_dataset_metadata", "--ignored", "--exact"];
	let extra_filter = [
		"export_dataset_metadata",
		"symbol_evidence",
		"--ignored",
		"--exact",
	];
	for args in [
		missing_ignored.as_slice(),
		missing_exact.as_slice(),
		wrong_filter.as_slice(),
		skip_value_only.as_slice(),
		extra_filter.as_slice(),
	] {
		assert!(
			validate_workflow_invocation(
				"export",
				"export_dataset_metadata",
				Some("export"),
				args,
			)
			.is_err()
		);
	}
	let valid_args = ["export_dataset_metadata", "--ignored", "--exact"];
	assert!(
		validate_workflow_invocation("export", "export_dataset_metadata", None, &valid_args,)
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
		.content_roots()
		.into_iter()
		.next()
		.ok_or_else(|| "no EU4 Workshop root discovered".into())
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
		write_metadata_export(&MetadataExportOptions {
			dataset_root: &dataset_root(),
			output_dir,
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
#[ignore = "reads the read-only Workshop/ACF inputs and installed EU4"]
fn common_module_acceptance() -> Result<(), Box<dyn Error>> {
	require_workflow("common-module", "common_module_acceptance")?;
	for path in [
		crate_root().join("tests/fixtures/workshop-product-cases-v2.json"),
		crate_root().join("tests/fixtures/legacy-baseline.json"),
	] {
		require_nonempty_file(&path, "Common applicability input")?;
	}
	let discovery = discover_eu4()?;
	let output = maintenance_root("common-module");
	require_fresh_output(&output, "Common module acceptance")?;
	let evaluator_artifact = executable_hash(&std::env::current_exe()?)?;
	let report = run_common_applicability_probe(&CommonApplicabilityOptions {
		case_manifest: &crate_root().join("tests/fixtures/workshop-product-cases-v2.json"),
		output_dir: &output,
		legacy_baseline: &crate_root().join("tests/fixtures/legacy-baseline.json"),
		discovery: &discovery,
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
	case_id: String,
	legacy_snapshot_id: String,
	product_input_version_id: String,
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

#[derive(Clone, Debug)]
struct CanonicalMeasurement<'a> {
	case_id: String,
	measurement: &'a MeasurementRecord,
	legacy_snapshot_id: Option<String>,
	product_input_version_id: Option<String>,
}

#[derive(Clone, Debug)]
struct CanonicalMeasurementCohort<'a> {
	descriptor: foch_merge_quality::report::MeasurementCohortDescriptor,
	measurements: Vec<CanonicalMeasurement<'a>>,
}

fn workshop_case_manifest_path() -> PathBuf {
	crate_root().join("tests/fixtures/workshop-product-cases-v2.json")
}

fn canonical_rollout_case_ids() -> Result<BTreeSet<String>, Box<dyn Error>> {
	let manifest = WorkshopCaseManifest::from_path(&workshop_case_manifest_path())?;
	Ok(manifest
		.cases
		.into_iter()
		.map(|case| case.case_id)
		.collect())
}

fn legacy_snapshot_case_index(
	snapshots: &[SnapshotRecord],
) -> Result<BTreeMap<String, String>, String> {
	let mut cases = BTreeMap::new();
	for snapshot in snapshots {
		if !snapshot.identity_is_valid() {
			return Err(format!(
				"legacy snapshot {} has an invalid identity",
				snapshot.snapshot_id
			));
		}
		if cases
			.insert(snapshot.snapshot_id.clone(), snapshot.case_id.clone())
			.is_some()
		{
			return Err(format!(
				"duplicate legacy snapshot identity {}",
				snapshot.snapshot_id
			));
		}
	}
	Ok(cases)
}

fn product_input_case_index(
	input_versions: &[InputVersionRecord],
) -> Result<BTreeMap<String, String>, String> {
	let mut cases = BTreeMap::new();
	for input in input_versions {
		if !input.identity_is_valid() {
			return Err(format!(
				"Workshop input version {} has an invalid identity",
				input.input_version_id
			));
		}
		if cases
			.insert(input.input_version_id.clone(), input.case_id.clone())
			.is_some()
		{
			return Err(format!(
				"duplicate Workshop input version identity {}",
				input.input_version_id
			));
		}
	}
	Ok(cases)
}

fn validate_canonical_measurements<'a>(
	measurements: Vec<CanonicalMeasurement<'a>>,
	canonical_case_ids: &BTreeSet<String>,
	label: &str,
	ignore_noncanonical: bool,
) -> Result<Vec<CanonicalMeasurement<'a>>, String> {
	let mut by_case = BTreeMap::new();
	for measurement in measurements {
		if !canonical_case_ids.contains(&measurement.case_id) {
			if ignore_noncanonical {
				continue;
			}
			return Err(format!(
				"{label} contains unexpected case {}",
				measurement.case_id
			));
		}
		if measurement.measurement.status() != TerminalStatus::Completed {
			return Err(format!(
				"{label} case {} is {:?}, expected Completed",
				measurement.case_id,
				measurement.measurement.status()
			));
		}
		let case_id = measurement.case_id.clone();
		if by_case.insert(case_id.clone(), measurement).is_some() {
			return Err(format!("{label} contains duplicate case {case_id}"));
		}
	}

	let actual_case_ids = by_case.keys().cloned().collect::<BTreeSet<_>>();
	if actual_case_ids != *canonical_case_ids {
		let missing = canonical_case_ids
			.difference(&actual_case_ids)
			.cloned()
			.collect::<Vec<_>>();
		let unexpected = actual_case_ids
			.difference(canonical_case_ids)
			.cloned()
			.collect::<Vec<_>>();
		return Err(format!(
			"{label} does not match the canonical Workshop cohort: missing [{}], unexpected [{}]",
			missing.join(", "),
			unexpected.join(", ")
		));
	}
	Ok(by_case.into_values().collect())
}

fn resolve_legacy_cohort<'a>(
	measurements: &[&'a MeasurementRecord],
	canonical_case_ids: &BTreeSet<String>,
	snapshot_cases: &BTreeMap<String, String>,
) -> Result<Vec<CanonicalMeasurement<'a>>, String> {
	let resolved = measurements
		.iter()
		.map(|measurement| {
			let snapshot_id = measurement.legacy_snapshot_id().ok_or_else(|| {
				format!(
					"legacy cohort contains V2 measurement {}",
					measurement.measurement_id()
				)
			})?;
			let case_id = snapshot_cases.get(snapshot_id).ok_or_else(|| {
				format!("legacy snapshot {snapshot_id} is missing from snapshots.jsonl")
			})?;
			Ok(CanonicalMeasurement {
				case_id: case_id.clone(),
				measurement,
				legacy_snapshot_id: Some(snapshot_id.to_string()),
				product_input_version_id: None,
			})
		})
		.collect::<Result<Vec<_>, String>>()?;
	// Scorer 1.3 is a larger frozen historical cohort. Project it onto the
	// current fixed Workshop denominator, then require that projection exactly.
	validate_canonical_measurements(resolved, canonical_case_ids, "Legacy scorer 1.3", true)
}

fn resolve_product_cohort<'a>(
	measurements: &[&'a MeasurementRecord],
	canonical_case_ids: &BTreeSet<String>,
	input_cases: &BTreeMap<String, String>,
) -> Result<Vec<CanonicalMeasurement<'a>>, String> {
	let resolved = measurements
		.iter()
		.map(|measurement| {
			let input_version_id = measurement.input_version_id().ok_or_else(|| {
				format!(
					"product cohort contains V1 measurement {}",
					measurement.measurement_id()
				)
			})?;
			let case_id = input_cases.get(input_version_id).ok_or_else(|| {
				format!(
					"Workshop input version {input_version_id} is missing from input_versions.jsonl"
				)
			})?;
			Ok(CanonicalMeasurement {
				case_id: case_id.clone(),
				measurement,
				legacy_snapshot_id: None,
				product_input_version_id: Some(input_version_id.to_string()),
			})
		})
		.collect::<Result<Vec<_>, String>>()?;
	validate_canonical_measurements(resolved, canonical_case_ids, "product cohort", false)
}

fn select_complete_product_cohort<'a>(
	measurements: &'a [MeasurementRecord],
	registry: &MeasurementCohortRegistry,
	canonical_case_ids: &BTreeSet<String>,
	input_cases: &BTreeMap<String, String>,
) -> Result<CanonicalMeasurementCohort<'a>, Box<dyn Error>> {
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
		match resolve_product_cohort(&cohort.measurements, canonical_case_ids, input_cases) {
			Ok(measurements) => complete.push(CanonicalMeasurementCohort {
				descriptor: cohort.descriptor,
				measurements,
			}),
			Err(error) => candidate_summaries.push(format!(
				"{}={completed}/{} Completed; {error}",
				descriptor.cohort_id,
				cohort.measurements.len()
			)),
		}
	}
	if complete.len() != 1 {
		return Err(format!(
			"expected one complete {}-case product cohort, found {}; candidates: {}",
			canonical_case_ids.len(),
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
	for path in [
		&paths.snapshots,
		&paths.input_versions,
		&paths.measurements,
		&paths.file_results,
	] {
		require_nonempty_file(path, "structured rollout input")?;
	}
	let output = maintenance_root("structured-rollout");
	require_fresh_output(&output, "structured rollout")?;
	let canonical_case_ids = canonical_rollout_case_ids()?;
	let snapshots = read_jsonl::<SnapshotRecord>(&paths.snapshots)?;
	let snapshot_cases = legacy_snapshot_case_index(&snapshots)?;
	let input_versions = read_jsonl::<InputVersionRecord>(&paths.input_versions)?;
	let input_cases = product_input_case_index(&input_versions)?;
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements)?;
	let registry = committed_measurement_cohort_registry()?;
	let selected_legacy = select_measurement_cohort(
		&measurements,
		&registry,
		MeasurementCohortSelector::ScorerVersion("1.3.0"),
	)?;
	let legacy = CanonicalMeasurementCohort {
		descriptor: selected_legacy.descriptor,
		measurements: resolve_legacy_cohort(
			&selected_legacy.measurements,
			&canonical_case_ids,
			&snapshot_cases,
		)?,
	};
	let product = select_complete_product_cohort(
		&measurements,
		&registry,
		&canonical_case_ids,
		&input_cases,
	)?;

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
	let legacy_measurements = legacy
		.measurements
		.iter()
		.map(|measurement| (measurement.case_id.as_str(), measurement))
		.collect::<BTreeMap<_, _>>();
	let product_measurements = product
		.measurements
		.iter()
		.map(|measurement| (measurement.case_id.as_str(), measurement))
		.collect::<BTreeMap<_, _>>();
	for ((case_id, relative_path), legacy_score) in &legacy_units {
		let product_score = product_units
			.get(&(case_id.clone(), relative_path.clone()))
			.expect("unit sets were checked");
		if legacy_score.verdict != product_score.verdict {
			verdict_changes += 1;
		}
		let legacy_measurement = legacy_measurements
			.get(case_id.as_str())
			.expect("legacy cohort was validated by case ID");
		let product_measurement = product_measurements
			.get(case_id.as_str())
			.expect("product cohort was validated by case ID");
		let delta = RolloutDelta {
			case_id: case_id.clone(),
			legacy_snapshot_id: legacy_measurement
				.legacy_snapshot_id
				.clone()
				.expect("legacy cohort carries snapshot identities"),
			product_input_version_id: product_measurement
				.product_input_version_id
				.clone()
				.expect("product cohort carries input version identities"),
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
		schema: "2.0.0",
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
	measurements: &[CanonicalMeasurement<'_>],
) -> Result<BTreeMap<(String, String), ScoredUnit>, Box<dyn Error>> {
	let cases_by_measurement = measurements
		.iter()
		.map(|resolved| {
			(
				resolved.measurement.measurement_id().to_string(),
				resolved.case_id.clone(),
			)
		})
		.collect::<BTreeMap<_, _>>();
	let mut units = BTreeMap::new();
	for file_result in file_results {
		let Some(case_id) = cases_by_measurement.get(&file_result.measurement_id) else {
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
		let key = (case_id.clone(), file_result.relative_path.clone());
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
	inputs: &[(String, String)],
	last_status: TerminalStatus,
) -> Vec<MeasurementRecord> {
	inputs
		.iter()
		.enumerate()
		.map(|(index, (input_version_id, _))| {
			let status = if index + 1 == inputs.len() {
				last_status
			} else {
				TerminalStatus::Completed
			};
			MeasurementRecord::new_v2(
				MeasurementIdentityV2 {
					input_version_id: input_version_id.clone(),
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

fn synthetic_product_inputs(canonical_case_ids: &BTreeSet<String>) -> Vec<(String, String)> {
	canonical_case_ids
		.iter()
		.enumerate()
		.map(|(index, case_id)| (format!("{:064x}", index + 1), case_id.clone()))
		.collect()
}

fn synthetic_input_case_index(inputs: &[(String, String)]) -> BTreeMap<String, String> {
	inputs.iter().cloned().collect()
}

#[test]
fn canonical_rollout_manifest_contains_14_cases() {
	assert_eq!(canonical_rollout_case_ids().unwrap().len(), 14);
}

#[test]
fn product_cohort_selection_ignores_partial_and_failed_artifacts() {
	let canonical_case_ids = canonical_rollout_case_ids().unwrap();
	let inputs = synthetic_product_inputs(&canonical_case_ids);
	let input_cases = synthetic_input_case_index(&inputs);
	let complete_hash = "a".repeat(64);
	let mut measurements =
		synthetic_product_cohort(&complete_hash, &inputs, TerminalStatus::Completed);
	let complete_cohort_id = measurements[0].cohort_id();
	measurements.extend(synthetic_product_cohort(
		&"b".repeat(64),
		&inputs[..inputs.len() - 1],
		TerminalStatus::Completed,
	));
	measurements.extend(synthetic_product_cohort(
		&"c".repeat(64),
		&inputs,
		TerminalStatus::Fatal,
	));

	let selected = select_complete_product_cohort(
		&measurements,
		&committed_measurement_cohort_registry().unwrap(),
		&canonical_case_ids,
		&input_cases,
	)
	.unwrap();

	assert_eq!(selected.descriptor.cohort_id, complete_cohort_id);
	assert_eq!(selected.measurements.len(), canonical_case_ids.len());
}

#[test]
fn canonical_product_cohort_rejects_missing_extra_duplicate_and_failed_cases() {
	let canonical_case_ids = canonical_rollout_case_ids().unwrap();
	let inputs = synthetic_product_inputs(&canonical_case_ids);
	let input_cases = synthetic_input_case_index(&inputs);
	let complete = synthetic_product_cohort(&"a".repeat(64), &inputs, TerminalStatus::Completed);
	let complete_refs = complete.iter().collect::<Vec<_>>();
	let resolved =
		resolve_product_cohort(&complete_refs, &canonical_case_ids, &input_cases).unwrap();
	assert_eq!(resolved.len(), canonical_case_ids.len());

	let missing = synthetic_product_cohort(
		&"b".repeat(64),
		&inputs[..inputs.len() - 1],
		TerminalStatus::Completed,
	);
	let missing_refs = missing.iter().collect::<Vec<_>>();
	assert!(
		resolve_product_cohort(&missing_refs, &canonical_case_ids, &input_cases)
			.unwrap_err()
			.contains("missing")
	);

	let mut extra_inputs = inputs.clone();
	extra_inputs.push(("e".repeat(64), "9999999999".to_string()));
	let extra_input_cases = synthetic_input_case_index(&extra_inputs);
	let extra = synthetic_product_cohort(&"c".repeat(64), &extra_inputs, TerminalStatus::Completed);
	let extra_refs = extra.iter().collect::<Vec<_>>();
	assert!(
		resolve_product_cohort(&extra_refs, &canonical_case_ids, &extra_input_cases)
			.unwrap_err()
			.contains("unexpected case")
	);

	let mut duplicate_inputs = inputs.clone();
	duplicate_inputs.push(("f".repeat(64), inputs.first().unwrap().1.clone()));
	let duplicate_input_cases = synthetic_input_case_index(&duplicate_inputs);
	let duplicate = synthetic_product_cohort(
		&"d".repeat(64),
		&duplicate_inputs,
		TerminalStatus::Completed,
	);
	let duplicate_refs = duplicate.iter().collect::<Vec<_>>();
	assert!(
		resolve_product_cohort(&duplicate_refs, &canonical_case_ids, &duplicate_input_cases,)
			.unwrap_err()
			.contains("duplicate case")
	);

	let failed = synthetic_product_cohort(&"e".repeat(64), &inputs, TerminalStatus::Fatal);
	let failed_refs = failed.iter().collect::<Vec<_>>();
	assert!(
		resolve_product_cohort(&failed_refs, &canonical_case_ids, &input_cases)
			.unwrap_err()
			.contains("expected Completed")
	);
}
