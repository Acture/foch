//! Guarded raw memory over the live game process.
//!
//! Every access goes through the same plausibility gates the plugin
//! backend used: addresses must sit above the null page, and the qword
//! accessors additionally require alignment. A failed gate returns
//! `None` instead of faulting, so a layout drift degrades into missing
//! data rather than a crash inside the game.

/// Reads and writes game memory behind plausibility gates.
#[derive(Clone, Copy, Debug)]
pub struct Memory;

impl Memory {
	/// Reads eight bytes; the address must be aligned and above the null
	/// page.
	pub fn read_qword(&self, addr: usize) -> Option<u64> {
		(addr > 0x1_0000 && addr & 7 == 0).then(|| unsafe { core::ptr::read(addr as *const u64) })
	}

	/// Reads four bytes at any alignment above the null page.
	pub fn read_i32(&self, addr: usize) -> Option<i32> {
		(addr > 0x1_0000).then(|| unsafe { core::ptr::read_unaligned(addr as *const i32) })
	}

	/// Reads one byte; the byte walkers must not touch the page after
	/// their object near a segment boundary.
	pub fn read_u8(&self, addr: usize) -> Option<u8> {
		(addr > 0x1_0000).then(|| unsafe { core::ptr::read(addr as *const u8) })
	}

	pub fn write_qword(&self, addr: usize, value: u64) -> Option<()> {
		(addr > 0x1_0000 && addr & 7 == 0)
			.then(|| unsafe { core::ptr::write(addr as *mut u64, value) })
	}

	pub fn write_i32(&self, addr: usize, value: i32) -> Option<()> {
		(addr > 0x1_0000).then(|| unsafe { core::ptr::write_unaligned(addr as *mut i32, value) })
	}

	pub fn write_bytes(&self, addr: usize, bytes: &[u8]) -> Option<()> {
		(addr > 0x1_0000).then(|| unsafe {
			core::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len())
		})
	}

	/// A pointer read that must itself be plausible: above the null page
	/// and aligned.
	pub fn read_ptr(&self, addr: usize) -> Option<usize> {
		self.read_qword(addr)
			.map(|value| value as usize)
			.filter(|p| *p > 0x1_0000)
	}
}

/// Reads an MSVC SSO `std::string` layout: size at `+0x10`, capacity at
/// `+0x18`, the bytes inline when the capacity is `<= 0xF` and behind the
/// first qword otherwise. `accept` decides the allowed byte set.
pub fn read_sso(memory: &Memory, addr: usize, accept: impl Fn(u8) -> bool) -> Option<String> {
	let length = memory.read_qword(addr + 0x10)? as usize;
	if length == 0 || length > 64 {
		return None;
	}
	let capacity = memory.read_qword(addr + 0x18)?;
	let data = if capacity > 0xF {
		memory.read_ptr(addr)?
	} else {
		addr
	};
	let mut bytes = Vec::with_capacity(length);
	for i in 0..length {
		let byte = memory.read_u8(data + i)?;
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
