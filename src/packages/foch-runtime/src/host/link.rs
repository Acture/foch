//! Address linker for the reserved pattern, hook, and patch slots.
//!
//! Hooks on one target chain by descending priority, then registration order.
//! The prologue jump is installed once, while entry init is still open, and
//! later enables only republish the chain. Allocations live until process exit.

use super::{image, prologue, query};
use crate::pattern::{self, Found, Pattern};
use foch_plugin_sdk as abi;
use std::{
	ffi::c_void,
	mem::{align_of, size_of},
	sync::atomic::{AtomicPtr, Ordering},
};
use windows_sys::Win32::System::{
	Diagnostics::Debug::FlushInstructionCache,
	Memory::{
		MEM_COMMIT, MEM_FREE, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION,
		PAGE_EXECUTE_READWRITE, PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect,
		VirtualQuery,
	},
	SystemInformation::{GetSystemInfo, SYSTEM_INFO},
	Threading::{ExitProcess, GetCurrentProcess},
};

const JUMP_BYTES: usize = prologue::MIN_STEAL;

const HOOK_MAGIC: u64 = 0xF0C4_4800_4E00_0001;
const PATCH_MAGIC: u64 = 0xF0C4_4800_4E00_0002;

pub struct Linker {
	sites: Vec<Site>,
	patches: Vec<usize>,
	seq: u64,
	init_open: bool,
	page_size: usize,
}

struct Site {
	target: usize,
	original: Vec<u8>,
	installed: bool,
	trampoline: usize,
	head: usize,
	hooks: Vec<usize>,
}

struct HookNode {
	magic: u64,
	priority: i32,
	seq: u64,
	enabled: bool,
	detour: usize,
	next: usize,
	site: usize,
}

struct PatchNode {
	magic: u64,
	address: usize,
	original: Vec<u8>,
	bytes: Vec<u8>,
	enabled: bool,
}

impl Default for Linker {
	fn default() -> Self {
		Self::new()
	}
}

impl Linker {
	pub fn new() -> Self {
		let mut system: SYSTEM_INFO = unsafe { std::mem::zeroed() };
		unsafe { GetSystemInfo(&mut system) };
		Self {
			sites: Vec::new(),
			patches: Vec::new(),
			seq: 0,
			init_open: false,
			page_size: system.dwPageSize as usize,
		}
	}

	pub fn open_init(&mut self, open: bool) {
		self.init_open = open;
	}

	/// Register an inline hook at `target` redirecting to `detour`.
	///
	/// # Safety
	/// `target` and `detour` must be valid code addresses for the process
	/// lifetime, and `next`/`hook` must point to writable output slots.
	pub unsafe fn hook_create(
		&mut self,
		target: *mut c_void,
		detour: *mut c_void,
		priority: i32,
		next: *mut *const *const c_void,
		hook: *mut *mut abi::FochHook,
	) -> i32 {
		clear_hook(next, hook);
		if next.is_null() || hook.is_null() || target.is_null() || detour.is_null() {
			return abi::result::E_INVALID;
		}
		if !query::executable(target) || !query::executable(detour) {
			return abi::result::E_INVALID;
		}
		let target = target as usize;
		if !query::readable(target as *const u8, JUMP_BYTES) {
			return abi::result::E_INVALID;
		}
		if !image::fits_page(target, JUMP_BYTES, self.page_size) {
			return abi::result::E_UNSUPPORTED;
		}
		let Some(seq) = self.seq.checked_add(1) else {
			return abi::result::E_MEMORY;
		};
		if let Some(index) = self.sites.iter().position(|site| site.target == target) {
			return self.push_hook(index, detour as usize, priority, seq, next, hook);
		}
		if self.overlaps_hook(target, JUMP_BYTES) || self.overlaps_patch(target, JUMP_BYTES) {
			return abi::result::E_CONFLICT;
		}
		let Some(prefix) = hook_prefix(target, self.page_size) else {
			return abi::result::E_INVALID;
		};
		let Some(stolen) = prologue::steal_len(&prefix) else {
			return abi::result::E_UNSUPPORTED;
		};
		if !image::fits_page(target, stolen, self.page_size)
			|| !query::executable((target + stolen - 1) as *const c_void)
		{
			return abi::result::E_UNSUPPORTED;
		}
		if self.overlaps_hook(target, stolen) || self.overlaps_patch(target, stolen) {
			return abi::result::E_CONFLICT;
		}
		let original = prefix[..stolen].to_vec();
		self.sites.push(Site {
			target,
			original,
			installed: false,
			trampoline: 0,
			head: 0,
			hooks: Vec::new(),
		});
		let index = self.sites.len() - 1;
		self.push_hook(index, detour as usize, priority, seq, next, hook)
	}

	/// Enable or disable a hook created by [`Self::hook_create`].
	///
	/// # Safety
	/// `hook` must be a handle returned by this linker (or null/invalid, which
	/// returns `E_INVALID`), and initial site installation must occur while no
	/// other thread is executing the stolen prologue window.
	pub unsafe fn hook_set_enabled(&mut self, hook: *mut abi::FochHook, enabled: i32) -> i32 {
		let Some(node) = hook_ptr(hook) else {
			return abi::result::E_INVALID;
		};
		let turn_on = enabled != 0;
		unsafe { (*node).enabled = turn_on };
		let index = unsafe { (*node).site };
		let site = &mut self.sites[index];
		if turn_on && !site.installed {
			if !self.init_open {
				unsafe { (*node).enabled = false };
				return abi::result::E_UNSUPPORTED;
			}
			let code = install(site);
			if code != abi::result::OK {
				unsafe { (*node).enabled = false };
				return code;
			}
			return abi::result::OK;
		}
		relink(site);
		abi::result::OK
	}

	/// Register a byte patch at `address`.
	///
	/// # Safety
	/// `address` and `bytes` must remain valid readable ranges of `len` bytes
	/// during creation, and `patch` must point to a writable output slot.
	pub unsafe fn patch_create(
		&mut self,
		address: *mut c_void,
		bytes: *const u8,
		len: usize,
		patch: *mut *mut abi::FochPatch,
	) -> i32 {
		if !patch.is_null() {
			unsafe { *patch = std::ptr::null_mut() };
		}
		if patch.is_null() || address.is_null() || bytes.is_null() || len == 0 {
			return abi::result::E_INVALID;
		}
		if !image::fits_page(address as usize, len, self.page_size) {
			return abi::result::E_UNSUPPORTED;
		}
		if !query::readable(bytes, len) || !query::readable(address as *const u8, len) {
			return abi::result::E_INVALID;
		}
		let address = address as usize;
		if self.overlaps_hook(address, len) || self.overlaps_patch(address, len) {
			return abi::result::E_CONFLICT;
		}
		let mut original = vec![0u8; len];
		let mut owned = vec![0u8; len];
		unsafe {
			std::ptr::copy_nonoverlapping(address as *const u8, original.as_mut_ptr(), len);
			std::ptr::copy_nonoverlapping(bytes, owned.as_mut_ptr(), len);
		}
		let node = Box::leak(Box::new(PatchNode {
			magic: PATCH_MAGIC,
			address,
			original,
			bytes: owned,
			enabled: false,
		}));
		self.patches.push(node as *mut PatchNode as usize);
		unsafe { *patch = (node as *mut PatchNode).cast() };
		abi::result::OK
	}

	/// Apply or revert a byte patch created by [`Self::patch_create`].
	///
	/// # Safety
	/// `patch` must be a handle returned by this linker, and no concurrent
	/// thread may execute or race against the patched bytes while toggling.
	pub unsafe fn patch_set_enabled(&mut self, patch: *mut abi::FochPatch, enabled: i32) -> i32 {
		let Some(node) = patch_ptr(patch) else {
			return abi::result::E_INVALID;
		};
		let turn_on = enabled != 0;
		let node = unsafe { &mut *node };
		if node.enabled == turn_on {
			return abi::result::OK;
		}
		if turn_on {
			let Some(current) = read_bytes(node.address, node.original.len()) else {
				return abi::result::E_INVALID;
			};
			if current != node.original {
				return abi::result::E_CONFLICT;
			}
			let code = write_owned(node.address, &node.bytes);
			if code == abi::result::OK {
				node.enabled = true;
			}
			return code;
		}
		let code = write_owned(node.address, &node.original);
		if code == abi::result::OK {
			node.enabled = false;
		}
		code
	}

	fn push_hook(
		&mut self,
		index: usize,
		detour: usize,
		priority: i32,
		seq: u64,
		next: *mut *const *const c_void,
		hook: *mut *mut abi::FochHook,
	) -> i32 {
		let cell = allocate(8, PAGE_READWRITE);
		if cell.is_null() {
			if self.sites[index].hooks.is_empty() && !self.sites[index].installed {
				self.sites.pop();
			}
			return abi::result::E_MEMORY;
		}
		let node = Box::leak(Box::new(HookNode {
			magic: HOOK_MAGIC,
			priority,
			seq,
			enabled: false,
			detour,
			next: cell as usize,
			site: index,
		}));
		self.seq = seq;
		self.sites[index].hooks.push(node as *mut HookNode as usize);
		unsafe {
			*next = cell.cast();
			*hook = (node as *mut HookNode).cast();
		}
		abi::result::OK
	}

	fn overlaps_hook(&self, address: usize, len: usize) -> bool {
		self.sites
			.iter()
			.any(|site| ranges_overlap(site.target, site.original.len(), address, len))
	}

	fn overlaps_patch(&self, address: usize, len: usize) -> bool {
		self.patches.iter().any(|&patch| {
			let patch = unsafe { &*(patch as *const PatchNode) };
			ranges_overlap(patch.address, patch.original.len(), address, len)
		})
	}
}

/// Search executable sections of the mapped PE image at `image_base` for a
/// unique pattern match.
///
/// # Safety
/// `image_base` must point to a mapped module image of at least `exe_size`
/// bytes (or null, which returns `E_INVALID`), and `address` must point to a
/// writable output slot.
pub unsafe fn find_in_module(
	image_base: *const u8,
	exe_size: usize,
	text: &str,
	address: *mut *mut c_void,
) -> i32 {
	if address.is_null() {
		return abi::result::E_INVALID;
	}
	unsafe { *address = std::ptr::null_mut() };
	let Some(pattern) = Pattern::parse(text) else {
		return abi::result::E_INVALID;
	};
	if image_base.is_null() || exe_size == 0 {
		return abi::result::E_INVALID;
	}
	let base = image_base;
	let Some(headers) = image_header(base, exe_size) else {
		return abi::result::E_INVALID;
	};
	let Ok(ranges) = image::executable_ranges(&headers) else {
		return abi::result::E_INVALID;
	};
	let mut pieces = Vec::new();
	for (rva, len) in ranges {
		pieces.extend(committed_pieces(base, rva, len, exe_size));
	}
	let mut found = None;
	for (off, len) in merge_adjacent(pieces) {
		let slice = unsafe { std::slice::from_raw_parts(base.add(off), len) };
		match pattern::find_unique(&pattern, slice) {
			Found::None => {}
			Found::Ambiguous => return abi::result::E_AMBIGUOUS,
			Found::Unique(at) => {
				if found.is_some() {
					return abi::result::E_AMBIGUOUS;
				}
				found = Some(off + at);
			}
		}
	}
	match found {
		Some(off) => {
			unsafe { *address = base.add(off) as *mut c_void };
			abi::result::OK
		}
		None => abi::result::E_NOT_FOUND,
	}
}

fn install(site: &mut Site) -> i32 {
	let Some(current) = read_bytes(site.target, site.original.len()) else {
		return abi::result::E_INVALID;
	};
	if current != site.original {
		return abi::result::E_CONFLICT;
	}
	let stolen = site.original.len();
	// A trampoline holding RIP-relative operands must sit inside the
	// intersection of all operands' disp32 reach so every copied disp32 can
	// be rewritten to its exact original target; windows without them keep
	// the original placement.
	let trampoline = match rip_trampoline_range(&site.original, site.target) {
		Some(Some((low, high))) => {
			allocate_in_range(low, high, stolen + JUMP_BYTES, PAGE_EXECUTE_READWRITE)
		}
		Some(None) => allocate(stolen + JUMP_BYTES, PAGE_EXECUTE_READWRITE),
		None => return abi::result::E_MEMORY,
	};
	let stub = allocate(16, PAGE_EXECUTE_READWRITE);
	if trampoline.is_null() || stub.is_null() {
		if !trampoline.is_null() {
			unsafe { VirtualFree(trampoline.cast(), 0, MEM_RELEASE) };
		}
		if !stub.is_null() {
			unsafe { VirtualFree(stub.cast(), 0, MEM_RELEASE) };
		}
		return abi::result::E_MEMORY;
	}
	let mut body = site.original.clone();
	if !relocate_rip_operands(&mut body, site.target, trampoline as usize) {
		unsafe {
			VirtualFree(trampoline.cast(), 0, MEM_RELEASE);
			VirtualFree(stub.cast(), 0, MEM_RELEASE);
		}
		return abi::result::E_MEMORY;
	}
	body.extend_from_slice(&absolute_jump(site.target + stolen));
	unsafe { std::ptr::copy_nonoverlapping(body.as_ptr(), trampoline, body.len()) };
	let mut stub_bytes = [0u8; 16];
	// jmp qword ptr [rip+2] lands on the aligned head cell at stub+8.
	stub_bytes[..6].copy_from_slice(&[0xFF, 0x25, 0x02, 0x00, 0x00, 0x00]);
	unsafe { std::ptr::copy_nonoverlapping(stub_bytes.as_ptr(), stub, stub_bytes.len()) };
	let head = stub as usize + 8;
	store(head, trampoline as usize);
	if !flush(trampoline.cast(), body.len()) || !flush(stub.cast(), stub_bytes.len()) {
		return abi::result::E_MEMORY;
	}
	site.trampoline = trampoline as usize;
	site.head = head;
	site.installed = true;
	relink(site);
	let mut redirect = vec![0x90; stolen];
	redirect[..JUMP_BYTES].copy_from_slice(&absolute_jump(stub as usize));
	match write_region(site.target as *mut u8, &redirect) {
		Ok(()) => abi::result::OK,
		Err(WriteFault::Intact) => {
			site.installed = false;
			abi::result::E_MEMORY
		}
		Err(WriteFault::Unrecoverable) => unsafe { ExitProcess(126) },
	}
}

fn relink(site: &Site) {
	if !site.installed || site.head == 0 {
		return;
	}
	let mut hooks = site.hooks.clone();
	hooks.sort_by(|&left, &right| {
		let left = unsafe { &*(left as *const HookNode) };
		let right = unsafe { &*(right as *const HookNode) };
		right
			.priority
			.cmp(&left.priority)
			.then(left.seq.cmp(&right.seq))
	});
	let mut successor = vec![site.trampoline; hooks.len()];
	let mut next = site.trampoline;
	for (index, &hook) in hooks.iter().enumerate().rev() {
		successor[index] = next;
		let node = unsafe { &*(hook as *const HookNode) };
		if node.enabled {
			next = node.detour;
		}
	}
	for (&hook, next) in hooks.iter().zip(successor) {
		let cell = unsafe { (*(hook as *const HookNode)).next };
		store(cell, next);
	}
	store(site.head, next);
}

fn store(cell: usize, value: usize) {
	unsafe {
		AtomicPtr::from_ptr(cell as *mut *mut c_void)
			.store(value as *mut c_void, Ordering::Release);
	}
}

fn hook_ptr(hook: *mut abi::FochHook) -> Option<*mut HookNode> {
	let node = aligned::<HookNode>(hook.cast())?;
	unsafe { ((*node).magic == HOOK_MAGIC).then_some(node) }
}

fn patch_ptr(patch: *mut abi::FochPatch) -> Option<*mut PatchNode> {
	let node = aligned::<PatchNode>(patch.cast())?;
	unsafe { ((*node).magic == PATCH_MAGIC).then_some(node) }
}

fn aligned<T>(ptr: *mut T) -> Option<*mut T> {
	if ptr.is_null() || !(ptr as usize).is_multiple_of(align_of::<T>()) {
		return None;
	}
	if !query::readable(ptr.cast(), size_of::<T>()) {
		return None;
	}
	Some(ptr)
}

fn clear_hook(next: *mut *const *const c_void, hook: *mut *mut abi::FochHook) {
	if !next.is_null() {
		unsafe { *next = std::ptr::null() };
	}
	if !hook.is_null() {
		unsafe { *hook = std::ptr::null_mut() };
	}
}

fn ranges_overlap(left: usize, left_len: usize, right: usize, right_len: usize) -> bool {
	left_len > 0
		&& right_len > 0
		&& left < right.saturating_add(right_len)
		&& right < left.saturating_add(left_len)
}

// Never require the maximum window to be readable: a valid 14-byte entry
// at a page end remains usable even when the next page is inaccessible.
fn hook_prefix(address: usize, page_size: usize) -> Option<Vec<u8>> {
	let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
	if unsafe {
		VirtualQuery(
			address as *const c_void,
			&mut info,
			size_of::<MEMORY_BASIC_INFORMATION>(),
		)
	} == 0
	{
		return None;
	}
	let region_end = (info.BaseAddress as usize).checked_add(info.RegionSize)?;
	let available = region_end
		.checked_sub(address)?
		.min(page_size - address % page_size)
		.min(prologue::MAX_STEAL);
	read_bytes(address, available)
}

fn read_bytes(address: usize, len: usize) -> Option<Vec<u8>> {
	let ptr = address as *const u8;
	if !query::readable(ptr, len) {
		return None;
	}
	let mut bytes = vec![0u8; len];
	unsafe { std::ptr::copy_nonoverlapping(ptr, bytes.as_mut_ptr(), len) };
	Some(bytes)
}

fn absolute_jump(dest: usize) -> [u8; JUMP_BYTES] {
	let mut bytes = [0u8; JUMP_BYTES];
	bytes[..6].copy_from_slice(&[0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]);
	bytes[6..].copy_from_slice(&dest.to_le_bytes());
	bytes
}

fn allocate(len: usize, protection: u32) -> *mut u8 {
	unsafe {
		VirtualAlloc(
			std::ptr::null_mut(),
			len,
			MEM_COMMIT | MEM_RESERVE,
			protection,
		)
		.cast()
	}
}

/// Inclusive trampoline address range `(low, high)` whose disp32 reaches every
/// RIP-relative target in `body` copied from `source`.
///
/// Returns `Some(None)` when `body` has no RIP-relative operands,
/// `Some(Some((low, high)))` when at least one valid trampoline address exists,
/// and `None` if decoding fails or the operands' reachable windows do not overlap.
fn rip_trampoline_range(body: &[u8], source: usize) -> Option<Option<(usize, usize)>> {
	let operands = prologue::rip_operands(body, body.len())?;
	if operands.is_empty() {
		return Some(None);
	}
	let mut min_dest: i128 = 0x1_0000;
	let mut max_dest: i128 = usize::MAX as i128;
	for operand in operands {
		let bytes: [u8; 4] = body
			.get(operand.disp_at..operand.disp_at + 4)?
			.try_into()
			.ok()?;
		let old = i32::from_le_bytes(bytes) as i128;
		let target = (source as i128) + (operand.next as i128) + old;
		let op_min = target - (operand.next as i128) - (i32::MAX as i128);
		let op_max = target - (operand.next as i128) - (i32::MIN as i128);
		min_dest = min_dest.max(op_min);
		max_dest = max_dest.min(op_max);
	}
	if min_dest > max_dest {
		return None;
	}
	Some(Some((min_dest as usize, max_dest as usize)))
}

/// First committed free region whose base lies in `[low, high]`. Returns null
/// when no granularity-aligned free space fits inside the reachable window.
fn allocate_in_range(low: usize, high: usize, len: usize, protection: u32) -> *mut u8 {
	let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
	let mut system: SYSTEM_INFO = unsafe { std::mem::zeroed() };
	unsafe { GetSystemInfo(&mut system) };
	let granularity = if system.dwAllocationGranularity == 0 {
		0x1000
	} else {
		system.dwAllocationGranularity as usize
	};
	let min_app = (system.lpMinimumApplicationAddress as usize).max(0x1_0000);
	let max_app = system.lpMaximumApplicationAddress as usize;
	let low = low.max(min_app);
	let high = if max_app > 0 { high.min(max_app) } else { high };
	if low > high {
		return std::ptr::null_mut();
	}
	let mut cursor = low;
	while cursor <= high {
		let filled = unsafe {
			VirtualQuery(
				cursor as *const c_void,
				&mut info,
				size_of::<MEMORY_BASIC_INFORMATION>(),
			)
		};
		if filled == 0 {
			break;
		}
		let base = info.BaseAddress as usize;
		let next = base.saturating_add(info.RegionSize);
		if info.State == MEM_FREE
			&& let Some(start) = base.max(low).div_ceil(granularity).checked_mul(granularity)
			&& start <= high
			&& start.checked_add(len).is_some_and(|end| end <= next)
		{
			let got = unsafe {
				VirtualAlloc(
					start as *const c_void,
					len,
					MEM_COMMIT | MEM_RESERVE,
					protection,
				)
			};
			if !got.is_null() {
				return got.cast();
			}
		}
		if next <= cursor {
			break;
		}
		cursor = next;
	}
	std::ptr::null_mut()
}

/// Rewrite every RIP-relative disp32 in a stolen window so the operands keep
/// their original absolute targets from the trampoline's address. Fails
/// without touching `body` when a decoded displacement falls outside disp32.
fn relocate_rip_operands(body: &mut [u8], source: usize, destination: usize) -> bool {
	let Some(operands) = prologue::rip_operands(body, body.len()) else {
		return false;
	};
	let mut patches = Vec::with_capacity(operands.len());
	for operand in operands {
		let bytes: [u8; 4] = match body.get(operand.disp_at..operand.disp_at + 4) {
			Some(bytes) => bytes.try_into().expect("slice of 4"),
			None => return false,
		};
		let old = i32::from_le_bytes(bytes) as i128;
		let target = (source as i128) + (operand.next as i128) + old;
		let moved = target - ((destination as i128) + (operand.next as i128));
		if moved < i32::MIN as i128 || moved > i32::MAX as i128 {
			return false;
		}
		patches.push((operand.disp_at, moved as i32));
	}
	for (disp_at, moved) in patches {
		body[disp_at..disp_at + 4].copy_from_slice(&moved.to_le_bytes());
	}
	true
}

fn flush(address: *const c_void, len: usize) -> bool {
	unsafe { FlushInstructionCache(GetCurrentProcess(), address, len) != 0 }
}

fn write_owned(address: usize, bytes: &[u8]) -> i32 {
	match write_region(address as *mut u8, bytes) {
		Ok(()) => abi::result::OK,
		Err(WriteFault::Intact) => abi::result::E_MEMORY,
		Err(WriteFault::Unrecoverable) => unsafe { ExitProcess(126) },
	}
}

#[derive(Debug, PartialEq, Eq)]
enum WriteFault {
	Intact,
	Unrecoverable,
}

fn write_region(address: *mut u8, bytes: &[u8]) -> Result<(), WriteFault> {
	let len = bytes.len();
	write_with(
		address,
		bytes,
		&mut |addr, protection, old| unsafe { VirtualProtect(addr, len, protection, old) != 0 },
		&mut |addr, size| flush(addr, size),
	)
}

fn write_with(
	address: *mut u8,
	bytes: &[u8],
	protect: &mut impl FnMut(*mut c_void, u32, *mut u32) -> bool,
	flush: &mut impl FnMut(*const c_void, usize) -> bool,
) -> Result<(), WriteFault> {
	let mut original = vec![0u8; bytes.len()];
	unsafe { std::ptr::copy_nonoverlapping(address, original.as_mut_ptr(), bytes.len()) };
	let mut old = 0;
	if !protect(address.cast(), PAGE_EXECUTE_READWRITE, &mut old) {
		return Err(WriteFault::Intact);
	}
	unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), address, bytes.len()) };
	let flushed = flush(address.cast(), bytes.len());
	let mut discarded = 0;
	let protected = protect(address.cast(), old, &mut discarded);
	if flushed && protected {
		return Ok(());
	}
	if !protect(address.cast(), PAGE_EXECUTE_READWRITE, &mut discarded) {
		return Err(WriteFault::Unrecoverable);
	}
	unsafe { std::ptr::copy_nonoverlapping(original.as_ptr(), address, bytes.len()) };
	let flushed = flush(address.cast(), bytes.len());
	let protected = protect(address.cast(), old, &mut discarded);
	if flushed && protected {
		Err(WriteFault::Intact)
	} else {
		Err(WriteFault::Unrecoverable)
	}
}

fn image_header(base: *const u8, image_size: usize) -> Option<Vec<u8>> {
	if image_size < 64 || !query::readable(base, 64) {
		return None;
	}
	let mut headers = vec![0u8; 64];
	unsafe { std::ptr::copy_nonoverlapping(base, headers.as_mut_ptr(), 64) };
	let nt = u32::from_le_bytes(headers[60..64].try_into().ok()?) as usize;
	if nt > image_size.saturating_sub(24) || nt > 1_048_576 {
		return None;
	}
	let coff_end = nt + 24;
	if !query::readable(base, coff_end) {
		return None;
	}
	headers.resize(coff_end, 0);
	unsafe { std::ptr::copy_nonoverlapping(base, headers.as_mut_ptr(), coff_end) };
	let count = u16::from_le_bytes(headers[nt + 6..nt + 8].try_into().ok()?) as usize;
	let optional = u16::from_le_bytes(headers[nt + 20..nt + 22].try_into().ok()?) as usize;
	let table_end = nt
		.checked_add(24)?
		.checked_add(optional)?
		.checked_add(count.checked_mul(40)?)?;
	if table_end > image_size || table_end > 1_048_576 || !query::readable(base, table_end) {
		return None;
	}
	headers.resize(table_end, 0);
	unsafe { std::ptr::copy_nonoverlapping(base, headers.as_mut_ptr(), table_end) };
	Some(headers)
}

fn committed_pieces(
	base: *const u8,
	rva: usize,
	len: usize,
	image_size: usize,
) -> Vec<(usize, usize)> {
	let Some(end_rva) = rva.checked_add(len) else {
		return Vec::new();
	};
	if end_rva > image_size {
		return Vec::new();
	}
	let start = (base as usize).saturating_add(rva);
	let end = (base as usize).saturating_add(end_rva);
	let mut pieces = Vec::new();
	let mut at = start;
	while at < end {
		let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
		let queried = unsafe {
			VirtualQuery(
				at as *const c_void,
				&mut info,
				size_of::<MEMORY_BASIC_INFORMATION>(),
			)
		};
		if queried == 0 {
			break;
		}
		let Some(region_end) = (info.BaseAddress as usize).checked_add(info.RegionSize) else {
			break;
		};
		if region_end <= at {
			break;
		}
		let chunk_end = end.min(region_end);
		let chunk = chunk_end - at;
		if info.State == MEM_COMMIT && query::readable(at as *const u8, chunk) {
			pieces.push((at - base as usize, chunk));
		}
		at = region_end;
	}
	pieces
}

fn merge_adjacent(mut parts: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
	parts.sort_by_key(|part| part.0);
	let mut merged: Vec<(usize, usize)> = Vec::new();
	for (start, len) in parts {
		if let Some(last) = merged.last_mut()
			&& last.0 + last.1 == start
		{
			last.1 += len;
		} else {
			merged.push((start, len));
		}
	}
	merged
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{Mutex, atomic::AtomicU64};

	static ORDER: Mutex<Vec<u8>> = Mutex::new(Vec::new());
	static NEXT_HI: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
	static NEXT_LO: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
	static NEXT_EARLY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
	static NEXT_LATE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
	static C1: AtomicU64 = AtomicU64::new(0);
	static C2: AtomicU64 = AtomicU64::new(0);

	unsafe extern "C" fn detour_hi() {
		ORDER.lock().unwrap().push(b'H');
		unsafe { call_next(NEXT_HI.load(Ordering::Acquire)) };
	}
	unsafe extern "C" fn detour_lo() {
		ORDER.lock().unwrap().push(b'L');
		unsafe { call_next(NEXT_LO.load(Ordering::Acquire)) };
	}
	unsafe extern "C" fn detour_early() {
		ORDER.lock().unwrap().push(b'E');
		unsafe { call_next(NEXT_EARLY.load(Ordering::Acquire)) };
	}
	unsafe extern "C" fn detour_late() {
		ORDER.lock().unwrap().push(b'T');
		unsafe { call_next(NEXT_LATE.load(Ordering::Acquire)) };
	}

	unsafe fn call_next(cell: usize) {
		let next = unsafe { *(cell as *const *const c_void) };
		let function: unsafe extern "C" fn() = unsafe { std::mem::transmute(next) };
		unsafe { function() };
	}

	fn code(len: usize) -> *mut u8 {
		let ptr = allocate(len, PAGE_EXECUTE_READWRITE);
		assert!(!ptr.is_null());
		ptr
	}

	fn write_pair(mem: *mut u8, first: *const AtomicU64, second: *const AtomicU64) {
		write_inc(mem, 0, first);
		write_inc(mem, JUMP_BYTES, second);
		unsafe { mem.add(JUMP_BYTES * 2).write(0xC3) };
	}

	fn write_inc(mem: *mut u8, at: usize, counter: *const AtomicU64) {
		let bytes = unsafe { std::slice::from_raw_parts_mut(mem.add(at), JUMP_BYTES) };
		bytes[0] = 0x48;
		bytes[1] = 0xB8;
		bytes[2..10].copy_from_slice(&(counter as usize).to_le_bytes());
		bytes[10..14].copy_from_slice(&[0xF0, 0x48, 0xFF, 0x00]);
	}

	fn call(target: *mut u8) {
		let function: unsafe extern "C" fn() = unsafe { std::mem::transmute(target) };
		unsafe { function() };
	}

	fn remember(slot: &std::sync::atomic::AtomicUsize, next: *const *const c_void) {
		slot.store(next as usize, Ordering::Release);
	}

	struct TestLinker(Linker);

	impl std::ops::Deref for TestLinker {
		type Target = Linker;
		fn deref(&self) -> &Self::Target {
			&self.0
		}
	}

	impl std::ops::DerefMut for TestLinker {
		fn deref_mut(&mut self) -> &mut Self::Target {
			&mut self.0
		}
	}

	impl TestLinker {
		fn new() -> Self {
			Self(Linker::new())
		}

		fn hook_create(
			&mut self,
			target: *mut c_void,
			detour: *mut c_void,
			priority: i32,
			next: *mut *const *const c_void,
			hook: *mut *mut abi::FochHook,
		) -> i32 {
			unsafe { self.0.hook_create(target, detour, priority, next, hook) }
		}

		fn hook_set_enabled(&mut self, hook: *mut abi::FochHook, enabled: i32) -> i32 {
			unsafe { self.0.hook_set_enabled(hook, enabled) }
		}

		fn patch_create(
			&mut self,
			address: *mut c_void,
			bytes: *const u8,
			len: usize,
			patch: *mut *mut abi::FochPatch,
		) -> i32 {
			unsafe { self.0.patch_create(address, bytes, len, patch) }
		}

		fn patch_set_enabled(&mut self, patch: *mut abi::FochPatch, enabled: i32) -> i32 {
			unsafe { self.0.patch_set_enabled(patch, enabled) }
		}
	}

	#[test]
	fn hooks_chain_by_priority_then_registration_and_disable_skips() {
		ORDER.lock().unwrap().clear();
		C1.store(0, Ordering::Release);
		C2.store(0, Ordering::Release);
		let target = code(64);
		write_pair(target, &C1, &C2);
		assert_eq!(
			prologue::steal_len(unsafe { std::slice::from_raw_parts(target, JUMP_BYTES) }),
			Some(JUMP_BYTES)
		);
		let mut linker = TestLinker::new();
		linker.open_init(true);
		let mut next_lo = std::ptr::null();
		let mut hook_lo = std::ptr::null_mut();
		assert_eq!(
			linker.hook_create(
				target.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next_lo,
				&mut hook_lo
			),
			abi::result::OK
		);
		let mut next_hi = std::ptr::null();
		let mut hook_hi = std::ptr::null_mut();
		assert_eq!(
			linker.hook_create(
				target.cast(),
				detour_hi as *mut c_void,
				10,
				&mut next_hi,
				&mut hook_hi
			),
			abi::result::OK
		);
		assert_eq!(unsafe { *target }, 0x48, "create does not patch");
		remember(&NEXT_LO, next_lo);
		remember(&NEXT_HI, next_hi);
		assert_eq!(linker.hook_set_enabled(hook_lo, 1), abi::result::OK);
		assert_eq!(linker.hook_set_enabled(hook_hi, 1), abi::result::OK);
		assert_eq!(unsafe { *target }, 0xFF);
		call(target);
		assert_eq!(ORDER.lock().unwrap().as_slice(), b"HL");
		assert_eq!(C1.load(Ordering::Acquire), 1);
		assert_eq!(C2.load(Ordering::Acquire), 1);

		ORDER.lock().unwrap().clear();
		assert_eq!(linker.hook_set_enabled(hook_hi, 0), abi::result::OK);
		assert_eq!(unsafe { *target }, 0xFF, "disable leaves the jump");
		call(target);
		assert_eq!(ORDER.lock().unwrap().as_slice(), b"L");
		assert_eq!(C1.load(Ordering::Acquire), 2);

		linker.open_init(false);
		ORDER.lock().unwrap().clear();
		assert_eq!(linker.hook_set_enabled(hook_lo, 0), abi::result::OK);
		assert_eq!(linker.hook_set_enabled(hook_lo, 1), abi::result::OK);
		call(target);
		assert_eq!(ORDER.lock().unwrap().as_slice(), b"L");

		let other = code(64);
		write_pair(other, &C1, &C2);
		let before = unsafe { *other };
		let mut next = std::ptr::null();
		let mut hook = std::ptr::null_mut();
		assert_eq!(
			linker.hook_create(
				other.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut hook
			),
			abi::result::OK
		);
		assert_eq!(linker.hook_set_enabled(hook, 1), abi::result::E_UNSUPPORTED);
		assert_eq!(unsafe { *other }, before);

		let tied = code(64);
		write_pair(tied, &C1, &C2);
		linker.open_init(true);
		let mut next_early = std::ptr::null();
		let mut hook_early = std::ptr::null_mut();
		let mut next_late = std::ptr::null();
		let mut hook_late = std::ptr::null_mut();
		assert_eq!(
			linker.hook_create(
				tied.cast(),
				detour_early as *mut c_void,
				0,
				&mut next_early,
				&mut hook_early
			),
			abi::result::OK
		);
		assert_eq!(
			linker.hook_create(
				tied.cast(),
				detour_late as *mut c_void,
				0,
				&mut next_late,
				&mut hook_late
			),
			abi::result::OK
		);
		remember(&NEXT_EARLY, next_early);
		remember(&NEXT_LATE, next_late);
		assert_eq!(linker.hook_set_enabled(hook_early, 1), abi::result::OK);
		assert_eq!(linker.hook_set_enabled(hook_late, 1), abi::result::OK);
		ORDER.lock().unwrap().clear();
		call(tied);
		assert_eq!(ORDER.lock().unwrap().as_slice(), b"ET");
	}

	#[test]
	fn unrelocatable_and_foreign_prologues_are_refused() {
		let mut linker = TestLinker::new();
		linker.open_init(true);
		let short = code(32);
		unsafe { short.write(0xC3) };
		let mut next = std::ptr::null();
		let mut hook = std::ptr::null_mut();
		assert_eq!(
			linker.hook_create(
				short.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut hook
			),
			abi::result::E_UNSUPPORTED
		);
		assert!(hook.is_null());

		// A RIP-relative load is no longer an unsupported prologue: the hook
		// installs and the trampoline rewrites the disp32 to the same target.
		{
			let mut linker = TestLinker::new();
			linker.open_init(true);
			let rip = code(32);
			// mov rax,[rip+0x10]; nop*9 — the load reads the qword that
			// follows the stolen window at rip+7+0x10 = rip+0x17.
			let mut bytes = [0x90u8; 14];
			bytes[..7].copy_from_slice(&[0x48, 0x8B, 0x05, 0x10, 0x00, 0x00, 0x00]);
			unsafe {
				std::ptr::copy_nonoverlapping(bytes.as_ptr(), rip, bytes.len());
			}
			let mut cell: *mut abi::FochHook = std::ptr::null_mut();
			let mut next: *const *const c_void = std::ptr::null();
			assert_eq!(
				linker.hook_create(
					rip.cast(),
					detour_lo as *mut c_void,
					0,
					&mut next,
					&mut cell
				),
				abi::result::OK
			);
			assert!(!next.is_null());
			assert_eq!(linker.hook_set_enabled(cell, 1), abi::result::OK);
			let site = &linker.sites[0];
			let moved: [u8; 4] = unsafe {
				std::slice::from_raw_parts(site.trampoline as *const u8, 7)[3..7]
					.try_into()
					.unwrap()
			};
			let expected = (rip as usize + 0x17).wrapping_sub(site.trampoline + 7) as i32;
			assert_eq!(i32::from_le_bytes(moved), expected);
		}

		let page = linker.page_size;
		let crossed = allocate(page * 2, PAGE_EXECUTE_READWRITE);
		assert!(!crossed.is_null());
		let target = unsafe { crossed.add(page - 7) };
		unsafe { std::ptr::write_bytes(target, 0x90, JUMP_BYTES) };
		assert_eq!(
			linker.hook_create(
				target.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut hook
			),
			abi::result::E_UNSUPPORTED
		);
		assert_eq!(unsafe { *target }, 0x90);

		let foreign = code(64);
		write_pair(foreign, &C1, &C2);
		assert_eq!(
			linker.hook_create(
				foreign.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut hook
			),
			abi::result::OK
		);
		unsafe { foreign.write(0xCC) };
		assert_eq!(linker.hook_set_enabled(hook, 1), abi::result::E_CONFLICT);
		assert_eq!(unsafe { *foreign }, 0xCC);
	}

	#[test]
	fn patches_restore_bytes_and_refuse_hook_overlap() {
		let mut linker = TestLinker::new();
		linker.open_init(true);
		let mem = code(64);
		unsafe { std::ptr::write_bytes(mem, 0x90, 64) };
		write_pair(mem, &C1, &C2);
		let mut next = std::ptr::null();
		let mut hook = std::ptr::null_mut();
		assert_eq!(
			linker.hook_create(
				mem.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut hook
			),
			abi::result::OK
		);
		let mut patch = std::ptr::null_mut();
		let bytes = [0xCC, 0xCC];
		assert_eq!(
			linker.patch_create(mem.cast(), bytes.as_ptr(), bytes.len(), &mut patch),
			abi::result::E_CONFLICT
		);
		assert_eq!(
			linker.patch_create(
				unsafe { mem.add(JUMP_BYTES) }.cast(),
				bytes.as_ptr(),
				bytes.len(),
				&mut patch
			),
			abi::result::OK
		);
		assert_eq!(unsafe { *mem.add(JUMP_BYTES) }, 0x48);
		assert_eq!(linker.patch_set_enabled(patch, 1), abi::result::OK);
		assert_eq!(unsafe { *mem.add(JUMP_BYTES) }, 0xCC);
		assert_eq!(linker.patch_set_enabled(patch, 0), abi::result::OK);
		assert_eq!(unsafe { *mem.add(JUMP_BYTES) }, 0x48);

		let mut again = std::ptr::null_mut();
		assert_eq!(
			linker.patch_create(
				unsafe { mem.add(JUMP_BYTES) }.cast(),
				bytes.as_ptr(),
				1,
				&mut again
			),
			abi::result::E_CONFLICT
		);
		unsafe { mem.add(32).write(0x11) };
		let mut shifted = std::ptr::null_mut();
		assert_eq!(
			linker.patch_create(
				unsafe { mem.add(32) }.cast(),
				bytes.as_ptr(),
				1,
				&mut shifted
			),
			abi::result::OK
		);
		unsafe { mem.add(32).write(0x22) };
		assert_eq!(
			linker.patch_set_enabled(shifted, 1),
			abi::result::E_CONFLICT
		);
		assert_eq!(unsafe { *mem.add(32) }, 0x22);
		assert_eq!(
			linker.hook_set_enabled(shifted.cast(), 1),
			abi::result::E_INVALID
		);
	}

	#[test]
	fn partial_writes_restore_bytes_and_protection() {
		for (fail_protect, fail_flush) in [(0, 1), (2, 0), (1, 0)] {
			let original = [0x41u8; 17];
			let mut memory = original;
			let mut protections = Vec::new();
			let mut flushes = 0;
			let result = write_with(
				memory.as_mut_ptr(),
				&[0xCC; 17],
				&mut |_, protection, old| {
					protections.push(protection);
					unsafe { old.write(0x20) };
					protections.len() != fail_protect
				},
				&mut |_, len| {
					assert_eq!(len, 17);
					flushes += 1;
					flushes != fail_flush
				},
			);
			assert_eq!(result, Err(WriteFault::Intact));
			assert_eq!(memory, original);
			if fail_protect != 1 {
				assert_eq!(flushes, 2);
				assert_eq!(protections.last(), Some(&0x20));
			}
		}
	}

	#[test]
	fn variable_windows_copy_complete_instructions_and_keep_chain_order() {
		use std::sync::atomic::AtomicUsize;
		static HI: AtomicUsize = AtomicUsize::new(0);
		static LO: AtomicUsize = AtomicUsize::new(0);
		static SEEN: Mutex<Vec<u8>> = Mutex::new(Vec::new());
		unsafe fn next(cell: usize, a: u64, b: u64) -> u64 {
			let ptr = unsafe { *(cell as *const *const c_void) };
			let f: unsafe extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(ptr) };
			unsafe { f(a, b) }
		}
		unsafe extern "C" fn hi(a: u64, b: u64) -> u64 {
			SEEN.lock().unwrap().push(b'H');
			unsafe { next(HI.load(Ordering::Acquire), a, b) + 100 }
		}
		unsafe extern "C" fn lo(a: u64, b: u64) -> u64 {
			SEEN.lock().unwrap().push(b'L');
			unsafe { next(LO.load(Ordering::Acquire), a, b) + 10 }
		}
		for len in [14, 15, 17, 18, 28] {
			let mut original = match len {
				14 | 15 => {
					let mut bytes = vec![0x90; len - 3];
					bytes.extend([0x48, 0x89, 0xC8]); // mov rax,rcx
					bytes
				}
				17 => {
					// The caller supplies the normal Windows x64 home area.
					let mut bytes = vec![0x48, 0x89, 0x4C, 0x24, 0x08];
					bytes.extend([0x90; 7]);
					bytes.extend([0x48, 0x8B, 0x44, 0x24, 0x08]);
					bytes
				}
				18 => {
					let mut bytes = vec![0x90; 13];
					bytes.extend([0x48, 0x8D, 0x44, 0x21, 0]); // lea rax,[rcx]
					bytes
				}
				28 => {
					let mut bytes = vec![0x90; 13];
					bytes.extend([0x2E; 5]);
					bytes.extend([0x48, 0xB8, 7, 0, 0, 0, 0, 0, 0, 0]);
					bytes
				}
				_ => unreachable!(),
			};
			assert_eq!(prologue::steal_len(&original), Some(len));
			original.extend([0x48, 0x03, 0xC2, 0xC3]); // add rax,rdx; ret
			let target = code(64);
			unsafe { std::ptr::copy_nonoverlapping(original.as_ptr(), target, original.len()) };
			let f: unsafe extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(target) };
			assert_eq!(unsafe { f(7, 11) }, 18);
			let mut linker = TestLinker::new();
			linker.open_init(true);
			let mut low = std::ptr::null_mut();
			let mut high = std::ptr::null_mut();
			let mut cell = std::ptr::null();
			assert_eq!(
				linker.hook_create(target.cast(), lo as *mut c_void, 0, &mut cell, &mut low),
				abi::result::OK
			);
			LO.store(cell as usize, Ordering::Release);
			assert_eq!(linker.hook_set_enabled(low, 1), abi::result::OK);
			assert_eq!(
				linker.hook_create(target.cast(), hi as *mut c_void, 10, &mut cell, &mut high),
				abi::result::OK
			);
			HI.store(cell as usize, Ordering::Release);
			assert_eq!(linker.hook_set_enabled(high, 1), abi::result::OK);
			let site = &linker.sites[0];
			assert_eq!(site.original, original[..len]);
			assert_eq!(
				unsafe { std::slice::from_raw_parts(site.trampoline as *const u8, len) },
				&original[..len]
			);
			assert_eq!(
				read_bytes(site.trampoline + len, JUMP_BYTES).unwrap(),
				absolute_jump(target as usize + len)
			);
			assert_eq!(
				read_bytes(target as usize + JUMP_BYTES, len - JUMP_BYTES).unwrap(),
				vec![0x90; len - JUMP_BYTES]
			);
			SEEN.lock().unwrap().clear();
			assert_eq!(unsafe { f(7, 11) }, 128);
			assert_eq!(*SEEN.lock().unwrap(), b"HL");
			assert_eq!(linker.hook_set_enabled(high, 0), abi::result::OK);
			SEEN.lock().unwrap().clear();
			assert_eq!(unsafe { f(7, 11) }, 28);
			assert_eq!(*SEEN.lock().unwrap(), b"L");
			assert_eq!(linker.hook_set_enabled(low, 0), abi::result::OK);
			assert_eq!(unsafe { f(7, 11) }, 18);
			assert_eq!(unsafe { *target }, 0xFF);
		}
	}

	#[test]
	fn the_whole_stolen_span_is_reserved_and_checked_for_drift() {
		let make_target = || {
			let target = code(64);
			unsafe {
				std::ptr::write_bytes(target, 0x90, 64);
				std::ptr::copy_nonoverlapping(
					[0x48, 0xB8, 1, 0, 0, 0, 0, 0, 0, 0].as_ptr(),
					target.add(7),
					10,
				);
			}
			target // first complete boundary at 17
		};
		let target = make_target();
		let mut linker = TestLinker::new();
		linker.open_init(true);
		let mut next = std::ptr::null();
		let mut hook = std::ptr::null_mut();
		let mut patch = std::ptr::null_mut();
		let bytes = [0xCC];
		assert_eq!(
			linker.hook_create(
				target.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut hook
			),
			abi::result::OK
		);
		assert_eq!(
			linker.patch_create(
				unsafe { target.add(16) }.cast(),
				bytes.as_ptr(),
				1,
				&mut patch
			),
			abi::result::E_CONFLICT
		);
		let mut adjacent = std::ptr::null_mut();
		assert_eq!(
			linker.hook_create(
				unsafe { target.add(16) }.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut adjacent
			),
			abi::result::E_CONFLICT
		);
		unsafe { target.add(16).write(2) };
		assert_eq!(linker.hook_set_enabled(hook, 1), abi::result::E_CONFLICT);
		assert_eq!(unsafe { *target.add(16) }, 2);
		let installed = make_target();
		assert_eq!(
			linker.hook_create(
				installed.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut hook
			),
			abi::result::OK
		);
		assert_eq!(linker.hook_set_enabled(hook, 1), abi::result::OK);
		assert_eq!(unsafe { *installed.add(16) }, 0x90);
		assert_eq!(
			linker.patch_create(
				unsafe { installed.add(16) }.cast(),
				bytes.as_ptr(),
				1,
				&mut patch
			),
			abi::result::E_CONFLICT
		);
		assert_eq!(
			linker.hook_create(
				unsafe { installed.add(16) }.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut adjacent
			),
			abi::result::E_CONFLICT
		);
		let target = make_target();
		assert_eq!(
			linker.patch_create(
				unsafe { target.add(16) }.cast(),
				bytes.as_ptr(),
				1,
				&mut patch
			),
			abi::result::OK
		);
		assert_eq!(
			linker.hook_create(
				target.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut hook
			),
			abi::result::E_CONFLICT
		);
	}

	#[test]
	fn page_end_14_bytes_work_and_longer_crossing_spans_are_refused() {
		use windows_sys::Win32::System::Memory::PAGE_NOACCESS;
		let mut linker = TestLinker::new();
		linker.open_init(true);
		let page = linker.page_size;
		let memory = code(page * 2);
		unsafe { std::ptr::write_bytes(memory, 0x90, page * 2) };
		let mut old = 0;
		assert_ne!(
			unsafe { VirtualProtect(memory.add(page).cast(), page, PAGE_NOACCESS, &mut old) },
			0
		);
		let target = unsafe { memory.add(page - 14) };
		let mut next = std::ptr::null();
		let mut hook = std::ptr::null_mut();
		assert_eq!(
			linker.hook_create(
				target.cast(),
				detour_lo as *mut c_void,
				0,
				&mut next,
				&mut hook
			),
			abi::result::OK
		);
		assert_eq!(linker.hook_set_enabled(hook, 1), abi::result::OK);
		for len in [15, 17] {
			let memory = code(page * 2);
			unsafe { std::ptr::write_bytes(memory, 0x90, page * 2) };
			let target = unsafe { memory.add(page - 14) };
			unsafe {
				std::ptr::copy_nonoverlapping(
					[0x48, 0xB8, 1, 0, 0, 0, 0, 0, 0, 0].as_ptr(),
					target.add(len - 10),
					10,
				);
			}
			assert_eq!(
				linker.hook_create(
					target.cast(),
					detour_lo as *mut c_void,
					0,
					&mut next,
					&mut hook
				),
				abi::result::E_UNSUPPORTED
			);
			assert_eq!(unsafe { *target }, 0x90);
		}
	}

	#[test]
	fn pattern_search_reports_not_found_or_a_real_match() {
		let mut address = std::ptr::dangling_mut::<c_void>();
		let base = unsafe {
			windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null())
		} as *const u8;
		assert!(!base.is_null());
		assert_eq!(
			unsafe { find_in_module(base, 0, "90", &mut address) },
			abi::result::E_INVALID
		);
		assert!(address.is_null());
		assert_eq!(
			unsafe { find_in_module(std::ptr::null(), 0x1000, "90", &mut address) },
			abi::result::E_INVALID
		);
		assert!(address.is_null());
		let mut info: windows_sys::Win32::System::ProcessStatus::MODULEINFO =
			unsafe { std::mem::zeroed() };
		assert_ne!(
			unsafe {
				windows_sys::Win32::System::ProcessStatus::K32GetModuleInformation(
					GetCurrentProcess(),
					base.cast_mut().cast(),
					&mut info,
					size_of::<windows_sys::Win32::System::ProcessStatus::MODULEINFO>() as u32,
				)
			},
			0
		);
		assert_eq!(
			unsafe { find_in_module(base, info.SizeOfImage as usize, "?? ??", &mut address) },
			abi::result::E_INVALID
		);
		let code = unsafe {
			find_in_module(
				base,
				info.SizeOfImage as usize,
				"CC CC CC CC CC CC CC CC CC CC",
				&mut address,
			)
		};
		assert!(matches!(
			code,
			abi::result::E_NOT_FOUND | abi::result::E_AMBIGUOUS | abi::result::OK
		));
		if code != abi::result::OK {
			assert!(address.is_null());
		} else {
			assert!(!address.is_null());
		}
	}

	#[test]
	fn decoder_rejections_leave_targets_unchanged() {
		unsafe extern "C" fn detour() -> u64 {
			99
		}
		let mut mixed = vec![0x90; 11];
		mixed.extend([0x66, 0x48, 0xC7, 0xC0, 7, 0, 0, 0, 0xC3]);
		let mut transaction = vec![0x90; 9];
		transaction.extend([0xC7, 0xF8, 0x10, 0, 0, 0]);
		for (index, bytes) in [mixed, transaction].into_iter().enumerate() {
			let target = code(4096);
			assert_eq!(write_owned(target as usize, &bytes), abi::result::OK);
			let original: unsafe extern "C" fn() -> u64 = unsafe { std::mem::transmute(target) };
			if index == 0 {
				assert_eq!(unsafe { original() }, 7);
			}
			let mut linker = TestLinker::new();
			linker.open_init(true);
			let mut next = std::ptr::null();
			let mut hook = std::ptr::null_mut();
			assert_eq!(
				linker.hook_create(
					target.cast(),
					detour as *const () as *mut c_void,
					1,
					&mut next,
					&mut hook
				),
				abi::result::E_UNSUPPORTED
			);
			assert!(next.is_null() && hook.is_null());
			assert!(linker.sites.is_empty());
			assert_eq!(
				unsafe { std::slice::from_raw_parts(target, bytes.len()) },
				bytes.as_slice()
			);
			if index == 0 {
				assert_eq!(unsafe { original() }, 7);
			}
			// XBEGIN is never executed: these tests also work without TSX.
		}
	}

	#[test]
	fn rip_trampoline_range_intersects_operands_and_relocate_is_atomic() {
		let source: usize = 0x1_40a8_9360;
		// Two RIP-relative loads in one 14-byte window:
		//   mov rax, [rip + 0x018b_6b18]  (7 bytes, target = source + 7 + 0x018b_6b18)
		//   mov rcx, [rip - 0x0040_0000]  (7 bytes, target = source + 14 - 0x0040_0000)
		let mut window = [0u8; 14];
		window[..3].copy_from_slice(&[0x48, 0x8B, 0x05]);
		window[3..7].copy_from_slice(&(0x018b_6b18_i32).to_le_bytes());
		window[7..10].copy_from_slice(&[0x48, 0x8B, 0x0D]);
		window[10..14].copy_from_slice(&(-0x0040_0000_i32).to_le_bytes());

		let (low, high) = rip_trampoline_range(&window, source)
			.expect("valid decode")
			.expect("non-empty RIP range");

		// A trampoline at `source - 0x7F00_0000` is out of disp32 reach of the
		// +24 MB global target and must lie strictly below `low`.
		assert!(source - 0x7F00_0000 < low);

		// Both exact endpoints succeed and preserve each absolute target.
		for dest in [low, high] {
			let mut body = window.to_vec();
			assert!(relocate_rip_operands(&mut body, source, dest));
			let d0 = i32::from_le_bytes(body[3..7].try_into().unwrap()) as i128;
			let d1 = i32::from_le_bytes(body[10..14].try_into().unwrap()) as i128;
			assert_eq!(dest as i128 + 7 + d0, source as i128 + 7 + 0x018b_6b18);
			assert_eq!(dest as i128 + 14 + d1, source as i128 + 14 - 0x0040_0000);
		}

		// Just outside either endpoint fails without mutating any byte of `body`.
		for bad_dest in [low - 1, high + 1] {
			let mut body = window.to_vec();
			assert!(!relocate_rip_operands(&mut body, source, bad_dest));
			assert_eq!(body, window);
		}

		// Window without RIP-relative operands returns Some(None).
		let plain = [0x90u8; 14];
		assert_eq!(rip_trampoline_range(&plain, source), Some(None));
	}
}
