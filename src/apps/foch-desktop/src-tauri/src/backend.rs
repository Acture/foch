use crate::dto::{
	AnalysisInputMode, AnalysisInputScope, BaseDataState, BaseDataView, DesktopError,
	DetectedPlaysetView, InputInspection, InputRecoveryOption, InstalledGameView,
	MergeAnalysisState, MergeDisposition, MergeUnitContributor, MergeUnitCounts, MergeUnitDetail,
	MergeUnitKind, OmittedPlaysetMod, PlaysetModView, ReadinessIssue, ReadinessState,
};
use foch::input::{
	CurrentEu4Input, InputPreparationMode, InputRequest, PreparedAnalysisInput,
	inspect_current_eu4_input,
};
use foch::merge::{
	AnalyzedMerge, CancellationToken, MergeAnalysisOptions, MergeAnalysisStatus, MergeError,
	ProgressObserver, analyze_merge,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const MAX_INSPECTION_ISSUES: usize = 64;
const MAX_PLAYSET_MODS: usize = 4_096;
const MAX_DEPENDENCIES_PER_MOD: usize = 512;
const MAX_CONTRIBUTORS_PER_UNIT: usize = 256;
const MAX_NOTES_PER_UNIT: usize = 64;
const MAX_SHORT_TEXT_CHARS: usize = 1_024;
const MAX_DETAIL_TEXT_CHARS: usize = 8_192;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AnalysisRunError {
	Cancelled,
	Failed(String),
}

#[derive(Debug)]
pub(crate) struct InspectedAnalysisInput {
	pub(crate) inspection: InputInspection,
	pub(crate) prepared: Option<PreparedDesktopInput>,
}

#[derive(Debug)]
pub(crate) struct PreparedDesktopInput {
	pub(crate) request: InputRequest,
	pub(crate) input_scope: AnalysisInputScope,
}

/// Counts and the player-facing note from one empty-target commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewCommit {
	pub(crate) merge_status: String,
	pub(crate) generated_file_count: u64,
	pub(crate) manual_conflict_count: u64,
	pub(crate) unsupported_input_count: u64,
	pub(crate) engine_failure_count: u64,
	pub(crate) note: String,
}

pub(crate) trait AnalysisReview: Send + Sync {
	fn state(&self) -> MergeAnalysisState;
	fn counts(&self) -> MergeUnitCounts;
	fn len(&self) -> usize;
	fn unit_at(&self, index: usize) -> Option<MergeUnitDetail>;
	fn unit(&self, id: &str) -> Option<MergeUnitDetail>;
	fn commit_empty_target(&self, out_dir: PathBuf) -> Result<ReviewCommit, DesktopError>;
}

pub(crate) trait AnalysisRunner: Send + Sync {
	fn inspect_input(&self) -> InspectedAnalysisInput;

	fn run_analysis(
		&self,
		analysis_id: &str,
		request: InputRequest,
		progress: &dyn ProgressObserver,
		cancellation: &CancellationToken,
	) -> Result<Arc<dyn AnalysisReview>, AnalysisRunError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FochAnalysisRunner;

impl AnalysisRunner for FochAnalysisRunner {
	fn inspect_input(&self) -> InspectedAnalysisInput {
		let input = inspect_current_eu4_input();
		let inspection = input_inspection_view(&input);
		let preparation_mode = match input.readiness {
			foch::input::InputReadiness::Ready => Some(InputPreparationMode::Complete),
			foch::input::InputReadiness::ReadyWithOmissions => {
				Some(InputPreparationMode::AvailableOnly)
			}
			foch::input::InputReadiness::Blocked => None,
		};
		let prepared = preparation_mode
			.and_then(|mode| input.prepare(mode))
			.map(|prepared| PreparedDesktopInput {
				input_scope: prepared_input_scope(&prepared),
				request: prepared.request,
			});
		InspectedAnalysisInput {
			inspection,
			prepared,
		}
	}

	fn run_analysis(
		&self,
		analysis_id: &str,
		request: InputRequest,
		progress: &dyn ProgressObserver,
		cancellation: &CancellationToken,
	) -> Result<Arc<dyn AnalysisReview>, AnalysisRunError> {
		let out_dir = unique_analysis_target(analysis_id);
		if out_dir.exists() {
			return Err(AnalysisRunError::Failed(format!(
				"temporary analysis target already exists: {}",
				out_dir.display()
			)));
		}
		let analyzed = analyze_merge(
			request,
			MergeAnalysisOptions {
				out_dir,
				include_game_base: true,
				include_base: false,
				gui_scroll_merge: false,
				force: false,
				strict: false,
				ignore_replace_path: false,
				dep_overrides: Vec::new(),
				resolution_config_path: None,
				interactive_conflict_handler: None,
				interactive_resolution_config_path: None,
				playset_fingerprint: None,
				provenance: false,
				merge_workers: foch::merge::default_merge_workers(),
				retained_paths: None,
			},
			progress,
			cancellation,
		)
		.map_err(map_merge_error)?;
		Ok(Arc::new(FochAnalysisReview::new(analyzed)))
	}
}

fn unique_analysis_target(analysis_id: &str) -> PathBuf {
	std::env::temp_dir().join(format!("foch-desktop-analysis-{analysis_id}"))
}

fn map_merge_error(error: MergeError) -> AnalysisRunError {
	match error {
		MergeError::Cancelled => AnalysisRunError::Cancelled,
		other => AnalysisRunError::Failed(other.to_string()),
	}
}

struct FochAnalysisReview {
	analyzed: Mutex<Option<AnalyzedMerge>>,
	state: MergeAnalysisState,
	counts: MergeUnitCounts,
	units: Vec<MergeUnitDetail>,
}

impl FochAnalysisReview {
	fn new(analyzed: AnalyzedMerge) -> Self {
		let state = match analyzed.analysis().status() {
			MergeAnalysisStatus::ReadyToCommit => MergeAnalysisState::Ready,
			MergeAnalysisStatus::CommittableWithDeferrals => MergeAnalysisState::ReadyWithDeferrals,
			MergeAnalysisStatus::Blocked => MergeAnalysisState::Blocked,
		};
		let summary = analyzed.review_summary();
		let counts = MergeUnitCounts {
			total: summary.total,
			safe: summary.safe,
			copy: summary.copy,
			needs_user_choice: summary.needs_user_choice,
			unsupported_input: summary.unsupported_input,
			engine_failure: summary.engine_failure,
			deferred: summary.deferred,
		};
		let units = analyzed.list_units().iter().map(unit_view).collect();
		Self {
			analyzed: Mutex::new(Some(analyzed)),
			state,
			counts,
			units,
		}
	}
}

impl AnalysisReview for FochAnalysisReview {
	fn state(&self) -> MergeAnalysisState {
		self.state
	}

	fn counts(&self) -> MergeUnitCounts {
		self.counts
	}

	fn len(&self) -> usize {
		self.units.len()
	}

	fn unit_at(&self, index: usize) -> Option<MergeUnitDetail> {
		self.units.get(index).cloned()
	}

	fn unit(&self, id: &str) -> Option<MergeUnitDetail> {
		self.units.iter().find(|unit| unit.id == id).cloned()
	}

	fn commit_empty_target(&self, out_dir: PathBuf) -> Result<ReviewCommit, DesktopError> {
		let analyzed = {
			let mut slot = self
				.analyzed
				.lock()
				.map_err(|_| DesktopError::internal("merge analysis lock was poisoned"))?;
			let Some(analyzed) = slot.as_ref() else {
				return Err(already_committed());
			};
			analyzed
				.validate_empty_target(&out_dir)
				.map_err(map_commit_error)?;
			slot.take().expect("the analysis was present")
		};
		let result = analyzed
			.commit_to_empty_dir(out_dir)
			.map_err(map_commit_error)?;
		Ok(review_commit(&result))
	}
}

fn already_committed() -> DesktopError {
	DesktopError::new(
		"already_committed",
		"this analysis was already exported; start a new analysis before exporting again",
	)
}

fn map_commit_error(error: MergeError) -> DesktopError {
	match error {
		MergeError::ReplacementAuthorizationRequired { path } => DesktopError::new(
			"output_not_empty",
			format!(
				"choose a new or empty folder; {} already contains files and was left untouched",
				path.display()
			),
		),
		MergeError::Validation { message, .. } => DesktopError::new("invalid_output_dir", message),
		other => DesktopError::new("merge_commit_failed", other.to_string()),
	}
}

fn review_commit(result: &foch::merge::CommitResult) -> ReviewCommit {
	let status = result.merge_status.status;
	ReviewCommit {
		merge_status: report_status_name(status).to_string(),
		generated_file_count: count_to_u64(result.merge_status.generated_file_count),
		manual_conflict_count: count_to_u64(result.merge_status.manual_conflict_count),
		unsupported_input_count: count_to_u64(result.merge_status.unsupported_input_count),
		engine_failure_count: count_to_u64(result.merge_status.engine_failure_count),
		note: commit_note(status),
	}
}

fn report_status_name(status: foch::model::MergeReportStatus) -> &'static str {
	match status {
		foch::model::MergeReportStatus::Ready => "ready",
		foch::model::MergeReportStatus::PartialSuccess => "partial_success",
		foch::model::MergeReportStatus::Blocked => "blocked",
		foch::model::MergeReportStatus::Fatal => "fatal",
	}
}

fn commit_note(status: foch::model::MergeReportStatus) -> String {
	match status {
		foch::model::MergeReportStatus::PartialSuccess => {
			"Safe files and definition modules were written. Unsafe units were deferred and were not overwritten.".to_string()
		}
		foch::model::MergeReportStatus::Ready => {
			"Exported the merged mod into an empty folder. Source mods and the game directory were not modified.".to_string()
		}
		foch::model::MergeReportStatus::Blocked => {
			"The merge was blocked and nothing was exported.".to_string()
		}
		foch::model::MergeReportStatus::Fatal => {
			"The merge failed and nothing was exported.".to_string()
		}
	}
}

fn count_to_u64(value: usize) -> u64 {
	u64::try_from(value).unwrap_or(u64::MAX)
}

fn unit_view(unit: &foch::merge::MergeUnitOutcome) -> MergeUnitDetail {
	let contributor_count = unit.contributors.len();
	MergeUnitDetail {
		id: unit.id.clone(),
		path: unit.path.as_str().to_owned(),
		family: bounded_text(&unit.family, MAX_SHORT_TEXT_CHARS),
		kind: match unit.kind {
			foch::merge::MergeUnitKind::File => MergeUnitKind::File,
			foch::merge::MergeUnitKind::DefinitionModule => MergeUnitKind::DefinitionModule,
		},
		disposition: match unit.disposition {
			foch::merge::MergeDisposition::Safe => MergeDisposition::Safe,
			foch::merge::MergeDisposition::Copy => MergeDisposition::Copy,
			foch::merge::MergeDisposition::NeedsUserChoice => MergeDisposition::NeedsUserChoice,
			foch::merge::MergeDisposition::UnsupportedInput => MergeDisposition::UnsupportedInput,
			foch::merge::MergeDisposition::EngineFailure => MergeDisposition::EngineFailure,
			foch::merge::MergeDisposition::Deferred => MergeDisposition::Deferred,
		},
		strategy: bounded_text(&unit.strategy, MAX_SHORT_TEXT_CHARS),
		summary: bounded_text(&unit.summary, MAX_DETAIL_TEXT_CHARS),
		output_path: unit
			.output_path
			.as_ref()
			.map(|path| bounded_text(path.as_str(), MAX_DETAIL_TEXT_CHARS)),
		// A definition module can write one file per contributing directory.
		output_paths: unit
			.output_paths
			.iter()
			.map(|path| bounded_text(path.as_str(), MAX_DETAIL_TEXT_CHARS))
			.collect(),
		contributors: unit
			.contributors
			.iter()
			.take(MAX_CONTRIBUTORS_PER_UNIT)
			.map(|contributor| MergeUnitContributor {
				mod_id: bounded_text(&contributor.mod_id, MAX_SHORT_TEXT_CHARS),
				name: bounded_text(&contributor.name, MAX_SHORT_TEXT_CHARS),
				position: contributor.precedence,
				is_base_game: contributor.is_base_game,
			})
			.collect(),
		contributor_count,
		notes: unit
			.notes
			.iter()
			.take(MAX_NOTES_PER_UNIT)
			.map(|note| bounded_text(note, MAX_DETAIL_TEXT_CHARS))
			.collect(),
	}
}

pub(crate) fn input_inspection_view(input: &CurrentEu4Input) -> InputInspection {
	let mut issues = input
		.issues
		.iter()
		.take(MAX_INSPECTION_ISSUES)
		.map(|issue| ReadinessIssue {
			id: bounded_text(&issue.id, MAX_SHORT_TEXT_CHARS),
			title: bounded_text(&issue.title, MAX_SHORT_TEXT_CHARS),
			detail: bounded_text(&issue.detail, MAX_DETAIL_TEXT_CHARS),
			action: issue
				.action
				.as_deref()
				.map(|action| bounded_text(action, MAX_DETAIL_TEXT_CHARS)),
		})
		.collect::<Vec<_>>();
	let playset = input.playset.as_ref().map(|playset| {
		let mod_count = playset.mods.len();
		let mods = playset
			.mods
			.iter()
			.take(MAX_PLAYSET_MODS)
			.map(|playset_mod| PlaysetModView {
				id: bounded_text(&playset_mod.id, MAX_SHORT_TEXT_CHARS),
				name: bounded_text(&playset_mod.name, MAX_SHORT_TEXT_CHARS),
				position: playset_mod.position,
				enabled: playset_mod.enabled,
				workshop_id: playset_mod
					.workshop_id
					.as_deref()
					.map(|id| bounded_text(id, MAX_SHORT_TEXT_CHARS)),
				workshop_manifest_id: playset_mod
					.workshop_manifest_id
					.as_deref()
					.map(|id| bounded_text(id, MAX_SHORT_TEXT_CHARS)),
				version: playset_mod
					.version
					.as_deref()
					.map(|version| bounded_text(version, MAX_SHORT_TEXT_CHARS)),
				declared_dependencies: playset_mod
					.declared_dependencies
					.iter()
					.take(MAX_DEPENDENCIES_PER_MOD)
					.map(|dependency| bounded_text(dependency, MAX_SHORT_TEXT_CHARS))
					.collect(),
				descriptor_path: playset_mod
					.descriptor_path
					.as_deref()
					.map(display_path),
				source_error: playset_mod
					.source_error
					.as_deref()
					.map(|error| bounded_text(error, MAX_DETAIL_TEXT_CHARS)),
			})
			.collect();
		if mod_count > MAX_PLAYSET_MODS && issues.len() < MAX_INSPECTION_ISSUES {
			issues.push(ReadinessIssue {
				id: "desktop_view_truncated".to_string(),
				title: "Playset view was truncated".to_string(),
				detail: format!(
					"The desktop view shows the first {MAX_PLAYSET_MODS} of {mod_count} mods. Analysis still uses the complete inspected playset."
				),
				action: None,
			});
		}
		DetectedPlaysetView {
			name: bounded_text(&playset.name, MAX_SHORT_TEXT_CHARS),
			source_path: display_path(&playset.source_path),
			mods,
		}
	});
	let readiness = match input.readiness {
		foch::input::InputReadiness::Ready => ReadinessState::Ready,
		foch::input::InputReadiness::ReadyWithOmissions => ReadinessState::ReadyWithOmissions,
		foch::input::InputReadiness::Blocked => ReadinessState::Blocked,
	};
	let recovery = input.recovery.as_ref().map(|recovery| InputRecoveryOption {
		kind: AnalysisInputMode::WithoutUnavailableMods,
		source_mod_count: recovery.source_mod_count,
		omitted_mods: recovery
			.omitted_mods
			.iter()
			.take(MAX_PLAYSET_MODS)
			.map(omitted_playset_mod_view)
			.collect(),
		omitted_mod_count: recovery.omitted_mods.len(),
		included_mod_count: recovery.included_mod_count,
	});
	InputInspection {
		inspection_id: String::new(),
		readiness,
		game: InstalledGameView {
			name: bounded_text(&input.game.name, MAX_SHORT_TEXT_CHARS),
			version: input
				.game
				.version
				.as_deref()
				.map(|version| bounded_text(version, MAX_SHORT_TEXT_CHARS)),
			install_path: input.game.install_path.as_deref().map(display_path),
		},
		base_data: BaseDataView {
			state: match input.base_data.state {
				foch::input::BaseDataState::Ready => BaseDataState::Ready,
				foch::input::BaseDataState::Missing => BaseDataState::Missing,
				foch::input::BaseDataState::Stale => BaseDataState::Stale,
			},
			version: input
				.base_data
				.version
				.as_deref()
				.map(|version| bounded_text(version, MAX_SHORT_TEXT_CHARS)),
			detail: bounded_text(&input.base_data.detail, MAX_DETAIL_TEXT_CHARS),
		},
		playset,
		issues,
		recovery,
	}
}

fn prepared_input_scope(prepared: &PreparedAnalysisInput) -> AnalysisInputScope {
	match prepared.recovery.as_ref() {
		Some(recovery) => AnalysisInputScope {
			mode: AnalysisInputMode::WithoutUnavailableMods,
			source_mod_count: recovery.source_mod_count,
			omitted_mods: recovery
				.omitted_mods
				.iter()
				.take(MAX_PLAYSET_MODS)
				.map(omitted_playset_mod_view)
				.collect(),
			omitted_mod_count: recovery.omitted_mods.len(),
			included_mod_count: recovery.included_mod_count,
		},
		None => AnalysisInputScope {
			mode: AnalysisInputMode::Complete,
			source_mod_count: prepared.source_mod_count,
			omitted_mods: Vec::new(),
			omitted_mod_count: 0,
			included_mod_count: prepared.source_mod_count,
		},
	}
}

fn omitted_playset_mod_view(omitted: &foch::input::OmittedPlaysetMod) -> OmittedPlaysetMod {
	OmittedPlaysetMod {
		id: bounded_text(&omitted.id, MAX_SHORT_TEXT_CHARS),
		name: bounded_text(&omitted.name, MAX_SHORT_TEXT_CHARS),
		position: omitted.position,
		reason: bounded_text(&omitted.reason, MAX_DETAIL_TEXT_CHARS),
	}
}

fn display_path(path: &std::path::Path) -> String {
	bounded_text(&path.to_string_lossy(), MAX_DETAIL_TEXT_CHARS)
}

pub(crate) fn bounded_text(value: &str, max_chars: usize) -> String {
	if value.chars().count() <= max_chars {
		return value.to_string();
	}
	value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn truncated_recovery_view_retains_full_selection_counts() {
		let source_mod_count = MAX_PLAYSET_MODS + 2;
		let omitted_mod_count = source_mod_count - 1;
		let mods = (0..source_mod_count)
			.map(|index| {
				json!({
					"id": index.to_string(),
					"name": format!("Mod {index}"),
					"position": index + 1,
					"enabled": true,
					"workshopId": index.to_string(),
					"workshopManifestId": null,
					"version": null,
					"declaredDependencies": [],
					"descriptorPath": null,
					"sourceError": "missing"
				})
			})
			.collect::<Vec<_>>();
		let omitted_mods = (0..omitted_mod_count)
			.map(|index| {
				json!({
					"id": index.to_string(),
					"name": format!("Mod {index}"),
					"position": index + 1,
					"reason": "missing"
				})
			})
			.collect::<Vec<_>>();
		let input: CurrentEu4Input = serde_json::from_value(json!({
			"readiness": "ready_with_omissions",
			"game": {
				"name": "Europa Universalis IV",
				"version": "1.37.5",
				"installPath": "/game"
			},
			"baseData": {
				"state": "ready",
				"version": "1.37.5",
				"detail": "ready"
			},
			"playset": {
				"name": "Current EU4 playset",
				"sourcePath": "/playset/dlc_load.json",
				"mods": mods
			},
			"issues": [],
			"recovery": {
				"sourceModCount": source_mod_count,
				"omittedMods": omitted_mods,
				"includedModCount": 1
			}
		}))
		.expect("deserialize oversized inspection view");

		let view = input_inspection_view(&input);
		let recovery = view.recovery.expect("recovery option");
		assert_eq!(view.playset.expect("playset").mods.len(), MAX_PLAYSET_MODS);
		assert_eq!(recovery.omitted_mods.len(), MAX_PLAYSET_MODS);
		assert_eq!(recovery.source_mod_count, source_mod_count);
		assert_eq!(recovery.omitted_mod_count, omitted_mod_count);
		assert_eq!(recovery.included_mod_count, 1);
	}

	struct PassthroughRunner;

	impl AnalysisRunner for PassthroughRunner {
		fn inspect_input(&self) -> InspectedAnalysisInput {
			InspectedAnalysisInput {
				inspection: InputInspection {
					inspection_id: String::new(),
					readiness: ReadinessState::Ready,
					game: InstalledGameView {
						name: "Europa Universalis IV".to_string(),
						version: Some("1.37.5".to_string()),
						install_path: None,
					},
					base_data: BaseDataView {
						state: BaseDataState::Ready,
						version: Some("1.37.5".to_string()),
						detail: "fixture".to_string(),
					},
					playset: None,
					issues: Vec::new(),
					recovery: None,
				},
				prepared: Some(PreparedDesktopInput {
					request: foch::input::InputRequest::from_playset_path(
						passthrough_playset(),
						foch::input::Config::default(),
					),
					input_scope: AnalysisInputScope {
						mode: AnalysisInputMode::Complete,
						source_mod_count: 1,
						omitted_mods: Vec::new(),
						omitted_mod_count: 0,
						included_mod_count: 1,
					},
				}),
			}
		}

		fn run_analysis(
			&self,
			analysis_id: &str,
			_request: foch::input::InputRequest,
			progress: &dyn foch::merge::ProgressObserver,
			cancellation: &foch::merge::CancellationToken,
		) -> Result<Arc<dyn AnalysisReview>, AnalysisRunError> {
			let out_dir = unique_analysis_target(analysis_id);
			if out_dir.exists() {
				return Err(AnalysisRunError::Failed(format!(
					"temporary analysis target already exists: {}",
					out_dir.display()
				)));
			}
			let analyzed = foch::merge::analyze_merge(
				foch::input::InputRequest::from_playset_path(
					passthrough_playset(),
					foch::input::Config::default(),
				),
				foch::merge::MergeAnalysisOptions {
					out_dir,
					include_game_base: false,
					include_base: false,
					gui_scroll_merge: false,
					force: false,
					strict: true,
					ignore_replace_path: false,
					dep_overrides: Vec::new(),
					resolution_config_path: None,
					interactive_conflict_handler: None,
					interactive_resolution_config_path: None,
					playset_fingerprint: None,
					provenance: false,
					merge_workers: foch::merge::default_merge_workers(),
					retained_paths: None,
				},
				progress,
				cancellation,
			)
			.map_err(map_merge_error)?;
			Ok(Arc::new(FochAnalysisReview::new(analyzed)))
		}
	}

	fn passthrough_playset() -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
			"../../../packages/foch/tests/fixtures/playsets/eu4_minimal_passthrough/dlc_load.json",
		)
	}

	#[test]
	fn empty_target_commit_refuses_a_filled_folder_then_exports_once() {
		let state = crate::state::DesktopState::new(Arc::new(PassthroughRunner));
		let inspection = state.inspect_input().expect("inspect fixture");
		let id = state
			.queue_analysis(&inspection.inspection_id, AnalysisInputMode::Complete)
			.expect("queue")
			.analysis_id;
		state.run_analysis(&id);
		assert_eq!(
			state.summary(&id).expect("summary").state,
			MergeAnalysisState::Ready
		);
		let temp = tempfile::TempDir::new().expect("temp dir");
		let occupied = temp.path().join("occupied");
		std::fs::create_dir_all(&occupied).expect("occupied dir");
		std::fs::write(occupied.join("keep.txt"), b"leave me\n").expect("seed");
		let refused = state
			.commit_analysis(&id, &occupied.display().to_string())
			.expect_err("non-empty target");
		assert_eq!(refused.code, "output_not_empty");
		assert_eq!(
			std::fs::read(occupied.join("keep.txt")).expect("preserved"),
			b"leave me\n"
		);
		assert!(!occupied.join("descriptor.mod").exists());
		assert!(state.list_units(&id, "", None, 1, 10).expect("units").total > 0);

		let exported = temp.path().join("exported mod");
		let result = state
			.commit_analysis(&id, &exported.display().to_string())
			.expect("export");
		assert_eq!(result.analysis_id, id);
		assert_eq!(result.merge_status, "ready");
		assert!(result.generated_file_count > 0);
		let descriptor =
			std::fs::read_to_string(exported.join("descriptor.mod")).expect("descriptor");
		assert!(descriptor.contains("exported mod (Merged)"), "{descriptor}");
		assert!(
			state
				.list_units(&id, "", None, 1, 10)
				.expect("units after export")
				.total > 0
		);
		let again = state
			.commit_analysis(&id, &temp.path().join("other").display().to_string())
			.expect_err("second export");
		assert_eq!(again.code, "already_committed");
	}
}
