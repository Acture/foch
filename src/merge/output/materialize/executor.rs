//! Analyze merge units on a bounded pool of worker threads and apply their
//! results on the calling thread, in plan order.
//!
//! Applying is the only step that writes the output tree, the report or the
//! review, and it always runs on one thread in plan order. Workers only
//! analyze, so the number of workers changes when a unit is analyzed, never
//! what is written or in which order.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::merge::analyze::CancellationToken;
use crate::merge::error::MergeError;

/// Stack size for threads that parse, merge and emit Clausewitz scripts. The
/// recursive-descent parser and emitter need the same headroom as the CLI's
/// main thread for deeply nested files, and a stack overflow aborts the process
/// instead of failing one unit.
pub(crate) const MERGE_THREAD_STACK_SIZE: usize = 64 * 1024 * 1024;

/// Units each worker may have dispatched ahead of the unit being applied. A
/// finished result holds little more than its rendered output, so the bound is
/// loose enough for workers to keep going past a slow unit; at 16 per worker,
/// a real window with a 9 s unit took a third longer.
const PENDING_UNITS_PER_WORKER: usize = 256;
/// How often the coordinator wakes while waiting, to notice cancellation and
/// report long-running units.
const HEARTBEAT: Duration = Duration::from_secs(1);
/// Units running at least this long are reported while in flight and when
/// they finish.
const SLOW_UNIT: Duration = Duration::from_secs(10);

/// The units to run and how to schedule them. Everything here is read only by
/// the coordinating thread.
pub(super) struct UnitSchedule<'a> {
	pub(super) count: usize,
	pub(super) workers: NonZeroUsize,
	/// Estimated peak memory, in bytes, of analyzing one unit.
	pub(super) estimate: &'a dyn Fn(usize) -> u64,
	/// Estimated memory the units handed to workers may use at once. A unit
	/// is handed over only while its estimate fits beside those not yet
	/// finished; one larger than the whole budget runs alone.
	pub(super) memory_budget: u64,
	/// Units with nothing worth handing to a worker; the coordinator analyzes
	/// them itself when their turn to be applied comes.
	pub(super) runs_here: &'a dyn Fn(usize) -> bool,
	/// Names a unit in progress logs.
	pub(super) label: &'a dyn Fn(usize) -> String,
}

/// Analyze every unit and apply each result in index order.
///
/// `analyze` runs on worker threads and must neither prompt nor write.
/// `analyze_here` runs on the calling thread, for units that `runs_here`
/// selects and for every unit when there is one worker, which then keeps
/// today's serial order exactly. The first error or panic in index order ends
/// the run; results computed for later units are discarded unapplied, and no
/// worker outlives the call.
pub(super) fn run_units<T: Send>(
	schedule: &UnitSchedule<'_>,
	cancellation: &CancellationToken,
	analyze: &(dyn Fn(usize) -> T + Sync),
	analyze_here: &mut dyn FnMut(usize) -> T,
	apply: &mut dyn FnMut(usize, T) -> Result<(), MergeError>,
) -> Result<(), MergeError> {
	if schedule.workers.get() == 1 {
		for index in 0..schedule.count {
			cancellation.check()?;
			let analysis: T = analyze_here(index);
			apply(index, analysis)?;
		}
		return Ok(());
	}
	run_parallel(schedule, cancellation, analyze, analyze_here, apply)
}

enum WorkerEvent<T> {
	Started {
		index: usize,
		worker: usize,
		started: Instant,
	},
	Finished {
		index: usize,
		worker: usize,
		analysis: thread::Result<T>,
		elapsed: Duration,
	},
}

/// Tells workers to stop analyzing queued units once the coordinator leaves,
/// whether it returns or unwinds.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
	fn drop(&mut self) {
		self.0.store(true, Ordering::Release);
	}
}

fn worker_name(worker: usize) -> String {
	format!("foch-merge-{worker}")
}

fn run_parallel<T: Send>(
	schedule: &UnitSchedule<'_>,
	cancellation: &CancellationToken,
	analyze: &(dyn Fn(usize) -> T + Sync),
	analyze_here: &mut dyn FnMut(usize) -> T,
	apply: &mut dyn FnMut(usize, T) -> Result<(), MergeError>,
) -> Result<(), MergeError> {
	let stop: AtomicBool = AtomicBool::new(false);
	let (job_sender, job_receiver) = mpsc::channel::<usize>();
	let job_receiver: Mutex<Receiver<usize>> = Mutex::new(job_receiver);
	let (event_sender, events) = mpsc::channel::<WorkerEvent<T>>();
	thread::scope(|scope| {
		// Declared first so it is dropped last: workers see the stop flag
		// before the closed queue, and skip what is still queued.
		let job_sender: Sender<usize> = job_sender;
		let _stop_on_drop: StopOnDrop<'_> = StopOnDrop(&stop);
		for worker in 0..schedule.workers.get() {
			let events: Sender<WorkerEvent<T>> = event_sender.clone();
			let job_receiver: &Mutex<Receiver<usize>> = &job_receiver;
			let stop: &AtomicBool = &stop;
			thread::Builder::new()
				.name(worker_name(worker))
				.stack_size(MERGE_THREAD_STACK_SIZE)
				.spawn_scoped(scope, move || {
					run_worker(worker, analyze, job_receiver, events, stop, cancellation);
				})?;
		}
		drop(event_sender);
		coordinate(
			schedule,
			cancellation,
			&job_sender,
			events,
			analyze_here,
			apply,
		)
	})
}

fn run_worker<T: Send>(
	worker: usize,
	analyze: &(dyn Fn(usize) -> T + Sync),
	jobs: &Mutex<Receiver<usize>>,
	events: Sender<WorkerEvent<T>>,
	stop: &AtomicBool,
	cancellation: &CancellationToken,
) {
	loop {
		let job = jobs.lock().unwrap_or_else(PoisonError::into_inner).recv();
		let Ok(index) = job else {
			return;
		};
		// The coordinator is leaving, or fails at its next cancellation check
		// without applying anything further.
		if stop.load(Ordering::Acquire) || cancellation.is_cancelled() {
			continue;
		}
		let started: Instant = Instant::now();
		if events
			.send(WorkerEvent::Started {
				index,
				worker,
				started,
			})
			.is_err()
		{
			return;
		}
		let analysis: thread::Result<T> = catch_unwind(AssertUnwindSafe(|| analyze(index)));
		let finished = WorkerEvent::Finished {
			index,
			worker,
			analysis,
			elapsed: started.elapsed(),
		};
		if events.send(finished).is_err() {
			return;
		}
	}
}

fn coordinate<T: Send>(
	schedule: &UnitSchedule<'_>,
	cancellation: &CancellationToken,
	jobs: &Sender<usize>,
	events: Receiver<WorkerEvent<T>>,
	analyze_here: &mut dyn FnMut(usize) -> T,
	apply: &mut dyn FnMut(usize, T) -> Result<(), MergeError>,
) -> Result<(), MergeError> {
	let window: usize = schedule.workers.get() * PENDING_UNITS_PER_WORKER;
	let mut next_dispatch: usize = 0;
	let mut next_apply: usize = 0;
	// Units handed to workers and not yet applied.
	let mut outstanding: usize = 0;
	// Estimates of units handed to workers and not yet finished.
	let mut unfinished_estimates: BTreeMap<usize, u64> = BTreeMap::new();
	let mut unfinished_bytes: u64 = 0;
	// The estimate of the unit waiting for memory, so it is taken only once.
	let mut waiting_estimate: Option<(usize, u64)> = None;
	let mut finished: BTreeMap<usize, thread::Result<T>> = BTreeMap::new();
	let mut in_flight: BTreeMap<usize, (usize, Instant)> = BTreeMap::new();
	let mut last_slow_report: Instant = Instant::now();
	while next_apply < schedule.count {
		// Checked before every unit is applied, as the serial loop does.
		cancellation.check()?;
		while next_dispatch < schedule.count && outstanding < window {
			if !(schedule.runs_here)(next_dispatch) {
				let estimate: u64 = match waiting_estimate {
					Some((index, estimate)) if index == next_dispatch => estimate,
					_ => (schedule.estimate)(next_dispatch),
				};
				if unfinished_bytes > 0
					&& unfinished_bytes.saturating_add(estimate) > schedule.memory_budget
				{
					waiting_estimate = Some((next_dispatch, estimate));
					break;
				}
				jobs.send(next_dispatch).map_err(|_| workers_exited())?;
				unfinished_estimates.insert(next_dispatch, estimate);
				unfinished_bytes = unfinished_bytes.saturating_add(estimate);
				outstanding += 1;
			}
			next_dispatch += 1;
		}
		if (schedule.runs_here)(next_apply) {
			let analysis: T = analyze_here(next_apply);
			apply(next_apply, analysis)?;
			next_apply += 1;
			continue;
		}
		if let Some(result) = finished.remove(&next_apply) {
			outstanding -= 1;
			let analysis: T = match result {
				Ok(analysis) => analysis,
				Err(payload) => resume_unwind(payload),
			};
			apply(next_apply, analysis)?;
			next_apply += 1;
			continue;
		}
		match events.recv_timeout(HEARTBEAT) {
			Ok(WorkerEvent::Started {
				index,
				worker,
				started,
			}) => {
				in_flight.insert(index, (worker, started));
			}
			Ok(WorkerEvent::Finished {
				index,
				worker,
				analysis,
				elapsed,
			}) => {
				in_flight.remove(&index);
				let estimate: u64 = unfinished_estimates.remove(&index).unwrap_or_default();
				unfinished_bytes = unfinished_bytes.saturating_sub(estimate);
				if elapsed >= SLOW_UNIT {
					eprintln!(
						"[merge] materialize: unit done {} on {} elapsed_ms={}",
						(schedule.label)(index),
						worker_name(worker),
						elapsed.as_millis()
					);
				}
				finished.insert(index, analysis);
			}
			Err(RecvTimeoutError::Timeout) => {}
			Err(RecvTimeoutError::Disconnected) => return Err(workers_exited()),
		}
		if last_slow_report.elapsed() >= SLOW_UNIT
			&& report_slow_units(schedule, &in_flight, next_apply, finished.len())
		{
			last_slow_report = Instant::now();
		}
	}
	Ok(())
}

/// Print the units that have run for a long time, oldest first. Returns
/// whether anything was reported.
fn report_slow_units(
	schedule: &UnitSchedule<'_>,
	in_flight: &BTreeMap<usize, (usize, Instant)>,
	applied: usize,
	analyzed_ahead: usize,
) -> bool {
	let mut slow: Vec<(usize, usize, Duration)> = in_flight
		.iter()
		.map(|(&index, &(worker, started))| (index, worker, started.elapsed()))
		.filter(|&(_, _, running)| running >= SLOW_UNIT)
		.collect();
	if slow.is_empty() {
		return false;
	}
	slow.sort_by_key(|&(_, _, running)| std::cmp::Reverse(running));
	let detail: Vec<String> = slow
		.iter()
		.take(4)
		.map(|&(index, worker, running)| {
			format!(
				"{} on {} for {:.1}s",
				(schedule.label)(index),
				worker_name(worker),
				running.as_secs_f64()
			)
		})
		.collect();
	eprintln!(
		"[merge] materialize: in flight={} applied={applied}/{} analyzed_ahead={analyzed_ahead} long-running: {}",
		in_flight.len(),
		schedule.count,
		detail.join("; ")
	);
	true
}

fn workers_exited() -> MergeError {
	MergeError::Validation {
		path: None,
		message: "internal error: merge workers exited before every unit was analyzed".to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::{PENDING_UNITS_PER_WORKER, UnitSchedule, run_units};
	use crate::merge::analyze::CancellationToken;
	use crate::merge::error::MergeError;
	use std::num::NonZeroUsize;
	use std::panic::{AssertUnwindSafe, catch_unwind};
	use std::sync::{Condvar, Mutex, MutexGuard};
	use std::thread::{self, ThreadId};
	use std::time::Duration;

	/// Deadlock guard for gates. A correct scheduler opens every gate long
	/// before this; an incorrect one fails the test instead of hanging it.
	const GATE_TIMEOUT: Duration = Duration::from_secs(30);
	/// How long a test keeps units busy so that a worker beyond the limit, or
	/// one ignoring cancellation, would start another unit. A correct
	/// scheduler passes however short this is.
	const EXTRA_WORK_WINDOW: Duration = Duration::from_millis(100);

	#[derive(Default)]
	struct Progress {
		started: usize,
		active: usize,
		max_active: usize,
		applied: usize,
		max_ahead: usize,
		finished: Vec<usize>,
		threads: Vec<Option<String>>,
		timed_out: bool,
	}

	/// Counters shared by the units of one run, plus a condition variable that
	/// lets a unit wait for the others.
	#[derive(Default)]
	struct Gate {
		progress: Mutex<Progress>,
		changed: Condvar,
	}

	impl Gate {
		fn update(&self, change: impl FnOnce(&mut Progress)) {
			change(&mut self.lock());
			self.changed.notify_all();
		}

		/// Waits until `open` holds. After one wait outlasts the guard, every
		/// later wait returns at once so a broken scheduler fails quickly.
		fn wait_until(&self, open: impl Fn(&Progress) -> bool) {
			if self.lock().timed_out {
				return;
			}
			let (mut progress, result) = self
				.changed
				.wait_timeout_while(self.lock(), GATE_TIMEOUT, |progress| !open(progress))
				.unwrap();
			if result.timed_out() {
				progress.timed_out = true;
			}
		}

		fn lock(&self) -> MutexGuard<'_, Progress> {
			self.progress.lock().unwrap()
		}
	}

	fn workers(count: usize) -> NonZeroUsize {
		NonZeroUsize::new(count).unwrap()
	}

	fn on_workers(_: usize) -> bool {
		false
	}

	fn label(index: usize) -> String {
		format!("unit {index}")
	}

	fn no_estimate(_: usize) -> u64 {
		0
	}

	fn schedule<'a>(count: usize, worker_count: usize) -> UnitSchedule<'a> {
		UnitSchedule {
			count,
			workers: workers(worker_count),
			estimate: &no_estimate,
			memory_budget: u64::MAX,
			runs_here: &on_workers,
			label: &label,
		}
	}

	fn not_here(index: usize) -> usize {
		panic!("unit {index} must be analyzed on a worker")
	}

	/// Cancels `cancellation` if `run` outlives twice the gate timeout, so a
	/// scheduler that stalls fails the test with `Cancelled` instead of hanging.
	fn with_watchdog<R>(cancellation: &CancellationToken, run: impl FnOnce() -> R) -> R {
		let (done, finished) = std::sync::mpsc::channel::<()>();
		thread::scope(|scope| {
			scope.spawn(move || {
				if matches!(
					finished.recv_timeout(GATE_TIMEOUT * 2),
					Err(std::sync::mpsc::RecvTimeoutError::Timeout)
				) {
					cancellation.cancel();
				}
			});
			let result: R = run();
			drop(done);
			result
		})
	}

	fn run_guarded<T: Send>(
		schedule: &UnitSchedule<'_>,
		analyze: &(dyn Fn(usize) -> T + Sync),
		analyze_here: &mut dyn FnMut(usize) -> T,
		apply: &mut dyn FnMut(usize, T) -> Result<(), MergeError>,
	) -> Result<(), MergeError> {
		let cancellation: CancellationToken = CancellationToken::new();
		with_watchdog(&cancellation, || {
			run_units(schedule, &cancellation, analyze, analyze_here, apply)
		})
	}

	#[test]
	fn results_apply_in_index_order_whatever_order_they_finish() {
		let gate: Gate = Gate::default();
		// Each unit waits for the next one, so they finish in reverse order.
		let analyze = |index: usize| -> usize {
			if index + 1 < 4 {
				gate.wait_until(|progress| progress.finished.contains(&(index + 1)));
			}
			gate.update(|progress| progress.finished.push(index));
			index * 10
		};
		let mut applied: Vec<(usize, usize)> = Vec::new();
		run_guarded(
			&schedule(4, 4),
			&analyze,
			&mut not_here,
			&mut |index, value| {
				applied.push((index, value));
				Ok(())
			},
		)
		.unwrap();
		let progress = gate.lock();
		assert!(!progress.timed_out);
		assert_eq!(progress.finished, vec![3, 2, 1, 0]);
		assert_eq!(applied, vec![(0, 0), (1, 10), (2, 20), (3, 30)]);
	}

	#[test]
	fn units_overlap_up_to_the_worker_count_on_named_worker_threads() {
		let gate: Gate = Gate::default();
		let count: usize = 12;
		let analyze = |index: usize| -> usize {
			gate.update(|progress| {
				progress.started += 1;
				progress.active += 1;
				progress.max_active = progress.max_active.max(progress.active);
				progress
					.threads
					.push(thread::current().name().map(str::to_string));
			});
			// Held until three units are running at once, which only happens
			// if they overlap. The unit that makes three then keeps them busy
			// long enough for a fourth worker, if there were one, to start.
			if gate.lock().active == 3 {
				thread::sleep(EXTRA_WORK_WINDOW);
			}
			gate.wait_until(|progress| progress.max_active >= 3 || progress.started == count);
			gate.update(|progress| progress.active -= 1);
			index
		};
		let mut applied: Vec<usize> = Vec::new();
		run_guarded(
			&schedule(count, 3),
			&analyze,
			&mut not_here,
			&mut |index, _| {
				applied.push(index);
				Ok(())
			},
		)
		.unwrap();
		let progress = gate.lock();
		assert!(!progress.timed_out);
		assert_eq!(progress.max_active, 3);
		assert_eq!(applied, (0..count).collect::<Vec<usize>>());
		assert!(
			progress.threads.iter().all(|name| name
				.as_deref()
				.is_some_and(|name| name.starts_with("foch-merge-"))),
			"{:?}",
			progress.threads
		);
	}

	#[test]
	fn units_analyzed_ahead_of_the_applied_one_are_bounded() {
		let gate: Gate = Gate::default();
		let worker_count: usize = 2;
		let window: usize = worker_count * PENDING_UNITS_PER_WORKER;
		let count: usize = window * 3;
		let started_when_released: Mutex<Option<usize>> = Mutex::new(None);
		let analyze = |index: usize| -> usize {
			gate.update(|progress| {
				progress.started += 1;
				progress.max_ahead = progress.max_ahead.max(progress.started - progress.applied);
			});
			if index == 0 {
				// Unit 0 holds up applying until the window is full.
				gate.wait_until(|progress| progress.started >= window);
				*started_when_released.lock().unwrap() = Some(gate.lock().started);
			}
			index
		};
		run_guarded(
			&schedule(count, worker_count),
			&analyze,
			&mut not_here,
			&mut |_, _| {
				gate.update(|progress| progress.applied += 1);
				Ok(())
			},
		)
		.unwrap();
		let progress = gate.lock();
		assert!(!progress.timed_out);
		assert_eq!(*started_when_released.lock().unwrap(), Some(window));
		assert_eq!(progress.max_ahead, window);
		assert_eq!(progress.applied, count);
	}

	#[test]
	fn the_first_error_in_index_order_ends_the_run() {
		let gate: Gate = Gate::default();
		let mut applied: Vec<usize> = Vec::new();
		// With two workers, one holds unit zero while the other runs units
		// one to nine in turn. Unit nine panics and its worker only then
		// starts unit ten, so the panic reaches the coordinator before unit
		// zero is released. The earlier error at unit five still decides.
		let analyze = |index: usize| -> usize {
			match index {
				0 => gate.wait_until(|progress| progress.finished.contains(&10)),
				9 => panic!("unit nine"),
				10 => gate.update(|progress| progress.finished.push(10)),
				_ => {}
			}
			index
		};
		let error: MergeError = run_guarded(
			&schedule(20, 2),
			&analyze,
			&mut not_here,
			&mut |index, _| {
				applied.push(index);
				if index == 5 {
					return Err(MergeError::Validation {
						path: Some("five".to_string()),
						message: "unit five failed".to_string(),
					});
				}
				Ok(())
			},
		)
		.unwrap_err();
		assert!(!gate.lock().timed_out);
		assert!(error.to_string().contains("unit five failed"), "{error}");
		assert_eq!(applied, vec![0, 1, 2, 3, 4, 5]);
	}

	#[test]
	fn a_panic_in_analysis_resumes_when_its_unit_is_applied() {
		let mut applied: Vec<usize> = Vec::new();
		let analyze = |index: usize| -> usize {
			if index == 3 {
				panic!("unit three");
			}
			index
		};
		let payload = catch_unwind(AssertUnwindSafe(|| {
			run_guarded(
				&schedule(10, 4),
				&analyze,
				&mut not_here,
				&mut |index, _| {
					applied.push(index);
					Ok(())
				},
			)
		}))
		.unwrap_err();
		assert_eq!(payload.downcast_ref::<&str>(), Some(&"unit three"));
		assert_eq!(applied, vec![0, 1, 2]);
	}

	#[test]
	fn cancellation_stops_before_the_next_unit_is_applied() {
		let cancellation: CancellationToken = CancellationToken::new();
		let mut applied: Vec<usize> = Vec::new();
		let error: MergeError = with_watchdog(&cancellation, || {
			run_units(
				&schedule(50, 3),
				&cancellation,
				&|index: usize| index,
				&mut not_here,
				&mut |index, _| {
					applied.push(index);
					if index == 4 {
						cancellation.cancel();
					}
					Ok(())
				},
			)
		})
		.unwrap_err();
		assert!(matches!(error, MergeError::Cancelled), "{error}");
		assert_eq!(applied, vec![0, 1, 2, 3, 4]);
	}

	#[test]
	fn one_worker_analyzes_and_applies_each_unit_in_turn_on_the_calling_thread() {
		let caller: ThreadId = thread::current().id();
		// Each unit is applied before the next is analyzed, as the serial loop
		// always did; interactive prompts rely on it.
		let steps: Mutex<Vec<(&str, usize)>> = Mutex::new(Vec::new());
		run_guarded(
			&schedule(4, 1),
			&|index: usize| -> usize { panic!("unit {index} reached a worker") },
			&mut |index| {
				assert_eq!(thread::current().id(), caller);
				steps.lock().unwrap().push(("analyze", index));
				index
			},
			&mut |index, _| {
				steps.lock().unwrap().push(("apply", index));
				Ok(())
			},
		)
		.unwrap();
		let expected: Vec<(&str, usize)> = (0..4)
			.flat_map(|index| [("analyze", index), ("apply", index)])
			.collect();
		assert_eq!(*steps.lock().unwrap(), expected);
	}

	#[test]
	fn queued_units_are_skipped_once_cancelled() {
		let cancellation: CancellationToken = CancellationToken::new();
		let started_after_cancel: Mutex<usize> = Mutex::new(0);
		let runs_here = |index: usize| index == 0;
		// Unit zero runs on the coordinator after the queue is filled. It
		// cancels, then keeps the coordinator from leaving for a while, which
		// is when idle workers would start queued units if they ignored it.
		let error: MergeError = with_watchdog(&cancellation, || {
			run_units(
				&UnitSchedule {
					runs_here: &runs_here,
					..schedule(40, 2)
				},
				&cancellation,
				&|index: usize| -> usize {
					if cancellation.is_cancelled() {
						*started_after_cancel.lock().unwrap() += 1;
					}
					index
				},
				&mut |index| {
					cancellation.cancel();
					thread::sleep(EXTRA_WORK_WINDOW);
					index
				},
				&mut |_, _| Ok(()),
			)
		})
		.unwrap_err();
		assert!(matches!(error, MergeError::Cancelled), "{error}");
		// A worker that checked just before the token changed may still start
		// its unit; none may start another.
		assert!(*started_after_cancel.lock().unwrap() <= 2);
	}

	#[test]
	fn units_that_run_here_never_reach_a_worker() {
		let caller: ThreadId = thread::current().id();
		let runs_here = |index: usize| index.is_multiple_of(3);
		let analyze = |index: usize| -> usize {
			assert!(!runs_here(index), "unit {index} reached a worker");
			assert_ne!(thread::current().id(), caller);
			index
		};
		let mut applied: Vec<usize> = Vec::new();
		run_guarded(
			&UnitSchedule {
				runs_here: &runs_here,
				..schedule(12, 3)
			},
			&analyze,
			&mut |index| {
				assert!(runs_here(index), "unit {index} was analyzed here");
				assert_eq!(thread::current().id(), caller);
				index
			},
			&mut |index, _| {
				applied.push(index);
				Ok(())
			},
		)
		.unwrap();
		assert_eq!(applied, (0..12).collect::<Vec<usize>>());
	}

	/// Records a unit's start and whether any other unit is running then.
	fn enter(gate: &Gate, index: usize) -> (usize, usize) {
		let mut entered: (usize, usize) = (0, 0);
		gate.update(|progress| {
			entered = (progress.started, progress.active);
			progress.started += 1;
			progress.active += 1;
			progress.max_active = progress.max_active.max(progress.active);
			progress.threads.push(Some(format!("enter {index}")));
		});
		entered
	}

	fn exit(gate: &Gate, index: usize) {
		gate.update(|progress| {
			progress.active -= 1;
			progress.finished.push(index);
		});
	}

	#[test]
	fn units_start_only_while_their_estimates_fit_the_memory_budget() {
		let gate: Gate = Gate::default();
		let count: usize = 6;
		let analyze = |index: usize| -> usize {
			let (_, running) = enter(&gate, index);
			if running == 1 {
				// Two fit the budget; give a third, if one were let through,
				// the time to start before either finishes.
				thread::sleep(EXTRA_WORK_WINDOW);
			} else {
				gate.wait_until(|progress| progress.max_active >= 2 || progress.started == count);
			}
			exit(&gate, index);
			index
		};
		run_guarded(
			&UnitSchedule {
				estimate: &|_| 10,
				memory_budget: 25,
				..schedule(count, 3)
			},
			&analyze,
			&mut not_here,
			&mut |_, _| Ok(()),
		)
		.unwrap();
		let progress = gate.lock();
		assert!(!progress.timed_out);
		assert_eq!(progress.max_active, 2);
		assert_eq!(progress.finished.len(), count);
	}

	#[test]
	fn a_unit_waits_until_its_estimate_fits_beside_the_running_ones() {
		let gate: Gate = Gate::default();
		let second_saw_first_finished: Mutex<Option<bool>> = Mutex::new(None);
		let analyze = |index: usize| -> usize {
			enter(&gate, index);
			if index == 0 {
				// Unit one would fit a worker at once, but not the budget.
				thread::sleep(EXTRA_WORK_WINDOW);
			} else {
				*second_saw_first_finished.lock().unwrap() =
					Some(gate.lock().finished.contains(&0));
			}
			exit(&gate, index);
			index
		};
		let estimates: [u64; 2] = [10, 20];
		run_guarded(
			&UnitSchedule {
				estimate: &|index| estimates[index],
				memory_budget: 25,
				..schedule(2, 2)
			},
			&analyze,
			&mut not_here,
			&mut |_, _| Ok(()),
		)
		.unwrap();
		assert_eq!(*second_saw_first_finished.lock().unwrap(), Some(true));
		assert_eq!(gate.lock().max_active, 1);
	}

	#[test]
	fn a_unit_larger_than_the_budget_runs_alone() {
		let gate: Gate = Gate::default();
		let large_unit: Mutex<Option<(usize, usize, usize)>> = Mutex::new(None);
		let analyze = |index: usize| -> usize {
			let (started_before, running) = enter(&gate, index);
			if index == 2 {
				// While it runs, nothing else may start.
				thread::sleep(EXTRA_WORK_WINDOW);
				let started_after: usize = gate.lock().started;
				*large_unit.lock().unwrap() = Some((started_before, running, started_after));
			}
			exit(&gate, index);
			index
		};
		let estimates: [u64; 5] = [1, 1, 100, 1, 1];
		let mut applied: Vec<usize> = Vec::new();
		run_guarded(
			&UnitSchedule {
				estimate: &|index| estimates[index],
				memory_budget: 25,
				..schedule(5, 3)
			},
			&analyze,
			&mut not_here,
			&mut |index, _| {
				applied.push(index);
				Ok(())
			},
		)
		.unwrap();
		let (started_before, running, started_after) =
			large_unit.lock().unwrap().expect("the large unit ran");
		assert_eq!(running, 0, "the large unit started beside another");
		assert_eq!(started_before, 2);
		assert_eq!(started_after, 3, "a unit started beside the large one");
		assert_eq!(applied, vec![0, 1, 2, 3, 4]);
	}

	/// Uses at least 4 MiB of stack, more in unoptimized builds: more than a
	/// default 2 MiB thread gets, well within what merge workers are given.
	fn consume_stack(depth: usize) -> usize {
		let frame: [u8; 4096] = std::hint::black_box([depth as u8; 4096]);
		if depth == 0 {
			return usize::from(frame[0]);
		}
		consume_stack(depth - 1) + usize::from(std::hint::black_box(frame[depth % 4096]))
	}

	#[test]
	fn workers_have_room_for_deeply_nested_scripts() {
		let mut applied: Vec<usize> = Vec::new();
		run_guarded(
			&schedule(4, 2),
			&|_: usize| consume_stack(1024),
			&mut not_here,
			&mut |_, depth_sum| {
				applied.push(depth_sum);
				Ok(())
			},
		)
		.unwrap();
		assert_eq!(applied.len(), 4);
	}
}
