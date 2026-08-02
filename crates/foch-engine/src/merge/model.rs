use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::parser::AstStatement;
use foch_merge_kernel::{
	ConflictResolution, MergeOutcome, NodeId, NormalizedTree, RevisionDelta, RevisionId,
	SourceNodeRef, StructuralConflict,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VanillaBaseMode {
	Required,
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
	pub outcome: MergeOutcome,
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
pub(crate) enum MergeOutputDirective {
	UseFile(PathBuf),
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
