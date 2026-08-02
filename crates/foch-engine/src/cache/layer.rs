use super::{CacheError, DagBaseCache, ModDiffCache, ModParseCache, ModsetCache};
use foch_core::cache::default_foch_cache_dir;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheLayer {
	Mods,
	Diffs,
	DagBase,
	Modsets,
	CwtRules,
	Parse,
}

impl CacheLayer {
	pub fn name(self) -> &'static str {
		match self {
			Self::Mods => "mods",
			Self::Diffs => "diffs",
			Self::DagBase => "dag-base",
			Self::Modsets => "modsets",
			Self::CwtRules => "cwt-rules",
			Self::Parse => "parse",
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheLayerEntryInfo {
	pub layer: CacheLayer,
	pub key: String,
	pub path: PathBuf,
	pub size_bytes: u64,
	pub modified: SystemTime,
}

pub struct EvictionStats {
	pub removed_entries: usize,
	pub freed_bytes: u64,
}

/// Filesystem lifecycle only — deliberately NOT a generic key/value store
/// (see docs/cache-architecture.md). Operates on on-disk entries by age/size.
pub trait CacheLayerOps {
	fn layer(&self) -> super::CacheLayer;
	fn path(&self) -> &Path;
	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError>;
	fn total_bytes(&self) -> Result<u64, super::CacheError>;
	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError>;
	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError>;
	fn clear(&self) -> Result<(), super::CacheError>;
}

pub fn all_layers() -> Vec<Box<dyn CacheLayerOps>> {
	all_layers_at(&default_foch_cache_dir())
}

fn all_layers_at(root: &Path) -> Vec<Box<dyn CacheLayerOps>> {
	vec![
		Box::new(ModParseCache::open(&root.join(CacheLayer::Mods.name()))),
		Box::new(ModDiffCache::open(&root.join(CacheLayer::Diffs.name()))),
		Box::new(DagBaseCache::open(&root.join(CacheLayer::DagBase.name()))),
		Box::new(ModsetCache::open(root)),
		Box::new(FileCacheLayer::new(
			CacheLayer::CwtRules,
			root.join(CacheLayer::CwtRules.name()),
			"bin",
		)),
		Box::new(FileCacheLayer::new(
			CacheLayer::Parse,
			root.join(CacheLayer::Parse.name()),
			"bin",
		)),
	]
}

impl CacheLayerOps for ModParseCache {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::Mods
	}

	fn path(&self) -> &Path {
		self.root()
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		list_file_entries(CacheLayer::Mods, self.root(), "rkyv")
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		purge_file_entries(self.root(), self.list_entries()?, days)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		evict_file_entries(self.list_entries()?, cap_bytes)
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(self.root())
	}
}

impl CacheLayerOps for ModDiffCache {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::Diffs
	}

	fn path(&self) -> &Path {
		self.layer_root()
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		list_file_entries(CacheLayer::Diffs, self.entries_root(), "bin")
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		purge_file_entries(self.entries_root(), self.list_entries()?, days)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		evict_file_entries(self.list_entries()?, cap_bytes)
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(self.layer_root())
	}
}

impl CacheLayerOps for DagBaseCache {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::DagBase
	}

	fn path(&self) -> &Path {
		self.layer_root()
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		list_file_entries(CacheLayer::DagBase, self.entries_root(), "bin")
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		purge_file_entries(self.entries_root(), self.list_entries()?, days)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		evict_file_entries(self.list_entries()?, cap_bytes)
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(self.layer_root())
	}
}

impl CacheLayerOps for ModsetCache {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::Modsets
	}

	fn path(&self) -> &Path {
		self.layer_root()
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		ModsetCache::list_entries(self).map(|entries| {
			entries
				.into_iter()
				.map(|entry| CacheLayerEntryInfo {
					layer: CacheLayer::Modsets,
					key: entry.key,
					path: entry.tarball_path,
					size_bytes: entry.size_bytes,
					modified: entry.modified,
				})
				.collect()
		})
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		ModsetCache::purge_older_than(self, days)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		let entries = ModsetCache::list_entries(self)?;
		let eviction_entries = entries
			.iter()
			.map(|entry| CacheLayerEntryInfo {
				layer: CacheLayer::Modsets,
				key: entry.key.clone(),
				path: entry.tarball_path.clone(),
				size_bytes: entry.size_bytes,
				modified: entry.modified,
			})
			.collect::<Vec<_>>();
		let evicted_paths = eviction_plan(eviction_entries, cap_bytes)
			.into_iter()
			.map(|entry| entry.path)
			.collect::<HashSet<_>>();
		let mut removed_entries = 0_usize;
		let mut freed_bytes = 0_u64;
		for entry in entries {
			if !evicted_paths.contains(&entry.tarball_path) {
				continue;
			}
			remove_if_exists(&entry.tarball_path)?;
			remove_if_exists(&entry.report_path)?;
			removed_entries += 1;
			freed_bytes = freed_bytes.saturating_add(entry.size_bytes);
		}
		prune_empty_dirs(self.layer_root());
		Ok(EvictionStats {
			removed_entries,
			freed_bytes,
		})
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(self.layer_root())
	}
}

struct FileCacheLayer {
	layer: CacheLayer,
	root: PathBuf,
	extension: &'static str,
}

impl FileCacheLayer {
	fn new(layer: CacheLayer, root: PathBuf, extension: &'static str) -> Self {
		Self {
			layer,
			root,
			extension,
		}
	}
}

impl CacheLayerOps for FileCacheLayer {
	fn layer(&self) -> super::CacheLayer {
		self.layer
	}

	fn path(&self) -> &Path {
		&self.root
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		list_file_entries(self.layer, &self.root, self.extension)
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		purge_file_entries(&self.root, self.list_entries()?, days)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		evict_file_entries(self.list_entries()?, cap_bytes)
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(&self.root)
	}
}

fn list_file_entries(
	layer: CacheLayer,
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
			layer,
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

	#[test]
	fn all_layers_present_and_report_size() {
		let temp = tempfile::tempdir().expect("cache root");
		let layers = all_layers_at(temp.path());
		fs::write(layers[0].path().join("entry.rkyv"), b"cache").expect("cache entry");

		assert_eq!(layers.len(), 6);
		assert_eq!(
			layers.iter().map(|layer| layer.layer()).collect::<Vec<_>>(),
			vec![
				CacheLayer::Mods,
				CacheLayer::Diffs,
				CacheLayer::DagBase,
				CacheLayer::Modsets,
				CacheLayer::CwtRules,
				CacheLayer::Parse,
			]
		);
		for layer in &layers {
			assert!(layer.path().starts_with(temp.path()));
		}
		assert_eq!(layers[0].total_bytes().expect("mods cache size"), 5);
	}

	#[test]
	fn eviction_plan_uses_newest_first_byte_cap_policy() {
		let entries = (0..4)
			.map(|index| CacheLayerEntryInfo {
				layer: CacheLayer::Mods,
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
				layer: CacheLayer::Parse,
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
