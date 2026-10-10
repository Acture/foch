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
	let mut old = 0;
	if unsafe { VirtualProtect(address.cast(), GATE_BYTES, PAGE_EXECUTE_READWRITE, &mut old) } == 0
	{
		return false;
	}
	unsafe {
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), address, GATE_BYTES);
	}
	let flushed =
		unsafe { FlushInstructionCache(GetCurrentProcess(), address.cast(), GATE_BYTES) } != 0;
	let mut discarded = 0;
	let protected = unsafe { VirtualProtect(address.cast(), GATE_BYTES, old, &mut discarded) } != 0;
	flushed && protected
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
	let _ = std::panic::catch_unwind(|| super::host::start(IMAGE_SIZE.load(Ordering::Acquire)));
}

#[unsafe(naked)]
unsafe extern "system" fn entry_gate() {
	// Windows x64 entry has RSP%16 == 8. Save all volatile registers and flags;
	// reserve shadow space plus XMM0..5 and align RSP before calling Rust.
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
