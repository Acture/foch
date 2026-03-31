use crate::cli::arg::{MergePlanArgs, MergePlanOutputFormat};
use crate::cli::handler::HandlerResult;
use foch_core::model::MergePlanFormat;
use foch_engine::{CheckRequest, Config, MergePlanOptions, run_merge_plan_with_options};
use foch_language::analyzer::report::{merge_plan_exit_code, render_merge_plan_text};

pub fn handle_merge_plan(merge_plan_args: &MergePlanArgs, config: Config) -> HandlerResult {
	let request = CheckRequest {
		playset_path: merge_plan_args.playset_path.clone(),
		config,
	};
	let options = MergePlanOptions {
		include_game_base: !merge_plan_args.no_game_base,
	};

	let result = run_merge_plan_with_options(request, options);
	let rendered = match to_merge_plan_format(merge_plan_args.format) {
		MergePlanFormat::Text => render_merge_plan_text(&result),
		MergePlanFormat::Json => serde_json::to_string_pretty(&result)?,
	};

	if let Some(path) = merge_plan_args.output.as_ref() {
		std::fs::write(path, rendered)?;
		println!("合并计划已写入: {}", path.display());
	} else {
		println!("{rendered}");
	}

	Ok(merge_plan_exit_code(&result))
}

fn to_merge_plan_format(format: MergePlanOutputFormat) -> MergePlanFormat {
	match format {
		MergePlanOutputFormat::Text => MergePlanFormat::Text,
		MergePlanOutputFormat::Json => MergePlanFormat::Json,
	}
}
