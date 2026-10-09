use std::num::NonZeroUsize;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use foch::input::CurrentEu4Input;
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
	pub can_analyze: bool,
	pub mod_scroll: usize,
	pub filter: Option<MergeDisposition>,
	pub query: String,
	/// Index into [`App::visible_units`].
	pub selected: usize,
	pub detail_scroll: u16,
	pub settings: AnalysisSettings,
	/// The settings of the analysis being run or browsed.
	pub analyzed_with: Option<AnalysisSettings>,
	/// Whether the input source can supply the EU4 base as an ancestor.
	pub game_base_available: bool,
	/// Cursor row while the options panel is open.
	pub options: Option<usize>,
	analyze_after_inspection: bool,
}

impl Default for App {
	fn default() -> Self {
		Self {
			phase: Phase::Inspecting,
			screen: Screen::Input,
			focus: Focus::Units,
			input: None,
			can_analyze: false,
			mod_scroll: 0,
			filter: None,
			query: String::new(),
			selected: 0,
			detail_scroll: 0,
			settings: AnalysisSettings::default(),
			analyzed_with: None,
			game_base_available: true,
			options: None,
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

	/// Whether the options changed since the browsed analysis ran.
	pub fn settings_changed(&self) -> bool {
		self.analyzed_with
			.is_some_and(|analyzed| analyzed != self.effective_settings())
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
		self.can_analyze = false;
		self.analyze_after_inspection = analyze_after;
		self.focus = Focus::Units;
	}

	pub(super) fn inspected(
		&mut self,
		input: CurrentEu4Input,
		can_analyze: bool,
		game_base_available: bool,
	) {
		self.input = Some(input);
		self.game_base_available = game_base_available;
		self.can_analyze = can_analyze;
		self.phase = Phase::Inspected;
		self.screen = Screen::Input;
		self.mod_scroll = 0;
	}

	pub(super) fn take_analyze_after_inspection(&mut self) -> bool {
		std::mem::take(&mut self.analyze_after_inspection) && self.can_analyze
	}

	pub(super) fn analyzing(&mut self) {
		self.can_analyze = false;
		self.analyzed_with = Some(self.effective_settings());
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
		if self.focus == Focus::Search {
			self.handle_search_key(key);
			return None;
		}
		if let Some(cursor) = self.options {
			return self.handle_options_key(key, cursor);
		}
		match key.code {
			KeyCode::Char('q') => return Some(AppCommand::Quit),
			KeyCode::Char('o') => {
				self.options = Some(0);
				return None;
			}
			KeyCode::Char('r') if !self.is_working() => return Some(AppCommand::Refresh),
			KeyCode::Char('a') if self.can_analyze && !self.is_working() => {
				return Some(AppCommand::Analyze);
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
		let last = self
			.input
			.as_ref()
			.and_then(|input| input.playset.as_ref())
			.map_or(0, |playset| playset.mods.len().saturating_sub(1));
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
