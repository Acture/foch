//! Foch DLL lifecycle for the read-only decide plugin.
//!
//! v1 refuses game activation: no engine callbacks are installed, no
//! game memory is touched. The status explains that session wiring —
//! the host-reported `exe_base`/`exe_sha256` feeding VerifiedGame,
//! plus the monthly cadence that assembles frames — is the next
//! milestone; the inference path itself is exercised offline.

use foch_plugin_sdk::{ABI_MAJOR, ABI_MINOR, FochPluginApi, FochStatus, FochStr, result, state};
use std::sync::atomic::{AtomicI32, Ordering};

const AWAITING_SESSION_WIRING: i32 = 201;
const BUILD_MISMATCH: i32 = 210;
const BAD_MODEL_ASSET: i32 = 211;

static REASON: AtomicI32 = AtomicI32::new(AWAITING_SESSION_WIRING);
const DETAIL: &str = "Read-only decide suggestions require engine session wiring: \
the host-reported image base, the host-computed digest for the build gate, \
and the monthly read cadence. This build reads and logs nothing.";

static API: FochPluginApi = FochPluginApi {
	size: std::mem::size_of::<FochPluginApi>() as u32,
	abi_major: ABI_MAJOR,
	abi_minor: ABI_MINOR,
	caps: 0,
	init,
	status,
	config_changed: None,
	set_enabled: None,
	shutdown: None,
};

/// # Safety
/// The caller follows the Foch SDK ABI and retains no mutable table references.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foch_plugin_get_api(host_abi_major: u32) -> *const FochPluginApi {
	if host_abi_major == ABI_MAJOR {
		&API
	} else {
		std::ptr::null()
	}
}

fn current_status() -> FochStatus {
	let reason = REASON.load(Ordering::Acquire);
	let (state_value, detail) = match reason {
		BAD_MODEL_ASSET => (
			state::FAILED,
			"Read-only decide: the weight asset failed to load.",
		),
		BUILD_MISMATCH => (
			state::REFUSED,
			"Read-only decide refused: the host-computed digest is not the pinned build.",
		),
		_ => (state::REFUSED, DETAIL),
	};
	let mut value = FochStatus::new(state_value);
	value.reason_code = reason;
	value.detail = FochStr::from_str(detail);
	value
}

unsafe extern "C" fn init(host: *const foch_plugin_sdk::FochHostApi) -> i32 {
	if host.is_null() {
		return result::E_INVALID;
	}
	REASON.store(AWAITING_SESSION_WIRING, Ordering::Release);
	unsafe { ((*host).report)(host, &current_status()) };
	result::OK
}

unsafe extern "C" fn status(out: *mut FochStatus) {
	if !out.is_null() {
		unsafe { *out = current_status() };
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_default_status_refuses_with_the_wiring_reason() {
		let status = current_status();
		assert_eq!(status.reason_code, AWAITING_SESSION_WIRING);
		assert_eq!(status.state, state::REFUSED);
	}
}
