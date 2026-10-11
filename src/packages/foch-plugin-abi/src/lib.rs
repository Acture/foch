//! Rust definitions of the Foch native plugin interface, ABI 1.0.
//!
//! `include/foch_plugin.h` is the normative C definition; these types mirror
//! it field for field so a Rust plugin and the Rust host share one layout. A
//! plugin exports [`ENTRY`] with the [`GetApi`] signature. See the header for
//! the ownership, threading and versioning rules.

#![no_std]

use core::ffi::c_void;

pub const ABI_MAJOR: u32 = 1;
pub const ABI_MINOR: u32 = 0;

/// The one export every native plugin provides.
pub const ENTRY: &str = "foch_plugin_get_api";
pub const ENTRY_NUL: &[u8] = b"foch_plugin_get_api\0";

/// UTF-8 with an explicit length; no NUL terminator required.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FochStr {
	pub ptr: *const u8,
	pub len: usize,
}

impl FochStr {
	pub const EMPTY: Self = Self {
		ptr: core::ptr::null(),
		len: 0,
	};

	pub const fn from_str(text: &str) -> Self {
		Self {
			ptr: text.as_ptr(),
			len: text.len(),
		}
	}

	/// # Safety
	/// `ptr` must point to `len` readable bytes for the returned lifetime.
	pub unsafe fn as_bytes<'a>(&self) -> &'a [u8] {
		if self.ptr.is_null() || self.len == 0 {
			&[]
		} else {
			unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
		}
	}

	/// # Safety
	/// As [`FochStr::as_bytes`]. Invalid UTF-8 yields `None`.
	pub unsafe fn as_str<'a>(&self) -> Option<&'a str> {
		core::str::from_utf8(unsafe { self.as_bytes() }).ok()
	}
}

pub mod result {
	pub const OK: i32 = 0;
	pub const E_INVALID: i32 = -1;
	pub const E_UNSUPPORTED: i32 = -2;
	pub const E_CONFLICT: i32 = -3;
	pub const E_MEMORY: i32 = -4;
	pub const E_NOT_FOUND: i32 = -5;
	pub const E_AMBIGUOUS: i32 = -6;
}

pub mod state {
	pub const INITIALIZING: i32 = 1;
	pub const ACTIVE: i32 = 2;
	pub const INACTIVE: i32 = 3;
	pub const REFUSED: i32 = 4;
	pub const FAILED: i32 = 5;
}

pub mod caps {
	pub const LIVE_CONFIG: u64 = 1 << 0;
	pub const LIVE_TOGGLE: u64 = 1 << 1;
	pub const SHUTDOWN: u64 = 1 << 2;
}

pub mod dir {
	pub const DATA: i32 = 1;
	pub const CACHE: i32 = 2;
	pub const LOG: i32 = 3;
	pub const PLUGIN: i32 = 4;
}

pub mod log {
	pub const ERROR: i32 = 1;
	pub const WARN: i32 = 2;
	pub const INFO: i32 = 3;
	pub const DEBUG: i32 = 4;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FochStatus {
	pub size: u32,
	pub state: i32,
	pub reason_code: i32,
	pub detail: FochStr,
}

impl FochStatus {
	pub const fn new(state: i32) -> Self {
		Self {
			size: core::mem::size_of::<Self>() as u32,
			state,
			reason_code: 0,
			detail: FochStr::EMPTY,
		}
	}
}

/// Opaque host handle for one registered detour.
#[repr(C)]
pub struct FochHook {
	_private: [u8; 0],
}

/// Opaque host handle for one registered byte patch.
#[repr(C)]
pub struct FochPatch {
	_private: [u8; 0],
}

#[repr(C)]
pub struct FochHostApi {
	pub size: u32,
	pub abi_major: u32,
	pub abi_minor: u32,
	pub plugin_id: FochStr,
	pub run_id: FochStr,
	pub game_version: FochStr,
	pub exe_sha256: FochStr,
	pub exe_base: *mut c_void,
	pub exe_size: usize,

	pub log: unsafe extern "C" fn(host: *const FochHostApi, level: i32, message: FochStr),
	pub report: unsafe extern "C" fn(host: *const FochHostApi, status: *const FochStatus),
	pub dir: unsafe extern "C" fn(host: *const FochHostApi, kind: i32) -> FochStr,
	pub config_json: unsafe extern "C" fn(host: *const FochHostApi) -> FochStr,
	pub plugin_state: unsafe extern "C" fn(host: *const FochHostApi, plugin_id: FochStr) -> i32,

	pub find_pattern: unsafe extern "C" fn(
		host: *const FochHostApi,
		pattern: FochStr,
		address: *mut *mut c_void,
	) -> i32,
	pub hook_create: unsafe extern "C" fn(
		host: *const FochHostApi,
		target: *mut c_void,
		detour: *mut c_void,
		priority: i32,
		next: *mut *const *const c_void,
		hook: *mut *mut FochHook,
	) -> i32,
	pub hook_set_enabled:
		unsafe extern "C" fn(host: *const FochHostApi, hook: *mut FochHook, enabled: i32) -> i32,
	pub patch_create: unsafe extern "C" fn(
		host: *const FochHostApi,
		address: *mut c_void,
		bytes: *const u8,
		len: usize,
		patch: *mut *mut FochPatch,
	) -> i32,
	pub patch_set_enabled:
		unsafe extern "C" fn(host: *const FochHostApi, patch: *mut FochPatch, enabled: i32) -> i32,
}

#[repr(C)]
pub struct FochPluginApi {
	pub size: u32,
	pub abi_major: u32,
	pub abi_minor: u32,
	pub caps: u64,
	pub init: unsafe extern "C" fn(host: *const FochHostApi) -> i32,
	pub status: unsafe extern "C" fn(out: *mut FochStatus),
	pub config_changed: Option<unsafe extern "C" fn(config_json: FochStr) -> i32>,
	pub set_enabled: Option<unsafe extern "C" fn(enabled: i32) -> i32>,
	pub shutdown: Option<unsafe extern "C" fn()>,
}

// SAFETY: the table is immutable plain data shared read-only across threads.
unsafe impl Sync for FochPluginApi {}

pub type GetApi = unsafe extern "C" fn(host_abi_major: u32) -> *const FochPluginApi;

#[cfg(test)]
mod tests {
	use super::*;
	use core::mem::{offset_of, size_of};

	// Offsets are part of the ABI; these pin the x64 layout the C header
	// produces with MSVC.
	#[cfg(target_pointer_width = "64")]
	#[test]
	fn layout_matches_the_c_header_on_x64() {
		assert_eq!(size_of::<FochStr>(), 16);
		assert_eq!(size_of::<FochStatus>(), 32);
		assert_eq!(offset_of!(FochHostApi, plugin_id), 16);
		assert_eq!(offset_of!(FochHostApi, exe_base), 80);
		assert_eq!(offset_of!(FochHostApi, log), 96);
		assert_eq!(offset_of!(FochHostApi, find_pattern), 136);
		assert_eq!(offset_of!(FochHostApi, patch_set_enabled), 168);
		assert_eq!(size_of::<FochHostApi>(), 176);
		assert_eq!(offset_of!(FochPluginApi, caps), 16);
		assert_eq!(offset_of!(FochPluginApi, init), 24);
		assert_eq!(size_of::<FochPluginApi>(), 64);
	}
}
