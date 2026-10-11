//! `foch check --fix`: syntax fixes, as a linter's are.

use crate::cli::arg::CheckArgs;
use crate::cli::handler::merge::install_launcher_stub;
use crate::cli::handler::{HandlerResult, resolve_input_source};
use foch::game::eu4::text::decode_paradox_bytes;
use foch::input::{Config, InputRequest, InputSource};
use foch::project::Project;
use foch::repair::{
	FixOptions, RepairPlan, is_workshop_item, plan_directory_repairs, plan_repairs,
	repair_in_place, restore_backup, write_patch_mod,
};
use std::path::Path;

/// The name the patch mod's descriptor gives it.
const PATCH_MOD_NAME: &str = "Foch syntax fixes";

/// Whether `check` runs in fix mode.
pub fn requested(args: &CheckArgs) -> bool {
	args.fix || args.diff || args.restore.is_some()
}

pub fn handle_check_fix(args: &CheckArgs, config: Config) -> HandlerResult {
	if let Some(backup) = &args.restore {
		let summary = restore_backup(backup)?;
		for path in &summary.restored {
			println!("restored {}", path.display());
		}
		for path in &summary.skipped {
			println!(
				"left {} as it is: it no longer holds the fixed bytes",
				path.display()
			);
		}
		return Ok(0);
	}
	if args.unsafe_fixes && !args.fix && !args.diff {
		return Err("--unsafe-fixes applies with --fix or --diff".into());
	}
	let paradox_data_path = config.paradox_data_path.clone();
	let options = FixOptions {
		unsafe_fixes: args.unsafe_fixes,
	};

	// A mod directory is an author's own work and is fixed in place; a
	// playset, or a mod Steam keeps in its Workshop folder, is other
	// people's mods, and the fixes go where the user says.
	let directory = args.playset_path.as_deref().filter(|path| path.is_dir());
	if args.fix && args.patch_mod.is_none() && !args.in_place {
		match directory {
			Some(root) if is_workshop_item(root) => {
				return Err(format!(
					"{} is a Steam Workshop mod, whose files Steam replaces when it updates; write a patch mod (--patch-mod <DIR>) or fix its own files with a backup (--in-place)",
					root.display()
				)
				.into());
			}
			Some(_) => {}
			None => {
				return Err(
					"fixing a playset writes either a patch mod (--patch-mod <DIR>) or the mods' own files (--in-place)"
						.into(),
				);
			}
		}
	}
	let plan = match directory {
		Some(root) => {
			let project = match &args.config {
				Some(path) => Project::load_from_path(path)?,
				None => Project::try_load(root)?,
			};
			plan_directory_repairs(root, &project, options)?
		}
		None => {
			let source = resolve_input_source(args.playset_path.as_deref(), &config)?;
			let project = load_project(args, &source)?;
			plan_repairs(&InputRequest::new(source, config), &project, options)?
		}
	};

	if args.diff {
		print_diff(&plan);
		print_unrepaired(&plan);
		return Ok(i32::from(
			!plan.files.is_empty() || !plan.unrepaired.is_empty(),
		));
	}

	print_changes(&plan);
	print_unrepaired(&plan);
	if plan.files.is_empty() {
		return Ok(i32::from(!plan.unrepaired.is_empty()));
	}
	if let Some(out) = &args.patch_mod {
		write_patch_mod(&plan, out, PATCH_MOD_NAME)?;
		println!(
			"wrote the patch mod \"{PATCH_MOD_NAME}\" to {}",
			out.display()
		);
		// The launcher lists only the `.mod` files of its own mod directory.
		match paradox_data_path
			.as_deref()
			.map(|dir| install_launcher_stub(out, dir, PATCH_MOD_NAME))
		{
			Some(Ok(stub)) => println!(
				"the launcher lists it through {}; enable it after the mods it fixes",
				stub.display()
			),
			Some(Err(error)) => {
				return Err(
					format!("the patch mod cannot be listed in the launcher: {error}").into(),
				);
			}
			None => println!(
				"to enable it, add a .mod file holding name=\"{PATCH_MOD_NAME}\" and path=\"{}\" to the game's mod folder (set paradox_data_path to have Foch add it), then load it after the mods it fixes",
				std::path::absolute(out)
					.unwrap_or_else(|_| out.clone())
					.display()
			),
		}
	} else {
		// Every in-place fix keeps the originals, so one made by mistake can
		// be undone.
		let backup = repair_in_place(&plan)?;
		println!(
			"fixed {} file(s); the originals are in {}",
			plan.files.len(),
			backup.display()
		);
		println!("undo with: foch check --restore \"{}\"", backup.display());
		if directory.is_none_or(is_workshop_item) {
			println!(
				"Steam replaces a Workshop mod's files when the mod updates or its files are verified, which undoes these fixes"
			);
		}
	}
	Ok(i32::from(!plan.unrepaired.is_empty()))
}

fn load_project(
	args: &CheckArgs,
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

fn print_changes(plan: &RepairPlan) {
	for file in &plan.files {
		println!("fix {}: {}", file.mod_id, file.path);
		for change in &file.changes {
			println!("  {change}");
		}
	}
}

fn print_unrepaired(plan: &RepairPlan) {
	for file in &plan.unrepaired {
		println!("cannot fix {}: {}", file.mod_id, file.path);
		for reason in &file.reasons {
			println!("  {reason}");
		}
	}
}

fn print_diff(plan: &RepairPlan) {
	for file in &plan.files {
		let before = decode_paradox_bytes(&file.original);
		let after = decode_paradox_bytes(&file.repaired);
		let label = format!("{}/{}", file.mod_id, file.path);
		print!(
			"{}",
			similar::TextDiff::from_lines(before.as_ref(), after.as_ref())
				.unified_diff()
				.header(&format!("a/{label}"), &format!("b/{label}"))
		);
	}
}
