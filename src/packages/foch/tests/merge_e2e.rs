use foch::game::eu4::content::ContentLoadPolicy;
use foch::game::eu4::content::eu4;
use foch::game::eu4::script::definition_module::{DefinitionModuleInput, load_definition_module};
use foch::game::eu4::script::parse_script_file;
use foch::game::eu4::script::parser::parse_clausewitz_file;
use foch::input::{Config, InputRequest};
use foch::merge::{
	AnalyzedMerge, CancellationToken, CommitAuthorization, ConflictDecision, ConflictHandler,
	ConflictView, MergeAnalysisOptions, MergeBackendId, MergeDisposition, NoopProgressObserver,
	analyze_merge, run_merge_for_evaluation, run_merge_with_options,
};
use foch::model::{
	ConflictKind, GamePath, GamePathBuf, MergeReportStatus, MergeTraceDecision, MergeTraceEntry,
	MergeTracePolicy,
};
use foch::playset::descriptor::load_descriptor;
use foch::project::compute_conflict_id;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tempfile::{Builder, TempDir};

static MERGE_TEMP_DIRS: OnceLock<Mutex<Vec<TempDir>>> = OnceLock::new();

struct UseFileHandler {
	path: PathBuf,
}

impl ConflictHandler for UseFileHandler {
	fn on_conflict(&mut self, _view: &ConflictView) -> ConflictDecision {
		ConflictDecision::UseFile(self.path.clone())
	}
}

fn playsets_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
		.join("playsets")
}

fn fixture_dir(name: &str) -> PathBuf {
	playsets_root().join(name)
}

/// A fixture's game path, spelled in `/` syntax.
fn game_path(rel: &str) -> &GamePath {
	GamePath::new(rel).expect("fixture path is a game path")
}

fn expected_path(name: &str, rel: &str) -> PathBuf {
	game_path(rel).to_path(fixture_dir(name).join("expected"))
}

// Reads a fixture playset from `tests/fixtures/playsets/<name>/`,
// runs the production merge pipeline into a tempdir, returns the output dir path.
fn run_merge_fixture(name: &str) -> PathBuf {
	let (result, out_dir) = run_merge_for_fixture(name, /*force=*/ true);
	assert_eq!(
		result.exit_code, 0,
		"merge fixture {name} should exit cleanly; report: {:#?}",
		result.report
	);
	assert_ne!(
		result.report.status,
		MergeReportStatus::Fatal,
		"merge fixture {name} produced a fatal report: {:#?}",
		result.report
	);
	out_dir
}

/// Lower-level harness used by both the strict copy-through tests and the
/// conflict-scenario tests. Returns the full [`CommitResult`] plus
/// the output dir so tests can assert on report fields, status, and the
/// produced tree without the wrapper enforcing its own success contract.
fn run_merge_for_fixture(name: &str, force: bool) -> (foch::merge::CommitResult, PathBuf) {
	run_merge_for_fixture_inner(
		name, force, /*provenance=*/ false, /*gui_scroll_merge=*/ false,
	)
}

fn run_merge_for_fixture_with_gui_scroll(
	name: &str,
	force: bool,
	gui_scroll_merge: bool,
) -> (foch::merge::CommitResult, PathBuf) {
	run_merge_for_fixture_inner(name, force, /*provenance=*/ false, gui_scroll_merge)
}

fn run_merge_for_fixture_with_provenance(
	name: &str,
	force: bool,
) -> (foch::merge::CommitResult, PathBuf) {
	run_merge_for_fixture_inner(
		name, force, /*provenance=*/ true, /*gui_scroll_merge=*/ false,
	)
}

fn run_merge_for_fixture_inner(
	name: &str,
	force: bool,
	provenance: bool,
	gui_scroll_merge: bool,
) -> (foch::merge::CommitResult, PathBuf) {
	let fixture = fixture_dir(name);
	assert!(
		fixture.is_dir(),
		"fixture does not exist: {}",
		fixture.display()
	);

	let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("merge-e2e");
	fs::create_dir_all(&scratch_root).expect("create merge e2e scratch root");
	let temp_dir = Builder::new()
		.prefix(&format!("{name}-"))
		.tempdir_in(&scratch_root)
		.expect("create merge e2e tempdir");
	let out_dir = temp_dir.path().join("out");
	let game_root = temp_dir.path().join("eu4-game");
	fs::create_dir_all(&game_root).expect("create fixture game root");

	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_root);
	let result = run_merge_with_options(
		InputRequest::from_playset_path(
			fixture.join("dlc_load.json"),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		),
		MergeAnalysisOptions {
			out_dir: out_dir.clone(),
			include_game_base: false,
			include_base: false,
			gui_scroll_merge,
			force,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_config_path: None,
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			playset_fingerprint: None,
			provenance,
			merge_workers: std::num::NonZeroUsize::new(2).unwrap(),
			retained_paths: None,
		},
	)
	.unwrap_or_else(|err| panic!("merge fixture {name} failed: {err}"));

	MERGE_TEMP_DIRS
		.get_or_init(|| Mutex::new(Vec::new()))
		.lock()
		.expect("merge tempdir registry lock")
		.push(temp_dir);

	(result, out_dir)
}

fn run_merge_for_playset(
	playset_path: &Path,
	out_dir: PathBuf,
	game_root: PathBuf,
	force: bool,
	resolution_config_path: Option<PathBuf>,
) -> foch::merge::CommitResult {
	analyze_merge_for_playset(
		playset_path,
		out_dir,
		game_root,
		force,
		resolution_config_path,
	)
	.commit(CommitAuthorization::EmptyTargetOnly)
	.expect("commit analyzed fixture merge")
}

fn analyze_merge_for_playset(
	playset_path: &Path,
	out_dir: PathBuf,
	game_root: PathBuf,
	force: bool,
	resolution_config_path: Option<PathBuf>,
) -> AnalyzedMerge {
	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_root);
	analyze_merge(
		InputRequest::from_playset_path(
			playset_path.to_path_buf(),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		),
		MergeAnalysisOptions {
			out_dir,
			include_game_base: false,
			include_base: false,
			gui_scroll_merge: false,
			force,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_config_path,
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			playset_fingerprint: None,
			provenance: false,
			merge_workers: std::num::NonZeroUsize::new(2).unwrap(),
			retained_paths: None,
		},
		&NoopProgressObserver,
		&CancellationToken::new(),
	)
	.expect("analyze custom playset")
}

fn copy_dir_recursive(source: &Path, destination: &Path) {
	fs::create_dir_all(destination)
		.unwrap_or_else(|err| panic!("create {}: {err}", destination.display()));
	for entry in
		fs::read_dir(source).unwrap_or_else(|err| panic!("read_dir {}: {err}", source.display()))
	{
		let entry = entry.expect("read_dir entry");
		let source_path = entry.path();
		let destination_path = destination.join(entry.file_name());
		if source_path.is_dir() {
			copy_dir_recursive(&source_path, &destination_path);
		} else {
			fs::copy(&source_path, &destination_path).unwrap_or_else(|err| {
				panic!(
					"copy {} to {}: {err}",
					source_path.display(),
					destination_path.display()
				)
			});
		}
	}
}

/// A host path as the text a foch.toml `use_file` holds.
fn toml_path(path: &Path) -> String {
	path.to_str().expect("a temp path is UTF-8").to_owned()
}

/// `path` as a quoted TOML string.
fn toml_path_value(path: &Path) -> String {
	toml::Value::String(toml_path(path)).to_string()
}

// Recursively scans output dir and asserts every structural EU4 .txt script file
// has balanced braces, paired quotes, and no parser diagnostics.
fn assert_structurally_sound(out_dir: &Path) {
	let mut files = Vec::new();
	collect_structural_text_files(out_dir, out_dir, &mut files);
	files.sort();
	assert!(
		!files.is_empty(),
		"no structural .txt files found under {}",
		out_dir.display()
	);

	for path in files {
		let content = fs::read_to_string(&path)
			.unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
		assert_balanced_braces(&path, &content);
		assert_even_unescaped_quote_count(&path, &content);
		assert_reparses_cleanly(&path);
	}
}

fn collect_structural_text_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) {
	for entry in fs::read_dir(dir).unwrap_or_else(|err| panic!("read_dir {}: {err}", dir.display()))
	{
		let entry = entry.expect("read_dir entry");
		let path = entry.path();
		if path.is_dir() {
			collect_structural_text_files(root, &path, files);
		} else if is_structural_text_file(root, &path) {
			files.push(path);
		}
	}
}

fn is_structural_text_file(root: &Path, path: &Path) -> bool {
	if !path
		.extension()
		.and_then(|ext| ext.to_str())
		.is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
	{
		return false;
	}

	let Ok(relative) = path.strip_prefix(root) else {
		return false;
	};
	let Some(Component::Normal(top)) = relative.components().next() else {
		return false;
	};
	top.to_str().is_some_and(|top| {
		["common", "events", "missions", "decisions", "history"]
			.iter()
			.any(|root| top.eq_ignore_ascii_case(root))
	})
}

fn assert_balanced_braces(path: &Path, content: &str) {
	let mut depth = 0isize;
	let mut in_string = false;
	let mut escaped = false;

	for (line_index, line) in content.lines().enumerate() {
		for (column_index, ch) in line.chars().enumerate() {
			if in_string {
				if escaped {
					escaped = false;
					continue;
				}
				match ch {
					'\\' => escaped = true,
					'"' => in_string = false,
					_ => {}
				}
				continue;
			}

			match ch {
				'#' => break,
				'"' => in_string = true,
				'{' => depth += 1,
				'}' => {
					depth -= 1;
					assert!(
						depth >= 0,
						"{} has an unmatched closing brace at {}:{}",
						path.display(),
						line_index + 1,
						column_index + 1
					);
				}
				_ => {}
			}
		}
	}

	assert_eq!(
		depth,
		0,
		"{} has {depth} unmatched opening brace(s)",
		path.display()
	);
}

fn assert_even_unescaped_quote_count(path: &Path, content: &str) {
	let count = unescaped_quote_count(content);
	assert_eq!(
		count % 2,
		0,
		"{} has an odd number of unescaped quotes ({count})",
		path.display()
	);
}

fn unescaped_quote_count(content: &str) -> usize {
	let bytes = content.as_bytes();
	let mut count = 0;
	for (index, byte) in bytes.iter().enumerate() {
		if *byte != b'"' {
			continue;
		}
		let mut slash_count = 0;
		let mut cursor = index;
		while cursor > 0 && bytes[cursor - 1] == b'\\' {
			slash_count += 1;
			cursor -= 1;
		}
		if slash_count % 2 == 0 {
			count += 1;
		}
	}
	count
}

fn assert_reparses_cleanly(path: &Path) {
	let parsed = parse_clausewitz_file(path);
	let diagnostics: Vec<String> = parsed
		.diagnostics
		.iter()
		.map(|diagnostic| {
			format!(
				"{}:{}: {}",
				diagnostic.span.start.line, diagnostic.span.start.column, diagnostic.message
			)
		})
		.collect();
	assert!(
		diagnostics.is_empty(),
		"{} did not re-parse cleanly:\n{}",
		path.display(),
		diagnostics.join("\n")
	);
}

// Compares the merged file at `out_dir/<rel>` against the checked-in golden file
// at `tests/fixtures/playsets/<name>/expected/<rel>`.
// Honours BLESS_SNAPSHOTS=1 by copying the actual output to the expected tree.
fn assert_matches_golden(name: &str, out_dir: &Path, rel: &str) {
	let actual = game_path(rel).to_path(out_dir);
	let expected = expected_path(name, rel);

	if env::var_os("BLESS_SNAPSHOTS").is_some() {
		let parent = expected
			.parent()
			.unwrap_or_else(|| panic!("expected path has no parent: {}", expected.display()));
		fs::create_dir_all(parent).expect("create expected snapshot parent");
		fs::copy(&actual, &expected).unwrap_or_else(|err| {
			panic!(
				"failed to bless {} from {}: {err}",
				expected.display(),
				actual.display()
			)
		});
		return;
	}

	let actual_bytes = fs::read(&actual)
		.unwrap_or_else(|err| panic!("failed to read actual {}: {err}", actual.display()));
	let expected_bytes = fs::read(&expected).unwrap_or_else(|err| {
		panic!(
			"failed to read expected golden {}: {err}; rerun with BLESS_SNAPSHOTS=1 after intentional output changes",
			expected.display()
		)
	});
	assert_eq!(
		actual_bytes, expected_bytes,
		"golden mismatch for {rel}; rerun with BLESS_SNAPSHOTS=1 after intentional output changes"
	);
}

#[test]
fn eu4_string_corruption_fixture_is_structurally_sound() {
	let out = run_merge_fixture("eu4_string_corruption");
	assert_structurally_sound(&out);
}

#[test]
fn eu4_string_corruption_cornwall_matches_golden() {
	let out = run_merge_fixture("eu4_string_corruption");
	let rel = "missions/ME_Cornwall_Missions.txt";
	if env::var_os("BLESS_SNAPSHOTS").is_none()
		&& !expected_path("eu4_string_corruption", rel).is_file()
	{
		return;
	}
	assert_matches_golden("eu4_string_corruption", &out, rel);
}

#[test]
fn eu4_minimal_passthrough_copies_per_path_files_and_materializes_common_module() {
	let out = run_merge_fixture("eu4_minimal_passthrough");
	assert_structurally_sound(&out);

	for rel in [
		"common/defines.lua",
		"localisation/minimal_l_english.yml",
		"events/foo.txt",
	] {
		assert_output_matches_fixture_input("eu4_minimal_passthrough", "minimal", &out, rel);
		assert_matches_golden("eu4_minimal_passthrough", &out, rel);
	}

	let output_path = out
		.join("common")
		.join("cultures")
		.join("zzz_foch_cultures.txt");
	let output = fs::read_to_string(&output_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", output_path.display()));
	for fragment in [
		"minimal_group = {",
		"graphical_culture = westerngfx",
		"minimal_culture = {",
		"primary = AAA",
		"\"Aedan\"",
		"\"Mab\"",
		"\"Fixture\"",
	] {
		assert!(
			output.contains(fragment),
			"missing {fragment:?} in:\n{output}"
		);
	}
	assert!(!out.join("common/cultures/00_cultures.txt").exists());
}

#[test]
fn analysis_never_restores_bundled_product_output() {
	let fixture = fixture_dir("eu4_minimal_passthrough");
	let temp_dir = tempfile::tempdir().expect("create tree parity tempdir");
	let default_out_dir = temp_dir.path().join("default-out");
	let explicit_out_dir = temp_dir.path().join("explicit-out");
	let game_root = temp_dir.path().join("empty-eu4-game");
	fs::create_dir_all(&game_root).expect("create empty game root");
	let workshop_root = temp_dir.path().join("steamapps/workshop");
	let workshop_mod = workshop_root.join("content/236850/200001");
	copy_dir_recursive(&fixture.join("mods/minimal"), &workshop_mod);
	fs::write(
		workshop_root.join("appworkshop_236850.acf"),
		r#""AppWorkshop"
{
	"appid" "236850"
	"WorkshopItemsInstalled"
	{
		"200001"
		{
			"size" "1"
			"timeupdated" "1780000000"
			"manifest" "300001"
		}
	}
}"#,
	)
	.expect("write Workshop ACF");
	let manifest_path = temp_dir.path().join("foch.toml");
	fs::write(
		&manifest_path,
		r#"
[project]
game = "eu4"

[[project.mods]]
id = "200001"
steam_id = "200001"
path = "steamapps/workshop/content/236850/200001"
workshop_identity = { app_id = 236850, workshop_id = "200001", manifest_id = "300001" }
"#,
	)
	.expect("write ACF-backed input manifest");
	let request = || {
		let mut game_path = HashMap::new();
		game_path.insert("eu4".to_string(), game_root.clone());
		InputRequest::from_manifest_path(
			manifest_path.clone(),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		)
	};
	let options = |out_dir: &Path| MergeAnalysisOptions {
		out_dir: out_dir.to_path_buf(),
		include_game_base: false,
		include_base: false,
		gui_scroll_merge: false,
		force: true,
		ignore_replace_path: false,
		dep_overrides: Vec::new(),
		resolution_config_path: None,
		interactive_conflict_handler: None,
		interactive_resolution_config_path: None,
		playset_fingerprint: None,
		provenance: false,
		merge_workers: std::num::NonZeroUsize::new(2).unwrap(),
		retained_paths: None,
	};
	let output_paths = [
		"common/defines.lua",
		"localisation/minimal_l_english.yml",
		"events/foo.txt",
		"common/cultures/zzz_foch_cultures.txt",
	];
	let read_outputs = |out_dir: &Path| {
		output_paths
			.iter()
			.map(|path| {
				fs::read(out_dir.join(path))
					.unwrap_or_else(|error| panic!("read tree output {path}: {error}"))
			})
			.collect::<Vec<_>>()
	};

	let default = run_merge_with_options(request(), options(&default_out_dir))
		.expect("run default tree merge");
	let default_outputs = read_outputs(&default_out_dir);
	let explicit = run_merge_for_evaluation(
		request(),
		options(&explicit_out_dir),
		MergeBackendId::GumtreePcsNway,
	)
	.expect("run explicit tree merge");
	assert_eq!(
		explicit.report.cache_source.as_deref(),
		None,
		"analysis must materialize current inputs instead of restoring bundled output"
	);
	assert_eq!(explicit.report.status, default.report.status);
	assert_eq!(read_outputs(&explicit_out_dir), default_outputs);
}

fn assert_output_matches_fixture_input(name: &str, mod_name: &str, out_dir: &Path, rel: &str) {
	let input = game_path(rel).to_path(fixture_dir(name).join("mods").join(mod_name));
	let actual = game_path(rel).to_path(out_dir);
	let input_bytes = fs::read(&input)
		.unwrap_or_else(|err| panic!("failed to read fixture input {}: {err}", input.display()));
	let actual_bytes = fs::read(&actual)
		.unwrap_or_else(|err| panic!("failed to read merge output {}: {err}", actual.display()));
	assert_eq!(
		actual_bytes, input_bytes,
		"copy-through output for {rel} should be byte-identical to fixture input"
	);
}

// ---------------------------------------------------------------------------
// Two-mod conflict fixture: exercises the resolution DSL end-to-end.
//
// `eu4_two_mod_conflict` ships three contributors that all redefine the same
// country-history file `history/countries/TES - Test.txt`. The two
// downstream contributors (conflict_a at precedence 1, conflict_b at
// precedence 2) each set `religion` to a different value relative to the
// baseline contributor's `catholic`. The patch engine surfaces this as a
// per-key sibling SetValue conflict that the user — or the resolution DSL
// — must arbitrate; without `foch.toml` the engine reports
// `manual_conflict_count >= 1`, with `[[resolutions]] match = "history/**"
// handler = "last_writer"` it routes the pick through the handler registry.
// ---------------------------------------------------------------------------

#[test]
fn eu4_two_mod_conflict_without_foch_toml_reports_manual_conflict() {
	let (result, out_dir) = run_merge_for_fixture("eu4_two_mod_conflict", false);
	assert_ne!(
		result.report.status,
		MergeReportStatus::Fatal,
		"strict merge should not be Fatal; report: {:#?}",
		result.report
	);
	assert!(
		result.report.manual_conflict_count >= 1,
		"strict two-mod conflict must surface at least one manual_conflict; report: {:#?}",
		result.report
	);
	assert!(out_dir.exists(), "out dir should still be materialized");
}

#[test]
#[ignore = "P-747: the report classifies a conflict by its parent path, so a root-level key binds no CWT field and `kind` stays None"]
fn eu4_schema_cardinality_conflict_is_tagged_from_cwt() {
	let (result, out_dir) = run_merge_for_fixture("eu4_schema_cardinality_conflict", false);
	assert_eq!(
		result.exit_code, 0,
		"schema-cardinality conflict should be deferred; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::PartialSuccess,
		"schema-cardinality conflict should produce partial output; report: {:#?}",
		result.report
	);
	assert!(
		result.report.manual_conflict_count >= 1,
		"schema-cardinality fixture must surface a manual conflict; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.conflict_resolutions[0].kind,
		Some(ConflictKind::SchemaCardinalityViolation),
		"country-history government_rank conflict should be tagged as a schema cardinality violation; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.conflict_resolutions[0].leaf_conflicts[0].kind,
		Some(ConflictKind::SchemaCardinalityViolation),
		"leaf conflict should carry the schema cardinality classification; report: {:#?}",
		result.report
	);
	assert!(out_dir.exists(), "out dir should still be materialized");
}

#[test]
fn eu4_two_mod_conflict_resolved_via_last_writer_handler() {
	let (result, out_dir) = run_merge_for_fixture("eu4_two_mod_conflict_resolved", false);
	assert_eq!(
		result.exit_code, 0,
		"DSL-resolved merge should exit 0; report: {:#?}",
		result.report
	);
	assert_ne!(
		result.report.status,
		MergeReportStatus::Fatal,
		"DSL-resolved merge should not be Fatal; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"last_writer handler must clear all manual conflicts; report: {:#?}",
		result.report
	);
	assert!(
		result
			.report
			.handler_resolutions
			.iter()
			.any(|record| record.action.eq_ignore_ascii_case("last_writer")),
		"handler_resolutions must record at least one last_writer entry; report: {:#?}",
		result.report
	);
	let merged_history_path = out_dir
		.join("history")
		.join("countries")
		.join("TES - Test.txt");
	assert!(
		merged_history_path.is_file(),
		"merged country-history file must be materialized at {}",
		merged_history_path.display()
	);
	let merged_text =
		fs::read_to_string(&merged_history_path).expect("read merged country history");
	assert!(
		merged_text.contains("religion = protestant"),
		"merged history should carry conflict_b's religion (protestant); got:\n{merged_text}"
	);
	assert!(
		!merged_text.contains("religion = orthodox"),
		"merged history must not retain conflict_a's religion; got:\n{merged_text}"
	);
	assert!(
		!merged_text.contains("religion = catholic"),
		"merged history must not retain baseline's religion; got:\n{merged_text}"
	);
	assert_structurally_sound(&out_dir);
}

#[test]
fn eu4_estates_preload_merges_modifier_branches_by_inner_key() {
	let (result, out_dir) = run_merge_for_fixture("eu4_cwt_suggested_estates_preload", false);
	assert_eq!(
		result.exit_code, 0,
		"estates_preload merge should complete; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"estates_preload merge should stay ready; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"estates_preload merge should not add manual conflicts; report: {:#?}",
		result.report
	);
	let merged_path = out_dir
		.join("common")
		.join("estates_preload")
		.join("zzz_foch_estates_preload.txt");
	let merged_text = fs::read_to_string(&merged_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_path.display()));
	assert_eq!(
		merged_text.matches("key = estate_balance").count(),
		1,
		"modifier.key must identify one merged estate_balance body; got:\n{merged_text}"
	);
	assert!(merged_text.contains("add_loyalty = 5"), "{merged_text}");
	assert!(merged_text.contains("add_influence = 10"), "{merged_text}");
	assert_eq!(
		merged_text.matches("key = estate_support").count(),
		1,
		"unchanged keyed modifiers must not duplicate; got:\n{merged_text}"
	);
}

#[test]
#[ignore = "cwt_suggested policy is not yet wired to the merge engine (see #42)"]
fn eu4_cwt_suggested_policy_merges_estates_preload_by_key() {
	let (result, out_dir) =
		run_merge_for_fixture("eu4_cwt_suggested_estates_preload_resolved", false);
	assert_eq!(
		result.exit_code, 0,
		"cwt_suggested merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"cwt_suggested merge should be ready; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"cwt_suggested merge should clear manual conflicts; report: {:#?}",
		result.report
	);
	let merged_path = out_dir
		.join("common")
		.join("estates_preload")
		.join("test_modifiers.txt");
	let merged_text = fs::read_to_string(&merged_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_path.display()));
	assert!(
		merged_text.contains("key = estate_balance"),
		"merged estates_preload output should retain the keyed modifier; got:\n{merged_text}"
	);
	assert!(
		merged_text.contains("add_loyalty = 5"),
		"merged estates_preload output should include loyalty patch; got:\n{merged_text}"
	);
	assert!(
		merged_text.contains("add_influence = 10"),
		"merged estates_preload output should include influence patch; got:\n{merged_text}"
	);
	assert_eq!(
		merged_text.matches("key = estate_balance").count(),
		1,
		"cwt_suggested merge should produce exactly one keyed modifier body; got:\n{merged_text}"
	);
	assert_structurally_sound(&out_dir);
}

#[test]
fn eu4_union_policy_lets_distinct_monarch_names_coexist() {
	let (result, out_dir) = run_merge_for_fixture("eu4_union_monarch_names_coexist", false);
	assert_eq!(
		result.exit_code, 0,
		"union merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"union merge should be ready; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"union merge should not surface manual conflicts; report: {:#?}",
		result.report
	);

	let merged_history_path = out_dir
		.join("history")
		.join("countries")
		.join("TES - Test.txt");
	let merged_text = fs::read_to_string(&merged_history_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_history_path.display()));
	for monarch_name in ["Aldus", "Berta", "Cedric"] {
		assert!(
			merged_text.contains(&format!("monarch_names = \"{monarch_name}\"")),
			"merged history should retain monarch name {monarch_name}; got:\n{merged_text}"
		);
	}
	assert_structurally_sound(&out_dir);
}

#[test]
fn eu4_boolean_or_policy_folds_scripted_trigger_into_or_block() {
	let (result, out_dir) = run_merge_for_fixture("eu4_boolean_or_scripted_trigger", false);
	assert_eq!(
		result.exit_code, 0,
		"BooleanOr merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"BooleanOr merge should be ready; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"BooleanOr merge should not surface manual conflicts; report: {:#?}",
		result.report
	);

	let merged_trigger_path = out_dir
		.join("common")
		.join("scripted_triggers")
		.join("zzz_foch_scripted_triggers.txt");
	let merged_text = fs::read_to_string(&merged_trigger_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_trigger_path.display()));
	assert!(
		merged_text.contains("is_test_country = {"),
		"merged scripted trigger should retain trigger key; got:\n{merged_text}"
	);
	assert_eq!(
		merged_text.matches("OR = {").count(),
		1,
		"BooleanOr should fold every contributor body into ONE shared OR (an OR of disjuncts); sibling OR blocks would be read as an implicit AND — the intersection — inverting the policy. got:\n{merged_text}"
	);
	for predicate in [
		"tag = TES",
		"has_country_flag = test_flag_a",
		"num_of_cities = 1",
	] {
		assert!(
			merged_text.contains(predicate),
			"merged scripted trigger should retain predicate {predicate}; got:\n{merged_text}"
		);
	}
	assert_structurally_sound(&out_dir);
}

#[test]
fn eu4_union_policy_concatenates_scripted_effect_bodies() {
	let (result, out_dir) = run_merge_for_fixture("eu4_union_scripted_effect", false);
	assert_eq!(
		result.exit_code, 0,
		"scripted effect union merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"scripted effect union merge should be ready; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"scripted effect union should not surface manual conflicts; report: {:#?}",
		result.report
	);

	let merged_effect_path = out_dir
		.join("common")
		.join("scripted_effects")
		.join("zzz_foch_scripted_effects.txt");
	let merged_text = fs::read_to_string(&merged_effect_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_effect_path.display()));
	assert!(
		merged_text.contains("test_shared_effect = {"),
		"merged scripted effect should retain effect key; got:\n{merged_text}"
	);
	assert_eq!(
		merged_text.matches("OR = {").count(),
		0,
		"effects are imperative and compose by doing both bodies, not by BooleanOr wrapping; got:\n{merged_text}"
	);
	for statement in ["set_country_flag = effect_a_ran", "add_prestige = 5"] {
		assert!(
			merged_text.contains(statement),
			"merged scripted effect should retain statement {statement}; got:\n{merged_text}"
		);
	}
	assert_structurally_sound(&out_dir);
}

#[test]
fn eu4_provenance_annotates_adopted_scripted_effect_and_writes_sidecar() {
	let (result, out_dir) =
		run_merge_for_fixture_with_provenance("eu4_union_scripted_effect", false);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"provenance run should still merge cleanly; report: {:#?}",
		result.report
	);

	let merged_effect_path = out_dir
		.join("common")
		.join("scripted_effects")
		.join("zzz_foch_scripted_effects.txt");
	let merged_text = fs::read_to_string(&merged_effect_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_effect_path.display()));

	// The adopted top-level definition carries an inline provenance comment
	// naming its source mods, directly above the definition.
	let comment_line = merged_text
		.lines()
		.find(|line| {
			line.trim_start()
				.starts_with("# foch: test_shared_effect from ")
		})
		.unwrap_or_else(|| panic!("expected a provenance comment; got:\n{merged_text}"));
	assert!(
		comment_line.matches(',').count() >= 1,
		"two mods contributed, so the comment should name both: {comment_line:?}"
	);
	let comment_idx = merged_text
		.find("# foch: test_shared_effect")
		.expect("comment present");
	let def_idx = merged_text
		.find("test_shared_effect = {")
		.expect("definition present");
	assert!(
		comment_idx < def_idx,
		"provenance comment must sit immediately above the definition; got:\n{merged_text}"
	);

	// The sidecar binds the report's lineage to the exact committed output bytes.
	let file_prov = result
		.report
		.definition_provenance
		.iter()
		.find(|(path, _)| path.ends_with("scripted_effects/zzz_foch_scripted_effects.txt"))
		.map(|(_, defs)| defs)
		.expect("report has provenance for the merged file");
	assert_eq!(
		file_prov.get("test_shared_effect").map(Vec::len),
		Some(2),
		"both contributing mods should be credited: {file_prov:?}"
	);

	let sidecar = out_dir.join(".foch").join("foch-provenance.json");
	let sidecar_text =
		fs::read_to_string(&sidecar).expect("provenance sidecar should be written when flag is on");
	assert!(
		sidecar_text.contains("test_shared_effect"),
		"sidecar should record the merged definition; got:\n{sidecar_text}"
	);
	let artifact: serde_json::Value = serde_json::from_str(&sidecar_text).expect("valid sidecar");
	assert_eq!(
		artifact["version"], 1,
		"sidecar must declare its schema version"
	);
	let files = artifact["files"].as_object().expect("versioned files");
	assert_eq!(files.len(), result.report.definition_provenance.len());
	for (path, definitions) in &result.report.definition_provenance {
		let bytes = fs::read(path.to_path(&out_dir)).expect("committed provenance script");
		assert_eq!(
			files[path.as_str()]["content_hash"],
			blake3::hash(&bytes).to_hex().to_string()
		);
		assert_eq!(
			files[path.as_str()]["definitions"],
			serde_json::json!(definitions)
		);
	}
	assert_eq!(
		file_prov["test_shared_effect"],
		["330001".to_string(), "330002".to_string()]
	);
	assert_eq!(
		artifact["mod_names"],
		serde_json::json!({"330001": "effect_a", "330002": "effect_b"})
	);
	assert_eq!(
		serde_json::to_value(&result.report).expect("report JSON")["provenance_mod_names"],
		artifact["mod_names"]
	);

	assert_structurally_sound(&out_dir);
}

#[test]
fn eu4_provenance_off_omits_sidecar_and_report_metadata() {
	let (result, out_dir) = run_merge_for_fixture("eu4_union_scripted_effect", false);
	assert_eq!(result.report.status, MergeReportStatus::Ready);
	assert!(!out_dir.join(".foch/foch-provenance.json").exists());
	let report = serde_json::to_value(&result.report).expect("report JSON");
	assert!(report.get("definition_provenance").is_none());
	assert!(report.get("provenance_mod_names").is_none());
}

#[test]
fn eu4_merge_trace_records_union_scripted_effect() {
	let (result, out_dir) =
		run_merge_for_fixture_with_provenance("eu4_union_scripted_effect", false);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"trace run should still merge cleanly; report: {:#?}",
		result.report
	);

	let trace = result
		.report
		.merge_trace
		.iter()
		.find(|(path, _)| path.ends_with("scripted_effects/zzz_foch_scripted_effects.txt"))
		.map(|(_, defs)| defs)
		.expect("report has merge trace for the merged file");
	let entry = trace
		.get("test_shared_effect")
		.expect("trace has test_shared_effect");
	assert_eq!(entry.policy, MergeTracePolicy::Union);
	assert_eq!(entry.decision, MergeTraceDecision::Unioned);
	assert_eq!(entry.contributors.len(), 2);

	let sidecar = out_dir.join(".foch").join("foch-merge-trace.json");
	let sidecar_text = fs::read_to_string(&sidecar).expect("merge trace sidecar should be written");
	let parsed: std::collections::BTreeMap<
		String,
		std::collections::BTreeMap<String, MergeTraceEntry>,
	> = serde_json::from_str(&sidecar_text).expect("trace sidecar parses");
	let sidecar_entry = parsed
		.values()
		.find_map(|defs| defs.get("test_shared_effect"))
		.expect("sidecar trace has test_shared_effect");
	assert_eq!(sidecar_entry.policy, MergeTracePolicy::Union);
	assert_eq!(sidecar_entry.decision, MergeTraceDecision::Unioned);
	assert_eq!(sidecar_entry.contributors.len(), 2);

	assert_structurally_sound(&out_dir);
}

#[test]
fn eu4_gfx_sprite_types_union_different_names_without_conflict() {
	let (result, out_dir) =
		run_merge_for_fixture("eu4_gfx_sprite_types_union_named_children", false);
	assert_eq!(
		result.exit_code, 0,
		"gfx sprite union merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"gfx sprite union merge should be ready; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"different named spriteType children should not conflict; report: {:#?}",
		result.report
	);

	let merged_gfx_path = out_dir.join("gfx").join("test.gfx");
	let merged_text = fs::read_to_string(&merged_gfx_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_gfx_path.display()));
	assert_eq!(
		merged_text.matches("spriteTypes = {").count(),
		1,
		"different spriteType children should merge into one spriteTypes container; got:\n{merged_text}"
	);
	for sprite in ["GFX_test_sprite_a", "GFX_test_sprite_b"] {
		assert!(
			merged_text.contains(sprite),
			"merged gfx should retain sprite {sprite}; got:\n{merged_text}"
		);
	}
}

#[test]
fn eu4_custom_gui_definitions_are_distinct_by_name() {
	// Each `custom_button`/`custom_window` is identified by its `name`, as the
	// GUI binds it; different names from different mods must not conflict.
	let (result, out_dir) = run_merge_for_fixture("eu4_custom_gui_union_named_buttons", false);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"distinct custom GUI names should merge; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"{:#?}",
		result.report
	);
	let mut merged = String::new();
	for entry in fs::read_dir(out_dir.join("common").join("custom_gui")).expect("custom_gui output")
	{
		merged.push_str(&fs::read_to_string(entry.expect("entry").path()).expect("read"));
	}
	for name in [
		"baseline_button",
		"gui_a_window",
		"gui_a_button",
		"gui_b_button",
	] {
		assert_eq!(
			merged.matches(name).count(),
			1,
			"{name} kept once; got:
{merged}"
		);
	}
	assert!(merged.contains("adm_power = 50") && merged.contains("add_dip_power = 10"));
}

#[test]
fn eu4_bookmarks_and_customizable_localization_are_distinct_by_name() {
	// `bookmark = { name = ... }` and `defined_text = { name = ... }` blocks
	// share their key; each mod's differently named definition must survive.
	let (result, out_dir) = run_merge_for_fixture("eu4_named_definitions_union", false);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"distinct names should merge; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"{:#?}",
		result.report
	);
	let read_all = |dir: PathBuf| {
		let mut merged = String::new();
		for entry in fs::read_dir(&dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display())) {
			merged.push_str(&fs::read_to_string(entry.expect("entry").path()).expect("read"));
		}
		merged
	};
	let bookmarks = read_all(out_dir.join("common").join("bookmarks"));
	let commands = read_all(out_dir.join("customizable_localization"));
	for mod_dir in ["baseline", "named_a", "named_b"] {
		assert_eq!(
			bookmarks.matches(&format!("\"BM_{mod_dir}\"")).count(),
			1,
			"{bookmarks}"
		);
	}
	// Both mods override the same file, each adding a different command.
	for command in ["GetBaselineText", "Getnamed_aText", "Getnamed_bText"] {
		assert_eq!(commands.matches(command).count(), 1, "{commands}");
	}
}

#[test]
fn eu4_gfx_sprite_types_same_name_divergence_conflicts() {
	let (result, _out_dir) =
		run_merge_for_fixture("eu4_gfx_sprite_types_same_name_conflict", false);
	assert_eq!(
		result.exit_code, 0,
		"same-name divergent spriteType conflict should be deferred; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::PartialSuccess,
		"same-name divergent spriteType conflict should produce partial output; report: {:#?}",
		result.report
	);
	assert!(
		result.report.manual_conflict_count >= 1,
		"same-name divergent spriteType children must surface a manual conflict; report: {:#?}",
		result.report
	);
}

#[test]
fn eu4_comment_only_override_is_noop_and_keeps_sibling_content() {
	let (result, out_dir) = run_merge_for_fixture("eu4_comment_only_override_noop", false);
	assert_eq!(
		result.exit_code, 0,
		"comment-only override should not block; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"comment-only override should be treated as no-op; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"comment-only override should not surface a manual conflict; report: {:#?}",
		result.report
	);

	let merged_path = out_dir
		.join("common")
		.join("governments")
		.join("zzz_foch_governments.txt");
	let merged_text = fs::read_to_string(&merged_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_path.display()));
	assert!(
		merged_text.contains("monarchy = {") && merged_text.contains("preferred_reform"),
		"real sibling content should survive comment-only override; got:\n{merged_text}"
	);
}

#[test]
fn eu4_governments_cross_file_module_emits_union_once() {
	let (result, out_dir) = run_merge_for_fixture("eu4_governments_cross_file_union", false);
	assert_eq!(
		result.exit_code, 0,
		"cross-file governments merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"cross-file governments merge should be ready; report: {:#?}",
		result.report
	);

	let governments_dir = out_dir.join("common").join("governments");
	let merged_path = governments_dir.join("zzz_foch_governments.txt");
	let merged_text = fs::read_to_string(&merged_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_path.display()));
	for definition in [
		"expanded_europa_government",
		"governments_expanded_government",
	] {
		assert_eq!(
			merged_text.matches(definition).count(),
			1,
			"{definition} should appear exactly once; got:\n{merged_text}"
		);
	}
	let emitted_government_files = fs::read_dir(&governments_dir)
		.expect("read governments output")
		.filter_map(Result::ok)
		.filter(|entry| entry.path().extension().is_some_and(|ext| ext == "txt"))
		.count();
	assert_eq!(
		emitted_government_files, 1,
		"module inputs must be consumed instead of copied through"
	);
	let generated_descriptor =
		load_descriptor(&out_dir.join("descriptor.mod")).expect("parse generated descriptor");
	assert_eq!(
		generated_descriptor.replace_path,
		vec![GamePathBuf::parse("common/governments").expect("valid game path")]
	);

	let relative =
		GamePath::new("common/governments/zzz_foch_governments.txt").expect("valid game path");
	assert_eq!(relative.to_path(&out_dir), merged_path);
	let parsed_output = parse_script_file("generated", &out_dir, relative);
	let descriptor = eu4()
		.classify_content_family(relative)
		.expect("governments descriptor");
	let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
		panic!("governments must use definition-module loading");
	};
	let runtime_view = load_definition_module(
		&[DefinitionModuleInput::new(relative, &parsed_output)],
		policy,
	)
	.expect("reload generated module using runtime policy");
	assert_eq!(runtime_view.ast.statements, parsed_output.ast.statements);
}

#[test]
fn eu4_institutions_cross_file_module_overlays_without_replace_path() {
	let (result, out_dir) = run_merge_for_fixture("eu4_institutions_cross_file_overlay", false);
	assert_eq!(
		result.exit_code, 0,
		"cross-file institutions merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(result.report.status, MergeReportStatus::Ready);

	let merged_path = out_dir
		.join("common")
		.join("institutions")
		.join("zzz_foch_institutions.txt");
	let merged_text = fs::read_to_string(&merged_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_path.display()));
	assert!(merged_text.contains("renaissance = {"), "{merged_text}");
	assert!(merged_text.contains("printing_press = {"), "{merged_text}");

	let generated_descriptor =
		load_descriptor(&out_dir.join("descriptor.mod")).expect("parse generated descriptor");
	assert!(
		!generated_descriptor
			.replace_path
			.iter()
			.any(|path| path.as_str() == "common/institutions")
	);
}

#[test]
fn eu4_gui_provenance_appends_tooltips_from_each_surviving_widget() {
	use std::collections::{BTreeMap, BTreeSet};

	const FIXTURE: &str = "eu4_gui_provenance";
	const SCRIPT: &str = "interface/test.gui";
	let source_bytes = || {
		walkdir::WalkDir::new(fixture_dir(FIXTURE))
			.into_iter()
			.map(|entry| entry.expect("GUI fixture entry"))
			.filter(|entry| entry.file_type().is_file())
			.map(|entry| (entry.path().to_path_buf(), fs::read(entry.path()).unwrap()))
			.collect::<BTreeMap<_, _>>()
	};
	let original_sources = source_bytes();
	let (plain, plain_dir) = run_merge_for_fixture(FIXTURE, false);
	assert_eq!(plain.exit_code, 0, "{:#?}", plain.report);
	assert_eq!(plain.report.status, MergeReportStatus::Ready);
	// The expected rendering is authored from the union of the two inputs. Only
	// checkout line endings are normalized; emitted bytes must stay canonical LF.
	let expected = fs::read_to_string(expected_path(FIXTURE, SCRIPT))
		.expect("expected unannotated GUI")
		.replace("\r\n", "\n");
	assert_eq!(
		fs::read(plain_dir.join(SCRIPT)).unwrap(),
		expected.as_bytes()
	);
	assert!(!plain_dir.join("localisation").exists());
	assert!(!plain_dir.join(".foch/foch-provenance.json").exists());
	let plain_widgets = gui_provenance_widget_fields(&plain_dir.join(SCRIPT));
	let mut previous = None;
	for _ in 0..2 {
		let (result, out_dir) = run_merge_for_fixture_with_provenance(FIXTURE, false);
		assert_eq!(result.exit_code, 0, "{:#?}", result.report);
		assert_eq!(result.report.status, MergeReportStatus::Ready);
		let script_bytes = fs::read(out_dir.join(SCRIPT)).expect("annotated GUI");
		let widgets = gui_provenance_widget_fields(&out_dir.join(SCRIPT));
		let mut expected_localisation = BTreeMap::new();
		for (widget, channel, value) in [
			(
				"alpha_icon",
				"pdx_tooltip",
				"$ALPHA_TT$\\n\\nMerged from GUI Alpha",
			),
			(
				"alpha_button",
				"tooltipText",
				"$ALPHA_BUTTON_TT$\\n\\nMerged from GUI Alpha",
			),
			(
				"beta_text",
				"pdx_tooltip",
				"$TEXT_HELP$\\n\\nMerged from GUI Beta",
			),
			(
				"shared_button",
				"pdx_tooltip",
				"$SHARED_TT$\\n\\nMerged from GUI Alpha, GUI Beta",
			),
		] {
			let key = widgets[widget]
				.get(channel)
				.unwrap_or_else(|| panic!("missing {widget}.{channel}"));
			assert!(
				key.starts_with("FOCH_PROVENANCE_GUI_"),
				"{widget}.{channel}: {key}"
			);
			assert!(
				expected_localisation
					.insert(key.clone(), value.to_string())
					.is_none(),
				"widgets must not share wrappers"
			);
		}
		assert_eq!(
			widgets["alpha_icon"]["pdx_tooltip_delayed"],
			"ALPHA_DELAYED"
		);
		assert_eq!(
			widgets["alpha_button"]["delayedTooltipText"],
			"ALPHA_BUTTON_DELAYED"
		);
		// Without a verified vanilla ancestor, an absent field may still have an
		// engine-provided tooltip; only authored immediate keys can be wrapped.
		for skipped in [
			"unsupported_window",
			"competing_icon",
			"dynamic_icon",
			"beta_implicit",
		] {
			assert_eq!(
				widgets[skipped], plain_widgets[skipped],
				"unsafe or unsupported widget {skipped} must remain intact"
			);
		}
		let localisation_files = walkdir::WalkDir::new(out_dir.join("localisation"))
			.into_iter()
			.map(|entry| entry.expect("generated localisation entry"))
			.filter(|entry| entry.file_type().is_file())
			.map(|entry| {
				(
					entry.path().strip_prefix(&out_dir).unwrap().to_path_buf(),
					fs::read(entry.path()).unwrap(),
				)
			})
			.collect::<BTreeMap<_, _>>();
		assert_eq!(
			localisation_files.len(),
			1,
			"one shared generated localisation file"
		);
		let localisation_bytes = localisation_files.values().next().unwrap();
		assert!(localisation_bytes.starts_with(&[0xef, 0xbb, 0xbf]));
		let localisation =
			std::str::from_utf8(&localisation_bytes[3..]).expect("UTF-8 localisation");
		for language in foch::game::eu4::content::EU4_LOCALISATION_LANGUAGE_HEADERS {
			assert!(localisation.contains(&format!("{language}:\n")));
		}
		let mut actual_keys = BTreeSet::new();
		for line in localisation
			.lines()
			.filter(|line| line.starts_with(" FOCH_PROVENANCE_GUI_"))
		{
			let (key, value) = line
				.trim_start()
				.split_once(":0 \"")
				.expect("localisation key/value");
			let value = value.strip_suffix('"').expect("quoted localisation value");
			assert_eq!(
				Some(value),
				expected_localisation.get(key).map(String::as_str),
				"localisation must belong to a surviving widget: {key}"
			);
			actual_keys.insert(key.to_string());
		}
		assert_eq!(actual_keys, expected_localisation.keys().cloned().collect());
		let sidecar_bytes = fs::read(out_dir.join(".foch/foch-provenance.json")).expect("sidecar");
		let sidecar: serde_json::Value = serde_json::from_slice(&sidecar_bytes).unwrap();
		assert_eq!(sidecar["version"], 1);
		assert_eq!(
			sidecar["files"][SCRIPT]["content_hash"],
			blake3::hash(&script_bytes).to_hex().to_string(),
			"sidecar must hash post-injection bytes"
		);
		let current = (script_bytes, localisation_files, sidecar_bytes);
		if let Some(previous) = previous.as_ref() {
			assert_eq!(
				&current, previous,
				"repeatable GUI, localisation paths/bytes, and sidecar"
			);
		}
		previous = Some(current);
	}
	assert_eq!(
		source_bytes(),
		original_sources,
		"GUI source mods must remain read-only"
	);
}

fn gui_provenance_widget_fields(
	path: &Path,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> {
	use foch::game::eu4::script::parser::{AstStatement, AstValue};
	let parsed = parse_clausewitz_file(path);
	assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
	let widgets = parsed
		.statements
		.iter()
		.find_map(|statement| match statement {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} if key == "guiTypes" => Some(items),
			_ => None,
		})
		.expect("guiTypes container");
	widgets
		.iter()
		.filter_map(|widget| {
			let AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} = widget
			else {
				return None;
			};
			let fields = items
				.iter()
				.filter_map(|field| match field {
					AstStatement::Assignment {
						key,
						value: AstValue::Scalar { value, .. },
						..
					} => Some((key.clone(), value.as_text())),
					_ => None,
				})
				.collect::<std::collections::BTreeMap<_, _>>();
			fields.get("name").cloned().map(|name| (name, fields))
		})
		.collect()
}

#[test]
fn eu4_gui_edit_wins_over_remove_keeps_the_edit() {
	// One mod edits a widget property (orientation) while another removes it.
	// GUI families opt into edit-wins, so the edit is kept and no manual
	// conflict is surfaced — a GUI "remove" is typically a trimmed widget copy
	// not re-shipping a field, not an intentional delete that should veto a
	// sibling mod's edit. Contrast eu4_edit_vs_delete_reports_structural_conflict
	// (history family, flag off), where the same edit-vs-delete shape conflicts.
	let (result, out_dir) = run_merge_for_fixture("eu4_gui_edit_wins_over_remove", false);
	assert_eq!(
		result.exit_code, 0,
		"gui edit-wins merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"gui edit-wins merge should be ready; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"edit-vs-remove on a GUI property must not conflict; report: {:#?}",
		result.report
	);

	let merged_gui_path = out_dir.join("interface").join("test.gui");
	let merged_text = fs::read_to_string(&merged_gui_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_gui_path.display()));
	assert!(
		merged_text.contains("CENTER"),
		"the edit (orientation = CENTER) must be kept; got:\n{merged_text}"
	);
	assert!(
		merged_text.contains("position"),
		"the untouched position block must remain; got:\n{merged_text}"
	);
}

#[test]
fn eu4_gui_scroll_merge_flag_stacks_divergent_same_name_container() {
	let (partial, _partial_out_dir) =
		run_merge_for_fixture("eu4_gui_scroll_stack_same_name_conflict", false);
	assert_eq!(
		partial.exit_code, 0,
		"same-name divergent GUI conflict should be deferred; report: {:#?}",
		partial.report
	);
	assert_eq!(
		partial.report.status,
		MergeReportStatus::PartialSuccess,
		"same-name divergent GUI conflict should produce partial output; report: {:#?}",
		partial.report
	);
	assert!(
		partial.report.manual_conflict_count >= 1,
		"same-name divergent GUI containers must surface a manual conflict without the flag; report: {:#?}",
		partial.report
	);

	let (result, out_dir) = run_merge_for_fixture_with_gui_scroll(
		"eu4_gui_scroll_stack_same_name_conflict",
		false,
		true,
	);
	assert_eq!(
		result.exit_code, 0,
		"GUI scroll-stack merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"GUI scroll-stack merge should be ready; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"GUI scroll-stack merge should clear manual conflicts; report: {:#?}",
		result.report
	);

	let merged_gui_path = out_dir.join("interface").join("test.gui");
	let merged_text = fs::read_to_string(&merged_gui_path)
		.unwrap_or_else(|err| panic!("read {}: {err}", merged_gui_path.display()));
	for expected in [
		"shared_window",
		"standardlistbox_slider",
		"foch_scroll_layer_0",
		"foch_scroll_layer_1",
		"unique_icon_a",
		"unique_icon_b",
	] {
		assert!(
			merged_text.contains(expected),
			"scroll-stack output should retain {expected}; got:\n{merged_text}"
		);
	}
}

#[test]
fn eu4_edit_vs_delete_reports_structural_conflict() {
	let (result, out_dir) = run_merge_for_fixture("eu4_mixed_kinds_conflict", false);
	assert_eq!(
		result.exit_code, 0,
		"edit-vs-delete conflict should be deferred; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::PartialSuccess,
		"edit-vs-delete conflict should produce partial output; report: {:#?}",
		result.report
	);
	assert!(
		result.report.manual_conflict_count >= 1,
		"edit-vs-delete changes must surface a manual conflict; report: {:#?}",
		result.report
	);
	assert!(
		result
			.report
			.conflict_resolutions
			.iter()
			.any(|resolution| resolution.reason.contains("delete_modify")),
		"tree conflict reason should identify delete_modify; report: {:#?}",
		result.report
	);
	assert!(out_dir.exists(), "out dir should still be materialized");
}

#[test]
fn eu4_recurse_policy_emits_conflict_on_divergent_sub_blocks() {
	let (result, out_dir) = run_merge_for_fixture("eu4_recurse_block_conflict", false);
	assert_eq!(
		result.exit_code, 0,
		"recursive conflict should be deferred; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::PartialSuccess,
		"recursive conflict should produce partial output; report: {:#?}",
		result.report
	);
	assert!(
		result.report.manual_conflict_count >= 1,
		"divergent Recurse sub-block edits must surface a manual conflict; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.conflict_resolutions[0].kind,
		Some(ConflictKind::DeepMergeable),
		"recursive block conflicts should be tagged as deep-mergeable; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.conflict_resolutions[0].leaf_conflicts[0].kind,
		Some(ConflictKind::DeepMergeable),
		"leaf conflict should carry the deep-mergeable classification; report: {:#?}",
		result.report
	);
	assert!(out_dir.exists(), "out dir should still be materialized");
}

#[test]
fn eu4_defer_handler_keeps_manual_conflict_with_attribution() {
	let (result, out_dir) = run_merge_for_fixture("eu4_handler_defer", false);
	assert_ne!(
		result.report.status,
		MergeReportStatus::Fatal,
		"defer handler merge should not be Fatal; report: {:#?}",
		result.report
	);
	assert!(
		result.report.manual_conflict_count >= 1,
		"defer handler must keep at least one manual conflict unresolved; report: {:#?}",
		result.report
	);
	assert!(
		result
			.report
			.handler_resolutions
			.iter()
			.any(|record| record.action.eq_ignore_ascii_case("defer")),
		"handler_resolutions must attribute the explicit defer decision; report: {:#?}",
		result.report
	);
	assert!(out_dir.exists(), "out dir should still be tracked");
}

#[test]
fn eu4_case_insensitive_explicit_defer_is_reviewed_without_a_forced_marker() {
	let fixture = fixture_dir("eu4_handler_defer");
	let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("merge-e2e");
	fs::create_dir_all(&scratch_root).expect("create merge e2e scratch root");
	let temp_dir = Builder::new()
		.prefix("eu4_case_insensitive_defer-")
		.tempdir_in(&scratch_root)
		.expect("create merge e2e tempdir");
	let out_dir = temp_dir.path().join("out");
	let game_root = temp_dir.path().join("eu4-game");
	fs::create_dir_all(&game_root).expect("create fixture game root");
	let config_path = temp_dir.path().join("foch.defer.toml");
	fs::write(
		&config_path,
		"[[resolutions]]\nmatch = \"history/**\"\nhandler = \"DeFeR\"\n",
	)
	.expect("write mixed-case defer config");
	let target = "history/countries/TES - Test.txt";

	let analyzed = analyze_merge_for_playset(
		&fixture.join("dlc_load.json"),
		out_dir.clone(),
		game_root,
		true,
		Some(config_path),
	);
	let units = analyzed.list_units();
	assert_eq!(units.len(), 1, "fixture must produce one review unit");
	assert_eq!(units[0].path.as_str(), target);
	assert_eq!(units[0].disposition, MergeDisposition::Deferred);
	assert_eq!(units[0].output_path, None);
	assert_eq!(analyzed.review_summary().deferred, 1);
	let result = analyzed
		.commit(CommitAuthorization::EmptyTargetOnly)
		.expect("commit explicit defer analysis");

	assert_eq!(result.report.status, MergeReportStatus::PartialSuccess);
	assert!(
		!game_path(target).to_path(&out_dir).exists(),
		"--force must not turn an explicit defer into a manual marker"
	);
}

#[test]
fn eu4_mixed_explicit_defer_and_unresolved_leaf_still_needs_user_choice() {
	let source_fixture = fixture_dir("eu4_two_mod_conflict");
	let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("merge-e2e");
	fs::create_dir_all(&scratch_root).expect("create merge e2e scratch root");
	let temp_dir = Builder::new()
		.prefix("eu4_mixed_defer_unresolved-")
		.tempdir_in(&scratch_root)
		.expect("create merge e2e tempdir");
	let playset = temp_dir.path().join("playset");
	copy_dir_recursive(&source_fixture, &playset);

	for (mod_dir, culture) in [("conflict_a", "french"), ("conflict_b", "german")] {
		let file = playset
			.join("mods")
			.join(mod_dir)
			.join("history")
			.join("countries")
			.join("TES - Test.txt");
		let text = fs::read_to_string(&file).expect("read copied country file");
		fs::write(
			&file,
			text.replace(
				"primary_culture = english",
				&format!("primary_culture = {culture}"),
			),
		)
		.expect("write copied country file with second conflict");
	}

	let target = "history/countries/TES - Test.txt";
	let config_path = temp_dir.path().join("foch.partial-defer.toml");
	fs::write(
		&config_path,
		r#"[[resolutions]]
match = "history/countries/TES - Test.txt::religion"
handler = "DeFeR"
"#,
	)
	.expect("write conflict-scoped defer config");
	let out_dir = temp_dir.path().join("out");
	let game_root = temp_dir.path().join("eu4-game");
	fs::create_dir_all(&game_root).expect("create fixture game root");

	let analyzed = analyze_merge_for_playset(
		&playset.join("dlc_load.json"),
		out_dir.clone(),
		game_root,
		true,
		Some(config_path),
	);
	let units = analyzed.list_units();
	assert_eq!(units.len(), 1, "fixture must produce one review unit");
	assert_eq!(units[0].path.as_str(), target);
	assert_eq!(
		units[0].disposition,
		MergeDisposition::NeedsUserChoice,
		"one explicit defer must not hide the unrelated unresolved leaf"
	);
	assert_eq!(
		units[0].output_path.as_ref().map(|path| path.as_str()),
		Some(target)
	);
	assert_eq!(analyzed.review_summary().needs_user_choice, 1);
	let result = analyzed
		.commit(CommitAuthorization::EmptyTargetOnly)
		.expect("commit mixed defer analysis");

	assert_eq!(result.report.status, MergeReportStatus::PartialSuccess);
	let marker =
		fs::read_to_string(game_path(target).to_path(&out_dir)).expect("read manual marker");
	assert!(marker.starts_with("FOCH_MERGE_CONFLICT"));
	assert!(marker.contains("primary_culture"));
}

#[cfg(not(any(target_os = "windows", target_os = "redox")))]
#[test]
fn eu4_keep_existing_handler_preserves_existing_output_file() {
	let fixture = fixture_dir("eu4_handler_keep_existing");
	assert!(
		fixture.is_dir(),
		"fixture does not exist: {}",
		fixture.display()
	);

	let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("merge-e2e");
	fs::create_dir_all(&scratch_root).expect("create merge e2e scratch root");
	let temp_dir = Builder::new()
		.prefix("eu4_handler_keep_existing-prepopulated-")
		.tempdir_in(&scratch_root)
		.expect("create merge e2e tempdir");
	let out_dir = temp_dir.path().join("out");
	let game_root = temp_dir.path().join("eu4-game");
	fs::create_dir_all(&game_root).expect("create fixture game root");

	let sentinel_path = out_dir
		.join("history")
		.join("countries")
		.join("TES - Test.txt");
	fs::create_dir_all(sentinel_path.parent().expect("sentinel parent"))
		.expect("create sentinel parent");
	fs::write(
		&sentinel_path,
		"# pre-existing sentinel
religion = sentinel
",
	)
	.expect("write pre-existing sentinel");

	let config_path = temp_dir.path().join("foch.keep-existing.toml");
	fs::copy(fixture.join("foch.toml"), &config_path).expect("copy keep_existing foch.toml");

	let analyze = |target_out: &Path| {
		let mut game_path = HashMap::new();
		game_path.insert("eu4".to_string(), game_root.clone());
		analyze_merge(
			InputRequest::from_playset_path(
				fixture.join("dlc_load.json"),
				Config {
					steam_root_path: None,
					paradox_data_path: None,
					game_path,
					extra_ignore_patterns: Vec::new(),
				},
			),
			MergeAnalysisOptions {
				out_dir: target_out.to_path_buf(),
				include_game_base: false,
				include_base: false,
				gui_scroll_merge: false,
				force: false,
				ignore_replace_path: false,
				dep_overrides: Vec::new(),
				resolution_config_path: Some(config_path.clone()),
				interactive_conflict_handler: None,
				interactive_resolution_config_path: None,
				playset_fingerprint: None,
				provenance: false,
				merge_workers: std::num::NonZeroUsize::new(2).unwrap(),
				retained_paths: None,
			},
			&NoopProgressObserver,
			&CancellationToken::new(),
		)
	};
	let run = |target_out: &Path| {
		let analyzed = analyze(target_out)?;
		let authorization = match analyzed.replacement_target()? {
			Some(target) => CommitAuthorization::ReplaceExisting(target),
			None => CommitAuthorization::EmptyTargetOnly,
		};
		analyzed.commit(authorization)
	};
	let result = run(&out_dir)
		.unwrap_or_else(|err| panic!("merge fixture eu4_handler_keep_existing failed: {err}"));

	assert_eq!(
		result.exit_code, 0,
		"keep_existing merge should exit 0; report: {:#?}",
		result.report
	);
	assert_ne!(
		result.report.status,
		MergeReportStatus::Fatal,
		"keep_existing merge should not be Fatal; report: {:#?}",
		result.report
	);
	let merged_text = fs::read_to_string(&sentinel_path).expect("read preserved sentinel");
	assert!(
		merged_text.contains("religion = sentinel"),
		"keep_existing should preserve the pre-existing output file; got:
{merged_text}"
	);
	assert!(
		result
			.report
			.handler_resolutions
			.iter()
			.any(|record| record.action.eq_ignore_ascii_case("kept_existing")),
		"handler_resolutions must record the keep_existing decision; report: {:#?}",
		result.report
	);

	let stale = analyze(&out_dir).expect("analyze keep_existing output for stale check");
	fs::write(
		&sentinel_path,
		"# changed after analysis\nreligion = stale_guard_sentinel\n",
	)
	.expect("change keep_existing output after analysis");
	let current_target = stale
		.replacement_target()
		.expect("fingerprint current target")
		.expect("non-empty current target");
	let stale_error = stale
		.commit(CommitAuthorization::ReplaceExisting(current_target))
		.expect_err("keep_existing output drift must stale the analysis");
	assert!(matches!(
		stale_error,
		foch::merge::MergeError::AnalyzedOutputChanged { .. }
	));
	assert!(
		fs::read_to_string(&sentinel_path)
			.expect("read stale-guard sentinel")
			.contains("stale_guard_sentinel")
	);

	fs::write(
		&sentinel_path,
		"# updated between identical merge requests\nreligion = updated_sentinel\n",
	)
	.expect("update pre-existing sentinel");
	let repeated = run(&out_dir)
		.unwrap_or_else(|err| panic!("repeated fixture eu4_handler_keep_existing failed: {err}"));
	assert_eq!(repeated.exit_code, 0);
	let repeated_text = fs::read_to_string(&sentinel_path).expect("read updated sentinel");
	assert!(
		repeated_text.contains("religion = updated_sentinel"),
		"repeated merge must preserve the current output file; got:\n{repeated_text}"
	);

	let initially_missing_out = temp_dir.path().join("initially-missing-out");
	let generated_path = initially_missing_out
		.join("history")
		.join("countries")
		.join("TES - Test.txt");
	let initial = run(&initially_missing_out)
		.unwrap_or_else(|err| panic!("initial missing-output merge failed: {err}"));
	assert_eq!(initial.exit_code, 0);
	assert!(
		generated_path.is_file(),
		"first run should generate the missing output"
	);
	fs::write(
		&generated_path,
		"# edited after the first merge\nreligion = post_merge_edit\n",
	)
	.expect("edit first generated output");

	let after_edit = run(&initially_missing_out)
		.unwrap_or_else(|err| panic!("merge after editing generated output failed: {err}"));
	assert_eq!(after_edit.exit_code, 0);
	let after_edit_text = fs::read_to_string(&generated_path).expect("read preserved edit");
	assert!(
		after_edit_text.contains("religion = post_merge_edit"),
		"a current keep_existing rule must preserve an edit made after the prior merge; got:\n{after_edit_text}"
	);
}

#[test]
fn eu4_use_file_resolution_replaces_output_file_end_to_end() {
	let fixture = fixture_dir("eu4_two_mod_conflict");
	let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("merge-e2e");
	fs::create_dir_all(&scratch_root).expect("create merge e2e scratch root");
	let temp_dir = Builder::new()
		.prefix("eu4_use_file_resolution-")
		.tempdir_in(&scratch_root)
		.expect("create merge e2e tempdir");
	let out_dir = temp_dir.path().join("out");
	let game_root = temp_dir.path().join("eu4-game");
	fs::create_dir_all(&game_root).expect("create fixture game root");

	let external_file = temp_dir.path().join("manual-resolution.txt");
	let external_bytes =
		b"# external whole-file resolution\nreligion = external_resolution\ncapital = 999\n";
	fs::write(&external_file, external_bytes).expect("write external resolution file");
	let config_path = temp_dir.path().join("foch.use-file.toml");
	fs::write(
		&config_path,
		format!(
			r#"[[resolutions]]
file = "history/countries/TES - Test.txt"
use_file = {}
"#,
			toml_path_value(&external_file)
		),
	)
	.expect("write use_file config");

	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_root);
	let analyzed = analyze_merge(
		InputRequest::from_playset_path(
			fixture.join("dlc_load.json"),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		),
		MergeAnalysisOptions {
			out_dir: out_dir.clone(),
			include_game_base: false,
			include_base: false,
			gui_scroll_merge: false,
			force: true,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_config_path: Some(config_path),
			interactive_conflict_handler: None,
			interactive_resolution_config_path: None,
			playset_fingerprint: None,
			provenance: false,
			merge_workers: std::num::NonZeroUsize::new(2).unwrap(),
			retained_paths: None,
		},
		&NoopProgressObserver,
		&CancellationToken::new(),
	)
	.expect("analyze use_file merge");
	fs::write(
		&external_file,
		"# changed while the prepared plan was under review\n",
	)
	.expect("mutate external resolution file");
	let result = analyzed
		.commit(CommitAuthorization::EmptyTargetOnly)
		.expect("commit analyzed use_file merge");

	assert_eq!(
		result.exit_code, 0,
		"use_file merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"use_file should resolve the file conflict; report: {:#?}",
		result.report
	);
	let output_file = out_dir
		.join("history")
		.join("countries")
		.join("TES - Test.txt");
	assert_eq!(
		fs::read(&output_file).expect("read use_file output"),
		external_bytes,
		"use_file should materialize the external file bytes verbatim"
	);
	let external_source = toml_path(&external_file);
	assert!(
		result.report.handler_resolutions.iter().any(|record| {
			record.action == "external"
				&& record.source.as_deref() == Some(external_source.as_str())
		}),
		"use_file materialization should be audited as an external handler resolution; report: {:#?}",
		result.report
	);

	MERGE_TEMP_DIRS
		.get_or_init(|| Mutex::new(Vec::new()))
		.lock()
		.expect("merge tempdir registry lock")
		.push(temp_dir);
}

#[test]
fn eu4_semantic_interactive_use_file_is_frozen_by_analysis() {
	let fixture = fixture_dir("eu4_two_mod_conflict");
	let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("merge-e2e");
	fs::create_dir_all(&scratch_root).expect("create merge e2e scratch root");
	let temp_dir = Builder::new()
		.prefix("eu4_interactive_use_file-")
		.tempdir_in(&scratch_root)
		.expect("create merge e2e tempdir");
	let out_dir = temp_dir.path().join("out");
	let game_root = temp_dir.path().join("eu4-game");
	fs::create_dir_all(&game_root).expect("create fixture game root");

	let external_file = temp_dir.path().join("interactive-resolution.txt");
	let analyzed_bytes = b"selected during analysis\n";
	fs::write(&external_file, analyzed_bytes).expect("write initial external file");
	let interactive_config = temp_dir.path().join("foch.interactive.toml");
	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_root);
	let analyzed = analyze_merge(
		InputRequest::from_playset_path(
			fixture.join("dlc_load.json"),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		),
		MergeAnalysisOptions {
			out_dir: out_dir.clone(),
			include_game_base: false,
			include_base: false,
			gui_scroll_merge: false,
			force: false,
			ignore_replace_path: false,
			dep_overrides: Vec::new(),
			resolution_config_path: None,
			interactive_conflict_handler: Some(Box::new(UseFileHandler {
				path: external_file.clone(),
			})),
			interactive_resolution_config_path: Some(interactive_config.clone()),
			playset_fingerprint: None,
			provenance: false,
			merge_workers: std::num::NonZeroUsize::new(2).unwrap(),
			retained_paths: None,
		},
		&NoopProgressObserver,
		&CancellationToken::new(),
	)
	.expect("analyze interactive use_file merge");
	fs::write(&external_file, b"changed after analysis\n").expect("update external file");
	let result = analyzed
		.commit(CommitAuthorization::EmptyTargetOnly)
		.expect("commit interactive use_file merge");

	assert_eq!(result.exit_code, 0, "report: {:#?}", result.report);
	assert_eq!(result.report.manual_conflict_count, 0);
	assert_eq!(
		fs::read(out_dir.join("history/countries/TES - Test.txt")).expect("read output"),
		analyzed_bytes
	);
	let persisted = fs::read_to_string(interactive_config).expect("read persisted decision");
	assert!(persisted.contains("use_file"), "{persisted}");
	assert!(
		result
			.report
			.handler_resolutions
			.iter()
			.any(|record| record.action == "external"),
		"report: {:#?}",
		result.report
	);

	MERGE_TEMP_DIRS
		.get_or_init(|| Mutex::new(Vec::new()))
		.lock()
		.expect("merge tempdir registry lock")
		.push(temp_dir);
}

#[test]
fn eu4_conflict_id_use_file_does_not_mask_second_unresolved_leaf_conflict() {
	let source_fixture = fixture_dir("eu4_two_mod_conflict");
	let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("merge-e2e");
	fs::create_dir_all(&scratch_root).expect("create merge e2e scratch root");
	let temp_dir = Builder::new()
		.prefix("eu4_use_file_with_unresolved_leaf-")
		.tempdir_in(&scratch_root)
		.expect("create merge e2e tempdir");
	let playset = temp_dir.path().join("playset");
	copy_dir_recursive(&source_fixture, &playset);

	for (mod_dir, culture) in [("conflict_a", "french"), ("conflict_b", "german")] {
		let file = playset
			.join("mods")
			.join(mod_dir)
			.join("history")
			.join("countries")
			.join("TES - Test.txt");
		let text = fs::read_to_string(&file).expect("read copied country file");
		fs::write(
			&file,
			text.replace(
				"primary_culture = english",
				&format!("primary_culture = {culture}"),
			),
		)
		.expect("write copied country file with second conflict");
	}

	let external_file = temp_dir.path().join("manual-resolution.txt");
	fs::write(
		&external_file,
		"# external whole-file resolution\nreligion = external_resolution\n",
	)
	.expect("write external resolution file");
	let target_rel = "history/countries/TES - Test.txt";
	let religion_conflict_id = compute_conflict_id(
		foch::model::GamePath::new(target_rel).expect("valid game path"),
		"",
		"religion",
	);
	let config_path = temp_dir.path().join("foch.one-conflict-use-file.toml");
	fs::write(
		&config_path,
		format!(
			r#"[[resolutions]]
conflict_id = "{religion_conflict_id}"
use_file = {}
"#,
			toml_path_value(&external_file)
		),
	)
	.expect("write conflict_id use_file config");
	let out_dir = temp_dir.path().join("out");
	let game_root = temp_dir.path().join("eu4-game");
	fs::create_dir_all(&game_root).expect("create fixture game root");

	let result = run_merge_for_playset(
		&playset.join("dlc_load.json"),
		out_dir.clone(),
		game_root,
		true,
		Some(config_path),
	);

	// A conflict_id-scoped use_file is a whole-file replacement only after the
	// file has no remaining unresolved leaf conflicts. Otherwise it would hide
	// unrelated manual work the user did not resolve with the external file.
	assert_eq!(
		result.exit_code, 0,
		"partial merge with a manual conflict marker should still exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(result.report.status, MergeReportStatus::PartialSuccess);
	assert!(
		result.report.manual_conflict_count >= 1,
		"the second leaf conflict must remain visible; report: {:#?}",
		result.report
	);
	assert!(
		result
			.report
			.conflict_resolutions
			.iter()
			.flat_map(|resolution| resolution.leaf_conflicts.iter())
			.any(|leaf| leaf.address_key == "primary_culture"),
		"the unresolved leaf should be primary_culture; report: {:#?}",
		result.report
	);
	assert!(
		!result
			.report
			.handler_resolutions
			.iter()
			.any(|record| record.action == "external"),
		"external write must not be audited before materialization; report: {:#?}",
		result.report
	);
	assert!(
		{
			let output_text = fs::read_to_string(game_path(target_rel).to_path(&out_dir))
				.expect("read partial output file");
			output_text.contains("FOCH_MERGE_CONFLICT")
				&& output_text.contains("primary_culture")
				&& !output_text.contains("external_resolution")
		},
		"partial output should contain a manual marker for the unresolved leaf, not the external whole-file resolution"
	);

	MERGE_TEMP_DIRS
		.get_or_init(|| Mutex::new(Vec::new()))
		.lock()
		.expect("merge tempdir registry lock")
		.push(temp_dir);
}

#[test]
fn eu4_priority_boost_overrides_load_order_winner() {
	// priority_boost is the e2e contract that an explicit mod-level precedence
	// override affects structural merge arbitration.
	let (result, out_dir) = run_merge_for_fixture("eu4_priority_boost", false);
	assert_eq!(
		result.exit_code, 0,
		"priority_boost merge should exit 0; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"priority_boost should resolve the shared event without manual conflicts; report: {:#?}",
		result.report
	);

	let merged_event_path = out_dir.join("events").join("test_events.txt");
	assert!(
		merged_event_path.is_file(),
		"merged event file must be materialized at {}",
		merged_event_path.display()
	);
	let merged_text = fs::read_to_string(&merged_event_path).expect("read merged event file");
	assert!(
		merged_text.contains("foch_300001_title"),
		"priority_boost should make mod 300001 win; got:
{merged_text}"
	);
	assert!(
		!merged_text.contains("foch_300002_title"),
		"priority_boost should override the natural load-order winner 300002; got:
{merged_text}"
	);
}

#[test]
fn structured_merge_allows_an_explicit_empty_base() {
	let fixture = fixture_dir("eu4_priority_boost");
	let temp_dir = tempfile::tempdir().expect("create structured merge tempdir");
	let out_dir = temp_dir.path().join("out");
	let prior_file = out_dir.join("events/test_events.txt");
	let game_root = temp_dir.path().join("empty-eu4-game");
	fs::create_dir_all(&game_root).expect("create empty game root");
	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_root);

	let result = run_merge_for_evaluation(
		InputRequest::from_playset_path(
			fixture.join("dlc_load.json"),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		),
		MergeAnalysisOptions {
			out_dir: out_dir.clone(),
			include_game_base: false,
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
			merge_workers: std::num::NonZeroUsize::new(2).unwrap(),
			retained_paths: Some(
				[GamePathBuf::parse("events/test_events.txt").expect("valid game path")].into(),
			),
		},
		MergeBackendId::GumtreePcsNway,
	)
	.expect("an explicitly disabled game base should permit an empty semantic base");
	assert_eq!(result.exit_code, 0, "report: {:#?}", result.report);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Ready,
		"report: {:#?}",
		result.report
	);
	let merged_text = fs::read_to_string(prior_file).expect("read merged output");
	assert!(merged_text.contains("foch_300001_title"), "{merged_text}");
	assert!(!merged_text.contains("foch_300002_title"), "{merged_text}");
}

#[test]
fn structured_merge_rejects_a_copy_through_unit_without_claiming_kernel_success() {
	let fixture = fixture_dir("eu4_minimal_passthrough");
	let temp_dir = tempfile::tempdir().expect("create structured merge tempdir");
	let out_dir = temp_dir.path().join("out");
	let game_root = temp_dir.path().join("empty-eu4-game");
	fs::create_dir_all(&game_root).expect("create empty game root");
	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_root);

	let error = run_merge_for_evaluation(
		InputRequest::from_playset_path(
			fixture.join("dlc_load.json"),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		),
		MergeAnalysisOptions {
			out_dir: out_dir.clone(),
			include_game_base: false,
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
			merge_workers: std::num::NonZeroUsize::new(2).unwrap(),
			retained_paths: Some(
				[GamePathBuf::parse("events/foo.txt").expect("valid game path")].into(),
			),
		},
		MergeBackendId::GumtreePcsNway,
	)
	.expect_err("structured merge must reject a copy-through unit");
	let message = error.to_string();
	assert!(
		message.contains("structured merge unsupported")
			&& message.contains("planned as copy_through")
			&& message.contains("candidate kernel was not invoked"),
		"unexpected structured rejection: {message}"
	);
	assert!(
		!out_dir.exists(),
		"a non-kernel structured run must not commit copy-through output"
	);
}

#[test]
fn eu4_on_actions_from_other_files_add_to_each_other() {
	// Patch 1.36 made on_actions additive: each mod's own file that defines
	// `on_startup` runs alongside the base file's, so none of them is an edit
	// of another and every file keeps its own definition.
	let (result, out_dir) = run_merge_for_fixture("eu4_on_actions_additive", false);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"{:#?}",
		result.report
	);
	let on_actions = out_dir.join("common").join("on_actions");
	assert!(
		!on_actions.join("zzz_foch_on_actions.txt").exists(),
		"additive on_actions must not be folded into one module"
	);
	for (file, event) in [
		("00_on_actions.txt", "base.1"),
		("a_on_actions.txt", "a.1"),
		("b_on_actions.txt", "b.1"),
	] {
		let path = on_actions.join(file);
		if path.exists() {
			let text = fs::read_to_string(&path).expect("read");
			assert!(text.contains(event), "{file}: {text}");
		}
	}
}

#[test]
fn eu4_replaced_trigger_chain_merges_branch_by_branch_through_the_dag() {
	// One mod replaces Persia's DLC split with one condition list, another adds
	// a culture to both branches. The merge rewrites its inputs branch by
	// branch, and the DAG lineage must still trace every node to its mod.
	let (result, out_dir) = run_merge_for_fixture("eu4_trigger_chain_replaced", false);
	assert_eq!(
		result.report.manual_conflict_count, 0,
		"{:#?}",
		result.report
	);
	let decision = fs::read_to_string(out_dir.join("decisions").join("PersianNation.txt"))
		.expect("merged decision");
	for condition in ["was_tag = AKK", "primary_culture = azeri_culture"] {
		assert_eq!(decision.matches(condition).count(), 1, "{decision}");
	}
	assert!(!decision.contains("has_dlc"), "{decision}");
}

/// Without an interactive handler, the review still carries each conflict's
/// competing candidates and links them to the playset's mods.
#[test]
fn eu4_review_exposes_rendered_candidates_without_an_interactive_handler() {
	let fixture = fixture_dir("eu4_two_mod_conflict");
	let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("merge-e2e");
	fs::create_dir_all(&scratch_root).expect("create merge e2e scratch root");
	let temp_dir = Builder::new()
		.prefix("eu4_review_candidates-")
		.tempdir_in(&scratch_root)
		.expect("create merge e2e tempdir");
	let game_root = temp_dir.path().join("eu4-game");
	fs::create_dir_all(&game_root).expect("create fixture game root");

	let analyzed = analyze_merge_for_playset(
		&fixture.join("dlc_load.json"),
		temp_dir.path().join("out"),
		game_root,
		false,
		None,
	);
	let review = analyzed.review();

	let mod_ids = review
		.mods()
		.iter()
		.map(|node| node.mod_id.as_str())
		.collect::<Vec<_>>();
	let [unit] = review.conflicts() else {
		panic!("one conflicted unit: {review:#?}");
	};
	assert_eq!(unit.unit_id, "file:history/countries/TES - Test.txt");
	let leaves = unit
		.nodes
		.iter()
		.flat_map(|node| node.conflicts.iter().map(move |leaf| (node, leaf)))
		.collect::<Vec<_>>();
	let (node, religion) = leaves
		.iter()
		.find(|(node, _)| node.segment == "religion")
		.expect("religion conflict leaf");
	let rendered = religion
		.candidates
		.iter()
		.map(|candidate| candidate.rendered.trim())
		.collect::<Vec<_>>();
	// Without a game base every mod inserts its own `religion`.
	assert_eq!(
		rendered,
		[
			"religion = catholic",
			"religion = orthodox",
			"religion = protestant"
		],
		"{religion:#?}"
	);
	for candidate in &religion.candidates {
		assert!(mod_ids.contains(&candidate.mod_id.as_str()));
		assert_ne!(candidate.mod_display_name, candidate.mod_id);
	}
	assert_eq!(religion.vanilla_snippet, None);
	assert!(religion.reason.is_some());
	let point = review
		.decisions()
		.iter()
		.find(|point| point.conflict_id == religion.conflict_id)
		.expect("religion decision point");
	assert_eq!(point.node_id, node.id);
	assert_eq!(
		review.units()[0].conflict_ids,
		leaves
			.iter()
			.map(|(_, leaf)| leaf.conflict_id.clone())
			.collect::<Vec<_>>()
	);
}
