//! Public engine facade.
//!
//! `foch-cli` and downstream consumers should import engine functionality from
//! this root module. Internal modules remain private so repository layout can
//! change without becoming part of the API contract.

mod base_data;
mod cache;
mod config;
mod emit;
mod graph;
mod merge;
mod request;
mod run_checks;
mod runtime;
mod simplify;
mod workspace;

pub use base_data::{
	BASE_DATA_DIR_ENV, BASE_DATA_RELEASE_BASE_URL_ENV, BASE_DATA_SCHEMA_VERSION,
	BaseAnalysisSnapshot, BaseBuildObserver, BaseBuildProfile, BaseDataSource,
	BaseSnapshotBuildResult, INSTALLED_COVERAGE_FILE_NAME, InstalledBaseDataEntry,
	InstalledBaseSnapshot, InstalledBaseSnapshotIdentity, ReleaseArtifactOutput,
	ReleaseDataManifest, SnapshotBundleOutput, build_base_snapshot,
	build_base_snapshot_with_observer, default_release_tag, install_built_snapshot,
	install_snapshot_from_release, installed_base_snapshot_identity, list_installed_base_data,
	load_installed_base_snapshot, resolve_game_root_and_version, write_release_artifacts,
	write_snapshot_bundle,
};
pub use cache::{
	CacheEntryInfo, CacheError, CacheLayer, CacheLayerEntryInfo, CacheLayerOps, CacheStats,
	CachedModsetResult, EvictionStats, ModsetCache, all_layers, cache_cap_bytes,
	default_dag_base_cache_dir, default_foch_cache_dir, default_mod_diff_cache_dir,
	default_mod_parse_cache_dir, default_modset_cache_dir, default_modset_cache_root_dir,
};
pub use config::{
	CONFIG_DIR_ENV, Config, ValidationItem, ValidationStatus, get_config_dir_path,
	load_or_init_config,
};
pub use graph::{
	GraphArtifactFormat, GraphBuildOptions, GraphBuildSummary, GraphModeSelection,
	GraphRootSelector, GraphScopeSelection, ModuleReport, SEMANTIC_GRAPH_PROGRESS_TARGET,
	merge_trace_edges_from_trace, run_graph_with_options, run_module_report, write_module_report,
};
pub use merge::{
	AnalysisStatusView, AstAdapterError, CandidateView, ClausewitzConflictSummary,
	ClausewitzDefinitionModuleOutcome, ClausewitzMergeOutcome, ClausewitzMergeTimings,
	ClausewitzScalarReduction, ConflictDecision, ConflictHandler, ConflictView,
	InteractiveCliHandler, MergeError, MergeExecuteOptions, MergeExecutionResult, MergeKernelMode,
	MergeStatusView, canonicalize_clausewitz_file, merge_clausewitz_definition_module,
	merge_clausewitz_files, run_merge_plan, run_merge_plan_with_options, run_merge_with_options,
	run_merge_with_options_and_kernel,
};
pub use request::{CheckRequest, MergePlanOptions, RunOptions, WorkspaceSource};
pub use run_checks::{CHECK_PROGRESS_TARGET, run_checks, run_checks_with_options};
pub use runtime::{RuntimeState, build_runtime_state_for_request};
pub use simplify::{SimplifyOptions, SimplifyReport, SimplifySummary, run_simplify_with_options};
pub use workspace::{
	FileFilter, WorkspaceResolveSummary, WorkspaceResolvedMod, WorkspaceSession, WorkspaceTarget,
	WorkspaceTargetRole, resolve_workspace_summary, resolve_workspace_targets,
};
