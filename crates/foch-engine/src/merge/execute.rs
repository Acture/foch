use super::error::MergeError;
use super::materialize::{MergeMaterializeOptions, materialize_merge_internal};
use crate::base_data::{detect_game_version, resolve_game_root, resolve_game_root_and_version};
use crate::cache::{
	ModsetCache, compute_mod_hash, compute_modset_cache_key, compute_resolution_map_hash,
	unpack_modset_tarball,
};
use crate::request::{CheckRequest, RunOptions};
use crate::run_checks_with_options;
use crate::workspace::resolve::build_mod_candidates;
use foch_core::config::{AppliedDepOverride, FochConfig, ResolutionMap};
use foch_core::domain::playlist::Playlist;
use foch_core::model::{
	AnalysisMode, ChannelMode, Finding, MERGE_REPORT_ARTIFACT_PATH, MergeReport, MergeReportStatus,
	MergeReportValidation,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct MergeExecuteOptions {
	pub out_dir: PathBuf,
	pub include_game_base: bool,
	pub force: bool,
	pub ignore_replace_path: bool,
	pub dep_overrides: Vec<AppliedDepOverride>,
	/// Optional explicit foch.toml path supplied by the CLI.
	pub resolution_config_path: Option<PathBuf>,
	/// Caller-computed playset fingerprint to stamp on the merge report so
	/// subsequent runs can detect "same mod set, reuse the cached output".
	/// `None` skips the stamp (e.g., merge invoked from a context where
	/// computing it isn't possible).
	pub playset_fingerprint: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MergeExecutionResult {
	pub report: MergeReport,
	pub exit_code: i32,
}

pub fn run_merge_with_options(
	request: CheckRequest,
	options: MergeExecuteOptions,
) -> Result<MergeExecutionResult, MergeError> {
	let modset_cache = build_modset_cache_context(
		&request,
		options.include_game_base,
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
			write_merge_report_artifact(&options.out_dir, &report)?;
			let exit_code = merge_report_exit_code(&report);
			return Ok(MergeExecutionResult { report, exit_code });
		}
		eprintln!(
			"[merge] modset_cache_hits=0 modset_cache_misses=1 key={}",
			short_key(&cache_context.key)
		);
	}

	let resolution_map = load_resolution_map(&request, options.resolution_config_path.as_deref())?;
	let mut report = materialize_merge_internal(
		request.clone(),
		&options.out_dir,
		MergeMaterializeOptions {
			include_game_base: options.include_game_base,
			force: options.force,
			ignore_replace_path: options.ignore_replace_path,
			dep_overrides: options.dep_overrides.clone(),
			resolution_map,
		},
	)?;
	report.playset_fingerprint = options.playset_fingerprint.clone();

	if report.status == MergeReportStatus::Fatal {
		write_merge_report_artifact(&options.out_dir, &report)?;
		store_modset_cache_entry(modset_cache.as_ref(), &options.out_dir, &report);
		return Ok(MergeExecutionResult {
			report,
			exit_code: 1,
		});
	}

	if report.status == MergeReportStatus::Blocked && !options.force {
		write_merge_report_artifact(&options.out_dir, &report)?;
		store_modset_cache_entry(modset_cache.as_ref(), &options.out_dir, &report);
		return Ok(MergeExecutionResult {
			report,
			exit_code: 2,
		});
	}

	let validation =
		revalidate_generated_output(&request, &options.out_dir, options.include_game_base)?;
	report.validation = validation;
	report.status = final_merge_status(&report);
	write_merge_report_artifact(&options.out_dir, &report)?;
	store_modset_cache_entry(modset_cache.as_ref(), &options.out_dir, &report);

	let exit_code = merge_report_exit_code(&report);

	Ok(MergeExecutionResult { report, exit_code })
}

#[derive(Clone, Debug)]
struct ModsetCacheContext {
	cache: ModsetCache,
	key: String,
}

fn build_modset_cache_context(
	request: &CheckRequest,
	include_game_base: bool,
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
	let key = compute_modset_cache_key(
		&mod_hashes,
		&resolution_map_hash,
		env!("CARGO_PKG_VERSION"),
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

fn merge_report_exit_code(report: &MergeReport) -> i32 {
	match report.status {
		MergeReportStatus::Ready => 0,
		MergeReportStatus::PartialSuccess => 0,
		MergeReportStatus::Blocked => 2,
		MergeReportStatus::Fatal => 3,
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

fn final_merge_status(report: &MergeReport) -> MergeReportStatus {
	// Only true I/O / fatal-class issues drop the merge to Fatal. Strict /
	// advisory / parse / unresolved-reference / missing-localisation findings
	// are surfaced separately; they're often present in each individual mod
	// already and should not silently demote a successful merge to Fatal.
	let has_fatal_errors = report.validation.fatal_errors > 0;

	if has_fatal_errors {
		MergeReportStatus::Fatal
	} else if report.manual_conflict_count > 0 {
		// If materialize already set PartialSuccess (--force resolved conflicts),
		// keep it.  Otherwise block.
		match report.status {
			MergeReportStatus::PartialSuccess => MergeReportStatus::PartialSuccess,
			_ => MergeReportStatus::Blocked,
		}
	} else if !report.handler_resolutions.is_empty() {
		MergeReportStatus::PartialSuccess
	} else {
		MergeReportStatus::Ready
	}
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
