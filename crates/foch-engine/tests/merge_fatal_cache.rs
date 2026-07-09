//! Integration test: a FATAL merge result must not be cached in the modset cache.
//!
//! This runs as its own binary (separate `tests/` file) so that env-var writes
//! to `FOCH_MODSET_CACHE_DIR` and `FOCH_DATA_DIR` are isolated from the parallel
//! merge e2e test suite.  Any shared-binary approach would contaminate other
//! tests that write real cache entries into the same dir.

use foch_core::model::MergeReportStatus;
use foch_engine::{CheckRequest, Config, MergeExecuteOptions, run_merge_with_options};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn playsets_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("fixtures")
		.join("playsets")
}

fn scratch_root() -> PathBuf {
	let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("target")
		.join("merge-fatal-cache");
	fs::create_dir_all(&root).expect("create scratch root");
	root
}

/// Count `.tar.gz` files directly inside `dir`; returns 0 when `dir` is absent.
fn count_tarballs(dir: &PathBuf) -> usize {
	let Ok(rd) = fs::read_dir(dir) else {
		return 0;
	};
	rd.flatten()
		.filter(|e| e.file_name().to_string_lossy().ends_with(".tar.gz"))
		.count()
}

/// A FATAL merge (triggered by an empty base-data dir) must NOT be written to
/// the modset cache.  After the fix, no `{key}.tar.gz` exists in the cache
/// dir, and a second identical run is a cache miss (not a replay).
#[test]
fn fatal_merge_is_not_cached() {
	let scratch = scratch_root();

	// Isolated temp dirs — dropped (cleaned up) when the test exits.
	let cache_dir = TempDir::new_in(&scratch).expect("cache temp dir");
	let data_dir = TempDir::new_in(&scratch).expect("data temp dir (intentionally empty)");
	let game_dir = TempDir::new_in(&scratch).expect("game temp dir");

	// `version.txt` lets `detect_game_version` succeed, so `modset_cache_game_version`
	// returns `Some(...)` and a cache context is built.  But `FOCH_DATA_DIR` is an
	// empty dir, so `load_installed_base_snapshot` returns `Ok(None)`, which
	// `resolve_workspace` converts to `WorkspaceResolveError { kind: Io }`, which
	// `run_merge_plan_with_options` turns into a fatal error.
	fs::write(game_dir.path().join("version.txt"), "1.37.0\n").expect("write version.txt");

	// Set env vars before any parallel work begins; restore at end.
	// SAFETY: this binary is single-threaded at this point; no rayon/tokio
	// workers are live yet.
	unsafe {
		std::env::set_var("FOCH_MODSET_CACHE_DIR", cache_dir.path());
		std::env::set_var("FOCH_DATA_DIR", data_dir.path());
	}

	let fixture = playsets_root().join("eu4_minimal_passthrough");

	let make_request = || {
		let mut game_path = HashMap::new();
		game_path.insert("eu4".to_string(), game_dir.path().to_path_buf());
		CheckRequest::from_playset_path(
			fixture.join("dlc_load.json"),
			Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		)
	};

	let make_options = |suffix: &str| {
		let out_dir = scratch.join(suffix);
		fs::create_dir_all(&out_dir).expect("create out dir");
		MergeExecuteOptions {
			out_dir,
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
			retained_paths: None,
		}
	};

	// --- Run 1 -----------------------------------------------------------
	let result1 = run_merge_with_options(make_request(), make_options("out-run1"))
		.expect("first run must not return Err");

	// Precondition: the run must be FATAL (empty FOCH_DATA_DIR → no installed snapshot)
	assert_eq!(
		result1.report.status,
		MergeReportStatus::Fatal,
		"precondition failed: expected Fatal but got {:?}. \
         Verify that FOCH_DATA_DIR is empty and include_game_base=true.",
		result1.report.status,
	);

	// After a FATAL run, no tarball should exist in the cache.
	let modsets_dir = cache_dir.path().join("modsets");
	let tar_count = count_tarballs(&modsets_dir);
	assert_eq!(
		tar_count,
		0,
		"FATAL merge must not write a cache entry; found {tar_count} .tar.gz file(s) in {}",
		modsets_dir.display(),
	);

	// --- Run 2 (same inputs) ---------------------------------------------
	let result2 = run_merge_with_options(make_request(), make_options("out-run2"))
		.expect("second run must not return Err");

	assert_ne!(
		result2.report.cache_source.as_deref(),
		Some("modset"),
		"second run must not replay from modset cache after a FATAL first run; \
         got cache_source={:?}",
		result2.report.cache_source,
	);

	// Belt-and-suspenders restore (process exits after the test regardless).
	unsafe {
		std::env::remove_var("FOCH_MODSET_CACHE_DIR");
		std::env::remove_var("FOCH_DATA_DIR");
	}
}
