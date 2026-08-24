use super::backend::backend_for;
use super::commit::{
	BaseSnapshotCommitGuard, CommitAuthorization, CommitResult, PriorOutputGuard,
	ProductInputCommitGuard,
};
use super::conflict_handler::ConflictHandler;
use super::error::MergeError;
use super::materialize::{
	MaterializeOutput, MergeMaterializeOptions, freeze_path_plan, materialize_analyzed_input,
};
use super::output::artifact_tree::AnalyzedArtifactTree;
use crate::game::eu4::base::snapshot::InstalledBaseSnapshotIdentity;
use crate::game::eu4::script::emit::EmitOptions;
static VALIDATION_PLAYSET_COUNTER: AtomicU64 = AtomicU64::new(0);
use crate::check::run_checks_with_options;
use crate::input::request::{CheckOptions, InputRequest};
use crate::input::{build_input_inventory_for_paths, resolve_input_from_inventory};
use crate::model::{
	AnalysisMode, ChannelMode, Finding, MERGE_EXECUTION_ATTESTATION_SCHEMA,
	MERGE_PROVENANCE_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH, MERGE_TRACE_ARTIFACT_PATH,
	MergeExecutionAttestation, MergePlanResult, MergeReport, MergeReportBaseSnapshot,
	MergeReportScope, MergeReportStatus, MergeReportValidation,
};
use crate::project::{AppliedDepOverride, Project, ResolutionDecision, ResolutionMap};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::kernel_adapter::MergeBackendId;

pub struct MergeAnalysisOptions {
	pub out_dir: PathBuf,
	pub include_game_base: bool,
	pub include_base: bool,
	pub gui_scroll_merge: bool,
	pub force: bool,
	pub ignore_replace_path: bool,
	pub dep_overrides: Vec<AppliedDepOverride>,
	/// Optional explicit foch.toml path supplied by the CLI.
	pub resolution_config_path: Option<PathBuf>,
	/// Optional frontend-provided handler for post-pass conflict prompts.
	pub interactive_conflict_handler: Option<Box<dyn ConflictHandler>>,
	/// foch.toml path where interactive prompt decisions should be persisted.
	pub interactive_resolution_config_path: Option<PathBuf>,
	/// Caller-computed playset fingerprint to stamp on the merge report so
	/// subsequent runs can detect "same mod set, reuse the cached output".
	/// `None` skips the stamp (e.g., merge invoked from a context where
	/// computing it isn't possible).
	pub playset_fingerprint: Option<String>,
	/// Annotate merged definitions with their adopted source mods (inline
	/// `# foch: …` comments + a `.foch/foch-provenance.json` sidecar). Off by
	/// default; when off, emitted output is byte-identical to a normal merge.
	pub provenance: bool,
	/// Optional relative-path retention set for scoring callers that only need
	/// target corpus paths. Full production merge leaves this unset.
	pub retained_paths: Option<BTreeSet<String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeAnalysisStatus {
	ReadyToCommit,
	CommittableWithDeferrals,
	Blocked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeAnalysisStage {
	Inventory,
	ResolveInput,
	SemanticMerge,
	ValidateOutput,
	FreezeArtifacts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MergeProgress {
	pub stage: MergeAnalysisStage,
	pub completed: bool,
	pub completed_units: Option<u64>,
	pub total_units: Option<u64>,
	pub elapsed: Duration,
}

pub trait ProgressObserver: Send + Sync {
	fn update(&self, progress: MergeProgress);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopProgressObserver;

impl ProgressObserver for NoopProgressObserver {
	fn update(&self, _progress: MergeProgress) {}
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
	cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn cancel(&self) {
		self.cancelled.store(true, Ordering::Release);
	}

	pub fn is_cancelled(&self) -> bool {
		self.cancelled.load(Ordering::Acquire)
	}

	pub(crate) fn check(&self) -> Result<(), MergeError> {
		if self.is_cancelled() {
			Err(MergeError::Cancelled)
		} else {
			Ok(())
		}
	}
}

#[derive(Clone, Debug)]
pub struct MergeAnalysis {
	pub(super) plan: MergePlanResult,
	pub(super) report: MergeReport,
	pub(super) merge_status: MergeStatusView,
	pub(super) analysis_status: AnalysisStatusView,
	status: MergeAnalysisStatus,
	activation_safe: bool,
	artifact_file_count: usize,
}

impl MergeAnalysis {
	pub fn plan(&self) -> &MergePlanResult {
		&self.plan
	}

	pub fn report(&self) -> &MergeReport {
		&self.report
	}

	pub fn merge_status(&self) -> &MergeStatusView {
		&self.merge_status
	}

	pub fn analysis_status(&self) -> &AnalysisStatusView {
		&self.analysis_status
	}

	pub fn status(&self) -> MergeAnalysisStatus {
		self.status
	}

	pub fn activation_safe(&self) -> bool {
		self.activation_safe
	}

	pub fn artifact_file_count(&self) -> usize {
		self.artifact_file_count
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergeStatusView {
	pub status: MergeReportStatus,
	pub manual_conflict_count: usize,
	pub unsupported_input_count: usize,
	pub engine_failure_count: usize,
	pub handler_resolution_count: usize,
	pub generated_file_count: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnalysisStatusView {
	pub fatal_errors: usize,
	pub strict_findings: usize,
	pub advisory_findings: usize,
	pub parse_errors: usize,
	pub unresolved_references: usize,
	pub missing_localisation: usize,
}

pub fn run_merge_with_options(
	request: InputRequest,
	options: MergeAnalysisOptions,
) -> Result<CommitResult, MergeError> {
	analyze_merge(
		request,
		options,
		&NoopProgressObserver,
		&CancellationToken::new(),
	)?
	.commit(CommitAuthorization::EmptyTargetOnly)
}

/// Run the complete semantic merge and freeze its exact output bytes without
/// touching the requested target directory.
///
/// [`AnalyzedMerge::commit`] only validates drift and atomically installs the
/// reviewed bytes; it never invokes a merge backend or schema engine again.
pub fn analyze_merge(
	request: InputRequest,
	options: MergeAnalysisOptions,
	progress: &dyn ProgressObserver,
	cancellation: &CancellationToken,
) -> Result<AnalyzedMerge, MergeError> {
	analyze_merge_with_backend_and_observer(
		request,
		options,
		MergeBackendId::GumtreePcsNway,
		progress,
		cancellation,
	)
}

pub fn run_merge_for_evaluation(
	request: InputRequest,
	options: MergeAnalysisOptions,
	backend: MergeBackendId,
) -> Result<CommitResult, MergeError> {
	analyze_merge_with_backend_and_observer(
		request,
		options,
		backend,
		&NoopProgressObserver,
		&CancellationToken::new(),
	)?
	.commit(CommitAuthorization::EmptyTargetOnly)
}

pub struct AnalyzedMerge {
	pub(super) out_dir: PathBuf,
	pub(super) analysis: MergeAnalysis,
	pub(super) artifacts: AnalyzedArtifactTree,
	pub(super) base_snapshot_commit_guard: Option<BaseSnapshotCommitGuard>,
	pub(super) product_input_commit_guard: Option<ProductInputCommitGuard>,
	pub(super) prior_output_guard: Option<PriorOutputGuard>,
}

struct PendingAnalysis {
	request: InputRequest,
	options: MergeAnalysisOptions,
	backend_id: MergeBackendId,
	input_result: Result<crate::input::ResolvedInput, crate::input::InputResolveError>,
	plan: MergePlanResult,
	resolution_map: ResolutionMap,
	emit_options: EmitOptions,
	frozen_external_files: BTreeMap<PathBuf, Vec<u8>>,
	base_snapshot_commit_guard: Option<BaseSnapshotCommitGuard>,
	product_input_commit_guard: Option<ProductInputCommitGuard>,
	execution_attestation: MergeExecutionAttestation,
}

impl AnalyzedMerge {
	pub fn analysis(&self) -> &MergeAnalysis {
		&self.analysis
	}
}

fn analyze_merge_with_backend_and_observer(
	request: InputRequest,
	options: MergeAnalysisOptions,
	backend_id: MergeBackendId,
	progress: &dyn ProgressObserver,
	cancellation: &CancellationToken,
) -> Result<AnalyzedMerge, MergeError> {
	let analysis_started = Instant::now();
	cancellation.check()?;
	notify_progress(
		progress,
		analysis_started,
		MergeAnalysisStage::Inventory,
		false,
		Some(0),
		None,
	);
	let inventory_started = Instant::now();
	let mut inventory_result = build_input_inventory_for_paths(
		&request,
		options.include_game_base,
		options.retained_paths.as_ref(),
	);
	if let Ok(inventory) = inventory_result.as_ref() {
		let versioned_mods = inventory
			.mod_hashes
			.iter()
			.filter(|hash| hash.is_some())
			.count();
		eprintln!(
			"[merge] build_input_inventory: done elapsed_ms={} mods={} versioned_mods={}",
			inventory_started.elapsed().as_millis(),
			inventory.mods.len(),
			versioned_mods
		);
	}
	if options.retained_paths.is_some()
		&& let Ok(inventory) = inventory_result.as_mut()
		&& inventory.mod_cache_game_version.is_none()
	{
		inventory.mod_cache_game_version = Some("unknown".to_string());
		inventory.cache_game_version = Some(format!("{} unknown", inventory.playlist.game.key()));
	}
	if let Ok(inventory) = inventory_result.as_mut() {
		inventory.defer_base_snapshot_current_validation();
	}
	let base_snapshot_commit_guard = match inventory_result.as_ref() {
		Ok(inventory) => BaseSnapshotCommitGuard::from_inventory(inventory)?,
		Err(_) => None,
	};
	let product_input_commit_guard = inventory_result.as_ref().ok().and_then(|inventory| {
		ProductInputCommitGuard::from_inventory(
			request.clone(),
			options.retained_paths.clone(),
			inventory,
		)
	});
	let execution_attestation = merge_execution_attestation(
		backend_id,
		options.retained_paths.is_some(),
		options.include_game_base,
		base_snapshot_commit_guard.as_ref(),
	);
	let LoadedMergePolicy {
		resolution_map,
		emit_options,
		frozen_external_files,
	} = load_merge_policy(&request, options.resolution_config_path.as_deref())?;
	let inventory_units = inventory_result
		.as_ref()
		.ok()
		.map_or(0, |inventory| inventory.mods.len() as u64);
	notify_progress(
		progress,
		analysis_started,
		MergeAnalysisStage::Inventory,
		true,
		Some(inventory_units),
		Some(inventory_units),
	);
	cancellation.check()?;
	notify_progress(
		progress,
		analysis_started,
		MergeAnalysisStage::ResolveInput,
		false,
		Some(0),
		None,
	);
	let resolve_started = Instant::now();
	let mut input_result = inventory_result.and_then(resolve_input_from_inventory);
	match input_result.as_ref() {
		Ok(input) => {
			let cache_hits = input
				.mod_snapshots
				.iter()
				.flatten()
				.filter(|snapshot| snapshot.cache_hit)
				.count();
			let cache_misses = input
				.mod_snapshots
				.iter()
				.flatten()
				.filter(|snapshot| !snapshot.cache_hit)
				.count();
			eprintln!(
				"[merge] resolve_input: done elapsed_ms={} mods={} files={} requested_paths={} effective_paths={} mod_snapshot_cache_hits={} mod_snapshot_cache_misses={}",
				resolve_started.elapsed().as_millis(),
				input.mods.len(),
				input.file_inventory.len(),
				input
					.requested_retained_paths
					.as_ref()
					.map_or(0, BTreeSet::len),
				input
					.effective_retained_paths
					.as_ref()
					.map_or(0, BTreeSet::len),
				cache_hits,
				cache_misses
			);
		}
		Err(err) => eprintln!(
			"[merge] resolve_input: error elapsed_ms={} kind={:?} message={}",
			resolve_started.elapsed().as_millis(),
			err.kind,
			err.message
		),
	}
	let plan = freeze_path_plan(
		&mut input_result,
		options.include_game_base,
		&resolution_map,
	);
	let plan_units = plan.paths.len() as u64;
	notify_progress(
		progress,
		analysis_started,
		MergeAnalysisStage::ResolveInput,
		true,
		Some(plan_units),
		Some(plan_units),
	);
	cancellation.check()?;

	complete_merge_analysis(
		PendingAnalysis {
			request,
			options,
			backend_id,
			input_result,
			plan,
			resolution_map,
			emit_options,
			frozen_external_files,
			base_snapshot_commit_guard,
			product_input_commit_guard,
			execution_attestation,
		},
		progress,
		cancellation,
		analysis_started,
	)
}

fn complete_merge_analysis(
	pending: PendingAnalysis,
	progress: &dyn ProgressObserver,
	cancellation: &CancellationToken,
	analysis_started: Instant,
) -> Result<AnalyzedMerge, MergeError> {
	let PendingAnalysis {
		request,
		options,
		backend_id,
		input_result,
		plan,
		resolution_map,
		emit_options,
		frozen_external_files,
		base_snapshot_commit_guard,
		product_input_commit_guard,
		execution_attestation,
	} = pending;

	let final_out_dir = options.out_dir.clone();
	let prior_out_dir = final_out_dir.is_dir().then_some(final_out_dir.as_path());
	let artifact_root = AnalyzedArtifactTree::create()?;
	let staging_dir = artifact_root.path().to_path_buf();
	cancellation.check()?;
	let semantic_total = plan.paths.len() as u64;
	notify_progress(
		progress,
		analysis_started,
		MergeAnalysisStage::SemanticMerge,
		false,
		Some(0),
		Some(semantic_total),
	);
	let effective_retained_paths = input_result
		.as_ref()
		.ok()
		.and_then(|input| input.effective_retained_paths.clone());
	let mut report = materialize_analyzed_input(
		request.clone(),
		MaterializeOutput {
			artifacts_dir: &staging_dir,
			prior_dir: prior_out_dir,
			target_dir: &final_out_dir,
		},
		MergeMaterializeOptions {
			include_game_base: options.include_game_base,
			include_base: options.include_base,
			gui_scroll_merge: options.gui_scroll_merge,
			force: options.force,
			ignore_replace_path: options.ignore_replace_path,
			dep_overrides: options.dep_overrides.clone(),
			resolution_map,
			emit_options,
			frozen_external_files,
			interactive_conflict_handler: options.interactive_conflict_handler,
			interactive_resolution_config_path: options.interactive_resolution_config_path,
			provenance: options.provenance,
			backend: backend_for(backend_id),
			retained_paths: effective_retained_paths,
			cancellation: cancellation.clone(),
		},
		input_result,
		plan.clone(),
		Some((progress, analysis_started)),
	)?;
	notify_progress(
		progress,
		analysis_started,
		MergeAnalysisStage::SemanticMerge,
		true,
		Some(semantic_total),
		Some(semantic_total),
	);
	cancellation.check()?;
	report.playset_fingerprint = options.playset_fingerprint.clone();
	report.execution = Some(execution_attestation);
	report.input = product_input_commit_guard
		.as_ref()
		.map(|guard| guard.expected.clone());

	if !matches!(
		report.status,
		MergeReportStatus::Fatal | MergeReportStatus::Blocked
	) && options.retained_paths.is_none()
	{
		notify_progress(
			progress,
			analysis_started,
			MergeAnalysisStage::ValidateOutput,
			false,
			Some(0),
			Some(report.generated_file_count as u64),
		);
		report.validation = revalidate_generated_output(
			&request,
			&staging_dir,
			options.include_game_base,
			base_snapshot_commit_guard
				.as_ref()
				.map(|guard| guard.identity.clone()),
		)?;
		let generated = report.generated_file_count as u64;
		notify_progress(
			progress,
			analysis_started,
			MergeAnalysisStage::ValidateOutput,
			true,
			Some(generated),
			Some(generated),
		);
	}
	cancellation.check()?;
	let execution = merge_execution_result(report);
	write_merge_report_artifact(&staging_dir, &execution.report)?;
	let prior_output_guard = PriorOutputGuard::from_report(&final_out_dir, &execution.report)?;
	notify_progress(
		progress,
		analysis_started,
		MergeAnalysisStage::FreezeArtifacts,
		false,
		Some(0),
		None,
	);
	let artifacts = AnalyzedArtifactTree::freeze(artifact_root)?;
	let artifact_count = artifacts.file_count() as u64;
	notify_progress(
		progress,
		analysis_started,
		MergeAnalysisStage::FreezeArtifacts,
		true,
		Some(artifact_count),
		Some(artifact_count),
	);
	let status = merge_analysis_status(&execution);
	let activation_safe = status == MergeAnalysisStatus::ReadyToCommit;
	let analysis = MergeAnalysis {
		plan,
		report: execution.report,
		merge_status: execution.merge_status,
		analysis_status: execution.analysis_status,
		status,
		activation_safe,
		artifact_file_count: artifacts.file_count(),
	};
	Ok(AnalyzedMerge {
		out_dir: final_out_dir,
		analysis,
		artifacts,
		base_snapshot_commit_guard,
		product_input_commit_guard,
		prior_output_guard,
	})
}

fn notify_progress(
	observer: &dyn ProgressObserver,
	started: Instant,
	stage: MergeAnalysisStage,
	completed: bool,
	completed_units: Option<u64>,
	total_units: Option<u64>,
) {
	observer.update(MergeProgress {
		stage,
		completed,
		completed_units,
		total_units,
		elapsed: started.elapsed(),
	});
}

fn merge_execution_attestation(
	backend: MergeBackendId,
	retained_path_evaluation: bool,
	include_game_base: bool,
	base_snapshot: Option<&BaseSnapshotCommitGuard>,
) -> MergeExecutionAttestation {
	let scope = if retained_path_evaluation {
		MergeReportScope::RetainedPathEvaluation
	} else {
		MergeReportScope::FullProductMerge
	};
	let base_snapshot = if !include_game_base {
		MergeReportBaseSnapshot::Disabled
	} else if let Some(base_snapshot) = base_snapshot {
		MergeReportBaseSnapshot::Resolved {
			identity: base_snapshot.identity.as_label(),
		}
	} else {
		MergeReportBaseSnapshot::Unavailable
	};
	MergeExecutionAttestation {
		schema: MERGE_EXECUTION_ATTESTATION_SCHEMA.to_string(),
		backend,
		scope,
		base_snapshot,
	}
}

pub(super) fn merge_execution_result(mut report: MergeReport) -> CommitResult {
	let merge_status = compute_merge_status(&report);
	report.status = merge_status.status;
	let analysis_status = compute_analysis_status(&report);
	let exit_code = merge_execution_exit_code(&merge_status, &analysis_status);
	CommitResult {
		report,
		merge_status,
		analysis_status,
		exit_code,
	}
}

fn merge_analysis_status(execution: &CommitResult) -> MergeAnalysisStatus {
	if matches!(
		execution.merge_status.status,
		MergeReportStatus::Fatal | MergeReportStatus::Blocked
	) || execution.analysis_status.fatal_errors > 0
	{
		MergeAnalysisStatus::Blocked
	} else if execution.merge_status.status == MergeReportStatus::PartialSuccess {
		MergeAnalysisStatus::CommittableWithDeferrals
	} else {
		MergeAnalysisStatus::ReadyToCommit
	}
}

pub(super) fn commit_exit_code(analysis: &MergeAnalysis) -> i32 {
	merge_execution_exit_code(&analysis.merge_status, &analysis.analysis_status)
}

fn compute_merge_status(report: &MergeReport) -> MergeStatusView {
	let status = if report.status == MergeReportStatus::Fatal {
		MergeReportStatus::Fatal
	} else if report.status == MergeReportStatus::Blocked {
		MergeReportStatus::Blocked
	} else if report.deferred_unit_count() > 0 || !report.handler_resolutions.is_empty() {
		MergeReportStatus::PartialSuccess
	} else {
		MergeReportStatus::Ready
	};

	MergeStatusView {
		status,
		manual_conflict_count: report.manual_conflict_count,
		unsupported_input_count: report.unsupported_input_count,
		engine_failure_count: report.engine_failure_count,
		handler_resolution_count: report.handler_resolutions.len(),
		generated_file_count: report.generated_file_count,
	}
}

fn compute_analysis_status(report: &MergeReport) -> AnalysisStatusView {
	AnalysisStatusView {
		fatal_errors: report.validation.fatal_errors,
		strict_findings: report.validation.strict_findings,
		advisory_findings: report.validation.advisory_findings,
		parse_errors: report.validation.parse_errors,
		unresolved_references: report.validation.unresolved_references,
		missing_localisation: report.validation.missing_localisation,
	}
}

fn merge_execution_exit_code(
	merge_status: &MergeStatusView,
	analysis_status: &AnalysisStatusView,
) -> i32 {
	if merge_status.status == MergeReportStatus::Fatal || analysis_status.fatal_errors > 0 {
		1
	} else if merge_status.status == MergeReportStatus::Blocked {
		2
	} else {
		0
	}
}

struct LoadedMergePolicy {
	resolution_map: ResolutionMap,
	emit_options: EmitOptions,
	frozen_external_files: BTreeMap<PathBuf, Vec<u8>>,
}

fn load_merge_policy(
	request: &InputRequest,
	explicit_path: Option<&Path>,
) -> Result<LoadedMergePolicy, MergeError> {
	let playset_root = request
		.source_path()
		.parent()
		.unwrap_or_else(|| Path::new("."));
	let config = if let Some(path) = explicit_path {
		Project::load_from_path(path).map_err(|err| MergeError::Validation {
			path: Some(path.display().to_string()),
			message: err.to_string(),
		})?
	} else {
		Project::try_load(playset_root).map_err(|err| MergeError::Validation {
			path: Some(playset_root.display().to_string()),
			message: err.to_string(),
		})?
	};
	let resolution_map =
		ResolutionMap::from_entries(&config.resolutions).map_err(|err| MergeError::Validation {
			path: Some(explicit_path.unwrap_or(playset_root).display().to_string()),
			message: err.to_string(),
		})?;
	let emit_options = EmitOptions::with_indent(config.emit_indent());
	let frozen_external_files = freeze_external_resolution_files(&resolution_map)?;
	Ok(LoadedMergePolicy {
		resolution_map,
		emit_options,
		frozen_external_files,
	})
}

fn freeze_external_resolution_files(
	resolution_map: &ResolutionMap,
) -> Result<BTreeMap<PathBuf, Vec<u8>>, MergeError> {
	let decisions = resolution_map
		.by_file
		.values()
		.chain(resolution_map.by_conflict_id.values())
		.chain(
			resolution_map
				.pattern_rules
				.iter()
				.map(|rule| &rule.decision),
		);
	let mut sources = BTreeSet::new();
	for decision in decisions {
		if let ResolutionDecision::UseFile(path) = decision {
			sources.insert(path.clone());
		}
	}

	let mut frozen = BTreeMap::new();
	for path in sources {
		let bytes = fs::read(&path).map_err(|err| MergeError::Validation {
			path: Some(path.display().to_string()),
			message: format!("failed to freeze external resolution source: {err}"),
		})?;
		frozen.insert(path, bytes);
	}
	Ok(frozen)
}

fn revalidate_generated_output(
	request: &InputRequest,
	out_dir: &Path,
	include_game_base: bool,
	base_snapshot_lease: Option<InstalledBaseSnapshotIdentity>,
) -> Result<MergeReportValidation, MergeError> {
	let canonical_out_dir = out_dir
		.canonicalize()
		.unwrap_or_else(|_| out_dir.to_path_buf());
	let parent_dir = canonical_out_dir
		.parent()
		.ok_or_else(|| MergeError::Validation {
			path: Some(canonical_out_dir.display().to_string()),
			message: format!(
				"generated output {} has no parent directory",
				canonical_out_dir.display()
			),
		})?;
	let out_dir_name = canonical_out_dir
		.file_name()
		.ok_or_else(|| MergeError::Validation {
			path: Some(canonical_out_dir.display().to_string()),
			message: format!(
				"generated output {} has no terminal directory name",
				canonical_out_dir.display()
			),
		})?
		.to_string_lossy();
	let validation_dir = validation_playlist_dir(parent_dir);
	fs::create_dir_all(validation_dir.join("mod")).map_err(|err| {
		MergeError::Io(io::Error::other(format!(
			"failed to create validation playset dir {}: {err}",
			validation_dir.display()
		)))
	})?;
	let synthetic_steam_id = format!("validation_{out_dir_name}");
	let descriptor_rel = format!("mod/ugc_{synthetic_steam_id}.mod");
	let dlc_load = serde_json::json!({
		"enabled_mods": [descriptor_rel.clone()],
		"disabled_dlcs": Vec::<String>::new(),
	});
	let dlc_load_bytes = serde_json::to_vec_pretty(&dlc_load).map_err(|err| {
		MergeError::Io(io::Error::other(format!(
			"failed to serialize validation dlc_load.json: {err}"
		)))
	})?;
	let dlc_load_path = validation_dir.join("dlc_load.json");
	fs::write(&dlc_load_path, dlc_load_bytes)?;
	let descriptor_body = format!(
		"name=\"{}\"\npath=\"{}\"\nremote_file_id=\"{}\"\n",
		escape_descriptor_value(&out_dir_name),
		escape_descriptor_value(&normalize_descriptor_path(&canonical_out_dir)),
		escape_descriptor_value(&synthetic_steam_id)
	);
	fs::write(validation_dir.join(&descriptor_rel), descriptor_body)?;

	let mut cleanup_error = None;
	let mut validation_request = InputRequest::new(
		crate::input::request::InputSource::DlcLoad(dlc_load_path.clone()),
		request.config.clone(),
	)
	.with_base_snapshot_lease(base_snapshot_lease);
	if let Some(expected) = request.expected_base_snapshot_identity.as_ref() {
		validation_request =
			validation_request.with_expected_base_snapshot_identity(expected.clone());
	}
	let result = run_checks_with_options(
		validation_request,
		CheckOptions {
			analysis_mode: AnalysisMode::Semantic,
			channel_mode: ChannelMode::All,
			include_game_base,
		},
	);
	if let Err(err) = fs::remove_dir_all(&validation_dir) {
		cleanup_error = Some(MergeError::Io(io::Error::other(format!(
			"failed to remove validation playset dir {}: {err}",
			validation_dir.display()
		))));
	}
	if let Some(err) = cleanup_error {
		return Err(err);
	}

	Ok(MergeReportValidation {
		fatal_errors: result.fatal_errors.len(),
		strict_findings: result.strict_findings.len(),
		advisory_findings: result.advisory_findings.len(),
		parse_errors: result.analysis_meta.parse_errors,
		unresolved_references: count_findings_for_rules(
			&result.findings,
			&[
				"unresolved-call-target",
				"missing-effect-parameter",
				"unresolved-flag-reference",
			],
		),
		missing_localisation: count_findings_for_rules(&result.findings, &["missing-localisation"]),
	})
}

fn count_findings_for_rules(findings: &[Finding], rule_ids: &[&str]) -> usize {
	findings
		.iter()
		.filter(|finding| rule_ids.contains(&finding.rule_id.as_str()))
		.count()
}

pub(super) fn write_merge_report_artifact(
	out_dir: &Path,
	report: &MergeReport,
) -> Result<(), MergeError> {
	let path = out_dir.join(MERGE_REPORT_ARTIFACT_PATH);
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let bytes = serde_json::to_vec_pretty(report).map_err(|err| {
		MergeError::Io(io::Error::other(format!(
			"failed to serialize merge report {}: {err}",
			path.display()
		)))
	})?;
	fs::write(path, bytes)?;
	write_provenance_artifact(out_dir, report)?;
	write_merge_trace_artifact(out_dir, report)?;
	Ok(())
}

/// Write the `.foch/foch-provenance.json` sidecar when provenance was collected
/// (i.e. `--provenance` was on). When the map is empty the sidecar is omitted,
/// and any stale relic from a previous provenance run is removed so toggling the
/// flag off leaves a clean tree.
fn write_provenance_artifact(out_dir: &Path, report: &MergeReport) -> Result<(), MergeError> {
	let path = out_dir.join(MERGE_PROVENANCE_ARTIFACT_PATH);
	if report.definition_provenance.is_empty() {
		if path.exists() {
			let _ = fs::remove_file(&path);
		}
		return Ok(());
	}
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let bytes = serde_json::to_vec_pretty(&report.definition_provenance).map_err(|err| {
		MergeError::Io(io::Error::other(format!(
			"failed to serialize provenance sidecar {}: {err}",
			path.display()
		)))
	})?;
	fs::write(path, bytes)?;
	Ok(())
}

fn write_merge_trace_artifact(out_dir: &Path, report: &MergeReport) -> Result<(), MergeError> {
	let path = out_dir.join(MERGE_TRACE_ARTIFACT_PATH);
	if report.merge_trace.is_empty() {
		if path.exists() {
			let _ = fs::remove_file(&path);
		}
		return Ok(());
	}
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let bytes = serde_json::to_vec_pretty(&report.merge_trace).map_err(|err| {
		MergeError::Io(io::Error::other(format!(
			"failed to serialize merge trace sidecar {}: {err}",
			path.display()
		)))
	})?;
	fs::write(path, bytes)?;
	Ok(())
}

fn validation_playlist_dir(parent_dir: &Path) -> PathBuf {
	let pid = std::process::id();
	let nonce = VALIDATION_PLAYSET_COUNTER.fetch_add(1, Ordering::Relaxed);
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|duration| duration.as_nanos())
		.unwrap_or_default();
	parent_dir.join(format!(".foch-merge-validation-{pid}-{nanos}-{nonce}"))
}

fn normalize_descriptor_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

fn escape_descriptor_value(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::game::eu4::Eu4;
	use crate::game::eu4::base::snapshot::{
		BASE_DATA_DIR_ENV, BASE_DATA_ENV_LOCK, BaseDataSource, build_base_snapshot,
		clear_cached_loaded_base_snapshot, install_built_snapshot,
		installed_snapshot_cold_decode_count, installed_snapshot_current_digest_count,
		installed_snapshot_current_validation_count, installed_snapshot_file_read_count,
		reset_installed_snapshot_test_counters,
	};
	use crate::input::FileFilter;
	use crate::input::config::Config;
	use crate::model::HandlerResolutionRecord;
	use std::collections::HashMap;
	use std::sync::Mutex;

	#[derive(Default)]
	struct RecordingProgressObserver {
		updates: Mutex<Vec<MergeProgress>>,
	}

	impl ProgressObserver for RecordingProgressObserver {
		fn update(&self, progress: MergeProgress) {
			self.updates
				.lock()
				.expect("progress observer lock")
				.push(progress);
		}
	}

	impl RecordingProgressObserver {
		fn updates(&self) -> Vec<MergeProgress> {
			self.updates.lock().expect("progress observer lock").clone()
		}
	}

	struct CancellingProgressObserver {
		cancellation: CancellationToken,
	}

	impl ProgressObserver for CancellingProgressObserver {
		fn update(&self, progress: MergeProgress) {
			if progress.stage == MergeAnalysisStage::SemanticMerge && !progress.completed {
				self.cancellation.cancel();
			}
		}
	}

	fn analyze_merge_for_test(
		request: InputRequest,
		options: MergeAnalysisOptions,
	) -> Result<AnalyzedMerge, MergeError> {
		analyze_merge(
			request,
			options,
			&NoopProgressObserver,
			&CancellationToken::new(),
		)
	}

	fn report_with(mut update: impl FnMut(&mut MergeReport)) -> MergeReport {
		let mut report = MergeReport::default();
		update(&mut report);
		report
	}

	#[test]
	fn product_attestation_distinguishes_scope_backend_and_disabled_base() {
		let attestation =
			merge_execution_attestation(MergeBackendId::GumtreePcsNway, false, false, None);

		assert_eq!(attestation.schema, MERGE_EXECUTION_ATTESTATION_SCHEMA);
		assert_eq!(attestation.backend, MergeBackendId::GumtreePcsNway);
		assert_eq!(attestation.scope, MergeReportScope::FullProductMerge);
		assert_eq!(attestation.base_snapshot, MergeReportBaseSnapshot::Disabled);

		let evaluation =
			merge_execution_attestation(MergeBackendId::AddressPatch, true, true, None);
		assert_eq!(evaluation.backend, MergeBackendId::AddressPatch);
		assert_eq!(evaluation.scope, MergeReportScope::RetainedPathEvaluation);
		assert_eq!(
			evaluation.base_snapshot,
			MergeReportBaseSnapshot::Unavailable
		);
	}

	#[test]
	fn analysis_freezes_complete_output_without_creating_target() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("tests/fixtures/playsets/eu4_minimal_passthrough");
		let out_dir = temp.path().join("out");
		let analyzed = analyze_merge_for_test(
			InputRequest::from_playset_path(
				fixture.join("dlc_load.json"),
				Config {
					steam_root_path: None,
					paradox_data_path: None,
					game_path: HashMap::new(),
					extra_ignore_patterns: Vec::new(),
				},
			),
			MergeAnalysisOptions {
				out_dir: out_dir.clone(),
				include_game_base: false,
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
		)
		.expect("analyze merge");

		assert!(analyzed.analysis().plan().strategies.total_paths > 0);
		assert!(analyzed.analysis().artifact_file_count() > 0);
		assert_ne!(analyzed.analysis().status(), MergeAnalysisStatus::Blocked);
		assert!(!out_dir.exists(), "analysis must not touch the output path");
		let reviewed_plan =
			serde_json::to_value(analyzed.analysis().plan()).expect("serialize reviewed plan");
		analyzed
			.commit(CommitAuthorization::EmptyTargetOnly)
			.expect("commit analyzed merge");
		let persisted_plan: serde_json::Value = serde_json::from_slice(
			&fs::read(out_dir.join(crate::model::MERGE_PLAN_ARTIFACT_PATH))
				.expect("read persisted plan"),
		)
		.expect("decode persisted plan");
		assert_eq!(persisted_plan, reviewed_plan);
	}

	#[test]
	fn analysis_freezes_emit_options() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let root = temp.path();
		let mod_root = root.join("mods/minimal");
		fs::create_dir_all(root.join("mod")).expect("create descriptor dir");
		fs::create_dir_all(mod_root.join("common/cultures")).expect("create culture dir");
		fs::write(
			root.join("dlc_load.json"),
			r#"{"enabled_mods":["mod/ugc_1.mod"],"disabled_dlcs":[]}"#,
		)
		.expect("write playset");
		fs::write(
			root.join("mod/ugc_1.mod"),
			"name=\"Minimal\"\npath=\"mods/minimal\"\nremote_file_id=\"1\"\n",
		)
		.expect("write launcher descriptor");
		fs::write(
			mod_root.join("descriptor.mod"),
			"name=\"Minimal\"\nremote_file_id=\"1\"\n",
		)
		.expect("write mod descriptor");
		fs::write(
			mod_root.join("common/cultures/test.txt"),
			"test_group = { graphical_culture = testgfx }\n",
		)
		.expect("write culture");
		let config_path = root.join("foch.toml");
		fs::write(&config_path, "[emit]\nindent = \"  \"\n").expect("write emit config");
		let out_dir = root.join("out");
		let analyzed = analyze_merge_for_test(
			InputRequest::from_playset_path(root.join("dlc_load.json"), Config::default()),
			MergeAnalysisOptions {
				out_dir: out_dir.clone(),
				include_game_base: false,
				include_base: false,
				gui_scroll_merge: false,
				force: false,
				ignore_replace_path: false,
				dep_overrides: Vec::new(),
				resolution_config_path: Some(config_path.clone()),
				interactive_conflict_handler: None,
				interactive_resolution_config_path: None,
				playset_fingerprint: None,
				provenance: false,
				retained_paths: None,
			},
		)
		.expect("analyze merge");

		fs::write(&config_path, "[emit]\nindent = \"\\t\"\n").expect("mutate emit config");
		analyzed
			.commit(CommitAuthorization::EmptyTargetOnly)
			.expect("commit analyzed merge");
		let output = fs::read_to_string(out_dir.join("common/cultures/zzz_foch_cultures.txt"))
			.expect("read generated culture module");
		assert!(output.contains("\n  graphical_culture"), "{output}");
		assert!(!output.contains("\n\tgraphical_culture"), "{output}");
	}

	#[test]
	fn cancelled_analysis_stops_before_reading_inputs() {
		let cancellation = CancellationToken::new();
		cancellation.cancel();
		let error = analyze_merge(
			InputRequest::from_playset_path(
				PathBuf::from("missing/cancelled-dlc-load.json"),
				Config::default(),
			),
			MergeAnalysisOptions {
				out_dir: PathBuf::from("target/cancelled-analysis-must-not-exist"),
				include_game_base: false,
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
			&NoopProgressObserver,
			&cancellation,
		)
		.err()
		.expect("cancelled analysis must stop immediately");

		assert!(matches!(error, MergeError::Cancelled));
	}

	#[test]
	fn cancellation_during_semantic_merge_leaves_target_untouched() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("tests/fixtures/playsets/eu4_minimal_passthrough");
		let out_dir = temp.path().join("out");
		let cancellation = CancellationToken::new();
		let observer = CancellingProgressObserver {
			cancellation: cancellation.clone(),
		};
		let error = analyze_merge(
			InputRequest::from_playset_path(fixture.join("dlc_load.json"), Config::default()),
			MergeAnalysisOptions {
				out_dir: out_dir.clone(),
				include_game_base: false,
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
			&observer,
			&cancellation,
		)
		.err()
		.expect("semantic analysis should be cancelled");

		assert!(matches!(error, MergeError::Cancelled));
		assert!(!out_dir.exists());
	}

	#[test]
	fn analysis_reports_ordered_stages_counts_and_elapsed_time() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("tests/fixtures/playsets/eu4_minimal_passthrough");
		let observer = RecordingProgressObserver::default();
		analyze_merge(
			InputRequest::from_playset_path(fixture.join("dlc_load.json"), Config::default()),
			MergeAnalysisOptions {
				out_dir: temp.path().join("out"),
				include_game_base: false,
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
			&observer,
			&CancellationToken::new(),
		)
		.expect("analyze merge");

		let updates = observer.updates();
		let stage_boundaries = updates
			.iter()
			.filter(|progress| progress.completed || progress.completed_units == Some(0))
			.map(|progress| (progress.stage, progress.completed))
			.collect::<Vec<_>>();
		assert_eq!(
			stage_boundaries,
			vec![
				(MergeAnalysisStage::Inventory, false),
				(MergeAnalysisStage::Inventory, true),
				(MergeAnalysisStage::ResolveInput, false),
				(MergeAnalysisStage::ResolveInput, true),
				(MergeAnalysisStage::SemanticMerge, false),
				(MergeAnalysisStage::SemanticMerge, true),
				(MergeAnalysisStage::ValidateOutput, false),
				(MergeAnalysisStage::ValidateOutput, true),
				(MergeAnalysisStage::FreezeArtifacts, false),
				(MergeAnalysisStage::FreezeArtifacts, true),
			],
		);
		assert!(
			updates
				.windows(2)
				.all(|pair| pair[0].elapsed <= pair[1].elapsed)
		);
		for progress in updates.iter().filter(|progress| progress.completed) {
			assert_eq!(progress.completed_units, progress.total_units);
		}
	}

	#[test]
	fn compute_merge_status_partial_on_manual_conflict() {
		let report = report_with(|report| {
			report.manual_conflict_count = 1;
			report.generated_file_count = 7;
		});

		assert_eq!(
			compute_merge_status(&report),
			MergeStatusView {
				status: MergeReportStatus::PartialSuccess,
				manual_conflict_count: 1,
				unsupported_input_count: 0,
				engine_failure_count: 0,
				handler_resolution_count: 0,
				generated_file_count: 7,
			}
		);
	}

	#[test]
	fn compute_merge_status_partial_on_handler_resolutions() {
		let report = report_with(|report| {
			report.handler_resolutions.push(HandlerResolutionRecord {
				path: "common/test.txt".to_string(),
				action: "last_writer".to_string(),
				source: None,
				rationale: None,
			});
		});

		assert_eq!(
			compute_merge_status(&report),
			MergeStatusView {
				status: MergeReportStatus::PartialSuccess,
				manual_conflict_count: 0,
				unsupported_input_count: 0,
				engine_failure_count: 0,
				handler_resolution_count: 1,
				generated_file_count: 0,
			}
		);
	}

	#[test]
	fn compute_merge_status_ready_on_clean_merge() {
		let report = MergeReport::default();

		assert_eq!(
			compute_merge_status(&report),
			MergeStatusView {
				status: MergeReportStatus::Ready,
				manual_conflict_count: 0,
				unsupported_input_count: 0,
				engine_failure_count: 0,
				handler_resolution_count: 0,
				generated_file_count: 0,
			}
		);
	}

	#[test]
	fn compute_merge_status_partial_on_non_conflict_deferred_units() {
		let unsupported = report_with(|report| report.unsupported_input_count = 1);
		let engine_failure = report_with(|report| report.engine_failure_count = 1);

		assert_eq!(
			compute_merge_status(&unsupported).status,
			MergeReportStatus::PartialSuccess
		);
		assert_eq!(
			compute_merge_status(&engine_failure).status,
			MergeReportStatus::PartialSuccess
		);
	}

	#[test]
	fn analysis_status_uses_precommit_lifecycle_terms() {
		let ready = merge_execution_result(MergeReport::default());
		assert_eq!(
			merge_analysis_status(&ready),
			MergeAnalysisStatus::ReadyToCommit
		);

		let deferred = merge_execution_result(report_with(|report| {
			report.unsupported_input_count = 1;
		}));
		assert_eq!(
			merge_analysis_status(&deferred),
			MergeAnalysisStatus::CommittableWithDeferrals
		);

		let blocked = merge_execution_result(report_with(|report| {
			report.status = MergeReportStatus::Blocked;
		}));
		assert_eq!(
			merge_analysis_status(&blocked),
			MergeAnalysisStatus::Blocked
		);
	}

	#[test]
	fn compute_analysis_status_fatal_on_fatal_errors() {
		let report = report_with(|report| {
			report.validation.fatal_errors = 2;
			report.validation.strict_findings = 3;
			report.validation.advisory_findings = 4;
			report.validation.parse_errors = 5;
			report.validation.unresolved_references = 6;
			report.validation.missing_localisation = 7;
		});

		assert_eq!(
			compute_analysis_status(&report),
			AnalysisStatusView {
				fatal_errors: 2,
				strict_findings: 3,
				advisory_findings: 4,
				parse_errors: 5,
				unresolved_references: 6,
				missing_localisation: 7,
			}
		);
	}

	#[test]
	fn compute_analysis_status_clean_when_no_findings() {
		let report = MergeReport::default();

		assert_eq!(
			compute_analysis_status(&report),
			AnalysisStatusView::default()
		);
	}

	#[test]
	fn merge_status_ignores_analysis_buckets() {
		let report = report_with(|report| {
			report.validation.strict_findings = 5;
		});

		assert_eq!(
			compute_merge_status(&report).status,
			MergeReportStatus::Ready
		);
	}

	#[test]
	fn analysis_status_ignores_merge_state() {
		let report = report_with(|report| {
			report.manual_conflict_count = 3;
			report.status = MergeReportStatus::Blocked;
		});

		assert_eq!(compute_analysis_status(&report).fatal_errors, 0);
	}

	#[test]
	fn full_merge_revalidation_reuses_initial_base_snapshot_lease() {
		let _guard = BASE_DATA_ENV_LOCK.lock().expect("base data env lock");
		let temp = tempfile::TempDir::new().expect("temp dir");
		unsafe {
			std::env::set_var(BASE_DATA_DIR_ENV, temp.path().join("base-data"));
		}

		let game = Eu4;
		let game_version = "1.37.5";
		let game_root = temp.path().join("eu4-game");
		fs::create_dir_all(game_root.join("common/scripted_triggers"))
			.expect("create base content root");
		fs::write(game_root.join("version.txt"), format!("{game_version}\n"))
			.expect("write game version");
		fs::write(
			game_root.join("common/scripted_triggers/base.txt"),
			"base_trigger = { always = yes }\n",
		)
		.expect("write base script");
		let filter = FileFilter::new(game, &[]).expect("build file filter");
		let built = build_base_snapshot(&game, &game_root, Some(game_version), &filter)
			.expect("build base snapshot");
		install_built_snapshot(
			&built.encoded_snapshot,
			BaseDataSource::Build,
			Some(built.snapshot_asset_name),
			Some(built.snapshot_sha256),
		)
		.expect("install base snapshot");

		let paradox_dir = temp.path().join("Europa Universalis IV");
		let mod_root = temp.path().join("mod-source");
		fs::create_dir_all(paradox_dir.join("mod")).expect("create playset mod dir");
		fs::create_dir_all(mod_root.join("common/scripted_triggers"))
			.expect("create mod content root");
		fs::write(
			mod_root.join("common/scripted_triggers/mod.txt"),
			"mod_trigger = { always = yes }\n",
		)
		.expect("write mod script");
		fs::write(
			paradox_dir.join("mod/ugc_100.mod"),
			format!(
				"name=\"Test Mod\"\npath=\"{}\"\nremote_file_id=\"100\"\n",
				escape_descriptor_value(&normalize_descriptor_path(&mod_root))
			),
		)
		.expect("write mod descriptor");
		fs::write(
			paradox_dir.join("dlc_load.json"),
			serde_json::to_vec_pretty(&serde_json::json!({
				"enabled_mods": ["mod/ugc_100.mod"],
				"disabled_dlcs": [],
			}))
			.expect("serialize playset"),
		)
		.expect("write playset");

		clear_cached_loaded_base_snapshot(temp.path());
		reset_installed_snapshot_test_counters();
		let mut game_path = HashMap::new();
		game_path.insert("eu4".to_string(), game_root);
		let request = InputRequest::from_playset_path(
			paradox_dir.join("dlc_load.json"),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		);
		let result = run_merge_with_options(
			request,
			MergeAnalysisOptions {
				out_dir: temp.path().join("out"),
				include_game_base: true,
				include_base: false,
				gui_scroll_merge: false,
				force: false,
				ignore_replace_path: false,
				dep_overrides: Vec::new(),
				resolution_config_path: None,
				interactive_conflict_handler: Some(Box::new(
					crate::merge::conflict_handler::DeferHandler,
				)),
				interactive_resolution_config_path: None,
				playset_fingerprint: None,
				provenance: false,
				retained_paths: None,
			},
		)
		.expect("run full merge with revalidation");

		assert_eq!(result.report.status, MergeReportStatus::Ready);
		assert!(result.report.input.is_none());
		let persisted = serde_json::from_slice::<MergeReport>(
			&fs::read(temp.path().join("out").join(MERGE_REPORT_ARTIFACT_PATH))
				.expect("read merge report"),
		)
		.expect("decode merge report");
		assert!(persisted.input.is_none());
		assert_eq!(installed_snapshot_file_read_count(), 1);
		assert_eq!(installed_snapshot_cold_decode_count(), 1);
		assert_eq!(installed_snapshot_current_validation_count(), 1);
		#[cfg(unix)]
		assert_eq!(installed_snapshot_current_digest_count(), 0);
		#[cfg(not(unix))]
		assert_eq!(installed_snapshot_current_digest_count(), 1);

		unsafe {
			std::env::remove_var(BASE_DATA_DIR_ENV);
		}
	}
}
