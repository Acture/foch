use crate::cli::arg::SimplifyArgs;
use crate::cli::handler::HandlerResult;
use foch_engine::{CheckRequest, Config, SimplifyOptions, run_simplify_with_options};

pub fn handle_simplify(simplify_args: &SimplifyArgs, config: Config) -> HandlerResult {
	if (simplify_args.in_place && simplify_args.out.is_some())
		|| (!simplify_args.in_place && simplify_args.out.is_none())
	{
		return Err("simplify requires exactly one of --out or --in-place".into());
	}
	let request = CheckRequest {
		playset_path: simplify_args.playset_path.clone(),
		config,
	};
	let summary = run_simplify_with_options(
		request,
		SimplifyOptions {
			include_game_base: !simplify_args.no_game_base,
			target_mod_id: simplify_args.target.clone(),
			out_dir: simplify_args.out.clone(),
			in_place: simplify_args.in_place,
		},
	)?;
	println!(
		"simplify complete: target_root={} removed_definitions={} removed_files={} report={}",
		summary.target_root.display(),
		summary.removed_definition_count,
		summary.removed_file_count,
		summary.report_path.display()
	);
	Ok(0)
}
