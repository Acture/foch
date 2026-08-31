//! EU4 builtin and vanilla-base knowledge used by semantic analysis.

pub mod builtin;
pub mod snapshot;
pub mod vanilla_index;
pub mod version;

pub use vanilla_index::VanillaSymbolIndex;
pub use version::analysis_rules_version;
