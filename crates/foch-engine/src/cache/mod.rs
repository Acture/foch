//! Engine-level caches.
//!
//! The per-mod parse cache lives in `foch-engine` because workspace resolution
//! owns mod roots, user ignore patterns, and game-version selection.

mod mod_diff_cache;
mod mod_parse_cache;

pub use mod_diff_cache::{
	MOD_DIFF_CACHE_VERSION, ModDiffCache, ModDiffCacheStats, default_mod_diff_cache_dir,
	mod_diff_cache_stats, reset_mod_diff_cache_stats,
};
pub use mod_parse_cache::{
	CacheError, CachedModData, MOD_PARSE_CACHE_VERSION, ModParseCache, compute_mod_hash,
	compute_mod_hash_with_filter, default_mod_parse_cache_dir,
};
