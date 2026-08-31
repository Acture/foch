use super::parser::{ParseResult, parse_clausewitz_content, parse_clausewitz_file};
use crate::platform::cache_store::{cache_version_namespace, default_foch_cache_dir};
use filetime::{FileTime, set_file_mtime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PARSE_CACHE_VERSION: &str = "11.0.0";
const PARSE_CACHE_DIR_NAME: &str = "parse";
const OBSOLETE_PARSE_CACHE_DIR_NAME: &str = "parse_cache";

#[cfg(test)]
thread_local! {
	static TEST_CACHE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
	static TEST_OBSOLETE_CACHE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ParseCacheEntry {
	version: String,
	content_key: String,
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

#[derive(Debug, Clone)]
pub struct CacheEntryInfo {
	pub key: String,
	pub path: PathBuf,
	pub size_bytes: u64,
	pub modified: SystemTime,
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
	let Ok(bytes) = fs::read(path) else {
		return (parse_clausewitz_file(path), false);
	};
	let content_key = parse_content_key(path, &bytes);
	parse_clausewitz_bytes_with_key(path, &bytes, content_key)
}

/// Parses caller-supplied bytes without reopening `path`.
///
/// The persistent cache is addressed only by parser mode and the actual bytes;
/// external installation or snapshot identities do not create a second parser
/// identity for identical input.
pub fn parse_clausewitz_bytes_cached(path: &Path, bytes: &[u8]) -> (ParseResult, bool) {
	let content_key = parse_content_key(path, bytes);
	parse_clausewitz_bytes_with_key(path, bytes, content_key)
}

fn parse_clausewitz_bytes_with_key(
	path: &Path,
	bytes: &[u8],
	content_key: String,
) -> (ParseResult, bool) {
	let cache_path = cache_file_for_key(&parser_cache_root(), &content_key);

	if let Some(mut result) = load_cache_hit(&cache_path, &content_key) {
		result.ast.path = path.to_path_buf();
		touch_cache_file(&cache_path);
		return (result, true);
	}

	let content = crate::game::eu4::text::decode_paradox_bytes(bytes);
	let parsed = parse_clausewitz_content(path.to_path_buf(), &content);
	let entry = ParseCacheEntry {
		version: PARSE_CACHE_VERSION.to_string(),
		content_key,
		result: parsed.clone(),
	};
	store_parse_cache_entry(&cache_path, &entry);

	(parsed, false)
}

fn active_cache_namespace() -> String {
	cache_version_namespace(PARSE_CACHE_VERSION).expect("parse cache version is valid SemVer")
}

fn parse_content_key(path: &Path, bytes: &[u8]) -> String {
	let mut hasher = Sha256::new();
	let mode = parser_mode(path);
	hasher.update((mode.len() as u64).to_le_bytes());
	hasher.update(mode);
	hasher.update((bytes.len() as u64).to_le_bytes());
	hasher.update(bytes);
	format!("{:x}", hasher.finalize())
}

fn parser_mode(path: &Path) -> &'static [u8] {
	if is_lua_path(path) {
		b"lua"
	} else {
		b"clausewitz"
	}
}

fn is_lua_path(path: &Path) -> bool {
	path.extension()
		.and_then(|extension| extension.to_str())
		.is_some_and(|extension| extension.eq_ignore_ascii_case("lua"))
}

fn load_cache_hit(path: &Path, content_key: &str) -> Option<ParseResult> {
	let raw = fs::read(path).ok()?;
	let entry = bincode::deserialize::<ParseCacheEntry>(&raw).ok()?;
	if entry.version != PARSE_CACHE_VERSION || entry.content_key != content_key {
		return None;
	}
	Some(entry.result)
}

pub fn parser_cache_root() -> PathBuf {
	parse_cache_base_root().join(active_cache_namespace())
}

fn parse_cache_base_root() -> PathBuf {
	#[cfg(test)]
	if let Some(root) = test_cache_root() {
		return root;
	}

	default_foch_cache_dir().join(PARSE_CACHE_DIR_NAME)
}

fn obsolete_parse_cache_base_root() -> Option<PathBuf> {
	#[cfg(test)]
	if let Some(root) = test_obsolete_cache_root() {
		return Some(root);
	}
	#[cfg(test)]
	if test_cache_root().is_some() {
		return None;
	}

	Some(default_foch_cache_dir().join(OBSOLETE_PARSE_CACHE_DIR_NAME))
}

#[cfg(test)]
fn test_cache_root() -> Option<PathBuf> {
	TEST_CACHE_ROOT.with(|root| root.borrow().clone())
}

#[cfg(test)]
fn set_test_cache_root(root: Option<PathBuf>) -> Option<PathBuf> {
	TEST_CACHE_ROOT.with(|current| current.replace(root))
}

#[cfg(test)]
fn test_obsolete_cache_root() -> Option<PathBuf> {
	TEST_OBSOLETE_CACHE_ROOT.with(|root| root.borrow().clone())
}

#[cfg(test)]
fn set_test_obsolete_cache_root(root: Option<PathBuf>) -> Option<PathBuf> {
	TEST_OBSOLETE_CACHE_ROOT.with(|current| current.replace(root))
}

fn cache_file_for_key(root: &Path, key: &str) -> PathBuf {
	root.join(&key[0..2])
		.join(&key[2..4])
		.join(format!("{key}.bin"))
}

#[cfg(test)]
fn parser_cache_file(path: &Path) -> PathBuf {
	let bytes = fs::read(path).expect("read parser cache test source");
	let key = parse_content_key(path, &bytes);
	cache_file_for_key(&parser_cache_root(), &key)
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
	let Ok(raw) = bincode::serialize(entry) else {
		return;
	};
	let tmp = path.with_extension(format!("bin.{}.tmp", std::process::id()));
	if fs::write(&tmp, raw).is_err() {
		return;
	}
	if fs::rename(&tmp, path).is_err() {
		let _ = fs::remove_file(tmp);
	}
}

pub fn cache_stats() -> CacheStats {
	let files = collect_all_cache_files();
	let mut stats = CacheStats {
		root: parser_cache_root(),
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

pub fn list_entries() -> Vec<CacheEntryInfo> {
	let mut entries = collect_all_cache_files()
		.into_iter()
		.map(|file| CacheEntryInfo {
			key: file
				.path
				.file_name()
				.and_then(|name| name.to_str())
				.unwrap_or("<unknown>")
				.to_string(),
			path: file.path,
			size_bytes: file.size,
			modified: file.mtime,
		})
		.collect::<Vec<_>>();
	entries.sort_by(|left, right| {
		left.modified
			.cmp(&right.modified)
			.then_with(|| left.key.cmp(&right.key))
			.then_with(|| left.path.cmp(&right.path))
	});
	entries
}

pub fn purge_older_than(days: u32) -> io::Result<usize> {
	let cutoff = cutoff_for_days(days);
	let mut purged = 0_usize;
	for file in collect_all_cache_files() {
		if file.mtime >= cutoff {
			continue;
		}
		match fs::remove_file(&file.path) {
			Ok(()) => purged += 1,
			Err(err) if err.kind() == io::ErrorKind::NotFound => purged += 1,
			Err(err) => return Err(err),
		}
	}
	prune_cache_roots();
	Ok(purged)
}

pub fn gc_with_cap(cap_bytes: u64) -> GcStats {
	let files = collect_all_cache_files();

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
	prune_cache_roots();

	let remaining = collect_all_cache_files();
	GcStats {
		scanned,
		kept: remaining.len() as u64,
		evicted,
		bytes_before,
		bytes_after: remaining.iter().map(|file| file.size).sum(),
	}
}

pub fn cache_clean() -> io::Result<()> {
	for root in cache_base_roots() {
		if root.exists() {
			fs::remove_dir_all(root)?;
		}
	}
	Ok(())
}

fn collect_all_cache_files() -> Vec<CacheFile> {
	let current_roots = cache_current_roots();
	let mut files = Vec::new();
	for root in cache_base_roots() {
		collect_cache_files(&root, &current_roots, &mut files);
	}
	files
}

fn cache_base_roots() -> Vec<PathBuf> {
	let mut roots = vec![parse_cache_base_root()];
	if let Some(obsolete) = obsolete_parse_cache_base_root()
		&& !roots.iter().any(|root| root == &obsolete)
	{
		roots.push(obsolete);
	}
	roots
}

fn cache_current_roots() -> Vec<PathBuf> {
	vec![parser_cache_root()]
}

fn collect_cache_files(root: &Path, current_roots: &[PathBuf], files: &mut Vec<CacheFile>) {
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
				is_current_version: current_roots.iter().any(|root| path.starts_with(root)),
				path,
				size: metadata.len(),
				mtime: metadata.modified().unwrap_or(UNIX_EPOCH),
			});
		}
	}
}

fn cutoff_for_days(days: u32) -> SystemTime {
	SystemTime::now()
		.checked_sub(Duration::from_secs(days as u64 * 24 * 60 * 60))
		.unwrap_or(UNIX_EPOCH)
}

fn prune_cache_roots() {
	for root in cache_base_roots() {
		prune_empty_dirs(&root);
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
		previous_obsolete: Option<PathBuf>,
	}

	impl CacheEnvGuard {
		fn new(root: &Path) -> Self {
			Self {
				previous: set_test_cache_root(Some(root.to_path_buf())),
				previous_obsolete: set_test_obsolete_cache_root(None),
			}
		}

		fn new_with_obsolete(root: &Path, obsolete_root: &Path) -> Self {
			Self {
				previous: set_test_cache_root(Some(root.to_path_buf())),
				previous_obsolete: set_test_obsolete_cache_root(Some(obsolete_root.to_path_buf())),
			}
		}
	}

	impl Drop for CacheEnvGuard {
		fn drop(&mut self) {
			set_test_cache_root(self.previous.take());
			set_test_obsolete_cache_root(self.previous_obsolete.take());
		}
	}

	#[test]
	fn shard_path() {
		let temp = tempdir().expect("tempdir");
		let _env = CacheEnvGuard::new(temp.path());
		let key = parse_content_key(Path::new("/mods/test/common/foo.txt"), b"answer = 42\n");
		let cache_file = cache_file_for_key(&parser_cache_root(), &key);
		let root = parser_cache_root();
		let relative = cache_file.strip_prefix(&root).expect("under cache root");
		let parts: Vec<_> = relative.iter().collect();

		assert_eq!(root, temp.path().join(active_cache_namespace()));
		assert_eq!(parts.len(), 3);
		let key = parts[2]
			.to_string_lossy()
			.strip_suffix(".bin")
			.expect("bin suffix")
			.to_string();
		assert_eq!(key.len(), 64);
		assert_eq!(parts[0], OsStr::new(&key[0..2]));
		assert_eq!(parts[1], OsStr::new(&key[2..4]));
	}

	#[test]
	fn parse_preserves_obsolete_cache_generations_until_explicit_gc() {
		let cache_temp = tempdir().expect("cache tempdir");
		let source_temp = tempdir().expect("source tempdir");
		let _env = CacheEnvGuard::new(cache_temp.path());
		let obsolete = cache_temp.path().join("v8");
		let unrelated = cache_temp.path().join("metadata");
		fs::create_dir_all(&obsolete).expect("create obsolete generation");
		fs::create_dir_all(&unrelated).expect("create unrelated directory");
		fs::write(obsolete.join("entry.json"), "stale").expect("write obsolete entry");
		fs::write(unrelated.join("owner.txt"), "keep").expect("write unrelated entry");
		let source = source_temp.path().join("source.txt");
		fs::write(&source, "root = { value = yes }\n").expect("write source");

		let (_, hit) = parse_clausewitz_file_cached(&source);

		assert!(!hit);
		assert!(obsolete.join("entry.json").is_file());
		assert!(parser_cache_root().is_dir());
		assert!(unrelated.join("owner.txt").is_file());

		let stats = gc_with_cap(0);

		assert!(stats.evicted >= 2);
		assert!(!obsolete.exists());
		assert!(!unrelated.exists());
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
	fn identical_content_reuses_cache_across_paths_and_rebases_ast_path() {
		let cache_temp = tempdir().expect("cache tempdir");
		let source_temp = tempdir().expect("source tempdir");
		let _env = CacheEnvGuard::new(cache_temp.path());
		let first = source_temp.path().join("first.txt");
		let second = source_temp.path().join("second.txt");
		fs::write(&first, "answer = 42\n").expect("write first source");
		fs::write(&second, "answer = 42\n").expect("write second source");

		let (_, first_hit) = parse_clausewitz_file_cached(&first);
		let (second_result, second_hit) = parse_clausewitz_file_cached(&second);

		assert!(!first_hit);
		assert!(second_hit);
		assert_eq!(second_result.ast.path, second);
		assert_eq!(cache_stats().file_count, 1);
	}

	#[test]
	fn caller_supplied_byte_cache_reuses_identical_bytes_without_reopening_path() {
		let cache_temp = tempdir().expect("cache tempdir");
		let _env = CacheEnvGuard::new(cache_temp.path());
		let missing_path = Path::new("/missing/mod/common/scripted_effects/a.txt");
		let bytes = b"effect = { add_prestige = 1 }\n";

		let (first, first_hit) = parse_clausewitz_bytes_cached(missing_path, bytes);
		let (second, second_hit) = parse_clausewitz_bytes_cached(missing_path, bytes);

		assert!(!first_hit);
		assert!(second_hit);
		assert_eq!(first, second);
		assert_eq!(cache_stats().file_count, 1);
	}

	#[test]
	fn lua_and_clausewitz_modes_have_distinct_content_keys() {
		let bytes = b"-- comment\nanswer = 42\n";
		let lua_key = parse_content_key(Path::new("defines.lua"), bytes);
		let clausewitz_key = parse_content_key(Path::new("interface.gui"), bytes);

		assert_ne!(lua_key, clausewitz_key);
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
	fn parse_preserves_obsolete_cache_root_until_explicit_clean() {
		let cache_temp = tempdir().expect("cache tempdir");
		let source_temp = tempdir().expect("source tempdir");
		let new_root = cache_temp.path().join(PARSE_CACHE_DIR_NAME);
		let obsolete_root = cache_temp.path().join(OBSOLETE_PARSE_CACHE_DIR_NAME);
		let _env = CacheEnvGuard::new_with_obsolete(&new_root, &obsolete_root);
		let source = source_temp.path().join("source.txt");
		fs::write(&source, "root = { value = yes }\n").expect("write source");
		let obsolete_generation = obsolete_root.join("v9");
		fs::create_dir_all(&obsolete_generation).expect("create obsolete cache generation");
		fs::write(obsolete_generation.join("entry.json"), "stale").expect("write obsolete entry");

		let (_, hit) = parse_clausewitz_file_cached(&source);

		assert!(!hit);
		assert!(obsolete_generation.is_dir());

		cache_clean().expect("clean all parse cache roots");

		assert!(!obsolete_root.exists());
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
