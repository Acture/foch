pub mod analysis;
pub mod analysis_version;
pub(crate) mod analyzer;
pub mod base_data;
pub mod documents;
pub mod eu4_builtin;
pub mod graph;
pub mod localisation;
pub mod merge;
pub mod mod_cache;
pub mod model;
pub mod param_contracts;
pub mod parser;
pub mod report;
pub(crate) mod runtime;
pub mod rules;
pub mod semantic_index;
pub mod simplify;
pub(crate) mod workspace;

pub use analysis::{AnalyzeOptions, analyze_visibility};
pub use analyzer::{run_checks, run_checks_with_options};
pub use base_data::{
	BaseAnalysisSnapshot, BaseDataSource, InstalledBaseDataEntry, ReleaseDataManifest,
	build_base_snapshot, default_release_tag, install_built_snapshot,
	install_snapshot_from_release, list_installed_base_data, write_snapshot_bundle,
};
pub use graph::{
	GraphArtifactFormat, GraphBuildOptions, GraphRootSelector, GraphScopeSelection,
	run_graph_with_options,
};
pub use merge::{
	MergeExecuteOptions, MergeExecutionResult, run_merge_plan, run_merge_plan_with_options,
	run_merge_with_options,
};
pub use model::{
	AnalysisMode, ChannelMode, CheckRequest, CheckResult, Finding,
	MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH, MERGED_MOD_DESCRIPTOR_PATH,
	MergePlanEntry, MergePlanFormat, MergePlanOptions, MergePlanResult, MergePlanStrategies,
	MergePlanStrategy, MergeReport, MergeReportStatus, MergeReportValidation, RunOptions, Severity,
	SymbolKind,
};
pub use parser::{AstFile, ParseResult, parse_clausewitz_file};
pub use semantic_index::build_semantic_index;
pub use simplify::{SimplifyOptions, SimplifyReport, SimplifySummary, run_simplify_with_options};
