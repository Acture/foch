//! CANDIDATE offsets — statically harvested, not yet live-verified.
//!
//! Everything here was derived statically from the pinned 1.37.5
//! binary (its province serializers, ledger getters, and the army
//! script setter). None of it has passed the shared dataplane's live
//! verification yet, so every read through this module is
//! `Option`-gated by the caller and a `None` must stay `None`: never
//! substitute zero to fill the plane and present it as complete live
//! state. When a candidate passes live verification it graduates into
//! the shared dataplane views and leaves this module.
//!
//! The candidates pin to the same executable the shared data plane does:
//! gate use with [`foch_runtime::dataplane::supports_build`] against
//! [`foch_runtime::dataplane::pinned_exe_sha256`] — this crate carries no
//! hash of its own.

/// Verified province fields are read through the shared dataplane. The
/// values below extend that view for fields it does not carry yet. Same
/// binary, same conventions (engine fixed point is ÷1000).
pub mod province {
	/// Dword compared with [`super::country::MARK`] to identify a
	/// country's capital province.
	pub const MARK: usize = 0x20;
	/// Base production, thousandths — the third ledger getter
	/// (FUN_140a0dff0, LEDGER_PRODUCTION). Tax and manpower already
	/// live in the shared layout.
	pub const BASE_PRODUCTION: usize = 0x3e8;
	/// Fort level (FUN_14133c150, serializes `fort_level`).
	pub const FORT_LEVEL: usize = 0x2de0;
	/// Unrest (sp_unrest in the same serializer).
	pub const UNREST: usize = 0x2de4;
	/// Devastation (sp_devastation).
	pub const DEVASTATION: usize = 0x42c;
	/// Packed owner qword; bits 32..48 hold the **country index** (the
	/// engine reads `(short)(packed >> 0x20)`), NOT the tag — the tag
	/// decodes through the country registry.
	pub const OWNER_INDEX: usize = 0x468;
	/// Packed controller qword; same bit layout as [`OWNER_INDEX`]
	/// (bits 32..48 hold the controlling country index).
	pub const CONTROLLER: usize = 0x470;
	/// Cores / trade-company region vector (begin +0x28, end +0x30 on
	/// the pointee).
	pub const CORES: usize = 0x8a8;
	/// Province flags byte; both bits of [`OCCUPIED_MASK`] set means
	/// the province is counted as occupied.
	pub const FLAGS: usize = 0x958;
	/// Both of these bits set in [`FLAGS`] means the province is
	/// counted as occupied.
	pub const OCCUPIED_MASK: u8 = 0x50;
	/// Next pointer on a country's province linked-list node (`i32`
	/// province index sits at `+0`, next pointer at `+0x10`).
	pub const LINK_NEXT: usize = 0x10;
}

/// Candidate fields on `CCountry` beyond the verified country view
/// ([`foch_runtime::dataplane::CountryView::treasury`] and the packed tag).
pub mod country {
	/// Current manpower on the country object (`SetManpower`).
	pub const MANPOWER: usize = 0xdb8;
	/// Manpower cap read by `CalcManpowerIncrease`.
	pub const MANPOWER_MAX: usize = 0xdbc;
	/// Province linked-list head on a country. Each node holds an `i32`
	/// province index at `+0` and the next pointer at
	/// [`super::province::LINK_NEXT`].
	pub const PROVINCES: usize = 0x1348;
	/// Dword compared with [`super::province::MARK`].
	pub const MARK: usize = 0x14b8;
	/// Overlord country id read while resolving the other side of a
	/// province.
	pub const OVERLORD: usize = 0x18f0;
	/// Save-side army vector begin. End is [`ARMY_LIST_END`]. The writer
	/// at `0x140d6c7d0` emits token `army` (`0x28a0`) once per 0x10-byte
	/// record and takes the army pointer from record+[`ARMY_RECORD_PTR`].
	/// The same function emits `navy` (`0x28a1`) from [`NAVY_LIST`].
	/// There is no RTTI name on that function; the army/navy pair is the
	/// evidence it is the country military save. Static only.
	pub const ARMY_LIST: usize = 0x50;
	/// End pointer of [`ARMY_LIST`]. Count is `(end - begin) / 0x10`.
	pub const ARMY_LIST_END: usize = 0x58;
	/// Bytes per army record in [`ARMY_LIST`].
	pub const ARMY_RECORD: usize = 0x10;
	/// Pointer inside one army record.
	pub const ARMY_RECORD_PTR: usize = 0x8;
	/// Navy vector begin, sibling of [`ARMY_LIST`]. End at +0x70.
	pub const NAVY_LIST: usize = 0x68;
	/// End pointer of [`NAVY_LIST`].
	pub const NAVY_LIST_END: usize = 0x70;
}

/// Save container that emits `regiment` (`0x28a2`) or `ship` (`0x28a3`).
///
/// `FUN_140d3d750` walks a pointer vector at +0x20/+0x28 (stride 8) and
/// calls the element's virtual save. This is not the AssignLeader army
/// object: that object's id dword sits at `army::ID` (`+0x24`), which
/// overlaps a qword begin pointer. Static only.
pub mod unit_container {
	/// Begin pointer. End is [`REGIMENTS_END`].
	pub const REGIMENTS: usize = 0x20;
	/// End pointer of [`REGIMENTS`].
	pub const REGIMENTS_END: usize = 0x28;
	/// Element stride. Each element is one pointer.
	pub const STRIDE: usize = 8;
}

/// One regiment as saved under token `strength` (`0x28a9`).
///
/// The writer loads a dword at +0xb0 and skips the token when the value
/// is 1000, the full-strength thousandths sentinel. The function is not
/// RTTI-named. The `morale` token (`0x28a8`) is written from a combat
/// object at +0x50 (next to attacker/defender/location) and from a
/// smaller object at +0x18; neither is this strength object, so regiment
/// morale stays unpinned.
pub mod regiment {
	/// Strength in thousandths. `1000` means full and is omitted on save.
	pub const STRENGTH: usize = 0xb0;
	/// Value the saver treats as "do not write strength".
	pub const FULL_STRENGTH: i32 = 1000;
}

/// Candidate fields on a war object (`WAR_SCORE_TOTAL` / `WAR_SCORE_SIDE`).
pub mod war {
	/// First participant list on the war (`begin` at `+0x48`, `end` at `+0x50`).
	pub const SIDE_A: usize = 0x48;
	/// Second participant list (`begin` at `+0x78`, `end` at `+0x80`).
	pub const SIDE_B: usize = 0x78;
	/// Clamped warscore total stored by the warscore reporter.
	pub const SCORE_VALUE: usize = 0x190;
}

/// Fields of the world/game context next to the shared layout's
/// province array.
pub mod world {
	/// Province-id table vector (begin at the offset, end +8); the army
	/// relocation setter bounds-checks ids against `(end − begin) / 8`.
	pub const PROVINCE_IDS: usize = 0x1c90;
	/// Province-template array (`SetMostCommonTerrain` walker), stride
	/// [`template::STRIDE]; a template's terrain object pointer sits at
	/// [`template::TERRAIN`] and the terrain NAME is an MSVC SSO string.
	pub const TEMPLATES: usize = 0x6c0;
}

/// One province template (`world::TEMPLATES` array element).
pub mod template {
	pub const STRIDE: usize = 0x230;
	/// Terrain object pointer.
	pub const TERRAIN: usize = 0x30;
	/// MSVC SSO terrain name on the terrain object (size/capacity at
	/// +0x130, standard layout).
	pub const NAME: usize = 0x118;
	/// int printed beside the terrain in SetMostCommonTerrain — the
	/// province id link, still to confirm.
	pub const PROVINCE_ID: usize = 0x150;
}

/// `FUN_14017a3d0` is a budget-category name switch (`cmp ecx, 9`), not a
/// province army vector. Case 4 returns the string `armies`; the other
/// cases are `advisors`, `savings`, `colonists`, `missionaries`, `navies`,
/// `subsidies`, `forts`, `corruption`, `states`. Do not read a province
/// field from it.
///
/// Composition lives on the combat-side object `combat.cpp` walks
/// (`FUN_140221750` at combat.cpp:0x727 calls `FUN_14021c980`; battle
/// setup `FUN_140237f90` calls `FUN_140237120` / `FUN_14021c7a0` on the
/// two sides at `+0x20` / `+0x28`). A province's stacks are still armies
/// whose [`army::LOCATION`] points at the province. `FUN_1401a5e50`
/// stores the country pointer on the army and does not link the army
/// into a country-owned vector, so the global army-array root stays
/// unpinned. Current morale is still unpinned: `FUN_1401dd600` caches a
/// dword on a different object. These offsets stay candidates until a
/// live read confirms them.
pub mod combat_side {
	/// Linked list of named amounts. Head pointer. Node layout is [`amount`].
	/// `FUN_14021c980` walks `node = [rcx+0x50]`, `next = [node+0x50]`.
	pub const AMOUNT_LIST: usize = 0x50;
	/// Pointer to the regiment-pointer array (`[object+0x190]`).
	pub const REGIMENTS: usize = 0x190;
	/// Dword count of pointers at [`REGIMENTS`] (`[object+0x19c]`).
	pub const REGIMENT_COUNT: usize = 0x19c;
	/// List `FUN_140d722c0` counts when [`ENGAGED`] is set. Battle setup
	/// adds that count into the per-side totals.
	pub const ENGAGED_LIST: usize = 0xe8;
	/// Bool gate for [`ENGAGED_LIST`] (`[side+0x169]`).
	pub const ENGAGED: usize = 0x169;
	/// Begin of the 0x20-byte unit vector `UNIT_LOSSES` (`FUN_1402209c0`)
	/// walks. End is [`UNIT_END`]. `BATTLE_RESULTS` (`FUN_140237d50`)
	/// passes each side (`battle+0x20` / `battle+0x28`) to that walk.
	/// Element layout is [`unit`].
	pub const IDENTITIES: usize = 0x100;
	/// End pointer of the unit vector, one qword after [`IDENTITIES`].
	pub const UNIT_END: usize = 0x108;
	/// Dword count that selects `FUN_14021c7a0`'s early identity return.
	pub const IDENTITY_COUNT: usize = 0xf8;
}

/// One element of the combat-side unit vector ([`combat_side::IDENTITIES`],
/// stride [`STRIDE`]). `UNIT_LOSSES` skips an element whose present byte
/// is zero or whose marker byte is 7, then sign-extends the type word.
/// Three further type ids compared against globals are also skipped;
/// those globals were not pinned, so a reader must not invent them.
/// This is not yet the infantry/cavalry/artillery split, and it is not
/// a current-morale field.
pub mod unit {
	pub const STRIDE: usize = 0x20;
	/// Packed identity qword. Bits 32..48 are a country index.
	pub const IDENTITY: usize = 0x10;
	/// Signed type word (`movsx eax, word ptr [elem+0x14]`).
	pub const TYPE: usize = 0x14;
	/// Marker byte. `UNIT_LOSSES` skips the element when this is 7.
	pub const MARKER: usize = 0x16;
	/// Present byte. Zero means the element is skipped.
	pub const PRESENT: usize = 0x17;
}

/// One node of [`combat_side::AMOUNT_LIST`].
pub mod amount {
	/// Next pointer. Zero ends the list.
	pub const NEXT: usize = 0x50;
	/// MSVC SSO name. `FUN_14021c980` compares it with the literal `dice`.
	pub const NAME: usize = 0x0;
	/// Second SSO on the node.
	pub const NAME_2: usize = 0x20;
	/// Dword added into the sum unless the name is `dice`.
	pub const COUNT: usize = 0x40;
	/// Flag byte beside the count.
	pub const FLAG: usize = 0x44;
}

/// Candidate fields of one army stack object (from the army script/save
/// setter `FUN_1401a5e50`, `AssignLeader`, and battle setup
/// `FUN_140237f90`).
pub mod army {
	/// Combat width cached on the army stack object.
	pub const COMBAT_WIDTH: usize = 0x1c;
	/// Army id dword (`[rcx+0x24]` in `AssignLeader`).
	pub const ID: usize = 0x24;
	/// Leader pointer (`[rcx+0xd8]` in `AssignLeader`).
	pub const LEADER: usize = 0xd8;
	/// Seven consecutive combat-stat dwords (each ÷1000) cached on the
	/// army stack object and read by battle setup (`FUN_140237f90`).
	pub const TACTICS: usize = 0x118;
	pub const DISCIPLINE: usize = 0x11c;
	pub const INF_FIRE: usize = 0x120;
	pub const INF_SHOCK: usize = 0x124;
	pub const CAV_FIRE: usize = 0x128;
	pub const ART_FIRE: usize = 0x12c;
	pub const ART_SHOCK: usize = 0x130;
	/// Country pointer. `FUN_1401a5e50` stores the registry lookup at
	/// `0x1401a6301` (`mov [rdi+0x21c], rcx`). The country-side army
	/// list is the save vector [`super::country::ARMY_LIST`], not a
	/// write in this setter.
	pub const COUNTRY: usize = 0x21c;
	/// Location: province pointer `id * 0x2e10 + *(world+0x1ca8)`, stored
	/// at `0x1401a603d` when save key `0x30fe` is applied.
	pub const LOCATION: usize = 0x228;
	/// Nested save object for key `0x30ff` (`lea rcx, [army+0x230]` then
	/// a virtual writer). Not identified as the regiment vector: the
	/// literals `regiment`, `morale`, and `strength` have no code xref.
	pub const SAVE_CHILD_A: usize = 0x230;
	/// Nested save object for the next key, `0x3100`, at `army+0x234`.
	/// Same limit as [`SAVE_CHILD_A`].
	pub const SAVE_CHILD_B: usize = 0x234;
	/// Region pointer, `index × 0x58` into the region table.
	pub const REGION: usize = 0x250;
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn candidate_offsets_stay_inside_one_province_stride() {
		let span = foch_runtime::dataplane::Province::record_span();
		for offset in [
			province::MARK,
			province::BASE_PRODUCTION,
			province::DEVASTATION,
			province::OWNER_INDEX,
			province::CONTROLLER,
			province::CORES,
			province::FLAGS,
			province::FORT_LEVEL,
			province::UNREST,
		] {
			assert!(offset + 8 <= span);
		}
		const {
			assert!(province::LINK_NEXT.is_multiple_of(8));
			assert!(province::LINK_NEXT >= 4);
			assert!(province::OCCUPIED_MASK != 0);
		}
	}

	#[test]
	fn country_war_and_army_candidate_offsets_have_valid_alignment_and_spacing() {
		const {
			assert!(country::MANPOWER + 4 == country::MANPOWER_MAX);
			assert!(country::PROVINCES.is_multiple_of(8));
			assert!(country::MARK.is_multiple_of(4));
			assert!(country::OVERLORD.is_multiple_of(8));
			assert!(country::ARMY_LIST.is_multiple_of(8));
			assert!(country::ARMY_LIST_END == country::ARMY_LIST + 8);
			assert!(country::ARMY_RECORD == 0x10);
			assert!(country::ARMY_RECORD_PTR < country::ARMY_RECORD);
			assert!(country::NAVY_LIST == country::ARMY_LIST + 0x18);
			assert!(country::NAVY_LIST_END == country::NAVY_LIST + 8);
			assert!(unit_container::REGIMENTS_END == unit_container::REGIMENTS + 8);
			assert!(unit_container::STRIDE == 8);
			assert!(regiment::STRENGTH == 0xb0);
			assert!(regiment::FULL_STRENGTH == 1000);

			assert!(war::SIDE_A.is_multiple_of(8));
			assert!(war::SIDE_B.is_multiple_of(8));
			assert!(war::SIDE_A + 16 <= war::SIDE_B);
			assert!(war::SCORE_VALUE.is_multiple_of(4));

			assert!(army::COMBAT_WIDTH.is_multiple_of(4));
			assert!(army::ID.is_multiple_of(4));
			assert!(army::LEADER.is_multiple_of(8));
			assert!(army::LOCATION == 0x228);
			assert!(army::SAVE_CHILD_A + 4 == army::SAVE_CHILD_B);
			assert!(army::COUNTRY + 0x14 == army::SAVE_CHILD_A);

			assert!(combat_side::AMOUNT_LIST.is_multiple_of(8));
			assert!(combat_side::REGIMENTS.is_multiple_of(8));
			assert!(combat_side::REGIMENT_COUNT.is_multiple_of(4));
			assert!(combat_side::ENGAGED_LIST.is_multiple_of(8));
			assert!(combat_side::IDENTITIES.is_multiple_of(8));
			assert!(combat_side::UNIT_END == combat_side::IDENTITIES + 8);
			assert!(combat_side::IDENTITY_COUNT.is_multiple_of(4));
			assert!(unit::STRIDE == 0x20);
			assert!(unit::TYPE + 2 == unit::MARKER);
			assert!(unit::MARKER + 1 == unit::PRESENT);
			assert!(unit::PRESENT < unit::STRIDE);
			assert!(amount::COUNT.is_multiple_of(4));
			assert!(amount::NEXT.is_multiple_of(8));
			assert!(amount::NAME < amount::NAME_2);
			assert!(amount::NAME_2 < amount::COUNT);
		}
		let combat_stats = [
			army::TACTICS,
			army::DISCIPLINE,
			army::INF_FIRE,
			army::INF_SHOCK,
			army::CAV_FIRE,
			army::ART_FIRE,
			army::ART_SHOCK,
		];
		for pair in combat_stats.windows(2) {
			assert_eq!(pair[0] + 4, pair[1]);
		}
	}

	#[test]
	fn the_shared_identity_is_self_consistent() {
		// Consistency of the shared gate only. The REAL rejection
		// boundary — actual host digest in, walk only after
		// supports_build — is `crate::view::VerifiedGame`; this test
		// is not host wiring and does not verify any candidate.
		let expected = foch_runtime::dataplane::pinned_exe_sha256();
		assert!(foch_runtime::dataplane::supports_build(expected));
	}
}
