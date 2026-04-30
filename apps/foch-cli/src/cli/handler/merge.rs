use crate::cli::arg::MergeArgs;
use crate::cli::handler::HandlerResult;
use foch_core::config::{AppliedDepOverride, FochConfig};
use foch_engine::{CheckRequest, Config, MergeExecuteOptions, run_merge_with_options};
use foch_language::analyzer::report::render_merge_report_text;
use std::path::Path;

pub fn handle_merge(merge_args: &MergeArgs, config: Config) -> HandlerResult {
	let request = CheckRequest {
		playset_path: merge_args.playset_path.clone(),
		config,
	};
	let dep_overrides = load_dep_overrides(merge_args)?;
	let execution = run_merge_with_options(
		request,
		MergeExecuteOptions {
			out_dir: merge_args.out.clone(),
			include_game_base: !merge_args.no_game_base,
			force: merge_args.force,
			ignore_replace_path: merge_args.ignore_replace_path,
			fallback: merge_args.fallback || merge_args.force,
			dep_overrides,
		},
	)?;
	println!("{}", render_merge_report_text(&execution.report));
	Ok(execution.exit_code)
}

fn load_dep_overrides(
	merge_args: &MergeArgs,
) -> Result<Vec<AppliedDepOverride>, Box<dyn std::error::Error>> {
	let local_config = if let Some(path) = merge_args.config.as_ref() {
		FochConfig::load_from_path(path)?
	} else {
		let playset_root = merge_args
			.playset_path
			.parent()
			.unwrap_or_else(|| Path::new("."));
		FochConfig::try_load(playset_root)?
	};

	let mut overrides: Vec<AppliedDepOverride> = local_config
		.overrides
		.iter()
		.map(AppliedDepOverride::config)
		.collect();
	overrides.extend(
		merge_args
			.ignore_dep
			.iter()
			.map(|item| AppliedDepOverride::cli(item.mod_id.clone(), item.dep_id.clone())),
	);
	Ok(overrides)
}
