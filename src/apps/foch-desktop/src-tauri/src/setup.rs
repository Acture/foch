//! First-launch EU4 check and base-data preparation for the desktop.
//!
//! The desktop calls the `foch` library directly. It does not spawn the CLI.

use crate::dto::{
	BaseDataState, DesktopError, InputInspection, InstallRecoveryNote, SetupPhase, SetupStageView,
	SetupStatus, VerifiedGamePath,
};
use crate::state::DesktopState;
use foch::game::eu4::Eu4;
use foch::game::eu4::base::analysis_rules_version;
use foch::game::eu4::base::snapshot::{
	BaseBuildObserver, BaseDataSource, build_base_snapshot_with_observer, detect_game_version,
	install_built_snapshot, installed_data_dir,
};
use foch::input::{
	Config, FileFilter, InputCheckObserver, InputCheckProgress, get_config_dir_path,
	load_config_read_only,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use uuid::Uuid;

pub(crate) struct BuildSpec {
	pub(crate) game_root: PathBuf,
	pub(crate) game_version: String,
	pub(crate) rules_version: String,
	pub(crate) input_identity: String,
}

pub(crate) struct BuildProgress {
	pub(crate) stage: String,
	pub(crate) completed: Option<u64>,
	pub(crate) total: Option<u64>,
}

type BuildProgressListener = Arc<dyn Fn(BuildProgress) + Send + Sync>;

pub(crate) trait SetupWork: Send + Sync {
	fn build(
		&self,
		spec: &BuildSpec,
		on_progress: BuildProgressListener,
		cancel: &Arc<AtomicBool>,
	) -> Result<(), String>;
}

pub(crate) fn foch_setup_work() -> Arc<dyn SetupWork> {
	Arc::new(FochSetupWork)
}

struct FochSetupWork;

impl SetupWork for FochSetupWork {
	fn build(
		&self,
		spec: &BuildSpec,
		on_progress: BuildProgressListener,
		cancel: &Arc<AtomicBool>,
	) -> Result<(), String> {
		let config = load_config_read_only().unwrap_or_default();
		let filter = FileFilter::new(Eu4, &config.extra_ignore_patterns)?;
		let stage_progress = Arc::clone(&on_progress);
		let document_progress = Arc::clone(&on_progress);
		let mut observer = BaseBuildObserver::silent(Eu4::KEY)
			.with_cancel_flag(Arc::clone(cancel))
			.with_stage_listener(Box::new(move |stage, done| {
				if !done {
					stage_progress(BuildProgress {
						stage: stage.to_string(),
						completed: None,
						total: None,
					});
				}
			}))
			.with_document_listener(move |stage, completed, total| {
				document_progress(BuildProgress {
					stage: stage.to_string(),
					completed: Some(completed),
					total: Some(total),
				});
			});
		let built = build_base_snapshot_with_observer(
			&Eu4,
			&spec.game_root,
			Some(&spec.game_version),
			&filter,
			&mut observer,
		)?;
		if cancel.load(Ordering::Relaxed) {
			return Err("base data build cancelled".to_string());
		}
		on_progress(BuildProgress {
			stage: "install".to_string(),
			completed: None,
			total: None,
		});
		if let Err(error) = install_built_snapshot(
			&built.encoded_snapshot,
			BaseDataSource::Build,
			Some(built.snapshot_asset_name),
			Some(built.snapshot_sha256),
		) {
			let _ = fs::remove_dir_all(installed_data_dir(Eu4::KEY, &spec.game_version));
			return Err(error);
		}
		Ok(())
	}
}

#[derive(Clone)]
pub(crate) struct SetupController {
	inner: Arc<SetupInner>,
}

struct SetupInner {
	work: Arc<dyn SetupWork>,
	book: Mutex<SetupBook>,
}

struct SetupBook {
	phase: SetupPhase,
	stage: Option<String>,
	completed: Option<u64>,
	total: Option<u64>,
	stages: Vec<SetupStageView>,
	inspection: Option<InputInspection>,
	message: Option<String>,
	build_id: Option<String>,
	build_key: Option<String>,
	build_stage: Option<String>,
	build_completed: Option<u64>,
	build_total: Option<u64>,
	document_completed: Option<u64>,
	document_total: Option<u64>,
	cancel: Option<Arc<AtomicBool>>,
	install_note: Option<InstallRecoveryNote>,
}

impl SetupController {
	pub(crate) fn new(work: Arc<dyn SetupWork>) -> Self {
		Self {
			inner: Arc::new(SetupInner {
				work,
				book: Mutex::new(SetupBook::idle()),
			}),
		}
	}

	pub(crate) fn status(&self) -> SetupStatus {
		self.view(&self.lock())
	}

	pub(crate) fn start_check(&self, desktop: &DesktopState) -> Result<SetupStatus, DesktopError> {
		{
			let mut book = self.lock();
			if book.phase == SetupPhase::Checking {
				return Ok(self.view(&book));
			}
			if book.phase == SetupPhase::Building {
				return Err(DesktopError::new(
					"setup_busy",
					"EU4 setup is already checking or building base data",
				));
			}
			book.phase = SetupPhase::Checking;
			book.message = None;
			book.build_id = None;
			book.build_key = None;
			book.build_stage = None;
			book.build_completed = None;
			book.build_total = None;
			book.cancel = None;
			book.stages.clear();
			record_stage(&mut book, "game_install", None, None);
		}
		let controller = self.clone();
		let desktop = desktop.clone();
		thread::spawn(move || controller.run_check(desktop));
		Ok(self.status())
	}

	pub(crate) fn start_build(&self, desktop: &DesktopState) -> Result<SetupStatus, DesktopError> {
		let spec = {
			let mut book = self.lock();
			if book.phase == SetupPhase::Checking {
				return Err(DesktopError::new(
					"setup_busy",
					"EU4 setup is already checking or building base data",
				));
			}
			let inspection = book.inspection.clone().ok_or_else(|| {
				DesktopError::new(
					"input_not_inspected",
					"inspect the current EU4 input before building base data",
				)
			})?;
			if inspection.base_data.state == BaseDataState::Ready {
				return Err(DesktopError::new(
					"base_data_ready",
					"installed base data already matches this EU4 version and rules",
				));
			}
			let install_path = inspection.game.install_path.clone().ok_or_else(|| {
				DesktopError::new(
					"game_path_required",
					"choose an EU4 installation before building base data",
				)
			})?;
			let game_version = inspection
				.game
				.version
				.clone()
				.or_else(|| inspection.base_data.version.clone())
				.ok_or_else(|| {
					DesktopError::new(
						"game_version_required",
						"EU4 version was not detected; choose an installation that contains launcher-settings.json or version.txt",
					)
				})?;
			let spec = BuildSpec {
				game_root: PathBuf::from(&install_path),
				game_version,
				rules_version: analysis_rules_version().to_string(),
				input_identity: input_identity(&inspection),
			};
			let key = build_key(&spec);
			if book.phase == SetupPhase::Building {
				if book.build_key.as_deref() == Some(key.as_str()) {
					return Ok(self.view(&book));
				}
				return Err(DesktopError::new(
					"base_build_busy",
					"a base data build for a different game, rules, or playset is already running",
				));
			}
			let build_id = Uuid::new_v4().to_string();
			book.phase = SetupPhase::Building;
			book.build_id = Some(build_id);
			book.build_key = Some(key);
			book.cancel = Some(Arc::new(AtomicBool::new(false)));
			book.document_completed = None;
			book.document_total = None;
			book.message = None;
			record_stage(&mut book, "detect_version", None, None);
			book.build_stage = Some("detect_version".to_string());
			book.build_completed = None;
			book.build_total = None;
			spec
		};
		let controller = self.clone();
		let desktop = desktop.clone();
		let cancel = self
			.lock()
			.cancel
			.clone()
			.expect("a started build stores its cancel flag");
		thread::spawn(move || controller.run_build(desktop, spec, cancel));
		Ok(self.status())
	}

	pub(crate) fn cancel_build(&self, build_id: &str) -> Result<SetupStatus, DesktopError> {
		let mut book = self.lock();
		if book.phase != SetupPhase::Building || book.build_id.as_deref() != Some(build_id) {
			return Err(DesktopError::new(
				"build_not_active",
				"that base data build is not running",
			));
		}
		if let Some(cancel) = book.cancel.as_ref() {
			cancel.store(true, Ordering::Relaxed);
		}
		book.message = Some(
			"Cancelling the base data build. A partial snapshot will not be installed.".to_string(),
		);
		Ok(self.view(&book))
	}

	fn run_check(&self, desktop: DesktopState) {
		let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			let mut observer = BookObserver {
				controller: self.clone(),
			};
			desktop.inspect_input_observing(&mut observer)
		}));
		match result {
			Ok(inspection) => self.finish_check(inspection),
			Err(_) => self.finish_check(Err(DesktopError::internal(
				"the EU4 input check panicked before it finished",
			))),
		}
	}

	fn run_build(&self, desktop: DesktopState, spec: BuildSpec, cancel: Arc<AtomicBool>) {
		let controller = self.clone();
		let on_progress: Arc<dyn Fn(BuildProgress) + Send + Sync> = Arc::new(move |progress| {
			controller.record_build(progress);
		});
		let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			self.inner.work.build(&spec, on_progress, &cancel)
		}));
		// Cancel during install can land after the snapshot is already in place.
		// A finished install still rechecks; only an unfinished build is cancelled.
		if cancel.load(Ordering::Relaxed) && !matches!(result, Ok(Ok(()))) {
			self.finish_cancelled();
			return;
		}
		match result {
			Ok(Ok(())) => {
				if !self.begin_recheck() {
					return;
				}
				self.run_check(desktop);
			}
			Ok(Err(error)) => self.finish_build_failed(error),
			Err(_) => self.finish_build_failed(
				"the base data build panicked before it finished; nothing was installed"
					.to_string(),
			),
		}
	}

	fn finish_check(&self, result: Result<InputInspection, DesktopError>) {
		let mut book = self.lock();
		if book.phase != SetupPhase::Checking {
			return;
		}
		match result {
			Ok(inspection) => {
				book.install_note = install_note(&inspection);
				book.inspection = Some(inspection);
				book.phase = SetupPhase::Checked;
				book.message = None;
			}
			Err(error) => {
				book.phase = SetupPhase::Failed;
				book.message = Some(error.message);
			}
		}
	}

	fn begin_recheck(&self) -> bool {
		let mut book = self.lock();
		if book.phase != SetupPhase::Building {
			return false;
		}
		book.phase = SetupPhase::Checking;
		book.build_id = None;
		book.build_key = None;
		book.build_stage = None;
		book.build_completed = None;
		book.build_total = None;
		book.cancel = None;
		book.message = None;
		book.stages.clear();
		record_stage(&mut book, "game_install", None, None);
		true
	}

	fn finish_cancelled(&self) {
		let mut book = self.lock();
		if book.phase != SetupPhase::Building {
			return;
		}
		book.phase = SetupPhase::BuildCancelled;
		book.build_id = None;
		book.build_key = None;
		book.cancel = None;
		book.message = Some(
			"The base data build was cancelled. A partial snapshot was not installed.".to_string(),
		);
	}

	fn finish_build_failed(&self, error: String) {
		let mut book = self.lock();
		if book.phase != SetupPhase::Building {
			return;
		}
		book.phase = SetupPhase::BuildFailed;
		book.build_id = None;
		book.build_key = None;
		book.cancel = None;
		book.message = Some(error);
	}

	fn record_check_progress(&self, progress: InputCheckProgress) {
		let mut book = self.lock();
		if book.phase != SetupPhase::Checking {
			return;
		}
		record_stage(
			&mut book,
			progress.stage.as_str(),
			progress.completed,
			progress.total,
		);
	}

	fn record_build(&self, progress: BuildProgress) {
		let mut book = self.lock();
		if book.phase != SetupPhase::Building {
			return;
		}
		book.build_stage = Some(progress.stage.clone());
		book.build_completed = progress.completed;
		book.build_total = progress.total;
		if progress.stage == "parse_documents"
			&& let (Some(completed), Some(total)) = (progress.completed, progress.total)
		{
			book.document_completed = Some(completed);
			book.document_total = Some(total);
		}
		record_stage(
			&mut book,
			&progress.stage,
			progress.completed,
			progress.total,
		);
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, SetupBook> {
		self.inner
			.book
			.lock()
			.unwrap_or_else(|error| error.into_inner())
	}

	fn view(&self, book: &SetupBook) -> SetupStatus {
		SetupStatus {
			phase: book.phase,
			stage: book.stage.clone(),
			completed: book.completed,
			total: book.total,
			stages: book.stages.clone(),
			inspection: book.inspection.clone(),
			message: book.message.clone(),
			build_id: book.build_id.clone(),
			build_stage: book.build_stage.clone(),
			build_completed: book.build_completed,
			build_total: book.build_total,
			document_completed: book.document_completed,
			document_total: book.document_total,
			install_note: book.install_note.clone(),
		}
	}
}

struct BookObserver {
	controller: SetupController,
}

impl InputCheckObserver for BookObserver {
	fn on_progress(&mut self, progress: InputCheckProgress) {
		self.controller.record_check_progress(progress);
	}
}

impl SetupBook {
	fn idle() -> Self {
		Self {
			phase: SetupPhase::Idle,
			stage: None,
			completed: None,
			total: None,
			stages: Vec::new(),
			inspection: None,
			message: None,
			build_id: None,
			build_key: None,
			build_stage: None,
			build_completed: None,
			build_total: None,
			document_completed: None,
			document_total: None,
			cancel: None,
			install_note: None,
		}
	}
}

fn record_stage(book: &mut SetupBook, stage: &str, completed: Option<u64>, total: Option<u64>) {
	book.stage = Some(stage.to_string());
	book.completed = completed;
	book.total = total;
	if let Some(existing) = book.stages.iter_mut().find(|item| item.stage == stage) {
		existing.completed = completed;
		existing.total = total;
	} else {
		book.stages.push(SetupStageView {
			stage: stage.to_string(),
			completed,
			total,
		});
	}
}

fn build_key(spec: &BuildSpec) -> String {
	format!(
		"{}\n{}\n{}\n{}",
		spec.game_root.display(),
		spec.game_version,
		spec.rules_version,
		spec.input_identity
	)
}

fn input_identity(inspection: &InputInspection) -> String {
	let Some(playset) = inspection.playset.as_ref() else {
		return String::new();
	};
	let mut lines = Vec::with_capacity(playset.mods.len() + 1);
	lines.push(playset.source_path.clone());
	for playset_mod in &playset.mods {
		lines.push(format!(
			"{}:{}:{}",
			playset_mod.position, playset_mod.id, playset_mod.enabled
		));
	}
	lines.join("\n")
}

pub(crate) fn verify_game_path(path: &Path) -> Result<VerifiedGamePath, DesktopError> {
	if !path.is_dir() {
		return Err(DesktopError::new(
			"game_path_invalid",
			format!(
				"{} is not an EU4 installation directory. Nothing was written.",
				path.display()
			),
		));
	}
	let Some(version) = detect_game_version(path) else {
		return Err(DesktopError::new(
			"game_path_invalid",
			format!(
				"EU4 version was not found under {}. Expected launcher-settings.json or version.txt. Nothing was written.",
				path.display()
			),
		));
	};
	Ok(VerifiedGamePath {
		version,
		install_path: path.display().to_string(),
	})
}

pub(crate) fn save_game_path(path: &Path) -> Result<VerifiedGamePath, DesktopError> {
	let verified = verify_game_path(path)?;
	let config_dir = get_config_dir_path().map_err(|error| {
		DesktopError::new(
			"config_unavailable",
			format!("Foch could not open its configuration directory: {error}"),
		)
	})?;
	fs::create_dir_all(&config_dir).map_err(|error| {
		DesktopError::new(
			"config_unavailable",
			format!(
				"Foch could not create its configuration directory {}: {error}",
				config_dir.display()
			),
		)
	})?;
	let config_file = config_dir.join("config.toml");
	let mut config = if config_file.is_file() {
		Config::load_config(&config_file).map_err(|error| {
			DesktopError::new(
				"config_invalid",
				format!("Foch could not read {}: {error}", config_file.display()),
			)
		})?
	} else {
		Config::default()
	};
	config
		.game_path
		.insert(Eu4::KEY.to_string(), path.to_path_buf());
	config.save_config(&config_file).map_err(|error| {
		DesktopError::new(
			"config_unavailable",
			format!("Foch could not write {}: {error}", config_file.display()),
		)
	})?;
	Ok(verified)
}

fn install_note(inspection: &InputInspection) -> Option<InstallRecoveryNote> {
	if inspection.base_data.state == BaseDataState::Ready {
		return None;
	}
	let version = inspection
		.base_data
		.version
		.clone()
		.or_else(|| inspection.game.version.clone())
		.unwrap_or_else(|| "unknown".to_string());
	let rules_version = analysis_rules_version().to_string();
	let path = inspection
		.game
		.install_path
		.clone()
		.unwrap_or_else(|| "<EU4 installation directory>".to_string());
	Some(InstallRecoveryNote {
		version: version.clone(),
		rules_version: rules_version.clone(),
		reason: format!(
			"Remote `foch data install` is not available for EU4 {version} and rules {rules_version}. Releases do not ship a required base-data asset, so a remote EU4 data asset is not a prerequisite. Build base data from the installed game."
		),
		local_command: format!(
			"foch data build eu4 --from-game-path \"{path}\" --game-version {version} --install"
		),
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::backend::{
		AnalysisRunError, AnalysisRunner, FochAnalysisRunner, InspectedAnalysisInput,
	};
	use crate::dto::{
		BaseDataState, BaseDataView, DetectedPlaysetView, InstalledGameView, PlaysetModView,
		ReadinessState,
	};
	use foch::DATA_VERSION;
	use foch::game::eu4::base::snapshot::{
		BASE_DATA_DIR_ENV, INSTALLED_METADATA_FILE_NAME, INSTALLED_SNAPSHOT_FILE_NAME,
		InstalledBaseDataMetadata,
	};
	use foch::input::{CONFIG_DIR_ENV, InputRequest};
	use foch::merge::{CancellationToken, ProgressObserver};
	use std::ffi::OsString;
	use std::sync::atomic::AtomicUsize;
	use std::sync::mpsc::{self, Receiver, Sender};
	use std::time::{Duration, Instant, SystemTime};
	use tempfile::TempDir;

	const GAME_VERSION: &str = "1.37.5";
	const FIRST_ID: &str = "42";
	const SECOND_ID: &str = "43";

	fn blocked_inspection(path: &str) -> InputInspection {
		InputInspection {
			inspection_id: "scripted".to_string(),
			readiness: ReadinessState::Blocked,
			game: InstalledGameView {
				name: "Europa Universalis IV".to_string(),
				version: Some(GAME_VERSION.to_string()),
				install_path: Some(path.to_string()),
			},
			base_data: BaseDataView {
				state: BaseDataState::Missing,
				version: Some(GAME_VERSION.to_string()),
				detail: "Base data for EU4 1.37.5 is not installed.".to_string(),
			},
			playset: Some(DetectedPlaysetView {
				name: "Current EU4 playset".to_string(),
				source_path: "dlc_load.json".to_string(),
				mods: vec![PlaysetModView {
					id: FIRST_ID.to_string(),
					name: "First".to_string(),
					position: 1,
					enabled: true,
					workshop_id: Some(FIRST_ID.to_string()),
					workshop_manifest_id: None,
					version: None,
					declared_dependencies: Vec::new(),
					descriptor_path: None,
					source_error: Some("missing for this session".to_string()),
				}],
			}),
			issues: Vec::new(),
			recovery: None,
		}
	}

	struct ScriptedRunner {
		entered: Mutex<Option<Sender<()>>>,
		release: Mutex<Receiver<()>>,
		calls: AtomicUsize,
		inspection: InputInspection,
	}

	impl AnalysisRunner for ScriptedRunner {
		fn inspect_input(&self) -> InspectedAnalysisInput {
			self.calls.fetch_add(1, Ordering::Relaxed);
			if let Some(entered) = self.entered.lock().unwrap().take() {
				entered.send(()).expect("announce inspection");
			}
			self.release
				.lock()
				.unwrap()
				.recv()
				.expect("release inspection");
			InspectedAnalysisInput {
				inspection: self.inspection.clone(),
				prepared: None,
			}
		}

		fn run_analysis(
			&self,
			_analysis_id: &str,
			_request: InputRequest,
			_resolutions: &[crate::dto::DesktopResolutionRule],
			_progress: &dyn ProgressObserver,
			_cancellation: &CancellationToken,
		) -> Result<Arc<dyn crate::backend::AnalysisReview>, AnalysisRunError> {
			unreachable!("setup tests do not run analysis")
		}
	}

	struct IdleWork;

	impl SetupWork for IdleWork {
		fn build(
			&self,
			_spec: &BuildSpec,
			_on_progress: BuildProgressListener,
			_cancel: &Arc<AtomicBool>,
		) -> Result<(), String> {
			unreachable!("check tests do not build")
		}
	}

	struct ScriptedWork {
		entered: Mutex<Option<Sender<()>>>,
		release: Mutex<Receiver<()>>,
		calls: AtomicUsize,
		installs: AtomicUsize,
	}

	impl SetupWork for ScriptedWork {
		fn build(
			&self,
			_spec: &BuildSpec,
			_on_progress: BuildProgressListener,
			cancel: &Arc<AtomicBool>,
		) -> Result<(), String> {
			self.calls.fetch_add(1, Ordering::Relaxed);
			if let Some(entered) = self.entered.lock().unwrap().take() {
				entered.send(()).expect("announce build");
			}
			self.release.lock().unwrap().recv().expect("release build");
			if cancel.load(Ordering::Relaxed) {
				return Err("base data build cancelled".to_string());
			}
			self.installs.fetch_add(1, Ordering::Relaxed);
			Ok(())
		}
	}

	fn wait_until(timeout: Duration, mut ready: impl FnMut() -> bool) {
		let started = Instant::now();
		while started.elapsed() < timeout {
			if ready() {
				return;
			}
			thread::sleep(Duration::from_millis(20));
		}
		panic!("timed out after {timeout:?}");
	}

	#[test]
	fn check_reports_game_install_before_inspection_returns() {
		let (entered_tx, entered_rx) = mpsc::channel();
		let (release_tx, release_rx) = mpsc::channel();
		let runner = Arc::new(ScriptedRunner {
			entered: Mutex::new(Some(entered_tx)),
			release: Mutex::new(release_rx),
			calls: AtomicUsize::new(0),
			inspection: blocked_inspection("C:\\EU4"),
		});
		let desktop = DesktopState::with_setup(runner.clone(), Arc::new(IdleWork));
		let started = desktop.start_setup_check().expect("start check");
		assert_eq!(started.phase, SetupPhase::Checking);
		assert_eq!(started.stage.as_deref(), Some("game_install"));
		assert_eq!(started.completed, None);
		assert_eq!(started.total, None);
		assert!(started.stages.iter().all(|stage| stage.completed.is_none()));
		entered_rx
			.recv_timeout(Duration::from_secs(5))
			.expect("entered");
		let during = desktop.setup_status();
		assert_eq!(during.phase, SetupPhase::Checking);
		assert_eq!(during.total, None);
		let again = desktop.start_setup_check().expect("join in-flight check");
		assert_eq!(again.phase, SetupPhase::Checking);
		assert_eq!(runner.calls.load(Ordering::Relaxed), 1);
		let busy = desktop
			.start_base_data_build()
			.expect_err("build while checking");
		assert_eq!(busy.code, "setup_busy");
		release_tx.send(()).expect("release");
		wait_until(Duration::from_secs(5), || {
			desktop.setup_status().phase == SetupPhase::Checked
		});
		let done = desktop.setup_status();
		assert_eq!(
			done.inspection
				.as_ref()
				.map(|item| item.game.version.as_deref()),
			Some(Some(GAME_VERSION))
		);
		assert_eq!(runner.calls.load(Ordering::Relaxed), 1);
	}

	#[test]
	fn duplicate_build_joins_and_cancel_does_not_install() {
		let (release_inspect_tx, release_inspect_rx) = mpsc::channel();
		release_inspect_tx.send(()).expect("inspect does not block");
		let runner = Arc::new(ScriptedRunner {
			entered: Mutex::new(None),
			release: Mutex::new(release_inspect_rx),
			calls: AtomicUsize::new(0),
			inspection: blocked_inspection("C:\\EU4"),
		});
		let (entered_tx, entered_rx) = mpsc::channel();
		let (release_tx, release_rx) = mpsc::channel();
		let work = Arc::new(ScriptedWork {
			entered: Mutex::new(Some(entered_tx)),
			release: Mutex::new(release_rx),
			calls: AtomicUsize::new(0),
			installs: AtomicUsize::new(0),
		});
		let desktop = DesktopState::with_setup(runner, work.clone());
		desktop.start_setup_check().expect("check");
		wait_until(Duration::from_secs(5), || {
			desktop.setup_status().phase == SetupPhase::Checked
		});
		let started = desktop.start_base_data_build().expect("build");
		assert_eq!(started.phase, SetupPhase::Building);
		let build_id = started.build_id.clone().expect("build id");
		entered_rx
			.recv_timeout(Duration::from_secs(5))
			.expect("build entered");
		let joined = desktop.start_base_data_build().expect("second click joins");
		assert_eq!(joined.build_id.as_deref(), Some(build_id.as_str()));
		assert_eq!(work.calls.load(Ordering::Relaxed), 1);
		let busy = desktop
			.start_setup_check()
			.expect_err("check while building");
		assert_eq!(busy.code, "setup_busy");
		let cancelling = desktop.cancel_base_data_build(&build_id).expect("cancel");
		assert_eq!(cancelling.phase, SetupPhase::Building);
		assert!(
			cancelling
				.message
				.unwrap()
				.contains("will not be installed")
		);
		release_tx.send(()).expect("release build");
		wait_until(Duration::from_secs(5), || {
			desktop.setup_status().phase == SetupPhase::BuildCancelled
		});
		assert_eq!(work.installs.load(Ordering::Relaxed), 0);
		assert!(desktop.setup_status().inspection.is_some());
	}

	struct InstallThenSucceed {
		entered: Mutex<Option<Sender<()>>>,
		release: Mutex<Receiver<()>>,
		installs: AtomicUsize,
	}

	impl SetupWork for InstallThenSucceed {
		fn build(
			&self,
			_spec: &BuildSpec,
			_on_progress: BuildProgressListener,
			_cancel: &Arc<AtomicBool>,
		) -> Result<(), String> {
			if let Some(entered) = self.entered.lock().unwrap().take() {
				entered.send(()).expect("announce install");
			}
			self.release
				.lock()
				.unwrap()
				.recv()
				.expect("release install");
			self.installs.fetch_add(1, Ordering::Relaxed);
			Ok(())
		}
	}

	#[test]
	fn cancel_after_install_ok_still_rechecks() {
		let (release_inspect_tx, release_inspect_rx) = mpsc::channel();
		release_inspect_tx.send(()).expect("first inspect");
		let runner = Arc::new(ScriptedRunner {
			entered: Mutex::new(None),
			release: Mutex::new(release_inspect_rx),
			calls: AtomicUsize::new(0),
			inspection: blocked_inspection("C:\\EU4"),
		});
		let (entered_tx, entered_rx) = mpsc::channel();
		let (release_tx, release_rx) = mpsc::channel();
		let work = Arc::new(InstallThenSucceed {
			entered: Mutex::new(Some(entered_tx)),
			release: Mutex::new(release_rx),
			installs: AtomicUsize::new(0),
		});
		let desktop = DesktopState::with_setup(runner, work.clone());
		desktop.start_setup_check().expect("check");
		wait_until(Duration::from_secs(5), || {
			desktop.setup_status().phase == SetupPhase::Checked
		});
		let started = desktop.start_base_data_build().expect("build");
		let build_id = started.build_id.clone().expect("build id");
		entered_rx
			.recv_timeout(Duration::from_secs(5))
			.expect("install entered");
		desktop.cancel_base_data_build(&build_id).expect("cancel");
		release_inspect_tx.send(()).expect("recheck");
		release_tx.send(()).expect("install finished");
		wait_until(Duration::from_secs(5), || {
			matches!(
				desktop.setup_status().phase,
				SetupPhase::Checked | SetupPhase::Failed | SetupPhase::BuildCancelled
			)
		});
		let done = desktop.setup_status();
		assert_eq!(
			done.phase,
			SetupPhase::Checked,
			"a finished install must recheck, not report nothing installed: {:?}",
			done.message
		);
		assert_eq!(work.installs.load(Ordering::Relaxed), 1);
		assert!(done.message.is_none());
	}

	struct StageThenFail;

	impl SetupWork for StageThenFail {
		fn build(
			&self,
			_spec: &BuildSpec,
			on_progress: BuildProgressListener,
			_cancel: &Arc<AtomicBool>,
		) -> Result<(), String> {
			on_progress(BuildProgress {
				stage: "parse_documents".to_string(),
				completed: Some(1),
				total: Some(2),
			});
			Err("build failed before install".to_string())
		}
	}

	#[test]
	fn start_check_drops_stale_build_stages() {
		let (release_inspect_tx, release_inspect_rx) = mpsc::channel();
		release_inspect_tx.send(()).expect("first inspect");
		let runner = Arc::new(ScriptedRunner {
			entered: Mutex::new(None),
			release: Mutex::new(release_inspect_rx),
			calls: AtomicUsize::new(0),
			inspection: blocked_inspection("C:\\EU4"),
		});
		let desktop = DesktopState::with_setup(runner, Arc::new(StageThenFail));
		desktop.start_setup_check().expect("check");
		wait_until(Duration::from_secs(5), || {
			desktop.setup_status().phase == SetupPhase::Checked
		});
		desktop.start_base_data_build().expect("build");
		wait_until(Duration::from_secs(5), || {
			desktop.setup_status().phase == SetupPhase::BuildFailed
		});
		let failed = desktop.setup_status();
		assert!(
			failed
				.stages
				.iter()
				.any(|stage| stage.stage == "parse_documents"),
			"the failed build records parse_documents"
		);
		let restarted = desktop.start_setup_check().expect("recheck");
		assert_eq!(restarted.phase, SetupPhase::Checking);
		assert_eq!(
			restarted
				.stages
				.iter()
				.map(|stage| stage.stage.as_str())
				.collect::<Vec<_>>(),
			vec!["game_install"]
		);
		release_inspect_tx.send(()).expect("release recheck");
		wait_until(Duration::from_secs(5), || {
			desktop.setup_status().phase == SetupPhase::Checked
		});
		assert!(
			desktop
				.setup_status()
				.stages
				.iter()
				.all(|stage| stage.stage != "parse_documents")
		);
	}

	struct EnvVarGuard {
		key: &'static str,
		previous: Option<OsString>,
	}

	impl EnvVarGuard {
		fn set(key: &'static str, value: &Path) -> Self {
			let previous = std::env::var_os(key);
			// SAFETY: fixture tests hold SETUP_ENV_LOCK until this guard drops.
			unsafe { std::env::set_var(key, value) };
			Self { key, previous }
		}
	}

	impl Drop for EnvVarGuard {
		fn drop(&mut self) {
			// SAFETY: the fixture test still holds SETUP_ENV_LOCK.
			unsafe {
				match &self.previous {
					Some(value) => std::env::set_var(self.key, value),
					None => std::env::remove_var(self.key),
				}
			}
		}
	}

	static SETUP_ENV_LOCK: Mutex<()> = Mutex::new(());

	struct TreeEntry {
		path: PathBuf,
		bytes: Option<Vec<u8>>,
		modified: Option<SystemTime>,
	}

	fn tree_snapshot(root: &Path) -> Vec<TreeEntry> {
		let mut entries = Vec::new();
		let mut stack = vec![root.to_path_buf()];
		while let Some(dir) = stack.pop() {
			let Ok(read) = fs::read_dir(&dir) else {
				continue;
			};
			for entry in read.flatten() {
				let path = entry.path();
				let metadata = fs::symlink_metadata(&path).expect("metadata");
				let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
				if metadata.is_dir() {
					entries.push(TreeEntry {
						path: relative,
						bytes: None,
						modified: metadata.modified().ok(),
					});
					stack.push(path);
				} else {
					entries.push(TreeEntry {
						path: relative,
						bytes: Some(fs::read(&path).unwrap_or_default()),
						modified: metadata.modified().ok(),
					});
				}
			}
		}
		entries.sort_by(|left, right| left.path.cmp(&right.path));
		entries
	}

	fn assert_same_tree(before: &[TreeEntry], after: &[TreeEntry], label: &str) {
		assert_eq!(before.len(), after.len(), "{label} entry count changed");
		for (left, right) in before.iter().zip(after) {
			assert_eq!(left.path, right.path, "{label} path changed");
			assert_eq!(
				left.bytes,
				right.bytes,
				"{label} bytes changed at {}",
				left.path.display()
			);
			assert_eq!(
				left.modified,
				right.modified,
				"{label} mtime changed at {}",
				left.path.display()
			);
		}
	}

	fn write_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
		let steam_root = root.join("Steam");
		let steamapps = steam_root.join("steamapps");
		let game_root = steamapps.join("common").join(Eu4::PARADOX_DATA_DIR_NAME);
		fs::create_dir_all(&game_root).expect("game root");
		fs::write(game_root.join("version.txt"), format!("{GAME_VERSION}\n")).expect("version");
		fs::create_dir_all(game_root.join("events")).expect("events");
		fs::write(
			game_root.join("events").join("foch_setup.txt"),
			"namespace = foch_setup\n",
		)
		.expect("event");
		fs::write(
			game_root.join("events").join("foch_setup_extra.txt"),
			"namespace = foch_setup_extra\n",
		)
		.expect("second event");
		let escaped = steam_root.to_str().expect("utf-8").replace('\\', "\\\\");
		fs::write(
			steamapps.join("libraryfolders.vdf"),
			format!("\"libraryfolders\"\n{{\n\t\"0\" {{ \"path\" \"{escaped}\" }}\n}}\n"),
		)
		.expect("libraryfolders");
		fs::write(
			steamapps.join("appmanifest_236850.acf"),
			"\"AppState\"\n{\n\t\"appid\" \"236850\"\n\t\"installdir\" \"Europa Universalis IV\"\n}\n",
		)
		.expect("appmanifest");
		let workshop = steamapps.join("workshop");
		let content = workshop.join("content").join(Eu4::STEAM_APP_ID.to_string());
		fs::create_dir_all(content.join(FIRST_ID)).expect("first item");
		fs::create_dir_all(content.join(SECOND_ID)).expect("second item");
		fs::write(
			workshop.join("appworkshop_236850.acf"),
			format!(
				r#""AppWorkshop"
{{
	"appid" "236850"
	"WorkshopItemsInstalled"
	{{
		"{FIRST_ID}"
		{{
			"size" "10"
			"timeupdated" "1"
			"manifest" "100"
		}}
		"{SECOND_ID}"
		{{
			"size" "20"
			"timeupdated" "2"
			"manifest" "44"
		}}
	}}
}}"#
			),
		)
		.expect("acf");
		for (id, name) in [
			(FIRST_ID, "First Workshop Mod"),
			(SECOND_ID, "Second Workshop Mod"),
		] {
			fs::write(
				content.join(id).join("descriptor.mod"),
				format!("name=\"{name}\"\nversion=\"1.0\"\nremote_file_id=\"{id}\"\n"),
			)
			.expect("descriptor");
		}
		let paradox = root.join("Paradox EU4");
		fs::create_dir_all(paradox.join("mod")).expect("mod dir");
		fs::write(
			paradox.join("dlc_load.json"),
			format!(
				"{{\"enabled_mods\":[\"mod/ugc_{FIRST_ID}.mod\",\"mod/ugc_{SECOND_ID}.mod\"],\"disabled_dlcs\":[]}}"
			),
		)
		.expect("playset");
		for (id, name) in [(FIRST_ID, "Launcher First"), (SECOND_ID, "Launcher Second")] {
			fs::write(
				paradox.join("mod").join(format!("ugc_{id}.mod")),
				format!("name=\"{name}\"\nremote_file_id=\"{id}\"\n"),
			)
			.expect("launcher descriptor");
		}
		(game_root, workshop, paradox)
	}

	fn save_paths(config_dir: &Path, game: &Path, steam: &Path, paradox: &Path) {
		fs::create_dir_all(config_dir).expect("config dir");
		let mut config = Config::default();
		config
			.game_path
			.insert(Eu4::KEY.to_string(), game.to_path_buf());
		config.steam_root_path = Some(steam.to_path_buf());
		config.paradox_data_path = Some(paradox.to_path_buf());
		config
			.save_config(&config_dir.join("config.toml"))
			.expect("save config");
	}

	fn omit_second_workshop_item(workshop: &Path) {
		fs::write(
			workshop.join("appworkshop_236850.acf"),
			format!(
				r#""AppWorkshop"
{{
	"appid" "236850"
	"WorkshopItemsInstalled"
	{{
		"{FIRST_ID}"
		{{
			"size" "10"
			"timeupdated" "1"
			"manifest" "100"
		}}
	}}
}}"#
			),
		)
		.expect("drop second item");
	}

	// Fields drop in declaration order. Restore the process environment and
	// delete the temp tree before releasing SETUP_ENV_LOCK. Releasing the lock
	// first lets the next fixture set FOCH_CONFIG_DIR, then this guard's Drop
	// writes the previous value back over it and the check reads the real EU4.
	struct Fixture {
		_config_guard: EnvVarGuard,
		_data_guard: EnvVarGuard,
		game_root: PathBuf,
		workshop: PathBuf,
		paradox: PathBuf,
		config_dir: PathBuf,
		data_dir: PathBuf,
		desktop: DesktopState,
		_temp: TempDir,
		_lock: std::sync::MutexGuard<'static, ()>,
	}

	fn open_fixture() -> Fixture {
		let lock = SETUP_ENV_LOCK
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let temp = TempDir::new().expect("temp");
		let config_dir = temp.path().join("config");
		let data_dir = temp.path().join("data");
		fs::create_dir_all(&data_dir).expect("data dir");
		let config_guard = EnvVarGuard::set(CONFIG_DIR_ENV, &config_dir);
		let data_guard = EnvVarGuard::set(BASE_DATA_DIR_ENV, &data_dir);
		let (game_root, workshop, paradox) = write_fixture(temp.path());
		save_paths(
			&config_dir,
			&game_root,
			&temp.path().join("Steam"),
			&paradox,
		);
		let desktop = DesktopState::new(Arc::new(FochAnalysisRunner));
		Fixture {
			_config_guard: config_guard,
			_data_guard: data_guard,
			game_root,
			workshop,
			paradox,
			config_dir,
			data_dir,
			desktop,
			_temp: temp,
			_lock: lock,
		}
	}

	fn wait_checked(desktop: &DesktopState) -> SetupStatus {
		desktop.start_setup_check().expect("start");
		wait_until(Duration::from_secs(30), || {
			matches!(
				desktop.setup_status().phase,
				SetupPhase::Checked | SetupPhase::Failed
			)
		});
		let status = desktop.setup_status();
		assert_eq!(
			status.phase,
			SetupPhase::Checked,
			"check failed: {:?}",
			status.message
		);
		status
	}

	#[test]
	fn missing_base_data_reports_real_stages_and_the_missing_mod() {
		let fixture = open_fixture();
		omit_second_workshop_item(&fixture.workshop);
		let before_game = tree_snapshot(&fixture.game_root);
		let before_workshop = tree_snapshot(&fixture.workshop);
		let before_launcher = tree_snapshot(&fixture.paradox);
		let status = wait_checked(&fixture.desktop);
		let inspection = status.inspection.expect("inspection");
		assert_eq!(inspection.game.version.as_deref(), Some(GAME_VERSION));
		assert_eq!(
			inspection.game.install_path.as_deref(),
			Some(fixture.game_root.to_str().unwrap())
		);
		assert_eq!(inspection.base_data.state, BaseDataState::Missing);
		assert!(inspection.recovery.is_none());
		let missing = inspection
			.playset
			.expect("playset")
			.mods
			.into_iter()
			.find(|item| item.id == SECOND_ID)
			.expect("second mod");
		assert!(
			missing
				.source_error
				.unwrap()
				.contains("absent from appworkshop_236850.acf")
		);
		let names: Vec<&str> = status
			.stages
			.iter()
			.map(|stage| stage.stage.as_str())
			.collect();
		assert_eq!(
			names,
			vec![
				"game_install",
				"launcher_input",
				"workshop_acf",
				"descriptors",
				"base_data"
			]
		);
		let stage = |name: &str| {
			status
				.stages
				.iter()
				.find(|stage| stage.stage == name)
				.unwrap()
		};
		assert_eq!(stage("launcher_input").total, None);
		assert_eq!(stage("workshop_acf").total, None);
		assert_eq!(stage("game_install").total, None);
		assert_eq!(stage("base_data").total, None);
		assert_eq!(stage("descriptors").completed, Some(2));
		assert_eq!(stage("descriptors").total, Some(2));
		let note = status.install_note.expect("install note");
		assert_eq!(note.version, GAME_VERSION);
		assert_eq!(note.rules_version, analysis_rules_version());
		assert!(note.reason.contains("not a prerequisite"));
		assert!(note.local_command.contains("foch data build eu4"));
		assert!(note.local_command.contains("--install"));
		assert!(note.reason.contains(&note.rules_version));
		assert_same_tree(&before_game, &tree_snapshot(&fixture.game_root), "game");
		assert_same_tree(
			&before_workshop,
			&tree_snapshot(&fixture.workshop),
			"workshop",
		);
		assert_same_tree(
			&before_launcher,
			&tree_snapshot(&fixture.paradox),
			"launcher",
		);
	}

	#[test]
	fn stale_rules_explain_the_local_build_recovery() {
		let fixture = open_fixture();
		let install_dir = installed_data_dir(Eu4::KEY, GAME_VERSION);
		fs::create_dir_all(&install_dir).expect("install dir");
		let metadata = InstalledBaseDataMetadata {
			snapshot_format_version: DATA_VERSION.to_string(),
			game: Eu4::KEY.to_string(),
			game_version: GAME_VERSION.to_string(),
			analysis_rules_version: "rules-stale".to_string(),
			generated_by_cli_version: "test".to_string(),
			source: BaseDataSource::Build,
			asset_name: None,
			sha256: None,
			vocabulary_manifest_sha256: None,
		};
		fs::write(
			install_dir.join(INSTALLED_METADATA_FILE_NAME),
			serde_json::to_string_pretty(&metadata).expect("metadata"),
		)
		.expect("write metadata");
		let status = wait_checked(&fixture.desktop);
		let inspection = status.inspection.expect("inspection");
		assert_eq!(inspection.base_data.state, BaseDataState::Stale);
		let note = status.install_note.expect("note");
		assert_eq!(note.version, GAME_VERSION);
		assert_eq!(note.rules_version, analysis_rules_version());
		assert!(
			note.reason.contains("rules-stale")
				|| inspection.base_data.detail.contains("rules-stale")
		);
		assert!(note.reason.contains("not a prerequisite"));
		assert!(
			note.local_command
				.contains(&fixture.game_root.display().to_string())
		);
		assert!(note.local_command.contains(GAME_VERSION));
		assert!(
			!fixture
				.data_dir
				.join("eu4")
				.join(GAME_VERSION)
				.join(INSTALLED_SNAPSHOT_FILE_NAME)
				.exists()
		);
	}

	#[test]
	fn wrong_path_can_be_verified_and_saved_without_touching_the_game() {
		let fixture = open_fixture();
		let empty = fixture._temp.path().join("empty-eu4");
		fs::create_dir_all(&empty).expect("empty");
		save_paths(
			&fixture.config_dir,
			&empty,
			&fixture._temp.path().join("Steam"),
			&fixture.paradox,
		);
		let before_game = tree_snapshot(&fixture.game_root);
		let before_workshop = tree_snapshot(&fixture.workshop);
		let before_launcher = tree_snapshot(&fixture.paradox);
		let status = wait_checked(&fixture.desktop);
		let inspection = status.inspection.expect("inspection");
		assert!(inspection.game.install_path.is_none());
		assert!(inspection.game.version.is_none());
		assert!(
			inspection
				.issues
				.iter()
				.any(|issue| issue.id == "eu4_install_unavailable")
		);
		let rejected = verify_game_path(&empty).expect_err("empty install");
		assert_eq!(rejected.code, "game_path_invalid");
		let config_after_reject = fs::read(fixture.config_dir.join("config.toml")).expect("config");
		let verified = verify_game_path(&fixture.game_root).expect("real install");
		assert_eq!(verified.version, GAME_VERSION);
		assert_eq!(
			fs::read(fixture.config_dir.join("config.toml")).expect("config"),
			config_after_reject,
			"verify must not write configuration"
		);
		let saved = save_game_path(&fixture.game_root).expect("save");
		assert_eq!(saved.version, GAME_VERSION);
		let config = Config::load_config(&fixture.config_dir.join("config.toml")).expect("reload");
		assert_eq!(
			config.game_path.get(Eu4::KEY).map(PathBuf::as_path),
			Some(fixture.game_root.as_path())
		);
		assert_eq!(
			config.steam_root_path.as_deref(),
			Some(fixture._temp.path().join("Steam").as_path())
		);
		let again = wait_checked(&fixture.desktop);
		let inspection = again.inspection.expect("recheck");
		assert_eq!(inspection.game.version.as_deref(), Some(GAME_VERSION));
		assert_eq!(
			inspection.game.install_path.as_deref(),
			Some(fixture.game_root.to_str().unwrap())
		);
		assert_same_tree(&before_game, &tree_snapshot(&fixture.game_root), "game");
		assert_same_tree(
			&before_workshop,
			&tree_snapshot(&fixture.workshop),
			"workshop",
		);
		assert_same_tree(
			&before_launcher,
			&tree_snapshot(&fixture.paradox),
			"launcher",
		);
	}

	#[test]
	fn build_from_the_installed_game_installs_then_rechecks() {
		let fixture = open_fixture();
		let before_game = tree_snapshot(&fixture.game_root);
		let before_workshop = tree_snapshot(&fixture.workshop);
		let before_launcher = tree_snapshot(&fixture.paradox);
		let status = wait_checked(&fixture.desktop);
		assert_eq!(
			status.inspection.unwrap().base_data.state,
			BaseDataState::Missing
		);
		fixture.desktop.start_base_data_build().expect("build");
		wait_until(Duration::from_secs(180), || {
			let phase = fixture.desktop.setup_status().phase;
			matches!(
				phase,
				SetupPhase::Checked | SetupPhase::BuildFailed | SetupPhase::BuildCancelled
			)
		});
		let done = fixture.desktop.setup_status();
		assert_eq!(
			done.phase,
			SetupPhase::Checked,
			"build did not recheck: {:?}",
			done.message
		);
		let inspection = done.inspection.expect("inspection");
		assert_eq!(inspection.base_data.state, BaseDataState::Ready);
		assert!(done.install_note.is_none());
		assert!(
			done.stages.iter().any(|stage| stage.stage == "descriptors"),
			"the recheck reports playset descriptor progress on its own stages"
		);
		assert!(
			done.stages
				.iter()
				.all(|stage| stage.stage != "parse_documents"),
			"document parse progress is not stored as a playset stage"
		);
		assert_eq!(done.document_completed, done.document_total);
		assert!(
			done.document_total.unwrap_or(0) > 0,
			"parse must report a real document total"
		);
		assert!(
			fixture
				.data_dir
				.join("eu4")
				.join(GAME_VERSION)
				.join(INSTALLED_SNAPSHOT_FILE_NAME)
				.is_file() || installed_data_dir(Eu4::KEY, GAME_VERSION)
				.join(INSTALLED_SNAPSHOT_FILE_NAME)
				.is_file()
		);
		assert_same_tree(&before_game, &tree_snapshot(&fixture.game_root), "game");
		assert_same_tree(
			&before_workshop,
			&tree_snapshot(&fixture.workshop),
			"workshop",
		);
		assert_same_tree(
			&before_launcher,
			&tree_snapshot(&fixture.paradox),
			"launcher",
		);
	}

	#[test]
	fn cancelling_during_detect_version_does_not_install() {
		let _lock = SETUP_ENV_LOCK
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let temp = TempDir::new().expect("temp");
		let _data = EnvVarGuard::set(BASE_DATA_DIR_ENV, &temp.path().join("data"));
		let game = temp.path().join("game");
		fs::create_dir_all(&game).expect("game");
		let cancel = Arc::new(AtomicBool::new(false));
		let flag = Arc::clone(&cancel);
		let progress: Arc<dyn Fn(BuildProgress) + Send + Sync> = Arc::new(move |progress| {
			if progress.stage == "detect_version" {
				flag.store(true, Ordering::Relaxed);
			}
		});
		let error = FochSetupWork
			.build(
				&BuildSpec {
					game_root: game,
					game_version: GAME_VERSION.to_string(),
					rules_version: analysis_rules_version().to_string(),
					input_identity: "playset".to_string(),
				},
				progress,
				&cancel,
			)
			.expect_err("cancelled build");
		assert!(error.contains("cancelled"));
		assert!(
			!installed_data_dir(Eu4::KEY, GAME_VERSION)
				.join(INSTALLED_SNAPSHOT_FILE_NAME)
				.exists()
		);
	}
}
