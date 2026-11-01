//! Committed-page checks shared by plugin loading and the address linker.

use std::{ffi::c_void, mem::size_of};
use windows_sys::Win32::System::Memory::{
	MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
	PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
	PAGE_WRITECOPY, VirtualQuery,
};

pub(super) fn executable(ptr: *const c_void) -> bool {
	let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
	let queried = unsafe { VirtualQuery(ptr, &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
	queried != 0
		&& info.State == MEM_COMMIT
		&& info.Protect & (PAGE_GUARD | PAGE_NOACCESS) == 0
		&& info.Protect & 0xf0 != 0
}

pub(super) fn readable(ptr: *const u8, len: usize) -> bool {
	if ptr.is_null() {
		return false;
	}
	let Some(end) = (ptr as usize).checked_add(len) else {
		return false;
	};
	let mut at = ptr as usize;
	while at < end {
		let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
		if unsafe {
			VirtualQuery(
				at as *const c_void,
				&mut info,
				size_of::<MEMORY_BASIC_INFORMATION>(),
			)
		} == 0 || info.State != MEM_COMMIT
			|| info.Protect & (PAGE_GUARD | PAGE_NOACCESS) != 0
			|| !matches!(
				info.Protect & 0xff,
				PAGE_READONLY
					| PAGE_READWRITE
					| PAGE_WRITECOPY
					| PAGE_EXECUTE_READ
					| PAGE_EXECUTE_READWRITE
					| PAGE_EXECUTE_WRITECOPY
			) {
			return false;
		}
		let Some(next) = (info.BaseAddress as usize).checked_add(info.RegionSize) else {
			return false;
		};
		if next <= at {
			return false;
		}
		at = next;
	}
	true
}
