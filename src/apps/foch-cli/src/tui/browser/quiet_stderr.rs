//! Merge analysis reports progress lines on the process's stderr, and the
//! `foch merge` measurement harness reads them, so their producer stays as it
//! is. The browser shares the terminal with stderr; while it is open, stderr
//! is detached so those lines never draw over the screen.

/// Detaches the process's stderr until dropped.
pub struct QuietStderr {
	#[cfg(unix)]
	saved: libc::c_int,
	#[cfg(windows)]
	saved: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(unix)]
impl QuietStderr {
	pub fn new() -> Option<Self> {
		// SAFETY: plain descriptor calls on owned descriptors; every failure
		// leaves stderr attached.
		unsafe {
			let saved = libc::dup(libc::STDERR_FILENO);
			if saved < 0 {
				return None;
			}
			let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
			if null < 0 {
				libc::close(saved);
				return None;
			}
			let replaced = libc::dup2(null, libc::STDERR_FILENO);
			libc::close(null);
			if replaced < 0 {
				libc::close(saved);
				return None;
			}
			Some(Self { saved })
		}
	}
}

#[cfg(unix)]
impl Drop for QuietStderr {
	fn drop(&mut self) {
		// SAFETY: `saved` is the descriptor `new` duplicated and still owns.
		unsafe {
			libc::dup2(self.saved, libc::STDERR_FILENO);
			libc::close(self.saved);
		}
	}
}

#[cfg(windows)]
impl QuietStderr {
	pub fn new() -> Option<Self> {
		use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetStdHandle};
		// SAFETY: swaps the process's standard error handle. Rust's stderr
		// looks the handle up on every write and discards output while it is
		// null.
		unsafe {
			let saved = GetStdHandle(STD_ERROR_HANDLE);
			(SetStdHandle(STD_ERROR_HANDLE, std::ptr::null_mut()) != 0).then_some(Self { saved })
		}
	}
}

#[cfg(windows)]
impl Drop for QuietStderr {
	fn drop(&mut self) {
		use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};
		// SAFETY: restores the handle `new` replaced.
		unsafe {
			SetStdHandle(STD_ERROR_HANDLE, self.saved);
		}
	}
}

#[cfg(not(any(unix, windows)))]
impl QuietStderr {
	pub fn new() -> Option<Self> {
		None
	}
}
