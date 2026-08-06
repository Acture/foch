//! Private merge-quality evaluation library for foch.
//!
//! Measures foch's structural-merge quality against community-authored
//! *compatibility patches* ("compatches"), which serve as human-written ground
//! truth for "what a good merge of mod A + mod B looks like".
//!
//! - [`corpus`] owns the discovered compatch candidate model.
//! - [`lifecycle`] owns immutable collection, injected measurement, reporting,
//!   and export workflows.
//! - [`orchestrate`] and [`score`] evaluate an already generated output tree and
//!   parsed product merge report; they do not select the product merge kernel.
//! - [`review_pack`] verifies frozen evidence through an injected runner.
//! - [`fixtures`] and [`symbols`] support explicitly ignored maintenance tests.
//!
//! The Steam Workshop discovery + SteamCMD download pipeline lives behind the
//! `steam` feature (network + external tooling); everything else is offline and
//! runs over committed fixtures or explicitly selected local data. The crate has
//! no executable target; product acceptance launches the public `foch` binary
//! from `foch-cli` integration tests.

pub mod archive;
pub mod common_module;
pub mod common_probe;
pub mod config;
pub mod corpus;
pub mod dataset;
pub mod fixtures;
pub mod lifecycle;
pub mod object_store;
pub mod orchestrate;
pub mod report;
pub mod review_annotation;
pub mod review_pack;
pub mod score;
pub mod shadow;
pub(crate) mod snapshot;
pub mod symbols;

#[cfg(feature = "steam")]
pub mod fetch;
#[cfg(feature = "steam")]
pub mod secrets;
#[cfg(feature = "steam")]
pub mod steam;

/// Result type for repository-owned evaluation and maintenance workflows.
pub type CmdResult = std::result::Result<(), Box<dyn std::error::Error>>;
