//! Platform-specific launch, linking and teardown.
//!
//! The game runs as an ordinary child process; the runner keeps the player's
//! real user directory intact by backing up and verifying the files the game
//! rewrites (see the crate docs). A stronger in-process redirect was tried and
//! dropped — see `notes/research/eu4-headless-testing.md`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub struct LaunchSpec {
	pub exe: PathBuf,
	pub working_dir: PathBuf,
	pub args: Vec<OsString>,
}

impl LaunchSpec {
	pub fn detail(&self) -> String {
		let args: Vec<String> = self
			.args
			.iter()
			.map(|a| a.to_string_lossy().into_owned())
			.collect();
		format!("{} {}", self.exe.display(), args.join(" "))
	}
}

/// A launched game, observed until it finishes or is stopped.
pub struct GameProcess(Child);

impl GameProcess {
	/// The exit code if the process has already exited, else `None`.
	pub fn exit_code(&mut self) -> Option<i32> {
		self.0
			.try_wait()
			.ok()
			.flatten()
			.map(|status| status.code().unwrap_or(0))
	}

	/// Terminate the process. The runner owns only this process; a game that
	/// spawns helpers of its own is responsible for them.
	pub fn stop(&mut self) {
		let _ = self.0.kill();
		let _ = self.0.wait();
	}
}

pub fn spawn(spec: &LaunchSpec) -> io::Result<GameProcess> {
	let mut command = Command::new(&spec.exe);
	command
		.current_dir(&spec.working_dir)
		.args(&spec.args)
		// Run without the Steam client; the game still initializes with its
		// app id present in the environment and the runtime layer's
		// `steam_appid.txt`.
		.env("SteamAppId", "236850")
		.env("SteamGameId", "236850")
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null());
	Ok(GameProcess(command.spawn()?))
}

/// Link a game data directory into the runtime layer without copying it.
pub fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
	#[cfg(windows)]
	{
		// A directory junction needs no symlink privilege.
		let status = Command::new("cmd")
			.args(["/c", "mklink", "/J"])
			.arg(link)
			.arg(target)
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.status()?;
		if status.success() {
			Ok(())
		} else {
			Err(io::Error::other(format!(
				"failed to create junction {} -> {}",
				link.display(),
				target.display()
			)))
		}
	}
	#[cfg(not(windows))]
	{
		std::os::unix::fs::symlink(target, link)
	}
}

/// Remove a runtime layer: unlink the linked directories first so their
/// targets are never touched, then delete the copied files.
pub fn remove_layer(directory: &Path) {
	let Ok(entries) = std::fs::read_dir(directory) else {
		return;
	};
	for entry in entries.flatten() {
		let Ok(file_type) = entry.file_type() else {
			continue;
		};
		if file_type.is_symlink() || (file_type.is_dir() && is_reparse_point(&entry.path())) {
			// A junction/symlink: removing the link leaves its target intact.
			let _ = std::fs::remove_dir(entry.path());
		} else if file_type.is_dir() {
			let _ = std::fs::remove_dir_all(entry.path());
		} else {
			let _ = std::fs::remove_file(entry.path());
		}
	}
	let _ = std::fs::remove_dir(directory);
}

#[cfg(windows)]
fn is_reparse_point(path: &Path) -> bool {
	use std::os::windows::fs::MetadataExt;
	const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
	std::fs::symlink_metadata(path)
		.map(|meta| meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
		.unwrap_or(false)
}

#[cfg(not(windows))]
fn is_reparse_point(_path: &Path) -> bool {
	false
}
