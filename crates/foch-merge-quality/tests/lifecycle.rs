#![cfg(target_os = "macos")]

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use foch_core::domain::game::Game;
use foch_core::model::{MergeReport, MergeReportStatus};
use foch_engine::{BaseDataSource, FileFilter, build_base_snapshot, install_built_snapshot};
use foch_merge_quality::config::{Eu4Discovery, WorkshopCatalog};
use foch_merge_quality::corpus::{Case, Corpus};
use foch_merge_quality::dataset::{
	DatasetPaths, EngineArtifactIdentity, FileResultRecord, MeasurementKernel, MeasurementRecord,
	MeasurementScope, SnapshotRecord, TerminalStatus, read_jsonl,
};
use foch_merge_quality::lifecycle::{
	CollectOptions, MeasureOptions, MeasurementRequest, MeasurementRunner,
	MeasurementRunnerIdentity, ReportCohort, ReportOptions, TerminalMerge, collect,
	measure_with_runner, report,
};

static BASE_DATA_ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
	key: &'static str,
	previous: Option<OsString>,
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

struct Fixture {
	game: PathBuf,
	dataset: PathBuf,
	results: PathBuf,
}

fn write_decision_mod(root: &Path, content: &str) {
	fs::create_dir_all(root.join("decisions")).unwrap();
	fs::write(root.join("descriptor.mod"), "name=\"fixture\"\n").unwrap();
	fs::write(root.join("decisions/example.txt"), content).unwrap();
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

fn collect_fixture(temp: &tempfile::TempDir, case_count: usize) -> (Fixture, EnvGuard) {
	let game = temp.path().join("game");
	let base_data = temp.path().join("base-data");
	let workshop = temp.path().join("workshop");
	let dataset = temp.path().join("dataset");
	let results = temp.path().join("results");
	fs::create_dir_all(game.join("decisions")).unwrap();
	fs::write(game.join("version.txt"), "1.37.5\n").unwrap();
	fs::write(
		game.join("decisions/example.txt"),
		"country_decisions = { shared = { potential = { tag = FRA } allow = { stability = 1 } effect = { add_prestige = 1 } } }\n",
	)
	.unwrap();
	let env_guard = install_test_base_data(&game, &base_data);
	write_decision_mod(
		&workshop.join("1"),
		"country_decisions = { shared = { potential = { tag = ENG } allow = { stability = 1 } effect = { add_prestige = 1 } } }\n",
	);
	write_decision_mod(
		&workshop.join("2"),
		"country_decisions = { shared = { potential = { tag = FRA } allow = { stability = 1 } effect = { add_prestige = 2 } } }\n",
	);
	let mut cases = Vec::with_capacity(case_count);
	for index in 0..case_count {
		let compatch_id = (100 + index).to_string();
		write_decision_mod(
			&workshop.join(&compatch_id),
			"country_decisions = { shared = { potential = { tag = ENG } allow = { stability = 1 } effect = { add_prestige = 2 } } }\n",
		);
		cases.push(Case {
			compatch_id,
			title: format!("Fixture compatibility patch {index}"),
			referenced_mods: vec!["1".to_string(), "2".to_string()],
			..Case::default()
		});
	}
	let corpus = temp.path().join("corpus.json");
	fs::write(
		&corpus,
		serde_json::to_vec_pretty(&Corpus {
			cases,
			..Corpus::default()
		})
		.unwrap(),
	)
	.unwrap();
	let discovery = Eu4Discovery {
		game_root: game.clone(),
		game_version: "1.37.5".to_string(),
		steam_build_id: None,
		steam_root: None,
		workshop: WorkshopCatalog {
			roots: vec![workshop],
		},
	};
	let summary = collect(&CollectOptions {
		corpus: &corpus,
		dataset_root: &dataset,
		discovery: &discovery,
		limit: 0,
	})
	.unwrap();
	assert_eq!(summary.snapshots, case_count);
	(
		Fixture {
			game,
			dataset,
			results,
		},
		env_guard,
	)
}

fn runner_identity() -> MeasurementRunnerIdentity {
	MeasurementRunnerIdentity {
		engine_artifact: EngineArtifactIdentity::foch_executable_blake3("a".repeat(64)),
		worker_protocol_version: "fake-product-runner-v1".to_string(),
		merge_kernel: MeasurementKernel::SemanticTree,
		scope: MeasurementScope::FullProductMerge,
	}
}

struct FakeRunner {
	identity: MeasurementRunnerIdentity,
	preflight_error: Option<String>,
	mutate_pinned_base: bool,
	outcomes: VecDeque<TerminalMerge>,
	calls: Vec<MeasurementRequest>,
}

impl FakeRunner {
	fn new(outcomes: impl IntoIterator<Item = TerminalMerge>) -> Self {
		Self {
			identity: runner_identity(),
			preflight_error: None,
			mutate_pinned_base: false,
			outcomes: outcomes.into_iter().collect(),
			calls: Vec::new(),
		}
	}

	fn with_preflight_error(detail: impl Into<String>) -> Self {
		Self {
			identity: runner_identity(),
			preflight_error: Some(detail.into()),
			mutate_pinned_base: false,
			outcomes: VecDeque::new(),
			calls: Vec::new(),
		}
	}

	fn with_base_mutation(outcome: TerminalMerge) -> Self {
		Self {
			identity: runner_identity(),
			preflight_error: None,
			mutate_pinned_base: true,
			outcomes: VecDeque::from([outcome]),
			calls: Vec::new(),
		}
	}
}

impl MeasurementRunner for FakeRunner {
	fn identity(&self) -> &MeasurementRunnerIdentity {
		&self.identity
	}

	fn preflight(&self) -> Result<(), String> {
		self.preflight_error.clone().map_or(Ok(()), Err)
	}

	fn run(&mut self, request: &MeasurementRequest) -> TerminalMerge {
		self.calls.push(request.clone());
		if self.mutate_pinned_base {
			fs::write(
				request.basegame_root.join("injected-after-preflight.txt"),
				"changed",
			)
			.unwrap();
		}
		let outcome = self
			.outcomes
			.pop_front()
			.expect("fake runner received an unexpected request");
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

struct PinnedBaseRunner {
	inner: FakeRunner,
	live_root: PathBuf,
	excluded_relative: PathBuf,
}

impl MeasurementRunner for PinnedBaseRunner {
	fn identity(&self) -> &MeasurementRunnerIdentity {
		self.inner.identity()
	}

	fn preflight(&self) -> Result<(), String> {
		self.inner.preflight()
	}

	fn run(&mut self, request: &MeasurementRequest) -> TerminalMerge {
		assert_ne!(request.basegame_root, self.live_root);
		assert!(request.basegame_root.join("version.txt").is_file());
		assert!(
			request
				.basegame_root
				.join("decisions/example.txt")
				.is_file()
		);
		assert!(!request.basegame_root.join(&self.excluded_relative).exists());
		self.inner.run(request)
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

fn fatal_report(detail: &str) -> TerminalMerge {
	TerminalMerge::Completed {
		report: Box::new(MergeReport {
			status: MergeReportStatus::Fatal,
			fatal_reason: Some(detail.to_string()),
			..MergeReport::default()
		}),
		merge_ms: 1,
	}
}

#[test]
fn completed_measurements_resume_without_reinvoking_cached_snapshots() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 2);
	let mut first_runner = FakeRunner::new([completed(MergeReportStatus::Blocked, 7)]);
	let first = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 1,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut first_runner,
	)
	.unwrap();
	assert_eq!((first.selected, first.cached, first.measured), (1, 0, 1));
	assert_eq!(first_runner.calls.len(), 1);
	assert_eq!(first_runner.calls[0].timeout, Duration::from_secs(30));
	assert!(
		!first_runner.calls[0]
			.expected_base_snapshot_identity
			.is_empty()
	);

	let mut resume_runner = FakeRunner::new([completed(MergeReportStatus::Ready, 9)]);
	let resumed = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut resume_runner,
	)
	.unwrap();
	assert_eq!(
		(resumed.selected, resumed.cached, resumed.measured),
		(2, 1, 1)
	);
	assert_eq!(resume_runner.calls.len(), 1);

	let mut cached_runner = FakeRunner::new([]);
	let cached = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut cached_runner,
	)
	.unwrap();
	assert_eq!((cached.selected, cached.cached, cached.measured), (2, 2, 0));
	assert!(cached_runner.calls.is_empty());

	let paths = DatasetPaths::new(&fixture.dataset);
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements).unwrap();
	assert_eq!(measurements.len(), 2);
	assert!(measurements.iter().all(|record| {
		record.status() == TerminalStatus::Completed
			&& record.merge_kernel() == Some(MeasurementKernel::SemanticTree)
			&& record.merged_output_hash().is_some()
	}));
	let file_results = read_jsonl::<FileResultRecord>(&paths.file_results).unwrap();
	assert_eq!(file_results.len(), 2);
	assert!(
		file_results
			.iter()
			.all(|record| { record.result["score"]["accepted_ok"].as_bool().unwrap() })
	);

	let cohort_id = measurements[0].cohort_id();
	report(&ReportOptions {
		dataset_root: &fixture.dataset,
		output_dir: &fixture.results,
		cohort_id: Some(&cohort_id),
		scorer_version: None,
		cohort: ReportCohort::AllCandidates,
		limit: 0,
		snapshot_ids: None,
	})
	.unwrap();
	let baseline: serde_json::Value =
		serde_json::from_str(&fs::read_to_string(fixture.results.join("baseline.json")).unwrap())
			.unwrap();
	assert_eq!(baseline["schema"], "2.0.0");
	assert_eq!(baseline["measurement_cohort_id"], cohort_id);
	assert_eq!(baseline["baseline_complete"], true);
	assert_eq!(baseline["completed_cases"], 2);
}

#[test]
fn crash_window_file_results_resume_idempotently_before_terminal_commit() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 1);
	let options = MeasureOptions {
		dataset_root: &fixture.dataset,
		timeout: Duration::from_secs(30),
		limit: 0,
		basegame_root: &fixture.game,
		snapshot_ids: None,
	};
	let mut first_runner = FakeRunner::new([completed(MergeReportStatus::Ready, 3)]);
	measure_with_runner(&options, &mut first_runner).unwrap();

	let paths = DatasetPaths::new(&fixture.dataset);
	let orphan_file_results = read_jsonl::<FileResultRecord>(&paths.file_results).unwrap();
	assert_eq!(orphan_file_results.len(), 1);
	fs::write(&paths.measurements, b"").unwrap();

	let mut replay_runner = FakeRunner::new([completed(MergeReportStatus::Ready, 3)]);
	let replayed = measure_with_runner(&options, &mut replay_runner).unwrap();
	assert_eq!((replayed.cached, replayed.measured), (0, 1));
	assert_eq!(replay_runner.calls.len(), 1);
	assert_eq!(
		read_jsonl::<FileResultRecord>(&paths.file_results).unwrap(),
		orphan_file_results
	);
	assert_eq!(
		read_jsonl::<MeasurementRecord>(&paths.measurements)
			.unwrap()
			.len(),
		1
	);

	let mut cached_runner = FakeRunner::new([]);
	let cached = measure_with_runner(&options, &mut cached_runner).unwrap();
	assert_eq!((cached.cached, cached.measured), (1, 0));
	assert!(cached_runner.calls.is_empty());
}

#[test]
fn crash_window_replay_rejects_changed_file_evidence_before_terminal_commit() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 1);
	let options = MeasureOptions {
		dataset_root: &fixture.dataset,
		timeout: Duration::from_secs(30),
		limit: 0,
		basegame_root: &fixture.game,
		snapshot_ids: None,
	};
	let mut first_runner = FakeRunner::new([completed(MergeReportStatus::Ready, 3)]);
	measure_with_runner(&options, &mut first_runner).unwrap();

	let paths = DatasetPaths::new(&fixture.dataset);
	fs::write(&paths.measurements, b"").unwrap();
	let mut orphan = read_jsonl::<FileResultRecord>(&paths.file_results)
		.unwrap()
		.pop()
		.unwrap();
	orphan.result["score"]["accepted_ok"] = serde_json::Value::Bool(false);
	orphan = FileResultRecord::new(orphan.measurement_id, orphan.relative_path, orphan.result);
	fs::write(
		&paths.file_results,
		format!("{}\n", serde_json::to_string(&orphan).unwrap()),
	)
	.unwrap();

	let mut replay_runner = FakeRunner::new([completed(MergeReportStatus::Ready, 3)]);
	let error = measure_with_runner(&options, &mut replay_runner).unwrap_err();
	assert!(error.to_string().contains("differs from replay"));
	assert_eq!(replay_runner.calls.len(), 1);
	assert!(
		read_jsonl::<MeasurementRecord>(&paths.measurements)
			.unwrap()
			.is_empty()
	);
}

#[test]
fn exact_snapshot_selection_pins_measurement_and_report_denominators() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 2);
	let paths = DatasetPaths::new(&fixture.dataset);
	let snapshots = read_jsonl::<SnapshotRecord>(&paths.snapshots).unwrap();
	let exact_ids = vec![snapshots[1].snapshot_id.clone()];
	let mut runner = FakeRunner::new([completed(MergeReportStatus::Ready, 3)]);
	let summary = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: Some(&exact_ids),
		},
		&mut runner,
	)
	.unwrap();
	assert_eq!((summary.selected, summary.measured), (1, 1));
	assert_eq!(runner.calls[0].snapshot_id, exact_ids[0]);

	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements).unwrap();
	let cohort_id = measurements[0].cohort_id();
	report(&ReportOptions {
		dataset_root: &fixture.dataset,
		output_dir: &fixture.results,
		cohort_id: Some(&cohort_id),
		scorer_version: None,
		cohort: ReportCohort::AllCandidates,
		limit: 0,
		snapshot_ids: Some(&exact_ids),
	})
	.unwrap();
	let baseline: serde_json::Value =
		serde_json::from_slice(&fs::read(fixture.results.join("baseline.json")).unwrap()).unwrap();
	assert_eq!(baseline["candidate_cases"], 1);
	assert_eq!(baseline["total_cases"], 1);
	assert_eq!(baseline["cases"][0]["snapshot_id"], exact_ids[0]);

	let duplicate_ids = vec![exact_ids[0].clone(), exact_ids[0].clone()];
	let mut never_runner = FakeRunner::new([]);
	let error = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: Some(&duplicate_ids),
		},
		&mut never_runner,
	)
	.unwrap_err();
	assert!(error.to_string().contains("duplicate IDs"));
	assert!(never_runner.calls.is_empty());
}

#[test]
fn altered_snapshot_payload_is_rejected_before_measurement_identity_reuse() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 1);
	let paths = DatasetPaths::new(&fixture.dataset);
	let mut snapshot = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.unwrap()
		.pop()
		.unwrap();
	snapshot.compatch.content_hash = "0".repeat(64);
	fs::write(
		&paths.snapshots,
		format!("{}\n", serde_json::to_string(&snapshot).unwrap()),
	)
	.unwrap();

	let mut runner = FakeRunner::new([]);
	let error = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut runner,
	)
	.unwrap_err();
	assert!(error.to_string().contains("invalid identity"));
	assert!(runner.calls.is_empty());
}

#[test]
fn cached_completed_measurement_requires_intact_cas_and_file_evidence() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 1);
	let mut first_runner = FakeRunner::new([completed(MergeReportStatus::Ready, 3)]);
	measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut first_runner,
	)
	.unwrap();
	let paths = DatasetPaths::new(&fixture.dataset);
	let measurement = read_jsonl::<MeasurementRecord>(&paths.measurements)
		.unwrap()
		.pop()
		.unwrap();
	let output_hash = measurement.merged_output_hash().unwrap();
	let output_file = paths
		.objects
		.join(&output_hash[..2])
		.join(output_hash)
		.join("tree/decisions/example.txt");
	let original = fs::read(&output_file).unwrap();
	fs::write(&output_file, b"corrupt cached output\n").unwrap();
	let mut cached_runner = FakeRunner::new([]);
	let error = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut cached_runner,
	)
	.unwrap_err();
	assert!(error.to_string().contains("CAS verification"));
	assert!(
		!error
			.to_string()
			.contains(&fixture.dataset.display().to_string())
	);
	assert!(cached_runner.calls.is_empty());

	fs::write(&output_file, original).unwrap();
	let original_file_results = fs::read_to_string(&paths.file_results).unwrap();
	let mut altered_file_result = read_jsonl::<FileResultRecord>(&paths.file_results)
		.unwrap()
		.pop()
		.unwrap();
	altered_file_result.result["score"]["acceptance_reason"] =
		serde_json::Value::String("altered detail with unchanged aggregates".to_string());
	fs::write(
		&paths.file_results,
		format!("{}\n", serde_json::to_string(&altered_file_result).unwrap()),
	)
	.unwrap();
	let error = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut cached_runner,
	)
	.unwrap_err();
	assert!(error.to_string().contains("invalid identity"));
	assert!(cached_runner.calls.is_empty());

	fs::write(&paths.file_results, original_file_results).unwrap();
	fs::write(&paths.file_results, b"").unwrap();
	let error = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut cached_runner,
	)
	.unwrap_err();
	assert!(error.to_string().contains("stores 0 file results"));
	assert!(cached_runner.calls.is_empty());
}

#[test]
fn cached_measurement_requires_present_and_unchanged_input_cas() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 1);
	let options = MeasureOptions {
		dataset_root: &fixture.dataset,
		timeout: Duration::from_secs(30),
		limit: 0,
		basegame_root: &fixture.game,
		snapshot_ids: None,
	};
	let mut first_runner = FakeRunner::new([completed(MergeReportStatus::Ready, 3)]);
	measure_with_runner(&options, &mut first_runner).unwrap();

	let paths = DatasetPaths::new(&fixture.dataset);
	let snapshot = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.unwrap()
		.pop()
		.unwrap();
	let source_hash = &snapshot.source_mods[0].content_hash;
	let source_file = paths
		.objects
		.join(&source_hash[..2])
		.join(source_hash)
		.join("tree/decisions/example.txt");
	fs::write(&source_file, b"corrupt cached input\n").unwrap();

	let mut cached_runner = FakeRunner::new([]);
	let corrupt_error = measure_with_runner(&options, &mut cached_runner).unwrap_err();
	assert!(corrupt_error.to_string().contains("CAS preflight failed"));
	assert!(cached_runner.calls.is_empty());

	fs::remove_file(source_file).unwrap();
	let missing_error = measure_with_runner(&options, &mut cached_runner).unwrap_err();
	assert!(missing_error.to_string().contains("CAS preflight failed"));
	assert!(cached_runner.calls.is_empty());
}

#[test]
fn product_base_mutation_is_rejected_before_terminal_persistence() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 1);
	let mut runner = FakeRunner::with_base_mutation(completed(MergeReportStatus::Ready, 3));
	let error = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut runner,
	)
	.unwrap_err();

	assert!(error.to_string().contains("pinned product base changed"));
	assert_eq!(runner.calls.len(), 1);
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
fn product_and_scorer_receive_only_the_pinned_base_view() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 1);
	let excluded_relative = PathBuf::from("added-after-snapshot.txt");
	fs::write(fixture.game.join(&excluded_relative), "unbound").unwrap();
	let mut runner = PinnedBaseRunner {
		inner: FakeRunner::new([completed(MergeReportStatus::Ready, 3)]),
		live_root: fixture.game.clone(),
		excluded_relative,
	};
	let summary = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
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
fn terminal_runner_outcomes_are_persisted_without_process_protocols() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 5);
	let private_root = fixture.game.display().to_string();
	let mut runner = FakeRunner::new([
		TerminalMerge::MergeFailed {
			detail: "no usable report".to_string(),
		},
		TerminalMerge::Crashed {
			detail: Some("signal 11".to_string()),
		},
		TerminalMerge::TimedOut {
			detail: Some("deadline exceeded".to_string()),
		},
		TerminalMerge::Fatal {
			detail: format!("runner setup failed under {private_root}"),
		},
		fatal_report("product report is fatal"),
	]);
	let summary = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut runner,
	)
	.unwrap();
	assert_eq!((summary.measured, summary.failed), (5, 5));
	assert_eq!(runner.calls.len(), 5);

	let paths = DatasetPaths::new(&fixture.dataset);
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements).unwrap();
	let statuses = measurements
		.iter()
		.map(MeasurementRecord::status)
		.collect::<Vec<_>>();
	assert_eq!(
		statuses,
		[
			TerminalStatus::MergeFailed,
			TerminalStatus::Crashed,
			TerminalStatus::TimedOut,
			TerminalStatus::Fatal,
			TerminalStatus::Fatal,
		]
	);
	assert_eq!(measurements[4].detail(), Some("product report is fatal"));
	assert_eq!(
		measurements[3].detail(),
		Some("runner setup failed under <absolute-path>")
	);
	assert!(!measurements[3].detail().unwrap().contains(&private_root));
	assert!(
		measurements
			.iter()
			.all(|record| record.merged_output_hash().is_none())
	);
}

#[test]
fn corrupt_input_fails_preflight_without_persisting_an_identity() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 1);
	let paths = DatasetPaths::new(&fixture.dataset);
	let snapshot = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.unwrap()
		.pop()
		.unwrap();
	let hash = &snapshot.source_mods[0].content_hash;
	fs::write(
		paths
			.objects
			.join(&hash[..2])
			.join(hash)
			.join("tree/decisions/example.txt"),
		"corrupt\n",
	)
	.unwrap();

	let mut runner = FakeRunner::new([]);
	let error = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut runner,
	)
	.unwrap_err();
	assert!(error.to_string().contains("CAS preflight failed"));
	assert!(
		!error
			.to_string()
			.contains(&fixture.dataset.display().to_string())
	);
	assert!(runner.calls.is_empty());
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements).unwrap();
	assert!(measurements.is_empty());
}

#[test]
fn runner_build_preflight_failure_is_redacted_and_not_persisted() {
	let _lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let temp = tempfile::tempdir().unwrap();
	let (fixture, _env) = collect_fixture(&temp, 1);
	let sentinel = "/private/sentinel/product-build";
	let mut runner = FakeRunner::with_preflight_error(format!("artifact changed at {sentinel}"));
	let error = measure_with_runner(
		&MeasureOptions {
			dataset_root: &fixture.dataset,
			timeout: Duration::from_secs(30),
			limit: 0,
			basegame_root: &fixture.game,
			snapshot_ids: None,
		},
		&mut runner,
	)
	.unwrap_err();
	assert!(error.to_string().contains("runner preflight failed"));
	assert!(error.to_string().contains("<absolute-path>"));
	assert!(!error.to_string().contains(sentinel));
	assert!(runner.calls.is_empty());
	let paths = DatasetPaths::new(&fixture.dataset);
	assert!(
		read_jsonl::<MeasurementRecord>(&paths.measurements)
			.unwrap()
			.is_empty()
	);
}
