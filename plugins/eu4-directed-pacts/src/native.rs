//! Foch DLL lifecycle. Refusal remains explicit until the engine bridge is verified.

use foch_plugin_sdk::{
	ABI_MAJOR, ABI_MINOR, FochHostApi, FochPluginApi, FochStatus, FochStr, result, state,
};
use std::sync::atomic::{AtomicI32, Ordering};

#[path = "probe.rs"]
mod probe;

const MISSING_WAR_BRIDGE: i32 = 201;
const BUILD_MISMATCH: i32 = 210;
const BAD_PROBE_CONFIG: i32 = 211;
const HOOK_REFUSED: i32 = 212;
const READ_ONLY_PROBE: i32 = 213;
const PROBE_FAILED: i32 = 214;
static REASON: AtomicI32 = AtomicI32::new(MISSING_WAR_BRIDGE);
const DETAIL: &str =
	"Directed pacts unavailable: a verified EU4 war, command and save bridge is required.";

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
	let (state, detail) = match reason {
		READ_ONLY_PROBE => (
			state::ACTIVE,
			"Read-only war roster diagnostic active; pact gameplay and war entry remain unavailable.",
		),
		PROBE_FAILED => (
			state::FAILED,
			"Read-only war roster diagnostic failed; pact gameplay remains unavailable.",
		),
		BUILD_MISMATCH => (
			state::REFUSED,
			"War roster diagnostic refused: the host-computed executable digest is not the pinned build.",
		),
		BAD_PROBE_CONFIG => (
			state::REFUSED,
			"War roster diagnostic refused: invalid explicit diagnostic configuration.",
		),
		HOOK_REFUSED => (
			state::REFUSED,
			"War roster diagnostic refused: verified monthly callback could not be installed.",
		),
		_ => (state::REFUSED, DETAIL),
	};
	let mut value = FochStatus::new(state);
	value.reason_code = reason;
	value.detail = FochStr::from_str(detail);
	value
}

fn publish(host: &FochHostApi, reason: i32) {
	REASON.store(reason, Ordering::Release);
	unsafe { (host.report)(host, &current_status()) };
}

fn probe_active() -> bool {
	REASON.load(Ordering::Acquire) == READ_ONLY_PROBE
}
fn probe_failed(host: &FochHostApi) {
	publish(host, PROBE_FAILED);
}

unsafe extern "C" fn init(host: *const FochHostApi) -> i32 {
	if host.is_null() {
		return result::E_INVALID;
	}
	// Read the size prefix before making a reference to the full table.
	if unsafe { host.cast::<u32>().read_unaligned() } < std::mem::size_of::<FochHostApi>() as u32 {
		return result::E_UNSUPPORTED;
	}
	let host = unsafe { &*host };
	if host.abi_major != ABI_MAJOR {
		return result::E_UNSUPPORTED;
	}
	let config = unsafe { (host.config_json)(host) };
	if config.len > 8192 {
		publish(host, BAD_PROBE_CONFIG);
		return result::OK;
	}
	let request = match probe::configuration(unsafe { config.as_bytes() }) {
		Ok(Some(request)) => request,
		Ok(None) => {
			publish(host, MISSING_WAR_BRIDGE);
			return result::OK;
		}
		Err(error) => {
			unsafe { (host.log)(host, foch_plugin_sdk::log::ERROR, FochStr::from_str(&error)) };
			publish(host, BAD_PROBE_CONFIG);
			return result::OK;
		}
	};
	if unsafe { host.plugin_id.as_str() } != Some(crate::ROSTER_PROBE_PLUGIN_ID) {
		publish(host, BAD_PROBE_CONFIG);
		return result::OK;
	}
	// Compare the actual host-computed digest, never the expected constant
	// supplied as though it were an observed executable identity.
	let digest = unsafe { host.exe_sha256.as_str() }.unwrap_or("");
	if !foch_runtime::dataplane::supports_build(digest) {
		publish(host, BUILD_MISMATCH);
		return result::OK;
	}
	match unsafe { probe::install(host, request) } {
		Ok(()) => publish(host, READ_ONLY_PROBE),
		Err(error) => {
			unsafe { (host.log)(host, foch_plugin_sdk::log::ERROR, FochStr::from_str(&error)) };
			publish(host, HOOK_REFUSED);
		}
	}
	result::OK
}

unsafe extern "C" fn status(out: *mut FochStatus) {
	if !out.is_null() {
		unsafe { out.write_unaligned(current_status()) };
	}
}
