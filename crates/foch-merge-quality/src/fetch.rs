//! Download curated compatches + their patched mods via SteamCMD (no subscribe).
//!
//! STUB — implemented by the MQ-fetch track. Curates a prime set from the corpus
//! (non-churn & ≥ min subscribers, top N by subs) and downloads via batched
//! `steamcmd +login <user> +workshop_download_item <appid> <id> ... +quit`.

use std::path::Path;

use crate::CmdResult;

/// Curate the top `fetch_n` compatches with ≥ `min_subs` subscribers (skipping
/// churned ones) and download them plus their patched mods into `workshop_dir`.
pub fn fetch(_corpus: &Path, _workshop_dir: &Path, _fetch_n: usize, _min_subs: i64) -> CmdResult {
	todo!("MQ-fetch: implement fetch")
}
