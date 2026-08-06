use std::collections::{BTreeSet, HashMap};
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
};
use foch_merge_quality::corpus::Case;
use foch_merge_quality::dataset::{EngineArtifactIdentity, MeasurementKernel, MeasurementScope};
use foch_merge_quality::lifecycle::{
	MeasurementRequest, MeasurementRunner, MeasurementRunnerIdentity, TerminalMerge,
	executable_hash,
};
use foch_merge_quality::review_pack::{StructuredKernelRequest, StructuredKernelRunner};
use foch_merge_quality::shadow::{
	SHADOW_COMPARE_SCHEMA, ShadowCaptureRequest, ShadowDiagnostic, ShadowDiagnosticKind,
	ShadowInputManifest, ShadowRunRecord, capture_input_manifest,
	validate_shadow_manifest_identity,
};

pub const RUNNER_PROTOCOL_VERSION: &str = "foch-cli-merge-report-v2";
const MAX_DIAGNOSTIC_BYTES: u64 = 64 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const DISABLED_BASE_SNAPSHOT_IDENTITY: &str = "explicitly-disabled";

/// Executes every measurement through the exact `foch` binary Cargo built for
/// this integration-test process.
pub struct ProductMeasurementRunner {
	executable: PathBuf,
	identity: MeasurementRunnerIdentity,
	base_data_root: PathBuf,
	no_game_base: bool,
}

impl ProductMeasurementRunner {
	pub fn full_product() -> io::Result<Self> {
		Self::new(false)
	}

	pub fn no_game_base_fixture() -> io::Result<Self> {
		Self::new(true)
	}

	pub fn executable(&self) -> &Path {
		&self.executable
	}

	fn new(no_game_base: bool) -> io::Result<Self> {
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
			no_game_base,
		})
	}

	fn run_product_merge(
		&self,
		request: &MeasurementRequest,
		force: bool,
	) -> Result<TerminalMerge, String> {
		let before_hash = executable_hash(&self.executable)
			.map_err(|error| format!("failed to hash product executable before merge: {error}"))?;
		if before_hash != self.identity.engine_artifact.hash {
			return Ok(TerminalMerge::Fatal {
				detail: "product executable changed after runner identity was created".to_string(),
			});
		}

		validate_request(request)?;
		if !self.no_game_base {
			validate_base_snapshot(request)?;
		}
		fs::create_dir_all(&request.output_dir)
			.map_err(|error| format!("failed to create measurement output directory: {error}"))?;
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
		let cache_root = run_root.path().join("foch-cache");
		let temp_root = run_root.path().join("tmp");
		for path in [
			&config_dir,
			&home_dir,
			&xdg_config_home,
			&xdg_data_home,
			&xdg_cache_home,
			&cache_root,
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
			.env("FOCH_CACHE_ROOT", &cache_root)
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
		if self.no_game_base {
			command.arg("--no-game-base");
		}
		if force {
			command.arg("--force");
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
		let stdout_capture = spawn_bounded_capture(stdout_reader);
		let stderr_capture = spawn_bounded_capture(stderr_reader);
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
		let _stdout = finish_bounded_capture(stdout_capture, "stdout");
		let stderr = finish_bounded_capture(stderr_capture, "stderr");
		let after_hash = executable_hash(&self.executable)
			.map_err(|error| format!("failed to hash product executable after merge: {error}"))?;
		if after_hash != before_hash {
			return Ok(TerminalMerge::Fatal {
				detail: "product executable changed while measurement was running".to_string(),
			});
		}

		let stderr = redact_diagnostic(
			&stderr,
			request,
			Some(run_root.path()),
			&self.executable,
			&self.base_data_root,
		);
		if timed_out {
			return Ok(TerminalMerge::TimedOut {
				detail: Some(with_diagnostic(
					format!("foch merge exceeded {} ms", request.timeout.as_millis()),
					&stderr,
				)),
			});
		}
		if let Some(signal) = exit_signal(&status) {
			return Ok(TerminalMerge::Crashed {
				detail: Some(with_diagnostic(
					format!("foch merge terminated by signal {signal}"),
					&stderr,
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
							&stderr,
						),
					});
				}
			},
			Err(error) if error.kind() == io::ErrorKind::NotFound => None,
			Err(error) => {
				return Ok(TerminalMerge::Fatal {
					detail: with_diagnostic(
						format!("failed to read product merge report: {error}"),
						&stderr,
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
							Some(run_root.path()),
							&self.executable,
							&self.base_data_root,
						),
						&stderr,
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
							Some(run_root.path()),
							&self.executable,
							&self.base_data_root,
						),
						&stderr,
					),
				});
			}
			return Ok(TerminalMerge::Completed {
				report: Box::new(report),
				merge_ms,
			});
		}

		let code = status.code().map_or_else(
			|| "without an exit code".to_string(),
			|code| format!("with exit code {code}"),
		);
		if status.success() {
			Ok(TerminalMerge::Fatal {
				detail: with_diagnostic(
					"foch merge succeeded without writing a merge report".to_string(),
					&stderr,
				),
			})
		} else {
			Ok(TerminalMerge::MergeFailed {
				detail: with_diagnostic(format!("foch merge failed {code}"), &stderr),
			})
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
		let terminal = self
			.run_product_merge(request, false)
			.unwrap_or_else(|detail| TerminalMerge::Fatal { detail });
		redact_terminal_merge(terminal, request, self)
	}
}

impl StructuredKernelRunner for ProductMeasurementRunner {
	fn run_structured(
		&mut self,
		request: StructuredKernelRequest<'_>,
	) -> io::Result<ShadowRunRecord> {
		if self.no_game_base {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"review-pack execution requires the full product runner",
			));
		}
		validate_structured_request(self, &request)?;
		validate_live_shadow_inputs(request.manifest, &self.executable)?;
		let measurement = measurement_request_from_shadow(&request)?;
		let terminal = self
			.run_product_merge(&measurement, request.manifest.inputs.force)
			.unwrap_or_else(|detail| TerminalMerge::Fatal { detail });
		let terminal = redact_terminal_merge(terminal, &measurement, self);
		validate_live_shadow_inputs(request.manifest, &self.executable)?;
		let mut record = shadow_record_from_terminal(
			&request.manifest.comparison_id,
			request.output_dir,
			terminal,
		);
		redact_shadow_record(&mut record, &measurement, self);
		Ok(record)
	}
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

fn validate_structured_request(
	runner: &ProductMeasurementRunner,
	request: &StructuredKernelRequest<'_>,
) -> io::Result<()> {
	validate_shadow_manifest_identity(request.manifest)?;
	let persisted =
		serde_json::from_slice::<ShadowInputManifest>(&fs::read(request.manifest_path)?)
			.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
	if &persisted != request.manifest {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"persisted shadow manifest does not match the requested manifest",
		));
	}
	let runner_executable = runner.executable.canonicalize()?;
	let requested_executable = request.executable.canonicalize()?;
	let manifest_executable = request.manifest.inputs.executable.canonicalize()?;
	if requested_executable != runner_executable || manifest_executable != runner_executable {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			"review-pack manifest and runner do not bind the same executable",
		));
	}
	let actual_hash = executable_hash(&runner_executable)?;
	if actual_hash != runner.identity.engine_artifact.hash
		|| actual_hash != request.manifest.inputs.executable_hash
	{
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"review-pack executable identity changed",
		));
	}
	Ok(())
}

fn validate_live_shadow_inputs(
	manifest: &ShadowInputManifest,
	executable: &Path,
) -> io::Result<()> {
	let retained_paths: BTreeSet<String> = manifest.inputs.retained_paths.iter().cloned().collect();
	let retained_base_paths: BTreeSet<String> = manifest
		.inputs
		.base_files
		.iter()
		.map(|file| file.relative_path.clone())
		.collect();
	let actual = capture_input_manifest(ShadowCaptureRequest {
		playset: &manifest.inputs.playset,
		game_root: &manifest.inputs.game_root,
		game_version: &manifest.inputs.game_version,
		retained_paths: &retained_paths,
		retained_base_paths: &retained_base_paths,
		base_snapshot_identity: &manifest.inputs.base_snapshot_identity,
		force: manifest.inputs.force,
		executable,
	})?;
	if actual.comparison_id != manifest.comparison_id {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"review-pack inputs changed after the shadow manifest was captured",
		));
	}
	Ok(())
}

fn measurement_request_from_shadow(
	request: &StructuredKernelRequest<'_>,
) -> io::Result<MeasurementRequest> {
	if request.manifest.inputs.mods.is_empty() {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			"review-pack manifest contains no source mods",
		));
	}
	let source_dirs = request
		.manifest
		.inputs
		.mods
		.iter()
		.map(|source| {
			source.root_path.clone().ok_or_else(|| {
				io::Error::new(
					io::ErrorKind::InvalidData,
					format!("review-pack source {} has no resolved root", source.mod_id),
				)
			})
		})
		.collect::<io::Result<Vec<_>>>()?;
	let referenced_mods = request
		.manifest
		.inputs
		.mods
		.iter()
		.map(|source| source.mod_id.clone())
		.collect();
	let compatch_dir = request
		.manifest_path
		.parent()
		.ok_or_else(|| io::Error::other("review-pack manifest has no parent directory"))?
		.canonicalize()?;
	Ok(MeasurementRequest {
		snapshot_id: request.manifest.comparison_id.clone(),
		case: Case {
			compatch_id: request.manifest.comparison_id.clone(),
			title: "review-pack Structured product run".to_string(),
			referenced_mods,
			..Case::default()
		},
		compatch_dir,
		source_dirs,
		output_dir: request.output_dir.to_path_buf(),
		basegame_root: request.manifest.inputs.game_root.clone(),
		expected_base_snapshot_identity: request.manifest.inputs.base_snapshot_identity.clone(),
		timeout: request.timeout,
	})
}

fn shadow_record_from_terminal(
	comparison_id: &str,
	output_dir: &Path,
	terminal: TerminalMerge,
) -> ShadowRunRecord {
	match terminal {
		TerminalMerge::Completed { report, merge_ms } => ShadowRunRecord {
			schema: SHADOW_COMPARE_SCHEMA.to_string(),
			comparison_id: comparison_id.to_string(),
			kernel: "structured".to_string(),
			output_dir: output_dir.to_path_buf(),
			output_valid: report.status == MergeReportStatus::Ready
				&& report.manual_conflict_count == 0
				&& report.validation.fatal_errors == 0,
			elapsed_ms: merge_ms,
			status: merge_report_status_name(report.status).to_string(),
			exit_code: Some(merge_report_exit_code(&report)),
			manual_conflict_count: Some(report.manual_conflict_count),
			handler_resolution_count: Some(report.handler_resolutions.len()),
			generated_file_count: Some(report.generated_file_count),
			fatal_reason: report.fatal_reason.clone(),
			error: None,
			diagnostics: merge_report_diagnostics(&report),
		},
		TerminalMerge::MergeFailed { detail } => {
			terminal_shadow_error(comparison_id, output_dir, "merge_failed", detail, false)
		}
		TerminalMerge::Crashed { detail } => terminal_shadow_error(
			comparison_id,
			output_dir,
			"crashed",
			detail.unwrap_or_else(|| "product merge process crashed".to_string()),
			false,
		),
		TerminalMerge::TimedOut { detail } => terminal_shadow_error(
			comparison_id,
			output_dir,
			"timed_out",
			detail.unwrap_or_else(|| "product merge process timed out".to_string()),
			false,
		),
		TerminalMerge::Fatal { detail } => {
			terminal_shadow_error(comparison_id, output_dir, "fatal", detail, true)
		}
	}
}

fn terminal_shadow_error(
	comparison_id: &str,
	output_dir: &Path,
	status: &str,
	detail: String,
	fatal: bool,
) -> ShadowRunRecord {
	ShadowRunRecord {
		schema: SHADOW_COMPARE_SCHEMA.to_string(),
		comparison_id: comparison_id.to_string(),
		kernel: "structured".to_string(),
		output_dir: output_dir.to_path_buf(),
		output_valid: false,
		elapsed_ms: 0,
		status: status.to_string(),
		exit_code: None,
		manual_conflict_count: None,
		handler_resolution_count: None,
		generated_file_count: None,
		fatal_reason: fatal.then(|| detail.clone()),
		error: Some(detail.clone()),
		diagnostics: vec![ShadowDiagnostic {
			kind: if fatal {
				ShadowDiagnosticKind::Fatal
			} else {
				ShadowDiagnosticKind::Error
			},
			path: None,
			message: detail,
		}],
	}
}

fn redact_shadow_record(
	record: &mut ShadowRunRecord,
	request: &MeasurementRequest,
	runner: &ProductMeasurementRunner,
) {
	let redact = |value: &str| {
		redact_diagnostic(
			value,
			request,
			None,
			&runner.executable,
			&runner.base_data_root,
		)
	};
	if let Some(fatal_reason) = record.fatal_reason.as_mut() {
		*fatal_reason = redact(fatal_reason);
	}
	if let Some(error) = record.error.as_mut() {
		*error = redact(error);
	}
	for diagnostic in &mut record.diagnostics {
		diagnostic.message = redact(&diagnostic.message);
		if let Some(path) = diagnostic.path.as_mut() {
			*path = redact(path);
		}
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
	}
}

fn merge_report_status_name(status: MergeReportStatus) -> &'static str {
	match status {
		MergeReportStatus::Ready => "ready",
		MergeReportStatus::PartialSuccess => "partial_success",
		MergeReportStatus::Blocked => "blocked",
		MergeReportStatus::Fatal => "fatal",
	}
}

fn merge_report_exit_code(report: &MergeReport) -> i32 {
	if report.validation.fatal_errors > 0 || report.status == MergeReportStatus::Fatal {
		1
	} else if report.status == MergeReportStatus::Blocked {
		2
	} else {
		0
	}
}

fn merge_report_diagnostics(report: &MergeReport) -> Vec<ShadowDiagnostic> {
	let mut diagnostics = Vec::new();
	if let Some(reason) = report.fatal_reason.as_ref() {
		diagnostics.push(ShadowDiagnostic {
			kind: ShadowDiagnosticKind::Fatal,
			path: None,
			message: reason.clone(),
		});
	}
	diagnostics.extend(
		report
			.warnings
			.iter()
			.cloned()
			.map(|message| ShadowDiagnostic {
				kind: ShadowDiagnosticKind::Warning,
				path: None,
				message,
			}),
	);
	diagnostics.extend(
		report
			.conflict_resolutions
			.iter()
			.map(|resolution| ShadowDiagnostic {
				kind: ShadowDiagnosticKind::Conflict,
				path: Some(resolution.path.clone()),
				message: resolution.reason.clone(),
			}),
	);
	diagnostics.extend(report.handler_resolutions.iter().map(|resolution| {
		ShadowDiagnostic {
			kind: ShadowDiagnosticKind::HandlerResolution,
			path: Some(resolution.path.clone()),
			message: resolution
				.rationale
				.clone()
				.unwrap_or_else(|| resolution.action.clone()),
		}
	}));
	diagnostics
}

fn validate_request(request: &MeasurementRequest) -> Result<(), String> {
	if request.case.referenced_mods.len() != request.source_dirs.len() {
		return Err(format!(
			"case declares {} source mods but {} roots were supplied",
			request.case.referenced_mods.len(),
			request.source_dirs.len()
		));
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

fn spawn_bounded_capture(
	mut reader: impl Read + Send + 'static,
) -> thread::JoinHandle<io::Result<(Vec<u8>, bool)>> {
	thread::spawn(move || {
		let capacity = usize::try_from(MAX_DIAGNOSTIC_BYTES).unwrap_or(usize::MAX);
		let mut retained = Vec::with_capacity(capacity);
		let mut buffer = [0_u8; 8192];
		let mut truncated = false;
		loop {
			let read = reader.read(&mut buffer)?;
			if read == 0 {
				break;
			}
			retained.extend_from_slice(&buffer[..read]);
			if retained.len() > capacity {
				let excess = retained.len() - capacity;
				retained.drain(..excess);
				truncated = true;
			}
		}
		Ok((retained, truncated))
	})
}

fn finish_bounded_capture(
	capture: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
	stream: &str,
) -> String {
	match capture.join() {
		Ok(Ok((bytes, truncated))) => {
			let body = String::from_utf8_lossy(&bytes);
			if truncated {
				format!("<{stream} truncated to last {MAX_DIAGNOSTIC_BYTES} bytes>\n{body}")
			} else {
				body.into_owned()
			}
		}
		Ok(Err(error)) => format!("<{stream} unavailable: {error}>"),
		Err(_) => format!("<{stream} capture panicked>"),
	}
}

fn redact_diagnostic(
	diagnostic: &str,
	request: &MeasurementRequest,
	run_root: Option<&Path>,
	executable: &Path,
	base_data_root: &Path,
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
	truncate_utf8(
		redacted,
		usize::try_from(MAX_DIAGNOSTIC_BYTES).unwrap_or(usize::MAX),
	)
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
	use foch_core::model::{HandlerResolutionRecord, MergeReportConflictResolution};
	use foch_merge_quality::corpus::Case;

	#[test]
	fn diagnostics_are_bounded_and_hide_local_roots() {
		let root = tempfile::tempdir().expect("fixture root");
		let dataset_root = root.path().join("private-dataset");
		let objects_root = dataset_root.join("objects");
		let compatch_root = objects_root.join("aa/hash/tree");
		let source_root = objects_root.join("bb/hash/tree");
		let basegame_root = root.path().join("private-base");
		let base_data_root = root.path().join("private-base-data");
		for path in [
			&compatch_root,
			&source_root,
			&basegame_root,
			&base_data_root,
		] {
			fs::create_dir_all(path).expect("create private fixture path");
		}
		let request = MeasurementRequest {
			snapshot_id: "snapshot".to_string(),
			case: Case {
				compatch_id: "compatch".to_string(),
				title: "fixture".to_string(),
				referenced_mods: vec!["source".to_string()],
				..Case::default()
			},
			compatch_dir: compatch_root,
			source_dirs: vec![source_root],
			output_dir: root.path().join("private-output"),
			basegame_root,
			expected_base_snapshot_identity: "none".to_string(),
			timeout: Duration::from_secs(1),
		};
		let raw = format!(
			"{} {} {} {} {} {} {}",
			request.compatch_dir.display(),
			request.source_dirs[0].display(),
			request.output_dir.display(),
			dataset_root.display(),
			objects_root.display(),
			base_data_root.display(),
			"x".repeat(usize::try_from(MAX_DIAGNOSTIC_BYTES).unwrap() + 1),
		);

		let redacted = redact_diagnostic(
			&raw,
			&request,
			Some(root.path()),
			Path::new(env!("CARGO_BIN_EXE_foch")),
			&base_data_root,
		);

		assert!(!redacted.contains(&root.path().display().to_string()));
		assert!(redacted.contains("<compatch-root>"));
		assert!(redacted.len() <= usize::try_from(MAX_DIAGNOSTIC_BYTES).unwrap() + 32);
	}

	#[test]
	fn process_capture_retains_only_the_bounded_tail() {
		let capacity = usize::try_from(MAX_DIAGNOSTIC_BYTES).unwrap();
		let mut bytes = vec![b'a'; capacity];
		bytes.extend(vec![b'b'; 17]);
		let captured =
			finish_bounded_capture(spawn_bounded_capture(io::Cursor::new(bytes)), "stderr");

		assert!(captured.starts_with("<stderr truncated to last"));
		assert!(captured.ends_with(&"b".repeat(17)));
		assert!(!captured.contains(&"a".repeat(capacity)));
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
	fn shadow_record_preserves_product_conflict_and_warning_diagnostics() {
		let report = MergeReport {
			status: MergeReportStatus::Blocked,
			manual_conflict_count: 1,
			generated_file_count: 2,
			warnings: vec!["review warning".to_string()],
			conflict_resolutions: vec![MergeReportConflictResolution {
				path: "events/example.txt".to_string(),
				reason: "manual conflict".to_string(),
				..MergeReportConflictResolution::default()
			}],
			handler_resolutions: vec![HandlerResolutionRecord {
				path: "common/example.txt".to_string(),
				action: "last_writer".to_string(),
				source: None,
				rationale: Some("reviewed resolution".to_string()),
			}],
			..MergeReport::default()
		};
		let record = shadow_record_from_terminal(
			"comparison",
			Path::new("output"),
			TerminalMerge::Completed {
				report: Box::new(report),
				merge_ms: 17,
			},
		);

		assert!(!record.output_valid);
		assert_eq!(record.status, "blocked");
		assert_eq!(record.exit_code, Some(2));
		assert_eq!(record.manual_conflict_count, Some(1));
		assert_eq!(record.handler_resolution_count, Some(1));
		assert_eq!(record.generated_file_count, Some(2));
		assert!(record.diagnostics.iter().any(|diagnostic| {
			diagnostic.kind == ShadowDiagnosticKind::Warning
				&& diagnostic.message == "review warning"
		}));
		assert!(record.diagnostics.iter().any(|diagnostic| {
			diagnostic.kind == ShadowDiagnosticKind::Conflict
				&& diagnostic.path.as_deref() == Some("events/example.txt")
		}));
		assert!(record.diagnostics.iter().any(|diagnostic| {
			diagnostic.kind == ShadowDiagnosticKind::HandlerResolution
				&& diagnostic.path.as_deref() == Some("common/example.txt")
				&& diagnostic.message == "reviewed resolution"
		}));
	}

	#[test]
	fn partial_success_and_inconsistent_ready_reports_are_not_review_evidence() {
		for report in [
			MergeReport {
				status: MergeReportStatus::PartialSuccess,
				manual_conflict_count: 1,
				..MergeReport::default()
			},
			MergeReport {
				status: MergeReportStatus::Ready,
				manual_conflict_count: 1,
				..MergeReport::default()
			},
		] {
			let record = shadow_record_from_terminal(
				"comparison",
				Path::new("output"),
				TerminalMerge::Completed {
					report: Box::new(report),
					merge_ms: 1,
				},
			);
			assert!(!record.output_valid);
		}
	}

	#[test]
	fn product_attestation_requires_exact_kernel_scope_and_base_identity() {
		let mut report = MergeReport {
			execution: Some(foch_core::model::MergeExecutionAttestation {
				schema: MERGE_EXECUTION_ATTESTATION_SCHEMA.to_string(),
				kernel: MergeReportKernel::SemanticTree,
				scope: MergeReportScope::FullProductMerge,
				base_snapshot: MergeReportBaseSnapshot::Resolved {
					identity: "base-identity".to_string(),
				},
			}),
			..MergeReport::default()
		};
		let request = MeasurementRequest {
			snapshot_id: "snapshot".to_string(),
			case: Case::default(),
			compatch_dir: PathBuf::from("/compatch"),
			source_dirs: Vec::new(),
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
	}
}
