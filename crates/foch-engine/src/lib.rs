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
	BASE_DATA_DIR_ENV, BASE_DATA_RELEASE_BASE_URL_ENV, BaseAnalysisSnapshot, BaseBuildObserver,
	BaseBuildProfile, BaseDataSource, BaseSnapshotBuildResult, INSTALLED_COVERAGE_FILE_NAME,
	InstalledBaseDataEntry, InstalledBaseSnapshot, ReleaseArtifactOutput, ReleaseDataManifest,
	SnapshotBundleOutput, build_base_snapshot, build_base_snapshot_with_observer,
	default_release_tag, install_built_snapshot, install_snapshot_from_release,
	list_installed_base_data, resolve_game_root_and_version, write_release_artifacts,
	write_snapshot_bundle,
};
pub use cache::{
	CacheEntryInfo, CacheError, CacheStats, CachedModsetResult, ModsetCache,
	default_dag_base_cache_dir, default_foch_cache_dir, default_mod_diff_cache_dir,
	default_mod_parse_cache_dir, default_modset_cache_dir, default_modset_cache_root_dir,
};
pub use config::{
	CONFIG_DIR_ENV, Config, ValidationItem, ValidationStatus, get_config_dir_path,
	load_or_init_config,
};
pub use graph::{
	GraphArtifactFormat, GraphBuildOptions, GraphBuildSummary, GraphModeSelection,
	GraphRootSelector, GraphScopeSelection, SEMANTIC_GRAPH_PROGRESS_TARGET, run_graph_with_options,
};
pub use merge::{
	AnalysisStatusView, CandidateView, ConflictDecision, ConflictHandler, ConflictView,
	InteractiveCliHandler, MergeError, MergeExecuteOptions, MergeExecutionResult, MergeStatusView,
	run_merge_plan, run_merge_plan_with_options, run_merge_with_options,
};
pub use request::{CheckRequest, MergePlanOptions, RunOptions};
pub use run_checks::{CHECK_PROGRESS_TARGET, run_checks, run_checks_with_options};
pub use simplify::{SimplifyOptions, SimplifyReport, SimplifySummary, run_simplify_with_options};
pub use workspace::{FileFilter, WorkspaceSession};
