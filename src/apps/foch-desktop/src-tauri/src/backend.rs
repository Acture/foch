use crate::dto::{
	AnalysisInputMode, AnalysisInputScope, BaseDataState, BaseDataView, DesktopConflictCandidate,
	DesktopConflictLeaf, DesktopDecisionAction, DesktopDecisionOption, DesktopError,
	DesktopResolutionRule, DesktopUnitConflicts, DetectedPlaysetView, InputInspection,
	InputRecoveryOption, InstalledGameView, MergeAnalysisState, MergeDisposition,
	MergeUnitContributor, MergeUnitCounts, MergeUnitDetail, MergeUnitKind, OmittedPlaysetMod,
	PlaysetModView, ReadinessIssue, ReadinessState,
};
use foch::input::{
	CurrentEu4Input, InputCheckObserver, InputPreparationMode, InputRequest,
	NoopInputCheckObserver, PreparedAnalysisInput, inspect_current_eu4_input_with_observer,
};
use foch::merge::{
	AnalyzedMerge, CancellationToken, MergeAnalysisOptions, MergeAnalysisStatus, MergeError,
	ProgressObserver, analyze_merge,
};
use foch::project::{ResolutionEntry, ResolutionMap, render_resolutions_toml};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_INSPECTION_ISSUES: usize = 64;
const MAX_PLAYSET_MODS: usize = 4_096;
const MAX_DEPENDENCIES_PER_MOD: usize = 512;
const MAX_CONTRIBUTORS_PER_UNIT: usize = 256;
const MAX_NOTES_PER_UNIT: usize = 64;
const MAX_SHORT_TEXT_CHARS: usize = 1_024;
const MAX_DETAIL_TEXT_CHARS: usize = 8_192;
/// Hard caps that keep one unit's conflict view bounded on huge mods.
const MAX_UNIT_CONFLICT_GROUPS: usize = 16;
const MAX_CONFLICTS_PER_UNIT: usize = 16;
const MAX_CANDIDATES_PER_CONFLICT: usize = 8;
const MAX_OPTIONS_PER_CONFLICT: usize = 12;
const MAX_CHANGE_SUMMARY_LINES: usize = 8;
const MAX_SNIPPET_CHARS: usize = 2_048;
/// Caps on the resolution rules a session re-analysis may carry.
pub(crate) const MAX_RESOLUTION_RULES: usize = 64;
const MAX_RESOLUTION_TEXT_CHARS: usize = 512;

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

	fn inspect_input_observing(
		&self,
		observer: &mut dyn InputCheckObserver,
	) -> InspectedAnalysisInput {
		let _ = observer;
		self.inspect_input()
	}

	fn run_analysis(
		&self,
		analysis_id: &str,
		request: InputRequest,
		resolutions: &[DesktopResolutionRule],
		progress: &dyn ProgressObserver,
		cancellation: &CancellationToken,
	) -> Result<Arc<dyn AnalysisReview>, AnalysisRunError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FochAnalysisRunner;

impl AnalysisRunner for FochAnalysisRunner {
	fn inspect_input(&self) -> InspectedAnalysisInput {
		self.inspect_input_observing(&mut NoopInputCheckObserver)
	}

	fn inspect_input_observing(
		&self,
		observer: &mut dyn InputCheckObserver,
	) -> InspectedAnalysisInput {
		let input = inspect_current_eu4_input_with_observer(observer);
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
		resolutions: &[DesktopResolutionRule],
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
		// Session-scoped `[[resolutions]]` config: written beside the
		// analysis target, deleted when this run ends however it ends. The
		// user's and any mod's `foch.toml` are never touched.
		let resolution_config_path = write_session_resolution_config(analysis_id, resolutions)?;
		let _resolution_guard = resolution_config_path
			.as_ref()
			.map(|path| ResolutionConfigGuard::new(path));
		let analyzed = analyze_merge(
			request,
			MergeAnalysisOptions {
				out_dir,
				include_game_base: true,
				include_base: false,
				gui_scroll_merge: false,
				force: false,
				// Review mode: conflicts nothing resolves defer for an
				// explicit choice instead of being settled by load order.
				strict: true,
				ignore_replace_path: false,
				dep_overrides: Vec::new(),
				resolution_config_path,
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

/// The session-scoped `foch.toml` sibling of an analysis target, holding only
/// the frontend's explicit `[[resolutions]]` choices.
fn unique_resolution_config_path(analysis_id: &str) -> PathBuf {
	std::env::temp_dir().join(format!("foch-desktop-resolutions-{analysis_id}.toml"))
}

/// Deletes the session resolution config when the analysis run that borrowed
/// it finishes — including a panic or an error unwind.
struct ResolutionConfigGuard {
	path: PathBuf,
}

impl ResolutionConfigGuard {
	fn new(path: &Path) -> Self {
		Self {
			path: path.to_path_buf(),
		}
	}
}

impl Drop for ResolutionConfigGuard {
	fn drop(&mut self) {
		let _ = std::fs::remove_file(&self.path);
	}
}

/// Map frontend resolution rules onto `foch::project::ResolutionEntry` and
/// reject anything the merge's own config loader would reject, so an invalid
/// rule fails closed before any analysis side effects exist.
pub(crate) fn desktop_resolution_entries(
	rules: &[DesktopResolutionRule],
) -> Result<Vec<ResolutionEntry>, DesktopError> {
	if rules.len() > MAX_RESOLUTION_RULES {
		return Err(DesktopError::new(
			"invalid_resolutions",
			format!("at most {MAX_RESOLUTION_RULES} resolution rules may be applied at once"),
		));
	}
	let bounded_field =
		|name: &str, value: &Option<String>| -> Result<Option<String>, DesktopError> {
			value
				.as_deref()
				.map(|text| {
					if text.trim().is_empty() {
						Err(invalid_resolution(name, "must not be blank"))
					} else if text.chars().count() > MAX_RESOLUTION_TEXT_CHARS {
						Err(invalid_resolution(
							name,
							format!("must contain at most {MAX_RESOLUTION_TEXT_CHARS} characters"),
						))
					} else {
						Ok(text.to_string())
					}
				})
				.transpose()
		};
	let mut entries = Vec::with_capacity(rules.len());
	for (index, rule) in rules.iter().enumerate() {
		let file = match &rule.file {
			Some(text) => {
				let bounded = bounded_field("file", &Some(text.clone()))?.expect("checked");
				Some(foch::model::GamePathBuf::parse(&bounded).map_err(|error| {
					DesktopError::new(
						"invalid_resolutions",
						format!("resolution rule {index}: file is not a game path: {error}"),
					)
				})?)
			}
			None => None,
		};
		entries.push(ResolutionEntry {
			file,
			conflict_id: None,
			mod_id: None,
			r#match: bounded_field("matchPattern", &rule.match_pattern)?,
			prefer_mod: bounded_field("preferMod", &rule.prefer_mod)?,
			prefer_candidate: None,
			use_file: None,
			keep_existing: None,
			priority_boost: None,
			handler: bounded_field("handler", &rule.handler)?,
			policy: None,
		});
	}
	ResolutionMap::from_entries(&entries).map_err(|error| {
		DesktopError::new(
			"invalid_resolutions",
			format!("resolution rules were rejected: {error}"),
		)
	})?;
	Ok(entries)
}

fn invalid_resolution(field: &str, problem: impl std::fmt::Display) -> DesktopError {
	DesktopError::new(
		"invalid_resolutions",
		format!("resolution rule field {field} {problem}"),
	)
}

/// The exact `[[resolutions]]` TOML the merge would load for these rules, so
/// a player can copy the same choices into their own `foch.toml`.
pub(crate) fn render_resolution_rules_toml(
	rules: &[DesktopResolutionRule],
) -> Result<String, DesktopError> {
	let entries = desktop_resolution_entries(rules)?;
	Ok(render_resolutions_toml(&entries))
}

fn write_session_resolution_config(
	analysis_id: &str,
	rules: &[DesktopResolutionRule],
) -> Result<Option<PathBuf>, AnalysisRunError> {
	if rules.is_empty() {
		return Ok(None);
	}
	let entries = desktop_resolution_entries(rules)
		.map_err(|error| AnalysisRunError::Failed(error.message))?;
	let path = unique_resolution_config_path(analysis_id);
	std::fs::write(&path, render_resolutions_toml(&entries)).map_err(|error| {
		AnalysisRunError::Failed(format!(
			"failed to write the session resolution config {}: {error}",
			path.display()
		))
	})?;
	Ok(Some(path))
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
		let review = analyzed.review();
		let units = analyzed
			.list_units()
			.iter()
			.map(|unit| unit_view(unit, review))
			.collect();
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

fn unit_view(
	unit: &foch::merge::MergeUnitOutcome,
	review: &foch::merge::MergeReview,
) -> MergeUnitDetail {
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
		conflicts: unit_conflicts_view(unit, review),
	}
}

/// A unit's deferred leaf conflicts as bounded structured views: the address
/// tree flattened to leaves, each with its competing candidates and the
/// explicit decision options the handler contract audits.
fn unit_conflicts_view(
	unit: &foch::merge::MergeUnitOutcome,
	review: &foch::merge::MergeReview,
) -> Vec<DesktopUnitConflicts> {
	review
		.conflicts()
		.iter()
		.filter(|conflicts| conflicts.unit_id == unit.id)
		.take(MAX_UNIT_CONFLICT_GROUPS)
		.map(|conflicts| DesktopUnitConflicts {
			file_path: bounded_text(conflicts.file_path.as_str(), MAX_DETAIL_TEXT_CHARS),
			reason: bounded_text(&conflicts.reason, MAX_DETAIL_TEXT_CHARS),
			leaves: conflicts
				.nodes
				.iter()
				.flat_map(|node| node.conflicts.iter().map(move |leaf| (node, leaf)))
				.take(MAX_CONFLICTS_PER_UNIT)
				.map(|(node, leaf)| conflict_leaf_view(node, leaf, review))
				.collect(),
		})
		.collect()
}

fn conflict_leaf_view(
	node: &foch::merge::AddressNode,
	leaf: &foch::merge::ConflictLeaf,
	review: &foch::merge::MergeReview,
) -> DesktopConflictLeaf {
	DesktopConflictLeaf {
		conflict_id: bounded_text(&leaf.conflict_id, MAX_SHORT_TEXT_CHARS),
		address: bounded_text(&node.address, MAX_SHORT_TEXT_CHARS),
		kind: leaf.kind.map(|kind| kind.as_str().to_string()),
		reason: leaf
			.reason
			.as_deref()
			.map(|reason| bounded_text(reason, MAX_DETAIL_TEXT_CHARS)),
		vanilla_snippet: leaf
			.vanilla_snippet
			.as_deref()
			.map(|snippet| bounded_text(snippet, MAX_SNIPPET_CHARS)),
		candidates: leaf
			.candidates
			.iter()
			.take(MAX_CANDIDATES_PER_CONFLICT)
			.map(|candidate| DesktopConflictCandidate {
				mod_id: bounded_text(&candidate.mod_id, MAX_SHORT_TEXT_CHARS),
				mod_display_name: bounded_text(&candidate.mod_display_name, MAX_SHORT_TEXT_CHARS),
				precedence: candidate.precedence,
				change_summary: candidate
					.change_summary
					.iter()
					.take(MAX_CHANGE_SUMMARY_LINES)
					.map(|line| bounded_text(line, MAX_SHORT_TEXT_CHARS))
					.collect(),
				rendered: bounded_text(&candidate.rendered, MAX_SNIPPET_CHARS),
			})
			.collect(),
		options: review
			.decisions()
			.iter()
			.find(|point| point.conflict_id == leaf.conflict_id)
			.map(|point| {
				point
					.options
					.iter()
					.take(MAX_OPTIONS_PER_CONFLICT)
					.map(decision_option_view)
					.collect()
			})
			.unwrap_or_default(),
	}
}

fn decision_option_view(option: &foch::merge::DecisionOption) -> DesktopDecisionOption {
	DesktopDecisionOption {
		action: match &option.action {
			foch::merge::DecisionAction::PreferMod { mod_id } => DesktopDecisionAction::PreferMod {
				mod_id: bounded_text(mod_id, MAX_SHORT_TEXT_CHARS),
			},
			foch::merge::DecisionAction::Handler { name } => DesktopDecisionAction::Handler {
				name: bounded_text(name, MAX_SHORT_TEXT_CHARS),
			},
		},
		file_resolution: option.scopes.iter().find_map(|scope| match scope {
			foch::merge::DecisionScope::File { resolution } => {
				Some(resolution_rule_view(resolution))
			}
			_ => None,
		}),
		directory_resolution: option.scopes.iter().find_map(|scope| match scope {
			foch::merge::DecisionScope::Directory { resolution } => {
				Some(resolution_rule_view(resolution))
			}
			_ => None,
		}),
	}
}

fn resolution_rule_view(entry: &ResolutionEntry) -> DesktopResolutionRule {
	DesktopResolutionRule {
		match_pattern: entry
			.r#match
			.as_deref()
			.map(|pattern| bounded_text(pattern, MAX_RESOLUTION_TEXT_CHARS)),
		file: entry
			.file
			.as_ref()
			.map(|file| bounded_text(file.as_str(), MAX_RESOLUTION_TEXT_CHARS)),
		prefer_mod: entry
			.prefer_mod
			.as_deref()
			.map(|mod_id| bounded_text(mod_id, MAX_RESOLUTION_TEXT_CHARS)),
		handler: entry
			.handler
			.as_deref()
			.map(|handler| bounded_text(handler, MAX_RESOLUTION_TEXT_CHARS)),
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
			resolutions: &[DesktopResolutionRule],
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
			let resolution_config_path = write_session_resolution_config(analysis_id, resolutions)?;
			let _resolution_guard = resolution_config_path
				.as_ref()
				.map(|path| ResolutionConfigGuard::new(path));
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
					resolution_config_path,
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
			.queue_analysis(
				&inspection.inspection_id,
				AnalysisInputMode::Complete,
				Vec::new(),
			)
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

	/// Runs the real `eu4_two_mod_conflict` fixture through the desktop
	/// backend, offline and without a game base, exactly like the e2e suite.
	struct ConflictFixtureRunner;

	impl AnalysisRunner for ConflictFixtureRunner {
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
					request: conflict_fixture_request(),
					input_scope: AnalysisInputScope {
						mode: AnalysisInputMode::Complete,
						source_mod_count: 3,
						omitted_mods: Vec::new(),
						omitted_mod_count: 0,
						included_mod_count: 3,
					},
				}),
			}
		}

		fn run_analysis(
			&self,
			analysis_id: &str,
			_request: foch::input::InputRequest,
			resolutions: &[DesktopResolutionRule],
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
			let resolution_config_path = write_session_resolution_config(analysis_id, resolutions)?;
			let _resolution_guard = resolution_config_path
				.as_ref()
				.map(|path| ResolutionConfigGuard::new(path));
			let analyzed = foch::merge::analyze_merge(
				conflict_fixture_request(),
				foch::merge::MergeAnalysisOptions {
					out_dir,
					include_game_base: false,
					include_base: false,
					gui_scroll_merge: false,
					force: false,
					strict: true,
					ignore_replace_path: false,
					dep_overrides: Vec::new(),
					resolution_config_path,
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

	fn conflict_fixture_request() -> foch::input::InputRequest {
		foch::input::InputRequest::from_playset_path(
			PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
				"../../../packages/foch/tests/fixtures/playsets/eu4_two_mod_conflict/dlc_load.json",
			),
			foch::input::Config::default(),
		)
	}

	fn conflict_fixture_root() -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("../../../packages/foch/tests/fixtures/playsets/eu4_two_mod_conflict")
	}

	fn religion_leaf(detail: &MergeUnitDetail) -> DesktopConflictLeaf {
		detail
			.conflicts
			.iter()
			.flat_map(|conflicts| conflicts.leaves.iter())
			.find(|leaf| leaf.address == "religion")
			.cloned()
			.expect("the religion leaf conflict")
	}

	fn queue_conflict_analysis(
		state: &crate::state::DesktopState,
		resolutions: Vec<DesktopResolutionRule>,
	) -> String {
		let inspection = state.inspect_input().expect("inspect conflict fixture");
		state
			.queue_analysis(
				&inspection.inspection_id,
				AnalysisInputMode::Complete,
				resolutions,
			)
			.expect("queue conflict analysis")
			.analysis_id
	}

	#[test]
	fn conflict_detail_surfaces_bounded_leaf_conflicts_for_a_real_fixture_merge() {
		let state = crate::state::DesktopState::new(Arc::new(ConflictFixtureRunner));
		let id = queue_conflict_analysis(&state, Vec::new());
		state.run_analysis(&id);
		let summary = state.summary(&id).expect("summary");
		assert_eq!(summary.state, MergeAnalysisState::ReadyWithDeferrals);
		assert!(
			summary.counts.needs_user_choice >= 1,
			"the strict fixture defers its conflict: {summary:?}"
		);
		let detail = state
			.unit(&id, "file:history/countries/TES - Test.txt")
			.expect("the conflicting unit");
		assert_eq!(detail.disposition, MergeDisposition::NeedsUserChoice);
		assert!(!detail.conflicts.is_empty());
		assert!(
			!detail.conflicts[0].file_path.is_empty() && !detail.conflicts[0].reason.is_empty()
		);
		let leaf = religion_leaf(&detail);
		assert!(leaf.reason.is_some(), "{leaf:?}");
		let rendered: Vec<&str> = leaf
			.candidates
			.iter()
			.map(|candidate| candidate.rendered.trim())
			.collect();
		for expected in [
			"religion = catholic",
			"religion = orthodox",
			"religion = protestant",
		] {
			assert!(rendered.contains(&expected), "{leaf:?}");
			assert!(
				leaf.candidates
					.iter()
					.all(|candidate| !candidate.mod_id.is_empty()
						&& !candidate.mod_display_name.is_empty())
			);
		}
		let prefer_options: Vec<&DesktopDecisionOption> = leaf
			.options
			.iter()
			.filter(|option| matches!(option.action, DesktopDecisionAction::PreferMod { .. }))
			.collect();
		assert!(
			prefer_options.iter().any(|option| leaf
				.candidates
				.iter()
				.any(|candidate| matches!(&option.action,
						DesktopDecisionAction::PreferMod { mod_id } if *mod_id == candidate.mod_id))),
			"every candidate is explicitly preferable: {leaf:?}"
		);
		for option in prefer_options {
			let file_rule = option.file_resolution.as_ref().expect("file scope rule");
			assert_eq!(
				file_rule.file.as_deref(),
				Some("history/countries/TES - Test.txt")
			);
			assert!(file_rule.prefer_mod.is_some());
			let directory_rule = option
				.directory_resolution
				.as_ref()
				.expect("directory rule");
			assert!(
				directory_rule
					.match_pattern
					.as_deref()
					.is_some_and(|pattern| pattern.starts_with("re:^history")),
				"{directory_rule:?}"
			);
			assert!(directory_rule.prefer_mod.is_some());
		}
	}

	#[test]
	fn prefer_mod_reanalysis_resolves_the_conflicting_unit_and_commits() {
		let state = crate::state::DesktopState::new(Arc::new(ConflictFixtureRunner));
		let first = queue_conflict_analysis(&state, Vec::new());
		state.run_analysis(&first);
		let detail = state
			.unit(&first, "file:history/countries/TES - Test.txt")
			.expect("the conflicting unit");
		let leaf = religion_leaf(&detail);
		let protestant = leaf
			.candidates
			.iter()
			.find(|candidate| candidate.rendered.trim() == "religion = protestant")
			.expect("the protestant candidate");
		let rule = DesktopResolutionRule {
			match_pattern: Some("history/**".to_string()),
			file: None,
			prefer_mod: Some(protestant.mod_id.clone()),
			handler: None,
		};
		let rendered =
			render_resolution_rules_toml(std::slice::from_ref(&rule)).expect("render rule");
		assert!(rendered.contains("[[resolutions]]"), "{rendered}");
		assert!(rendered.contains("prefer_mod"), "{rendered}");

		let second = queue_conflict_analysis(&state, vec![rule]);
		state.run_analysis(&second);
		let summary = state.summary(&second).expect("summary of the resolved run");
		assert_eq!(
			summary.state,
			MergeAnalysisState::Ready,
			"the explicit prefer_mod rule clears the deferral: {summary:?}"
		);
		assert_eq!(summary.counts.needs_user_choice, 0);
		// The session-scoped config is gone once the analysis finished.
		assert!(!unique_resolution_config_path(&second).exists());

		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged");
		let result = state
			.commit_analysis(&second, &out_dir.display().to_string())
			.expect("commit the resolved merge");
		assert_eq!(result.merge_status, "ready");
		let merged = std::fs::read_to_string(out_dir.join("history/countries/TES - Test.txt"))
			.expect("the merged country history");
		assert!(merged.contains("religion = protestant"), "{merged}");
	}

	#[test]
	fn invalid_resolution_rules_fail_closed_and_never_touch_sources() {
		let state = crate::state::DesktopState::new(Arc::new(ConflictFixtureRunner));
		let source_file =
			conflict_fixture_root().join("mods/conflict_a/history/countries/TES - Test.txt");
		let before = std::fs::read(&source_file).expect("read the fixture source");
		let invalid = [
			// Two actions on one rule.
			DesktopResolutionRule {
				match_pattern: Some("history/**".to_string()),
				file: None,
				prefer_mod: Some("ugc_300002".to_string()),
				handler: Some("last_writer".to_string()),
			},
			// No selector at all.
			DesktopResolutionRule {
				match_pattern: None,
				file: None,
				prefer_mod: Some("ugc_300002".to_string()),
				handler: None,
			},
			// Blank mod id.
			DesktopResolutionRule {
				match_pattern: Some("history/**".to_string()),
				file: None,
				prefer_mod: Some("   ".to_string()),
				handler: None,
			},
			// A file selector that is not a game path.
			DesktopResolutionRule {
				match_pattern: None,
				file: Some("../escape.txt".to_string()),
				prefer_mod: Some("ugc_300002".to_string()),
				handler: None,
			},
		];
		for rule in invalid {
			let inspection = state.inspect_input().expect("inspect conflict fixture");
			let error = state
				.queue_analysis(
					&inspection.inspection_id,
					AnalysisInputMode::Complete,
					vec![rule],
				)
				.expect_err("invalid rule must fail closed");
			assert_eq!(error.code, "invalid_resolutions", "{error:?}");
		}
		// Too many rules is also rejected before anything runs.
		let too_many = vec![
			DesktopResolutionRule {
				match_pattern: Some("history/**".to_string()),
				file: None,
				prefer_mod: Some("ugc_300002".to_string()),
				handler: None,
			};
			MAX_RESOLUTION_RULES + 1
		];
		let inspection = state.inspect_input().expect("inspect for the bulk case");
		let error = state
			.queue_analysis(
				&inspection.inspection_id,
				AnalysisInputMode::Complete,
				too_many,
			)
			.expect_err("bulk rules must fail closed");
		assert_eq!(error.code, "invalid_resolutions");
		assert_eq!(
			std::fs::read(&source_file).expect("read the fixture source again"),
			before,
			"a rejected rule must never touch source mods"
		);
	}
}
