#[path = "merge_quality/runner.rs"]
mod runner;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use foch_core::model::{
	MERGE_EXECUTION_ATTESTATION_SCHEMA, MERGE_REPORT_ARTIFACT_PATH, MergeReportBaseSnapshot,
	MergeReportKernel, MergeReportScope, MergeReportStatus, ProductInputManifest, ProductInputMod,
};
use foch_merge_quality::config::{DiscoveryOverrides, discover_eu4};
use foch_merge_quality::corpus::{Case, assess_oracle_candidate};
use foch_merge_quality::dataset::{
	DatasetPaths, MeasurementKernel, MeasurementRecord, MeasurementScope, ObservationRecord,
	SCORER_VERSION, SnapshotRecord, TerminalStatus, WorkshopObservationRecord, read_jsonl,
};
use foch_merge_quality::lifecycle::{
	MeasurementRequest, MeasurementRunner, TerminalMerge, WorkshopMeasureOptions,
	WorkshopReportOptions, measure_workshop_with_runner, report_workshop,
};
use foch_merge_quality::orchestrate::{
	ScoreExistingOutputRequest, score_existing_output_with_cache,
};
use foch_merge_quality::report::WorkshopReportCohort;
use foch_merge_quality::score::ScoreCache;
use foch_merge_quality::workshop_inputs::WorkshopCaseManifest;
use runner::{ProductMeasurementRunner, ProductPreviewObservation, RUNNER_PROTOCOL_VERSION};

const WORKSHOP_PREVIEW_TIMEOUT: Duration = Duration::from_secs(5 * 60);
// Confirmed publication includes structural solving, COW clone/copy-through,
// and product revalidation. Keep this distinct from the read-only plan gate.
const WORKSHOP_EXPORT_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CACHE_GATE_CASE_ID: &str = "1351632822";
const CACHE_GATE_SOURCE_IDS: [&str; 3] = ["1449952810", "1796527319", "2016264376"];
const DEFAULT_CACHE_CAP_BYTES: u64 = 1 << 30;
const FROZEN_V1_SCORABLE_SNAPSHOT_IDS: [&str; 6] = [
	"24048fe131d8365e61b3d70172e57d57f6a38ffb000f19a30ee35c20935a5803",
	"54fb216188ec4267c3006b2b05b8c994ba75961a7337839a05a9f23272d62d0c",
	"695b4ed11d04270342f5cb007ebd14001513bd91e3187434a6531e8f76ab7d7c",
	"b1f12d633dd249e644a0205708e8c23898d990eb8eea8d43134ff78188ef61f7",
	"d60369eb6a10122a1478df23c06031a84858075f09db11574e8d29cc1a260f5c",
	"ebdf8ea89e7fa5edb103708cdab2ce02dca53af16642d4aa1f5e577781033e30",
];
const FROZEN_V1_SNAPSHOT_IDS: [&str; 23] = [
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
const LEGACY_CAS_GUARD_ENV: &str = "FOCH_LEGACY_CAS_GUARD";
const LEGACY_CAS_GUARD: &str = "seatbelt-v1";

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
	let source_a_before = capture_tree_bytes(&source_a);
	let source_b_before = capture_tree_bytes(&source_b);
	let case = Case {
		compatch_id: "tiny-human".to_string(),
		title: "tiny product/scorer seam".to_string(),
		referenced_mods: vec!["910001".to_string(), "910002".to_string()],
		..Case::default()
	};
	let request = MeasurementRequest {
		input_version_id: "tiny-product-seam".to_string(),
		case: case.clone(),
		compatch_dir: compatch.clone(),
		source_dirs: vec![source_a.clone(), source_b.clone()],
		source_manifest: None,
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
	let preview = runner.run_cache_probe(&request);
	assert_eq!(
		preview.failure, None,
		"preview failed: {:?}",
		preview.failure
	);
	assert!(preview.plan_output.contains("Foch Merge Plan"));
	assert!(!preview.output_exists);
	assert!(!preview.report_exists);

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
		capture_tree_bytes(&source_a),
		source_a_before,
		"product runner must not mutate source A"
	);
	assert_eq!(
		capture_tree_bytes(&source_b),
		source_b_before,
		"product runner must not mutate source B"
	);
}

#[test]
fn frozen_v1_report_denominators_are_stable() {
	let dataset_root = repo_root().join("crates/foch-merge-quality/dataset");
	let snapshot_ids = frozen_v1_snapshot_ids(&dataset_root);
	assert_frozen_v1_snapshot_contract(&dataset_root, &snapshot_ids);
}

#[test]
#[ignore = "requires the installed EU4 base and fixed Workshop case 1351632822"]
fn workshop_product_cache_residency_gate() {
	require_acceptance("workshop-cache-residency-gate");
	assert!(
		std::env::var_os("FOCH_CACHE_MAX_BYTES").is_none(),
		"FOCH_CACHE_MAX_BYTES must be absent for the default 1 GiB gate"
	);
	assert_eq!(
		foch_engine::cache_cap_bytes(),
		DEFAULT_CACHE_CAP_BYTES,
		"cache gate must exercise the product's default 1 GiB per-layer cap"
	);
	let dataset_root = repo_root().join("crates/foch-merge-quality/dataset");
	require_legacy_cas_guard(&DatasetPaths::new(&dataset_root));
	let _env_lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let basegame = required_basegame_root();
	let discovery = discover_eu4(&DiscoveryOverrides {
		game_root: Some(basegame.clone()),
		..DiscoveryOverrides::default()
	})
	.expect("discover read-only EU4 Workshop catalog");
	let manifest =
		WorkshopCaseManifest::from_path(&fixtures_root().join("workshop-product-cases-v2.json"))
			.expect("read fixed Workshop case manifest");
	let definition = manifest
		.cases
		.iter()
		.find(|definition| definition.case_id == CACHE_GATE_CASE_ID)
		.expect("fixed cache-gate case remains in the manifest");
	assert_eq!(definition.compatch_workshop_id, CACHE_GATE_CASE_ID);
	assert_eq!(
		definition
			.source_workshop_ids
			.iter()
			.map(String::as_str)
			.collect::<Vec<_>>(),
		CACHE_GATE_SOURCE_IDS,
		"cache gate source set and precedence must remain fixed"
	);
	let compatch = discovery
		.workshop
		.require_item(CACHE_GATE_CASE_ID)
		.expect("resolve fixed cache-gate compatch from Workshop ACF");
	let sources = CACHE_GATE_SOURCE_IDS
		.iter()
		.map(|workshop_id| {
			discovery
				.workshop
				.require_item(workshop_id)
				.unwrap_or_else(|error| panic!("resolve source {workshop_id}: {error}"))
		})
		.collect::<Vec<_>>();
	let expected_installs = std::iter::once(compatch.clone())
		.chain(sources.iter().cloned())
		.collect::<Vec<_>>();
	let base_snapshot =
		foch_engine::installed_base_snapshot_identity("eu4", &discovery.game_version)
			.expect("read installed EU4 base snapshot identity")
			.unwrap_or_else(|| {
				panic!(
					"no installed base snapshot for eu4@{}",
					discovery.game_version
				)
			});
	let work = tempfile::Builder::new()
		.prefix("foch-workshop-cache-gate-")
		.tempdir()
		.expect("create cache-gate work directory");
	let source_manifest = ProductInputManifest::new(
		sources
			.iter()
			.enumerate()
			.map(|(index, source)| ProductInputMod {
				mod_id: definition.source_workshop_ids[index].clone(),
				precedence: index + 1,
				workshop_identity: source.identity.clone(),
			})
			.collect(),
	);
	let mut request = MeasurementRequest {
		input_version_id: format!("cache-residency-gate-{CACHE_GATE_CASE_ID}"),
		case: definition.to_case(),
		compatch_dir: compatch.content_path,
		source_dirs: sources
			.iter()
			.map(|source| source.content_path.clone())
			.collect(),
		source_manifest: Some(source_manifest),
		output_dir: work.path().join("cold-output"),
		basegame_root: basegame,
		expected_base_snapshot_identity: base_snapshot.as_label(),
		timeout: WORKSHOP_PREVIEW_TIMEOUT,
	};
	let runner = ProductMeasurementRunner::workshop_cache_gate()
		.expect("construct isolated default-cap cache-gate runner");

	let cold = runner.run_cache_probe(&request);
	assert_cache_gate_observation("cold", &cold, 0, 3, "parse_done");
	request.output_dir = work.path().join("warm-output");
	let warm = runner.run_cache_probe(&request);
	assert_cache_gate_observation("warm", &warm, 3, 0, "disk_hit");

	let reloaded = discovery
		.workshop
		.reload()
		.expect("reload Workshop ACF after cache gate");
	let actual_installs = std::iter::once(CACHE_GATE_CASE_ID)
		.chain(CACHE_GATE_SOURCE_IDS)
		.map(|workshop_id| {
			reloaded
				.require_item(workshop_id)
				.unwrap_or_else(|error| panic!("re-resolve Workshop item {workshop_id}: {error}"))
		})
		.collect::<Vec<_>>();
	assert_eq!(
		actual_installs, expected_installs,
		"Workshop ACF identity changed during cache gate"
	);
}

#[test]
#[ignore = "requires the installed EU4 base and fixed 14-case Workshop cohort"]
fn workshop_product_corpus_acceptance() {
	let dataset_root = repo_root().join("crates/foch-merge-quality/dataset");
	let dataset_paths = DatasetPaths::new(&dataset_root);
	require_legacy_cas_guard(&dataset_paths);
	require_acceptance("full-product-workshop");
	let _env_lock = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let basegame = required_basegame_root();
	let discovery = discover_eu4(&DiscoveryOverrides {
		game_root: Some(basegame.clone()),
		..DiscoveryOverrides::default()
	})
	.expect("discover read-only EU4 Workshop catalog");
	let object_records_before = fs::read(&dataset_paths.object_records)
		.expect("read historical object records without opening CAS payloads");
	let snapshots_before = fs::read(&dataset_paths.snapshots)
		.expect("read historical snapshot metadata without opening CAS payloads");
	let case_manifest = fixtures_root().join("workshop-product-cases-v2.json");
	let mut runner = ProductMeasurementRunner::full_product_fail_fast()
		.expect("construct fail-fast product runner");
	let measured = measure_workshop_with_runner(
		&WorkshopMeasureOptions {
			case_manifest: &case_manifest,
			dataset_root: &dataset_root,
			discovery: &discovery,
			timeout: WORKSHOP_EXPORT_TIMEOUT,
			basegame_root: &basegame,
		},
		&mut runner,
	)
	.expect("measure fixed Workshop product cohort");
	assert_eq!(measured.selected, 14);
	assert!(
		measured.measured > 0,
		"a resumed Workshop acceptance must execute at least one product case; a fully cached replay does not validate the current binary"
	);
	assert_eq!(
		measured.cached + measured.measured,
		14,
		"a resumed Workshop cohort must account for all fixed cases"
	);
	assert_eq!(measured.failed, 0, "all fixed Workshop cases must succeed");
	assert_eq!(
		measured
			.input_version_ids
			.iter()
			.collect::<BTreeSet<_>>()
			.len(),
		14,
		"every logical case must resolve to a distinct input version"
	);

	let selected_inputs = measured
		.input_version_ids
		.iter()
		.map(String::as_str)
		.collect::<BTreeSet<_>>();
	let records = read_jsonl::<MeasurementRecord>(&dataset_paths.measurements)
		.expect("read Workshop measurements")
		.into_iter()
		.filter(|record| {
			record
				.input_version_id()
				.is_some_and(|input_id| selected_inputs.contains(input_id))
				&& record.cohort_id() == measured.cohort_id
		})
		.collect::<Vec<_>>();
	assert_eq!(records.len(), 14);
	assert!(records.iter().all(|record| {
		record.status() == TerminalStatus::Completed
			&& record.input_version_id().is_some()
			&& record.merged_output_hash().is_none()
			&& record.evidence_bundle_hash().is_some()
			&& record.summary().is_some()
	}));
	let summaries = records
		.iter()
		.map(|record| {
			record
				.summary()
				.expect("completed product measurement summary")
		})
		.collect::<Vec<_>>();
	assert!(
		summaries.iter().all(|summary| {
			matches!(
				summary.merge_status.as_deref(),
				Some("ready" | "partial_success")
			)
		}),
		"Workshop acceptance requires publishable product reports; blocked is not completion"
	);
	let multi_source_files = summaries
		.iter()
		.map(|summary| summary.multi_source_files)
		.sum::<usize>();
	let accepted_multi_source_files = summaries
		.iter()
		.map(|summary| summary.accepted_multi_source_files)
		.sum::<usize>();
	assert!(
		multi_source_files > 0,
		"fixed Workshop cohort must contain multi-source scoring units"
	);
	assert!(
		accepted_multi_source_files > 0,
		"Workshop acceptance requires at least one accepted multi-source output"
	);

	let output = repo_root()
		.join("target/merge-quality/workshop-product-corpus")
		.join(&measured.cohort_id);
	report_workshop(&WorkshopReportOptions {
		case_manifest: &case_manifest,
		dataset_root: &dataset_root,
		discovery: &discovery,
		output_dir: &output,
		cohort_id: &measured.cohort_id,
		cohort: WorkshopReportCohort::AllCandidates,
	})
	.expect("write Workshop product baseline");
	assert_workshop_product_report(&output.join("baseline.json"), 14);

	assert_eq!(
		fs::read(&dataset_paths.object_records).expect("re-read historical object records"),
		object_records_before,
		"Workshop acceptance must not mutate legacy object metadata"
	);
	assert_eq!(
		fs::read(&dataset_paths.snapshots).expect("re-read historical snapshots"),
		snapshots_before,
		"Workshop acceptance must not mutate legacy snapshots"
	);
}

fn require_acceptance(expected: &str) {
	assert_eq!(
		std::env::var(ACCEPTANCE_ENV).unwrap_or_default(),
		expected,
		"run the matching fixed script under scripts/merge-quality"
	);
}

fn assert_cache_gate_observation(
	phase: &str,
	observation: &ProductPreviewObservation,
	expected_hits: usize,
	expected_misses: usize,
	expected_event: &str,
) {
	assert_eq!(
		observation.failure, None,
		"{phase} cache-gate preview failed: {:?}",
		observation.failure
	);
	assert!(
		observation.plan_output.contains("Foch Merge Plan"),
		"{phase} cache-gate preview did not return a merge plan"
	);
	assert!(
		!observation.plan_output.contains("<stdout truncated"),
		"{phase} cache-gate preview plan exceeded the bounded capture"
	);
	assert!(
		!observation.output_exists,
		"{phase} cache-gate preview must not materialize the output directory"
	);
	assert!(
		!observation.report_exists,
		"{phase} cache-gate preview must not write a merge report"
	);
	let diagnostics = &observation.cache_diagnostics;
	assert!(
		!diagnostics.contains("truncated"),
		"{phase} cache diagnostics were truncated; refusing an incomplete assertion"
	);
	let cache_store_lines = diagnostics
		.lines()
		.filter(|line| line.starts_with("[merge] mod_snapshot: cache_store "))
		.collect::<Vec<_>>();
	assert!(
		cache_store_lines
			.iter()
			.all(|line| line.contains(" state=stored ")),
		"{phase} cache gate observed a non-resident semantic snapshot store:\n{diagnostics}"
	);
	let workspace_summary =
		format!("mod_parse_cache_hits={expected_hits} mod_parse_cache_misses={expected_misses}");
	assert_eq!(
		diagnostics
			.lines()
			.filter(|line| line.contains(&workspace_summary))
			.count(),
		1,
		"{phase} cache-gate workspace summary mismatch:\n{diagnostics}"
	);
	assert_diagnostic_mod_ids(
		phase,
		"start",
		diagnostic_mod_ids(diagnostics, |line| {
			line.starts_with("[merge] mod_snapshot: start ")
		}),
	);
	match expected_event {
		"parse_done" => {
			assert_diagnostic_mod_ids(
				phase,
				"cold parse",
				diagnostic_mod_ids(diagnostics, |line| {
					line.starts_with("[merge] mod_snapshot: parse_done ")
				}),
			);
			assert_diagnostic_mod_ids(
				phase,
				"cold store",
				cache_store_lines
					.iter()
					.map(|line| diagnostic_mod_id(line))
					.collect(),
			);
			assert!(
				!diagnostics.contains("[merge] mod_snapshot: cache_hit "),
				"cold cache gate unexpectedly hit a semantic snapshot:\n{diagnostics}"
			);
		}
		"disk_hit" => {
			assert!(
				cache_store_lines.is_empty(),
				"warm cache gate unexpectedly attempted semantic snapshot stores:\n{diagnostics}"
			);
			assert_diagnostic_mod_ids(
				phase,
				"warm disk hit",
				diagnostic_mod_ids(diagnostics, |line| {
					line.starts_with("[merge] mod_snapshot: cache_hit ")
						&& line.contains(" source=disk ")
				}),
			);
			assert!(
				!diagnostics.contains(" source=process "),
				"warm cache gate must prove cross-process disk hits:\n{diagnostics}"
			);
			assert!(
				!diagnostics.contains("[merge] mod_snapshot: parse_done "),
				"warm cache gate unexpectedly rebuilt a semantic snapshot:\n{diagnostics}"
			);
		}
		other => panic!("unsupported cache-gate event {other}"),
	}
}

fn diagnostic_mod_ids(diagnostics: &str, select: impl Fn(&str) -> bool) -> Vec<String> {
	diagnostics
		.lines()
		.filter(|line| select(line))
		.map(diagnostic_mod_id)
		.collect()
}

fn diagnostic_mod_id(line: &str) -> String {
	line.split_ascii_whitespace()
		.find_map(|field| field.strip_prefix("mod_id="))
		.unwrap_or_else(|| panic!("cache diagnostic has no mod_id field: {line}"))
		.to_string()
}

fn assert_diagnostic_mod_ids(phase: &str, event: &str, actual: Vec<String>) {
	let expected = CACHE_GATE_SOURCE_IDS
		.into_iter()
		.map(str::to_string)
		.collect::<BTreeSet<_>>();
	assert_eq!(
		actual.len(),
		expected.len(),
		"{phase} {event} diagnostics must contain one line per fixed source"
	);
	assert_eq!(
		actual.into_iter().collect::<BTreeSet<_>>(),
		expected,
		"{phase} {event} diagnostics covered the wrong source IDs"
	);
}

fn require_legacy_cas_guard(paths: &DatasetPaths) {
	assert_eq!(
		std::env::var(LEGACY_CAS_GUARD_ENV).unwrap_or_default(),
		LEGACY_CAS_GUARD,
		"run scripts/merge-quality/acceptance.fish; raw cargo execution is not a valid acceptance because it does not prove legacy CAS isolation"
	);
	assert_legacy_path_inaccessible(&paths.legacy_objects);
	assert_legacy_path_inaccessible(&paths.legacy_work);
}

fn assert_legacy_path_inaccessible(path: &Path) {
	match fs::symlink_metadata(path) {
		Err(error)
			if matches!(
				error.kind(),
				std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
			) => {}
		Err(error) => panic!(
			"legacy path {} was not demonstrably denied by Seatbelt: {error}",
			path.display()
		),
		Ok(_) => panic!(
			"legacy path {} remained accessible despite {LEGACY_CAS_GUARD_ENV}={LEGACY_CAS_GUARD}",
			path.display()
		),
	}
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

fn capture_tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
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

fn assert_workshop_product_report(path: &Path, expected_cases: usize) {
	let report: serde_json::Value = serde_json::from_slice(
		&fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
	)
	.expect("parse Workshop product baseline report");
	assert_eq!(report["schema"], "3.0.0");
	assert_eq!(report["merge_kernel"], "semantic_tree");
	assert_eq!(report["scorer_version"], SCORER_VERSION);
	assert_eq!(report["candidate_cases"], expected_cases);
	assert_eq!(report["total_cases"], expected_cases);
	assert_eq!(report["terminal_cases"], expected_cases);
	assert_eq!(report["completed_cases"], expected_cases);
	assert_eq!(report["merge_failed_cases"], 0);
	assert_eq!(report["status_counts"]["completed"], expected_cases);
	assert_eq!(report["baseline_complete"], true);
	let multi_source_total = report["multi_source"]["total"]
		.as_u64()
		.expect("Workshop report multi-source total");
	let multi_source_accepted = report["multi_source"]["accepted"]
		.as_u64()
		.expect("Workshop report accepted multi-source count");
	assert!(
		multi_source_total > 0,
		"fixed Workshop report must contain multi-source scoring units"
	);
	assert!(
		multi_source_accepted > 0,
		"Workshop report must contain at least one accepted multi-source output"
	);
	let cases = report["cases"].as_array().expect("Workshop report cases");
	assert!(cases.iter().all(|case| {
		case.get("snapshot_id").is_none()
			&& case["input_version_id"]
				.as_str()
				.is_some_and(|value| value.len() == 64)
			&& case["evidence_bundle_hash"]
				.as_str()
				.is_some_and(|value| value.len() == 64)
			&& matches!(
				case["summary"]["merge_status"].as_str(),
				Some("ready" | "partial_success")
			)
	}));
}

fn scorable_snapshot_ids(dataset_root: &Path, snapshot_ids: &[String]) -> BTreeSet<String> {
	let paths = DatasetPaths::new(dataset_root);
	let snapshots = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.expect("read canonical snapshots")
		.into_iter()
		.map(|snapshot| (snapshot.snapshot_id.clone(), snapshot))
		.collect::<BTreeMap<_, _>>();
	let latest_observations = read_jsonl::<WorkshopObservationRecord>(&paths.observations)
		.expect("read canonical observations")
		.into_iter()
		.filter_map(|observation| match observation {
			WorkshopObservationRecord::V1(observation) => Some(observation),
			WorkshopObservationRecord::V2(_) => None,
		})
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

fn assert_frozen_v1_snapshot_contract(dataset_root: &Path, snapshot_ids: &[String]) {
	assert_eq!(snapshot_ids.len(), FROZEN_V1_SNAPSHOT_IDS.len());
	assert_eq!(
		scorable_snapshot_ids(dataset_root, snapshot_ids),
		FROZEN_V1_SCORABLE_SNAPSHOT_IDS
			.into_iter()
			.map(str::to_string)
			.collect::<BTreeSet<_>>(),
		"the fixed product cohort's scorable membership changed"
	);
}

fn frozen_v1_snapshot_ids(dataset_root: &Path) -> Vec<String> {
	let paths = DatasetPaths::new(dataset_root);
	let counts = read_jsonl::<SnapshotRecord>(&paths.snapshots)
		.expect("read canonical snapshots")
		.into_iter()
		.fold(BTreeMap::<String, usize>::new(), |mut counts, snapshot| {
			*counts.entry(snapshot.snapshot_id).or_default() += 1;
			counts
		});
	let required = FROZEN_V1_SNAPSHOT_IDS
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
