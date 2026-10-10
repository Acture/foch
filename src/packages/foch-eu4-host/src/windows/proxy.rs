//! Argument-preserving tail jumps to the actual system VERSION exports.

use core::arch::naked_asm;
use windows_sys::Win32::System::{
	LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
	SystemInformation::GetSystemDirectoryW,
};

static mut EXPORTS: [usize; 17] = [0; 17];

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

pub(super) unsafe fn initialize() -> bool {
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
	for (index, name) in NAMES.iter().enumerate() {
		let Some(function) = (unsafe { GetProcAddress(module, name.as_ptr()) }) else {
			return false;
		};
		// Written once under the loader lock, before any caller can execute an
		// export. The system DLL is deliberately retained for process lifetime.
		unsafe {
			core::ptr::addr_of_mut!(EXPORTS)
				.cast::<usize>()
				.add(index)
				.write(function as usize);
		}
	}
	true
}

macro_rules! forward {
	($name:ident, $index:expr) => {
		#[unsafe(naked)]
		#[unsafe(no_mangle)]
		pub unsafe extern "system" fn $name() {
			naked_asm!("jmp qword ptr [rip + {table} + {offset}]", table = sym EXPORTS, offset = const ($index * 8));
		}
	};
}

forward!(GetFileVersionInfoA, 0);
forward!(GetFileVersionInfoByHandle, 1);
forward!(GetFileVersionInfoExA, 2);
forward!(GetFileVersionInfoExW, 3);
forward!(GetFileVersionInfoSizeA, 4);
forward!(GetFileVersionInfoSizeExA, 5);
forward!(GetFileVersionInfoSizeExW, 6);
forward!(GetFileVersionInfoSizeW, 7);
forward!(GetFileVersionInfoW, 8);
forward!(VerFindFileA, 9);
forward!(VerFindFileW, 10);
forward!(VerInstallFileA, 11);
forward!(VerInstallFileW, 12);
forward!(VerLanguageNameA, 13);
forward!(VerLanguageNameW, 14);
forward!(VerQueryValueA, 15);
forward!(VerQueryValueW, 16);
