use crate::cli::arg::{FochCliCacheArgs, FochCliCacheCommands};
use crate::cli::handler::HandlerResult;
use foch_language::analyzer::semantic_index::parse_cache::{
	cache_cap_bytes, cache_clean, cache_stats, gc_with_cap,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn handle_cache(cache_args: &FochCliCacheArgs) -> HandlerResult {
	match &cache_args.command {
		FochCliCacheCommands::Stats => handle_cache_stats(),
		FochCliCacheCommands::Gc(args) => handle_cache_gc(args.cap_bytes),
		FochCliCacheCommands::Clean => handle_cache_clean(),
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
	let stats = cache_stats();
	let cap = cache_cap_bytes();
	println!("cache root: {}", stats.root.display());
	println!("files:      {}", stats.file_count);
	println!(
		"size:       {} / {} cap",
		format_bytes(stats.total_bytes),
		format_bytes(cap)
	);
	println!("oldest:     {}", format_optional_time(stats.oldest_mtime));
	println!("newest:     {}", format_optional_time(stats.newest_mtime));
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

fn handle_cache_clean() -> HandlerResult {
	let stats = cache_stats();
	cache_clean()?;
	println!(
		"removed: {} ({})",
		stats.root.display(),
		format_bytes(stats.total_bytes)
	);
	Ok(0)
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
