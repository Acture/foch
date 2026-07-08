use crate::cli::arg::{
	FochCliCacheArgs, FochCliCacheCleanArgs, FochCliCacheClearArgs, FochCliCacheCommands,
	FochCliCacheLayerArg, FochCliCacheListArgs, FochCliCacheStatsArgs,
};
use crate::cli::handler::HandlerResult;
use foch_engine::{
	CacheLayer, CacheLayerEntryInfo, CacheLayerOps, all_layers, cache_cap_bytes,
	default_foch_cache_dir,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default)]
struct LayerStats {
	entry_count: usize,
	total_bytes: u64,
	oldest_mtime: Option<SystemTime>,
	newest_mtime: Option<SystemTime>,
}

pub fn handle_cache(cache_args: &FochCliCacheArgs) -> HandlerResult {
	match &cache_args.command {
		FochCliCacheCommands::Stats(args) => handle_cache_stats(args),
		FochCliCacheCommands::List(args) => handle_cache_list(args),
		FochCliCacheCommands::Clean(args) => handle_cache_clean(args),
		FochCliCacheCommands::Clear(args) => handle_cache_clear(args),
		FochCliCacheCommands::Where => handle_cache_where(),
	}
}

/// Runs automatic post-command cache GC by byte-capping every cache layer.
pub fn run_auto_cache_gc() {
	let cap = cache_cap_bytes();
	for layer in all_layers() {
		match layer.evict_to_byte_cap(cap) {
			Ok(stats) => tracing::debug!(
				layer = layer.layer().name(),
				evicted = stats.removed_entries,
				freed_bytes = stats.freed_bytes,
				cap_bytes = cap,
				"cache GC complete"
			),
			Err(err) => tracing::warn!(
				layer = layer.layer().name(),
				error = %err,
				"cache GC failed"
			),
		}
	}
}

fn handle_cache_stats(args: &FochCliCacheStatsArgs) -> HandlerResult {
	let root = default_foch_cache_dir();
	let layers = selected_layers(args.layer);
	let mut total_size = 0_u64;
	let mut total_entries = 0_usize;
	let mut oldest = None;
	let mut newest = None;

	println!("cache root: {}", root.display());
	println!("layer       entries  size       path");
	for layer in &layers {
		let stats = layer_stats(layer.as_ref())?;
		total_size = total_size.saturating_add(stats.total_bytes);
		total_entries = total_entries.saturating_add(stats.entry_count);
		oldest = min_time(oldest, stats.oldest_mtime);
		newest = max_time(newest, stats.newest_mtime);
		println!(
			"{:<11} {:>7}  {:>9}  {}",
			layer.layer().name(),
			stats.entry_count,
			format_bytes(stats.total_bytes),
			layer.layer().path().display()
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
	let layers = selected_layers(args.layer);
	let mut entries = Vec::new();
	for layer in &layers {
		entries.extend(layer.list_entries()?);
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
	let layers = selected_layers(args.layer);

	let mut purged = 0_usize;
	for layer in &layers {
		purged += layer.purge_older_than(args.older_than)?;
	}
	println!(
		"removed: {purged} entries older than {} days",
		args.older_than
	);

	let cap = args.byte_cap.unwrap_or_else(cache_cap_bytes);
	let mut evicted_entries = 0_usize;
	let mut freed_bytes = 0_u64;
	for layer in &layers {
		let stats = layer.evict_to_byte_cap(cap)?;
		evicted_entries = evicted_entries.saturating_add(stats.removed_entries);
		freed_bytes = freed_bytes.saturating_add(stats.freed_bytes);
	}
	println!(
		"byte-cap: evicted {} files ({}) with cap {}",
		evicted_entries,
		format_bytes(freed_bytes),
		format_bytes(cap)
	);
	Ok(0)
}

fn handle_cache_clear(args: &FochCliCacheClearArgs) -> HandlerResult {
	if !args.yes {
		return Err("refusing to clear cache without --yes".into());
	}
	let layers = selected_layers(args.layer);
	let stats = combined_stats(&layers)?;
	for layer in &layers {
		layer.clear()?;
	}
	println!(
		"removed: {} entries ({}) from {}",
		stats.entry_count,
		format_bytes(stats.total_bytes),
		format_layer_selection(args.layer)
	);
	Ok(0)
}

fn handle_cache_where() -> HandlerResult {
	println!("{}", default_foch_cache_dir().display());
	Ok(0)
}

fn selected_layers(arg: Option<FochCliCacheLayerArg>) -> Vec<Box<dyn CacheLayerOps>> {
	let selected = match arg.unwrap_or(FochCliCacheLayerArg::All) {
		FochCliCacheLayerArg::Parse => Some(CacheLayer::Parse),
		FochCliCacheLayerArg::Mods => Some(CacheLayer::Mods),
		FochCliCacheLayerArg::Diffs => Some(CacheLayer::Diffs),
		FochCliCacheLayerArg::DagBase => Some(CacheLayer::DagBase),
		FochCliCacheLayerArg::Modsets => Some(CacheLayer::Modsets),
		FochCliCacheLayerArg::CwtRules => Some(CacheLayer::CwtRules),
		FochCliCacheLayerArg::All => None,
	};
	all_layers()
		.into_iter()
		.filter(|layer| selected.is_none_or(|selected| layer.layer() == selected))
		.collect()
}

fn layer_stats(layer: &dyn CacheLayerOps) -> Result<LayerStats, Box<dyn std::error::Error>> {
	let entries = layer.list_entries()?;
	Ok(stats_from_entries(&entries))
}

fn combined_stats(
	layers: &[Box<dyn CacheLayerOps>],
) -> Result<LayerStats, Box<dyn std::error::Error>> {
	let mut entries = Vec::new();
	for layer in layers {
		entries.extend(layer.list_entries()?);
	}
	Ok(stats_from_entries(&entries))
}

fn stats_from_entries(entries: &[CacheLayerEntryInfo]) -> LayerStats {
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

fn format_layer_selection(layer: Option<FochCliCacheLayerArg>) -> String {
	match layer.unwrap_or(FochCliCacheLayerArg::All) {
		FochCliCacheLayerArg::All => default_foch_cache_dir().display().to_string(),
		other => selected_layers(Some(other))
			.into_iter()
			.next()
			.map(|layer| layer.layer().path().display().to_string())
			.unwrap_or_else(|| default_foch_cache_dir().display().to_string()),
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
