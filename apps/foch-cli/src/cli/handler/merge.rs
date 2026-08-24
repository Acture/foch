use crate::cli::arg::MergeArgs;
use crate::cli::handler::{HandlerResult, resolve_workspace_source};
use foch::model::{MERGE_REPORT_ARTIFACT_PATH, MergeReport, ProductInputManifest};
use foch::playset::Playset;
use foch::playset::descriptor::load_descriptor;
use foch::project::compute_playset_fingerprint;
use foch::project::{AppliedDepOverride, Project};
use foch_engine::{
	CancellationToken, CheckRequest, CommitAuthorization, Config, ConflictHandler,
	InteractiveCliHandler, MergeAnalysisOptions, NoopProgressObserver, WorkspaceSource,
	analyze_merge, resolve_product_input_manifest,
};

use crate::tui::conflict_handler::InteractiveTuiHandler;
use foch_language::analyzer::report::{
	merge_plan_exit_code, render_merge_plan_text, render_merge_report_text,
};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

pub fn handle_merge(merge_args: &MergeArgs, config: Config) -> HandlerResult {
	let source = resolve_workspace_source(merge_args.playset_path.as_deref(), &config)?;
	let paradox_data_path = config.paradox_data_path.clone();
	let request = CheckRequest::new(source.clone(), config);
	let local_config = load_local_foch_config(merge_args, &source)?;
	let fingerprint = compute_fingerprint_for_source(&request, &local_config);
	let dep_overrides = applied_dep_overrides(merge_args, &local_config);
	let (interactive_conflict_handler, interactive_resolution_config_path) =
		build_interactive_conflict_handler(merge_args, &source);
	let analyzed = analyze_merge(
		request,
		MergeAnalysisOptions {
			out_dir: merge_args.out.clone(),
			include_game_base: !merge_args.no_game_base,
			include_base: merge_args.include_base,
			gui_scroll_merge: merge_args.gui_scroll_merge,
			force: merge_args.force,
			ignore_replace_path: merge_args.ignore_replace_path,
			dep_overrides,
			resolution_config_path: merge_args.config.clone().or_else(|| match &source {
				WorkspaceSource::Manifest(path) => Some(path.clone()),
				WorkspaceSource::DlcLoad(_) => None,
			}),
			interactive_conflict_handler,
			interactive_resolution_config_path,
			playset_fingerprint: fingerprint.clone(),
			provenance: merge_args.provenance,
			retained_paths: None,
		},
		&NoopProgressObserver,
		&CancellationToken::new(),
	)?;
	let analysis = analyzed.analysis();
	println!("{}", render_merge_plan_text(analysis.plan()));
	let plan_exit_code = merge_plan_exit_code(analysis.plan());
	if analysis.plan().has_fatal_errors() {
		return Ok(plan_exit_code);
	}
	if !confirm_merge_commit(merge_args, merge_args.out.as_path())? {
		return Ok(0);
	}

	let authorization = match analyzed.replacement_target()? {
		Some(target) => {
			if !confirm_existing_out_dir(target.path())? {
				return Ok(1);
			}
			CommitAuthorization::ReplaceExisting(target)
		}
		None => CommitAuthorization::EmptyTargetOnly,
	};
	let execution = analyzed.commit(authorization)?;
	println!("{}", render_merge_report_text(&execution.report));
	if let Some(tip) = render_unresolved_conflict_tip(&execution.report, merge_args.out.as_path()) {
		eprintln!("{tip}");
	}
	if matches!(
		execution.merge_status.status,
		foch::model::MergeReportStatus::Ready | foch::model::MergeReportStatus::PartialSuccess
	) && let Some(paradox_dir) = paradox_data_path.as_ref()
		&& let Err(err) = install_launcher_stub(&merge_args.out, paradox_dir)
	{
		eprintln!("[foch] failed to install launcher stub: {err}");
	}
	Ok(execution.exit_code)
}

fn confirm_merge_commit(
	merge_args: &MergeArgs,
	out_dir: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
	if merge_args.confirm {
		return Ok(true);
	}

	if merge_args.non_interactive {
		eprintln!("[foch] analysis complete; output not written. Pass --confirm to commit it.");
		return Ok(false);
	}

	let stdin = std::io::stdin();
	let stderr = std::io::stderr();
	if !stdin.is_terminal() || !stderr.is_terminal() {
		eprintln!("[foch] analysis complete; output not written. Pass --confirm to commit it.");
		return Ok(false);
	}

	let mut handle = stderr.lock();
	write!(
		handle,
		"[foch] commit this analyzed merge to {}? [y/N] ",
		out_dir.display()
	)?;
	handle.flush()?;
	drop(handle);

	let mut answer = String::new();
	stdin.lock().read_line(&mut answer)?;
	let answer = answer.trim().to_ascii_lowercase();
	if answer == "y" || answer == "yes" {
		Ok(true)
	} else {
		eprintln!("[foch] analysis kept for review; output directory not modified");
		Ok(false)
	}
}

fn build_interactive_conflict_handler(
	merge_args: &MergeArgs,
	source: &WorkspaceSource,
) -> (Option<Box<dyn ConflictHandler>>, Option<PathBuf>) {
	if merge_args.non_interactive {
		return (None, None);
	}

	if merge_args.cli_prompt {
		eprintln!(
			"[foch] interactive mode: simple prompt will appear for unresolved conflicts. Press q to abort, d to defer."
		);
		return (
			Some(Box::new(InteractiveCliHandler::new())),
			Some(resolve_resolution_config_path(merge_args, source)),
		);
	}

	if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
		eprintln!(
			"[foch] interactive mode: ratatui UI will appear for unresolved conflicts. Press q to abort, d to defer."
		);
		return (
			Some(Box::new(InteractiveTuiHandler::new())),
			Some(resolve_resolution_config_path(merge_args, source)),
		);
	}

	(None, None)
}

fn render_unresolved_conflict_tip(report: &MergeReport, out_dir: &Path) -> Option<String> {
	let unresolved_conflicts = report.manual_conflict_count;
	if unresolved_conflicts == 0 {
		return None;
	}

	let report_path = out_dir.join(MERGE_REPORT_ARTIFACT_PATH);
	let plural = if unresolved_conflicts == 1 { "" } else { "s" };
	let verb = if unresolved_conflicts == 1 {
		"was"
	} else {
		"were"
	};
	let mut lines = vec![
		format!(
			"Tip: {unresolved_conflicts} unresolved merge conflict{plural} {verb} SKIPPED (not written to {}).",
			out_dir.display()
		),
		format!("  1. Inspect {} for details.", report_path.display()),
		"  2. Choose a reviewed resolution interactively, or add a supported foch.toml [[resolutions]] entry with handler = \"last_writer\"."
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
	lines.push(
		"Foch committed the safe units and withheld only these conflicts; use an explicit resolution when you're ready."
			.to_string(),
	);
	Some(lines.join("\n"))
}

fn load_local_foch_config(
	merge_args: &MergeArgs,
	source: &WorkspaceSource,
) -> Result<Project, Box<dyn std::error::Error>> {
	if let Some(path) = merge_args.config.as_ref() {
		Ok(Project::load_from_path(path)?)
	} else if let WorkspaceSource::Manifest(path) = source {
		Ok(Project::load_from_path(path)?)
	} else {
		let playset_root = playset_root_for(source.path());
		Ok(Project::try_load(&playset_root)?)
	}
}

fn applied_dep_overrides(
	merge_args: &MergeArgs,
	local_config: &Project,
) -> Vec<AppliedDepOverride> {
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
	overrides
}

/// Compute the playset fingerprint without doing a full workspace resolve.
///
/// Launcher playsets use their ordered enabled-mod list and descriptor
/// versions. Manifest workspaces use only their ordered, trusted Workshop ACF
/// identities; imports or local unversioned mods fail closed. Neither path
/// inventories a mod root. The fingerprint also binds foch overrides and
/// resolutions.
fn compute_fingerprint_for_source(
	request: &CheckRequest,
	local_config: &Project,
) -> Option<String> {
	match &request.source {
		WorkspaceSource::DlcLoad(path) => compute_fingerprint_for_playset(path, local_config),
		WorkspaceSource::Manifest(_) => compute_fingerprint_for_manifest(request, local_config),
	}
}

fn compute_fingerprint_for_playset(playset_path: &Path, local_config: &Project) -> Option<String> {
	let playlist = Playset::from_dlc_load(playset_path).ok()?;
	let playset_root = playset_path.parent().unwrap_or_else(|| Path::new("."));
	let mut mods: Vec<(String, String)> = Vec::new();
	for entry in &playlist.mods {
		if !entry.enabled {
			continue;
		}
		let steam_id = entry.steam_id.clone()?;
		let descriptor_path = playset_root.join("mod").join(format!("ugc_{steam_id}.mod"));
		let version = load_descriptor(&descriptor_path)
			.ok()
			.and_then(|descriptor| descriptor.version)?;
		mods.push((steam_id, version));
	}
	Some(compute_playset_fingerprint(
		&mods,
		&local_config.overrides,
		&local_config.resolutions,
	))
}

fn compute_fingerprint_for_manifest(
	request: &CheckRequest,
	local_config: &Project,
) -> Option<String> {
	let manifest = resolve_product_input_manifest(request, None).ok()?;
	Some(compute_fingerprint_for_workshop_manifest(
		&manifest,
		local_config,
	))
}

fn compute_fingerprint_for_workshop_manifest(
	manifest: &ProductInputManifest,
	local_config: &Project,
) -> String {
	let mods = manifest
		.mods
		.iter()
		.map(|input| {
			(
				input.mod_id.clone(),
				format!(
					"steam-acf:{}:{}:{}",
					input.workshop_identity.app_id,
					input.workshop_identity.workshop_id,
					input.workshop_identity.manifest_id
				),
			)
		})
		.collect::<Vec<_>>();
	compute_playset_fingerprint(&mods, &local_config.overrides, &local_config.resolutions)
}

fn resolve_resolution_config_path(merge_args: &MergeArgs, source: &WorkspaceSource) -> PathBuf {
	if let Some(path) = merge_args.config.as_ref() {
		return path.clone();
	}

	if let WorkspaceSource::Manifest(path) = source {
		return path.clone();
	}

	if let Ok(cwd) = std::env::current_dir() {
		let cwd_config = cwd.join("foch.toml");
		if cwd_config.is_file() {
			return cwd_config;
		}
	}

	playset_root_for(source.path()).join("foch.toml")
}

fn playset_root_for(playset_path: &Path) -> PathBuf {
	playset_path
		.parent()
		.unwrap_or_else(|| Path::new("."))
		.to_path_buf()
}

/// Confirm replacement of the target captured by the engine's opaque token.
/// Commit revalidates that exact target under the output lock before and after
/// staging the frozen analyzed bytes.
fn confirm_existing_out_dir(out_dir: &Path) -> io::Result<bool> {
	let stdin = std::io::stdin();
	let stderr = std::io::stderr();
	if !stdin.is_terminal() || !stderr.is_terminal() {
		eprintln!(
			"[foch] --out {} already exists and is non-empty; refusing to overwrite without a separate interactive confirmation. Delete it manually or run from a TTY.",
			out_dir.display()
		);
		return Ok(false);
	}

	let mut handle = stderr.lock();
	write!(
		handle,
		"[foch] --out {} already exists and is non-empty. Replace it with the analyzed merge? [y/N] ",
		out_dir.display()
	)?;
	handle.flush()?;
	drop(handle);

	let mut answer = String::new();
	stdin.lock().read_line(&mut answer)?;
	let answer = answer.trim().to_ascii_lowercase();
	if answer != "y" && answer != "yes" {
		eprintln!("[foch] aborted; output directory not modified");
		return Ok(false);
	}

	Ok(true)
}

/// Drop a `<paradox_data_path>/mod/foch_<slug>.mod` stub pointing at the
/// freshly-merged `out_dir` so the Paradox launcher lists the merge under
/// "Mods" without the user having to hand-write a descriptor.
///
/// The launcher only enumerates `.mod` files inside its game-specific mod
/// directory; the in-`out_dir` `descriptor.mod` we already write isn't
/// enough on its own. The user still has to open the launcher and toggle
/// the merge on (and disable the source mods to avoid double-loading).
fn install_launcher_stub(
	out_dir: &Path,
	paradox_data_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
	let mod_dir = paradox_data_path.join("mod");
	fs::create_dir_all(&mod_dir)?;
	let absolute_out = fs::canonicalize(out_dir).unwrap_or_else(|_| out_dir.to_path_buf());
	let slug = launcher_stub_slug(out_dir);
	let stub_path = mod_dir.join(format!("foch_{slug}.mod"));
	let display_name = format!("foch merge ({slug})");
	let descriptor_value =
		strip_extended_length_prefix(&absolute_out.to_string_lossy()).replace('\\', "/");
	let body = format!(
		"# foch-managed launcher stub for {}\nname=\"{}\"\npath=\"{}\"\nsupported_version=\"*\"\n",
		out_dir.display(),
		escape_descriptor(&display_name),
		escape_descriptor(&descriptor_value)
	);
	fs::write(&stub_path, body)?;
	let display_stub = strip_extended_length_prefix(&stub_path.to_string_lossy());
	eprintln!(
		"[foch] launcher stub installed at {display_stub}; enable it in the Paradox Launcher and disable the source mods to use the merge."
	);
	Ok(())
}

/// Strip Windows extended-length path prefixes (`\\?\` / `\\?\UNC\`) so paths
/// written into Paradox descriptors and printed to the user are loadable by
/// the launcher and shell-friendly. Non-Windows / non-prefixed paths are
/// returned verbatim.
fn strip_extended_length_prefix(path: &str) -> String {
	if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
		format!(r"\\{rest}")
	} else if let Some(rest) = path.strip_prefix(r"\\?\") {
		rest.to_string()
	} else if let Some(rest) = path.strip_prefix("//?/UNC/") {
		format!("//{rest}")
	} else if let Some(rest) = path.strip_prefix("//?/") {
		rest.to_string()
	} else {
		path.to_string()
	}
}

fn launcher_stub_slug(out_dir: &Path) -> String {
	let raw = out_dir
		.file_name()
		.map(|s| s.to_string_lossy().into_owned())
		.unwrap_or_else(|| "merge".to_string());
	raw.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
				c
			} else {
				'_'
			}
		})
		.collect()
}

fn escape_descriptor(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch::model::ProductInputMod;
	use foch::playset::steam::{SteamId, WorkshopInstallIdentity};

	fn workshop_manifest(manifest_id: u64) -> ProductInputManifest {
		ProductInputManifest::new(vec![ProductInputMod {
			mod_id: "1001".to_string(),
			precedence: 1,
			workshop_identity: WorkshopInstallIdentity {
				app_id: 236_850,
				workshop_id: SteamId::new(1_001),
				manifest_id: SteamId::new(manifest_id),
			},
		}])
	}

	#[test]
	fn manifest_fingerprint_uses_ordered_acf_identity() {
		let config = Project::default();
		let first = compute_fingerprint_for_workshop_manifest(&workshop_manifest(2_001), &config);
		let second = compute_fingerprint_for_workshop_manifest(&workshop_manifest(2_002), &config);

		assert_ne!(first, second);
	}

	#[test]
	fn manifest_fingerprint_fails_closed_when_acf_resolution_fails() {
		let request =
			CheckRequest::from_manifest_path(PathBuf::from("missing-foch.toml"), Config::default());
		assert!(compute_fingerprint_for_manifest(&request, &Project::default()).is_none());
	}
}
