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
		Some(country_index_from_packed(packed))
	}

	/// The controller's country index from the packed controller qword
	/// (bits 32..48, matching [`Self::owner_country_index`]).
	pub fn controller_index(&self, province: usize) -> Option<i32> {
		let packed = self
			.memory
			.read_qword(province.checked_add(candidates::province::CONTROLLER)?)?;
		Some(country_index_from_packed(packed))
	}

	/// Whether the province's flags byte has both occupied bits
	/// ([`candidates::province::OCCUPIED_MASK`]) set.
	pub fn occupied_flags(&self, province: usize) -> Option<bool> {
		let flags = self
			.memory
			.read_u8(province.checked_add(candidates::province::FLAGS)?)?;
		Some(is_occupied_flags(flags))
	}

	/// The province mark dword compared against a country's capital mark
	/// ([`candidates::country::MARK`]).
	pub fn province_mark(&self, province: usize) -> Option<u32> {
		self.memory
			.read_i32(province.checked_add(candidates::province::MARK)?)
			.map(|raw| raw as u32)
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

	/// The terrain object's display name behind one province template
	/// (SSO string at terrain+0x118, read through the shared
	/// dataplane's `read_sso`). Candidate offsets.
	pub fn template_terrain_name(&self, template_index: usize) -> Option<String> {
		let templates = self
			.memory
			.read_ptr(self.context.checked_add(candidates::world::TEMPLATES)?)?;
		let template =
			templates.checked_add(template_index.checked_mul(candidates::template::STRIDE)?)?;
		let terrain = self
			.memory
			.read_ptr(template.checked_add(candidates::template::TERRAIN)?)?;
		foch_runtime::dataplane::read_sso(
			&self.memory,
			terrain.checked_add(candidates::template::NAME)?,
			foch_runtime::dataplane::is_name_byte,
		)
	}

	/// The terrain name of one province: scans the template array for
	/// the template whose province-id field matches, bounded by
	/// `bound` (the caller's province count works — template ids and
	/// province ids share the engine's numbering space). Candidate
	/// offsets; a missing template or name is `None`.
	pub fn terrain_of(&self, province_id: u32, bound: usize) -> Option<String> {
		for index in 0..bound {
			let templates = self
				.memory
				.read_ptr(self.context.checked_add(candidates::world::TEMPLATES)?)?;
			let template =
				templates.checked_add(index.checked_mul(candidates::template::STRIDE)?)?;
			let marked = self
				.memory
				.read_i32(template.checked_add(candidates::template::PROVINCE_ID)?)?;
			if marked == province_id as i32 {
				return self.template_terrain_name(index);
			}
		}
		None
	}

	/// The war's clamped score total from a war object address
	/// (candidate offset `war::SCORE_VALUE`).
	pub fn war_score_total(&self, war: usize) -> Option<i32> {
		self.memory
			.read_i32(war.checked_add(candidates::war::SCORE_VALUE)?)
	}

	/// Dword count of regiment pointers on a combat-side object
	/// ([`candidates::combat_side::REGIMENT_COUNT`]). Negative reads are
	/// refused. This is not a live-verified headcount.
	pub fn regiment_slot_count(&self, object: usize) -> Option<i32> {
		let count = self
			.memory
			.read_i32(object.checked_add(candidates::combat_side::REGIMENT_COUNT)?)?;
		(count >= 0).then_some(count)
	}

	/// Army pointers from the country save vector
	/// ([`candidates::country::ARMY_LIST`]). `None` means the header is
	/// malformed. An empty vector is an empty list. Static candidate;
	/// not a live-verified country field.
	pub fn save_army_pointers(&self, country: usize) -> Option<Vec<usize>> {
		save_record_pointers(
			&self.memory,
			country,
			candidates::country::ARMY_LIST,
			candidates::country::ARMY_LIST_END,
			candidates::country::ARMY_RECORD,
			candidates::country::ARMY_RECORD_PTR,
		)
	}

	/// Strength thousandths on the regiment save object
	/// ([`candidates::regiment::STRENGTH`]). `None` if the address
	/// cannot be read. `1000` is a real full-strength value, not a
	/// missing field.
	pub fn regiment_strength(&self, regiment: usize) -> Option<i32> {
		self.memory
			.read_i32(regiment.checked_add(candidates::regiment::STRENGTH)?)
	}

	/// Present unit type words from the combat-side vector
	/// ([`candidates::combat_side::IDENTITIES`]). Elements with a zero
	/// present byte or marker 7 are omitted, matching `UNIT_LOSSES`.
	/// `None` is a malformed header, never an empty stand. An empty
	/// vector is `Some` of an empty list. Types are not classified into
	/// infantry, cavalry and artillery, and morale is not read.
	pub fn present_unit_types(&self, side: usize) -> Option<Vec<i16>> {
		let begin_at = side.checked_add(candidates::combat_side::IDENTITIES)?;
		let end_at = side.checked_add(candidates::combat_side::UNIT_END)?;
		let begin = self.memory.read_qword(begin_at)? as usize;
		let end = self.memory.read_qword(end_at)? as usize;
		if begin == 0 && end == 0 {
			return Some(Vec::new());
		}
		if begin <= 0x1_0000 || end < begin {
			return None;
		}
		let bytes = end - begin;
		if !bytes.is_multiple_of(candidates::unit::STRIDE) {
			return None;
		}
		let count = bytes / candidates::unit::STRIDE;
		if count > 512 {
			return None;
		}
		let mut types = Vec::new();
		for index in 0..count {
			let element = begin + index * candidates::unit::STRIDE;
			let present = self
				.memory
				.read_u8(element.checked_add(candidates::unit::PRESENT)?)?;
			if present == 0 {
				continue;
			}
			let marker = self
				.memory
				.read_u8(element.checked_add(candidates::unit::MARKER)?)?;
			if marker == 7 {
				continue;
			}
			let low = self
				.memory
				.read_u8(element.checked_add(candidates::unit::TYPE)?)?;
			let high = self
				.memory
				.read_u8(element.checked_add(candidates::unit::TYPE + 1)?)?;
			types.push(i16::from_le_bytes([low, high]));
		}
		Some(types)
	}

	/// Infantry / cavalry / artillery amounts from the named-amount list
	/// on a combat-side object ([`candidates::combat_side::AMOUNT_LIST`]).
	///
	/// `None` means the walk failed or every non-`dice` name was
	/// unrecognized — the caller must not invent zeros. A null head is a
	/// real empty list and returns zeros. Current morale is not read;
	/// that field is not pinned.
	pub fn regiment_counts(&self, object: usize) -> Option<RegimentCounts> {
		let head_at = object.checked_add(candidates::combat_side::AMOUNT_LIST)?;
		let head = self.memory.read_qword(head_at)? as usize;
		if head == 0 {
			return Some(RegimentCounts::default());
		}
		if head <= 0x1_0000 {
			return None;
		}
		let mut entries = Vec::new();
		let mut node = head;
		for _ in 0..64 {
			let name = foch_runtime::dataplane::read_sso(
				&self.memory,
				node.checked_add(candidates::amount::NAME)?,
				foch_runtime::dataplane::is_name_byte,
			)?;
			let count = self
				.memory
				.read_i32(node.checked_add(candidates::amount::COUNT)?)?;
			entries.push((name, count));
			let next = self
				.memory
				.read_qword(node.checked_add(candidates::amount::NEXT)?)?;
			if next == 0 {
				return counts_from_named(&entries);
			}
			let next = next as usize;
			if next <= 0x1_0000 {
				return None;
			}
			node = next;
		}
		None
	}
}

/// How one named amount contributes to the three plane columns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegimentKind {
	Infantry,
	Cavalry,
	Artillery,
}

/// Candidate headcounts for columns 8–10. Not live-verified.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegimentCounts {
	pub infantry: i32,
	pub cavalry: i32,
	pub artillery: i32,
}

/// Classify one node name. `dice` is the literal `FUN_14021c980` skips.
/// Unit-type suffixes (`western_medieval_infantry`) count as that arm.
pub fn regiment_kind(name: &str) -> Option<RegimentKind> {
	let name = name.trim().to_ascii_lowercase();
	if name == "dice" || name.is_empty() {
		return None;
	}
	if name == "infantry" || name.ends_with("_infantry") {
		Some(RegimentKind::Infantry)
	} else if name == "cavalry" || name.ends_with("_cavalry") {
		Some(RegimentKind::Cavalry)
	} else if name == "artillery" || name.ends_with("_artillery") {
		Some(RegimentKind::Artillery)
	} else {
		None
	}
}

/// Fold named amounts. `None` when a non-dice name was present and none
/// of them classified — an unrecognized list must not become zeros.
pub fn counts_from_named(entries: &[(String, i32)]) -> Option<RegimentCounts> {
	let mut counts = RegimentCounts::default();
	let mut recognized = false;
	let mut other = false;
	for (name, amount) in entries {
		if name.eq_ignore_ascii_case("dice") {
			continue;
		}
		match regiment_kind(name) {
			Some(RegimentKind::Infantry) => {
				counts.infantry = counts.infantry.saturating_add(*amount);
				recognized = true;
			}
			Some(RegimentKind::Cavalry) => {
				counts.cavalry = counts.cavalry.saturating_add(*amount);
				recognized = true;
			}
			Some(RegimentKind::Artillery) => {
				counts.artillery = counts.artillery.saturating_add(*amount);
				recognized = true;
			}
			None => other = true,
		}
	}
	(recognized || !other).then_some(counts)
}

/// The pure extraction behind [`VerifiedGame::owner_country_index`] and
/// [`VerifiedGame::controller_index`]: bits 32..48 hold the country index.
fn save_record_pointers(
	memory: &foch_runtime::dataplane::Memory,
	object: usize,
	begin_at: usize,
	end_at: usize,
	stride: usize,
	pointer_at: usize,
) -> Option<Vec<usize>> {
	let begin = memory.read_qword(object.checked_add(begin_at)?)? as usize;
	let end = memory.read_qword(object.checked_add(end_at)?)? as usize;
	if begin == 0 && end == 0 {
		return Some(Vec::new());
	}
	if begin <= 0x1_0000 || end < begin || !stride.is_multiple_of(8) {
		return None;
	}
	let bytes = end - begin;
	if !bytes.is_multiple_of(stride) {
		return None;
	}
	let count = bytes / stride;
	if count > 512 {
		return None;
	}
	let mut pointers = Vec::with_capacity(count);
	for index in 0..count {
		let slot = begin + index * stride + pointer_at;
		let pointer = memory.read_qword(slot)? as usize;
		if pointer != 0 && pointer <= 0x1_0000 {
			return None;
		}
		if pointer != 0 {
			pointers.push(pointer);
		}
	}
	Some(pointers)
}

/// The pure extraction behind [`VerifiedGame::owner_country_index`] and
/// [`VerifiedGame::controller_index`]: bits 32..48 hold the country index.
pub fn country_index_from_packed(packed: u64) -> i32 {
	((packed >> 0x20) as u16) as i32
}

/// The pure bitmask check behind [`VerifiedGame::occupied_flags`]: both
/// bits of [`candidates::province::OCCUPIED_MASK`] must be set.
pub fn is_occupied_flags(flags: u8) -> bool {
	flags & candidates::province::OCCUPIED_MASK == candidates::province::OCCUPIED_MASK
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
	fn verified_game_accepts_both_embedded_1375_builds() {
		// The shared gate admits the stock Steam 1.37.5 build next to the
		// pinned AMDAC one; both come from the compiled-in profile registry.
		const STOCK_STEAM: &str =
			"9ad3efe1af169f40ee577f9dae5debbc87af6fb8b5450fb345ebf110dc4d771a";
		let memory = Memory;
		let pinned = foch_runtime::dataplane::pinned_exe_sha256();
		assert_ne!(pinned, STOCK_STEAM);
		assert!(VerifiedGame::new(memory, usize::MAX, pinned).is_some());
		assert!(VerifiedGame::new(memory, usize::MAX, STOCK_STEAM).is_some());
		// Mutations of the accepted digests are unknown builds, not keys.
		for digest in [
			STOCK_STEAM.to_ascii_uppercase(),
			format!("{STOCK_STEAM}0"),
			"9".repeat(64),
		] {
			assert!(
				VerifiedGame::new(memory, usize::MAX, &digest).is_none(),
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
		assert_eq!(game.controller_index(usize::MAX), None);
		assert_eq!(game.occupied_flags(usize::MAX), None);
		assert_eq!(game.province_mark(usize::MAX), None);
	}

	#[test]
	fn owner_index_reads_the_engine_window_of_the_packed_qword() {
		// High 32 bits hold the index; the engine truncates to a short.
		let packed = (0x0000_012c_u64 << 0x20) | 0xffff_ffff;
		assert_eq!(country_index_from_packed(packed), 0x012c);
	}

	#[test]
	fn occupied_flags_requires_both_mask_bits() {
		assert!(!is_occupied_flags(0x00));
		assert!(!is_occupied_flags(0x10));
		assert!(!is_occupied_flags(0x40));
		assert!(is_occupied_flags(candidates::province::OCCUPIED_MASK));
		assert!(is_occupied_flags(0xff));
	}

	#[cfg(windows)]
	#[test]
	fn verified_game_reads_controller_occupied_and_mark_from_province_buffer() {
		let mut words = vec![0_u64; Province::record_span() / 8];
		let base = words.as_mut_ptr() as *mut u8;
		// SAFETY: every offset is inside `words` (one province record),
		// which is validated by `candidates::tests`.
		unsafe {
			let mark = 0x1234_5678_u32.to_le_bytes();
			core::ptr::copy_nonoverlapping(mark.as_ptr(), base.add(candidates::province::MARK), 4);
			let controller = ((0x002a_u64 << 0x20) | 0xdead_beef).to_le_bytes();
			core::ptr::copy_nonoverlapping(
				controller.as_ptr(),
				base.add(candidates::province::CONTROLLER),
				8,
			);
			*base.add(candidates::province::FLAGS) = candidates::province::OCCUPIED_MASK | 0x01;
		}
		let expected = foch_runtime::dataplane::pinned_exe_sha256();
		let game = VerifiedGame::new(Memory, base as usize, expected)
			.expect("shared pinned digest must open the gate");
		let province = base as usize;
		assert_eq!(game.province_mark(province), Some(0x1234_5678));
		assert_eq!(game.controller_index(province), Some(0x002a));
		assert_eq!(game.occupied_flags(province), Some(true));

		// Flipping one bit of the occupied mask clears the occupied state.
		unsafe {
			*base.add(candidates::province::FLAGS) = 0x10;
		}
		assert_eq!(game.occupied_flags(province), Some(false));
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

	fn named(pairs: &[(&str, i32)]) -> Vec<(String, i32)> {
		pairs
			.iter()
			.map(|(name, count)| ((*name).to_string(), *count))
			.collect()
	}

	#[test]
	fn named_amounts_split_into_arms_and_refuse_an_unknown_list() {
		let counts = counts_from_named(&named(&[
			("dice", 99),
			("western_medieval_infantry", 10),
			("cavalry", 4),
			("artillery", 2),
		]))
		.expect("classified");
		assert_eq!(counts.infantry, 10);
		assert_eq!(counts.cavalry, 4);
		assert_eq!(counts.artillery, 2);
		assert_eq!(
			counts_from_named(&named(&[("dice", 5)])),
			Some(RegimentCounts::default())
		);
		assert!(counts_from_named(&named(&[("huskarls", 8)])).is_none());
	}

	#[cfg(windows)]
	#[test]
	fn regiment_counts_walk_a_local_amount_list() {
		let mut words = vec![0_u64; 0x80];
		let base = words.as_mut_ptr() as usize;
		let dice = base + 0x80;
		let infantry = base + 0x100;
		unsafe {
			write_sso(dice, "dice");
			write_i32(dice + candidates::amount::COUNT, 99);
			write_qword(dice + candidates::amount::NEXT, infantry as u64);
			write_sso(infantry, "infantry");
			write_i32(infantry + candidates::amount::COUNT, 10);
			write_qword(base + candidates::combat_side::AMOUNT_LIST, dice as u64);
			write_i32(base + candidates::combat_side::REGIMENT_COUNT, 1);
		}
		let expected = foch_runtime::dataplane::pinned_exe_sha256();
		let game = VerifiedGame::new(Memory, base, expected).expect("pinned digest opens the gate");
		let counts = game.regiment_counts(base).expect("walk");
		assert_eq!(counts.infantry, 10);
		assert_eq!(counts.cavalry, 0);
		assert_eq!(game.regiment_slot_count(base), Some(1));

		unsafe { write_qword(base + candidates::combat_side::AMOUNT_LIST, 0) };
		assert_eq!(game.regiment_counts(base), Some(RegimentCounts::default()));
	}

	#[cfg(windows)]
	#[test]
	fn present_unit_types_skip_absent_and_marker_seven() {
		let mut words = vec![0_u64; 0x80];
		let base = words.as_mut_ptr() as usize;
		let first = base + 0x200;
		let second = first + candidates::unit::STRIDE;
		unsafe {
			write_i16(first + candidates::unit::TYPE, 4);
			write_u8(first + candidates::unit::PRESENT, 1);
			write_i16(second + candidates::unit::TYPE, 9);
			write_u8(second + candidates::unit::MARKER, 7);
			write_u8(second + candidates::unit::PRESENT, 1);
			write_qword(base + candidates::combat_side::IDENTITIES, first as u64);
			write_qword(
				base + candidates::combat_side::UNIT_END,
				(second + candidates::unit::STRIDE) as u64,
			);
		}
		let expected = foch_runtime::dataplane::pinned_exe_sha256();
		let game = VerifiedGame::new(Memory, base, expected).expect("pinned digest opens the gate");
		assert_eq!(game.present_unit_types(base), Some(vec![4]));
		let record = base + 0x300;
		let army = base + 0x340;
		unsafe {
			write_qword(record + candidates::country::ARMY_RECORD_PTR, army as u64);
			write_qword(base + candidates::country::ARMY_LIST, record as u64);
			write_qword(
				base + candidates::country::ARMY_LIST_END,
				(record + candidates::country::ARMY_RECORD) as u64,
			);
			write_i32(army + candidates::regiment::STRENGTH, 750);
		}
		assert_eq!(game.save_army_pointers(base), Some(vec![army]));
		assert_eq!(game.regiment_strength(army), Some(750));
		unsafe {
			write_qword(base + candidates::combat_side::IDENTITIES, 0);
			write_qword(base + candidates::combat_side::UNIT_END, 0);
		}
		assert_eq!(game.present_unit_types(base), Some(Vec::new()));
	}

	#[cfg(windows)]
	unsafe fn write_sso(addr: usize, name: &str) {
		let base = addr as *mut u8;
		unsafe {
			core::ptr::copy_nonoverlapping(name.as_ptr(), base, name.len());
			let len = (name.len() as u64).to_le_bytes();
			core::ptr::copy_nonoverlapping(len.as_ptr(), base.add(0x10), 8);
			let cap = 0x0f_u64.to_le_bytes();
			core::ptr::copy_nonoverlapping(cap.as_ptr(), base.add(0x18), 8);
		}
	}

	#[cfg(windows)]
	unsafe fn write_i32(addr: usize, value: i32) {
		unsafe {
			core::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), addr as *mut u8, 4);
		}
	}

	#[cfg(windows)]
	unsafe fn write_u8(addr: usize, value: u8) {
		unsafe {
			*(addr as *mut u8) = value;
		}
	}

	#[cfg(windows)]
	unsafe fn write_i16(addr: usize, value: i16) {
		unsafe {
			core::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), addr as *mut u8, 2);
		}
	}

	#[cfg(windows)]
	unsafe fn write_qword(addr: usize, value: u64) {
		unsafe {
			core::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), addr as *mut u8, 8);
		}
	}
}
