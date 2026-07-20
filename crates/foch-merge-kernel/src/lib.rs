//! Parser-independent structured merge primitives.

mod class_mapping;
mod conflict;
mod matching;
mod merge;
mod pcs;
mod policy;
mod provenance;
mod tree;

pub use class_mapping::{ClassId, ClassMapping, RevisionClass};
pub use conflict::{ConflictKind, MergeOutcome, MergeTimings, StructuralConflict};
pub use matching::{AmbiguousMatch, MatchKind, MatchRecord, MatcherConfig, Matching, TreeMatcher};
pub use merge::{three_way_merge, three_way_merge_with_policy};
pub use pcs::{PcsNode, PcsTriple};
pub use policy::{
	ChildSetContext, ConservativeMergePolicy, DeleteModifyContext, DeleteUnchangedContext,
	MergePolicy, NodeConflictContext, PolicyDecision,
};
pub use provenance::{RevisionNode, SourceSet};
pub use tree::{
	ChildCardinality, ChildOrder, NodeId, NormalizedNode, NormalizedTree, RevisionId, SemanticKey,
	SemanticKeyScope, SubtreeHash, TreeError, TreeNode,
};
