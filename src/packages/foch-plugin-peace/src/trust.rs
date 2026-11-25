//! Ally-trust preservation for mixed peace deals.
//!
//! In a native one-direction peace, allies on the winning side lose trust
//! when the leader takes spoils without compensating them. A bilateral deal
//! changes the arithmetic: what the leader gives can offset what the allies
//! collectively lose, and a clause that directly benefits an ally (returning
//! their cores, releasing them from an unwanted war) is a gain, not a loss.
//!
//! The rule: an ally's trust is preserved when the deal's net value is
//! positive (the leader is not overreaching) and the ally's own share —
//! clauses whose receiver is that ally, minus their contribution to what
//! the leader demands — is non-negative.
//!
//! Static read of the pinned 1.37.5 image (sha256 `c9ec55a4…ae9f8e`, file
//! only): the treaty clone at `0x9a1040` jumps through the kind table at
//! `0x9a1d9c`. Kind 3 returns an object whose vtable is `0x1cd4ee0`; slot
//! `+0x98` is Execute `0x9a60e0`, and that function calls `CCountry::Annex`
//! (`0x367390`). RTTI names are not in the image, so `CCancelVassalTreaty`
//! is not a separate symbol here. Before one of those Annex calls, Execute
//! takes a side path only when byte `country+0x958` has bit `0x20` set on
//! the party loaded from `[rsi+0x38]` and clear on the party at
//! `[r14+0x14b0]`, then calls `0x943d00`. The same bit is tested by other
//! treaty executes. No trust immediate or trust symbol is on that path, so
//! [`preserves_trust`] stays the product rule rather than a transcription
//! of the native branch.

/// One ally's stake in a mixed deal: what they gain and what they lose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllyStake {
	/// Per-mille value of clauses whose receiver is this ally.
	pub gains: i32,
	/// Per-mille value this ally contributes to the leader's demands.
	pub losses: i32,
}

/// The deal-level net: what the leader demands minus what they give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DealNet {
	/// Per-mille: demands minus concessions. Positive = leader nets ahead.
	pub net: i32,
	/// True when any demand clause's receiver is an ally of the leader.
	pub ally_benefits: bool,
}

/// Decides whether an ally's trust should be preserved under a mixed deal.
///
/// Trust is preserved when both hold:
/// 1. The deal's net is non-negative: the leader is not taking more than
///    they give, so no ally is being short-changed by overreach.
/// 2. The ally's own share is non-negative: their gains meet or exceed
///    their losses. An ally who receives returned cores while contributing
///    nothing is clearly better off; an ally whose territory is demanded
///    without compensation is not.
pub fn preserves_trust(ally: &AllyStake, deal: &DealNet) -> bool {
	deal.net >= 0 && ally.gains >= ally.losses
}

/// Aggregates a set of ally stakes: trust is preserved only when every
/// ally individually satisfies [`preserves_trust`]. A single short-changed
/// ally still loses trust — the deal does not hide individual unfairness
/// behind aggregate benevolence.
pub fn preserves_all_allies(allies: &[AllyStake], deal: &DealNet) -> bool {
	allies.iter().all(|ally| preserves_trust(ally, deal))
}

/// The annex Execute side path at `0x9a6583`: bit `0x20` of `country+0x958`
/// is set for `primary` and clear for `other`. This is the native gate in
/// front of one `CCountry::Annex` call, not a trust amount.
pub fn annex_execute_side_path(primary_flag: u8, other_flag: u8) -> bool {
	(primary_flag & 0x20) != 0 && (other_flag & 0x20) == 0
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn positive_net_and_gainful_ally_preserves_trust() {
		let ally = AllyStake {
			gains: 20_000,
			losses: 5_000,
		};
		let deal = DealNet {
			net: 30_000,
			ally_benefits: true,
		};
		assert!(preserves_trust(&ally, &deal));
	}

	#[test]
	fn negative_net_loses_trust_even_for_gainful_ally() {
		let ally = AllyStake {
			gains: 10_000,
			losses: 0,
		};
		let deal = DealNet {
			net: -5_000,
			ally_benefits: true,
		};
		assert!(!preserves_trust(&ally, &deal));
	}

	#[test]
	fn positive_net_but_net_losing_ally_loses_trust() {
		let ally = AllyStake {
			gains: 3_000,
			losses: 8_000,
		};
		let deal = DealNet {
			net: 40_000,
			ally_benefits: false,
		};
		assert!(!preserves_trust(&ally, &deal));
	}

	#[test]
	fn exactly_balanced_ally_preserves_trust() {
		let ally = AllyStake {
			gains: 5_000,
			losses: 5_000,
		};
		let deal = DealNet {
			net: 0,
			ally_benefits: false,
		};
		assert!(preserves_trust(&ally, &deal));
	}

	#[test]
	fn one_short_changed_ally_breaks_it_for_everyone() {
		let gainful = AllyStake {
			gains: 10_000,
			losses: 0,
		};
		let short_changed = AllyStake {
			gains: 0,
			losses: 15_000,
		};
		let deal = DealNet {
			net: 50_000,
			ally_benefits: true,
		};
		assert!(!preserves_all_allies(&[gainful, short_changed], &deal));
	}

	#[test]
	fn all_gainful_allies_preserve_trust() {
		let allies = [
			AllyStake {
				gains: 10_000,
				losses: 2_000,
			},
			AllyStake {
				gains: 8_000,
				losses: 0,
			},
		];
		let deal = DealNet {
			net: 25_000,
			ally_benefits: true,
		};
		assert!(preserves_all_allies(&allies, &deal));
	}

	#[test]
	fn empty_ally_set_trivially_preserves_trust() {
		let deal = DealNet {
			net: -10_000,
			ally_benefits: false,
		};
		assert!(preserves_all_allies(&[], &deal));
	}

	#[test]
	fn annex_execute_side_path_requires_the_primary_bit_only() {
		assert!(annex_execute_side_path(0x20, 0));
		assert!(annex_execute_side_path(0x21, 0x1f));
		assert!(!annex_execute_side_path(0x20, 0x20));
		assert!(!annex_execute_side_path(0, 0x20));
		assert!(!annex_execute_side_path(0, 0));
	}
}
