// SPDX-License-Identifier: GPL-3.0-only
//
// Owned-tree adaptation of Mergiraf 0.18.0 `src/pcs.rs`, `src/changeset.rs`,
// and the PCS cleanup in `src/merge_3dm.rs` at upstream revision
// e8e13887b85b8cb56b1dc1624c5f94e3d39182b6.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::merge::kernel::ClassId;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PcsNode {
	Start,
	Class(ClassId),
	End,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PcsTriple {
	pub parent: ClassId,
	pub child: PcsNode,
	pub successor: PcsNode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PcsCycle {
	pub remaining: Vec<PcsNode>,
	pub triples: Vec<PcsTriple>,
}

pub(crate) fn merge_orders(
	parent: ClassId,
	base: &[ClassId],
	revisions: &[&[ClassId]],
) -> Result<Vec<ClassId>, PcsCycle> {
	if let Some(first) = revisions.first()
		&& revisions.iter().all(|revision| *revision == *first)
	{
		return Ok(first.to_vec());
	}

	let base_triples = sequence_triples(parent, base);
	let revision_triples = revisions
		.iter()
		.map(|revision| sequence_triples(parent, revision))
		.collect::<Vec<_>>();
	let removed = revision_triples
		.iter()
		.flat_map(|triples| base_triples.difference(triples))
		.copied()
		.collect::<BTreeSet<_>>();
	let mut merged = base_triples
		.difference(&removed)
		.copied()
		.collect::<BTreeSet<_>>();
	for triples in &revision_triples {
		merged.extend(triples.difference(&base_triples).copied());
	}
	topological_order(&merged, base, revisions)
}

pub(crate) fn sequence_triples(parent: ClassId, children: &[ClassId]) -> BTreeSet<PcsTriple> {
	let mut sequence = Vec::with_capacity(children.len() + 2);
	sequence.push(PcsNode::Start);
	sequence.extend(children.iter().copied().map(PcsNode::Class));
	sequence.push(PcsNode::End);
	sequence
		.windows(2)
		.map(|window| PcsTriple {
			parent,
			child: window[0],
			successor: window[1],
		})
		.collect()
}

fn topological_order(
	triples: &BTreeSet<PcsTriple>,
	base: &[ClassId],
	revisions: &[&[ClassId]],
) -> Result<Vec<ClassId>, PcsCycle> {
	let mut nodes = BTreeSet::from([PcsNode::Start, PcsNode::End]);
	// Callers filter resolved deletions before PCS, so every remaining class is live.
	nodes.extend(base.iter().copied().map(PcsNode::Class));
	for revision in revisions {
		nodes.extend(revision.iter().copied().map(PcsNode::Class));
	}
	let mut outgoing: BTreeMap<PcsNode, BTreeSet<PcsNode>> = BTreeMap::new();
	let mut indegree: BTreeMap<PcsNode, usize> = BTreeMap::new();
	for triple in triples {
		nodes.insert(triple.child);
		nodes.insert(triple.successor);
		if outgoing
			.entry(triple.child)
			.or_default()
			.insert(triple.successor)
		{
			*indegree.entry(triple.successor).or_default() += 1;
		}
		indegree.entry(triple.child).or_default();
	}
	for node in &nodes {
		indegree.entry(*node).or_default();
	}

	let ranks = stable_ranks(&nodes, base, revisions);
	let mut ready = indegree
		.iter()
		.filter(|(_, count)| **count == 0)
		.map(|(node, _)| (ranks[node].clone(), *node))
		.collect::<BTreeSet<_>>();
	let mut ordered = Vec::with_capacity(nodes.len());
	while let Some((_, node)) = ready.pop_first() {
		ordered.push(node);
		for successor in outgoing.get(&node).into_iter().flatten() {
			let count = indegree
				.get_mut(successor)
				.expect("successor has an indegree entry");
			*count -= 1;
			if *count == 0 {
				ready.insert((ranks[successor].clone(), *successor));
			}
		}
	}
	if ordered.len() != nodes.len() {
		let emitted = ordered.iter().copied().collect::<BTreeSet<_>>();
		return Err(PcsCycle {
			remaining: nodes.difference(&emitted).copied().collect(),
			triples: triples.iter().copied().collect(),
		});
	}
	Ok(ordered
		.into_iter()
		.filter_map(|node| match node {
			PcsNode::Class(class) => Some(class),
			PcsNode::Start | PcsNode::End => None,
		})
		.collect())
}

fn stable_ranks(
	nodes: &BTreeSet<PcsNode>,
	base: &[ClassId],
	revisions: &[&[ClassId]],
) -> BTreeMap<PcsNode, (u8, Vec<usize>, PcsNode)> {
	let positions = |sequence: &[ClassId]| {
		sequence
			.iter()
			.enumerate()
			.map(|(index, class)| (*class, index))
			.collect::<BTreeMap<_, _>>()
	};
	let base_positions = positions(base);
	let revision_positions = revisions
		.iter()
		.map(|revision| positions(revision))
		.collect::<Vec<_>>();
	let missing = usize::MAX;
	nodes
		.iter()
		.map(|node| {
			let rank = match node {
				PcsNode::Start => (0, vec![0; revisions.len() + 1], *node),
				PcsNode::Class(class) => {
					let mut positions = Vec::with_capacity(revisions.len() + 1);
					positions.push(*base_positions.get(class).unwrap_or(&missing));
					positions.extend(
						revision_positions
							.iter()
							.map(|revision| *revision.get(class).unwrap_or(&missing)),
					);
					(1, positions, *node)
				}
				PcsNode::End => (2, vec![missing; revisions.len() + 1], *node),
			};
			(*node, rank)
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn class(value: u32) -> ClassId {
		ClassId::new(value)
	}

	#[test]
	fn independent_insertions_are_both_retained() {
		let merged = merge_orders(
			class(0),
			&[class(1)],
			&[&[class(2), class(1)], &[class(1), class(3)]],
		)
		.unwrap();

		assert_eq!(merged, vec![class(2), class(1), class(3)]);
	}

	#[test]
	fn one_sided_reorder_wins_over_unchanged_base() {
		let base = [class(1), class(2), class(3)];
		let left = [class(2), class(1), class(3)];

		assert_eq!(
			merge_orders(class(0), &base, &[&left, &base]).unwrap(),
			left
		);
	}

	#[test]
	fn live_class_missing_from_one_revision_is_retained() {
		assert_eq!(
			merge_orders(class(0), &[class(1)], &[&[], &[class(1)]]).unwrap(),
			vec![class(1)]
		);
	}

	#[test]
	fn incompatible_reorders_report_a_cycle() {
		let result = merge_orders(
			class(0),
			&[class(1), class(2), class(3)],
			&[
				&[class(2), class(1), class(3)],
				&[class(1), class(3), class(2)],
			],
		);

		assert!(result.is_err());
	}

	#[test]
	fn three_independent_insertions_are_retained_in_revision_order() {
		let merged = merge_orders(
			class(0),
			&[class(1)],
			&[
				&[class(2), class(1)],
				&[class(1), class(3)],
				&[class(1), class(4)],
			],
		)
		.unwrap();

		assert_eq!(merged, vec![class(2), class(1), class(3), class(4)]);
	}
}
