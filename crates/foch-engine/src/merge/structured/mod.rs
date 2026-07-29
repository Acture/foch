mod ast_adapter;
mod control_flow;
mod definition_module;
mod merge;
mod policy;
mod tree_kernel;
mod trivia;

pub use ast_adapter::AstAdapterError;
pub(crate) use definition_module::merge_clausewitz_definition_module_with_source_selections;
pub use definition_module::{
	ClausewitzDefinitionModuleOutcome, merge_clausewitz_definition_module,
};
#[cfg(test)]
pub(crate) use merge::merge_event_files;
pub use merge::{
	ClausewitzConflictSummary, ClausewitzMergeOutcome, ClausewitzMergeTimings,
	ClausewitzScalarReduction, canonicalize_clausewitz_file, merge_clausewitz_files,
};
pub(crate) use merge::{
	apply_nary_scalar_reducers, merge_clausewitz_files_with_source_selections,
	merge_event_files_with_source_selections,
};
pub(crate) use tree_kernel::{
	ClausewitzFileAdapter, DefinitionModuleAdapter, EventFileAdapter, TreeConflictCandidate,
	TreeDagProtocol, TreeDagState, TreeMergeAdapter, TreeMergeUnit,
};

#[cfg(test)]
mod tests;
