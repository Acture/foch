use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
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
	AnalyzedMerge, DecisionAction, DecisionOption, DecisionPoint, DecisionScope,
	MergeAnalysisStatus, MergeDisposition, MergeProgress, MergeReviewSummary, MergeUnitOutcome,
	UnitConflicts, default_merge_workers,
};
use foch::plugin::{
	self, GameIdentity, LanguagePreference, MirrorPreference, PluginOverview, TransportPreference,
	UiLanguage, UserPreferences,
};
use foch::project::ResolutionEntry;
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
	/// Leaf conflicts for units that have them, keyed by `unit_id`.
	pub conflicts: BTreeMap<String, UnitConflicts>,
	/// Every decision point, in review order.
	pub decisions: Vec<DecisionPoint>,
}

impl AnalysisView {
	pub fn from_analyzed(analyzed: &AnalyzedMerge) -> Self {
		let review = analyzed.review();
		Self {
			status: analyzed.analysis().status(),
			summary: *analyzed.review_summary(),
			units: analyzed.list_units().to_vec(),
			conflicts: review
				.conflicts()
				.iter()
				.map(|conflicts| (conflicts.unit_id.clone(), conflicts.clone()))
				.collect(),
			decisions: review.decisions().to_vec(),
		}
	}

	pub fn conflicts_for(&self, unit_id: &str) -> Option<&UnitConflicts> {
		self.conflicts.get(unit_id)
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
	pub tab_launch: Rect,
	pub tab_merge: Rect,
	pub tab_settings: Rect,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppCommand {
	Quit,
	Analyze,
	Refresh,
	/// Re-inspect the input and the plugin list without starting an analysis.
	RefreshPlugins,
	BuildBaseData,
	PrepareOnly,
	LaunchGame,
	InstallPlugin,
	InstallSpec(String),
}

/// The three pages of bare `foch`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TopTab {
	LaunchPlugins,
	#[default]
	MergeReview,
	NetworkSettings,
}

/// Inline editor for one plugin's `[config.*]` fields.
#[derive(Clone, Debug)]
pub(super) struct ConfigEditor {
	pub plugin_id: String,
	pub cursor: usize,
	pub fields: Vec<foch::plugin::ConfigFieldSummary>,
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
	(
		"t",
		"copy a foch.toml resolution template",
		"paste [[resolutions]] into foch.toml",
	),
	("C", "copy the analysis command", ""),
	("m", "release the mouse for terminal text selection", ""),
	("i", "switch between playset and review", ""),
	("?", "this help", ""),
	("t", "test download connectivity", ""),
	("q", "quit", ""),
];

/// The browser's whole visible state. It only reads: no key leads to a
/// decision, an artifact, or a write.
#[derive(Debug)]
pub struct App {
	pub phase: Phase,
	pub top_tab: TopTab,
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
	pub(super) ui_language: UiLanguage,
	pub(super) preferences: UserPreferences,
	pub(super) preferences_path: Option<std::path::PathBuf>,
	pub(super) playset_name: String,
	pub(super) store_root: Option<std::path::PathBuf>,
	pub(super) selections_path: Option<std::path::PathBuf>,
	pub(super) plugin_overview: Option<PluginOverview>,
	pub(super) plugin_cursor: usize,
	pub(super) plugin_warnings: Vec<String>,
	pub(super) config_editor: Option<ConfigEditor>,
	pub(super) settings_cursor: usize,
	pub(super) confirming_launch: bool,
	pub(super) install_input: Option<String>,
	pub(super) banner: Option<String>,
	pub(super) last_run_dir: Option<std::path::PathBuf>,
	pub(super) runtime_states: std::collections::BTreeMap<String, String>,
	pub(super) host_label: String,
	/// Last Tab 3 connectivity probe. Kept apart from `notice`, which the next
	/// key clears.
	pub(super) connectivity: Option<foch::plugin::ConnectivityReport>,
	pub(super) connectivity_probe: fn(
		&foch::plugin::TransportConfig,
		&foch::plugin::ConnectivityProbeOptions,
	) -> foch::plugin::ConnectivityReport,
}

fn live_connectivity_probe(
	config: &foch::plugin::TransportConfig,
	options: &foch::plugin::ConnectivityProbeOptions,
) -> foch::plugin::ConnectivityReport {
	foch::plugin::probe_connectivity(config, options)
}

impl Default for App {
	fn default() -> Self {
		Self {
			phase: Phase::Inspecting,
			top_tab: TopTab::MergeReview,
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
			ui_language: UiLanguage::En,
			preferences: UserPreferences::default(),
			preferences_path: None,
			playset_name: "default".to_string(),
			store_root: None,
			selections_path: None,
			plugin_overview: None,
			plugin_cursor: 0,
			plugin_warnings: Vec::new(),
			config_editor: None,
			settings_cursor: 0,
			confirming_launch: false,
			install_input: None,
			banner: None,
			last_run_dir: None,
			runtime_states: std::collections::BTreeMap::new(),
			host_label: String::new(),
			connectivity: None,
			connectivity_probe: live_connectivity_probe,
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
			strict: false,
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

	/// Copy a commented `[[resolutions]]` template for the selected unit, or
	/// say why there is nothing to copy. Never writes a file.
	fn copy_resolution_template(&mut self) {
		let language = self.ui_language;
		let unit_id = self.selected_unit().map(|unit| unit.id.clone());
		let template = unit_id.as_deref().and_then(|unit_id| {
			self.analysis()
				.and_then(|view| resolution_template(view, unit_id, language))
		});
		match template {
			Some(template) => {
				self.notice = Some(super::text::copied_resolution_notice(language));
				self.effect = Some(Effect::Copy(template));
			}
			None => {
				self.effect = None;
				self.notice = Some(super::text::no_resolution_notice(language));
			}
		}
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
		if let Some(name) = self
			.input
			.as_ref()
			.and_then(|input| input.playset.as_ref())
			.map(|playset| playset.name.clone())
		{
			self.playset_name = name;
		}
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
		if let Some(tab) = function_tab(key.code) {
			self.switch_tab(tab);
			return None;
		}
		if self.help {
			self.help = false;
			return (key.code == KeyCode::Char('q')).then_some(AppCommand::Quit);
		}
		if self.focus == Focus::Search {
			self.handle_search_key(key);
			return None;
		}
		if self.config_editor.is_some() {
			self.handle_config_key(key);
			return None;
		}
		if self.install_input.is_some() {
			return self.handle_install_input(key);
		}
		if self.confirming_launch {
			self.confirming_launch = false;
			return match key.code {
				KeyCode::Char('q') => Some(AppCommand::Quit),
				KeyCode::Char('y') => Some(AppCommand::LaunchGame),
				_ => None,
			};
		}
		if let Some(tab) = digit_tab(key.code)
			&& !self.review_digits_filter()
		{
			self.switch_tab(tab);
			return None;
		}
		if self.top_tab == TopTab::LaunchPlugins {
			return self.handle_launch_key(key);
		}
		if self.top_tab == TopTab::NetworkSettings {
			return self.handle_settings_key(key);
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
				Focus::Detail => super::render::detail_for(self, unit)
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

	/// Bare `foch` opens on Launch & Plugins and loads saved UI settings.
	pub(super) fn for_bare_foch() -> Self {
		let preferences_path = plugin::preferences_path().ok();
		let preferences = preferences_path
			.as_deref()
			.map(UserPreferences::load_or_default)
			.unwrap_or_default();
		let ui_language = preferences.resolved_language();
		Self {
			top_tab: TopTab::LaunchPlugins,
			ui_language,
			preferences,
			preferences_path,
			store_root: plugin::paths::checked_store_root(
				&foch::game::eu4::base::snapshot::data_root(),
			)
			.ok(),
			selections_path: plugin::paths::selections_path().ok(),
			host_label: host_dll_label(),
			..Self::default()
		}
	}

	/// Launch-tab shell with explicit settings, for tests that must not read
	/// the player's config directory.
	#[cfg(test)]
	pub(super) fn open_launch(
		preferences: UserPreferences,
		preferences_path: Option<PathBuf>,
	) -> Self {
		let ui_language = match preferences.language {
			LanguagePreference::ZhCn => UiLanguage::ZhCn,
			LanguagePreference::En => UiLanguage::En,
			LanguagePreference::Auto => preferences.resolved_language(),
		};
		Self {
			top_tab: TopTab::LaunchPlugins,
			ui_language,
			preferences,
			preferences_path,
			host_label: host_dll_label(),
			..Self::default()
		}
	}

	pub(super) fn review_attention_count(&self) -> usize {
		if let Some(view) = self.analysis() {
			view.summary.needs_user_choice
				+ view.summary.unsupported_input
				+ view.summary.engine_failure
		} else {
			self.problem_mods().len()
		}
	}

	fn review_digits_filter(&self) -> bool {
		self.top_tab == TopTab::MergeReview
			&& self.screen == Screen::Review
			&& self.analysis().is_some()
			&& self.focus != Focus::Search
	}

	fn switch_tab(&mut self, tab: TopTab) {
		self.top_tab = tab;
		self.options = None;
		self.confirming_exclusions = false;
		self.confirming_build = false;
		self.confirming_launch = false;
		self.install_input = None;
		self.config_editor = None;
		self.help = false;
		if self.focus == Focus::Search {
			self.focus = Focus::Units;
		}
		if tab == TopTab::LaunchPlugins {
			self.reload_plugins();
		}
	}

	pub(super) fn reload_plugins(&mut self) {
		let (Some(store), Some(selections)) =
			(self.store_root.clone(), self.selections_path.clone())
		else {
			return;
		};
		let roots = self.declared_roots();
		let (declared, warnings) = plugin::discover_declared_plugins(&roots);
		self.plugin_warnings = warnings;
		let game = self.game_identity();
		match plugin::build_overview(
			&self.playset_name,
			&store,
			&selections,
			&declared,
			game.as_ref(),
		) {
			Ok(overview) => {
				let last = overview.plugins.len().saturating_sub(1);
				if self.plugin_cursor > last {
					self.plugin_cursor = last;
				}
				self.plugin_overview = Some(overview);
			}
			Err(error) => self.refusal = Some(error.to_string()),
		}
	}

	fn declared_roots(&self) -> Vec<(String, PathBuf)> {
		self.input
			.as_ref()
			.and_then(|input| input.playset.as_ref())
			.map(|playset| {
				playset
					.mods
					.iter()
					.filter_map(|playset_mod| {
						let root = playset_mod
							.descriptor_path
							.as_ref()?
							.parent()?
							.to_path_buf();
						Some((playset_mod.name.clone(), root))
					})
					.collect()
			})
			.unwrap_or_default()
	}

	fn game_identity(&self) -> Option<GameIdentity> {
		let version = self.input.as_ref()?.game.version.as_deref()?;
		Some(GameIdentity {
			game: "eu4".into(),
			version: plugin::parse_game_version(version)?,
			platform: plugin::WINDOWS_X64.into(),
		})
	}

	fn selected_plugin(&self) -> Option<&foch::plugin::PluginEntrySummary> {
		self.plugin_overview
			.as_ref()?
			.plugins
			.get(self.plugin_cursor)
	}

	fn handle_launch_key(&mut self, key: KeyEvent) -> Option<AppCommand> {
		match key.code {
			KeyCode::Char('q') => return Some(AppCommand::Quit),
			KeyCode::Char('?') => self.help = true,
			KeyCode::Char('m') => {
				self.mouse_capture = !self.mouse_capture;
				self.effect = Some(Effect::MouseCapture(self.mouse_capture));
			}
			KeyCode::Char('r') if !self.is_working() => return Some(AppCommand::RefreshPlugins),
			KeyCode::Up | KeyCode::Char('k') => {
				self.plugin_cursor = self.plugin_cursor.saturating_sub(1);
			}
			KeyCode::Down | KeyCode::Char('j') => {
				let last = self
					.plugin_overview
					.as_ref()
					.map(|overview| overview.plugins.len().saturating_sub(1))
					.unwrap_or(0);
				self.plugin_cursor = self.plugin_cursor.saturating_add(1).min(last);
			}
			KeyCode::Char(' ') => self.toggle_selected_plugin(),
			KeyCode::Char('c') => self.open_config_editor(),
			KeyCode::Char('i') => return Some(AppCommand::InstallPlugin),
			KeyCode::Char('I') => self.install_input = Some(String::new()),
			KeyCode::Char('p') if !self.is_working() => return Some(AppCommand::PrepareOnly),
			KeyCode::Char('L') => self.confirming_launch = true,
			KeyCode::Char('s') => self.refresh_runtime_states(),
			_ => {}
		}
		None
	}

	fn handle_settings_key(&mut self, key: KeyEvent) -> Option<AppCommand> {
		let last = 2usize;
		match key.code {
			KeyCode::Char('q') => return Some(AppCommand::Quit),
			KeyCode::Up | KeyCode::Char('k') => {
				self.settings_cursor = self.settings_cursor.saturating_sub(1);
			}
			KeyCode::Down | KeyCode::Char('j') => {
				self.settings_cursor = self.settings_cursor.saturating_add(1).min(last);
			}
			KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Right | KeyCode::Char('+') => {
				self.cycle_setting(true);
			}
			KeyCode::Left | KeyCode::Char('-') => self.cycle_setting(false),
			KeyCode::Char('t') => {
				let config = self.preferences.to_transport_config();
				let options = foch::plugin::ConnectivityProbeOptions::default();
				self.connectivity = Some((self.connectivity_probe)(&config, &options));
			}
			KeyCode::Char('?') => self.help = true,
			_ => {}
		}
		None
	}

	fn cycle_setting(&mut self, forward: bool) {
		match self.settings_cursor {
			0 => {
				self.preferences.transport = cycle_transport(self.preferences.transport, forward);
			}
			1 => {
				self.preferences.mirror = cycle_mirror(self.preferences.mirror, forward);
				if self.preferences.mirror == MirrorPreference::Custom
					&& self.preferences.custom_mirrors.is_empty()
				{
					self.preferences.mirror = if forward {
						MirrorPreference::Auto
					} else {
						MirrorPreference::Direct
					};
					self.refusal = Some(match self.ui_language {
						UiLanguage::ZhCn => "自定义镜像需要先写入 https 前缀".to_string(),
						UiLanguage::En => {
							"Custom mirrors need an https prefix in ui-settings.toml".to_string()
						}
					});
				}
			}
			_ => {
				self.preferences.language = cycle_language(self.preferences.language, forward);
				self.ui_language = match self.preferences.language {
					LanguagePreference::ZhCn => UiLanguage::ZhCn,
					LanguagePreference::En => UiLanguage::En,
					LanguagePreference::Auto => self.preferences.resolved_language(),
				};
			}
		}
		self.persist_preferences();
	}

	fn persist_preferences(&mut self) {
		let Some(path) = self.preferences_path.clone() else {
			return;
		};
		if let Err(error) = self.preferences.save(&path) {
			self.refusal = Some(error.to_string());
		}
	}

	fn toggle_selected_plugin(&mut self) {
		let Some(entry) = self.selected_plugin().cloned() else {
			return;
		};
		if entry.installed_versions.is_empty() {
			self.refusal = Some(super::text::not_installed(self.ui_language, &entry.id));
			return;
		}
		let (Some(store), Some(selections)) =
			(self.store_root.clone(), self.selections_path.clone())
		else {
			self.refusal = Some("plugin selections are not available".to_string());
			return;
		};
		let version = entry
			.selected_version
			.as_deref()
			.and_then(|version| semver::Version::parse(version).ok())
			.or_else(|| {
				entry
					.installed_versions
					.last()
					.and_then(|version| semver::Version::parse(version).ok())
			});
		if let Err(error) = plugin::set_plugin_enabled(
			&selections,
			&store,
			&self.playset_name,
			&entry.id,
			!entry.enabled,
			version,
		) {
			self.refusal = Some(error.to_string());
			return;
		}
		self.reload_plugins();
	}

	fn open_config_editor(&mut self) {
		let Some(entry) = self.selected_plugin() else {
			return;
		};
		if entry.config_fields.is_empty() {
			self.refusal = Some(match self.ui_language {
				UiLanguage::ZhCn => "这个插件没有可编辑的配置".to_string(),
				UiLanguage::En => "This plugin has no configuration fields".to_string(),
			});
			return;
		}
		self.config_editor = Some(ConfigEditor {
			plugin_id: entry.id.clone(),
			cursor: 0,
			fields: entry.config_fields.clone(),
		});
	}

	fn handle_config_key(&mut self, key: KeyEvent) {
		match key.code {
			KeyCode::Esc | KeyCode::Char('q') => {
				self.config_editor = None;
				return;
			}
			KeyCode::Up | KeyCode::Char('k') => {
				if let Some(editor) = self.config_editor.as_mut() {
					editor.cursor = editor.cursor.saturating_sub(1);
				}
				return;
			}
			KeyCode::Down | KeyCode::Char('j') => {
				if let Some(editor) = self.config_editor.as_mut() {
					let last = editor.fields.len().saturating_sub(1);
					editor.cursor = editor.cursor.saturating_add(1).min(last);
				}
				return;
			}
			_ => {}
		}
		let Some(editor) = self.config_editor.clone() else {
			return;
		};
		let Some(field) = editor.fields.get(editor.cursor) else {
			return;
		};
		let mut value = field.value.clone();
		match (field.kind.as_str(), key.code) {
			("bool", KeyCode::Char(' ') | KeyCode::Enter) => {
				value = toml::Value::Boolean(!value.as_bool().unwrap_or(false));
			}
			("int", KeyCode::Right | KeyCode::Char('+')) => {
				let next = value.as_integer().unwrap_or(0).saturating_add(1);
				if field.max.is_some_and(|max| next > max) {
					return;
				}
				value = toml::Value::Integer(next);
			}
			("int", KeyCode::Left | KeyCode::Char('-')) => {
				let next = value.as_integer().unwrap_or(0).saturating_sub(1);
				if field.min.is_some_and(|min| next < min) {
					return;
				}
				value = toml::Value::Integer(next);
			}
			("string", KeyCode::Char(ch)) => {
				let mut text = value.as_str().unwrap_or("").to_string();
				text.push(ch);
				value = toml::Value::String(text);
			}
			("string", KeyCode::Backspace) => {
				let mut text = value.as_str().unwrap_or("").to_string();
				text.pop();
				value = toml::Value::String(text);
			}
			_ => return,
		}
		let key_name = field.key.clone();
		let plugin_id = editor.plugin_id.clone();
		if let Err(error) = self.persist_config(&plugin_id, &key_name, value.clone()) {
			self.refusal = Some(error);
			return;
		}
		if let Some(editor) = self.config_editor.as_mut()
			&& let Some(field) = editor.fields.get_mut(editor.cursor)
		{
			field.value = value;
		}
	}

	fn persist_config(&self, id: &str, key: &str, value: toml::Value) -> Result<(), String> {
		let (Some(store), Some(selections)) =
			(self.store_root.as_ref(), self.selections_path.as_ref())
		else {
			return Err("plugin selections are not available".to_string());
		};
		plugin::set_plugin_config_value(selections, store, &self.playset_name, id, key, value)
			.map_err(|error| error.to_string())
	}

	fn handle_install_input(&mut self, key: KeyEvent) -> Option<AppCommand> {
		match key.code {
			KeyCode::Esc => self.install_input = None,
			KeyCode::Enter => {
				let spec = self.install_input.take().unwrap_or_default();
				if spec.trim().is_empty() {
					return None;
				}
				return Some(AppCommand::InstallSpec(spec));
			}
			KeyCode::Backspace => {
				if let Some(spec) = self.install_input.as_mut() {
					spec.pop();
				}
			}
			KeyCode::Char(ch) => {
				if let Some(spec) = self.install_input.as_mut() {
					spec.push(ch);
				}
			}
			_ => {}
		}
		None
	}

	fn refresh_runtime_states(&mut self) {
		let Some(run_dir) = self.last_run_dir.clone() else {
			self.refusal = Some(match self.ui_language {
				UiLanguage::ZhCn => "还没有启动记录，先按 L 启动或按 p 预检".to_string(),
				UiLanguage::En => {
					"No launch yet. Press L to start EU4 or p to prepare.".to_string()
				}
			});
			return;
		};
		match plugin::deployment::read_plan(&run_dir)
			.and_then(|plan| plugin::deployment::states(&plan, &run_dir))
		{
			Ok(states) => {
				self.runtime_states = states
					.into_iter()
					.map(|(id, value)| {
						let state = value
							.get("state")
							.and_then(|state| state.as_str())
							.unwrap_or("unknown")
							.to_string();
						(id, state)
					})
					.collect();
				self.banner = Some(match self.ui_language {
					UiLanguage::ZhCn => format!("已刷新 {} 的插件状态", run_dir.display()),
					UiLanguage::En => format!("Refreshed plugin states from {}", run_dir.display()),
				});
			}
			Err(error) => self.refusal = Some(error.to_string()),
		}
	}

	pub(super) fn note_plugin_result(&mut self, banner: String, run_dir: Option<PathBuf>) {
		self.banner = Some(banner);
		if let Some(run_dir) = run_dir {
			self.last_run_dir = Some(run_dir);
		}
		self.reload_plugins();
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
		if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
			let at = Position::new(mouse.column, mouse.row);
			let hit = self.hit.get();
			let tab = if hit.tab_launch.contains(at) {
				Some(TopTab::LaunchPlugins)
			} else if hit.tab_merge.contains(at) {
				Some(TopTab::MergeReview)
			} else if hit.tab_settings.contains(at) {
				Some(TopTab::NetworkSettings)
			} else {
				None
			};
			if let Some(tab) = tab {
				self.switch_tab(tab);
				return;
			}
		}
		if self.top_tab != TopTab::MergeReview {
			return;
		}
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
		// `r` refreshes and `R` opens Workshop repair. On Merge Review, `t`
		// copies a read-only `[[resolutions]]` template. Network Settings
		// uses the same key to probe download connectivity.
		if key.code == KeyCode::Char('t') {
			self.copy_resolution_template();
			return;
		}
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

struct ModChoice {
	mod_id: String,
	path: String,
	precedence: usize,
	display_name: String,
}

fn blank_resolution() -> ResolutionEntry {
	ResolutionEntry {
		file: None,
		conflict_id: None,
		mod_id: None,
		r#match: None,
		prefer_mod: None,
		prefer_candidate: None,
		use_file: None,
		keep_existing: None,
		priority_boost: None,
		handler: None,
		policy: None,
	}
}

/// File-scoped `prefer_mod` choices for one unit, highest precedence first.
fn mod_choices(conflicts: &UnitConflicts, decisions: &[DecisionPoint]) -> Vec<ModChoice> {
	let mut choices = Vec::<ModChoice>::new();
	for point in decisions
		.iter()
		.filter(|point| point.unit_id == conflicts.unit_id)
	{
		for option in &point.options {
			let Some((mod_id, file)) = file_prefer_mod(option) else {
				continue;
			};
			if choices.iter().any(|choice| choice.mod_id == mod_id) {
				continue;
			}
			let path = file.unwrap_or_else(|| conflicts.file_path.as_str().to_string());
			choices.push(ModChoice {
				display_name: candidate_name(conflicts, &mod_id),
				precedence: mod_precedence(conflicts, &mod_id),
				mod_id,
				path,
			});
		}
	}
	choices.sort_by(|left, right| {
		right
			.precedence
			.cmp(&left.precedence)
			.then_with(|| left.mod_id.cmp(&right.mod_id))
	});
	choices
}

fn file_prefer_mod(option: &DecisionOption) -> Option<(String, Option<String>)> {
	let DecisionAction::PreferMod { mod_id } = &option.action else {
		return None;
	};
	let resolution = option.scopes.iter().find_map(|scope| match scope {
		DecisionScope::File { resolution } => Some(resolution),
		_ => None,
	})?;
	let chosen = resolution
		.prefer_mod
		.clone()
		.unwrap_or_else(|| mod_id.clone());
	let file = resolution
		.file
		.as_ref()
		.map(|file| file.as_str().to_string());
	Some((chosen, file))
}

pub(super) fn offers_resolution_template(decisions: &[DecisionPoint], unit_id: &str) -> bool {
	decisions.iter().any(|point| {
		point.unit_id == unit_id
			&& point
				.options
				.iter()
				.any(|option| file_prefer_mod(option).is_some())
	})
}

fn file_handlers(decisions: &[DecisionPoint], unit_id: &str) -> Vec<String> {
	let mut names = Vec::new();
	for point in decisions.iter().filter(|point| point.unit_id == unit_id) {
		for option in &point.options {
			let DecisionAction::Handler { name } = &option.action else {
				continue;
			};
			let file_scoped = option
				.scopes
				.iter()
				.any(|scope| matches!(scope, DecisionScope::File { .. }));
			if file_scoped && !names.iter().any(|seen| seen == name) {
				names.push(name.clone());
			}
		}
	}
	names
}

fn candidate_name(conflicts: &UnitConflicts, mod_id: &str) -> String {
	conflicts
		.nodes
		.iter()
		.flat_map(|node| &node.conflicts)
		.flat_map(|leaf| &leaf.candidates)
		.find(|candidate| candidate.mod_id == mod_id)
		.map(|candidate| candidate.mod_display_name.clone())
		.filter(|name| !name.is_empty())
		.unwrap_or_else(|| mod_id.to_string())
}

fn mod_precedence(conflicts: &UnitConflicts, mod_id: &str) -> usize {
	conflicts
		.nodes
		.iter()
		.flat_map(|node| &node.conflicts)
		.flat_map(|leaf| {
			leaf.candidates
				.iter()
				.filter(|candidate| candidate.mod_id == mod_id)
				.map(|candidate| candidate.precedence)
				.chain(
					leaf.contributors
						.iter()
						.filter(|contributor| contributor.mod_id == mod_id)
						.map(|contributor| contributor.precedence),
				)
		})
		.max()
		.unwrap_or(0)
}

fn resolution_template(
	view: &AnalysisView,
	unit_id: &str,
	language: foch::plugin::UiLanguage,
) -> Option<String> {
	let conflicts = view.conflicts.get(unit_id)?;
	let choices = mod_choices(conflicts, &view.decisions);
	let (active_path, active_name, active_precedence, active_mod) = {
		let active = choices.first()?;
		(
			active.path.clone(),
			active.display_name.clone(),
			active.precedence,
			active.mod_id.clone(),
		)
	};
	let active_block = serialize_resolution(prefer_entry(&active_path, &active_mod))?;
	let mut parts = vec![
		super::text::resolution_template_preamble(language, &active_path),
		super::text::choice_comment(language, &active_name, active_precedence),
		active_block,
	];
	for choice in choices.iter().skip(1) {
		let block = serialize_resolution(prefer_entry(&choice.path, &choice.mod_id))?;
		parts.push(super::text::choice_comment(
			language,
			&choice.display_name,
			choice.precedence,
		));
		parts.push(comment_block(&block));
	}
	for handler in file_handlers(&view.decisions, unit_id) {
		let block = serialize_resolution(handler_entry(&active_path, &handler))?;
		parts.push(super::text::handler_comment(language, &handler));
		parts.push(comment_block(&block));
	}
	Some(parts.join("\n"))
}

fn prefer_entry(path: &str, mod_id: &str) -> ResolutionEntry {
	ResolutionEntry {
		r#match: Some(path.to_string()),
		prefer_mod: Some(mod_id.to_string()),
		..blank_resolution()
	}
}

fn handler_entry(path: &str, handler: &str) -> ResolutionEntry {
	ResolutionEntry {
		r#match: Some(path.to_string()),
		handler: Some(handler.to_string()),
		..blank_resolution()
	}
}

fn serialize_resolution(entry: ResolutionEntry) -> Option<String> {
	#[derive(serde::Serialize)]
	struct Document {
		resolutions: Vec<ResolutionEntry>,
	}
	toml::to_string(&Document {
		resolutions: vec![entry],
	})
	.ok()
	.map(|text| text.trim_end().to_string())
}

fn comment_block(block: &str) -> String {
	block
		.lines()
		.map(|line| {
			if line.is_empty() {
				"#".to_string()
			} else {
				format!("# {line}")
			}
		})
		.collect::<Vec<_>>()
		.join("\n")
}

fn function_tab(code: KeyCode) -> Option<TopTab> {
	match code {
		KeyCode::F(1) => Some(TopTab::LaunchPlugins),
		KeyCode::F(2) => Some(TopTab::MergeReview),
		KeyCode::F(3) => Some(TopTab::NetworkSettings),
		_ => None,
	}
}

fn digit_tab(code: KeyCode) -> Option<TopTab> {
	match code {
		KeyCode::Char('1') => Some(TopTab::LaunchPlugins),
		KeyCode::Char('2') => Some(TopTab::MergeReview),
		KeyCode::Char('3') => Some(TopTab::NetworkSettings),
		_ => None,
	}
}

fn cycle<T: Copy + PartialEq>(items: &[T], value: T, forward: bool) -> T {
	let index = items.iter().position(|item| *item == value).unwrap_or(0);
	let next = if forward {
		(index + 1) % items.len()
	} else {
		(index + items.len() - 1) % items.len()
	};
	items[next]
}

fn cycle_transport(value: TransportPreference, forward: bool) -> TransportPreference {
	cycle(
		&[
			TransportPreference::Auto,
			TransportPreference::Http,
			TransportPreference::Gh,
		],
		value,
		forward,
	)
}

fn cycle_mirror(value: MirrorPreference, forward: bool) -> MirrorPreference {
	cycle(
		&[
			MirrorPreference::Auto,
			MirrorPreference::Cn,
			MirrorPreference::Direct,
			MirrorPreference::Custom,
		],
		value,
		forward,
	)
}

fn cycle_language(value: LanguagePreference, forward: bool) -> LanguagePreference {
	cycle(
		&[
			LanguagePreference::Auto,
			LanguagePreference::ZhCn,
			LanguagePreference::En,
		],
		value,
		forward,
	)
}

fn host_dll_label() -> String {
	let Ok(executable) = std::env::current_exe() else {
		return "unknown".to_string();
	};
	let Some(directory) = executable.parent() else {
		return "unknown".to_string();
	};
	for name in [
		foch_runtime::host::BUILT_DLL,
		foch_runtime::host::DISTRIBUTED_DLL,
	] {
		if directory.join(name).is_file() {
			return format!("ready ({name})");
		}
	}
	format!(
		"missing {} / {}",
		foch_runtime::host::BUILT_DLL,
		foch_runtime::host::DISTRIBUTED_DLL
	)
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
	use foch::merge::{
		AddressNode, ConflictCandidate, ConflictContributor, ConflictLeaf, DecisionAction,
		DecisionOption, DecisionPoint, DecisionScope, MergeReviewContributor, MergeReviewSummary,
		MergeUnitKind,
	};
	use foch::project::ResolutionEntry;
	use serde_json::json;
	use std::collections::BTreeMap;

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

	fn rendered(app: &App) -> String {
		use ratatui::Terminal;
		use ratatui::backend::TestBackend;
		let mut terminal = Terminal::new(TestBackend::new(140, 40)).expect("terminal");
		terminal
			.draw(|frame| super::super::render::draw(frame, app))
			.expect("draw");
		let buffer = terminal.backend().buffer().clone();
		(0..buffer.area.height)
			.map(|y| {
				let mut row = String::new();
				let mut skip = 0u16;
				for x in 0..buffer.area.width {
					if skip > 0 {
						skip -= 1;
						continue;
					}
					let symbol = buffer[(x, y)].symbol();
					skip = ratatui::text::Span::raw(symbol).width().saturating_sub(1) as u16;
					row.push_str(symbol);
				}
				row
			})
			.collect::<Vec<_>>()
			.join("\n")
	}

	#[test]
	fn production_launch_tab_is_the_default_and_digits_switch_pages() {
		let mut app = App::open_launch(UserPreferences::default(), None);
		app.ui_language = UiLanguage::En;
		assert_eq!(app.top_tab, TopTab::LaunchPlugins);
		assert!(rendered(&app).contains("[1] Launch & Plugins"));
		press(&mut app, KeyCode::Char('2'));
		assert_eq!(app.top_tab, TopTab::MergeReview);
		press(&mut app, KeyCode::Char('3'));
		assert_eq!(app.top_tab, TopTab::NetworkSettings);
		press(&mut app, KeyCode::F(1));
		assert_eq!(app.top_tab, TopTab::LaunchPlugins);
	}

	#[test]
	fn launch_tab_alerts_toggles_config_and_settings_persist() {
		let temp = tempfile::tempdir().unwrap();
		let store = temp.path().join("store");
		std::fs::create_dir(&store).unwrap();
		let version = store.join("demo.plugin").join("0.1.0+local");
		std::fs::create_dir_all(&version).unwrap();
		std::fs::write(
			version.join("foch-plugin.toml"),
			r#"
schema = 1
[plugin]
id = "demo.plugin"
name = "Demo Plugin"
version = "0.1.0"
description = "A store fixture."
[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "*"
[entry]
kind = "legacy"
path = "plugins/demo.dll"
phase = "entry"
[config.limit]
type = "int"
default = 1
min = 0
max = 2
"#,
		)
		.unwrap();
		let selections = temp.path().join("selections.toml");
		let preferences = temp.path().join("ui-settings.toml");
		let mut app = App::open_launch(
			UserPreferences {
				language: LanguagePreference::En,
				..UserPreferences::default()
			},
			Some(preferences.clone()),
		);
		app.store_root = Some(store);
		app.selections_path = Some(selections);
		app.playset_name = "play".to_string();
		app.plugin_overview = Some(
			foch::plugin::build_overview(
				"play",
				app.store_root.as_ref().unwrap(),
				app.selections_path.as_ref().unwrap(),
				&[foch::plugin::DeclaredModPlugin {
					plugin_id: "dev.foch.eu4-dynamic-trade-blackhole".into(),
					repo: Some("Acture/eu4-dynamic-trade-blackhole".into()),
					game: "eu4".into(),
					game_versions: None,
					required: true,
					min_version: None,
					sha256: None,
					source_mods: vec!["Dynamic Trade".into()],
				}],
				None,
			)
			.unwrap(),
		);
		let shown = rendered(&app);
		assert!(
			shown.contains(
				"Mod \"Dynamic Trade\" recommends plugin dev.foch.eu4-dynamic-trade-blackhole"
			),
			"{shown}"
		);
		assert!(shown.contains("[NOT INSTALLED]"), "{shown}");
		app.plugin_cursor = app
			.plugin_overview
			.as_ref()
			.unwrap()
			.plugins
			.iter()
			.position(|plugin| plugin.id == "demo.plugin")
			.expect("demo plugin");
		press(&mut app, KeyCode::Char(' '));
		let demo = app
			.plugin_overview
			.as_ref()
			.unwrap()
			.plugins
			.iter()
			.find(|plugin| plugin.id == "demo.plugin")
			.unwrap();
		assert!(demo.enabled, "{demo:?}");
		assert!(rendered(&app).contains("[ENABLED]"));
		app.plugin_cursor = app
			.plugin_overview
			.as_ref()
			.unwrap()
			.plugins
			.iter()
			.position(|plugin| plugin.id == "demo.plugin")
			.expect("demo plugin after toggle");

		press(&mut app, KeyCode::Char('c'));
		assert!(app.config_editor.is_some());
		press(&mut app, KeyCode::Right);
		press(&mut app, KeyCode::Right);
		press(&mut app, KeyCode::Char('+'));
		let limit = app.config_editor.as_ref().unwrap().fields[0].value.clone();
		assert_eq!(limit, toml::Value::Integer(2), "int stays inside max");
		press(&mut app, KeyCode::Left);
		press(&mut app, KeyCode::Left);
		press(&mut app, KeyCode::Left);
		press(&mut app, KeyCode::Char('-'));
		let limit = app.config_editor.as_ref().unwrap().fields[0].value.clone();
		assert_eq!(limit, toml::Value::Integer(0), "int stays inside min");
		press(&mut app, KeyCode::Esc);
		assert!(app.config_editor.is_none());

		press(&mut app, KeyCode::Char('3'));
		assert_eq!(app.top_tab, TopTab::NetworkSettings);
		assert!(rendered(&app).contains("Download transport"));
		assert!(rendered(&app).contains("Auto (HTTPS → gh)"));
		press(&mut app, KeyCode::Char(' '));
		let saved = std::fs::read_to_string(&preferences).unwrap();
		assert!(saved.contains("http"), "{saved}");
		press(&mut app, KeyCode::Down);
		press(&mut app, KeyCode::Down);
		press(&mut app, KeyCode::Left);
		assert_eq!(app.ui_language, UiLanguage::ZhCn);
		let shown = rendered(&app);
		assert!(shown.contains("[1] 启动与插件"), "{shown}");
		assert!(shown.contains("下载传输"), "{shown}");
		assert!(shown.contains("简体中文"), "{shown}");
		press(&mut app, KeyCode::Char('1'));
		app.ui_language = UiLanguage::En;
		assert!(rendered(&app).contains("[1] Launch & Plugins"));
	}

	thread_local! {
		static STUB_CONNECTIVITY: std::cell::RefCell<Option<foch::plugin::ConnectivityReport>> =
			const { std::cell::RefCell::new(None) };
	}

	fn stub_connectivity_probe(
		config: &foch::plugin::TransportConfig,
		_options: &foch::plugin::ConnectivityProbeOptions,
	) -> foch::plugin::ConnectivityReport {
		STUB_CONNECTIVITY.with(|slot| {
			let mut report = slot.borrow().clone().expect("connectivity stub");
			report.transport = config.transport;
			report.mirror_policy = config.mirror.clone();
			report
		})
	}

	fn stub_route(reachable: bool, status: u16, detail: &str) -> foch::plugin::ConnectivityReport {
		foch::plugin::ConnectivityReport {
			transport: foch::plugin::InstallTransport::Http,
			mirror_policy: foch::plugin::MirrorPolicy::Direct,
			routes: vec![foch::plugin::RouteProbeOutcome {
				label: "direct".to_string(),
				url: "http://127.0.0.1/offline".to_string(),
				kind: foch::plugin::ProbeRouteKind::Direct,
				reachable,
				http_status: Some(status),
				latency_ms: 7,
				detail: detail.to_string(),
			}],
		}
	}

	#[test]
	fn settings_tab_t_renders_a_reachable_probe_without_saving_preferences() {
		let temp = tempfile::tempdir().unwrap();
		let preferences = temp.path().join("ui-settings.toml");
		let mut app = App::open_launch(
			UserPreferences {
				language: LanguagePreference::En,
				transport: TransportPreference::Http,
				..UserPreferences::default()
			},
			Some(preferences.clone()),
		);
		app.connectivity_probe = stub_connectivity_probe;
		STUB_CONNECTIVITY.with(|slot| {
			*slot.borrow_mut() = Some(stub_route(true, 200, "HEAD"));
		});
		press(&mut app, KeyCode::Char('3'));
		assert_eq!(app.top_tab, TopTab::NetworkSettings);
		press(&mut app, KeyCode::Char('t'));
		assert!(!preferences.exists(), "t must not write ui-settings.toml");
		assert_eq!(
			app.connectivity.as_ref().map(|report| report.transport),
			Some(foch::plugin::InstallTransport::Http)
		);
		let shown = rendered(&app);
		assert!(
			shown.contains("direct reachable HTTP 200 7 ms HEAD"),
			"{shown}"
		);
		assert!(shown.contains("test connectivity"), "{shown}");
		assert!(
			shown.contains("SHA-256 and PE export validation"),
			"{shown}"
		);
	}

	#[test]
	fn settings_tab_t_renders_an_unreachable_probe_in_chinese() {
		let mut app = App::open_launch(
			UserPreferences {
				language: LanguagePreference::ZhCn,
				..UserPreferences::default()
			},
			None,
		);
		app.connectivity_probe = stub_connectivity_probe;
		STUB_CONNECTIVITY.with(|slot| {
			*slot.borrow_mut() = Some(stub_route(false, 502, "HEAD 502"));
		});
		press(&mut app, KeyCode::Char('3'));
		press(&mut app, KeyCode::Char('t'));
		let shown = rendered(&app);
		assert!(
			shown.contains("direct 不可达 HTTP 502 7 ms HEAD 502"),
			"{shown}"
		);
		assert!(shown.contains("测试连通性"), "{shown}");
		assert!(shown.contains("PE 导出表"), "{shown}");
	}

	const IDEA_FILE: &str = "common/ideas/00_country_ideas.txt";

	fn game_path(text: &str) -> foch::model::GamePathBuf {
		foch::model::GamePathBuf::parse(text).expect("game path")
	}

	fn prefer_file(path: &foch::model::GamePathBuf, mod_id: &str) -> DecisionOption {
		DecisionOption {
			action: DecisionAction::PreferMod {
				mod_id: mod_id.to_string(),
			},
			scopes: vec![DecisionScope::File {
				resolution: ResolutionEntry {
					file: Some(path.clone()),
					prefer_mod: Some(mod_id.to_string()),
					..super::blank_resolution()
				},
			}],
		}
	}

	fn handler_file(name: &str) -> DecisionOption {
		DecisionOption {
			action: DecisionAction::Handler {
				name: name.to_string(),
			},
			scopes: vec![DecisionScope::File {
				resolution: ResolutionEntry {
					handler: Some(name.to_string()),
					..super::blank_resolution()
				},
			}],
		}
	}

	fn decision(
		unit_id: &str,
		path: &foch::model::GamePathBuf,
		conflict_id: &str,
		address: &str,
	) -> DecisionPoint {
		DecisionPoint {
			id: format!("conflict:{conflict_id}"),
			unit_id: unit_id.to_string(),
			node_id: format!("{unit_id}#{address}"),
			conflict_id: conflict_id.to_string(),
			file_path: path.clone(),
			address: address.to_string(),
			options: vec![
				prefer_file(path, "111"),
				prefer_file(path, "222"),
				handler_file("last_writer"),
			],
		}
	}

	fn candidate(mod_id: &str, name: &str, precedence: usize, rendered: &str) -> ConflictCandidate {
		ConflictCandidate {
			mod_id: mod_id.to_string(),
			mod_display_name: name.to_string(),
			precedence,
			change_summary: vec![format!("set idea_cost from {name}")],
			rendered: rendered.to_string(),
		}
	}

	fn leaf(
		conflict_id: &str,
		rendered_low: &str,
		rendered_high: &str,
		vanilla: &str,
	) -> ConflictLeaf {
		ConflictLeaf {
			conflict_id: conflict_id.to_string(),
			kind: Some(foch::model::ConflictKind::DeepMergeable),
			contributors: vec![
				ConflictContributor {
					mod_id: "111".to_string(),
					precedence: 1,
				},
				ConflictContributor {
					mod_id: "222".to_string(),
					precedence: 2,
				},
			],
			reason: Some("both mods set idea_cost".to_string()),
			vanilla_snippet: Some(vanilla.to_string()),
			candidates: vec![
				candidate("111", "Low Mod", 1, rendered_low),
				candidate("222", "High Mod", 2, rendered_high),
			],
		}
	}

	fn address_node(unit_id: &str, address: &str, leaf: ConflictLeaf) -> AddressNode {
		AddressNode {
			id: format!("{unit_id}#{address}"),
			parent: None,
			segment: address.to_string(),
			address: address.to_string(),
			conflicts: vec![leaf],
		}
	}

	fn outcome(
		id: &str,
		path: &foch::model::GamePathBuf,
		disposition: MergeDisposition,
	) -> MergeUnitOutcome {
		MergeUnitOutcome {
			id: id.to_string(),
			path: path.clone(),
			family: "ideas".to_string(),
			kind: MergeUnitKind::File,
			disposition,
			strategy: "structural_merge".to_string(),
			summary: "two mods define idea_cost".to_string(),
			output_path: None,
			output_paths: Vec::new(),
			contributors: vec![
				MergeReviewContributor {
					mod_id: "base".to_string(),
					name: "Europa Universalis IV".to_string(),
					source_paths: vec![path.as_str().to_string()],
					precedence: 0,
					is_base_game: true,
				},
				MergeReviewContributor {
					mod_id: "111".to_string(),
					name: "Low Mod".to_string(),
					source_paths: vec![path.as_str().to_string()],
					precedence: 1,
					is_base_game: false,
				},
				MergeReviewContributor {
					mod_id: "222".to_string(),
					name: "High Mod".to_string(),
					source_paths: vec![path.as_str().to_string()],
					precedence: 2,
					is_base_game: false,
				},
			],
			notes: Vec::new(),
			conflict_ids: vec!["cost".to_string(), "name".to_string()],
			handler_resolutions: Vec::new(),
		}
	}

	fn conflict_analysis() -> AnalysisView {
		let path = game_path(IDEA_FILE);
		let unit_id = "file:common/ideas/00_country_ideas.txt";
		let conflicts = UnitConflicts {
			unit_id: unit_id.to_string(),
			file_path: path.clone(),
			reason: "needs a choice".to_string(),
			deferred_reason: foch::model::DeferredUnitReason::NeedsUserChoice,
			nodes: vec![
				address_node(
					unit_id,
					"country_ideas/admin/idea_cost",
					leaf("cost", "idea_cost = 2", "idea_cost = 3", "idea_cost = 1"),
				),
				address_node(
					unit_id,
					"country_ideas/admin/idea_name",
					leaf(
						"name",
						"idea_name = low",
						"idea_name = high",
						"idea_name = vanilla",
					),
				),
			],
		};
		let safe_path = game_path("common/ideas/00_basic_ideas.txt");
		AnalysisView {
			status: MergeAnalysisStatus::CommittableWithDeferrals,
			summary: MergeReviewSummary {
				total: 2,
				safe: 1,
				needs_user_choice: 1,
				..MergeReviewSummary::default()
			},
			units: vec![
				outcome(unit_id, &path, MergeDisposition::NeedsUserChoice),
				outcome(
					&format!("file:{}", safe_path.as_str()),
					&safe_path,
					MergeDisposition::Safe,
				),
			],
			conflicts: BTreeMap::from([(unit_id.to_string(), conflicts)]),
			decisions: vec![
				decision(unit_id, &path, "cost", "country_ideas/admin/idea_cost"),
				decision(unit_id, &path, "name", "country_ideas/admin/idea_name"),
			],
		}
	}

	fn detail_string(unit: &MergeUnitOutcome, view: &AnalysisView, language: UiLanguage) -> String {
		super::super::render::unit_detail(
			unit,
			view.conflicts.get(&unit.id),
			&view.decisions,
			language,
		)
		.lines
		.iter()
		.map(|line| {
			line.spans
				.iter()
				.map(|span| span.content.as_ref())
				.collect::<String>()
		})
		.collect::<Vec<_>>()
		.join("\n")
	}

	fn frame_of(app: &App, width: u16, height: u16) -> String {
		use ratatui::Terminal;
		use ratatui::backend::TestBackend;
		let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
		terminal
			.draw(|frame| super::super::render::draw(frame, app))
			.expect("draw");
		let buffer = terminal.backend().buffer().clone();
		(0..buffer.area.height)
			.map(|row| {
				let mut text = String::new();
				let mut skip = 0u16;
				for column in 0..buffer.area.width {
					if skip > 0 {
						skip -= 1;
						continue;
					}
					let symbol = buffer[(column, row)].symbol();
					skip = ratatui::text::Span::raw(symbol).width().saturating_sub(1) as u16;
					text.push_str(symbol);
				}
				text
			})
			.collect::<Vec<_>>()
			.join("\n")
	}

	#[test]
	fn review_detail_shows_leaf_conflicts_in_english_and_chinese() {
		let view = conflict_analysis();
		let unit = &view.units[0];
		let english = detail_string(unit, &view, UiLanguage::En);
		assert!(english.contains("Leaf conflicts (2)"), "{english}");
		assert!(english.contains("Vanilla / 原版"), "{english}");
		assert!(
			english.contains("deep_mergeable · both mods set idea_cost"),
			"{english}"
		);
		assert!(
			english.contains("country_ideas/admin/idea_cost"),
			"{english}"
		);
		assert!(english.contains("idea_cost = 1"), "{english}");
		assert!(english.contains("Low Mod"), "{english}");
		assert!(english.contains("High Mod"), "{english}");
		assert!(english.contains("111 · precedence 1"), "{english}");
		assert!(english.contains("222 · precedence 2"), "{english}");
		assert!(english.contains("idea_cost = 2"), "{english}");
		assert!(english.contains("idea_cost = 3"), "{english}");
		assert!(
			english.contains("Press t to copy a foch.toml [[resolutions]] template"),
			"{english}"
		);

		let chinese = detail_string(unit, &view, UiLanguage::ZhCn);
		assert!(chinese.contains("叶子冲突 (2)"), "{chinese}");
		assert!(chinese.contains("Vanilla / 原版"), "{chinese}");
		assert!(
			chinese.contains("类型 deep_mergeable · both mods set idea_cost"),
			"{chinese}"
		);
		assert!(chinese.contains("222 · 优先级 2"), "{chinese}");
		assert!(
			chinese.contains("按 t 复制 foch.toml [[resolutions]] 解决规则模板"),
			"{chinese}"
		);

		let mut app = App {
			ui_language: UiLanguage::En,
			..App::default()
		};
		app.analyzed(Ok(view));
		let shown = frame_of(&app, 200, 80);
		assert!(shown.contains("Leaf conflicts (2)"), "{shown}");
		assert!(shown.contains("Vanilla / 原版"), "{shown}");
		assert!(shown.contains("High Mod"), "{shown}");
		assert!(shown.contains("Low Mod"), "{shown}");
		assert!(shown.contains("idea_cost = 1"), "{shown}");
		assert!(shown.contains(" t  resolution template"), "{shown}");

		app.ui_language = UiLanguage::ZhCn;
		let shown = frame_of(&app, 200, 80);
		assert!(shown.contains("叶子冲突 (2)"), "{shown}");
		assert!(shown.contains("Vanilla / 原版"), "{shown}");
		assert!(shown.contains("优先级 2"), "{shown}");
		assert!(
			shown.contains("按 t 复制 foch.toml [[resolutions]] 解决规则模板"),
			"{shown}"
		);
		assert!(shown.contains(" t  解决规则模板"), "{shown}");
	}

	#[test]
	fn t_copies_a_valid_resolution_template_for_the_highest_precedence_mod() {
		assert_eq!(
			KEY_HELP
				.iter()
				.filter(|(key, action, _)| {
					*key == "t" && *action == "copy a foch.toml resolution template"
				})
				.count(),
			1
		);
		assert_eq!(
			KEY_HELP
				.iter()
				.filter(|(key, action, _)| *key == "t" && *action == "test download connectivity")
				.count(),
			1
		);
		assert!(
			KEY_HELP
				.iter()
				.any(|(key, action, _)| { *key == "r" && *action == "inspect the input again" })
		);
		assert!(KEY_HELP.iter().any(|(key, action, _)| {
			*key == "R" && *action == "repair every broken mod in Steam"
		}));

		let mut app = App {
			ui_language: UiLanguage::En,
			..App::default()
		};
		app.analyzed(Ok(conflict_analysis()));
		assert_eq!(
			press(&mut app, KeyCode::Char('r')),
			Some(AppCommand::Refresh)
		);
		assert!(app.effect.is_none());

		app.analyzed(Ok(conflict_analysis()));
		assert_eq!(press(&mut app, KeyCode::Char('t')), None);
		let snippet = match app.effect.clone() {
			Some(Effect::Copy(text)) => text,
			other => panic!("expected a clipboard copy, got {other:?}"),
		};
		assert!(
			app.notice
				.as_deref()
				.is_some_and(|notice| notice.starts_with("Copied foch.toml resolution template")),
			"{:?}",
			app.notice
		);
		assert_eq!(
			snippet
				.lines()
				.filter(|line| *line == "[[resolutions]]")
				.count(),
			1,
			"{snippet}"
		);
		let project = foch::project::Project::from_toml_str(&snippet).expect(&snippet);
		assert_eq!(project.resolutions.len(), 1, "{snippet}");
		assert_eq!(
			project.resolutions[0].r#match.as_deref(),
			Some(IDEA_FILE),
			"{snippet}"
		);
		assert_eq!(
			project.resolutions[0].prefer_mod.as_deref(),
			Some("222"),
			"{snippet}"
		);
		assert!(snippet.contains("# prefer_mod = \"111\""), "{snippet}");
		assert!(snippet.contains("last_writer"), "{snippet}");
		assert!(project.resolutions[0].handler.is_none());

		app.ui_language = UiLanguage::ZhCn;
		assert_eq!(press(&mut app, KeyCode::Char('t')), None);
		assert!(
			app.notice
				.as_deref()
				.is_some_and(|notice| notice.starts_with("已复制 foch.toml 解决规则模板")),
			"{:?}",
			app.notice
		);
	}

	#[test]
	fn t_on_a_safe_unit_copies_nothing() {
		let mut app = App {
			ui_language: UiLanguage::En,
			..App::default()
		};
		app.analyzed(Ok(conflict_analysis()));
		press(&mut app, KeyCode::Down);
		assert_eq!(
			app.selected_unit().map(|unit| unit.disposition),
			Some(MergeDisposition::Safe)
		);
		assert_eq!(press(&mut app, KeyCode::Char('t')), None);
		assert!(app.effect.is_none(), "{:?}", app.effect);
		assert!(
			app.notice
				.as_deref()
				.is_some_and(|notice| notice.starts_with("Nothing was copied")),
			"{:?}",
			app.notice
		);
		let safe = app.selected_unit().expect("safe unit");
		let view = app.analysis().expect("analysis");
		let detail = detail_string(safe, view, UiLanguage::En);
		assert!(!detail.contains("Leaf conflicts"), "{detail}");

		app.ui_language = UiLanguage::ZhCn;
		assert_eq!(press(&mut app, KeyCode::Char('t')), None);
		assert!(app.effect.is_none(), "{:?}", app.effect);
		assert!(
			app.notice
				.as_deref()
				.is_some_and(|notice| notice.starts_with("未复制任何内容")),
			"{:?}",
			app.notice
		);
	}

	#[test]
	fn huge_leaf_conflicts_stay_bounded_and_say_how_many_were_hidden() {
		use super::super::render::{MAX_LEAF_CONFLICTS, MAX_SNIPPET_LINES};

		let path = game_path("events/wide.txt");
		let unit_id = "file:events/wide.txt";
		let long_vanilla = format!("idea = {}", "Z".repeat(400));
		let rendered = (0..20)
			.map(|index| {
				if index + 1 == 20 {
					"UNIQUE_TAIL_LINE_SHOULD_BE_HIDDEN".to_string()
				} else {
					format!("shown-or-hidden-line-{index}")
				}
			})
			.collect::<Vec<_>>()
			.join("\n");
		let mut nodes = Vec::new();
		for index in 0..MAX_LEAF_CONFLICTS + 1 {
			let address = if index == MAX_LEAF_CONFLICTS {
				"hidden/leaf_nine_marker".to_string()
			} else {
				format!("visible/leaf_{index}")
			};
			let vanilla = if index == 0 {
				long_vanilla.clone()
			} else {
				"short = 1".to_string()
			};
			let body = if index == 0 {
				rendered.clone()
			} else {
				"short = 2".to_string()
			};
			nodes.push(address_node(
				unit_id,
				&address,
				leaf(&format!("c{index}"), &body, "short = 3", &vanilla),
			));
		}
		let conflicts = UnitConflicts {
			unit_id: unit_id.to_string(),
			file_path: path.clone(),
			reason: "many".to_string(),
			deferred_reason: foch::model::DeferredUnitReason::NeedsUserChoice,
			nodes,
		};
		let unit = outcome(unit_id, &path, MergeDisposition::NeedsUserChoice);
		let view = AnalysisView {
			status: MergeAnalysisStatus::Blocked,
			summary: MergeReviewSummary {
				total: 1,
				needs_user_choice: 1,
				..MergeReviewSummary::default()
			},
			units: vec![unit.clone()],
			conflicts: BTreeMap::from([(unit_id.to_string(), conflicts)]),
			decisions: Vec::new(),
		};
		let english = detail_string(&unit, &view, UiLanguage::En);
		let hidden_lines = 20 - MAX_SNIPPET_LINES;
		let conflict_count = MAX_LEAF_CONFLICTS + 1;
		assert!(
			english.contains(&format!("+{hidden_lines} more lines")),
			"{english}"
		);
		assert!(english.contains("+1 more conflicts"), "{english}");
		assert!(
			english.contains(&format!("Leaf conflicts ({conflict_count})")),
			"{english}"
		);
		assert!(english.contains("shown-or-hidden-line-0"), "{english}");
		assert!(
			english.contains(&format!("shown-or-hidden-line-{}", MAX_SNIPPET_LINES - 1)),
			"{english}"
		);
		assert!(
			!english.contains(&format!("shown-or-hidden-line-{MAX_SNIPPET_LINES}")),
			"{english}"
		);
		assert!(
			!english.contains("UNIQUE_TAIL_LINE_SHOULD_BE_HIDDEN"),
			"{english}"
		);
		assert!(!english.contains("hidden/leaf_nine_marker"), "{english}");
		assert!(!english.contains(&"Z".repeat(100)), "{english}");
		assert!(english.contains('…'), "{english}");

		let chinese = detail_string(&unit, &view, UiLanguage::ZhCn);
		assert!(
			chinese.contains(&format!("叶子冲突 ({conflict_count})")),
			"{chinese}"
		);
		assert!(
			chinese.contains(&format!("还有 {hidden_lines} 行")),
			"{chinese}"
		);
		assert!(chinese.contains("还有 1 个冲突"), "{chinese}");
		assert!(
			!chinese.contains("UNIQUE_TAIL_LINE_SHOULD_BE_HIDDEN"),
			"{chinese}"
		);
		assert!(!chinese.contains("hidden/leaf_nine_marker"), "{chinese}");
	}
}
