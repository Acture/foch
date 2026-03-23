pub mod analysis;
pub mod analysis_version;
pub mod base_data;
pub mod documents;
pub mod engine;
pub mod eu4_builtin;
pub mod graph;
pub mod localisation;
pub mod merge_plan;
pub mod mod_cache;
pub mod model;
pub mod param_contracts;
pub mod parser;
pub mod report;
pub mod rules;
pub mod semantic_index;

pub use analysis::{AnalyzeOptions, analyze_visibility};
pub use base_data::{
	BaseAnalysisSnapshot, BaseDataSource, InstalledBaseDataEntry, ReleaseDataManifest,
	build_base_snapshot, default_release_tag, install_built_snapshot,
	install_snapshot_from_release, list_installed_base_data, write_snapshot_bundle,
};
pub use engine::{run_checks, run_checks_with_options};
pub use graph::export_graph;
pub use merge_plan::{run_merge_plan, run_merge_plan_with_options};
pub use model::{
	AnalysisMode, ChannelMode, CheckRequest, CheckResult, Finding, GraphFormat, MergePlanEntry,
	MergePlanFormat, MergePlanOptions, MergePlanResult, MergePlanStrategies, MergePlanStrategy,
	MergeReport, MergeReportStatus, MergeReportValidation, RunOptions, Severity,
	MERGED_MOD_DESCRIPTOR_PATH, MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH,
};
pub use parser::{AstFile, ParseResult, parse_clausewitz_file};
pub use semantic_index::build_semantic_index;
