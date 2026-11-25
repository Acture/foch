//! One native peace action carries one offer. A mixed submission clones the
//! inactive side's selected terms into the active offer, lets the original
//! submission copy that merged offer, then restores the editor's selection.

use crate::composition::{self, Clause, Draft, Side};

/// Offsets of the two directional drafts inside the native peace view.
pub const RECEIVE_OFFER: usize = 0x48;
pub const GIVE_OFFER: usize = 0x208;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
	UnrelatedParties,
	InvalidClause,
	DuplicateClause,
	NothingToMerge,
}

/// What the send path needs to merge one negotiation's two directions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixedSend {
	/// The active side's own selected terms, restored after submission.
	pub restore: Vec<Clause>,
	/// The inactive side's selected terms cloned into the active offer.
	pub merged: Vec<Clause>,
	pub active: Side,
}

pub fn offer_offset(side: Side) -> usize {
	match side {
		Side::Receive => RECEIVE_OFFER,
		Side::Give => GIVE_OFFER,
	}
}

/// A clause's identity without its native address, so a submitted term can
/// be matched against the terms an executed offer actually carried.
pub type ClauseKey = (u32, u64);

pub fn clause_key(clause: &Clause) -> ClauseKey {
	(clause.kind, clause.target)
}

/// The sent give-side keys that the executed offer no longer carries.
pub fn missing_give_keys(executed: &[Clause], sent: &[ClauseKey]) -> Vec<ClauseKey> {
	let present: std::collections::BTreeSet<ClauseKey> = executed.iter().map(clause_key).collect();
	sent.iter()
		.copied()
		.filter(|key| !present.contains(key))
		.collect()
}

/// `active` is the editor tab the submission is sent from; the other side's
/// terms are merged into it. Both drafts must be non-empty and pass
/// `composition::compose`, so one clause identity is never sent twice.
pub fn plan(receive: &Draft, give: &Draft, active: Side) -> Result<MixedSend, PlanError> {
	composition::compose(receive, give).map_err(|error| match error {
		composition::ComposeError::UnrelatedParties => PlanError::UnrelatedParties,
		composition::ComposeError::InvalidClause => PlanError::InvalidClause,
		composition::ComposeError::DuplicateClause => PlanError::DuplicateClause,
	})?;
	let (active_draft, inactive_draft) = match active {
		Side::Receive => (receive, give),
		Side::Give => (give, receive),
	};
	if active_draft.selected.is_empty() || inactive_draft.selected.is_empty() {
		return Err(PlanError::NothingToMerge);
	}
	Ok(MixedSend {
		restore: active_draft.selected.clone(),
		merged: inactive_draft.selected.clone(),
		active,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	fn clause(address: usize, kind: u32, target: u64) -> Clause {
		Clause {
			address,
			kind,
			target,
		}
	}

	fn drafts() -> (Draft, Draft) {
		(
			Draft {
				giver: 2,
				receiver: 1,
				selected: vec![clause(0x100, 3, 111), clause(0x180, 5, 7)],
			},
			Draft {
				giver: 1,
				receiver: 2,
				selected: vec![clause(0x200, 0, 0), clause(0x280, 3, 1769)],
			},
		)
	}

	#[test]
	fn offer_offsets_are_the_native_view_layout() {
		assert_eq!(offer_offset(Side::Receive), 0x48);
		assert_eq!(offer_offset(Side::Give), 0x208);
	}

	#[test]
	fn merges_the_inactive_side_into_the_active_offer() {
		let (receive, give) = drafts();
		let receiving = plan(&receive, &give, Side::Receive).unwrap();
		assert_eq!(receiving.active, Side::Receive);
		assert_eq!(receiving.restore, receive.selected);
		assert_eq!(receiving.merged, give.selected);
		let giving = plan(&receive, &give, Side::Give).unwrap();
		assert_eq!(giving.active, Side::Give);
		assert_eq!(giving.restore, give.selected);
		assert_eq!(giving.merged, receive.selected);
	}

	#[test]
	fn a_single_direction_has_nothing_to_merge_from_either_tab() {
		let (mut receive, give) = drafts();
		receive.selected.clear();
		assert_eq!(
			plan(&receive, &give, Side::Receive),
			Err(PlanError::NothingToMerge)
		);
		assert_eq!(
			plan(&receive, &give, Side::Give),
			Err(PlanError::NothingToMerge)
		);
	}

	#[test]
	fn unrelated_drafts_are_refused_before_any_merge() {
		let (receive, mut give) = drafts();
		give.receiver = 3;
		for side in [Side::Receive, Side::Give] {
			assert_eq!(
				plan(&receive, &give, side),
				Err(PlanError::UnrelatedParties)
			);
		}
	}

	#[test]
	fn duplicated_clause_ownership_is_refused() {
		let (receive, mut give) = drafts();
		give.selected[0] = clause(0x100, 3, 111);
		assert_eq!(
			plan(&receive, &give, Side::Receive),
			Err(PlanError::DuplicateClause)
		);
	}

	#[test]
	fn missing_give_keys_report_only_what_execution_lost() {
		let (receive, give) = drafts();
		let sent: Vec<ClauseKey> = give.selected.iter().map(clause_key).collect();
		let mut executed = receive.selected.clone();
		executed.push(clause(0x900, 0, 0));
		// One give term survived, one was dropped.
		let mut survivors = give.selected.clone();
		survivors.pop();
		executed.extend(survivors);
		assert_eq!(
			missing_give_keys(&executed, &sent),
			vec![clause_key(&give.selected[1])]
		);
		assert!(missing_give_keys(&executed, &sent).len() == 1);
	}

	#[test]
	fn missing_give_keys_empty_when_nothing_was_sent() {
		let executed = [clause(0x100, 3, 111)];
		assert!(missing_give_keys(&executed, &[]).is_empty());
	}
}
