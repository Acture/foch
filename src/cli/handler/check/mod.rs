use crate::check::model::{AnalysisMode, ChannelMode, CheckRequest, CheckResult, RunOptions};
use crate::check::report::render_text;
use crate::check::run_checks_with_options;
use crate::cli::arg::{AnalysisModeArg, CheckArgs, CheckChannelArg, CheckOutputFormat};
use crate::cli::config::Config;
use crate::cli::handler::HandlerResult;

pub fn handle_check(check_args: &CheckArgs, config: Config) -> HandlerResult {
	let request = CheckRequest {
		playset_path: check_args.playset_path.clone(),
		config,
	};
	let run_options = RunOptions {
		analysis_mode: to_analysis_mode(check_args.analysis_mode),
		channel_mode: to_channel_mode(check_args.channel),
		include_game_base: !check_args.no_game_base,
	};

	let result = run_checks_with_options(request, run_options.clone());
	if let Some(path) = check_args.parse_issue_report.as_ref() {
		std::fs::write(path, serde_json::to_string_pretty(&result.parse_issue_report)?)?;
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
		println!("检查结果已写入: {}", path.display());
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
