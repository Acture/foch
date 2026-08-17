use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use foch_core::utils::steam::SteamId;

use crate::corpus::WorkshopProvenance;
pub const SCHEMA: &str = "1.0.0";
pub const INPUT_VERSION_SCHEMA_V2: &str = "2.0.0";
pub const WORKSHOP_OBSERVATION_SCHEMA_V2: &str = "2.0.0";
pub const MEASUREMENT_SCHEMA_V1: &str = "1.0.0";
pub const MEASUREMENT_SCHEMA_V2: &str = "2.0.0";
pub const SCORER_VERSION: &str = "2.1.0";

/// Frozen V1 tree statistics retained for decoding historical metadata.
/// Live Workshop inputs do not materialize tree objects.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct TreeStats {
	pub files: u64,
	pub directories: u64,
	pub symlinks: u64,
	pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
	Compatch,
	SourceMod,
	MergedOutput,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ObjectRecord {
	pub schema: String,
	pub object_id: String,
	pub kind: ObjectKind,
	pub content_hash: String,
	pub workshop_id: Option<String>,
	pub stats: TreeStats,
}

impl ObjectRecord {
	pub fn new(
		kind: ObjectKind,
		content_hash: String,
		workshop_id: Option<String>,
		stats: TreeStats,
	) -> Self {
		let kind_name = serde_json::to_string(&kind).expect("ObjectKind serializes");
		let workshop_id_part = workshop_id.as_deref().unwrap_or("");
		let object_id = stable_id(
			"object-record",
			&[
				kind_name.as_bytes(),
				workshop_id_part.as_bytes(),
				content_hash.as_bytes(),
			],
		);
		Self {
			schema: SCHEMA.to_string(),
			object_id,
			kind,
			content_hash,
			workshop_id,
			stats,
		}
	}
}

impl IdentifiedRecord for ObjectRecord {
	fn record_id(&self) -> &str {
		&self.object_id
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct GameIdentity {
	pub app_id: u32,
	pub version: String,
	pub steam_build_id: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GameInputIdentityV2 {
	pub app_id: SteamId,
	pub version: String,
	pub steam_build_id: Option<SteamId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopInputVersionV2 {
	pub workshop_id: SteamId,
	pub manifest_id: SteamId,
}

/// Immutable identity for one live-Workshop product input set.
///
/// Paths, ACF timestamps, and content-tree digests are deliberately absent.
/// Steam's read-only ACF is the sole authority for ordered Workshop versions.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputVersionRecord {
	pub schema: String,
	pub input_version_id: String,
	pub case_id: String,
	pub game: GameInputIdentityV2,
	pub compatch: WorkshopInputVersionV2,
	/// Source mods in declared playset order. Order is part of input identity.
	pub source_mods: Vec<WorkshopInputVersionV2>,
}

impl InputVersionRecord {
	pub fn new(
		case_id: String,
		game: GameInputIdentityV2,
		compatch: WorkshopInputVersionV2,
		source_mods: Vec<WorkshopInputVersionV2>,
	) -> Self {
		let identity = serde_json::to_vec(&(&game, &compatch, &source_mods))
			.expect("Workshop input identity serializes");
		let input_version_id = stable_id(
			"workshop-input-version-v2",
			&[case_id.as_bytes(), &identity],
		);
		Self {
			schema: INPUT_VERSION_SCHEMA_V2.to_string(),
			input_version_id,
			case_id,
			game,
			compatch,
			source_mods,
		}
	}

	pub fn identity_is_valid(&self) -> bool {
		self.schema == INPUT_VERSION_SCHEMA_V2
			&& !self.case_id.is_empty()
			&& !self.game.version.is_empty()
			&& *self
				== Self::new(
					self.case_id.clone(),
					self.game.clone(),
					self.compatch.clone(),
					self.source_mods.clone(),
				)
	}
}

impl IdentifiedRecord for InputVersionRecord {
	fn record_id(&self) -> &str {
		&self.input_version_id
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopItemObservationV2 {
	pub workshop_id: SteamId,
	pub manifest_id: SteamId,
	pub time_updated: u64,
	pub size_bytes: u64,
	pub ugc_handle: Option<SteamId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SnapshotObjectRef {
	pub workshop_id: String,
	pub content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SnapshotRecord {
	pub schema: String,
	pub snapshot_id: String,
	pub case_id: String,
	pub game: GameIdentity,
	pub compatch: SnapshotObjectRef,
	/// Source mods in declared playset order. Order is part of snapshot identity.
	pub source_mods: Vec<SnapshotObjectRef>,
}

impl SnapshotRecord {
	pub fn new(
		case_id: String,
		game: GameIdentity,
		compatch: SnapshotObjectRef,
		source_mods: Vec<SnapshotObjectRef>,
	) -> Self {
		let identity = serde_json::to_vec(&(&game, &compatch, &source_mods))
			.expect("snapshot identity serializes");
		let snapshot_id = stable_id("snapshot", &[case_id.as_bytes(), &identity]);
		Self {
			schema: SCHEMA.to_string(),
			snapshot_id,
			case_id,
			game,
			compatch,
			source_mods,
		}
	}

	pub fn identity_is_valid(&self) -> bool {
		*self
			== Self::new(
				self.case_id.clone(),
				self.game.clone(),
				self.compatch.clone(),
				self.source_mods.clone(),
			)
	}
}

impl IdentifiedRecord for SnapshotRecord {
	fn record_id(&self) -> &str {
		&self.snapshot_id
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WorkshopObservation {
	pub workshop_id: String,
	pub title: String,
	pub time_created: i64,
	pub time_updated: i64,
	pub provenance: WorkshopProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ObservationRecord {
	pub schema: String,
	pub observation_id: String,
	pub snapshot_id: String,
	pub observed_at: String,
	pub compatch: WorkshopObservation,
	pub source_mods: Vec<WorkshopObservation>,
	pub subscriptions: i64,
	pub mod_churned: bool,
}

impl ObservationRecord {
	pub fn new(
		snapshot_id: String,
		observed_at: String,
		compatch: WorkshopObservation,
		source_mods: Vec<WorkshopObservation>,
		subscriptions: i64,
		mod_churned: bool,
	) -> Self {
		let payload = serde_json::to_vec(&(
			&snapshot_id,
			&observed_at,
			&compatch,
			&source_mods,
			subscriptions,
			mod_churned,
		))
		.expect("observation identity serializes");
		Self {
			schema: SCHEMA.to_string(),
			observation_id: stable_id("observation", &[&payload]),
			snapshot_id,
			observed_at,
			compatch,
			source_mods,
			subscriptions,
			mod_churned,
		}
	}
}

impl IdentifiedRecord for ObservationRecord {
	fn record_id(&self) -> &str {
		&self.observation_id
	}
}

/// One read-only observation of the Steam ACF state backing an input version.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopObservationRecordV2 {
	pub schema: String,
	pub observation_id: String,
	pub input_version_id: String,
	pub observed_at: String,
	pub compatch: WorkshopItemObservationV2,
	pub source_mods: Vec<WorkshopItemObservationV2>,
}

impl WorkshopObservationRecordV2 {
	pub fn new(
		input_version_id: String,
		observed_at: String,
		compatch: WorkshopItemObservationV2,
		source_mods: Vec<WorkshopItemObservationV2>,
	) -> Self {
		let payload =
			serde_json::to_vec(&(&input_version_id, &observed_at, &compatch, &source_mods))
				.expect("Workshop observation identity serializes");
		Self {
			schema: WORKSHOP_OBSERVATION_SCHEMA_V2.to_string(),
			observation_id: stable_id("workshop-observation-v2", &[&payload]),
			input_version_id,
			observed_at,
			compatch,
			source_mods,
		}
	}

	pub fn identity_is_valid(&self) -> bool {
		self.schema == WORKSHOP_OBSERVATION_SCHEMA_V2
			&& is_blake3_hash(&self.input_version_id)
			&& !self.observed_at.is_empty()
			&& *self
				== Self::new(
					self.input_version_id.clone(),
					self.observed_at.clone(),
					self.compatch.clone(),
					self.source_mods.clone(),
				)
	}

	pub fn matches_input_version(&self, input: &InputVersionRecord) -> bool {
		self.input_version_id == input.input_version_id
			&& item_observation_matches_input(&self.compatch, &input.compatch)
			&& self.source_mods.len() == input.source_mods.len()
			&& self
				.source_mods
				.iter()
				.zip(&input.source_mods)
				.all(|(observation, version)| item_observation_matches_input(observation, version))
	}
}

fn item_observation_matches_input(
	observation: &WorkshopItemObservationV2,
	version: &WorkshopInputVersionV2,
) -> bool {
	observation.workshop_id == version.workshop_id && observation.manifest_id == version.manifest_id
}

impl IdentifiedRecord for WorkshopObservationRecordV2 {
	fn record_id(&self) -> &str {
		&self.observation_id
	}
}

/// Compatibility reader for the append-only observations stream.
///
/// V1 remains represented by [`ObservationRecord`] so historical callers and
/// serialization stay untouched. New code can read the shared JSONL as this
/// enum and append only `V2` records.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum WorkshopObservationRecord {
	V2(WorkshopObservationRecordV2),
	V1(ObservationRecord),
}

impl WorkshopObservationRecord {
	pub fn schema(&self) -> &str {
		match self {
			Self::V1(record) => &record.schema,
			Self::V2(record) => &record.schema,
		}
	}
}

impl From<ObservationRecord> for WorkshopObservationRecord {
	fn from(record: ObservationRecord) -> Self {
		Self::V1(record)
	}
}

impl From<WorkshopObservationRecordV2> for WorkshopObservationRecord {
	fn from(record: WorkshopObservationRecordV2) -> Self {
		Self::V2(record)
	}
}

impl IdentifiedRecord for WorkshopObservationRecord {
	fn record_id(&self) -> &str {
		match self {
			Self::V1(record) => record.record_id(),
			Self::V2(record) => record.record_id(),
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
	Completed,
	MergeFailed,
	Crashed,
	TimedOut,
	Fatal,
}

impl TerminalStatus {
	pub fn counts_as_merge_failed(self) -> bool {
		self != Self::Completed
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MeasurementSummary {
	pub merge_status: Option<String>,
	pub ground_truth_files: usize,
	pub multi_source_files: usize,
	pub accepted_ground_truth_files: usize,
	pub accepted_multi_source_files: usize,
	pub all_ground_truth_verdicts: BTreeMap<String, usize>,
	pub multi_source_verdicts: BTreeMap<String, usize>,
	pub setup_ms: u64,
	pub merge_ms: u64,
	pub scoring_ms: u64,
	pub total_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineArtifactKind {
	FochExecutable,
}

impl EngineArtifactKind {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::FochExecutable => "foch_executable",
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactHashAlgorithm {
	Blake3,
}

impl ArtifactHashAlgorithm {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Blake3 => "blake3",
		}
	}
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub struct EngineArtifactIdentity {
	pub kind: EngineArtifactKind,
	pub hash_algorithm: ArtifactHashAlgorithm,
	pub hash: String,
}

impl EngineArtifactIdentity {
	pub fn foch_executable_blake3(hash: impl Into<String>) -> Self {
		Self {
			kind: EngineArtifactKind::FochExecutable,
			hash_algorithm: ArtifactHashAlgorithm::Blake3,
			hash: hash.into(),
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementKernel {
	LegacyAddressPatchReference,
	SemanticTree,
}

impl MeasurementKernel {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::LegacyAddressPatchReference => "legacy_address_patch_reference",
			Self::SemanticTree => "semantic_tree",
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementScope {
	FullProductMerge,
}

impl MeasurementScope {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::FullProductMerge => "full_product_merge",
		}
	}
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(tag = "identity_kind", rename_all = "snake_case")]
pub enum MeasurementCohortKey {
	OrchestratorBoundV1 {
		executable_hash: String,
		scorer_version: String,
		config_hash: String,
	},
	EngineArtifactV2 {
		engine_artifact: EngineArtifactIdentity,
		runner_protocol_version: String,
		merge_kernel: MeasurementKernel,
		scope: MeasurementScope,
		scorer_version: String,
		scorer_config_hash: String,
	},
}

impl MeasurementCohortKey {
	pub fn cohort_id(&self) -> String {
		match self {
			Self::OrchestratorBoundV1 {
				executable_hash,
				scorer_version,
				config_hash,
			} => stable_id(
				"measurement-cohort-v1",
				&[
					executable_hash.as_bytes(),
					scorer_version.as_bytes(),
					config_hash.as_bytes(),
				],
			),
			Self::EngineArtifactV2 {
				engine_artifact,
				runner_protocol_version,
				merge_kernel,
				scope,
				scorer_version,
				scorer_config_hash,
			} => stable_id(
				"measurement-cohort-v2",
				&[
					engine_artifact.kind.as_str().as_bytes(),
					engine_artifact.hash_algorithm.as_str().as_bytes(),
					engine_artifact.hash.as_bytes(),
					runner_protocol_version.as_bytes(),
					merge_kernel.as_str().as_bytes(),
					scope.as_str().as_bytes(),
					scorer_version.as_bytes(),
					scorer_config_hash.as_bytes(),
				],
			),
		}
	}

	pub fn scorer_version(&self) -> &str {
		match self {
			Self::OrchestratorBoundV1 { scorer_version, .. }
			| Self::EngineArtifactV2 { scorer_version, .. } => scorer_version,
		}
	}

	pub const fn merge_kernel(&self) -> Option<MeasurementKernel> {
		match self {
			Self::OrchestratorBoundV1 { .. } => None,
			Self::EngineArtifactV2 { merge_kernel, .. } => Some(*merge_kernel),
		}
	}

	pub const fn scope(&self) -> Option<MeasurementScope> {
		match self {
			Self::OrchestratorBoundV1 { .. } => None,
			Self::EngineArtifactV2 { scope, .. } => Some(*scope),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyMeasurementIdentityV1 {
	pub snapshot_id: String,
	pub executable_hash: String,
	pub scorer_version: String,
	pub config_hash: String,
}

impl LegacyMeasurementIdentityV1 {
	pub fn cohort_key(&self) -> MeasurementCohortKey {
		MeasurementCohortKey::OrchestratorBoundV1 {
			executable_hash: self.executable_hash.clone(),
			scorer_version: self.scorer_version.clone(),
			config_hash: self.config_hash.clone(),
		}
	}

	pub fn cohort_id(&self) -> String {
		self.cohort_key().cohort_id()
	}

	pub fn measurement_id(&self) -> String {
		stable_id(
			"measurement",
			&[
				self.snapshot_id.as_bytes(),
				self.executable_hash.as_bytes(),
				self.scorer_version.as_bytes(),
				self.config_hash.as_bytes(),
			],
		)
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasurementIdentityV2 {
	pub input_version_id: String,
	pub engine_artifact: EngineArtifactIdentity,
	pub runner_protocol_version: String,
	pub merge_kernel: MeasurementKernel,
	pub scope: MeasurementScope,
	pub scorer_version: String,
	pub scorer_config_hash: String,
}

impl MeasurementIdentityV2 {
	pub fn cohort_key(&self) -> MeasurementCohortKey {
		MeasurementCohortKey::EngineArtifactV2 {
			engine_artifact: self.engine_artifact.clone(),
			runner_protocol_version: self.runner_protocol_version.clone(),
			merge_kernel: self.merge_kernel,
			scope: self.scope,
			scorer_version: self.scorer_version.clone(),
			scorer_config_hash: self.scorer_config_hash.clone(),
		}
	}

	pub fn cohort_id(&self) -> String {
		self.cohort_key().cohort_id()
	}

	pub fn measurement_id(&self) -> String {
		let cohort_id = self.cohort_id();
		stable_id(
			"measurement-v2",
			&[self.input_version_id.as_bytes(), cohort_id.as_bytes()],
		)
	}
}

/// One immutable terminal measurement. V1 preserves the historical
/// orchestrator-bound identity; all new measurements must use V2.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "schema")]
pub enum MeasurementRecord {
	#[serde(rename = "1.0.0")]
	V1 {
		measurement_id: String,
		snapshot_id: String,
		executable_hash: String,
		scorer_version: String,
		config_hash: String,
		started_at: String,
		finished_at: String,
		status: TerminalStatus,
		detail: Option<String>,
		merged_output_hash: Option<String>,
		summary: Option<MeasurementSummary>,
	},
	#[serde(rename = "2.0.0")]
	V2 {
		measurement_id: String,
		input_version_id: String,
		engine_artifact: EngineArtifactIdentity,
		runner_protocol_version: String,
		merge_kernel: MeasurementKernel,
		scope: MeasurementScope,
		scorer_version: String,
		scorer_config_hash: String,
		started_at: String,
		finished_at: String,
		status: TerminalStatus,
		detail: Option<String>,
		evidence_bundle_hash: Option<String>,
		summary: Option<MeasurementSummary>,
	},
}

impl MeasurementRecord {
	#[allow(clippy::too_many_arguments)]
	pub fn new_v1(
		identity: LegacyMeasurementIdentityV1,
		started_at: String,
		finished_at: String,
		status: TerminalStatus,
		detail: Option<String>,
		merged_output_hash: Option<String>,
		summary: Option<MeasurementSummary>,
	) -> Self {
		Self::V1 {
			measurement_id: identity.measurement_id(),
			snapshot_id: identity.snapshot_id,
			executable_hash: identity.executable_hash,
			scorer_version: identity.scorer_version,
			config_hash: identity.config_hash,
			started_at,
			finished_at,
			status,
			detail,
			merged_output_hash,
			summary,
		}
	}

	#[allow(clippy::too_many_arguments)]
	pub fn new_v2(
		identity: MeasurementIdentityV2,
		started_at: String,
		finished_at: String,
		status: TerminalStatus,
		detail: Option<String>,
		evidence_bundle_hash: Option<String>,
		summary: Option<MeasurementSummary>,
	) -> Self {
		Self::V2 {
			measurement_id: identity.measurement_id(),
			input_version_id: identity.input_version_id,
			engine_artifact: identity.engine_artifact,
			runner_protocol_version: identity.runner_protocol_version,
			merge_kernel: identity.merge_kernel,
			scope: identity.scope,
			scorer_version: identity.scorer_version,
			scorer_config_hash: identity.scorer_config_hash,
			started_at,
			finished_at,
			status,
			detail,
			evidence_bundle_hash,
			summary,
		}
	}

	pub const fn schema(&self) -> &'static str {
		match self {
			Self::V1 { .. } => MEASUREMENT_SCHEMA_V1,
			Self::V2 { .. } => MEASUREMENT_SCHEMA_V2,
		}
	}

	pub fn measurement_id(&self) -> &str {
		match self {
			Self::V1 { measurement_id, .. } | Self::V2 { measurement_id, .. } => measurement_id,
		}
	}

	pub fn legacy_snapshot_id(&self) -> Option<&str> {
		match self {
			Self::V1 { snapshot_id, .. } => Some(snapshot_id),
			Self::V2 { .. } => None,
		}
	}

	pub fn input_version_id(&self) -> Option<&str> {
		match self {
			Self::V1 { .. } => None,
			Self::V2 {
				input_version_id, ..
			} => Some(input_version_id),
		}
	}

	pub fn scorer_version(&self) -> &str {
		match self {
			Self::V1 { scorer_version, .. } | Self::V2 { scorer_version, .. } => scorer_version,
		}
	}

	pub fn config_hash(&self) -> &str {
		match self {
			Self::V1 { config_hash, .. } => config_hash,
			Self::V2 {
				scorer_config_hash, ..
			} => scorer_config_hash,
		}
	}

	pub fn legacy_executable_hash(&self) -> Option<&str> {
		match self {
			Self::V1 {
				executable_hash, ..
			} => Some(executable_hash),
			Self::V2 { .. } => None,
		}
	}

	pub fn engine_artifact(&self) -> Option<&EngineArtifactIdentity> {
		match self {
			Self::V1 { .. } => None,
			Self::V2 {
				engine_artifact, ..
			} => Some(engine_artifact),
		}
	}

	pub fn runner_protocol_version(&self) -> Option<&str> {
		match self {
			Self::V1 { .. } => None,
			Self::V2 {
				runner_protocol_version,
				..
			} => Some(runner_protocol_version),
		}
	}

	pub const fn merge_kernel(&self) -> Option<MeasurementKernel> {
		match self {
			Self::V1 { .. } => None,
			Self::V2 { merge_kernel, .. } => Some(*merge_kernel),
		}
	}

	pub const fn scope(&self) -> Option<MeasurementScope> {
		match self {
			Self::V1 { .. } => None,
			Self::V2 { scope, .. } => Some(*scope),
		}
	}

	pub fn started_at(&self) -> &str {
		match self {
			Self::V1 { started_at, .. } | Self::V2 { started_at, .. } => started_at,
		}
	}

	pub fn finished_at(&self) -> &str {
		match self {
			Self::V1 { finished_at, .. } | Self::V2 { finished_at, .. } => finished_at,
		}
	}

	pub const fn status(&self) -> TerminalStatus {
		match self {
			Self::V1 { status, .. } | Self::V2 { status, .. } => *status,
		}
	}

	pub fn detail(&self) -> Option<&str> {
		match self {
			Self::V1 { detail, .. } | Self::V2 { detail, .. } => detail.as_deref(),
		}
	}

	pub fn merged_output_hash(&self) -> Option<&str> {
		match self {
			Self::V1 {
				merged_output_hash, ..
			} => merged_output_hash.as_deref(),
			Self::V2 { .. } => None,
		}
	}

	pub fn evidence_bundle_hash(&self) -> Option<&str> {
		match self {
			Self::V1 { .. } => None,
			Self::V2 {
				evidence_bundle_hash,
				..
			} => evidence_bundle_hash.as_deref(),
		}
	}

	/// Completed V2 measurements must carry a valid compact evidence bundle;
	/// failed terminal outcomes must not claim successful evidence archival.
	pub fn evidence_reference_is_valid(&self) -> bool {
		match self {
			Self::V1 { .. } => true,
			Self::V2 {
				status,
				evidence_bundle_hash,
				..
			} => {
				matches!(
					(*status, evidence_bundle_hash),
					(TerminalStatus::Completed, Some(hash)) if is_blake3_hash(hash)
				) || (*status != TerminalStatus::Completed && evidence_bundle_hash.is_none())
			}
		}
	}

	pub fn summary(&self) -> Option<&MeasurementSummary> {
		match self {
			Self::V1 { summary, .. } | Self::V2 { summary, .. } => summary.as_ref(),
		}
	}

	pub fn cohort_key(&self) -> MeasurementCohortKey {
		match self {
			Self::V1 {
				executable_hash,
				scorer_version,
				config_hash,
				..
			} => MeasurementCohortKey::OrchestratorBoundV1 {
				executable_hash: executable_hash.clone(),
				scorer_version: scorer_version.clone(),
				config_hash: config_hash.clone(),
			},
			Self::V2 {
				engine_artifact,
				runner_protocol_version,
				merge_kernel,
				scope,
				scorer_version,
				scorer_config_hash,
				..
			} => MeasurementCohortKey::EngineArtifactV2 {
				engine_artifact: engine_artifact.clone(),
				runner_protocol_version: runner_protocol_version.clone(),
				merge_kernel: *merge_kernel,
				scope: *scope,
				scorer_version: scorer_version.clone(),
				scorer_config_hash: scorer_config_hash.clone(),
			},
		}
	}

	pub fn cohort_id(&self) -> String {
		self.cohort_key().cohort_id()
	}

	pub fn identity_is_valid(&self) -> bool {
		let expected = match self {
			Self::V1 {
				snapshot_id,
				executable_hash,
				scorer_version,
				config_hash,
				..
			} => LegacyMeasurementIdentityV1 {
				snapshot_id: snapshot_id.clone(),
				executable_hash: executable_hash.clone(),
				scorer_version: scorer_version.clone(),
				config_hash: config_hash.clone(),
			}
			.measurement_id(),
			Self::V2 {
				input_version_id,
				engine_artifact,
				runner_protocol_version,
				merge_kernel,
				scope,
				scorer_version,
				scorer_config_hash,
				..
			} => MeasurementIdentityV2 {
				input_version_id: input_version_id.clone(),
				engine_artifact: engine_artifact.clone(),
				runner_protocol_version: runner_protocol_version.clone(),
				merge_kernel: *merge_kernel,
				scope: *scope,
				scorer_version: scorer_version.clone(),
				scorer_config_hash: scorer_config_hash.clone(),
			}
			.measurement_id(),
		};
		self.measurement_id() == expected
	}
}

impl IdentifiedRecord for MeasurementRecord {
	fn record_id(&self) -> &str {
		self.measurement_id()
	}
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct FileResultRecord {
	pub schema: String,
	pub file_result_id: String,
	pub measurement_id: String,
	pub relative_path: String,
	pub result: serde_json::Value,
}

impl FileResultRecord {
	/// Construct payload-bound evidence for a V2 measurement.
	pub fn new(measurement_id: String, relative_path: String, result: serde_json::Value) -> Self {
		Self::new_v2(measurement_id, relative_path, result)
	}

	/// Reconstruct the historical V1 path-only identity without changing
	/// committed Legacy records.
	pub fn new_v1(
		measurement_id: String,
		relative_path: String,
		result: serde_json::Value,
	) -> Self {
		let file_result_id = stable_id(
			"file-result",
			&[measurement_id.as_bytes(), relative_path.as_bytes()],
		);
		Self {
			schema: SCHEMA.to_string(),
			file_result_id,
			measurement_id,
			relative_path,
			result,
		}
	}

	pub fn new_v2(
		measurement_id: String,
		relative_path: String,
		result: serde_json::Value,
	) -> Self {
		let result_bytes = serde_json::to_vec(&result).expect("file-result evidence serializes");
		let file_result_id = stable_id(
			"file-result-v2",
			&[
				measurement_id.as_bytes(),
				relative_path.as_bytes(),
				&result_bytes,
			],
		);
		Self {
			schema: SCHEMA.to_string(),
			file_result_id,
			measurement_id,
			relative_path,
			result,
		}
	}

	pub fn identity_is_valid_for(&self, measurement: &MeasurementRecord) -> bool {
		let expected = match measurement {
			MeasurementRecord::V1 { .. } => Self::new_v1(
				self.measurement_id.clone(),
				self.relative_path.clone(),
				self.result.clone(),
			),
			MeasurementRecord::V2 { .. } => Self::new_v2(
				self.measurement_id.clone(),
				self.relative_path.clone(),
				self.result.clone(),
			),
		};
		*self == expected
	}
}

impl IdentifiedRecord for FileResultRecord {
	fn record_id(&self) -> &str {
		&self.file_result_id
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatasetPaths {
	pub root: PathBuf,
	/// Legacy V1 input/output CAS. New Workshop-backed measurements must not
	/// write to this root.
	pub legacy_objects: PathBuf,
	pub legacy_work: PathBuf,
	pub evidence_objects: PathBuf,
	pub evidence_work: PathBuf,
	pub object_records: PathBuf,
	pub snapshots: PathBuf,
	pub input_versions: PathBuf,
	pub observations: PathBuf,
	pub measurements: PathBuf,
	pub file_results: PathBuf,
	pub shadow_measurements: PathBuf,
	pub annotations: PathBuf,
	pub manifest: PathBuf,
}

impl DatasetPaths {
	pub fn new(root: impl Into<PathBuf>) -> Self {
		let root = root.into();
		Self {
			legacy_objects: root.join("objects"),
			legacy_work: root.join(".work"),
			evidence_objects: root.join("evidence_objects"),
			evidence_work: root.join(".evidence-work"),
			object_records: root.join("object_records.jsonl"),
			snapshots: root.join("snapshots.jsonl"),
			input_versions: root.join("input_versions.jsonl"),
			observations: root.join("observations.jsonl"),
			measurements: root.join("measurements.jsonl"),
			file_results: root.join("file_results.jsonl"),
			shadow_measurements: root.join("shadow_measurements.jsonl"),
			annotations: root.join("annotations.jsonl"),
			manifest: root.join("dataset.json"),
			root,
		}
	}

	/// Create only the metadata and compact-evidence paths used by live
	/// Workshop measurement. The legacy input/output CAS roots are deliberately
	/// untouched so a product run can prove it has no dependency on them.
	pub fn ensure_workshop_layout(&self) -> io::Result<()> {
		fs::create_dir_all(&self.root)?;
		fs::create_dir_all(&self.evidence_objects)?;
		fs::create_dir_all(&self.evidence_work)?;
		for path in [
			&self.input_versions,
			&self.observations,
			&self.measurements,
			&self.file_results,
		] {
			if !path.exists() {
				fs::write(path, b"")?;
			}
		}
		if !self.manifest.exists() {
			let manifest = serde_json::json!({
				"schema": SCHEMA,
				"format": "foch-merge-corpus"
			});
			fs::write(
				&self.manifest,
				format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
			)?;
		}
		Ok(())
	}
}

pub trait IdentifiedRecord {
	fn record_id(&self) -> &str;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendOutcome {
	Inserted,
	AlreadyPresent,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AppendSummary {
	pub inserted: usize,
	pub already_present: usize,
}

pub fn append_unique<T>(path: &Path, record: &T) -> io::Result<AppendOutcome>
where
	T: DeserializeOwned + IdentifiedRecord + PartialEq + Serialize,
{
	let summary = append_unique_many(path, std::slice::from_ref(record))?;
	Ok(if summary.inserted == 1 {
		AppendOutcome::Inserted
	} else {
		AppendOutcome::AlreadyPresent
	})
}

/// Append one V2 ACF observation to the mixed historical observation stream.
///
/// Using the compatibility enum for the index scan is required because the
/// existing prefix contains V1 records with a different wire shape.
pub fn append_workshop_observation_v2(
	path: &Path,
	record: &WorkshopObservationRecordV2,
) -> io::Result<AppendOutcome> {
	append_unique(path, &WorkshopObservationRecord::V2(record.clone()))
}

/// Append a batch with one lock, one index scan, and one atomic rewrite. This
/// avoids quadratic I/O for per-file measurement records.
pub fn append_unique_many<T>(path: &Path, records: &[T]) -> io::Result<AppendSummary>
where
	T: DeserializeOwned + IdentifiedRecord + PartialEq + Serialize,
{
	if records.is_empty() {
		return Ok(AppendSummary::default());
	}
	let parent = path.parent().ok_or_else(|| {
		io::Error::new(
			ErrorKind::InvalidInput,
			format!("JSONL path has no parent: {}", path.display()),
		)
	})?;
	fs::create_dir_all(parent)?;
	let _lock = DatasetLock::acquire(parent)?;

	let existing = read_jsonl::<T>(path)?;
	let existing_by_id: HashMap<&str, &T> = existing
		.iter()
		.map(|record| (record.record_id(), record))
		.collect();
	let mut pending_by_id: HashMap<&str, &T> = HashMap::new();
	let mut inserted = Vec::new();
	let mut already_present = 0_usize;
	for record in records {
		if let Some(found) = existing_by_id.get(record.record_id()) {
			if *found == record {
				already_present += 1;
				continue;
			}
			return Err(io::Error::new(
				ErrorKind::AlreadyExists,
				format!(
					"record ID {} already exists with different content",
					record.record_id()
				),
			));
		}
		if let Some(found) = pending_by_id.get(record.record_id()) {
			if *found == record {
				already_present += 1;
				continue;
			}
			return Err(io::Error::new(
				ErrorKind::AlreadyExists,
				format!(
					"batch contains record ID {} with different content",
					record.record_id()
				),
			));
		}
		pending_by_id.insert(record.record_id(), record);
		inserted.push(record);
	}
	if inserted.is_empty() {
		return Ok(AppendSummary {
			inserted: 0,
			already_present,
		});
	}

	let mut output = if path.is_file() {
		fs::read(path)?
	} else {
		Vec::new()
	};
	if !output.is_empty() && !output.ends_with(b"\n") {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!(
				"JSONL file has an incomplete final line: {}",
				path.display()
			),
		));
	}
	for record in &inserted {
		serde_json::to_writer(&mut output, record).map_err(io::Error::other)?;
		output.push(b'\n');
	}
	atomic_write(path, &output)?;
	Ok(AppendSummary {
		inserted: inserted.len(),
		already_present,
	})
}

pub fn read_jsonl<T>(path: &Path) -> io::Result<Vec<T>>
where
	T: DeserializeOwned,
{
	if !path.exists() {
		return Ok(Vec::new());
	}
	let text = fs::read_to_string(path)?;
	text.lines()
		.enumerate()
		.filter(|(_, line)| !line.trim().is_empty())
		.map(|(index, line)| {
			serde_json::from_str(line).map_err(|err| {
				io::Error::new(
					ErrorKind::InvalidData,
					format!("{}:{}: {err}", path.display(), index + 1),
				)
			})
		})
		.collect()
}

pub fn stable_id(namespace: &str, parts: &[&[u8]]) -> String {
	let mut hasher = blake3::Hasher::new();
	hasher.update(&(namespace.len() as u64).to_le_bytes());
	hasher.update(namespace.as_bytes());
	for part in parts {
		hasher.update(&(part.len() as u64).to_le_bytes());
		hasher.update(part);
	}
	hasher.finalize().to_hex().to_string()
}

fn is_blake3_hash(value: &str) -> bool {
	value.len() == 64
		&& value
			.bytes()
			.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn now_rfc3339() -> String {
	OffsetDateTime::now_utc()
		.format(&Rfc3339)
		.expect("RFC3339 formatting is infallible for UTC timestamps")
}

fn atomic_write(path: &Path, content: &[u8]) -> io::Result<()> {
	let parent = path.parent().expect("validated parent");
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_nanos();
	let temp = parent.join(format!(".jsonl-{}-{nanos}.tmp", std::process::id()));
	fs::write(&temp, content)?;
	fs::rename(&temp, path)
}

struct DatasetLock {
	_file: fs::File,
}

impl DatasetLock {
	fn acquire(root: &Path) -> io::Result<Self> {
		let path = root.join(".lock");
		let file = fs::OpenOptions::new()
			.create(true)
			.read(true)
			.write(true)
			.truncate(false)
			.open(path)?;
		let started = Instant::now();
		loop {
			match try_lock_exclusive(&file) {
				Ok(()) => return Ok(Self { _file: file }),
				Err(err) if err.kind() == ErrorKind::WouldBlock => {
					if started.elapsed() >= Duration::from_secs(30) {
						return Err(io::Error::new(
							ErrorKind::WouldBlock,
							"timed out waiting for dataset append lock after 30 seconds",
						));
					}
					thread::sleep(Duration::from_millis(25));
				}
				Err(err) => return Err(err),
			}
		}
	}
}

fn try_lock_exclusive(file: &fs::File) -> io::Result<()> {
	Ok(file.try_lock()?)
}

#[cfg(test)]
mod tests {
	use std::collections::{BTreeMap, BTreeSet, HashMap};

	use serde::{Deserialize, Serialize};

	use super::*;

	#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
	struct Record {
		id: String,
		value: String,
	}

	impl IdentifiedRecord for Record {
		fn record_id(&self) -> &str {
			&self.id
		}
	}

	#[test]
	fn append_is_idempotent_and_rejects_id_collisions() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("records.jsonl");
		let record = Record {
			id: "same".to_string(),
			value: "first".to_string(),
		};
		assert_eq!(
			append_unique(&path, &record).unwrap(),
			AppendOutcome::Inserted
		);
		assert_eq!(
			append_unique(&path, &record).unwrap(),
			AppendOutcome::AlreadyPresent
		);
		let collision = Record {
			id: "same".to_string(),
			value: "different".to_string(),
		};
		assert_eq!(
			append_unique(&path, &collision).unwrap_err().kind(),
			ErrorKind::AlreadyExists
		);
		assert_eq!(read_jsonl::<Record>(&path).unwrap(), vec![record]);
	}

	#[test]
	fn batch_append_deduplicates_without_rewriting_per_record() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("records.jsonl");
		let records = vec![
			Record {
				id: "a".to_string(),
				value: "first".to_string(),
			},
			Record {
				id: "b".to_string(),
				value: "second".to_string(),
			},
		];
		assert_eq!(
			append_unique_many(&path, &records).unwrap(),
			AppendSummary {
				inserted: 2,
				already_present: 0,
			}
		);
		assert_eq!(
			append_unique_many(&path, &records).unwrap(),
			AppendSummary {
				inserted: 0,
				already_present: 2,
			}
		);
		assert_eq!(read_jsonl::<Record>(&path).unwrap(), records);
	}

	#[test]
	fn dataset_lock_is_exclusive_and_released_with_its_file_handle() {
		let temp = tempfile::tempdir().unwrap();
		let first = DatasetLock::acquire(temp.path()).unwrap();
		let second_file = fs::OpenOptions::new()
			.read(true)
			.write(true)
			.open(temp.path().join(".lock"))
			.unwrap();

		assert_eq!(
			try_lock_exclusive(&second_file).unwrap_err().kind(),
			ErrorKind::WouldBlock
		);
		drop(first);
		try_lock_exclusive(&second_file).unwrap();
	}

	#[test]
	fn stable_ids_are_order_sensitive_and_namespace_scoped() {
		let ab = stable_id("snapshot", &[b"a", b"b"]);
		let ba = stable_id("snapshot", &[b"b", b"a"]);
		let measurement = stable_id("measurement", &[b"a", b"b"]);
		assert_ne!(ab, ba);
		assert_ne!(ab, measurement);
		assert_eq!(ab, stable_id("snapshot", &[b"a", b"b"]));
	}

	#[test]
	fn workshop_layout_initialization_is_repeatable_without_legacy_cas() {
		let temp = tempfile::tempdir().unwrap();
		let paths = DatasetPaths::new(temp.path().join("dataset"));
		paths.ensure_workshop_layout().unwrap();
		paths.ensure_workshop_layout().unwrap();
		assert!(!paths.legacy_objects.exists());
		assert!(!paths.legacy_work.exists());
		assert!(paths.evidence_objects.is_dir());
		assert!(paths.evidence_work.is_dir());
		assert!(paths.input_versions.is_file());
		let manifest: serde_json::Value =
			serde_json::from_str(&fs::read_to_string(paths.manifest).unwrap()).unwrap();
		assert_eq!(manifest["schema"], SCHEMA);
	}

	fn snapshot(source_ids: &[&str]) -> SnapshotRecord {
		SnapshotRecord::new(
			"case-1".to_string(),
			GameIdentity {
				app_id: 236850,
				version: "1.37.5".to_string(),
				steam_build_id: Some(42),
			},
			SnapshotObjectRef {
				workshop_id: "compatch".to_string(),
				content_hash: "c".repeat(64),
			},
			source_ids
				.iter()
				.map(|id| SnapshotObjectRef {
					workshop_id: (*id).to_string(),
					content_hash: id.repeat(64),
				})
				.collect(),
		)
	}

	fn steam_id(value: &str) -> SteamId {
		value.parse().unwrap()
	}

	fn workshop_input(workshop_id: &str, manifest_id: &str) -> WorkshopInputVersionV2 {
		WorkshopInputVersionV2 {
			workshop_id: steam_id(workshop_id),
			manifest_id: steam_id(manifest_id),
		}
	}

	fn input_version(source_mods: Vec<WorkshopInputVersionV2>) -> InputVersionRecord {
		InputVersionRecord::new(
			"case-1".to_string(),
			GameInputIdentityV2 {
				app_id: steam_id("236850"),
				version: "1.37.5".to_string(),
				steam_build_id: Some(steam_id("18446744073709551615")),
			},
			workshop_input("3630876155", "9007199254740993"),
			source_mods,
		)
	}

	fn v2_identity() -> MeasurementIdentityV2 {
		MeasurementIdentityV2 {
			input_version_id: "input-version".to_string(),
			engine_artifact: EngineArtifactIdentity::foch_executable_blake3("artifact"),
			runner_protocol_version: "1.0.0".to_string(),
			merge_kernel: MeasurementKernel::LegacyAddressPatchReference,
			scope: MeasurementScope::FullProductMerge,
			scorer_version: SCORER_VERSION.to_string(),
			scorer_config_hash: "config".to_string(),
		}
	}

	#[derive(Clone, Copy)]
	struct V2IdentityParts<'a> {
		input_version_id: &'a str,
		engine_artifact_kind: &'a str,
		engine_artifact_hash_algorithm: &'a str,
		engine_artifact_hash: &'a str,
		runner_protocol_version: &'a str,
		merge_kernel: &'a str,
		scope: &'a str,
		scorer_version: &'a str,
		scorer_config_hash: &'a str,
	}

	impl V2IdentityParts<'_> {
		fn cohort_id(self) -> String {
			stable_id(
				"measurement-cohort-v2",
				&[
					self.engine_artifact_kind.as_bytes(),
					self.engine_artifact_hash_algorithm.as_bytes(),
					self.engine_artifact_hash.as_bytes(),
					self.runner_protocol_version.as_bytes(),
					self.merge_kernel.as_bytes(),
					self.scope.as_bytes(),
					self.scorer_version.as_bytes(),
					self.scorer_config_hash.as_bytes(),
				],
			)
		}

		fn measurement_id(self) -> String {
			let cohort_id = self.cohort_id();
			stable_id(
				"measurement-v2",
				&[self.input_version_id.as_bytes(), cohort_id.as_bytes()],
			)
		}
	}

	#[test]
	fn snapshot_identity_is_repeatable_and_preserves_source_order() {
		let first = snapshot(&["a", "b"]);
		let repeated = snapshot(&["a", "b"]);
		let reordered = snapshot(&["b", "a"]);
		assert_eq!(first, repeated);
		assert!(first.identity_is_valid());
		assert_ne!(first.snapshot_id, reordered.snapshot_id);

		let mut altered = first;
		altered.compatch.content_hash = "altered".to_string();
		assert!(!altered.identity_is_valid());
	}

	#[test]
	fn workshop_input_version_binds_canonical_acf_manifests_and_order() {
		let first = input_version(vec![
			workshop_input("111", "1001"),
			workshop_input("222", "1002"),
		]);
		assert!(first.identity_is_valid());
		assert_eq!(first, input_version(first.source_mods.clone()));

		let mut changed_manifest = first.clone();
		changed_manifest.source_mods[0].manifest_id = steam_id("2001");
		assert!(!changed_manifest.identity_is_valid());
		let changed_manifest = InputVersionRecord::new(
			changed_manifest.case_id,
			changed_manifest.game,
			changed_manifest.compatch,
			changed_manifest.source_mods,
		);
		assert_ne!(first.input_version_id, changed_manifest.input_version_id);

		let reordered = input_version(first.source_mods.iter().cloned().rev().collect());
		assert_ne!(first.input_version_id, reordered.input_version_id);

		let wire = serde_json::to_value(&first).unwrap();
		assert_eq!(wire["game"]["app_id"], "236850");
		assert_eq!(wire["game"]["steam_build_id"], "18446744073709551615");
		assert_eq!(wire["compatch"]["manifest_id"], "9007199254740993");
		assert!(wire["compatch"].get("product_input_digest").is_none());
		assert!(wire["game"]["app_id"].is_string());
	}

	#[test]
	fn steam_ids_reject_noncanonical_or_lossy_wire_values() {
		for invalid in ["", "-1", "+1", "01", "1.0", "18446744073709551616"] {
			assert!(invalid.parse::<SteamId>().is_err(), "{invalid}");
		}
		assert!(serde_json::from_str::<SteamId>("236850").is_err());
		assert_eq!(
			serde_json::from_str::<SteamId>("\"9007199254740993\"")
				.unwrap()
				.as_str(),
			"9007199254740993"
		);
	}

	#[test]
	fn workshop_v2_observation_appends_without_rewriting_v1_bytes() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("observations.jsonl");
		let v1 = ObservationRecord::new(
			"snapshot".to_string(),
			"2026-01-01T00:00:00Z".to_string(),
			WorkshopObservation {
				workshop_id: "3630876155".to_string(),
				title: "compatch".to_string(),
				time_created: 1,
				time_updated: 2,
				provenance: WorkshopProvenance::default(),
			},
			Vec::new(),
			10,
			false,
		);
		let v1_line = serde_json::to_string(&v1).unwrap();
		fs::write(&path, format!("{v1_line}\n")).unwrap();
		let input = input_version(Vec::new());
		let v2 = WorkshopObservationRecordV2::new(
			input.input_version_id.clone(),
			"2026-08-08T00:00:00Z".to_string(),
			WorkshopItemObservationV2 {
				workshop_id: steam_id("3630876155"),
				manifest_id: steam_id("9007199254740993"),
				time_updated: 1_754_608_753,
				size_bytes: 42,
				ugc_handle: Some(steam_id("18446744073709551615")),
			},
			Vec::new(),
		);
		assert!(v2.identity_is_valid());
		assert!(v2.matches_input_version(&input));
		assert_eq!(
			append_workshop_observation_v2(&path, &v2).unwrap(),
			AppendOutcome::Inserted
		);

		let bytes = fs::read_to_string(&path).unwrap();
		assert!(bytes.starts_with(&format!("{v1_line}\n")));
		let records = read_jsonl::<WorkshopObservationRecord>(&path).unwrap();
		assert!(matches!(records[0], WorkshopObservationRecord::V1(_)));
		assert!(matches!(records[1], WorkshopObservationRecord::V2(_)));
		assert_eq!(records[1].schema(), WORKSHOP_OBSERVATION_SCHEMA_V2);
	}

	#[test]
	fn v2_file_result_identity_binds_the_full_payload() {
		let first = FileResultRecord::new(
			"measurement".to_string(),
			"common/a.txt".to_string(),
			serde_json::json!({"score": {"verdict": "exact"}}),
		);
		let changed = FileResultRecord::new(
			"measurement".to_string(),
			"common/a.txt".to_string(),
			serde_json::json!({"score": {"verdict": "semantic"}}),
		);

		assert_ne!(first.file_result_id, changed.file_result_id);
	}

	#[test]
	fn measurement_identity_is_content_addressed() {
		let make = |started_at: &str| {
			MeasurementRecord::new_v1(
				LegacyMeasurementIdentityV1 {
					snapshot_id: "snapshot".to_string(),
					executable_hash: "executable".to_string(),
					scorer_version: SCORER_VERSION.to_string(),
					config_hash: "config".to_string(),
				},
				started_at.to_string(),
				"finished".to_string(),
				TerminalStatus::Completed,
				None,
				None,
				None,
			)
		};
		assert_eq!(
			make("first").measurement_id(),
			make("later").measurement_id()
		);
		assert!(TerminalStatus::TimedOut.counts_as_merge_failed());
		assert!(!TerminalStatus::Completed.counts_as_merge_failed());
	}

	#[test]
	fn historical_measurement_wire_format_round_trips_byte_for_byte() {
		let line = concat!(
			"{\"schema\":\"1.0.0\",",
			"\"measurement_id\":\"93e40b750e503c261c083dbbe2fcd3f1ab81f2851f409534651f0f01cf8e7d6b\",",
			"\"snapshot_id\":\"snapshot\",",
			"\"executable_hash\":\"executable\",",
			"\"scorer_version\":\"1.0.0\",",
			"\"config_hash\":\"config\",",
			"\"started_at\":\"started\",",
			"\"finished_at\":\"finished\",",
			"\"status\":\"completed\",",
			"\"detail\":null,",
			"\"merged_output_hash\":null,",
			"\"summary\":null}"
		);
		let record: MeasurementRecord = serde_json::from_str(line).unwrap();
		assert_eq!(record.schema(), MEASUREMENT_SCHEMA_V1);
		assert!(record.identity_is_valid());
		assert_eq!(serde_json::to_string(&record).unwrap(), line);
	}

	#[test]
	fn v2_identity_sensitivity_matrix_covers_every_semantic_input() {
		let identity = v2_identity();
		let cohort_id = identity.cohort_id();
		let measurement_id = identity.measurement_id();
		let parts = V2IdentityParts {
			input_version_id: &identity.input_version_id,
			engine_artifact_kind: identity.engine_artifact.kind.as_str(),
			engine_artifact_hash_algorithm: identity.engine_artifact.hash_algorithm.as_str(),
			engine_artifact_hash: &identity.engine_artifact.hash,
			runner_protocol_version: &identity.runner_protocol_version,
			merge_kernel: identity.merge_kernel.as_str(),
			scope: identity.scope.as_str(),
			scorer_version: &identity.scorer_version,
			scorer_config_hash: &identity.scorer_config_hash,
		};
		assert_eq!(parts.cohort_id(), cohort_id);
		assert_eq!(parts.measurement_id(), measurement_id);

		let mutations = [
			(
				"input_version_id",
				V2IdentityParts {
					input_version_id: "other-input-version",
					..parts
				},
				false,
			),
			(
				"engine artifact kind",
				V2IdentityParts {
					engine_artifact_kind: "other_engine_artifact",
					..parts
				},
				true,
			),
			(
				"engine artifact hash algorithm",
				V2IdentityParts {
					engine_artifact_hash_algorithm: "other_hash_algorithm",
					..parts
				},
				true,
			),
			(
				"engine artifact digest",
				V2IdentityParts {
					engine_artifact_hash: "other-artifact",
					..parts
				},
				true,
			),
			(
				"runner protocol",
				V2IdentityParts {
					runner_protocol_version: "2.0.0",
					..parts
				},
				true,
			),
			(
				"merge kernel",
				V2IdentityParts {
					merge_kernel: MeasurementKernel::SemanticTree.as_str(),
					..parts
				},
				true,
			),
			(
				"scope",
				V2IdentityParts {
					scope: "other_scope",
					..parts
				},
				true,
			),
			(
				"scorer version",
				V2IdentityParts {
					scorer_version: "different-scorer-version",
					..parts
				},
				true,
			),
			(
				"scorer config hash",
				V2IdentityParts {
					scorer_config_hash: "other-config",
					..parts
				},
				true,
			),
		];
		for (field, mutated, changes_cohort) in mutations {
			assert_ne!(
				mutated.measurement_id(),
				measurement_id,
				"{field} must affect measurement identity"
			);
			if changes_cohort {
				assert_ne!(
					mutated.cohort_id(),
					cohort_id,
					"{field} must affect cohort identity"
				);
			} else {
				assert_eq!(
					mutated.cohort_id(),
					cohort_id,
					"{field} is measurement-specific"
				);
			}
		}

		let record = MeasurementRecord::new_v2(
			identity.clone(),
			"started".to_string(),
			"finished".to_string(),
			TerminalStatus::Completed,
			None,
			None,
			None,
		);

		assert_eq!(record.schema(), MEASUREMENT_SCHEMA_V2);
		assert_eq!(record.scorer_version(), SCORER_VERSION);
		assert_eq!(record.runner_protocol_version(), Some("1.0.0"));
		assert_eq!(record.cohort_id(), cohort_id);
		assert_eq!(record.measurement_id(), measurement_id);
		assert_eq!(
			record.merge_kernel(),
			Some(MeasurementKernel::LegacyAddressPatchReference)
		);
		assert!(record.identity_is_valid());
		let wire = serde_json::to_value(&record).unwrap();
		assert_eq!(wire["runner_protocol_version"], "1.0.0");
		assert!(wire.get("worker_protocol_version").is_none());
		assert_eq!(wire["merge_kernel"], "legacy_address_patch_reference");
		assert_eq!(wire["scope"], "full_product_merge");
		assert_eq!(wire["schema"], MEASUREMENT_SCHEMA_V2);
		assert_eq!(wire["input_version_id"], "input-version");
		assert!(wire.get("snapshot_id").is_none());
		assert!(wire.get("evidence_bundle_hash").is_some());
		assert!(wire.get("merged_output_hash").is_none());

		let mut structured = identity;
		structured.merge_kernel = MeasurementKernel::SemanticTree;
		assert_ne!(structured.cohort_id(), cohort_id);
		assert_ne!(structured.measurement_id(), measurement_id);
		let structured = MeasurementRecord::new_v2(
			structured,
			"started".to_string(),
			"finished".to_string(),
			TerminalStatus::Completed,
			None,
			None,
			None,
		);
		assert_eq!(
			serde_json::to_value(structured).unwrap()["merge_kernel"],
			"semantic_tree"
		);
	}

	#[test]
	fn v2_evidence_reference_matches_terminal_status() {
		let record = |status: TerminalStatus, evidence_bundle_hash: Option<String>| {
			MeasurementRecord::new_v2(
				v2_identity(),
				"started".to_string(),
				"finished".to_string(),
				status,
				None,
				evidence_bundle_hash,
				None,
			)
		};
		assert!(
			record(TerminalStatus::Completed, Some("a".repeat(64))).evidence_reference_is_valid()
		);
		assert!(!record(TerminalStatus::Completed, None).evidence_reference_is_valid());
		assert!(record(TerminalStatus::Crashed, None).evidence_reference_is_valid());
		assert!(
			!record(TerminalStatus::Crashed, Some("a".repeat(64))).evidence_reference_is_valid()
		);
		assert!(
			!record(TerminalStatus::Completed, Some("invalid".to_string()))
				.evidence_reference_is_valid()
		);
	}

	#[test]
	fn v2_operational_record_fields_do_not_affect_identity() {
		let identity = v2_identity();
		let baseline = MeasurementRecord::new_v2(
			identity.clone(),
			"2026-01-01T00:00:00Z".to_string(),
			"2026-01-01T00:00:01Z".to_string(),
			TerminalStatus::Completed,
			None,
			None,
			None,
		);
		let operationally_different = MeasurementRecord::new_v2(
			identity,
			"2026-02-02T00:00:00Z".to_string(),
			"2026-02-02T00:01:00Z".to_string(),
			TerminalStatus::Fatal,
			Some("different detail".to_string()),
			Some("different merged output".to_string()),
			Some(MeasurementSummary {
				merge_status: Some("failed".to_string()),
				ground_truth_files: 1,
				multi_source_files: 2,
				accepted_ground_truth_files: 3,
				accepted_multi_source_files: 4,
				all_ground_truth_verdicts: BTreeMap::from([("different".to_string(), 5)]),
				multi_source_verdicts: BTreeMap::from([("different".to_string(), 6)]),
				setup_ms: 7,
				merge_ms: 8,
				scoring_ms: 9,
				total_ms: 10,
			}),
		);

		assert_ne!(baseline, operationally_different);
		assert_eq!(
			baseline.measurement_id(),
			operationally_different.measurement_id()
		);
		assert_eq!(baseline.cohort_id(), operationally_different.cohort_id());
		assert!(baseline.identity_is_valid());
		assert!(operationally_different.identity_is_valid());
	}

	#[test]
	fn appending_v2_does_not_rewrite_existing_v1_bytes() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("measurements.jsonl");
		let v1_line = concat!(
			"{\"schema\":\"1.0.0\",",
			"\"measurement_id\":\"93e40b750e503c261c083dbbe2fcd3f1ab81f2851f409534651f0f01cf8e7d6b\",",
			"\"snapshot_id\":\"snapshot\",",
			"\"executable_hash\":\"executable\",",
			"\"scorer_version\":\"1.0.0\",",
			"\"config_hash\":\"config\",",
			"\"started_at\":\"started\",",
			"\"finished_at\":\"finished\",",
			"\"status\":\"completed\",",
			"\"detail\":null,",
			"\"merged_output_hash\":null,",
			"\"summary\":null}"
		);
		fs::write(&path, format!("{v1_line}\n")).unwrap();
		let v2 = MeasurementRecord::new_v2(
			MeasurementIdentityV2 {
				input_version_id: "other-input-version".to_string(),
				engine_artifact: EngineArtifactIdentity::foch_executable_blake3("artifact"),
				runner_protocol_version: "1.0.0".to_string(),
				merge_kernel: MeasurementKernel::LegacyAddressPatchReference,
				scope: MeasurementScope::FullProductMerge,
				scorer_version: SCORER_VERSION.to_string(),
				scorer_config_hash: "config".to_string(),
			},
			"started".to_string(),
			"finished".to_string(),
			TerminalStatus::Completed,
			None,
			None,
			None,
		);

		assert_eq!(append_unique(&path, &v2).unwrap(), AppendOutcome::Inserted);
		let bytes = fs::read_to_string(&path).unwrap();
		assert!(bytes.starts_with(&format!("{v1_line}\n")));
		let records = read_jsonl::<MeasurementRecord>(&path).unwrap();
		assert_eq!(records.len(), 2);
		assert_eq!(records[0].schema(), MEASUREMENT_SCHEMA_V1);
		assert_eq!(records[1].schema(), MEASUREMENT_SCHEMA_V2);
	}

	#[test]
	fn frozen_v1_evidence_has_two_complete_cohorts_and_valid_foreign_keys() {
		struct FrozenCohort {
			executable_hash: &'static str,
			scorer_version: &'static str,
			config_hash: &'static str,
			expected_file_results: usize,
		}

		const COHORTS: [FrozenCohort; 2] = [
			FrozenCohort {
				executable_hash: "16fcde0535ad3c759492f1aa76ad6164d466cb6fea8125a65f36c3bebb06ea91",
				scorer_version: "1.0.0",
				config_hash: "e2580bc8c745bf7aca520ce909f093028455a9745d5fae6f92b94424d2986393",
				expected_file_results: 30_739,
			},
			FrozenCohort {
				executable_hash: "0507a19de246a59bd2f718ad2941fd4d0c9ec07d469ab911a1e6b04bb11ba519",
				scorer_version: "1.3.0",
				config_hash: "8beffefe06b044798b769b805fb556dd93769ebdbf367df3d6468ef6834d5665",
				expected_file_results: 29_481,
			},
		];

		let dataset_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("dataset");
		let measurements =
			read_jsonl::<MeasurementRecord>(&dataset_root.join("measurements.jsonl")).unwrap();
		let v1_measurements: Vec<&MeasurementRecord> = measurements
			.iter()
			.filter(|measurement| measurement.schema() == MEASUREMENT_SCHEMA_V1)
			.collect();
		assert_eq!(v1_measurements.len(), 46);
		let v1_measurement_ids: BTreeSet<&str> = v1_measurements
			.iter()
			.copied()
			.map(MeasurementRecord::measurement_id)
			.collect();
		assert_eq!(v1_measurement_ids.len(), 46);

		for cohort in &COHORTS {
			let cohort_measurements: Vec<&MeasurementRecord> = v1_measurements
				.iter()
				.copied()
				.filter(|measurement| {
					measurement.legacy_executable_hash() == Some(cohort.executable_hash)
						&& measurement.scorer_version() == cohort.scorer_version
						&& measurement.config_hash() == cohort.config_hash
				})
				.collect();
			assert_eq!(cohort_measurements.len(), 23, "{}", cohort.scorer_version);
			for measurement in cohort_measurements {
				assert_eq!(measurement.schema(), MEASUREMENT_SCHEMA_V1);
				assert!(measurement.identity_is_valid());
			}
		}

		let measurement_by_id: HashMap<&str, &MeasurementRecord> = measurements
			.iter()
			.map(|measurement| (measurement.measurement_id(), measurement))
			.collect();
		let file_results =
			read_jsonl::<FileResultRecord>(&dataset_root.join("file_results.jsonl")).unwrap();
		let mut file_result_counts = BTreeMap::<&str, usize>::new();
		for file_result in &file_results {
			let measurement = measurement_by_id
				.get(file_result.measurement_id.as_str())
				.unwrap_or_else(|| {
					panic!(
						"file result {} references missing measurement {}",
						file_result.file_result_id, file_result.measurement_id
					)
				});
			if measurement.schema() == MEASUREMENT_SCHEMA_V1 {
				*file_result_counts
					.entry(measurement.scorer_version())
					.or_default() += 1;
			}
		}
		assert_eq!(file_result_counts.values().sum::<usize>(), 60_220);
		assert_eq!(
			file_result_counts,
			COHORTS
				.iter()
				.map(|cohort| (cohort.scorer_version, cohort.expected_file_results))
				.collect()
		);
	}

	#[test]
	fn committed_v1_evidence_jsonl_prefixes_are_frozen() {
		let dataset_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("dataset");
		let files: [(&str, usize, &str); 5] = [
			(
				"snapshots.jsonl",
				14_882,
				"3305e28717526ca30995c04d88aa20ea2b97eb9738bb28d6bc77d387e99ae109",
			),
			(
				"observations.jsonl",
				77_958,
				"05df761e1128c32f4c58c6616460a34fe0cb401f67d7ca2c817827770f29623e",
			),
			(
				"measurements.jsonl",
				43_182,
				"bf7d4fd1b01389bd67f8cf430fc7441c3991bbb309f6e4f1c094861c4bdec255",
			),
			(
				"file_results.jsonl",
				35_906_768,
				"1bae838fabbf0989843c0c6ac5b159a05a622415522ecb1b9140044e3743f811",
			),
			(
				"object_records.jsonl",
				34_798,
				"6db356013a6d8f873bb83ce3bbae0d600ef9c9dd47e634ffee7d9eb68c46590e",
			),
		];
		for (name, frozen_prefix_len, expected_hash) in files {
			let bytes = fs::read(dataset_root.join(name)).unwrap();
			assert!(
				bytes.len() >= frozen_prefix_len,
				"{name} is shorter than its frozen V1 prefix"
			);
			let frozen_prefix = &bytes[..frozen_prefix_len];
			assert_eq!(
				frozen_prefix.last(),
				Some(&b'\n'),
				"{name} frozen V1 prefix must end at a JSONL record boundary"
			);
			assert_eq!(
				blake3::hash(frozen_prefix).to_hex().to_string(),
				expected_hash,
				"{name} frozen V1 prefix changed"
			);
		}
	}
}
