use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use foch_core::config::{FochConfig, WorkspaceConfig, WorkspaceMod};
use foch_core::domain::game::Game;
use foch_core::model::{
	MERGE_EXECUTION_ATTESTATION_SCHEMA, MERGE_REPORT_ARTIFACT_PATH, MergeReport,
	MergeReportBaseSnapshot, MergeReportKernel, MergeReportScope, MergeReportStatus,
	PRODUCT_INPUT_DIGEST_ALGORITHM, PRODUCT_INPUT_MANIFEST_SCHEMA, PRODUCT_INPUT_PROFILE,
	ProductInputManifest, ProductInputMod,
};
use foch_core::utils::steam::{SteamId, WorkshopInstallIdentity};
use foch_merge_quality::corpus::Case;
use foch_merge_quality::dataset::{EngineArtifactIdentity, MeasurementKernel, MeasurementScope};
use foch_merge_quality::lifecycle::{
	MeasurementRequest, MeasurementRunner, MeasurementRunnerIdentity, TerminalMerge,
	executable_hash,
};

pub const RUNNER_PROTOCOL_VERSION: &str = "foch-cli-publishable-merge-report-v5";
const MAX_DIAGNOSTIC_BYTES: u64 = 64 * 1024;
const MAX_PLAN_BYTES: u64 = 4 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const DISABLED_BASE_SNAPSHOT_IDENTITY: &str = "explicitly-disabled";
const CACHE_MAX_BYTES_ENV: &str = "FOCH_CACHE_MAX_BYTES";
const CACHE_DIAGNOSTIC_PREFIXES: [&[u8]; 2] =
	[b"[merge] mod_snapshot:", b"[merge] resolve_workspace:"];

enum ProductCacheRoot {
	SystemManaged { path: PathBuf },
	RunnerScoped { owner: tempfile::TempDir },
}

impl ProductCacheRoot {
	fn path(&self) -> &Path {
		match self {
			Self::SystemManaged { path } => path,
			Self::RunnerScoped { owner } => owner.path(),
		}
	}
}

struct ProductCacheEnvironment {
	root: ProductCacheRoot,
	max_bytes: Option<OsString>,
}

impl ProductCacheEnvironment {
	fn system_managed() -> Self {
		Self {
			root: ProductCacheRoot::SystemManaged {
				path: foch_engine::default_foch_cache_dir(),
			},
			max_bytes: std::env::var_os(CACHE_MAX_BYTES_ENV),
		}
	}

	fn runner_scoped() -> io::Result<Self> {
		let owner = tempfile::Builder::new()
			.prefix("foch-product-fixture-cache-")
			.tempdir()?;
		Ok(Self {
			root: ProductCacheRoot::RunnerScoped { owner },
			max_bytes: std::env::var_os(CACHE_MAX_BYTES_ENV),
		})
	}

	fn runner_scoped_default_cap() -> io::Result<Self> {
		Self::runner_scoped_default_cap_for(std::env::var_os(CACHE_MAX_BYTES_ENV))
	}

	fn runner_scoped_default_cap_for(configured_cap: Option<OsString>) -> io::Result<Self> {
		if configured_cap.is_some() {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!(
					"{CACHE_MAX_BYTES_ENV} must be absent for the default 1 GiB cache-residency gate"
				),
			));
		}
		let owner = tempfile::Builder::new()
			.prefix("foch-product-cache-gate-")
			.tempdir()?;
		Ok(Self {
			root: ProductCacheRoot::RunnerScoped { owner },
			max_bytes: None,
		})
	}

	fn apply_to(&self, command: &mut Command) {
		command.env(foch_core::cache::CACHE_ROOT_ENV, self.root.path());
		if let Some(max_bytes) = &self.max_bytes {
			command.env(CACHE_MAX_BYTES_ENV, max_bytes);
		}
	}
}

pub(crate) struct ProductPreviewObservation {
	pub failure: Option<String>,
	pub plan_output: String,
	pub cache_diagnostics: String,
	pub output_exists: bool,
	pub report_exists: bool,
}

struct ProductCommandResult {
	run_root: tempfile::TempDir,
	status: ExitStatus,
	timed_out: bool,
	merge_ms: u64,
	stdout: String,
	stderr: String,
	cache_diagnostics: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalFailureMode {
	Return,
	Panic,
}

/// Executes every measurement through the exact `foch` binary Cargo built for
/// this integration-test process.
pub struct ProductMeasurementRunner {
	executable: PathBuf,
	identity: MeasurementRunnerIdentity,
	base_data_root: PathBuf,
	cache: ProductCacheEnvironment,
	no_game_base: bool,
	terminal_failure_mode: TerminalFailureMode,
	cohort_cold_built_mod_ids: BTreeSet<String>,
}

impl ProductMeasurementRunner {
	pub fn full_product() -> io::Result<Self> {
		Self::new(
			false,
			ProductCacheEnvironment::system_managed(),
			TerminalFailureMode::Return,
		)
	}

	pub(crate) fn full_product_fail_fast() -> io::Result<Self> {
		Self::full_product_fail_fast_for(std::env::var_os(CACHE_MAX_BYTES_ENV))
	}

	fn full_product_fail_fast_for(configured_cap: Option<OsString>) -> io::Result<Self> {
		Self::new(
			false,
			ProductCacheEnvironment::runner_scoped_default_cap_for(configured_cap)?,
			TerminalFailureMode::Panic,
		)
	}

	pub fn no_game_base_fixture() -> io::Result<Self> {
		Self::new(
			true,
			ProductCacheEnvironment::runner_scoped()?,
			TerminalFailureMode::Return,
		)
	}

	pub(crate) fn workshop_cache_gate() -> io::Result<Self> {
		Self::new(
			false,
			ProductCacheEnvironment::runner_scoped_default_cap()?,
			TerminalFailureMode::Return,
		)
	}

	fn new(
		no_game_base: bool,
		cache: ProductCacheEnvironment,
		terminal_failure_mode: TerminalFailureMode,
	) -> io::Result<Self> {
		let executable = PathBuf::from(env!("CARGO_BIN_EXE_foch"));
		let hash = executable_hash(&executable)?;
		let base_data_root = std::env::var_os(foch_engine::BASE_DATA_DIR_ENV)
			.map(PathBuf::from)
			.unwrap_or_else(|| {
				dirs::data_local_dir()
					.unwrap_or_else(std::env::temp_dir)
					.join("foch")
					.join("data")
			});
		Ok(Self {
			executable,
			identity: MeasurementRunnerIdentity {
				engine_artifact: EngineArtifactIdentity::foch_executable_blake3(hash),
				runner_protocol_version: RUNNER_PROTOCOL_VERSION.to_string(),
				merge_kernel: MeasurementKernel::SemanticTree,
				scope: MeasurementScope::FullProductMerge,
			},
			base_data_root,
			cache,
			no_game_base,
			terminal_failure_mode,
			cohort_cold_built_mod_ids: BTreeSet::new(),
		})
	}

	fn run_product_command(
		&self,
		request: &MeasurementRequest,
		confirm: bool,
	) -> Result<ProductCommandResult, String> {
		let before_hash = executable_hash(&self.executable)
			.map_err(|error| format!("failed to hash product executable before merge: {error}"))?;
		if before_hash != self.identity.engine_artifact.hash {
			return Err("product executable changed after runner identity was created".to_string());
		}

		validate_request(request)?;
		if !self.no_game_base {
			validate_base_snapshot(request)?;
		}
		let run_root = tempfile::Builder::new()
			.prefix("foch-product-measurement-")
			.tempdir()
			.map_err(|error| format!("failed to create isolated runner directory: {error}"))?;
		let manifest_path = run_root.path().join("foch.toml");
		let config_dir = run_root.path().join("config");
		let home_dir = run_root.path().join("home");
		let xdg_config_home = run_root.path().join("xdg-config");
		let xdg_data_home = run_root.path().join("xdg-data");
		let xdg_cache_home = run_root.path().join("xdg-cache");
		let temp_root = run_root.path().join("tmp");
		for path in [
			&config_dir,
			&home_dir,
			&xdg_config_home,
			&xdg_data_home,
			&xdg_cache_home,
			&temp_root,
		] {
			fs::create_dir_all(path)
				.map_err(|error| format!("failed to create isolated runner state: {error}"))?;
		}
		write_workspace_manifest(&manifest_path, request)?;
		write_engine_config(&config_dir, request, run_root.path())?;

		let mut command = Command::new(&self.executable);
		command
			.env_clear()
			.env("FOCH_CONFIG_DIR", &config_dir)
			.env(foch_engine::BASE_DATA_DIR_ENV, &self.base_data_root)
			.env("HOME", &home_dir)
			.env("XDG_CONFIG_HOME", &xdg_config_home)
			.env("XDG_DATA_HOME", &xdg_data_home)
			.env("XDG_CACHE_HOME", &xdg_cache_home)
			.env("TMPDIR", &temp_root)
			.env("NO_COLOR", "1")
			.current_dir(run_root.path())
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.arg("merge")
			.arg(&manifest_path)
			.arg("--out")
			.arg(&request.output_dir)
			.arg("--non-interactive");
		self.cache.apply_to(&mut command);
		if confirm {
			command.arg("--confirm");
		}
		if self.no_game_base {
			command.arg("--no-game-base");
		}

		let merge_started = Instant::now();
		let mut child = command
			.spawn()
			.map_err(|error| format!("failed to start product merge: {error}"))?;
		let stdout_reader = child
			.stdout
			.take()
			.ok_or_else(|| "failed to capture product stdout".to_string())?;
		let stderr_reader = child
			.stderr
			.take()
			.ok_or_else(|| "failed to capture product stderr".to_string())?;
		let stdout_capture = spawn_bounded_prefix_capture(stdout_reader);
		let stderr_capture = spawn_bounded_diagnostic_capture(stderr_reader);
		let mut timed_out = false;
		let status = loop {
			match child
				.try_wait()
				.map_err(|error| format!("failed to poll product merge: {error}"))?
			{
				Some(status) => break status,
				None if merge_started.elapsed() >= request.timeout => {
					timed_out = true;
					let _ = child.kill();
					break child.wait().map_err(|error| {
						format!("failed to reap timed-out product merge: {error}")
					})?;
				}
				None => thread::sleep(POLL_INTERVAL.min(request.timeout)),
			}
		};
		let merge_ms = u64::try_from(merge_started.elapsed().as_millis()).unwrap_or(u64::MAX);
		let stdout = finish_bounded_prefix_capture(stdout_capture, "stdout");
		let (stderr, cache_diagnostics) =
			finish_bounded_diagnostic_capture(stderr_capture, "stderr");
		let after_hash = executable_hash(&self.executable)
			.map_err(|error| format!("failed to hash product executable after merge: {error}"))?;
		if after_hash != before_hash {
			return Err("product executable changed while measurement was running".to_string());
		}

		let stdout = redact_diagnostic_with_limit(
			&stdout,
			request,
			Some(run_root.path()),
			&self.executable,
			&self.base_data_root,
			self.cache.root.path(),
			MAX_PLAN_BYTES + 256,
		);
		let stderr = redact_diagnostic(
			&stderr,
			request,
			Some(run_root.path()),
			&self.executable,
			&self.base_data_root,
			self.cache.root.path(),
		);
		let cache_diagnostics = redact_diagnostic(
			&cache_diagnostics,
			request,
			Some(run_root.path()),
			&self.executable,
			&self.base_data_root,
			self.cache.root.path(),
		);
		Ok(ProductCommandResult {
			run_root,
			status,
			timed_out,
			merge_ms,
			stdout,
			stderr,
			cache_diagnostics,
		})
	}

	fn run_product_merge(
		&self,
		request: &MeasurementRequest,
		captured_cache_diagnostics: Option<&mut String>,
	) -> Result<TerminalMerge, String> {
		let result = self.run_product_command(request, true)?;
		if let Some(captured) = captured_cache_diagnostics {
			captured.clone_from(&result.cache_diagnostics);
		}
		if result.timed_out {
			return Ok(TerminalMerge::TimedOut {
				detail: Some(with_diagnostic(
					format!("foch merge exceeded {} ms", request.timeout.as_millis()),
					&result.stderr,
				)),
			});
		}
		if let Some(signal) = exit_signal(&result.status) {
			return Ok(TerminalMerge::Crashed {
				detail: Some(with_diagnostic(
					format!("foch merge terminated by signal {signal}"),
					&result.stderr,
				)),
			});
		}

		let report_path = request.output_dir.join(MERGE_REPORT_ARTIFACT_PATH);
		let report = match fs::read(&report_path) {
			Ok(bytes) => match serde_json::from_slice::<MergeReport>(&bytes) {
				Ok(report) => Some(report),
				Err(error) => {
					return Ok(TerminalMerge::Fatal {
						detail: with_diagnostic(
							format!("product merge report is invalid JSON: {error}"),
							&result.stderr,
						),
					});
				}
			},
			Err(error) if error.kind() == io::ErrorKind::NotFound => None,
			Err(error) => {
				return Ok(TerminalMerge::Fatal {
					detail: with_diagnostic(
						format!("failed to read product merge report: {error}"),
						&result.stderr,
					),
				});
			}
		};
		if let Some(report) = report {
			if let Err(detail) = validate_product_attestation(&report, request, self.no_game_base) {
				return Ok(TerminalMerge::Fatal {
					detail: with_diagnostic(
						redact_diagnostic(
							&detail,
							request,
							Some(result.run_root.path()),
							&self.executable,
							&self.base_data_root,
							self.cache.root.path(),
						),
						&result.stderr,
					),
				});
			}
			if report.status == MergeReportStatus::Fatal {
				let reason = report
					.fatal_reason
					.as_deref()
					.unwrap_or("product merge report has fatal status");
				return Ok(TerminalMerge::Fatal {
					detail: with_diagnostic(
						redact_diagnostic(
							reason,
							request,
							Some(result.run_root.path()),
							&self.executable,
							&self.base_data_root,
							self.cache.root.path(),
						),
						&result.stderr,
					),
				});
			}
			if !is_publishable_product_status(report.status) {
				return Ok(TerminalMerge::MergeFailed {
					detail: with_diagnostic(
						format!(
							"product merge report is blocked with {} unresolved manual conflicts",
							report.manual_conflict_count
						),
						&result.stderr,
					),
				});
			}
			return Ok(TerminalMerge::Completed {
				report: Box::new(report),
				merge_ms: result.merge_ms,
			});
		}

		let code = result.status.code().map_or_else(
			|| "without an exit code".to_string(),
			|code| format!("with exit code {code}"),
		);
		if result.status.success() {
			Ok(TerminalMerge::Fatal {
				detail: with_diagnostic(
					"confirmed foch merge succeeded without writing a merge report".to_string(),
					&result.stderr,
				),
			})
		} else {
			Ok(TerminalMerge::MergeFailed {
				detail: with_diagnostic(format!("foch merge failed {code}"), &result.stderr),
			})
		}
	}

	pub(crate) fn run_cache_probe(
		&self,
		request: &MeasurementRequest,
	) -> ProductPreviewObservation {
		let result = self.run_product_command(request, false);
		let (failure, plan_output, cache_diagnostics) = match result {
			Ok(result) => (
				preview_failure(&result, request.timeout),
				result.stdout,
				result.cache_diagnostics,
			),
			Err(detail) => (Some(detail), String::new(), String::new()),
		};
		ProductPreviewObservation {
			failure: failure.map(|detail| {
				redact_diagnostic(
					&detail,
					request,
					None,
					&self.executable,
					&self.base_data_root,
					self.cache.root.path(),
				)
			}),
			plan_output,
			cache_diagnostics,
			output_exists: request.output_dir.exists(),
			report_exists: request.output_dir.join(MERGE_REPORT_ARTIFACT_PATH).exists(),
		}
	}
}

impl MeasurementRunner for ProductMeasurementRunner {
	fn identity(&self) -> &MeasurementRunnerIdentity {
		&self.identity
	}

	fn preflight(&self) -> Result<(), String> {
		let actual = executable_hash(&self.executable)
			.map_err(|_| "failed to verify product executable before cohort".to_string())?;
		if actual != self.identity.engine_artifact.hash {
			return Err("product executable changed after runner identity was created".to_string());
		}
		Ok(())
	}

	fn run(&mut self, request: &MeasurementRequest) -> TerminalMerge {
		let mut cache_diagnostics = String::new();
		let capture = (self.terminal_failure_mode == TerminalFailureMode::Panic)
			.then_some(&mut cache_diagnostics);
		let terminal = self
			.run_product_merge(request, capture)
			.unwrap_or_else(|detail| TerminalMerge::Fatal { detail });
		let terminal = redact_terminal_merge(terminal, request, self);
		let terminal = enforce_terminal_failure_mode(terminal, self.terminal_failure_mode);
		if self.terminal_failure_mode == TerminalFailureMode::Panic {
			validate_cohort_cache_diagnostics(
				&mut self.cohort_cold_built_mod_ids,
				request,
				&cache_diagnostics,
			)
			.unwrap_or_else(|detail| {
				panic!("full-product acceptance cache-residency invariant failed: {detail}")
			});
		}
		terminal
	}
}

fn is_publishable_product_status(status: MergeReportStatus) -> bool {
	matches!(
		status,
		MergeReportStatus::Ready | MergeReportStatus::PartialSuccess
	)
}

fn preview_failure(result: &ProductCommandResult, timeout: Duration) -> Option<String> {
	if result.timed_out {
		return Some(with_diagnostic(
			format!("foch merge preview exceeded {} ms", timeout.as_millis()),
			&result.stderr,
		));
	}
	if let Some(signal) = exit_signal(&result.status) {
		return Some(with_diagnostic(
			format!("foch merge preview terminated by signal {signal}"),
			&result.stderr,
		));
	}
	if !result.status.success() {
		let code = result.status.code().map_or_else(
			|| "without an exit code".to_string(),
			|code| format!("with exit code {code}"),
		);
		return Some(with_diagnostic(
			format!("foch merge preview failed {code}"),
			&result.stderr,
		));
	}
	None
}

#[derive(Debug, Eq, PartialEq)]
struct CompletedCacheDiagnostics {
	started_mod_ids: Vec<String>,
	parsed_mod_ids: Vec<String>,
	stored_mod_ids: Vec<String>,
	disk_hit_mod_ids: Vec<String>,
}

fn parse_completed_cache_diagnostics(
	diagnostics: &str,
) -> Result<CompletedCacheDiagnostics, String> {
	if diagnostics.trim().is_empty() {
		return Err("cache diagnostics are missing".to_string());
	}
	if diagnostics.contains("truncated")
		|| diagnostics.contains("unavailable")
		|| diagnostics.contains("capture panicked")
	{
		return Err("cache diagnostics are incomplete".to_string());
	}
	let lines = diagnostics.lines().collect::<Vec<_>>();

	let workspace_summaries = lines
		.iter()
		.copied()
		.filter(|line| line.starts_with("[merge] resolve_workspace: done "))
		.collect::<Vec<_>>();
	let [workspace_summary] = workspace_summaries.as_slice() else {
		return Err("expected exactly one completed workspace cache summary".to_string());
	};
	let expected_hits = diagnostic_usize_field(workspace_summary, "mod_parse_cache_hits")?;
	let expected_misses = diagnostic_usize_field(workspace_summary, "mod_parse_cache_misses")?;
	let expected_mods = diagnostic_usize_field(workspace_summary, "mods")?;
	let started_mod_ids = diagnostic_mod_ids(&lines, "[merge] mod_snapshot: start ")?;
	let parsed_mod_ids = diagnostic_mod_ids(&lines, "[merge] mod_snapshot: parse_done ")?;
	let cache_store_lines = lines
		.iter()
		.copied()
		.filter(|line| line.starts_with("[merge] mod_snapshot: cache_store "))
		.collect::<Vec<_>>();
	let mut stored_mod_ids = Vec::with_capacity(cache_store_lines.len());
	for line in cache_store_lines {
		let state = diagnostic_field(line, "state")?;
		if state != "stored" {
			return Err(format!(
				"semantic snapshot cache store was not resident: state={state}"
			));
		}
		stored_mod_ids.push(diagnostic_field(line, "mod_id")?.to_string());
	}
	let cache_hit_lines = lines
		.iter()
		.copied()
		.filter(|line| line.starts_with("[merge] mod_snapshot: cache_hit "))
		.collect::<Vec<_>>();
	let mut disk_hit_mod_ids = Vec::with_capacity(cache_hit_lines.len());
	for line in cache_hit_lines {
		if diagnostic_field(line, "source")? != "disk" {
			return Err("full-product child reported a non-disk semantic cache hit".to_string());
		}
		disk_hit_mod_ids.push(diagnostic_field(line, "mod_id")?.to_string());
	}

	ensure_unique_mod_ids("start", &started_mod_ids)?;
	ensure_unique_mod_ids("parse_done", &parsed_mod_ids)?;
	ensure_unique_mod_ids("cache_store", &stored_mod_ids)?;
	ensure_unique_mod_ids("disk cache hit", &disk_hit_mod_ids)?;
	if started_mod_ids.len() != expected_mods
		|| parsed_mod_ids.len() != expected_misses
		|| stored_mod_ids.len() != expected_misses
		|| disk_hit_mod_ids.len() != expected_hits
		|| expected_hits + expected_misses != expected_mods
	{
		return Err("workspace cache summary does not match per-mod diagnostics".to_string());
	}
	let started = started_mod_ids.iter().cloned().collect::<BTreeSet<_>>();
	let observed = parsed_mod_ids
		.iter()
		.chain(&disk_hit_mod_ids)
		.cloned()
		.collect::<BTreeSet<_>>();
	if started != observed {
		return Err("per-mod cache outcomes do not match started mods".to_string());
	}
	if parsed_mod_ids.iter().cloned().collect::<BTreeSet<_>>()
		!= stored_mod_ids.iter().cloned().collect::<BTreeSet<_>>()
	{
		return Err("parsed semantic snapshots were not all stored resident".to_string());
	}
	Ok(CompletedCacheDiagnostics {
		started_mod_ids,
		parsed_mod_ids,
		stored_mod_ids,
		disk_hit_mod_ids,
	})
}

fn validate_cohort_cache_diagnostics(
	cold_built_mod_ids: &mut BTreeSet<String>,
	request: &MeasurementRequest,
	diagnostics: &str,
) -> Result<(), String> {
	let parsed = parse_completed_cache_diagnostics(diagnostics)?;
	let expected = request
		.case
		.referenced_mods
		.iter()
		.cloned()
		.collect::<BTreeSet<_>>();
	if expected.len() != request.case.referenced_mods.len() {
		return Err("measurement request contains duplicate source mod IDs".to_string());
	}
	if parsed
		.started_mod_ids
		.iter()
		.cloned()
		.collect::<BTreeSet<_>>()
		!= expected
	{
		return Err("cache diagnostics covered the wrong source mod IDs".to_string());
	}
	record_cold_builds(cold_built_mod_ids, &parsed.parsed_mod_ids)
}

fn record_cold_builds(
	cold_built_mod_ids: &mut BTreeSet<String>,
	parsed_mod_ids: &[String],
) -> Result<(), String> {
	if let Some(repeated) = parsed_mod_ids
		.iter()
		.find(|mod_id| cold_built_mod_ids.contains(*mod_id))
	{
		return Err(format!(
			"semantic snapshot for mod_id={repeated} was cold-built more than once in the cohort"
		));
	}
	cold_built_mod_ids.extend(parsed_mod_ids.iter().cloned());
	Ok(())
}

fn diagnostic_mod_ids(lines: &[&str], prefix: &str) -> Result<Vec<String>, String> {
	lines
		.iter()
		.copied()
		.filter(|line| line.starts_with(prefix))
		.map(|line| diagnostic_field(line, "mod_id").map(str::to_string))
		.collect()
}

fn diagnostic_usize_field(line: &str, key: &str) -> Result<usize, String> {
	diagnostic_field(line, key)?
		.parse::<usize>()
		.map_err(|_| format!("cache diagnostic field {key} is not an integer"))
}

fn diagnostic_field<'a>(line: &'a str, key: &str) -> Result<&'a str, String> {
	let prefix = format!("{key}=");
	line.split_ascii_whitespace()
		.find_map(|field| field.strip_prefix(&prefix))
		.ok_or_else(|| format!("cache diagnostic is missing field {key}"))
}

fn ensure_unique_mod_ids(label: &str, mod_ids: &[String]) -> Result<(), String> {
	if mod_ids.iter().collect::<BTreeSet<_>>().len() == mod_ids.len() {
		Ok(())
	} else {
		Err(format!("duplicate mod_id in {label} cache diagnostics"))
	}
}

fn enforce_terminal_failure_mode(
	terminal: TerminalMerge,
	mode: TerminalFailureMode,
) -> TerminalMerge {
	if mode == TerminalFailureMode::Panic {
		match &terminal {
			TerminalMerge::MergeFailed { detail } => {
				panic!("full-product acceptance aborted on first merge_failed: {detail}")
			}
			TerminalMerge::Crashed { detail } => panic!(
				"full-product acceptance aborted on first crashed: {}",
				detail.as_deref().unwrap_or("no diagnostic")
			),
			TerminalMerge::TimedOut { detail } => panic!(
				"full-product acceptance aborted on first timed_out: {}",
				detail.as_deref().unwrap_or("no diagnostic")
			),
			TerminalMerge::Fatal { detail } => {
				panic!("full-product acceptance aborted on first fatal: {detail}")
			}
			TerminalMerge::Completed { .. } => {}
		}
	}
	terminal
}

fn redact_terminal_merge(
	terminal: TerminalMerge,
	request: &MeasurementRequest,
	runner: &ProductMeasurementRunner,
) -> TerminalMerge {
	let redact = |value: String| {
		redact_diagnostic(
			&value,
			request,
			None,
			&runner.executable,
			&runner.base_data_root,
			runner.cache.root.path(),
		)
	};
	match terminal {
		TerminalMerge::MergeFailed { detail } => TerminalMerge::MergeFailed {
			detail: redact(detail),
		},
		TerminalMerge::Crashed { detail } => TerminalMerge::Crashed {
			detail: detail.map(redact),
		},
		TerminalMerge::TimedOut { detail } => TerminalMerge::TimedOut {
			detail: detail.map(redact),
		},
		TerminalMerge::Fatal { detail } => TerminalMerge::Fatal {
			detail: redact(detail),
		},
		completed @ TerminalMerge::Completed { .. } => completed,
	}
}

fn validate_product_attestation(
	report: &MergeReport,
	request: &MeasurementRequest,
	no_game_base: bool,
) -> Result<(), String> {
	let execution = report
		.execution
		.as_ref()
		.ok_or_else(|| "product merge report has no execution attestation".to_string())?;
	if execution.schema != MERGE_EXECUTION_ATTESTATION_SCHEMA {
		return Err(format!(
			"product merge report attestation schema mismatch: expected {}, found {}",
			MERGE_EXECUTION_ATTESTATION_SCHEMA, execution.schema
		));
	}
	if execution.kernel != MergeReportKernel::SemanticTree {
		return Err("product merge report did not attest the semantic-tree kernel".to_string());
	}
	if execution.scope != MergeReportScope::FullProductMerge {
		return Err("product merge report did not attest full-product scope".to_string());
	}
	match (&execution.base_snapshot, no_game_base) {
		(MergeReportBaseSnapshot::Disabled, true)
			if request.expected_base_snapshot_identity == DISABLED_BASE_SNAPSHOT_IDENTITY =>
		{
			Ok(())
		}
		(MergeReportBaseSnapshot::Resolved { identity }, false)
			if identity == &request.expected_base_snapshot_identity =>
		{
			Ok(())
		}
		(MergeReportBaseSnapshot::Resolved { identity }, false) => Err(format!(
			"product merge report base snapshot identity mismatch: expected {}, found {}",
			request.expected_base_snapshot_identity, identity
		)),
		(MergeReportBaseSnapshot::Unavailable, false) => {
			Err("product merge report could not attest a base snapshot".to_string())
		}
		_ => Err("product merge report base snapshot mode mismatch".to_string()),
	}?;

	let Some(expected_manifest) = request.source_manifest.as_ref() else {
		return if report.input.is_none() {
			Ok(())
		} else {
			Err("local fixture unexpectedly produced a Workshop input attestation".to_string())
		};
	};
	if !expected_manifest.digest_is_valid() {
		return Err("measurement request has an invalid Workshop input manifest".to_string());
	}
	let actual = report
		.input
		.as_ref()
		.ok_or_else(|| "product merge report has no Workshop input attestation".to_string())?;
	if actual.schema != PRODUCT_INPUT_MANIFEST_SCHEMA
		|| actual.profile != PRODUCT_INPUT_PROFILE
		|| actual.digest_algorithm != PRODUCT_INPUT_DIGEST_ALGORITHM
	{
		return Err("product merge report has an unsupported input profile".to_string());
	}
	let expected = expected_manifest.attestation();
	if actual != &expected {
		return Err(format!(
			"product merge input attestation mismatch: expected {}, found {}",
			expected.digest, actual.digest
		));
	}
	Ok(())
}

fn validate_request(request: &MeasurementRequest) -> Result<(), String> {
	if request.case.referenced_mods.len() != request.source_dirs.len() {
		return Err(format!(
			"case declares {} source mods but {} roots were supplied",
			request.case.referenced_mods.len(),
			request.source_dirs.len()
		));
	}
	if let Some(manifest) = request.source_manifest.as_ref() {
		if !manifest.digest_is_valid() {
			return Err("source manifest digest is invalid".to_string());
		}
		if manifest.mods.len() != request.case.referenced_mods.len() {
			return Err("source manifest does not match the measurement topology".to_string());
		}
		for (index, (expected_id, input)) in request
			.case
			.referenced_mods
			.iter()
			.zip(&manifest.mods)
			.enumerate()
		{
			if input.mod_id != *expected_id
				|| input.precedence != index + 1
				|| input.workshop_identity.workshop_id.as_str() != expected_id
			{
				return Err(format!(
					"source manifest entry {} does not match Workshop source {expected_id}",
					index + 1
				));
			}
		}
	}
	for (label, path) in std::iter::once(("compatch", &request.compatch_dir))
		.chain(std::iter::once(("base game", &request.basegame_root)))
		.chain(request.source_dirs.iter().map(|path| ("source mod", path)))
	{
		if !path.is_absolute() {
			return Err(format!("{label} root must be absolute"));
		}
		if !path.is_dir() {
			return Err(format!("{label} root is unavailable"));
		}
	}
	Ok(())
}

fn validate_base_snapshot(request: &MeasurementRequest) -> Result<(), String> {
	let version = foch_merge_quality::config::detect_game_version(&request.basegame_root)
		.ok_or_else(|| "failed to detect measurement base-game version".to_string())?;
	let installed = foch_engine::installed_base_snapshot_identity("eu4", &version)?
		.ok_or_else(|| format!("no installed base snapshot for eu4@{version}"))?;
	let actual = installed.as_label();
	if actual != request.expected_base_snapshot_identity {
		return Err("installed base snapshot changed after measurement selection".to_string());
	}
	Ok(())
}

fn write_workspace_manifest(path: &Path, request: &MeasurementRequest) -> Result<(), String> {
	let mods = request
		.case
		.referenced_mods
		.iter()
		.zip(&request.source_dirs)
		.enumerate()
		.map(|(position, (id, root))| {
			let root = root
				.canonicalize()
				.map_err(|error| format!("failed to resolve immutable source root: {error}"))?;
			Ok(WorkspaceMod {
				id: Some(id.clone()),
				path: Some(root),
				steam_id: Some(id.clone()),
				workshop_identity: request
					.source_manifest
					.as_ref()
					.map(|manifest| manifest.mods[position].workshop_identity.clone()),
				enabled: true,
				position: Some(position),
			})
		})
		.collect::<Result<Vec<_>, String>>()?;
	let manifest = FochConfig {
		workspace: Some(WorkspaceConfig {
			game: Some(Game::EuropaUniversalis4),
			game_path: Some(
				request
					.basegame_root
					.canonicalize()
					.map_err(|error| format!("failed to resolve base-game root: {error}"))?,
			),
			paradox_data_path: None,
			imports: Vec::new(),
			mods,
		}),
		..FochConfig::default()
	};
	let encoded = toml::to_string_pretty(&manifest)
		.map_err(|error| format!("failed to encode product workspace manifest: {error}"))?;
	fs::write(path, encoded)
		.map_err(|error| format!("failed to write product workspace manifest: {error}"))
}

fn write_engine_config(
	config_dir: &Path,
	request: &MeasurementRequest,
	run_root: &Path,
) -> Result<(), String> {
	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), request.basegame_root.clone());
	let config = foch_engine::Config {
		steam_root_path: Some(run_root.join("unavailable-steam-root")),
		paradox_data_path: None,
		game_path,
		extra_ignore_patterns: Vec::new(),
	};
	config
		.save_config(&config_dir.join("config.toml"))
		.map_err(|error| format!("failed to write isolated engine config: {error}"))
}

fn spawn_bounded_prefix_capture(
	mut reader: impl Read + Send + 'static,
) -> thread::JoinHandle<io::Result<(Vec<u8>, bool)>> {
	thread::spawn(move || {
		let capacity = usize::try_from(MAX_PLAN_BYTES).unwrap_or(usize::MAX);
		let mut retained = Vec::with_capacity(capacity);
		let mut buffer = [0_u8; 8192];
		let mut truncated = false;
		loop {
			let read = reader.read(&mut buffer)?;
			if read == 0 {
				break;
			}
			let remaining = capacity.saturating_sub(retained.len());
			retained.extend_from_slice(&buffer[..read.min(remaining)]);
			if read > remaining {
				truncated = true;
			}
		}
		Ok((retained, truncated))
	})
}

fn finish_bounded_prefix_capture(
	capture: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
	stream: &str,
) -> String {
	match capture.join() {
		Ok(Ok((bytes, truncated))) => {
			let body = String::from_utf8_lossy(&bytes);
			if truncated {
				format!("{body}\n<{stream} truncated after first {MAX_PLAN_BYTES} bytes>")
			} else {
				body.into_owned()
			}
		}
		Ok(Err(error)) => format!("<{stream} unavailable: {error}>"),
		Err(_) => format!("<{stream} capture panicked>"),
	}
}

struct BoundedDiagnosticCapture {
	tail: Vec<u8>,
	tail_truncated: bool,
	cache_diagnostics: Vec<u8>,
	cache_diagnostics_truncated: bool,
}

fn spawn_bounded_diagnostic_capture(
	reader: impl Read + Send + 'static,
) -> thread::JoinHandle<io::Result<BoundedDiagnosticCapture>> {
	thread::spawn(move || {
		use std::io::BufRead;

		let capacity = usize::try_from(MAX_DIAGNOSTIC_BYTES).unwrap_or(usize::MAX);
		let mut reader = io::BufReader::new(reader);
		let mut tail = Vec::with_capacity(capacity);
		let mut cache_diagnostics = Vec::new();
		let mut line = Vec::new();
		let mut tail_truncated = false;
		let mut cache_diagnostics_truncated = false;
		loop {
			line.clear();
			let read = reader.read_until(b'\n', &mut line)?;
			if read == 0 {
				break;
			}
			tail.extend_from_slice(&line);
			if tail.len() > capacity {
				let excess = tail.len() - capacity;
				tail.drain(..excess);
				tail_truncated = true;
			}
			if CACHE_DIAGNOSTIC_PREFIXES
				.iter()
				.any(|prefix| line.starts_with(prefix))
			{
				let remaining = capacity.saturating_sub(cache_diagnostics.len());
				if line.len() <= remaining {
					cache_diagnostics.extend_from_slice(&line);
				} else {
					cache_diagnostics.extend_from_slice(&line[..remaining]);
					cache_diagnostics_truncated = true;
				}
			}
		}
		Ok(BoundedDiagnosticCapture {
			tail,
			tail_truncated,
			cache_diagnostics,
			cache_diagnostics_truncated,
		})
	})
}

fn finish_bounded_diagnostic_capture(
	capture: thread::JoinHandle<io::Result<BoundedDiagnosticCapture>>,
	stream: &str,
) -> (String, String) {
	match capture.join() {
		Ok(Ok(captured)) => {
			let tail = String::from_utf8_lossy(&captured.tail);
			let tail = if captured.tail_truncated {
				format!("<{stream} truncated to last {MAX_DIAGNOSTIC_BYTES} bytes>\n{tail}")
			} else {
				tail.into_owned()
			};
			let cache_diagnostics = String::from_utf8_lossy(&captured.cache_diagnostics);
			let cache_diagnostics = if captured.cache_diagnostics_truncated {
				format!(
					"{cache_diagnostics}\n<cache diagnostics truncated at {MAX_DIAGNOSTIC_BYTES} bytes>"
				)
			} else {
				cache_diagnostics.into_owned()
			};
			(tail, cache_diagnostics)
		}
		Ok(Err(error)) => {
			let unavailable = format!("<{stream} unavailable: {error}>");
			(unavailable.clone(), unavailable)
		}
		Err(_) => {
			let unavailable = format!("<{stream} capture panicked>");
			(unavailable.clone(), unavailable)
		}
	}
}

fn redact_diagnostic(
	diagnostic: &str,
	request: &MeasurementRequest,
	run_root: Option<&Path>,
	executable: &Path,
	base_data_root: &Path,
	cache_root: &Path,
) -> String {
	redact_diagnostic_with_limit(
		diagnostic,
		request,
		run_root,
		executable,
		base_data_root,
		cache_root,
		MAX_DIAGNOSTIC_BYTES,
	)
}

fn redact_diagnostic_with_limit(
	diagnostic: &str,
	request: &MeasurementRequest,
	run_root: Option<&Path>,
	executable: &Path,
	base_data_root: &Path,
	cache_root: &Path,
	max_bytes: u64,
) -> String {
	let mut replacements = Vec::<(PathBuf, &'static str)>::new();
	push_redaction_paths(&mut replacements, executable, "<foch-executable>");
	if let Some(run_root) = run_root {
		push_redaction_paths(&mut replacements, run_root, "<runner-root>");
	}
	push_redaction_paths(&mut replacements, &request.output_dir, "<output-root>");
	push_redaction_paths(&mut replacements, &request.compatch_dir, "<compatch-root>");
	push_redaction_paths(&mut replacements, &request.basegame_root, "<basegame-root>");
	push_redaction_paths(&mut replacements, base_data_root, "<base-data-root>");
	push_redaction_paths(&mut replacements, cache_root, "<cache-root>");
	for source in &request.source_dirs {
		push_redaction_paths(&mut replacements, source, "<source-root>");
	}
	for path in std::iter::once(&request.compatch_dir).chain(&request.source_dirs) {
		let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
		for ancestor in resolved.ancestors() {
			match ancestor.file_name().and_then(|name| name.to_str()) {
				Some("objects") => {
					push_redaction_paths(&mut replacements, ancestor, "<dataset-objects-root>");
					if let Some(dataset_root) = ancestor.parent() {
						push_redaction_paths(&mut replacements, dataset_root, "<dataset-root>");
					}
					break;
				}
				Some("work") => {
					push_redaction_paths(&mut replacements, ancestor, "<dataset-work-root>");
					if let Some(dataset_root) = ancestor.parent() {
						push_redaction_paths(&mut replacements, dataset_root, "<dataset-root>");
					}
					break;
				}
				_ => {}
			}
		}
	}
	replacements.sort_by_key(|(path, _)| std::cmp::Reverse(path.as_os_str().len()));
	replacements.dedup_by(|left, right| left.0 == right.0);
	let mut redacted = diagnostic.to_string();
	for (path, replacement) in replacements {
		let raw = path.to_string_lossy();
		redacted = redacted.replace(raw.as_ref(), replacement);
		let normalized = raw.replace('\\', "/");
		if normalized != raw {
			redacted = redacted.replace(&normalized, replacement);
		}
	}
	truncate_utf8(redacted, usize::try_from(max_bytes).unwrap_or(usize::MAX))
}

fn push_redaction_paths(
	replacements: &mut Vec<(PathBuf, &'static str)>,
	path: &Path,
	replacement: &'static str,
) {
	if path.as_os_str().is_empty() {
		return;
	}
	replacements.push((path.to_path_buf(), replacement));
	if let Ok(canonical) = path.canonicalize()
		&& canonical != path
	{
		replacements.push((canonical, replacement));
	}
}

fn truncate_utf8(mut text: String, max_bytes: usize) -> String {
	if text.len() <= max_bytes {
		return text;
	}
	let mut boundary = max_bytes;
	while !text.is_char_boundary(boundary) {
		boundary -= 1;
	}
	text.truncate(boundary);
	text.push_str("\n<diagnostic truncated>");
	text
}

fn with_diagnostic(message: String, diagnostic: &str) -> String {
	if diagnostic.trim().is_empty() {
		message
	} else {
		format!("{message}; stderr:\n{}", diagnostic.trim())
	}
}

#[cfg(unix)]
fn exit_signal(status: &ExitStatus) -> Option<i32> {
	use std::os::unix::process::ExitStatusExt;

	status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &ExitStatus) -> Option<i32> {
	None
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::OsStr;

	fn command_env<'a>(command: &'a Command, key: &str) -> Option<&'a OsStr> {
		for (name, value) in command.get_envs() {
			if name == OsStr::new(key) {
				return value;
			}
		}
		None
	}

	#[test]
	fn full_product_and_fixture_runners_use_distinct_cache_lifetimes() {
		let system_root = foch_engine::default_foch_cache_dir();
		let inherited_max_bytes = std::env::var_os(CACHE_MAX_BYTES_ENV);
		let full_product = ProductMeasurementRunner::full_product().expect("full-product runner");
		assert!(matches!(
			&full_product.cache.root,
			ProductCacheRoot::SystemManaged { .. }
		));
		assert_eq!(full_product.cache.root.path(), system_root);
		assert_eq!(
			full_product.cache.max_bytes.as_deref(),
			inherited_max_bytes.as_deref()
		);
		assert_eq!(
			full_product.terminal_failure_mode,
			TerminalFailureMode::Return
		);
		let mut full_product_command = Command::new(&full_product.executable);
		full_product_command.env_clear();
		full_product.cache.apply_to(&mut full_product_command);
		assert_eq!(
			command_env(&full_product_command, foch_core::cache::CACHE_ROOT_ENV),
			Some(system_root.as_os_str())
		);
		assert_eq!(
			command_env(&full_product_command, CACHE_MAX_BYTES_ENV),
			inherited_max_bytes.as_deref()
		);
		let fail_fast =
			ProductMeasurementRunner::full_product_fail_fast_for(None).expect("fail-fast runner");
		assert_eq!(fail_fast.terminal_failure_mode, TerminalFailureMode::Panic);
		assert!(matches!(
			&fail_fast.cache.root,
			ProductCacheRoot::RunnerScoped { .. }
		));
		assert_eq!(fail_fast.cache.max_bytes, None);
		let fail_fast_root = fail_fast.cache.root.path().to_path_buf();
		assert_ne!(fail_fast_root, system_root);
		assert!(fail_fast_root.is_dir());
		drop(fail_fast);
		assert!(!fail_fast_root.exists());

		let fixture = ProductMeasurementRunner::no_game_base_fixture().expect("fixture runner");
		assert!(matches!(
			&fixture.cache.root,
			ProductCacheRoot::RunnerScoped { .. }
		));
		let fixture_root = fixture.cache.root.path().to_path_buf();
		assert_ne!(fixture_root, system_root);
		assert!(fixture_root.is_dir());
		assert_eq!(
			fixture.cache.max_bytes.as_deref(),
			inherited_max_bytes.as_deref()
		);

		drop(fixture);
		assert!(!fixture_root.exists());
	}

	#[test]
	fn cache_gate_rejects_an_override_and_applies_no_cap() {
		let Err(error) = ProductMeasurementRunner::full_product_fail_fast_for(Some(
			OsString::from("1073741825"),
		)) else {
			panic!("fail-fast acceptance runner must reject every cap override");
		};
		assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

		let Err(error) = ProductCacheEnvironment::runner_scoped_default_cap_for(Some(
			OsString::from("1073741825"),
		)) else {
			panic!("cache gate must reject every cap override");
		};
		assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
		assert!(error.to_string().contains(CACHE_MAX_BYTES_ENV));

		let cache = ProductCacheEnvironment::runner_scoped_default_cap_for(None)
			.expect("default-cap cache environment");
		let mut command = Command::new(env!("CARGO_BIN_EXE_foch"));
		command.env_clear();
		cache.apply_to(&mut command);
		assert_eq!(
			command_env(&command, foch_core::cache::CACHE_ROOT_ENV),
			Some(cache.root.path().as_os_str())
		);
		assert_eq!(command_env(&command, CACHE_MAX_BYTES_ENV), None);
	}

	#[test]
	fn command_cache_environment_reuses_captured_root_and_optional_cap() {
		let mut runner = ProductMeasurementRunner::no_game_base_fixture().expect("fixture runner");
		let stable_root = runner.cache.root.path().to_path_buf();
		runner.cache.max_bytes = Some(OsString::from("12345"));

		for _ in 0..2 {
			let mut command = Command::new(&runner.executable);
			command.env_clear();
			runner.cache.apply_to(&mut command);
			assert_eq!(
				command_env(&command, foch_core::cache::CACHE_ROOT_ENV),
				Some(stable_root.as_os_str())
			);
			assert_eq!(
				command_env(&command, CACHE_MAX_BYTES_ENV),
				Some(OsStr::new("12345"))
			);
		}

		runner.cache.max_bytes = None;
		let mut default_capped = Command::new(&runner.executable);
		default_capped.env_clear();
		runner.cache.apply_to(&mut default_capped);
		assert_eq!(command_env(&default_capped, CACHE_MAX_BYTES_ENV), None);
	}

	#[test]
	fn diagnostics_are_bounded_and_hide_local_roots() {
		let root = tempfile::tempdir().expect("fixture root");
		let dataset_root = root.path().join("private-dataset");
		let objects_root = dataset_root.join("objects");
		let compatch_root = objects_root.join("aa/hash/tree");
		let source_root = objects_root.join("bb/hash/tree");
		let basegame_root = root.path().join("private-base");
		let base_data_root = root.path().join("private-base-data");
		let cache_root = root.path().join("private-cache");
		for path in [
			&compatch_root,
			&source_root,
			&basegame_root,
			&base_data_root,
			&cache_root,
		] {
			fs::create_dir_all(path).expect("create private fixture path");
		}
		let request = MeasurementRequest {
			input_version_id: "fixture-input".to_string(),
			case: Case {
				compatch_id: "compatch".to_string(),
				title: "fixture".to_string(),
				referenced_mods: vec!["source".to_string()],
				..Case::default()
			},
			compatch_dir: compatch_root,
			source_dirs: vec![source_root],
			source_manifest: None,
			output_dir: root.path().join("private-output"),
			basegame_root,
			expected_base_snapshot_identity: "none".to_string(),
			timeout: Duration::from_secs(1),
		};
		let raw = format!(
			"{} {} {} {} {} {} {} {}",
			request.compatch_dir.display(),
			request.source_dirs[0].display(),
			request.output_dir.display(),
			dataset_root.display(),
			objects_root.display(),
			base_data_root.display(),
			cache_root.display(),
			"x".repeat(usize::try_from(MAX_DIAGNOSTIC_BYTES).unwrap() + 1),
		);

		let redacted = redact_diagnostic(
			&raw,
			&request,
			Some(root.path()),
			Path::new(env!("CARGO_BIN_EXE_foch")),
			&base_data_root,
			&cache_root,
		);

		assert!(!redacted.contains(&root.path().display().to_string()));
		assert!(redacted.contains("<compatch-root>"));
		assert!(redacted.contains("<cache-root>"));
		assert!(redacted.len() <= usize::try_from(MAX_DIAGNOSTIC_BYTES).unwrap() + 32);
	}

	#[test]
	fn process_capture_retains_only_the_bounded_prefix() {
		let capacity = usize::try_from(MAX_PLAN_BYTES).unwrap();
		let mut bytes = vec![b'a'; capacity];
		bytes.extend(vec![b'b'; 17]);
		let captured = finish_bounded_prefix_capture(
			spawn_bounded_prefix_capture(io::Cursor::new(bytes)),
			"stdout",
		);

		let expected_prefix = "a".repeat(capacity);
		assert!(captured.starts_with(expected_prefix.as_str()));
		assert!(captured.ends_with("<stdout truncated after first 4194304 bytes>"));
		assert!(!captured.contains(&"b".repeat(17)));
	}

	#[test]
	fn cache_diagnostic_capture_survives_a_truncated_stderr_tail() {
		let capacity = usize::try_from(MAX_DIAGNOSTIC_BYTES).unwrap();
		let mut bytes = b"[merge] mod_snapshot: cache_hit mod_id=42 source=disk\n".to_vec();
		bytes.extend(vec![b'x'; capacity + 17]);
		let (tail, cache_diagnostics) = finish_bounded_diagnostic_capture(
			spawn_bounded_diagnostic_capture(io::Cursor::new(bytes)),
			"stderr",
		);

		assert!(tail.starts_with("<stderr truncated to last"));
		assert!(!tail.contains("mod_id=42"));
		assert_eq!(
			cache_diagnostics,
			"[merge] mod_snapshot: cache_hit mod_id=42 source=disk\n"
		);
	}

	fn completed_cache_diagnostics(parsed: &[&str], disk_hits: &[&str]) -> String {
		let mut lines = Vec::new();
		for mod_id in parsed.iter().chain(disk_hits) {
			lines.push(format!(
				"[merge] mod_snapshot: start mod_id={mod_id} files=1"
			));
		}
		for mod_id in parsed {
			lines.push(format!(
				"[merge] mod_snapshot: parse_done mod_id={mod_id} elapsed_ms=1 cache_hits=0 cache_misses=1"
			));
			lines.push(format!(
				"[merge] mod_snapshot: cache_store mod_id={mod_id} state=stored elapsed_ms=1 total_ms=2 compressed_bytes=100 uncompressed_bytes=200"
			));
		}
		for mod_id in disk_hits {
			lines.push(format!(
				"[merge] mod_snapshot: cache_hit mod_id={mod_id} source=disk elapsed_ms=1"
			));
		}
		lines.push(format!(
			"[merge] resolve_workspace: done elapsed_ms=1 mods={} files=3 requested_paths=0 effective_paths=0 mod_parse_cache_hits={} mod_parse_cache_misses={}",
			parsed.len() + disk_hits.len(),
			disk_hits.len(),
			parsed.len()
		));
		format!("{}\n", lines.join("\n"))
	}

	#[test]
	fn completed_cache_diagnostic_parser_is_fail_closed() {
		let diagnostics = completed_cache_diagnostics(&["a", "b"], &["c"]);
		assert_eq!(
			parse_completed_cache_diagnostics(&diagnostics).expect("complete diagnostics"),
			CompletedCacheDiagnostics {
				started_mod_ids: vec!["a".to_string(), "b".to_string(), "c".to_string()],
				parsed_mod_ids: vec!["a".to_string(), "b".to_string()],
				stored_mod_ids: vec!["a".to_string(), "b".to_string()],
				disk_hit_mod_ids: vec!["c".to_string()],
			}
		);
		assert!(parse_completed_cache_diagnostics("").is_err());
		assert!(
			parse_completed_cache_diagnostics(&format!(
				"{diagnostics}<cache diagnostics truncated>"
			))
			.is_err()
		);
		assert!(
			parse_completed_cache_diagnostics(
				&diagnostics.replace("source=disk", "source=process")
			)
			.is_err()
		);
		assert!(
			parse_completed_cache_diagnostics(&completed_cache_diagnostics(&["a", "a"], &[]))
				.is_err()
		);
		assert!(
			parse_completed_cache_diagnostics(
				&diagnostics.replace("state=stored", "state=rejected_too_large")
			)
			.is_err()
		);
		assert!(
			parse_completed_cache_diagnostics(&diagnostics.replace("state=stored", "state=error"))
				.is_err()
		);
		let missing_store = diagnostics
			.lines()
			.filter(|line| {
				!(line.starts_with("[merge] mod_snapshot: cache_store ")
					&& line.contains("mod_id=a"))
			})
			.collect::<Vec<_>>()
			.join("\n");
		assert!(parse_completed_cache_diagnostics(&missing_store).is_err());
	}

	#[test]
	fn cohort_cold_build_detector_rejects_only_a_second_parse() {
		let mut cold_built = BTreeSet::new();
		record_cold_builds(&mut cold_built, &["a".to_string(), "b".to_string()])
			.expect("first cold builds");
		record_cold_builds(&mut cold_built, &[]).expect("disk hits are not recorded");
		record_cold_builds(&mut cold_built, &["c".to_string()]).expect("new cold build");
		let error = record_cold_builds(&mut cold_built, &["a".to_string()])
			.expect_err("second cold build must fail");
		assert!(error.contains("mod_id=a"));
		assert_eq!(
			cold_built,
			["a".to_string(), "b".to_string(), "c".to_string()]
				.into_iter()
				.collect()
		);
	}

	#[test]
	fn fail_fast_mode_panics_only_for_terminal_process_failures() {
		let completed = TerminalMerge::Completed {
			report: Box::new(MergeReport::default()),
			merge_ms: 1,
		};
		assert!(matches!(
			enforce_terminal_failure_mode(completed, TerminalFailureMode::Panic),
			TerminalMerge::Completed { .. }
		));
		assert!(matches!(
			enforce_terminal_failure_mode(
				TerminalMerge::TimedOut { detail: None },
				TerminalFailureMode::Return,
			),
			TerminalMerge::TimedOut { .. }
		));

		for terminal in [
			TerminalMerge::MergeFailed {
				detail: "failed".to_string(),
			},
			TerminalMerge::Crashed { detail: None },
			TerminalMerge::TimedOut { detail: None },
			TerminalMerge::Fatal {
				detail: "fatal".to_string(),
			},
		] {
			assert!(
				std::panic::catch_unwind(|| {
					enforce_terminal_failure_mode(terminal, TerminalFailureMode::Panic)
				})
				.is_err()
			);
		}
	}

	#[test]
	fn only_ready_or_partial_success_reports_are_publishable() {
		assert!(is_publishable_product_status(MergeReportStatus::Ready));
		assert!(is_publishable_product_status(
			MergeReportStatus::PartialSuccess
		));
		assert!(!is_publishable_product_status(MergeReportStatus::Blocked));
		assert!(!is_publishable_product_status(MergeReportStatus::Fatal));
	}

	#[test]
	fn cohort_preflight_rejects_changed_product_identity_without_leaking_path() {
		let mut runner = ProductMeasurementRunner::no_game_base_fixture().expect("runner");
		runner.identity.engine_artifact.hash = "0".repeat(64);
		let error = runner.preflight().expect_err("identity mismatch must fail");

		assert_eq!(
			error,
			"product executable changed after runner identity was created"
		);
		assert!(!error.contains(&runner.executable.display().to_string()));
	}

	#[test]
	fn product_attestation_requires_exact_execution_and_input_identity() {
		let mut report = MergeReport {
			execution: Some(foch_core::model::MergeExecutionAttestation {
				schema: MERGE_EXECUTION_ATTESTATION_SCHEMA.to_string(),
				kernel: MergeReportKernel::SemanticTree,
				scope: MergeReportScope::FullProductMerge,
				base_snapshot: MergeReportBaseSnapshot::Resolved {
					identity: "base-identity".to_string(),
				},
			}),
			input: Some(ProductInputManifest::new(Vec::new()).attestation()),
			..MergeReport::default()
		};
		let request = MeasurementRequest {
			input_version_id: "fixture-input".to_string(),
			case: Case::default(),
			compatch_dir: PathBuf::from("/compatch"),
			source_dirs: Vec::new(),
			source_manifest: Some(ProductInputManifest::new(Vec::new())),
			output_dir: PathBuf::from("/output"),
			basegame_root: PathBuf::from("/base"),
			expected_base_snapshot_identity: "base-identity".to_string(),
			timeout: Duration::from_secs(1),
		};

		validate_product_attestation(&report, &request, false)
			.expect("matching product attestation");
		report.execution.as_mut().unwrap().scope = MergeReportScope::RetainedPathEvaluation;
		assert!(validate_product_attestation(&report, &request, false).is_err());
		report.execution.as_mut().unwrap().scope = MergeReportScope::FullProductMerge;
		report.execution.as_mut().unwrap().kernel = MergeReportKernel::AddressPatchReference;
		assert!(validate_product_attestation(&report, &request, false).is_err());
		report.execution.as_mut().unwrap().kernel = MergeReportKernel::SemanticTree;
		report.execution.as_mut().unwrap().base_snapshot = MergeReportBaseSnapshot::Resolved {
			identity: "other".to_string(),
		};
		assert!(validate_product_attestation(&report, &request, false).is_err());
		report.execution.as_mut().unwrap().base_snapshot = MergeReportBaseSnapshot::Resolved {
			identity: "base-identity".to_string(),
		};
		report.input.as_mut().unwrap().digest = "0".repeat(64);
		assert!(validate_product_attestation(&report, &request, false).is_err());
		report.input = None;
		assert!(validate_product_attestation(&report, &request, false).is_err());
	}

	#[test]
	fn product_attestation_uses_the_request_acf_manifest() {
		let root = tempfile::tempdir().expect("fixture root");
		let source_manifest = ProductInputManifest::new(vec![ProductInputMod {
			mod_id: "1001".to_string(),
			precedence: 1,
			workshop_identity: WorkshopInstallIdentity {
				app_id: 236_850,
				workshop_id: SteamId::new(1_001),
				manifest_id: SteamId::new(2_001),
			},
		}]);
		let report = MergeReport {
			execution: Some(foch_core::model::MergeExecutionAttestation {
				schema: MERGE_EXECUTION_ATTESTATION_SCHEMA.to_string(),
				kernel: MergeReportKernel::SemanticTree,
				scope: MergeReportScope::FullProductMerge,
				base_snapshot: MergeReportBaseSnapshot::Disabled,
			}),
			input: Some(source_manifest.attestation()),
			..MergeReport::default()
		};
		let request = MeasurementRequest {
			input_version_id: "fixture-input".to_string(),
			case: Case {
				referenced_mods: vec!["1001".to_string()],
				..Case::default()
			},
			compatch_dir: root.path().join("compatch"),
			source_dirs: vec![root.path().join("source")],
			source_manifest: Some(source_manifest),
			output_dir: root.path().join("output"),
			basegame_root: root.path().join("base"),
			expected_base_snapshot_identity: DISABLED_BASE_SNAPSHOT_IDENTITY.to_string(),
			timeout: Duration::from_secs(1),
		};

		validate_product_attestation(&report, &request, true)
			.expect("attestation uses ACF manifest");
	}
}
