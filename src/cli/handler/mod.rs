pub mod check;
pub mod config;
pub mod data;
pub mod merge;
pub mod merge_plan;

pub type HandlerResult = Result<i32, Box<dyn std::error::Error>>;
