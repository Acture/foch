pub(crate) mod conflict_handler;
pub(crate) mod dag;
pub(crate) mod error;
pub(crate) mod execute;
pub(crate) mod handler_registry;
pub(crate) mod localisation_merge;
pub(crate) mod materialize;
pub(crate) mod namespace;
pub(crate) mod normalize;
pub(crate) mod patch;
pub(crate) mod patch_apply;
pub(crate) mod patch_deps;
pub(crate) mod patch_merge;
#[cfg(test)]
mod patch_real_mods;
pub(crate) mod plan;
pub(crate) mod stale_vanilla;

pub use conflict_handler::{ConflictDecision, ConflictHandler, InteractiveCliHandler};
pub use error::MergeError;
pub use execute::{
	AnalysisStatusView, MergeExecuteOptions, MergeExecutionResult, MergeStatusView,
	run_merge_with_options,
};
pub use patch::ClausewitzPatch;
pub use patch_merge::{AttributedPatch, PatchAddress, PatchConflict};
pub use plan::{run_merge_plan, run_merge_plan_with_options};
