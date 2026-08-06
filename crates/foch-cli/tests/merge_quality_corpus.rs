#[path = "merge_quality/runner.rs"]
mod runner;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use foch_core::domain::game::Game;
use foch_core::model::{
	MERGE_EXECUTION_ATTESTATION_SCHEMA, MERGE_REPORT_ARTIFACT_PATH, MergeReportBaseSnapshot,
	MergeReportKernel, MergeReportScope, MergeReportStatus,
};
use foch_engine::{BaseDataSource, FileFilter, build_base_snapshot, install_built_snapshot};
use foch_merge_quality::config::{
	DiscoveryOverrides, Eu4Discovery, WorkshopCatalog, detect_game_version, discover_eu4_game,
};
use foch_merge_quality::corpus::{Case, assess_oracle_candidate};
use foch_merge_quality::dataset::{
	DatasetPaths, MeasurementKernel, MeasurementRecord, MeasurementScope, ObservationRecord,
	SCORER_VERSION, SnapshotRecord, TerminalStatus, read_jsonl,
};
use foch_merge_quality::lifecycle::{
	CollectOptions, MeasureOptions, MeasurementRequest, MeasurementRunner, ReportCohort,
	ReportOptions, TerminalMerge, collect, measure_with_runner, report,
};
use foch_merge_quality::object_store::digest_tree;
use foch_merge_quality::orchestrate::{
	ScoreExistingOutputRequest, score_existing_output_with_cache,
};
use foch_merge_quality::review_pack::{
	REVIEW_PACK_CASE_COUNT, REVIEW_PACK_LEGACY_UNIT_COUNT, REVIEW_PACK_STRUCTURED_UNIT_COUNT,
	ReviewPackBuildOptions, ReviewPackVerifyOptions, build_review_pack_with_runner,
	verify_review_pack,
};
use foch_merge_quality::score::ScoreCache;
use runner::{ProductMeasurementRunner, RUNNER_PROTOCOL_VERSION};

const FIXTURE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const FULL_CORPUS_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const FIXED_PRODUCT_SCORABLE_SNAPSHOT_IDS: [&str; 6] = [
	"24048fe131d8365e61b3d70172e57d57f6a38ffb000f19a30ee35c20935a5803",
	"54fb216188ec4267c3006b2b05b8c994ba75961a7337839a05a9f23272d62d0c",
	"695b4ed11d04270342f5cb007ebd14001513bd91e3187434a6531e8f76ab7d7c",
	"b1f12d633dd249e644a0205708e8c23898d990eb8eea8d43134ff78188ef61f7",
	"d60369eb6a10122a1478df23c06031a84858075f09db11574e8d29cc1a260f5c",
	"ebdf8ea89e7fa5edb103708cdab2ce02dca53af16642d4aa1f5e577781033e30",
];
const FIXED_PRODUCT_SNAPSHOT_IDS: [&str; 23] = [
	"00fdd9728a2db4dd886c759d86e5f25f93f2013d3b1998ad99d8599627b66d8b",
	"02f3aba17eac3872918a21785384be530d8b915019f2739d0eff48cf7051b033",
	"1b4c8c5294e8b95d8adb719b1d4fc440eceb18d8e26db4157354291f60ade6ab",
	"24048fe131d8365e61b3d70172e57d57f6a38ffb000f19a30ee35c20935a5803",
	"33db0274995f8a104ad1e42a627b876f4431269a8b8fd042f4ea4d386fec9e7c",
	"367c1c4409de7db82d8ed6c830ee98e998eea813f77e3e931cb59f5831eec06a",
	"36e1cecb4454c6e363a1c0dff148498869dca0385f1cb2668716e90733979fbf",
	"48de620bb78ea81db6995eebae567efce9e0e86ecba1290b51e92ddddb2a7e76",
	"54fb216188ec4267c3006b2b05b8c994ba75961a7337839a05a9f23272d62d0c",
	"6068b3252e7a4e26ff68790ffa3dacca934d289341e2bfc65471ec6e5c4496fc",
	"653cb528e2706cc73546cf745471f3d61b317b812047ca71a5dbf6f920348295",
	"695b4ed11d04270342f5cb007ebd14001513bd91e3187434a6531e8f76ab7d7c",
	"6dd5d45ae9667a78fe6a2faeb813b8c932186db75c1ae9dc36ab531241413029",
	"70792b6429f5a3d367bb2ed74590ae5df5cbde228947de4f35b5c31e2a5d652b",
	"9672d4b3ebea1e1d24f6cd022a6b9e22155bf7ede4edbccdf31a193a06024a59",
	"b1f12d633dd249e644a0205708e8c23898d990eb8eea8d43134ff78188ef61f7",
	"b8fc95ed868f0997df220171e970c844063dd98a0e933d8d75cfe8ec10d4be8d",
	"be0d3ce7c9e9c964a7545615e4797a333be885ec284956b9e59d141fed566ef2",
	"c90c17b490bbda62ff84cabbadbb2d9251e4d4be67ef0fe5dbe2a7a1a44ba5c1",
	"cd2f4221134938abd04715690496a3bcd1733de78f9cfe801b4fa3402784491b",
	"d60369eb6a10122a1478df23c06031a84858075f09db11574e8d29cc1a260f5c",
	"db903c1706911ed2e237864e1fa5ad27134b97d82af907268f5cf147e423a0e5",
	"ebdf8ea89e7fa5edb103708cdab2ce02dca53af16642d4aa1f5e577781033e30",
];

static BASE_DATA_ENV_LOCK: Mutex<()> = Mutex::new(());
const ACCEPTANCE_ENV: &str = "FOCH_MERGE_QUALITY_ACCEPTANCE";

#[test]
fn tiny_product_cli_to_pure_scorer_seam() {
	let root = tempfile::tempdir().expect("tiny product fixture root");
	let source_a = root.path().join("source-a");
	let source_b = root.path().join("source-b");
	let compatch = root.path().join("human-compatch");
	let output = root.path().join("product-output");
	let empty_base = root.path().join("empty-base");
	fs::create_dir_all(&empty_base).expect("create explicit empty base");
	write_source_mod(
		&source_a,
		"910001",
		"test_shared_effect = { set_country_flag = effect_a_ran }\n",
	);
	write_source_mod(
		&source_b,
		"910002",
		"test_shared_effect = { add_prestige = 5 }\n",
	);
	write_file(
		&compatch,
		"common/scripted_effects/human.txt",
		concat!(
			"test_shared_effect = {\n",
			"\tset_country_flag = effect_a_ran\n",
			"\tadd_prestige = 5\n",
			"}\n",
		),
	);
	let source_a_before = snapshot_tree(&source_a);
	let source_b_before = snapshot_tree(&source_b);
	let case = Case {
		compatch_id: "tiny-human".to_string(),
		title: "tiny product/scorer seam".to_string(),
		referenced_mods: vec!["910001".to_string(), "910002".to_string()],
		..Case::default()
	};
	let request = MeasurementRequest {
		snapshot_id: "tiny-product-seam".to_string(),
		case: case.clone(),
		compatch_dir: compatch.clone(),
		source_dirs: vec![source_a.clone(), source_b.clone()],
		output_dir: output.clone(),
		basegame_root: empty_base,
		expected_base_snapshot_identity: "explicitly-disabled".to_string(),
		timeout: Duration::from_secs(60),
	};
	let mut runner =
		ProductMeasurementRunner::no_game_base_fixture().expect("construct product runner");
	assert_eq!(
		runner.identity().merge_kernel,
		MeasurementKernel::SemanticTree
	);
	assert_eq!(runner.identity().scope, MeasurementScope::FullProductMerge);
	assert_eq!(
		runner.identity().runner_protocol_version,
		RUNNER_PROTOCOL_VERSION
	);
	assert_eq!(runner.identity().engine_artifact.hash.len(), 64);

	let (merge_report, merge_ms) = match runner.run(&request) {
		TerminalMerge::Completed { report, merge_ms } => (report, merge_ms),
		terminal => panic!("tiny public product merge did not complete: {terminal:?}"),
	};
	assert_eq!(merge_report.status, MergeReportStatus::Ready);
	let execution = merge_report
		.execution
		.as_ref()
		.expect("public product report carries execution attestation");
	assert_eq!(execution.schema, MERGE_EXECUTION_ATTESTATION_SCHEMA);
	assert_eq!(execution.kernel, MergeReportKernel::SemanticTree);
	assert_eq!(execution.scope, MergeReportScope::FullProductMerge);
	assert_eq!(execution.base_snapshot, MergeReportBaseSnapshot::Disabled);
	let report_bytes =
		fs::read(output.join(MERGE_REPORT_ARTIFACT_PATH)).expect("public CLI wrote merge report");
	let report_json: serde_json::Value =
		serde_json::from_slice(&report_bytes).expect("parse persisted merge report");
	assert_eq!(report_json["status"], "ready");
	let product_output = output.join("common/scripted_effects/zzz_foch_scripted_effects.txt");
	let product_text = fs::read_to_string(&product_output).expect("read Structured product output");
	assert!(product_text.contains("set_country_flag = effect_a_ran"));
	assert!(product_text.contains("add_prestige = 5"));

	let mut score_cache = ScoreCache::new();
	let source_dirs = [source_a.clone(), source_b.clone()];
	let scored = score_existing_output_with_cache(
		&ScoreExistingOutputRequest {
			case: &case,
			compatch_dir: &compatch,
			source_dirs: &source_dirs,
			output_dir: &output,
			report: &merge_report,
			basegame_root: None,
			merge_ms,
		},
		&mut score_cache,
	)
	.expect("score existing product output")
	.result;
	let expected = BTreeMap::from([("accepted_equivalent".to_string(), 1)]);
	assert_expected_verdicts(&scored.multi_source_verdicts, &expected)
		.expect("tiny product verdict baseline matches");
	let deliberately_wrong = BTreeMap::from([("matches_human".to_string(), 1)]);
	let mismatch = assert_expected_verdicts(&scored.multi_source_verdicts, &deliberately_wrong)
		.expect_err("a deliberate verdict mismatch must fail the acceptance assertion");
	assert!(mismatch.contains("expected") && mismatch.contains("actual"));
	assert_eq!(
		snapshot_tree(&source_a),
		source_a_before,
		"product runner must not mutate source A"
	);
	assert_eq!(
		snapshot_tree(&source_b),
		source_b_before,
		"product runner must not mutate source B"
	);
}

#[test]
fn fixed_product_report_denominators_are_stable() {
	let dataset_root = repo_root().join("crates/foch-merge-quality/dataset");
	let snapshot_ids = fixed_product_snapshot_ids(&dataset_root);
	assert_fixed_product_snapshot_contract(&dataset_root, &snapshot_ids);
}

#[test]
#[ignore = "requires the local base-game fixture and executes six real product merges"]
fn product_fixture_acceptance() {
	require_acceptance("product-fixture");
	let _env_lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let root = tempfile::tempdir().expect("product fixture acceptance root");
	let fixture = root.path().join("fixture");
	foch_merge_quality::archive::unpack(&fixtures_root().join("corpus.tar.gz"), &fixture)
		.expect("unpack six-case corpus fixture");
	foch_merge_quality::archive::unpack(&fixtures_root().join("basegame-text.tar.gz"), &fixture)
		.expect("unpack version-bound base-game fixture");
	let basegame = fixture.join("basegame");
	let game_version = detect_game_version(&basegame).expect("fixture has game version");
	let data_root = root.path().join("base-data");
	let _data_root = ScopedEnv::set(foch_engine::BASE_DATA_DIR_ENV, &data_root);
	install_fixture_base_snapshot(&basegame, &game_version);
	let discovery = Eu4Discovery {
		game_root: basegame.clone(),
		game_version,
		steam_build_id: None,
		steam_root: None,
		workshop: WorkshopCatalog {
			roots: vec![fixture.join("workshop")],
		},
	};
	let dataset_root = root.path().join("dataset");
	let collected = collect(&CollectOptions {
		corpus: &fixture.join("corpus.json"),
		dataset_root: &dataset_root,
		discovery: &discovery,
		limit: 0,
	})
	.expect("collect six immutable fixture cases");
	assert_eq!(collected.snapshots, 6);
	let fixture_snapshot_ids = exact_snapshot_ids(&dataset_root, 6);
	let mut runner = ProductMeasurementRunner::full_product().expect("construct product runner");
	let measured = measure_with_runner(
		&MeasureOptions {
			dataset_root: &dataset_root,
			timeout: FIXTURE_TIMEOUT,
			limit: 0,
			basegame_root: &basegame,
			snapshot_ids: Some(&fixture_snapshot_ids),
		},
		&mut runner,
	)
	.expect("measure six product fixture cases");
	assert_eq!(measured.selected, 6);
	assert_eq!(
		measured.cached, 0,
		"product fixture cohort must start fresh"
	);
	assert_eq!(measured.measured, 6);
	assert_eq!(measured.failed, 0);
	let records = exact_product_records(&dataset_root, &measured, &fixture_snapshot_ids);
	assert_complete_product_cohort(&records, 6);
	let cohort_id = unique_cohort_id(&records);
	let output = repo_root()
		.join("target/merge-quality/product-fixture-baseline")
		.join(&cohort_id);
	report(&ReportOptions {
		dataset_root: &dataset_root,
		output_dir: &output,
		cohort_id: Some(&cohort_id),
		scorer_version: None,
		cohort: ReportCohort::Scorable,
		limit: 0,
		snapshot_ids: Some(&fixture_snapshot_ids),
	})
	.expect("write product fixture baseline");
	assert_product_report(&output.join("baseline.json"), 6, 6, 6);
}

#[test]
#[ignore = "requires private CAS objects and runs the fixed 23-case product cohort"]
fn full_product_corpus_acceptance() {
	require_acceptance("full-product-corpus");
	let _env_lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let dataset_root = repo_root().join("crates/foch-merge-quality/dataset");
	let snapshot_ids = fixed_product_snapshot_ids(&dataset_root);
	assert_fixed_product_snapshot_contract(&dataset_root, &snapshot_ids);
	let basegame = required_basegame_root();
	let mut runner = ProductMeasurementRunner::full_product().expect("construct product runner");
	let measured = measure_with_runner(
		&MeasureOptions {
			dataset_root: &dataset_root,
			timeout: FULL_CORPUS_TIMEOUT,
			limit: 0,
			basegame_root: &basegame,
			snapshot_ids: Some(&snapshot_ids),
		},
		&mut runner,
	)
	.expect("measure fixed full product corpus");
	assert_eq!(measured.selected, FIXED_PRODUCT_SNAPSHOT_IDS.len());
	assert_eq!(
		measured.cached + measured.measured,
		FIXED_PRODUCT_SNAPSHOT_IDS.len(),
		"a resumed product cohort must account for every fixed snapshot"
	);
	assert_eq!(
		measured.failed, 0,
		"every product case must be terminal-successful"
	);
	let records = exact_product_records(&dataset_root, &measured, &snapshot_ids);
	assert_complete_product_cohort(&records, FIXED_PRODUCT_SNAPSHOT_IDS.len());
	let actual_ids = records
		.iter()
		.map(|record| record.snapshot_id())
		.collect::<BTreeSet<_>>();
	let expected_ids = FIXED_PRODUCT_SNAPSHOT_IDS
		.into_iter()
		.collect::<BTreeSet<_>>();
	assert_eq!(actual_ids, expected_ids);
	let cohort_id = unique_cohort_id(&records);
	let output_root = repo_root()
		.join("target/merge-quality/full-product-corpus")
		.join(&cohort_id);
	for (name, cohort, expected_report_cases) in [
		(
			"scorable",
			ReportCohort::Scorable,
			FIXED_PRODUCT_SCORABLE_SNAPSHOT_IDS.len(),
		),
		(
			"all-candidates",
			ReportCohort::AllCandidates,
			FIXED_PRODUCT_SNAPSHOT_IDS.len(),
		),
	] {
		let output = output_root.join(name);
		report(&ReportOptions {
			dataset_root: &dataset_root,
			output_dir: &output,
			cohort_id: Some(&cohort_id),
			scorer_version: None,
			cohort,
			limit: 0,
			snapshot_ids: Some(&snapshot_ids),
		})
		.expect("write complete product cohort report");
		assert_product_report(
			&output.join("baseline.json"),
			FIXED_PRODUCT_SNAPSHOT_IDS.len(),
			FIXED_PRODUCT_SCORABLE_SNAPSHOT_IDS.len(),
			expected_report_cases,
		);
	}
}

#[test]
#[ignore = "requires private CAS objects, the pinned EU4 build, and six real product merges"]
fn review_pack_acceptance() {
	require_acceptance("review-pack");
	let _env_lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let dataset_root = repo_root().join("crates/foch-merge-quality/dataset");
	let dataset_paths = DatasetPaths::new(&dataset_root);
	let annotations_before = fs::read(&dataset_paths.annotations)
		.expect("read immutable dataset annotations before review-pack build");
	let object_records_before = fs::read(&dataset_paths.object_records)
		.expect("read immutable object records before review-pack build");
	let objects_before =
		digest_tree(&dataset_paths.objects).expect("digest canonical CAS before review-pack build");
	let fixture_root = fixtures_root();
	let selection = fixture_root.join("review-pack-selection.json");
	let legacy_baseline = fixture_root.join("legacy-baseline.json");
	let expected_verdicts = fixture_root.join("expected.json");
	let frozen_inputs_before = [
		fs::read(&selection).expect("read pinned review-pack selection"),
		fs::read(&legacy_baseline).expect("read frozen Legacy baseline"),
		fs::read(&expected_verdicts).expect("read frozen expected verdicts"),
	];
	let game = discover_eu4_game(&DiscoveryOverrides {
		game_root: Some(required_basegame_root()),
		..DiscoveryOverrides::default()
	})
	.expect("discover pinned EU4 build for review pack");
	let mut runner = ProductMeasurementRunner::full_product().expect("construct product runner");
	let artifact_hash = runner.identity().engine_artifact.hash.clone();
	let executable = runner.executable().to_path_buf();
	let output_dir = repo_root()
		.join("target/merge-quality/review-pack")
		.join(&artifact_hash);
	assert!(
		!output_dir.exists(),
		"review-pack output already exists at {}; refusing to overwrite it",
		output_dir.display()
	);

	let build = build_review_pack_with_runner(
		&ReviewPackBuildOptions {
			selection: &selection,
			legacy_baseline: &legacy_baseline,
			expected_verdicts: &expected_verdicts,
			dataset_root: &dataset_root,
			output_dir: &output_dir,
			game: &game,
			executable: &executable,
			timeout: FIXTURE_TIMEOUT,
			force: false,
			wiki_knowledge_snapshot_id: None,
		},
		&mut runner,
	);
	assert_eq!(
		fs::read(&dataset_paths.annotations).expect("read annotations after review-pack build"),
		annotations_before,
		"review-pack build must not mutate dataset annotations"
	);
	assert_eq!(
		fs::read(&dataset_paths.object_records)
			.expect("read object records after review-pack build"),
		object_records_before,
		"review-pack build must not append canonical object metadata"
	);
	assert_eq!(
		digest_tree(&dataset_paths.objects).expect("digest canonical CAS after review-pack build"),
		objects_before,
		"review-pack build must not write canonical CAS objects"
	);
	let built = build.expect("build fixed review pack with the public product runner");
	assert_eq!(built.manifest.executable_blake3, artifact_hash);
	assert_eq!(built.summary.case_count, REVIEW_PACK_CASE_COUNT);
	assert_eq!(built.summary.legacy_units, REVIEW_PACK_LEGACY_UNIT_COUNT);
	assert_eq!(
		built.summary.structured_units,
		REVIEW_PACK_STRUCTURED_UNIT_COUNT
	);

	let verified = verify_review_pack(&ReviewPackVerifyOptions {
		pack_dir: &output_dir,
		selection: &selection,
		legacy_baseline: &legacy_baseline,
		expected_verdicts: &expected_verdicts,
		dataset_root: &dataset_root,
		game: &game,
	});
	assert_eq!(
		fs::read(&dataset_paths.annotations).expect("read annotations after review-pack verify"),
		annotations_before,
		"review-pack verification must not mutate dataset annotations"
	);
	assert_eq!(
		fs::read(&dataset_paths.object_records)
			.expect("read object records after review-pack verify"),
		object_records_before,
		"review-pack verification must not mutate canonical object metadata"
	);
	assert_eq!(
		digest_tree(&dataset_paths.objects).expect("digest canonical CAS after review-pack verify"),
		objects_before,
		"review-pack verification must not mutate canonical CAS objects"
	);
	assert_eq!(
		[
			fs::read(&selection).expect("re-read pinned selection"),
			fs::read(&legacy_baseline).expect("re-read frozen Legacy baseline"),
			fs::read(&expected_verdicts).expect("re-read frozen expected verdicts"),
		],
		frozen_inputs_before,
		"review-pack build and verification must not rewrite frozen inputs"
	);
	let verified = verified.expect("verify fixed review-pack artifacts");
	assert_eq!(
		verified.units_verified,
		REVIEW_PACK_LEGACY_UNIT_COUNT + REVIEW_PACK_STRUCTURED_UNIT_COUNT
	);
	assert_eq!(verified.proposals_verified, verified.units_verified);
}

fn require_acceptance(expected: &str) {
	assert_eq!(
		std::env::var(ACCEPTANCE_ENV).unwrap_or_default(),
		expected,
		"run the matching fixed script under scripts/merge-quality"
	);
}

fn write_source_mod(root: &Path, id: &str, body: &str) {
	write_file(
		root,
		"descriptor.mod",
		&format!("name=\"{id}\"\nremote_file_id=\"{id}\"\n"),
	);
	write_file(root, "common/scripted_effects/source.txt", body);
}

fn write_file(root: &Path, relative: &str, body: &str) {
	let path = root.join(relative);
	fs::create_dir_all(path.parent().expect("fixture file parent"))
		.expect("create fixture file parent");
	fs::write(path, body).expect("write fixture file");
}

fn snapshot_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
	walkdir::WalkDir::new(root)
		.into_iter()
		.map(|entry| entry.expect("walk source fixture"))
		.filter(|entry| entry.file_type().is_file())
		.map(|entry| {
			let relative = entry
				.path()
				.strip_prefix(root)
				.expect("source entry under fixture root")
				.to_string_lossy()
				.replace('\\', "/");
			let bytes = fs::read(entry.path()).expect("read source fixture file");
			(relative, bytes)
		})
		.collect()
}

fn assert_expected_verdicts(
	actual: &BTreeMap<String, usize>,
	expected: &BTreeMap<String, usize>,
) -> Result<(), String> {
	if actual == expected {
		Ok(())
	} else {
		Err(format!(
			"expected verdicts {expected:?}, actual verdicts {actual:?}"
		))
	}
}

fn install_fixture_base_snapshot(basegame: &Path, game_version: &str) {
	let game = Game::EuropaUniversalis4;
	let filter = FileFilter::for_game(game.clone());
	let built = build_base_snapshot(&game, basegame, Some(game_version), &filter)
		.expect("build fixture base snapshot");
	install_built_snapshot(
		&built.encoded_snapshot,
		BaseDataSource::Build,
		Some(built.snapshot_asset_name),
		Some(built.snapshot_sha256),
	)
	.expect("install fixture base snapshot");
}

fn exact_product_records(
	dataset_root: &Path,
	run: &foch_merge_quality::lifecycle::MeasureRunSummary,
	snapshot_ids: &[String],
) -> Vec<MeasurementRecord> {
	let paths = DatasetPaths::new(dataset_root);
	let snapshot_ids = snapshot_ids
		.iter()
		.map(String::as_str)
		.collect::<BTreeSet<_>>();
	read_jsonl::<MeasurementRecord>(&paths.measurements)
		.expect("read product measurements")
		.into_iter()
		.filter(|record| {
			snapshot_ids.contains(record.snapshot_id())
				&& record.schema() == "2.0.0"
				&& record.scorer_version() == SCORER_VERSION
				&& record.config_hash() == run.scorer_config_hash
				&& record.runner_protocol_version() == Some(RUNNER_PROTOCOL_VERSION)
				&& record.merge_kernel() == Some(MeasurementKernel::SemanticTree)
				&& record.scope() == Some(MeasurementScope::FullProductMerge)
				&& record
					.engine_artifact()
					.is_some_and(|artifact| artifact.hash == run.product_hash)
		})
		.collect()
}

fn assert_complete_product_cohort(records: &[MeasurementRecord], expected_cases: usize) {
	assert_eq!(records.len(), expected_cases);
	assert_eq!(
		records
			.iter()
			.map(MeasurementRecord::snapshot_id)
			.collect::<BTreeSet<_>>()
			.len(),
		expected_cases,
		"product cohort snapshots must be unique"
	);
	assert!(records.iter().all(|record| {
		record.status() == TerminalStatus::Completed
			&& record.merged_output_hash().is_some()
			&& record.summary().is_some()
	}));
}

fn unique_cohort_id(records: &[MeasurementRecord]) -> String {
	let cohort_ids = records
		.iter()
		.map(MeasurementRecord::cohort_id)
		.collect::<BTreeSet<_>>();
	assert_eq!(cohort_ids.len(), 1, "product records must form one cohort");
	cohort_ids.into_iter().next().expect("one cohort ID")
}

fn assert_product_report(
	path: &Path,
	expected_candidate_cases: usize,
	expected_scorable_cases: usize,
	expected_report_cases: usize,
) {
	let report: serde_json::Value = serde_json::from_slice(
		&fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
	)
	.expect("parse product baseline report");
	assert_eq!(report["schema"], "2.0.0");
	assert_eq!(report["merge_kernel"], "semantic_tree");
	assert_eq!(report["scorer_version"], "2.0.0");
	assert_eq!(report["candidate_cases"], expected_candidate_cases);
	assert_eq!(report["scorable_cases"], expected_scorable_cases);
	assert_eq!(
		report["excluded_cases"],
		expected_candidate_cases - expected_scorable_cases
	);
	assert_eq!(report["total_cases"], expected_report_cases);
	assert_eq!(report["terminal_cases"], expected_report_cases);
	assert_eq!(report["completed_cases"], expected_report_cases);
	assert_eq!(report["merge_failed_cases"], 0);
	assert_eq!(report["status_counts"]["completed"], expected_report_cases);
	assert_eq!(report["baseline_complete"], true);
}

fn scorable_snapshot_ids(dataset_root: &Path, snapshot_ids: &[String]) -> BTreeSet<String> {
	let paths = DatasetPaths::new(dataset_root);
	let snapshots = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.expect("read canonical snapshots")
		.into_iter()
		.map(|snapshot| (snapshot.snapshot_id.clone(), snapshot))
		.collect::<BTreeMap<_, _>>();
	let latest_observations = read_jsonl::<ObservationRecord>(&paths.observations)
		.expect("read canonical observations")
		.into_iter()
		.fold(
			BTreeMap::<String, ObservationRecord>::new(),
			|mut latest, observation| {
				let replace = latest
					.get(&observation.snapshot_id)
					.is_none_or(|current| observation.observed_at > current.observed_at);
				if replace {
					latest.insert(observation.snapshot_id.clone(), observation);
				}
				latest
			},
		);
	snapshot_ids
		.iter()
		.filter_map(|snapshot_id| {
			let snapshot = snapshots
				.get(snapshot_id)
				.expect("fixed product snapshot exists");
			let observation = latest_observations
				.get(snapshot_id)
				.expect("fixed product snapshot has an observation");
			let scorable = assess_oracle_candidate(
				&observation.compatch.title,
				snapshot.source_mods.len(),
				observation.mod_churned,
			)
			.is_scorable();
			scorable.then(|| snapshot_id.clone())
		})
		.collect()
}

fn assert_fixed_product_snapshot_contract(dataset_root: &Path, snapshot_ids: &[String]) {
	assert_eq!(snapshot_ids.len(), FIXED_PRODUCT_SNAPSHOT_IDS.len());
	assert_eq!(
		scorable_snapshot_ids(dataset_root, snapshot_ids),
		FIXED_PRODUCT_SCORABLE_SNAPSHOT_IDS
			.into_iter()
			.map(str::to_string)
			.collect::<BTreeSet<_>>(),
		"the fixed product cohort's scorable membership changed"
	);
}

fn fixed_product_snapshot_ids(dataset_root: &Path) -> Vec<String> {
	let paths = DatasetPaths::new(dataset_root);
	let counts = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.expect("read canonical snapshots")
		.into_iter()
		.fold(BTreeMap::<String, usize>::new(), |mut counts, snapshot| {
			*counts.entry(snapshot.snapshot_id).or_default() += 1;
			counts
		});
	let required = FIXED_PRODUCT_SNAPSHOT_IDS
		.into_iter()
		.map(str::to_string)
		.collect::<Vec<_>>();
	for snapshot_id in &required {
		assert_eq!(
			counts.get(snapshot_id),
			Some(&1),
			"required product snapshot must occur exactly once: {snapshot_id}"
		);
	}
	required
}

fn exact_snapshot_ids(dataset_root: &Path, expected: usize) -> Vec<String> {
	let paths = DatasetPaths::new(dataset_root);
	let mut ids = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.expect("read collected snapshots")
		.into_iter()
		.map(|snapshot| snapshot.snapshot_id)
		.collect::<Vec<_>>();
	ids.sort();
	assert_eq!(ids.len(), expected, "unexpected collected snapshot count");
	assert_eq!(
		ids.iter().collect::<BTreeSet<_>>().len(),
		expected,
		"collected snapshot IDs must be unique"
	);
	ids
}

fn fixtures_root() -> PathBuf {
	repo_root().join("crates/foch-merge-quality/tests/fixtures")
}

fn required_basegame_root() -> PathBuf {
	let path = std::env::var_os(foch_merge_quality::config::EU4_ROOT_ENV)
		.map(PathBuf::from)
		.unwrap_or_else(|| {
			dirs::home_dir()
				.unwrap_or_else(|| PathBuf::from("."))
				.join("Library/Application Support/Steam/steamapps/common/Europa Universalis IV")
		});
	assert!(
		path.is_dir(),
		"set {} to a version-matched EU4 installation",
		foch_merge_quality::config::EU4_ROOT_ENV
	);
	path
}

fn repo_root() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.and_then(Path::parent)
		.expect("repository root")
		.to_path_buf()
}

struct ScopedEnv {
	key: &'static str,
	previous: Option<std::ffi::OsString>,
}

impl ScopedEnv {
	fn set(key: &'static str, value: &Path) -> Self {
		let previous = std::env::var_os(key);
		unsafe {
			std::env::set_var(key, value);
		}
		Self { key, previous }
	}
}

impl Drop for ScopedEnv {
	fn drop(&mut self) {
		unsafe {
			if let Some(previous) = self.previous.take() {
				std::env::set_var(self.key, previous);
			} else {
				std::env::remove_var(self.key);
			}
		}
	}
}
