//! Exact-build boundary for the native peace editor.
//!
//! Verified pricing chain of the 1.37.5 image (by disassembly; no runtime
//! observation yet): `CPeaceOffer::GetCost` (0x979060) reads the cached total
//! dword at offer +0xf0; the give-direction read only adds a 0.5% tolerance.
//! The native cost rebuild prices every selected clause through the treaty
//! vtable's +0x118 accumulator `(treaty, totals, direction, negate)`, whose
//! last argument selects the sign — the rebuild always passes 0, so a merged
//! offer's cached total is the SUM of both directions until the plugin
//! re-applies the give side with `negate` set. The per-clause magnitude comes
//! from two price columns selected by the shared +0x140 direction predicate
//! (0x99cf20), which inspects the treaty's own giver/receiver parties. The
//! warscore guard that shows `CANNOT_OFFER_TOO_HIGH_WARSCORE` is 0x97a1c0.
//! Treaty vtables hang off the kind-dispatched clone switch at 0x9a1040; the
//! base vtable 0x1c41668 keeps +0x78/+0x118 pure.
use foch_plugin_sdk::FochHostApi;
use foch_runtime::dataplane::{Memory, supports_build};

#[derive(Clone, Copy)]
pub struct CodeSite {
	pub rva: usize,
	pub original: &'static [u8],
}

impl CodeSite {
	pub fn matches(self, image: &[u8]) -> bool {
		image.get(self.rva..self.rva.saturating_add(self.original.len())) == Some(self.original)
	}
}

pub const SUMMARY_UPDATE: CodeSite = CodeSite {
	rva: 0x814d23,
	original: &[0xe8, 0x98, 0x58, 0xaf, 0x00],
};
pub const SUMMARY_SUGGEST: CodeSite = CodeSite {
	rva: 0x130fb84,
	original: &[0xe8, 0x37, 0xaa, 0xff, 0xff],
};
pub const SUMMARY_SURRENDER: CodeSite = CodeSite {
	rva: 0x13112d7,
	original: &[0xe8, 0xe4, 0x92, 0xff, 0xff],
};
pub const SEND_CALLBACK: CodeSite = CodeSite {
	rva: 0x13096b6,
	original: &[0x48, 0x8d, 0x05, 0xf3, 0x5a, 0x00, 0x00],
};

/// Call inside SEND that constructs the 0x210-byte peace action from the
/// active offer. The action stores the direction byte at +0x41, copy-constructs
/// the offer at +0x48 (one `OFFER_SIZE` span) and sets its submitted flag at
/// +0x208. Verified by disassembly of the 1.37.5 image; not yet redirected.
pub const ACTION_CONSTRUCTOR_CALL: CodeSite = CodeSite {
	rva: 0x130f22f,
	original: &[0xe8, 0xbc, 0x98, 0x28, 0xff],
};
/// Call into the treaty execution path for an accepted action; the execute
/// function is `CPeaceOffer::Execute` (log string at 0x1cd0db0) and updates
/// the war score through 0x990770 (`Execute_UpdateWarScoreImpact`).
/// [`crate::contract`] records the static fact that `Execute` itself calls
/// the all-positive rebuild. Which function the accepting AI uses, and
/// whether that function sees the net total, is still not verified in game.
pub const EXECUTE_CALL: CodeSite = CodeSite {
	rva: 0x59a6ea,
	original: &[0xe8, 0x41, 0xc0, 0x3d, 0x00],
};
pub const SUMMARY: CodeSite = CodeSite {
	rva: 0x130a5c0,
	original: &[
		0x48, 0x89, 0x4c, 0x24, 0x08, 0x55, 0x53, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56,
		0x41,
	],
};
pub const SEND: CodeSite = CodeSite {
	rva: 0x130f1b0,
	original: &[
		0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x74, 0x24, 0x18, 0x57, 0x48, 0x83, 0xec, 0x30,
		0x48,
	],
};
pub const SET_TEXT: CodeSite = CodeSite {
	rva: 0x152aea0,
	original: &[
		0x40, 0x53, 0x48, 0x83, 0xec, 0x40, 0x48, 0x8b, 0xd9, 0x48, 0xc7, 0x44, 0x24, 0x20, 0x00,
		0x00,
	],
};
pub const OFFER_CONSTRUCTOR: CodeSite = CodeSite {
	rva: 0x975d40,
	original: &[
		0x48, 0x89, 0x5c, 0x24, 0x10, 0x48, 0x89, 0x6c, 0x24, 0x18, 0x48, 0x89, 0x74, 0x24, 0x20,
		0x48,
	],
};
pub const OFFER_COPY: CodeSite = CodeSite {
	rva: 0x975f40,
	original: &[
		0x48, 0x89, 0x4c, 0x24, 0x08, 0x53, 0x55, 0x56, 0x57, 0x41, 0x56, 0x48, 0x83, 0xec, 0x20,
		0x48,
	],
};
pub const OFFER_DESTROY: CodeSite = CodeSite {
	rva: 0x9761c0,
	original: &[
		0x48, 0x89, 0x5c, 0x24, 0x08, 0x57, 0x48, 0x83, 0xec, 0x20, 0x48, 0x8d, 0x05, 0xe7, 0x16,
		0x2e,
	],
};
pub const CLONE_CLAUSE: CodeSite = CodeSite {
	rva: 0x9a1040,
	original: &[
		0x40, 0x53, 0x48, 0x83, 0xec, 0x30, 0x48, 0x8b, 0x01, 0x48, 0x8d, 0x54, 0x24, 0x20, 0x48,
		0x8b,
	],
};
pub const VECTOR_APPEND: CodeSite = CodeSite {
	rva: 0xcc220,
	original: &[
		0x40, 0x53, 0x55, 0x41, 0x55, 0x41, 0x57, 0x48, 0x83, 0xec, 0x28, 0x48, 0x8b, 0x41, 0x08,
		0x4c,
	],
};
pub const REBUILD_COST: CodeSite = CodeSite {
	rva: 0x978b20,
	original: &[
		0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x6c, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18,
		0x57,
	],
};
pub const ACTION_CONSTRUCTOR: CodeSite = CodeSite {
	rva: 0x598af0,
	original: &[
		0x48, 0x89, 0x4c, 0x24, 0x08, 0x53, 0x48, 0x83, 0xec, 0x20, 0x48, 0x8b, 0xd9, 0xc7, 0x41,
		0x08,
	],
};
pub const EXECUTE: CodeSite = CodeSite {
	rva: 0x976730,
	original: &[
		0x48, 0x89, 0x4c, 0x24, 0x08, 0x55, 0x53, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56,
		0x41,
	],
};

const CHECKS: &[CodeSite] = &[
	SUMMARY_UPDATE,
	SUMMARY_SUGGEST,
	SUMMARY_SURRENDER,
	SEND_CALLBACK,
	ACTION_CONSTRUCTOR_CALL,
	EXECUTE_CALL,
	SUMMARY,
	SEND,
	SET_TEXT,
	OFFER_CONSTRUCTOR,
	OFFER_COPY,
	OFFER_DESTROY,
	CLONE_CLAUSE,
	VECTOR_APPEND,
	REBUILD_COST,
	ACTION_CONSTRUCTOR,
	EXECUTE,
];

/// # Safety
/// The host and its image/string ranges must be valid for this ABI call.
pub unsafe fn verify(host: &FochHostApi) -> Result<(), &'static str> {
	if host.abi_major != foch_plugin_sdk::ABI_MAJOR
		|| (host.size as usize) < std::mem::size_of::<FochHostApi>()
	{
		return Err("incompatible Foch plugin host");
	}
	if !unsafe { host.exe_sha256.as_str() }.is_some_and(supports_build) {
		return Err("the peace editor supports only its verified EU4 executable");
	}
	if host.exe_base.is_null() || host.exe_size == 0 {
		return Err("the game image is absent");
	}
	if host.exe_size > isize::MAX as usize
		|| (host.exe_base as usize)
			.checked_add(host.exe_size)
			.is_none()
	{
		return Err("invalid game image extent");
	}
	let memory = Memory;
	let base = host.exe_base as usize;
	for site in CHECKS {
		let end = site
			.rva
			.checked_add(site.original.len())
			.ok_or("invalid code site")?;
		if end > host.exe_size {
			return Err("native peace editor code lies outside the game image");
		}
		// Read the whole site in one guarded bulk read, not byte by byte.
		let site_addr = base.checked_add(site.rva).ok_or("invalid code site")?;
		let mut actual = vec![0u8; site.original.len()];
		memory
			.read_bytes(site_addr, &mut actual)
			.ok_or("native peace editor code is unreadable")?;
		if actual != site.original {
			return Err("native peace editor code differs from the supported profile");
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn requires_the_whole_instruction_and_its_exact_operands() {
		let site = CodeSite {
			rva: 1,
			original: &[0xe8, 1, 2, 3, 4],
		};
		assert!(site.matches(&[0, 0xe8, 1, 2, 3, 4]));
		assert!(!site.matches(&[0, 0xe8, 1, 2, 3, 5]));
		assert!(!site.matches(&[0, 0xe8, 1, 2, 3]));
		assert!(!site.matches(&[]));
	}

	#[test]
	fn overflowing_rva_fails_closed() {
		let site = CodeSite {
			rva: usize::MAX - 1,
			original: &[1, 2, 3],
		};
		assert!(!site.matches(&[0; 8]));
	}

	/// A stub host table whose callbacks are never called: the digest gate in
	/// [`verify`] fires before any image access, so a null base is enough.
	fn host(exe_sha256: &str) -> FochHostApi {
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
			_: *const FochHostApi,
			_: FochStr,
			_: *mut *mut core::ffi::c_void,
		) -> i32 {
			result::E_UNSUPPORTED
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
			plugin_id: FochStr::from_str("dev.foch.peace"),
			run_id: FochStr::EMPTY,
			game_version: FochStr::EMPTY,
			exe_sha256: FochStr::from_str(exe_sha256),
			exe_base: core::ptr::null_mut(),
			exe_size: 0,
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

	#[test]
	fn verify_rejects_unknown_build_digests_before_touching_the_image() {
		let host = host(&"0".repeat(64));
		assert_eq!(
			// SAFETY: the stub host table and its literals outlive this call.
			unsafe { verify(&host) }.unwrap_err(),
			"the peace editor supports only its verified EU4 executable"
		);
	}

	#[test]
	fn verify_passes_the_pinned_digest_gate_before_image_checks() {
		// The gate accepted the pinned digest, so the null image is the next
		// refusal — proving both sides of the build-digest boundary.
		let host = host(foch_runtime::dataplane::pinned_exe_sha256());
		assert_eq!(
			// SAFETY: the stub host table and its literals outlive this call.
			unsafe { verify(&host) }.unwrap_err(),
			"the game image is absent"
		);
	}
}
