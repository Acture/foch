pub mod check;
pub mod config;
pub mod merge_plan;

pub type HandlerResult = Result<i32, Box<dyn std::error::Error>>;
