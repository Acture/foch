use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadinessState {
	Ready,
	ReadyWithOmissions,
	Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReadinessIssue {
	pub(crate) id: String,
	pub(crate) title: String,
	pub(crate) detail: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub(crate) action: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledGameView {
	pub(crate) name: String,
	pub(crate) version: Option<String>,
	pub(crate) install_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BaseDataState {
	Ready,
	Missing,
	Stale,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BaseDataView {
	pub(crate) state: BaseDataState,
	pub(crate) version: Option<String>,
	pub(crate) detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaysetModView {
	pub(crate) id: String,
	pub(crate) name: String,
	pub(crate) position: usize,
	pub(crate) enabled: bool,
	pub(crate) workshop_id: Option<String>,
	pub(crate) workshop_manifest_id: Option<String>,
	pub(crate) version: Option<String>,
	pub(crate) declared_dependencies: Vec<String>,
	pub(crate) descriptor_path: Option<String>,
	pub(crate) source_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DetectedPlaysetView {
	pub(crate) name: String,
	pub(crate) source_path: String,
	pub(crate) mods: Vec<PlaysetModView>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AnalysisInputMode {
	Complete,
	WithoutUnavailableMods,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OmittedPlaysetMod {
	pub(crate) id: String,
	pub(crate) name: String,
	pub(crate) position: usize,
	pub(crate) reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InputRecoveryOption {
	pub(crate) kind: AnalysisInputMode,
	pub(crate) source_mod_count: usize,
	pub(crate) omitted_mods: Vec<OmittedPlaysetMod>,
	pub(crate) omitted_mod_count: usize,
	pub(crate) included_mod_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InputInspection {
	pub(crate) inspection_id: String,
	pub(crate) readiness: ReadinessState,
	pub(crate) game: InstalledGameView,
	pub(crate) base_data: BaseDataView,
	pub(crate) playset: Option<DetectedPlaysetView>,
	pub(crate) issues: Vec<ReadinessIssue>,
	pub(crate) recovery: Option<InputRecoveryOption>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupPhase {
	Idle,
	Checking,
	Checked,
	Building,
	Failed,
	BuildFailed,
	BuildCancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetupStageView {
	pub(crate) stage: String,
	pub(crate) completed: Option<u64>,
	pub(crate) total: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallRecoveryNote {
	pub(crate) version: String,
	pub(crate) rules_version: String,
	pub(crate) reason: String,
	pub(crate) local_command: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetupStatus {
	pub(crate) phase: SetupPhase,
	pub(crate) stage: Option<String>,
	pub(crate) completed: Option<u64>,
	pub(crate) total: Option<u64>,
	pub(crate) stages: Vec<SetupStageView>,
	pub(crate) inspection: Option<InputInspection>,
	pub(crate) message: Option<String>,
	pub(crate) build_id: Option<String>,
	pub(crate) build_stage: Option<String>,
	pub(crate) build_completed: Option<u64>,
	pub(crate) build_total: Option<u64>,
	/// Throttled document counts from the base-data parse stage.
	/// Playset descriptor counts stay on `stages` and are not mixed in.
	pub(crate) document_completed: Option<u64>,
	pub(crate) document_total: Option<u64>,
	pub(crate) install_note: Option<InstallRecoveryNote>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerifiedGamePath {
	pub(crate) version: String,
	pub(crate) install_path: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergeAnalysisState {
	Queued,
	Running,
	Ready,
	ReadyWithDeferrals,
	Blocked,
	Cancelled,
	Failed,
}

impl MergeAnalysisState {
	pub(crate) fn is_active(self) -> bool {
		matches!(self, Self::Queued | Self::Running)
	}

	pub(crate) fn is_complete(self) -> bool {
		matches!(self, Self::Ready | Self::ReadyWithDeferrals | Self::Blocked)
	}
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergeAnalysisStage {
	Inventory,
	ResolveInput,
	SemanticMerge,
	ValidateOutput,
	FreezeArtifacts,
	Complete,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MergeUnitCounts {
	pub(crate) total: usize,
	pub(crate) safe: usize,
	pub(crate) copy: usize,
	pub(crate) needs_user_choice: usize,
	pub(crate) unsupported_input: usize,
	pub(crate) engine_failure: usize,
	pub(crate) deferred: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartMergeAnalysisResult {
	pub(crate) analysis_id: String,
	pub(crate) input_scope: AnalysisInputScope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnalysisInputScope {
	pub(crate) mode: AnalysisInputMode,
	pub(crate) source_mod_count: usize,
	pub(crate) omitted_mods: Vec<OmittedPlaysetMod>,
	pub(crate) omitted_mod_count: usize,
	pub(crate) included_mod_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MergeCommitResult {
	pub(crate) analysis_id: String,
	pub(crate) output_dir: String,
	pub(crate) merge_status: String,
	pub(crate) generated_file_count: u64,
	pub(crate) manual_conflict_count: u64,
	pub(crate) unsupported_input_count: u64,
	pub(crate) engine_failure_count: u64,
	pub(crate) committed_at_note: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRouteProbe {
	pub(crate) label: String,
	pub(crate) url: String,
	pub(crate) kind: String,
	pub(crate) reachable: bool,
	pub(crate) http_status: Option<u16>,
	pub(crate) latency_ms: u64,
	pub(crate) detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConnectivityReport {
	pub(crate) transport: String,
	pub(crate) mirror_policy: String,
	pub(crate) routes: Vec<DesktopRouteProbe>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MergeAnalysisSummary {
	pub(crate) analysis_id: String,
	pub(crate) state: MergeAnalysisState,
	pub(crate) stage: MergeAnalysisStage,
	pub(crate) completed_units: u64,
	pub(crate) total_units: u64,
	pub(crate) elapsed_ms: u64,
	pub(crate) counts: MergeUnitCounts,
	pub(crate) message: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergeDisposition {
	Safe,
	Copy,
	NeedsUserChoice,
	UnsupportedInput,
	EngineFailure,
	Deferred,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergeUnitKind {
	File,
	DefinitionModule,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MergeUnitListItem {
	pub(crate) id: String,
	pub(crate) path: String,
	pub(crate) family: String,
	pub(crate) kind: MergeUnitKind,
	pub(crate) disposition: MergeDisposition,
	pub(crate) strategy: String,
	pub(crate) contributor_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MergeUnitPage {
	pub(crate) items: Vec<MergeUnitListItem>,
	pub(crate) total: usize,
	pub(crate) page: usize,
	pub(crate) page_size: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MergeUnitContributor {
	pub(crate) mod_id: String,
	pub(crate) name: String,
	pub(crate) position: usize,
	pub(crate) is_base_game: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MergeUnitDetail {
	pub(crate) id: String,
	pub(crate) path: String,
	pub(crate) family: String,
	pub(crate) kind: MergeUnitKind,
	pub(crate) disposition: MergeDisposition,
	pub(crate) strategy: String,
	pub(crate) summary: String,
	pub(crate) output_path: Option<String>,
	#[serde(default)]
	pub(crate) output_paths: Vec<String>,
	pub(crate) contributors: Vec<MergeUnitContributor>,
	#[serde(skip)]
	pub(crate) contributor_count: usize,
	pub(crate) notes: Vec<String>,
	/// This unit's deferred leaf conflicts, bounded per review contract.
	pub(crate) conflicts: Vec<DesktopUnitConflicts>,
}

/// One explicit merge resolution the frontend chose, mapping one-to-one onto
/// a `foch::project::ResolutionEntry`: exactly one selector (`matchPattern`
/// or `file`) and exactly one action (`preferMod` or `handler`).
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopResolutionRule {
	pub(crate) match_pattern: Option<String>,
	pub(crate) file: Option<String>,
	pub(crate) prefer_mod: Option<String>,
	pub(crate) handler: Option<String>,
}

/// What a decision option does, mirroring `foch::merge::DecisionAction`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
	tag = "kind",
	rename_all = "snake_case",
	rename_all_fields = "camelCase"
)]
pub(crate) enum DesktopDecisionAction {
	PreferMod { mod_id: String },
	Handler { name: String },
}

/// One way to resolve a leaf conflict, with the exact `foch.toml` rules that
/// would persist the choice for the file or its directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopDecisionOption {
	pub(crate) action: DesktopDecisionAction,
	pub(crate) file_resolution: Option<DesktopResolutionRule>,
	pub(crate) directory_resolution: Option<DesktopResolutionRule>,
}

/// One competing version of a conflicted leaf, rendered for review.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConflictCandidate {
	pub(crate) mod_id: String,
	pub(crate) mod_display_name: String,
	pub(crate) precedence: usize,
	pub(crate) change_summary: Vec<String>,
	pub(crate) rendered: String,
}

/// One leaf conflict: where it sits, why it deferred, and every explicit
/// resolution the handler contract audits.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConflictLeaf {
	pub(crate) conflict_id: String,
	pub(crate) address: String,
	pub(crate) kind: Option<String>,
	pub(crate) reason: Option<String>,
	pub(crate) vanilla_snippet: Option<String>,
	pub(crate) candidates: Vec<DesktopConflictCandidate>,
	pub(crate) options: Vec<DesktopDecisionOption>,
}

/// A deferred unit's leaf conflicts, flattened from the address tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUnitConflicts {
	pub(crate) file_path: String,
	pub(crate) reason: String,
	pub(crate) leaves: Vec<DesktopConflictLeaf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopPluginBadge {
	Enabled,
	Installed,
	NotInstalled,
	Conflict,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopConfigField {
	pub(crate) key: String,
	pub(crate) kind: String,
	pub(crate) description: Option<String>,
	pub(crate) value: serde_json::Value,
	pub(crate) min: Option<i64>,
	pub(crate) max: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPluginEntry {
	pub(crate) id: String,
	pub(crate) name: String,
	pub(crate) description: Option<String>,
	pub(crate) installed_versions: Vec<String>,
	pub(crate) selected_version: Option<String>,
	pub(crate) enabled: bool,
	pub(crate) install_spec: Option<String>,
	pub(crate) declared_by_mods: Vec<String>,
	pub(crate) required_by_mods: Vec<String>,
	pub(crate) conflicts: Vec<String>,
	pub(crate) config_fields: Vec<DesktopConfigField>,
	pub(crate) badge: DesktopPluginBadge,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPluginPlan {
	pub(crate) launchable: bool,
	pub(crate) order: Vec<String>,
	pub(crate) errors: Vec<String>,
	pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPreferencesView {
	pub(crate) transport: String,
	pub(crate) mirror: String,
	pub(crate) custom_mirrors: Vec<String>,
	pub(crate) language: String,
	pub(crate) effective_language: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSettingsInput {
	pub(crate) transport: String,
	pub(crate) mirror: String,
	#[serde(default)]
	pub(crate) custom_mirrors: Vec<String>,
	pub(crate) language: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPluginOverview {
	pub(crate) playset: String,
	pub(crate) game_root: Option<String>,
	pub(crate) game_version: Option<String>,
	pub(crate) host_dll_available: bool,
	pub(crate) host_dll_detail: String,
	pub(crate) entries: Vec<DesktopPluginEntry>,
	pub(crate) companion_warnings: Vec<String>,
	pub(crate) plan: Option<DesktopPluginPlan>,
	pub(crate) preferences: DesktopPreferencesView,
	pub(crate) problems: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPluginInstall {
	pub(crate) id: String,
	pub(crate) version: String,
	pub(crate) source: String,
	pub(crate) conflicts: Vec<String>,
	pub(crate) already_present: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopLaunchResult {
	pub(crate) run_dir: String,
	pub(crate) plan_sha256: String,
	pub(crate) prepared_only: bool,
	pub(crate) pid: Option<u32>,
	pub(crate) plugin_count: usize,
	pub(crate) version_dll: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPluginRuntimeState {
	pub(crate) id: String,
	pub(crate) state: String,
	pub(crate) reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopLaunchStatus {
	pub(crate) run_id: String,
	pub(crate) plugins: Vec<DesktopPluginRuntimeState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopError {
	pub(crate) code: &'static str,
	pub(crate) message: String,
}

impl DesktopError {
	pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
		Self {
			code,
			message: message.into(),
		}
	}

	pub(crate) fn internal(message: impl Into<String>) -> Self {
		Self::new("internal_error", message)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn serializes_typescript_contract_with_camel_case_fields() {
		let summary = MergeAnalysisSummary {
			analysis_id: "6c5c879b-d757-4245-aa22-f40d57fd6738".to_string(),
			state: MergeAnalysisState::ReadyWithDeferrals,
			stage: MergeAnalysisStage::Complete,
			completed_units: 5,
			total_units: 5,
			elapsed_ms: 12,
			counts: MergeUnitCounts {
				total: 5,
				needs_user_choice: 1,
				..MergeUnitCounts::default()
			},
			message: None,
		};
		assert_eq!(
			serde_json::to_value(summary).unwrap(),
			json!({
				"analysisId": "6c5c879b-d757-4245-aa22-f40d57fd6738",
				"state": "ready_with_deferrals",
				"stage": "complete",
				"completedUnits": 5,
				"totalUnits": 5,
				"elapsedMs": 12,
				"counts": {
					"total": 5,
					"safe": 0,
					"copy": 0,
					"needsUserChoice": 1,
					"unsupportedInput": 0,
					"engineFailure": 0,
					"deferred": 0
				},
				"message": null
			})
		);
	}

	#[test]
	fn deserializes_disposition_filter_from_snake_case() {
		assert_eq!(
			serde_json::from_value::<MergeDisposition>(json!("needs_user_choice")).unwrap(),
			MergeDisposition::NeedsUserChoice
		);
	}

	#[test]
	fn serializes_stable_error_shape() {
		assert_eq!(
			serde_json::to_value(DesktopError::new("analysis_busy", "busy")).unwrap(),
			json!({ "code": "analysis_busy", "message": "busy" })
		);
	}

	#[test]
	fn serializes_bounded_conflict_contract_with_resolution_rules() {
		let conflicts = vec![DesktopUnitConflicts {
			file_path: "history/countries/TES - Test.txt".to_string(),
			reason: "manual conflict".to_string(),
			leaves: vec![DesktopConflictLeaf {
				conflict_id: "ab12cd34".to_string(),
				address: "religion".to_string(),
				kind: Some("deep_mergeable".to_string()),
				reason: Some("three mods set religion".to_string()),
				vanilla_snippet: Some("religion = catholic".to_string()),
				candidates: vec![DesktopConflictCandidate {
					mod_id: "ugc_300002".to_string(),
					mod_display_name: "Conflict B".to_string(),
					precedence: 2,
					change_summary: vec!["set religion".to_string()],
					rendered: "religion = protestant".to_string(),
				}],
				options: vec![DesktopDecisionOption {
					action: DesktopDecisionAction::PreferMod {
						mod_id: "ugc_300002".to_string(),
					},
					file_resolution: Some(DesktopResolutionRule {
						file: Some("history/countries/TES - Test.txt".to_string()),
						prefer_mod: Some("ugc_300002".to_string()),
						..DesktopResolutionRule::default()
					}),
					directory_resolution: Some(DesktopResolutionRule {
						match_pattern: Some("re:^history/countries/[^/]+$".to_string()),
						prefer_mod: Some("ugc_300002".to_string()),
						..DesktopResolutionRule::default()
					}),
				}],
			}],
		}];
		let detail = MergeUnitDetail {
			id: "file:history/countries/TES - Test.txt".to_string(),
			path: "history/countries/TES - Test.txt".to_string(),
			family: "countries".to_string(),
			kind: MergeUnitKind::File,
			disposition: MergeDisposition::NeedsUserChoice,
			strategy: "structural_merge".to_string(),
			summary: "deferred".to_string(),
			output_path: None,
			output_paths: Vec::new(),
			contributors: Vec::new(),
			contributor_count: 0,
			notes: Vec::new(),
			conflicts,
		};
		let value = serde_json::to_value(detail).unwrap();
		assert_eq!(
			value["conflicts"][0]["leaves"][0]["options"][0]["action"],
			json!({ "kind": "prefer_mod", "modId": "ugc_300002" })
		);
		assert_eq!(
			value["conflicts"][0]["leaves"][0]["options"][0]["fileResolution"],
			json!({
				"matchPattern": null,
				"file": "history/countries/TES - Test.txt",
				"preferMod": "ugc_300002",
				"handler": null
			})
		);
		let rule: DesktopResolutionRule = serde_json::from_value(json!({
			"matchPattern": "history/**",
			"file": null,
			"preferMod": "ugc_300002",
			"handler": null
		}))
		.unwrap();
		assert_eq!(rule.match_pattern.as_deref(), Some("history/**"));
	}

	#[test]
	fn serializes_commit_and_connectivity_contracts() {
		let commit = MergeCommitResult {
			analysis_id: "6c5c879b-d757-4245-aa22-f40d57fd6738".to_string(),
			output_dir: "D:/merged".to_string(),
			merge_status: "partial_success".to_string(),
			generated_file_count: 4,
			manual_conflict_count: 1,
			unsupported_input_count: 0,
			engine_failure_count: 0,
			committed_at_note: "deferred".to_string(),
		};
		assert_eq!(
			serde_json::to_value(commit).unwrap(),
			json!({
				"analysisId": "6c5c879b-d757-4245-aa22-f40d57fd6738",
				"outputDir": "D:/merged",
				"mergeStatus": "partial_success",
				"generatedFileCount": 4,
				"manualConflictCount": 1,
				"unsupportedInputCount": 0,
				"engineFailureCount": 0,
				"committedAtNote": "deferred"
			})
		);
		let report = DesktopConnectivityReport {
			transport: "http".to_string(),
			mirror_policy: "direct".to_string(),
			routes: vec![DesktopRouteProbe {
				label: "direct".to_string(),
				url: "http://127.0.0.1/direct".to_string(),
				kind: "direct".to_string(),
				reachable: true,
				http_status: Some(200),
				latency_ms: 12,
				detail: "HEAD".to_string(),
			}],
		};
		assert_eq!(
			serde_json::to_value(report).unwrap(),
			json!({
				"transport": "http",
				"mirrorPolicy": "direct",
				"routes": [{
					"label": "direct",
					"url": "http://127.0.0.1/direct",
					"kind": "direct",
					"reachable": true,
					"httpStatus": 200,
					"latencyMs": 12,
					"detail": "HEAD"
				}]
			})
		);
	}
}
