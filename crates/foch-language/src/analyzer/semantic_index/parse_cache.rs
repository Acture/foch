use super::super::parser::{ParseResult, parse_clausewitz_file};
use filetime::{FileTime, set_file_mtime};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const PARSE_CACHE_VERSION: u32 = 6;
const CACHE_VERSION_DIR: &str = "v6";
const DEFAULT_CACHE_CAP_BYTES: u64 = 1 << 30;

#[cfg(test)]
thread_local! {
	static TEST_CACHE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ParseCacheEntry {
	version: u32,
	file_len: u64,
	modified_nanos: u128,
	result: ParseResult,
}

#[derive(Debug, Clone, Default)]
pub struct CacheStats {
	pub root: PathBuf,
	pub file_count: u64,
	pub total_bytes: u64,
	pub oldest_mtime: Option<SystemTime>,
	pub newest_mtime: Option<SystemTime>,
}

#[derive(Debug, Clone, Default)]
pub struct GcStats {
	pub scanned: u64,
	pub kept: u64,
	pub evicted: u64,
	pub bytes_before: u64,
	pub bytes_after: u64,
}

#[derive(Debug, Clone)]
struct CacheFile {
	path: PathBuf,
	size: u64,
	mtime: SystemTime,
	is_current_version: bool,
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
		touch_cache_file(&cache_path);
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

pub fn parser_cache_root() -> PathBuf {
	parse_cache_base_root().join(CACHE_VERSION_DIR)
}

fn parse_cache_base_root() -> PathBuf {
	#[cfg(test)]
	if let Some(root) = test_cache_root() {
		return root;
	}

	if let Ok(override_dir) = std::env::var("FOCH_PARSE_CACHE_DIR") {
		return PathBuf::from(override_dir);
	}
	dirs::cache_dir()
		.unwrap_or_else(|| PathBuf::from(".").join(".cache"))
		.join("foch")
		.join("parse_cache")
}

#[cfg(test)]
fn test_cache_root() -> Option<PathBuf> {
	TEST_CACHE_ROOT.with(|root| root.borrow().clone())
}

#[cfg(test)]
fn set_test_cache_root(root: Option<PathBuf>) -> Option<PathBuf> {
	TEST_CACHE_ROOT.with(|current| current.replace(root))
}

fn parser_cache_file(path: &Path) -> PathBuf {
	let normalized = path.to_string_lossy().replace('\\', "/");
	let mut hasher = DefaultHasher::new();
	normalized.hash(&mut hasher);
	let key = format!("{:016x}", hasher.finish());
	parser_cache_root()
		.join(&key[0..2])
		.join(&key[2..4])
		.join(format!("{key}.json"))
}

fn touch_cache_file(path: &Path) {
	let _ = set_file_mtime(path, FileTime::now());
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
	let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
	if fs::write(&tmp, raw).is_err() {
		return;
	}
	if fs::rename(&tmp, path).is_err() {
		let _ = fs::remove_file(tmp);
	}
}

pub fn cache_stats() -> CacheStats {
	let root = parse_cache_base_root();
	let current_root = parser_cache_root();
	let mut files = Vec::new();
	collect_cache_files(&root, &current_root, &mut files);

	let mut stats = CacheStats {
		root,
		file_count: files.len() as u64,
		total_bytes: files.iter().map(|file| file.size).sum(),
		oldest_mtime: None,
		newest_mtime: None,
	};
	for file in files {
		stats.oldest_mtime = Some(match stats.oldest_mtime {
			Some(current) if current <= file.mtime => current,
			_ => file.mtime,
		});
		stats.newest_mtime = Some(match stats.newest_mtime {
			Some(current) if current >= file.mtime => current,
			_ => file.mtime,
		});
	}
	stats
}

pub fn gc_with_cap(cap_bytes: u64) -> GcStats {
	let root = parse_cache_base_root();
	let current_root = parser_cache_root();
	let mut files = Vec::new();
	collect_cache_files(&root, &current_root, &mut files);

	let scanned = files.len() as u64;
	let bytes_before = files.iter().map(|file| file.size).sum();
	let mut evict = Vec::new();
	let mut current_files = Vec::new();

	for file in files {
		if file.is_current_version {
			current_files.push(file);
		} else {
			evict.push(file);
		}
	}

	current_files.sort_by(|left, right| {
		right
			.mtime
			.partial_cmp(&left.mtime)
			.unwrap_or(Ordering::Equal)
			.then_with(|| left.path.cmp(&right.path))
	});

	let mut kept_bytes = 0_u64;
	for file in current_files {
		let fits = cap_bytes > 0 && kept_bytes.saturating_add(file.size) <= cap_bytes;
		if fits {
			kept_bytes = kept_bytes.saturating_add(file.size);
		} else {
			evict.push(file);
		}
	}

	let mut evicted = 0_u64;
	for file in evict {
		match fs::remove_file(&file.path) {
			Ok(()) => evicted += 1,
			Err(err) if err.kind() == std::io::ErrorKind::NotFound => evicted += 1,
			Err(err) => eprintln!(
				"[foch] warning: failed to evict parse cache file {}: {err}",
				file.path.display()
			),
		}
	}
	prune_empty_dirs(&root);

	let mut remaining = Vec::new();
	collect_cache_files(&root, &current_root, &mut remaining);
	GcStats {
		scanned,
		kept: remaining.len() as u64,
		evicted,
		bytes_before,
		bytes_after: remaining.iter().map(|file| file.size).sum(),
	}
}

pub fn cache_clean() -> std::io::Result<()> {
	let root = parse_cache_base_root();
	if root.exists() {
		fs::remove_dir_all(root)?;
	}
	Ok(())
}

pub fn cache_cap_bytes() -> u64 {
	std::env::var("FOCH_CACHE_MAX_BYTES")
		.ok()
		.and_then(|value| value.trim().parse().ok())
		.unwrap_or(DEFAULT_CACHE_CAP_BYTES)
}

fn collect_cache_files(root: &Path, current_root: &Path, files: &mut Vec<CacheFile>) {
	let Ok(metadata) = fs::metadata(root) else {
		return;
	};
	if !metadata.is_dir() {
		return;
	}

	let mut stack = vec![root.to_path_buf()];
	while let Some(dir) = stack.pop() {
		let Ok(entries) = fs::read_dir(&dir) else {
			continue;
		};
		for entry in entries.flatten() {
			let path = entry.path();
			let Ok(file_type) = entry.file_type() else {
				continue;
			};
			if file_type.is_dir() {
				stack.push(path);
				continue;
			}
			if !file_type.is_file() {
				continue;
			}
			let Ok(metadata) = entry.metadata() else {
				continue;
			};
			files.push(CacheFile {
				is_current_version: path.starts_with(current_root),
				path,
				size: metadata.len(),
				mtime: metadata.modified().unwrap_or(UNIX_EPOCH),
			});
		}
	}
}

fn prune_empty_dirs(root: &Path) {
	if !root.is_dir() {
		return;
	}
	prune_empty_dirs_inner(root, root);
}

fn prune_empty_dirs_inner(root: &Path, keep_root: &Path) {
	let Ok(entries) = fs::read_dir(root) else {
		return;
	};
	let dirs: Vec<PathBuf> = entries
		.flatten()
		.map(|entry| entry.path())
		.filter(|path| path.is_dir())
		.collect();

	for dir in dirs {
		prune_empty_dirs_inner(&dir, keep_root);
		if dir != keep_root && is_empty_dir(&dir) {
			let _ = fs::remove_dir(&dir);
		}
	}
}

fn is_empty_dir(path: &Path) -> bool {
	fs::read_dir(path)
		.map(|mut entries| entries.next().is_none())
		.unwrap_or(false)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::OsStr;
	use std::time::Duration;
	use tempfile::tempdir;

	struct CacheEnvGuard {
		previous: Option<PathBuf>,
	}

	impl CacheEnvGuard {
		fn new(root: &Path) -> Self {
			Self {
				previous: set_test_cache_root(Some(root.to_path_buf())),
			}
		}
	}

	impl Drop for CacheEnvGuard {
		fn drop(&mut self) {
			set_test_cache_root(self.previous.take());
		}
	}

	#[test]
	fn shard_path() {
		let temp = tempdir().expect("tempdir");
		let _env = CacheEnvGuard::new(temp.path());
		let cache_file = parser_cache_file(Path::new("/mods/test/common/foo.txt"));
		let root = parser_cache_root();
		let relative = cache_file.strip_prefix(&root).expect("under cache root");
		let parts: Vec<_> = relative.iter().collect();

		assert_eq!(root, temp.path().join(CACHE_VERSION_DIR));
		assert_eq!(parts.len(), 3);
		let key = parts[2]
			.to_string_lossy()
			.strip_suffix(".json")
			.expect("json suffix")
			.to_string();
		assert_eq!(key.len(), 16);
		assert_eq!(parts[0], OsStr::new(&key[0..2]));
		assert_eq!(parts[1], OsStr::new(&key[2..4]));
	}

	#[test]
	fn lru_eviction_under_cap() {
		let temp = tempdir().expect("tempdir");
		let _env = CacheEnvGuard::new(temp.path());
		let mut paths = Vec::new();
		for index in 0..5_u64 {
			let path = parser_cache_root()
				.join(format!("{index:02}"))
				.join("aa")
				.join(format!("cache-{index}.json"));
			write_cache_file(&path, 10);
			set_mtime(&path, UNIX_EPOCH + Duration::from_secs(index + 1));
			paths.push(path);
		}

		let stats = gc_with_cap(20);

		assert_eq!(stats.scanned, 5);
		assert_eq!(stats.evicted, 3);
		assert_eq!(stats.kept, 2);
		assert_eq!(stats.bytes_before, 50);
		assert_eq!(stats.bytes_after, 20);
		assert!(!paths[0].exists());
		assert!(!paths[1].exists());
		assert!(!paths[2].exists());
		assert!(paths[3].exists());
		assert!(paths[4].exists());
	}

	#[test]
	fn touch_on_hit_extends_lru() {
		let cache_temp = tempdir().expect("cache tempdir");
		let source_temp = tempdir().expect("source tempdir");
		let _env = CacheEnvGuard::new(cache_temp.path());
		let source = source_temp.path().join("source.txt");
		fs::write(&source, "root = { value = yes }\n").expect("write source");
		let (_, first_hit) = parse_clausewitz_file_cached(&source);
		assert!(!first_hit);
		let cache_file = parser_cache_file(&source);
		let old_mtime = UNIX_EPOCH + Duration::from_secs(1);
		set_mtime(&cache_file, old_mtime);

		let (_, second_hit) = parse_clausewitz_file_cached(&source);

		assert!(second_hit);
		let touched = fs::metadata(&cache_file)
			.expect("cache metadata")
			.modified()
			.expect("cache mtime");
		assert!(touched > old_mtime);
	}

	#[test]
	fn gc_empty_dir_no_panic() {
		let temp = tempdir().expect("tempdir");
		let missing = temp.path().join("missing-cache-root");
		let _env = CacheEnvGuard::new(&missing);

		let stats = gc_with_cap(1024);

		assert_eq!(stats.scanned, 0);
		assert_eq!(stats.kept, 0);
		assert_eq!(stats.evicted, 0);
		assert_eq!(stats.bytes_before, 0);
		assert_eq!(stats.bytes_after, 0);
	}

	#[test]
	fn cache_clean_removes_root() {
		let temp = tempdir().expect("tempdir");
		let _env = CacheEnvGuard::new(temp.path());
		let path = parser_cache_root().join("aa").join("bb").join("entry.json");
		write_cache_file(&path, 10);

		cache_clean().expect("clean cache");

		assert!(!temp.path().exists());
	}

	#[test]
	fn cap_zero_evicts_all() {
		let temp = tempdir().expect("tempdir");
		let _env = CacheEnvGuard::new(temp.path());
		for index in 0..3_u64 {
			let path = parser_cache_root()
				.join("aa")
				.join("bb")
				.join(format!("entry-{index}.json"));
			write_cache_file(&path, 10);
		}

		let stats = gc_with_cap(0);

		assert_eq!(stats.scanned, 3);
		assert_eq!(stats.evicted, 3);
		assert_eq!(stats.kept, 0);
		assert_eq!(stats.bytes_after, 0);
	}

	#[test]
	fn cached_then_evicted_then_reread() {
		let cache_temp = tempdir().expect("cache tempdir");
		let source_temp = tempdir().expect("source tempdir");
		let _env = CacheEnvGuard::new(cache_temp.path());
		let source = source_temp.path().join("source.txt");
		fs::write(&source, "answer = 42\n").expect("write source");

		let (_, first_hit) = parse_clausewitz_file_cached(&source);
		let (_, second_hit) = parse_clausewitz_file_cached(&source);
		let gc_stats = gc_with_cap(0);
		let (_, third_hit) = parse_clausewitz_file_cached(&source);

		assert!(!first_hit);
		assert!(second_hit);
		assert_eq!(gc_stats.evicted, 1);
		assert!(!third_hit);
		assert!(parser_cache_file(&source).exists());
	}

	fn write_cache_file(path: &Path, size: usize) {
		fs::create_dir_all(path.parent().expect("cache parent")).expect("create parent");
		fs::write(path, vec![b'x'; size]).expect("write cache file");
	}

	fn set_mtime(path: &Path, time: SystemTime) {
		set_file_mtime(path, FileTime::from_system_time(time)).expect("set mtime");
	}
}
