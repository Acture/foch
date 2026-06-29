//! Extract scored-file slices from a local workshop into the committed test
//! fixture tree: for each selected compatch, copy the files the scorer compares
//! (in both patched mods + the compatch's hand-merged version), plus each mod's
//! `descriptor.mod`. Keeps the scoring test reproducible without shipping mods.
//!
//! STUB — implemented by the MQ-integrate track.

use std::path::Path;

use crate::CmdResult;

/// Extract fixtures for the given compatch `ids` (empty = all fully-local cases
/// in the corpus) from `workshop_dir` into `out_dir`.
pub fn extract(
	_corpus: &Path,
	_workshop_dir: &Path,
	_out_dir: &Path,
	_ids: &[String],
) -> CmdResult {
	todo!("MQ-integrate: implement extract-fixtures")
}
