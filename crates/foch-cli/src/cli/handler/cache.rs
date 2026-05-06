use crate::cli::arg::{
	FochCliCacheArgs, FochCliCacheCleanArgs, FochCliCacheClearArgs, FochCliCacheCommands,
	FochCliCacheLayerArg, FochCliCacheListArgs,
};
use crate::cli::handler::HandlerResult;
use foch_engine::{
	ModsetCache, default_dag_base_cache_dir, default_foch_cache_dir, default_mod_diff_cache_dir,
	default_mod_parse_cache_dir,
};
use foch_language::analyzer::semantic_index::parse_cache::{cache_cap_bytes, gc_with_cap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheLayer {
	Mods,
	Diffs,
	DagBase,
	Modsets,
}

#[derive(Clone, Debug)]
struct LayerEntry {
	layer: CacheLayer,
	key: String,
	path: PathBuf,
	size_bytes: u64,
	modified: SystemTime,
}

#[derive(Clone, Debug, Default)]
struct LayerStats {
	entry_count: usize,
	total_bytes: u64,
	oldest_mtime: Option<SystemTime>,
	newest_mtime: Option<SystemTime>,
}

pub fn handle_cache(cache_args: &FochCliCacheArgs) -> HandlerResult {
	match &cache_args.command {
		FochCliCacheCommands::Stats => handle_cache_stats(),
		FochCliCacheCommands::List(args) => handle_cache_list(args),
		FochCliCacheCommands::Clean(args) => handle_cache_clean(args),
		FochCliCacheCommands::Clear(args) => handle_cache_clear(args),
		FochCliCacheCommands::Where => handle_cache_where(),
		FochCliCacheCommands::Gc(args) => handle_cache_gc(args.cap_bytes),
	}
}

pub fn run_auto_gc() {
	let cap = cache_cap_bytes();
	let stats = gc_with_cap(cap);
	tracing::debug!(
		scanned = stats.scanned,
		kept = stats.kept,
		evicted = stats.evicted,
		bytes_before = stats.bytes_before,
		bytes_after = stats.bytes_after,
		cap_bytes = cap,
		"parse cache GC complete"
	);
}

fn handle_cache_stats() -> HandlerResult {
	let root = default_foch_cache_dir();
	let mut total_size = 0_u64;
	let mut total_entries = 0_usize;
	let mut oldest = None;
	let mut newest = None;

	println!("cache root: {}", root.display());
	println!("layer       entries  size       path");
	for layer in CacheLayer::all() {
		let stats = layer_stats(layer)?;
		total_size = total_size.saturating_add(stats.total_bytes);
		total_entries = total_entries.saturating_add(stats.entry_count);
		oldest = min_time(oldest, stats.oldest_mtime);
		newest = max_time(newest, stats.newest_mtime);
		println!(
			"{:<11} {:>7}  {:>9}  {}",
			layer.name(),
			stats.entry_count,
			format_bytes(stats.total_bytes),
			layer.path().display()
		);
	}
	println!(
		"total:      {:>7}  {:>9}",
		total_entries,
		format_bytes(total_size)
	);
	println!("oldest:     {}", format_optional_time(oldest));
	println!("newest:     {}", format_optional_time(newest));
	Ok(0)
}

fn handle_cache_list(args: &FochCliCacheListArgs) -> HandlerResult {
	let layers = args
		.layer
		.map(CacheLayer::from_arg)
		.map(|layer| vec![layer])
		.unwrap_or_else(CacheLayer::all);
	let mut entries = Vec::new();
	for layer in layers {
		entries.extend(layer_entries(layer)?);
	}
	entries.sort_by(|left, right| {
		left.layer
			.name()
			.cmp(right.layer.name())
			.then_with(|| left.modified.cmp(&right.modified))
			.then_with(|| left.key.cmp(&right.key))
	});

	println!("layer       modified                         size       key");
	for entry in entries {
		println!(
			"{:<11} {:<32} {:>9}  {}",
			entry.layer.name(),
			format_system_time(entry.modified),
			format_bytes(entry.size_bytes),
			entry.key
		);
	}
	Ok(0)
}

fn handle_cache_clean(args: &FochCliCacheCleanArgs) -> HandlerResult {
	let mut purged = 0_usize;
	purged += purge_layer_older_than(CacheLayer::Mods, args.older_than)?;
	purged += purge_layer_older_than(CacheLayer::Diffs, args.older_than)?;
	purged += purge_layer_older_than(CacheLayer::DagBase, args.older_than)?;
	purged += ModsetCache::open(&default_foch_cache_dir()).purge_older_than(args.older_than)?;
	println!(
		"removed: {purged} entries older than {} days",
		args.older_than
	);
	Ok(0)
}

fn handle_cache_clear(args: &FochCliCacheClearArgs) -> HandlerResult {
	let root = default_foch_cache_dir();
	if !args.yes {
		return Err("refusing to clear cache without --yes".into());
	}
	let stats = combined_stats()?;
	if root.exists() {
		fs::remove_dir_all(&root)?;
	}
	println!(
		"removed: {} entries ({}) from {}",
		stats.entry_count,
		format_bytes(stats.total_bytes),
		root.display()
	);
	Ok(0)
}

fn handle_cache_where() -> HandlerResult {
	println!("{}", default_foch_cache_dir().display());
	Ok(0)
}

fn handle_cache_gc(cap_bytes: Option<u64>) -> HandlerResult {
	let cap = cap_bytes.unwrap_or_else(cache_cap_bytes);
	let stats = gc_with_cap(cap);
	let evicted_bytes = stats.bytes_before.saturating_sub(stats.bytes_after);
	println!(
		"scanned:  {} files ({})",
		stats.scanned,
		format_bytes(stats.bytes_before)
	);
	println!(
		"evicted:  {} files ({})",
		stats.evicted,
		format_bytes(evicted_bytes)
	);
	println!(
		"kept:     {} files ({})",
		stats.kept,
		format_bytes(stats.bytes_after)
	);
	Ok(0)
}

impl CacheLayer {
	fn all() -> Vec<Self> {
		vec![Self::Mods, Self::Diffs, Self::DagBase, Self::Modsets]
	}

	fn from_arg(arg: FochCliCacheLayerArg) -> Self {
		match arg {
			FochCliCacheLayerArg::Mods => Self::Mods,
			FochCliCacheLayerArg::Diffs => Self::Diffs,
			FochCliCacheLayerArg::DagBase => Self::DagBase,
			FochCliCacheLayerArg::Modsets => Self::Modsets,
		}
	}

	fn name(self) -> &'static str {
		match self {
			Self::Mods => "mods",
			Self::Diffs => "diffs",
			Self::DagBase => "dag-base",
			Self::Modsets => "modsets",
		}
	}

	fn path(self) -> PathBuf {
		match self {
			Self::Mods => default_mod_parse_cache_dir(),
			Self::Diffs => default_mod_diff_cache_dir(),
			Self::DagBase => default_dag_base_cache_dir(),
			Self::Modsets => default_foch_cache_dir().join("modsets"),
		}
	}

	fn extension(self) -> Option<&'static str> {
		match self {
			Self::Mods => Some("rkyv"),
			Self::Diffs | Self::DagBase => Some("bin"),
			Self::Modsets => None,
		}
	}
}

fn layer_stats(layer: CacheLayer) -> Result<LayerStats, Box<dyn std::error::Error>> {
	let entries = layer_entries(layer)?;
	Ok(stats_from_entries(&entries))
}

fn combined_stats() -> Result<LayerStats, Box<dyn std::error::Error>> {
	let mut entries = Vec::new();
	for layer in CacheLayer::all() {
		entries.extend(layer_entries(layer)?);
	}
	Ok(stats_from_entries(&entries))
}

fn stats_from_entries(entries: &[LayerEntry]) -> LayerStats {
	let mut stats = LayerStats {
		entry_count: entries.len(),
		total_bytes: entries.iter().map(|entry| entry.size_bytes).sum(),
		oldest_mtime: None,
		newest_mtime: None,
	};
	for entry in entries {
		stats.oldest_mtime = min_time(stats.oldest_mtime, Some(entry.modified));
		stats.newest_mtime = max_time(stats.newest_mtime, Some(entry.modified));
	}
	stats
}

fn layer_entries(layer: CacheLayer) -> Result<Vec<LayerEntry>, Box<dyn std::error::Error>> {
	if layer == CacheLayer::Modsets {
		return Ok(ModsetCache::open(&default_foch_cache_dir())
			.list_entries()?
			.into_iter()
			.map(|entry| LayerEntry {
				layer,
				key: entry.key,
				path: entry.tarball_path,
				size_bytes: entry.size_bytes,
				modified: entry.modified,
			})
			.collect());
	}

	let root = layer.path();
	let Some(extension) = layer.extension() else {
		return Ok(Vec::new());
	};
	let mut entries = Vec::new();
	if !root.is_dir() {
		return Ok(entries);
	}
	for entry in WalkDir::new(&root)
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
		let metadata = fs::metadata(&path)?;
		entries.push(LayerEntry {
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
	Ok(entries)
}

fn purge_layer_older_than(
	layer: CacheLayer,
	days: u32,
) -> Result<usize, Box<dyn std::error::Error>> {
	if layer == CacheLayer::Modsets {
		return Ok(ModsetCache::open(&default_foch_cache_dir()).purge_older_than(days)?);
	}
	let cutoff = SystemTime::now()
		.checked_sub(Duration::from_secs(days as u64 * 24 * 60 * 60))
		.unwrap_or(UNIX_EPOCH);
	let mut purged = 0_usize;
	for entry in layer_entries(layer)? {
		if entry.modified >= cutoff {
			continue;
		}
		match fs::remove_file(&entry.path) {
			Ok(()) => purged += 1,
			Err(err) if err.kind() == std::io::ErrorKind::NotFound => purged += 1,
			Err(err) => return Err(err.into()),
		}
	}
	prune_empty_dirs(&layer.path());
	Ok(purged)
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

fn min_time(current: Option<SystemTime>, candidate: Option<SystemTime>) -> Option<SystemTime> {
	match (current, candidate) {
		(Some(current), Some(candidate)) => Some(current.min(candidate)),
		(None, Some(candidate)) => Some(candidate),
		(Some(current), None) => Some(current),
		(None, None) => None,
	}
}

fn max_time(current: Option<SystemTime>, candidate: Option<SystemTime>) -> Option<SystemTime> {
	match (current, candidate) {
		(Some(current), Some(candidate)) => Some(current.max(candidate)),
		(None, Some(candidate)) => Some(candidate),
		(Some(current), None) => Some(current),
		(None, None) => None,
	}
}

fn format_bytes(bytes: u64) -> String {
	const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
	if bytes < 1024 {
		return format!("{bytes} B");
	}

	let mut value = bytes as f64;
	let mut unit = 0_usize;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	format!("{value:.1} {}", UNITS[unit])
}

fn format_optional_time(time: Option<SystemTime>) -> String {
	match time {
		Some(time) => format_system_time(time),
		None => "n/a".to_string(),
	}
}

fn format_system_time(time: SystemTime) -> String {
	let timestamp = match time.duration_since(UNIX_EPOCH) {
		Ok(duration) => duration.as_secs() as i64,
		Err(err) => -(err.duration().as_secs() as i64),
	};
	let days = timestamp.div_euclid(86_400);
	let seconds_of_day = timestamp.rem_euclid(86_400);
	let hour = seconds_of_day / 3_600;
	let minute = (seconds_of_day % 3_600) / 60;
	let second = seconds_of_day % 60;
	let (year, month, day) = civil_from_days(days);
	format!(
		"{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC ({})",
		format_age(time)
	)
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, u32, u32) {
	let z = days_since_unix_epoch + 719_468;
	let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
	let doe = z - era * 146_097;
	let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
	let y = yoe + era * 400;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let d = doy - (153 * mp + 2) / 5 + 1;
	let m = mp + if mp < 10 { 3 } else { -9 };
	let year = y + if m <= 2 { 1 } else { 0 };
	(year, m as u32, d as u32)
}

fn format_age(time: SystemTime) -> String {
	match SystemTime::now().duration_since(time) {
		Ok(age) => format_duration_ago(age),
		Err(err) => format!("{} in the future", format_duration(err.duration())),
	}
}

fn format_duration_ago(duration: Duration) -> String {
	if duration < Duration::from_secs(5) {
		"just now".to_string()
	} else {
		format!("{} ago", format_duration(duration))
	}
}

fn format_duration(duration: Duration) -> String {
	let seconds = duration.as_secs();
	if seconds < 60 {
		format!("{seconds}s")
	} else if seconds < 3_600 {
		format!("{} min", seconds / 60)
	} else if seconds < 86_400 {
		format!("{} h", seconds / 3_600)
	} else {
		format!("{} days", seconds / 86_400)
	}
}
