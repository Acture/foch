//! Runs Foch test bundles against a real EU4 install, in isolation.
//!
//! The install stays read-only: the runner builds a short-path *runtime
//! layer* that copies the executable and loose files and links the large data
//! directories, then launches the copied `eu4.exe` with a throwaway user
//! directory holding only the source mod and the generated test layer. It
//! never launches through Steam and never writes the player's own profile.
//!
//! The player's real EU4 user directory is kept intact by backing up the few
//! files the game rewrites and restoring them if their contents change (the
//! game may still touch their modification time). A stronger in-process
//! redirect was tried and dropped; see `notes/research/eu4-headless-testing.md`
//! and the `CreateFileW`-redirect route noted there. The caller turns a
//! [`SessionOutcome`] into a `foch_test::RunArtifacts` for judging; this crate
//! performs the I/O that `foch-test` deliberately does not.

pub mod layout;
mod platform;
pub mod runtime;

use runtime::RuntimeLayer;

use foch::game::eu4::Eu4;
use foch::game::eu4::base::snapshot::resolve_game_root;
use foch::input::Config;
use foch_test::RunnerExit;
use foch_test::compile::Bundle;
use foch_test::judge::Timing;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// A mod the mod under test declares a dependency on, resolved to its
/// installed directory. Loaded read-only before the source mod so the mod's
/// content that relies on it resolves.
#[derive(Clone, Debug)]
pub struct DependencyMod {
	pub name: String,
	pub path: PathBuf,
}

/// The game installation to launch and where to stage the throwaway runtime.
#[derive(Clone, Debug)]
pub struct Installation {
	/// EU4 install root (read-only). Resolve with [`locate_game`].
	pub game_root: PathBuf,
	/// Short-path directory under which per-session runtime layers are built,
	/// kept short so game files stay within `MAX_PATH`.
	pub runtime_base: PathBuf,
	/// The player's real EU4 user directory to guard, if known.
	pub real_user_dir: Option<PathBuf>,
}

/// Everything that varies per run.
#[derive(Clone, Debug)]
pub struct RunOptions {
	pub timeout: Duration,
	/// Set from outside (e.g. a Ctrl-C handler) to stop the current launch. The
	/// game is stopped gracefully and the runtime layer is still torn down.
	pub cancel: Arc<AtomicBool>,
	/// Load the Foch in-game host and the enabled plugins of one playset
	/// into every session's runtime layer, mirroring `foch plugin launch`.
	/// The host's own events file is reported through
	/// [`SessionOutcome::host_events`].
	pub host: Option<HostLaunch>,
}

/// The plugin host to load into test sessions.
#[derive(Clone, Debug)]
pub struct HostLaunch {
	/// The playset whose enabled plugins stage into each session.
	pub playset: String,
	/// The independent host DLL to deploy as the layer's `VERSION.dll`.
	pub host_dll: PathBuf,
}

impl Default for RunOptions {
	fn default() -> Self {
		Self {
			timeout: Duration::from_secs(300),
			cancel: Arc::new(AtomicBool::new(false)),
			host: None,
		}
	}
}

/// Remove runtime layers left under `runtime_base` by earlier runs that exited
/// without their [`RuntimeLayer`] drop (a crash or a hard kill). Only entries
/// older than `max_age` are removed, so a concurrent run's fresh layer is never
/// touched. Junctions are unlinked rather than followed, so linked game data is
/// never deleted. Best-effort: inaccessible entries are left alone.
pub fn sweep_runtime_base(runtime_base: &Path, max_age: Duration) {
	let now = SystemTime::now();
	let Ok(entries) = fs::read_dir(runtime_base) else {
		return;
	};
	for entry in entries.flatten() {
		let Ok(metadata) = entry.metadata() else {
			continue;
		};
		if metadata.is_dir()
			&& ordinary_runtime_marker(&entry.path().join("foch-runtime"))
			&& fs::symlink_metadata(entry.path().join("foch-player-runtime"))
				.is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
			&& metadata
				.modified()
				.is_ok_and(|modified| is_stale(modified, now, max_age))
		{
			platform::remove_layer(&entry.path());
		}
	}
}

fn ordinary_runtime_marker(path: &Path) -> bool {
	fs::symlink_metadata(path).is_ok_and(|metadata| {
		let linked = metadata.file_type().is_symlink();
		#[cfg(windows)]
		let linked = {
			use std::os::windows::fs::MetadataExt;
			linked || metadata.file_attributes() & 0x400 != 0
		};
		metadata.is_file() && !linked
	})
}

fn is_stale(modified: SystemTime, now: SystemTime, max_age: Duration) -> bool {
	now.duration_since(modified).is_ok_and(|age| age > max_age)
}

/// What one launch produced, ready to turn into a `foch_test::RunArtifacts`.
#[derive(Clone, Debug)]
pub struct SessionOutcome {
	pub game_log: Option<String>,
	pub error_log: Option<String>,
	pub exit: RunnerExit,
	pub timing: Timing,
	/// The real user directory's guarded files were unchanged.
	pub profile_untouched: bool,
	/// Human-readable account of how the game was launched.
	pub launch_detail: String,
	/// The plugin host's events file, when [`RunOptions::host`] staged one.
	pub host_events: Option<PathBuf>,
	/// The identity stamped into this launch's host plan. One unique value
	/// per actual launch: re-running the same bundle produces a new one, so
	/// receipt judging binds to what really ran, never to the compiled
	/// bundle alone. `None` when no host was staged.
	pub host_launch_id: Option<String>,
	/// The OS process id the runner spawned, captured at spawn.
	pub launch_pid: u32,
	/// The Windows session the process ran in; `None` when the OS answer
	/// was unavailable.
	pub launch_session_id: Option<u32>,
}

/// What this built-in runner can provide, as verified against a real game.
/// Cases needing anything else are refused before launch.
pub fn capabilities() -> foch_test::compile::RunnerCapabilities {
	foch_test::compile::RunnerCapabilities {
		// Launching with `-start_tag` begins at EU4's default start; other
		// dates are not yet reachable. Only 1444.11.11 is verified.
		start_dates: vec![foch_test::model::GameDate::parse("1444.11.11").expect("valid date")],
		// The `ai` console command disables AI (verified: AI actions drop to
		// zero), giving deterministic runs.
		ai_off: true,
		// Several cases in one launch are not yet verified in a real game.
		shared_sessions: false,
	}
}

/// Resolve the EU4 install from Foch configuration and Steam, honoring an
/// explicit override first.
pub fn locate_game(config: &Config, explicit: Option<&Path>) -> Result<PathBuf, String> {
	if let Some(path) = explicit {
		return if path.is_dir() {
			Ok(path.to_path_buf())
		} else {
			Err(format!("game path {} is not a directory", path.display()))
		};
	}
	resolve_game_root(config, &Eu4)
		.ok_or_else(|| {
			"could not locate the EU4 install; pass --game-path or set game_path.eu4 / the Steam path".into()
		})
}

/// Files in the real user directory the game may rewrite; guarded by content.
const GUARDED_FILES: &[&str] = &[
	"settings.txt",
	"pdx_settings.txt",
	"dlc_load.json",
	"game_data.json",
];

#[derive(Debug)]
struct ProfileGuard {
	directory: PathBuf,
	before: BTreeMap<&'static str, Option<Vec<u8>>>,
	backups: BTreeMap<&'static str, Vec<u8>>,
}

impl ProfileGuard {
	fn capture(directory: &Path) -> Self {
		let mut before = BTreeMap::new();
		let mut backups = BTreeMap::new();
		for &name in GUARDED_FILES {
			let content = fs::read(directory.join(name)).ok();
			if let Some(bytes) = &content {
				backups.insert(name, bytes.clone());
			}
			before.insert(name, content);
		}
		Self {
			directory: directory.to_path_buf(),
			before,
			backups,
		}
	}

	/// Returns whether the guarded files are unchanged, restoring any that the
	/// game rewrote with different contents.
	fn verify_and_restore(&self) -> bool {
		let mut untouched = true;
		for (&name, original) in &self.before {
			let path = self.directory.join(name);
			let current = fs::read(&path).ok();
			if &current == original {
				continue;
			}
			untouched = false;
			// Restore a file that existed before; delete one the game newly
			// created. A failure here leaves the real profile changed, so the
			// session stays flagged (untouched = false).
			let restored = match self.backups.get(name) {
				Some(bytes) => fs::write(&path, bytes),
				None => match fs::remove_file(&path) {
					Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
					other => other,
				},
			};
			if let Err(error) = restored {
				eprintln!("warning: could not restore {}: {error}", path.display());
			}
		}
		untouched
	}
}

/// Build the runtime layer, launch the game, wait for every case to finish (or
/// time out), collect the logs, and tear the layer down. `session_dir` is where
/// the throwaway profile and logs live; `source_mod` is the mod under test.
pub fn run_session(
	bundle: &Bundle,
	source_mod: &Path,
	dependencies: &[DependencyMod],
	session_dir: &Path,
	installation: &Installation,
	options: &RunOptions,
) -> io::Result<SessionOutcome> {
	let runtime = assemble_runtime(bundle, &installation.game_root, &installation.runtime_base)?;
	if let Some(message) = runtime.budget_error() {
		return Err(io::Error::other(message));
	}
	// One unique identity per actual launch: the compiled namespace is
	// deterministic across re-runs, so it cannot distinguish them.
	let launch_id = format!(
		"{}_{:x}",
		bundle.namespace,
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap_or_default()
			.as_nanos()
	);
	let (host_events, host_launch_id) = match &options.host {
		Some(host) => {
			let events = stage_host(&runtime, installation, host, &launch_id)?;
			(Some(events), Some(launch_id))
		}
		None => (None, None),
	};
	let user_dir = session_dir.join("userdir");
	let real_settings = installation
		.real_user_dir
		.as_ref()
		.map(|dir| dir.join("settings.txt"));
	assemble_profile(
		bundle,
		source_mod,
		dependencies,
		&user_dir,
		real_settings.as_deref(),
	)?;

	let guard = installation
		.real_user_dir
		.as_ref()
		.filter(|dir| dir.is_dir())
		.map(|dir| ProfileGuard::capture(dir));

	let launch = platform::LaunchSpec {
		exe: runtime.directory.join("eu4.exe"),
		working_dir: runtime.directory.clone(),
		args: launch_args(bundle, &user_dir),
	};
	let log = user_dir.join("logs").join("game.log");
	let error_log = user_dir.join("logs").join("error.log");

	let started = Instant::now();
	let mut process = platform::spawn(&launch)?;
	let identity = process.identity();
	let launch_detail = launch.detail();
	// Persist the launch identity while the child handle is alive, so even a
	// process that dies within seconds leaves a truthful record. A failure
	// here runs the full teardown — stop and wait the owned child, then the
	// guarded-profile check the normal path performs — before returning.
	let diagnostics_path = session_dir.join("launch-diagnostics.json");
	let record = serde_json::json!({
		"pid": identity.pid,
		"session_id": identity.session_id,
		"launch": launch_detail,
		"working_dir": launch.working_dir,
		"args": launch.args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
		// Stdio is null by design; capturing could block a chatty child, so
		// the record states the absence instead of risking it.
		"stdout_stderr": "not-captured",
	});
	if let Err(error) = write_diagnostics(&diagnostics_path, &record) {
		let error = diagnostic_write_failed(&mut process, guard.as_ref(), error);
		// The runtime layer (dropped on this return) holds the only copy of
		// the host's events; salvage what is readable before it goes.
		if let Some(source) = host_events.as_deref() {
			let _ = keep_host_events(source, session_dir);
		}
		return Err(error);
	}
	let (exit, termination) = watch(&mut process, bundle, &log, options.timeout, &options.cancel);
	// The termination account is diagnostics: a failure to persist it warns
	// but never unwinds a finished session, so the guarded-profile check and
	// log collection below always run.
	let record = serde_json::json!({
		"pid": identity.pid,
		"session_id": identity.session_id,
		"launch": launch_detail,
		"working_dir": launch.working_dir,
		"args": launch.args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
		"stdout_stderr": "not-captured",
		"termination": termination,
	});
	if let Err(error) = write_diagnostics(&diagnostics_path, &record) {
		eprintln!("warning: could not persist launch termination diagnostics: {error}");
	}
	let timing = Timing {
		wall_ms: started.elapsed().as_millis() as u64,
		..Timing::default()
	};

	let profile_untouched = guard
		.as_ref()
		.map(ProfileGuard::verify_and_restore)
		.unwrap_or(true);
	// The plugin host writes its events inside the runtime layer, which is
	// removed below; keep the evidence beside the session's other logs.
	let host_events = host_events
		.as_ref()
		.map(|source| keep_host_events(source, session_dir));
	let outcome = SessionOutcome {
		game_log: fs::read_to_string(&log).ok(),
		error_log: fs::read_to_string(&error_log).ok(),
		exit,
		timing,
		profile_untouched,
		launch_detail,
		host_events,
		host_launch_id,
		launch_pid: identity.pid,
		launch_session_id: identity.session_id,
	};
	// `runtime` is dropped here, which removes the layer.
	Ok(outcome)
}

/// Launch args: the throwaway user directory plus the bundle's own args.
fn launch_args(bundle: &Bundle, user_dir: &Path) -> Vec<OsString> {
	let mut args = vec![OsString::from(format!("-userdir={}", user_dir.display()))];
	args.extend(bundle.launch.args.iter().map(OsString::from));
	args
}

/// Wait until the game logs an `END` for every case, exits, or times out.
/// The [`RunnerExit`] keeps its judgment meaning; the returned
/// [`TerminationDetail`] records what actually ended the process, including
/// the underlying exit record after a runner-driven stop.
fn watch(
	process: &mut platform::GameProcess,
	bundle: &Bundle,
	log: &Path,
	timeout: Duration,
	cancel: &AtomicBool,
) -> (RunnerExit, TerminationDetail) {
	let started = Instant::now();
	loop {
		if cancel.load(Ordering::Relaxed) {
			process.stop();
			return (
				RunnerExit::Cancelled,
				TerminationDetail::stopped("cancel", process),
			);
		}
		if let Some(record) = process.exited() {
			let exit = if record.success {
				RunnerExit::Success
			} else {
				// A signal death has no exit code; the record keeps `None`
				// instead of rewriting an unknown termination to zero.
				RunnerExit::Failed { code: record.code }
			};
			return (
				exit,
				TerminationDetail {
					cause: "natural-exit",
					code: record.code,
					success: Some(record.success),
				},
			);
		}
		if let Ok(text) = fs::read_to_string(log)
			&& layout::all_cases_ended(&text, bundle)
		{
			process.stop();
			// A semantic success: the cases finished, and the runner stops
			// the process. The underlying record after the stop is still
			// written out, kill code and all.
			return (
				RunnerExit::Success,
				TerminationDetail::stopped("end-stop", process),
			);
		}
		if started.elapsed() >= timeout {
			process.stop();
			return (
				RunnerExit::TimedOut {
					seconds: timeout.as_secs(),
				},
				TerminationDetail::stopped("timeout", process),
			);
		}
		std::thread::sleep(Duration::from_millis(200));
	}
}

/// How the process actually ended, persisted with the launch diagnostics.
#[derive(serde::Serialize)]
struct TerminationDetail {
	/// One of `natural-exit`, `end-stop`, `timeout`, `cancel`.
	cause: &'static str,
	/// The exit code when the OS reports one; `None` for signal deaths and
	/// runner-driven kills that report none.
	code: Option<i32>,
	/// Whether the OS calls the final state a success; a kill is not.
	success: Option<bool>,
}

impl TerminationDetail {
	/// The detail after the runner stopped the process itself: read back
	/// what the OS now reports for the killed child.
	fn stopped(cause: &'static str, process: &mut platform::GameProcess) -> Self {
		let record = process.exited();
		Self {
			cause,
			code: record.and_then(|record| record.code),
			success: record.map(|record| record.success),
		}
	}
}

/// The teardown for a failed post-spawn diagnostics write: stop and wait
/// the just-spawned owned child, then run the guarded-profile restore the
/// normal session path performs — the guard has no `Drop`, so this funnel
/// is the only thing standing between a failed write and a leaked process
/// or a modified player profile — and only then hand back the error.
fn diagnostic_write_failed(
	process: &mut platform::GameProcess,
	guard: Option<&ProfileGuard>,
	error: io::Error,
) -> io::Error {
	process.stop();
	let untouched = guard.map(ProfileGuard::verify_and_restore).unwrap_or(true);
	if !untouched {
		// `verify_and_restore` already restored what it could; `false`
		// means the session touched the guarded files at all.
		eprintln!("warning: the guarded profile was touched; restored where possible");
	}
	io::Error::other(format!("cannot write launch diagnostics: {error}"))
}

fn write_diagnostics(path: &Path, record: &serde_json::Value) -> io::Result<()> {
	fs::write(path, serde_json::to_vec_pretty(record).unwrap_or_default())
}

/// Copy the host's events out of the runtime layer (which is about to be
/// removed) beside the session's other logs. Best effort: when a byte cannot
/// be moved, the original path stays so the caller still has the only copy.
fn keep_host_events(source: &Path, session_dir: &Path) -> PathBuf {
	let kept = session_dir.join("foch-host-events.jsonl");
	match fs::read(source) {
		Ok(bytes) => fs::write(&kept, bytes)
			.map(|()| kept)
			.unwrap_or_else(|_| source.to_path_buf()),
		Err(_) => source.to_path_buf(),
	}
}

/// Write the throwaway profile inside `user_dir`: the generated test layer
/// (written from the bundle's own files), descriptors enabling the source mod
/// and that layer, a `dlc_load.json`, and a windowed/muted settings file.
fn assemble_profile(
	bundle: &Bundle,
	source_mod: &Path,
	dependencies: &[DependencyMod],
	user_dir: &Path,
	real_settings: Option<&Path>,
) -> io::Result<()> {
	let mod_dir = user_dir.join("mod");
	fs::create_dir_all(&mod_dir)?;
	// The runner owns this copy of the test layer, so it never aliases a copy
	// the CLI may also have written beside the bundle.
	let test_mod = user_dir.join("test_mod");
	for (relative, content) in &bundle.files {
		let Some(rest) = relative.strip_prefix("test_mod/") else {
			continue;
		};
		let destination = test_mod.join(rest);
		if let Some(parent) = destination.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::write(destination, content)?;
	}

	// Enable declared dependencies first, then the source mod, then the test
	// layer last so its events win any tie.
	let mut enabled = Vec::new();
	for (index, dependency) in dependencies.iter().enumerate() {
		let file = format!("dep_{index:03}.mod");
		write_descriptor(&mod_dir.join(&file), &dependency.name, &dependency.path)?;
		enabled.push(file);
	}
	write_descriptor(
		&mod_dir.join("source_mod.mod"),
		"Foch source mod",
		source_mod,
	)?;
	enabled.push("source_mod.mod".to_string());
	write_descriptor(
		&mod_dir.join("test_mod.mod"),
		&bundle.test_mod_name,
		&test_mod,
	)?;
	enabled.push("test_mod.mod".to_string());
	let enabled: Vec<&str> = enabled.iter().map(String::as_str).collect();
	fs::write(user_dir.join("dlc_load.json"), layout::dlc_load(&enabled))?;

	if let Some(path) = real_settings
		&& let Ok(template) = fs::read_to_string(path)
	{
		fs::write(
			user_dir.join("settings.txt"),
			layout::isolated_settings(&template),
		)?;
	}
	Ok(())
}

fn write_descriptor(path: &Path, name: &str, mod_path: &Path) -> io::Result<()> {
	let normalized = mod_path.to_string_lossy().replace('\\', "/");
	fs::write(path, format!("name=\"{name}\"\npath=\"{normalized}\"\n"))
}

fn assemble_runtime(
	bundle: &Bundle,
	game_root: &Path,
	runtime_base: &Path,
) -> io::Result<RuntimeLayer> {
	let runtime = RuntimeLayer::prepare(game_root, runtime_base, &bundle.namespace)?;
	foch::plugin::store::replace_package_file(
		&runtime.directory.join("commands.txt"),
		layout::command_file(bundle, bundle.requires.ai_off).as_bytes(),
	)?;
	for (relative, content) in &bundle.files {
		if relative.ends_with("_start.txt") && !relative.contains('/') {
			foch::plugin::store::replace_package_file(
				&runtime.directory.join(relative),
				content.as_bytes(),
			)?;
		}
	}
	Ok(runtime)
}

/// Deploy the plugin host into a fresh session runtime layer: write the
/// host DLL as the layer's `VERSION.dll` and stage the playset's enabled
/// plugins exactly as `foch plugin launch` does. Returns the host's events
/// file for the caller to report. The layer is freshly prepared, so the
/// exclusive `VERSION.dll` create cannot clash with a game file.
fn stage_host(
	runtime: &RuntimeLayer,
	installation: &Installation,
	host: &HostLaunch,
	run_id: &str,
) -> io::Result<PathBuf> {
	if !cfg!(all(windows, target_arch = "x86_64")) {
		return Err(io::Error::other(
			"the plugin host only runs on Windows x64 game builds",
		));
	}
	use foch::plugin::{self, GameIdentity, Selections, planner, store};

	let raw_version = foch::game::eu4::base::snapshot::detect_game_version(&installation.game_root)
		.ok_or_else(|| io::Error::other("could not detect the EU4 version"))?;
	let game = GameIdentity {
		game: "eu4".into(),
		version: planner::parse_game_version(&raw_version)
			.ok_or_else(|| io::Error::other("invalid EU4 version"))?,
		platform: planner::WINDOWS_X64.into(),
	};
	let selections_path =
		plugin::paths::selections_path().map_err(|error| io::Error::other(error.to_string()))?;
	let selections = Selections::load(&selections_path)
		.map_err(|error| io::Error::other(format!("plugin selections: {error}")))?;
	let selected = selections.for_playset(&host.playset).to_selections();
	let data_root = foch::game::eu4::base::snapshot::data_root();
	let store_root = plugin::paths::checked_store_root(&data_root)
		.map_err(|error| io::Error::other(error.to_string()))?;
	runtime::ensure_outside_game(&installation.game_root, &data_root)?;
	let deployment = plugin::deployment::resolve(&game, &store_root, &selected)
		.map_err(|error| io::Error::other(format!("plugin plan: {error}")))?;
	let host_bytes = crate::host::read(&host.host_dll)?;
	store::create_package_file(&runtime.directory.join("VERSION.dll"), &host_bytes)?;
	let prepared = deployment
		.stage(
			&runtime.directory,
			&data_root,
			run_id,
			&installation.game_root,
		)
		.map_err(|error| io::Error::other(format!("plugin staging: {error}")))?;
	Ok(PathBuf::from(prepared.plan.events))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[cfg(any(unix, windows))]
	#[test]
	fn sweeping_requires_an_ordinary_runtime_marker_and_preserves_other_contents() {
		let scratch = tempfile::tempdir().unwrap();
		let base = scratch.path().join("runtimes");
		let source = scratch.path().join("game");
		fs::create_dir(&source).unwrap();
		fs::create_dir(&base).unwrap();
		let external = scratch.path().join("external-marker");
		fs::write(&external, b"external bytes").unwrap();
		for name in ["linked", "dangling", "directory-marker", "unmarked"] {
			let directory = base.join(name);
			fs::create_dir(&directory).unwrap();
			fs::write(directory.join("unrelated"), b"keep these bytes").unwrap();
			let marker = directory.join("foch-runtime");
			match name {
				"linked" | "dangling" => {
					let target = if name == "linked" {
						external.clone()
					} else {
						scratch.path().join("missing")
					};
					#[cfg(unix)]
					std::os::unix::fs::symlink(&target, &marker).unwrap();
					#[cfg(windows)]
					std::os::windows::fs::symlink_file(&target, &marker).unwrap();
				}
				"directory-marker" => fs::create_dir(&marker).unwrap(),
				_ => {}
			}
		}
		let owned = RuntimeLayer::prepare(&source, &base, "owned").unwrap();
		sweep_runtime_base(&base, Duration::ZERO);
		assert!(!owned.directory.exists());
		for name in ["linked", "dangling", "directory-marker", "unmarked"] {
			assert_eq!(
				fs::read(base.join(name).join("unrelated"))
					.unwrap_or_else(|error| panic!("swept {name}: {error}")),
				b"keep these bytes",
				"swept {name}"
			);
		}
		assert_eq!(fs::read(&external).unwrap(), b"external bytes");
		assert_eq!(fs::read_dir(&source).unwrap().count(), 0);
	}

	#[test]
	fn sweeping_preserves_every_existing_player_marker_entry() {
		let scratch = tempfile::tempdir().unwrap();
		let source = scratch.path().join("game");
		let base = scratch.path().join("runtimes");
		fs::create_dir(&source).unwrap();
		let external = scratch.path().join("external-marker");
		fs::write(&external, b"external bytes").unwrap();
		for name in ["file", "directory", "link", "dangling", "absent"] {
			let mut layer = RuntimeLayer::prepare(&source, &base, name).unwrap();
			let directory = layer.retain().unwrap();
			fs::write(directory.join("keep"), b"player bytes").unwrap();
			let marker = directory.join("foch-player-runtime");
			if name != "file" {
				fs::remove_file(&marker).unwrap();
			}
			match name {
				"directory" => fs::create_dir(&marker).unwrap(),
				"link" | "dangling" => {
					let target = if name == "link" {
						external.clone()
					} else {
						scratch.path().join("missing")
					};
					#[cfg(unix)]
					std::os::unix::fs::symlink(&target, &marker).unwrap();
					#[cfg(windows)]
					std::os::windows::fs::symlink_file(&target, &marker).unwrap();
				}
				_ => {}
			}
			sweep_runtime_base(&base, Duration::ZERO);
			if name == "absent" {
				assert!(!directory.exists(), "absent marker prevented sweeping");
			} else {
				assert_eq!(
					fs::read(directory.join("keep"))
						.unwrap_or_else(|error| panic!("swept {name} player: {error}")),
					b"player bytes"
				);
				platform::remove_layer(&directory);
			}
			drop(layer);
		}
		assert_eq!(fs::read(&external).unwrap(), b"external bytes");
		assert_eq!(fs::read_dir(&source).unwrap().count(), 0);
	}

	#[test]
	fn stale_layers_are_older_than_the_grace_period() {
		let now = SystemTime::now();
		let max_age = Duration::from_secs(3600);
		// A layer touched two hours ago is stale; one from a minute ago is not.
		assert!(is_stale(now - Duration::from_secs(7200), now, max_age));
		assert!(!is_stale(now - Duration::from_secs(60), now, max_age));
		// A modification time in the future (clock skew) is never stale.
		assert!(!is_stale(now + Duration::from_secs(60), now, max_age));
	}
}

#[cfg(test)]
mod launch_diagnostics {
	use super::*;
	// within seconds must still leave a truthful identity and exit record, a
	// watch-driven stop must be distinguishable from a natural exit, and the
	// missing game log stays a runner-level runtime error — never a pass.

	use super::platform::{self, LaunchSpec};
	use std::path::PathBuf;
	use std::time::Duration;

	fn fake_child(exit_code: i32) -> LaunchSpec {
		let program = if cfg!(windows) { "cmd" } else { "sh" };
		let flag = if cfg!(windows) { "/C" } else { "-c" };
		let script = format!("exit {exit_code}");
		let mut dir = std::env::temp_dir();
		dir.push("foch-launch-diagnostics");
		std::fs::create_dir_all(&dir).unwrap();
		LaunchSpec {
			exe: PathBuf::from(program),
			working_dir: dir,
			args: vec![flag.into(), script.into()],
		}
	}

	/// The launcher may inject Steam env vars; a fake child tolerates them.
	#[test]
	fn a_fast_exiting_child_still_records_identity_and_real_exit_code() {
		let mut process = platform::spawn(&fake_child(3)).unwrap();
		let identity = process.identity();
		assert!(identity.pid != 0, "the OS assigned a real pid");
		if cfg!(windows) {
			assert!(
				identity.session_id.is_some(),
				"a live child has a Windows session id"
			);
		} else {
			assert_eq!(identity.session_id, None);
		}
		// Wait for the natural exit: code 3, not a rewritten zero.
		let mut record = None;
		for _ in 0..100 {
			if let Some(found) = process.exited() {
				record = Some(found);
				break;
			}
			std::thread::sleep(Duration::from_millis(20));
		}
		let record = record.expect("the child exits on its own");
		assert_eq!(record.code, Some(3));
		assert!(!record.success);
	}

	#[test]
	fn success_only_means_a_zero_exit() {
		let mut process = platform::spawn(&fake_child(0)).unwrap();
		let mut record = None;
		for _ in 0..100 {
			if let Some(found) = process.exited() {
				record = Some(found);
				break;
			}
			std::thread::sleep(Duration::from_millis(20));
		}
		let record = record.expect("the child exits on its own");
		assert_eq!(record.code, Some(0));
		assert!(record.success);
	}

	/// A watch-style stop produces no natural exit record while alive, and the
	/// caller keeps the distinction (TimedOut/Cancelled) at its own level.
	#[test]
	fn a_stopped_child_leaves_no_false_success() {
		let mut process = platform::spawn(&idle_spec()).unwrap();
		assert!(
			process.exited().is_none(),
			"an idle child has not exited; the watch timeout owns the stop"
		);
		process.stop();
		let record = process.exited().expect("stop() reaps the child");
		assert!(!record.success, "a kill is never a pass");
	}

	/// The runner maps a clean process exit with no game log to a runtime
	/// error, not a pass — pin the judgment at the boundary the CLI relies on.
	#[test]
	fn a_clean_exit_without_game_log_still_means_runtime_error() {
		use foch_test::{RunArtifacts, RunnerExit, Status, Timing, judge};
		// Fabricate the smallest judged session the way run_session would for a
		// 1.6s vanish: exit Success, no logs. Reuse the pipeline fixtures' shape
		// via the public compile+judge API on a one-case plan.
		let sources = vec![foch_test::SourceFile {
			mod_name: "Diag".into(),
			path: foch_test::RelPath::new("tests/a.txt").unwrap(),
			kind: foch_test::SourceKind::TestsDir,
			text: r#"test = { name = probe time = 1444.11.11 tag = SWE
		effect = { set_country_flag = ready }
		expect = { has_country_flag = ready } }"#
				.into(),
		}];
		let collection = foch_test::collect(&sources);
		assert!(
			collection.diagnostics.is_empty(),
			"{:#?}",
			collection.diagnostics
		);
		let expanded =
			foch_test::expand(&collection, &foch_test::ExpandOptions::default()).unwrap();
		let linted = foch_test::lint(&expanded, &foch_test::ProjectFacts::default());
		let plan = foch_test::group(expanded, &linted, &foch_test::GroupOptions::default());
		let context = foch_test::RunContext {
			run_id: foch_test::model::RunId::parse("run-1").unwrap(),
			mod_name: "Diag".into(),
		};
		let bundle = foch_test::compile(&plan, plan.sessions[0].id, &context).unwrap();
		let artifacts = RunArtifacts {
			game_log: None,
			error_log: None,
			exit: RunnerExit::Success,
			timing: Timing {
				wall_ms: 1600,
				..Timing::default()
			},
			profile_untouched: true,
			host_events: None,
			host_launch_id: None,
		};
		let results = judge(&bundle, &plan, &artifacts);
		assert_eq!(results[0].status, Status::RuntimeError);
		assert!(
			results[0]
				.notes
				.iter()
				.any(|note| note.contains("game.log")),
			"the missing log is named: {:#?}",
			results[0].notes
		);
	}

	/// The exact funnel `run_session` uses when the post-spawn diagnostics
	/// write fails: stop and wait the owned child, restore the guarded
	/// profile the normal session path restores, then propagate. A fake
	/// profile with a rewritten guarded file and a controlled write failure
	/// — the real user profile is never touched.
	#[test]
	fn a_failed_diagnostics_write_still_stops_the_child_and_restores_the_guard() {
		let scratch = tempfile::tempdir().unwrap();
		let profile = scratch.path().join("profile");
		fs::create_dir(&profile).unwrap();
		let guarded = profile.join("settings.txt");
		fs::write(&guarded, "original\n").unwrap();
		let guard = ProfileGuard::capture(&profile);
		// The game rewrote a guarded file before the diagnostics write fails.
		fs::write(&guarded, "rewritten by the game\n").unwrap();

		let mut process = platform::spawn(&idle_spec()).unwrap();
		assert!(
			process.exited().is_none(),
			"the child is alive when the failure path starts"
		);
		// Controlled I/O failure: the diagnostics target is a directory.
		let session = scratch.path().join("session");
		let blocked = session.join("launch-diagnostics.json");
		fs::create_dir_all(&blocked).unwrap();
		let record = serde_json::json!({ "pid": process.identity().pid });
		let error = write_diagnostics(&blocked, &record)
			.expect_err("the directory target forces the write failure this drives");

		let returned = diagnostic_write_failed(&mut process, Some(&guard), error);
		assert!(
			returned.to_string().contains("launch diagnostics"),
			"the propagated error names the failed write: {returned}"
		);
		assert!(
			process.exited().is_some(),
			"the child is stopped and waited, not leaked"
		);
		assert_eq!(
			fs::read_to_string(&guarded).unwrap(),
			"original\n",
			"the guarded file is restored even on the early-error path"
		);
	}

	/// Every way the real `watch()` loop can end — natural exit, an END-driven
	/// stop, a timeout, a cancel — returns the same [`RunnerExit`] judgment it
	/// always did, plus a truthful termination detail with the record the OS
	/// reports after the runner-driven stop.
	#[test]
	fn watch_records_every_termination_cause_with_the_real_exit_record() {
		use foch_test::compile::{Launch, Requirements};
		use foch_test::model::{AiMode, CaseIndex, GameDate, RunId, Tag};
		use foch_test::plan::SessionId;

		fn one_case_bundle() -> Bundle {
			let date = GameDate::parse("1444.11.11").unwrap();
			Bundle {
				protocol: 2,
				run_id: RunId::parse("run-1").unwrap(),
				mod_name: "demo".into(),
				session: SessionId(0),
				cases: vec![CaseIndex(0)],
				marker_prefix: "FOCH_TEST_V2 run-1 abc123".into(),
				namespace: "foch_test_demo_abc123".into(),
				test_mod_name: "demo tests [abc123]".into(),
				files: BTreeMap::new(),
				launch: Launch {
					args: vec!["-start_tag=SWE".into()],
					ai: AiMode::Off,
					player: Tag::parse("SWE").unwrap(),
					start_date: date,
					end_date: date,
				},
				checks: Vec::new(),
				windows: Vec::new(),
				requires: Requirements {
					start_date: date,
					ai_off: false,
					shared_session: false,
				},
			}
		}

		let scratch = tempfile::tempdir().unwrap();
		let log = scratch.path().join("game.log");
		let cancel = AtomicBool::new(false);

		// Natural exit: the child dies on its own with code 3 before any END.
		let mut child = platform::spawn(&fake_child(3)).unwrap();
		let (exit, detail) = watch(
			&mut child,
			&one_case_bundle(),
			&log,
			Duration::from_secs(30),
			&cancel,
		);
		assert_eq!(exit, RunnerExit::Failed { code: Some(3) });
		assert_eq!(detail.cause, "natural-exit");
		assert_eq!(detail.code, Some(3));
		assert_eq!(detail.success, Some(false));

		// END-driven stop: the log already ends the only case, so watch stops
		// the idle child itself. The judgment stays Success; the underlying
		// kill record is still written out truthfully.
		let bundle = one_case_bundle();
		fs::write(
			&log,
			format!(
				"[x]: EVENT [1444.11.11]:{} END\n",
				bundle.case_prefix(CaseIndex(0))
			),
		)
		.unwrap();
		let mut idle = platform::spawn(&idle_spec()).unwrap();
		let (exit, detail) = watch(&mut idle, &bundle, &log, Duration::from_secs(30), &cancel);
		assert_eq!(exit, RunnerExit::Success);
		assert_eq!(detail.cause, "end-stop");
		assert_eq!(
			detail.success,
			Some(false),
			"a runner-driven kill is never a success"
		);
		// The termination account persists the way run_session rewrites the
		// record after watch: the cause and the honest kill record are
		// readable back from the session directory.
		let mut persisted = serde_json::json!({ "pid": idle.identity().pid });
		persisted["termination"] = serde_json::to_value(&detail).unwrap();
		let diagnostics = scratch.path().join("termination.json");
		write_diagnostics(&diagnostics, &persisted).unwrap();
		let read =
			serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&diagnostics).unwrap())
				.unwrap();
		assert_eq!(read["termination"]["cause"], "end-stop");
		assert_eq!(read["termination"]["success"], false);

		// Timeout: nothing ends the idle child before the deadline.
		fs::remove_file(&log).unwrap();
		let mut slow = platform::spawn(&idle_spec()).unwrap();
		let (exit, detail) = watch(
			&mut slow,
			&one_case_bundle(),
			&log,
			Duration::from_millis(500),
			&cancel,
		);
		assert_eq!(exit, RunnerExit::TimedOut { seconds: 0 });
		assert_eq!(detail.cause, "timeout");
		assert_eq!(detail.success, Some(false));

		// Cancel: the flag wins on the first poll.
		let mut stopped = platform::spawn(&idle_spec()).unwrap();
		let cancelled = AtomicBool::new(true);
		let (exit, detail) = watch(
			&mut stopped,
			&one_case_bundle(),
			&log,
			Duration::from_secs(30),
			&cancelled,
		);
		assert_eq!(exit, RunnerExit::Cancelled);
		assert_eq!(detail.cause, "cancel");
		assert_eq!(detail.success, Some(false));
	}

	/// A single owned child that stays alive until something stops it: the
	/// process itself idles (a loopback `ping` on Windows, `sleep` elsewhere),
	/// not a shell wrapping a descendant — a `stop()` must reach the real
	/// child, never kill only a wrapper and strand a grandchild, and `pause`
	/// would exit on the immediate EOF of its null stdin.
	fn idle_spec() -> LaunchSpec {
		let (exe, args): (&str, Vec<&str>) = if cfg!(windows) {
			("ping", vec!["-n", "30", "127.0.0.1"])
		} else {
			("sleep", vec!["30"])
		};
		let mut dir = std::env::temp_dir();
		dir.push("foch-launch-diagnostics");
		std::fs::create_dir_all(&dir).unwrap();
		LaunchSpec {
			exe: PathBuf::from(exe),
			working_dir: dir,
			args: args.into_iter().map(Into::into).collect(),
		}
	}
}
