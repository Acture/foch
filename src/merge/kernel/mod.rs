//! Parser-independent structured merge primitives.

mod class_mapping;
mod conflict;
mod decision;
mod delta;
mod matching;
mod nway;
mod nway_merge;
mod nway_policy;
mod pcs;
mod policy;
mod provenance;
mod selection;
mod tree;

pub use class_mapping::{ClassId, ClassMapping, RevisionClass};
pub use conflict::{
	ConflictKind, ConflictResolution, MergeOutcome, MergeTimings, RevisionSourceRef,
	StructuralConflict, StructuralConflictDraft,
};
pub use decision::{
	MergeDecisionEvidence, MergeDecisionReason, MergeDecisionResult, MergePolicyKind,
};
pub use delta::{DeltaOperation, MergeInputId, RevisionDelta};
pub use matching::{Matching, TreeMatcher};
pub use nway::{
	MergeRevision, NWayClassFacts, NWayClassSelection, NWayCorrespondence, NWayInputError,
	NWayScalarSynthesis, NWaySelectionPlan,
};
#[cfg(test)]
pub use nway_merge::n_way_merge;
pub use nway_merge::{
	NWayMergeError, n_way_merge_with_policy, n_way_merge_with_policy_and_resolutions,
};
#[cfg(test)]
pub use policy::ConservativeMergePolicy;
pub use policy::{MergePolicy, NWayClassContext, NWayDeleteContext, NWayNodeView, PolicyDecision};
pub use provenance::{RevisionNode, SourceSet};
pub use selection::{ConflictNodeId, SourceNodeRef};
pub use tree::{
	ChildCardinality, ChildOrder, NodeId, NormalizedNode, NormalizedTree, RevisionId, SemanticKey,
	SemanticKeyLineage, SemanticKeyMatchMode, SemanticKeyScope, SubtreeHash, TreeError, TreeNode,
};
