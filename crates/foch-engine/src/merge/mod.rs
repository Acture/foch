#[cfg(test)]
mod architecture_tests;
pub(crate) mod boolean;
pub(crate) mod cwt_suggestions;
pub(crate) mod error;
pub(crate) mod execute;
pub(crate) mod gui;
pub(crate) mod kernel;
pub(crate) mod model;
pub(crate) mod namespace;
pub(crate) mod output;
pub(crate) mod patch_engine;
#[cfg(test)]
mod patch_real_mods;
pub(crate) mod plan;
pub(crate) mod planning;
pub(crate) mod resolution;
pub(crate) mod semantic_fingerprint;
pub(crate) mod structured;

pub use error::MergeError;
pub use execute::{
	AnalysisStatusView, MergeExecuteOptions, MergeExecutionResult, MergeStatusView, PreparedMerge,
	prepare_merge_with_options, run_merge_for_evaluation, run_merge_with_options,
};
pub use kernel::MergeEvaluationKernel;
#[allow(unused_imports)]
pub(crate) use output::{localisation_merge, materialize, stale_vanilla};
#[cfg(test)]
pub(crate) use patch_engine::patch_apply;
pub(crate) use patch_engine::{normalize, patch, patch_merge};
pub use plan::{run_merge_plan, run_merge_plan_with_options};
pub(crate) use planning::dag;
#[allow(unused_imports)]
pub use resolution::conflict_handler::{
	ConflictDecision, ConflictHandler, ConflictMetadataCandidate, ConflictMetadataView,
	ConflictViewRequirement, InteractiveCliHandler, MetadataConflictDecision,
};
pub use resolution::conflict_view::{CandidateView, ConflictView};
pub(crate) use resolution::{conflict_handler, conflict_view, handler_registry};
pub use structured::{
	AstAdapterError, ClausewitzDefinitionModuleOutcome, ClausewitzMergeOutcome,
	ClausewitzMergeTimings, ClausewitzScalarReduction, canonicalize_clausewitz_file,
	merge_clausewitz_definition_module, merge_clausewitz_files,
};
