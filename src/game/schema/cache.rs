use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::error::CwtLoadError;
use super::query::{CompiledRulePack, CwtQuery, PACK_FORMAT_VERSION};
use super::source::{CwtSchemaId, CwtSource, SchemaPack, cwt_schema_id_from_dir};

const COMPILED_RULE_CACHE_DIR_NAME: &str = "cwt-rules";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CwtLoadStatus {
	CacheHit,
	CompiledFromSource,
}

pub(crate) struct CwtLoad {
	pub(crate) facts: Arc<CwtQuery>,
	pub(crate) status: CwtLoadStatus,
	pub(crate) source_id: CwtSchemaId,
	pub(crate) cache_path: Option<PathBuf>,
	pub(crate) timings: CwtLoadTimings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CwtLoadTimings {
	pub(crate) source_hash: Duration,
	pub(crate) cache_read: Option<Duration>,
	pub(crate) source_compile: Option<Duration>,
	pub(crate) total: Duration,
}

pub(crate) fn default_cwt_cache_dir() -> PathBuf {
	crate::platform::cache_store::default_foch_cache_dir().join(COMPILED_RULE_CACHE_DIR_NAME)
}

pub(crate) fn load_cwt_from_dir(
	root: &Path,
	source: CwtSource,
	cache_dir: Option<&Path>,
) -> Result<CwtLoad, CwtLoadError> {
	let total_started = Instant::now();
	let hash_started = Instant::now();
	let source_id = cwt_schema_id_from_dir(root)?;
	let source_hash = hash_started.elapsed();
	let source_id_hex = source_id.to_hex();
	let cache_path = cache_dir.map(|dir| {
		let generation = open_compiled_rule_cache_generation(dir);
		compiled_rule_cache_path(&generation, &source_id_hex)
	});
	let mut cache_read = None;
	if let Some(path) = cache_path.as_ref() {
		let cache_started = Instant::now();
		let cached_pack = read_cached_compiled_pack(path, &source_id_hex);
		cache_read = Some(cache_started.elapsed());
		if let Some(pack) = cached_pack {
			return Ok(CwtLoad {
				facts: Arc::new(CwtQuery::new(pack)),
				status: CwtLoadStatus::CacheHit,
				source_id,
				cache_path,
				timings: CwtLoadTimings {
					source_hash,
					cache_read,
					source_compile: None,
					total: total_started.elapsed(),
				},
			});
		}
	}

	let compile_started = Instant::now();
	let schema_pack = SchemaPack::load_from_dir_with_id(root, source, source_id.clone())?;
	let compiled_pack = CompiledRulePack::from_schema_pack(&schema_pack);
	let source_compile = compile_started.elapsed();
	if let Some(path) = cache_path.as_ref() {
		write_cached_compiled_pack(path, &compiled_pack);
	}
	Ok(CwtLoad {
		facts: Arc::new(CwtQuery::new(compiled_pack)),
		status: CwtLoadStatus::CompiledFromSource,
		source_id,
		cache_path,
		timings: CwtLoadTimings {
			source_hash,
			cache_read,
			source_compile: Some(source_compile),
			total: total_started.elapsed(),
		},
	})
}

fn open_compiled_rule_cache_generation(cache_dir: &Path) -> PathBuf {
	let namespace = crate::platform::cache_store::cache_version_namespace(PACK_FORMAT_VERSION)
		.expect("compiled CWT pack version is valid SemVer");
	let generation = cache_dir.join(namespace);
	let _ = fs::create_dir_all(&generation);
	generation
}

fn compiled_rule_cache_path(generation_dir: &Path, source_id: &str) -> PathBuf {
	generation_dir.join(format!("rules-src-{source_id}.bin"))
}

fn read_cached_compiled_pack(path: &Path, source_id: &str) -> Option<CompiledRulePack> {
	let bytes = fs::read(path).ok()?;
	let pack = CompiledRulePack::from_bytes(&bytes).ok()?;
	(pack.source_id.as_deref() == Some(source_id)).then_some(pack)
}

fn write_cached_compiled_pack(path: &Path, pack: &CompiledRulePack) {
	let Some(parent) = path.parent() else {
		return;
	};
	if fs::create_dir_all(parent).is_err() {
		return;
	}
	let Ok(bytes) = pack.to_bytes() else {
		return;
	};
	let _ = fs::write(path, bytes);
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::open_compiled_rule_cache_generation;

	#[test]
	fn opening_a_generation_does_not_delete_other_generations_or_flat_entries() {
		let cache = tempfile::tempdir().expect("create cache fixture");
		let prior = cache.path().join("v0.10.0");
		let flat = cache.path().join("rules-src-old.bin");
		fs::create_dir_all(&prior).expect("create prior generation");
		fs::write(prior.join("rules.bin"), b"prior").expect("write prior cache entry");
		fs::write(&flat, b"flat").expect("write legacy flat cache entry");

		let current = open_compiled_rule_cache_generation(cache.path());

		assert!(current.is_dir());
		assert!(prior.join("rules.bin").is_file());
		assert!(flat.is_file());
	}
}
