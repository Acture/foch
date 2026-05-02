use crate::cli::arg::MergeArgs;
use crate::cli::handler::{HandlerResult, resolve_playset_path};
use foch_core::config::{AppliedDepOverride, FochConfig};
use foch_core::domain::descriptor::load_descriptor;
use foch_core::domain::playlist::Playlist;
use foch_core::fingerprint::compute_playset_fingerprint;
use foch_core::model::{MERGE_REPORT_ARTIFACT_PATH, MergeReport};
use foch_engine::merge::conflict_handler::{InteractiveMode, set_interactive_mode_and_config};
use foch_engine::{CheckRequest, Config, MergeExecuteOptions, run_merge_with_options};
use foch_language::analyzer::report::render_merge_report_text;
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

pub fn handle_merge(merge_args: &MergeArgs, config: Config) -> HandlerResult {
	let playset_path = resolve_playset_path(merge_args.playset_path.as_deref(), &config)?;
	let paradox_data_path = config.paradox_data_path.clone();
	let request = CheckRequest {
		playset_path: playset_path.clone(),
		config,
	};
	let fallback_enabled = merge_args.fallback || merge_args.force;
	let local_config = load_local_foch_config(merge_args, &playset_path)?;
	let fingerprint = compute_fingerprint_for_playset(&playset_path, &local_config);
	if let Some(exit) = handle_existing_out_dir(&merge_args.out, fingerprint.as_deref())? {
		return Ok(exit);
	}
	let dep_overrides = applied_dep_overrides(merge_args, &local_config);
	let interactive_enabled = !merge_args.non_interactive && std::io::stdin().is_terminal();
	let interactive_mode = if interactive_enabled && !merge_args.cli_prompt {
		InteractiveMode::Tui
	} else {
		InteractiveMode::Cli
	};
	let interactive_config_path = if interactive_enabled {
		let prompt_kind = match interactive_mode {
			InteractiveMode::Tui => "ratatui UI",
			InteractiveMode::Cli | InteractiveMode::Auto => "simple prompt",
		};
		eprintln!(
			"[foch] interactive mode: {prompt_kind} will appear for unresolved conflicts. Press q to abort, d to defer."
		);
		Some(resolve_resolution_config_path(merge_args, &playset_path))
	} else {
		None
	};
	set_interactive_mode_and_config(interactive_mode, interactive_config_path);
	let execution = run_merge_with_options(
		request,
		MergeExecuteOptions {
			out_dir: merge_args.out.clone(),
			include_game_base: !merge_args.no_game_base,
			force: merge_args.force,
			ignore_replace_path: merge_args.ignore_replace_path,
			fallback: fallback_enabled,
			dep_overrides,
			playset_fingerprint: fingerprint,
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
	if matches!(
		execution.report.status,
		foch_core::model::MergeReportStatus::Ready
			| foch_core::model::MergeReportStatus::PartialSuccess
	) && let Some(paradox_dir) = paradox_data_path.as_ref()
		&& let Err(err) = install_launcher_stub(&merge_args.out, paradox_dir)
	{
		eprintln!("[foch] failed to install launcher stub: {err}");
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

fn load_local_foch_config(
	merge_args: &MergeArgs,
	playset_path: &Path,
) -> Result<FochConfig, Box<dyn std::error::Error>> {
	if let Some(path) = merge_args.config.as_ref() {
		Ok(FochConfig::load_from_path(path)?)
	} else {
		let playset_root = playset_root_for(playset_path);
		Ok(FochConfig::try_load(&playset_root)?)
	}
}

fn applied_dep_overrides(
	merge_args: &MergeArgs,
	local_config: &FochConfig,
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
/// Reads `dlc_load.json` for the ordered enabled-mods list, then loads each
/// mod's descriptor file at `<playset_root>/mod/ugc_<steam_id>.mod` to pull
/// the version field. Combines that with the foch.toml overrides /
/// resolutions. Returns `None` if anything required is missing — the caller
/// then treats the run as un-fingerprintable and skips the cache check.
fn compute_fingerprint_for_playset(
	playset_path: &Path,
	local_config: &FochConfig,
) -> Option<String> {
	let playlist = Playlist::from_dlc_load(playset_path).ok()?;
	let playset_root = playset_path.parent().unwrap_or_else(|| Path::new("."));
	let mut mods: Vec<(String, String)> = Vec::new();
	for entry in &playlist.mods {
		if !entry.enabled {
			continue;
		}
		let Some(steam_id) = entry.steam_id.clone() else {
			continue;
		};
		let descriptor_path = playset_root.join("mod").join(format!("ugc_{steam_id}.mod"));
		let version = load_descriptor(&descriptor_path)
			.ok()
			.and_then(|d| d.version)
			.unwrap_or_default();
		mods.push((steam_id, version));
	}
	Some(compute_playset_fingerprint(
		&mods,
		&local_config.overrides,
		&local_config.resolutions,
	))
}

fn resolve_resolution_config_path(merge_args: &MergeArgs, playset_path: &Path) -> PathBuf {
	if let Some(path) = merge_args.config.as_ref() {
		return path.clone();
	}

	if let Ok(cwd) = std::env::current_dir() {
		let cwd_config = cwd.join("foch.toml");
		if cwd_config.is_file() {
			return cwd_config;
		}
	}

	playset_root_for(playset_path).join("foch.toml")
}

fn playset_root_for(playset_path: &Path) -> PathBuf {
	playset_path
		.parent()
		.unwrap_or_else(|| Path::new("."))
		.to_path_buf()
}

/// Refuse to silently overwrite a non-empty existing output directory.
///
/// If the existing report's `playset_fingerprint` matches the current run's,
/// the merge is short-circuited: the previous report is printed and the saved
/// exit code is returned. Otherwise the user is prompted to overwrite (or the
/// run is aborted on a non-TTY).
///
/// Returns:
/// - `Ok(None)` to proceed with a fresh merge (directory absent, empty, or
///   user confirmed at the prompt — the directory is wiped first)
/// - `Ok(Some(exit_code))` to abort early without invoking the merge engine
/// - `Err(_)` for filesystem or IO errors
fn handle_existing_out_dir(
	out_dir: &Path,
	current_fingerprint: Option<&str>,
) -> Result<Option<i32>, Box<dyn std::error::Error>> {
	if !out_dir.exists() {
		return Ok(None);
	}
	if !out_dir.is_dir() {
		return Err(format!(
			"--out path {} exists and is not a directory; refusing to overwrite",
			out_dir.display()
		)
		.into());
	}
	let has_entries = fs::read_dir(out_dir)?.next().is_some();
	if !has_entries {
		return Ok(None);
	}

	if let Some(current) = current_fingerprint
		&& let Some(cached) = read_cached_report(out_dir)
		&& cached.playset_fingerprint.as_deref() == Some(current)
	{
		eprintln!(
			"[foch] {} matches the prior merge fingerprint; reusing the existing output.",
			out_dir.display()
		);
		println!("{}", render_merge_report_text(&cached));
		return Ok(Some(merge_report_exit_code(&cached)));
	}

	let stdin = std::io::stdin();
	let stderr = std::io::stderr();
	if !stdin.is_terminal() || !stderr.is_terminal() {
		eprintln!(
			"[foch] --out {} already exists and is non-empty (and the prior merge has a different mod set or no recorded fingerprint); refusing to overwrite without an interactive confirmation. Delete it manually or run from a TTY.",
			out_dir.display()
		);
		return Ok(Some(1));
	}

	let mut handle = stderr.lock();
	write!(
		handle,
		"[foch] --out {} already exists and the prior merge differs (or has no recorded fingerprint). Overwrite? [y/N] ",
		out_dir.display()
	)?;
	handle.flush()?;
	drop(handle);

	let mut answer = String::new();
	stdin.lock().read_line(&mut answer)?;
	let answer = answer.trim().to_ascii_lowercase();
	if answer != "y" && answer != "yes" {
		eprintln!("[foch] aborted; output directory not modified");
		return Ok(Some(1));
	}

	fs::remove_dir_all(out_dir)?;
	Ok(None)
}

fn read_cached_report(out_dir: &Path) -> Option<MergeReport> {
	let report_path = out_dir.join(MERGE_REPORT_ARTIFACT_PATH);
	let raw = fs::read_to_string(&report_path).ok()?;
	serde_json::from_str(&raw).ok()
}

fn merge_report_exit_code(report: &MergeReport) -> i32 {
	use foch_core::model::MergeReportStatus;
	match report.status {
		MergeReportStatus::Ready => 0,
		MergeReportStatus::PartialSuccess => 0,
		MergeReportStatus::Blocked => 2,
		MergeReportStatus::Fatal => 3,
	}
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
	let descriptor_value = absolute_out.to_string_lossy().replace('\\', "/");
	let body = format!(
		"# foch-managed launcher stub for {}\nname=\"{}\"\npath=\"{}\"\nsupported_version=\"*\"\n",
		out_dir.display(),
		escape_descriptor(&display_name),
		escape_descriptor(&descriptor_value)
	);
	fs::write(&stub_path, body)?;
	eprintln!(
		"[foch] launcher stub installed at {}; enable it in the Paradox Launcher and disable the source mods to use the merge.",
		stub_path.display()
	);
	Ok(())
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
