//! Typed EU4 game services and the isolated out-of-process test runner.
//! The independent `foch-runtime-host` package owns the in-process DLL.

pub mod dataplane;
pub mod host;
pub mod pattern;
pub mod plan;
#[cfg(feature = "runner")]
pub mod runner;
