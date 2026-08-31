use crate::dto::{
	BaseDataState, BaseDataView, DetectedPlaysetView, InputInspection, InstalledGameView,
	MergeAnalysisState, MergeDisposition, MergeUnitContributor, MergeUnitCounts, MergeUnitDetail,
	MergeUnitKind, PlaysetModView, ReadinessIssue, ReadinessState,
};
use foch::input::{CurrentEu4Input, InputRequest, inspect_current_eu4_input};
use foch::merge::{
	AnalyzedMerge, CancellationToken, MergeAnalysisOptions, MergeAnalysisStatus, MergeError,
	ProgressObserver, analyze_merge,
};
use std::path::PathBuf;
use std::sync::Arc;

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
	pub(crate) request: Option<InputRequest>,
}

pub(crate) trait AnalysisReview: Send + Sync {
	fn state(&self) -> MergeAnalysisState;
	fn counts(&self) -> MergeUnitCounts;
	fn len(&self) -> usize;
	fn unit_at(&self, index: usize) -> Option<MergeUnitDetail>;
	fn unit(&self, id: &str) -> Option<MergeUnitDetail>;
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
		InspectedAnalysisInput {
			inspection,
			request: input.into_request(),
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
				ignore_replace_path: false,
				dep_overrides: Vec::new(),
				resolution_config_path: None,
				interactive_conflict_handler: None,
				interactive_resolution_config_path: None,
				playset_fingerprint: None,
				provenance: false,
				retained_paths: None,
			},
			progress,
			cancellation,
		)
		.map_err(map_merge_error)?;
		Ok(Arc::new(FochAnalysisReview { analyzed }))
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
	analyzed: AnalyzedMerge,
}

impl AnalysisReview for FochAnalysisReview {
	fn state(&self) -> MergeAnalysisState {
		match self.analyzed.analysis().status() {
			MergeAnalysisStatus::ReadyToCommit => MergeAnalysisState::Ready,
			MergeAnalysisStatus::CommittableWithDeferrals => MergeAnalysisState::ReadyWithDeferrals,
			MergeAnalysisStatus::Blocked => MergeAnalysisState::Blocked,
		}
	}

	fn counts(&self) -> MergeUnitCounts {
		let summary = self.analyzed.review_summary();
		MergeUnitCounts {
			total: summary.total,
			safe: summary.safe,
			copy: summary.copy,
			needs_user_choice: summary.needs_user_choice,
			unsupported_input: summary.unsupported_input,
			engine_failure: summary.engine_failure,
			deferred: summary.deferred,
		}
	}

	fn len(&self) -> usize {
		self.analyzed.list_units().len()
	}

	fn unit_at(&self, index: usize) -> Option<MergeUnitDetail> {
		self.analyzed.list_units().get(index).map(unit_view)
	}

	fn unit(&self, id: &str) -> Option<MergeUnitDetail> {
		self.analyzed.unit(id).map(unit_view)
	}
}

fn unit_view(unit: &foch::merge::MergeUnitOutcome) -> MergeUnitDetail {
	let contributor_count = unit.contributors.len();
	MergeUnitDetail {
		id: unit.id.clone(),
		path: unit.path.clone(),
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
			.as_deref()
			.map(|path| bounded_text(path, MAX_DETAIL_TEXT_CHARS)),
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
	InputInspection {
		readiness: match input.readiness {
			foch::input::InputReadiness::Ready => ReadinessState::Ready,
			foch::input::InputReadiness::Blocked => ReadinessState::Blocked,
		},
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
