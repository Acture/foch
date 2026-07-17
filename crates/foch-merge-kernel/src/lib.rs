//! Parser-independent structured merge primitives.

mod class_mapping;
mod conflict;
mod matching;
mod merge;
mod pcs;
mod provenance;
mod tree;

pub use class_mapping::{ClassId, ClassMapping, RevisionClass};
pub use conflict::{ConflictKind, MergeOutcome, MergeTimings, StructuralConflict};
pub use matching::{AmbiguousMatch, MatchKind, MatchRecord, MatcherConfig, Matching, TreeMatcher};
pub use merge::three_way_merge;
pub use pcs::{PcsNode, PcsTriple};
pub use provenance::{RevisionNode, SourceSet};
pub use tree::{
	ChildOrder, NodeId, NormalizedNode, NormalizedTree, RevisionId, SemanticKey, SubtreeHash,
	TreeError, TreeNode,
};
