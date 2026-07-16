use foch_core::config::compute_conflict_id;
use foch_core::domain::descriptor::load_descriptor;
use foch_core::model::{
	ConflictKind, MergeReportStatus, MergeTraceDecision, MergeTraceEntry, MergeTracePolicy,
};
use foch_engine::{CheckRequest, Config, MergeExecuteOptions, run_merge_with_options};
use foch_language::analyzer::content_family::{ContentLoadPolicy, GameProfile};
use foch_language::analyzer::definition_module::{DefinitionModuleInput, load_definition_module};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::parse_clausewitz_file;
use foch_language::analyzer::semantic_index::parse_script_file;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tempfile::{Builder, TempDir};

static MERGE_TEMP_DIRS: OnceLock<Mutex<Vec<TempDir>>> = OnceLock::new();

fn playsets_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
		.join("playsets")
}

fn fixture_dir(name: &str) -> PathBuf {
	playsets_root().join(name)
}

fn rel_path(rel: &str) -> PathBuf {
	rel.split('/').collect()
}

fn expected_path(name: &str, rel: &str) -> PathBuf {
	fixture_dir(name).join("expected").join(rel_path(rel))
}

// Reads a fixture playset from `crates/foch-engine/tests/fixtures/playsets/<name>/`,
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
/// conflict-scenario tests. Returns the full [`MergeExecutionResult`] plus
/// the output dir so tests can assert on report fields, status, and the
/// produced tree without the wrapper enforcing its own success contract.
fn run_merge_for_fixture(name: &str, force: bool) -> (foch_engine::MergeExecutionResult, PathBuf) {
	run_merge_for_fixture_inner(
		name, force, /*provenance=*/ false, /*gui_scroll_merge=*/ false,
	)
}

fn run_merge_for_fixture_with_gui_scroll(
	name: &str,
	force: bool,
	gui_scroll_merge: bool,
) -> (foch_engine::MergeExecutionResult, PathBuf) {
	run_merge_for_fixture_inner(name, force, /*provenance=*/ false, gui_scroll_merge)
}

fn run_merge_for_fixture_with_provenance(
	name: &str,
	force: bool,
) -> (foch_engine::MergeExecutionResult, PathBuf) {
	run_merge_for_fixture_inner(
		name, force, /*provenance=*/ true, /*gui_scroll_merge=*/ false,
	)
}

fn run_merge_for_fixture_inner(
	name: &str,
	force: bool,
	provenance: bool,
	gui_scroll_merge: bool,
) -> (foch_engine::MergeExecutionResult, PathBuf) {
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
		CheckRequest::from_playset_path(
			fixture.join("dlc_load.json"),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		),
		MergeExecuteOptions {
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
) -> foch_engine::MergeExecutionResult {
	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_root);
	run_merge_with_options(
		CheckRequest::from_playset_path(
			playset_path.to_path_buf(),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		),
		MergeExecuteOptions {
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
			retained_paths: None,
		},
	)
	.expect("run merge with custom playset")
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

fn toml_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
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
	let top = top.to_string_lossy().to_ascii_lowercase();
	matches!(
		top.as_str(),
		"common" | "events" | "missions" | "decisions" | "history"
	)
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
// at `crates/foch-engine/tests/fixtures/playsets/<name>/expected/<rel>`.
// Honours BLESS_SNAPSHOTS=1 by copying the actual output to the expected tree.
fn assert_matches_golden(name: &str, out_dir: &Path, rel: &str) {
	let actual = out_dir.join(rel_path(rel));
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

fn assert_output_matches_fixture_input(name: &str, mod_name: &str, out_dir: &Path, rel: &str) {
	let input = fixture_dir(name)
		.join("mods")
		.join(mod_name)
		.join(rel_path(rel));
	let actual = out_dir.join(rel_path(rel));
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
#[ignore = "requires vendor/cwtools-eu4-config, output/cwtools-eu4-config, or FOCH_CWTOOLS_SCHEMA_DIR"]
fn eu4_schema_cardinality_conflict_is_tagged_from_cwt() {
	let (result, out_dir) = run_merge_for_fixture("eu4_schema_cardinality_conflict", false);
	assert_eq!(
		result.exit_code, 2,
		"strict schema-cardinality merge should block; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Blocked,
		"strict schema-cardinality merge should be blocked; report: {:#?}",
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

	// The in-memory report and the on-disk sidecar both carry the same map.
	let file_prov = result
		.report
		.definition_provenance
		.iter()
		.find(|(path, _)| {
			path.replace('\\', "/")
				.ends_with("scripted_effects/zzz_foch_scripted_effects.txt")
		})
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

	assert_structurally_sound(&out_dir);
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
		.find(|(path, _)| {
			path.replace('\\', "/")
				.ends_with("scripted_effects/zzz_foch_scripted_effects.txt")
		})
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
fn eu4_gfx_sprite_types_same_name_divergence_conflicts() {
	let (result, _out_dir) =
		run_merge_for_fixture("eu4_gfx_sprite_types_same_name_conflict", false);
	assert_eq!(
		result.exit_code, 2,
		"same-name divergent spriteType merge should block; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Blocked,
		"same-name divergent spriteType merge should be blocked; report: {:#?}",
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
		vec!["common/governments".to_string()]
	);

	let parsed_output = parse_script_file("generated", &out_dir, &merged_path)
		.expect("parse generated governments module");
	let relative = Path::new("common/governments/zzz_foch_governments.txt");
	let descriptor = eu4_profile()
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
			.any(|path| path == "common/institutions")
	);
}

#[test]
fn eu4_gui_edit_wins_over_remove_keeps_the_edit() {
	// One mod edits a widget property (orientation) while another removes it.
	// GUI families opt into edit-wins, so the edit is kept and no manual
	// conflict is surfaced — a GUI "remove" is typically a trimmed widget copy
	// not re-shipping a field, not an intentional delete that should veto a
	// sibling mod's edit. Contrast eu4_mixed_kinds_conflict (history family,
	// flag off), where the same SetValue-vs-RemoveNode shape stays a conflict.
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
	let (blocked, _blocked_out_dir) =
		run_merge_for_fixture("eu4_gui_scroll_stack_same_name_conflict", false);
	assert_eq!(
		blocked.exit_code, 2,
		"same-name divergent GUI merge without flag should block; report: {:#?}",
		blocked.report
	);
	assert_eq!(
		blocked.report.status,
		MergeReportStatus::Blocked,
		"same-name divergent GUI merge without flag should be blocked; report: {:#?}",
		blocked.report
	);
	assert!(
		blocked.report.manual_conflict_count >= 1,
		"same-name divergent GUI containers must surface a manual conflict without the flag; report: {:#?}",
		blocked.report
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
fn eu4_mixed_kinds_set_value_vs_remove_node_reports_conflict() {
	let (result, out_dir) = run_merge_for_fixture("eu4_mixed_kinds_conflict", false);
	assert_eq!(
		result.exit_code, 2,
		"strict mixed-kinds merge should block; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Blocked,
		"strict mixed-kinds merge should be blocked; report: {:#?}",
		result.report
	);
	assert!(
		result.report.manual_conflict_count >= 1,
		"mixed SetValue/RemoveNode edits must surface a manual conflict; report: {:#?}",
		result.report
	);
	assert!(
		result
			.report
			.conflict_resolutions
			.iter()
			.any(|resolution| resolution.reason.contains("mixed patch kinds")),
		"mixed-kinds conflict reason should mention mixed patch kinds; report: {:#?}",
		result.report
	);
	assert!(out_dir.exists(), "out dir should still be materialized");
}

#[test]
fn eu4_recurse_policy_emits_conflict_on_divergent_sub_blocks() {
	let (result, out_dir) = run_merge_for_fixture("eu4_recurse_block_conflict", false);
	assert_eq!(
		result.exit_code, 2,
		"strict recurse merge should block; report: {:#?}",
		result.report
	);
	assert_eq!(
		result.report.status,
		MergeReportStatus::Blocked,
		"strict recurse merge should be blocked; report: {:#?}",
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

	let run = |target_out: &Path| {
		let mut game_path = HashMap::new();
		game_path.insert("eu4".to_string(), game_root.clone());
		run_merge_with_options(
			CheckRequest::from_playset_path(
				fixture.join("dlc_load.json"),
				Config {
					steam_root_path: None,
					paradox_data_path: None,
					game_path,
					extra_ignore_patterns: Vec::new(),
				},
			),
			MergeExecuteOptions {
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
				retained_paths: None,
			},
		)
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

	fs::write(
		&sentinel_path,
		"# updated between identical merge requests\nreligion = updated_sentinel\n",
	)
	.expect("update pre-existing sentinel");
	let repeated = run(&out_dir)
		.unwrap_or_else(|err| panic!("repeated fixture eu4_handler_keep_existing failed: {err}"));
	assert_eq!(repeated.exit_code, 0);
	assert_eq!(
		repeated.report.cache_source, None,
		"keep_existing output is mutable state and must bypass a prior modset cache entry"
	);
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
	assert_eq!(after_edit.report.cache_source, None);
	let after_edit_text = fs::read_to_string(&generated_path).expect("read preserved edit");
	assert!(
		after_edit_text.contains("religion = post_merge_edit"),
		"a current keep_existing rule must bypass cache even when the cached run had no prior file; got:\n{after_edit_text}"
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
use_file = "{}"
"#,
			toml_path(&external_file)
		),
	)
	.expect("write use_file config");

	let result = run_merge_for_playset(
		&fixture.join("dlc_load.json"),
		out_dir.clone(),
		game_root,
		true,
		Some(config_path),
	);

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
	let religion_conflict_id = compute_conflict_id(Path::new(target_rel), "", "religion");
	let config_path = temp_dir.path().join("foch.one-conflict-use-file.toml");
	fs::write(
		&config_path,
		format!(
			r#"[[resolutions]]
conflict_id = "{religion_conflict_id}"
use_file = "{}"
"#,
			toml_path(&external_file)
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
			let output_text = fs::read_to_string(out_dir.join(rel_path(target_rel)))
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
