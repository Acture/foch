pub(crate) mod error;
pub(crate) mod execute;
pub(crate) mod localisation_merge;
pub(crate) mod materialize;
pub(crate) mod namespace;
pub(crate) mod patch_engine;
#[cfg(test)]
mod patch_real_mods;
pub(crate) mod plan;
pub(crate) mod planning;
pub(crate) mod resolution;
pub(crate) mod stale_vanilla;

pub use error::MergeError;
pub use execute::{
	AnalysisStatusView, MergeExecuteOptions, MergeExecutionResult, MergeStatusView,
	run_merge_with_options,
};
pub(crate) use patch_engine::{normalize, patch, patch_apply, patch_merge};
pub use plan::{run_merge_plan, run_merge_plan_with_options};
pub(crate) use planning::{dag, patch_deps};
pub use resolution::conflict_handler::{ConflictDecision, ConflictHandler, InteractiveCliHandler};
pub use resolution::conflict_view::{CandidateView, ConflictView};
pub(crate) use resolution::{conflict_handler, conflict_view, handler_registry};
