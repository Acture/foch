use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::event::KeyEvent;
use foch::input::{
	CurrentEu4Input, InputPreparationMode, InputReadiness, InputRequest, inspect_current_eu4_input,
};
use foch::merge::{
	CancellationToken, MergeAnalysisOptions, MergeError, MergeProgress, ProgressObserver,
	analyze_merge,
};

use super::app::{AnalysisSettings, AnalysisView, App, AppCommand, Phase};

/// One read-only look at the current input: what to show, and the frozen
/// request an analysis of exactly that input would consume.
pub struct Inspection {
	pub input: CurrentEu4Input,
	pub request: Option<InputRequest>,
	/// Whether this source can supply the analyzed EU4 base as the merge
	/// ancestor.
	pub game_base_available: bool,
}

/// Where the browser reads its input from. The product source inspects the
/// installed EU4 game and its launcher playset; tests supply fixtures.
pub trait BrowserSource: Send + Sync {
	fn inspect(&self) -> Inspection;
}

/// The installed EU4 game and its current launcher playset, inspected without
/// initializing configuration.
#[derive(Clone, Copy, Debug, Default)]
pub struct CurrentEu4Source;

impl BrowserSource for CurrentEu4Source {
	fn inspect(&self) -> Inspection {
		let input = inspect_current_eu4_input();
		let mode = match input.readiness {
			InputReadiness::Ready => Some(InputPreparationMode::Complete),
			InputReadiness::ReadyWithOmissions => Some(InputPreparationMode::AvailableOnly),
			InputReadiness::Blocked => None,
		};
		let request = mode
			.and_then(|mode| input.clone().prepare(mode))
			.map(|prepared| prepared.request);
		Inspection {
			input,
			request,
			game_base_available: true,
		}
	}
}

enum WorkerMessage {
	Inspected {
		generation: u64,
		inspection: Box<Inspection>,
	},
	Progress {
		generation: u64,
		progress: MergeProgress,
	},
	Analyzed {
		generation: u64,
		result: Result<Box<AnalysisView>, String>,
	},
}

struct ChannelProgress {
	generation: u64,
	tx: Sender<WorkerMessage>,
}

impl ProgressObserver for ChannelProgress {
	fn update(&self, progress: MergeProgress) {
		let _ = self.tx.send(WorkerMessage::Progress {
			generation: self.generation,
			progress,
		});
	}
}

/// The browser state plus the background work feeding it. Every inspection
/// and analysis carries a generation, so results from work a refresh replaced
/// are dropped instead of mixed into the new snapshot.
pub struct Session {
	app: App,
	source: Arc<dyn BrowserSource>,
	tx: Sender<WorkerMessage>,
	rx: Receiver<WorkerMessage>,
	generation: u64,
	pending: Option<InputRequest>,
	cancellation: Option<CancellationToken>,
	busy: bool,
}

impl Session {
	pub fn new(source: Arc<dyn BrowserSource>) -> Self {
		let (tx, rx) = mpsc::channel();
		let mut session = Self {
			app: App::default(),
			source,
			tx,
			rx,
			generation: 0,
			pending: None,
			cancellation: None,
			busy: false,
		};
		session.start_inspection(false);
		session
	}

	pub fn app(&self) -> &App {
		&self.app
	}

	/// Whether an inspection or analysis is still running.
	pub fn is_busy(&self) -> bool {
		self.busy
	}

	/// Apply one key press. Returns `false` once the user asked to quit.
	pub fn handle_key(&mut self, key: KeyEvent) -> bool {
		match self.app.handle_key(key) {
			Some(AppCommand::Quit) => {
				self.cancel_running();
				return false;
			}
			Some(AppCommand::Analyze) => self.start_analysis(),
			Some(AppCommand::Refresh) => {
				// Refreshing a browsed analysis replaces both the input
				// snapshot and the analysis; before one, it only re-inspects.
				let analyzed = matches!(self.app.phase, Phase::Reviewed(_) | Phase::Failed(_));
				self.start_inspection(analyzed);
			}
			None => {}
		}
		true
	}

	/// Apply every finished worker message, waiting up to `timeout` for the
	/// first one. Returns whether anything changed.
	pub fn pump(&mut self, timeout: Duration) -> bool {
		let mut changed = false;
		let mut next = self.rx.recv_timeout(timeout).ok();
		while let Some(message) = next {
			changed |= self.apply(message);
			next = self.rx.try_recv().ok();
		}
		changed
	}

	/// Block until the current inspection or analysis finishes.
	pub fn wait_idle(&mut self) {
		while self.busy {
			self.pump(Duration::from_millis(50));
		}
	}

	fn apply(&mut self, message: WorkerMessage) -> bool {
		match message {
			WorkerMessage::Inspected {
				generation,
				inspection,
			} if generation == self.generation => {
				let Inspection {
					input,
					request,
					game_base_available,
				} = *inspection;
				self.pending = request;
				self.busy = false;
				self.app
					.inspected(input, self.pending.is_some(), game_base_available);
				if self.app.take_analyze_after_inspection() {
					self.start_analysis();
				}
				true
			}
			WorkerMessage::Progress {
				generation,
				progress,
			} if generation == self.generation => {
				self.app.progress(progress);
				true
			}
			WorkerMessage::Analyzed { generation, result } if generation == self.generation => {
				self.busy = false;
				self.cancellation = None;
				self.app.analyzed(result.map(|view| *view));
				true
			}
			_ => false,
		}
	}

	fn start_inspection(&mut self, analyze_after: bool) {
		self.cancel_running();
		self.generation += 1;
		self.pending = None;
		self.busy = true;
		self.app.inspecting(analyze_after);
		let generation = self.generation;
		let source = Arc::clone(&self.source);
		let tx = self.tx.clone();
		std::thread::spawn(move || {
			let message = match catch_unwind(AssertUnwindSafe(|| source.inspect())) {
				Ok(inspection) => WorkerMessage::Inspected {
					generation,
					inspection: Box::new(inspection),
				},
				Err(panic) => WorkerMessage::Analyzed {
					generation,
					result: Err(format!(
						"input inspection panicked: {}",
						panic_text(&*panic)
					)),
				},
			};
			let _ = tx.send(message);
		});
	}

	fn start_analysis(&mut self) {
		// An input snapshot is analyzed once; a later analysis needs a new
		// snapshot from an explicit refresh.
		let Some(request) = self.pending.take() else {
			return;
		};
		self.generation += 1;
		self.busy = true;
		self.app.analyzing();
		let settings = self.app.effective_settings();
		let generation = self.generation;
		let cancellation = CancellationToken::new();
		self.cancellation = Some(cancellation.clone());
		let tx = self.tx.clone();
		spawn_with_merge_stack(move || {
			let progress = ChannelProgress {
				generation,
				tx: tx.clone(),
			};
			let result = catch_unwind(AssertUnwindSafe(|| {
				analyze_input(request, &settings, &progress, &cancellation)
			}))
			.unwrap_or_else(|panic| Err(format!("analysis panicked: {}", panic_text(&*panic))))
			.map(Box::new);
			let _ = tx.send(WorkerMessage::Analyzed { generation, result });
		});
	}

	fn cancel_running(&mut self) {
		if let Some(cancellation) = self.cancellation.take() {
			cancellation.cancel();
		}
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		self.cancel_running();
	}
}

/// Analysis walks deeply nested Clausewitz trees; give it the same stack the
/// CLI's main thread gets.
fn spawn_with_merge_stack(work: impl FnOnce() + Send + 'static) {
	std::thread::Builder::new()
		.name("foch-tui-analysis".to_string())
		.stack_size(64 * 1024 * 1024)
		.spawn(work)
		.expect("spawn analysis thread");
}

/// Run the complete N-way analysis of one frozen input. The browser never
/// commits, so the target only names where a commit would have gone; it is a
/// fresh path that analysis must leave untouched.
pub fn analyze_input(
	request: InputRequest,
	settings: &AnalysisSettings,
	progress: &dyn ProgressObserver,
	cancellation: &CancellationToken,
) -> Result<AnalysisView, String> {
	let out_dir = unused_analysis_target();
	let analyzed = analyze_merge(
		request,
		MergeAnalysisOptions {
			out_dir: out_dir.clone(),
			include_game_base: settings.game_base,
			include_base: false,
			gui_scroll_merge: settings.gui_scroll_merge,
			force: settings.force,
			ignore_replace_path: settings.ignore_replace_path,
			dep_overrides: Vec::new(),
			resolution_config_path: None,
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			playset_fingerprint: None,
			provenance: false,
			merge_workers: settings.merge_workers,
			retained_paths: None,
		},
		progress,
		cancellation,
	)
	.map_err(|error| match error {
		MergeError::Cancelled => "analysis cancelled".to_string(),
		other => other.to_string(),
	})?;
	debug_assert!(!out_dir.exists(), "analysis must not create its target");
	Ok(AnalysisView::from_analyzed(&analyzed))
}

fn unused_analysis_target() -> PathBuf {
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |elapsed| elapsed.as_nanos());
	std::env::temp_dir().join(format!("foch-tui-analysis-{}-{nanos}", std::process::id()))
}

/// A worker panic surfaces as a failed result instead of a browser that waits
/// forever.
fn panic_text(panic: &(dyn Any + Send)) -> String {
	panic
		.downcast_ref::<&str>()
		.map(|text| (*text).to_string())
		.or_else(|| panic.downcast_ref::<String>().cloned())
		.unwrap_or_else(|| "unknown panic".to_string())
}
