//! Where a merge spends its time: wall-clock stages, logged as they run, and
//! the CPU time the tree kernels accumulate across workers.
//!
//! A recording collects both for one run so it can be appended to a timing
//! history kept apart from the merged mod, whose bytes stay deterministic.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Run `f` as a named stage, logging its start and its wall-clock time.
/// The closure may return a summary appended to the done line.
pub fn stage_log_with<F, T>(name: &str, f: F) -> T
where
	F: FnOnce() -> (T, Option<String>),
{
	eprintln!("[merge] {name}: start");
	let started = Instant::now();
	let (value, summary) = f();
	let elapsed_ms = started.elapsed().as_millis();
	match summary {
		Some(s) => eprintln!("[merge] {name}: done elapsed_ms={elapsed_ms} {s}"),
		None => eprintln!("[merge] {name}: done elapsed_ms={elapsed_ms}"),
	}
	record_stage(name, started);
	value
}

/// `stage_log_with` for a stage without a summary.
pub fn stage<T>(name: &str, f: impl FnOnce() -> T) -> T {
	stage_log_with(name, || (f(), None))
}

/// One finished stage of a recording. Stages nest: a stage's interval
/// contains those of the stages it ran.
#[derive(Clone, Debug, Serialize)]
pub struct StageTiming {
	pub name: String,
	/// Offset from the start of the recording.
	pub start_ms: u64,
	pub elapsed_ms: u64,
}

struct ActiveRecording {
	started: Instant,
	stages: Vec<StageTiming>,
}

static RECORDING: Mutex<Option<ActiveRecording>> = Mutex::new(None);

/// Add a stage that started at `started` and ends now to the active recording.
pub(crate) fn record_stage(name: &str, started: Instant) {
	let elapsed = started.elapsed();
	let mut recording = RECORDING.lock().unwrap_or_else(|error| error.into_inner());
	if let Some(recording) = recording.as_mut() {
		recording.stages.push(StageTiming {
			name: name.to_owned(),
			start_ms: millis(started.saturating_duration_since(recording.started)),
			elapsed_ms: millis(elapsed),
		});
	}
}

/// Collects the stages and kernel time of one run. Stages from anywhere in
/// the process are recorded while it is active, so concurrent runs in one
/// process share it.
pub struct TimingRecording {
	started: Instant,
	started_at: SystemTime,
	kernels: KernelTimes,
}

impl TimingRecording {
	pub fn begin() -> Self {
		let started = Instant::now();
		*RECORDING.lock().unwrap_or_else(|error| error.into_inner()) = Some(ActiveRecording {
			started,
			stages: Vec::new(),
		});
		Self {
			started,
			started_at: SystemTime::now(),
			kernels: KernelTimes::now(),
		}
	}

	pub fn finish(self) -> RunTiming {
		let stages = RECORDING
			.lock()
			.unwrap_or_else(|error| error.into_inner())
			.take()
			.map(|recording| recording.stages)
			.unwrap_or_default();
		RunTiming {
			started_at_unix_ms: self.started_at.duration_since(UNIX_EPOCH).map_or(0, millis),
			total_ms: millis(self.started.elapsed()),
			stages,
			kernel_cpu_ms: KernelTimes::now().cpu_ms_since(&self.kernels),
		}
	}
}

/// The timing of one finished run.
#[derive(Clone, Debug, Serialize)]
pub struct RunTiming {
	pub started_at_unix_ms: u64,
	pub total_ms: u64,
	pub stages: Vec<StageTiming>,
	/// CPU time per kernel step, summed across workers.
	pub kernel_cpu_ms: BTreeMap<&'static str, u64>,
}

/// One line of the merge timing history.
#[derive(Debug, Serialize)]
pub struct MergeTimingEntry<'a> {
	pub foch_version: &'static str,
	pub analysis_rules_version: &'static str,
	pub input: &'a str,
	pub out_dir: &'a str,
	pub status: &'a str,
	#[serde(flatten)]
	pub timing: &'a RunTiming,
}

/// The append-only merge timing history under the foch cache directory.
pub fn merge_timing_history_path() -> PathBuf {
	crate::platform::cache_store::default_foch_cache_dir()
		.join("timings")
		.join("merge-timings.jsonl")
}

/// Append `entry` as one JSON line to the history at `path`.
pub fn append_merge_timing(path: &Path, entry: &MergeTimingEntry<'_>) -> io::Result<()> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
	line.push(b'\n');
	// One write per line keeps concurrent appenders from interleaving.
	fs::OpenOptions::new()
		.create(true)
		.append(true)
		.open(path)?
		.write_all(&line)
}

fn millis(duration: std::time::Duration) -> u64 {
	u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// A step of the tree kernels whose CPU time is summed across workers.
#[derive(Clone, Copy, Debug)]
pub(crate) enum KernelStep {
	ObserveNormalize,
	ObserveMatch,
	ObserveDelta,
	ObserveLineage,
	JoinCanonicalize,
	JoinNormalize,
	JoinMerge,
	JoinRender,
	JoinFinish,
	JoinLineage,
	JoinResolve,
}

impl KernelStep {
	const ALL: [Self; 11] = [
		Self::ObserveNormalize,
		Self::ObserveMatch,
		Self::ObserveDelta,
		Self::ObserveLineage,
		Self::JoinCanonicalize,
		Self::JoinNormalize,
		Self::JoinMerge,
		Self::JoinRender,
		Self::JoinFinish,
		Self::JoinLineage,
		Self::JoinResolve,
	];

	const fn name(self) -> &'static str {
		match self {
			Self::ObserveNormalize => "observe.normalize",
			Self::ObserveMatch => "observe.match",
			Self::ObserveDelta => "observe.delta",
			Self::ObserveLineage => "observe.lineage",
			Self::JoinCanonicalize => "join.canonicalize",
			Self::JoinNormalize => "join.normalize",
			Self::JoinMerge => "join.merge",
			Self::JoinRender => "join.render",
			Self::JoinFinish => "join.finish",
			Self::JoinLineage => "join.lineage",
			Self::JoinResolve => "join.resolve",
		}
	}
}

static KERNEL_MICROS: [AtomicU64; KernelStep::ALL.len()] =
	[const { AtomicU64::new(0) }; KernelStep::ALL.len()];

/// Charge the time since `started` to `step`.
pub(crate) fn charge(step: KernelStep, started: Instant) {
	KERNEL_MICROS[step as usize].fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
}

/// Time `f` as `step`.
pub(crate) fn kernel<T>(step: KernelStep, f: impl FnOnce() -> T) -> T {
	let started = Instant::now();
	let value = f();
	charge(step, started);
	value
}

/// Kernel CPU time accumulated so far in this process. Merges running
/// concurrently in one process share the counters.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct KernelTimes([u64; KernelStep::ALL.len()]);

impl KernelTimes {
	pub(crate) fn now() -> Self {
		Self(std::array::from_fn(|index| {
			KERNEL_MICROS[index].load(Ordering::Relaxed)
		}))
	}

	fn cpu_ms_since(&self, earlier: &Self) -> BTreeMap<&'static str, u64> {
		KernelStep::ALL
			.iter()
			.map(|step| {
				let index = *step as usize;
				(
					step.name(),
					self.0[index].saturating_sub(earlier.0[index]) / 1000,
				)
			})
			.collect()
	}

	/// The time accumulated since `earlier`, as `name_cpu_ms=ms` pairs.
	pub(crate) fn summary_since(&self, earlier: &Self) -> String {
		self.cpu_ms_since(earlier)
			.into_iter()
			.map(|(name, ms)| format!("{name}_cpu_ms={ms}"))
			.collect::<Vec<_>>()
			.join(" ")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_recording_keeps_its_stages_and_appends_one_line_per_run() {
		let recording = TimingRecording::begin();
		stage("outer", || stage("inner", || ()));
		let timing = recording.finish();
		let find = |name: &str| {
			timing
				.stages
				.iter()
				.find(|stage| stage.name == name)
				.unwrap_or_else(|| panic!("{name} was not recorded: {:?}", timing.stages))
		};
		let (outer, inner) = (find("outer"), find("inner"));
		assert!(outer.start_ms <= inner.start_ms && inner.elapsed_ms <= outer.elapsed_ms);
		let dir = tempfile::tempdir().expect("temp dir");
		let path = dir.path().join("timings").join("merge-timings.jsonl");
		for _ in 0..2 {
			append_merge_timing(
				&path,
				&MergeTimingEntry {
					foch_version: "test",
					analysis_rules_version: "test",
					input: "in",
					out_dir: "out",
					status: "ready",
					timing: &timing,
				},
			)
			.expect("append timing");
		}
		let lines = fs::read_to_string(&path).expect("read history");
		assert_eq!(lines.lines().count(), 2);
		let entry: serde_json::Value =
			serde_json::from_str(lines.lines().next().unwrap()).expect("one JSON object per line");
		assert!(entry["stages"].is_array());
		assert!(entry["kernel_cpu_ms"]["join.merge"].is_u64());
	}
}
