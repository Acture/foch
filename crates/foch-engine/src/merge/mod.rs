pub(crate) mod boolean;
pub(crate) mod cwt_suggestions;
pub(crate) mod error;
pub(crate) mod execute;
pub(crate) mod namespace;
pub(crate) mod output;
pub(crate) mod patch_engine;
#[cfg(test)]
mod patch_real_mods;
pub(crate) mod plan;
pub(crate) mod planning;
pub(crate) mod resolution;
pub(crate) mod structured;

pub use error::MergeError;
pub use execute::{
	AnalysisStatusView, MergeExecuteOptions, MergeExecutionResult, MergeStatusView,
	run_merge_with_options, run_merge_with_options_and_kernel,
};
#[allow(unused_imports)]
pub(crate) use output::{localisation_merge, materialize, stale_vanilla};
pub(crate) use patch_engine::{normalize, patch, patch_apply, patch_merge};
pub use plan::{run_merge_plan, run_merge_plan_with_options};
pub(crate) use planning::{dag, patch_deps};
pub use resolution::conflict_handler::{ConflictDecision, ConflictHandler, InteractiveCliHandler};
pub use resolution::conflict_view::{CandidateView, ConflictView};
pub(crate) use resolution::{conflict_handler, conflict_view, handler_registry};
pub use structured::{
	AstAdapterError, ClausewitzConflictSummary, ClausewitzMergeOutcome, ClausewitzMergeTimings,
	MergeKernelMode, merge_clausewitz_files,
};
