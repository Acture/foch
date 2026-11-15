//! One-shot gate: restore bytes before their first execution; no relocated
//! instructions, permanent detours, or remote-process writes are involved.

use crate::image::{self, GATE_BYTES};
use core::{arch::naked_asm, ffi::c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::System::{
	Diagnostics::Debug::FlushInstructionCache,
	LibraryLoader::GetModuleHandleW,
	Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect},
	ProcessStatus::{K32GetModuleInformation, MODULEINFO},
	SystemInformation::{GetSystemInfo, SYSTEM_INFO},
	Threading::{ExitProcess, GetCurrentProcess},
};

static mut ENTRY: usize = 0;
static mut ORIGINAL: [u8; GATE_BYTES] = [0; GATE_BYTES];
static IMAGE_SIZE: AtomicUsize = AtomicUsize::new(0);

fn jump() -> [u8; GATE_BYTES] {
	let mut bytes = [0u8; GATE_BYTES];
	bytes[..6].copy_from_slice(&[0xff, 0x25, 0, 0, 0, 0]);
	bytes[6..].copy_from_slice(&(entry_gate as *const () as usize).to_le_bytes());
	bytes
}

pub(super) unsafe fn install() -> bool {
	let base = unsafe { GetModuleHandleW(std::ptr::null()) };
	let mut info: MODULEINFO = unsafe { core::mem::zeroed() };
	if base.is_null()
		|| unsafe {
			K32GetModuleInformation(
				GetCurrentProcess(),
				base,
				&mut info,
				core::mem::size_of::<MODULEINFO>() as u32,
			)
		} == 0
	{
		return false;
	}
	let headers = unsafe {
		core::slice::from_raw_parts(base.cast::<u8>(), (info.SizeOfImage as usize).min(4096))
	};
	let Ok(image) = image::inspect(headers) else {
		return false;
	};
	if image.size != info.SizeOfImage as usize {
		return false;
	}
	let entry = unsafe { base.cast::<u8>().add(image.entry) };
	let mut system: SYSTEM_INFO = unsafe { core::mem::zeroed() };
	unsafe { GetSystemInfo(&mut system) };
	// VirtualProtect returns the previous protection of only the first page.
	// Refuse a cross-page gate so both writes can restore it exactly.
	if !image::gate_fits_page(entry as usize, system.dwPageSize as usize) {
		return false;
	}
	unsafe {
		ENTRY = entry as usize;
		core::ptr::copy_nonoverlapping(
			entry,
			core::ptr::addr_of_mut!(ORIGINAL).cast::<u8>(),
			GATE_BYTES,
		);
	}
	IMAGE_SIZE.store(image.size, Ordering::Release);
	unsafe { write(entry, &jump()) }
}

unsafe fn write(address: *mut u8, bytes: &[u8; GATE_BYTES]) -> bool {
	let result = unsafe {
		write_with(
			address,
			bytes,
			&mut |address, protection, old| {
				VirtualProtect(address, GATE_BYTES, protection, old) != 0
			},
			&mut |address| FlushInstructionCache(GetCurrentProcess(), address, GATE_BYTES) != 0,
		)
	};
	match result {
		Ok(()) => true,
		Err(WriteError::OriginalIntact) => false,
		// A failed rollback must not let DllMain return FALSE and unload code
		// still targeted by the executable entry point.
		Err(WriteError::Unrecoverable) => unsafe { ExitProcess(126) },
	}
}

#[derive(Debug, PartialEq)]
enum WriteError {
	OriginalIntact,
	Unrecoverable,
}

unsafe fn write_with(
	address: *mut u8,
	bytes: &[u8; GATE_BYTES],
	protect: &mut impl FnMut(*mut c_void, u32, *mut u32) -> bool,
	flush: &mut impl FnMut(*const c_void) -> bool,
) -> Result<(), WriteError> {
	let original = unsafe { address.cast::<[u8; GATE_BYTES]>().read_unaligned() };
	let mut old = 0;
	if !protect(address.cast(), PAGE_EXECUTE_READWRITE, &mut old) {
		return Err(WriteError::OriginalIntact);
	}
	unsafe {
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), address, GATE_BYTES);
	}
	let flushed = flush(address.cast());
	let mut discarded = 0;
	let protected = protect(address.cast(), old, &mut discarded);
	if flushed && protected {
		return Ok(());
	}
	if !protect(address.cast(), PAGE_EXECUTE_READWRITE, &mut discarded) {
		return Err(WriteError::Unrecoverable);
	}
	unsafe { core::ptr::copy_nonoverlapping(original.as_ptr(), address, GATE_BYTES) };
	let flushed = flush(address.cast());
	let protected = protect(address.cast(), old, &mut discarded);
	if flushed && protected {
		Err(WriteError::OriginalIntact)
	} else {
		Err(WriteError::Unrecoverable)
	}
}

unsafe extern "system" fn enter() {
	let address = unsafe { ENTRY as *mut u8 };
	let expected = jump();
	let actual = unsafe { core::slice::from_raw_parts(address, GATE_BYTES) };
	// Refuse to overwrite a change made by another DLL's initialization.
	if actual != expected || !unsafe { write(address, &*core::ptr::addr_of!(ORIGINAL)) } {
		unsafe {
			ExitProcess(126);
		}
	}
	// Panics must not unwind through the kernel's executable-start frame.
	super::proxy::initialize();
	let _ = std::panic::catch_unwind(|| super::host::start(IMAGE_SIZE.load(Ordering::Acquire)));
}

#[unsafe(naked)]
unsafe extern "system" fn entry_gate() {
	// Windows x64 entry has RSP%16 == 8. Eight pushes use 64 bytes;
	// subtract 136 = 32 shadow + 96 XMM + 8 alignment, so RSP%16 == 0
	// before calling Rust. Preserve all volatile registers and flags.
	naked_asm!(
		"pushfq", "push rax", "push rcx", "push rdx", "push r8", "push r9", "push r10", "push r11",
		"sub rsp, 136",
		"movdqu [rsp + 32], xmm0", "movdqu [rsp + 48], xmm1", "movdqu [rsp + 64], xmm2", "movdqu [rsp + 80], xmm3",
		"movdqu [rsp + 96], xmm4", "movdqu [rsp + 112], xmm5",
		"call {enter}",
		"movdqu xmm0, [rsp + 32]", "movdqu xmm1, [rsp + 48]", "movdqu xmm2, [rsp + 64]", "movdqu xmm3, [rsp + 80]",
		"movdqu xmm4, [rsp + 96]", "movdqu xmm5, [rsp + 112]",
		"add rsp, 136",
		"pop r11", "pop r10", "pop r9", "pop r8", "pop rdx", "pop rcx", "pop rax", "popfq",
		"jmp qword ptr [rip + {entry}]",
		enter = sym enter, entry = sym ENTRY,
	);
}

pub(super) fn image_base() -> *mut c_void {
	unsafe { GetModuleHandleW(std::ptr::null()) }.cast()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn partial_gate_writes_restore_bytes_cache_and_original_protection() {
		for (fail_protect, fail_flush) in [(0, 1), (2, 0), (1, 0)] {
			let original = [0x90; GATE_BYTES];
			let mut memory = original;
			let mut protection_calls = Vec::new();
			let mut flushes = 0;
			let result = unsafe {
				write_with(
					memory.as_mut_ptr(),
					&[0xcc; GATE_BYTES],
					&mut |_, protection, old| {
						protection_calls.push(protection);
						old.write(0x20);
						protection_calls.len() != fail_protect
					},
					&mut |_| {
						flushes += 1;
						flushes != fail_flush
					},
				)
			};
			assert_eq!(result, Err(WriteError::OriginalIntact));
			assert_eq!(memory, original);
			if fail_protect != 1 {
				assert_eq!(flushes, 2);
				assert_eq!(protection_calls.last(), Some(&0x20));
			}
		}
	}

	#[test]
	fn an_unrecoverable_gate_write_cannot_allow_proxy_unloading() {
		let mut memory = [0x90; GATE_BYTES];
		let mut calls = 0;
		let result = unsafe {
			write_with(
				memory.as_mut_ptr(),
				&[0xcc; GATE_BYTES],
				&mut |_, _, old| {
					calls += 1;
					old.write(0x20);
					calls != 3
				},
				&mut |_| false,
			)
		};
		assert_eq!(result, Err(WriteError::Unrecoverable));
	}
}
