//! The EU4 data plane: typed, guarded access to the live game.
//!
//! Plugins build their policies on these views instead of carrying
//! their own offset tables and pointer walkers. Offsets come from
//! compiled-in JSON profiles selected by executable digest
//! ([`pinned_exe_sha256`] is the default build) and are a crate-private
//! detail.
//! [`Memory`] uses checked kernel copies on Windows; complete readable
//! ranges can still hold the wrong object or stale state. Callers must
//! establish identity, type, lifetime and game-thread stability.
//! The `test-support` feature adds `dataplane::testing` on Windows for
//! synthetic scenes. Production plugin builds leave that feature off.

pub mod flags;
pub mod game;
pub(crate) mod layout;
pub mod memory;
pub(crate) mod profile;
pub mod routes;
#[cfg(all(windows, feature = "test-support"))]
pub mod testing;
pub mod war;

pub use flags::{CountryFlag, CountryFlags, FlagReadError};
pub use game::{
	Countries, CountryView, Eu4Game, NodeView, PackedCountryTag, Province, TradeNodes,
	decode_packed_tag,
};
pub use memory::{Memory, StdVector, is_key_byte, is_name_byte, read_sso};
pub use routes::{RouteMutation, live_order};
pub use war::{CampaignStamp, CountryRef, WarRoster, check_country_restrictions, read_world};

use foch_plugin_sdk::{FochHostApi, FochStr, result};

/// A host-computed executable digest must match a compiled-in profile
/// before using the data plane. Matching is the exact digest, never a
/// version string.
pub fn supports_build(exe_sha256: &str) -> bool {
	layout::profile_for_build(exe_sha256).is_some()
}

/// The executable digest of the default compiled-in profile.
pub fn pinned_exe_sha256() -> &'static str {
	layout::default_profile().exe_sha256.as_str()
}

/// Resolve the pinned monthly economy tick and cross-check the host pattern scan.
///
/// The returned address is the hook target inside the host image. Callers do
/// not supply the tick RVA, the context or country-manager globals, or the
/// prologue pattern.
///
/// # Safety
/// `host` must be a live [`FochHostApi`] whose `exe_sha256` bytes and
/// `find_pattern` callback stay valid for this call. The pointer is an image
/// address, not a Rust reference, and is only meaningful while that image
/// stays mapped.
pub unsafe fn resolve_monthly_tick(
	host: &FochHostApi,
) -> Result<*mut core::ffi::c_void, &'static str> {
	// SAFETY: the caller keeps exe_sha256's bytes alive for this read.
	// A mismatched build is rejected before the platform gate so callers
	// never scan an image this data plane does not describe.
	let digest = unsafe { host.exe_sha256.as_str() }.unwrap_or("");
	let Some(profile) = layout::profile_for_build(digest) else {
		return Err("host executable digest does not match a compiled-in profile");
	};
	if !cfg!(all(windows, target_arch = "x86_64")) {
		return Err("monthly tick resolution requires Windows x64");
	}
	if host.exe_base.is_null() {
		return Err("host image base is null");
	}
	let tick_rva = profile.monthly_tick.rva;
	let tick_end = tick_rva.checked_add(1).ok_or("tick address overflow")?;
	let globals_end = profile
		.context_global
		.offset
		.max(profile.country_manager_global.offset)
		.checked_add(8)
		.ok_or("tick address overflow")?;
	if host.exe_size < tick_end.max(globals_end) {
		return Err("host image does not contain the pinned game fields");
	}
	let tick = (host.exe_base as usize)
		.checked_add(tick_rva)
		.ok_or("tick address overflow")?;
	let mut resolved = core::ptr::null_mut();
	// SAFETY: the caller supplies a live find_pattern callback. The pattern
	// lives in the compiled-in profile and is only borrowed for this call.
	let code = unsafe {
		(host.find_pattern)(
			host,
			FochStr::from_str(&profile.monthly_tick.pattern),
			&mut resolved,
		)
	};
	if code != result::OK || resolved.is_null() || resolved as usize != tick {
		return Err("verified monthly callback signature or RVA did not match");
	}
	Ok(resolved)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn guarded_accessors_reject_the_null_page() {
		let memory = Memory;
		assert_eq!(memory.read_qword(0x1000), None);
		assert_eq!(memory.read_qword(8), None); // aligned but below the gate
		assert_eq!(unsafe { memory.write_i32(0x20, 1) }, None);
	}

	/// Build a fake in-process object so the pointer-walking readers can
	/// be tested against real (aligned, plausible) addresses.
	#[cfg(windows)]
	fn scratch(qwords: usize) -> (Vec<u64>, usize) {
		let mut buffer = vec![0_u64; qwords];
		let address = buffer.as_mut_ptr() as usize;
		buffer[0] = address as u64; // self-pointer handy for SSO data
		(buffer, address)
	}

	#[test]
	#[cfg(windows)]
	fn sso_keys_read_inline_and_pointer_forms() {
		let memory = Memory;
		let (mut buffer, address) = scratch(6);
		buffer[0] = u64::from_le_bytes(*b"zanzibar");
		buffer[1] = 0;
		buffer[2] = 8; // length at +0x10
		buffer[3] = 0xF; // capacity at +0x18: inline
		assert_eq!(
			read_sso(&memory, address, is_key_byte).as_deref(),
			Some("zanzibar")
		);
		// Pointer form: capacity > 0xF dereferences the first qword,
		// aimed at key bytes living later in the same allocation.
		let (mut buffer, address) = scratch(6);
		buffer[0] = (address + 8 * 5) as u64;
		buffer[2] = 4;
		buffer[3] = 0x1F;
		buffer[5] = u64::from_le_bytes(*b"siamxxxx");
		assert_eq!(
			read_sso(&memory, address, is_key_byte).as_deref(),
			Some("siam")
		);
	}

	#[test]
	#[cfg(windows)]
	fn sso_reads_reject_implausible_lengths_and_charsets() {
		let memory = Memory;
		let (mut buffer, address) = scratch(6);
		buffer[2] = 65; // beyond the documented key range
		buffer[3] = 0xF;
		assert_eq!(read_sso(&memory, address, is_key_byte), None);
		buffer[2] = 4;
		buffer[0] = u64::from_le_bytes([b'U', b'P', b'P', b'E', 0, 0, 0, 0]);
		assert_eq!(read_sso(&memory, address, is_key_byte), None);
		// The display-name charset accepts the uppercase.
		assert_eq!(
			read_sso(&memory, address, is_name_byte).as_deref(),
			Some("UPPE")
		);
	}

	#[test]
	fn open_entries_stay_first_in_vanilla_order() {
		assert_eq!(live_order(&[false, false]), vec![0, 1]);
		assert_eq!(live_order(&[false, true, false]), vec![0, 2, 1]);
		assert_eq!(live_order(&[true, true]), vec![0, 1]);
		assert_eq!(live_order(&[true, false, true]), vec![1, 0, 2]);
		assert!(live_order(&[]).is_empty());
	}

	fn host(exe_sha256: &str, exe_base: usize, exe_size: usize) -> foch_plugin_sdk::FochHostApi {
		use foch_plugin_sdk::*;
		unsafe extern "C" fn log(_: *const FochHostApi, _: i32, _: FochStr) {}
		unsafe extern "C" fn report(_: *const FochHostApi, _: *const FochStatus) {}
		unsafe extern "C" fn dir(_: *const FochHostApi, _: i32) -> FochStr {
			FochStr::EMPTY
		}
		unsafe extern "C" fn config(_: *const FochHostApi) -> FochStr {
			FochStr::EMPTY
		}
		unsafe extern "C" fn plugin_state(_: *const FochHostApi, _: FochStr) -> i32 {
			state::INACTIVE
		}
		unsafe extern "C" fn find(
			host: *const FochHostApi,
			pattern: FochStr,
			out: *mut *mut core::ffi::c_void,
		) -> i32 {
			FIND_CALLS.fetch_add(1, Ordering::Relaxed);
			// SAFETY: the test pattern is a static literal borrowed for this call.
			let seen = unsafe { pattern.as_str() }.unwrap_or("");
			let expected = super::profile::default_profile();
			if seen != expected.monthly_tick.pattern {
				return result::E_INVALID;
			}
			match FIND_MODE.load(Ordering::Relaxed) {
				1 => {
					// SAFETY: `host` and `out` belong to this synchronous test call.
					// The image base is a plausible pointer and still not the tick.
					let base = unsafe { (*host).exe_base };
					unsafe { *out = base };
					result::OK
				}
				2 => {
					// SAFETY: `host` is the live table passed to resolve_monthly_tick.
					let base = unsafe { (*host).exe_base } as usize;
					unsafe {
						*out = base.wrapping_add(super::profile::default_profile().monthly_tick.rva)
							as *mut core::ffi::c_void;
					}
					result::OK
				}
				_ => result::E_UNSUPPORTED,
			}
		}
		unsafe extern "C" fn create(
			_: *const FochHostApi,
			_: *mut core::ffi::c_void,
			_: *mut core::ffi::c_void,
			_: i32,
			_: *mut *const *const core::ffi::c_void,
			_: *mut *mut FochHook,
		) -> i32 {
			result::E_UNSUPPORTED
		}
		unsafe extern "C" fn enable(_: *const FochHostApi, _: *mut FochHook, _: i32) -> i32 {
			result::E_UNSUPPORTED
		}
		unsafe extern "C" fn patch(
			_: *const FochHostApi,
			_: *mut core::ffi::c_void,
			_: *const u8,
			_: usize,
			_: *mut *mut FochPatch,
		) -> i32 {
			result::E_UNSUPPORTED
		}
		unsafe extern "C" fn patch_enable(_: *const FochHostApi, _: *mut FochPatch, _: i32) -> i32 {
			result::E_UNSUPPORTED
		}
		FochHostApi {
			size: size_of::<FochHostApi>() as u32,
			abi_major: ABI_MAJOR,
			abi_minor: ABI_MINOR,
			plugin_id: FochStr::from_str("dev.foch.dataplane"),
			run_id: FochStr::from_str("unit"),
			game_version: FochStr::from_str("1.37.5"),
			exe_sha256: FochStr::from_str(exe_sha256),
			exe_base: exe_base as *mut core::ffi::c_void,
			exe_size,
			log,
			report,
			dir,
			config_json: config,
			plugin_state,
			find_pattern: find,
			hook_create: create,
			hook_set_enabled: enable,
			patch_create: patch,
			patch_set_enabled: patch_enable,
		}
	}

	use std::sync::atomic::{AtomicUsize, Ordering};
	static FIND_CALLS: AtomicUsize = AtomicUsize::new(0);
	static FIND_MODE: AtomicUsize = AtomicUsize::new(0);

	#[test]
	fn resolve_monthly_tick_rejects_bad_digest_size_and_mismatched_address() {
		FIND_CALLS.store(0, Ordering::Relaxed);
		FIND_MODE.store(2, Ordering::Relaxed);
		let pinned = pinned_exe_sha256();
		for digest in ["", "1.37.5", "c9ec55a4", &pinned.to_ascii_uppercase()] {
			let host = host(digest, 0x1400_0000, 0x300_0000);
			// SAFETY: `host` is a complete test table whose callbacks stay live.
			let error = unsafe { resolve_monthly_tick(&host) }.unwrap_err();
			assert_eq!(
				error, "host executable digest does not match a compiled-in profile",
				"{digest}"
			);
		}
		assert_eq!(FIND_CALLS.load(Ordering::Relaxed), 0);
		if !cfg!(all(windows, target_arch = "x86_64")) {
			let host = host(pinned, 0x1400_0000, 0x300_0000);
			assert_eq!(
				unsafe { resolve_monthly_tick(&host) }.unwrap_err(),
				"monthly tick resolution requires Windows x64"
			);
			assert_eq!(FIND_CALLS.load(Ordering::Relaxed), 0);
			return;
		}

		FIND_MODE.store(0, Ordering::Relaxed);
		let null_base = host(pinned, 0, 0x300_0000);
		assert_eq!(
			unsafe { resolve_monthly_tick(&null_base) }.unwrap_err(),
			"host image base is null"
		);
		let pinned_profile = super::profile::default_profile();
		let short = host(pinned, 0x1400_0000, pinned_profile.monthly_tick.rva);
		assert_eq!(
			unsafe { resolve_monthly_tick(&short) }.unwrap_err(),
			"host image does not contain the pinned game fields"
		);
		let globals_clipped = host(
			pinned,
			0x1400_0000,
			pinned_profile
				.context_global
				.offset
				.max(pinned_profile.country_manager_global.offset),
		);
		assert_eq!(
			unsafe { resolve_monthly_tick(&globals_clipped) }.unwrap_err(),
			"host image does not contain the pinned game fields"
		);
		assert_eq!(FIND_CALLS.load(Ordering::Relaxed), 0);

		FIND_MODE.store(1, Ordering::Relaxed);
		let mismatched = host(pinned, 0x1400_0000, 0x300_0000);
		assert_eq!(
			unsafe { resolve_monthly_tick(&mismatched) }.unwrap_err(),
			"verified monthly callback signature or RVA did not match"
		);
		assert_eq!(FIND_CALLS.load(Ordering::Relaxed), 1);

		FIND_MODE.store(0, Ordering::Relaxed);
		let refused = host(pinned, 0x1400_0000, 0x300_0000);
		assert_eq!(
			unsafe { resolve_monthly_tick(&refused) }.unwrap_err(),
			"verified monthly callback signature or RVA did not match"
		);

		FIND_MODE.store(2, Ordering::Relaxed);
		// Both embedded 1.37.5 builds resolve through their own registry
		// entry; the stock Steam profile carries the identical tick RVA.
		const STOCK: &str = "9ad3efe1af169f40ee577f9dae5debbc87af6fb8b5450fb345ebf110dc4d771a";
		let stock_profile = layout::profile_for_build(STOCK).expect("stock Steam profile");
		for (digest, profile) in [(pinned, pinned_profile), (STOCK, stock_profile)] {
			let matched = host(digest, 0x1400_0000, 0x300_0000);
			let tick = unsafe { resolve_monthly_tick(&matched) }.unwrap();
			assert_eq!(
				tick as usize,
				0x1400_0000 + profile.monthly_tick.rva,
				"{digest}"
			);
		}
	}
}
