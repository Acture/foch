//! Engine-level caches.
//!
//! The per-mod parse cache lives in `foch-engine` because workspace resolution
//! owns mod roots, user ignore patterns, and game-version selection.

mod dag_base_cache;
mod mod_diff_cache;
mod mod_parse_cache;
mod modset_cache;

pub use dag_base_cache::default_dag_base_cache_dir;
pub(crate) use dag_base_cache::{DagBaseCache, dag_base_cache_stats, reset_dag_base_cache_stats};
pub use mod_diff_cache::default_mod_diff_cache_dir;
pub(crate) use mod_diff_cache::{ModDiffCache, mod_diff_cache_stats, reset_mod_diff_cache_stats};
pub use mod_parse_cache::{CacheError, default_foch_cache_dir, default_mod_parse_cache_dir};
pub(crate) use mod_parse_cache::{
	CachedModData, ModParseCache, compute_mod_hash, compute_mod_hash_with_filter,
};
pub use modset_cache::{
	CacheEntryInfo, CacheStats, CachedModsetResult, ModsetCache, default_modset_cache_dir,
	default_modset_cache_root_dir,
};
pub(crate) use modset_cache::{
	compute_modset_cache_key, compute_resolution_map_hash, unpack_modset_tarball,
};
