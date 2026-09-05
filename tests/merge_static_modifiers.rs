//! Base-aware public analyze/commit coverage. This process owns its synthetic
//! base-data environment; keep the matrix in one test before Rayon starts.

#[path = "support/static_modifiers.rs"]
mod static_modifiers;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use foch::game::eu4::Eu4;
use foch::game::eu4::base::snapshot::{
	BaseDataSource, BaseSnapshotBuildResult, build_base_snapshot, install_built_snapshot,
};
use foch::input::{Config, FileFilter, InputRequest};
use foch::merge::{
	AnalyzedMerge, CancellationToken, CommitAuthorization, CommitResult, ConflictDecision,
	ConflictHandler, ConflictView, MergeAnalysisOptions, MergeDisposition, MergeUnitKind,
	MergeUnitOutcome, NoopProgressObserver, analyze_merge,
};
use static_modifiers::{CASES, OUTPUT, SOURCE, assert_output, source_bytes, write_manifest};
use tempfile::TempDir;

struct CaptureConflicts(Arc<Mutex<Vec<ConflictView>>>);

impl ConflictHandler for CaptureConflicts {
	fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
		self.0
			.lock()
			.expect("captured conflicts lock")
			.push(view.clone());
		ConflictDecision::Defer { record: None }
	}
}

#[test]
fn static_modifiers_preserve_contributions_through_analyze_and_commit() {
	let scratch: TempDir = TempDir::new().expect("test scratch");
	let fixture: PathBuf =
		PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/static_modifiers");
	let original: BTreeMap<PathBuf, Vec<u8>> = source_bytes(&fixture);
	let game_root: PathBuf = fixture.join("base");
	// This integration-test executable has one test and has not started workers.
	unsafe {
		std::env::set_var("FOCH_DATA_DIR", scratch.path().join("data"));
		std::env::set_var("FOCH_CACHE_ROOT", scratch.path().join("cache"));
	}
	let built: BaseSnapshotBuildResult = build_base_snapshot(
		&Eu4,
		&game_root,
		Some("p553-1.0"),
		&FileFilter::for_game(Eu4),
	)
	.expect("build explicit fixture ancestor");
	install_built_snapshot(
		&built.encoded_snapshot,
		BaseDataSource::Build,
		Some(built.snapshot_asset_name),
		Some(built.snapshot_sha256),
	)
	.expect("install fixture ancestor");
	let config: Config = Config {
		game_path: HashMap::from([("eu4".into(), game_root)]),
		..Config::default()
	};
	for case in CASES {
		let manifest: PathBuf = write_manifest(&fixture, &scratch.path().join(case.name), case);
		let mut previous_units: Option<Vec<MergeUnitOutcome>> = None;
		let mut previous_output: Option<Vec<u8>> = None;
		let mut previous_evidence: Option<serde_json::Value> = None;
		for run in 0..2 {
			let captured: Arc<Mutex<Vec<ConflictView>>> = Arc::default();
			let out: PathBuf = scratch.path().join(case.name).join(format!("out-{run}"));
			let analyzed: AnalyzedMerge = analyze_merge(
				InputRequest::from_manifest_path(manifest.clone(), config.clone()),
				MergeAnalysisOptions {
					out_dir: out.clone(),
					include_game_base: true,
					provenance: true,
					include_base: false,
					gui_scroll_merge: false,
					force: false,
					ignore_replace_path: false,
					dep_overrides: Vec::new(),
					resolution_config_path: None,
					interactive_conflict_handler: Some(Box::new(CaptureConflicts(
						captured.clone(),
					))),
					interactive_resolution_config_path: Some(scratch.path().join("decisions.toml")),
					playset_fingerprint: None,
					retained_paths: None,
				},
				&NoopProgressObserver,
				&CancellationToken::default(),
			)
			.unwrap_or_else(|error| panic!("{}: {error}", case.name));
			assert!(!out.exists(), "analysis must leave target untouched");
			let views: Vec<ConflictView> = captured.lock().expect("captured views").clone();
			assert_eq!(
				views.len(),
				usize::from(case.expected.is_none()),
				"only final disagreements are prompted: {}",
				case.name
			);
			if let Some(view) = views.first() {
				assert_eq!(
					view.vanilla_snippet.as_deref(),
					Some("global_tax_modifier = 0.10")
				);
				let candidates: Vec<&str> = view
					.candidates
					.iter()
					.map(|candidate| candidate.mod_id.as_str())
					.collect();
				let expected: Vec<&str> = case
					.mods
					.iter()
					.copied()
					.filter(|id| *id != "vanilla")
					.collect();
				assert_eq!(candidates, expected, "only actual changes are selectable");
				for candidate in &view.candidates {
					let expected: &str = match candidate.mod_id.as_str() {
						"tax" | "equal" => "global_tax_modifier = 0.15",
						"up" => "global_tax_modifier = 0.20",
						"down" => "global_tax_modifier = 0.05",
						"last" => "global_tax_modifier = 0.25",
						"deleted" => "(removed)",
						id => panic!("unexpected final candidate {id}"),
					};
					assert_eq!(candidate.candidate_rendered, expected);
				}
			}
			let units: Vec<MergeUnitOutcome> = analyzed.list_units().to_vec();
			let unit: &MergeUnitOutcome = units
				.iter()
				.find(|unit| unit.family == "common/static_modifiers")
				.expect("static modifiers review unit");
			assert_eq!(unit.kind, MergeUnitKind::DefinitionModule);
			assert_eq!(
				unit.disposition,
				if case.expected.is_some() {
					MergeDisposition::Safe
				} else {
					MergeDisposition::NeedsUserChoice
				},
				"{}: {unit:#?}",
				case.name
			);
			assert_eq!(unit.output_path.as_deref(), case.expected.map(|_| OUTPUT));
			assert!(
				unit.contributors.iter().any(|source| source.is_base_game
					&& source
						.source_paths
						.iter()
						.any(|path| std::path::Path::new(path).ends_with(SOURCE))),
				"explicit ancestor must be visible: {unit:#?}"
			);
			let mods: Vec<&str> = unit
				.contributors
				.iter()
				.filter(|source| !source.is_base_game)
				.map(|source| source.mod_id.as_str())
				.collect();
			assert_eq!(mods, case.mods, "review must preserve input order");
			assert!(
				unit.contributors
					.windows(2)
					.all(|pair| pair[0].precedence < pair[1].precedence)
			);
			if let Some(previous) = &previous_units {
				assert_eq!(
					&units, previous,
					"review IDs, contributors and outcomes must be stable"
				);
			}
			previous_units = Some(units);
			let result: CommitResult = analyzed
				.commit(CommitAuthorization::EmptyTargetOnly)
				.expect("commit analyzed bytes");
			assert_output(case, &out, &result.report);
			let evidence: serde_json::Value = serde_json::json!({
				"provenance": result.report.definition_provenance,
				"trace": result.report.merge_trace,
				"conflicts": result.report.conflict_resolutions,
			});
			if let Some(previous) = &previous_evidence {
				assert_eq!(
					&evidence, previous,
					"stable conflict IDs and contribution evidence"
				);
			}
			previous_evidence = Some(evidence);
			if case.expected.is_some() {
				let bytes: Vec<u8> = std::fs::read(out.join(OUTPUT)).expect("read output");
				if let Some(previous) = &previous_output {
					assert_eq!(&bytes, previous, "deterministic module output");
				}
				previous_output = Some(bytes);
			}
		}
	}
	assert_eq!(
		source_bytes(&fixture),
		original,
		"base and mods must remain read-only"
	);
}
