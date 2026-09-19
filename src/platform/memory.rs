//! Physical memory and this process's peak resident memory, for sizing how
//! much merge work may run at once. A value the platform does not report is
//! `None`.

/// Total physical memory, in bytes.
pub(crate) fn physical_memory_bytes() -> Option<u64> {
	imp::physical_memory_bytes()
}

/// The most memory this process has held resident so far, in bytes.
pub(crate) fn peak_resident_bytes() -> Option<u64> {
	imp::peak_resident_bytes()
}

#[cfg(target_os = "macos")]
mod imp {
	use std::mem::{MaybeUninit, size_of};

	pub(super) fn physical_memory_bytes() -> Option<u64> {
		let mut bytes: u64 = 0;
		let mut size: libc::size_t = size_of::<u64>();
		// SAFETY: `hw.memsize` is a 64-bit integer, and the buffer and its
		// length describe exactly one.
		let result: libc::c_int = unsafe {
			libc::sysctlbyname(
				c"hw.memsize".as_ptr(),
				(&raw mut bytes).cast(),
				&mut size,
				std::ptr::null_mut(),
				0,
			)
		};
		(result == 0 && size == size_of::<u64>()).then_some(bytes)
	}

	pub(super) fn peak_resident_bytes() -> Option<u64> {
		let mut usage: MaybeUninit<libc::rusage> = MaybeUninit::zeroed();
		// SAFETY: getrusage fills the rusage it is given and reports failure.
		if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
			return None;
		}
		// SAFETY: getrusage succeeded, so every field is initialized.
		let usage: libc::rusage = unsafe { usage.assume_init() };
		// macOS reports the peak in bytes.
		u64::try_from(usage.ru_maxrss).ok()
	}
}

#[cfg(target_os = "linux")]
mod imp {
	pub(super) fn physical_memory_bytes() -> Option<u64> {
		super::kibibyte_field(&std::fs::read_to_string("/proc/meminfo").ok()?, "MemTotal:")
	}

	pub(super) fn peak_resident_bytes() -> Option<u64> {
		super::kibibyte_field(
			&std::fs::read_to_string("/proc/self/status").ok()?,
			"VmHWM:",
		)
	}
}

#[cfg(windows)]
mod imp {
	use std::mem::size_of;
	use windows_sys::Win32::System::ProcessStatus::{
		K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
	};
	use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
	use windows_sys::Win32::System::Threading::GetCurrentProcess;

	pub(super) fn physical_memory_bytes() -> Option<u64> {
		let mut status: MEMORYSTATUSEX = MEMORYSTATUSEX {
			dwLength: size_of::<MEMORYSTATUSEX>() as u32,
			..MEMORYSTATUSEX::default()
		};
		// SAFETY: `status` is a MEMORYSTATUSEX whose length field is set, as the
		// call requires.
		(unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullTotalPhys)
	}

	pub(super) fn peak_resident_bytes() -> Option<u64> {
		let mut counters: PROCESS_MEMORY_COUNTERS = PROCESS_MEMORY_COUNTERS {
			cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
			..PROCESS_MEMORY_COUNTERS::default()
		};
		// SAFETY: the current-process pseudo-handle needs no closing, and `cb`
		// is the size of the counters passed.
		let result =
			unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
		(result != 0).then_some(counters.PeakWorkingSetSize as u64)
	}
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
	pub(super) fn physical_memory_bytes() -> Option<u64> {
		None
	}

	pub(super) fn peak_resident_bytes() -> Option<u64> {
		None
	}
}

/// A `Name:   <n> kB` field of a Linux `/proc` status file, in bytes.
#[cfg(any(target_os = "linux", test))]
fn kibibyte_field(status: &str, name: &str) -> Option<u64> {
	status
		.lines()
		.find_map(|line| line.strip_prefix(name))
		.and_then(|value| value.trim().strip_suffix("kB"))
		.and_then(|kibibytes| kibibytes.trim().parse::<u64>().ok())
		.map(|kibibytes| kibibytes * 1024)
}

#[cfg(test)]
mod tests {
	use super::kibibyte_field;

	#[test]
	fn linux_status_fields_are_read_in_bytes() {
		let status: &str = "Name:\tfoch\nVmPeak:\t 900 kB\nVmHWM:\t    2048 kB\n";
		assert_eq!(kibibyte_field(status, "VmHWM:"), Some(2048 * 1024));
		assert_eq!(
			kibibyte_field("MemTotal:       32768 kB\n", "MemTotal:"),
			Some(32768 * 1024)
		);
		assert_eq!(kibibyte_field(status, "VmSwap:"), None);
		assert_eq!(kibibyte_field("VmHWM:\tmany kB\n", "VmHWM:"), None);
	}
}
