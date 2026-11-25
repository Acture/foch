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
