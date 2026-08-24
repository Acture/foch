//! Measurement cohort registry and deterministic cohort selection.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use foch::model::MergeBackendId;

use crate::merge_quality::corpus::{ORACLE_POLICY_VERSION, OracleAssessment};
use crate::merge_quality::dataset::{
	InputVersionRecord, MeasurementCohortKey, MeasurementRecord, MeasurementSummary, TerminalStatus,
};

pub const MEASUREMENT_REPORT_SCHEMA: &str = "2.0.0";
pub const WORKSHOP_MEASUREMENT_REPORT_SCHEMA: &str = "3.0.0";
pub const MEASUREMENT_COHORT_REGISTRY_SCHEMA: &str = "1.0.0";

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RegisteredMeasurementCohort {
	pub identity: MeasurementCohortKey,
	pub backend: MergeBackendId,
	pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MeasurementCohortRegistry {
	pub schema: String,
	pub cohorts: Vec<RegisteredMeasurementCohort>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasurementCohortDescriptor {
	pub cohort_id: String,
	pub identity: MeasurementCohortKey,
	pub scorer_version: String,
	pub backend: Option<MergeBackendId>,
	pub label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeasurementCohortSelector<'a> {
	CohortId(&'a str),
	ScorerVersion(&'a str),
}

#[derive(Clone, Debug)]
pub struct SelectedMeasurementCohort<'a> {
	pub descriptor: MeasurementCohortDescriptor,
	pub measurements: Vec<&'a MeasurementRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopReportCase {
	pub input_version: InputVersionRecord,
	pub title: String,
	pub oracle: OracleAssessment,
}

impl WorkshopReportCase {
	pub fn new(
		input_version: InputVersionRecord,
		title: impl Into<String>,
		oracle: OracleAssessment,
	) -> Self {
		Self {
			input_version,
			title: title.into(),
			oracle,
		}
	}
}

#[derive(Clone, Copy, Debug)]
pub struct WorkshopReportRequest<'a> {
	pub generated_at: &'a str,
	pub cases: &'a [WorkshopReportCase],
	pub measurements: &'a [MeasurementRecord],
	pub registry: &'a MeasurementCohortRegistry,
	pub selector: MeasurementCohortSelector<'a>,
	pub cohort: WorkshopReportCohort,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkshopReportCohort {
	Scorable,
	AllCandidates,
}

impl WorkshopReportCohort {
	fn includes(self, oracle: &OracleAssessment) -> bool {
		match self {
			Self::Scorable => oracle.is_scorable(),
			Self::AllCandidates => true,
		}
	}

	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Scorable => "scorable",
			Self::AllCandidates => "all_candidates",
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopMeasurementReport {
	pub schema: String,
	pub generated_at: String,
	pub measurement_cohort_id: String,
	pub measurement_cohort_label: String,
	pub measurement_identity: MeasurementCohortKey,
	pub backend: MergeBackendId,
	pub scorer_version: String,
	pub oracle_policy_version: String,
	pub cohort: String,
	pub candidate_cases: usize,
	pub scorable_cases: usize,
	pub excluded_cases: usize,
	pub baseline_complete: bool,
	pub total_cases: usize,
	pub terminal_cases: usize,
	pub completed_cases: usize,
	pub merge_failed_cases: usize,
	pub status_counts: BTreeMap<String, usize>,
	pub reference_output: WorkshopQualityAggregate,
	pub multi_source: WorkshopQualityAggregate,
	pub cases: Vec<WorkshopMeasurementCase>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopQualityAggregate {
	pub accepted: usize,
	pub total: usize,
	pub verdicts: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopMeasurementCase {
	pub case_id: String,
	pub input_version_id: String,
	pub title: String,
	pub oracle: OracleAssessment,
	pub status: String,
	pub measurement_id: Option<String>,
	pub evidence_bundle_hash: Option<String>,
	pub detail: Option<String>,
	pub summary: Option<MeasurementSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkshopReportError {
	CohortSelection(MeasurementCohortSelectionError),
	InvalidInputVersion {
		input_version_id: String,
	},
	DuplicateCaseId {
		case_id: String,
	},
	DuplicateInputVersion {
		input_version_id: String,
	},
	MissingMergeBackend {
		cohort_id: String,
	},
	LegacyMeasurement {
		measurement_id: String,
	},
	DuplicateTerminalMeasurement {
		input_version_id: String,
	},
	InvalidEvidenceReference {
		measurement_id: String,
	},
	InvalidTerminalPayload {
		measurement_id: String,
		detail: String,
	},
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MeasurementCohortSelectionError {
	InvalidMeasurementIdentity {
		measurement_id: String,
	},
	InvalidRegistrySchema {
		actual: String,
	},
	DuplicateRegistryEntry {
		cohort_id: String,
	},
	RegistryBackendMismatch {
		cohort_id: String,
		identity_backend: MergeBackendId,
		registered_backend: MergeBackendId,
	},
	UnregisteredLegacyCohort {
		cohort_id: String,
	},
	NotFound {
		selector: String,
	},
	AmbiguousScorerVersion {
		scorer_version: String,
		cohort_ids: Vec<String>,
	},
}

impl fmt::Display for MeasurementCohortSelectionError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::InvalidMeasurementIdentity { measurement_id } => {
				write!(
					formatter,
					"measurement {measurement_id} has an invalid identity"
				)
			}
			Self::InvalidRegistrySchema { actual } => write!(
				formatter,
				"unsupported measurement cohort registry schema {actual:?}"
			),
			Self::DuplicateRegistryEntry { cohort_id } => {
				write!(formatter, "duplicate registry entry for cohort {cohort_id}")
			}
			Self::RegistryBackendMismatch {
				cohort_id,
				identity_backend,
				registered_backend,
			} => write!(
				formatter,
				"registry backend mismatch for cohort {cohort_id}: identity={} registered={}",
				identity_backend.as_str(),
				registered_backend.as_str()
			),
			Self::UnregisteredLegacyCohort { cohort_id } => write!(
				formatter,
				"legacy measurement cohort {cohort_id} is not registered"
			),
			Self::NotFound { selector } => {
				write!(formatter, "no measurement cohort matches {selector}")
			}
			Self::AmbiguousScorerVersion {
				scorer_version,
				cohort_ids,
			} => write!(
				formatter,
				"scorer version {scorer_version:?} matches multiple cohorts: {}",
				cohort_ids.join(", ")
			),
		}
	}
}

impl Error for MeasurementCohortSelectionError {}

impl fmt::Display for WorkshopReportError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::CohortSelection(error) => error.fmt(formatter),
			Self::InvalidInputVersion { input_version_id } => write!(
				formatter,
				"Workshop input version {input_version_id} has an invalid identity"
			),
			Self::DuplicateCaseId { case_id } => {
				write!(
					formatter,
					"Workshop report contains duplicate case {case_id}"
				)
			}
			Self::DuplicateInputVersion { input_version_id } => write!(
				formatter,
				"Workshop report contains duplicate input version {input_version_id}"
			),
			Self::MissingMergeBackend { cohort_id } => {
				write!(formatter, "measurement cohort {cohort_id} has no backend")
			}
			Self::LegacyMeasurement { measurement_id } => write!(
				formatter,
				"Workshop report cohort contains legacy measurement {measurement_id}"
			),
			Self::DuplicateTerminalMeasurement { input_version_id } => write!(
				formatter,
				"Workshop input version {input_version_id} has duplicate terminal measurements"
			),
			Self::InvalidEvidenceReference { measurement_id } => write!(
				formatter,
				"measurement {measurement_id} has an invalid evidence bundle reference"
			),
			Self::InvalidTerminalPayload {
				measurement_id,
				detail,
			} => write!(
				formatter,
				"measurement {measurement_id} has an invalid terminal payload: {detail}"
			),
		}
	}
}

impl Error for WorkshopReportError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::CohortSelection(error) => Some(error),
			_ => None,
		}
	}
}

impl From<MeasurementCohortSelectionError> for WorkshopReportError {
	fn from(error: MeasurementCohortSelectionError) -> Self {
		Self::CohortSelection(error)
	}
}

pub fn committed_measurement_cohort_registry()
-> Result<MeasurementCohortRegistry, serde_json::Error> {
	Ok(MeasurementCohortRegistry {
		schema: MEASUREMENT_COHORT_REGISTRY_SCHEMA.to_string(),
		cohorts: Vec::new(),
	})
}

pub fn measurement_cohort_descriptors(
	records: &[MeasurementRecord],
	registry: &MeasurementCohortRegistry,
) -> Result<Vec<MeasurementCohortDescriptor>, MeasurementCohortSelectionError> {
	let grouped = group_measurements(records)?;
	let registered = validated_registry(registry)?;
	grouped
		.keys()
		.map(|identity| describe_cohort(identity, &registered))
		.collect()
}

pub fn select_measurement_cohort<'a>(
	records: &'a [MeasurementRecord],
	registry: &MeasurementCohortRegistry,
	selector: MeasurementCohortSelector<'_>,
) -> Result<SelectedMeasurementCohort<'a>, MeasurementCohortSelectionError> {
	let mut grouped = group_measurements(records)?;
	let registered = validated_registry(registry)?;
	let identity = match selector {
		MeasurementCohortSelector::CohortId(cohort_id) => grouped
			.keys()
			.find(|identity| identity.cohort_id() == cohort_id)
			.cloned()
			.ok_or_else(|| MeasurementCohortSelectionError::NotFound {
				selector: format!("cohort ID {cohort_id:?}"),
			})?,
		MeasurementCohortSelector::ScorerVersion(scorer_version) => {
			let matches: Vec<MeasurementCohortKey> = grouped
				.keys()
				.filter(|identity| identity.scorer_version() == scorer_version)
				.cloned()
				.collect();
			match matches.as_slice() {
				[] => {
					return Err(MeasurementCohortSelectionError::NotFound {
						selector: format!("scorer version {scorer_version:?}"),
					});
				}
				[identity] => identity.clone(),
				_ => {
					let mut cohort_ids: Vec<String> = matches
						.iter()
						.map(MeasurementCohortKey::cohort_id)
						.collect();
					cohort_ids.sort();
					return Err(MeasurementCohortSelectionError::AmbiguousScorerVersion {
						scorer_version: scorer_version.to_string(),
						cohort_ids,
					});
				}
			}
		}
	};
	let measurements = grouped
		.remove(&identity)
		.expect("selected identity came from grouped measurements");
	Ok(SelectedMeasurementCohort {
		descriptor: describe_cohort(&identity, &registered)?,
		measurements,
	})
}

/// Build a Workshop-backed V2 report without opening legacy snapshots or CAS.
///
/// The caller supplies the exact logical cases and their resolved input
/// versions. This keeps historical V1 reporting independent while making the
/// live cohort's denominator explicit and deterministic.
pub fn build_workshop_measurement_report(
	request: WorkshopReportRequest<'_>,
) -> Result<WorkshopMeasurementReport, WorkshopReportError> {
	let mut ordered_cases: Vec<&WorkshopReportCase> = request.cases.iter().collect();
	let mut case_ids = BTreeSet::new();
	let mut input_version_ids = BTreeSet::new();
	for case in &ordered_cases {
		let input = &case.input_version;
		if !input.identity_is_valid() {
			return Err(WorkshopReportError::InvalidInputVersion {
				input_version_id: input.input_version_id.clone(),
			});
		}
		if !case_ids.insert(input.case_id.as_str()) {
			return Err(WorkshopReportError::DuplicateCaseId {
				case_id: input.case_id.clone(),
			});
		}
		if !input_version_ids.insert(input.input_version_id.as_str()) {
			return Err(WorkshopReportError::DuplicateInputVersion {
				input_version_id: input.input_version_id.clone(),
			});
		}
	}
	ordered_cases.sort_by(|left, right| {
		(
			left.input_version.case_id.as_str(),
			left.input_version.input_version_id.as_str(),
		)
			.cmp(&(
				right.input_version.case_id.as_str(),
				right.input_version.input_version_id.as_str(),
			))
	});
	let candidate_cases = ordered_cases.len();
	let scorable_cases = ordered_cases
		.iter()
		.filter(|case| case.oracle.is_scorable())
		.count();
	ordered_cases.retain(|case| request.cohort.includes(&case.oracle));
	let report_input_ids: BTreeSet<&str> = ordered_cases
		.iter()
		.map(|case| case.input_version.input_version_id.as_str())
		.collect();

	let selected =
		select_measurement_cohort(request.measurements, request.registry, request.selector)?;
	let backend =
		selected
			.descriptor
			.backend
			.ok_or_else(|| WorkshopReportError::MissingMergeBackend {
				cohort_id: selected.descriptor.cohort_id.clone(),
			})?;
	let mut measurements_by_input = BTreeMap::<&str, &MeasurementRecord>::new();
	for measurement in selected.measurements {
		let Some(input_version_id) = measurement.input_version_id() else {
			return Err(WorkshopReportError::LegacyMeasurement {
				measurement_id: measurement.measurement_id().to_string(),
			});
		};
		if !report_input_ids.contains(input_version_id) {
			continue;
		}
		validate_workshop_terminal(measurement)?;
		if measurements_by_input
			.insert(input_version_id, measurement)
			.is_some()
		{
			return Err(WorkshopReportError::DuplicateTerminalMeasurement {
				input_version_id: input_version_id.to_string(),
			});
		}
	}

	let mut cases = Vec::with_capacity(ordered_cases.len());
	let mut status_counts = BTreeMap::new();
	let mut terminal_cases = 0_usize;
	let mut completed_cases = 0_usize;
	let mut reference_output = WorkshopQualityAggregate::default();
	let mut multi_source = WorkshopQualityAggregate::default();
	for case in ordered_cases {
		let input = &case.input_version;
		let measurement = measurements_by_input
			.get(input.input_version_id.as_str())
			.copied();
		let (status, measurement_id, evidence_bundle_hash, detail, summary) = match measurement {
			Some(measurement) => {
				terminal_cases += 1;
				let status = terminal_status_name(measurement.status()).to_string();
				if measurement.status() == TerminalStatus::Completed {
					completed_cases += 1;
					let summary = measurement
						.summary()
						.expect("completed Workshop terminal was validated");
					merge_summary(&mut reference_output, &mut multi_source, summary);
				}
				(
					status,
					Some(measurement.measurement_id().to_string()),
					measurement.evidence_bundle_hash().map(str::to_string),
					measurement.detail().map(str::to_string),
					measurement.summary().cloned(),
				)
			}
			None => ("missing".to_string(), None, None, None, None),
		};
		*status_counts.entry(status.clone()).or_default() += 1;
		cases.push(WorkshopMeasurementCase {
			case_id: input.case_id.clone(),
			input_version_id: input.input_version_id.clone(),
			title: case.title.clone(),
			oracle: case.oracle.clone(),
			status,
			measurement_id,
			evidence_bundle_hash,
			detail,
			summary,
		});
	}
	let total_cases = cases.len();

	Ok(WorkshopMeasurementReport {
		schema: WORKSHOP_MEASUREMENT_REPORT_SCHEMA.to_string(),
		generated_at: request.generated_at.to_string(),
		measurement_cohort_id: selected.descriptor.cohort_id,
		measurement_cohort_label: selected.descriptor.label,
		measurement_identity: selected.descriptor.identity,
		backend,
		scorer_version: selected.descriptor.scorer_version,
		oracle_policy_version: ORACLE_POLICY_VERSION.to_string(),
		cohort: request.cohort.as_str().to_string(),
		candidate_cases,
		scorable_cases,
		excluded_cases: candidate_cases.saturating_sub(scorable_cases),
		baseline_complete: total_cases != 0 && terminal_cases == total_cases,
		total_cases,
		terminal_cases,
		completed_cases,
		merge_failed_cases: terminal_cases.saturating_sub(completed_cases),
		status_counts,
		reference_output,
		multi_source,
		cases,
	})
}

fn validate_workshop_terminal(measurement: &MeasurementRecord) -> Result<(), WorkshopReportError> {
	if !measurement.evidence_reference_is_valid() {
		return Err(WorkshopReportError::InvalidEvidenceReference {
			measurement_id: measurement.measurement_id().to_string(),
		});
	}
	match (measurement.status(), measurement.summary()) {
		(TerminalStatus::Completed, Some(summary)) if summary_is_valid(summary) => Ok(()),
		(TerminalStatus::Completed, Some(_)) => Err(WorkshopReportError::InvalidTerminalPayload {
			measurement_id: measurement.measurement_id().to_string(),
			detail: "summary counts are inconsistent".to_string(),
		}),
		(TerminalStatus::Completed, None) => Err(WorkshopReportError::InvalidTerminalPayload {
			measurement_id: measurement.measurement_id().to_string(),
			detail: "completed measurement has no summary".to_string(),
		}),
		(_, None) => Ok(()),
		(_, Some(_)) => Err(WorkshopReportError::InvalidTerminalPayload {
			measurement_id: measurement.measurement_id().to_string(),
			detail: "failed measurement carries a completed summary".to_string(),
		}),
	}
}

fn summary_is_valid(summary: &MeasurementSummary) -> bool {
	summary.accepted_ground_truth_files <= summary.ground_truth_files
		&& summary.multi_source_files <= summary.ground_truth_files
		&& summary.accepted_multi_source_files <= summary.multi_source_files
		&& summary.accepted_multi_source_files <= summary.accepted_ground_truth_files
		&& summary.all_ground_truth_verdicts.values().sum::<usize>() == summary.ground_truth_files
		&& summary.multi_source_verdicts.values().sum::<usize>() == summary.multi_source_files
}

fn merge_summary(
	reference_output: &mut WorkshopQualityAggregate,
	multi_source: &mut WorkshopQualityAggregate,
	summary: &MeasurementSummary,
) {
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

fn merge_counts(target: &mut BTreeMap<String, usize>, source: &BTreeMap<String, usize>) {
	for (key, count) in source {
		*target.entry(key.clone()).or_default() += count;
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

fn group_measurements(
	records: &[MeasurementRecord],
) -> Result<BTreeMap<MeasurementCohortKey, Vec<&MeasurementRecord>>, MeasurementCohortSelectionError>
{
	let mut grouped: BTreeMap<MeasurementCohortKey, Vec<&MeasurementRecord>> = BTreeMap::new();
	for record in records {
		if !record.identity_is_valid() {
			return Err(
				MeasurementCohortSelectionError::InvalidMeasurementIdentity {
					measurement_id: record.measurement_id().to_string(),
				},
			);
		}
		grouped.entry(record.cohort_key()).or_default().push(record);
	}
	Ok(grouped)
}

fn validated_registry(
	registry: &MeasurementCohortRegistry,
) -> Result<
	BTreeMap<MeasurementCohortKey, &RegisteredMeasurementCohort>,
	MeasurementCohortSelectionError,
> {
	if registry.schema != MEASUREMENT_COHORT_REGISTRY_SCHEMA {
		return Err(MeasurementCohortSelectionError::InvalidRegistrySchema {
			actual: registry.schema.clone(),
		});
	}
	let mut registered = BTreeMap::new();
	let mut cohort_ids = BTreeSet::new();
	for cohort in &registry.cohorts {
		let cohort_id = cohort.identity.cohort_id();
		if !cohort_ids.insert(cohort_id.clone())
			|| registered.insert(cohort.identity.clone(), cohort).is_some()
		{
			return Err(MeasurementCohortSelectionError::DuplicateRegistryEntry { cohort_id });
		}
		if let Some(identity_backend) = cohort.identity.backend()
			&& identity_backend != cohort.backend
		{
			return Err(MeasurementCohortSelectionError::RegistryBackendMismatch {
				cohort_id,
				identity_backend,
				registered_backend: cohort.backend,
			});
		}
	}
	Ok(registered)
}

fn describe_cohort(
	identity: &MeasurementCohortKey,
	registered: &BTreeMap<MeasurementCohortKey, &RegisteredMeasurementCohort>,
) -> Result<MeasurementCohortDescriptor, MeasurementCohortSelectionError> {
	let cohort_id = identity.cohort_id();
	let registered = registered.get(identity).copied();
	let backend = match identity.backend() {
		Some(backend) => backend,
		None => registered.map(|cohort| cohort.backend).ok_or_else(|| {
			MeasurementCohortSelectionError::UnregisteredLegacyCohort {
				cohort_id: cohort_id.clone(),
			}
		})?,
	};
	let label = registered
		.map(|cohort| cohort.label.clone())
		.unwrap_or_else(|| {
			format!(
				"{} (scorer {})",
				backend.as_str(),
				identity.scorer_version()
			)
		});
	Ok(MeasurementCohortDescriptor {
		cohort_id,
		identity: identity.clone(),
		scorer_version: identity.scorer_version().to_string(),
		backend: Some(backend),
		label,
	})
}

#[cfg(test)]
mod tests {
	use foch::playset::steam::SteamId;

	use crate::merge_quality::corpus::assess_oracle_candidate;
	use crate::merge_quality::dataset::{
		EngineArtifactIdentity, GameInputIdentityV2, LegacyMeasurementIdentityV1,
		MeasurementIdentityV2, MeasurementScope, TerminalStatus, WorkshopInputVersionV2,
	};

	use super::*;

	fn completed_v2(
		artifact_hash: &str,
		backend: MergeBackendId,
		scorer_version: &str,
	) -> MeasurementRecord {
		MeasurementRecord::new_v2(
			MeasurementIdentityV2 {
				input_version_id: "snapshot".to_string(),
				engine_artifact: EngineArtifactIdentity::foch_executable_blake3(artifact_hash),
				runner_protocol_version: "1.0.0".to_string(),
				backend,
				scope: MeasurementScope::FullProductMerge,
				scorer_version: scorer_version.to_string(),
				scorer_config_hash: "config".to_string(),
			},
			"started".to_string(),
			"finished".to_string(),
			TerminalStatus::Completed,
			None,
			None,
			None,
		)
	}

	fn steam_id(value: &str) -> SteamId {
		value.parse().unwrap()
	}

	fn workshop_input_version(case_id: &str) -> InputVersionRecord {
		let item = |workshop_id: &str, manifest_id: &str| WorkshopInputVersionV2 {
			workshop_id: steam_id(workshop_id),
			manifest_id: steam_id(manifest_id),
		};
		InputVersionRecord::new(
			case_id.to_string(),
			GameInputIdentityV2 {
				app_id: steam_id("236850"),
				version: "1.37.5".to_string(),
				steam_build_id: Some(steam_id("12345")),
			},
			item(case_id, "1000"),
			vec![item("101", "1001"), item("102", "1002")],
		)
	}

	fn measurement_summary() -> MeasurementSummary {
		MeasurementSummary {
			merge_status: Some("complete".to_string()),
			ground_truth_files: 3,
			multi_source_files: 2,
			accepted_ground_truth_files: 2,
			accepted_multi_source_files: 1,
			all_ground_truth_verdicts: BTreeMap::from([
				("exact".to_string(), 2),
				("different".to_string(), 1),
			]),
			multi_source_verdicts: BTreeMap::from([
				("exact".to_string(), 1),
				("different".to_string(), 1),
			]),
			setup_ms: 1,
			merge_ms: 2,
			scoring_ms: 3,
			total_ms: 6,
		}
	}

	fn workshop_measurement(input_version_id: &str, status: TerminalStatus) -> MeasurementRecord {
		let completed = status == TerminalStatus::Completed;
		MeasurementRecord::new_v2(
			MeasurementIdentityV2 {
				input_version_id: input_version_id.to_string(),
				engine_artifact: EngineArtifactIdentity::foch_executable_blake3("a".repeat(64)),
				runner_protocol_version: "workshop-product-v1".to_string(),
				backend: MergeBackendId::GumtreePcsNway,
				scope: MeasurementScope::FullProductMerge,
				scorer_version: "2.0.0".to_string(),
				scorer_config_hash: "b".repeat(64),
			},
			"started".to_string(),
			"finished".to_string(),
			status,
			None,
			completed.then(|| "e".repeat(64)),
			completed.then(measurement_summary),
		)
	}

	#[test]
	fn scorer_only_selection_rejects_multiple_full_cohorts() {
		let registry = MeasurementCohortRegistry {
			schema: MEASUREMENT_COHORT_REGISTRY_SCHEMA.to_string(),
			cohorts: Vec::new(),
		};
		let records = vec![
			completed_v2("artifact-a", MergeBackendId::AddressPatch, "1.3.0"),
			completed_v2("artifact-b", MergeBackendId::GumtreePcsNway, "1.3.0"),
		];

		let error = select_measurement_cohort(
			&records,
			&registry,
			MeasurementCohortSelector::ScorerVersion("1.3.0"),
		)
		.unwrap_err();

		let MeasurementCohortSelectionError::AmbiguousScorerVersion {
			scorer_version,
			cohort_ids,
		} = error
		else {
			panic!("expected scorer ambiguity");
		};
		assert_eq!(scorer_version, "1.3.0");
		assert_eq!(cohort_ids.len(), 2);
		assert!(cohort_ids[0] < cohort_ids[1]);
	}

	#[test]
	fn unregistered_v1_cohort_is_rejected_instead_of_guessed() {
		let registry = MeasurementCohortRegistry {
			schema: MEASUREMENT_COHORT_REGISTRY_SCHEMA.to_string(),
			cohorts: Vec::new(),
		};
		let record = MeasurementRecord::new_v1(
			LegacyMeasurementIdentityV1 {
				snapshot_id: "snapshot".to_string(),
				executable_hash: "unknown-artifact".to_string(),
				scorer_version: "1.0.0".to_string(),
				config_hash: "unknown-config".to_string(),
			},
			"started".to_string(),
			"finished".to_string(),
			TerminalStatus::Completed,
			None,
			None,
			None,
		);
		let expected_cohort_id = record.cohort_id();
		let records = vec![record];

		let error = select_measurement_cohort(
			&records,
			&registry,
			MeasurementCohortSelector::ScorerVersion("1.0.0"),
		)
		.unwrap_err();

		assert_eq!(
			error,
			MeasurementCohortSelectionError::UnregisteredLegacyCohort {
				cohort_id: expected_cohort_id,
			}
		);
	}

	#[test]
	fn v2_descriptor_gets_backend_from_its_identity() {
		let registry = MeasurementCohortRegistry {
			schema: MEASUREMENT_COHORT_REGISTRY_SCHEMA.to_string(),
			cohorts: Vec::new(),
		};
		let records = vec![completed_v2(
			"artifact",
			MergeBackendId::GumtreePcsNway,
			"1.4.0",
		)];
		let selected = select_measurement_cohort(
			&records,
			&registry,
			MeasurementCohortSelector::ScorerVersion("1.4.0"),
		)
		.unwrap();
		assert_eq!(selected.descriptor.scorer_version, "1.4.0");
		assert_eq!(
			selected.descriptor.backend,
			Some(MergeBackendId::GumtreePcsNway)
		);
		assert!(selected.descriptor.label.contains("gumtree-pcs-nway"));
	}

	#[test]
	fn workshop_report_is_stable_for_v2_inputs() {
		let scorable_input = workshop_input_version("10");
		let excluded_input = workshop_input_version("20");
		let cases = vec![
			WorkshopReportCase::new(
				excluded_input.clone(),
				"Standalone overhaul",
				assess_oracle_candidate("Standalone overhaul", 2, false),
			),
			WorkshopReportCase::new(
				scorable_input.clone(),
				"Compatibility patch",
				assess_oracle_candidate("Compatibility patch", 2, false),
			),
		];
		let measurements = vec![
			workshop_measurement(&excluded_input.input_version_id, TerminalStatus::Crashed),
			workshop_measurement(&scorable_input.input_version_id, TerminalStatus::Completed),
		];
		let cohort_id = measurements[0].cohort_id();
		let registry = MeasurementCohortRegistry {
			schema: MEASUREMENT_COHORT_REGISTRY_SCHEMA.to_string(),
			cohorts: Vec::new(),
		};
		let build = |cohort| {
			build_workshop_measurement_report(WorkshopReportRequest {
				generated_at: "2026-08-08T00:00:00Z",
				cases: &cases,
				measurements: &measurements,
				registry: &registry,
				selector: MeasurementCohortSelector::CohortId(&cohort_id),
				cohort,
			})
			.unwrap()
		};

		let all = build(WorkshopReportCohort::AllCandidates);
		assert_eq!(all.schema, WORKSHOP_MEASUREMENT_REPORT_SCHEMA);
		assert_eq!(all.candidate_cases, 2);
		assert_eq!(all.scorable_cases, 1);
		assert_eq!(all.excluded_cases, 1);
		assert_eq!(all.total_cases, 2);
		assert_eq!(all.terminal_cases, 2);
		assert_eq!(all.completed_cases, 1);
		assert_eq!(all.merge_failed_cases, 1);
		assert!(all.baseline_complete);
		assert_eq!(all.status_counts["completed"], 1);
		assert_eq!(all.status_counts["crashed"], 1);
		assert_eq!(all.reference_output.accepted, 2);
		assert_eq!(all.reference_output.total, 3);
		assert_eq!(all.multi_source.accepted, 1);
		assert_eq!(all.multi_source.total, 2);
		assert_eq!(all.cases[0].case_id, "10");
		assert_eq!(all.cases[1].case_id, "20");
		assert_eq!(
			all.cases[0].input_version_id,
			scorable_input.input_version_id
		);
		assert!(all.cases[0].evidence_bundle_hash.is_some());
		assert!(all.cases[1].evidence_bundle_hash.is_none());
		let wire = serde_json::to_value(&all).unwrap();
		assert!(wire["cases"][0].get("input_version_id").is_some());
		assert!(wire["cases"][0].get("snapshot_id").is_none());

		let scorable = build(WorkshopReportCohort::Scorable);
		assert_eq!(scorable.candidate_cases, 2);
		assert_eq!(scorable.scorable_cases, 1);
		assert_eq!(scorable.total_cases, 1);
		assert_eq!(scorable.completed_cases, 1);
		assert!(scorable.baseline_complete);
	}

	#[test]
	fn workshop_report_rejects_duplicate_terminals() {
		let input = workshop_input_version("10");
		let cases = vec![WorkshopReportCase::new(
			input.clone(),
			"Compatibility patch",
			assess_oracle_candidate("Compatibility patch", 2, false),
		)];
		let first = workshop_measurement(&input.input_version_id, TerminalStatus::Completed);
		let mut second = first.clone();
		if let MeasurementRecord::V2 { finished_at, .. } = &mut second {
			*finished_at = "later".to_string();
		}
		let cohort_id = first.cohort_id();
		let measurements = vec![first, second];
		let registry = MeasurementCohortRegistry {
			schema: MEASUREMENT_COHORT_REGISTRY_SCHEMA.to_string(),
			cohorts: Vec::new(),
		};

		let error = build_workshop_measurement_report(WorkshopReportRequest {
			generated_at: "now",
			cases: &cases,
			measurements: &measurements,
			registry: &registry,
			selector: MeasurementCohortSelector::CohortId(&cohort_id),
			cohort: WorkshopReportCohort::AllCandidates,
		})
		.unwrap_err();

		assert_eq!(
			error,
			WorkshopReportError::DuplicateTerminalMeasurement {
				input_version_id: input.input_version_id,
			}
		);
	}

	#[test]
	fn workshop_report_requires_completed_summary_and_evidence() {
		let input = workshop_input_version("10");
		let cases = vec![WorkshopReportCase::new(
			input.clone(),
			"Compatibility patch",
			assess_oracle_candidate("Compatibility patch", 2, false),
		)];
		let valid = workshop_measurement(&input.input_version_id, TerminalStatus::Completed);
		let cohort_id = valid.cohort_id();
		let registry = MeasurementCohortRegistry {
			schema: MEASUREMENT_COHORT_REGISTRY_SCHEMA.to_string(),
			cohorts: Vec::new(),
		};
		let assert_rejected = |measurement: MeasurementRecord| {
			let measurements = vec![measurement];
			build_workshop_measurement_report(WorkshopReportRequest {
				generated_at: "now",
				cases: &cases,
				measurements: &measurements,
				registry: &registry,
				selector: MeasurementCohortSelector::CohortId(&cohort_id),
				cohort: WorkshopReportCohort::Scorable,
			})
			.unwrap_err()
		};

		let mut missing_evidence = valid.clone();
		if let MeasurementRecord::V2 {
			evidence_bundle_hash,
			..
		} = &mut missing_evidence
		{
			*evidence_bundle_hash = None;
		}
		assert!(matches!(
			assert_rejected(missing_evidence),
			WorkshopReportError::InvalidEvidenceReference { .. }
		));

		let mut missing_summary = valid;
		if let MeasurementRecord::V2 { summary, .. } = &mut missing_summary {
			*summary = None;
		}
		assert!(matches!(
			assert_rejected(missing_summary),
			WorkshopReportError::InvalidTerminalPayload { .. }
		));
	}
}
