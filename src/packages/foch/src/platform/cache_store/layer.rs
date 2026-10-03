use super::CacheError;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheLayerEntryInfo {
	pub layer_name: String,
	pub key: String,
	pub path: PathBuf,
	pub size_bytes: u64,
	pub modified: SystemTime,
}

pub struct EvictionStats {
	pub removed_entries: usize,
	pub freed_bytes: u64,
}

/// Filesystem lifecycle for one named directory of extension-filtered entries.
///
/// The caller owns the meaning and enumeration of layer names. This type only
/// lists, sizes, evicts, and clears matching files on disk.
pub struct FileCacheLayer {
	name: String,
	root: PathBuf,
	extension: String,
}

impl FileCacheLayer {
	pub fn new(
		name: impl Into<String>,
		root: impl Into<PathBuf>,
		extension: impl Into<String>,
	) -> Self {
		Self {
			name: name.into(),
			root: root.into(),
			extension: extension.into(),
		}
	}

	pub fn name(&self) -> &str {
		&self.name
	}

	pub fn path(&self) -> &Path {
		&self.root
	}

	pub fn list_entries(&self) -> Result<Vec<CacheLayerEntryInfo>, CacheError> {
		list_file_entries(&self.name, &self.root, &self.extension)
	}

	pub fn total_bytes(&self) -> Result<u64, CacheError> {
		Ok(total_entry_bytes(&self.list_entries()?))
	}

	pub fn purge_older_than(&self, days: u32) -> Result<usize, CacheError> {
		purge_file_entries(&self.root, self.list_entries()?, days)
	}

	pub fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, CacheError> {
		evict_file_entries(self.list_entries()?, cap_bytes)
	}

	pub fn clear(&self) -> Result<(), CacheError> {
		clear_dir(&self.root)
	}
}

fn list_file_entries(
	layer_name: &str,
	root: &Path,
	extension: &str,
) -> Result<Vec<CacheLayerEntryInfo>, CacheError> {
	let mut entries = Vec::new();
	if !root.is_dir() {
		return Ok(entries);
	}
	for entry in WalkDir::new(root)
		.min_depth(1)
		.into_iter()
		.filter_map(Result::ok)
	{
		if !entry.file_type().is_file() {
			continue;
		}
		let path = entry.into_path();
		if path.extension().and_then(|value| value.to_str()) != Some(extension) {
			continue;
		}
		let metadata = fs::metadata(&path).map_err(CacheError::Io)?;
		entries.push(CacheLayerEntryInfo {
			layer_name: layer_name.to_string(),
			key: path
				.file_name()
				.and_then(|name| name.to_str())
				.unwrap_or("<unknown>")
				.to_string(),
			path,
			size_bytes: metadata.len(),
			modified: metadata.modified().unwrap_or(UNIX_EPOCH),
		});
	}
	entries.sort_by(|left, right| {
		left.modified
			.cmp(&right.modified)
			.then_with(|| left.key.cmp(&right.key))
			.then_with(|| left.path.cmp(&right.path))
	});
	Ok(entries)
}

fn total_entry_bytes(entries: &[CacheLayerEntryInfo]) -> u64 {
	entries.iter().map(|entry| entry.size_bytes).sum()
}

fn purge_file_entries(
	root: &Path,
	entries: Vec<CacheLayerEntryInfo>,
	days: u32,
) -> Result<usize, CacheError> {
	let cutoff = cutoff_for_days(days);
	let mut purged = 0_usize;
	for entry in entries {
		if entry.modified >= cutoff {
			continue;
		}
		remove_if_exists(&entry.path)?;
		purged += 1;
	}
	prune_empty_dirs(root);
	Ok(purged)
}

fn evict_file_entries(
	entries: Vec<CacheLayerEntryInfo>,
	cap_bytes: u64,
) -> Result<EvictionStats, CacheError> {
	let entries = eviction_plan(entries, cap_bytes);
	let mut removed_entries = 0_usize;
	let mut freed_bytes = 0_u64;
	let mut pruned_roots = Vec::new();
	for entry in entries {
		remove_if_exists(&entry.path)?;
		if let Some(parent) = entry.path.parent() {
			pruned_roots.push(parent.to_path_buf());
		}
		removed_entries += 1;
		freed_bytes = freed_bytes.saturating_add(entry.size_bytes);
	}
	for root in pruned_roots {
		prune_empty_dirs(&root);
	}
	Ok(EvictionStats {
		removed_entries,
		freed_bytes,
	})
}

fn eviction_plan(
	mut entries: Vec<CacheLayerEntryInfo>,
	cap_bytes: u64,
) -> Vec<CacheLayerEntryInfo> {
	entries.sort_by(|left, right| {
		right
			.modified
			.cmp(&left.modified)
			.then_with(|| left.key.cmp(&right.key))
			.then_with(|| left.path.cmp(&right.path))
	});
	let mut kept_bytes = 0_u64;
	let mut evicted = Vec::new();
	for entry in entries {
		let fits = cap_bytes > 0 && kept_bytes.saturating_add(entry.size_bytes) <= cap_bytes;
		if fits {
			kept_bytes = kept_bytes.saturating_add(entry.size_bytes);
			continue;
		}
		evicted.push(entry);
	}
	evicted
}

fn cutoff_for_days(days: u32) -> SystemTime {
	SystemTime::now()
		.checked_sub(Duration::from_secs(days as u64 * 24 * 60 * 60))
		.unwrap_or(UNIX_EPOCH)
}

fn remove_if_exists(path: &Path) -> Result<(), CacheError> {
	match fs::remove_file(path) {
		Ok(()) => Ok(()),
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(err) => Err(CacheError::Io(err)),
	}
}

fn clear_dir(path: &Path) -> Result<(), CacheError> {
	if path.exists() {
		fs::remove_dir_all(path).map_err(CacheError::Io)?;
	}
	Ok(())
}

fn prune_empty_dirs(root: &Path) {
	if !root.is_dir() {
		return;
	}
	let dirs = WalkDir::new(root)
		.min_depth(1)
		.contents_first(true)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_dir())
		.map(|entry| entry.into_path())
		.collect::<Vec<_>>();
	for dir in dirs {
		let _ = fs::remove_dir(&dir);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use filetime::{FileTime, set_file_mtime};

	fn write_entry(path: &Path, bytes: &[u8], modified: SystemTime) {
		fs::create_dir_all(path.parent().expect("cache entry parent"))
			.expect("create cache entry parent");
		fs::write(path, bytes).expect("write cache entry");
		set_file_mtime(path, FileTime::from_system_time(modified)).expect("set cache entry mtime");
	}

	#[test]
	fn generic_layer_reports_identity_entries_and_size() {
		let temp = tempfile::tempdir().expect("cache root");
		let layer = FileCacheLayer::new("objects", temp.path().join("objects"), "blob");
		write_entry(
			&layer.path().join("entry.blob"),
			b"cache",
			SystemTime::now(),
		);
		write_entry(
			&layer.path().join("ignored.tmp"),
			b"ignored",
			SystemTime::now(),
		);

		assert_eq!(layer.name(), "objects");
		assert_eq!(layer.path(), temp.path().join("objects"));
		let entries = layer.list_entries().expect("list generic entries");
		assert_eq!(entries.len(), 1);
		assert_eq!(entries[0].layer_name, "objects");
		assert_eq!(layer.total_bytes().expect("generic cache size"), 5);
	}

	#[test]
	fn file_cache_lifecycle_spans_flat_current_and_old_generations() {
		let temp = tempfile::tempdir().expect("cache root");
		let layer = FileCacheLayer::new("objects", temp.path().join("objects"), "blob");
		let old_time = SystemTime::now() - Duration::from_secs(40 * 24 * 60 * 60);
		let current_time = SystemTime::now();
		let current = layer.path().join("v2.0.0/current.blob");
		let old = layer.path().join("v0.0.1/old.blob");
		let flat = layer.path().join("flat.blob");
		write_entry(&current, b"c", current_time);
		write_entry(&old, b"oo", old_time);
		write_entry(&flat, b"fff", old_time);

		assert_eq!(layer.list_entries().expect("list entries").len(), 3);
		assert_eq!(layer.total_bytes().expect("total bytes"), 6);
		assert_eq!(layer.purge_older_than(30).expect("purge entries"), 2);
		assert!(current.is_file());
		assert!(!old.exists());
		assert!(!flat.exists());

		write_entry(&old, b"oo", old_time);
		write_entry(&flat, b"fff", old_time);
		let eviction = layer.evict_to_byte_cap(1).expect("evict entries");
		assert_eq!(eviction.removed_entries, 2);
		assert_eq!(eviction.freed_bytes, 5);
		assert!(current.is_file());
		assert!(!old.exists());
		assert!(!flat.exists());
	}

	#[test]
	fn eviction_plan_uses_newest_first_byte_cap_policy() {
		let entries = (0..4)
			.map(|index| CacheLayerEntryInfo {
				layer_name: "objects".to_string(),
				key: format!("entry-{index}"),
				path: PathBuf::from(format!("entry-{index}.rkyv")),
				size_bytes: 10,
				modified: UNIX_EPOCH + Duration::from_secs(index),
			})
			.collect::<Vec<_>>();

		let evicted = eviction_plan(entries, 20);

		assert_eq!(
			evicted
				.into_iter()
				.map(|entry| entry.key)
				.collect::<Vec<_>>(),
			vec!["entry-1", "entry-0"]
		);
	}

	#[test]
	fn eviction_plan_with_zero_cap_evicts_every_entry() {
		let entries = (0..2)
			.map(|index| CacheLayerEntryInfo {
				layer_name: "objects".to_string(),
				key: format!("entry-{index}"),
				path: PathBuf::from(format!("entry-{index}.json")),
				size_bytes: 10,
				modified: UNIX_EPOCH + Duration::from_secs(index),
			})
			.collect::<Vec<_>>();

		let evicted = eviction_plan(entries, 0);

		assert_eq!(evicted.len(), 2);
	}
}
