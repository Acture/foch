//! Windows x64 proxy and in-process plugin loading.

mod gate;
mod host;
mod proxy;

use std::{
	ffi::c_void,
	sync::atomic::{AtomicUsize, Ordering},
};
use windows_sys::{
	Win32::{Foundation::HMODULE, System::LibraryLoader::GetModuleFileNameW},
	core::BOOL,
};

static MODULE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: called by Windows with valid loader arguments. No plugin code runs
// here: forwarding slots are static and only a one-shot gate is prepared.
#[unsafe(no_mangle)]
unsafe extern "system" fn DllMain(module: HMODULE, reason: u32, reserved: *mut c_void) -> BOOL {
	if reason != 1 {
		return 1;
	}
	MODULE.store(module as usize, Ordering::Release);
	// A dynamically loaded proxy is forwarding-only. Installing a gate after
	// the executable has already started would overwrite live code.
	if !reserved.is_null() && unsafe { is_eu4() } && !unsafe { gate::install() } {
		return 0;
	}
	1
}

unsafe fn is_eu4() -> bool {
	let mut path = [0u16; 32768];
	let len =
		unsafe { GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), path.len() as u32) }
			as usize;
	if len == 0 || len >= path.len() {
		return false;
	}
	let name = path[..len]
		.rsplit(|c| *c == b'\\' as u16 || *c == b'/' as u16)
		.next()
		.unwrap_or_default();
	name.len() == 7
		&& name
			.iter()
			.zip(b"eu4.exe")
			.all(|(have, want)| *have <= 127 && (*have as u8).eq_ignore_ascii_case(want))
}
