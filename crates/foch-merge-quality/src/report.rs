//! Measurement cohort registry and deterministic cohort selection.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::dataset::{MeasurementCohortKey, MeasurementKernel, MeasurementRecord};

pub const MEASUREMENT_REPORT_SCHEMA: &str = "2.0.0";
pub const MEASUREMENT_COHORT_REGISTRY_SCHEMA: &str = "1.0.0";

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RegisteredMeasurementCohort {
	pub identity: MeasurementCohortKey,
	pub merge_kernel: MeasurementKernel,
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
	pub merge_kernel: Option<MeasurementKernel>,
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
	RegistryKernelMismatch {
		cohort_id: String,
		identity_kernel: MeasurementKernel,
		registered_kernel: MeasurementKernel,
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
			Self::RegistryKernelMismatch {
				cohort_id,
				identity_kernel,
				registered_kernel,
			} => write!(
				formatter,
				"registry kernel mismatch for cohort {cohort_id}: identity={} registered={}",
				identity_kernel.as_str(),
				registered_kernel.as_str()
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

pub fn committed_measurement_cohort_registry()
-> Result<MeasurementCohortRegistry, serde_json::Error> {
	serde_json::from_str(include_str!("../dataset/measurement-cohorts.json"))
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
		if let Some(identity_kernel) = cohort.identity.merge_kernel()
			&& identity_kernel != cohort.merge_kernel
		{
			return Err(MeasurementCohortSelectionError::RegistryKernelMismatch {
				cohort_id,
				identity_kernel,
				registered_kernel: cohort.merge_kernel,
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
	let merge_kernel = match identity.merge_kernel() {
		Some(kernel) => kernel,
		None => registered
			.map(|cohort| cohort.merge_kernel)
			.ok_or_else(
				|| MeasurementCohortSelectionError::UnregisteredLegacyCohort {
					cohort_id: cohort_id.clone(),
				},
			)?,
	};
	let label = registered
		.map(|cohort| cohort.label.clone())
		.unwrap_or_else(|| {
			format!(
				"{} (scorer {})",
				merge_kernel.as_str(),
				identity.scorer_version()
			)
		});
	Ok(MeasurementCohortDescriptor {
		cohort_id,
		identity: identity.clone(),
		scorer_version: identity.scorer_version().to_string(),
		merge_kernel: Some(merge_kernel),
		label,
	})
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use crate::dataset::{
		EngineArtifactIdentity, LegacyMeasurementIdentityV1, MeasurementIdentityV2,
		MeasurementScope, TerminalStatus,
	};

	use super::*;

	fn completed_v2(
		artifact_hash: &str,
		kernel: MeasurementKernel,
		scorer_version: &str,
	) -> MeasurementRecord {
		MeasurementRecord::new_v2(
			MeasurementIdentityV2 {
				snapshot_id: "snapshot".to_string(),
				engine_artifact: EngineArtifactIdentity::foch_executable_blake3(artifact_hash),
				runner_protocol_version: "1.0.0".to_string(),
				merge_kernel: kernel,
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

	#[test]
	fn committed_registry_labels_both_frozen_v1_cohorts_honestly() {
		let registry = committed_measurement_cohort_registry().unwrap();
		assert_eq!(registry.schema, MEASUREMENT_COHORT_REGISTRY_SCHEMA);
		assert_eq!(registry.cohorts.len(), 2);
		for cohort in registry.cohorts {
			assert!(matches!(
				cohort.identity,
				MeasurementCohortKey::OrchestratorBoundV1 { .. }
			));
			assert_eq!(
				cohort.merge_kernel,
				MeasurementKernel::LegacyAddressPatchReference
			);
			assert!(cohort.label.contains("Legacy AddressPatchReference"));
		}
	}

	#[test]
	fn tracked_v1_measurements_resolve_to_the_two_registered_legacy_cohorts() {
		let dataset_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("dataset");
		let records = crate::dataset::read_jsonl::<MeasurementRecord>(
			&dataset_root.join("measurements.jsonl"),
		)
		.unwrap();
		let registry = committed_measurement_cohort_registry().unwrap();
		let descriptors = measurement_cohort_descriptors(&records, &registry).unwrap();

		assert_eq!(descriptors.len(), 2);
		assert!(descriptors.iter().all(|descriptor| {
			descriptor.merge_kernel == Some(MeasurementKernel::LegacyAddressPatchReference)
		}));
		let scorer_1_3 = select_measurement_cohort(
			&records,
			&registry,
			MeasurementCohortSelector::ScorerVersion("1.3.0"),
		)
		.unwrap();
		assert_eq!(scorer_1_3.measurements.len(), 23);
	}

	#[test]
	fn selects_a_frozen_v1_cohort_by_full_identity() {
		let registry = committed_measurement_cohort_registry().unwrap();
		let identity = registry.cohorts[0].identity.clone();
		let MeasurementCohortKey::OrchestratorBoundV1 {
			executable_hash,
			scorer_version,
			config_hash,
		} = identity.clone()
		else {
			panic!("committed fixture must be V1");
		};
		let record = MeasurementRecord::new_v1(
			LegacyMeasurementIdentityV1 {
				snapshot_id: "snapshot".to_string(),
				executable_hash,
				scorer_version,
				config_hash,
			},
			"started".to_string(),
			"finished".to_string(),
			TerminalStatus::Completed,
			None,
			None,
			None,
		);
		let records = vec![record];
		let cohort_id = identity.cohort_id();

		let selected = select_measurement_cohort(
			&records,
			&registry,
			MeasurementCohortSelector::CohortId(&cohort_id),
		)
		.unwrap();

		assert_eq!(selected.measurements.len(), 1);
		assert_eq!(selected.descriptor.identity, identity);
		assert_eq!(
			selected.descriptor.merge_kernel,
			Some(MeasurementKernel::LegacyAddressPatchReference)
		);
		assert!(
			selected
				.descriptor
				.label
				.contains("Legacy AddressPatchReference")
		);
	}

	#[test]
	fn scorer_only_selection_rejects_multiple_full_cohorts() {
		let registry = MeasurementCohortRegistry {
			schema: MEASUREMENT_COHORT_REGISTRY_SCHEMA.to_string(),
			cohorts: Vec::new(),
		};
		let records = vec![
			completed_v2(
				"artifact-a",
				MeasurementKernel::LegacyAddressPatchReference,
				"1.3.0",
			),
			completed_v2("artifact-b", MeasurementKernel::SemanticTree, "1.3.0"),
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
	fn v2_descriptor_gets_kernel_from_its_identity() {
		let registry = MeasurementCohortRegistry {
			schema: MEASUREMENT_COHORT_REGISTRY_SCHEMA.to_string(),
			cohorts: Vec::new(),
		};
		let records = vec![completed_v2(
			"artifact",
			MeasurementKernel::SemanticTree,
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
			selected.descriptor.merge_kernel,
			Some(MeasurementKernel::SemanticTree)
		);
		assert!(selected.descriptor.label.contains("semantic_tree"));
	}
}
