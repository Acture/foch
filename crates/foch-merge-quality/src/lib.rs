//! foch merge-quality harness.
//!
//! Measures foch's structural-merge quality against community-authored
//! *compatibility patches* ("compatches"), which serve as human-written ground
//! truth for "what a good merge of mod A + mod B looks like".
//!
//! - [`corpus`] — the discovered set of compatches (serialized as `corpus.json`).
//! - [`score`] — run `foch merge` in-process and classify the result per file.
//! - [`orchestrate`] — `run` (score the locally-available corpus) and `learn`
//!   (classify how humans resolved overlaps).
//! - [`report`] — writers for `results.json` / `report.md` / `rules.md`.
//! - [`fixtures`] — extract full local cases for the committed test corpus.
//! - [`symbols`] — full-local report for cross-file symbol conflicts.
//!
//! The Steam Workshop discovery + SteamCMD download pipeline lives behind the
//! `steam` feature (network + external tooling); everything else is offline and
//! runs over the committed fixtures / a local workshop directory.

pub mod archive;
pub mod config;
pub mod corpus;
pub mod dataset;
pub mod fixtures;
pub mod lifecycle;
pub mod object_store;
pub mod orchestrate;
pub mod report;
pub mod score;
pub mod symbols;

#[cfg(feature = "steam")]
pub mod fetch;
#[cfg(feature = "steam")]
pub mod secrets;
#[cfg(feature = "steam")]
pub mod steam;

/// Result type for command-level operations (the CLI subcommands return this).
pub type CmdResult = std::result::Result<(), Box<dyn std::error::Error>>;
