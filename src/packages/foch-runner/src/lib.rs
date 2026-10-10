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
}

impl Default for RunOptions {
	fn default() -> Self {
		Self {
			timeout: Duration::from_secs(300),
			cancel: Arc::new(AtomicBool::new(false)),
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
			&& entry.path().join("foch-runtime").is_file()
			&& !entry.path().join("foch-player-runtime").exists()
			&& metadata
				.modified()
				.is_ok_and(|modified| is_stale(modified, now, max_age))
		{
			platform::remove_layer(&entry.path());
		}
	}
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
	let launch_detail = launch.detail();
	let exit = watch(&mut process, bundle, &log, options.timeout, &options.cancel);
	let timing = Timing {
		wall_ms: started.elapsed().as_millis() as u64,
		..Timing::default()
	};

	let profile_untouched = guard
		.as_ref()
		.map(ProfileGuard::verify_and_restore)
		.unwrap_or(true);
	let outcome = SessionOutcome {
		game_log: fs::read_to_string(&log).ok(),
		error_log: fs::read_to_string(&error_log).ok(),
		exit,
		timing,
		profile_untouched,
		launch_detail,
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
fn watch(
	process: &mut platform::GameProcess,
	bundle: &Bundle,
	log: &Path,
	timeout: Duration,
	cancel: &AtomicBool,
) -> RunnerExit {
	let started = Instant::now();
	loop {
		if cancel.load(Ordering::Relaxed) {
			process.stop();
			return RunnerExit::Cancelled;
		}
		if let Some(code) = process.exit_code() {
			return if code == 0 {
				RunnerExit::Success
			} else {
				RunnerExit::Failed { code: Some(code) }
			};
		}
		if let Ok(text) = fs::read_to_string(log)
			&& layout::all_cases_ended(&text, bundle)
		{
			process.stop();
			return RunnerExit::Success;
		}
		if started.elapsed() >= timeout {
			process.stop();
			return RunnerExit::TimedOut {
				seconds: timeout.as_secs(),
			};
		}
		std::thread::sleep(Duration::from_millis(200));
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
	fs::write(
		runtime.directory.join("commands.txt"),
		layout::command_file(bundle, bundle.requires.ai_off),
	)?;
	for (relative, content) in &bundle.files {
		if relative.ends_with("_start.txt") && !relative.contains('/') {
			fs::write(runtime.directory.join(relative), content)?;
		}
	}
	Ok(runtime)
}

#[cfg(test)]
mod tests {
	use super::*;

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
