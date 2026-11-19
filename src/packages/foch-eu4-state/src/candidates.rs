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

/// Candidate fields on `CCountry` beyond the verified
/// [`foch_runtime::dataplane::layout::country`] fields (`TAG`, `TREASURY`).
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
	/// int printed beside the terrain in SetMostCommonTerrain — the
	/// province id link, still to confirm.
	pub const PROVINCE_ID: usize = 0x150;
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
	/// Country pointer (registry-resolved).
	pub const COUNTRY: usize = 0x21c;
	/// Location: a province pointer into the shared layout's array.
	pub const LOCATION: usize = 0x228;
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

			assert!(war::SIDE_A.is_multiple_of(8));
			assert!(war::SIDE_B.is_multiple_of(8));
			assert!(war::SIDE_A + 16 <= war::SIDE_B);
			assert!(war::SCORE_VALUE.is_multiple_of(4));

			assert!(army::COMBAT_WIDTH.is_multiple_of(4));
			assert!(army::ID.is_multiple_of(4));
			assert!(army::LEADER.is_multiple_of(8));
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
