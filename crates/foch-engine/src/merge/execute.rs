use super::backend::backend_for;
use super::conflict_handler::ConflictHandler;
use super::error::MergeError;
use super::materialize::{
	MaterializeOutput, MergeMaterializeOptions, OutputTransaction,
	materialize_prepared_merge_with_workspace_result, prepare_merge_plan,
};
use super::output::artifact_tree::AnalyzedArtifactTree;
use crate::base_data::{
	InstalledBaseSnapshotIdentity, InstalledBaseSnapshotPublicationGuard,
	lock_and_validate_installed_base_snapshot_identity,
};
use crate::cache::{
	ModsetCache, compute_modset_cache_key, compute_resolution_map_hash, unpack_modset_tarball,
};
use crate::emit::EmitOptions;

// SemVer identity for cached merge output. Bump patch for output bug fixes,
// minor for additive semantics, and major for incompatible cache payloads.
const MODSET_CACHE_VERSION: &str = "14.3.0";
static VALIDATION_PLAYSET_COUNTER: AtomicU64 = AtomicU64::new(0);
use crate::request::{CheckRequest, RunOptions};
use crate::run_checks_with_options;
use crate::workspace::{
	WorkspaceInventory, build_workspace_inventory_for_paths, resolve_product_input_manifest,
	resolve_workspace_from_inventory,
};
use foch::model::{
	AnalysisMode, ChannelMode, Finding, MERGE_EXECUTION_ATTESTATION_SCHEMA,
	MERGE_PLAN_ARTIFACT_PATH, MERGE_PROVENANCE_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH,
	MERGE_TRACE_ARTIFACT_PATH, MergeExecutionAttestation, MergePlanResult, MergePlanTarget,
	MergeReport, MergeReportBaseSnapshot, MergeReportScope, MergeReportStatus,
	MergeReportValidation, ProductInputAttestation, ProductInputManifest,
};
use foch::project::{AppliedDepOverride, Project, ResolutionDecision, ResolutionMap};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

use super::kernel::MergeBackendId;

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

#[derive(Clone, Debug)]
pub struct CommitResult {
	pub report: MergeReport,
	pub merge_status: MergeStatusView,
	pub analysis_status: AnalysisStatusView,
	pub exit_code: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitAuthorization {
	EmptyTargetOnly,
	ReplaceExisting(ReplacementTarget),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacementTarget {
	path: PathBuf,
	fingerprint: blake3::Hash,
	file_count: usize,
	total_bytes: u64,
}

impl ReplacementTarget {
	pub fn path(&self) -> &Path {
		&self.path
	}

	pub fn file_count(&self) -> usize {
		self.file_count
	}

	pub fn total_bytes(&self) -> u64 {
		self.total_bytes
	}

	pub fn fingerprint(&self) -> String {
		self.fingerprint.to_hex().to_string()
	}
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
	plan: MergePlanResult,
	report: MergeReport,
	merge_status: MergeStatusView,
	analysis_status: AnalysisStatusView,
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
	request: CheckRequest,
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
	request: CheckRequest,
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
	request: CheckRequest,
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
	out_dir: PathBuf,
	analysis: MergeAnalysis,
	artifacts: AnalyzedArtifactTree,
	base_snapshot_publish_guard: Option<BaseSnapshotPublishGuard>,
	product_input_publish_guard: Option<ProductInputPublishGuard>,
	prior_output_guard: Option<PriorOutputGuard>,
}

struct PendingAnalysis {
	request: CheckRequest,
	options: MergeAnalysisOptions,
	backend_id: MergeBackendId,
	workspace_result:
		Result<crate::workspace::ResolvedWorkspace, crate::workspace::WorkspaceResolveError>,
	plan: MergePlanResult,
	resolution_map: ResolutionMap,
	emit_options: EmitOptions,
	frozen_external_files: BTreeMap<PathBuf, Vec<u8>>,
	base_snapshot_publish_guard: Option<BaseSnapshotPublishGuard>,
	product_input_publish_guard: Option<ProductInputPublishGuard>,
	execution_attestation: MergeExecutionAttestation,
	modset_cache_key: Option<String>,
	modset_cache_bypass: Option<&'static str>,
}

impl AnalyzedMerge {
	pub fn analysis(&self) -> &MergeAnalysis {
		&self.analysis
	}

	/// Capture the exact non-empty target the caller is being asked to replace.
	pub fn replacement_target(&self) -> Result<Option<ReplacementTarget>, MergeError> {
		fingerprint_replacement_target(&self.out_dir)
	}

	pub fn commit(self, authorization: CommitAuthorization) -> Result<CommitResult, MergeError> {
		let transaction = OutputTransaction::begin(&self.out_dir)?;
		let expected_replacement = match (transaction.prior_dir(), authorization) {
			(None, CommitAuthorization::EmptyTargetOnly) => None,
			(None, CommitAuthorization::ReplaceExisting(_)) => {
				return Err(MergeError::ReplacementTargetChanged { path: self.out_dir });
			}
			(Some(_), CommitAuthorization::EmptyTargetOnly) => {
				return Err(MergeError::ReplacementAuthorizationRequired { path: self.out_dir });
			}
			(Some(_), CommitAuthorization::ReplaceExisting(expected)) => {
				validate_replacement_target(&self.out_dir, &expected)?;
				Some(expected)
			}
		};
		if let Some(guard) = self.prior_output_guard.as_ref() {
			guard.validate()?;
		}
		self.artifacts.copy_into(transaction.staging_dir())?;
		let _base_snapshot_commit_guard = validate_publish_guards(
			self.base_snapshot_publish_guard.as_ref(),
			self.product_input_publish_guard.as_ref(),
		)?;
		if let Some(expected) = expected_replacement.as_ref() {
			validate_replacement_target(&self.out_dir, expected)?;
		}
		transaction.commit()?;
		let exit_code = commit_exit_code(&self.analysis);
		Ok(CommitResult {
			report: self.analysis.report,
			merge_status: self.analysis.merge_status,
			analysis_status: self.analysis.analysis_status,
			exit_code,
		})
	}
}

fn analyze_merge_with_backend_and_observer(
	request: CheckRequest,
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
	let mut inventory_result = build_workspace_inventory_for_paths(
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
			"[merge] build_workspace_inventory: done elapsed_ms={} mods={} versioned_mods={}",
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
	let base_snapshot_publish_guard = match inventory_result.as_ref() {
		Ok(inventory) => BaseSnapshotPublishGuard::from_inventory(inventory)?,
		Err(_) => None,
	};
	let product_input_publish_guard = inventory_result.as_ref().ok().and_then(|inventory| {
		ProductInputPublishGuard::from_inventory(
			request.clone(),
			options.retained_paths.clone(),
			inventory,
		)
	});
	let execution_attestation = merge_execution_attestation(
		backend_id,
		options.retained_paths.is_some(),
		options.include_game_base,
		base_snapshot_publish_guard.as_ref(),
	);
	let LoadedMergePolicy {
		resolution_map,
		emit_options,
		frozen_external_files,
		policy_hash: merge_policy_hash,
	} = load_merge_policy(&request, options.resolution_config_path.as_deref())?;
	let has_interactive_conflict_handler = options.interactive_conflict_handler.is_some();
	let depends_on_prior_output = resolution_map_depends_on_prior_output(&resolution_map);
	// Full product output must always be materialized from its source roots. The
	// retained-path evaluation cache is intentionally small and is bound to the
	// frozen policy and analysis metadata reviewed with the result.
	let (modset_cache_key, modset_cache_bypass) = if options.retained_paths.is_none() {
		(None, Some("full_product_output"))
	} else if !modset_cache_is_eligible(has_interactive_conflict_handler, depends_on_prior_output) {
		let reason = if has_interactive_conflict_handler {
			"interactive_conflict_handler"
		} else {
			"keep_existing_resolution"
		};
		(None, Some(reason))
	} else {
		let key = inventory_result.as_ref().ok().and_then(|inventory| {
			build_modset_cache_key(inventory, &options, backend_id, &merge_policy_hash)
		});
		if key.is_some() {
			(key, None)
		} else {
			(None, Some("incomplete_input_identity"))
		}
	};
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
	let mut workspace_result = inventory_result.and_then(resolve_workspace_from_inventory);
	match workspace_result.as_ref() {
		Ok(workspace) => {
			let cache_hits = workspace
				.mod_snapshots
				.iter()
				.flatten()
				.filter(|snapshot| snapshot.cache_hit)
				.count();
			let cache_misses = workspace
				.mod_snapshots
				.iter()
				.flatten()
				.filter(|snapshot| !snapshot.cache_hit)
				.count();
			eprintln!(
				"[merge] resolve_workspace: done elapsed_ms={} mods={} files={} requested_paths={} effective_paths={} mod_parse_cache_hits={} mod_parse_cache_misses={}",
				resolve_started.elapsed().as_millis(),
				workspace.mods.len(),
				workspace.file_inventory.len(),
				workspace
					.requested_retained_paths
					.as_ref()
					.map_or(0, BTreeSet::len),
				workspace
					.effective_retained_paths
					.as_ref()
					.map_or(0, BTreeSet::len),
				cache_hits,
				cache_misses
			);
		}
		Err(err) => eprintln!(
			"[merge] resolve_workspace: error elapsed_ms={} kind={:?} message={}",
			resolve_started.elapsed().as_millis(),
			err.kind,
			err.message
		),
	}
	let plan = prepare_merge_plan(
		&mut workspace_result,
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
			workspace_result,
			plan,
			resolution_map,
			emit_options,
			frozen_external_files,
			base_snapshot_publish_guard,
			product_input_publish_guard,
			execution_attestation,
			modset_cache_key,
			modset_cache_bypass,
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
		workspace_result,
		plan,
		resolution_map,
		emit_options,
		frozen_external_files,
		base_snapshot_publish_guard,
		product_input_publish_guard,
		execution_attestation,
		modset_cache_key,
		modset_cache_bypass,
	} = pending;

	let modset_cache = if let Some(key) = modset_cache_key {
		match ModsetCache::open_default_versioned(MODSET_CACHE_VERSION) {
			Ok(cache) => Some(ModsetCacheContext { cache, key }),
			Err(err) => {
				tracing::warn!(
					cache_version = MODSET_CACHE_VERSION,
					error = %err,
					"modset cache version cleanup failed; continuing without modset cache"
				);
				None
			}
		}
	} else {
		if let Some(reason) = modset_cache_bypass {
			eprintln!("[merge] modset_cache_bypass={reason}");
		}
		None
	};

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
	let mut cache_hit = false;
	let mut cached_report = None;
	if let Some(cache_context) = modset_cache.as_ref() {
		if let Some(cached) = cache_context.cache.lookup(&cache_context.key) {
			if modset_cache_entry_depends_on_prior_output(&cached.report) {
				eprintln!(
					"[merge] modset_cache_bypass=keep_existing key={}",
					short_key(&cache_context.key)
				);
			} else {
				eprintln!(
					"[merge] modset_cache_hits=1 modset_cache_misses=0 key={}",
					short_key(&cache_context.key)
				);
				unpack_modset_tarball(&cached.tarball_path, &staging_dir).map_err(|err| {
					MergeError::Io(io::Error::other(format!(
						"failed to unpack modset cache entry {} into {}: {err}",
						cached.tarball_path.display(),
						staging_dir.display()
					)))
				})?;
				rewrite_cached_generated_descriptor(
					&staging_dir,
					&final_out_dir,
					request.source_path(),
					&plan,
				)?;
				write_merge_plan_artifact(&staging_dir, &plan)?;
				let mut report = cached.report;
				report.cache_source = Some("modset".to_string());
				report.playset_fingerprint = options.playset_fingerprint.clone();
				report.execution = Some(execution_attestation.clone());
				report.input = product_input_publish_guard
					.as_ref()
					.map(|guard| guard.expected.clone());
				cached_report = Some(report);
				cache_hit = true;
			}
		}
		if !cache_hit {
			eprintln!(
				"[merge] modset_cache_hits=0 modset_cache_misses=1 key={}",
				short_key(&cache_context.key)
			);
		}
	}

	let mut report = if let Some(report) = cached_report {
		report
	} else {
		let effective_retained_paths = workspace_result
			.as_ref()
			.ok()
			.and_then(|workspace| workspace.effective_retained_paths.clone());
		materialize_prepared_merge_with_workspace_result(
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
			workspace_result,
			plan.clone(),
			Some((progress, analysis_started)),
		)?
	};
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
	report.input = product_input_publish_guard
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
			base_snapshot_publish_guard
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
	if !cache_hit && execution.report.status != MergeReportStatus::Fatal {
		let _cache_input_guard = if modset_cache.is_some() {
			validate_publish_guards(
				base_snapshot_publish_guard.as_ref(),
				product_input_publish_guard.as_ref(),
			)?
		} else {
			None
		};
		store_modset_cache_entry(modset_cache.as_ref(), &staging_dir, &execution.report);
	}
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
		base_snapshot_publish_guard,
		product_input_publish_guard,
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
	base_snapshot: Option<&BaseSnapshotPublishGuard>,
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

#[derive(Clone, Debug)]
struct ModsetCacheContext {
	cache: ModsetCache,
	key: String,
}

#[derive(Clone, Debug)]
struct BaseSnapshotPublishGuard {
	game_key: String,
	game_version: String,
	playlist_path: PathBuf,
	identity: InstalledBaseSnapshotIdentity,
}

#[derive(Clone, Debug)]
struct ProductInputPublishGuard {
	request: CheckRequest,
	retained_paths: Option<BTreeSet<String>>,
	expected: ProductInputAttestation,
}

#[derive(Clone, Debug)]
struct PriorOutputGuard {
	root: PathBuf,
	files: BTreeMap<PathBuf, blake3::Hash>,
}

impl PriorOutputGuard {
	fn from_report(root: &Path, report: &MergeReport) -> Result<Option<Self>, MergeError> {
		let mut files = BTreeMap::new();
		for resolution in &report.handler_resolutions {
			if !resolution.action.eq_ignore_ascii_case("kept_existing") {
				continue;
			}
			let relative = safe_output_relative_path(Path::new(&resolution.path))?;
			let bytes = fs::read(root.join(&relative))?;
			files.insert(relative, blake3::hash(&bytes));
		}
		if files.is_empty() {
			Ok(None)
		} else {
			Ok(Some(Self {
				root: root.to_path_buf(),
				files,
			}))
		}
	}

	fn validate(&self) -> Result<(), MergeError> {
		for (relative, expected) in &self.files {
			let path = self.root.join(relative);
			let unchanged = fs::read(&path)
				.map(|bytes| blake3::hash(&bytes) == *expected)
				.unwrap_or(false);
			if !unchanged {
				return Err(MergeError::AnalyzedOutputChanged { path });
			}
		}
		Ok(())
	}
}

fn safe_output_relative_path(path: &Path) -> Result<PathBuf, MergeError> {
	if path.as_os_str().is_empty()
		|| path
			.components()
			.any(|component| !matches!(component, std::path::Component::Normal(_)))
	{
		return Err(MergeError::Validation {
			path: Some(path.display().to_string()),
			message: "output path is not a safe relative path".to_string(),
		});
	}
	Ok(path.to_path_buf())
}

fn fingerprint_replacement_target(root: &Path) -> Result<Option<ReplacementTarget>, MergeError> {
	match fs::symlink_metadata(root) {
		Ok(metadata) if metadata.file_type().is_dir() => {}
		Ok(_) => {
			return Err(MergeError::Validation {
				path: Some(root.display().to_string()),
				message: "merge output target is not a directory".to_string(),
			});
		}
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
		Err(error) => return Err(MergeError::Io(error)),
	}

	let mut entries = WalkDir::new(root)
		.min_depth(1)
		.follow_links(false)
		.into_iter()
		.collect::<Result<Vec<_>, _>>()
		.map_err(|error| {
			let message = error.to_string();
			match error.into_io_error() {
				Some(error) => MergeError::Io(io::Error::new(error.kind(), message)),
				None => MergeError::Validation {
					path: Some(root.display().to_string()),
					message,
				},
			}
		})?;
	entries.sort_by(|left, right| left.path().cmp(right.path()));
	if entries.is_empty() {
		return Ok(None);
	}

	let mut hasher = blake3::Hasher::new();
	let mut file_count = 0usize;
	let mut total_bytes = 0u64;
	for entry in entries {
		let relative = entry
			.path()
			.strip_prefix(root)
			.map_err(|_| MergeError::Validation {
				path: Some(entry.path().display().to_string()),
				message: "replacement target entry escaped its root".to_string(),
			})?;
		let relative = safe_output_relative_path(relative)?;
		let relative = relative.to_str().ok_or_else(|| MergeError::Validation {
			path: Some(relative.display().to_string()),
			message: "replacement target path is not valid UTF-8".to_string(),
		})?;
		if entry.file_type().is_dir() {
			hash_replacement_part(&mut hasher, b"directory");
			hash_replacement_part(&mut hasher, relative.as_bytes());
		} else if entry.file_type().is_file() {
			let bytes = fs::read(entry.path())?;
			hash_replacement_part(&mut hasher, b"file");
			hash_replacement_part(&mut hasher, relative.as_bytes());
			hash_replacement_part(&mut hasher, &bytes);
			file_count = file_count.saturating_add(1);
			total_bytes = total_bytes.saturating_add(bytes.len() as u64);
		} else {
			return Err(MergeError::Validation {
				path: Some(entry.path().display().to_string()),
				message: "replacement target contains a symlink or special file".to_string(),
			});
		}
	}

	Ok(Some(ReplacementTarget {
		path: root.to_path_buf(),
		fingerprint: hasher.finalize(),
		file_count,
		total_bytes,
	}))
}

fn validate_replacement_target(
	root: &Path,
	expected: &ReplacementTarget,
) -> Result<(), MergeError> {
	let observed = fingerprint_replacement_target(root)?;
	if expected.path != root || observed.as_ref() != Some(expected) {
		return Err(MergeError::ReplacementTargetChanged {
			path: root.to_path_buf(),
		});
	}
	Ok(())
}

fn hash_replacement_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
	hasher.update(&(bytes.len() as u64).to_le_bytes());
	hasher.update(bytes);
}

impl ProductInputPublishGuard {
	fn from_inventory(
		request: CheckRequest,
		retained_paths: Option<BTreeSet<String>>,
		inventory: &WorkspaceInventory,
	) -> Option<Self> {
		Some(Self {
			request,
			retained_paths,
			expected: inventory.product_input_manifest.as_ref()?.attestation(),
		})
	}

	fn validate(&self) -> Result<(), MergeError> {
		let observed = resolve_product_input_manifest(&self.request, self.retained_paths.as_ref())
			.map_err(|err| MergeError::WorkspaceResolve {
				path: err.path,
				message: format!(
					"failed to revalidate product inputs before publication: {}",
					err.message
				),
			})?
			.attestation();
		if observed != self.expected {
			return Err(MergeError::WorkspaceResolve {
				path: self.request.source_path().to_path_buf(),
				message: format!(
					"product inputs changed during merge: expected {}, observed {}",
					self.expected.digest, observed.digest
				),
			});
		}
		Ok(())
	}
}

impl BaseSnapshotPublishGuard {
	fn from_inventory(inventory: &WorkspaceInventory) -> Result<Option<Self>, MergeError> {
		let Some(identity) = inventory.base_snapshot_identity.as_ref() else {
			return Ok(None);
		};
		let Some(game_version) = inventory.mod_cache_game_version.as_ref() else {
			return Err(MergeError::WorkspaceResolve {
				path: inventory.playlist_path.clone(),
				message: "base snapshot identity is missing its game version".to_string(),
			});
		};
		Ok(Some(Self {
			game_key: inventory.playlist.game.key().to_string(),
			game_version: game_version.clone(),
			playlist_path: inventory.playlist_path.clone(),
			identity: identity.clone(),
		}))
	}
}

#[cfg(test)]
fn finalize_merge_output<Guard>(
	transaction: OutputTransaction,
	execution: CommitResult,
	cache_context: Option<&ModsetCacheContext>,
	store_cache: bool,
	validate_base_snapshot: impl FnOnce(&Path) -> Result<Guard, MergeError>,
) -> Result<CommitResult, MergeError> {
	finalize_merge_output_with_publish(
		transaction,
		execution,
		cache_context,
		store_cache,
		validate_base_snapshot,
		OutputTransaction::commit,
	)
}

#[cfg(test)]
fn finalize_merge_output_with_publish<Guard>(
	transaction: OutputTransaction,
	execution: CommitResult,
	cache_context: Option<&ModsetCacheContext>,
	store_cache: bool,
	validate_base_snapshot: impl FnOnce(&Path) -> Result<Guard, MergeError>,
	commit: impl FnOnce(OutputTransaction) -> Result<(), MergeError>,
) -> Result<CommitResult, MergeError> {
	write_merge_report_artifact(transaction.staging_dir(), &execution.report)?;
	// Validate every mutable input before the staging tree becomes reusable or
	// visible. A failed guard must never poison the modset cache under an old key.
	let _base_snapshot_publication_guard = validate_base_snapshot(transaction.staging_dir())?;
	if store_cache {
		store_modset_cache_entry(cache_context, transaction.staging_dir(), &execution.report);
	}
	commit(transaction)?;
	Ok(execution)
}

fn validate_base_snapshot_publish_guard(
	guard: Option<&BaseSnapshotPublishGuard>,
) -> Result<Option<InstalledBaseSnapshotPublicationGuard>, MergeError> {
	let Some(guard) = guard else {
		return Ok(None);
	};
	lock_and_validate_installed_base_snapshot_identity(
		&guard.game_key,
		&guard.game_version,
		&guard.identity,
	)
	.map(Some)
	.map_err(|message| MergeError::WorkspaceResolve {
		path: guard.playlist_path.clone(),
		message,
	})
}

fn validate_publish_guards(
	base_snapshot_guard: Option<&BaseSnapshotPublishGuard>,
	product_input_guard: Option<&ProductInputPublishGuard>,
) -> Result<Option<InstalledBaseSnapshotPublicationGuard>, MergeError> {
	if let Some(product_input_guard) = product_input_guard {
		product_input_guard.validate()?;
	}
	validate_base_snapshot_publish_guard(base_snapshot_guard)
}

fn build_modset_cache_key(
	inventory: &WorkspaceInventory,
	options: &MergeAnalysisOptions,
	backend_id: MergeBackendId,
	merge_policy_hash: &str,
) -> Option<String> {
	let game_version = inventory
		.cache_game_version
		.clone()
		.unwrap_or_else(|| format!("{} unknown", inventory.playlist.game.key()));
	let mut mod_hashes = Vec::new();
	for (_candidate, hash) in inventory
		.mods
		.iter()
		.zip(inventory.mod_hashes.iter())
		.filter(|(candidate, _hash)| candidate.entry.enabled)
	{
		mod_hashes.push(hash.clone()?);
	}
	let retained_paths_label = retained_paths_cache_label(
		inventory.effective_retained_paths.as_ref(),
		&inventory.retained_module_policy_versions,
	);
	let dep_overrides_label = dep_overrides_cache_label(&options.dep_overrides);
	let product_digest = modset_cache_product_digest(inventory, options.provenance)?;
	let foch_version = modset_cache_version_label(ModsetCacheBehavior {
		include_base: options.include_base,
		gui_scroll_merge: options.gui_scroll_merge,
		force: options.force,
		ignore_replace_path: options.ignore_replace_path,
		provenance: options.provenance,
		dep_overrides: &dep_overrides_label,
		retained_paths: &retained_paths_label,
		merge_backend: backend_id.as_str(),
		product_digest: &product_digest,
	});
	Some(compute_modset_cache_key(
		&mod_hashes,
		merge_policy_hash,
		&foch_version,
		&game_version,
	))
}

#[derive(Clone, Copy, Debug)]
struct ModsetCacheBehavior<'a> {
	include_base: bool,
	gui_scroll_merge: bool,
	force: bool,
	ignore_replace_path: bool,
	provenance: bool,
	dep_overrides: &'a str,
	retained_paths: &'a str,
	merge_backend: &'a str,
	product_digest: &'a str,
}

fn modset_cache_version_label(behavior: ModsetCacheBehavior<'_>) -> String {
	format!(
		"{} modset_cache={MODSET_CACHE_VERSION} merge_backend={} include_base={} provenance={} gui_scroll_merge={} force={} ignore_replace_path={} dep_overrides={} retained_paths={} product={}",
		env!("CARGO_PKG_VERSION"),
		behavior.merge_backend,
		behavior.include_base,
		behavior.provenance,
		behavior.gui_scroll_merge,
		behavior.force,
		behavior.ignore_replace_path,
		behavior.dep_overrides,
		behavior.retained_paths,
		behavior.product_digest,
	)
}

fn modset_cache_product_digest(inventory: &WorkspaceInventory, provenance: bool) -> Option<String> {
	let manifest = inventory.product_input_manifest.as_ref()?;
	if !provenance {
		return Some(manifest.digest.clone());
	}

	let display_names = manifest
		.mods
		.iter()
		.map(|product_mod| {
			let display_name = inventory
				.mods
				.iter()
				.find(|candidate| candidate.mod_id == product_mod.mod_id)
				.and_then(|candidate| {
					candidate
						.descriptor
						.as_ref()
						.map(|descriptor| descriptor.name.trim())
						.filter(|name| !name.is_empty())
						.map(str::to_string)
						.or_else(|| {
							candidate
								.entry
								.display_name
								.as_deref()
								.map(str::trim)
								.filter(|name| !name.is_empty())
								.map(str::to_string)
						})
				})
				.unwrap_or_else(|| product_mod.mod_id.clone());
			(product_mod.mod_id.clone(), display_name)
		})
		.collect::<Vec<_>>();
	Some(modset_cache_product_identity(
		manifest,
		Some(&display_names),
	))
}

fn modset_cache_product_identity(
	manifest: &ProductInputManifest,
	provenance_display_names: Option<&[(String, String)]>,
) -> String {
	let Some(display_names) = provenance_display_names else {
		return manifest.digest.clone();
	};

	let mut hasher = blake3::Hasher::new();
	for value in [&manifest.digest, "provenance-display-names-v1"] {
		let bytes = value.as_bytes();
		hasher.update(&(bytes.len() as u64).to_le_bytes());
		hasher.update(bytes);
	}
	hasher.update(&(display_names.len() as u64).to_le_bytes());
	for (mod_id, display_name) in display_names {
		for value in [mod_id, display_name] {
			let bytes = value.as_bytes();
			hasher.update(&(bytes.len() as u64).to_le_bytes());
			hasher.update(bytes);
		}
	}
	hasher.finalize().to_hex().to_string()
}

fn rewrite_cached_generated_descriptor(
	staging_dir: &Path,
	published_out_dir: &Path,
	playset_path: &Path,
	plan: &MergePlanResult,
) -> Result<(), MergeError> {
	let mut replace_prefixes = BTreeSet::new();
	for entry in &plan.paths {
		if !matches!(&entry.target, MergePlanTarget::Module { .. }) {
			continue;
		}
		let staged_output = staging_dir.join(entry.output_path());
		match fs::metadata(&staged_output) {
			Ok(metadata) if metadata.is_file() => {
				if let Some(prefix) = entry.target.replace_prefix() {
					replace_prefixes.insert(prefix.to_string());
				}
			}
			Ok(_) => {}
			Err(err) if err.kind() == io::ErrorKind::NotFound => {}
			Err(err) => return Err(MergeError::Io(err)),
		}
	}

	let descriptor_root = if published_out_dir.is_absolute() {
		published_out_dir.to_path_buf()
	} else {
		std::env::current_dir()?.join(published_out_dir)
	};
	let normalized_out_dir = normalize_descriptor_path(&descriptor_root);
	let normalized_playset_path = normalize_descriptor_path(playset_path);
	let escaped_name = escape_descriptor_value(&format!("{} (Merged)", plan.playset_name));
	let escaped_path = escape_descriptor_value(&normalized_out_dir);
	let escaped_playset = escape_descriptor_value(&normalized_playset_path);
	let mut descriptor = format!(
		"# Source playset: {escaped_playset}\nname=\"{escaped_name}\"\npath=\"{escaped_path}\"\n"
	);
	for prefix in replace_prefixes {
		descriptor.push_str(&format!(
			"replace_path=\"{}\"\n",
			escape_descriptor_value(&prefix)
		));
	}
	fs::write(
		staging_dir.join(foch::model::MERGED_MOD_DESCRIPTOR_PATH),
		descriptor,
	)?;
	Ok(())
}

fn modset_cache_is_eligible(
	has_interactive_conflict_handler: bool,
	depends_on_prior_output: bool,
) -> bool {
	!has_interactive_conflict_handler && !depends_on_prior_output
}

fn resolution_map_depends_on_prior_output(resolution_map: &ResolutionMap) -> bool {
	resolution_map
		.by_file
		.values()
		.chain(resolution_map.by_conflict_id.values())
		.chain(
			resolution_map
				.pattern_rules
				.iter()
				.map(|rule| &rule.decision),
		)
		.any(resolution_decision_depends_on_prior_output)
}

fn resolution_decision_depends_on_prior_output(decision: &ResolutionDecision) -> bool {
	match decision {
		ResolutionDecision::KeepExisting => true,
		ResolutionDecision::Handler(name) => name.eq_ignore_ascii_case("keep_existing"),
		ResolutionDecision::PreferMod(_)
		| ResolutionDecision::PreferCandidate(_)
		| ResolutionDecision::UseFile(_)
		| ResolutionDecision::UseLiveFile(_) => false,
	}
}

fn modset_cache_entry_depends_on_prior_output(report: &MergeReport) -> bool {
	report
		.handler_resolutions
		.iter()
		.any(|record| record.action.eq_ignore_ascii_case("kept_existing"))
}

fn dep_overrides_cache_label(dep_overrides: &[AppliedDepOverride]) -> String {
	if dep_overrides.is_empty() {
		return "none".to_string();
	}

	let mut hasher = blake3::Hasher::new();
	hasher.update(&(dep_overrides.len() as u64).to_le_bytes());
	for dep_override in dep_overrides {
		for value in [
			dep_override.mod_id.as_str(),
			dep_override.dep_id.as_str(),
			&dep_override.source.to_string(),
		] {
			let bytes = value.as_bytes();
			hasher.update(&(bytes.len() as u64).to_le_bytes());
			hasher.update(bytes);
		}
	}
	format!("ordered:{}", hasher.finalize().to_hex())
}

fn retained_paths_cache_label(
	retained_paths: Option<&BTreeSet<String>>,
	module_policy_versions: &std::collections::BTreeMap<foch::model::MergeUnitId, u32>,
) -> String {
	let Some(retained_paths) = retained_paths else {
		return "full".to_string();
	};
	let mut hasher = blake3::Hasher::new();
	for path in retained_paths {
		let bytes = path.as_bytes();
		hasher.update(&(bytes.len() as u64).to_le_bytes());
		hasher.update(bytes);
	}
	for (module, version) in module_policy_versions {
		for value in [&module.family_id, &module.module_name] {
			let bytes = value.as_bytes();
			hasher.update(&(bytes.len() as u64).to_le_bytes());
			hasher.update(bytes);
		}
		hasher.update(&version.to_le_bytes());
	}
	format!("subset:{}", hasher.finalize().to_hex())
}

fn store_modset_cache_entry(
	cache_context: Option<&ModsetCacheContext>,
	out_dir: &Path,
	report: &MergeReport,
) {
	let Some(cache_context) = cache_context else {
		return;
	};
	if report.status == MergeReportStatus::Fatal {
		return;
	}
	if let Err(err) = cache_context
		.cache
		.store(&cache_context.key, out_dir, report)
	{
		eprintln!(
			"[merge] warning: failed to store modset cache entry {}: {err}",
			short_key(&cache_context.key)
		);
	}
}

fn short_key(key: &str) -> &str {
	key.get(..16).unwrap_or(key)
}

fn merge_execution_result(mut report: MergeReport) -> CommitResult {
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

fn commit_exit_code(analysis: &MergeAnalysis) -> i32 {
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
	policy_hash: String,
}

fn load_merge_policy(
	request: &CheckRequest,
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
	let serialized_policy = toml::to_string(&config).map_err(|err| MergeError::Validation {
		path: Some(explicit_path.unwrap_or(playset_root).display().to_string()),
		message: format!("failed to freeze merge policy: {err}"),
	})?;
	let policy_hash = frozen_merge_policy_hash(&serialized_policy, &frozen_external_files);
	Ok(LoadedMergePolicy {
		resolution_map,
		emit_options,
		frozen_external_files,
		policy_hash,
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

fn frozen_merge_policy_hash(
	serialized_policy: &str,
	frozen_external_files: &BTreeMap<PathBuf, Vec<u8>>,
) -> String {
	let mut hasher = blake3::Hasher::new();
	let config_hash = compute_resolution_map_hash(serialized_policy.as_bytes());
	hasher.update(config_hash.as_bytes());
	for (path, bytes) in frozen_external_files {
		let normalized_path = path.to_string_lossy().replace('\\', "/");
		hasher.update(&(normalized_path.len() as u64).to_le_bytes());
		hasher.update(normalized_path.as_bytes());
		hasher.update(&(bytes.len() as u64).to_le_bytes());
		hasher.update(bytes);
	}
	hasher.finalize().to_hex().to_string()
}

fn revalidate_generated_output(
	request: &CheckRequest,
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
	let mut validation_request = CheckRequest::new(
		crate::request::WorkspaceSource::DlcLoad(dlc_load_path.clone()),
		request.config.clone(),
	)
	.with_base_snapshot_lease(base_snapshot_lease);
	if let Some(expected) = request.expected_base_snapshot_identity.as_ref() {
		validation_request =
			validation_request.with_expected_base_snapshot_identity(expected.clone());
	}
	let result = run_checks_with_options(
		validation_request,
		RunOptions {
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

fn write_merge_report_artifact(out_dir: &Path, report: &MergeReport) -> Result<(), MergeError> {
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

fn write_merge_plan_artifact(out_dir: &Path, plan: &MergePlanResult) -> Result<(), MergeError> {
	let path = out_dir.join(MERGE_PLAN_ARTIFACT_PATH);
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let bytes = serde_json::to_vec_pretty(plan).map_err(|err| {
		MergeError::Io(io::Error::other(format!(
			"failed to serialize merge plan {}: {err}",
			path.display()
		)))
	})?;
	fs::write(path, bytes)?;
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
	use crate::base_data::{
		BASE_DATA_DIR_ENV, BASE_DATA_ENV_LOCK, BaseDataSource, build_base_snapshot,
		clear_cached_loaded_base_snapshot, install_built_snapshot,
		installed_base_snapshot_identity, installed_snapshot_cold_decode_count,
		installed_snapshot_current_digest_count, installed_snapshot_current_validation_count,
		installed_snapshot_file_read_count, lock_and_validate_installed_base_snapshot_identity,
		reset_installed_snapshot_test_counters,
	};
	use crate::config::Config;
	use crate::workspace::FileFilter;
	use foch::game::eu4::Eu4;
	use foch::model::{HandlerResolutionRecord, MergePlanEntry, ProductInputMod};
	use foch::playset::steam::{SteamId, WorkshopInstallIdentity};
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
		request: CheckRequest,
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
			CheckRequest::from_playset_path(
				fixture.join("dlc_load.json"),
				crate::Config {
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
			&fs::read(out_dir.join(foch::model::MERGE_PLAN_ARTIFACT_PATH))
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
			CheckRequest::from_playset_path(root.join("dlc_load.json"), Config::default()),
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
			CheckRequest::from_playset_path(
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
			CheckRequest::from_playset_path(fixture.join("dlc_load.json"), Config::default()),
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
			CheckRequest::from_playset_path(fixture.join("dlc_load.json"), Config::default()),
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
	fn commit_requires_separate_replacement_authorization() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("tests/fixtures/playsets/eu4_minimal_passthrough");
		let out_dir = temp.path().join("out");
		let analyzed = analyze_merge_for_test(
			CheckRequest::from_playset_path(fixture.join("dlc_load.json"), Config::default()),
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
		fs::create_dir_all(&out_dir).expect("create output");
		fs::write(out_dir.join("user-file.txt"), b"preserve me\n").expect("seed output");

		let error = analyzed
			.commit(CommitAuthorization::EmptyTargetOnly)
			.expect_err("replacement must require explicit authorization");

		assert!(matches!(
			error,
			MergeError::ReplacementAuthorizationRequired { .. }
		));
		assert_eq!(
			fs::read(out_dir.join("user-file.txt")).expect("read preserved output"),
			b"preserve me\n"
		);
	}

	#[test]
	fn commit_rejects_a_replacement_target_changed_after_confirmation() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("tests/fixtures/playsets/eu4_minimal_passthrough");
		let out_dir = temp.path().join("out");
		let analyzed = analyze_merge_for_test(
			CheckRequest::from_playset_path(fixture.join("dlc_load.json"), Config::default()),
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
		fs::create_dir_all(&out_dir).expect("create output");
		let user_file = out_dir.join("user-file.txt");
		fs::write(&user_file, b"confirmed bytes\n").expect("seed output");
		let replacement = analyzed
			.replacement_target()
			.expect("fingerprint output")
			.expect("non-empty output token");
		fs::write(&user_file, b"changed after confirmation\n").expect("mutate output");

		let error = analyzed
			.commit(CommitAuthorization::ReplaceExisting(replacement))
			.expect_err("changed output must invalidate replacement authorization");

		assert!(matches!(error, MergeError::ReplacementTargetChanged { .. }));
		assert_eq!(
			fs::read(&user_file).expect("read preserved changed output"),
			b"changed after confirmation\n"
		);
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
	fn retained_paths_cache_label_is_order_insensitive_and_subset_sensitive() {
		let no_module_policies = std::collections::BTreeMap::new();
		let left = BTreeSet::from([
			"common/scripted_effects/a.txt".to_string(),
			"interface/frontend.gui".to_string(),
		]);
		let right = BTreeSet::from([
			"interface/frontend.gui".to_string(),
			"common/scripted_effects/a.txt".to_string(),
		]);
		let different = BTreeSet::from(["common/scripted_effects/a.txt".to_string()]);

		assert_eq!(
			retained_paths_cache_label(Some(&left), &no_module_policies),
			retained_paths_cache_label(Some(&right), &no_module_policies)
		);
		assert_ne!(
			retained_paths_cache_label(Some(&left), &no_module_policies),
			retained_paths_cache_label(Some(&different), &no_module_policies)
		);
		assert_eq!(
			retained_paths_cache_label(None, &no_module_policies),
			"full"
		);
	}

	#[test]
	fn retained_paths_cache_label_includes_module_policy_version() {
		let retained = BTreeSet::from(["common/governments/00_governments.txt".to_string()]);
		let module = foch::model::MergeUnitId {
			family_id: "governments".to_string(),
			module_name: "governments".to_string(),
		};
		let version_one = std::collections::BTreeMap::from([(module.clone(), 1)]);
		let version_two = std::collections::BTreeMap::from([(module, 2)]);

		assert_ne!(
			retained_paths_cache_label(Some(&retained), &version_one),
			retained_paths_cache_label(Some(&retained), &version_two)
		);
	}

	#[test]
	fn modset_cache_key_separates_force_from_non_force_runs() {
		let key_for = |force| {
			let version = modset_cache_version_label(ModsetCacheBehavior {
				include_base: false,
				gui_scroll_merge: false,
				force,
				ignore_replace_path: false,
				provenance: false,
				dep_overrides: "none",
				retained_paths: "full",
				merge_backend: "address-patch",
				product_digest: "same-product",
			});
			compute_modset_cache_key(
				&["same-mod".to_string()],
				"same-resolution",
				&version,
				"same-game",
			)
		};

		assert_ne!(key_for(false), key_for(true));
	}

	#[test]
	fn modset_cache_key_separates_merge_backends() {
		let key_for = |merge_backend| {
			let version = modset_cache_version_label(ModsetCacheBehavior {
				include_base: false,
				gui_scroll_merge: false,
				force: false,
				ignore_replace_path: false,
				provenance: false,
				dep_overrides: "none",
				retained_paths: "full",
				merge_backend,
				product_digest: "same-product",
			});
			compute_modset_cache_key(
				&["same-mod".to_string()],
				"same-resolution",
				&version,
				"same-game",
			)
		};

		assert_ne!(key_for("address-patch"), key_for("gumtree-pcs-nway"));
	}

	#[test]
	fn modset_cache_key_separates_programmatic_dep_overrides() {
		let key_for = |dep_overrides: &[AppliedDepOverride]| {
			let dep_overrides = dep_overrides_cache_label(dep_overrides);
			let version = modset_cache_version_label(ModsetCacheBehavior {
				include_base: false,
				gui_scroll_merge: false,
				force: false,
				ignore_replace_path: false,
				provenance: false,
				dep_overrides: &dep_overrides,
				retained_paths: "full",
				merge_backend: "address-patch",
				product_digest: "same-product",
			});
			compute_modset_cache_key(
				&["same-mod".to_string()],
				"same-resolution",
				&version,
				"same-game",
			)
		};
		let override_edge = AppliedDepOverride::cli("child", "parent");

		assert_ne!(key_for(&[]), key_for(&[override_edge]));
	}

	#[test]
	fn modset_cache_key_binds_path_free_product_manifest() {
		let product_mod =
			|mod_id: &str, precedence: usize, workshop_id, manifest_id| ProductInputMod {
				mod_id: mod_id.to_string(),
				precedence,
				workshop_identity: WorkshopInstallIdentity {
					app_id: 236_850,
					workshop_id: SteamId::new(workshop_id),
					manifest_id: SteamId::new(manifest_id),
				},
			};
		let first = product_mod("mod-a", 1, 1_001, 2_001);
		let second = product_mod("mod-b", 2, 1_002, 2_002);
		let manifest = ProductInputManifest::new(vec![first.clone(), second.clone()]);
		let reordered = ProductInputManifest::new(vec![
			product_mod("mod-b", 1, 1_002, 2_002),
			product_mod("mod-a", 2, 1_001, 2_001),
		]);
		let revised =
			ProductInputManifest::new(vec![product_mod("mod-a", 1, 1_001, 2_003), second]);
		let key_for = |manifest: &ProductInputManifest,
		               source_path: &Path,
		               playset_name: &str,
		               out_dir: &Path| {
			let _publication_metadata = (source_path, playset_name, out_dir);
			let product_digest = modset_cache_product_identity(manifest, None);
			let version = modset_cache_version_label(ModsetCacheBehavior {
				include_base: false,
				gui_scroll_merge: false,
				force: false,
				ignore_replace_path: false,
				provenance: false,
				dep_overrides: "none",
				retained_paths: "subset:same",
				merge_backend: "gumtree-pcs-nway",
				product_digest: &product_digest,
			});
			compute_modset_cache_key(
				&["same-mod".to_string()],
				"same-policy",
				&version,
				"same-game",
			)
		};

		let first_key = key_for(
			&manifest,
			Path::new("/first/source/foch.toml"),
			"First playset",
			Path::new("/first/output"),
		);
		let moved_key = key_for(
			&manifest,
			Path::new("/moved/source/renamed.toml"),
			"Renamed playset",
			Path::new("/different/output"),
		);
		assert_eq!(first_key, moved_key);
		assert_ne!(
			first_key,
			key_for(
				&reordered,
				Path::new("/first/source/foch.toml"),
				"First playset",
				Path::new("/first/output"),
			)
		);
		assert_ne!(
			first_key,
			key_for(
				&revised,
				Path::new("/first/source/foch.toml"),
				"First playset",
				Path::new("/first/output"),
			)
		);
	}

	#[test]
	fn provenance_product_identity_binds_ordered_display_names() {
		let manifest = ProductInputManifest::new(vec![ProductInputMod {
			mod_id: "mod-a".to_string(),
			precedence: 1,
			workshop_identity: WorkshopInstallIdentity {
				app_id: 236_850,
				workshop_id: SteamId::new(1_001),
				manifest_id: SteamId::new(2_001),
			},
		}]);
		let original = vec![("mod-a".to_string(), "Original".to_string())];
		let renamed = vec![("mod-a".to_string(), "Renamed".to_string())];

		assert_eq!(
			modset_cache_product_identity(&manifest, None),
			manifest.digest
		);
		assert_ne!(
			modset_cache_product_identity(&manifest, Some(&original)),
			modset_cache_product_identity(&manifest, Some(&renamed))
		);
	}

	#[test]
	fn interactive_conflict_handler_bypasses_modset_cache() {
		assert!(modset_cache_is_eligible(false, false));
		assert!(!modset_cache_is_eligible(true, false));
		assert!(!modset_cache_is_eligible(false, true));
	}

	#[test]
	fn keep_existing_resolution_bypasses_modset_cache_before_lookup() {
		let mut direct = ResolutionMap::default();
		direct.by_file.insert(
			PathBuf::from("history/countries/TES - Test.txt"),
			ResolutionDecision::KeepExisting,
		);
		let mut handler = ResolutionMap::default();
		handler.by_conflict_id.insert(
			"conflict-id".to_string(),
			ResolutionDecision::Handler("KEEP_EXISTING".to_string()),
		);

		assert!(resolution_map_depends_on_prior_output(&direct));
		assert!(resolution_map_depends_on_prior_output(&handler));
		assert!(!resolution_map_depends_on_prior_output(
			&ResolutionMap::default()
		));
	}

	#[test]
	fn modset_cache_entry_with_keep_existing_depends_on_current_output() {
		let report = report_with(|report| {
			report.handler_resolutions.push(HandlerResolutionRecord {
				path: "history/countries/TES - Test.txt".to_string(),
				action: "kept_existing".to_string(),
				source: None,
				rationale: None,
			});
		});

		assert!(modset_cache_entry_depends_on_prior_output(&report));
		assert!(!modset_cache_entry_depends_on_prior_output(
			&MergeReport::default()
		));
	}

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
	#[test]
	fn output_transaction_reports_the_prior_tree_it_observed() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");

		let missing = OutputTransaction::begin(&out_dir).expect("begin missing transaction");
		assert_eq!(missing.prior_dir(), None);
		drop(missing);

		fs::create_dir(&out_dir).expect("create prior output");
		fs::write(out_dir.join("prior.txt"), "prior output\n").expect("write prior output");
		let existing = OutputTransaction::begin(&out_dir).expect("begin existing transaction");
		assert_eq!(existing.prior_dir(), Some(out_dir.as_path()));
	}

	#[test]
	fn output_transaction_treats_an_existing_empty_directory_as_missing() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		fs::create_dir(&out_dir).expect("create empty output");

		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		assert_eq!(transaction.prior_dir(), None);
		fs::write(transaction.staging_dir().join("new.txt"), "new output\n")
			.expect("write staged output");
		transaction.commit().expect("commit transaction");

		assert_eq!(
			fs::read_to_string(out_dir.join("new.txt")).expect("read published output"),
			"new output\n"
		);
	}

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
	#[test]
	fn output_transaction_replaces_the_complete_tree_without_overlay() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		fs::create_dir_all(out_dir.join("common/governments")).expect("create old output");
		fs::write(out_dir.join("descriptor.mod"), "old descriptor\n")
			.expect("write old descriptor");
		fs::write(
			out_dir.join("common/governments/stale.txt"),
			"stale government\n",
		)
		.expect("write stale module sibling");

		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		assert_eq!(transaction.staging_dir().parent(), out_dir.parent());
		fs::create_dir_all(transaction.staging_dir().join("common/governments"))
			.expect("create staged output");
		fs::write(
			transaction
				.staging_dir()
				.join("common/governments/current.txt"),
			"current government\n",
		)
		.expect("write staged module");
		transaction.commit().expect("commit transaction");

		assert_eq!(
			fs::read_to_string(out_dir.join("common/governments/current.txt"))
				.expect("read current module"),
			"current government\n"
		);
		assert!(!out_dir.join("common/governments/stale.txt").exists());
		assert!(!out_dir.join("descriptor.mod").exists());
	}

	#[test]
	fn output_transaction_error_preserves_the_old_complete_tree() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		fs::create_dir_all(out_dir.join("common/governments")).expect("create old output");
		fs::write(out_dir.join("descriptor.mod"), "old descriptor\n")
			.expect("write old descriptor");
		fs::write(
			out_dir.join("common/governments/complete.txt"),
			"old complete module\n",
		)
		.expect("write old module");

		let result = (|| -> Result<(), MergeError> {
			let transaction = OutputTransaction::begin(&out_dir)?;
			fs::create_dir_all(transaction.staging_dir().join("common/governments"))?;
			fs::write(
				transaction
					.staging_dir()
					.join("common/governments/partial.txt"),
				"partial module\n",
			)?;
			Err(MergeError::Io(io::Error::other("injected failure")))
		})();

		assert!(result.is_err());
		assert_eq!(
			fs::read_to_string(out_dir.join("descriptor.mod")).expect("read old descriptor"),
			"old descriptor\n"
		);
		assert_eq!(
			fs::read_to_string(out_dir.join("common/governments/complete.txt"))
				.expect("read old module"),
			"old complete module\n"
		);
		assert!(!out_dir.join("common/governments/partial.txt").exists());
	}

	#[test]
	fn output_transaction_rejects_an_existing_regular_file() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		fs::write(&out_dir, "do not replace\n").expect("write existing output file");

		let error = match OutputTransaction::begin(&out_dir) {
			Ok(_) => panic!("regular output file must be rejected"),
			Err(error) => error,
		};

		assert!(error.to_string().contains("must be a real directory"));
		assert_eq!(
			fs::read_to_string(&out_dir).expect("read preserved output file"),
			"do not replace\n"
		);
	}

	#[cfg(any(target_os = "windows", target_os = "redox"))]
	#[test]
	fn output_transaction_rejects_existing_directory_without_atomic_exchange() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		fs::create_dir(&out_dir).expect("create existing output");
		fs::write(out_dir.join("prior.txt"), "prior output\n").expect("write prior output");

		let error = match OutputTransaction::begin(&out_dir) {
			Ok(_) => panic!("existing output requires atomic directory exchange"),
			Err(error) => error,
		};

		assert!(
			error
				.to_string()
				.contains("atomic replacement of an existing output directory is unsupported")
		);
	}

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
	#[test]
	fn output_transaction_rejects_a_replaced_directory_before_publish() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		fs::create_dir(&out_dir).expect("create initial output");
		fs::write(out_dir.join("prior.txt"), "prior output\n").expect("write initial output");
		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		fs::write(transaction.staging_dir().join("new.txt"), "new output\n")
			.expect("write staged output");

		fs::remove_file(out_dir.join("prior.txt")).expect("remove initial output file");
		fs::remove_dir(&out_dir).expect("remove initial output");
		fs::create_dir(&out_dir).expect("create concurrent replacement");
		fs::write(out_dir.join("concurrent.txt"), "preserve me\n")
			.expect("write concurrent replacement");
		let error = transaction
			.commit()
			.expect_err("concurrent directory replacement must be rejected");

		assert!(
			error
				.to_string()
				.contains("changed while the replacement was staged")
		);
		assert_eq!(
			fs::read_to_string(out_dir.join("concurrent.txt"))
				.expect("read concurrent replacement"),
			"preserve me\n"
		);
		assert!(!out_dir.join("new.txt").exists());
	}

	#[test]
	fn output_transaction_drop_does_not_delete_a_replaced_staging_directory() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		let staging_dir = transaction.staging_dir().to_path_buf();

		fs::remove_dir(&staging_dir).expect("remove owned staging directory");
		fs::create_dir(&staging_dir).expect("create replacement staging directory");
		fs::write(staging_dir.join("sentinel.txt"), "preserve me\n")
			.expect("write replacement sentinel");
		drop(transaction);

		assert_eq!(
			fs::read_to_string(staging_dir.join("sentinel.txt"))
				.expect("replacement staging directory must survive"),
			"preserve me\n"
		);
	}

	#[test]
	fn output_transactions_for_the_same_target_are_serialized() {
		use std::sync::{Arc, Barrier, mpsc};
		use std::thread;
		use std::time::Duration;

		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		let first = OutputTransaction::begin(&out_dir).expect("begin first transaction");
		let started = Arc::new(Barrier::new(2));
		let worker_barrier = Arc::clone(&started);
		let worker_out_dir = out_dir.clone();
		let (acquired_tx, acquired_rx) = mpsc::channel();
		let worker = thread::spawn(move || {
			worker_barrier.wait();
			let second =
				OutputTransaction::begin(&worker_out_dir).expect("begin second transaction");
			acquired_tx.send(()).expect("report acquired lock");
			drop(second);
		});

		started.wait();
		assert!(
			acquired_rx
				.recv_timeout(Duration::from_millis(100))
				.is_err(),
			"second transaction acquired the target lock before the first was dropped"
		);
		drop(first);
		acquired_rx
			.recv_timeout(Duration::from_secs(2))
			.expect("second transaction should acquire the released lock");
		worker.join().expect("join transaction worker");
	}

	#[cfg(unix)]
	#[test]
	fn output_transaction_rejects_an_existing_directory_symlink() {
		use std::os::unix::fs::symlink;

		let temp = tempfile::TempDir::new().expect("temp dir");
		let target = temp.path().join("actual-output");
		let out_dir = temp.path().join("merged-mod");
		fs::create_dir(&target).expect("create symlink target");
		fs::write(target.join("sentinel.txt"), "do not replace\n").expect("write sentinel");
		symlink(&target, &out_dir).expect("create output symlink");

		let error = match OutputTransaction::begin(&out_dir) {
			Ok(_) => panic!("output symlink must be rejected"),
			Err(error) => error,
		};

		assert!(error.to_string().contains("must be a real directory"));
		assert!(
			fs::symlink_metadata(&out_dir)
				.expect("read symlink")
				.file_type()
				.is_symlink()
		);
		assert_eq!(
			fs::read_to_string(target.join("sentinel.txt")).expect("read preserved target"),
			"do not replace\n"
		);
	}

	#[cfg(unix)]
	#[test]
	fn output_transaction_rejects_an_existing_unix_socket() {
		use std::os::unix::fs::FileTypeExt;
		use std::os::unix::net::UnixListener;

		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		let listener = UnixListener::bind(&out_dir).expect("bind output socket");

		let error = match OutputTransaction::begin(&out_dir) {
			Ok(_) => panic!("output socket must be rejected"),
			Err(error) => error,
		};

		assert!(error.to_string().contains("must be a real directory"));
		assert!(
			fs::symlink_metadata(&out_dir)
				.expect("read socket")
				.file_type()
				.is_socket()
		);
		drop(listener);
	}

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
	#[test]
	fn modset_cache_restore_replaces_instead_of_overlaying_output() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let cache = ModsetCache::open(&temp.path().join("cache"));
		let cached_output = temp.path().join("cached-output");
		fs::create_dir_all(cached_output.join("common/governments")).expect("create cached output");
		fs::write(
			cached_output.join("common/governments/current.txt"),
			"cached current module\n",
		)
		.expect("write cached module");
		fs::write(
			cached_output.join(foch::model::MERGED_MOD_DESCRIPTOR_PATH),
			"# Source playset: /cached/source.toml\nname=\"Cached (Merged)\"\npath=\"/cached/output\"\nreplace_path=\"common/governments\"\nreplace_path=\"common/scripted_effects\"\n",
		)
		.expect("write cached descriptor");
		let cached_plan = MergePlanResult {
			generated_at: "cached".to_string(),
			..MergePlanResult::default()
		};
		write_merge_plan_artifact(&cached_output, &cached_plan).expect("write cached plan");
		cache
			.store("cache-key", &cached_output, &MergeReport::default())
			.expect("store cache entry");
		let cached = cache.lookup("cache-key").expect("cache hit");

		let out_dir = temp.path().join("merged-mod");
		fs::create_dir_all(out_dir.join("common/governments")).expect("create old output");
		fs::write(
			out_dir.join("common/governments/stale.txt"),
			"stale module\n",
		)
		.expect("write stale module");

		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		unpack_modset_tarball(&cached.tarball_path, transaction.staging_dir())
			.expect("restore cache into staging");
		let reviewed_plan = MergePlanResult {
			playset_name: "Current playset".to_string(),
			generated_at: "reviewed".to_string(),
			paths: vec![
				MergePlanEntry {
					target: MergePlanTarget::Module {
						id: foch::model::MergeUnitId {
							family_id: "governments".to_string(),
							module_name: "governments".to_string(),
						},
						input_paths: Vec::new(),
						output_path: "common/governments/current.txt".to_string(),
						replace_prefix: Some("common/governments".to_string()),
					},
					strategy: foch::model::MergePlanStrategy::StructuralMerge,
					contributors: Vec::new(),
					winner: None,
					notes: Vec::new(),
				},
				MergePlanEntry {
					target: MergePlanTarget::Module {
						id: foch::model::MergeUnitId {
							family_id: "scripted_effects".to_string(),
							module_name: "scripted_effects".to_string(),
						},
						input_paths: Vec::new(),
						output_path: "common/scripted_effects/missing.txt".to_string(),
						replace_prefix: Some("common/scripted_effects".to_string()),
					},
					strategy: foch::model::MergePlanStrategy::StructuralMerge,
					contributors: Vec::new(),
					winner: None,
					notes: Vec::new(),
				},
			],
			..MergePlanResult::default()
		};
		let current_source = temp.path().join("current/source.toml");
		rewrite_cached_generated_descriptor(
			transaction.staging_dir(),
			&out_dir,
			&current_source,
			&reviewed_plan,
		)
		.expect("rewrite cached descriptor for current request");
		write_merge_plan_artifact(transaction.staging_dir(), &reviewed_plan)
			.expect("replace cached plan with reviewed plan");
		transaction.commit().expect("commit cached output");

		assert_eq!(
			fs::read_to_string(out_dir.join("common/governments/current.txt"))
				.expect("read cached module"),
			"cached current module\n"
		);
		assert!(!out_dir.join("common/governments/stale.txt").exists());
		let persisted_plan: MergePlanResult = serde_json::from_slice(
			&fs::read(out_dir.join(MERGE_PLAN_ARTIFACT_PATH)).expect("read persisted plan"),
		)
		.expect("decode persisted plan");
		assert_eq!(persisted_plan.generated_at, "reviewed");
		let descriptor = fs::read_to_string(out_dir.join(foch::model::MERGED_MOD_DESCRIPTOR_PATH))
			.expect("read rewritten descriptor");
		assert!(
			descriptor.contains(&format!(
				"# Source playset: {}",
				normalize_descriptor_path(&current_source)
			)),
			"{descriptor}"
		);
		assert!(descriptor.contains("name=\"Current playset (Merged)\""));
		assert!(
			descriptor.contains(&format!("path=\"{}\"", normalize_descriptor_path(&out_dir))),
			"{descriptor}"
		);
		assert!(descriptor.contains("replace_path=\"common/governments\""));
		assert!(!descriptor.contains("common/scripted_effects"));
		assert!(!descriptor.contains("/cached/"));
	}

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
	#[test]
	fn modset_cache_stale_base_after_restore_preserves_old_output() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let cache = ModsetCache::open(&temp.path().join("cache"));
		let cached_output = temp.path().join("cached-output");
		fs::create_dir_all(cached_output.join("common/governments")).expect("create cached output");
		fs::write(
			cached_output.join("common/governments/current.txt"),
			"cached current module\n",
		)
		.expect("write cached module");
		cache
			.store("cache-key", &cached_output, &MergeReport::default())
			.expect("store cache entry");
		let cached = cache.lookup("cache-key").expect("cache hit");

		let out_dir = temp.path().join("merged-mod");
		fs::create_dir_all(&out_dir).expect("create old output");
		fs::write(out_dir.join("descriptor.mod"), "old descriptor\n")
			.expect("write old descriptor");
		let base_snapshot = temp.path().join("base-snapshot.bin");
		fs::write(&base_snapshot, "base-v1").expect("write original base token");
		let expected_base = fs::read(&base_snapshot).expect("read original base token");

		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		let staging_dir = transaction.staging_dir().to_path_buf();
		unpack_modset_tarball(&cached.tarball_path, &staging_dir)
			.expect("restore cache into staging");
		let execution = merge_execution_result(cached.report);
		let result = finalize_merge_output(transaction, execution, None, false, |staging_dir| {
			assert!(staging_dir.join("common/governments/current.txt").is_file());
			assert!(staging_dir.join(MERGE_REPORT_ARTIFACT_PATH).is_file());
			fs::write(&base_snapshot, "base-v2")?;
			if fs::read(&base_snapshot)? != expected_base {
				return Err(MergeError::WorkspaceResolve {
					path: base_snapshot.clone(),
					message: "base snapshot changed after cache extraction".to_string(),
				});
			}
			Ok(())
		});

		let error = result.expect_err("stale base must prevent publication");
		assert!(error.to_string().contains("base snapshot changed"));
		assert_eq!(
			fs::read_to_string(out_dir.join("descriptor.mod")).expect("read old output"),
			"old descriptor\n"
		);
		assert!(!out_dir.join("common/governments/current.txt").exists());
	}

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
	#[test]
	fn failed_publication_guard_does_not_store_or_publish_subset_output() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let cache_context = ModsetCacheContext {
			cache: ModsetCache::open(&temp.path().join("cache")),
			key: "subset-cache-key".to_string(),
		};
		let out_dir = temp.path().join("merged-mod");
		fs::create_dir_all(&out_dir).expect("create old output");
		fs::write(out_dir.join("descriptor.mod"), "old descriptor\n")
			.expect("write old descriptor");
		let base_snapshot = temp.path().join("base-snapshot.bin");
		fs::write(&base_snapshot, "base-v1").expect("write original base token");
		let expected_base = fs::read(&base_snapshot).expect("read original base token");

		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		fs::write(
			transaction.staging_dir().join("subset.txt"),
			"new subset output\n",
		)
		.expect("write staged subset");
		let execution = merge_execution_result(MergeReport::default());
		let result = finalize_merge_output(
			transaction,
			execution,
			Some(&cache_context),
			true,
			|staging_dir| {
				assert!(staging_dir.join(MERGE_REPORT_ARTIFACT_PATH).is_file());
				assert!(cache_context.cache.lookup(&cache_context.key).is_none());
				fs::write(&base_snapshot, "base-v2")?;
				if fs::read(&base_snapshot)? != expected_base {
					return Err(MergeError::WorkspaceResolve {
						path: base_snapshot.clone(),
						message: "base snapshot changed before subset publish".to_string(),
					});
				}
				Ok(())
			},
		);

		let error = result.expect_err("stale base must prevent subset publication");
		assert!(error.to_string().contains("base snapshot changed"));
		assert_eq!(
			fs::read_to_string(out_dir.join("descriptor.mod")).expect("read old output"),
			"old descriptor\n"
		);
		assert!(!out_dir.join("subset.txt").exists());
		assert!(cache_context.cache.lookup(&cache_context.key).is_none());
	}

	#[test]
	fn finalization_holds_the_publication_guard_through_publish() {
		use crate::base_data::InstalledBaseSnapshotPublicationGuard;
		use std::sync::Arc;
		use std::sync::atomic::{AtomicBool, Ordering};

		struct PublicationGuardProbe {
			_guard: InstalledBaseSnapshotPublicationGuard,
			alive: Arc<AtomicBool>,
		}

		impl Drop for PublicationGuardProbe {
			fn drop(&mut self) {
				self.alive.store(false, Ordering::SeqCst);
			}
		}

		let _env_guard = BASE_DATA_ENV_LOCK.lock().expect("base data env lock");
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
		let identity = installed_base_snapshot_identity(game.key(), game_version)
			.expect("read installed identity")
			.expect("installed identity exists");

		let out_dir = temp.path().join("merged-mod");
		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		fs::write(transaction.staging_dir().join("new.txt"), "new output\n")
			.expect("write staged output");
		let execution = merge_execution_result(MergeReport::default());
		let guard_alive = Arc::new(AtomicBool::new(false));
		let validate_guard_alive = Arc::clone(&guard_alive);
		let publish_guard_alive = Arc::clone(&guard_alive);
		finalize_merge_output_with_publish(
			transaction,
			execution,
			None,
			false,
			|_| {
				let guard = lock_and_validate_installed_base_snapshot_identity(
					game.key(),
					game_version,
					&identity,
				)
				.map_err(|message| MergeError::WorkspaceResolve {
					path: game_root.clone(),
					message,
				})?;
				validate_guard_alive.store(true, Ordering::SeqCst);
				Ok(PublicationGuardProbe {
					_guard: guard,
					alive: validate_guard_alive,
				})
			},
			|transaction| {
				assert!(
					publish_guard_alive.load(Ordering::SeqCst),
					"commit guard dropped before OutputTransaction::commit"
				);
				transaction.commit()
			},
		)
		.expect("finalize merge output");

		assert!(!guard_alive.load(Ordering::SeqCst));
		assert_eq!(
			fs::read_to_string(out_dir.join("new.txt")).expect("read published output"),
			"new output\n"
		);

		unsafe {
			std::env::remove_var(BASE_DATA_DIR_ENV);
		}
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
		let request = CheckRequest::from_playset_path(
			paradox_dir.join("dlc_load.json"),
			crate::Config {
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

	#[cfg(not(any(target_os = "windows", target_os = "redox")))]
	#[test]
	fn modset_cache_unpack_error_preserves_old_output() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		fs::create_dir_all(&out_dir).expect("create old output");
		fs::write(out_dir.join("descriptor.mod"), "old descriptor\n")
			.expect("write old descriptor");
		let invalid_tarball = temp.path().join("invalid.tar.gz");
		fs::write(&invalid_tarball, "not a tarball").expect("write invalid tarball");

		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		let result = unpack_modset_tarball(&invalid_tarball, transaction.staging_dir());
		drop(transaction);

		assert!(result.is_err());
		assert_eq!(
			fs::read_to_string(out_dir.join("descriptor.mod")).expect("read old descriptor"),
			"old descriptor\n"
		);
	}
}
