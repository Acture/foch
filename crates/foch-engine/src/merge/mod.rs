pub(crate) mod address_patch;
#[cfg(test)]
mod architecture_tests;
pub(crate) mod backend;
pub(crate) mod boolean;
pub(crate) mod error;
pub(crate) mod execute;
pub(crate) mod gui;
pub(crate) mod kernel_adapter;
pub(crate) mod model;
pub(crate) mod namespace;
pub(crate) mod output;
#[cfg(test)]
mod patch_real_mods;
pub(crate) mod plan;
pub(crate) mod planning;
pub(crate) mod resolution;
pub(crate) mod semantic_fingerprint;
pub(crate) mod structured;

#[cfg(test)]
pub(crate) use address_patch::patch_apply;
pub(crate) use address_patch::{normalize, patch, patch_merge};
pub use error::MergeError;
pub use execute::{
	AnalysisStatusView, AnalyzedMerge, CancellationToken, CommitAuthorization, CommitResult,
	MergeAnalysis, MergeAnalysisOptions, MergeAnalysisStage, MergeAnalysisStatus, MergeProgress,
	MergeStatusView, NoopProgressObserver, ProgressObserver, ReplacementTarget, analyze_merge,
	run_merge_for_evaluation, run_merge_with_options,
};
pub use kernel_adapter::{MergeBackendDescriptor, MergeBackendId};
#[allow(unused_imports)]
pub(crate) use output::{localisation_merge, materialize, stale_vanilla};
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
