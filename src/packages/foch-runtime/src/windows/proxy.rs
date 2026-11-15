//! Argument-preserving tail jumps to the actual system VERSION exports.

use core::arch::naked_asm;
use core::ffi::c_void;
use std::sync::{
	OnceLock,
	atomic::{AtomicPtr, Ordering},
};
use windows_sys::Win32::System::{
	LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
	SystemInformation::GetSystemDirectoryW,
};
use windows_sys::Win32::{
	Foundation::{GetLastError, SetLastError},
	System::Threading::ExitProcess,
};

// Each slot initially reaches its own argument-preserving lazy resolver.
// Atomic publication lets concurrent first calls use either valid target.
static EXPORTS: [AtomicPtr<c_void>; 17] = [
	AtomicPtr::new(resolve_info_a as *mut c_void),
	AtomicPtr::new(resolve_info_handle as *mut c_void),
	AtomicPtr::new(resolve_info_ex_a as *mut c_void),
	AtomicPtr::new(resolve_info_ex_w as *mut c_void),
	AtomicPtr::new(resolve_size_a as *mut c_void),
	AtomicPtr::new(resolve_size_ex_a as *mut c_void),
	AtomicPtr::new(resolve_size_ex_w as *mut c_void),
	AtomicPtr::new(resolve_size_w as *mut c_void),
	AtomicPtr::new(resolve_info_w as *mut c_void),
	AtomicPtr::new(resolve_find_a as *mut c_void),
	AtomicPtr::new(resolve_find_w as *mut c_void),
	AtomicPtr::new(resolve_install_a as *mut c_void),
	AtomicPtr::new(resolve_install_w as *mut c_void),
	AtomicPtr::new(resolve_language_a as *mut c_void),
	AtomicPtr::new(resolve_language_w as *mut c_void),
	AtomicPtr::new(resolve_query_a as *mut c_void),
	AtomicPtr::new(resolve_query_w as *mut c_void),
];
static INITIALIZED: OnceLock<bool> = OnceLock::new();

const NAMES: [&[u8]; 17] = [
	b"GetFileVersionInfoA\0",
	b"GetFileVersionInfoByHandle\0",
	b"GetFileVersionInfoExA\0",
	b"GetFileVersionInfoExW\0",
	b"GetFileVersionInfoSizeA\0",
	b"GetFileVersionInfoSizeExA\0",
	b"GetFileVersionInfoSizeExW\0",
	b"GetFileVersionInfoSizeW\0",
	b"GetFileVersionInfoW\0",
	b"VerFindFileA\0",
	b"VerFindFileW\0",
	b"VerInstallFileA\0",
	b"VerInstallFileW\0",
	b"VerLanguageNameA\0",
	b"VerLanguageNameW\0",
	b"VerQueryValueA\0",
	b"VerQueryValueW\0",
];

// Called at the executable entry gate or on the first forwarded API call,
// never by this proxy's DllMain. Preserve the caller's Win32 last-error value.
pub(super) extern "system" fn initialize() {
	let error = unsafe { GetLastError() };
	if !INITIALIZED.get_or_init(|| unsafe { load_system() }) {
		unsafe { ExitProcess(126) };
	}
	unsafe { SetLastError(error) };
}

unsafe fn load_system() -> bool {
	let mut path = [0u16; 32768];
	let len = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), path.len() as u32) } as usize;
	let suffix: [u16; 13] = [92, 118, 101, 114, 115, 105, 111, 110, 46, 100, 108, 108, 0];
	if len == 0 || len + suffix.len() > path.len() {
		return false;
	}
	path[len..len + suffix.len()].copy_from_slice(&suffix);
	let module = unsafe {
		LoadLibraryExW(
			path.as_ptr(),
			std::ptr::null_mut(),
			LOAD_LIBRARY_SEARCH_SYSTEM32,
		)
	};
	if module.is_null() {
		return false;
	}
	let mut targets = [std::ptr::null_mut(); 17];
	for (index, name) in NAMES.iter().enumerate() {
		let Some(function) = (unsafe { GetProcAddress(module, name.as_ptr()) }) else {
			return false;
		};
		targets[index] = function as *mut c_void;
	}
	// Resolve the complete table before publishing it. Retain the system DLL
	// for process lifetime, including forwarding-only consumers.
	for (slot, target) in EXPORTS.iter().zip(targets) {
		slot.store(target, Ordering::Release);
	}
	true
}

macro_rules! forward {
	($name:ident, $resolve:ident, $index:expr) => {
		#[unsafe(naked)]
		#[unsafe(no_mangle)]
		pub unsafe extern "system" fn $name() {
			naked_asm!("jmp qword ptr [rip + {table} + {offset}]", table = sym EXPORTS, offset = const ($index * 8));
		}
		#[unsafe(naked)]
		unsafe extern "system" fn $resolve() {
			// Match the gate's x64 register/flags preservation; stack arguments
			// and the caller's return address remain intact for the tail jump.
			// Eight pushes use 64 bytes; 136 adds shadow/XMM space plus the
			// 8-byte pad that aligns RSP to 16 before calling initialize.
			naked_asm!(
				"pushfq", "push rax", "push rcx", "push rdx", "push r8", "push r9", "push r10", "push r11",
				"sub rsp, 136",
				"movdqu [rsp + 32], xmm0", "movdqu [rsp + 48], xmm1", "movdqu [rsp + 64], xmm2", "movdqu [rsp + 80], xmm3",
				"movdqu [rsp + 96], xmm4", "movdqu [rsp + 112], xmm5",
				"call {initialize}",
				"movdqu xmm0, [rsp + 32]", "movdqu xmm1, [rsp + 48]", "movdqu xmm2, [rsp + 64]", "movdqu xmm3, [rsp + 80]",
				"movdqu xmm4, [rsp + 96]", "movdqu xmm5, [rsp + 112]",
				"add rsp, 136",
				"pop r11", "pop r10", "pop r9", "pop r8", "pop rdx", "pop rcx", "pop rax", "popfq",
				"jmp qword ptr [rip + {table} + {offset}]",
				initialize = sym initialize, table = sym EXPORTS, offset = const ($index * 8),
			);
		}
	};
}

forward!(GetFileVersionInfoA, resolve_info_a, 0);
forward!(GetFileVersionInfoByHandle, resolve_info_handle, 1);
forward!(GetFileVersionInfoExA, resolve_info_ex_a, 2);
forward!(GetFileVersionInfoExW, resolve_info_ex_w, 3);
forward!(GetFileVersionInfoSizeA, resolve_size_a, 4);
forward!(GetFileVersionInfoSizeExA, resolve_size_ex_a, 5);
forward!(GetFileVersionInfoSizeExW, resolve_size_ex_w, 6);
forward!(GetFileVersionInfoSizeW, resolve_size_w, 7);
forward!(GetFileVersionInfoW, resolve_info_w, 8);
forward!(VerFindFileA, resolve_find_a, 9);
forward!(VerFindFileW, resolve_find_w, 10);
forward!(VerInstallFileA, resolve_install_a, 11);
forward!(VerInstallFileW, resolve_install_w, 12);
forward!(VerLanguageNameA, resolve_language_a, 13);
forward!(VerLanguageNameW, resolve_language_w, 14);
forward!(VerQueryValueA, resolve_query_a, 15);
forward!(VerQueryValueW, resolve_query_w, 16);
