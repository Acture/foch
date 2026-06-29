//! Steam Workshop discovery via the Steam Web API (reqwest blocking).
//!
//! STUB — implemented by the MQ-net track. Builds the corpus from QueryFiles
//! (search) + GetPublishedFileDetails (metadata) + GetDetails?includechildren
//! (required-items pairing), then writes `corpus.json`.

use std::path::Path;

use crate::CmdResult;

/// Discover EU4 compatches, pair each with the mods it patches, and write the
/// resulting corpus to `corpus_out`. `max_items` caps candidates per search term.
pub fn discover(_corpus_out: &Path, _max_items: usize) -> CmdResult {
	todo!("MQ-net: implement discover")
}
