//! Host-backed logging. The plugin stores the host table once in `init`
//! and every later log line goes through it; every host callback is
//! thread-safe, so the game thread logs directly.

use foch_plugin_abi::{FochHostApi, FochStr, log};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};

static HOST: AtomicPtr<FochHostApi> = AtomicPtr::new(null_mut());

pub fn install(host: *const FochHostApi) {
	HOST.store(host as *mut FochHostApi, Ordering::Release);
}

/// Emit one line through the host event stream; a missing host drops it.
pub fn write(level: i32, message: &str) {
	let host = HOST.load(Ordering::Acquire);
	if host.is_null() {
		return;
	}
	let host = unsafe { &*host };
	// SAFETY: the host table and its log callback are valid process-wide.
	unsafe {
		(host.log)(
			host,
			level,
			FochStr {
				ptr: message.as_ptr(),
				len: message.len(),
			},
		)
	};
}

pub const WARN: i32 = log::WARN;
pub const INFO: i32 = log::INFO;
pub const DEBUG: i32 = log::DEBUG;

#[cfg(test)]
mod tests {
	#[test]
	fn write_without_a_host_drops_the_line() {
		super::write(super::INFO, "nobody is listening");
	}
}
