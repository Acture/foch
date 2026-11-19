//! Static contract of the pinned EU4 1.37.5 Steam x64 image.
//!
//! Disassembled from `eu4.exe` sha256
//! `c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e`
//! without launching the process. Nothing here is an in-game observation.
//! Callers must keep treating AI acceptance and per-clause settlement as
//! unverified until a disposable campaign shows them.

/// Image slots in one offer's kind table.
///
/// The constructor compares the table length with immediate `0x28` and grows
/// or shrinks it to 40 buckets of `0x18` bytes. The clause clone compares the
/// kind with immediate `0x27` and returns null above that. These are layout
/// limits of this executable, not a list of vanilla treaty names. A scripted
/// treaty from `common/peace_treaties` has to occupy one of these slots; its
/// identity beyond the slot is the opaque target word.
pub const KIND_SLOTS: usize = 40;

/// A kind the pinned clone can construct. Kind `KIND_SLOTS` and above match
/// the clone's `ja` to `xor eax, eax`.
pub fn kind_slot_admitted(kind: u32) -> bool {
	(kind as usize) < KIND_SLOTS
}

/// Where a cached headline total (offer `+0xf0`) is read or replaced.
///
/// The rebuild at `0x978b20` writes the totals object at offer `+0xe8` after
/// calling every selected clause's `vtable+0x118` with `negate` immediate 0
/// and the offer's own direction byte at `+0x20`. That call replaces a signed
/// net total with the all-positive sum of the offer's direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreSite {
	/// Copy constructor `0x975f40` copies the dword and does not call rebuild.
	CopyConstructor,
	/// `GetCost` `0x979060` loads the dword and does not call rebuild.
	GetCost,
	/// Rebuild `0x978b20`.
	Rebuild,
	/// `CPeaceOffer::Execute` `0x976730` calls rebuild at `0x97696d` on the
	/// path where `0x97bd30` returns non-null, before the clause walk.
	Execute,
	/// Peace-view refresh `0x1309120` rebuilds the active draft at view
	/// `+0x48` or `+0x208` according to the direction byte at `+0x660`.
	ActiveDraftRefresh,
}

/// `true` when this site leaves a previously written net total in place.
///
/// `Execute` does not. The function `0x598ed0` that later calls `Execute` at
/// `0x59a6ea` also calls rebuild on its embedded offer at `+0x48` after two
/// lookups succeed; that call is not a separate site because `Execute` itself
/// rebuilds again. A different `GetCost` caller at `0x36e510` constructs a
/// new offer and never copies an existing one, so it cannot be observing the
/// player's net total. Which of these the accepting AI actually uses is not
/// named in the image and is not verified in game.
pub fn cached_net_survives(site: ScoreSite) -> bool {
	matches!(site, ScoreSite::CopyConstructor | ScoreSite::GetCost)
}

/// How a mixed clause is allowed to be described from this static read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClauseFamily {
	/// Kind 0. Its `Execute` at `0x9b4040` returns before doing work when the
	/// dword at clause `+0x30` is zero. The plugin's second concessions pass
	/// exists because the offer-direction executor is what pays this amount.
	/// Payment itself is not verified in game.
	Ducats,
	/// Kind 3. Vtable `0x1cd4ee0`, `Execute` `0x9a60e0`, which calls
	/// `CCountry::Annex`. The side path is the bit test in `trust`, not a
	/// subject-type enum.
	Annex,
	/// Kind 39. `Execute` `0xac9fa0` reads clause `+0x18` and the pointer at
	/// `+0x30`, then makes one virtual call. It does not test an offer
	/// direction byte in that body. It is not identified as a named vanilla
	/// treaty.
	TrailingSlot,
	/// Every other slot, including scripted `common/peace_treaties` entries
	/// and subject types reached through `create_subject` / `copy_from`.
	/// The target word is not decoded.
	Opaque,
}

/// Static reads do not prove that a mixed clause settles in the direction
/// its own parties name. Live settlement is still required for every family.
pub fn mixed_settlement_verified_in_game(_family: ClauseFamily) -> bool {
	false
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_table_admits_every_slot_and_rejects_the_clone_default() {
		assert!(kind_slot_admitted(0));
		assert!(kind_slot_admitted((KIND_SLOTS as u32) - 1));
		assert!(!kind_slot_admitted(KIND_SLOTS as u32));
		assert!(!kind_slot_admitted(u32::MAX));
	}

	#[test]
	fn only_a_copy_and_getcost_keep_a_cached_net_total() {
		assert!(cached_net_survives(ScoreSite::CopyConstructor));
		assert!(cached_net_survives(ScoreSite::GetCost));
		assert!(!cached_net_survives(ScoreSite::Rebuild));
		assert!(!cached_net_survives(ScoreSite::Execute));
		assert!(!cached_net_survives(ScoreSite::ActiveDraftRefresh));
	}

	#[test]
	fn no_clause_family_is_marked_live_verified() {
		for family in [
			ClauseFamily::Ducats,
			ClauseFamily::Annex,
			ClauseFamily::TrailingSlot,
			ClauseFamily::Opaque,
		] {
			assert!(!mixed_settlement_verified_in_game(family));
		}
	}
}
