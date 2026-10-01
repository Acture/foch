//! Bounded diagnostic for the fixed cache-gate case. Run in separate processes
//! with the same FOCH_CACHE_ROOT to compare cold and disk-backed input loading.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use foch::game::eu4::base::snapshot::{
	InstalledBaseSnapshotIdentity, installed_base_snapshot_identity,
};
use foch::input::{Config, InputRequest};
use foch::merge::{
	AnalyzedMerge, CancellationToken, CommitAuthorization, CommitResult, MergeAnalysisOptions,
	NoopProgressObserver, analyze_merge,
};
use foch::model::GamePathBuf;
use foch::project::{Project, ProjectConfig, ProjectMod};

use super::fixtures_root;
use super::merge_quality::config::{DiscoveryOverrides, Eu4Discovery, discover_eu4};
use super::merge_quality::workshop_inputs::{
	ResolvedWorkshopCase, WorkshopCaseDefinition, WorkshopCaseManifest,
};

#[test]
#[ignore = "requires installed cache-gate Workshop sources and EU4 base; retained positions only"]
fn workshop_positions_cache_probe() {
	assert!(
		std::env::var_os(foch::platform::cache_store::CACHE_ROOT_ENV).is_some(),
		"set FOCH_CACHE_ROOT to an isolated cache and reuse it in a second process"
	);
	let discovery: Eu4Discovery =
		discover_eu4(&DiscoveryOverrides::default()).expect("discover EU4");
	let manifest: WorkshopCaseManifest =
		WorkshopCaseManifest::from_path(&fixtures_root().join("workshop-product-cases-v2.json"))
			.expect("fixed case manifest");
	let definition: &WorkshopCaseDefinition = manifest
		.cases
		.iter()
		.find(|case| case.case_id == "1351632822")
		.expect("fixed cache-gate case");
	let case: ResolvedWorkshopCase = ResolvedWorkshopCase::resolve(&discovery.workshop, definition)
		.expect("resolve paired ACF identities");
	let base: InstalledBaseSnapshotIdentity =
		installed_base_snapshot_identity("eu4", &discovery.game_version)
			.expect("base identity")
			.expect("installed base");
	let scratch_root: PathBuf =
		PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/validation/p736-timeout-fix");
	fs::create_dir_all(&scratch_root).expect("probe root");
	let scratch: PathBuf = tempfile::Builder::new()
		.prefix("positions-")
		.tempdir_in(scratch_root)
		.expect("probe directory")
		.keep();
	let project: PathBuf = scratch.join("foch.toml");
	let config: Project = Project {
		project: Some(ProjectConfig {
			game: Some(foch::game::eu4::Eu4),
			game_path: Some(discovery.game_root.clone()),
			mods: case
				.sources
				.iter()
				.enumerate()
				.map(|(position, source)| ProjectMod {
					id: Some(source.install.identity.workshop_id.to_string()),
					steam_id: Some(source.install.identity.workshop_id.to_string()),
					path: Some(source.install.content_path.clone()),
					workshop_identity: Some(source.install.identity.clone()),
					enabled: true,
					position: Some(position),
				})
				.collect(),
			..ProjectConfig::default()
		}),
		..Project::default()
	};
	fs::write(&project, toml::to_string_pretty(&config).unwrap()).unwrap();
	eprintln!("positions probe artifacts: {}", scratch.display());
	let started: Instant = Instant::now();
	let analyzed: AnalyzedMerge = analyze_merge(
		InputRequest::from_manifest_path(
			project,
			Config {
				game_path: HashMap::from([("eu4".into(), discovery.game_root)]),
				..Config::default()
			},
		)
		.with_expected_base_snapshot_identity(base.as_label()),
		MergeAnalysisOptions {
			out_dir: scratch.join("out"),
			include_game_base: true,
			include_base: false,
			gui_scroll_merge: false,
			force: false,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_config_path: None,
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			playset_fingerprint: None,
			provenance: false,
			merge_workers: foch::merge::default_merge_workers(),
			retained_paths: Some(BTreeSet::from([
				GamePathBuf::parse("map/positions.txt").unwrap()
			])),
		},
		&NoopProgressObserver,
		&CancellationToken::default(),
	)
	.expect("analyze retained positions");
	eprintln!(
		"positions analysis: {:?}, {:?}",
		started.elapsed(),
		analyzed.list_units()
	);
	assert_eq!(analyzed.list_units().len(), 1);
	let result: CommitResult = analyzed
		.commit(CommitAuthorization::EmptyTargetOnly)
		.expect("commit retained output");
	fs::write(
		scratch.join("report.json"),
		serde_json::to_vec_pretty(&result.report).unwrap(),
	)
	.unwrap();
	case.validate_unchanged(&discovery.workshop)
		.expect("ACF identities unchanged");
}
