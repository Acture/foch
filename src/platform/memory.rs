//! Physical memory and the memory this process holds now, for sizing how much
//! merge work may run at once. A value the platform does not report is
//! `None`.

/// Physical memory available to this process, in bytes: the machine's, or a
/// lower container limit where the platform reports one.
pub(crate) fn physical_memory_bytes() -> Option<u64> {
	imp::physical_memory_bytes()
}

/// Memory this process holds now, in bytes. It is a current value rather
/// than a peak, so a long-lived process that finished a large merge is not
/// charged for memory it has since released.
pub(crate) fn resident_bytes() -> Option<u64> {
	imp::resident_bytes()
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

	pub(super) fn resident_bytes() -> Option<u64> {
		let mut usage: MaybeUninit<libc::rusage_info_v2> = MaybeUninit::zeroed();
		// SAFETY: RUSAGE_INFO_V2 fills a rusage_info_v2, which the buffer is,
		// and reports failure.
		let result: libc::c_int = unsafe {
			libc::proc_pid_rusage(
				libc::getpid(),
				libc::RUSAGE_INFO_V2,
				usage.as_mut_ptr().cast::<libc::rusage_info_t>(),
			)
		};
		if result != 0 {
			return None;
		}
		// SAFETY: the call succeeded, so every field is initialized. The
		// physical footprint is what the system charges the process for,
		// compressed memory included.
		Some(unsafe { usage.assume_init() }.ri_phys_footprint)
	}
}

#[cfg(target_os = "linux")]
mod imp {
	pub(super) fn physical_memory_bytes() -> Option<u64> {
		let machine: u64 =
			super::kibibyte_field(&std::fs::read_to_string("/proc/meminfo").ok()?, "MemTotal:")?;
		// A container's memory limit, cgroup v2 then v1, when it is lower.
		let limit: Option<u64> = [
			"/sys/fs/cgroup/memory.max",
			"/sys/fs/cgroup/memory/memory.limit_in_bytes",
		]
		.into_iter()
		.find_map(|path| super::cgroup_limit(&std::fs::read_to_string(path).ok()?));
		Some(limit.map_or(machine, |limit| limit.min(machine)))
	}

	pub(super) fn resident_bytes() -> Option<u64> {
		super::kibibyte_field(
			&std::fs::read_to_string("/proc/self/status").ok()?,
			"VmRSS:",
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

	pub(super) fn resident_bytes() -> Option<u64> {
		let mut counters: PROCESS_MEMORY_COUNTERS = PROCESS_MEMORY_COUNTERS {
			cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
			..PROCESS_MEMORY_COUNTERS::default()
		};
		// SAFETY: the current-process pseudo-handle needs no closing, and `cb`
		// is the size of the counters passed.
		let result =
			unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
		(result != 0).then_some(counters.WorkingSetSize as u64)
	}
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
	pub(super) fn physical_memory_bytes() -> Option<u64> {
		None
	}

	pub(super) fn resident_bytes() -> Option<u64> {
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

/// A cgroup memory limit file's value, in bytes; `max` means no limit.
#[cfg(any(target_os = "linux", test))]
fn cgroup_limit(contents: &str) -> Option<u64> {
	contents.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
	use super::{cgroup_limit, kibibyte_field};

	#[test]
	fn cgroup_limits_are_bytes_and_max_is_no_limit() {
		assert_eq!(cgroup_limit("1073741824\n"), Some(1 << 30));
		assert_eq!(cgroup_limit("max\n"), None);
		assert_eq!(cgroup_limit(""), None);
	}

	#[test]
	fn linux_status_fields_are_read_in_bytes() {
		let status: &str = "Name:\tfoch\nVmHWM:\t 900 kB\nVmRSS:\t    2048 kB\n";
		assert_eq!(kibibyte_field(status, "VmRSS:"), Some(2048 * 1024));
		assert_eq!(
			kibibyte_field("MemTotal:       32768 kB\n", "MemTotal:"),
			Some(32768 * 1024)
		);
		assert_eq!(kibibyte_field(status, "VmSwap:"), None);
		assert_eq!(kibibyte_field("VmRSS:\tmany kB\n", "VmRSS:"), None);
	}
}
