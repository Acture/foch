pub mod ir;
pub(crate) mod emit;
pub(crate) mod execute;
pub(crate) mod materialize;
pub(crate) mod normalize;
pub mod plan;

pub use execute::{run_merge_with_options, MergeExecuteOptions, MergeExecutionResult};
pub use plan::{run_merge_plan, run_merge_plan_with_options};
