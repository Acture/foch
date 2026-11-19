//! Guarded raw memory over the live game process.
//!
//! Every access is a kernel copy through `ReadProcessMemory` /
//! `WriteProcessMemory` on the current process: the kernel validates
//! the whole range and reports the bytes actually transferred, so an
//! unmapped, freed, or crossing range becomes a clean `None` instead of
//! an access violation. This is not a proof of object liveness — the
//! game can free a structure between two calls — and writes are not
//! atomic: a refused write may still have transferred a prefix of its
//! bytes. Preconditions: callers run on the game thread (the monthly
//! hook context every current caller uses). Only Windows x64 is
//! supported; on other targets every accessor returns `None`.

/// Reads and writes game memory behind plausibility gates.
#[derive(Clone, Copy, Debug)]
pub struct Memory;

#[cfg(windows)]
mod kernel {
	use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
	use windows_sys::Win32::System::Threading::GetCurrentProcess;

	/// Copy `len` bytes between the game and this process via the
	/// kernel. The whole requested range must transfer: a partial copy
	/// (a range crossing out of a mapped region) is a failure.
	///
	/// Writes pass a guard pre-filter first: an attempted
	/// `WriteProcessMemory` on a guarded page consumes the guard
	/// modifier even though the transfer fails (observed on this
	/// machine), while a refused read leaves it intact. The pre-filter
	/// refuses committed-but-guarded or non-writable targets without
	/// touching them; the kernel copy remains the authority for
	/// everything else, including any check-to-invalidate race the
	/// pre-filter cannot see.
	pub(crate) fn copy(addr: usize, bytes: *mut u8, len: usize, write: bool) -> bool {
		if write && !super::writable_unguarded(addr, len) {
			return false;
		}
		// SAFETY: GetCurrentProcess returns the current process's
		// pseudo-handle (never closed); the copy direction matches the
		// pointer's mutability, and the kernel bounds-checks the remote
		// range so an invalid address fails instead of faulting.
		let handle = unsafe { GetCurrentProcess() };
		let mut transferred = 0_usize;
		let ok = unsafe {
			if write {
				WriteProcessMemory(
					handle,
					addr as *mut core::ffi::c_void,
					bytes as *const core::ffi::c_void,
					len,
					&mut transferred,
				)
			} else {
				ReadProcessMemory(
					handle,
					addr as *const core::ffi::c_void,
					bytes as *mut core::ffi::c_void,
					len,
					&mut transferred,
				)
			}
		};
		ok != 0 && transferred == len
	}
}

impl Memory {
	/// Reads eight bytes. The address must sit above the null page and
	/// be aligned — pointer-shaped accesses stay pointer-shaped — and
	/// the kernel must transfer all eight bytes.
	#[cfg(windows)]
	pub fn read_qword(&self, addr: usize) -> Option<u64> {
		let mut out = 0_u64.to_le_bytes();
		(addr & 7 == 0 && self.copy_read(addr, &mut out)).then(|| u64::from_le_bytes(out))
	}

	/// Reads four bytes at any alignment inside a readable range.
	#[cfg(windows)]
	pub fn read_i32(&self, addr: usize) -> Option<i32> {
		let mut out = 0_i32.to_le_bytes();
		self.copy_read(addr, &mut out)
			.then(|| i32::from_le_bytes(out))
	}

	/// Reads one byte from a readable range.
	#[cfg(windows)]
	pub fn read_u8(&self, addr: usize) -> Option<u8> {
		let mut out = [0_u8];
		self.copy_read(addr, &mut out).then(|| out[0])
	}

	/// Reads `out.len()` bytes via a single guarded kernel copy. An empty
	/// read succeeds without touching memory; returns `None` below the
	/// null-page gate, on address overflow, or on a partial/refused copy.
	#[cfg(windows)]
	pub fn read_bytes(&self, addr: usize, out: &mut [u8]) -> Option<()> {
		if out.is_empty() {
			return Some(());
		}
		self.copy_read(addr, out).then_some(())
	}

	/// Writes eight bytes; the address must be aligned. On `None` a
	/// prefix of the value may have been written — there is no rollback.
	///
	/// # Safety
	///
	/// The kernel validates mappedness, but nothing validates Rust's
	/// aliasing: the caller must guarantee the target is engine-owned
	/// memory that no Rust reference or `bool`/immutable borrow
	/// observes, and that the game thread's structure-stability
	/// precondition holds. The higher-level mutators (`CountryView::pay`,
	/// `RouteMutation::set_closed`) are unsafe for the same reason and
	/// document their actual targets; every caller asserts the contract
	/// at its own unsafe boundary.
	#[cfg(windows)]
	pub unsafe fn write_qword(&self, addr: usize, value: u64) -> Option<()> {
		(addr & 7 == 0 && self.copy_write(addr, &value.to_le_bytes())).then_some(())
	}

	/// Writes four bytes at any alignment. On `None` a prefix may have
	/// been written — there is no rollback.
	///
	/// # Safety
	///
	/// Same contract as [`write_qword`](Memory::write_qword): an
	/// engine-owned target with no Rust-visible alias, on the game
	/// thread.
	#[cfg(windows)]
	pub unsafe fn write_i32(&self, addr: usize, value: i32) -> Option<()> {
		self.copy_write(addr, &value.to_le_bytes()).then_some(())
	}

	/// Writes `bytes`. An empty write succeeds without touching memory;
	/// on `None` a prefix of `bytes` may have been written — there is no
	/// rollback.
	///
	/// # Safety
	///
	/// Same contract as [`write_qword`](Memory::write_qword): an
	/// engine-owned target with no Rust-visible alias, on the game
	/// thread.
	#[cfg(windows)]
	pub unsafe fn write_bytes(&self, addr: usize, bytes: &[u8]) -> Option<()> {
		self.copy_write(addr, bytes).then_some(())
	}

	/// A pointer read that must itself be plausible: above the null page
	/// and aligned.
	#[cfg(windows)]
	pub fn read_ptr(&self, addr: usize) -> Option<usize> {
		self.read_qword(addr)
			.map(|value| value as usize)
			.filter(|p| *p > 0x1_0000)
	}

	/// The read pre-gates: above the null page, qword accesses aligned,
	/// the range length must not overflow — then the kernel copy.
	#[cfg(windows)]
	fn copy_read(&self, addr: usize, out: &mut [u8]) -> bool {
		self.gates(addr, out.len()) && kernel::copy(addr, out.as_mut_ptr(), out.len(), false)
	}

	/// The write pre-gates, then the kernel copy.
	#[cfg(windows)]
	fn copy_write(&self, addr: usize, bytes: &[u8]) -> bool {
		bytes.is_empty()
			|| (self.gates(addr, bytes.len())
				&& kernel::copy(addr, bytes.as_ptr() as *mut u8, bytes.len(), true))
	}

	#[cfg(windows)]
	/// Range gates only: above the null page and no address-space
	/// overflow. Alignment is a property of the qword-shaped methods,
	/// not of byte ranges — an unaligned eight-byte `write_bytes` is a
	/// legal bulk copy and must not be refused by length inference.
	fn gates(&self, addr: usize, len: usize) -> bool {
		addr > 0x1_0000 && addr.checked_add(len).is_some()
	}

	/// Non-Windows targets are unsupported: every access declines
	/// instead of pretending a guard ran.
	#[cfg(not(windows))]
	pub fn read_qword(&self, _addr: usize) -> Option<u64> {
		None
	}

	#[cfg(not(windows))]
	pub fn read_i32(&self, _addr: usize) -> Option<i32> {
		None
	}

	#[cfg(not(windows))]
	pub fn read_u8(&self, _addr: usize) -> Option<u8> {
		None
	}

	#[cfg(not(windows))]
	pub fn read_bytes(&self, _addr: usize, _out: &mut [u8]) -> Option<()> {
		None
	}

	/// Unsupported target: declines without touching memory.
	///
	/// # Safety
	///
	/// Same contract as the Windows
	/// [`write_qword`](Memory::write_qword): callers assert an
	/// engine-owned, unaliased target on the engine thread — the
	/// signature must not drift across platforms even where the access
	/// always declines.
	#[cfg(not(windows))]
	pub unsafe fn write_qword(&self, _addr: usize, _value: u64) -> Option<()> {
		None
	}

	/// Unsupported target: declines without touching memory.
	///
	/// # Safety
	///
	/// Same contract as [`write_qword`](Memory::write_qword).
	#[cfg(not(windows))]
	pub unsafe fn write_i32(&self, _addr: usize, _value: i32) -> Option<()> {
		None
	}

	/// Unsupported target: declines without touching memory.
	///
	/// # Safety
	///
	/// Same contract as [`write_qword`](Memory::write_qword).
	#[cfg(not(windows))]
	pub unsafe fn write_bytes(&self, _addr: usize, _bytes: &[u8]) -> Option<()> {
		None
	}

	#[cfg(not(windows))]
	pub fn read_ptr(&self, _addr: usize) -> Option<usize> {
		None
	}
}

/// Whether `[addr, addr + len)` lies entirely inside one committed,
/// writable, unguarded region — probed, not touched, so refusing a
/// guarded page leaves its guard modifier intact. VirtualQuery reports
/// the contiguous run of one protection; a request crossing past its
/// end (into a following GUARD or NOACCESS region, say) is refused
/// whole rather than split.
#[cfg(windows)]
fn writable_unguarded(addr: usize, len: usize) -> bool {
	use windows_sys::Win32::System::Memory::{
		MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY,
		PAGE_GUARD, PAGE_READWRITE, PAGE_WRITECOPY, VirtualQuery,
	};
	let Some(end) = addr.checked_add(len) else {
		return false;
	};
	let mut info = MEMORY_BASIC_INFORMATION::default();
	// SAFETY: VirtualQuery writes into `info` for the queried address
	// and touches nothing else.
	let queried = unsafe {
		VirtualQuery(
			addr as *const core::ffi::c_void,
			&mut info,
			size_of::<MEMORY_BASIC_INFORMATION>(),
		)
	};
	if queried == 0 || info.State != MEM_COMMIT || info.Protect & PAGE_GUARD != 0 {
		return false;
	}
	let region_end = (info.BaseAddress as usize).saturating_add(info.RegionSize);
	if end > region_end {
		return false;
	}
	info.Protect
		& (PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)
		!= 0
}

/// Guarded MSVC `std::vector` header (`{ begin, end, capacity }`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StdVector {
	pub begin: usize,
	pub end: usize,
	pub capacity: usize,
}

impl StdVector {
	/// Reads a three-qword MSVC `std::vector` header and checks
	/// stride-independent bounds (`begin <= end <= capacity`, and either an
	/// empty null vector or a `begin` pointer above the null page).
	pub fn read(memory: &Memory, addr: usize) -> Option<Self> {
		let begin = memory.read_qword(addr)? as usize;
		let end = memory.read_qword(addr.checked_add(8)?)? as usize;
		let capacity = memory.read_qword(addr.checked_add(16)?)? as usize;
		let header = Self {
			begin,
			end,
			capacity,
		};
		header.bounds_valid().then_some(header)
	}

	fn bounds_valid(&self) -> bool {
		self.end >= self.begin
			&& self.capacity >= self.end
			&& ((self.begin == 0 && self.capacity == 0) || self.begin > 0x1_0000)
	}

	/// Validates alignment, bounds, stride divisibility, and element count
	/// against `max_elements`, returning the element count.
	pub fn len(&self, stride: usize, max_elements: usize) -> Option<usize> {
		if stride == 0 || !self.bounds_valid() {
			return None;
		}
		let alignment = 1usize << stride.trailing_zeros().min(3);
		if !self.begin.is_multiple_of(alignment)
			|| !(self.end - self.begin).is_multiple_of(stride)
			|| !(self.capacity - self.begin).is_multiple_of(stride)
		{
			return None;
		}
		let count = (self.end - self.begin) / stride;
		(count <= max_elements).then_some(count)
	}
}

/// Reads an MSVC SSO `std::string` layout: size at `+0x10`, capacity at
/// `+0x18`, the bytes inline when the capacity is `<= 0xF` and behind the
/// first qword otherwise. `accept` decides the allowed byte set.
pub fn read_sso(memory: &Memory, addr: usize, accept: impl Fn(u8) -> bool) -> Option<String> {
	// Every offset arithmetic stays checked: a near-`usize::MAX` object
	// header must decline, not wrap back into the address space.
	let size_at = addr.checked_add(0x10)?;
	let capacity_at = addr.checked_add(0x18)?;
	let length = memory.read_qword(size_at)? as usize;
	if length == 0 || length > 64 {
		return None;
	}
	let capacity = memory.read_qword(capacity_at)?;
	let data = if capacity > 0xF {
		memory.read_ptr(addr)?
	} else {
		addr
	};
	let mut bytes = Vec::with_capacity(length);
	for i in 0..length {
		let byte = memory.read_u8(data.checked_add(i)?)?;
		if !accept(byte) {
			return None;
		}
		bytes.push(byte);
	}
	String::from_utf8(bytes).ok()
}

/// The internal-key charset: `a-z0-9_`.
pub fn is_key_byte(byte: u8) -> bool {
	byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
}

/// The display-name charset: printable ASCII, spaces, and any UTF-8 lead
/// or continuation byte.
pub fn is_name_byte(byte: u8) -> bool {
	byte == b' ' || byte.is_ascii_graphic() || byte >= 0x80
}

#[cfg(all(test, windows))]
mod tests {
	use super::*;

	/// Self-made guard pages: one committed page followed by one
	/// reserved-uncommitted page inside the same allocation, so a
	/// boundary access that overruns the committed page provably leaves
	/// mapped memory. Released on drop.
	struct GuardPages(usize);

	impl GuardPages {
		fn new() -> Option<GuardPages> {
			use windows_sys::Win32::System::Memory::{
				MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
			};
			let base =
				unsafe { VirtualAlloc(core::ptr::null(), 0x2_000, MEM_RESERVE, PAGE_READWRITE) }
					as usize;
			if base == 0 {
				return None;
			}
			let committed = unsafe {
				VirtualAlloc(
					base as *mut core::ffi::c_void,
					0x1_000,
					MEM_COMMIT,
					PAGE_READWRITE,
				)
			} as usize;
			if committed != base {
				Self::free(base);
				return None;
			}
			Some(GuardPages(base))
		}

		/// The first address of the second (reserved) page.
		fn dark(&self) -> usize {
			self.0 + 0x1_000
		}

		/// A variant with both pages committed; the second page's
		/// protection is the caller's to set, for cross-region tests.
		fn both_committed() -> Option<GuardPages> {
			use windows_sys::Win32::System::Memory::{
				MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
			};
			let base =
				unsafe { VirtualAlloc(core::ptr::null(), 0x2_000, MEM_RESERVE, PAGE_READWRITE) }
					as usize;
			if base == 0 {
				return None;
			}
			let committed = unsafe {
				VirtualAlloc(
					base as *mut core::ffi::c_void,
					0x2_000,
					MEM_COMMIT,
					PAGE_READWRITE,
				)
			} as usize;
			if committed != base {
				Self::free(base);
				return None;
			}
			Some(GuardPages(base))
		}

		/// The address two bytes before the committed page ends: a
		/// four-byte access there provably crosses the boundary.
		fn boundary(&self) -> usize {
			self.0 + 0x1_000 - 2
		}

		/// Change the protection of the committed page's first page and
		/// return the previous protection.
		fn protect(&self, new: u32) -> u32 {
			use windows_sys::Win32::System::Memory::VirtualProtect;
			let mut old = 0_u32;
			let flipped =
				unsafe { VirtualProtect(self.0 as *mut core::ffi::c_void, 0x1_000, new, &mut old) };
			assert_ne!(flipped, 0, "VirtualProtect failed");
			old
		}

		/// The current protection at `addr` per VirtualQuery.
		fn protection_at(&self, addr: usize) -> u32 {
			use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
			let mut info = MEMORY_BASIC_INFORMATION::default();
			let queried = unsafe {
				VirtualQuery(
					addr as *const core::ffi::c_void,
					&mut info,
					size_of::<MEMORY_BASIC_INFORMATION>(),
				)
			};
			assert_ne!(queried, 0, "VirtualQuery failed");
			info.Protect
		}

		/// The committed (first) page's current protection.
		fn protection(&self) -> u32 {
			self.protection_at(self.0)
		}

		fn free(base: usize) {
			use windows_sys::Win32::System::Memory::{MEM_RELEASE, VirtualFree};
			// Releasing the whole original allocation takes the base,
			// size zero, and MEM_RELEASE; the return says it happened.
			// The commit-failure cleanup path calls this same checked
			// release.
			let released = unsafe { VirtualFree(base as *mut core::ffi::c_void, 0, MEM_RELEASE) };
			assert_ne!(released, 0, "MEM_RELEASE failed for the test pages");
		}
	}

	impl Drop for GuardPages {
		fn drop(&mut self) {
			GuardPages::free(self.0);
		}
	}

	#[test]
	fn valid_allocations_pass_every_accessor() {
		let memory = Memory;
		let buffer = [0_u64; 4];
		let address = buffer.as_ptr() as usize;
		// The value must clear the null-page plausibility gate for
		// read_ptr, so pick one above 0x1_0000.
		assert!(unsafe { memory.write_qword(address, 0x1_ABCD) }.is_some());
		assert_eq!(memory.read_qword(address), Some(0x1_ABCD));
		assert_eq!(memory.read_i32(address), Some(0x1_ABCD_i32));
		assert_eq!(memory.read_u8(address), Some(0xCD));
		assert_eq!(memory.read_ptr(address), Some(0x1_ABCD));
		assert!(unsafe { memory.write_bytes(address, &[1, 2, 3, 4]) }.is_some());
		// An empty write is a success that touches nothing.
		assert_eq!(unsafe { memory.write_bytes(address, &[]) }, Some(()));
		// An unaligned in-page i32 reads the bytes write_bytes left:
		// [1, 2, 3, 4] means offset +2 holds [3, 4, 0, 0] = 0x0403.
		assert_eq!(memory.read_i32(address + 2), Some(0x0403));
	}

	#[test]
	fn null_page_alignment_and_overflow_short_circuit() {
		let memory = Memory;
		assert_eq!(memory.read_qword(0x1000), None);
		assert_eq!(unsafe { memory.write_i32(0x20, 1) }, None);
		// Misaligned qword on a valid buffer declines before any copy.
		let buffer = [0_u64; 4];
		let address = buffer.as_ptr() as usize;
		assert_eq!(memory.read_qword(address + 1), None);
		// A range that overflows the address space declines arithmetically.
		let top = usize::MAX & !7;
		assert_eq!(memory.read_qword(top), None);
	}

	#[test]
	fn reserved_uncommitted_pages_refuse_without_faulting() {
		// Reserving two pages of address space cannot legitimately fail;
		// a silent skip would fake the coverage this test exists for.
		let pages = GuardPages::new().expect("reserve the guard pages");
		let memory = Memory;
		// The old implementation dereferenced here and took an access
		// violation; the kernel copy must refuse instead.
		assert_eq!(memory.read_qword(pages.dark()), None);
		assert_eq!(memory.read_u8(pages.dark()), None);
		assert_eq!(unsafe { memory.write_i32(pages.dark(), 1) }, None);
		assert_eq!(unsafe { memory.write_bytes(pages.dark(), &[0; 8]) }, None);
	}

	#[test]
	fn ranges_crossing_out_of_a_committed_page_refuse() {
		let pages = GuardPages::new().expect("reserve the guard pages");
		let memory = Memory;
		// Two readable bytes remain; four-byte accesses at this address
		// cross into the reserved page and must be refused whole.
		assert!(memory.read_u8(pages.boundary()).is_some());
		assert_eq!(memory.read_i32(pages.boundary()), None);
		assert_eq!(
			unsafe { memory.write_bytes(pages.boundary(), &[0; 4]) },
			None
		);
		// A refused write may have transferred a prefix (here: two
		// bytes); the contract documents no rollback, so only the
		// failure itself is asserted.
	}

	#[test]
	fn unaligned_bulk_writes_pass_but_qword_writes_do_not() {
		let memory = Memory;
		let buffer = [0_u64; 4];
		let address = buffer.as_ptr() as usize;
		// An unaligned eight-byte bulk write is a legal range copy…
		assert!(unsafe { memory.write_bytes(address + 1, &[7; 8]) }.is_some());
		// …while the qword-shaped access keeps its alignment gate.
		assert_eq!(unsafe { memory.write_qword(address + 1, 0) }, None);
	}

	#[test]
	fn sso_reads_at_the_address_space_edge_decline_instead_of_wrapping() {
		let memory = Memory;
		// A header near usize::MAX must decline through checked
		// arithmetic, never wrap into low memory or panic.
		let top = usize::MAX - 8;
		assert_eq!(read_sso(&memory, top, is_key_byte), None);
		assert_eq!(read_sso(&memory, usize::MAX, is_name_byte), None);
	}

	#[test]
	fn noaccess_pages_refuse_every_access() {
		use windows_sys::Win32::System::Memory::PAGE_NOACCESS;
		let pages = GuardPages::new().expect("reserve the guard pages");
		let old = pages.protect(PAGE_NOACCESS);
		let memory = Memory;
		assert_eq!(memory.read_qword(pages.0), None);
		assert_eq!(memory.read_u8(pages.0), None);
		assert_eq!(unsafe { memory.write_i32(pages.0, 1) }, None);
		pages.protect(old);
	}

	#[test]
	fn guard_pages_refuse_reads_and_writes_and_the_guard_survives() {
		use windows_sys::Win32::System::Memory::{PAGE_GUARD, PAGE_READWRITE};
		let guard = PAGE_READWRITE | PAGE_GUARD;
		let memory = Memory;

		// Each branch gets its own freshly guarded page, so an operation
		// that might change the protection cannot masquerade as the
		// other branch's starting state.
		let read_pages = GuardPages::new().expect("reserve the read-probe page");
		let old = read_pages.protect(guard);
		assert_eq!(read_pages.protection(), guard);
		// Observed on Windows 11 (rust 1.95): the kernel copy REFUSES a
		// guarded page, and the refused READ leaves the guard intact.
		assert_eq!(memory.read_qword(read_pages.0), None);
		assert_eq!(read_pages.protection(), guard);
		read_pages.protect(old);

		let write_pages = GuardPages::new().expect("reserve the write-probe page");
		let old = write_pages.protect(guard);
		assert_eq!(write_pages.protection(), guard);
		// Observed without the pre-filter: a refused WriteProcessMemory
		// CONSUMES the guard modifier (protection drops to READWRITE).
		// The write pre-filter now refuses guarded targets untouched,
		// so the refusal leaves the guard in place.
		assert_eq!(unsafe { memory.write_i32(write_pages.0, 1) }, None);
		assert_eq!(write_pages.protection(), guard);
		write_pages.protect(old);
	}

	#[test]
	fn writes_crossing_into_a_guarded_region_refuse_and_keep_the_guard() {
		use windows_sys::Win32::System::Memory::{PAGE_GUARD, PAGE_READWRITE};
		let pages = GuardPages::both_committed().expect("commit both test pages");
		let guard = PAGE_READWRITE | PAGE_GUARD;
		// Guard only the second page; the first stays plain RW.
		let mut old = 0_u32;
		let flipped = unsafe {
			windows_sys::Win32::System::Memory::VirtualProtect(
				pages.dark() as *mut core::ffi::c_void,
				0x1_000,
				guard,
				&mut old,
			)
		};
		assert_ne!(flipped, 0, "guarding the second page failed");
		let memory = Memory;
		// A four-byte write at the first page's last two bytes crosses
		// into the guarded region: the range pre-filter must refuse it
		// whole, before any kernel copy could consume the guard.
		let boundary = pages.dark() - 2;
		assert_eq!(unsafe { memory.write_bytes(boundary, &[0xAA; 4]) }, None);
		assert_eq!(pages.protection_at(pages.dark()), guard);
		// The first page's bytes are untouched by the refused write…
		assert_eq!(memory.read_u8(boundary), Some(0));
		// …while a write entirely inside the first page succeeds.
		assert!(unsafe { memory.write_bytes(boundary - 4, &[0xAA; 4]) }.is_some());
		let restored = unsafe {
			windows_sys::Win32::System::Memory::VirtualProtect(
				pages.dark() as *mut core::ffi::c_void,
				0x1_000,
				old,
				&mut old,
			)
		};
		assert_ne!(restored, 0, "restoring the second page failed");
	}

	#[test]
	fn read_only_pages_read_successfully_and_refuse_writes() {
		use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
		let pages = GuardPages::new().expect("reserve the guard pages");
		let address = pages.0 | 0x800; // mid-page, aligned
		let mut old = 0_u32;
		let flipped = unsafe {
			VirtualProtect(
				address as *mut core::ffi::c_void,
				8,
				PAGE_READONLY,
				&mut old,
			)
		};
		assert_ne!(flipped, 0, "could not flip the test page to read-only");
		let memory = Memory;
		// The kernel copy still reads a read-only page…
		assert!(memory.read_qword(address).is_some());
		// …but refuses to write it.
		assert_eq!(unsafe { memory.write_qword(address, 0) }, None);
		// Restore before drop frees the region.
		let mut back = 0_u32;
		let _ = unsafe { VirtualProtect(address as *mut core::ffi::c_void, 8, old, &mut back) };
	}

	#[test]
	fn read_bytes_reads_valid_ranges_and_rejects_invalid_ones() {
		let memory = Memory;
		let source: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
		let address = source.as_ptr() as usize;

		let mut out = [0_u8; 10];
		assert_eq!(memory.read_bytes(address + 3, &mut out), Some(()));
		assert_eq!(out, [3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);

		// Empty slice succeeds without touching memory.
		assert_eq!(memory.read_bytes(address, &mut []), Some(()));

		// Null-page and overflowing ranges are refused.
		assert_eq!(memory.read_bytes(0x1000, &mut out), None);
		assert_eq!(memory.read_bytes(usize::MAX - 2, &mut out), None);

		// Partial reads across a committed/reserved boundary are refused whole.
		let pages = GuardPages::new().expect("reserve the guard pages");
		let mut cross = [0_u8; 4];
		assert_eq!(memory.read_bytes(pages.boundary(), &mut cross), None);
		assert_eq!(memory.read_bytes(pages.dark(), &mut cross), None);
	}

	#[test]
	fn std_vector_validates_bounds_alignment_stride_and_limit() {
		let memory = Memory;
		let data = [0_u64; 8];
		let base = data.as_ptr() as usize;

		// Empty null vector is valid.
		let empty_header = [0_u64, 0, 0];
		let empty = StdVector::read(&memory, empty_header.as_ptr() as usize).unwrap();
		assert_eq!(
			empty,
			StdVector {
				begin: 0,
				end: 0,
				capacity: 0,
			}
		);
		assert_eq!(empty.len(8, 16), Some(0));

		// Populated vector with extra reserved capacity.
		let header = [base as u64, (base + 24) as u64, (base + 64) as u64];
		let vec = StdVector::read(&memory, header.as_ptr() as usize).unwrap();
		assert_eq!(vec.len(8, 16), Some(3));
		assert_eq!(vec.len(8, 3), Some(3));
		assert_eq!(vec.len(8, 2), None); // exceeds max_elements
		assert_eq!(vec.len(0, 16), None); // zero stride
		assert_eq!(vec.len(16, 16), None); // end - begin (24) not multiple of 16

		// Empty vector with reserved capacity.
		let reserved = [base as u64, base as u64, (base + 16) as u64];
		let reserved_vec = StdVector::read(&memory, reserved.as_ptr() as usize).unwrap();
		assert_eq!(reserved_vec.len(8, 16), Some(0));

		// Misaligned begin fails alignment check for stride 8, passes for stride 1.
		let misaligned = [(base + 1) as u64, (base + 9) as u64, (base + 17) as u64];
		let misaligned_vec = StdVector::read(&memory, misaligned.as_ptr() as usize).unwrap();
		assert_eq!(misaligned_vec.len(8, 16), None);
		assert_eq!(misaligned_vec.len(1, 16), Some(8));

		// Non-multiple capacity fails stride check.
		let bad_cap = [base as u64, (base + 8) as u64, (base + 12) as u64];
		let bad_cap_vec = StdVector::read(&memory, bad_cap.as_ptr() as usize).unwrap();
		assert_eq!(bad_cap_vec.len(8, 16), None);

		// Invalid bounds rejected at read time.
		for bad in [
			[(base + 16) as u64, base as u64, (base + 16) as u64], // end < begin
			[base as u64, (base + 16) as u64, (base + 8) as u64],  // capacity < end
			[0_u64, 0, 16],                                        // null begin with nonzero capacity
			[0x1000_u64, 0x1008, 0x1008],                          // begin inside null page
		] {
			assert_eq!(StdVector::read(&memory, bad.as_ptr() as usize), None);
		}
	}
}
