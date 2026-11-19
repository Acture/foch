//! Foch DLL lifecycle for the read-only decide plugin.
//!
//! Default: REFUSED, nothing installed. An explicit opt-in config
//! installs the read-only monthly probe (see [`probe`]): the
//! host-reported `exe_base`/`exe_sha256` feed VerifiedGame's build
//! gate, one frame is read and scored per install, and the
//! suggestion is logged as a `FOCH_RECEIPT/1` receipt. No game
//! memory writes, ever.

use foch_plugin_sdk::{
	ABI_MAJOR, ABI_MINOR, FochHostApi, FochPluginApi, FochStatus, FochStr, result, state,
};
use std::sync::atomic::{AtomicI32, Ordering};

const AWAITING_SESSION_WIRING: i32 = 201;
const BUILD_MISMATCH: i32 = 210;
const BAD_MODEL_ASSET: i32 = 211;
const PROBE_ACTIVE: i32 = 212;
const PROBE_FAILED: i32 = 213;

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
	config_changed: Some(config_changed),
	set_enabled: None,
	shutdown: None,
};

#[path = "probe.rs"]
mod probe;

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

unsafe extern "C" fn config_changed(config_json: FochStr) -> i32 {
	let Some(text) = (unsafe { config_json.as_str() }) else {
		return result::E_INVALID;
	};
	match probe::parse_config(text) {
		Ok(_) => result::OK, // installs happen once, at init
		Err(_) => result::E_INVALID,
	}
}

unsafe extern "C" fn init(host: *const FochHostApi) -> i32 {
	if host.is_null() {
		return result::E_INVALID;
	}
	let host = unsafe { &*host };
	REASON.store(AWAITING_SESSION_WIRING, Ordering::Release);
	let config = unsafe { (host.config_json)(host) };
	if let Some(text) = unsafe { config.as_str() } {
		match probe::parse_config(text) {
			Ok(Some(request)) => {
				let outcome = probe::install(host, request);
				REASON.store(
					if outcome.is_ok() {
						PROBE_ACTIVE
					} else {
						PROBE_FAILED
					},
					Ordering::Release,
				);
				if let Err(reason) = outcome {
					unsafe {
						(host.log)(
							host,
							foch_plugin_sdk::log::ERROR,
							FochStr::from_str(&reason),
						)
					};
				}
			}
			Err(reason) => {
				REASON.store(BAD_MODEL_ASSET, Ordering::Release);
				unsafe {
					(host.log)(
						host,
						foch_plugin_sdk::log::ERROR,
						FochStr::from_str(&reason),
					)
				};
			}
			Ok(None) => {}
		}
	}
	unsafe { (host.report)(host, &current_status()) };
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
