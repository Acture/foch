use std::cell::Cell;
use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Instant;

use crate::cli::arg::{DataBuildArgs, MergeArgs};
use crate::cli::handler::input::{RepairTarget, repair_targets};

use crossterm::event::{
	KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use foch::input::{CurrentEu4Input, DetectedPlaysetMod};
use foch::merge::{
	AnalyzedMerge, MergeAnalysisStatus, MergeDisposition, MergeProgress, MergeReviewSummary,
	MergeUnitOutcome, default_merge_workers,
};
use ratatui::layout::{Position, Rect};

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

/// The panels of the playset screen, in `Tab` order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InputPane {
	#[default]
	Mods,
	Mod,
	Issues,
}

impl InputPane {
	fn next(self) -> Self {
		match self {
			Self::Mods => Self::Mod,
			Self::Mod => Self::Issues,
			Self::Issues => Self::Mods,
		}
	}

	fn previous(self) -> Self {
		match self {
			Self::Mods => Self::Issues,
			Self::Mod => Self::Mods,
			Self::Issues => Self::Mod,
		}
	}
}

/// Where the last frame drew each panel, and the first row a scrolled list
/// showed, so a mouse position maps back to a panel and a row.
#[derive(Clone, Copy, Debug, Default)]
pub struct HitAreas {
	pub header: Rect,
	pub banner: Rect,
	pub mods: Rect,
	pub mods_offset: usize,
	pub selected_mod: Rect,
	pub issues: Rect,
	pub summary: Rect,
	pub units: Rect,
	pub units_offset: usize,
	pub detail: Rect,
}

impl HitAreas {
	/// The panels the current screen shows, for confining a text selection.
	fn panels(&self, reviewing: bool) -> Vec<Rect> {
		if reviewing {
			vec![self.header, self.summary, self.units, self.detail]
		} else {
			vec![
				self.header,
				self.banner,
				self.mods,
				self.selected_mod,
				self.issues,
			]
		}
	}
}

/// The inside of a bordered panel.
fn inner(area: Rect) -> Rect {
	Rect::new(
		area.x.saturating_add(1),
		area.y.saturating_add(1),
		area.width.saturating_sub(2),
		area.height.saturating_sub(2),
	)
}

/// `at` moved into `bounds`.
fn clamp(at: Position, bounds: Rect) -> Position {
	Position::new(
		at.x.clamp(bounds.x, bounds.right().saturating_sub(1).max(bounds.x)),
		at.y.clamp(bounds.y, bounds.bottom().saturating_sub(1).max(bounds.y)),
	)
}

const WHEEL_STEP: usize = 3;

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
	/// Building EU4 base data from the game installation.
	Building {
		started: Instant,
		/// The stage running now, by its name in [`BUILD_STAGES`].
		stage: Option<String>,
		/// How many stages have finished.
		finished: usize,
	},
}

/// What the session must do in response to a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppCommand {
	Quit,
	Analyze,
	Refresh,
	BuildBaseData,
}

/// Something outside the browser a key asked for, carried out by the run
/// loop: none of these writes a file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
	/// Put text on the system clipboard.
	Copy(String),
	/// Open these mods' Workshop pages in Steam, as
	/// `foch input repair --open` does.
	OpenWorkshop(Vec<RepairTarget>),
	/// Capture the mouse, or release it to the terminal for its own text
	/// selection.
	MouseCapture(bool),
}

/// The stages of a base data build, as `foch data build` names them, with
/// what each does.
pub const BUILD_STAGES: [(&str, &str); 8] = [
	("detect_version", "Detect the game version"),
	("collect_inventory", "Collect game files"),
	("discover_documents", "Find script documents"),
	("parse_documents", "Parse every document"),
	("build_semantic_index", "Index definitions and references"),
	("materialize_snapshot", "Assemble the snapshot"),
	("encode_snapshot", "Encode the snapshot"),
	("write_outputs", "Install into Foch's data directory"),
];

/// Every key, with the `foch` command that does the same outside the
/// browser, as the help panel lists them.
pub const KEY_HELP: &[(&str, &str, &str)] = &[
	(
		"a",
		"analyze the playset",
		"foch merge --out <OUT> --non-interactive",
	),
	(
		"x",
		"exclude or include the selected mod",
		"foch merge --exclude <MOD>",
	),
	(
		"X",
		"exclude every mod that cannot be analyzed",
		"foch merge --exclude <MOD>...",
	),
	(
		"R",
		"repair every broken mod in Steam",
		"foch input repair --open",
	),
	(
		"w",
		"repair the selected mod in Steam",
		"foch input repair --open --mod <MOD>",
	),
	("r", "inspect the input again", "foch input inspect"),
	(
		"B",
		"build EU4 base data from the game",
		"foch data build eu4 --from-game-path <GAME> --install",
	),
	(
		"o",
		"analysis options",
		"--no-game-base, --force, --jobs ...",
	),
	("0-6 /", "filter and search the review", ""),
	("Tab", "next panel", ""),
	("c", "copy the focused panel", ""),
	("C", "copy the analysis command", ""),
	("m", "release the mouse for terminal text selection", ""),
	("i", "switch between playset and review", ""),
	("?", "this help", ""),
	("q", "quit", ""),
];

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
	/// The focused panel of the playset screen.
	pub input_pane: InputPane,
	pub mod_detail_scroll: u16,
	pub issues_scroll: u16,
	/// Filled in by each frame; read by mouse handling.
	pub hit: Cell<HitAreas>,
	pub settings: AnalysisSettings,
	/// The settings and exclusions of the analysis being run or browsed.
	pub analyzed_with: Option<(AnalysisSettings, BTreeSet<String>)>,
	/// Whether the input source can supply the EU4 base as an ancestor.
	pub game_base_available: bool,
	/// Cursor row while the options panel is open.
	pub options: Option<usize>,
	/// Asking whether to analyze without the excluded mods.
	pub confirming_exclusions: bool,
	/// Asking whether to build base data, which writes Foch's data directory.
	pub confirming_build: bool,
	/// Why the last key could not do what it asks, until the next key.
	pub refusal: Option<String>,
	/// What the last key did, until the next key.
	pub notice: Option<String>,
	/// Waiting for the run loop to carry it out.
	pub effect: Option<Effect>,
	pub mouse_capture: bool,
	pub help: bool,
	/// Where the left button went down, while it is held.
	pub drag_anchor: Option<Position>,
	/// The screen cells a drag covers, from where it started to where it
	/// is; highlighted, and copied when the button is released.
	pub selection: Option<(Position, Position)>,
	/// The inside of the panel the drag started in: a selection never
	/// leaves it, so it copies only that panel's text and no borders.
	pub selection_bounds: Rect,
	/// The finished selection is waiting to be copied from the next frame.
	pub copy_selection: bool,
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
			input_pane: InputPane::Mods,
			mod_detail_scroll: 0,
			issues_scroll: 0,
			hit: Cell::new(HitAreas::default()),
			settings: AnalysisSettings::default(),
			analyzed_with: None,
			game_base_available: true,
			options: None,
			confirming_exclusions: false,
			confirming_build: false,
			refusal: None,
			notice: None,
			effect: None,
			mouse_capture: true,
			help: false,
			drag_anchor: None,
			selection: None,
			selection_bounds: Rect::default(),
			copy_selection: false,
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
		self.settings
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

	/// Whether the analysis would use the game base while its base data is
	/// not ready.
	pub fn needs_base_data(&self) -> bool {
		self.settings.game_base && !self.game_base_available
	}

	pub fn can_analyze(&self) -> bool {
		self.selectable
			&& !self.needs_base_data()
			&& !self.is_working()
			&& self.problem_mods().is_empty()
			&& self.included_count() > 0
	}

	/// The `foch data build` arguments `B` runs: the inspected EU4
	/// installation, installed into Foch's data directory.
	pub fn data_build_args(&self) -> Option<DataBuildArgs> {
		let game_root = self.input.as_ref()?.game.install_path.clone()?;
		Some(DataBuildArgs {
			game_name: "eu4".to_string(),
			from_game_path: game_root,
			game_version: "auto".to_string(),
			install: true,
			output_dir: None,
			release_asset: false,
			profile_out: None,
		})
	}

	pub(super) fn building(&mut self) {
		self.phase = Phase::Building {
			started: Instant::now(),
			stage: None,
			finished: 0,
		};
	}

	pub(super) fn build_stage(&mut self, name: String, done: bool) {
		if let Phase::Building {
			stage, finished, ..
		} = &mut self.phase
		{
			if done {
				*finished += 1;
			}
			*stage = Some(name);
		}
	}

	pub(super) fn built(&mut self, result: Result<Vec<String>, String>) {
		match result {
			Ok(report) => {
				self.notice = Some(format!("Base data built. {}", report.join(" ")));
			}
			Err(error) => {
				self.refusal = Some(format!("Building base data failed: {error}"));
			}
		}
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
			review_json: None,
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
		matches!(
			self.phase,
			Phase::Inspecting | Phase::Analyzing { .. } | Phase::Building { .. }
		)
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
		self.mod_detail_scroll = 0;
		self.issues_scroll = 0;
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
		self.notice = None;
		self.selection = None;
		if self.help {
			self.help = false;
			return (key.code == KeyCode::Char('q')).then_some(AppCommand::Quit);
		}
		if self.focus == Focus::Search {
			self.handle_search_key(key);
			return None;
		}
		if let Some(cursor) = self.options {
			return self.handle_options_key(key, cursor);
		}
		if self.confirming_build {
			self.confirming_build = false;
			return match key.code {
				KeyCode::Char('q') => Some(AppCommand::Quit),
				KeyCode::Char('y') => Some(AppCommand::BuildBaseData),
				_ => None,
			};
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
			KeyCode::Char('?') => {
				self.help = true;
				return None;
			}
			KeyCode::Char('c') => {
				let text = self.focused_text();
				self.notice = Some(format!("Copied {} characters.", text.chars().count()));
				self.effect = Some(Effect::Copy(text));
				return None;
			}
			KeyCode::Char('C') => {
				self.notice = Some("Copied the analysis command.".to_string());
				self.effect = Some(Effect::Copy(self.cli_command()));
				return None;
			}
			KeyCode::Char('m') => {
				self.mouse_capture = !self.mouse_capture;
				self.notice = Some(
					if self.mouse_capture {
						"Mouse captured: click panels and rows, scroll with the wheel."
					} else {
						"Mouse released: select text with the terminal; press m to capture it again."
					}
					.to_string(),
				);
				self.effect = Some(Effect::MouseCapture(self.mouse_capture));
				return None;
			}
			KeyCode::Char('R') => {
				self.repair(Vec::new());
				return None;
			}
			KeyCode::Char('B') => {
				match (self.is_working(), self.data_build_args()) {
					(true, _) => {
						self.refusal = Some("Wait for the current work to finish.".to_string())
					}
					(false, None) => {
						self.refusal = Some(
							"The EU4 installation was not found, so there is nothing to build base data from."
								.to_string(),
						);
					}
					(false, Some(_)) => self.confirming_build = true,
				}
				return None;
			}
			KeyCode::Char('w') => {
				if let Some(id) = self.mods().get(self.mod_scroll).map(|m| m.id.clone()) {
					self.repair(vec![id]);
				}
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

	/// Ask Steam to show the named mods, or every mod that cannot be
	/// analyzed, through the same targets `foch input repair` lists.
	fn repair(&mut self, named: Vec<String>) {
		let Some(input) = &self.input else {
			return;
		};
		let command = if named.is_empty() {
			"foch input repair --open".to_string()
		} else {
			format!("foch input repair --open --mod {}", named.join(" --mod "))
		};
		match repair_targets(input, &named) {
			Ok(targets) if targets.iter().all(|target| target.workshop_id.is_none()) => {
				self.refusal = Some(if targets.is_empty() {
					"Every mod can be analyzed; nothing to repair.".to_string()
				} else {
					"These mods are not Workshop items; repair them outside Steam.".to_string()
				});
			}
			Ok(targets) => {
				self.notice = Some(format!(
					"Opening {} Workshop page(s) in Steam: unsubscribe and subscribe again, wait for the download, then press r.   CLI: {command}",
					targets
						.iter()
						.filter(|target| target.workshop_id.is_some())
						.count()
				));
				self.effect = Some(Effect::OpenWorkshop(targets));
			}
			Err(error) => self.refusal = Some(error),
		}
	}

	/// The text of the focused panel, for `c`.
	pub fn focused_text(&self) -> String {
		if self.analysis().is_some() && self.screen == Screen::Review {
			let Some(unit) = self.selected_unit() else {
				return String::new();
			};
			return match self.focus {
				Focus::Detail => super::render::unit_detail(unit)
					.lines
					.iter()
					.map(|line| {
						line.spans
							.iter()
							.map(|span| span.content.as_ref())
							.collect::<String>()
					})
					.collect::<Vec<_>>()
					.join("\n"),
				_ => unit.path.as_str().to_owned(),
			};
		}
		let selected = self.mods().get(self.mod_scroll);
		match self.input_pane {
			InputPane::Mods => selected
				.map(|m| format!("#{} {} {}", m.position, m.id, m.name))
				.unwrap_or_default(),
			InputPane::Mod => selected
				.map(|m| {
					let mut lines = vec![m.name.clone(), format!("#{} {}", m.position, m.id)];
					if let Some(version) = &m.version {
						lines.push(format!("version {version}"));
					}
					if !m.declared_dependencies.is_empty() {
						lines.push(format!(
							"depends on: {}",
							m.declared_dependencies.join(", ")
						));
					}
					if let Some(error) = &m.source_error {
						lines.push(error.clone());
					}
					lines.join("\n")
				})
				.unwrap_or_default(),
			InputPane::Issues => self
				.input
				.iter()
				.flat_map(|input| &input.issues)
				.map(|issue| {
					let mut text = format!("{}: {}", issue.title, issue.detail);
					if let Some(action) = &issue.action {
						text.push_str(&format!("\n  -> {action}"));
					}
					text
				})
				.collect::<Vec<_>>()
				.join("\n"),
		}
	}

	/// Why `a` cannot start an analysis right now.
	fn analyze_refusal(&self) -> String {
		let problems = self.problem_mods().len();
		match self.phase {
			Phase::Inspecting => {
				"Still inspecting the input; analysis can start when it finishes.".to_string()
			}
			Phase::Building { .. } => {
				"Base data is being built; analysis can start when it finishes.".to_string()
			}
			Phase::Analyzing { .. } => "An analysis is already running.".to_string(),
			Phase::Reviewed(_) | Phase::Failed(_) => {
				"This input snapshot was already analyzed. Press r to re-inspect and analyze again."
					.to_string()
			}
			Phase::Inspected if self.selectable && self.needs_base_data() => {
				"The EU4 base data is not ready. Press B to build it here, or press o and turn off the EU4 base as ancestor (--no-game-base)."
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
			KeyCode::Tab => {
				self.input_pane = self.input_pane.next();
				return;
			}
			KeyCode::BackTab => {
				self.input_pane = self.input_pane.previous();
				return;
			}
			_ => {}
		}
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
		match self.input_pane {
			InputPane::Mods => {
				let last = self.mods().len().saturating_sub(1);
				self.select_mod(step(self.mod_scroll, key.code, last));
			}
			InputPane::Mod => {
				self.mod_detail_scroll =
					step(self.mod_detail_scroll as usize, key.code, u16::MAX as usize) as u16;
			}
			InputPane::Issues => {
				self.issues_scroll =
					step(self.issues_scroll as usize, key.code, u16::MAX as usize) as u16;
			}
		}
		if key.code == KeyCode::Esc && self.analysis().is_some() {
			self.screen = Screen::Review;
		}
	}

	fn select_mod(&mut self, index: usize) {
		if index != self.mod_scroll {
			self.mod_scroll = index;
			self.mod_detail_scroll = 0;
		}
	}

	fn select_unit(&mut self, index: usize) {
		if index != self.selected {
			self.selected = index;
			self.detail_scroll = 0;
		}
	}

	/// A click focuses the panel under the pointer and, in a list, selects
	/// the row; the wheel scrolls the panel under the pointer. Mouse input
	/// is ignored while a dialog is open.
	pub fn handle_mouse(&mut self, mouse: MouseEvent) {
		if self.options.is_some()
			|| self.confirming_exclusions
			|| self.help
			|| self.focus == Focus::Search
		{
			return;
		}
		let at = Position::new(mouse.column, mouse.row);
		// Dragging selects screen text to copy, as in a plain terminal; a
		// click without a drag focuses and selects.
		match mouse.kind {
			MouseEventKind::Down(MouseButton::Left) => {
				let reviewing = self.analysis().is_some() && self.screen == Screen::Review;
				self.selection_bounds = self
					.hit
					.get()
					.panels(reviewing)
					.into_iter()
					.find(|panel| panel.contains(at))
					.map(inner)
					.unwrap_or(Rect::new(0, at.y, u16::MAX, 1));
				self.drag_anchor = Some(clamp(at, self.selection_bounds));
				self.selection = None;
			}
			MouseEventKind::Drag(MouseButton::Left) => {
				if let Some(anchor) = self.drag_anchor {
					self.selection = Some((anchor, clamp(at, self.selection_bounds)));
				}
				return;
			}
			MouseEventKind::Up(MouseButton::Left) => {
				self.drag_anchor = None;
				if self.selection.is_some_and(|(start, end)| start != end) {
					self.copy_selection = true;
				} else {
					self.selection = None;
				}
				return;
			}
			_ => {}
		}
		let hit = self.hit.get();
		let down = match mouse.kind {
			MouseEventKind::ScrollDown => Some(true),
			MouseEventKind::ScrollUp => Some(false),
			MouseEventKind::Down(_) => None,
			_ => return,
		};
		let scroll = |value: usize, last: usize| match down {
			Some(true) => value.saturating_add(WHEEL_STEP).min(last),
			Some(false) => value.saturating_sub(WHEEL_STEP),
			None => value,
		};
		// The row a click landed on, below a panel's border and any header.
		let row = |area: Rect, offset: usize, header: u16| {
			let first = area.y + 1 + header;
			(at.y >= first && at.y < area.bottom().saturating_sub(1))
				.then(|| offset + usize::from(at.y - first))
		};
		let reviewing = self.analysis().is_some() && self.screen == Screen::Review;
		if reviewing {
			if hit.units.contains(at) {
				self.focus = Focus::Units;
				let last = self.visible_units().len().saturating_sub(1);
				match (down, row(hit.units, hit.units_offset, 0)) {
					(Some(_), _) => self.select_unit(scroll(self.selected, last)),
					(None, Some(index)) if index <= last => self.select_unit(index),
					_ => {}
				}
			} else if hit.detail.contains(at) && self.selected_unit().is_some() {
				self.focus = Focus::Detail;
				self.detail_scroll = scroll(self.detail_scroll as usize, u16::MAX as usize) as u16;
			}
			return;
		}
		if hit.mods.contains(at) {
			self.input_pane = InputPane::Mods;
			let last = self.mods().len().saturating_sub(1);
			match (down, row(hit.mods, hit.mods_offset, 1)) {
				(Some(_), _) => self.select_mod(scroll(self.mod_scroll, last)),
				(None, Some(index)) if index <= last && !self.mods().is_empty() => {
					self.select_mod(index);
				}
				_ => {}
			}
		} else if hit.selected_mod.contains(at) {
			self.input_pane = InputPane::Mod;
			self.mod_detail_scroll =
				scroll(self.mod_detail_scroll as usize, u16::MAX as usize) as u16;
		} else if hit.issues.contains(at) {
			self.input_pane = InputPane::Issues;
			self.issues_scroll = scroll(self.issues_scroll as usize, u16::MAX as usize) as u16;
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
			(Focus::Units, KeyCode::Enter | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab) => {
				if self.selected_unit().is_some() {
					self.focus = Focus::Detail;
				}
			}
			(Focus::Detail, KeyCode::Esc | KeyCode::Left | KeyCode::Tab | KeyCode::BackTab) => {
				self.focus = Focus::Units;
			}
			(Focus::Detail, code) => {
				self.detail_scroll =
					step(self.detail_scroll as usize, code, u16::MAX as usize) as u16;
			}
			(Focus::Units, KeyCode::Esc) if !self.query.is_empty() => self.set_query(String::new()),
			(Focus::Units, code) => {
				let last = self.visible_units().len().saturating_sub(1);
				self.select_unit(step(self.selected, code, last));
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

	fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
		MouseEvent {
			kind,
			column,
			row,
			modifiers: KeyModifiers::NONE,
		}
	}

	#[test]
	fn tab_moves_the_keyboard_between_playset_panels() {
		let mut app = App::default();
		app.inspected(input_with_a_broken_mod(), true, true);

		assert_eq!(app.input_pane, InputPane::Mods);
		press(&mut app, KeyCode::Down);
		assert_eq!(app.mod_scroll, 1);
		press(&mut app, KeyCode::Tab);
		press(&mut app, KeyCode::Tab);
		assert_eq!(app.input_pane, InputPane::Issues);
		press(&mut app, KeyCode::Down);
		assert_eq!(app.issues_scroll, 1);
		assert_eq!(app.mod_scroll, 1, "only the focused panel moves");
		press(&mut app, KeyCode::BackTab);
		assert_eq!(app.input_pane, InputPane::Mod);
	}

	#[test]
	fn a_click_focuses_a_panel_and_selects_its_row() {
		use crossterm::event::MouseButton;

		let mut app = App::default();
		app.inspected(input_with_a_broken_mod(), true, true);
		app.hit.set(HitAreas {
			mods: Rect::new(0, 10, 40, 10),
			selected_mod: Rect::new(40, 10, 40, 5),
			issues: Rect::new(40, 15, 40, 5),
			..HitAreas::default()
		});

		// Border at y=10, header at y=11, first mod at y=12.
		app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 13));
		assert_eq!(app.mod_scroll, 1);
		app.handle_mouse(mouse(MouseEventKind::ScrollDown, 50, 17));
		assert_eq!(app.input_pane, InputPane::Issues);
		assert_eq!(app.issues_scroll, 3);
		app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 12));
		assert_eq!(app.input_pane, InputPane::Mod);
	}

	#[test]
	fn b_builds_base_data_with_the_command_it_shows_after_consent() {
		use crate::cli::arg::{FochCli, FochCliCommands, FochCliDataCommands};
		use clap::Parser;

		let mut app = App::default();
		app.inspected(input_with_a_broken_mod(), true, false);
		assert_eq!(press(&mut app, KeyCode::Char('B')), None);
		assert!(
			app.refusal
				.as_deref()
				.is_some_and(|refusal| refusal.contains("not found"))
		);

		let mut input = input_with_a_broken_mod();
		input.game.install_path = Some(PathBuf::from("G:/Steam Library/EU4"));
		app.inspected(input, true, false);
		assert_eq!(press(&mut app, KeyCode::Char('B')), None);
		assert!(app.confirming_build);
		assert_eq!(
			press(&mut app, KeyCode::Char('y')),
			Some(AppCommand::BuildBaseData)
		);

		let expected = app.data_build_args().expect("build arguments");
		let argv = std::iter::once("foch".to_string()).chain(expected.command_line());
		let parsed = FochCli::try_parse_from(argv).expect("shown command parses");
		let Some(FochCliCommands::Data(data)) = parsed.command else {
			panic!("shown command is not `foch data`");
		};
		let FochCliDataCommands::Build(parsed) = data.command else {
			panic!("shown command is not `foch data build`");
		};
		assert_eq!(parsed, expected);
	}

	#[test]
	fn missing_base_data_offers_b_in_the_footer_and_the_build_shows_its_stages() {
		use ratatui::Terminal;
		use ratatui::backend::TestBackend;

		let mut input = input_with_a_broken_mod();
		input.game.install_path = Some(PathBuf::from("G:/EU4"));
		let mut app = App::default();
		app.inspected(input, true, false);
		let screen = |app: &App| {
			let mut terminal = Terminal::new(TestBackend::new(160, 40)).expect("terminal");
			terminal
				.draw(|frame| super::super::render::draw(frame, app))
				.expect("draw");
			let buffer = terminal.backend().buffer().clone();
			(0..buffer.area.height)
				.map(|y| {
					(0..buffer.area.width)
						.map(|x| buffer[(x, y)].symbol())
						.collect::<String>()
				})
				.collect::<Vec<_>>()
				.join(
					"
",
				)
		};
		assert!(screen(&app).contains(" B  build base data"));

		app.building();
		app.build_stage("detect_version".to_string(), false);
		app.build_stage("detect_version".to_string(), true);
		app.build_stage("collect_inventory".to_string(), false);
		let shown = screen(&app);
		assert!(shown.contains("stage 2/8"), "{shown}");
		assert!(shown.contains("✔ Detect the game version"), "{shown}");
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
