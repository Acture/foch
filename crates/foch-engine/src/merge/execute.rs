use super::conflict_handler::ConflictHandler;
use super::error::MergeError;
use super::materialize::{
	MergeMaterializeOptions, OutputTransaction, materialize_merge_with_workspace_result,
};
use crate::base_data::{
	InstalledBaseSnapshotIdentity, InstalledBaseSnapshotPublicationGuard,
	lock_and_validate_installed_base_snapshot_identity,
};
use crate::cache::{
	ModsetCache, compute_modset_cache_key, compute_resolution_map_hash, unpack_modset_tarball,
};

// SemVer identity for cached merge output. Bump patch for output bug fixes,
// minor for additive semantics, and major for incompatible cache payloads.
const MODSET_CACHE_VERSION: &str = "13.0.0";
use crate::request::{CheckRequest, RunOptions};
use crate::run_checks_with_options;
use crate::workspace::{
	WorkspaceInventory, build_workspace_inventory_with_hash_cache, resolve_workspace_from_inventory,
};
use foch_core::config::{AppliedDepOverride, FochConfig, ResolutionDecision, ResolutionMap};
use foch_core::model::{
	AnalysisMode, ChannelMode, Finding, MERGE_PROVENANCE_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH,
	MERGE_TRACE_ARTIFACT_PATH, MergeReport, MergeReportStatus, MergeReportValidation,
};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct MergeExecuteOptions {
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
pub struct MergeExecutionResult {
	pub report: MergeReport,
	pub merge_status: MergeStatusView,
	pub analysis_status: AnalysisStatusView,
	pub exit_code: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergeStatusView {
	pub status: MergeReportStatus,
	pub manual_conflict_count: usize,
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
	options: MergeExecuteOptions,
) -> Result<MergeExecutionResult, MergeError> {
	let inventory_started = Instant::now();
	let mut inventory_result = build_workspace_inventory_with_hash_cache(
		&request,
		options.include_game_base,
		options.retained_paths.is_some(),
		options.retained_paths.as_ref(),
	);
	if let Ok(inventory) = inventory_result.as_ref() {
		let hashed_mods = inventory
			.mod_hashes
			.iter()
			.filter(|hash| hash.is_some())
			.count();
		eprintln!(
			"[merge] build_workspace_inventory: done elapsed_ms={} mods={} hashed_mods={}",
			inventory_started.elapsed().as_millis(),
			inventory.mods.len(),
			hashed_mods
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
	let resolution_map = load_resolution_map(&request, options.resolution_config_path.as_deref())?;
	let has_interactive_conflict_handler = options.interactive_conflict_handler.is_some();
	let depends_on_prior_output = resolution_map_depends_on_prior_output(&resolution_map);
	let modset_cache =
		if modset_cache_is_eligible(has_interactive_conflict_handler, depends_on_prior_output) {
			let cache = match ModsetCache::open_default_versioned(MODSET_CACHE_VERSION) {
				Ok(cache) => Some(cache),
				Err(err) => {
					tracing::warn!(
						cache_version = MODSET_CACHE_VERSION,
						error = %err,
						"modset cache version cleanup failed; continuing without modset cache"
					);
					None
				}
			};
			cache.and_then(|cache| {
				inventory_result.as_ref().ok().and_then(|inventory| {
					build_modset_cache_context(&request, inventory, &options, cache)
				})
			})
		} else if has_interactive_conflict_handler {
			eprintln!("[merge] modset_cache_bypass=interactive_conflict_handler");
			None
		} else {
			eprintln!("[merge] modset_cache_bypass=keep_existing_resolution");
			None
		};
	let final_out_dir = options.out_dir.clone();
	let transaction = OutputTransaction::begin(&final_out_dir)?;
	let prior_out_dir = transaction.prior_dir().map(Path::to_path_buf);
	let staging_dir = transaction.staging_dir().to_path_buf();
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
				let mut report = cached.report;
				report.cache_source = Some("modset".to_string());
				report.playset_fingerprint = options.playset_fingerprint.clone();
				let execution = merge_execution_result(report);
				return finalize_merge_output(transaction, execution, None, false, |_| {
					validate_base_snapshot_publish_guard(base_snapshot_publish_guard.as_ref())
				});
			}
		}
		eprintln!(
			"[merge] modset_cache_hits=0 modset_cache_misses=1 key={}",
			short_key(&cache_context.key)
		);
	}

	let interactive_conflict_handler = options.interactive_conflict_handler;
	let interactive_resolution_config_path = options.interactive_resolution_config_path;
	let effective_retained_paths = inventory_result
		.as_ref()
		.ok()
		.and_then(|inventory| inventory.effective_retained_paths.clone());
	let resolve_started = Instant::now();
	let workspace_result = inventory_result.and_then(resolve_workspace_from_inventory);
	if let Ok(workspace) = workspace_result.as_ref() {
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
	let mut report = materialize_merge_with_workspace_result(
		request.clone(),
		&staging_dir,
		prior_out_dir.as_deref(),
		&final_out_dir,
		MergeMaterializeOptions {
			include_game_base: options.include_game_base,
			include_base: options.include_base,
			gui_scroll_merge: options.gui_scroll_merge,
			force: options.force,
			ignore_replace_path: options.ignore_replace_path,
			dep_overrides: options.dep_overrides.clone(),
			resolution_map,
			interactive_conflict_handler,
			interactive_resolution_config_path,
			provenance: options.provenance,
			retained_paths: effective_retained_paths,
		},
		workspace_result,
	)?;
	report.playset_fingerprint = options.playset_fingerprint.clone();

	if report.status == MergeReportStatus::Fatal {
		let execution = merge_execution_result(report);
		return finalize_merge_output(transaction, execution, modset_cache.as_ref(), false, |_| {
			validate_base_snapshot_publish_guard(base_snapshot_publish_guard.as_ref())
		});
	}

	if report.status == MergeReportStatus::Blocked && !options.force {
		let execution = merge_execution_result(report);
		return finalize_merge_output(transaction, execution, modset_cache.as_ref(), true, |_| {
			validate_base_snapshot_publish_guard(base_snapshot_publish_guard.as_ref())
		});
	}

	if options.retained_paths.is_none() {
		let validation = revalidate_generated_output(
			&request,
			&staging_dir,
			options.include_game_base,
			base_snapshot_publish_guard
				.as_ref()
				.map(|guard| guard.identity.clone()),
		)?;
		report.validation = validation;
	}
	let execution = merge_execution_result(report);
	finalize_merge_output(transaction, execution, modset_cache.as_ref(), true, |_| {
		validate_base_snapshot_publish_guard(base_snapshot_publish_guard.as_ref())
	})
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

fn finalize_merge_output<Guard>(
	transaction: OutputTransaction,
	execution: MergeExecutionResult,
	cache_context: Option<&ModsetCacheContext>,
	store_cache: bool,
	validate_base_snapshot: impl FnOnce(&Path) -> Result<Guard, MergeError>,
) -> Result<MergeExecutionResult, MergeError> {
	finalize_merge_output_with_publish(
		transaction,
		execution,
		cache_context,
		store_cache,
		validate_base_snapshot,
		OutputTransaction::publish,
	)
}

fn finalize_merge_output_with_publish<Guard>(
	transaction: OutputTransaction,
	execution: MergeExecutionResult,
	cache_context: Option<&ModsetCacheContext>,
	store_cache: bool,
	validate_base_snapshot: impl FnOnce(&Path) -> Result<Guard, MergeError>,
	publish: impl FnOnce(OutputTransaction) -> Result<(), MergeError>,
) -> Result<MergeExecutionResult, MergeError> {
	write_merge_report_artifact(transaction.staging_dir(), &execution.report)?;
	if store_cache {
		store_modset_cache_entry(cache_context, transaction.staging_dir(), &execution.report);
	}
	// Keep this as the last semantic check: extraction and report generation
	// or cache storage may race with replacement of the snapshot lease.
	let _base_snapshot_publication_guard = validate_base_snapshot(transaction.staging_dir())?;
	publish(transaction)?;
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

fn build_modset_cache_context(
	request: &CheckRequest,
	inventory: &WorkspaceInventory,
	options: &MergeExecuteOptions,
	cache: ModsetCache,
) -> Option<ModsetCacheContext> {
	if options
		.resolution_config_path
		.as_deref()
		.is_some_and(|path| !path.is_file())
	{
		return None;
	}
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
	let playset_root = request
		.source_path()
		.parent()
		.unwrap_or_else(|| Path::new("."));
	let resolution_map_hash = compute_resolution_map_hash(&resolution_config_bytes(
		playset_root,
		options.resolution_config_path.as_deref(),
	));
	let retained_paths_label = retained_paths_cache_label(
		inventory.effective_retained_paths.as_ref(),
		&inventory.retained_module_policy_versions,
	);
	let dep_overrides_label = dep_overrides_cache_label(&options.dep_overrides);
	let output_dir_label = output_dir_cache_label(&options.out_dir);
	let foch_version = modset_cache_version_label(ModsetCacheBehavior {
		include_base: options.include_base,
		gui_scroll_merge: options.gui_scroll_merge,
		force: options.force,
		ignore_replace_path: options.ignore_replace_path,
		provenance: options.provenance,
		dep_overrides: &dep_overrides_label,
		retained_paths: &retained_paths_label,
		output_dir: &output_dir_label,
	});
	let key = compute_modset_cache_key(
		&mod_hashes,
		&resolution_map_hash,
		&foch_version,
		&game_version,
	);
	Some(ModsetCacheContext { cache, key })
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
	output_dir: &'a str,
}

fn modset_cache_version_label(behavior: ModsetCacheBehavior<'_>) -> String {
	format!(
		"{} modset_cache={MODSET_CACHE_VERSION} include_base={} provenance={} gui_scroll_merge={} force={} ignore_replace_path={} dep_overrides={} retained_paths={} output_dir={}",
		env!("CARGO_PKG_VERSION"),
		behavior.include_base,
		behavior.provenance,
		behavior.gui_scroll_merge,
		behavior.force,
		behavior.ignore_replace_path,
		behavior.dep_overrides,
		behavior.retained_paths,
		behavior.output_dir,
	)
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
		ResolutionDecision::PreferMod(_) | ResolutionDecision::UseFile(_) => false,
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
	module_policy_versions: &std::collections::BTreeMap<foch_core::model::MergeUnitId, u32>,
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

fn output_dir_cache_label(out_dir: &Path) -> String {
	let identity = if out_dir.is_absolute() {
		out_dir.to_path_buf()
	} else {
		std::env::current_dir()
			.map(|cwd| cwd.join(out_dir))
			.unwrap_or_else(|_| out_dir.to_path_buf())
	};
	blake3::hash(normalize_descriptor_path(&identity).as_bytes())
		.to_hex()
		.to_string()
}

fn resolution_config_bytes(playset_root: &Path, explicit_path: Option<&Path>) -> Vec<u8> {
	let mut bytes = Vec::new();
	let paths = explicit_path
		.map(|path| vec![path.to_path_buf()])
		.unwrap_or_else(|| resolution_config_search_paths(playset_root));
	for path in paths {
		let Ok(raw) = fs::read(&path) else {
			continue;
		};
		let normalized_path = path.to_string_lossy().replace('\\', "/");
		bytes.extend_from_slice(&(normalized_path.len() as u64).to_le_bytes());
		bytes.extend_from_slice(normalized_path.as_bytes());
		bytes.extend_from_slice(&(raw.len() as u64).to_le_bytes());
		bytes.extend_from_slice(&raw);
	}
	bytes
}

fn resolution_config_search_paths(playset_root: &Path) -> Vec<PathBuf> {
	let mut paths = Vec::new();
	let mut seen = std::collections::HashSet::new();
	if let Ok(cwd) = std::env::current_dir() {
		push_unique_path(&mut paths, &mut seen, cwd.join("foch.toml"));
	}
	push_unique_path(&mut paths, &mut seen, playset_root.join("foch.toml"));
	if let Some(home) = dirs::home_dir() {
		push_unique_path(
			&mut paths,
			&mut seen,
			home.join(".config").join("foch").join("foch.toml"),
		);
	}
	paths
}

fn push_unique_path(
	paths: &mut Vec<PathBuf>,
	seen: &mut std::collections::HashSet<PathBuf>,
	path: PathBuf,
) {
	if seen.insert(path.clone()) {
		paths.push(path);
	}
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

fn merge_execution_result(mut report: MergeReport) -> MergeExecutionResult {
	let merge_status = compute_merge_status(&report);
	report.status = merge_status.status;
	let analysis_status = compute_analysis_status(&report);
	let exit_code = merge_execution_exit_code(&merge_status, &analysis_status);
	MergeExecutionResult {
		report,
		merge_status,
		analysis_status,
		exit_code,
	}
}

fn compute_merge_status(report: &MergeReport) -> MergeStatusView {
	let status = if report.status == MergeReportStatus::Fatal {
		MergeReportStatus::Fatal
	} else if report.manual_conflict_count > 0 {
		match report.status {
			MergeReportStatus::PartialSuccess => MergeReportStatus::PartialSuccess,
			_ => MergeReportStatus::Blocked,
		}
	} else if !report.handler_resolutions.is_empty() {
		MergeReportStatus::PartialSuccess
	} else {
		MergeReportStatus::Ready
	};

	MergeStatusView {
		status,
		manual_conflict_count: report.manual_conflict_count,
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

fn load_resolution_map(
	request: &CheckRequest,
	explicit_path: Option<&Path>,
) -> Result<ResolutionMap, MergeError> {
	let playset_root = request
		.source_path()
		.parent()
		.unwrap_or_else(|| Path::new("."));
	let config = if let Some(path) = explicit_path {
		FochConfig::load_from_path(path).map_err(|err| MergeError::Validation {
			path: Some(path.display().to_string()),
			message: err.to_string(),
		})?
	} else {
		FochConfig::try_load(playset_root).map_err(|err| MergeError::Validation {
			path: Some(playset_root.display().to_string()),
			message: err.to_string(),
		})?
	};
	ResolutionMap::from_entries(&config.resolutions).map_err(|err| MergeError::Validation {
		path: Some(explicit_path.unwrap_or(playset_root).display().to_string()),
		message: err.to_string(),
	})
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
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|duration| duration.as_nanos())
		.unwrap_or_default();
	parent_dir.join(format!(".foch-merge-validation-{pid}-{nanos}"))
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
	use crate::workspace::FileFilter;
	use foch_core::domain::game::Game;
	use foch_core::model::HandlerResolutionRecord;
	use std::collections::HashMap;

	fn report_with(mut update: impl FnMut(&mut MergeReport)) -> MergeReport {
		let mut report = MergeReport::default();
		update(&mut report);
		report
	}

	#[test]
	fn compute_merge_status_blocked_on_manual_conflict() {
		let report = report_with(|report| {
			report.manual_conflict_count = 1;
			report.generated_file_count = 7;
		});

		assert_eq!(
			compute_merge_status(&report),
			MergeStatusView {
				status: MergeReportStatus::Blocked,
				manual_conflict_count: 1,
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
				handler_resolution_count: 0,
				generated_file_count: 0,
			}
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
		let module = foch_core::model::MergeUnitId {
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
				output_dir: "same-output",
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
				output_dir: "same-output",
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

	#[test]
	fn output_transaction_reports_the_prior_tree_it_observed() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");

		let missing = OutputTransaction::begin(&out_dir).expect("begin missing transaction");
		assert_eq!(missing.prior_dir(), None);
		drop(missing);

		fs::create_dir(&out_dir).expect("create prior output");
		let existing = OutputTransaction::begin(&out_dir).expect("begin existing transaction");
		assert_eq!(existing.prior_dir(), Some(out_dir.as_path()));
	}

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
		transaction.publish().expect("publish transaction");

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

	#[test]
	fn output_transaction_rejects_a_replaced_directory_before_publish() {
		let temp = tempfile::TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("merged-mod");
		fs::create_dir(&out_dir).expect("create initial output");
		let transaction = OutputTransaction::begin(&out_dir).expect("begin transaction");
		fs::write(transaction.staging_dir().join("new.txt"), "new output\n")
			.expect("write staged output");

		fs::remove_dir(&out_dir).expect("remove initial output");
		fs::create_dir(&out_dir).expect("create concurrent replacement");
		fs::write(out_dir.join("concurrent.txt"), "preserve me\n")
			.expect("write concurrent replacement");
		let error = transaction
			.publish()
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
		transaction.publish().expect("publish cached output");

		assert_eq!(
			fs::read_to_string(out_dir.join("common/governments/current.txt"))
				.expect("read cached module"),
			"cached current module\n"
		);
		assert!(!out_dir.join("common/governments/stale.txt").exists());
	}

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

	#[test]
	fn normal_subset_stale_base_after_cache_store_preserves_old_output() {
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
				assert!(cache_context.cache.lookup(&cache_context.key).is_some());
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

		let game = Game::EuropaUniversalis4;
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
		let filter = FileFilter::new(game.clone(), &[]).expect("build file filter");
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
					"publication guard dropped before OutputTransaction::publish"
				);
				transaction.publish()
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

		let game = Game::EuropaUniversalis4;
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
		let filter = FileFilter::new(game.clone(), &[]).expect("build file filter");
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
		let result = run_merge_with_options(
			CheckRequest::from_playset_path(
				paradox_dir.join("dlc_load.json"),
				crate::Config {
					steam_root_path: None,
					paradox_data_path: None,
					game_path,
					extra_ignore_patterns: Vec::new(),
				},
			),
			MergeExecuteOptions {
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
