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
		junction(target, link)
	}
	#[cfg(not(windows))]
	{
		std::os::unix::fs::symlink(target, link)
	}
}

#[cfg(windows)]
fn junction(target: &Path, link: &Path) -> io::Result<()> {
	use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
	use windows_sys::Win32::{
		Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT},
		System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_REPARSE_POINT},
	};
	let target = foch::plugin::deployment::path_text(&target.canonicalize()?)?.replace('/', "\\");
	if target.starts_with(r"\\") {
		return Err(io::Error::other(
			"directory junctions require a local target",
		));
	}
	let substitute: Vec<u16> = format!(r"\??\{target}").encode_utf16().collect();
	let print: Vec<u16> = target.encode_utf16().collect();
	let data_length = 8 + (substitute.len() + print.len() + 2) * 2;
	if data_length + 8 > 16_384 {
		return Err(io::Error::other(
			"junction target exceeds the reparse buffer limit",
		));
	}
	// REPARSE_DATA_BUFFER, mount-point variant (IO_REPARSE_TAG_MOUNT_POINT).
	// Offsets/lengths count bytes from PathBuffer, excluding terminators.
	let mut buffer: Vec<u16> = vec![
		0x0003,
		0xa000,
		data_length as u16,
		0,
		0,
		(substitute.len() * 2) as u16,
		((substitute.len() + 1) * 2) as u16,
		(print.len() * 2) as u16,
	];
	buffer.extend(substitute);
	buffer.push(0);
	buffer.extend(print);
	buffer.push(0);
	std::fs::create_dir(link)?;
	let result = (|| {
		let file = std::fs::OpenOptions::new()
			.read(true)
			.write(true)
			.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
			.open(link)?;
		let mut returned = 0;
		// The owned aligned buffer remains alive for this synchronous call.
		if unsafe {
			DeviceIoControl(
				file.as_raw_handle(),
				FSCTL_SET_REPARSE_POINT,
				buffer.as_ptr().cast(),
				(buffer.len() * 2) as u32,
				std::ptr::null_mut(),
				0,
				&mut returned,
				std::ptr::null_mut(),
			)
		} == 0
		{
			return Err(io::Error::last_os_error());
		}
		Ok(())
	})();
	if result.is_err() {
		let _ = std::fs::remove_dir(link);
	}
	result
}

/// Remove a runtime layer: unlink the linked directories first so their
/// targets are never touched, then delete the copied files.
pub fn remove_layer(directory: &Path) {
	let Ok(metadata) = std::fs::symlink_metadata(directory) else {
		return;
	};
	if metadata.file_type().is_symlink() || is_reparse_point(directory) {
		unlink(directory, metadata.is_dir());
		return;
	}
	let Ok(entries) = std::fs::read_dir(directory) else {
		return;
	};
	for entry in entries.flatten() {
		let Ok(file_type) = entry.file_type() else {
			continue;
		};
		if file_type.is_symlink() || (file_type.is_dir() && is_reparse_point(&entry.path())) {
			// A junction/symlink: removing the link leaves its target intact.
			unlink(&entry.path(), file_type.is_dir());
		} else if file_type.is_dir() {
			remove_layer(&entry.path());
		} else {
			let _ = std::fs::remove_file(entry.path());
		}
	}
	let _ = std::fs::remove_dir(directory);
}

fn unlink(path: &Path, directory: bool) {
	#[cfg(windows)]
	{
		use std::os::windows::fs::FileTypeExt;
		let directory = directory
			|| std::fs::symlink_metadata(path)
				.is_ok_and(|metadata| metadata.file_type().is_symlink_dir());
		if directory {
			let _ = std::fs::remove_dir(path);
			return;
		}
	}
	#[cfg(not(windows))]
	let _ = directory;
	let _ = std::fs::remove_file(path);
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
