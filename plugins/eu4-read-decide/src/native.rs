//! Foch DLL lifecycle for the read-only decide plugin.
//!
//! Default: REFUSED, nothing installed. An explicit opt-in config
//! installs the read-only monthly probe (see [`probe`]): the
//! host-reported `exe_base`/`exe_sha256` feed VerifiedGame's build
//! gate, one frame is read and scored per install, and the
//! suggestion is logged as a `FOCH_RECEIPT/1` receipt. No game
//! memory writes, ever.

use crate::probe;
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

/// The reason code an install failure reports: the variant decides, so
/// the status detail always matches what actually went wrong.
fn reason_of(error: &probe::InstallError) -> i32 {
	match error {
		probe::InstallError::BadAsset(_) => BAD_MODEL_ASSET,
		probe::InstallError::BuildMismatch(_) => BUILD_MISMATCH,
		probe::InstallError::HookFailed(_) => PROBE_FAILED,
	}
}

/// The status for one reason code. Every code maps to the state and
/// detail that truthfully describe it; the awaiting-wiring default is
/// the REFUSED "nothing installed" contract.
fn status_for(reason: i32) -> FochStatus {
	let (state_value, detail) = match reason {
		PROBE_ACTIVE => (state::ACTIVE, "Read-only decide monthly probe active."),
		PROBE_FAILED => (
			state::FAILED,
			"Read-only decide monthly probe failed to install.",
		),
		BAD_MODEL_ASSET => (
			state::FAILED,
			"Read-only decide: the weight asset failed to load.",
		),
		BUILD_MISMATCH => (
			state::REFUSED,
			"Read-only decide refused: the host-computed digest is not a supported build.",
		),
		// AWAITING_SESSION_WIRING and any unknown code share this arm:
		// the REFUSED "nothing installed" contract is the default.
		_ => (state::REFUSED, DETAIL),
	};
	let mut value = FochStatus::new(state_value);
	value.reason_code = reason;
	value.detail = FochStr::from_str(detail);
	value
}

fn current_status() -> FochStatus {
	status_for(REASON.load(Ordering::Acquire))
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
				let reason = match &outcome {
					Ok(()) => PROBE_ACTIVE,
					Err(error) => reason_of(error),
				};
				REASON.store(reason, Ordering::Release);
				if let Err(error) = outcome {
					unsafe { log_error(host, &error.to_string()) };
				}
			}
			Err(reason) => {
				// A rejected config also means the probe never installed:
				// FAILED with the install reason, not a fake asset error.
				REASON.store(PROBE_FAILED, Ordering::Release);
				unsafe { log_error(host, &reason) };
			}
			Ok(None) => {}
		}
	}
	unsafe { (host.report)(host, &current_status()) };
	result::OK
}

unsafe fn log_error(host: &FochHostApi, text: &str) {
	unsafe { (host.log)(host, foch_plugin_sdk::log::ERROR, FochStr::from_str(text)) };
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

	#[test]
	fn every_reason_code_maps_to_its_honest_state_and_detail() {
		let cases: [(i32, i32, &str); 5] = [
			(
				AWAITING_SESSION_WIRING,
				state::REFUSED,
				"Read-only decide suggestions require engine session wiring",
			),
			(
				PROBE_ACTIVE,
				state::ACTIVE,
				"Read-only decide monthly probe active.",
			),
			(
				PROBE_FAILED,
				state::FAILED,
				"Read-only decide monthly probe failed to install.",
			),
			(
				BAD_MODEL_ASSET,
				state::FAILED,
				"Read-only decide: the weight asset failed to load.",
			),
			(
				BUILD_MISMATCH,
				state::REFUSED,
				"Read-only decide refused: the host-computed digest is not a supported build.",
			),
		];
		for (reason, expected_state, detail_prefix) in cases {
			let status = status_for(reason);
			assert_eq!(status.reason_code, reason, "reason {reason}");
			assert_eq!(status.state, expected_state, "reason {reason}");
			let detail = unsafe { status.detail.as_str() }.unwrap_or_default();
			assert!(
				detail.starts_with(detail_prefix),
				"reason {reason}: {detail}"
			);
		}
		// An unknown code stays on the awaiting-wiring refusal contract.
		let unknown = status_for(-1);
		assert_eq!(unknown.state, state::REFUSED);
		assert_eq!(unknown.reason_code, -1);
		let detail = unsafe { unknown.detail.as_str() }.unwrap_or_default();
		assert!(detail.starts_with("Read-only decide suggestions require engine session wiring"));
	}

	#[test]
	fn install_errors_map_to_their_reason_codes() {
		let cases = [
			(
				probe::InstallError::BadAsset("weights".into()),
				BAD_MODEL_ASSET,
			),
			(
				probe::InstallError::BuildMismatch("digest".into()),
				BUILD_MISMATCH,
			),
			(probe::InstallError::HookFailed("hook".into()), PROBE_FAILED),
		];
		for (error, expected) in cases {
			assert_eq!(reason_of(&error), expected, "{error}");
			let status = status_for(reason_of(&error));
			assert_eq!(status.reason_code, expected);
		}
	}
}
