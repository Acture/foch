use crate::cli::arg::MergeArgs;
use crate::cli::handler::{HandlerResult, resolve_input_source};
use foch::game::eu4::analysis::report::{merge_plan_exit_code, render_merge_report_text};
use foch::input::{Config, InputRequest, InputSource, resolve_product_input_manifest};
use foch::merge::{
	AnalyzedMerge, CancellationToken, CommitAuthorization, ConflictHandler, InteractiveCliHandler,
	MergeAnalysisOptions, MergeAnalysisStatus, MergeDisposition, MergeUnitKind,
	NoopProgressObserver, analyze_merge,
};
use foch::model::{MERGE_REPORT_ARTIFACT_PATH, MergeReport, ProductInputManifest};
use foch::playset::Playset;
use foch::playset::descriptor::{
	descriptor_path_text, escape_descriptor_value, load_launcher_descriptor,
};
use foch::project::compute_playset_fingerprint;
use foch::project::{AppliedDepOverride, Project};

use crate::tui::conflict_handler::InteractiveTuiHandler;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

pub fn handle_merge(merge_args: &MergeArgs, config: Config) -> HandlerResult {
	let source = resolve_input_source(merge_args.playset_path.as_deref(), &config)?;
	let paradox_data_path = config.paradox_data_path.clone();
	let request = InputRequest::new(source.clone(), config);
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
				InputSource::Manifest(path) => Some(path.clone()),
				InputSource::DlcLoad(_) => None,
			}),
			interactive_conflict_handler,
			interactive_resolution_config_path,
			playset_fingerprint: fingerprint.clone(),
			provenance: merge_args.provenance,
			merge_workers: merge_args
				.jobs
				.unwrap_or_else(foch::merge::default_merge_workers),
			retained_paths: None,
		},
		&NoopProgressObserver,
		&CancellationToken::new(),
	)?;
	let analysis = analyzed.analysis();
	println!(
		"{}",
		render_merge_review_text(&analyzed, merge_args.review_all)
	);
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

fn render_merge_review_text(analyzed: &AnalyzedMerge, review_all: bool) -> String {
	const UNITS_PER_DISPOSITION: usize = 20;
	let analysis = analyzed.analysis();
	let summary = analyzed.review_summary();
	let status = match analysis.status() {
		MergeAnalysisStatus::ReadyToCommit => "ready_to_commit",
		MergeAnalysisStatus::CommittableWithDeferrals => "committable_with_deferrals",
		MergeAnalysisStatus::Blocked => "blocked",
	};
	let mut output = format!(
		"Foch Merge Review\nstatus: {status}\nunits: {}\n  safe: {}\n  copy: {}\n  needs_user_choice: {}\n  unsupported_input: {}\n  engine_failure: {}\n  deferred: {}\n",
		summary.total,
		summary.safe,
		summary.copy,
		summary.needs_user_choice,
		summary.unsupported_input,
		summary.engine_failure,
		summary.deferred,
	);
	if !analysis.plan().fatal_errors.is_empty() {
		output.push_str("fatal errors:\n");
		for error in &analysis.plan().fatal_errors {
			output.push_str(&format!("  - {error}\n"));
		}
	}
	output.push_str("review units:\n");
	let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
	for unit in analyzed.list_units() {
		let disposition = match unit.disposition {
			MergeDisposition::Safe => "safe",
			MergeDisposition::Copy => "copy",
			MergeDisposition::NeedsUserChoice => "needs_user_choice",
			MergeDisposition::UnsupportedInput => "unsupported_input",
			MergeDisposition::EngineFailure => "engine_failure",
			MergeDisposition::Deferred => "deferred",
		};
		let count: &mut usize = counts.entry(disposition).or_default();
		*count += 1;
		if !review_all && *count > UNITS_PER_DISPOSITION {
			continue;
		}
		let kind = match unit.kind {
			MergeUnitKind::File => "file",
			MergeUnitKind::DefinitionModule => "definition_module",
		};
		output.push_str(&format!(
			"- [{disposition}] {}\n  id: {}\n  family: {}\n  kind: {kind}\n  strategy: {}\n  summary: {}\n",
			unit.path, unit.id, unit.family, unit.strategy, unit.summary,
		));
		// A unit can write one file per contributing directory. Listing only the
		// primary path would hide the rest of what the merge produced.
		for path in &unit.output_paths {
			output.push_str(&format!("  output: {path}\n"));
		}
		if !unit.contributors.is_empty() {
			let contributors = unit
				.contributors
				.iter()
				.map(|contributor| {
					format!(
						"{} ({}, precedence {})",
						contributor.name, contributor.mod_id, contributor.precedence
					)
				})
				.collect::<Vec<_>>()
				.join(", ");
			output.push_str(&format!("  contributors: {contributors}\n"));
		}
		for note in &unit.notes {
			output.push_str(&format!("  note: {note}\n"));
		}
	}
	if !review_all && counts.values().any(|count| *count > UNITS_PER_DISPOSITION) {
		output.push_str("additional review units (not displayed):\n");
		for (disposition, count) in counts {
			if count > UNITS_PER_DISPOSITION {
				output.push_str(&format!(
					"  {} more {disposition} units\n",
					count - UNITS_PER_DISPOSITION
				));
			}
		}
		output.push_str("Pass --review-all to display every unit before committing.\n");
	}
	output
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
	source: &InputSource,
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
	source: &InputSource,
) -> Result<Project, Box<dyn std::error::Error>> {
	if let Some(path) = merge_args.config.as_ref() {
		Ok(Project::load_from_path(path)?)
	} else if let InputSource::Manifest(path) = source {
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

/// Compute the playset fingerprint without resolving the complete input.
///
/// Launcher playsets use their ordered enabled-mod list and descriptor
/// versions. Project inputs use only their ordered, trusted Workshop ACF
/// identities; imports or local unversioned mods fail closed. Neither path
/// inventories a mod root. The fingerprint also binds foch overrides and
/// resolutions.
fn compute_fingerprint_for_source(
	request: &InputRequest,
	local_config: &Project,
) -> Option<String> {
	match &request.source {
		InputSource::DlcLoad(path) => compute_fingerprint_for_playset(path, local_config),
		InputSource::Manifest(_) => compute_fingerprint_for_manifest(request, local_config),
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
		let version = load_launcher_descriptor(&descriptor_path)
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
	request: &InputRequest,
	local_config: &Project,
) -> Option<String> {
	let manifest = resolve_product_input_manifest(request).ok()?;
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

fn resolve_resolution_config_path(merge_args: &MergeArgs, source: &InputSource) -> PathBuf {
	if let Some(path) = merge_args.config.as_ref() {
		return path.clone();
	}

	if let InputSource::Manifest(path) = source {
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
	let slug = launcher_stub_slug(out_dir)?;
	let stub_path = mod_dir.join(format!("foch_{slug}.mod"));
	let display_name = format!("foch merge ({slug})");
	let descriptor_value = descriptor_path_text(&absolute_out).map_err(|reason| {
		format!(
			"merged output {} cannot be named in a launcher descriptor: {reason}",
			absolute_out.display()
		)
	})?;
	let body = format!(
		"# foch-managed launcher stub for {}\nname=\"{}\"\npath=\"{}\"\nsupported_version=\"*\"\n",
		out_dir.display(),
		escape_descriptor_value(&display_name),
		descriptor_value
	);
	fs::write(&stub_path, body)?;
	eprintln!(
		"[foch] launcher stub installed at {}; enable it in the Paradox Launcher and disable the source mods to use the merge.",
		stub_path.display()
	);
	Ok(())
}

/// The launcher stub's file name part, taken from the output directory's
/// name. The stub is written at that name, so a name that is not UTF-8 is an
/// error instead of being rendered lossily into some other stub's name.
fn launcher_stub_slug(out_dir: &Path) -> Result<String, String> {
	let raw = match out_dir.file_name() {
		Some(name) => name.to_str().ok_or_else(|| {
			format!(
				"output directory name {} is not valid UTF-8, so it cannot name a launcher stub",
				out_dir.display()
			)
		})?,
		None => "merge",
	};
	Ok(raw
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
				c
			} else {
				'_'
			}
		})
		.collect())
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

	/// The launcher stub names the merged output with the library's descriptor
	/// encoder: reading the stub back yields the output directory. Both sides
	/// are compared canonically, since Windows canonicalizes to a verbatim
	/// `\\?\` prefix that the descriptor spells as the plain one.
	#[test]
	fn launcher_stub_names_the_merged_output_exactly() {
		let temp = tempfile::tempdir().expect("temp dir");
		let out_dir = temp.path().join("merged out Ölände");
		fs::create_dir_all(&out_dir).expect("create output");
		let paradox_dir = temp.path().join("paradox");

		install_launcher_stub(&out_dir, &paradox_dir).expect("install stub");

		let stub = paradox_dir.join("mod").join(format!(
			"foch_{}.mod",
			launcher_stub_slug(&out_dir).expect("UTF-8 output name")
		));
		let descriptor = load_launcher_descriptor(&stub).expect("read stub");
		assert_eq!(
			fs::canonicalize(descriptor.path.expect("stub path")).expect("canonical stub path"),
			fs::canonicalize(&out_dir).expect("canonical output")
		);
	}

	/// A directory the descriptor format has no `path` text for gets no stub,
	/// rather than one naming another directory.
	#[test]
	fn launcher_stub_refuses_an_output_directory_holding_a_quote() {
		let temp = tempfile::tempdir().expect("temp dir");
		let out_dir = temp.path().join("merged \"out\"");
		let paradox_dir = temp.path().join("paradox");

		let error = install_launcher_stub(&out_dir, &paradox_dir)
			.expect_err("a quote has no descriptor spelling")
			.to_string();
		assert!(error.contains("cannot be named"), "{error}");
		assert_eq!(
			fs::read_dir(paradox_dir.join("mod"))
				.expect("mod dir")
				.count(),
			0
		);
	}

	/// The stub's file name comes from the output directory's name, so a name
	/// that is not UTF-8 is an error, never a lossy name another output could
	/// also render to. In memory: some filesystems refuse such names. The
	/// directory does not exist, so its descriptor `path` would be refused as
	/// well; the install error must be the slug's, which is checked first.
	#[cfg(unix)]
	#[test]
	fn launcher_stub_refuses_an_output_directory_name_that_is_not_utf8() {
		use std::ffi::OsStr;
		use std::os::unix::ffi::OsStrExt;

		let temp = tempfile::tempdir().expect("temp dir");
		let paradox_dir = temp.path().join("paradox");
		for name in [&b"merged\xff"[..], &b"merged\xfe"[..]] {
			let out_dir = temp.path().join(OsStr::from_bytes(name));
			let error = launcher_stub_slug(&out_dir).expect_err("not UTF-8");
			assert!(error.contains("cannot name a launcher stub"), "{error}");
			let error = install_launcher_stub(&out_dir, &paradox_dir)
				.expect_err("not UTF-8")
				.to_string();
			assert!(error.contains("cannot name a launcher stub"), "{error}");
		}
		assert_eq!(
			fs::read_dir(paradox_dir.join("mod"))
				.expect("mod dir")
				.count(),
			0,
			"no stub is written"
		);
		assert_eq!(
			launcher_stub_slug(Path::new("/")).as_deref(),
			Ok("merge"),
			"a directory without a name keeps the fallback"
		);
	}

	/// A link whose own name is not UTF-8 can point at a UTF-8 directory, so
	/// the descriptor `path` has text while the stub's name does not: the slug
	/// alone refuses it. Linux only: macOS filesystems refuse such names.
	#[cfg(target_os = "linux")]
	#[test]
	fn launcher_stub_refuses_a_link_name_that_is_not_utf8() {
		use std::ffi::OsStr;
		use std::os::unix::ffi::OsStrExt;

		let temp = tempfile::tempdir().expect("temp dir");
		let target = temp.path().join("merged");
		fs::create_dir_all(&target).expect("create output");
		let link = temp.path().join(OsStr::from_bytes(b"merged\xff"));
		std::os::unix::fs::symlink(&target, &link).expect("link output");
		let paradox_dir = temp.path().join("paradox");

		let error = install_launcher_stub(&link, &paradox_dir)
			.expect_err("a link name that is not UTF-8")
			.to_string();
		assert!(error.contains("cannot name a launcher stub"), "{error}");
		assert_eq!(
			fs::read_dir(paradox_dir.join("mod"))
				.expect("mod dir")
				.count(),
			0,
			"no stub is written"
		);
	}

	#[test]
	fn manifest_fingerprint_uses_ordered_acf_identity() {
		let config = Project::default();
		let first = compute_fingerprint_for_workshop_manifest(&workshop_manifest(2_001), &config);
		let second = compute_fingerprint_for_workshop_manifest(&workshop_manifest(2_002), &config);

		assert_ne!(first, second);
	}

	/// The fingerprint reads each launcher descriptor's version only, so a
	/// `replace_path` the mod's own descriptor would reject does not unset it.
	#[test]
	fn playset_fingerprint_ignores_a_launcher_descriptor_replace_path() {
		let temp = tempfile::tempdir().expect("temp dir");
		std::fs::create_dir_all(temp.path().join("mod")).expect("create launcher mod dir");
		let dlc_load = temp.path().join("dlc_load.json");
		std::fs::write(
			&dlc_load,
			r#"{"enabled_mods":["mod/ugc_1001.mod"],"disabled_dlcs":[]}"#,
		)
		.expect("write dlc_load");
		let descriptor = |extra: &str| {
			std::fs::write(
				temp.path().join("mod").join("ugc_1001.mod"),
				format!("name=\"Named\"\nremote_file_id=\"1001\"\nversion=\"2.0\"\n{extra}"),
			)
			.expect("write launcher descriptor");
			compute_fingerprint_for_playset(&dlc_load, &Project::default())
		};

		let plain = descriptor("");
		assert!(plain.is_some());
		assert_eq!(descriptor("replace_path=\"common/../events\"\n"), plain);
	}

	#[test]
	fn manifest_fingerprint_fails_closed_when_acf_resolution_fails() {
		let request =
			InputRequest::from_manifest_path(PathBuf::from("missing-foch.toml"), Config::default());
		assert!(compute_fingerprint_for_manifest(&request, &Project::default()).is_none());
	}
}
