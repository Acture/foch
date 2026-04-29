pub(crate) mod dag;
pub mod emit;
pub mod error;
pub(crate) mod execute;
pub(crate) mod localisation_merge;
pub(crate) mod materialize;
pub mod namespace;
pub(crate) mod normalize;
pub mod patch;
pub mod patch_apply;
pub(crate) mod patch_deps;
pub mod patch_merge;
pub mod plan;

pub use error::MergeError;
pub use execute::{MergeExecuteOptions, MergeExecutionResult, run_merge_with_options};
pub use plan::{run_merge_plan, run_merge_plan_with_options};
