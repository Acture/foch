use super::{
	CacheError, DagBaseCache, ModDiffCache, ModParseCache, ModsetCache, default_dag_base_cache_dir,
	default_mod_diff_cache_dir, default_mod_parse_cache_dir, default_modset_cache_dir,
};
use foch_language::analyzer::semantic_index::parse_cache;
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

	pub fn path(self) -> PathBuf {
		match self {
			Self::Mods => default_mod_parse_cache_dir(),
			Self::Diffs => default_mod_diff_cache_dir(),
			Self::DagBase => default_dag_base_cache_dir(),
			Self::Modsets => default_modset_cache_dir(),
			Self::CwtRules => foch_cwt::default_compiled_rule_cache_dir(),
			Self::Parse => parse_cache::parser_cache_root(),
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
	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError>;
	fn total_bytes(&self) -> Result<u64, super::CacheError>;
	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError>;
	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError>;
	fn clear(&self) -> Result<(), super::CacheError>;
}

pub fn all_layers() -> Vec<Box<dyn CacheLayerOps>> {
	vec![
		Box::new(ModParseCache::open_default()),
		Box::new(ModDiffCache::open_default()),
		Box::new(DagBaseCache::open_default()),
		Box::new(ModsetCache::open_default()),
		Box::new(CwtRuleCacheLayer),
		Box::new(ParseCacheLayer),
	]
}

impl CacheLayerOps for ModParseCache {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::Mods
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		list_file_entries(CacheLayer::Mods, &default_mod_parse_cache_dir(), "rkyv")
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		purge_file_entries(&default_mod_parse_cache_dir(), self.list_entries()?, days)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		evict_file_entries(self.list_entries()?, cap_bytes)
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(&default_mod_parse_cache_dir())
	}
}

impl CacheLayerOps for ModDiffCache {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::Diffs
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		list_file_entries(CacheLayer::Diffs, &default_mod_diff_cache_dir(), "bin")
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		purge_file_entries(&default_mod_diff_cache_dir(), self.list_entries()?, days)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		evict_file_entries(self.list_entries()?, cap_bytes)
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(&default_mod_diff_cache_dir())
	}
}

impl CacheLayerOps for DagBaseCache {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::DagBase
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		list_file_entries(CacheLayer::DagBase, &default_dag_base_cache_dir(), "bin")
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		purge_file_entries(&default_dag_base_cache_dir(), self.list_entries()?, days)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		evict_file_entries(self.list_entries()?, cap_bytes)
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(&default_dag_base_cache_dir())
	}
}

impl CacheLayerOps for ModsetCache {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::Modsets
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
		let evicted_keys = eviction_plan(eviction_entries, cap_bytes)
			.into_iter()
			.map(|entry| entry.key)
			.collect::<HashSet<_>>();
		let mut removed_entries = 0_usize;
		let mut freed_bytes = 0_u64;
		for entry in entries {
			if !evicted_keys.contains(&entry.key) {
				continue;
			}
			remove_if_exists(&entry.tarball_path)?;
			remove_if_exists(&entry.report_path)?;
			removed_entries += 1;
			freed_bytes = freed_bytes.saturating_add(entry.size_bytes);
		}
		prune_empty_dirs(&default_modset_cache_dir());
		Ok(EvictionStats {
			removed_entries,
			freed_bytes,
		})
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(&default_modset_cache_dir())
	}
}

struct CwtRuleCacheLayer;

impl CacheLayerOps for CwtRuleCacheLayer {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::CwtRules
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		list_file_entries(
			CacheLayer::CwtRules,
			&foch_cwt::default_compiled_rule_cache_dir(),
			"bin",
		)
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		purge_file_entries(
			&foch_cwt::default_compiled_rule_cache_dir(),
			self.list_entries()?,
			days,
		)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		evict_file_entries(self.list_entries()?, cap_bytes)
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		clear_dir(&foch_cwt::default_compiled_rule_cache_dir())
	}
}

struct ParseCacheLayer;

impl CacheLayerOps for ParseCacheLayer {
	fn layer(&self) -> super::CacheLayer {
		CacheLayer::Parse
	}

	fn list_entries(&self) -> Result<Vec<super::CacheLayerEntryInfo>, super::CacheError> {
		Ok(parse_cache::list_entries()
			.into_iter()
			.map(|entry| CacheLayerEntryInfo {
				layer: CacheLayer::Parse,
				key: entry.key,
				path: entry.path,
				size_bytes: entry.size_bytes,
				modified: entry.modified,
			})
			.collect())
	}

	fn total_bytes(&self) -> Result<u64, super::CacheError> {
		Ok(total_entry_bytes(&<Self as CacheLayerOps>::list_entries(
			self,
		)?))
	}

	fn purge_older_than(&self, days: u32) -> Result<usize, super::CacheError> {
		parse_cache::purge_older_than(days).map_err(CacheError::Io)
	}

	fn evict_to_byte_cap(&self, cap_bytes: u64) -> Result<EvictionStats, super::CacheError> {
		let stats = parse_cache::gc_with_cap(cap_bytes);
		Ok(EvictionStats {
			removed_entries: stats.evicted.min(usize::MAX as u64) as usize,
			freed_bytes: stats.bytes_before.saturating_sub(stats.bytes_after),
		})
	}

	fn clear(&self) -> Result<(), super::CacheError> {
		parse_cache::cache_clean().map_err(CacheError::Io)
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
		let layers = all_layers();
		assert_eq!(layers.len(), 6);
		for l in &layers {
			let _ = l.total_bytes().unwrap_or(0);
		}
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
