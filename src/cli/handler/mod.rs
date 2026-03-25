pub mod check;
pub mod config;
pub mod data;
pub mod graph;
pub mod merge;
pub mod merge_plan;
pub mod simplify;

pub type HandlerResult = Result<i32, Box<dyn std::error::Error>>;
