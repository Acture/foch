use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::analysis::Severity;
use crate::config::AppliedDepOverride;
use crate::utils::steam::WorkshopInstallIdentity;

pub const MERGED_MOD_DESCRIPTOR_PATH: &str = "descriptor.mod";
pub const MERGE_PLAN_ARTIFACT_PATH: &str = ".foch/foch-merge-plan.json";
pub const MERGE_REPORT_ARTIFACT_PATH: &str = ".foch/foch-merge-report.json";
pub const MERGE_PROVENANCE_ARTIFACT_PATH: &str = ".foch/foch-provenance.json";
pub const MERGE_TRACE_ARTIFACT_PATH: &str = ".foch/foch-merge-trace.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergePlanFormat {
	Text,
	Json,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergePlanStrategy {
	#[default]
	CopyThrough,
	LastWriterOverlay,
	StructuralMerge,
	/// Key-level dedup merge for `localisation/**.yml` files. Each merged
	/// file contains the union of keys from all contributors; on key
	/// collision the highest-precedence contributor wins.
	LocalisationMerge,
	ManualConflict,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanContributor {
	pub mod_id: String,
	pub source_path: String,
	pub precedence: usize,
	pub is_base_game: bool,
}

#[derive(Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct MergeUnitId {
	pub family_id: String,
	pub module_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergePlanTarget {
	File {
		path: String,
	},
	Module {
		id: MergeUnitId,
		input_paths: Vec<String>,
		output_path: String,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		replace_prefix: Option<String>,
	},
}

impl MergePlanTarget {
	pub fn output_path(&self) -> &str {
		match self {
			Self::File { path } => path,
			Self::Module { output_path, .. } => output_path,
		}
	}

	pub fn module_id(&self) -> Option<&MergeUnitId> {
		match self {
			Self::File { .. } => None,
			Self::Module { id, .. } => Some(id),
		}
	}

	pub fn input_paths(&self) -> &[String] {
		match self {
			Self::File { path } => std::slice::from_ref(path),
			Self::Module { input_paths, .. } => input_paths,
		}
	}

	pub fn replace_prefix(&self) -> Option<&str> {
		match self {
			Self::File { .. } => None,
			Self::Module { replace_prefix, .. } => replace_prefix.as_deref(),
		}
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergePlanEntry {
	pub target: MergePlanTarget,
	pub strategy: MergePlanStrategy,
	pub contributors: Vec<MergePlanContributor>,
	pub winner: Option<MergePlanContributor>,
	#[serde(default)]
	pub notes: Vec<String>,
}

impl MergePlanEntry {
	pub fn output_path(&self) -> &str {
		self.target.output_path()
	}
}

#[cfg(test)]
mod tests {
	use super::{
		MergePlanEntry, MergePlanStrategy, MergePlanTarget, MergeUnitId, ProductInputManifest,
		ProductInputMod,
	};
	use crate::utils::steam::{SteamId, WorkshopInstallIdentity};

	#[test]
	fn module_target_serializes_every_required_runtime_field() {
		let entry = MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: "governments".to_string(),
					module_name: "governments".to_string(),
				},
				input_paths: vec!["common/governments/00_governments.txt".to_string()],
				output_path: "common/governments/zzz_foch_governments.txt".to_string(),
				replace_prefix: Some("common/governments".to_string()),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: Vec::new(),
			winner: None,
			notes: Vec::new(),
		};

		let json = serde_json::to_value(&entry).expect("serialize merge plan entry");
		assert_eq!(json["target"]["kind"], "module");
		assert_eq!(
			json["target"]["output_path"],
			"common/governments/zzz_foch_governments.txt"
		);
		assert_eq!(json["target"]["replace_prefix"], "common/governments");
	}

	#[test]
	fn file_target_exposes_its_output_path() {
		let target = MergePlanTarget::File {
			path: "common/scripted_effects/example.txt".to_string(),
		};

		assert_eq!(target.output_path(), "common/scripted_effects/example.txt");
		assert!(target.module_id().is_none());
	}

	#[test]
	fn product_input_manifest_digest_binds_mod_order_and_acf_version() {
		let first = ProductInputMod {
			mod_id: "mod-a".to_string(),
			precedence: 1,
			workshop_identity: WorkshopInstallIdentity {
				app_id: 236_850,
				workshop_id: SteamId::new(1_001),
				manifest_id: SteamId::new(2_001),
			},
		};
		let second = ProductInputMod {
			mod_id: "mod-b".to_string(),
			precedence: 2,
			workshop_identity: WorkshopInstallIdentity {
				app_id: 236_850,
				workshop_id: SteamId::new(1_002),
				manifest_id: SteamId::new(2_002),
			},
		};
		let manifest = ProductInputManifest::new(vec![first.clone(), second.clone()]);
		let reordered = ProductInputManifest::new(vec![second, first.clone()]);
		let mut changed = first;
		changed.workshop_identity.manifest_id = SteamId::new(2_003);
		let changed = ProductInputManifest::new(vec![changed]);

		assert_eq!(manifest.digest.len(), 64);
		assert_ne!(manifest.digest, reordered.digest);
		assert_ne!(manifest.digest, changed.digest);
		assert_eq!(manifest.attestation().mod_count, 2);
	}
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanStrategies {
	pub total_paths: usize,
	pub copy_through: usize,
	pub last_writer_overlay: usize,
	pub structural_merge: usize,
	#[serde(default)]
	pub localisation_merge: usize,
	pub manual_conflict: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanResult {
	pub game: String,
	pub playset_name: String,
	pub generated_at: String,
	pub include_game_base: bool,
	pub strategies: MergePlanStrategies,
	pub paths: Vec<MergePlanEntry>,
	#[serde(skip_serializing, skip_deserializing)]
	pub fatal_errors: Vec<String>,
}

impl MergePlanResult {
	pub fn has_fatal_errors(&self) -> bool {
		!self.fatal_errors.is_empty()
	}

	pub fn has_manual_conflicts(&self) -> bool {
		self.strategies.manual_conflict > 0
	}

	pub fn push_fatal_error(&mut self, message: impl Into<String>) {
		self.fatal_errors.push(message.into());
	}
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeReportStatus {
	#[default]
	Ready,
	/// Safe units were exported while unresolved units were deferred or forced.
	PartialSuccess,
	/// Publication was stopped by an explicit non-conflict gate.
	Blocked,
	Fatal,
}

/// Wire schema for the execution identity embedded in every product merge
/// report. Merge-quality consumers reject reports without this attestation
/// instead of inferring the implementation from the command they launched.
pub const MERGE_EXECUTION_ATTESTATION_SCHEMA: &str = "1.0.0";

/// Wire schema and hashing profile for ordered Steam Workshop revisions.
///
/// Normal product execution trusts the read-only Workshop ACF as the source
/// version authority. It must never derive this identity by walking or hashing
/// the Workshop content tree.
pub const PRODUCT_INPUT_MANIFEST_SCHEMA: &str = "2.0.0";
pub const PRODUCT_INPUT_PROFILE: &str = "steam-workshop-acf-v1";
pub const PRODUCT_INPUT_DIGEST_ALGORITHM: &str = "blake3";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductInputMod {
	pub mod_id: String,
	pub precedence: usize,
	pub workshop_identity: WorkshopInstallIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductInputManifest {
	pub schema: String,
	pub profile: String,
	pub digest_algorithm: String,
	pub digest: String,
	pub mods: Vec<ProductInputMod>,
}

impl ProductInputManifest {
	pub fn new(mods: Vec<ProductInputMod>) -> Self {
		let mut manifest = Self {
			schema: PRODUCT_INPUT_MANIFEST_SCHEMA.to_string(),
			profile: PRODUCT_INPUT_PROFILE.to_string(),
			digest_algorithm: PRODUCT_INPUT_DIGEST_ALGORITHM.to_string(),
			digest: String::new(),
			mods,
		};
		manifest.digest = manifest.compute_digest();
		manifest
	}

	pub fn attestation(&self) -> ProductInputAttestation {
		ProductInputAttestation {
			schema: self.schema.clone(),
			profile: self.profile.clone(),
			digest_algorithm: self.digest_algorithm.clone(),
			digest: self.digest.clone(),
			mod_count: self.mods.len(),
		}
	}

	pub fn digest_is_valid(&self) -> bool {
		self.digest == self.compute_digest()
	}

	fn compute_digest(&self) -> String {
		let mut hasher = blake3::Hasher::new();
		update_digest_field(&mut hasher, &self.schema);
		update_digest_field(&mut hasher, &self.profile);
		update_digest_field(&mut hasher, &self.digest_algorithm);
		hasher.update(&(self.mods.len() as u64).to_le_bytes());
		for mod_input in &self.mods {
			update_digest_field(&mut hasher, &mod_input.mod_id);
			hasher.update(&(mod_input.precedence as u64).to_le_bytes());
			hasher.update(&mod_input.workshop_identity.app_id.to_le_bytes());
			update_digest_field(
				&mut hasher,
				mod_input.workshop_identity.workshop_id.as_str(),
			);
			update_digest_field(
				&mut hasher,
				mod_input.workshop_identity.manifest_id.as_str(),
			);
		}
		hasher.finalize().to_hex().to_string()
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductInputAttestation {
	pub schema: String,
	pub profile: String,
	pub digest_algorithm: String,
	pub digest: String,
	pub mod_count: usize,
}

fn update_digest_field(hasher: &mut blake3::Hasher, value: &str) {
	hasher.update(&(value.len() as u64).to_le_bytes());
	hasher.update(value.as_bytes());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeReportKernel {
	AddressPatchReference,
	SemanticTree,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeReportScope {
	FullProductMerge,
	RetainedPathEvaluation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MergeReportBaseSnapshot {
	Disabled,
	Resolved { identity: String },
	Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeExecutionAttestation {
	pub schema: String,
	pub kernel: MergeReportKernel,
	pub scope: MergeReportScope,
	pub base_snapshot: MergeReportBaseSnapshot,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReportValidation {
	pub fatal_errors: usize,
	pub strict_findings: usize,
	pub advisory_findings: usize,
	pub parse_errors: usize,
	pub unresolved_references: usize,
	pub missing_localisation: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReportRename {
	pub family_id: String,
	pub original_key: String,
	pub renamed_key: String,
	pub mod_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReportConflictContributor {
	pub mod_id: String,
	pub mod_version: String,
	pub precedence: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
	DeepMergeable,
	SchemaCardinalityViolation,
}

impl ConflictKind {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::DeepMergeable => "deep_mergeable",
			Self::SchemaCardinalityViolation => "schema_cardinality_violation",
		}
	}
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LeafConflictDetail {
	pub address_path: String,
	pub address_key: String,
	pub conflict_id: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub kind: Option<ConflictKind>,
	#[serde(default)]
	pub contributors: Vec<MergeReportConflictContributor>,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredUnitReason {
	#[default]
	NeedsUserChoice,
	UnsupportedInput,
	EngineFailure,
}

impl DeferredUnitReason {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::NeedsUserChoice => "needs_user_choice",
			Self::UnsupportedInput => "unsupported_input",
			Self::EngineFailure => "engine_failure",
		}
	}
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReportConflictResolution {
	pub path: String,
	pub reason: String,
	#[serde(default)]
	pub deferred_reason: DeferredUnitReason,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub kind: Option<ConflictKind>,
	#[serde(default)]
	pub leaf_conflicts: Vec<LeafConflictDetail>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct HandlerResolutionRecord {
	pub path: String,
	pub action: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub source: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub rationale: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeTraceContributor {
	pub mod_id: String,
	pub precedence: usize,
	pub dag_level: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeTracePolicy {
	CopyThrough,
	Overlay,
	Union,
	BooleanOr,
	NamedContainer,
	Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeTraceDecision {
	Adopted,
	Overridden,
	Unioned,
	Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeTraceEntry {
	pub contributors: Vec<MergeTraceContributor>,
	pub policy: MergeTracePolicy,
	pub decision: MergeTraceDecision,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeTraceEdge {
	pub merged_definition: String,
	pub source_mod: String,
	pub policy: MergeTracePolicy,
	pub decision: MergeTraceDecision,
	pub precedence: usize,
	pub dag_level: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DepMisuseEvidence {
	pub semantic_refs_to_dep: u32,
	pub false_remove_count: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DepMisuseFinding {
	pub mod_id: String,
	pub mod_display_name: String,
	pub suspicious_dep_id: String,
	pub suspicious_dep_display_name: String,
	pub evidence: DepMisuseEvidence,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VersionMismatchFinding {
	pub tag: String,
	pub severity: Severity,
	pub mod_id: String,
	pub mod_display_name: String,
	pub supported_version: String,
	pub game_version: String,
	pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StaleVanillaTargetDescriptor {
	pub mod_id: String,
	pub mod_version: String,
	pub file_path: String,
	pub patch_kind: String,
	pub target_path: Vec<String>,
	pub target_key: Option<String>,
	pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReport {
	pub status: MergeReportStatus,
	/// Stable, product-authored execution identity. Historical reports omit it;
	/// consumers that require an exact kernel/scope contract must reject `None`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub execution: Option<MergeExecutionAttestation>,
	/// Stable attestation for the exact ordered mod inputs observed by the
	/// product. It intentionally contains no local paths or input payloads.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub input: Option<ProductInputAttestation>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cache_source: Option<String>,
	/// When `status == Fatal` because workspace resolution failed, the
	/// underlying cause (e.g. missing/stale installed base data with the
	/// `foch data install` hint), mirroring what `foch check` surfaces.
	/// `None` on success — omitted from the report JSON so a non-fatal
	/// report stays byte-identical.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub fatal_reason: Option<String>,
	/// Deferred units with genuine competing outcomes that require a reviewed
	/// user or policy choice.
	pub manual_conflict_count: usize,
	/// Deferred units whose syntax or content shape is not supported yet.
	#[serde(default)]
	pub unsupported_input_count: usize,
	/// Deferred units caused by an internal merge invariant or implementation
	/// failure. These are product bugs, not user conflicts.
	#[serde(default)]
	pub engine_failure_count: usize,
	pub generated_file_count: usize,
	pub copied_file_count: usize,
	pub overlay_file_count: usize,
	#[serde(default)]
	pub definition_module_count: usize,
	#[serde(default)]
	pub definition_module_generated_count: usize,
	#[serde(default)]
	pub definition_module_blocked_count: usize,
	#[serde(default)]
	pub definition_module_elapsed_ms: u64,
	/// Unchanged vanilla base-game CopyThrough files intentionally not written
	/// to the merged mod because the game already ships them.
	#[serde(default)]
	pub base_passthrough_skipped_file_count: usize,
	/// Files whose patch-merge result was AST-equal to the vanilla base
	/// (modulo whitespace and comments) and were therefore skipped: shipping
	/// them would just shadow the game's own copy with byte-for-byte
	/// equivalent content.
	#[serde(default)]
	pub noop_skipped_file_count: usize,
	/// Generated files removed because every merge key already exists with
	/// identical content in a different file in the same opted-in family
	/// namespace. Tracked separately from same-path vanilla NoOp skips so the
	/// pruning reason remains auditable.
	#[serde(default)]
	pub cross_file_noop_skipped_file_count: usize,
	/// Individual generated entries removed because the same file's vanilla base
	/// already defines the key with an identical value in an opted-in family.
	#[serde(default)]
	pub per_entry_noop_skipped_count: usize,
	pub validation: MergeReportValidation,
	#[serde(default)]
	pub renames: Vec<MergeReportRename>,
	#[serde(default)]
	pub conflict_resolutions: Vec<MergeReportConflictResolution>,
	#[serde(default)]
	pub handler_resolutions: Vec<HandlerResolutionRecord>,
	#[serde(default)]
	pub dep_misuse: Vec<DepMisuseFinding>,
	#[serde(default)]
	pub version_mismatch: Vec<VersionMismatchFinding>,
	#[serde(default)]
	pub stale_vanilla_targets: Vec<StaleVanillaTargetDescriptor>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
	// D2 local dependency overrides applied during DAG-based merge.
	#[serde(default)]
	pub dep_overrides_applied: Vec<AppliedDepOverride>,
	/// SHA-256 fingerprint of the playset state that produced this report —
	/// the ordered enabled-mods list with each mod's version, plus the
	/// sorted local foch.toml [[overrides]] and [[resolutions]] entries.
	/// Re-running `foch merge --out X` against a directory whose existing
	/// report has the same fingerprint can skip the merge entirely and
	/// reuse the previous result.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub playset_fingerprint: Option<String>,
	/// Per merged file path → per top-level definition key → the mods whose
	/// content is adopted into the output, in DAG-precedence order. Only
	/// populated when `--provenance` is enabled. Diagnostic metadata only; it
	/// does not affect the emitted game files, so it is omitted from the report
	/// (and thus the report stays byte-identical) when the flag is off.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub definition_provenance: BTreeMap<String, BTreeMap<String, Vec<String>>>,
	/// Per merged file path → per top-level definition key → merge audit trail.
	/// Populated with `definition_provenance` when `--provenance` is enabled.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub merge_trace: BTreeMap<String, BTreeMap<String, MergeTraceEntry>>,
}

impl MergeReport {
	pub fn deferred_unit_count(&self) -> usize {
		self.manual_conflict_count
			.saturating_add(self.unsupported_input_count)
			.saturating_add(self.engine_failure_count)
	}
}
