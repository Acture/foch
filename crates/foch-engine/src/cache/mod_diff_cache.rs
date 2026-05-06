//! Persistent cache for per-mod patch sets against a deterministic base.
//!
//! `ClausewitzPatch` currently derives serde but not rkyv, so this layer uses
//! bincode. Entries are stored one file per `(target_path, mod_hash,
//! vanilla_hash)` key to avoid rewriting a shared per-mod index and to keep
//! failed writes isolated.

use super::mod_parse_cache::{CacheError, default_foch_cache_dir};
use crate::merge::patch::ClausewitzPatch;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bump when the cached patch payload or diff behavior becomes incompatible.
pub const MOD_DIFF_CACHE_VERSION: u32 = 2;
const CACHE_ENV: &str = "FOCH_MOD_DIFF_CACHE_DIR";
const HASH_HEX_LEN: usize = 16;
const COMPACT_HASH_LEN: usize = 12;

static MOD_DIFF_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static MOD_DIFF_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModDiffCacheStats {
	pub hits: usize,
	pub misses: usize,
}

#[derive(Clone, Debug)]
pub struct ModDiffCache {
	root: PathBuf,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredModDiff {
	cache_version: u32,
	target_path: String,
	mod_hash: String,
	vanilla_hash: String,
	foch_version: String,
	game_version: String,
	patches: Vec<ClausewitzPatch>,
}

impl ModDiffCache {
	pub fn open(cache_dir: &Path) -> Self {
		let _ = fs::create_dir_all(cache_dir);
		Self {
			root: cache_dir.to_path_buf(),
		}
	}

	pub fn open_default() -> Self {
		Self::open(&default_mod_diff_cache_dir())
	}

	pub fn lookup(
		&self,
		target_path: &str,
		mod_hash: &str,
		vanilla_hash: &str,
		foch_version: &str,
		game_version: &str,
	) -> Option<Vec<ClausewitzPatch>> {
		let hit = self.lookup_inner(
			target_path,
			mod_hash,
			vanilla_hash,
			foch_version,
			game_version,
		);
		if hit.is_some() {
			MOD_DIFF_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
		} else {
			MOD_DIFF_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
		}
		hit
	}

	pub fn store(
		&self,
		target_path: &str,
		mod_hash: &str,
		vanilla_hash: &str,
		foch_version: &str,
		game_version: &str,
		patches: &[ClausewitzPatch],
	) -> Result<(), CacheError> {
		fs::create_dir_all(&self.root).map_err(CacheError::Io)?;
		let payload = StoredModDiff {
			cache_version: MOD_DIFF_CACHE_VERSION,
			target_path: target_path.to_string(),
			mod_hash: mod_hash.to_string(),
			vanilla_hash: vanilla_hash.to_string(),
			foch_version: foch_version.to_string(),
			game_version: game_version.to_string(),
			patches: patches.to_vec(),
		};
		let encoded =
			bincode::serialize(&payload).map_err(|err| CacheError::Encode(err.to_string()))?;
		let path = self.cache_file(
			target_path,
			mod_hash,
			vanilla_hash,
			foch_version,
			game_version,
		);
		let tmp = path.with_extension(format!("bin.{}.tmp", std::process::id()));
		fs::write(&tmp, encoded).map_err(CacheError::Io)?;
		fs::rename(&tmp, &path).map_err(|err| {
			let _ = fs::remove_file(&tmp);
			CacheError::Io(err)
		})?;
		Ok(())
	}

	fn lookup_inner(
		&self,
		target_path: &str,
		mod_hash: &str,
		vanilla_hash: &str,
		foch_version: &str,
		game_version: &str,
	) -> Option<Vec<ClausewitzPatch>> {
		let path = self.cache_file(
			target_path,
			mod_hash,
			vanilla_hash,
			foch_version,
			game_version,
		);
		let raw = fs::read(path).ok()?;
		let stored = bincode::deserialize::<StoredModDiff>(&raw).ok()?;
		if stored.cache_version != MOD_DIFF_CACHE_VERSION
			|| stored.target_path != target_path
			|| stored.mod_hash != mod_hash
			|| stored.vanilla_hash != vanilla_hash
			|| stored.foch_version != foch_version
			|| stored.game_version != game_version
		{
			return None;
		}
		Some(stored.patches)
	}

	fn cache_file(
		&self,
		target_path: &str,
		mod_hash: &str,
		vanilla_hash: &str,
		foch_version: &str,
		game_version: &str,
	) -> PathBuf {
		self.root.join(cache_filename(
			MOD_DIFF_CACHE_VERSION,
			target_path,
			mod_hash,
			vanilla_hash,
			foch_version,
			game_version,
		))
	}
}

pub fn default_mod_diff_cache_dir() -> PathBuf {
	if let Ok(override_dir) = std::env::var(CACHE_ENV) {
		return PathBuf::from(override_dir);
	}
	default_foch_cache_dir().join("diffs")
}

pub fn mod_diff_cache_stats() -> ModDiffCacheStats {
	ModDiffCacheStats {
		hits: MOD_DIFF_CACHE_HITS.load(Ordering::Relaxed),
		misses: MOD_DIFF_CACHE_MISSES.load(Ordering::Relaxed),
	}
}

pub fn reset_mod_diff_cache_stats() {
	MOD_DIFF_CACHE_HITS.store(0, Ordering::Relaxed);
	MOD_DIFF_CACHE_MISSES.store(0, Ordering::Relaxed);
}

fn cache_filename(
	cache_version: u32,
	target_path: &str,
	mod_hash: &str,
	vanilla_hash: &str,
	foch_version: &str,
	game_version: &str,
) -> String {
	let key = cache_key(
		target_path,
		mod_hash,
		vanilla_hash,
		foch_version,
		game_version,
	);
	format!(
		"{}__{}__cv{}__v{}__g{}__{}.bin",
		compact_hash(mod_hash),
		compact_hash(vanilla_hash),
		cache_version,
		sanitize_component(foch_version),
		sanitize_component(game_version),
		key,
	)
}

fn cache_key(
	target_path: &str,
	mod_hash: &str,
	vanilla_hash: &str,
	foch_version: &str,
	game_version: &str,
) -> String {
	let mut hasher = blake3::Hasher::new();
	update_hash_part(&mut hasher, target_path.as_bytes());
	update_hash_part(&mut hasher, mod_hash.as_bytes());
	update_hash_part(&mut hasher, vanilla_hash.as_bytes());
	update_hash_part(&mut hasher, foch_version.as_bytes());
	update_hash_part(&mut hasher, game_version.as_bytes());
	hasher.finalize().to_hex()[..HASH_HEX_LEN].to_string()
}

fn update_hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
	hasher.update(&(bytes.len() as u64).to_le_bytes());
	hasher.update(bytes);
}

fn compact_hash(value: &str) -> String {
	let compact: String = value.chars().take(COMPACT_HASH_LEN).collect();
	sanitize_component(if compact.is_empty() { value } else { &compact })
}

fn sanitize_component(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for ch in value.chars() {
		if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
			out.push(ch);
		} else {
			out.push('_');
		}
	}
	if out.is_empty() {
		"unknown".to_string()
	} else {
		out
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::parser::{AstValue, ScalarValue, Span, SpanRange};
	use std::path::PathBuf;
	use std::sync::atomic::{AtomicUsize, Ordering};

	static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

	fn cache_dir(name: &str) -> PathBuf {
		let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.and_then(Path::parent)
			.expect("repo root");
		let path = repo_root.join("target").join("test-cache").join(format!(
			"{name}-{}-{}",
			std::process::id(),
			TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
		));
		let _ = fs::remove_dir_all(&path);
		fs::create_dir_all(&path).expect("create cache dir");
		path
	}

	fn span() -> SpanRange {
		SpanRange {
			start: Span {
				line: 1,
				column: 1,
				offset: 0,
			},
			end: Span {
				line: 1,
				column: 2,
				offset: 1,
			},
		}
	}

	fn scalar(value: &str) -> AstValue {
		AstValue::Scalar {
			value: ScalarValue::Number(value.to_string()),
			span: span(),
		}
	}

	fn sample_patches() -> Vec<ClausewitzPatch> {
		vec![ClausewitzPatch::SetValue {
			path: vec!["root".to_string()],
			key: "value".to_string(),
			old_value: scalar("1"),
			new_value: scalar("2"),
		}]
	}

	#[test]
	fn mod_diff_cache_lookup_miss_then_store_then_hit() {
		let cache = ModDiffCache::open(&cache_dir("mod-diff-hit"));
		let patches = sample_patches();

		assert!(
			cache
				.lookup("common/foo.txt", "mod-a", "vanilla-a", "0.1.0", "eu4 1.37")
				.is_none()
		);
		cache
			.store(
				"common/foo.txt",
				"mod-a",
				"vanilla-a",
				"0.1.0",
				"eu4 1.37",
				&patches,
			)
			.expect("store patches");

		assert_eq!(
			cache
				.lookup("common/foo.txt", "mod-a", "vanilla-a", "0.1.0", "eu4 1.37")
				.expect("cache hit"),
			patches
		);
	}

	#[test]
	fn mod_diff_cache_invalidates_when_vanilla_changes() {
		let cache = ModDiffCache::open(&cache_dir("mod-diff-vanilla"));
		cache
			.store(
				"common/foo.txt",
				"mod-a",
				"vanilla-a",
				"0.1.0",
				"eu4 1.37",
				&sample_patches(),
			)
			.expect("store patches");

		assert!(
			cache
				.lookup("common/foo.txt", "mod-a", "vanilla-b", "0.1.0", "eu4 1.37")
				.is_none()
		);
	}

	#[test]
	fn mod_diff_cache_invalidates_when_mod_changes() {
		let cache = ModDiffCache::open(&cache_dir("mod-diff-mod"));
		cache
			.store(
				"common/foo.txt",
				"mod-a",
				"vanilla-a",
				"0.1.0",
				"eu4 1.37",
				&sample_patches(),
			)
			.expect("store patches");

		assert!(
			cache
				.lookup("common/foo.txt", "mod-b", "vanilla-a", "0.1.0", "eu4 1.37")
				.is_none()
		);
	}

	#[test]
	fn mod_diff_cache_invalidates_when_game_version_changes() {
		let cache = ModDiffCache::open(&cache_dir("mod-diff-game-version"));
		cache
			.store(
				"common/foo.txt",
				"mod-a",
				"vanilla-a",
				"0.1.0",
				"eu4 1.36",
				&sample_patches(),
			)
			.expect("store patches");

		assert!(
			cache
				.lookup("common/foo.txt", "mod-a", "vanilla-a", "0.1.0", "eu4 1.37",)
				.is_none()
		);
	}

	#[test]
	fn mod_diff_cache_filename_encodes_cache_version() {
		let current = cache_filename(
			MOD_DIFF_CACHE_VERSION,
			"common/foo.txt",
			"mod-a",
			"vanilla-a",
			"0.1.0",
			"eu4 1.37",
		);
		let bumped = cache_filename(
			MOD_DIFF_CACHE_VERSION + 1,
			"common/foo.txt",
			"mod-a",
			"vanilla-a",
			"0.1.0",
			"eu4 1.37",
		);

		assert_ne!(current, bumped);
		assert!(current.contains(&format!("__cv{}__", MOD_DIFF_CACHE_VERSION)));
		assert!(current.contains("__v0.1.0__"));
		assert!(current.contains("__geu4_1.37__"));
	}

	#[test]
	fn mod_diff_cache_persists_across_processes() {
		let dir = cache_dir("mod-diff-persist");
		let patches = sample_patches();
		ModDiffCache::open(&dir)
			.store(
				"common/foo.txt",
				"mod-a",
				"vanilla-a",
				"0.1.0",
				"eu4 1.37",
				&patches,
			)
			.expect("store patches");

		let reopened = ModDiffCache::open(&dir);
		assert_eq!(
			reopened
				.lookup("common/foo.txt", "mod-a", "vanilla-a", "0.1.0", "eu4 1.37")
				.expect("cache hit after reopen"),
			patches
		);
	}
}
