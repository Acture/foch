//! Pure helpers for assembling a runtime layer and reading its logs.
//!
//! Launching a copied `eu4.exe` keeps the original install read-only, but it
//! reintroduces Windows' `MAX_PATH` limit: the game opens files by
//! `<runtime dir>\<relative path>`, and anything past 259 characters silently
//! fails to load. These helpers keep that budget and the log scan testable
//! without a game.

use foch_test::compile::Bundle;

/// Windows caps a classic path at 260 chars including the terminator.
pub const MAX_PATH: usize = 260;

/// Whether every game file still fits under `MAX_PATH` when the install is
/// reached through a runtime directory of `runtime_dir_len` characters.
/// `longest_relative` is the longest `<dir>\<file>` path below the install,
/// relative to its root.
pub fn fits_path_budget(runtime_dir_len: usize, longest_relative: usize) -> bool {
	// runtime dir + separator + relative path + NUL terminator.
	runtime_dir_len + longest_relative + 2 <= MAX_PATH
}

/// The console command file for a session: the bundle's own commands, with
/// `ai` prepended to disable AI for deterministic results.
pub fn command_file(bundle: &Bundle, disable_ai: bool) -> String {
	let base = bundle
		.files
		.get("commands.txt")
		.map(String::as_str)
		.unwrap_or_default();
	if disable_ai {
		format!("ai\n{base}")
	} else {
		base.to_string()
	}
}

/// A windowed, muted copy of the player's `settings.txt`, so a test launch
/// never goes fullscreen or plays sound. Keys absent from the template are
/// left as the engine defaults them.
pub fn isolated_settings(template: &str) -> String {
	let mut out = String::with_capacity(template.len());
	for line in template.split_inclusive('\n') {
		let trimmed = line.trim_start();
		let indent = &line[..line.len() - trimmed.len()];
		let rewritten = if let Some((key, _)) = trimmed.split_once('=') {
			match key.trim() {
				"fullScreen" => Some(format!("{indent}fullScreen=no\n")),
				"borderless" => Some(format!("{indent}borderless=no\n")),
				"master_volume" | "music_volume" | "sound_fx_volume" | "ambient_volume"
				| "dev_master_volume" => Some(format!("{indent}{}=0.000000\n", key.trim())),
				_ => None,
			}
		} else {
			None
		};
		out.push_str(&rewritten.unwrap_or_else(|| line.to_string()));
	}
	if !out.ends_with('\n') {
		out.push('\n');
	}
	out
}

/// A `dlc_load.json` enabling exactly the source mod and generated test layer.
pub fn dlc_load(mod_files: &[&str]) -> String {
	let mods: Vec<String> = mod_files
		.iter()
		.map(|name| format!("\"mod/{name}\""))
		.collect();
	format!(
		"{{\"enabled_mods\":[{}],\"disabled_dlcs\":[]}}",
		mods.join(",")
	)
}

/// Whether the engine log shows an `END` record for every case in the
/// session, meaning the run is complete and the game can be stopped.
pub fn all_cases_ended(log: &str, bundle: &Bundle) -> bool {
	let marker = &bundle.marker_prefix;
	bundle.cases.iter().all(|&case| {
		let needle = format!("{} END", bundle.case_prefix(case));
		log.lines()
			.any(|line| line.contains(marker.as_str()) && line.trim_end().ends_with(&needle))
	})
}

/// Classifies a top-level game-directory entry for the runtime layer: small
/// loose files are copied so the engine can read and rewrite them freely;
/// large data directories are linked so nothing is duplicated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerAction {
	Copy,
	Link,
	Skip,
}

/// The launcher, checksum patcher and stale console history never belong in a
/// fresh runtime layer; everything else loose is copied, directories linked.
pub fn classify_entry(name: &str, is_dir: bool) -> LayerAction {
	let lower = name.to_ascii_lowercase();
	if lower == "plugins" || foch::plugin::store::is_proxy_name(&lower) {
		return LayerAction::Skip;
	}
	if is_dir {
		return LayerAction::Link;
	}
	let skip = matches!(
		lower.as_str(),
		"universal-checksum-patcher.exe" | "console_history.txt" | "userdir.txt"
	);
	if skip {
		LayerAction::Skip
	} else {
		LayerAction::Copy
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_test::compile::{Bundle, Launch, Requirements};
	use foch_test::model::{AiMode, CaseIndex, GameDate, RunId, Tag};
	use foch_test::plan::SessionId;
	use std::collections::BTreeMap;

	fn bundle() -> Bundle {
		let date = GameDate::parse("1444.11.11").unwrap();
		Bundle {
			protocol: 2,
			run_id: RunId::parse("run-1").unwrap(),
			mod_name: "demo".into(),
			session: SessionId(0),
			cases: vec![CaseIndex(0), CaseIndex(2)],
			marker_prefix: "FOCH_TEST_V2 run-1 abc123".into(),
			namespace: "foch_test_demo_abc123".into(),
			test_mod_name: "demo tests [abc123]".into(),
			files: BTreeMap::from([(
				"commands.txt".to_string(),
				"speed 0\nrun start.txt\nspeed 5\n".to_string(),
			)]),
			launch: Launch {
				args: vec!["-start_tag=SWE".into()],
				ai: AiMode::Off,
				player: Tag::parse("SWE").unwrap(),
				start_date: date,
				end_date: date,
			},
			checks: Vec::new(),
			windows: Vec::new(),
			requires: Requirements {
				start_date: date,
				ai_off: true,
				shared_session: false,
			},
		}
	}

	#[test]
	fn path_budget_matches_windows_max_path() {
		// 127 is EU4's longest relative path; a 131-char runtime dir just fits.
		assert!(fits_path_budget(131, 127));
		assert!(!fits_path_budget(132, 127));
	}

	#[test]
	fn command_file_prepends_ai_only_when_disabling() {
		let bundle = bundle();
		assert_eq!(
			command_file(&bundle, false),
			"speed 0\nrun start.txt\nspeed 5\n"
		);
		assert_eq!(
			command_file(&bundle, true),
			"ai\nspeed 0\nrun start.txt\nspeed 5\n"
		);
	}

	#[test]
	fn settings_are_windowed_and_muted_preserving_other_keys() {
		let template = "graphics={\n\tfullScreen=yes\n\tborderless=yes\n\tsize={ x=1920 }\n}\nmaster_volume=100.000000\nmusic_volume=50.000000\ndifficulty=\"LUCK_NONE\"\n";
		let out = isolated_settings(template);
		assert!(out.contains("\tfullScreen=no\n"));
		assert!(out.contains("\tborderless=no\n"));
		assert!(out.contains("master_volume=0.000000\n"));
		assert!(out.contains("music_volume=0.000000\n"));
		assert!(
			out.contains("\tsize={ x=1920 }\n"),
			"unrelated keys preserved"
		);
		assert!(out.contains("difficulty=\"LUCK_NONE\"\n"));
	}

	#[test]
	fn all_cases_ended_requires_every_case() {
		let bundle = bundle();
		let line = |case: usize, record: &str| {
			format!(
				"[x]: EVENT [1444.11.11]:{} c{case} {record}\n",
				bundle.marker_prefix
			)
		};
		let mut log = String::new();
		log.push_str(&line(0, "END"));
		assert!(!all_cases_ended(&log, &bundle), "case 2 has not ended");
		log.push_str(&line(2, "PASS scope"));
		assert!(!all_cases_ended(&log, &bundle));
		log.push_str(&line(2, "END"));
		assert!(all_cases_ended(&log, &bundle));
	}

	#[test]
	fn layer_classification_copies_files_links_dirs_skips_noise() {
		assert_eq!(classify_entry("eu4.exe", false), LayerAction::Copy);
		assert_eq!(classify_entry("d3d9.dll", false), LayerAction::Skip);
		assert_eq!(classify_entry("VERSION.DLL", false), LayerAction::Skip);
		assert_eq!(classify_entry("Plugins", true), LayerAction::Skip);
		assert_eq!(classify_entry("steam_api64.dll", false), LayerAction::Copy);
		assert_eq!(classify_entry("common", true), LayerAction::Link);
		assert_eq!(
			classify_entry("console_history.txt", false),
			LayerAction::Skip
		);
		assert_eq!(classify_entry("userdir.txt", false), LayerAction::Skip);
	}

	#[test]
	fn dlc_load_enables_source_and_test_mods() {
		assert_eq!(
			dlc_load(&["source_mod.mod", "test_mod.mod"]),
			"{\"enabled_mods\":[\"mod/source_mod.mod\",\"mod/test_mod.mod\"],\"disabled_dlcs\":[]}"
		);
	}
}
