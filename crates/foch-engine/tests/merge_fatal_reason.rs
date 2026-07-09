//! Integration test: a FATAL merge caused by failed workspace resolution must
//! surface *why* it failed in the report (`fatal_reason`), mirroring `foch
//! check`. Otherwise the user sees `status: FATAL` with zero diagnostics.
//!
//! Runs as its own binary (separate `tests/` file) so its env-var writes to
//! `FOCH_DATA_DIR` are isolated from the parallel merge e2e suite.

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
		.join("merge-fatal-reason");
	fs::create_dir_all(&root).expect("create scratch root");
	root
}

/// With `include_game_base=true` and an empty `FOCH_DATA_DIR`, resolution fails
/// because no installed base snapshot exists.  The merge ends FATAL, and the
/// report must carry the resolve error message (the WHY) so the user knows to
/// rerun `foch data install` — not just an opaque `status: FATAL`.
#[test]
fn fatal_merge_surfaces_resolve_error_reason() {
	let scratch = scratch_root();

	let data_dir = TempDir::new_in(&scratch).expect("data temp dir (intentionally empty)");
	let game_dir = TempDir::new_in(&scratch).expect("game temp dir");

	// `version.txt` lets `detect_game_version` succeed; the empty `FOCH_DATA_DIR`
	// then makes `load_installed_base_snapshot` yield no snapshot, so
	// `resolve_workspace` returns the "missing installed base data" error.
	fs::write(game_dir.path().join("version.txt"), "1.37.0\n").expect("write version.txt");

	// SAFETY: this binary is single-threaded at this point; no rayon/tokio
	// workers are live yet.
	unsafe {
		std::env::set_var("FOCH_DATA_DIR", data_dir.path());
	}

	let fixture = playsets_root().join("eu4_minimal_passthrough");

	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_dir.path().to_path_buf());
	let request = CheckRequest::from_playset_path(
		fixture.join("dlc_load.json"),
		Config {
			steam_root_path: None,
			paradox_data_path: None,
			game_path,
			extra_ignore_patterns: Vec::new(),
		},
	);

	let out_dir = scratch.join("out");
	fs::create_dir_all(&out_dir).expect("create out dir");
	let options = MergeExecuteOptions {
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
	};

	let report = run_merge_with_options(request, options)
		.expect("run must not return Err")
		.report;

	assert_eq!(
		report.status,
		MergeReportStatus::Fatal,
		"precondition failed: expected Fatal (empty FOCH_DATA_DIR + include_game_base=true)"
	);

	let reason = report
		.fatal_reason
		.as_deref()
		.expect("fatal merge from failed resolution must carry a fatal_reason");
	assert!(
		reason.contains("base data") && reason.contains("foch data install"),
		"fatal_reason must surface the resolve cause + install hint; got: {reason:?}"
	);

	unsafe {
		std::env::remove_var("FOCH_DATA_DIR");
	}
}
