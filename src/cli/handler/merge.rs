use crate::check::{CheckRequest, MergeExecuteOptions, run_merge_with_options};
use crate::check::report::render_merge_report_text;
use crate::cli::arg::MergeArgs;
use crate::cli::config::Config;
use crate::cli::handler::HandlerResult;

pub fn handle_merge(merge_args: &MergeArgs, config: Config) -> HandlerResult {
	let request = CheckRequest {
		playset_path: merge_args.playset_path.clone(),
		config,
	};
	let execution = run_merge_with_options(
		request,
		MergeExecuteOptions {
			out_dir: merge_args.out.clone(),
			include_game_base: !merge_args.no_game_base,
			force: merge_args.force,
		},
	)?;
	println!("{}", render_merge_report_text(&execution.report));
	Ok(execution.exit_code)
}
