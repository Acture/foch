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
	/// Cores / trade-company region vector (begin +0x28, end +0x30 on
	/// the pointee).
	pub const CORES: usize = 0x8a8;
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

/// Fields of one army (from the army script/save setter
/// FUN_1401a5e50). The verified combat-modifier block (+0x118..0x130)
/// lives in foch-eu4-decide's `sites` table, not here.
pub mod army {
	/// Location: a province pointer into the shared layout's array.
	pub const LOCATION: usize = 0x228;
	/// Country pointer (registry-resolved).
	pub const COUNTRY: usize = 0x21c;
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
			province::BASE_PRODUCTION,
			province::FORT_LEVEL,
			province::UNREST,
			province::DEVASTATION,
			province::OWNER_INDEX,
			province::CORES,
		] {
			assert!(offset + 8 <= span);
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
