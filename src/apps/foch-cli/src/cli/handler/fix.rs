use crate::cli::arg::FixArgs;
use crate::cli::handler::{HandlerResult, resolve_input_source};
use foch::input::{Config, InputRequest, InputSource};
use foch::project::Project;
use foch::repair::{RepairPlan, plan_repairs, repair_in_place, restore_backup, write_patch_mod};
use std::path::Path;

/// The name the patch mod's descriptor gives it.
const PATCH_MOD_NAME: &str = "Foch syntax repairs";

pub fn handle_fix(args: &FixArgs, config: Config) -> HandlerResult {
	if let Some(backup) = &args.restore {
		let summary = restore_backup(backup)?;
		for path in &summary.restored {
			println!("restored {}", path.display());
		}
		for path in &summary.skipped {
			println!(
				"left {} as it is: it no longer holds the repaired bytes",
				path.display()
			);
		}
		return Ok(0);
	}

	let source = resolve_input_source(args.playset_path.as_deref(), &config)?;
	let project = load_project(args, &source)?;
	let request = InputRequest::new(source, config);
	let plan = plan_repairs(&request, &project)?;
	print_plan(&plan);
	if plan.files.is_empty() {
		return Ok(0);
	}
	if args.out.is_none() && !args.in_place {
		println!(
			"nothing written; pass --out <DIR> for a patch mod or --in-place for the mods themselves, with --confirm"
		);
		return Ok(0);
	}
	if !args.confirm {
		println!("nothing written; pass --confirm to write these repairs");
		return Ok(0);
	}
	if let Some(out) = &args.out {
		write_patch_mod(&plan, out, PATCH_MOD_NAME)?;
		println!(
			"wrote the patch mod \"{PATCH_MOD_NAME}\" to {}; enable it after the mods it repairs",
			out.display()
		);
	} else {
		let backup = repair_in_place(&plan)?;
		println!(
			"repaired {} source file(s); the originals are in {}",
			plan.files.len(),
			backup.display()
		);
		println!("undo with: foch fix --restore \"{}\"", backup.display());
		println!(
			"Steam replaces a Workshop mod's files when the mod updates or its files are verified, which undoes these repairs"
		);
	}
	Ok(0)
}

fn load_project(
	args: &FixArgs,
	source: &InputSource,
) -> Result<Project, Box<dyn std::error::Error>> {
	if let Some(path) = &args.config {
		Ok(Project::load_from_path(path)?)
	} else if let InputSource::Manifest(path) = source {
		Ok(Project::load_from_path(path)?)
	} else {
		let root = source.path().parent().unwrap_or_else(|| Path::new("."));
		Ok(Project::try_load(root)?)
	}
}

fn print_plan(plan: &RepairPlan) {
	if plan.files.is_empty() && plan.unrepaired.is_empty() {
		println!("no file needs a syntax repair");
		return;
	}
	for file in &plan.files {
		println!("repair {}: {}", file.mod_id, file.path);
		for change in &file.changes {
			println!("  {change}");
		}
	}
	for file in &plan.unrepaired {
		println!("cannot repair {}: {}", file.mod_id, file.path);
		for reason in &file.reasons {
			println!("  {reason}");
		}
	}
}
