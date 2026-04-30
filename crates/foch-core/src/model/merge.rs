use serde::{Deserialize, Serialize};

use super::analysis::Severity;
use crate::config::AppliedDepOverride;

pub const MERGED_MOD_DESCRIPTOR_PATH: &str = "descriptor.mod";
pub const MERGE_PLAN_ARTIFACT_PATH: &str = ".foch/foch-merge-plan.json";
pub const MERGE_REPORT_ARTIFACT_PATH: &str = ".foch/foch-merge-report.json";

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanEntry {
	pub path: String,
	pub strategy: MergePlanStrategy,
	pub contributors: Vec<MergePlanContributor>,
	pub winner: Option<MergePlanContributor>,
	#[serde(default)]
	pub generated: bool,
	#[serde(default)]
	pub notes: Vec<String>,
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
	/// Some manual conflicts were resolved by --force (binary files copied from winner).
	PartialSuccess,
	Blocked,
	Fatal,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeReportConflictKind {
	#[default]
	LastWriterFallback,
	TrueConflictSkipped,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReportConflictContributor {
	pub mod_id: String,
	pub mod_version: String,
	pub precedence: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReportConflictResolution {
	pub path: String,
	pub kind: MergeReportConflictKind,
	pub reason: String,
	pub winning_mod: String,
	#[serde(default)]
	pub marker_written: bool,
	#[serde(default)]
	pub contributors: Vec<MergeReportConflictContributor>,
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
	pub manual_conflict_count: usize,
	#[serde(default)]
	pub fallback_resolved_count: usize,
	pub generated_file_count: usize,
	pub copied_file_count: usize,
	pub overlay_file_count: usize,
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
}
