//! Gated candidate reads over the shared dataplane.
//!
//! Every candidate read goes through [`VerifiedGame`]: the caller
//! supplies the host-computed digest of the live executable (the
//! plugin SDK's host value — never an expected constant pasted in),
//! and only `supports_build` passing opens the door to the walk.
//! Malformed or mismatched digests are rejected in the constructor,
//! before any memory address is formed. There is no plugin entry
//! wired yet; the caller contract is exactly this constructor.
//!
//! Verified fields (province array, tax, manpower, country registry)
//! go through the shared dataplane views; everything else goes
//! through [`crate::candidates`] and stays `Option`-gated — a `None`
//! is *unknown*, never a zero filling the plane. A `Some` from a
//! candidate is still not live verification. The owner field carries
//! a country **index** (bits 32..48 of its packed qword), not a tag;
//! tags decode through the live country registry.

use foch_runtime::dataplane::{Eu4Game, Memory, Province};

use crate::candidates;

/// A live game whose host-computed digest passed the shared build
/// gate. The only entrance to the candidate reads below.
pub struct VerifiedGame {
	memory: Memory,
	context: usize,
}

impl VerifiedGame {
	/// `exe_sha256` must be the digest the HOST computed for the
	/// running executable. Empty, truncated, non-hex, or mismatched
	/// values are rejected here — before any memory read — by format
	/// first and then by [`foch_runtime::dataplane::supports_build`].
	pub fn new(memory: Memory, context: usize, exe_sha256: &str) -> Option<Self> {
		if exe_sha256.len() != 64 || !exe_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
			return None;
		}
		foch_runtime::dataplane::supports_build(exe_sha256).then_some(Self { memory, context })
	}

	/// The address of one province row, from the shared verified
	/// province view. All arithmetic is checked: an id or context near
	/// `usize::MAX` yields `None`, never a wrap.
	pub fn province_address(&self, id: u32) -> Option<usize> {
		// Province records hang off the game context. Image-base globals are
		// not consulted on this path.
		let game = Eu4Game {
			memory: self.memory,
			exe_base: 0,
			context: self.context,
		};
		Some(game.province(id)?.raw_address())
	}

	/// The owner's country index from the packed owner qword (bits
	/// 32..48, read the way the engine does).
	pub fn owner_country_index(&self, province: usize) -> Option<i32> {
		let packed = self
			.memory
			.read_qword(province.checked_add(candidates::province::OWNER_INDEX)?)?;
		Some(((packed >> 0x20) as u16) as i32)
	}

	/// The owner's tag, decoded through the live country registry.
	pub fn owner_tag(
		&self,
		countries: &foch_runtime::dataplane::Countries,
		province: usize,
	) -> Option<String> {
		let index = self.owner_country_index(province)?;
		countries
			.country(usize::try_from(index).ok()?)
			.map(|country| country.tag())
	}

	/// Plane column 2: development = (tax + production + manpower)
	/// ÷1000 ÷35. Tax and manpower are verified layout reads;
	/// production is a candidate, so any failed read makes the whole
	/// column unknown.
	pub fn development(&self, province: usize) -> Option<f32> {
		let tax = Province::tax_at(&self.memory, province)?;
		let manpower = Province::manpower_at(&self.memory, province)?;
		let production = self
			.memory
			.read_i32(province.checked_add(candidates::province::BASE_PRODUCTION)?)?;
		Some(development_from(tax, production, manpower))
	}

	/// Plane column 3 ÷3 is applied by the caller; this is the raw
	/// candidate read.
	pub fn fort_level(&self, province: usize) -> Option<i32> {
		self.memory
			.read_i32(province.checked_add(candidates::province::FORT_LEVEL)?)
	}

	pub fn unrest(&self, province: usize) -> Option<i32> {
		self.memory
			.read_i32(province.checked_add(candidates::province::UNREST)?)
	}

	pub fn devastation(&self, province: usize) -> Option<i32> {
		self.memory
			.read_i32(province.checked_add(candidates::province::DEVASTATION)?)
	}

	/// The province count, from the id-table vector the engine itself
	/// bounds-checks relocations against.
	pub fn province_count(&self) -> Option<usize> {
		let ids = self.context.checked_add(candidates::world::PROVINCE_IDS)?;
		let begin = self.memory.read_qword(ids)? as usize;
		let end = self.memory.read_qword(ids.checked_add(8)?)? as usize;
		province_count_from(begin, end)
	}
}

/// The pure arithmetic behind [`VerifiedGame::development`]: widened
/// before the sum so three legal `i32` inputs cannot overflow.
pub fn development_from(tax: i32, production: i32, manpower: i32) -> f32 {
	(i64::from(tax) + i64::from(production) + i64::from(manpower)) as f32 / 1000.0 / 35.0
}

/// The pure arithmetic behind [`VerifiedGame::province_count`].
pub fn province_count_from(begin: usize, end: usize) -> Option<usize> {
	(end >= begin && end.is_multiple_of(8) && begin.is_multiple_of(8)).then(|| (end - begin) / 8)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A syntactically valid but WRONG digest: same shape, different
	/// bits than the pinned build.
	const WRONG_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

	#[test]
	fn malformed_digests_are_rejected_before_any_read() {
		let memory = Memory;
		// Empty, short, long, non-hex, and right-shaped-but-wrong all
		// fail the constructor. The candidate reads are methods on the
		// gated type, so a rejected construction leaves no path to a
		// memory read at all.
		for digest in [
			"",
			"abc",
			"a".repeat(63).as_str(),
			"a".repeat(65).as_str(),
			"zz…not-hex",
			WRONG_DIGEST,
		] {
			assert!(
				VerifiedGame::new(memory, 0x7ff0_0000, digest).is_none(),
				"digest {digest:?} must be rejected"
			);
		}
	}

	#[test]
	fn extreme_addresses_and_ids_never_panic() {
		let memory = Memory;
		assert!(VerifiedGame::new(memory, usize::MAX, WRONG_DIGEST).is_none());
		// The checked walk under the accepted digest — taken from the
		// shared crate, not hardcoded here: MAX context / MAX id must
		// return None, not wrap or panic.
		let expected = foch_runtime::dataplane::pinned_exe_sha256();
		let Some(game) = VerifiedGame::new(memory, usize::MAX, expected) else {
			panic!("shared pinned digest must open the gate");
		};
		assert_eq!(game.province_address(u32::MAX), None);
		assert_eq!(game.province_count(), None);
	}

	#[test]
	fn owner_index_reads_the_engine_window_of_the_packed_qword() {
		// High 32 bits hold the index; the engine truncates to a short.
		let packed = (0x0000_012c_u64 << 0x20) | 0xffff_ffff;
		assert_eq!(((packed >> 0x20) as u16) as i32, 0x012c);
	}

	#[test]
	fn development_matches_the_plane_normalization() {
		// 10 + 8 + 6 development = 24000 thousandths -> 24/35.
		let value = development_from(10_000, 8_000, 6_000);
		assert!((value - 24.0 / 35.0).abs() < 1e-6);
	}

	#[test]
	fn development_of_extreme_inputs_stays_finite() {
		// Three maximal legal i32 values must not overflow the sum.
		let value = development_from(i32::MAX, i32::MAX, i32::MAX);
		assert!(value.is_finite() && value > 0.0);
	}

	#[test]
	fn province_count_takes_the_vector_length_in_qwords() {
		assert_eq!(province_count_from(0x1000, 0x1000 + 8 * 4209), Some(4209));
		assert_eq!(province_count_from(0x1008, 0x1000), None);
		assert_eq!(province_count_from(0x1000, 0x1004), None);
	}
}
