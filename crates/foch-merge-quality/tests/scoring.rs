//! Reproducible, network-free merge-quality scoring over the committed corpus
//! plus a local, version-bound vanilla text archive.
//!
//! The acceptance fixture uses two compressed archives: committed
//! `corpus.tar.gz` contains the deduplicated full Workshop-like layout
//! (`workshop/<steam_id>/...`) and `corpus.json`; local
//! `basegame-text.tar.gz` contains every text file from the version-bound
//! vanilla installation. Full context is required because foch's merge strategy
//! depends on workspace-wide validation.
//!
//! The proprietary `basegame-text.tar.gz` payload is intentionally ignored by
//! git, so this acceptance test must be requested explicitly on a machine that
//! has extracted it. CI still compiles the complete test harness.
//!
//! `tests/fixtures/expected.json` is the committed baseline: `compatch_id ->
//! { verdict -> count }`. Regenerate both with `foch-mq run` + `extract-fixtures`
//! when the corpus grows. See `fixtures/CREDITS.md` for provenance + takedown.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use foch_core::domain::game::Game;
use foch_engine::{
	BASE_DATA_SCHEMA_VERSION, BaseDataSource, FileFilter, build_base_snapshot,
	install_built_snapshot, load_installed_base_snapshot,
};
use foch_language::analysis_version::analysis_rules_version;
use foch_merge_quality::corpus::Corpus;
use foch_merge_quality::orchestrate::{BaseGameMode, score_case_from_paths_with_cache};
use foch_merge_quality::score::ScoreCache;

static BASE_DATA_ENV_LOCK: Mutex<()> = Mutex::new(());

struct FixtureBaseData {
	previous_data_root: Option<std::ffi::OsString>,
}

impl FixtureBaseData {
	fn install(game_root: &Path) -> Self {
		let version = foch_merge_quality::config::detect_game_version(game_root)
			.expect("fixture base game has a version");
		let data_root = fixture_base_data_cache_root(game_root, &version);
		std::fs::create_dir_all(&data_root).expect("create fixture base-data cache root");
		let previous_data_root = std::env::var_os("FOCH_DATA_DIR");
		unsafe {
			std::env::set_var("FOCH_DATA_DIR", &data_root);
		}
		let game = Game::EuropaUniversalis4;
		match load_installed_base_snapshot(game.key(), &version, None) {
			Ok(Some(_)) => eprintln!("[fixture] base snapshot cache hit: {}", data_root.display()),
			Ok(None) => {
				eprintln!(
					"[fixture] base snapshot cache miss: {}",
					data_root.display()
				);
				let filter = FileFilter::for_game(game.clone());
				let built = build_base_snapshot(&game, game_root, Some(&version), &filter)
					.expect("build fixture base snapshot");
				install_built_snapshot(
					&built.encoded_snapshot,
					BaseDataSource::Build,
					Some(built.snapshot_asset_name),
					Some(built.snapshot_sha256),
				)
				.expect("install fixture base snapshot");
			}
			Err(err) => panic!("load cached fixture base snapshot: {err}"),
		}
		prune_stale_base_data_caches(&data_root);
		Self { previous_data_root }
	}
}

impl Drop for FixtureBaseData {
	fn drop(&mut self) {
		unsafe {
			if let Some(previous) = self.previous_data_root.take() {
				std::env::set_var("FOCH_DATA_DIR", previous);
			} else {
				std::env::remove_var("FOCH_DATA_DIR");
			}
		}
	}
}

fn fixture_base_data_cache_root(game_root: &Path, game_version: &str) -> std::path::PathBuf {
	let fixture_root = game_root.parent().expect("basegame has a fixture root");
	let manifest: serde_json::Value = serde_json::from_slice(
		&std::fs::read(fixture_root.join("basegame-manifest.json"))
			.expect("read basegame manifest"),
	)
	.expect("parse basegame manifest");
	let raw_tree_digest = manifest["content_hash"]
		.as_str()
		.expect("basegame manifest has a content hash");
	let cache_key = blake3::hash(
		format!(
			"eu4\0{game_version}\0{raw_tree_digest}\0{}\0{}",
			BASE_DATA_SCHEMA_VERSION,
			analysis_rules_version()
		)
		.as_bytes(),
	)
	.to_hex();
	fixture_root
		.join(".base-data")
		.join(&cache_key.as_str()[..20])
}

fn prune_stale_base_data_caches(current: &Path) {
	let Some(parent) = current.parent() else {
		return;
	};
	let Ok(entries) = std::fs::read_dir(parent) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if path != current && path.is_dir() {
			let _ = std::fs::remove_dir_all(path);
		}
	}
}

/// Every committed fixture reproduces its base-aware baseline verdict tally. Data-driven:
/// add a case by running `extract-fixtures` + regenerating `expected.json` —
/// no test code change needed.
#[test]
#[ignore = "requires local tests/fixtures/basegame-text.tar.gz"]
fn committed_corpus_reproduces_base_aware_baseline() {
	let _guard = BASE_DATA_ENV_LOCK
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	let expected = common::expected_verdicts();
	assert!(!expected.is_empty(), "baseline has at least one case");

	// Reuse the immutable committed corpus by the combined archive hash.
	let corpus = common::cached_base_aware_corpus_root();
	let fixture_corpus_text =
		std::fs::read_to_string(corpus.join("corpus.json")).expect("read fixture corpus");
	let fixture_corpus = Corpus::from_json(&fixture_corpus_text).expect("parse fixture corpus");
	let workshop = corpus.join("workshop");
	let basegame = corpus.join("basegame");
	assert!(
		basegame.join("version.txt").is_file(),
		"committed quality fixture must include a version-bound vanilla text snapshot"
	);
	assert!(
		corpus.join("basegame-manifest.json").is_file(),
		"committed quality fixture must describe the vanilla text snapshot"
	);
	let archived_manifest: serde_json::Value = serde_json::from_slice(
		&std::fs::read(corpus.join("basegame-manifest.json")).expect("read archived base manifest"),
	)
	.expect("parse archived base manifest");
	let visible_manifest: serde_json::Value = serde_json::from_slice(
		&std::fs::read(common::fixtures_root().join("basegame-manifest.json"))
			.expect("read visible base manifest"),
	)
	.expect("parse visible base manifest");
	assert_eq!(
		archived_manifest, visible_manifest,
		"visible and archived base manifests must be identical"
	);
	let _base_data = FixtureBaseData::install(&basegame);
	let mut score_cache = ScoreCache::new();
	let mut actual = BTreeMap::new();

	for compatch_id in expected.keys() {
		let case = fixture_corpus
			.cases
			.iter()
			.find(|case| &case.compatch_id == compatch_id)
			.unwrap_or_else(|| panic!("fixture corpus contains case {compatch_id}"));
		let compatch = workshop.join(&case.compatch_id);
		let mod_dirs = case
			.referenced_mods
			.iter()
			.map(|id| workshop.join(id))
			.collect::<Vec<_>>();
		let out = tempfile::tempdir().expect("create fixture merge output");
		let result = score_case_from_paths_with_cache(
			case,
			&compatch,
			&mod_dirs,
			out.path(),
			BaseGameMode::Path(&basegame),
			None,
			&mut score_cache,
		)
		.expect("score full fixture case");
		actual.insert(compatch_id.clone(), result.multi_source_verdicts);
	}
	assert_eq!(actual, expected, "merge-quality verdict baseline drifted");
}
