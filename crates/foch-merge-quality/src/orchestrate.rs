//! Scoring orchestration: `run` scores every locally-available compatch in the
//! corpus; `learn` classifies how humans resolved overlaps.
//!
//! STUB — implemented by the MQ-orchestrate track. Reuses [`crate::score`]
//! primitives and writes artifacts via [`crate::report`].

use std::path::Path;

use crate::CmdResult;

/// Options for [`run`].
pub struct RunOptions<'a> {
	/// Path to `corpus.json`.
	pub corpus: &'a Path,
	/// Steam Workshop content dir holding the downloaded mods + compatches.
	pub workshop_dir: &'a Path,
	/// Directory to write `results.json` + `report.md` into.
	pub results_dir: &'a Path,
	/// Cap on number of cases scored (`0` = all).
	pub limit: usize,
	/// Preserve per-case temp merge directories.
	pub keep: bool,
}

/// Filter the corpus to fully-local cases (compatch + all patched mods present
/// in `workshop_dir`), run foch merge on each, score against the compatch, and
/// write `results.json` + `report.md`.
pub fn run(_opts: &RunOptions) -> CmdResult {
	todo!("MQ-orchestrate: implement score-the-local-corpus")
}

/// Read `results.json` from `results_dir`, classify how humans resolved each
/// overlap, and write `rules.md`.
pub fn learn(_results_dir: &Path) -> CmdResult {
	todo!("MQ-orchestrate: implement learn")
}
