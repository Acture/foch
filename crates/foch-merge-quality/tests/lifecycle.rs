#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use foch_core::domain::game::Game;
use foch_core::model::{MergeReport, MergeReportStatus, ProductInputManifest};
use foch_engine::{BaseDataSource, FileFilter, build_base_snapshot, install_built_snapshot};
use foch_merge_quality::config::{Eu4Discovery, WorkshopCatalog};
use foch_merge_quality::dataset::{
	DatasetPaths, EngineArtifactIdentity, FileResultRecord, InputVersionRecord, MeasurementKernel,
	MeasurementRecord, MeasurementScope, TerminalStatus, WorkshopObservationRecord, read_jsonl,
};
use foch_merge_quality::lifecycle::{
	MeasurementRequest, MeasurementRunner, MeasurementRunnerIdentity, TerminalMerge,
	WorkshopMeasureOptions, WorkshopReportOptions, measure_workshop_with_runner, report_workshop,
};
use foch_merge_quality::report::WorkshopReportCohort;
use foch_merge_quality::workshop_inputs::WorkshopCaseManifest;

static BASE_DATA_ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
	key: &'static str,
	previous: Option<OsString>,
}

struct WorkshopContentPoison {
	permissions: Vec<(PathBuf, fs::Permissions)>,
}

impl WorkshopContentPoison {
	fn new(workshop_root: &Path) -> Self {
		let mut permissions = Vec::new();
		for entry in fs::read_dir(workshop_root).unwrap() {
			let entry = entry.unwrap();
			if !entry.file_type().unwrap().is_dir() {
				continue;
			}
			let path = entry.path().join("decisions");
			if !path.is_dir() {
				continue;
			}
			let original = fs::metadata(&path).unwrap().permissions();
			let mut poisoned = original.clone();
			poisoned.set_mode(0o000);
			fs::set_permissions(&path, poisoned).unwrap();
			permissions.push((path, original));
		}
		Self { permissions }
	}
}

impl Drop for WorkshopContentPoison {
	fn drop(&mut self) {
		for (path, permissions) in &self.permissions {
			let _ = fs::set_permissions(path, permissions.clone());
		}
	}
}

impl EnvGuard {
	fn set(key: &'static str, value: &Path) -> Self {
		let previous = std::env::var_os(key);
		unsafe {
			std::env::set_var(key, value);
		}
		Self { key, previous }
	}
}

impl Drop for EnvGuard {
	fn drop(&mut self) {
		unsafe {
			if let Some(previous) = &self.previous {
				std::env::set_var(self.key, previous);
			} else {
				std::env::remove_var(self.key);
			}
		}
	}
}

fn write_decision_mod(root: &Path, content: &str) {
	fs::create_dir_all(root.join("decisions")).unwrap();
	fs::write(root.join("descriptor.mod"), "name=\"fixture\"\n").unwrap();
	fs::write(root.join("decisions/example.txt"), content).unwrap();
}

fn write_workshop_acf(path: &Path, workshop_ids: impl IntoIterator<Item = String>) {
	let installed = workshop_ids
		.into_iter()
		.map(|workshop_id| {
			format!(
				r#"		"{workshop_id}"
		{{
			"size" "1"
			"timeupdated" "1780000000"
			"manifest" "{workshop_id}0001"
		}}"#
			)
		})
		.collect::<Vec<_>>()
		.join("\n");
	fs::write(
		path,
		format!(
			r#""AppWorkshop"
{{
	"appid" "236850"
	"WorkshopItemsInstalled"
	{{
{installed}
	}}
}}"#
		),
	)
	.unwrap();
}

fn vdf_path_value(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "\\\\")
}

fn install_test_base_data(game_root: &Path, data_root: &Path) -> EnvGuard {
	let guard = EnvGuard::set("FOCH_DATA_DIR", data_root);
	let game = Game::EuropaUniversalis4;
	let filter = FileFilter::for_game(game.clone());
	let built = build_base_snapshot(&game, game_root, Some("1.37.5"), &filter).unwrap();
	install_built_snapshot(
		&built.encoded_snapshot,
		BaseDataSource::Build,
		Some(built.snapshot_asset_name),
		Some(built.snapshot_sha256),
	)
	.unwrap();
	guard
}

struct LiveWorkshopFixture {
	game: PathBuf,
	game_appmanifest: PathBuf,
	dataset: PathBuf,
	workshop: PathBuf,
	workshop_acf: PathBuf,
	case_manifest: PathBuf,
	discovery: Eu4Discovery,
}

fn fixed_workshop_case_manifest_path() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
		.join("workshop-product-cases-v2.json")
}

fn live_workshop_fixture(
	temp: &tempfile::TempDir,
	dataset_name: &str,
) -> (LiveWorkshopFixture, EnvGuard) {
	let steam = temp.path().join(format!("{dataset_name}-Steam"));
	let library = temp.path().join(format!("{dataset_name}-Library"));
	let game = library.join("steamapps/common/Europa Universalis IV");
	let game_appmanifest = library.join("steamapps/appmanifest_236850.acf");
	let base_data = temp.path().join(format!("{dataset_name}-base-data"));
	let workshop_root = library.join("steamapps/workshop");
	let workshop = workshop_root.join("content/236850");
	let workshop_acf = workshop_root.join("appworkshop_236850.acf");
	let dataset = temp.path().join(dataset_name);
	let fixed_case_manifest = fixed_workshop_case_manifest_path();
	let manifest = WorkshopCaseManifest::from_path(&fixed_case_manifest).unwrap();
	assert_eq!(manifest.cases.len(), 14);
	let case_manifest = temp.path().join(format!("{dataset_name}-cases.json"));
	fs::write(
		&case_manifest,
		serde_json::to_vec_pretty(&manifest).unwrap(),
	)
	.unwrap();

	fs::create_dir_all(steam.join("steamapps")).unwrap();
	fs::create_dir_all(game.join("decisions")).unwrap();
	fs::write(
		steam.join("steamapps/libraryfolders.vdf"),
		format!(
			"\"libraryfolders\"\n{{\n\t\"0\" {{ \"path\" \"{}\" }}\n\t\"1\" {{ \"path\" \"{}\" }}\n}}",
			vdf_path_value(&steam),
			vdf_path_value(&library)
		),
	)
	.unwrap();
	fs::write(
		&game_appmanifest,
		r#""AppState"
{
	"appid" "236850"
	"installdir" "Europa Universalis IV"
	"buildid" "4242"
}"#,
	)
	.unwrap();
	fs::write(game.join("version.txt"), "1.37.5\n").unwrap();
	fs::write(
		game.join("decisions/example.txt"),
		"country_decisions = { shared = { potential = { tag = FRA } allow = { stability = 1 } effect = { add_prestige = 1 } } }\n",
	)
	.unwrap();
	let env_guard = install_test_base_data(&game, &base_data);

	let item_ids = manifest
		.required_item_ids()
		.into_iter()
		.map(str::to_string)
		.collect::<Vec<_>>();
	for (index, workshop_id) in item_ids.iter().enumerate() {
		write_decision_mod(
			&workshop.join(workshop_id),
			&format!(
				"country_decisions = {{ shared = {{ potential = {{ tag = FRA }} allow = {{ stability = 1 }} effect = {{ add_prestige = {} }} }} }}\n",
				(index % 9) + 1
			),
		);
	}
	write_workshop_acf(&workshop_acf, item_ids);
	let discovery = Eu4Discovery {
		game_root: game.clone(),
		game_version: "1.37.5".to_string(),
		steam_build_id: Some(4242),
		steam_root: Some(steam),
		workshop: WorkshopCatalog::from_override(236850, workshop.clone(), workshop_acf.clone())
			.unwrap(),
	};
	(
		LiveWorkshopFixture {
			game,
			game_appmanifest,
			dataset,
			workshop,
			workshop_acf,
			case_manifest,
			discovery,
		},
		env_guard,
	)
}

fn workshop_tree_snapshot(root: &Path) -> BTreeMap<String, (bool, Vec<u8>)> {
	walkdir::WalkDir::new(root)
		.into_iter()
		.map(|entry| entry.unwrap())
		.filter(|entry| entry.path() != root)
		.map(|entry| {
			let relative = entry
				.path()
				.strip_prefix(root)
				.unwrap()
				.to_string_lossy()
				.replace('\\', "/");
			let is_dir = entry.file_type().is_dir();
			let bytes = if entry.file_type().is_file() {
				fs::read(entry.path()).unwrap()
			} else {
				Vec::new()
			};
			(relative, (is_dir, bytes))
		})
		.collect()
}

struct PoisonedLegacyPaths {
	paths: Vec<PathBuf>,
}

impl PoisonedLegacyPaths {
	fn new(paths: [&Path; 2]) -> Self {
		use std::os::unix::fs::PermissionsExt;

		let paths = paths.into_iter().map(Path::to_path_buf).collect::<Vec<_>>();
		for path in &paths {
			fs::write(path, b"legacy path must remain untouched").unwrap();
			fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
		}
		Self { paths }
	}

	fn assert_untouched(mut self) {
		self.restore();
		for path in &self.paths {
			assert_eq!(
				fs::read(path).unwrap(),
				b"legacy path must remain untouched"
			);
			assert!(path.is_file());
		}
		self.paths.clear();
	}

	fn restore(&self) {
		use std::os::unix::fs::PermissionsExt;

		for path in &self.paths {
			if let Ok(metadata) = fs::symlink_metadata(path) {
				let _ = fs::set_permissions(
					path,
					fs::Permissions::from_mode(metadata.permissions().mode() | 0o600),
				);
			}
		}
	}
}

impl Drop for PoisonedLegacyPaths {
	fn drop(&mut self) {
		self.restore();
	}
}

fn runner_identity() -> MeasurementRunnerIdentity {
	MeasurementRunnerIdentity {
		engine_artifact: EngineArtifactIdentity::foch_executable_blake3("a".repeat(64)),
		runner_protocol_version: "fake-product-runner-v1".to_string(),
		merge_kernel: MeasurementKernel::SemanticTree,
		scope: MeasurementScope::FullProductMerge,
	}
}

struct FakeRunner {
	identity: MeasurementRunnerIdentity,
	outcomes: VecDeque<TerminalMerge>,
	calls: Vec<MeasurementRequest>,
}

impl FakeRunner {
	fn new(outcomes: impl IntoIterator<Item = TerminalMerge>) -> Self {
		Self {
			identity: runner_identity(),
			outcomes: outcomes.into_iter().collect(),
			calls: Vec::new(),
		}
	}
}

impl MeasurementRunner for FakeRunner {
	fn identity(&self) -> &MeasurementRunnerIdentity {
		&self.identity
	}

	fn preflight(&self) -> Result<(), String> {
		Ok(())
	}

	fn run(&mut self, request: &MeasurementRequest) -> TerminalMerge {
		self.calls.push(request.clone());
		let mut outcome = self
			.outcomes
			.pop_front()
			.expect("fake runner received an unexpected request");
		if let TerminalMerge::Completed { report, .. } = &mut outcome {
			report.input = Some(
				request
					.source_manifest
					.as_ref()
					.expect("Workshop request must carry ordered ACF identities")
					.attestation(),
			);
		}
		if matches!(
			&outcome,
			TerminalMerge::Completed { report, .. }
				if report.status != MergeReportStatus::Fatal
		) {
			let relative = Path::new("decisions/example.txt");
			fs::create_dir_all(request.output_dir.join("decisions")).unwrap();
			fs::copy(
				request.compatch_dir.join(relative),
				request.output_dir.join(relative),
			)
			.unwrap();
		}
		outcome
	}
}

struct MaterializingCompatchRunner {
	inner: FakeRunner,
	content: Option<String>,
}

impl MeasurementRunner for MaterializingCompatchRunner {
	fn identity(&self) -> &MeasurementRunnerIdentity {
		self.inner.identity()
	}

	fn preflight(&self) -> Result<(), String> {
		self.inner.preflight()
	}

	fn run(&mut self, request: &MeasurementRequest) -> TerminalMerge {
		write_decision_mod(
			&request.compatch_dir,
			&self
				.content
				.take()
				.expect("compatch content is materialized exactly once"),
		);
		self.inner.run(request)
	}
}

enum WorkshopMutation {
	AcfManifest { path: PathBuf, workshop_id: String },
	SourceContent,
	ReportInput,
}

struct MutatingWorkshopRunner {
	inner: FakeRunner,
	mutation: Option<WorkshopMutation>,
}

impl MeasurementRunner for MutatingWorkshopRunner {
	fn identity(&self) -> &MeasurementRunnerIdentity {
		self.inner.identity()
	}

	fn preflight(&self) -> Result<(), String> {
		self.inner.preflight()
	}

	fn run(&mut self, request: &MeasurementRequest) -> TerminalMerge {
		let mut terminal = self.inner.run(request);
		match self.mutation.take().expect("mutation runs exactly once") {
			WorkshopMutation::AcfManifest { path, workshop_id } => {
				let before = fs::read_to_string(&path).unwrap();
				let needle = format!("\"manifest\" \"{workshop_id}0001\"");
				let replacement = format!("\"manifest\" \"{workshop_id}0002\"");
				let after = before.replacen(&needle, &replacement, 1);
				assert_ne!(before, after, "fixture ACF manifest entry must exist");
				fs::write(path, after).unwrap();
			}
			WorkshopMutation::SourceContent => {
				write_decision_mod(
					&request.source_dirs[0],
					"country_decisions = { shared = { potential = { tag = ENG } allow = { stability = 2 } effect = { add_prestige = 9 } } }\n",
				);
			}
			WorkshopMutation::ReportInput => {
				let TerminalMerge::Completed { report, .. } = &mut terminal else {
					panic!("report-input mutation requires a completed merge");
				};
				report.input = Some(ProductInputManifest::new(Vec::new()).attestation());
			}
		}
		terminal
	}
}

fn completed(status: MergeReportStatus, merge_ms: u64) -> TerminalMerge {
	TerminalMerge::Completed {
		report: Box::new(MergeReport {
			status,
			..MergeReport::default()
		}),
		merge_ms,
	}
}

fn completed_outcomes(count: usize) -> Vec<TerminalMerge> {
	(0..count)
		.map(|_| completed(MergeReportStatus::Ready, 3))
		.collect()
}

#[test]
fn workshop_scorer_walk_starts_after_the_product_runner() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = live_workshop_fixture(&temp, "workshop-scorer-order");
	let mut manifest = WorkshopCaseManifest::from_path(&fixture.case_manifest).unwrap();
	manifest.cases.truncate(1);
	fs::write(
		&fixture.case_manifest,
		serde_json::to_vec_pretty(&manifest).unwrap(),
	)
	.unwrap();
	let compatch = fixture
		.workshop
		.join(&manifest.cases[0].compatch_workshop_id);
	let decision = compatch.join("decisions/example.txt");
	let content = fs::read_to_string(&decision).unwrap();
	fs::remove_file(decision).unwrap();
	let mut runner = MaterializingCompatchRunner {
		inner: FakeRunner::new([completed(MergeReportStatus::Ready, 3)]),
		content: Some(content),
	};

	let summary = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &fixture.case_manifest,
			dataset_root: &fixture.dataset,
			discovery: &fixture.discovery,
			timeout: Duration::from_secs(30),
			basegame_root: &fixture.game,
		},
		&mut runner,
	)
	.unwrap();

	assert_eq!(
		(summary.selected, summary.measured, summary.failed),
		(1, 1, 0)
	);
	assert_eq!(runner.inner.calls.len(), 1);
}

#[test]
fn workshop_live_measurement_is_cas_free_and_resumes_from_compact_evidence() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = live_workshop_fixture(&temp, "workshop-live");
	let paths = DatasetPaths::new(&fixture.dataset);
	let workshop_before = workshop_tree_snapshot(&fixture.workshop);
	let acf_before = fs::read(&fixture.workshop_acf).unwrap();
	assert!(!paths.legacy_objects.exists());
	assert!(!paths.legacy_work.exists());

	let mut runner = FakeRunner::new(completed_outcomes(14));
	let first = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &fixture.case_manifest,
			dataset_root: &fixture.dataset,
			discovery: &fixture.discovery,
			timeout: Duration::from_secs(30),
			basegame_root: &fixture.game,
		},
		&mut runner,
	)
	.unwrap();

	assert_eq!((first.selected, first.cached, first.measured), (14, 0, 14));
	assert_eq!(first.failed, 0);
	assert_eq!(first.input_version_ids.len(), 14);
	assert_eq!(runner.calls.len(), 14);
	assert!(runner.calls.iter().all(|request| {
		request
			.source_manifest
			.as_ref()
			.is_some_and(ProductInputManifest::digest_is_valid)
	}));
	assert!(!paths.legacy_objects.exists());
	assert!(!paths.legacy_work.exists());
	assert!(!paths.object_records.exists());
	assert!(!paths.snapshots.exists());
	assert_eq!(workshop_tree_snapshot(&fixture.workshop), workshop_before);
	assert_eq!(fs::read(&fixture.workshop_acf).unwrap(), acf_before);

	let input_versions = read_jsonl::<InputVersionRecord>(&paths.input_versions).unwrap();
	assert_eq!(input_versions.len(), 14);
	assert!(
		input_versions
			.iter()
			.all(InputVersionRecord::identity_is_valid)
	);
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements).unwrap();
	assert_eq!(measurements.len(), 14);
	assert!(measurements.iter().all(|record| {
		matches!(record, MeasurementRecord::V2 { .. })
			&& record.status() == TerminalStatus::Completed
			&& record.evidence_bundle_hash().is_some()
	}));
	let observations = read_jsonl::<WorkshopObservationRecord>(&paths.observations).unwrap();
	assert_eq!(observations.len(), 14);
	assert!(
		observations
			.iter()
			.all(|record| matches!(record, WorkshopObservationRecord::V2(_)))
	);
	assert_eq!(
		read_jsonl::<FileResultRecord>(&paths.file_results)
			.unwrap()
			.len(),
		14
	);
	assert!(paths.evidence_objects.is_dir());
	assert_eq!(fs::read_dir(&paths.evidence_work).unwrap().count(), 0);
	let workshop_content_poison = WorkshopContentPoison::new(&fixture.workshop);
	let report_dir = temp.path().join("workshop-live-report");
	report_workshop(&WorkshopReportOptions {
		case_manifest: &fixture.case_manifest,
		dataset_root: &fixture.dataset,
		discovery: &fixture.discovery,
		output_dir: &report_dir,
		cohort_id: &first.cohort_id,
		cohort: WorkshopReportCohort::AllCandidates,
	})
	.unwrap();
	assert!(report_dir.join("baseline.json").is_file());

	let poison = PoisonedLegacyPaths::new([&paths.legacy_objects, &paths.legacy_work]);
	let mut cached_runner = FakeRunner::new([]);
	let cached = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &fixture.case_manifest,
			dataset_root: &fixture.dataset,
			discovery: &fixture.discovery,
			timeout: Duration::from_secs(30),
			basegame_root: &fixture.game,
		},
		&mut cached_runner,
	)
	.unwrap();
	assert_eq!(
		(cached.selected, cached.cached, cached.measured),
		(14, 14, 0)
	);
	assert_eq!(cached.input_version_ids, first.input_version_ids);
	assert!(cached_runner.calls.is_empty());
	drop(workshop_content_poison);

	let mut remaining_measurements = Vec::new();
	for measurement in measurements.iter().skip(1) {
		serde_json::to_writer(&mut remaining_measurements, measurement).unwrap();
		remaining_measurements.push(b'\n');
	}
	fs::write(&paths.measurements, remaining_measurements).unwrap();
	let mut recovery_runner = FakeRunner::new([completed(MergeReportStatus::Ready, 3)]);
	let recovered = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &fixture.case_manifest,
			dataset_root: &fixture.dataset,
			discovery: &fixture.discovery,
			timeout: Duration::from_secs(30),
			basegame_root: &fixture.game,
		},
		&mut recovery_runner,
	)
	.unwrap();
	assert_eq!(
		(recovered.selected, recovered.cached, recovered.measured),
		(14, 13, 1)
	);
	assert_eq!(recovery_runner.calls.len(), 1);
	assert_eq!(
		read_jsonl::<MeasurementRecord>(&paths.measurements)
			.unwrap()
			.len(),
		14
	);
	poison.assert_untouched();
	assert_eq!(workshop_tree_snapshot(&fixture.workshop), workshop_before);
	assert_eq!(fs::read(&fixture.workshop_acf).unwrap(), acf_before);
}

#[test]
fn workshop_live_measurement_rejects_acf_drift_without_terminal_record() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = live_workshop_fixture(&temp, "workshop-acf-drift");
	let manifest = WorkshopCaseManifest::from_path(&fixture.case_manifest).unwrap();
	let first_case = &manifest.cases[0];
	let mut runner = MutatingWorkshopRunner {
		inner: FakeRunner::new([completed(MergeReportStatus::Ready, 3)]),
		mutation: Some(WorkshopMutation::AcfManifest {
			path: fixture.workshop_acf.clone(),
			workshop_id: first_case.compatch_workshop_id.clone(),
		}),
	};

	let error = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &fixture.case_manifest,
			dataset_root: &fixture.dataset,
			discovery: &fixture.discovery,
			timeout: Duration::from_secs(30),
			basegame_root: &fixture.game,
		},
		&mut runner,
	)
	.unwrap_err();

	assert!(error.to_string().contains("ACF changed"), "{error}");
	assert_eq!(runner.inner.calls.len(), 1);
	let paths = DatasetPaths::new(&fixture.dataset);
	assert!(
		read_jsonl::<MeasurementRecord>(&paths.measurements)
			.unwrap()
			.is_empty()
	);
	assert!(
		read_jsonl::<InputVersionRecord>(&paths.input_versions)
			.unwrap()
			.is_empty()
	);
	assert!(
		read_jsonl::<WorkshopObservationRecord>(&paths.observations)
			.unwrap()
			.is_empty()
	);
	assert!(!paths.legacy_objects.exists());
	assert!(!paths.legacy_work.exists());
}

#[test]
fn workshop_live_measurement_does_not_reidentify_content_without_acf_change() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = live_workshop_fixture(&temp, "workshop-input-drift");
	let mut manifest = WorkshopCaseManifest::from_path(&fixture.case_manifest).unwrap();
	manifest.cases.truncate(1);
	fs::write(
		&fixture.case_manifest,
		serde_json::to_vec_pretty(&manifest).unwrap(),
	)
	.unwrap();
	let mut runner = MutatingWorkshopRunner {
		inner: FakeRunner::new([completed(MergeReportStatus::Ready, 3)]),
		mutation: Some(WorkshopMutation::SourceContent),
	};

	let summary = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &fixture.case_manifest,
			dataset_root: &fixture.dataset,
			discovery: &fixture.discovery,
			timeout: Duration::from_secs(30),
			basegame_root: &fixture.game,
		},
		&mut runner,
	)
	.unwrap();

	assert_eq!(
		(summary.selected, summary.measured, summary.failed),
		(1, 1, 0)
	);
	assert_eq!(runner.inner.calls.len(), 1);
	let paths = DatasetPaths::new(&fixture.dataset);
	assert_eq!(
		read_jsonl::<MeasurementRecord>(&paths.measurements)
			.unwrap()
			.len(),
		1
	);
	assert_eq!(
		read_jsonl::<InputVersionRecord>(&paths.input_versions)
			.unwrap()
			.len(),
		1
	);
	assert_eq!(
		read_jsonl::<WorkshopObservationRecord>(&paths.observations)
			.unwrap()
			.len(),
		1
	);
	assert!(!paths.legacy_objects.exists());
	assert!(!paths.legacy_work.exists());
}

#[test]
fn workshop_live_measurement_rejects_wrong_product_report_input() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = live_workshop_fixture(&temp, "workshop-report-input");
	let mut runner = MutatingWorkshopRunner {
		inner: FakeRunner::new([completed(MergeReportStatus::Ready, 3)]),
		mutation: Some(WorkshopMutation::ReportInput),
	};

	let error = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &fixture.case_manifest,
			dataset_root: &fixture.dataset,
			discovery: &fixture.discovery,
			timeout: Duration::from_secs(30),
			basegame_root: &fixture.game,
		},
		&mut runner,
	)
	.unwrap_err();

	assert!(
		error.to_string().contains("report input does not match"),
		"{error}"
	);
	let paths = DatasetPaths::new(&fixture.dataset);
	assert!(
		read_jsonl::<MeasurementRecord>(&paths.measurements)
			.unwrap()
			.is_empty()
	);
	assert!(
		read_jsonl::<FileResultRecord>(&paths.file_results)
			.unwrap()
			.is_empty()
	);
}

#[test]
fn workshop_live_measurement_requires_steam_build_identity() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = live_workshop_fixture(&temp, "workshop-missing-build");
	let mut discovery = fixture.discovery.clone();
	discovery.steam_build_id = None;
	discovery.steam_root = None;
	let mut runner = FakeRunner::new([]);

	let error = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &fixture.case_manifest,
			dataset_root: &fixture.dataset,
			discovery: &discovery,
			timeout: Duration::from_secs(30),
			basegame_root: &fixture.game,
		},
		&mut runner,
	)
	.unwrap_err();

	assert!(
		error.to_string().contains("requires a Steam build id"),
		"{error}"
	);
	assert!(runner.calls.is_empty());
}

#[test]
fn workshop_report_rejects_stale_game_build_and_version_identity() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = live_workshop_fixture(&temp, "workshop-report-game-drift");
	let mut runner = FakeRunner::new(completed_outcomes(14));
	let measured = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &fixture.case_manifest,
			dataset_root: &fixture.dataset,
			discovery: &fixture.discovery,
			timeout: Duration::from_secs(30),
			basegame_root: &fixture.game,
		},
		&mut runner,
	)
	.unwrap();

	let original_appmanifest = fs::read(&fixture.game_appmanifest).unwrap();
	let changed_appmanifest = String::from_utf8(original_appmanifest.clone())
		.unwrap()
		.replace("\"buildid\" \"4242\"", "\"buildid\" \"4343\"");
	fs::write(&fixture.game_appmanifest, changed_appmanifest).unwrap();
	let build_output = temp.path().join("report-build-drift");
	let build_error = report_workshop(&WorkshopReportOptions {
		case_manifest: &fixture.case_manifest,
		dataset_root: &fixture.dataset,
		discovery: &fixture.discovery,
		output_dir: &build_output,
		cohort_id: &measured.cohort_id,
		cohort: WorkshopReportCohort::AllCandidates,
	})
	.unwrap_err();
	assert!(
		build_error
			.to_string()
			.contains("base-game identity differ"),
		"{build_error}"
	);
	assert!(!build_output.join("baseline.json").exists());

	fs::write(&fixture.game_appmanifest, original_appmanifest).unwrap();
	fs::write(fixture.game.join("version.txt"), "1.37.6\n").unwrap();
	let version_output = temp.path().join("report-version-drift");
	let version_error = report_workshop(&WorkshopReportOptions {
		case_manifest: &fixture.case_manifest,
		dataset_root: &fixture.dataset,
		discovery: &fixture.discovery,
		output_dir: &version_output,
		cohort_id: &measured.cohort_id,
		cohort: WorkshopReportCohort::AllCandidates,
	})
	.unwrap_err();
	assert!(
		version_error
			.to_string()
			.contains("base-game identity differ"),
		"{version_error}"
	);
	assert!(!version_output.join("baseline.json").exists());
}
