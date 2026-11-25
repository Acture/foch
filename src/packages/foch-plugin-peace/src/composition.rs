//! Selection identities are borrowed native clauses, never native allocations.
//! Keeping the giver and receiver on every selection prevents a direction tab
//! from silently reinterpreting a concession as a demand.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Clause {
	pub address: usize,
	pub kind: u32,
	pub target: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
	pub giver: u64,
	pub receiver: u64,
	pub selected: Vec<Clause>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
	Receive,
	Give,
}

impl Side {
	pub fn opposite(self) -> Self {
		match self {
			Side::Receive => Side::Give,
			Side::Give => Side::Receive,
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
	pub clause: Clause,
	pub giver: u64,
	pub receiver: u64,
	pub side: Side,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposeError {
	UnrelatedParties,
	InvalidClause,
	DuplicateClause,
}

pub fn compose(receive: &Draft, give: &Draft) -> Result<Vec<Selection>, ComposeError> {
	if receive.giver == receive.receiver
		|| receive.giver != give.receiver
		|| receive.receiver != give.giver
	{
		return Err(ComposeError::UnrelatedParties);
	}

	let mut addresses = std::collections::BTreeSet::new();
	let mut result = Vec::with_capacity(receive.selected.len() + give.selected.len());
	for (draft, side) in [(receive, Side::Receive), (give, Side::Give)] {
		for &clause in &draft.selected {
			if clause.address == 0 || !crate::contract::kind_slot_admitted(clause.kind) {
				return Err(ComposeError::InvalidClause);
			}
			if !addresses.insert(clause.address) {
				return Err(ComposeError::DuplicateClause);
			}
			result.push(Selection {
				clause,
				giver: draft.giver,
				receiver: draft.receiver,
				side,
			});
		}
	}
	Ok(result)
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
				selected: vec![clause(0x100, 3, 111)],
			},
			Draft {
				giver: 1,
				receiver: 2,
				selected: vec![clause(0x200, 3, 1769)],
			},
		)
	}

	#[test]
	fn retains_both_directions_in_one_selection() {
		let (receive, give) = drafts();
		let selections = compose(&receive, &give).unwrap();
		assert_eq!(selections.len(), 2);
		assert_eq!(
			(
				selections[0].giver,
				selections[0].receiver,
				selections[0].side
			),
			(2, 1, Side::Receive)
		);
		assert_eq!(
			(
				selections[1].giver,
				selections[1].receiver,
				selections[1].side
			),
			(1, 2, Side::Give)
		);
		assert_eq!(selections[1].clause.target, 1769);
	}

	#[test]
	fn composing_does_not_change_either_editor_draft() {
		let (receive, give) = drafts();
		let before = (receive.clone(), give.clone());
		compose(&receive, &give).unwrap();
		compose(&receive, &give).unwrap();
		assert_eq!((receive, give), before);
	}

	#[test]
	fn cash_in_opposite_directions_keeps_its_own_participants() {
		let (mut receive, mut give) = drafts();
		receive.selected = vec![clause(0x100, 0, 0)];
		give.selected = vec![clause(0x200, 0, 0)];
		let selections = compose(&receive, &give).unwrap();
		assert_eq!(selections.len(), 2);
		assert_ne!(selections[0].giver, selections[1].giver);
	}

	#[test]
	fn supports_a_concession_only_without_turning_it_into_a_demand() {
		let (mut receive, give) = drafts();
		receive.selected.clear();
		let selections = compose(&receive, &give).unwrap();
		assert_eq!(selections.len(), 1);
		assert_eq!(selections[0].side, Side::Give);
		assert_eq!(selections[0].giver, 1);
	}

	#[test]
	fn rejects_a_draft_from_another_negotiation() {
		let (receive, mut give) = drafts();
		give.receiver = 3;
		assert_eq!(
			compose(&receive, &give),
			Err(ComposeError::UnrelatedParties)
		);
	}

	#[test]
	fn rejects_duplicate_native_ownership_instead_of_freeing_one_clause_twice() {
		let (receive, mut give) = drafts();
		give.selected[0].address = receive.selected[0].address;
		assert_eq!(compose(&receive, &give), Err(ComposeError::DuplicateClause));
	}

	#[test]
	fn rejects_null_and_unknown_native_clauses() {
		let (mut receive, give) = drafts();
		receive.selected[0].address = 0;
		assert_eq!(compose(&receive, &give), Err(ComposeError::InvalidClause));
		receive.selected[0] = clause(0x100, 40, 111);
		assert_eq!(compose(&receive, &give), Err(ComposeError::InvalidClause));
	}

	#[test]
	fn a_scripted_or_modded_target_stays_opaque_inside_the_image_table() {
		let (mut receive, give) = drafts();
		receive.selected = vec![clause(
			0x100,
			(crate::contract::KIND_SLOTS as u32) - 1,
			u64::MAX,
		)];
		let selections = compose(&receive, &give).unwrap();
		assert_eq!(selections[0].clause.kind, 39);
		assert_eq!(selections[0].clause.target, u64::MAX);
		assert_eq!(selections[0].side, Side::Receive);
	}

	#[test]
	fn empty_drafts_produce_an_empty_selection() {
		let (mut receive, mut give) = drafts();
		receive.selected.clear();
		give.selected.clear();
		assert!(compose(&receive, &give).unwrap().is_empty());
	}
}
