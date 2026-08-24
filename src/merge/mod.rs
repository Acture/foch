//! Merge analysis, review, and commit lifecycle.

pub(crate) mod address_patch;
pub(crate) mod analyze;
#[cfg(test)]
mod architecture_tests;
pub(crate) mod backend;
pub(crate) mod boolean;
mod commit;
pub(crate) mod error;
pub(crate) mod gui;
pub(crate) mod kernel;
pub(crate) mod kernel_adapter;
pub(crate) mod model;
pub(crate) mod namespace;
pub(crate) mod output;
#[cfg(test)]
mod patch_real_mods;
pub(crate) mod path_plan;
pub(crate) mod planning;
pub(crate) mod resolution;
pub(crate) mod semantic_fingerprint;
pub(crate) mod structured;

#[cfg(test)]
pub(crate) use address_patch::patch_apply;
pub(crate) use address_patch::{normalize, patch, patch_merge};
pub use analyze::{
	AnalysisStatusView, AnalyzedMerge, CancellationToken, MergeAnalysis, MergeAnalysisOptions,
	MergeAnalysisStage, MergeAnalysisStatus, MergeProgress, MergeStatusView, NoopProgressObserver,
	ProgressObserver, analyze_merge, run_merge_for_evaluation, run_merge_with_options,
};
pub use commit::{CommitAuthorization, CommitResult, ReplacementTarget};
pub use error::MergeError;
pub use kernel_adapter::{MergeBackendDescriptor, MergeBackendId};
#[allow(unused_imports)]
pub(crate) use output::{localisation_merge, materialize, stale_vanilla};
pub use path_plan::{PathPlanOptions, run_merge_plan, run_merge_plan_with_options};
pub(crate) use planning::dag;
#[allow(unused_imports)]
pub use resolution::conflict_handler::{
	ConflictDecision, ConflictHandler, ConflictMetadataCandidate, ConflictMetadataView,
	ConflictViewRequirement, InteractiveCliHandler, MetadataConflictDecision,
};
pub use resolution::conflict_view::{CandidateView, ConflictView};
pub(crate) use resolution::{conflict_handler, conflict_view, handler_registry};
