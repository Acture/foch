pub mod check;
pub mod config;

pub type HandlerResult = Result<i32, Box<dyn std::error::Error>>;
