//! Dynamic trade-topology plugin for EU4 1.37.5 on the Foch native host.
//!
//! The plugin closes and reopens vanilla outgoing trade routes on the
//! game thread so that trade converges toward wealthy nodes instead of
//! the fixed vanilla end nodes; see `policy.rs` for the decision rules
//! and the package README for the reversed engine layout this rides on.
//!
//! Startup contract (ABI 1.0): `init` verifies the pinned `eu4.exe`
//! digest, resolves every signature through the host's `find_pattern`,
//! and only then installs the monthly hook — all while the entry-phase
//! window is open, because a first hook enable is refused later. Any
//! refusal leaves the DLL loaded and forwarding, just inert.

mod config;
mod engine;
mod hook;
mod logging;
mod offsets;
mod policy;
mod runtime;
mod state;

use foch_plugin_abi::{
	ABI_MAJOR, ABI_MINOR, FochHostApi, FochPluginApi, FochStr, result, state as state_code,
};
use state::Reason;

static PLUGIN_API: FochPluginApi = FochPluginApi {
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

/// The one export every native plugin provides. No side effects.
///
/// # Safety
///
/// The returned table (or null) is a process-lifetime static; the caller
/// follows the SDK contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foch_plugin_get_api(host_abi_major: u32) -> *const FochPluginApi {
	if host_abi_major == ABI_MAJOR {
		&PLUGIN_API
	} else {
		std::ptr::null()
	}
}

fn refuse(host: &FochHostApi, reason: Reason, detail: &str) -> i32 {
	// Safe: the host table and its callbacks are valid for the process
	// lifetime, and `report` accepts any thread.
	state::set(state_code::REFUSED, reason);
	unsafe { (host.report)(host, &state::status()) };
	logging::write(foch_plugin_abi::log::WARN, detail);
	result::OK
}

/// # Safety
///
/// `host` must be the host table the ABI handed to this process, valid
/// for the process lifetime; callers follow the SDK contract.
unsafe extern "C" fn init(host: *const FochHostApi) -> i32 {
	// SAFETY: the host passes a valid table pointer for the process
	// lifetime; a null table is rejected without touching it.
	let Some(host) = (unsafe { host.as_ref() }) else {
		return result::E_INVALID;
	};
	logging::install(host);

	// SAFETY: host strings are stable and readable for the call.
	let game_version = unsafe { host.game_version.as_str() }.unwrap_or("");
	let exe_sha256 = unsafe { host.exe_sha256.as_str() }.unwrap_or("");
	logging::write(
		foch_plugin_abi::log::INFO,
		&format!(
			"init: game {game_version}, exe sha256 {exe_sha256}, image 0x{:x}+0x{:x}",
			host.exe_base as usize, host.exe_size
		),
	);

	let config_bytes = unsafe { (host.config_json)(host).as_bytes().to_vec() };
	let config = config::Config::from_json(&config_bytes);
	logging::write(
		foch_plugin_abi::log::INFO,
		&format!("config: mode {:?}, enabled {}", config.mode, config.enabled),
	);

	// Identity first: the digest is the sole proof this build was reversed.
	let Some(sigs) = offsets::Sigs::for_build(exe_sha256) else {
		return refuse(
			host,
			Reason::ExeMismatch,
			"refused: eu4.exe is not the reversed build",
		);
	};
	if sigs.is_pending() {
		return refuse(
			host,
			Reason::OffsetsPending,
			"refused: RE signatures still pending",
		);
	}

	let resolve = |signature: &'static str, label: &str| -> Result<usize, ()> {
		let mut address = std::ptr::null_mut();
		let code = unsafe { (host.find_pattern)(host, FochStr::from_str(signature), &mut address) };
		if code == result::OK && !address.is_null() {
			logging::write(
				foch_plugin_abi::log::DEBUG,
				&format!("{label} at 0x{:x}", address as usize),
			);
			return Ok(address as usize);
		}
		let outcome = if code == result::E_AMBIGUOUS {
			"ambiguous"
		} else {
			"not found"
		};
		logging::write(
			foch_plugin_abi::log::WARN,
			&format!("{label} pattern {outcome}"),
		);
		Err(())
	};
	let Ok(tick) = resolve(sigs.tick, "trade recalculation") else {
		return refuse(
			host,
			Reason::PatternFailed,
			"refused: a pinned signature failed to resolve",
		);
	};
	// Cross-check the resolved address against the spike's recorded RVA:
	// a matching pattern at a different address means a rebuilt exe that
	// happens to share these bytes, which the layout tables do not cover.
	if tick - host.exe_base as usize != offsets::layout::TICK_RVA {
		logging::write(
			foch_plugin_abi::log::WARN,
			&format!(
				"tick resolved at +0x{:x}, spike recorded +0x{:x}; refusing",
				tick - host.exe_base as usize,
				offsets::layout::TICK_RVA
			),
		);
		return refuse(
			host,
			Reason::BadLayout,
			"refused: the economy update moved from its recorded offset",
		);
	}

	// The hook must be created and first-enabled inside this init call:
	// the host refuses a first prologue jump after the entry window.
	let mut next: *const *const std::ffi::c_void = std::ptr::null();
	let mut hook = std::ptr::null_mut();
	let code = unsafe {
		(host.hook_create)(
			host,
			tick as *mut std::ffi::c_void,
			hook::economy_update_detour as *mut std::ffi::c_void,
			100,
			&mut next,
			&mut hook,
		)
	};
	if code != result::OK {
		return refuse(
			host,
			Reason::PatternFailed,
			"refused: hook_create failed on the economy update",
		);
	}
	let code = unsafe { (host.hook_set_enabled)(host, hook, 1) };
	if code != result::OK {
		return refuse(
			host,
			Reason::PatternFailed,
			"refused: first hook enable outside the entry window",
		);
	}
	hook::install_next_cell(next as *mut *const *const std::ffi::c_void);

	runtime::install(config, host.exe_base as usize);
	state::set(state_code::ACTIVE, Reason::None);
	unsafe { (host.report)(host, &state::status()) };
	logging::write(
		foch_plugin_abi::log::INFO,
		"active: economy-update hook installed",
	);
	result::OK
}

/// # Safety
///
/// `out` must be null or a writable `FochStatus`; callers follow the SDK
/// contract.
unsafe extern "C" fn status(out: *mut foch_plugin_abi::FochStatus) {
	if let Some(out) = unsafe { out.as_mut() } {
		*out = state::status();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn get_api_pins_the_abi_major() {
		let api = unsafe { foch_plugin_get_api(ABI_MAJOR) };
		assert!(!api.is_null());
		assert_eq!(unsafe { (*api).abi_major }, ABI_MAJOR);
		assert!(unsafe { foch_plugin_get_api(ABI_MAJOR + 1) }.is_null());
	}

	#[test]
	fn a_null_host_is_invalid_without_touching_state() {
		assert_eq!(unsafe { init(std::ptr::null()) }, result::E_INVALID);
	}
}
