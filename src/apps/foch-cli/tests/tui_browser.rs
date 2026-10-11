//! The bare-`foch` browser flow over a real fixture analysis:
//! launch → inspect → analyze → filter → detail → explicit refresh, with no
//! filesystem mutation anywhere in the inputs.
//!
//! This binary holds exactly one test: it points the process-wide Foch
//! configuration and cache roots at scratch directories before any thread
//! starts.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use foch::input::{Config, CurrentEu4Input};
use foch::merge::MergeDisposition;
use foch_cli::tui::browser::{
	AnalysisInput, BrowserSource, Focus, Inspection, Phase, Screen, Session, draw,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use walkdir::WalkDir;

const CONFLICT_PATH: &str = "events/conflict.txt";

struct FixtureSource {
	root: PathBuf,
	playlist: PathBuf,
	inspections: AtomicUsize,
}

impl BrowserSource for FixtureSource {
	fn inspect(&self) -> Inspection {
		self.inspections.fetch_add(1, Ordering::SeqCst);
		let mut config = Config::default();
		config
			.game_path
			.insert("eu4".to_string(), self.root.join("eu4-game"));
		Inspection {
			input: displayed_input(&self.playlist),
			analysis: Some(AnalysisInput::Path {
				path: self.playlist.clone(),
				config,
			}),
			game_base_available: false,
		}
	}
}

fn displayed_input(playlist: &Path) -> CurrentEu4Input {
	serde_json::from_value(json!({
		"readiness": "ready",
		"game": {
			"name": "Europa Universalis IV",
			"version": "1.37.5",
			"installPath": null
		},
		"baseData": {
			"state": "missing",
			"version": null,
			"detail": "fixture analyzes without the game base"
		},
		"playset": {
			"name": "Fixture playset",
			"sourcePath": playlist,
			"mods": [
				{
					"id": "7251", "name": "Fixture A", "position": 1, "enabled": true,
					"workshopId": "7251", "workshopManifestId": null, "version": "1.0.0",
					"declaredDependencies": [], "descriptorPath": null, "sourceError": null
				},
				{
					"id": "7252", "name": "Fixture B", "position": 2, "enabled": true,
					"workshopId": "7252", "workshopManifestId": null, "version": "1.0.0",
					"declaredDependencies": ["Fixture A"], "descriptorPath": null,
					"sourceError": null
				}
			]
		},
		"issues": [],
		"recovery": null
	}))
	.expect("fixture input view")
}

fn stage_playset(root: &Path) -> PathBuf {
	let playlist = root.join("playlist.json");
	fs::create_dir_all(root.join("mod")).expect("mod metadata dir");
	fs::create_dir_all(root.join("eu4-game")).expect("game root");
	fs::write(
		&playlist,
		json!({
			"enabled_mods": ["mod/ugc_7251.mod", "mod/ugc_7252.mod"],
			"disabled_dlcs": [],
		})
		.to_string(),
	)
	.expect("write dlc_load");
	for (id, name) in [("7251", "Fixture A"), ("7252", "Fixture B")] {
		let mod_root = root.join(id);
		fs::create_dir_all(&mod_root).expect("mod root");
		fs::write(
			mod_root.join("descriptor.mod"),
			format!("name=\"{name}\"\nversion=\"1.0.0\"\n"),
		)
		.expect("descriptor");
		let path = foch::playset::descriptor::descriptor_path_text(&mod_root)
			.expect("descriptor path text");
		fs::write(
			root.join("mod").join(format!("ugc_{id}.mod")),
			format!("name=\"{name}\"\npath=\"{path}\"\nremote_file_id=\"{id}\"\n"),
		)
		.expect("ugc descriptor");
	}
	write(
		root,
		"7251",
		CONFLICT_PATH,
		"country_event = { id = test.1 }\n",
	);
	// Malformed Clausewitz that names no definition, so no part of it can be
	// repaired or isolated: the structural merge reports unsupported input.
	write(
		root,
		"7252",
		CONFLICT_PATH,
		"} name { = invalid syntax with unclosed\nbraces\n",
	);
	fs::create_dir_all(root.join("7251/gfx")).expect("gfx dir");
	fs::write(root.join("7251/gfx/only-a.dds"), [0, 1, 2]).expect("asset");
	playlist
}

fn write(root: &Path, mod_id: &str, relative: &str, content: &str) {
	let path = root.join(mod_id).join(relative);
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	fs::write(path, content).expect("script file");
}

/// Every path under `root` with its bytes (`None` for a directory).
fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
	WalkDir::new(root)
		.into_iter()
		.map(|entry| entry.expect("walk fixture"))
		.map(|entry| {
			let bytes = entry
				.file_type()
				.is_file()
				.then(|| fs::read(entry.path()).expect("read fixture file"));
			(entry.path().to_path_buf(), bytes)
		})
		.collect()
}

fn key(session: &mut Session, code: KeyCode) {
	assert!(
		session.handle_key(KeyEvent::new(code, KeyModifiers::NONE)),
		"{code:?} must not quit"
	);
}

fn screen(session: &Session) -> String {
	let mut terminal = Terminal::new(TestBackend::new(140, 40)).expect("test terminal");
	terminal
		.draw(|frame| draw(frame, session.app()))
		.expect("draw browser");
	let buffer = terminal.backend().buffer();
	(0..buffer.area.height)
		.map(|y| {
			(0..buffer.area.width)
				.map(|x| buffer[(x, y)].symbol())
				.collect::<String>()
		})
		.collect::<Vec<_>>()
		.join("\n")
}

fn assert_shows(session: &Session, expected: &str) {
	let rendered = screen(session);
	assert!(
		rendered.contains(expected),
		"expected the browser to show {expected:?}; got:\n{rendered}"
	);
}

#[test]
fn browser_inspects_analyzes_filters_details_and_refreshes_without_writing() {
	let fixture = TempDir::new().expect("fixture dir");
	let user = TempDir::new().expect("user dir");
	// SAFETY: this is the binary's only test and no other thread runs yet.
	unsafe {
		std::env::set_var("FOCH_CONFIG_DIR", user.path().join("config"));
		std::env::set_var("FOCH_CACHE_ROOT", user.path().join("cache"));
		std::env::set_var("HOME", user.path().join("home"));
		std::env::set_var("XDG_DATA_HOME", user.path().join("xdg-data"));
	}
	let playlist = stage_playset(fixture.path());
	let before = tree(fixture.path());
	let source = Arc::new(FixtureSource {
		root: fixture.path().to_path_buf(),
		playlist,
		inspections: AtomicUsize::new(0),
	});

	// Launch: the first screen shows the base, playset, ordered mods and
	// their declared dependencies before anything is analyzed.
	let mut session = Session::new(source.clone());
	session.wait_idle();
	assert!(matches!(session.app().phase, Phase::Inspected));
	assert_shows(&session, "Fixture playset");
	assert_shows(&session, "1 Fixture A");
	assert_shows(&session, "2 Fixture B");
	// This input has no EU4 base data: analyzing without the vanilla
	// ancestor is an explicit choice, never a silent fallback.
	assert_shows(&session, "The EU4 base data is not ready.");
	key(&mut session, KeyCode::Char('a'));
	assert!(matches!(session.app().phase, Phase::Inspected));
	key(&mut session, KeyCode::Char('o'));
	assert_shows(&session, "(base data not ready)");
	key(&mut session, KeyCode::Char(' '));
	key(&mut session, KeyCode::Esc);
	assert!(!session.app().settings.game_base);
	assert_shows(&session, "Press  a  to analyze all 2 mods.");
	key(&mut session, KeyCode::Down);
	assert_shows(&session, "depends on: Fixture A");

	// x leaves a mod out; the banner names the matching CLI command.
	assert_shows(&session, "--out <OUT> --no-game-base --non-interactive");
	key(&mut session, KeyCode::Char('x'));
	assert_shows(&session, "--exclude 7252");
	assert_shows(&session, "excluded");
	key(&mut session, KeyCode::Char('x'));
	assert!(session.app().excluded_positions().is_empty());

	// Analyze every contributor.
	key(&mut session, KeyCode::Char('a'));
	assert!(matches!(session.app().phase, Phase::Analyzing { .. }));
	session.wait_idle();
	let analysis = match &session.app().phase {
		Phase::Reviewed(view) => view.clone(),
		Phase::Failed(error) => panic!("analysis failed: {error}"),
		other => panic!("unexpected phase {other:?}"),
	};
	assert_eq!(session.app().screen, Screen::Review);
	assert_eq!(analysis.summary.unsupported_input, 1, "{analysis:?}");
	assert_shows(&session, "4 unsupported_input 1");
	assert_shows(&session, "5 engine_failure 0");
	// The input snapshot was analyzed once; only a refresh analyzes again.
	key(&mut session, KeyCode::Char('a'));
	assert!(!session.is_busy());

	// Filter to the unit that needs review.
	key(&mut session, KeyCode::Char('4'));
	let visible = session.app().visible_units();
	assert_eq!(visible.len(), 1);
	let unit = session.app().selected_unit().expect("selected unit");
	assert_eq!(unit.disposition, MergeDisposition::UnsupportedInput);
	assert_eq!(unit.path.as_str(), CONFLICT_PATH);

	// Its detail names the missing ancestor and both contributors.
	key(&mut session, KeyCode::Enter);
	assert_eq!(session.app().focus, Focus::Detail);
	assert_shows(&session, "unsupported_input");
	assert_shows(&session, "none: no vanilla definition at this unit");
	assert_shows(&session, "Contributors (precedence order)");
	assert_shows(&session, "Fixture A");
	assert_shows(&session, "Fixture B");
	key(&mut session, KeyCode::Esc);

	// Search across every disposition.
	key(&mut session, KeyCode::Char('0'));
	key(&mut session, KeyCode::Char('/'));
	for value in "only-a".chars() {
		key(&mut session, KeyCode::Char(value));
	}
	key(&mut session, KeyCode::Enter);
	let unit = session.app().selected_unit().expect("searched unit");
	assert_eq!(unit.path.as_str(), "gfx/only-a.dds");
	assert_eq!(session.app().visible_units().len(), 1);

	// The playset view stays reachable from the review.
	key(&mut session, KeyCode::Char('i'));
	assert_eq!(session.app().screen, Screen::Input);
	assert_shows(&session, "depends on: Fixture A");
	key(&mut session, KeyCode::Char('i'));
	assert_eq!(session.app().screen, Screen::Review);

	// Options change only the next analysis.
	key(&mut session, KeyCode::Char('o'));
	assert_shows(&session, "Analysis options");
	key(&mut session, KeyCode::Down);
	key(&mut session, KeyCode::Down);
	key(&mut session, KeyCode::Char(' '));
	assert!(session.app().settings.ignore_replace_path);
	key(&mut session, KeyCode::Esc);
	assert!(session.app().options.is_none());
	assert!(session.app().settings_changed());
	assert_shows(&session, "Options changed: press r to re-analyze.");

	// An explicit refresh takes a new input snapshot and a new analysis.
	key(&mut session, KeyCode::Char('r'));
	assert!(session.is_busy());
	session.wait_idle();
	assert_eq!(source.inspections.load(Ordering::SeqCst), 2);
	assert!(matches!(session.app().phase, Phase::Reviewed(_)));
	assert_shows(&session, "4 unsupported_input 1");
	let (analyzed_with, _) = session
		.app()
		.analyzed_with
		.clone()
		.expect("analysis settings");
	assert!(analyzed_with.ignore_replace_path);
	assert!(!analyzed_with.game_base);
	assert!(!session.app().settings_changed());

	assert!(!session.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
	drop(session);
	assert_eq!(
		tree(fixture.path()),
		before,
		"the browser must not change its inputs"
	);
	assert!(
		!user.path().join("config").exists(),
		"the browser must not initialize configuration"
	);
}
