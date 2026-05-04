use foch_core::model::MergeReportStatus;
use foch_engine::{CheckRequest, Config, MergeExecuteOptions, run_merge_with_options};
use foch_language::analyzer::parser::parse_clausewitz_file;
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
	let (result, out_dir) =
		run_merge_for_fixture(name, /*force=*/ true, /*fallback=*/ false);
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
fn run_merge_for_fixture(
	name: &str,
	force: bool,
	fallback: bool,
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
		CheckRequest {
			playset_path: fixture.join("dlc_load.json"),
			config: Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		},
		MergeExecuteOptions {
			out_dir: out_dir.clone(),
			include_game_base: false,
			force,
			ignore_replace_path: false,
			fallback,
			dep_overrides: Vec::new(),
			playset_fingerprint: None,
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
#[ignore = "unignore after fix/string-corruption-emit is rebased; master currently corrupts string scalar emission"]
fn eu4_string_corruption_fixture_is_structurally_sound() {
	let out = run_merge_fixture("eu4_string_corruption");
	assert_structurally_sound(&out);
}

#[test]
#[ignore = "unignore after fix/string-corruption-emit is rebased and the golden snapshot is blessed"]
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
fn eu4_minimal_passthrough_copies_single_contributor_files_byte_for_byte() {
	let out = run_merge_fixture("eu4_minimal_passthrough");
	assert_structurally_sound(&out);

	for rel in [
		"common/defines.lua",
		"localisation/minimal_l_english.yml",
		"events/foo.txt",
		"common/cultures/00_cultures.txt",
	] {
		assert_output_matches_fixture_input("eu4_minimal_passthrough", "minimal", &out, rel);
		assert_matches_golden("eu4_minimal_passthrough", &out, rel);
	}
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
// baseline contributor's `catholic`. Once leaf-level resolvers stop
// silently picking the highest-precedence patch this should produce a
// per-key SetValue conflict at `religion`.
//
// Both tests are currently `#[ignore]` because the engine's leaf resolvers
// (resolve_set_values default branch in patch_merge.rs) silently fall back
// to LastWriter on same-key sibling conflicts, so the engine reports
// `manual_conflict_count = 0` and never invokes the handler chain. Once the
// `rec-default-step1` / leaf-conflict-fix slice lands these should turn
// green automatically:
//   - eu4_two_mod_conflict_without_foch_toml_reports_manual_conflict
//     verifies the strict path (no foch.toml override) reports the
//     conflict instead of silently picking conflict_b's religion.
//   - eu4_two_mod_conflict_resolved_via_last_writer_handler verifies the
//     `[[resolutions]] match = "history/**" handler = "last_writer"` route
//     records a HandlerResolutionRecord and lands conflict_b's religion
//     in the merged output.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "blocked on leaf-conflict-fix: engine currently silent-LastWriter for same-key SetValue (see plan.md and rec-default-step1)"]
fn eu4_two_mod_conflict_without_foch_toml_reports_manual_conflict() {
	let (result, out_dir) = run_merge_for_fixture("eu4_two_mod_conflict", false, false);
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
	assert_eq!(
		result.report.fallback_resolved_count, 0,
		"strict merge must not silently fall back; report: {:#?}",
		result.report
	);
	assert!(out_dir.exists(), "out dir should still be materialized");
}

#[test]
#[ignore = "blocked on leaf-conflict-fix: engine currently silent-LastWriter for same-key SetValue (see plan.md and rec-default-step1)"]
fn eu4_two_mod_conflict_resolved_via_last_writer_handler() {
	let (result, out_dir) = run_merge_for_fixture("eu4_two_mod_conflict_resolved", false, false);
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
	assert_eq!(
		result.report.fallback_resolved_count, 0,
		"resolution should be attributed to the handler, not the fallback path; report: {:#?}",
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
