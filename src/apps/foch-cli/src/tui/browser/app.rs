use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Instant;

use crate::cli::arg::MergeArgs;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use foch::input::{CurrentEu4Input, DetectedPlaysetMod};
use foch::merge::{
	AnalyzedMerge, MergeAnalysisStatus, MergeDisposition, MergeProgress, MergeReviewSummary,
	MergeUnitOutcome, default_merge_workers,
};

const PAGE_STEP: usize = 10;

/// Every disposition, in the order the summary and filter keys list them.
pub const DISPOSITIONS: [MergeDisposition; 6] = [
	MergeDisposition::Safe,
	MergeDisposition::Copy,
	MergeDisposition::NeedsUserChoice,
	MergeDisposition::UnsupportedInput,
	MergeDisposition::EngineFailure,
	MergeDisposition::Deferred,
];

pub fn disposition_label(disposition: MergeDisposition) -> &'static str {
	match disposition {
		MergeDisposition::Safe => "safe",
		MergeDisposition::Copy => "copy",
		MergeDisposition::NeedsUserChoice => "needs_user_choice",
		MergeDisposition::UnsupportedInput => "unsupported_input",
		MergeDisposition::EngineFailure => "engine_failure",
		MergeDisposition::Deferred => "deferred",
	}
}

/// A finished analysis, detached from the frozen artifacts it was read from.
#[derive(Clone, Debug)]
pub struct AnalysisView {
	pub status: MergeAnalysisStatus,
	pub summary: MergeReviewSummary,
	pub units: Vec<MergeUnitOutcome>,
}

impl AnalysisView {
	pub fn from_analyzed(analyzed: &AnalyzedMerge) -> Self {
		Self {
			status: analyzed.analysis().status(),
			summary: *analyzed.review_summary(),
			units: analyzed.list_units().to_vec(),
		}
	}

	pub fn count(&self, disposition: MergeDisposition) -> usize {
		let summary = &self.summary;
		match disposition {
			MergeDisposition::Safe => summary.safe,
			MergeDisposition::Copy => summary.copy,
			MergeDisposition::NeedsUserChoice => summary.needs_user_choice,
			MergeDisposition::UnsupportedInput => summary.unsupported_input,
			MergeDisposition::EngineFailure => summary.engine_failure,
			MergeDisposition::Deferred => summary.deferred,
		}
	}
}

/// The analysis switches the browser can change. They live only in memory
/// and apply to the next analysis; nothing is saved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnalysisSettings {
	/// Use the analyzed EU4 base as the merge ancestor (`--no-game-base`
	/// turns this off).
	pub game_base: bool,
	/// `--gui-scroll-merge`.
	pub gui_scroll_merge: bool,
	/// `--ignore-replace-path`.
	pub ignore_replace_path: bool,
	/// `--force`: emit supported fallbacks for deferred conflicts.
	pub force: bool,
	pub merge_workers: NonZeroUsize,
}

impl Default for AnalysisSettings {
	fn default() -> Self {
		Self {
			game_base: true,
			gui_scroll_merge: false,
			ignore_replace_path: false,
			force: false,
			merge_workers: default_merge_workers(),
		}
	}
}

/// Rows in the options panel, in [`AnalysisSettings`] field order.
pub const OPTION_COUNT: usize = 5;
const WORKERS_OPTION: usize = 4;

impl AnalysisSettings {
	fn toggle(&mut self, option: usize) {
		match option {
			0 => self.game_base = !self.game_base,
			1 => self.gui_scroll_merge = !self.gui_scroll_merge,
			2 => self.ignore_replace_path = !self.ignore_replace_path,
			3 => self.force = !self.force,
			_ => {}
		}
	}

	fn adjust_workers(&mut self, more: bool) {
		let workers = self.merge_workers.get();
		let workers = if more {
			workers.saturating_add(1)
		} else {
			workers - 1
		};
		self.merge_workers = NonZeroUsize::new(workers).unwrap_or(NonZeroUsize::MIN);
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Screen {
	Input,
	Review,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
	Units,
	Detail,
	Search,
}

#[derive(Clone, Debug)]
pub enum Phase {
	Inspecting,
	Inspected,
	Analyzing {
		started: Instant,
		progress: Option<MergeProgress>,
	},
	Reviewed(Box<AnalysisView>),
	Failed(String),
}

/// What the session must do in response to a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppCommand {
	Quit,
	Analyze,
	Refresh,
}

/// The browser's whole visible state. It only reads: no key leads to a
/// decision, an artifact, or a write.
#[derive(Debug)]
pub struct App {
	pub phase: Phase,
	pub screen: Screen,
	pub focus: Focus,
	pub input: Option<CurrentEu4Input>,
	/// Whether the current input snapshot can still be analyzed with some
	/// selection of its mods. Each snapshot is analyzed at most once.
	pub selectable: bool,
	/// Mods, by playset id, the user left out of the next analysis. Kept
	/// across refreshes; ids no longer in the playset are ignored.
	pub excluded: BTreeSet<String>,
	/// The INPUT_SOURCE `foch merge` takes for this input, or `None` for the
	/// current EU4 playset.
	pub source_path: Option<PathBuf>,
	pub mod_scroll: usize,
	pub filter: Option<MergeDisposition>,
	pub query: String,
	/// Index into [`App::visible_units`].
	pub selected: usize,
	pub detail_scroll: u16,
	pub settings: AnalysisSettings,
	/// The settings and exclusions of the analysis being run or browsed.
	pub analyzed_with: Option<(AnalysisSettings, BTreeSet<String>)>,
	/// Whether the input source can supply the EU4 base as an ancestor.
	pub game_base_available: bool,
	/// Cursor row while the options panel is open.
	pub options: Option<usize>,
	/// Asking whether to analyze without the excluded mods.
	pub confirming_exclusions: bool,
	/// Why the last key could not do what it asks, until the next key.
	pub refusal: Option<String>,
	analyze_after_inspection: bool,
}

impl Default for App {
	fn default() -> Self {
		Self {
			phase: Phase::Inspecting,
			screen: Screen::Input,
			focus: Focus::Units,
			input: None,
			selectable: false,
			excluded: BTreeSet::new(),
			source_path: None,
			mod_scroll: 0,
			filter: None,
			query: String::new(),
			selected: 0,
			detail_scroll: 0,
			settings: AnalysisSettings::default(),
			analyzed_with: None,
			game_base_available: true,
			options: None,
			confirming_exclusions: false,
			refusal: None,
			analyze_after_inspection: false,
		}
	}
}

impl App {
	pub fn analysis(&self) -> Option<&AnalysisView> {
		match &self.phase {
			Phase::Reviewed(view) => Some(view),
			_ => None,
		}
	}

	/// The settings an analysis started now would use.
	pub fn effective_settings(&self) -> AnalysisSettings {
		AnalysisSettings {
			game_base: self.settings.game_base && self.game_base_available,
			..self.settings
		}
	}

	/// Whether the options or exclusions changed since the browsed analysis
	/// ran.
	pub fn settings_changed(&self) -> bool {
		self.analyzed_with
			.as_ref()
			.is_some_and(|(settings, excluded)| {
				*settings != self.effective_settings() || *excluded != self.effective_exclusions()
			})
	}

	pub fn mods(&self) -> &[DetectedPlaysetMod] {
		self.input
			.as_ref()
			.and_then(|input| input.playset.as_ref())
			.map_or(&[], |playset| &playset.mods)
	}

	pub fn is_excluded(&self, playset_mod: &DetectedPlaysetMod) -> bool {
		self.excluded.contains(&playset_mod.id)
	}

	/// The exclusions that name a mod of the current playset.
	pub fn effective_exclusions(&self) -> BTreeSet<String> {
		self.mods()
			.iter()
			.filter(|playset_mod| self.is_excluded(playset_mod))
			.map(|playset_mod| playset_mod.id.clone())
			.collect()
	}

	/// Playset positions of the excluded mods, as input preparation takes
	/// them.
	pub fn excluded_positions(&self) -> BTreeSet<usize> {
		self.mods()
			.iter()
			.filter(|playset_mod| self.is_excluded(playset_mod))
			.map(|playset_mod| playset_mod.position)
			.collect()
	}

	/// Mods that cannot be analyzed and are not excluded yet.
	pub fn problem_mods(&self) -> Vec<&DetectedPlaysetMod> {
		self.mods()
			.iter()
			.filter(|playset_mod| {
				playset_mod.source_error.is_some() && !self.is_excluded(playset_mod)
			})
			.collect()
	}

	pub fn included_count(&self) -> usize {
		self.mods()
			.iter()
			.filter(|playset_mod| !self.is_excluded(playset_mod))
			.count()
	}

	pub fn can_analyze(&self) -> bool {
		self.selectable
			&& !self.is_working()
			&& self.problem_mods().is_empty()
			&& self.included_count() > 0
	}

	/// The `foch merge` arguments of the analysis `a` runs. The browser
	/// analyzes through exactly these, writing nothing, so `cli_command`
	/// reproduces its analysis.
	pub fn merge_args(&self, out: PathBuf) -> MergeArgs {
		let settings = self.effective_settings();
		MergeArgs {
			playset_path: self.source_path.clone(),
			out,
			force: settings.force,
			no_game_base: !settings.game_base,
			include_base: false,
			exclude: self
				.mods()
				.iter()
				.filter(|playset_mod| self.is_excluded(playset_mod))
				.map(|playset_mod| playset_mod.id.clone())
				.collect(),
			gui_scroll_merge: settings.gui_scroll_merge,
			ignore_replace_path: settings.ignore_replace_path,
			ignore_dep: Vec::new(),
			config: None,
			provenance: false,
			confirm: false,
			non_interactive: true,
			review_all: false,
			cli_prompt: false,
			jobs: (settings.merge_workers != default_merge_workers())
				.then_some(settings.merge_workers),
		}
	}

	/// The `foch` command that runs the analysis `a` would run, so a script
	/// or agent can reproduce it without the browser.
	pub fn cli_command(&self) -> String {
		let args = self.merge_args(PathBuf::from("<OUT>")).command_line();
		std::iter::once("foch".to_string())
			.chain(args.into_iter().map(|arg| {
				if arg.contains(char::is_whitespace) {
					format!("\"{arg}\"")
				} else {
					arg
				}
			}))
			.collect::<Vec<_>>()
			.join(" ")
	}

	pub fn is_working(&self) -> bool {
		matches!(self.phase, Phase::Inspecting | Phase::Analyzing { .. })
	}

	/// Indices of the units that pass the disposition filter and the query,
	/// in review order.
	pub fn visible_units(&self) -> Vec<usize> {
		let Some(view) = self.analysis() else {
			return Vec::new();
		};
		let query = self.query.to_lowercase();
		view.units
			.iter()
			.enumerate()
			.filter(|(_, unit)| self.filter.is_none_or(|filter| unit.disposition == filter))
			.filter(|(_, unit)| {
				query.is_empty()
					|| unit.id.to_lowercase().contains(&query)
					|| unit.family.to_lowercase().contains(&query)
			})
			.map(|(index, _)| index)
			.collect()
	}

	pub fn selected_unit(&self) -> Option<&MergeUnitOutcome> {
		let index = *self.visible_units().get(self.selected)?;
		self.analysis()?.units.get(index)
	}

	pub(super) fn inspecting(&mut self, analyze_after: bool) {
		self.phase = Phase::Inspecting;
		self.confirming_exclusions = false;
		self.selectable = false;
		self.analyze_after_inspection = analyze_after;
		self.focus = Focus::Units;
	}

	pub(super) fn inspected(
		&mut self,
		input: CurrentEu4Input,
		selectable: bool,
		game_base_available: bool,
	) {
		self.input = Some(input);
		self.source_path = None;
		self.game_base_available = game_base_available;
		self.selectable = selectable;
		self.phase = Phase::Inspected;
		self.screen = Screen::Input;
		self.mod_scroll = 0;
	}

	pub(super) fn take_analyze_after_inspection(&mut self) -> bool {
		if !std::mem::take(&mut self.analyze_after_inspection) || !self.can_analyze() {
			return false;
		}
		if !self.effective_exclusions().is_empty() {
			// A refresh never leaves mods out on its own; ask again.
			self.confirming_exclusions = true;
			return false;
		}
		true
	}

	pub(super) fn analyzing(&mut self) {
		self.selectable = false;
		self.analyzed_with = Some((self.effective_settings(), self.effective_exclusions()));
		self.phase = Phase::Analyzing {
			started: Instant::now(),
			progress: None,
		};
	}

	pub(super) fn progress(&mut self, update: MergeProgress) {
		if let Phase::Analyzing { progress, .. } = &mut self.phase {
			*progress = Some(update);
		}
	}

	pub(super) fn analyzed(&mut self, result: Result<AnalysisView, String>) {
		match result {
			Ok(view) => {
				self.phase = Phase::Reviewed(Box::new(view));
				self.screen = Screen::Review;
				self.focus = Focus::Units;
				self.selected = 0;
				self.detail_scroll = 0;
			}
			Err(error) => self.phase = Phase::Failed(error),
		}
	}

	pub fn handle_key(&mut self, key: KeyEvent) -> Option<AppCommand> {
		if key.kind == KeyEventKind::Release {
			return None;
		}
		if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
			return Some(AppCommand::Quit);
		}
		self.refusal = None;
		if self.focus == Focus::Search {
			self.handle_search_key(key);
			return None;
		}
		if let Some(cursor) = self.options {
			return self.handle_options_key(key, cursor);
		}
		if self.confirming_exclusions {
			return match key.code {
				KeyCode::Char('q') => Some(AppCommand::Quit),
				KeyCode::Char('y') => {
					self.confirming_exclusions = false;
					Some(AppCommand::Analyze)
				}
				KeyCode::Char('n') | KeyCode::Esc => {
					self.confirming_exclusions = false;
					None
				}
				_ => None,
			};
		}
		match key.code {
			KeyCode::Char('q') => return Some(AppCommand::Quit),
			KeyCode::Char('o') => {
				self.options = Some(0);
				return None;
			}
			KeyCode::Char('r') if !self.is_working() => return Some(AppCommand::Refresh),
			KeyCode::Char('a') if self.can_analyze() => {
				if !self.effective_exclusions().is_empty() {
					self.confirming_exclusions = true;
					return None;
				}
				return Some(AppCommand::Analyze);
			}
			KeyCode::Char('a') => {
				self.refusal = Some(self.analyze_refusal());
				return None;
			}
			KeyCode::Char('i') | KeyCode::Char('p') if self.analysis().is_some() => {
				self.screen = match self.screen {
					Screen::Input => Screen::Review,
					Screen::Review => Screen::Input,
				};
				return None;
			}
			_ => {}
		}
		match self.screen {
			Screen::Input => self.handle_input_key(key),
			Screen::Review => self.handle_review_key(key),
		}
		None
	}

	/// Why `a` cannot start an analysis right now.
	fn analyze_refusal(&self) -> String {
		let problems = self.problem_mods().len();
		match self.phase {
			Phase::Inspecting => {
				"Still inspecting the input; analysis can start when it finishes.".to_string()
			}
			Phase::Analyzing { .. } => "An analysis is already running.".to_string(),
			Phase::Reviewed(_) | Phase::Failed(_) => {
				"This input snapshot was already analyzed. Press r to re-inspect and analyze again."
					.to_string()
			}
			Phase::Inspected if !self.selectable => {
				"Cannot analyze: the issues listed under Issues block this input. Fix them, then press r."
					.to_string()
			}
			Phase::Inspected if problems > 0 => format!(
				"{problems} mod{} cannot be analyzed. Press X to exclude {}, or x on a selected mod.",
				if problems == 1 { "" } else { "s" },
				if problems == 1 { "it" } else { "them all" }
			),
			Phase::Inspected => "Every mod is excluded; press x to include one again.".to_string(),
		}
	}

	fn handle_options_key(&mut self, key: KeyEvent, cursor: usize) -> Option<AppCommand> {
		match key.code {
			KeyCode::Char('q') => return Some(AppCommand::Quit),
			KeyCode::Esc | KeyCode::Char('o') => self.options = None,
			KeyCode::Up | KeyCode::Char('k') => self.options = Some(cursor.saturating_sub(1)),
			KeyCode::Down | KeyCode::Char('j') => {
				self.options = Some((cursor + 1).min(OPTION_COUNT - 1));
			}
			KeyCode::Left | KeyCode::Char('-') if cursor == WORKERS_OPTION => {
				self.settings.adjust_workers(false);
			}
			KeyCode::Right | KeyCode::Char('+') if cursor == WORKERS_OPTION => {
				self.settings.adjust_workers(true);
			}
			KeyCode::Enter | KeyCode::Char(' ') => self.settings.toggle(cursor),
			_ => {}
		}
		None
	}

	fn handle_input_key(&mut self, key: KeyEvent) {
		match key.code {
			KeyCode::Char('x') => {
				if let Some(id) = self.mods().get(self.mod_scroll).map(|m| m.id.clone())
					&& !self.excluded.remove(&id)
				{
					self.excluded.insert(id);
				}
				return;
			}
			KeyCode::Char('X') => {
				let problems = self
					.problem_mods()
					.into_iter()
					.map(|playset_mod| playset_mod.id.clone())
					.collect::<Vec<_>>();
				self.excluded.extend(problems);
				return;
			}
			_ => {}
		}
		let last = self.mods().len().saturating_sub(1);
		self.mod_scroll = step(self.mod_scroll, key.code, last);
		if key.code == KeyCode::Esc && self.analysis().is_some() {
			self.screen = Screen::Review;
		}
	}

	fn handle_review_key(&mut self, key: KeyEvent) {
		if let KeyCode::Char(digit @ '0'..='6') = key.code {
			let filter = match digit {
				'0' => None,
				digit => Some(DISPOSITIONS[digit as usize - '1' as usize]),
			};
			self.set_filter(filter);
			return;
		}
		match (self.focus, key.code) {
			(_, KeyCode::Char('/')) => self.focus = Focus::Search,
			(Focus::Units, KeyCode::Enter | KeyCode::Right | KeyCode::Tab) => {
				if self.selected_unit().is_some() {
					self.focus = Focus::Detail;
				}
			}
			(Focus::Detail, KeyCode::Esc | KeyCode::Left | KeyCode::Tab) => {
				self.focus = Focus::Units;
			}
			(Focus::Detail, code) => {
				self.detail_scroll =
					step(self.detail_scroll as usize, code, u16::MAX as usize) as u16;
			}
			(Focus::Units, KeyCode::Esc) if !self.query.is_empty() => self.set_query(String::new()),
			(Focus::Units, code) => {
				let last = self.visible_units().len().saturating_sub(1);
				let selected = step(self.selected, code, last);
				if selected != self.selected {
					self.selected = selected;
					self.detail_scroll = 0;
				}
			}
			(Focus::Search, _) => unreachable!("search keys are handled first"),
		}
	}

	fn handle_search_key(&mut self, key: KeyEvent) {
		match key.code {
			KeyCode::Enter => self.focus = Focus::Units,
			KeyCode::Esc => {
				self.focus = Focus::Units;
				self.set_query(String::new());
			}
			KeyCode::Backspace => {
				let mut query = self.query.clone();
				query.pop();
				self.set_query(query);
			}
			KeyCode::Char(value) => {
				let mut query = self.query.clone();
				query.push(value);
				self.set_query(query);
			}
			_ => {}
		}
	}

	fn set_filter(&mut self, filter: Option<MergeDisposition>) {
		self.reselect(|app| app.filter = filter);
	}

	fn set_query(&mut self, query: String) {
		self.reselect(|app| app.query = query);
	}

	/// Change what is visible while keeping the selected unit selected when
	/// it is still visible.
	fn reselect(&mut self, change: impl FnOnce(&mut Self)) {
		let selected_id = self.selected_unit().map(|unit| unit.id.clone());
		change(self);
		let visible = self.visible_units();
		self.selected = selected_id
			.and_then(|id| {
				let units = &self.analysis()?.units;
				visible.iter().position(|index| units[*index].id == id)
			})
			.unwrap_or(0);
		self.detail_scroll = 0;
	}
}

fn step(current: usize, code: KeyCode, last: usize) -> usize {
	match code {
		KeyCode::Up | KeyCode::Char('k') => current.saturating_sub(1),
		KeyCode::Down | KeyCode::Char('j') => current.saturating_add(1).min(last),
		KeyCode::PageUp => current.saturating_sub(PAGE_STEP),
		KeyCode::PageDown => current.saturating_add(PAGE_STEP).min(last),
		KeyCode::Home | KeyCode::Char('g') => 0,
		KeyCode::End | KeyCode::Char('G') => last,
		_ => current,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	/// Mod 2 cannot be analyzed; mod 1 can.
	fn input_with_a_broken_mod() -> CurrentEu4Input {
		let playset_mod = |position: usize, id: &str, error: Option<&str>| {
			json!({
				"id": id, "name": format!("Mod {id}"), "position": position, "enabled": true,
				"workshopId": id, "workshopManifestId": null, "version": null,
				"declaredDependencies": [], "descriptorPath": null, "sourceError": error
			})
		};
		serde_json::from_value(json!({
			"readiness": "blocked",
			"game": { "name": "Europa Universalis IV", "version": "1.37.5", "installPath": null },
			"baseData": { "state": "ready", "version": "1.37.5", "detail": "ready" },
			"playset": {
				"name": "Current EU4 playset",
				"sourcePath": "dlc_load.json",
				"mods": [playset_mod(1, "41", None), playset_mod(2, "43", Some("missing"))]
			},
			"issues": [],
			"recovery": null
		}))
		.expect("input view")
	}

	fn press(app: &mut App, code: KeyCode) -> Option<AppCommand> {
		app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
	}

	#[test]
	fn a_broken_mod_must_be_excluded_and_the_exclusion_confirmed() {
		let mut app = App::default();
		app.inspected(input_with_a_broken_mod(), true, true);

		assert!(!app.can_analyze());
		assert_eq!(press(&mut app, KeyCode::Char('a')), None);
		assert!(
			app.refusal
				.as_deref()
				.is_some_and(|refusal| refusal.contains("Press X"))
		);

		press(&mut app, KeyCode::Char('X'));
		assert_eq!(app.excluded_positions(), BTreeSet::from([2]));
		assert!(app.can_analyze());
		assert!(app.cli_command().ends_with("--exclude 43"));

		assert_eq!(press(&mut app, KeyCode::Char('a')), None);
		assert!(app.confirming_exclusions);
		assert_eq!(press(&mut app, KeyCode::Esc), None);
		assert!(!app.confirming_exclusions);
		press(&mut app, KeyCode::Char('a'));
		assert_eq!(
			press(&mut app, KeyCode::Char('y')),
			Some(AppCommand::Analyze)
		);
	}

	/// The command the browser shows parses, through the CLI's own parser,
	/// back to exactly the arguments the browser analyzes with.
	#[test]
	fn the_shown_command_is_the_analysis_the_browser_runs() {
		use crate::cli::arg::{FochCli, FochCliCommands};
		use clap::Parser;

		let mut app = App::default();
		app.inspected(input_with_a_broken_mod(), true, true);
		app.excluded.insert("43".to_string());
		app.settings.game_base = false;
		app.settings.gui_scroll_merge = true;
		app.settings.ignore_replace_path = true;
		app.settings.force = true;
		app.settings.merge_workers = NonZeroUsize::new(3).expect("non-zero");
		for source_path in [None, Some(PathBuf::from("dir with space/dlc_load.json"))] {
			app.source_path = source_path;
			let expected = app.merge_args(PathBuf::from("<OUT>"));
			let argv = std::iter::once("foch".to_string()).chain(expected.command_line());
			let parsed = FochCli::try_parse_from(argv).expect("shown command parses");
			let Some(FochCliCommands::Merge(parsed)) = parsed.command else {
				panic!("shown command is not `foch merge`");
			};
			assert_eq!(parsed, expected);
		}
		assert!(
			app.cli_command()
				.starts_with("foch merge \"dir with space/dlc_load.json\"")
		);
	}

	#[test]
	fn x_toggles_any_selected_mod() {
		let mut app = App::default();
		app.inspected(input_with_a_broken_mod(), true, true);

		press(&mut app, KeyCode::Char('x'));
		assert_eq!(app.excluded_positions(), BTreeSet::from([1]));
		press(&mut app, KeyCode::Char('x'));
		assert!(app.excluded_positions().is_empty());
	}

	#[test]
	fn a_blocked_analyze_key_says_why_until_the_next_key() {
		let mut app = App::default();
		app.inspected(input_with_a_broken_mod(), false, true);

		assert_eq!(press(&mut app, KeyCode::Char('a')), None);
		assert!(
			app.refusal
				.as_deref()
				.is_some_and(|refusal| refusal.contains("press r"))
		);
		press(&mut app, KeyCode::Down);
		assert!(app.refusal.is_none());
	}

	#[test]
	fn a_refresh_asks_again_before_leaving_mods_out() {
		let mut app = App::default();
		app.excluded.insert("43".to_string());
		app.inspecting(true);
		app.inspected(input_with_a_broken_mod(), true, true);

		assert!(!app.take_analyze_after_inspection());
		assert!(app.confirming_exclusions);
	}
}
