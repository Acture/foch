// SPDX-License-Identifier: GPL-3.0-only
//
// Parser-independent adaptation of Mergiraf 0.18.0's matching and GumTree
// implementation (`src/matching.rs`, `src/tree_matcher.rs`, and
// `src/tree_matcher/priority_list.rs`) at upstream revision
// e8e13887b85b8cb56b1dc1624c5f94e3d39182b6.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
	ChildCardinality, NodeId, NormalizedNode, NormalizedTree, SemanticKey, SemanticKeyScope,
	SubtreeHash,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
	Exact,
	SemanticAnchor,
	SoftSignature,
	RequiredSlot,
	DescendantSimilarity,
	Recovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MatchRecord {
	pub left: NodeId,
	pub right: NodeId,
	pub kind: MatchKind,
	/// Fixed-point score in millionths. Exact and anchor matches use 1_000_000.
	pub score: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AmbiguousMatch {
	pub left: NodeId,
	pub candidates: Vec<NodeId>,
	pub score: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Matching {
	by_left: BTreeMap<NodeId, MatchRecord>,
	by_right: BTreeMap<NodeId, NodeId>,
	ambiguous: Vec<AmbiguousMatch>,
}

impl Matching {
	pub fn compose_through_base(base_left: &Self, base_right: &Self) -> Self {
		let mut composed = Self::default();
		for left_record in base_left.records() {
			let Some(right_record) = base_right.record(left_record.left) else {
				continue;
			};
			composed.insert(MatchRecord {
				left: left_record.right,
				right: right_record.right,
				kind: if left_record.kind == MatchKind::Exact
					&& right_record.kind == MatchKind::Exact
				{
					MatchKind::Exact
				} else {
					MatchKind::Recovery
				},
				score: left_record.score.min(right_record.score),
			});
		}
		composed
	}

	pub fn get_from_left(&self, left: NodeId) -> Option<NodeId> {
		self.by_left.get(&left).map(|record| record.right)
	}

	pub fn get_from_right(&self, right: NodeId) -> Option<NodeId> {
		self.by_right.get(&right).copied()
	}

	pub fn record(&self, left: NodeId) -> Option<&MatchRecord> {
		self.by_left.get(&left)
	}

	pub fn records(&self) -> impl ExactSizeIterator<Item = &MatchRecord> {
		self.by_left.values()
	}

	pub fn ambiguities(&self) -> &[AmbiguousMatch] {
		&self.ambiguous
	}

	pub fn len(&self) -> usize {
		self.by_left.len()
	}

	pub fn is_empty(&self) -> bool {
		self.by_left.is_empty()
	}

	fn is_left_matched(&self, left: NodeId) -> bool {
		self.by_left.contains_key(&left)
	}

	fn is_right_matched(&self, right: NodeId) -> bool {
		self.by_right.contains_key(&right)
	}

	fn is_left_ambiguous(&self, left: NodeId) -> bool {
		self.ambiguous
			.iter()
			.any(|ambiguity| ambiguity.left == left)
	}

	fn is_right_ambiguous(&self, right: NodeId) -> bool {
		self.ambiguous
			.iter()
			.any(|ambiguity| ambiguity.candidates.contains(&right))
	}

	fn insert(&mut self, record: MatchRecord) -> bool {
		if self.is_left_matched(record.left) || self.is_right_matched(record.right) {
			return false;
		}
		self.by_right.insert(record.right, record.left);
		self.by_left.insert(record.left, record);
		true
	}

	fn record_ambiguity(&mut self, ambiguity: AmbiguousMatch) {
		if !self.ambiguous.contains(&ambiguity) {
			self.ambiguous.push(ambiguity);
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MatcherConfig {
	pub min_height: u32,
	/// Fixed-point Dice threshold in millionths.
	pub similarity_threshold: u32,
	pub max_recovery_size: usize,
}

impl Default for MatcherConfig {
	fn default() -> Self {
		Self {
			min_height: 1,
			similarity_threshold: 500_000,
			max_recovery_size: 100,
		}
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TreeMatcher {
	config: MatcherConfig,
}

impl TreeMatcher {
	pub const fn new(config: MatcherConfig) -> Self {
		Self { config }
	}

	pub fn match_trees(&self, left: &NormalizedTree, right: &NormalizedTree) -> Matching {
		self.match_trees_with_seed(left, right, None)
	}

	pub fn match_trees_with_seed(
		&self,
		left: &NormalizedTree,
		right: &NormalizedTree,
		seed: Option<&Matching>,
	) -> Matching {
		let mut matching = Matching::default();
		let left_root = left.root();
		let right_root = right.root();
		let left_root_node = left.node(left_root).unwrap();
		let right_root_node = right.node(right_root).unwrap();
		if compatible(left_root_node, right_root_node) {
			if isomorphic_subtree(left, right, left_root, right_root) {
				match_isomorphic_subtree(left, right, &mut matching, left_root, right_root);
			} else {
				matching.insert(MatchRecord {
					left: left_root,
					right: right_root,
					kind: MatchKind::Recovery,
					score: 500_000,
				});
			}
		}

		if let Some(seed) = seed {
			for record in seed.records() {
				if left.node(record.left).is_err() || right.node(record.right).is_err() {
					continue;
				}
				if !compatible_at(left, right, &matching, record.left, record.right) {
					continue;
				}
				let exact = isomorphic_subtree(left, right, record.left, record.right);
				matching.insert(MatchRecord {
					left: record.left,
					right: record.right,
					kind: if exact {
						MatchKind::Exact
					} else {
						MatchKind::Recovery
					},
					score: if exact { 1_000_000 } else { record.score },
				});
			}
		}

		match_unique_anchors(left, right, &mut matching);
		match_required_slots(left, right, &mut matching);
		match_parent_scoped_anchors(left, right, &mut matching);
		match_soft_signatures(left, right, &mut matching);
		match_exact_subtrees(left, right, &mut matching, self.config.min_height);
		match_repeated_parent_scoped_siblings(
			left,
			right,
			&mut matching,
			self.config.similarity_threshold,
		);
		match_parent_scoped_anchors(left, right, &mut matching);
		match_by_descendants(left, right, &mut matching, self.config.similarity_threshold);
		match_required_slots(left, right, &mut matching);
		match_parent_scoped_anchors(left, right, &mut matching);
		recover_children(left, right, &mut matching, self.config.max_recovery_size);
		match_parent_scoped_anchors(left, right, &mut matching);
		match_repeated_parent_scoped_siblings(
			left,
			right,
			&mut matching,
			self.config.similarity_threshold,
		);
		match_parent_scoped_anchors(left, right, &mut matching);
		match_soft_signatures(left, right, &mut matching);
		recover_children(left, right, &mut matching, self.config.max_recovery_size);
		record_unresolved_soft_signature_ambiguities(left, right, &mut matching);
		matching
	}
}

fn compatible(left: &NormalizedNode, right: &NormalizedNode) -> bool {
	left.kind == right.kind
		&& left.child_cardinality == right.child_cardinality
		&& !anchors_forbid(left.anchor.as_ref(), right.anchor.as_ref())
}

fn compatible_at(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &Matching,
	left_id: NodeId,
	right_id: NodeId,
) -> bool {
	let left_node = left.node(left_id).unwrap();
	let right_node = right.node(right_id).unwrap();
	if !compatible(left_node, right_node) {
		return false;
	}
	let parent_scoped = [left_node.anchor.as_ref(), right_node.anchor.as_ref()]
		.into_iter()
		.flatten()
		.any(|anchor| anchor.scope == SemanticKeyScope::Parent);
	if !parent_scoped {
		return true;
	}
	let (Some(left_parent), Some(right_parent)) = (left_node.parent, right_node.parent) else {
		return false;
	};
	matching.get_from_left(left_parent) == Some(right_parent)
}

fn match_soft_signatures(left: &NormalizedTree, right: &NormalizedTree, matching: &mut Matching) {
	loop {
		match_required_slots(left, right, matching);
		let parents = matching.records().copied().collect::<Vec<_>>();
		let mut changed = false;
		for parent in parents {
			let left_parent = left.node(parent.left).unwrap();
			let right_parent = right.node(parent.right).unwrap();
			if left_parent.child_cardinality == ChildCardinality::ExactlyOne
				|| right_parent.child_cardinality == ChildCardinality::ExactlyOne
			{
				continue;
			}
			let left_groups =
				unmatched_signature_groups(left, &left_parent.children, matching, true);
			let right_groups =
				unmatched_signature_groups(right, &right_parent.children, matching, false);
			for (key, left_nodes) in left_groups {
				let Some(right_nodes) = right_groups.get(&key) else {
					continue;
				};
				if let ([left_node], [right_node]) = (left_nodes.as_slice(), right_nodes.as_slice())
					&& compatible_at(left, right, matching, *left_node, *right_node)
				{
					changed |= matching.insert(MatchRecord {
						left: *left_node,
						right: *right_node,
						kind: MatchKind::SoftSignature,
						score: 900_000,
					});
				}
			}
		}
		if !changed {
			break;
		}
	}
}

fn record_unresolved_soft_signature_ambiguities(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &mut Matching,
) {
	let parents = matching.records().copied().collect::<Vec<_>>();
	for parent in parents {
		let left_groups = unmatched_signature_groups(
			left,
			&left.node(parent.left).unwrap().children,
			matching,
			true,
		);
		let right_groups = unmatched_signature_groups(
			right,
			&right.node(parent.right).unwrap().children,
			matching,
			false,
		);
		for (key, left_nodes) in left_groups {
			let Some(right_nodes) = right_groups.get(&key) else {
				continue;
			};
			for left_node in left_nodes {
				matching.record_ambiguity(AmbiguousMatch {
					left: left_node,
					candidates: right_nodes.clone(),
					score: 900_000,
				});
			}
		}
	}
}

fn unmatched_signature_groups(
	tree: &NormalizedTree,
	children: &[NodeId],
	matching: &Matching,
	left_side: bool,
) -> BTreeMap<(String, String), Vec<NodeId>> {
	let mut groups = BTreeMap::new();
	for child in children {
		let unavailable = if left_side {
			matching.is_left_matched(*child) || matching.is_left_ambiguous(*child)
		} else {
			matching.is_right_matched(*child) || matching.is_right_ambiguous(*child)
		};
		if unavailable {
			continue;
		}
		let node = tree.node(*child).unwrap();
		let Some(signature) = &node.signature else {
			continue;
		};
		groups
			.entry((node.kind.clone(), signature.clone()))
			.or_insert_with(Vec::new)
			.push(*child);
	}
	groups
}

fn anchors_forbid(left: Option<&SemanticKey>, right: Option<&SemanticKey>) -> bool {
	matches!(
		(left, right),
		(Some(left), Some(right))
			if left.scope == right.scope
				&& left.namespace == right.namespace
				&& left.value != right.value
	)
}

fn match_unique_anchors(left: &NormalizedTree, right: &NormalizedTree, matching: &mut Matching) {
	let left_anchors = anchors_by_key(left);
	let right_anchors = anchors_by_key(right);
	for (key, left_nodes) in left_anchors {
		let Some(right_nodes) = right_anchors.get(&key) else {
			continue;
		};
		if let ([left_id], [right_id]) = (left_nodes.as_slice(), right_nodes.as_slice())
			&& compatible(left.node(*left_id).unwrap(), right.node(*right_id).unwrap())
		{
			matching.insert(MatchRecord {
				left: *left_id,
				right: *right_id,
				kind: MatchKind::SemanticAnchor,
				score: 1_000_000,
			});
		}
	}
}

fn anchors_by_key(tree: &NormalizedTree) -> BTreeMap<SemanticKey, Vec<NodeId>> {
	let mut anchors = BTreeMap::new();
	for (id, node) in tree.nodes() {
		if let Some(anchor) = &node.anchor
			&& anchor.scope == SemanticKeyScope::Global
		{
			anchors
				.entry(anchor.clone())
				.or_insert_with(Vec::new)
				.push(id);
		}
	}
	anchors
}

fn match_parent_scoped_anchors(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &mut Matching,
) {
	loop {
		match_required_slots(left, right, matching);
		let parents = matching.records().copied().collect::<Vec<_>>();
		let mut changed = false;
		for parent in parents {
			let left_groups = unmatched_parent_anchor_groups(
				left,
				&left.node(parent.left).unwrap().children,
				matching,
				true,
			);
			let right_groups = unmatched_parent_anchor_groups(
				right,
				&right.node(parent.right).unwrap().children,
				matching,
				false,
			);
			for (key, left_nodes) in left_groups {
				let Some(right_nodes) = right_groups.get(&key) else {
					continue;
				};
				let repeated = left_nodes
					.first()
					.is_some_and(|id| anchor_repeats_under_parent(left, *id, &key))
					|| right_nodes
						.first()
						.is_some_and(|id| anchor_repeats_under_parent(right, *id, &key));
				if repeated {
					continue;
				}
				if let ([left_node], [right_node]) = (left_nodes.as_slice(), right_nodes.as_slice())
					&& compatible_at(left, right, matching, *left_node, *right_node)
				{
					changed |= matching.insert(MatchRecord {
						left: *left_node,
						right: *right_node,
						kind: MatchKind::SemanticAnchor,
						score: 1_000_000,
					});
				}
			}
		}
		if !changed {
			break;
		}
	}
}

fn unmatched_parent_anchor_groups(
	tree: &NormalizedTree,
	children: &[NodeId],
	matching: &Matching,
	left_side: bool,
) -> BTreeMap<SemanticKey, Vec<NodeId>> {
	let mut groups = BTreeMap::new();
	for child in children {
		let unavailable = if left_side {
			matching.is_left_matched(*child) || matching.is_left_ambiguous(*child)
		} else {
			matching.is_right_matched(*child) || matching.is_right_ambiguous(*child)
		};
		if unavailable {
			continue;
		}
		let Some(anchor) = tree.node(*child).unwrap().anchor.as_ref() else {
			continue;
		};
		if anchor.scope != SemanticKeyScope::Parent {
			continue;
		}
		groups
			.entry(anchor.clone())
			.or_insert_with(Vec::new)
			.push(*child);
	}
	groups
}

fn match_repeated_parent_scoped_siblings(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &mut Matching,
	threshold: u32,
) {
	loop {
		let parents = matching.records().copied().collect::<Vec<_>>();
		let mut proposals = Vec::new();
		for parent in parents {
			let left_groups = unmatched_parent_anchor_groups(
				left,
				&left.node(parent.left).unwrap().children,
				matching,
				true,
			);
			let right_groups = unmatched_parent_anchor_groups(
				right,
				&right.node(parent.right).unwrap().children,
				matching,
				false,
			);
			for (key, left_nodes) in left_groups {
				let Some(right_nodes) = right_groups.get(&key) else {
					continue;
				};
				let repeated = left_nodes
					.first()
					.is_some_and(|id| anchor_repeats_under_parent(left, *id, &key))
					|| right_nodes
						.first()
						.is_some_and(|id| anchor_repeats_under_parent(right, *id, &key));
				if !repeated {
					continue;
				}
				let candidates = repeated_sibling_candidates(
					left,
					right,
					matching,
					&left_nodes,
					right_nodes,
					threshold,
				);
				for candidate in &candidates {
					if is_mutual_unique_best(*candidate, &candidates) {
						proposals.push(*candidate);
					}
				}
			}
		}

		proposals.sort_by(|left_candidate, right_candidate| {
			right_candidate
				.score
				.cmp(&left_candidate.score)
				.then_with(|| left_candidate.left.cmp(&right_candidate.left))
				.then_with(|| left_candidate.right.cmp(&right_candidate.right))
		});
		let mut changed = false;
		for proposal in proposals {
			changed |= matching.insert(MatchRecord {
				left: proposal.left,
				right: proposal.right,
				kind: MatchKind::DescendantSimilarity,
				score: proposal.score,
			});
		}
		if !changed {
			break;
		}
		match_required_slots(left, right, matching);
		match_parent_scoped_anchors(left, right, matching);
	}

	record_unresolved_repeated_sibling_ambiguities(left, right, matching, threshold);
}

fn repeated_sibling_candidates(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &Matching,
	left_nodes: &[NodeId],
	right_nodes: &[NodeId],
	threshold: u32,
) -> Vec<RecoveryCandidate> {
	let left_features = left_nodes
		.iter()
		.map(|id| (*id, subtree_features(left, *id)))
		.collect::<BTreeMap<_, _>>();
	let right_features = right_nodes
		.iter()
		.map(|id| (*id, subtree_features(right, *id)))
		.collect::<BTreeMap<_, _>>();
	let mut candidates = Vec::new();
	for left_id in left_nodes {
		for right_id in right_nodes {
			if !compatible_at(left, right, matching, *left_id, *right_id) {
				continue;
			}
			let score = raw_subtree_similarity(
				left_features.get(left_id).expect("left features collected"),
				right_features
					.get(right_id)
					.expect("right features collected"),
			);
			if score > threshold {
				candidates.push(RecoveryCandidate {
					left: *left_id,
					right: *right_id,
					score,
				});
			}
		}
	}
	candidates
}

fn is_mutual_unique_best(candidate: RecoveryCandidate, candidates: &[RecoveryCandidate]) -> bool {
	let left_best = candidates
		.iter()
		.filter(|other| other.left == candidate.left)
		.map(|other| other.score)
		.max();
	let right_best = candidates
		.iter()
		.filter(|other| other.right == candidate.right)
		.map(|other| other.score)
		.max();
	if left_best != Some(candidate.score) || right_best != Some(candidate.score) {
		return false;
	}
	let left_ties = candidates
		.iter()
		.filter(|other| other.left == candidate.left && other.score == candidate.score)
		.count();
	let right_ties = candidates
		.iter()
		.filter(|other| other.right == candidate.right && other.score == candidate.score)
		.count();
	left_ties == 1 && right_ties == 1
}

fn subtree_features(
	tree: &NormalizedTree,
	root: NodeId,
) -> BTreeMap<(String, Option<String>), usize> {
	let mut features = BTreeMap::new();
	let mut stack = vec![root];
	while let Some(id) = stack.pop() {
		let node = tree.node(id).unwrap();
		*features
			.entry((node.kind.clone(), node.value.clone()))
			.or_default() += 1;
		stack.extend(node.children.iter().rev().copied());
	}
	features
}

fn raw_subtree_similarity(
	left: &BTreeMap<(String, Option<String>), usize>,
	right: &BTreeMap<(String, Option<String>), usize>,
) -> u32 {
	let left_size = left.values().sum::<usize>();
	let right_size = right.values().sum::<usize>();
	let denominator = left_size + right_size;
	if denominator == 0 {
		return 0;
	}
	let overlap = left
		.iter()
		.map(|(feature, left_count)| left_count.min(right.get(feature).unwrap_or(&0)))
		.sum::<usize>();
	u32::try_from((2_000_000usize.saturating_mul(overlap)) / denominator).unwrap_or(1_000_000)
}

fn record_unresolved_repeated_sibling_ambiguities(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &mut Matching,
	threshold: u32,
) {
	let parents = matching.records().copied().collect::<Vec<_>>();
	for parent in parents {
		let left_groups = unmatched_parent_anchor_groups(
			left,
			&left.node(parent.left).unwrap().children,
			matching,
			true,
		);
		let right_groups = unmatched_parent_anchor_groups(
			right,
			&right.node(parent.right).unwrap().children,
			matching,
			false,
		);
		for (key, left_nodes) in left_groups {
			let Some(right_nodes) = right_groups.get(&key) else {
				continue;
			};
			let repeated = left_nodes
				.first()
				.is_some_and(|id| anchor_repeats_under_parent(left, *id, &key))
				|| right_nodes
					.first()
					.is_some_and(|id| anchor_repeats_under_parent(right, *id, &key));
			if !repeated {
				continue;
			}
			let candidates = repeated_sibling_candidates(
				left,
				right,
				matching,
				&left_nodes,
				right_nodes,
				threshold,
			);
			for left_node in left_nodes {
				let Some(best_score) = candidates
					.iter()
					.filter(|candidate| candidate.left == left_node)
					.map(|candidate| candidate.score)
					.max()
				else {
					continue;
				};
				let best_candidates = candidates
					.iter()
					.filter(|candidate| {
						candidate.left == left_node && candidate.score == best_score
					})
					.map(|candidate| candidate.right)
					.collect::<Vec<_>>();
				matching.record_ambiguity(AmbiguousMatch {
					left: left_node,
					candidates: best_candidates,
					score: best_score,
				});
			}
		}
	}
}

fn match_required_slots(left: &NormalizedTree, right: &NormalizedTree, matching: &mut Matching) {
	loop {
		let parents = matching.records().copied().collect::<Vec<_>>();
		let mut changed = false;
		for parent in parents {
			let left_parent = left.node(parent.left).unwrap();
			let right_parent = right.node(parent.right).unwrap();
			if left_parent.child_cardinality != ChildCardinality::ExactlyOne
				|| right_parent.child_cardinality != ChildCardinality::ExactlyOne
			{
				continue;
			}
			let ([left_child], [right_child]) = (
				left_parent.children.as_slice(),
				right_parent.children.as_slice(),
			) else {
				continue;
			};
			if !compatible_at(left, right, matching, *left_child, *right_child) {
				continue;
			}
			let matched_right = matching.get_from_left(*left_child);
			let matched_left = matching.get_from_right(*right_child);
			if matched_right.is_some_and(|matched| matched != *right_child)
				|| matched_left.is_some_and(|matched| matched != *left_child)
			{
				matching.record_ambiguity(AmbiguousMatch {
					left: *left_child,
					candidates: vec![*right_child],
					score: 1_000_000,
				});
				continue;
			}
			if matched_right == Some(*right_child) {
				continue;
			}
			changed |= matching.insert(MatchRecord {
				left: *left_child,
				right: *right_child,
				kind: MatchKind::RequiredSlot,
				score: 1_000_000,
			});
		}
		if !changed {
			break;
		}
	}
}

fn match_exact_subtrees(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &mut Matching,
	min_height: u32,
) {
	let mut right_by_hash: BTreeMap<SubtreeHash, Vec<NodeId>> = BTreeMap::new();
	for (right_id, right_node) in right.nodes() {
		if right_node.height >= min_height
			&& !matching.is_right_matched(right_id)
			&& !child_of_unmatched_required_parent(right, right_id, matching, false)
		{
			right_by_hash
				.entry(right_node.subtree_hash)
				.or_default()
				.push(right_id);
		}
	}
	let mut left_nodes = left
		.nodes()
		.filter(|(id, node)| {
			node.height >= min_height
				&& !matching.is_left_matched(*id)
				&& !child_of_unmatched_required_parent(left, *id, matching, true)
		})
		.collect::<Vec<_>>();
	left_nodes.sort_by(|(left_id, left_node), (right_id, right_node)| {
		right_node
			.height
			.cmp(&left_node.height)
			.then_with(|| left_id.cmp(right_id))
	});
	for (left_id, left_node) in left_nodes {
		let Some(candidates) = right_by_hash.get(&left_node.subtree_hash) else {
			continue;
		};
		let mut ranked = candidates
			.iter()
			.copied()
			.filter(|right_id| !matching.is_right_matched(*right_id))
			.filter(|right_id| compatible_at(left, right, matching, left_id, *right_id))
			.map(|right_id| {
				(
					right_id,
					exact_context_score(left, right, matching, left_id, right_id),
				)
			})
			.collect::<Vec<_>>();
		ranked.sort_by(|(left_id, left_score), (right_id, right_score)| {
			right_score
				.cmp(left_score)
				.then_with(|| left_id.cmp(right_id))
		});
		let Some((right_id, _)) = ranked.first().copied() else {
			continue;
		};
		match_isomorphic_subtree(left, right, matching, left_id, right_id);
	}
}

fn child_of_unmatched_required_parent(
	tree: &NormalizedTree,
	id: NodeId,
	matching: &Matching,
	left_side: bool,
) -> bool {
	let Some(parent) = tree.node(id).unwrap().parent else {
		return false;
	};
	if tree.node(parent).unwrap().child_cardinality != ChildCardinality::ExactlyOne {
		return false;
	}
	if left_side {
		!matching.is_left_matched(parent)
	} else {
		!matching.is_right_matched(parent)
	}
}

fn exact_context_score(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &Matching,
	left_id: NodeId,
	right_id: NodeId,
) -> u32 {
	let left_node = left.node(left_id).unwrap();
	let right_node = right.node(right_id).unwrap();
	let mut score = 0;
	if let (Some(left_parent), Some(right_parent)) = (left_node.parent, right_node.parent)
		&& matching.get_from_left(left_parent) == Some(right_parent)
	{
		score += 100;
		let left_index = child_index(left, left_parent, left_id);
		let right_index = child_index(right, right_parent, right_id);
		if left_index == right_index {
			score += 20;
		}
	}
	score
}

fn child_index(tree: &NormalizedTree, parent: NodeId, child: NodeId) -> Option<usize> {
	tree.node(parent)
		.unwrap()
		.children
		.iter()
		.position(|candidate| *candidate == child)
}

fn match_isomorphic_subtree(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &mut Matching,
	left_id: NodeId,
	right_id: NodeId,
) {
	let Some(child_pairs) = isomorphic_child_pairs(left, right, left_id, right_id) else {
		return;
	};
	matching.insert(MatchRecord {
		left: left_id,
		right: right_id,
		kind: MatchKind::Exact,
		score: 1_000_000,
	});
	for (left_child, right_child) in child_pairs {
		match_isomorphic_subtree(left, right, matching, left_child, right_child);
	}
}

fn isomorphic_subtree(
	left: &NormalizedTree,
	right: &NormalizedTree,
	left_id: NodeId,
	right_id: NodeId,
) -> bool {
	isomorphic_child_pairs(left, right, left_id, right_id).is_some()
}

fn isomorphic_child_pairs(
	left: &NormalizedTree,
	right: &NormalizedTree,
	left_id: NodeId,
	right_id: NodeId,
) -> Option<Vec<(NodeId, NodeId)>> {
	let left_node = left.node(left_id).ok()?;
	let right_node = right.node(right_id).ok()?;
	if left_node.subtree_hash != right_node.subtree_hash
		|| !shallow_tree_eq(left_node, right_node)
		|| left_node.children.len() != right_node.children.len()
	{
		return None;
	}
	if left_node.child_order == crate::ChildOrder::Ordered {
		let pairs = left_node
			.children
			.iter()
			.copied()
			.zip(right_node.children.iter().copied())
			.collect::<Vec<_>>();
		return pairs
			.iter()
			.all(|(left_child, right_child)| {
				isomorphic_subtree(left, right, *left_child, *right_child)
			})
			.then_some(pairs);
	}

	let mut remaining = right_node.children.clone();
	let mut pairs = Vec::with_capacity(left_node.children.len());
	for left_child in &left_node.children {
		let left_hash = left.node(*left_child).ok()?.subtree_hash;
		let index = remaining.iter().position(|right_child| {
			right
				.node(*right_child)
				.is_ok_and(|node| node.subtree_hash == left_hash)
				&& isomorphic_subtree(left, right, *left_child, *right_child)
		})?;
		pairs.push((*left_child, remaining.remove(index)));
	}
	Some(pairs)
}

fn shallow_tree_eq(left: &NormalizedNode, right: &NormalizedNode) -> bool {
	left.kind == right.kind
		&& left.value == right.value
		&& left.anchor == right.anchor
		&& left.signature == right.signature
		&& left.child_order == right.child_order
		&& left.child_cardinality == right.child_cardinality
}

fn match_by_descendants(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &mut Matching,
	threshold: u32,
) {
	let mut changed = true;
	while changed {
		changed = false;
		let mut left_nodes = left
			.nodes()
			.filter(|(id, node)| {
				!node.children.is_empty()
					&& !matching.is_left_matched(*id)
					&& !matching.is_left_ambiguous(*id)
					&& !child_of_unmatched_required_parent(left, *id, matching, true)
			})
			.collect::<Vec<_>>();
		left_nodes.sort_by(|(left_id, left_node), (right_id, right_node)| {
			left_node
				.height
				.cmp(&right_node.height)
				.then_with(|| left_id.cmp(right_id))
		});
		for (left_id, _left_node) in left_nodes {
			let mut candidates = right
				.nodes()
				.filter(|(right_id, _right_node)| {
					!matching.is_right_matched(*right_id)
						&& !matching.is_right_ambiguous(*right_id)
						&& !child_of_unmatched_required_parent(right, *right_id, matching, false)
						&& compatible_at(left, right, matching, left_id, *right_id)
				})
				.map(|(right_id, _)| {
					(
						right_id,
						descendant_similarity(left, right, matching, left_id, right_id),
					)
				})
				.filter(|(_, score)| *score >= threshold)
				.collect::<Vec<_>>();
			candidates.sort_by(|(left_id, left_score), (right_id, right_score)| {
				right_score
					.cmp(left_score)
					.then_with(|| left_id.cmp(right_id))
			});
			let Some((right_id, best_score)) = candidates.first().copied() else {
				continue;
			};
			let tied = candidates
				.iter()
				.take_while(|(_, score)| *score == best_score)
				.map(|(candidate, _)| *candidate)
				.collect::<Vec<_>>();
			if tied.len() > 1 {
				matching.record_ambiguity(AmbiguousMatch {
					left: left_id,
					candidates: tied,
					score: best_score,
				});
				continue;
			}
			changed |= matching.insert(MatchRecord {
				left: left_id,
				right: right_id,
				kind: MatchKind::DescendantSimilarity,
				score: best_score,
			});
		}
	}
}

fn descendant_similarity(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &Matching,
	left_root: NodeId,
	right_root: NodeId,
) -> u32 {
	let left_descendants = descendants(left, left_root);
	let right_descendants = descendants(right, right_root);
	let right_set = right_descendants.iter().copied().collect::<BTreeSet<_>>();
	let overlap = left_descendants
		.iter()
		.filter_map(|left_id| matching.get_from_left(*left_id))
		.filter(|right_id| right_set.contains(right_id))
		.count();
	let denominator = left_descendants.len() + right_descendants.len() + 2;
	if denominator == 0 {
		return 0;
	}
	u32::try_from((2_000_000usize.saturating_mul(overlap)) / denominator).unwrap_or(1_000_000)
}

fn descendants(tree: &NormalizedTree, root: NodeId) -> Vec<NodeId> {
	let mut descendants = Vec::new();
	let mut stack = tree.node(root).unwrap().children.clone();
	while let Some(node) = stack.pop() {
		descendants.push(node);
		stack.extend(tree.node(node).unwrap().children.iter().rev().copied());
	}
	descendants
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RecoveryCandidate {
	left: NodeId,
	right: NodeId,
	score: u32,
}

fn recover_children(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &mut Matching,
	max_recovery_size: usize,
) {
	let mut processed = BTreeSet::new();
	loop {
		match_required_slots(left, right, matching);
		let mut parents = matching
			.records()
			.copied()
			.filter(|record| processed.insert((record.left, record.right)))
			.collect::<Vec<_>>();
		if parents.is_empty() {
			break;
		}
		parents.sort_by(|left_record, right_record| {
			left.node(left_record.left)
				.unwrap()
				.height
				.cmp(&left.node(right_record.left).unwrap().height)
				.then_with(|| left_record.left.cmp(&right_record.left))
		});
		for parent in parents {
			let left_parent = left.node(parent.left).unwrap();
			let right_parent = right.node(parent.right).unwrap();
			let left_children = left_parent.children.clone();
			let right_children = right_parent.children.clone();
			if left_parent.child_cardinality == ChildCardinality::ExactlyOne
				&& right_parent.child_cardinality == ChildCardinality::ExactlyOne
			{
				continue;
			}
			if left_children.len().saturating_mul(right_children.len())
				> max_recovery_size.saturating_mul(max_recovery_size)
			{
				continue;
			}
			let mut candidates = Vec::new();
			for (left_index, left_id) in left_children.iter().copied().enumerate() {
				if matching.is_left_matched(left_id) || matching.is_left_ambiguous(left_id) {
					continue;
				}
				for (right_index, right_id) in right_children.iter().copied().enumerate() {
					if matching.is_right_matched(right_id) || matching.is_right_ambiguous(right_id)
					{
						continue;
					}
					let Some(score) = recovery_score(
						left,
						right,
						matching,
						left_id,
						right_id,
						left_index,
						right_index,
					) else {
						continue;
					};
					candidates.push(RecoveryCandidate {
						left: left_id,
						right: right_id,
						score,
					});
				}
			}
			candidates.sort_by(|left_candidate, right_candidate| {
				right_candidate
					.score
					.cmp(&left_candidate.score)
					.then_with(|| left_candidate.left.cmp(&right_candidate.left))
					.then_with(|| left_candidate.right.cmp(&right_candidate.right))
			});
			for candidate in candidates {
				matching.insert(MatchRecord {
					left: candidate.left,
					right: candidate.right,
					kind: MatchKind::Recovery,
					score: candidate.score,
				});
			}
		}
	}
}

#[allow(clippy::too_many_arguments)]
fn recovery_score(
	left: &NormalizedTree,
	right: &NormalizedTree,
	matching: &Matching,
	left_id: NodeId,
	right_id: NodeId,
	left_index: usize,
	right_index: usize,
) -> Option<u32> {
	let left_node = left.node(left_id).unwrap();
	let right_node = right.node(right_id).unwrap();
	if !compatible_at(left, right, matching, left_id, right_id) {
		return None;
	}
	if let (Some(left_anchor), Some(right_anchor)) = (&left_node.anchor, &right_node.anchor)
		&& left_anchor == right_anchor
	{
		if left_anchor.scope == SemanticKeyScope::Parent
			&& (anchor_repeats_under_parent(left, left_id, left_anchor)
				|| anchor_repeats_under_parent(right, right_id, right_anchor))
		{
			return None;
		}
		return Some(1_000_000);
	}
	if left_node.subtree_hash == right_node.subtree_hash
		&& isomorphic_subtree(left, right, left_id, right_id)
	{
		return Some(950_000);
	}
	let similarity = descendant_similarity(left, right, matching, left_id, right_id);
	if similarity > 0 {
		return Some(600_000u32.saturating_add(similarity / 3));
	}
	if left_index == right_index
		&& left_node.signature.is_some()
		&& right_node.signature.is_some()
		&& left_node.signature != right_node.signature
	{
		return Some(575_000);
	}
	if left_node.children.is_empty() && right_node.children.is_empty() && left_index == right_index
	{
		return Some(if left_node.value == right_node.value {
			550_000
		} else {
			500_000
		});
	}
	None
}

fn anchor_repeats_under_parent(tree: &NormalizedTree, id: NodeId, anchor: &SemanticKey) -> bool {
	let Some(parent) = tree.node(id).unwrap().parent else {
		return false;
	};
	tree.node(parent)
		.unwrap()
		.children
		.iter()
		.filter(|child| tree.node(**child).unwrap().anchor.as_ref() == Some(anchor))
		.count()
		> 1
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::TreeNode;

	fn scalar(value: &str) -> TreeNode {
		TreeNode::leaf("scalar", value)
	}

	fn block(kind: &str, children: Vec<TreeNode>) -> TreeNode {
		TreeNode::branch(kind, children)
	}

	#[test]
	fn exact_subtrees_match_recursively() {
		let left = NormalizedTree::from_root(block(
			"root",
			vec![block("entry", vec![scalar("a"), scalar("b")])],
		))
		.unwrap();
		let right = left.clone();
		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.len(), left.len());
		assert!(
			matching
				.records()
				.all(|record| record.kind == MatchKind::Exact)
		);
	}

	#[test]
	fn semantic_anchors_match_modified_subtrees() {
		let left = NormalizedTree::from_root(block(
			"root",
			vec![block("option", vec![scalar("old")]).with_anchor("option.name", "A")],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![block("option", vec![scalar("new")]).with_anchor("option.name", "A")],
		))
		.unwrap();
		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), Some(NodeId::new(1)));
		assert_eq!(
			matching.record(NodeId::new(1)).unwrap().kind,
			MatchKind::SemanticAnchor
		);
	}

	#[test]
	fn parent_scoped_anchors_only_match_within_corresponding_parents() {
		let parent = |name: &str, value: &str| {
			block(
				"parent",
				vec![
					block("entry", vec![scalar(value)])
						.with_parent_scoped_anchor("entry.name", "shared"),
				],
			)
			.with_anchor("parent.name", name)
		};
		let left = NormalizedTree::from_root(block(
			"root",
			vec![parent("A", "left-a"), parent("B", "left-b")],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![parent("B", "right-b"), parent("A", "right-a")],
		))
		.unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(2)), Some(NodeId::new(5)));
		assert_eq!(matching.get_from_left(NodeId::new(5)), Some(NodeId::new(2)));
		assert_eq!(
			matching.record(NodeId::new(2)).unwrap().kind,
			MatchKind::SemanticAnchor
		);
	}

	#[test]
	fn repeated_parent_scoped_siblings_match_by_subtree_content() {
		let repeated = |identity: &str, revision: &str| {
			block("entry", vec![scalar(identity), scalar(revision)])
				.with_parent_scoped_anchor("entry.kind", "modifier")
		};
		let left = NormalizedTree::from_root(block(
			"root",
			vec![repeated("a", "left-a"), repeated("b", "left-b")],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![repeated("b", "right-b"), repeated("a", "right-a")],
		))
		.unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), Some(NodeId::new(4)));
		assert_eq!(matching.get_from_left(NodeId::new(4)), Some(NodeId::new(1)));
	}

	#[test]
	fn ambiguous_repeated_parent_scoped_siblings_do_not_fall_back_to_position() {
		let repeated = |value: &str| {
			block("entry", vec![scalar("shared"), scalar(value)])
				.with_parent_scoped_anchor("entry.kind", "modifier")
		};
		let left =
			NormalizedTree::from_root(block("root", vec![repeated("left-a"), repeated("left-b")]))
				.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![repeated("right-a"), repeated("right-b")],
		))
		.unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), None);
		assert_eq!(matching.get_from_left(NodeId::new(4)), None);
		assert_eq!(matching.ambiguities().len(), 2);
	}

	#[test]
	fn dissimilar_repeated_parent_scoped_siblings_remain_independent() {
		let repeated = |child_kind: &str, value: &str| {
			block("entry", vec![block(child_kind, vec![scalar(value)])])
				.with_parent_scoped_anchor("entry.kind", "modifier")
		};
		let left = NormalizedTree::from_root(block(
			"root",
			vec![repeated("left-a", "one"), repeated("left-b", "two")],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![repeated("right-a", "three"), repeated("right-b", "four")],
		))
		.unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), None);
		assert_eq!(matching.get_from_left(NodeId::new(4)), None);
		assert!(matching.ambiguities().is_empty());
	}

	#[test]
	fn residual_repeated_siblings_do_not_fall_back_to_key_only_matching() {
		let repeated = |value: &str| {
			block("entry", vec![scalar(value)])
				.with_parent_scoped_anchor("entry.kind", "trade_goods")
		};
		let left =
			NormalizedTree::from_root(block("root", vec![repeated("ivory"), repeated("cloves")]))
				.unwrap();
		let right =
			NormalizedTree::from_root(block("root", vec![repeated("ivory"), repeated("fur")]))
				.unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), Some(NodeId::new(1)));
		assert_eq!(matching.get_from_left(NodeId::new(3)), None);
		assert_eq!(matching.get_from_right(NodeId::new(3)), None);
		assert!(matching.ambiguities().is_empty());
	}

	#[test]
	fn required_slot_matches_divergent_compatible_children() {
		let slot = |field: &str| {
			NormalizedTree::from_root(
				block(
					"slot",
					vec![block("block", vec![block(field, vec![scalar("yes")])])],
				)
				.with_child_cardinality(ChildCardinality::ExactlyOne),
			)
			.unwrap()
		};
		let left = slot("left_only");
		let right = slot("right_only");

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), Some(NodeId::new(1)));
		assert_eq!(
			matching.record(NodeId::new(1)).unwrap().kind,
			MatchKind::RequiredSlot
		);
	}

	#[test]
	fn required_slot_reports_a_preexisting_inconsistent_child_match() {
		let tree = || {
			NormalizedTree::from_root(block(
				"root",
				vec![
					block("slot", vec![block("block", vec![scalar("value")])])
						.with_child_cardinality(ChildCardinality::ExactlyOne),
					block("block", vec![scalar("other")]),
				],
			))
			.unwrap()
		};
		let left = tree();
		let right = tree();
		let mut matching = Matching::default();
		for (left, right) in [(0, 0), (1, 1), (2, 4)] {
			matching.insert(MatchRecord {
				left: NodeId::new(left),
				right: NodeId::new(right),
				kind: MatchKind::Recovery,
				score: 500_000,
			});
		}

		match_required_slots(&left, &right, &mut matching);

		assert_eq!(matching.ambiguities().len(), 1);
		assert_eq!(matching.ambiguities()[0].left, NodeId::new(2));
		assert_eq!(matching.ambiguities()[0].candidates, vec![NodeId::new(2)]);
	}

	#[test]
	fn incompatible_semantic_anchors_never_match() {
		let left = NormalizedTree::from_root(block(
			"root",
			vec![block("option", vec![scalar("same")]).with_anchor("option.name", "A")],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![block("option", vec![scalar("same")]).with_anchor("option.name", "B")],
		))
		.unwrap();
		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), None);
	}

	#[test]
	fn repeated_control_flow_matches_by_descendant_overlap() {
		let left = NormalizedTree::from_root(block(
			"option",
			vec![
				block("if", vec![scalar("republican"), scalar("adm")]),
				block("if", vec![scalar("old_candidate"), scalar("loyalty")]),
			],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"option",
			vec![
				block("if", vec![scalar("republican"), scalar("adm")]),
				block("if", vec![scalar("upgraded_candidate"), scalar("mil")]),
				block("if", vec![scalar("spread_target"), scalar("support")]),
			],
		))
		.unwrap();
		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), Some(NodeId::new(1)));
		assert_ne!(matching.get_from_left(NodeId::new(4)), Some(NodeId::new(1)));
	}

	#[test]
	fn equal_soft_signatures_match_changed_siblings() {
		let left = NormalizedTree::from_root(block(
			"root",
			vec![block("chain", vec![scalar("left")]).with_signature("guard:a")],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![
				block("chain", vec![scalar("unrelated")]).with_signature("guard:b"),
				block("chain", vec![scalar("right")]).with_signature("guard:a"),
			],
		))
		.unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), Some(NodeId::new(3)));
	}

	#[test]
	fn duplicate_soft_signatures_are_reported_as_ambiguous() {
		let left = NormalizedTree::from_root(block(
			"root",
			vec![block("chain", vec![scalar("left")]).with_signature("guard:a")],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![
				block("chain", vec![scalar("right-1")]).with_signature("guard:a"),
				block("chain", vec![scalar("right-2")]).with_signature("guard:a"),
			],
		))
		.unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), None);
		assert_eq!(matching.ambiguities().len(), 1);
	}

	#[test]
	fn competing_left_soft_signatures_are_reported_as_ambiguous() {
		let left = NormalizedTree::from_root(block(
			"root",
			vec![
				block("chain", vec![scalar("left-1")]).with_signature("guard:a"),
				block("chain", vec![scalar("left-2")]).with_signature("guard:a"),
			],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![block("chain", vec![scalar("right")]).with_signature("guard:a")],
		))
		.unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), None);
		assert_eq!(matching.get_from_left(NodeId::new(3)), None);
		assert_eq!(matching.ambiguities().len(), 2);
	}

	#[test]
	fn positional_recovery_matches_a_fully_rewritten_signed_child() {
		let left = NormalizedTree::from_root(block(
			"root",
			vec![block("branch", vec![scalar("left")]).with_signature("guard:left")],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![block("branch", vec![scalar("right")]).with_signature("guard:right")],
		))
		.unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(NodeId::new(1)), Some(NodeId::new(1)));
		assert_eq!(
			matching.record(NodeId::new(1)).unwrap().kind,
			MatchKind::Recovery
		);
	}

	#[test]
	fn changed_root_is_linked_without_claiming_an_exact_match() {
		let left = NormalizedTree::from_root(block("root", vec![scalar("left")])).unwrap();
		let right = NormalizedTree::from_root(block("root", vec![scalar("right")])).unwrap();

		let matching = TreeMatcher::default().match_trees(&left, &right);

		assert_eq!(matching.get_from_left(left.root()), Some(right.root()));
		assert_eq!(
			matching.record(left.root()).unwrap().kind,
			MatchKind::Recovery
		);
	}

	#[test]
	fn base_matchings_compose_into_a_left_right_seed() {
		let base = NormalizedTree::from_root(block(
			"root",
			vec![block("entry", vec![scalar("old")]).with_anchor("entry", "a")],
		))
		.unwrap();
		let left = NormalizedTree::from_root(block(
			"root",
			vec![block("entry", vec![scalar("left")]).with_anchor("entry", "a")],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![block("entry", vec![scalar("right")]).with_anchor("entry", "a")],
		))
		.unwrap();
		let matcher = TreeMatcher::default();
		let seed = Matching::compose_through_base(
			&matcher.match_trees(&base, &left),
			&matcher.match_trees(&base, &right),
		);

		assert_eq!(seed.get_from_left(NodeId::new(1)), Some(NodeId::new(1)));
		assert_eq!(
			seed.record(NodeId::new(1)).unwrap().kind,
			MatchKind::Recovery
		);
	}

	#[test]
	fn matching_is_deterministic() {
		let left = NormalizedTree::from_root(block(
			"root",
			vec![
				block("if", vec![scalar("a")]),
				block("if", vec![scalar("b")]),
			],
		))
		.unwrap();
		let right = NormalizedTree::from_root(block(
			"root",
			vec![
				block("if", vec![scalar("b")]),
				block("if", vec![scalar("a")]),
			],
		))
		.unwrap();
		let matcher = TreeMatcher::default();

		assert_eq!(
			matcher.match_trees(&left, &right),
			matcher.match_trees(&left, &right)
		);
	}
}
