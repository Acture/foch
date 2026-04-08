use super::super::parser::{ParseResult, parse_clausewitz_file};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const PARSE_CACHE_VERSION: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ParseCacheEntry {
	version: u32,
	file_len: u64,
	modified_nanos: u128,
	result: ParseResult,
}

pub fn parse_clausewitz_file_cached(path: &Path) -> (ParseResult, bool) {
	let signature = file_signature(path);
	let cache_path = parser_cache_file(path);

	if let Some((file_len, modified_nanos)) = signature
		&& let Ok(raw) = fs::read_to_string(&cache_path)
		&& let Ok(entry) = serde_json::from_str::<ParseCacheEntry>(&raw)
		&& entry.version == PARSE_CACHE_VERSION
		&& entry.file_len == file_len
		&& entry.modified_nanos == modified_nanos
	{
		return (entry.result, true);
	}

	let parsed = parse_clausewitz_file(path);

	if let Some((file_len, modified_nanos)) = signature {
		let entry = ParseCacheEntry {
			version: PARSE_CACHE_VERSION,
			file_len,
			modified_nanos,
			result: parsed.clone(),
		};
		store_parse_cache_entry(&cache_path, &entry);
	}

	(parsed, false)
}

fn file_signature(path: &Path) -> Option<(u64, u128)> {
	let metadata = fs::metadata(path).ok()?;
	let modified = metadata
		.modified()
		.ok()
		.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
		.map_or(0, |duration| duration.as_nanos());
	Some((metadata.len(), modified))
}

fn parser_cache_root() -> PathBuf {
	if let Ok(override_dir) = std::env::var("FOCH_PARSE_CACHE_DIR") {
		return PathBuf::from(override_dir);
	}
	dirs::cache_dir()
		.unwrap_or_else(std::env::temp_dir)
		.join("foch")
		.join("parse_cache")
}

fn parser_cache_file(path: &Path) -> PathBuf {
	let normalized = path.to_string_lossy().replace('\\', "/");
	let mut hasher = DefaultHasher::new();
	normalized.hash(&mut hasher);
	let key = format!("{:016x}", hasher.finish());
	parser_cache_root().join(format!("{key}.json"))
}

fn store_parse_cache_entry(path: &Path, entry: &ParseCacheEntry) {
	let Some(parent) = path.parent() else {
		return;
	};
	if fs::create_dir_all(parent).is_err() {
		return;
	}
	let Ok(raw) = serde_json::to_string(entry) else {
		return;
	};
	let tmp = path.with_extension("json.tmp");
	if fs::write(&tmp, raw).is_err() {
		return;
	}
	let _ = fs::rename(tmp, path);
}
