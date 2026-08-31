use crate::backend::{
	AnalysisReview, AnalysisRunError, AnalysisRunner, FochAnalysisRunner, InspectedAnalysisInput,
	bounded_text,
};
use crate::dto::{
	DesktopError, InputInspection, MergeAnalysisStage, MergeAnalysisState, MergeDisposition,
	MergeUnitCounts, MergeUnitDetail, MergeUnitListItem, MergeUnitPage, ReadinessState,
};
use foch::input::InputRequest;
use foch::merge::{
	CancellationToken, MergeAnalysisStage as FochMergeAnalysisStage, MergeProgress,
	ProgressObserver,
};
use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use uuid::Uuid;

const MAX_TERMINAL_ANALYSES: usize = 4;
const MAX_PAGE_SIZE: usize = 100;
const MAX_QUERY_CHARS: usize = 256;
const MAX_MESSAGE_CHARS: usize = 8_192;

#[derive(Clone)]
pub(crate) struct DesktopState {
	core: Arc<DesktopStateCore>,
}

struct DesktopStateCore {
	runner: Arc<dyn AnalysisRunner>,
	records: Mutex<RecordBook>,
}

#[derive(Default)]
struct RecordBook {
	active: Option<String>,
	input_inspected: bool,
	pending_request: Option<InputRequest>,
	by_id: HashMap<String, AnalysisRecord>,
	terminal_fifo: VecDeque<String>,
}

struct AnalysisRecord {
	state: MergeAnalysisState,
	stage: MergeAnalysisStage,
	completed_units: u64,
	total_units: u64,
	started: Instant,
	observed_elapsed: Duration,
	terminal_elapsed: Option<Duration>,
	counts: MergeUnitCounts,
	message: Option<String>,
	cancellation: CancellationToken,
	request: Option<InputRequest>,
	review: Option<Arc<dyn AnalysisReview>>,
}

impl DesktopState {
	pub(crate) fn production() -> Self {
		Self::new(Arc::new(FochAnalysisRunner))
	}

	pub(crate) fn new(runner: Arc<dyn AnalysisRunner>) -> Self {
		Self {
			core: Arc::new(DesktopStateCore {
				runner,
				records: Mutex::new(RecordBook::default()),
			}),
		}
	}

	pub(crate) fn inspect_input(&self) -> Result<InputInspection, DesktopError> {
		let InspectedAnalysisInput {
			inspection,
			request,
		} = self.core.runner.inspect_input();
		let mut records = self.records()?;
		records.input_inspected = true;
		records.pending_request = if inspection.readiness == ReadinessState::Ready {
			request
		} else {
			None
		};
		Ok(inspection)
	}

	pub(crate) fn queue_analysis(&self) -> Result<String, DesktopError> {
		let mut records = self.records()?;
		if records.active.is_some() {
			return Err(DesktopError::new(
				"analysis_busy",
				"another merge analysis is queued or running",
			));
		}
		if !records.input_inspected {
			return Err(DesktopError::new(
				"input_not_inspected",
				"inspect the current EU4 input before starting analysis",
			));
		}
		let Some(request) = records.pending_request.take() else {
			return Err(DesktopError::new(
				"input_not_ready",
				"the latest EU4 input inspection is not ready for analysis",
			));
		};
		let analysis_id = loop {
			let candidate = Uuid::new_v4().to_string();
			if !records.by_id.contains_key(&candidate) {
				break candidate;
			}
		};
		records.active = Some(analysis_id.clone());
		records.by_id.insert(
			analysis_id.clone(),
			AnalysisRecord {
				state: MergeAnalysisState::Queued,
				stage: MergeAnalysisStage::Inventory,
				completed_units: 0,
				total_units: 0,
				started: Instant::now(),
				observed_elapsed: Duration::ZERO,
				terminal_elapsed: None,
				counts: MergeUnitCounts::default(),
				message: None,
				cancellation: CancellationToken::new(),
				request: Some(request),
				review: None,
			},
		);
		Ok(analysis_id)
	}

	pub(crate) fn run_analysis(&self, analysis_id: &str) {
		let (cancellation, request) = match self.begin_run(analysis_id) {
			Ok(Some(run)) => run,
			Ok(None) => return,
			Err(error) => {
				self.finish_failure(analysis_id, error.message);
				return;
			}
		};
		let observer = StateProgressObserver {
			state: self.clone(),
			analysis_id: analysis_id.to_string(),
		};
		let result = catch_unwind(AssertUnwindSafe(|| {
			self.core
				.runner
				.run_analysis(analysis_id, request, &observer, &cancellation)
		}));
		match result {
			Ok(Ok(review)) if cancellation.is_cancelled() => {
				drop(review);
				self.finish_cancelled(analysis_id);
			}
			Ok(Ok(review)) => self.finish_success(analysis_id, review),
			Ok(Err(AnalysisRunError::Cancelled)) => self.finish_cancelled(analysis_id),
			Ok(Err(_)) if cancellation.is_cancelled() => self.finish_cancelled(analysis_id),
			Ok(Err(AnalysisRunError::Failed(message))) => {
				self.finish_failure(analysis_id, message);
			}
			Err(payload) => self.finish_failure(
				analysis_id,
				format!("analysis worker panicked: {}", panic_message(payload)),
			),
		}
	}

	pub(crate) fn cancel_analysis(&self, analysis_id: &str) -> Result<(), DesktopError> {
		validate_analysis_id(analysis_id)?;
		let mut records = self.records()?;
		let Some(record) = records.by_id.get_mut(analysis_id) else {
			return Err(DesktopError::new(
				"analysis_not_found",
				"merge analysis was not found",
			));
		};
		if !record.state.is_active() {
			return Ok(());
		}
		record.cancellation.cancel();
		if record.state == MergeAnalysisState::Queued {
			record.request = None;
			self.mark_terminal_locked(
				&mut records,
				analysis_id,
				MergeAnalysisState::Cancelled,
				Some("analysis cancelled before it started".to_string()),
				None,
			);
		} else {
			record.message = Some("cancellation requested".to_string());
		}
		Ok(())
	}

	pub(crate) fn summary(
		&self,
		analysis_id: &str,
	) -> Result<crate::dto::MergeAnalysisSummary, DesktopError> {
		validate_analysis_id(analysis_id)?;
		let records = self.records()?;
		let Some(record) = records.by_id.get(analysis_id) else {
			return Err(DesktopError::new(
				"analysis_not_found",
				"merge analysis was not found",
			));
		};
		let elapsed = record
			.terminal_elapsed
			.unwrap_or_else(|| record.started.elapsed().max(record.observed_elapsed));
		Ok(crate::dto::MergeAnalysisSummary {
			analysis_id: analysis_id.to_string(),
			state: record.state,
			stage: record.stage,
			completed_units: record.completed_units,
			total_units: record.total_units,
			elapsed_ms: duration_millis(elapsed),
			counts: record.counts,
			message: record.message.clone(),
		})
	}

	pub(crate) fn list_units(
		&self,
		analysis_id: &str,
		query: &str,
		disposition: Option<MergeDisposition>,
		page: usize,
		page_size: usize,
	) -> Result<MergeUnitPage, DesktopError> {
		validate_analysis_id(analysis_id)?;
		validate_page(page, page_size, query)?;
		let review = self.completed_review(analysis_id)?;
		let normalized_query = query.to_lowercase();
		let mut matching = Vec::new();
		for index in 0..review.len() {
			let Some(unit) = review.unit_at(index) else {
				return Err(DesktopError::internal(
					"analysis review changed while it was being read",
				));
			};
			if disposition.is_some_and(|expected| unit.disposition != expected)
				|| !unit_matches(&unit, &normalized_query)
			{
				continue;
			}
			matching.push(MergeUnitListItem {
				id: unit.id,
				path: unit.path,
				family: unit.family,
				kind: unit.kind,
				disposition: unit.disposition,
				strategy: unit.strategy,
				contributor_count: unit.contributor_count,
			});
		}
		let total = matching.len();
		let start = page
			.checked_sub(1)
			.and_then(|offset| offset.checked_mul(page_size))
			.unwrap_or(usize::MAX);
		let items = matching.into_iter().skip(start).take(page_size).collect();
		Ok(MergeUnitPage {
			items,
			total,
			page,
			page_size,
		})
	}

	pub(crate) fn unit(
		&self,
		analysis_id: &str,
		unit_id: &str,
	) -> Result<MergeUnitDetail, DesktopError> {
		validate_analysis_id(analysis_id)?;
		let review = self.completed_review(analysis_id)?;
		review
			.unit(unit_id)
			.ok_or_else(|| DesktopError::new("unit_not_found", "merge review unit was not found"))
	}

	fn records(&self) -> Result<MutexGuard<'_, RecordBook>, DesktopError> {
		self.core
			.records
			.lock()
			.map_err(|_| DesktopError::internal("desktop analysis state is unavailable"))
	}

	fn begin_run(
		&self,
		analysis_id: &str,
	) -> Result<Option<(CancellationToken, InputRequest)>, DesktopError> {
		let mut records = self.records()?;
		let Some(record) = records.by_id.get_mut(analysis_id) else {
			return Ok(None);
		};
		if record.state != MergeAnalysisState::Queued {
			return Ok(None);
		}
		if record.cancellation.is_cancelled() {
			return Ok(None);
		}
		let request = record.request.take().ok_or_else(|| {
			DesktopError::internal("queued merge analysis has no inspected input request")
		})?;
		record.state = MergeAnalysisState::Running;
		Ok(Some((record.cancellation.clone(), request)))
	}

	fn update_progress(&self, analysis_id: &str, progress: MergeProgress) {
		let Ok(mut records) = self.records() else {
			return;
		};
		let Some(record) = records.by_id.get_mut(analysis_id) else {
			return;
		};
		if record.state != MergeAnalysisState::Running {
			return;
		}
		record.stage = map_stage(progress.stage);
		record.observed_elapsed = record.observed_elapsed.max(progress.elapsed);
		if let Some(completed_units) = progress.completed_units {
			record.completed_units = completed_units;
		}
		if let Some(total_units) = progress.total_units {
			record.total_units = total_units;
		}
	}

	fn finish_success(&self, analysis_id: &str, review: Arc<dyn AnalysisReview>) {
		let state = review.state();
		if !state.is_complete() {
			self.finish_failure(
				analysis_id,
				"analysis backend returned a non-terminal review state",
			);
			return;
		}
		let counts = review.counts();
		if counts.total != review.len() {
			self.finish_failure(
				analysis_id,
				"analysis review count does not match its unit ledger",
			);
			return;
		}
		let Ok(mut records) = self.records() else {
			return;
		};
		let Some(record) = records.by_id.get_mut(analysis_id) else {
			return;
		};
		if !record.state.is_active() {
			return;
		}
		if record.cancellation.is_cancelled() {
			self.mark_terminal_locked(
				&mut records,
				analysis_id,
				MergeAnalysisState::Cancelled,
				Some("analysis cancelled".to_string()),
				None,
			);
			return;
		}
		record.counts = counts;
		record.completed_units = usize_to_u64(counts.total);
		record.total_units = usize_to_u64(counts.total);
		self.mark_terminal_locked(&mut records, analysis_id, state, None, Some(review));
	}

	fn finish_cancelled(&self, analysis_id: &str) {
		let Ok(mut records) = self.records() else {
			return;
		};
		self.mark_terminal_locked(
			&mut records,
			analysis_id,
			MergeAnalysisState::Cancelled,
			Some("analysis cancelled".to_string()),
			None,
		);
	}

	fn finish_failure(&self, analysis_id: &str, message: impl AsRef<str>) {
		let Ok(mut records) = self.records() else {
			return;
		};
		self.mark_terminal_locked(
			&mut records,
			analysis_id,
			MergeAnalysisState::Failed,
			Some(bounded_text(message.as_ref(), MAX_MESSAGE_CHARS)),
			None,
		);
	}

	fn mark_terminal_locked(
		&self,
		records: &mut RecordBook,
		analysis_id: &str,
		state: MergeAnalysisState,
		message: Option<String>,
		review: Option<Arc<dyn AnalysisReview>>,
	) {
		let Some(record) = records.by_id.get_mut(analysis_id) else {
			return;
		};
		if !record.state.is_active() {
			return;
		}
		record.state = state;
		record.stage = MergeAnalysisStage::Complete;
		record.terminal_elapsed = Some(record.started.elapsed().max(record.observed_elapsed));
		record.message = message;
		record.request = None;
		record.review = review;
		if records.active.as_deref() == Some(analysis_id) {
			records.active = None;
		}
		records.terminal_fifo.push_back(analysis_id.to_string());
		while records.terminal_fifo.len() > MAX_TERMINAL_ANALYSES {
			if let Some(expired) = records.terminal_fifo.pop_front() {
				records.by_id.remove(&expired);
			}
		}
	}

	fn completed_review(&self, analysis_id: &str) -> Result<Arc<dyn AnalysisReview>, DesktopError> {
		let records = self.records()?;
		let Some(record) = records.by_id.get(analysis_id) else {
			return Err(DesktopError::new(
				"analysis_not_found",
				"merge analysis was not found",
			));
		};
		if !record.state.is_complete() {
			return Err(DesktopError::new(
				"analysis_not_complete",
				"merge analysis has not produced a review ledger",
			));
		}
		record
			.review
			.clone()
			.ok_or_else(|| DesktopError::internal("completed merge analysis has no review ledger"))
	}
}

struct StateProgressObserver {
	state: DesktopState,
	analysis_id: String,
}

impl ProgressObserver for StateProgressObserver {
	fn update(&self, progress: MergeProgress) {
		self.state.update_progress(&self.analysis_id, progress);
	}
}

fn validate_analysis_id(analysis_id: &str) -> Result<(), DesktopError> {
	Uuid::parse_str(analysis_id)
		.map_err(|_| DesktopError::new("invalid_analysis_id", "analysisId must be a valid UUID"))?;
	Ok(())
}

fn validate_page(page: usize, page_size: usize, query: &str) -> Result<(), DesktopError> {
	if page == 0 {
		return Err(DesktopError::new("invalid_page", "page must be at least 1"));
	}
	if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
		return Err(DesktopError::new(
			"invalid_page_size",
			format!("pageSize must be between 1 and {MAX_PAGE_SIZE}"),
		));
	}
	if query.chars().count() > MAX_QUERY_CHARS {
		return Err(DesktopError::new(
			"query_too_long",
			format!("query must contain at most {MAX_QUERY_CHARS} characters"),
		));
	}
	Ok(())
}

fn unit_matches(unit: &MergeUnitDetail, normalized_query: &str) -> bool {
	if normalized_query.is_empty() {
		return true;
	}
	[
		unit.id.as_str(),
		unit.path.as_str(),
		unit.family.as_str(),
		unit.strategy.as_str(),
		unit.summary.as_str(),
	]
	.into_iter()
	.any(|value| value.to_lowercase().contains(normalized_query))
}

fn map_stage(stage: FochMergeAnalysisStage) -> MergeAnalysisStage {
	match stage {
		FochMergeAnalysisStage::Inventory => MergeAnalysisStage::Inventory,
		FochMergeAnalysisStage::ResolveInput => MergeAnalysisStage::ResolveInput,
		FochMergeAnalysisStage::SemanticMerge => MergeAnalysisStage::SemanticMerge,
		FochMergeAnalysisStage::ValidateOutput => MergeAnalysisStage::ValidateOutput,
		FochMergeAnalysisStage::FreezeArtifacts => MergeAnalysisStage::FreezeArtifacts,
	}
}

fn duration_millis(duration: Duration) -> u64 {
	u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn usize_to_u64(value: usize) -> u64 {
	u64::try_from(value).unwrap_or(u64::MAX)
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
	if let Some(message) = payload.downcast_ref::<&str>() {
		return bounded_text(message, MAX_MESSAGE_CHARS);
	}
	if let Some(message) = payload.downcast_ref::<String>() {
		return bounded_text(message, MAX_MESSAGE_CHARS);
	}
	"unknown panic payload".to_string()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::dto::{
		BaseDataState, BaseDataView, InstalledGameView, MergeUnitContributor, MergeUnitKind,
		ReadinessIssue,
	};
	use foch::input::Config;
	use std::path::PathBuf;
	use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
	use std::thread;

	struct FakeReview {
		state: MergeAnalysisState,
		units: Vec<MergeUnitDetail>,
	}

	impl AnalysisReview for FakeReview {
		fn state(&self) -> MergeAnalysisState {
			self.state
		}

		fn counts(&self) -> MergeUnitCounts {
			let mut counts = MergeUnitCounts {
				total: self.units.len(),
				..MergeUnitCounts::default()
			};
			for unit in &self.units {
				match unit.disposition {
					MergeDisposition::Safe => counts.safe += 1,
					MergeDisposition::Copy => counts.copy += 1,
					MergeDisposition::NeedsUserChoice => counts.needs_user_choice += 1,
					MergeDisposition::UnsupportedInput => counts.unsupported_input += 1,
					MergeDisposition::EngineFailure => counts.engine_failure += 1,
					MergeDisposition::Deferred => counts.deferred += 1,
				}
			}
			counts
		}

		fn len(&self) -> usize {
			self.units.len()
		}

		fn unit_at(&self, index: usize) -> Option<MergeUnitDetail> {
			self.units.get(index).cloned()
		}

		fn unit(&self, id: &str) -> Option<MergeUnitDetail> {
			self.units.iter().find(|unit| unit.id == id).cloned()
		}
	}

	enum FakeOutcome {
		Complete(Arc<dyn AnalysisReview>),
		Fail(String),
		Panic,
		WaitForCancellation(Arc<AtomicBool>),
	}

	struct FakeRunner {
		inspection: Mutex<InputInspection>,
		outcomes: Mutex<VecDeque<FakeOutcome>>,
		inspection_count: AtomicUsize,
		seen_request_paths: Mutex<Vec<PathBuf>>,
	}

	impl AnalysisRunner for FakeRunner {
		fn inspect_input(&self) -> InspectedAnalysisInput {
			let sequence = self.inspection_count.fetch_add(1, Ordering::Relaxed) + 1;
			let inspection = self.inspection.lock().unwrap().clone();
			let request = (inspection.readiness == ReadinessState::Ready)
				.then(|| fake_request(&format!("inspection-{sequence}")));
			InspectedAnalysisInput {
				inspection,
				request,
			}
		}

		fn run_analysis(
			&self,
			_analysis_id: &str,
			request: InputRequest,
			progress: &dyn ProgressObserver,
			cancellation: &CancellationToken,
		) -> Result<Arc<dyn AnalysisReview>, AnalysisRunError> {
			self.seen_request_paths
				.lock()
				.unwrap()
				.push(request.source_path().to_path_buf());
			let outcome = self.outcomes.lock().unwrap().pop_front().unwrap();
			match outcome {
				FakeOutcome::Complete(review) => Ok(review),
				FakeOutcome::Fail(message) => Err(AnalysisRunError::Failed(message)),
				FakeOutcome::Panic => panic!("fake analysis panic"),
				FakeOutcome::WaitForCancellation(started) => {
					progress.update(MergeProgress {
						stage: FochMergeAnalysisStage::SemanticMerge,
						completed: false,
						completed_units: Some(2),
						total_units: Some(7),
						elapsed: Duration::from_millis(5),
					});
					started.store(true, Ordering::Release);
					while !cancellation.is_cancelled() {
						thread::yield_now();
					}
					Err(AnalysisRunError::Cancelled)
				}
			}
		}
	}

	fn ready_inspection() -> InputInspection {
		InputInspection {
			readiness: ReadinessState::Ready,
			game: InstalledGameView {
				name: "Europa Universalis IV".to_string(),
				version: Some("1.37.5".to_string()),
				install_path: Some("/game".to_string()),
			},
			base_data: BaseDataView {
				state: BaseDataState::Ready,
				version: Some("1.37.5".to_string()),
				detail: "ready".to_string(),
			},
			playset: None,
			issues: Vec::new(),
		}
	}

	fn fake_state(outcomes: Vec<FakeOutcome>) -> DesktopState {
		DesktopState::new(fake_runner(ready_inspection(), outcomes))
	}

	fn fake_runner(inspection: InputInspection, outcomes: Vec<FakeOutcome>) -> Arc<FakeRunner> {
		Arc::new(FakeRunner {
			inspection: Mutex::new(inspection),
			outcomes: Mutex::new(outcomes.into()),
			inspection_count: AtomicUsize::new(0),
			seen_request_paths: Mutex::new(Vec::new()),
		})
	}

	fn fake_request(token: &str) -> InputRequest {
		InputRequest::from_playset_path(PathBuf::from(token), Config::default())
	}

	fn queue_fake_analysis(state: &DesktopState) -> String {
		state.inspect_input().unwrap();
		state.queue_analysis().unwrap()
	}

	fn run_fake_analysis(state: &DesktopState, analysis_id: &str) {
		state.run_analysis(analysis_id);
	}

	fn review(state: MergeAnalysisState, units: Vec<MergeUnitDetail>) -> Arc<dyn AnalysisReview> {
		Arc::new(FakeReview { state, units })
	}

	fn unit(id: &str, path: &str, disposition: MergeDisposition) -> MergeUnitDetail {
		MergeUnitDetail {
			id: id.to_string(),
			path: path.to_string(),
			family: "common/ideas".to_string(),
			kind: MergeUnitKind::File,
			disposition,
			strategy: "structural_merge".to_string(),
			summary: format!("review {path}"),
			output_path: Some(path.to_string()),
			contributors: vec![MergeUnitContributor {
				mod_id: "mod-a".to_string(),
				name: "Mod A".to_string(),
				position: 0,
				is_base_game: false,
			}],
			contributor_count: 1,
			notes: vec!["note".to_string()],
		}
	}

	#[test]
	fn enforces_one_active_analysis_and_generates_v4_ids() {
		let state = fake_state(Vec::new());
		assert_eq!(
			state.queue_analysis().unwrap_err().code,
			"input_not_inspected"
		);
		let id = queue_fake_analysis(&state);
		let parsed = Uuid::parse_str(&id).unwrap();
		assert_eq!(parsed.get_version(), Some(uuid::Version::Random));
		assert_eq!(state.queue_analysis().unwrap_err().code, "analysis_busy");
		state.cancel_analysis(&id).unwrap();
		assert_eq!(
			state.summary(&id).unwrap().state,
			MergeAnalysisState::Cancelled
		);
	}

	#[test]
	fn binds_the_displayed_inspection_token_to_the_worker() {
		let runner = fake_runner(
			ready_inspection(),
			vec![FakeOutcome::Complete(review(
				MergeAnalysisState::Ready,
				Vec::new(),
			))],
		);
		let state = DesktopState::new(runner.clone());
		state.inspect_input().unwrap();
		let mut changed_environment = ready_inspection();
		changed_environment.game.install_path = Some("/different-game".to_string());
		*runner.inspection.lock().unwrap() = changed_environment;

		let id = state.queue_analysis().unwrap();
		state.run_analysis(&id);

		assert_eq!(runner.inspection_count.load(Ordering::Relaxed), 1);
		assert_eq!(
			runner.seen_request_paths.lock().unwrap().as_slice(),
			[PathBuf::from("inspection-1")]
		);
	}

	#[test]
	fn blocked_reinspection_clears_the_previous_ready_token() {
		let runner = fake_runner(ready_inspection(), Vec::new());
		let state = DesktopState::new(runner.clone());
		state.inspect_input().unwrap();
		let mut blocked = ready_inspection();
		blocked.readiness = ReadinessState::Blocked;
		blocked.issues.push(ReadinessIssue {
			id: "base_missing".to_string(),
			title: "Base data missing".to_string(),
			detail: "Build it first".to_string(),
			action: None,
		});
		*runner.inspection.lock().unwrap() = blocked;

		state.inspect_input().unwrap();

		assert_eq!(runner.inspection_count.load(Ordering::Relaxed), 2);
		assert_eq!(state.queue_analysis().unwrap_err().code, "input_not_ready");
		assert!(runner.seen_request_paths.lock().unwrap().is_empty());
	}

	#[test]
	fn cancelling_a_queued_analysis_drops_its_bound_input_without_running() {
		let runner = fake_runner(
			ready_inspection(),
			vec![FakeOutcome::Complete(review(
				MergeAnalysisState::Ready,
				Vec::new(),
			))],
		);
		let state = DesktopState::new(runner.clone());
		state.inspect_input().unwrap();
		let id = state.queue_analysis().unwrap();

		state.cancel_analysis(&id).unwrap();
		state.run_analysis(&id);

		assert_eq!(
			state.summary(&id).unwrap().state,
			MergeAnalysisState::Cancelled
		);
		assert!(runner.seen_request_paths.lock().unwrap().is_empty());
		assert_eq!(runner.outcomes.lock().unwrap().len(), 1);
	}

	#[test]
	fn maps_all_success_states_and_retains_only_four_terminal_analyses() {
		let states = [
			MergeAnalysisState::Ready,
			MergeAnalysisState::ReadyWithDeferrals,
			MergeAnalysisState::Blocked,
			MergeAnalysisState::Ready,
			MergeAnalysisState::ReadyWithDeferrals,
		];
		let outcomes = states
			.iter()
			.map(|state| FakeOutcome::Complete(review(*state, Vec::new())))
			.collect();
		let desktop = fake_state(outcomes);
		let mut ids = Vec::new();
		for expected in states {
			let id = queue_fake_analysis(&desktop);
			run_fake_analysis(&desktop, &id);
			assert_eq!(desktop.summary(&id).unwrap().state, expected);
			ids.push(id);
		}
		assert_eq!(
			desktop.summary(&ids[0]).unwrap_err().code,
			"analysis_not_found"
		);
		for id in &ids[1..] {
			assert!(desktop.summary(id).is_ok());
		}
	}

	#[test]
	fn reports_progress_and_cancels_running_analysis_idempotently() {
		let started = Arc::new(AtomicBool::new(false));
		let state = fake_state(vec![FakeOutcome::WaitForCancellation(started.clone())]);
		let id = queue_fake_analysis(&state);
		let worker_state = state.clone();
		let worker_id = id.clone();
		let worker = thread::spawn(move || worker_state.run_analysis(&worker_id));
		while !started.load(Ordering::Acquire) {
			thread::yield_now();
		}
		let running = state.summary(&id).unwrap();
		assert_eq!(running.state, MergeAnalysisState::Running);
		assert_eq!(running.stage, MergeAnalysisStage::SemanticMerge);
		assert_eq!((running.completed_units, running.total_units), (2, 7));
		state.cancel_analysis(&id).unwrap();
		worker.join().unwrap();
		let cancelled = state.summary(&id).unwrap();
		assert_eq!(cancelled.state, MergeAnalysisState::Cancelled);
		assert_eq!(cancelled.stage, MergeAnalysisStage::Complete);
		state.cancel_analysis(&id).unwrap();
	}

	#[test]
	fn filters_before_paginating_and_validates_bounds() {
		let units = vec![
			unit("file:common/a.txt", "common/a.txt", MergeDisposition::Safe),
			unit("file:events/b.txt", "events/b.txt", MergeDisposition::Copy),
			unit("file:common/c.txt", "common/c.txt", MergeDisposition::Safe),
		];
		let state = fake_state(vec![FakeOutcome::Complete(review(
			MergeAnalysisState::Ready,
			units,
		))]);
		let id = queue_fake_analysis(&state);
		run_fake_analysis(&state, &id);
		let first = state
			.list_units(&id, "COMMON", Some(MergeDisposition::Safe), 1, 1)
			.unwrap();
		assert_eq!(first.total, 2);
		assert_eq!(first.items[0].path, "common/a.txt");
		let second = state
			.list_units(&id, "common", Some(MergeDisposition::Safe), 2, 1)
			.unwrap();
		assert_eq!(second.items[0].path, "common/c.txt");
		assert!(
			state
				.list_units(&id, "", None, usize::MAX, 100)
				.unwrap()
				.items
				.is_empty()
		);
		assert_eq!(
			state.list_units(&id, "", None, 0, 1).unwrap_err().code,
			"invalid_page"
		);
		assert_eq!(
			state.list_units(&id, "", None, 1, 101).unwrap_err().code,
			"invalid_page_size"
		);
		assert_eq!(
			state
				.list_units(&id, &"界".repeat(257), None, 1, 10)
				.unwrap_err()
				.code,
			"query_too_long"
		);
		assert_eq!(
			state.unit(&id, "missing").unwrap_err().code,
			"unit_not_found"
		);
	}

	#[test]
	fn rejects_review_access_until_analysis_completes() {
		let state = fake_state(Vec::new());
		let id = queue_fake_analysis(&state);
		assert_eq!(
			state.list_units(&id, "", None, 1, 10).unwrap_err().code,
			"analysis_not_complete"
		);
	}

	#[test]
	fn converts_worker_failures_and_panics_to_terminal_failures() {
		let state = fake_state(vec![
			FakeOutcome::Fail("backend failed".to_string()),
			FakeOutcome::Panic,
		]);
		let failed_id = queue_fake_analysis(&state);
		run_fake_analysis(&state, &failed_id);
		let failed = state.summary(&failed_id).unwrap();
		assert_eq!(failed.state, MergeAnalysisState::Failed);
		assert_eq!(failed.message.as_deref(), Some("backend failed"));
		let panic_id = queue_fake_analysis(&state);
		run_fake_analysis(&state, &panic_id);
		let panicked = state.summary(&panic_id).unwrap();
		assert_eq!(panicked.state, MergeAnalysisState::Failed);
		assert!(panicked.message.unwrap().contains("fake analysis panic"));
	}

	#[test]
	fn freezes_terminal_elapsed_time() {
		let state = fake_state(vec![FakeOutcome::Complete(review(
			MergeAnalysisState::Ready,
			Vec::new(),
		))]);
		let id = queue_fake_analysis(&state);
		run_fake_analysis(&state, &id);
		let first = state.summary(&id).unwrap().elapsed_ms;
		thread::sleep(Duration::from_millis(2));
		assert_eq!(state.summary(&id).unwrap().elapsed_ms, first);
	}

	#[test]
	fn returns_stable_input_and_identifier_errors() {
		let mut inspection = ready_inspection();
		inspection.readiness = ReadinessState::Blocked;
		inspection.issues.push(ReadinessIssue {
			id: "base_missing".to_string(),
			title: "Base data missing".to_string(),
			detail: "Build it first".to_string(),
			action: None,
		});
		let state = DesktopState::new(Arc::new(FakeRunner {
			inspection: Mutex::new(inspection),
			outcomes: Mutex::new(VecDeque::new()),
			inspection_count: AtomicUsize::new(0),
			seen_request_paths: Mutex::new(Vec::new()),
		}));
		state.inspect_input().unwrap();
		assert_eq!(state.queue_analysis().unwrap_err().code, "input_not_ready");
		assert_eq!(
			state.summary("not-a-uuid").unwrap_err().code,
			"invalid_analysis_id"
		);
		assert_eq!(
			state.summary(&Uuid::new_v4().to_string()).unwrap_err().code,
			"analysis_not_found"
		);
	}
}
