use super::error::MergeError;
use super::materialize::{MergeMaterializeOptions, materialize_merge_internal};
use crate::request::{CheckRequest, RunOptions};
use crate::run_checks_with_options;
use foch_core::config::{AppliedDepOverride, FochConfig, ResolutionMap};
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
	pub fallback: bool,
	pub dep_overrides: Vec<AppliedDepOverride>,
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
	let resolution_map = load_resolution_map(&request)?;
	let mut report = materialize_merge_internal(
		request.clone(),
		&options.out_dir,
		MergeMaterializeOptions {
			include_game_base: options.include_game_base,
			force: options.force,
			ignore_replace_path: options.ignore_replace_path,
			fallback: options.fallback,
			dep_overrides: options.dep_overrides.clone(),
			resolution_map,
		},
	)?;

	if report.status == MergeReportStatus::Fatal {
		return Ok(MergeExecutionResult {
			report,
			exit_code: 1,
		});
	}

	if report.status == MergeReportStatus::Blocked && !options.force {
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

	let exit_code = match report.status {
		MergeReportStatus::Ready => 0,
		MergeReportStatus::PartialSuccess => 0,
		MergeReportStatus::Blocked => 2,
		MergeReportStatus::Fatal => 3,
	};

	Ok(MergeExecutionResult { report, exit_code })
}

fn load_resolution_map(request: &CheckRequest) -> Result<ResolutionMap, MergeError> {
	let playset_root = request
		.playset_path
		.parent()
		.unwrap_or_else(|| Path::new("."));
	let config = FochConfig::try_load(playset_root).map_err(|err| MergeError::Validation {
		path: Some(playset_root.display().to_string()),
		message: err.to_string(),
	})?;
	ResolutionMap::from_entries(&config.resolutions).map_err(|err| MergeError::Validation {
		path: Some(playset_root.display().to_string()),
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
		"name=\"{out_dir_name}\"\npath=\"{}\"\nremote_file_id=\"{synthetic_steam_id}\"\n",
		canonical_out_dir.display()
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
			&["S002", "S004", "A004"],
		),
		missing_localisation: count_findings_for_rules(&result.findings, &["A005"]),
	})
}

fn count_findings_for_rules(findings: &[Finding], rule_ids: &[&str]) -> usize {
	findings
		.iter()
		.filter(|finding| rule_ids.contains(&finding.rule_id.as_str()))
		.count()
}

fn final_merge_status(report: &MergeReport) -> MergeReportStatus {
	let has_validation_errors = report.validation.fatal_errors > 0
		|| report.validation.strict_findings > 0
		|| report.validation.parse_errors > 0;

	if has_validation_errors {
		MergeReportStatus::Fatal
	} else if report.manual_conflict_count > 0 {
		// If materialize already set PartialSuccess (--force resolved conflicts),
		// keep it.  Otherwise block.
		match report.status {
			MergeReportStatus::PartialSuccess => MergeReportStatus::PartialSuccess,
			_ => MergeReportStatus::Fatal,
		}
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
