mod ast_adapter;
mod control_flow;
mod dag_join;
mod definition_module;
mod merge;
mod policy;
mod trivia;

pub use ast_adapter::AstAdapterError;
pub(crate) use dag_join::{StructuredDagProtocol, StructuredDagState, StructuredJoinKind};
pub use definition_module::{
	ClausewitzDefinitionModuleOutcome, merge_clausewitz_definition_module,
};
pub(crate) use merge::merge_event_files;
pub use merge::{
	ClausewitzConflictSummary, ClausewitzMergeOutcome, ClausewitzMergeTimings,
	ClausewitzScalarReduction, canonicalize_clausewitz_file, merge_clausewitz_files,
};

#[cfg(test)]
mod tests;
