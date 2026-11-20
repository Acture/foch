use crate::cli::arg::{AnalysisModeArg, CheckArgs, CheckChannelArg, CheckOutputFormat};
use crate::cli::handler::{HandlerResult, resolve_input_source};
use foch::check::run_checks_with_options;
use foch::game::eu4::analysis::report::render_text;
use foch::input::{CheckOptions, Config, InputRequest, InputSource};
use foch::model::{AnalysisMode, ChannelMode, CheckResult};
use foch::playset::descriptor::descriptor_path_text;
use std::path::{Path, PathBuf};

mod fix;

pub fn handle_check(check_args: &CheckArgs, config: Config) -> HandlerResult {
	if fix::requested(check_args) {
		return fix::handle_check_fix(check_args, config);
	}
	// A mod directory is not a launcher playset. `fs::read` on that directory
	// fails, so point the existing playset loader at a one-mod `dlc_load.json`
	// whose descriptor path is the directory itself.
	let held = hold_mod_directory(check_args.playset_path.as_deref())?;
	let source = match &held {
		Some(playset) => InputSource::DlcLoad(playset.dlc.clone()),
		None => resolve_input_source(check_args.playset_path.as_deref(), &config)?,
	};
	let request = InputRequest::new(source, config);
	let run_options = CheckOptions {
		analysis_mode: to_analysis_mode(check_args.analysis_mode),
		channel_mode: to_channel_mode(check_args.channel),
		include_game_base: !check_args.no_game_base,
	};

	let result = run_checks_with_options(request, run_options.clone());
	if let Some(path) = check_args.parse_issue_report.as_ref() {
		std::fs::write(
			path,
			serde_json::to_string_pretty(&result.parse_issue_report)?,
		)?;
	}

	let output_result = select_output_result(&result, run_options.channel_mode);
	let rendered = match check_args.format {
		CheckOutputFormat::Text => render_text(
			&output_result,
			!check_args.no_color,
			run_options.channel_mode,
		),
		CheckOutputFormat::Json => serde_json::to_string_pretty(&output_result)?,
	};

	if let Some(path) = check_args.output.as_ref() {
		std::fs::write(path, rendered)?;
		println!("check result written to: {}", path.display());
	} else {
		println!("{rendered}");
	}

	if result.has_fatal_errors() {
		return Ok(1);
	}

	if check_args.strict && result.has_strict_findings() {
		return Ok(2);
	}

	Ok(0)
}

fn select_output_result(result: &CheckResult, channel_mode: ChannelMode) -> CheckResult {
	let mut output = result.clone();
	output.findings = result.filtered_findings(channel_mode);
	if channel_mode == ChannelMode::Strict {
		output.advisory_findings.clear();
	}
	output
}

fn to_analysis_mode(mode: AnalysisModeArg) -> AnalysisMode {
	match mode {
		AnalysisModeArg::Basic => AnalysisMode::Basic,
		AnalysisModeArg::Semantic => AnalysisMode::Semantic,
	}
}

fn to_channel_mode(channel: CheckChannelArg) -> ChannelMode {
	match channel {
		CheckChannelArg::Strict => ChannelMode::Strict,
		CheckChannelArg::All => ChannelMode::All,
	}
}

/// Keeps the synthetic playset alive for the duration of one directory check.
struct ModDirectoryPlayset {
	root: PathBuf,
	dlc: PathBuf,
}

impl Drop for ModDirectoryPlayset {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.root);
	}
}

/// A directory with its own `descriptor.mod` is one mod, not `dlc_load.json`.
fn hold_mod_directory(path: Option<&Path>) -> std::io::Result<Option<ModDirectoryPlayset>> {
	let Some(path) = path else {
		return Ok(None);
	};
	if !path.is_dir() || !path.join("descriptor.mod").is_file() {
		return Ok(None);
	}
	let nanos = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|duration| duration.as_nanos())
		.unwrap_or(0);
	let mut created = None;
	for salt in 0..16 {
		let root = std::env::temp_dir().join(format!(
			"foch-mod-check-{}-{nanos}-{salt}",
			std::process::id()
		));
		match std::fs::create_dir(&root) {
			Ok(()) => {
				created = Some(ModDirectoryPlayset {
					dlc: root.join("dlc_load.json"),
					root,
				});
				break;
			}
			Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
			Err(error) => return Err(error),
		}
	}
	let Some(playset) = created else {
		return Err(std::io::Error::other(
			"could not create a directory for the mod check",
		));
	};
	std::fs::create_dir(playset.root.join("mod"))?;
	let path_text = descriptor_path_text(&path.canonicalize()?).map_err(|error| {
		std::io::Error::other(format!(
			"mod directory {} has no launcher path: {error}",
			path.display()
		))
	})?;
	std::fs::write(
		playset.root.join("mod").join("ugc_1.mod"),
		format!("name=\"mod directory\"\npath=\"{path_text}\"\nremote_file_id=\"1\"\n"),
	)?;
	std::fs::write(
		&playset.dlc,
		"{\"enabled_mods\":[\"mod/ugc_1.mod\"],\"disabled_dlcs\":[]}\n",
	)?;
	Ok(Some(playset))
}
