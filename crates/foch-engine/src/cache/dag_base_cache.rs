//! Persistent cache for dependency-DAG base statement lists.
//!
//! `AstStatement` derives serde but not rkyv, so this layer uses bincode. Each
//! `(deps_hash, file_path)` entry is stored as its own file; this avoids shared
//! index rewrites while allowing independent invalidation per file and upstream
//! dependency set.

use super::mod_parse_cache::{CacheError, default_foch_cache_dir};
use foch_language::analyzer::parser::AstStatement;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bump when the cached DAG-base payload or synthesis behavior changes.
pub const DAG_BASE_CACHE_VERSION: u32 = 3;
const CACHE_ENV: &str = "FOCH_DAG_BASE_CACHE_DIR";
const HASH_HEX_LEN: usize = 16;
const COMPACT_HASH_LEN: usize = 12;

static DAG_BASE_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static DAG_BASE_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DagBaseCacheStats {
	pub hits: usize,
	pub misses: usize,
}

#[derive(Clone, Debug)]
pub struct DagBaseCache {
	root: PathBuf,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredDagBase {
	cache_version: u32,
	deps_hash: String,
	file_path: String,
	foch_version: String,
	game_version: String,
	statements: Vec<AstStatement>,
}

impl DagBaseCache {
	pub fn open(cache_dir: &Path) -> Self {
		let _ = fs::create_dir_all(cache_dir);
		Self {
			root: cache_dir.to_path_buf(),
		}
	}

	pub fn open_default() -> Self {
		Self::open(&default_dag_base_cache_dir())
	}

	pub fn lookup(
		&self,
		deps_hash: &str,
		file_path: &str,
		foch_version: &str,
		game_version: &str,
	) -> Option<Vec<AstStatement>> {
		let hit = self.lookup_inner(deps_hash, file_path, foch_version, game_version);
		if hit.is_some() {
			DAG_BASE_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
		} else {
			DAG_BASE_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
		}
		hit
	}

	pub fn store(
		&self,
		deps_hash: &str,
		file_path: &str,
		foch_version: &str,
		game_version: &str,
		statements: &[AstStatement],
	) -> Result<(), CacheError> {
		fs::create_dir_all(&self.root).map_err(CacheError::Io)?;
		let payload = StoredDagBase {
			cache_version: DAG_BASE_CACHE_VERSION,
			deps_hash: deps_hash.to_string(),
			file_path: file_path.to_string(),
			foch_version: foch_version.to_string(),
			game_version: game_version.to_string(),
			statements: statements.to_vec(),
		};
		let encoded =
			bincode::serialize(&payload).map_err(|err| CacheError::Encode(err.to_string()))?;
		let path = self.cache_file(deps_hash, file_path, foch_version, game_version);
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
		deps_hash: &str,
		file_path: &str,
		foch_version: &str,
		game_version: &str,
	) -> Option<Vec<AstStatement>> {
		let path = self.cache_file(deps_hash, file_path, foch_version, game_version);
		let raw = fs::read(path).ok()?;
		let stored = bincode::deserialize::<StoredDagBase>(&raw).ok()?;
		if stored.cache_version != DAG_BASE_CACHE_VERSION
			|| stored.deps_hash != deps_hash
			|| stored.file_path != file_path
			|| stored.foch_version != foch_version
			|| stored.game_version != game_version
		{
			return None;
		}
		Some(stored.statements)
	}

	fn cache_file(
		&self,
		deps_hash: &str,
		file_path: &str,
		foch_version: &str,
		game_version: &str,
	) -> PathBuf {
		self.root.join(cache_filename(
			DAG_BASE_CACHE_VERSION,
			deps_hash,
			file_path,
			foch_version,
			game_version,
		))
	}
}

pub fn default_dag_base_cache_dir() -> PathBuf {
	if let Ok(override_dir) = std::env::var(CACHE_ENV) {
		return PathBuf::from(override_dir);
	}
	default_foch_cache_dir().join("dag-base")
}

pub fn dag_base_cache_stats() -> DagBaseCacheStats {
	DagBaseCacheStats {
		hits: DAG_BASE_CACHE_HITS.load(Ordering::Relaxed),
		misses: DAG_BASE_CACHE_MISSES.load(Ordering::Relaxed),
	}
}

pub fn reset_dag_base_cache_stats() {
	DAG_BASE_CACHE_HITS.store(0, Ordering::Relaxed);
	DAG_BASE_CACHE_MISSES.store(0, Ordering::Relaxed);
}

fn cache_filename(
	cache_version: u32,
	deps_hash: &str,
	file_path: &str,
	foch_version: &str,
	game_version: &str,
) -> String {
	let key = cache_key(deps_hash, file_path, foch_version, game_version);
	format!(
		"{}__cv{}__v{}__g{}__{}.bin",
		compact_hash(deps_hash),
		cache_version,
		sanitize_component(foch_version),
		sanitize_component(game_version),
		key,
	)
}

fn cache_key(deps_hash: &str, file_path: &str, foch_version: &str, game_version: &str) -> String {
	let mut hasher = blake3::Hasher::new();
	update_hash_part(&mut hasher, deps_hash.as_bytes());
	update_hash_part(&mut hasher, file_path.as_bytes());
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

	fn statement(key: &str, value: &str) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: span(),
			value: AstValue::Scalar {
				value: ScalarValue::Number(value.to_string()),
				span: span(),
			},
			span: span(),
		}
	}

	fn sample_statements() -> Vec<AstStatement> {
		vec![statement("foo", "1"), statement("bar", "2")]
	}

	#[test]
	fn dag_base_cache_lookup_with_same_deps_hash_hits() {
		let cache = DagBaseCache::open(&cache_dir("dag-base-hit"));
		let statements = sample_statements();

		assert!(
			cache
				.lookup("deps-a", "common/foo.txt", "0.1.0", "eu4 1.37")
				.is_none()
		);
		cache
			.store("deps-a", "common/foo.txt", "0.1.0", "eu4 1.37", &statements)
			.expect("store base");

		assert_eq!(
			cache
				.lookup("deps-a", "common/foo.txt", "0.1.0", "eu4 1.37")
				.expect("cache hit"),
			statements
		);
	}

	#[test]
	fn dag_base_cache_invalidates_when_dep_set_changes() {
		let cache = DagBaseCache::open(&cache_dir("dag-base-deps"));
		cache
			.store(
				"deps-a-b",
				"common/foo.txt",
				"0.1.0",
				"eu4 1.37",
				&sample_statements(),
			)
			.expect("store base");

		assert!(
			cache
				.lookup("deps-a-c", "common/foo.txt", "0.1.0", "eu4 1.37")
				.is_none()
		);
	}

	#[test]
	fn dag_base_cache_invalidates_when_dep_content_changes() {
		let cache = DagBaseCache::open(&cache_dir("dag-base-content"));
		cache
			.store(
				"hash-before",
				"common/foo.txt",
				"0.1.0",
				"eu4 1.37",
				&sample_statements(),
			)
			.expect("store base");

		assert!(
			cache
				.lookup("hash-after", "common/foo.txt", "0.1.0", "eu4 1.37")
				.is_none()
		);
	}

	#[test]
	fn dag_base_cache_invalidates_when_game_version_changes() {
		let cache = DagBaseCache::open(&cache_dir("dag-base-game-version"));
		cache
			.store(
				"deps-a",
				"common/foo.txt",
				"0.1.0",
				"eu4 1.36",
				&sample_statements(),
			)
			.expect("store base");

		assert!(
			cache
				.lookup("deps-a", "common/foo.txt", "0.1.0", "eu4 1.37")
				.is_none()
		);
	}

	#[test]
	fn dag_base_cache_filename_encodes_cache_version() {
		let current = cache_filename(
			DAG_BASE_CACHE_VERSION,
			"deps-a",
			"common/foo.txt",
			"0.1.0",
			"eu4 1.37",
		);
		let bumped = cache_filename(
			DAG_BASE_CACHE_VERSION + 1,
			"deps-a",
			"common/foo.txt",
			"0.1.0",
			"eu4 1.37",
		);

		assert_ne!(current, bumped);
		assert!(current.contains(&format!("__cv{}__", DAG_BASE_CACHE_VERSION)));
		assert!(current.contains("__v0.1.0__"));
		assert!(current.contains("__geu4_1.37__"));
	}

	#[test]
	fn dag_base_cache_independent_per_file_path() {
		let cache = DagBaseCache::open(&cache_dir("dag-base-path"));
		cache
			.store(
				"deps-a",
				"common/foo.txt",
				"0.1.0",
				"eu4 1.37",
				&sample_statements(),
			)
			.expect("store base");

		assert!(
			cache
				.lookup("deps-a", "common/bar.txt", "0.1.0", "eu4 1.37")
				.is_none()
		);
	}
}
