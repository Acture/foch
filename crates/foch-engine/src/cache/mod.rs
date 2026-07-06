//! Engine-level caches.
//!
//! The per-mod parse cache lives in `foch-engine` because workspace resolution
//! owns mod roots, user ignore patterns, and game-version selection.

mod dag_base_cache;
pub mod layer;
mod mod_diff_cache;
mod mod_parse_cache;
mod modset_cache;
pub(crate) mod parsed_scripts;

pub use dag_base_cache::default_dag_base_cache_dir;
pub(crate) use dag_base_cache::{DagBaseCache, dag_base_cache_stats, reset_dag_base_cache_stats};
pub use layer::{CacheLayer, CacheLayerEntryInfo, CacheLayerOps, EvictionStats, all_layers};
pub use mod_diff_cache::default_mod_diff_cache_dir;
pub(crate) use mod_diff_cache::{ModDiffCache, mod_diff_cache_stats, reset_mod_diff_cache_stats};
pub use mod_parse_cache::{CacheError, default_foch_cache_dir, default_mod_parse_cache_dir};
pub(crate) use mod_parse_cache::{
	CachedModData, ModParseCache, compute_mod_hash_for_files, compute_mod_hash_with_filter,
};
pub use modset_cache::{
	CacheEntryInfo, CacheStats, CachedModsetResult, ModsetCache, default_modset_cache_dir,
	default_modset_cache_root_dir,
};
pub(crate) use modset_cache::{
	compute_modset_cache_key, compute_resolution_map_hash, unpack_modset_tarball,
};

const DEFAULT_CACHE_CAP_BYTES: u64 = 1 << 30;

pub fn cache_cap_bytes() -> u64 {
	std::env::var("FOCH_CACHE_MAX_BYTES")
		.ok()
		.and_then(|value| value.trim().parse().ok())
		.unwrap_or(DEFAULT_CACHE_CAP_BYTES)
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::semantic_index::parse_cache;
	use std::path::{Path, PathBuf};
	use std::sync::{Mutex, MutexGuard};

	static ENV_LOCK: Mutex<()> = Mutex::new(());

	struct EnvGuard {
		_lock: MutexGuard<'static, ()>,
		previous: Vec<(&'static str, Option<String>)>,
	}

	impl EnvGuard {
		fn new(cache_root: &Path) -> Self {
			let lock = ENV_LOCK.lock().expect("env lock");
			let keys = [
				"FOCH_CACHE_ROOT",
				"FOCH_CACHE_DIR",
				"FOCH_MOD_PARSE_CACHE_DIR",
				"FOCH_MOD_DIFF_CACHE_DIR",
				"FOCH_DAG_BASE_CACHE_DIR",
				"FOCH_MODSET_CACHE_DIR",
				"FOCH_PARSE_CACHE_DIR",
				"FOCH_CACHE_MAX_BYTES",
			];
			let previous = keys
				.into_iter()
				.map(|key| (key, std::env::var(key).ok()))
				.collect::<Vec<_>>();
			unsafe {
				std::env::set_var("FOCH_CACHE_ROOT", cache_root);
				for key in keys.into_iter().filter(|key| *key != "FOCH_CACHE_ROOT") {
					std::env::remove_var(key);
				}
			}
			Self {
				_lock: lock,
				previous,
			}
		}
	}

	impl Drop for EnvGuard {
		fn drop(&mut self) {
			unsafe {
				for (key, value) in &self.previous {
					if let Some(value) = value {
						std::env::set_var(key, value);
					} else {
						std::env::remove_var(key);
					}
				}
			}
		}
	}

	fn cache_root(name: &str) -> PathBuf {
		let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.and_then(Path::parent)
			.expect("repo root");
		let root = repo_root
			.join("target")
			.join("test-cache")
			.join(format!("{name}-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("create cache root");
		root
	}

	#[test]
	fn cache_root_unification_uses_single_dir() {
		let root = cache_root("root-unification");
		let _env = EnvGuard::new(&root);

		assert_eq!(default_foch_cache_dir(), root);
		assert_eq!(default_mod_parse_cache_dir(), root.join("mods"));
		assert_eq!(default_mod_diff_cache_dir(), root.join("diffs"));
		assert_eq!(default_dag_base_cache_dir(), root.join("dag-base"));
		assert_eq!(default_modset_cache_dir(), root.join("modsets"));
		assert_eq!(
			parse_cache::parser_cache_root(),
			root.join("parse").join("v8")
		);
	}

	#[test]
	fn cache_cap_bytes_uses_shared_env_setting() {
		let root = cache_root("cache-cap");
		let _env = EnvGuard::new(&root);
		unsafe {
			std::env::set_var("FOCH_CACHE_MAX_BYTES", "12345");
		}

		assert_eq!(cache_cap_bytes(), 12_345);
	}
}
