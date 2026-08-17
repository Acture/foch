mod ast_adapter;
mod control_flow;
mod definition_module;
mod merge;
mod observer;
mod policy;
mod tree_kernel;
mod trivia;

pub use ast_adapter::AstAdapterError;
pub(crate) use ast_adapter::{semantic_node_address, top_level_assignment_key};
pub(crate) use definition_module::merge_clausewitz_definition_module_n_way_with_resolutions;
pub use definition_module::{
	ClausewitzDefinitionModuleOutcome, merge_clausewitz_definition_module,
};
#[cfg(test)]
pub(crate) use merge::merge_event_files;
pub(crate) use merge::{
	ClausewitzKernelFacts, clausewitz_files_semantically_equivalent,
	clausewitz_statements_semantically_equivalent, merge_clausewitz_files_n_way_with_resolutions,
	merge_event_files_n_way_with_resolutions, normalize_clausewitz_partition,
};
pub use merge::{
	ClausewitzMergeOutcome, ClausewitzMergeTimings, ClausewitzScalarReduction,
	canonicalize_clausewitz_file, merge_clausewitz_files, merge_clausewitz_files_n_way,
};
pub(crate) use observer::observe_merge_trace;
pub(crate) use tree_kernel::{
	ClausewitzFileAdapter, ClausewitzFileJoin, DefinitionModuleAdapter, DefinitionModuleJoin,
	EventFileAdapter, EventFileJoin, TreeDagProtocol, TreeDagState, TreeJoinProtocol,
	TreeMergeUnit, TreePartitionAdapter, semantic_conflict_id, semantic_conflict_view,
};

#[cfg(test)]
mod tests;
