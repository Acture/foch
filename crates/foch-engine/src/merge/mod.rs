pub(crate) mod emit;
pub mod error;
pub(crate) mod execute;
pub mod ir;
pub(crate) mod materialize;
pub(crate) mod normalize;
pub mod plan;

pub use error::MergeError;
pub use execute::{MergeExecuteOptions, MergeExecutionResult, run_merge_with_options};
pub use plan::{run_merge_plan, run_merge_plan_with_options};
