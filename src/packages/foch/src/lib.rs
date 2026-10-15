//! Foch's EU4 project, playset, analysis, and merge library.

mod data_version;

pub use data_version::DATA_VERSION;

pub mod check;
pub mod game;
pub mod graph;
pub mod input;
pub mod merge;
pub mod model;
pub mod platform;
pub mod playset;
pub mod plugin;
pub mod project;
pub mod repair;
pub mod simplify;
