//! Private merge-quality evaluation library for foch.
//!
//! Measures foch's structural-merge quality against community-authored
//! *compatibility patches* ("compatches"), which serve as human-written ground
//! truth for "what a good merge of mod A + mod B looks like".
//!
//! - [`corpus`] owns the discovered compatch candidate model.
//! - [`lifecycle`] owns read-only Workshop measurement and reporting workflows.
//! - [`orchestrate`] and [`score`] evaluate an already generated output tree and
//!   parsed product merge report; they do not select the product merge kernel.
//! - [`symbols`] supports explicitly ignored full-local analysis.
//!
//! Workshop inputs are resolved read-only from local Steam installations. The
//! crate has no download or subscription surface and no executable target;
//! product acceptance launches the public `foch` binary from `foch-cli`
//! integration tests.

pub mod archive;
pub mod common_module;
pub mod common_probe;
pub mod config;
pub mod corpus;
pub mod dataset;
pub mod evidence_store;
pub mod lifecycle;
pub mod orchestrate;
pub mod report;
pub mod score;
pub mod shadow;
pub mod symbols;
pub mod workshop_inputs;

/// Result type for repository-owned evaluation and maintenance workflows.
pub type CmdResult = std::result::Result<(), Box<dyn std::error::Error>>;
