use super::super::super::error::MergeError;
use remove_dir_all::RemoveDir;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static OUTPUT_TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Publishes a fully staged output tree without exposing partial contents.
///
/// This protects process-level atomicity and cooperating-writer serialization.
/// It does not fsync staged files or parent directories, so it is not a
/// power-loss durability transaction.
pub(crate) struct OutputTransaction {
	final_dir: PathBuf,
	initial_target: OutputTargetState,
	staging_identity: OutputDirectoryIdentity,
	staging_dir: Option<PathBuf>,
	_lock_file: File,
}

#[derive(Debug, Eq, PartialEq)]
enum OutputTargetState {
	Missing,
	Directory(OutputDirectoryIdentity),
}

#[derive(Debug, Eq, PartialEq)]
struct OutputDirectoryIdentity(same_file::Handle);

impl OutputTransaction {
	pub(crate) fn begin(final_dir: &Path) -> Result<Self, MergeError> {
		ensure_transaction_platform_supported()?;
		let parent = output_parent(final_dir);
		fs::create_dir_all(parent)?;
		let lock_file = acquire_output_lock(final_dir)?;
		let initial_target = inspect_output_target(final_dir)?;
		ensure_target_replacement_supported(&initial_target)?;
		let staging_dir = create_unique_sibling_dir(final_dir, "staging")?;
		let staging_identity = match inspect_output_target(&staging_dir) {
			Ok(OutputTargetState::Directory(identity)) => identity,
			Ok(OutputTargetState::Missing) => unreachable!("new staging directory disappeared"),
			Err(error) => {
				eprintln!(
					"[merge] warning: could not verify ownership of new staging directory {}; leaving it untouched: {error}",
					staging_dir.display()
				);
				return Err(error);
			}
		};
		Ok(Self {
			final_dir: final_dir.to_path_buf(),
			initial_target,
			staging_identity,
			staging_dir: Some(staging_dir),
			_lock_file: lock_file,
		})
	}

	pub(crate) fn staging_dir(&self) -> &Path {
		self.staging_dir
			.as_deref()
			.expect("published output transaction has no staging directory")
	}

	pub(crate) fn prior_dir(&self) -> Option<&Path> {
		matches!(self.initial_target, OutputTargetState::Directory(_))
			.then_some(self.final_dir.as_path())
	}

	pub(crate) fn publish(mut self) -> Result<(), MergeError> {
		let staging_dir = self
			.staging_dir
			.as_ref()
			.cloned()
			.expect("output transaction can only be published once");
		let current_target = inspect_output_target(&self.final_dir)?;
		if current_target != self.initial_target {
			return Err(output_target_changed_error(&self.final_dir));
		}

		let OutputTargetState::Directory(expected_identity) = current_target else {
			if let Err(error) = rename_output_noreplace(&staging_dir, &self.final_dir) {
				return Err(transaction_error(
					error,
					format!("failed to publish new output {}", self.final_dir.display()),
				));
			}
			self.staging_dir = None;
			return Ok(());
		};

		if let Err(error) = exchange_output_paths(&staging_dir, &self.final_dir) {
			return Err(transaction_error(
				error,
				format!(
					"failed to atomically publish output {}",
					self.final_dir.display()
				),
			));
		}

		match inspect_output_target(&staging_dir) {
			Ok(OutputTargetState::Directory(identity)) if identity == expected_identity => {
				self.staging_identity = expected_identity;
				self.cleanup_staging("published output backup");
			}
			Ok(observed) => {
				self.staging_dir = None;
				return Err(MergeError::Io(io::Error::new(
					io::ErrorKind::InvalidInput,
					format!(
						"merge output {} changed during atomic publication; the displaced target ({observed:?}) was preserved at {} and the staged output is now published",
						self.final_dir.display(),
						staging_dir.display()
					),
				)));
			}
			Err(error) => {
				self.staging_dir = None;
				let error_kind = match &error {
					MergeError::Io(error) => error.kind(),
					_ => io::ErrorKind::Other,
				};
				return Err(MergeError::Io(io::Error::new(
					error_kind,
					format!(
						"merge output {} was published, but the displaced target at {} could not be verified and was left untouched: {error}",
						self.final_dir.display(),
						staging_dir.display()
					),
				)));
			}
		}
		Ok(())
	}

	fn cleanup_staging(&mut self, description: &str) {
		let Some(staging_dir) = self.staging_dir.as_deref() else {
			return;
		};
		match remove_owned_output_directory(staging_dir, &mut self.staging_identity) {
			Ok(OwnedCleanup::Removed | OwnedCleanup::Missing) => self.staging_dir = None,
			Ok(OwnedCleanup::NotOwned) => {
				eprintln!(
					"[merge] warning: transaction no longer owns {description} {}; leaving it untouched",
					staging_dir.display()
				);
				self.staging_dir = None;
			}
			Err(error) => eprintln!(
				"[merge] warning: failed to clean {description} {}: {error}",
				staging_dir.display()
			),
		}
	}
}

impl Drop for OutputTransaction {
	fn drop(&mut self) {
		self.cleanup_staging("transaction staging directory");
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnedCleanup {
	Removed,
	Missing,
	NotOwned,
}

fn remove_owned_output_directory(
	path: &Path,
	expected_identity: &mut OutputDirectoryIdentity,
) -> io::Result<OwnedCleanup> {
	let metadata = match fs::symlink_metadata(path) {
		Ok(metadata) => metadata,
		Err(error) if error.kind() == io::ErrorKind::NotFound => {
			return Ok(OwnedCleanup::Missing);
		}
		Err(error) => return Err(error),
	};
	if !metadata.file_type().is_dir() {
		return Ok(OwnedCleanup::NotOwned);
	}
	let current_identity = OutputDirectoryIdentity(same_file::Handle::from_path(path)?);
	if &current_identity != expected_identity {
		return Ok(OwnedCleanup::NotOwned);
	}
	expected_identity
		.0
		.as_file_mut()
		.remove_dir_contents(Some(path))?;
	let current_identity = OutputDirectoryIdentity(same_file::Handle::from_path(path)?);
	if &current_identity != expected_identity {
		return Ok(OwnedCleanup::NotOwned);
	}
	fs::remove_dir(path)?;
	Ok(OwnedCleanup::Removed)
}

fn acquire_output_lock(final_dir: &Path) -> Result<File, MergeError> {
	let lock_path = sibling_control_path(final_dir, "lock")?;
	let lock_file = OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(false)
		.open(&lock_path)
		.map_err(|error| {
			transaction_error(
				error,
				format!(
					"failed to open output transaction lock {}",
					lock_path.display()
				),
			)
		})?;
	lock_file.lock().map_err(|error| {
		transaction_error(
			error,
			format!("failed to lock output transaction {}", lock_path.display()),
		)
	})?;
	Ok(lock_file)
}

fn create_unique_sibling_dir(final_dir: &Path, kind: &str) -> Result<PathBuf, MergeError> {
	for _ in 0..1_024 {
		let candidate = unique_sibling_path(final_dir, kind)?;
		match fs::create_dir(&candidate) {
			Ok(()) => return Ok(candidate),
			Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
			Err(error) => return Err(MergeError::Io(error)),
		}
	}
	Err(MergeError::Io(io::Error::new(
		io::ErrorKind::AlreadyExists,
		format!(
			"failed to allocate sibling staging directory for {}",
			final_dir.display()
		),
	)))
}

fn unique_sibling_path(final_dir: &Path, kind: &str) -> Result<PathBuf, MergeError> {
	let file_name = final_dir.file_name().ok_or_else(|| {
		MergeError::Io(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!(
				"merge output must name a directory with a sibling: {}",
				final_dir.display()
			),
		))
	})?;
	let nonce = OUTPUT_TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
	let mut sibling_name = OsString::from(".");
	sibling_name.push(file_name);
	sibling_name.push(format!(".foch-{kind}-{}-{nonce}", std::process::id()));
	Ok(output_parent(final_dir).join(sibling_name))
}

fn sibling_control_path(final_dir: &Path, kind: &str) -> Result<PathBuf, MergeError> {
	let file_name = final_dir.file_name().ok_or_else(|| {
		MergeError::Io(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!(
				"merge output must name a directory with a sibling: {}",
				final_dir.display()
			),
		))
	})?;
	let mut sibling_name = OsString::from(".");
	sibling_name.push(file_name);
	sibling_name.push(format!(".foch-{kind}"));
	Ok(output_parent(final_dir).join(sibling_name))
}

fn output_parent(path: &Path) -> &Path {
	path.parent()
		.filter(|parent| !parent.as_os_str().is_empty())
		.unwrap_or_else(|| Path::new("."))
}

fn inspect_output_target(path: &Path) -> Result<OutputTargetState, MergeError> {
	match fs::symlink_metadata(path) {
		Ok(metadata) if metadata.file_type().is_dir() => Ok(OutputTargetState::Directory(
			OutputDirectoryIdentity(same_file::Handle::from_path(path)?),
		)),
		Ok(_) => Err(MergeError::Io(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!(
				"merge output must be a real directory or not exist: {}",
				path.display()
			),
		))),
		Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(OutputTargetState::Missing),
		Err(error) => Err(MergeError::Io(error)),
	}
}

fn output_target_changed_error(path: &Path) -> MergeError {
	MergeError::Io(io::Error::new(
		io::ErrorKind::InvalidInput,
		format!(
			"merge output changed while the replacement was staged: {}",
			path.display()
		),
	))
}

#[cfg(target_vendor = "apple")]
fn rename_output_noreplace(source: &Path, target: &Path) -> io::Result<()> {
	rename_output_with_flags(source, target, rustix::fs::RenameFlags::NOREPLACE)
}

#[cfg(target_vendor = "apple")]
fn exchange_output_paths(source: &Path, target: &Path) -> io::Result<()> {
	rename_output_with_flags(source, target, rustix::fs::RenameFlags::EXCHANGE)
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "redox"))]
fn rename_output_noreplace(source: &Path, target: &Path) -> io::Result<()> {
	rename_output_with_flags(source, target, rustix::fs::RenameFlags::NOREPLACE)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn exchange_output_paths(source: &Path, target: &Path) -> io::Result<()> {
	rename_output_with_flags(source, target, rustix::fs::RenameFlags::EXCHANGE)
}

#[cfg(any(
	target_vendor = "apple",
	target_os = "linux",
	target_os = "android",
	target_os = "redox"
))]
fn rename_output_with_flags(
	source: &Path,
	target: &Path,
	flags: rustix::fs::RenameFlags,
) -> io::Result<()> {
	rustix::fs::renameat_with(rustix::fs::CWD, source, rustix::fs::CWD, target, flags)
		.map_err(io::Error::from)
}

#[cfg(target_os = "windows")]
fn rename_output_noreplace(source: &Path, target: &Path) -> io::Result<()> {
	use std::os::windows::ffi::OsStrExt;
	use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

	let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
	let target_wide: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
	// Omitting MOVEFILE_REPLACE_EXISTING makes this a no-replace move.
	let moved = unsafe { MoveFileExW(source_wide.as_ptr(), target_wide.as_ptr(), 0) };
	if moved == 0 {
		Err(io::Error::last_os_error())
	} else {
		Ok(())
	}
}

#[cfg(any(target_os = "windows", target_os = "redox"))]
fn exchange_output_paths(source: &Path, target: &Path) -> io::Result<()> {
	let _ = (source, target);
	Err(io::Error::new(
		io::ErrorKind::Unsupported,
		"atomic replacement of an existing output directory is unsupported on this platform",
	))
}

#[cfg(not(any(
	target_vendor = "apple",
	target_os = "linux",
	target_os = "android",
	target_os = "redox",
	target_os = "windows"
)))]
fn rename_output_noreplace(_source: &Path, _target: &Path) -> io::Result<()> {
	Err(io::Error::new(
		io::ErrorKind::Unsupported,
		"atomic no-replace directory publication is unsupported on this platform",
	))
}

#[cfg(not(any(
	target_vendor = "apple",
	target_os = "linux",
	target_os = "android",
	target_os = "redox",
	target_os = "windows"
)))]
fn exchange_output_paths(_source: &Path, _target: &Path) -> io::Result<()> {
	Err(io::Error::new(
		io::ErrorKind::Unsupported,
		"transactional directory replacement is unsupported on this platform",
	))
}

#[cfg(any(
	target_vendor = "apple",
	target_os = "linux",
	target_os = "android",
	target_os = "redox",
	target_os = "windows"
))]
fn ensure_transaction_platform_supported() -> Result<(), MergeError> {
	Ok(())
}

#[cfg(not(any(
	target_vendor = "apple",
	target_os = "linux",
	target_os = "android",
	target_os = "redox",
	target_os = "windows"
)))]
fn ensure_transaction_platform_supported() -> Result<(), MergeError> {
	Err(MergeError::Io(io::Error::new(
		io::ErrorKind::Unsupported,
		"transactional directory publication is unsupported on this platform",
	)))
}

#[cfg(any(target_os = "windows", target_os = "redox"))]
fn ensure_target_replacement_supported(target: &OutputTargetState) -> Result<(), MergeError> {
	if matches!(target, OutputTargetState::Directory(_)) {
		return Err(MergeError::Io(io::Error::new(
			io::ErrorKind::Unsupported,
			"atomic replacement of an existing output directory is unsupported on this platform",
		)));
	}
	Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "redox")))]
fn ensure_target_replacement_supported(_target: &OutputTargetState) -> Result<(), MergeError> {
	Ok(())
}

fn transaction_error(error: io::Error, context: String) -> MergeError {
	MergeError::Io(io::Error::new(error.kind(), format!("{context}: {error}")))
}
