//! foch merge-quality harness.
//!
//! Measures foch's structural-merge quality against community-authored
//! *compatibility patches* ("compatches"), which serve as human-written ground
//! truth for "what a good merge of mod A + mod B looks like".
//!
//! - [`corpus`] — the discovered set of compatches (serialized as `corpus.json`).
//! - [`score`] — run `foch merge` in-process and classify the result per file.
//!
//! The Steam Workshop discovery + SteamCMD download pipeline lives behind the
//! `steam` feature (network + external tooling); scoring is offline and runs
//! over the committed fixtures.

pub mod corpus;
pub mod score;
