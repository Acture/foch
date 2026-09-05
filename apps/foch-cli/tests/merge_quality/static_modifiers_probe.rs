//! P-553 family probe using the public analyze/commit path and installed sources.
//! Retention bounds the merge output; cold input resolution still builds complete
//! source semantic snapshots. No cohort JSONL is written.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use foch::game::eu4::base::snapshot::{
	InstalledBaseSnapshotIdentity, installed_base_snapshot_identity,
};
use foch::game::eu4::script::parser::{AstStatement, AstValue};
use foch::game::eu4::script::{ParsedScriptFile, parse_script_file};
use foch::input::{Config, InputRequest};
use foch::merge::{
	AnalyzedMerge, CancellationToken, CommitAuthorization, CommitResult, MergeAnalysisOptions,
	MergeDisposition, NoopProgressObserver, analyze_merge,
};
use foch::project::{Project, ProjectConfig, ProjectMod};

use super::merge_quality::config::{DiscoveryOverrides, Eu4Discovery, discover_eu4};
use super::merge_quality::dataset::InputVersionRecord;
use super::merge_quality::workshop_inputs::{
	ResolvedWorkshopCase, WorkshopCaseDefinition, WorkshopCaseManifest,
};
use super::{capture_tree_bytes, fixtures_root};

const FAMILY: &str = "common/static_modifiers";
const OUTPUT: &str = "common/static_modifiers/zzz_foch_static_modifiers.txt";

#[test]
#[ignore = "requires installed Workshop case 3630934157 and a current EU4 base snapshot"]
fn workshop_static_modifiers_product_probe() {
	let discovery: Eu4Discovery =
		discover_eu4(&DiscoveryOverrides::default()).expect("discover installed EU4 and Workshop");
	let manifest: WorkshopCaseManifest =
		WorkshopCaseManifest::from_path(&fixtures_root().join("workshop-product-cases-v2.json"))
			.expect("fixed case manifest");
	let definition: &WorkshopCaseDefinition = manifest
		.cases
		.iter()
		.find(|case| case.case_id == "3630934157")
		.expect("fixed RCE/EE case");
	let case: ResolvedWorkshopCase = ResolvedWorkshopCase::resolve(&discovery.workshop, definition)
		.expect("verify paired ACF identities and input availability");
	let base: InstalledBaseSnapshotIdentity =
		installed_base_snapshot_identity("eu4", &discovery.game_version)
			.expect("verify base identity")
			.expect("installed ancestor required");
	let scratch_root: PathBuf =
		PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/p553-workshop");
	fs::create_dir_all(&scratch_root).expect("create probe root");
	let scratch: PathBuf = tempfile::Builder::new()
		.prefix("static-modifiers-")
		.tempdir_in(scratch_root)
		.expect("probe directory")
		.keep();
	eprintln!("P-553 bounded probe artifacts: {}", scratch.display());
	eprintln!(
		"P-553: retained merge scope is one family; cold source snapshot loading can take minutes"
	);
	let project: PathBuf = scratch.join("foch.toml");
	let project_config: Project = Project {
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
	fs::write(
		&project,
		toml::to_string_pretty(&project_config).expect("serialize exact ordered input"),
	)
	.expect("write probe manifest");
	let roots: Vec<PathBuf> = std::iter::once(discovery.game_root.clone())
		.chain(
			case.sources
				.iter()
				.map(|source| source.install.content_path.clone()),
		)
		.collect();
	// Only this small content family is byte-checked. ACF remains Workshop identity.
	let before: Vec<BTreeMap<String, Vec<u8>>> = roots
		.iter()
		.map(|root| capture_tree_bytes(&root.join(FAMILY)))
		.collect();
	let retained: BTreeSet<String> = before
		.iter()
		.flat_map(|files| files.keys().map(|path| format!("{FAMILY}/{path}")))
		.collect();
	let input: InputVersionRecord = case
		.input_version(&discovery.game_version, discovery.steam_build_id)
		.expect("record input identity");
	fs::write(
		scratch.join("input.json"),
		serde_json::to_vec_pretty(&input).unwrap(),
	)
	.unwrap();
	let config: Config = Config {
		game_path: HashMap::from([("eu4".into(), discovery.game_root)]),
		..Config::default()
	};
	let out: PathBuf = scratch.join("out");
	let analyzed: AnalyzedMerge = analyze_merge(
		InputRequest::from_manifest_path(project, config)
			.with_expected_base_snapshot_identity(base.as_label()),
		MergeAnalysisOptions {
			out_dir: out.clone(),
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
			provenance: true,
			retained_paths: Some(retained),
		},
		&NoopProgressObserver,
		&CancellationToken::default(),
	)
	.expect("analyze retained static modifiers with installed base");
	assert!(!out.exists());
	assert_eq!(
		analyzed.list_units().len(),
		1,
		"probe must remain one family"
	);
	let disposition: MergeDisposition = analyzed.list_units()[0].disposition;
	fs::write(
		scratch.join("review.txt"),
		format!("{:#?}", analyzed.list_units()),
	)
	.unwrap();
	let result: CommitResult = analyzed
		.commit(CommitAuthorization::EmptyTargetOnly)
		.expect("commit bounded analyzed result");
	let mut definitions: BTreeMap<String, Vec<BTreeMap<String, String>>> = BTreeMap::new();
	if disposition == MergeDisposition::Safe {
		let parsed: ParsedScriptFile =
			parse_script_file("probe", &out, &out.join(OUTPUT)).expect("reparse generated module");
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
		for statement in &parsed.ast.statements {
			if let AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} = statement
			{
				let values: BTreeMap<String, String> = items
					.iter()
					.filter_map(|item| match item {
						AstStatement::Assignment {
							key,
							value: AstValue::Scalar { value, .. },
							..
						} => Some((key.clone(), value.as_text())),
						_ => None,
					})
					.collect();
				definitions.entry(key.clone()).or_default().push(values);
			}
		}
	} else {
		assert!(
			!out.join(OUTPUT).exists(),
			"unsafe module must remain omitted"
		);
	}
	let after: Vec<BTreeMap<String, Vec<u8>>> = roots
		.iter()
		.map(|root| capture_tree_bytes(&root.join(FAMILY)))
		.collect();
	assert_eq!(before, after, "source family bytes must remain unchanged");
	case.validate_unchanged(&discovery.workshop)
		.expect("revalidate paired ACF records");
	let evidence: serde_json::Value = serde_json::json!({
		"scope": "bounded_static_modifiers_only", "base_snapshot": base.as_label(),
		"disposition": format!("{disposition:?}"), "report": result.report,
		"definitions": definitions, "source_family_bytes_unchanged": true,
		"workshop_acf_unchanged": true,
	});
	fs::write(
		scratch.join("evidence.json"),
		serde_json::to_vec_pretty(&evidence).unwrap(),
	)
	.unwrap();
	eprintln!(
		"P-553 probe disposition={disposition:?}; evidence={}",
		scratch.join("evidence.json").display()
	);
	assert_eq!(
		disposition,
		MergeDisposition::Safe,
		"inspect the recorded probe evidence"
	);
	assert!(
		definitions.values().all(|variants| variants.len() == 1),
		"output must not duplicate definitions"
	);
	let prestige: &BTreeMap<String, String> = &definitions["prestige"][0];
	for (key, expected) in [
		("land_morale", 0.1),
		("rce_monthly_religion_mechanic_sylvan_affinity_change", 2.0),
		("monthly_frankish_chivalry", 0.05),
	] {
		assert_eq!(
			prestige[key].parse::<f64>().expect("numeric modifier"),
			expected,
			"retained prestige contribution: {key}"
		);
	}
	assert_eq!(
		result.report.definition_provenance[OUTPUT]["prestige"],
		["3342969370", "2164202838"]
	);
}
