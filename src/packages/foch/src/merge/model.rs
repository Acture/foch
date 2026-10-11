use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::game::eu4::script::parser::AstStatement;
use crate::merge::kernel::{
	ConflictResolution, MergeOutcome, NodeId, NormalizedTree, RevisionDelta, RevisionId,
	SourceNodeRef, StructuralConflict, TreeMatcher,
};
use crate::model::HandlerResolutionRecord;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VanillaBaseMode {
	Required,
	/// The base-game inventory was included and the exact semantic unit was
	/// absent. The empty ancestor is therefore observed product state, not a
	/// caller request to ignore vanilla.
	KnownAbsent,
	ExplicitlyDisabled,
}

impl VanillaBaseMode {
	pub(crate) fn from_include_game_base(include_game_base: bool) -> Self {
		if include_game_base {
			Self::Required
		} else {
			Self::ExplicitlyDisabled
		}
	}

	pub(crate) fn requires_non_empty(self) -> bool {
		self == Self::Required
	}
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SemanticMergeSource {
	pub source_id: String,
	pub precedence: usize,
}

/// Cumulative semantic ancestry for one normalized output node.
///
/// This is deliberately separate from [`SemanticMergeSource`]-based adopted
/// provenance: replacing a vanilla value adopts the mod's value while the
/// semantic identity still descends from vanilla.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum SemanticOrigin {
	Vanilla,
	Mod(SemanticMergeSource),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum SemanticPartitionId {
	File,
	Definition(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticDeltaPartition {
	pub partition: SemanticPartitionId,
	pub base_tree: NormalizedTree,
	pub revision_tree: NormalizedTree,
	pub delta: RevisionDelta,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticSourceDelta {
	pub source: SemanticMergeSource,
	pub partitions: Vec<SemanticDeltaPartition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticPartitionLineage {
	pub tree: NormalizedTree,
	pub sources: BTreeMap<NodeId, BTreeSet<SemanticMergeSource>>,
	pub origins: BTreeMap<NodeId, BTreeSet<SemanticOrigin>>,
}

fn same_scalar_value(kind: &str, left: &Option<String>, right: &Option<String>) -> bool {
	if left == right {
		return true;
	}
	kind == crate::merge::structured::NUMBER_KIND
		&& match (left, right) {
			(Some(left), Some(right)) => matches!(
				(left.parse::<f64>(), right.parse::<f64>()),
				(Ok(left), Ok(right)) if left == right
			),
			_ => false,
		}
}

impl SemanticPartitionLineage {
	/// This lineage on `tree`, a re-normalization of the same content whose
	/// children may be ordered differently: every node of `tree` must match a
	/// lineage node of the same kind and value, which lends it that node's
	/// sources and origins. A number counts as the same value in another
	/// spelling, since numeric canonicalization depends on the schema context
	/// a re-normalized chain gives it. `None` when the content differs.
	pub(crate) fn rebased_onto(&self, tree: &NormalizedTree) -> Option<Self> {
		if self.tree.len() != tree.len() {
			return None;
		}
		let matching = TreeMatcher::default().match_trees(&self.tree, tree);
		let mut sources = BTreeMap::new();
		let mut origins = BTreeMap::new();
		for (node, rebased) in tree.nodes() {
			let original = matching.get_from_right(node)?;
			let lineage_node = self.tree.node(original).ok()?;
			if lineage_node.kind != rebased.kind
				|| !same_scalar_value(&rebased.kind, &lineage_node.value, &rebased.value)
			{
				return None;
			}
			if let Some(node_sources) = self.sources.get(&original) {
				sources.insert(node, node_sources.clone());
			}
			origins.insert(node, self.origins.get(&original)?.clone());
		}
		Some(Self {
			tree: tree.clone(),
			sources,
			origins,
		})
	}

	pub(crate) fn vanilla(tree: NormalizedTree) -> Self {
		let origins = tree
			.nodes()
			.map(|(node, _)| (node, BTreeSet::from([SemanticOrigin::Vanilla])))
			.collect();
		Self {
			tree,
			sources: BTreeMap::new(),
			origins,
		}
	}
}

/// Parser-independent facts for one normalized merge partition.
///
/// Definition modules may produce one partition per top-level definition, so
/// node IDs are scoped by this value and must never be compared across entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticMergeFacts {
	pub partition: SemanticPartitionId,
	pub sources: BTreeMap<RevisionId, SemanticMergeSource>,
	pub base_tree: NormalizedTree,
	pub revision_trees: BTreeMap<RevisionId, NormalizedTree>,
	pub input_rewrites: BTreeMap<RevisionId, InputRewrite>,
	pub outcome: MergeOutcome,
}

/// A revision the merge read in a rewritten form. Its lineage is that of the
/// mod's own file, `original`; every node of the rewritten tree traces to the
/// original node it was matched to, or else to the nearest matched ancestor,
/// which is the part of the mod's file the rewrite replaced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InputRewrite {
	pub original: NormalizedTree,
	pub nodes: BTreeMap<NodeId, NodeId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticConflictCandidate {
	pub source: SourceNodeRef,
	pub source_id: String,
	pub precedence: usize,
	pub statement: Option<AstStatement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticMergeConflict {
	pub conflict: StructuralConflict,
	pub reason: String,
	pub base_statement: Option<AstStatement>,
	pub candidates: Vec<SemanticConflictCandidate>,
	pub source_selectable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExternalFileResolution {
	Frozen(PathBuf),
	Live(PathBuf),
}

impl ExternalFileResolution {
	pub(crate) fn source_path(&self) -> &PathBuf {
		match self {
			Self::Frozen(path) | Self::Live(path) => path,
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MergeOutputDirective {
	UseFile(ExternalFileResolution),
	KeepExisting,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticMergeComputation {
	pub statements: Vec<AstStatement>,
	pub source_deltas: Vec<SemanticSourceDelta>,
	pub merge_facts: Vec<SemanticMergeFacts>,
	pub partition_lineage: BTreeMap<SemanticPartitionId, SemanticPartitionLineage>,
	pub unresolved_conflicts: Vec<SemanticMergeConflict>,
	pub handler_resolutions: Vec<HandlerResolutionRecord>,
	pub resolved_conflict_ids: Vec<String>,
	pub conflict_resolutions: Vec<ConflictResolution>,
	pub output_directives: Vec<MergeOutputDirective>,
}

impl SemanticMergeComputation {
	pub(crate) fn push_output_directive(&mut self, directive: MergeOutputDirective) {
		if !self.output_directives.contains(&directive) {
			self.output_directives.push(directive);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::merge::kernel::TreeNode;

	fn tree(values: &[&str]) -> NormalizedTree {
		NormalizedTree::from_root(TreeNode::branch(
			"root",
			values
				.iter()
				.map(|value| TreeNode::leaf("leaf", *value))
				.collect(),
		))
		.expect("valid tree")
	}

	fn value_of(tree: &NormalizedTree, node: NodeId) -> Option<String> {
		tree.node(node).expect("node").value.clone()
	}

	#[test]
	fn lineage_follows_reordered_content_and_rejects_changed_content() {
		let original_tree = tree(&["a", "b"]);
		let mut original = SemanticPartitionLineage::vanilla(original_tree.clone());
		for (node, _) in original_tree.nodes() {
			if let Some(value) = value_of(&original_tree, node) {
				original.sources.insert(
					node,
					BTreeSet::from([SemanticMergeSource {
						source_id: format!("mod_{value}"),
						precedence: 1,
					}]),
				);
			}
		}

		let reordered = original
			.rebased_onto(&tree(&["b", "a"]))
			.expect("the same content in another order");
		for (node, sources) in &reordered.sources {
			let value = value_of(&reordered.tree, *node).expect("a sourced leaf");
			let source = &sources.iter().next().expect("one source").source_id;
			assert_eq!(source, &format!("mod_{value}"));
		}
		assert_eq!(reordered.origins.len(), reordered.tree.len());
		assert!(original.rebased_onto(&tree(&["a", "c"])).is_none());
		let number = |text: &str| {
			NormalizedTree::from_root(TreeNode::branch(
				"root",
				vec![TreeNode::leaf(crate::merge::structured::NUMBER_KIND, text)],
			))
			.expect("valid tree")
		};
		let spelled = SemanticPartitionLineage::vanilla(number("0.2"));
		assert!(spelled.rebased_onto(&number("0.200")).is_some());
		assert!(spelled.rebased_onto(&number("0.201")).is_none());
		assert!(original.rebased_onto(&tree(&["a"])).is_none());
	}
}
