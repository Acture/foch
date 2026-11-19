//! The EU4 data plane: typed, guarded access to the live game.
//!
//! Plugins build their policies on these views instead of carrying
//! their own offset tables and pointer walkers. The layout is pinned to
//! one build (see [`layout`]). [`Memory`] uses checked kernel copies on Windows;
//! complete readable ranges can still hold the wrong object or stale state.
//! Callers must establish identity, type, lifetime and game-thread stability.

pub mod flags;
pub mod game;
pub mod layout;
pub mod memory;
pub mod routes;

pub use flags::{CountryFlag, CountryFlags, FlagReadError};
pub use game::{
	Countries, CountryView, Eu4Game, NodeView, PackedCountryTag, Province, TradeNodes,
	decode_packed_tag,
};
pub use memory::{Memory, StdVector, is_key_byte, is_name_byte, read_sso};
pub use routes::{RouteMutation, live_order};

/// A host-computed executable digest must match before using the pinned layout.
pub fn supports_build(exe_sha256: &str) -> bool {
	exe_sha256 == layout::EXPECTED_EXE_SHA256
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn guarded_accessors_reject_the_null_page() {
		let memory = Memory;
		assert_eq!(memory.read_qword(0x1000), None);
		assert_eq!(memory.read_qword(8), None); // aligned but below the gate
		assert_eq!(unsafe { memory.write_i32(0x20, 1) }, None);
	}

	/// Build a fake in-process object so the pointer-walking readers can
	/// be tested against real (aligned, plausible) addresses.
	#[cfg(windows)]
	fn scratch(qwords: usize) -> (Vec<u64>, usize) {
		let mut buffer = vec![0_u64; qwords];
		let address = buffer.as_mut_ptr() as usize;
		buffer[0] = address as u64; // self-pointer handy for SSO data
		(buffer, address)
	}

	#[test]
	#[cfg(windows)]
	fn sso_keys_read_inline_and_pointer_forms() {
		let memory = Memory;
		let (mut buffer, address) = scratch(6);
		buffer[0] = u64::from_le_bytes(*b"zanzibar");
		buffer[1] = 0;
		buffer[2] = 8; // length at +0x10
		buffer[3] = 0xF; // capacity at +0x18: inline
		assert_eq!(
			read_sso(&memory, address, is_key_byte).as_deref(),
			Some("zanzibar")
		);
		// Pointer form: capacity > 0xF dereferences the first qword,
		// aimed at key bytes living later in the same allocation.
		let (mut buffer, address) = scratch(6);
		buffer[0] = (address + 8 * 5) as u64;
		buffer[2] = 4;
		buffer[3] = 0x1F;
		buffer[5] = u64::from_le_bytes(*b"siamxxxx");
		assert_eq!(
			read_sso(&memory, address, is_key_byte).as_deref(),
			Some("siam")
		);
	}

	#[test]
	#[cfg(windows)]
	fn sso_reads_reject_implausible_lengths_and_charsets() {
		let memory = Memory;
		let (mut buffer, address) = scratch(6);
		buffer[2] = 65; // beyond the documented key range
		buffer[3] = 0xF;
		assert_eq!(read_sso(&memory, address, is_key_byte), None);
		buffer[2] = 4;
		buffer[0] = u64::from_le_bytes([b'U', b'P', b'P', b'E', 0, 0, 0, 0]);
		assert_eq!(read_sso(&memory, address, is_key_byte), None);
		// The display-name charset accepts the uppercase.
		assert_eq!(
			read_sso(&memory, address, is_name_byte).as_deref(),
			Some("UPPE")
		);
	}

	#[test]
	fn open_entries_stay_first_in_vanilla_order() {
		assert_eq!(live_order(&[false, false]), vec![0, 1]);
		assert_eq!(live_order(&[false, true, false]), vec![0, 2, 1]);
		assert_eq!(live_order(&[true, true]), vec![0, 1]);
		assert_eq!(live_order(&[true, false, true]), vec![1, 0, 2]);
		assert!(live_order(&[]).is_empty());
	}
}
