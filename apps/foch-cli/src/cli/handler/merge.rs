use crate::cli::arg::MergeArgs;
use crate::cli::handler::HandlerResult;
use foch_core::config::{AppliedDepOverride, FochConfig};
use foch_core::model::{MERGE_REPORT_ARTIFACT_PATH, MergeReport};
use foch_engine::merge::conflict_handler::set_interactive_config_path;
use foch_engine::{CheckRequest, Config, MergeExecuteOptions, run_merge_with_options};
use foch_language::analyzer::report::render_merge_report_text;
use std::path::{Path, PathBuf};

pub fn handle_merge(merge_args: &MergeArgs, config: Config) -> HandlerResult {
	let request = CheckRequest {
		playset_path: merge_args.playset_path.clone(),
		config,
	};
	let fallback_enabled = merge_args.fallback || merge_args.force;
	let dep_overrides = load_dep_overrides(merge_args)?;
	let interactive_config_path = if merge_args.interactive {
		eprintln!(
			"[foch] interactive mode: prompts will appear for unresolved conflicts. Press q to abort, d to defer."
		);
		Some(resolve_resolution_config_path(merge_args))
	} else {
		None
	};
	set_interactive_config_path(interactive_config_path);
	let execution = run_merge_with_options(
		request,
		MergeExecuteOptions {
			out_dir: merge_args.out.clone(),
			include_game_base: !merge_args.no_game_base,
			force: merge_args.force,
			ignore_replace_path: merge_args.ignore_replace_path,
			fallback: fallback_enabled,
			dep_overrides,
		},
	)?;
	println!("{}", render_merge_report_text(&execution.report));
	if let Some(tip) = render_unresolved_conflict_tip(
		&execution.report,
		merge_args.out.as_path(),
		fallback_enabled,
	) {
		eprintln!("{tip}");
	}
	Ok(execution.exit_code)
}

fn render_unresolved_conflict_tip(
	report: &MergeReport,
	out_dir: &Path,
	fallback_enabled: bool,
) -> Option<String> {
	let unresolved_conflicts = report.manual_conflict_count;
	if fallback_enabled || unresolved_conflicts == 0 {
		return None;
	}

	let report_path = out_dir.join(MERGE_REPORT_ARTIFACT_PATH);
	let plural = if unresolved_conflicts == 1 { "" } else { "s" };
	let mut lines = vec![
		format!(
			"Tip: {unresolved_conflicts} unresolved merge conflict{plural} were SKIPPED (not written to {}).",
			out_dir.display()
		),
		format!("  1. Inspect {} for details.", report_path.display()),
		"  2. Re-run with --fallback to materialize last-writer output with conflict markers."
			.to_string(),
	];
	if let Some(finding) = report.dep_misuse.first() {
		lines.push(format!(
			"  3. Possible spurious dep: {} -> {}; try --ignore-dep {}:{}.",
			finding.mod_display_name,
			finding.suspicious_dep_display_name,
			finding.mod_id,
			finding.suspicious_dep_id
		));
	} else {
		lines.push("  3. Resolve skipped files manually, then re-run merge.".to_string());
	}
	lines.push("Foch kept your output safe; use --fallback when you're ready.".to_string());
	Some(lines.join("\n"))
}

fn load_dep_overrides(
	merge_args: &MergeArgs,
) -> Result<Vec<AppliedDepOverride>, Box<dyn std::error::Error>> {
	let local_config = if let Some(path) = merge_args.config.as_ref() {
		FochConfig::load_from_path(path)?
	} else {
		let playset_root = playset_root_for(&merge_args.playset_path);
		FochConfig::try_load(&playset_root)?
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

fn resolve_resolution_config_path(merge_args: &MergeArgs) -> PathBuf {
	if let Some(path) = merge_args.config.as_ref() {
		return path.clone();
	}

	if let Ok(cwd) = std::env::current_dir() {
		let cwd_config = cwd.join("foch.toml");
		if cwd_config.is_file() {
			return cwd_config;
		}
	}

	playset_root_for(&merge_args.playset_path).join("foch.toml")
}

fn playset_root_for(playset_path: &Path) -> PathBuf {
	playset_path
		.parent()
		.unwrap_or_else(|| Path::new("."))
		.to_path_buf()
}
