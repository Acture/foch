pub mod config;
pub mod domain;
pub mod model;
pub mod text;
pub mod utils;

pub use config::{
	ConfigError, ResolutionDecision, ResolutionEntry, ResolutionMap, compute_conflict_id,
};
pub use text::decode_paradox_bytes;
