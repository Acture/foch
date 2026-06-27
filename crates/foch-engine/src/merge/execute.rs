use super::conflict_handler::ConflictHandler;
use super::error::MergeError;
use super::materialize::{MergeMaterializeOptions, materialize_merge_internal};
use crate::base_data::{detect_game_version, resolve_game_root, resolve_game_root_and_version};
use crate::cache::{
	ModsetCache, compute_mod_hash, compute_modset_cache_key, compute_resolution_map_hash,
	unpack_modset_tarball,
};

// Bump when merge-report semantics change so cached artifacts don't hide new metadata.
const MODSET_CACHE_FORMAT_VERSION: &str =
	"modset-cache-include-base-gfx-effects-union-provenance-gui-tooltip-v7";
use crate::request::{CheckRequest, RunOptions};
use crate::run_checks_with_options;
use crate::workspace::resolve::build_mod_candidates;
use foch_core::config::{AppliedDepOverride, FochConfig, ResolutionMap};
use foch_core::domain::playlist::Playlist;
use foch_core::model::{
	AnalysisMode, ChannelMode, Finding, MERGE_PROVENANCE_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH,
	MergeReport, MergeReportStatus, MergeReportValidation,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct MergeExecuteOptions {
	pub out_dir: PathBuf,
	pub include_game_base: bool,
	pub include_base: bool,
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
	/// Also inject additive EU4 GUI widget tooltips and generated localisation
	/// from collected provenance. Off by default.
	pub gui_tooltip: bool,
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
	let modset_cache = build_modset_cache_context(
		&request,
		options.include_game_base,
		options.include_base,
		options.provenance || options.gui_tooltip,
		options.gui_tooltip,
		options.resolution_config_path.as_deref(),
	);
	if let Some(cache_context) = modset_cache.as_ref() {
		if let Some(cached) = cache_context.cache.lookup(&cache_context.key) {
			eprintln!(
				"[merge] modset_cache_hits=1 modset_cache_misses=0 key={}",
				short_key(&cache_context.key)
			);
			unpack_modset_tarball(&cached.tarball_path, &options.out_dir).map_err(|err| {
				MergeError::Io(io::Error::other(format!(
					"failed to unpack modset cache entry {} into {}: {err}",
					cached.tarball_path.display(),
					options.out_dir.display()
				)))
			})?;
			let mut report = cached.report;
			report.cache_source = Some("modset".to_string());
			report.playset_fingerprint = options.playset_fingerprint.clone();
			let execution = merge_execution_result(report);
			write_merge_report_artifact(&options.out_dir, &execution.report)?;
			return Ok(execution);
		}
		eprintln!(
			"[merge] modset_cache_hits=0 modset_cache_misses=1 key={}",
			short_key(&cache_context.key)
		);
	}

	let resolution_map = load_resolution_map(&request, options.resolution_config_path.as_deref())?;
	let interactive_conflict_handler = options.interactive_conflict_handler;
	let interactive_resolution_config_path = options.interactive_resolution_config_path;
	let mut report = materialize_merge_internal(
		request.clone(),
		&options.out_dir,
		MergeMaterializeOptions {
			include_game_base: options.include_game_base,
			include_base: options.include_base,
			force: options.force,
			ignore_replace_path: options.ignore_replace_path,
			dep_overrides: options.dep_overrides.clone(),
			resolution_map,
			interactive_conflict_handler,
			interactive_resolution_config_path,
			provenance: options.provenance || options.gui_tooltip,
			gui_tooltip: options.gui_tooltip,
		},
	)?;
	report.playset_fingerprint = options.playset_fingerprint.clone();

	if report.status == MergeReportStatus::Fatal {
		let execution = merge_execution_result(report);
		write_merge_report_artifact(&options.out_dir, &execution.report)?;
		store_modset_cache_entry(modset_cache.as_ref(), &options.out_dir, &execution.report);
		return Ok(execution);
	}

	if report.status == MergeReportStatus::Blocked && !options.force {
		let execution = merge_execution_result(report);
		write_merge_report_artifact(&options.out_dir, &execution.report)?;
		store_modset_cache_entry(modset_cache.as_ref(), &options.out_dir, &execution.report);
		return Ok(execution);
	}

	let validation =
		revalidate_generated_output(&request, &options.out_dir, options.include_game_base)?;
	report.validation = validation;
	let execution = merge_execution_result(report);
	write_merge_report_artifact(&options.out_dir, &execution.report)?;
	store_modset_cache_entry(modset_cache.as_ref(), &options.out_dir, &execution.report);

	Ok(execution)
}

#[derive(Clone, Debug)]
struct ModsetCacheContext {
	cache: ModsetCache,
	key: String,
}

fn build_modset_cache_context(
	request: &CheckRequest,
	include_game_base: bool,
	include_base: bool,
	provenance: bool,
	gui_tooltip: bool,
	resolution_config_path: Option<&Path>,
) -> Option<ModsetCacheContext> {
	if resolution_config_path.is_some_and(|path| !path.is_file()) {
		return None;
	}
	let playlist = Playlist::from_dlc_load(&request.playset_path).ok()?;
	let game_version = modset_cache_game_version(request, &playlist, include_game_base)?;
	let candidates = build_mod_candidates(request, &playlist);
	let mut mod_hashes = Vec::new();
	for candidate in candidates
		.iter()
		.filter(|candidate| candidate.entry.enabled)
	{
		let root = candidate.root_path.as_ref()?;
		mod_hashes.push(compute_mod_hash(root).ok()?);
	}
	let playset_root = request
		.playset_path
		.parent()
		.unwrap_or_else(|| Path::new("."));
	let resolution_map_hash = compute_resolution_map_hash(&resolution_config_bytes(
		playset_root,
		resolution_config_path,
	));
	let foch_version = format!(
		"{} {MODSET_CACHE_FORMAT_VERSION} include_base={include_base} provenance={provenance} gui_tooltip={gui_tooltip}",
		env!("CARGO_PKG_VERSION"),
	);
	let key = compute_modset_cache_key(
		&mod_hashes,
		&resolution_map_hash,
		&foch_version,
		&game_version,
	);
	Some(ModsetCacheContext {
		cache: ModsetCache::open_default(),
		key,
	})
}

fn modset_cache_game_version(
	request: &CheckRequest,
	playlist: &Playlist,
	include_game_base: bool,
) -> Option<String> {
	let version = if include_game_base {
		resolve_game_root_and_version(&request.config, &playlist.game)
			.ok()
			.map(|(_, version)| version)?
	} else {
		resolve_game_root(&request.config, &playlist.game)
			.as_ref()
			.and_then(|root| detect_game_version(root))
			.unwrap_or_else(|| "unknown".to_string())
	};
	Some(format!("{} {version}", playlist.game.key()))
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
		.playset_path
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
	let result = run_checks_with_options(
		CheckRequest {
			playset_path: dlc_load_path.clone(),
			config: request.config.clone(),
		},
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
	use foch_core::model::HandlerResolutionRecord;

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
}
