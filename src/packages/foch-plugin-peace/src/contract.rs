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

/// First 15 bytes of `CPeaceOffer` cost rebuild `0x978b20`.
///
/// Three `mov [rsp+disp8], r64` home-slot saves. None is RIP-relative and
/// none transfers control, so the host's `steal_len` accepts exactly this
/// window (`foch-runtime-host`'s prologue test locks that). The following
/// instruction is `push rdi`; the offer pointer arrives in `rcx`.
pub const REBUILD_PROLOGUE: &[u8] = &[
	0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x6c, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18,
];

/// Bytes the rebuild hook steals. Equal to [`REBUILD_PROLOGUE`].
pub const REBUILD_STEAL: usize = REBUILD_PROLOGUE.len();

/// How a selected clause's parties sit relative to the offer's own pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartyRelation {
	/// Clause giver and receiver are the offer's giver and receiver.
	Same,
	/// Clause giver and receiver are the offer's pair swapped.
	Swapped,
	/// A third party, or an offer whose two parties are not a real pair.
	Other,
}

/// Classifies one clause against the offer that contains it.
///
/// A one-direction offer's own terms are [`PartyRelation::Same`]. A clause
/// cloned from the other direction keeps the parties it was built with, so
/// it is [`PartyRelation::Swapped`]. Returning a core to someone who is not
/// either negotiator is [`PartyRelation::Other`] and must not be treated as
/// the give side.
pub fn party_relation(
	offer_giver: u64,
	offer_receiver: u64,
	clause_giver: u64,
	clause_receiver: u64,
) -> PartyRelation {
	if offer_giver == 0 || offer_receiver == 0 || offer_giver == offer_receiver {
		return PartyRelation::Other;
	}
	if clause_giver == offer_giver && clause_receiver == offer_receiver {
		PartyRelation::Same
	} else if clause_giver == offer_receiver && clause_receiver == offer_giver {
		PartyRelation::Swapped
	} else {
		PartyRelation::Other
	}
}

/// Indexes of the give-side clauses to reprice after a vanilla rebuild.
///
/// `direction == 0` is the concession offer; any other byte is the demand
/// offer, matching the pinned image's direction byte at offer `+0x20`. The
/// vanilla rebuild has already added every selected clause with `negate = 0`.
/// Each returned clause needs two `negate = 1` accumulator calls: the first
/// cancels that positive add, the second subtracts it, which is the send
/// path's existing net correction.
///
/// Returns `None` unless the offer has at least one [`PartyRelation::Same`]
/// clause and one [`PartyRelation::Swapped`] clause. A one-direction offer
/// therefore stays exactly as the vanilla rebuild left it.
pub fn mixed_give_indexes(direction: u8, relations: &[PartyRelation]) -> Option<Vec<usize>> {
	let same = relations.contains(&PartyRelation::Same);
	let swapped = relations.contains(&PartyRelation::Swapped);
	if !same || !swapped {
		return None;
	}
	let give_is_swapped = direction != 0;
	Some(
		relations
			.iter()
			.enumerate()
			.filter_map(|(index, relation)| {
				let give = if give_is_swapped {
					*relation == PartyRelation::Swapped
				} else {
					*relation == PartyRelation::Same
				};
				give.then_some(index)
			})
			.collect(),
	)
}

/// Where a clause-family execute reads direction, from the pinned image only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticDirectionRead {
	/// Rebuild passes offer `+0x20` to every selected clause. The byte at
	/// clause `+0x20` is the selected flag, not a direction.
	OfferDirectionIntoAccumulator,
	/// Kind 0's `vtable+0x98` (`0x9b4040`) returns when clause `+0x30` is
	/// zero and does not read an offer direction byte in that body.
	DucatsClauseAmount,
	/// Kind 39's `vtable+0x98` (`0xac9fa0`) reads clause `+0x18` and the
	/// pointer at clause `+0x30`. Its `vtable+0xF0` is the shared stub
	/// `0xe66b0`, which only stores zero through its second argument.
	ScriptedClauseFields,
	/// Kind 3's `vtable+0xF0` (`0x9a7070`) compares the receiver argument
	/// `Execute` copied from offer `+0x18` with the clause's own `+0x18`.
	AnnexOfferReceiver,
	/// `Execute_CedeProvinces` (`0x990ed0`) saves its third argument and
	/// never reads that save back. The province move it calls onward was
	/// not traced to an offer-direction byte.
	CedeLogIgnoresSavedOffer,
	/// The string `war_reparations` is in the image. No execute was tied to it.
	WarReparationsUnidentified,
}

/// None of the direction reads above were observed in a running game.
pub fn direction_read_verified_in_game(_read: StaticDirectionRead) -> bool {
	false
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
	fn the_rebuild_prologue_is_fifteen_bytes_of_home_slot_saves() {
		assert_eq!(REBUILD_PROLOGUE.len(), 15);
		assert_eq!(REBUILD_STEAL, 15);
		assert!(profile_site_starts_with_the_hook_window());
	}

	fn profile_site_starts_with_the_hook_window() -> bool {
		crate::profile::REBUILD_COST
			.original
			.starts_with(REBUILD_PROLOGUE)
	}

	#[test]
	fn a_one_direction_offer_is_not_repriced() {
		assert_eq!(
			mixed_give_indexes(1, &[PartyRelation::Same, PartyRelation::Same]),
			None
		);
		assert_eq!(
			mixed_give_indexes(0, &[PartyRelation::Same, PartyRelation::Other]),
			None
		);
		assert_eq!(mixed_give_indexes(0, &[PartyRelation::Swapped]), None);
		assert_eq!(mixed_give_indexes(1, &[]), None);
	}

	#[test]
	fn a_mixed_offer_reprices_only_its_give_side() {
		let relations = [
			PartyRelation::Same,
			PartyRelation::Swapped,
			PartyRelation::Other,
		];
		assert_eq!(mixed_give_indexes(1, &relations), Some(vec![1]));
		assert_eq!(mixed_give_indexes(0, &relations), Some(vec![0]));
	}

	#[test]
	fn swapped_parties_are_not_confused_with_a_third_country() {
		assert_eq!(party_relation(10, 20, 10, 20), PartyRelation::Same);
		assert_eq!(party_relation(10, 20, 20, 10), PartyRelation::Swapped);
		assert_eq!(party_relation(10, 20, 10, 30), PartyRelation::Other);
		assert_eq!(party_relation(10, 10, 10, 10), PartyRelation::Other);
		assert_eq!(party_relation(0, 20, 20, 0), PartyRelation::Other);
	}

	#[test]
	fn no_direction_read_is_marked_live_verified() {
		for read in [
			StaticDirectionRead::OfferDirectionIntoAccumulator,
			StaticDirectionRead::DucatsClauseAmount,
			StaticDirectionRead::ScriptedClauseFields,
			StaticDirectionRead::AnnexOfferReceiver,
			StaticDirectionRead::CedeLogIgnoresSavedOffer,
			StaticDirectionRead::WarReparationsUnidentified,
		] {
			assert!(!direction_read_verified_in_game(read));
		}
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
