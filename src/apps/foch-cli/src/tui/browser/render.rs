use foch::input::{BaseDataState, CurrentEu4Input, DetectedPlaysetMod};
use foch::merge::{
	ConflictLeaf, DecisionPoint, MergeAnalysisStage, MergeAnalysisStatus, MergeDisposition,
	MergeProgress, MergeUnitKind, MergeUnitOutcome, UnitConflicts,
};
use foch::plugin::UiLanguage;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
	Block, BorderType, Borders, Cell, Clear, Gauge, List, ListItem, ListState, Paragraph, Row,
	Table, TableState, Wrap,
};

use super::app::{
	App, BUILD_STAGES, DISPOSITIONS, Focus, HitAreas, InputPane, KEY_HELP, OPTION_COUNT, Phase,
	Screen, TopTab, disposition_label,
};
use super::text;

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const OK: Color = Color::Green;
const WARN: Color = Color::Yellow;
const BAD: Color = Color::Red;

const STAGES: [(MergeAnalysisStage, &str); 5] = [
	(MergeAnalysisStage::Inventory, "Inventory inputs"),
	(MergeAnalysisStage::ResolveInput, "Resolve the playset"),
	(MergeAnalysisStage::SemanticMerge, "Merge every unit"),
	(MergeAnalysisStage::ValidateOutput, "Validate the result"),
	(MergeAnalysisStage::FreezeArtifacts, "Freeze the result"),
];

pub fn draw(frame: &mut Frame<'_>, app: &App) {
	let [top, main, footer] = Layout::vertical([
		Constraint::Length(1),
		Constraint::Min(6),
		Constraint::Length(1),
	])
	.areas(frame.area());
	draw_tab_bar(frame, app, top);
	match app.top_tab {
		TopTab::LaunchPlugins => draw_launch(frame, app, main),
		TopTab::NetworkSettings => draw_settings(frame, app, main),
		TopTab::MergeReview => draw_merge(frame, app, main),
	}
	frame.render_widget(Paragraph::new(footer_keys(app)), footer);
	if let Some(cursor) = app.options {
		draw_options(frame, app, cursor);
	}
	if app.confirming_exclusions {
		draw_exclusion_prompt(frame, app);
	}
	if app.confirming_build {
		draw_build_prompt(frame, app);
	}
	if app.help {
		draw_help(frame, app);
	}
	if app.config_editor.is_some() {
		draw_config_editor(frame, app);
	}
	if app.install_input.is_some() {
		draw_install_input(frame, app);
	}
	if app.confirming_launch {
		draw_launch_prompt(frame, app);
	}
	if let Some(selection) = app.selection {
		let buffer = frame.buffer_mut();
		let bounds = app.selection_bounds.intersection(buffer.area);
		for position in selected_cells(selection, bounds) {
			buffer[position]
				.modifier
				.toggle(ratatui::style::Modifier::REVERSED);
		}
	}
}

/// The cells from one corner of a drag to the other, in reading order within
/// `area` (the panel the drag started in): whole panel rows between the
/// first and the last.
fn selected_cells((start, end): (Position, Position), area: Rect) -> Vec<Position> {
	let (start, end) = if (start.y, start.x) <= (end.y, end.x) {
		(start, end)
	} else {
		(end, start)
	};
	let mut cells = Vec::new();
	for y in start.y..=end.y.min(area.bottom().saturating_sub(1)) {
		let first = if y == start.y { start.x } else { area.x };
		let last = if y == end.y {
			end.x
		} else {
			area.right().saturating_sub(1)
		};
		for x in first..=last.min(area.right().saturating_sub(1)) {
			cells.push(Position::new(x, y));
		}
	}
	cells
}

/// The text a drag covered in a drawn frame: wide characters once, panel
/// borders and trailing blanks dropped.
pub fn selected_text(
	selection: (Position, Position),
	bounds: Rect,
	buffer: &ratatui::buffer::Buffer,
) -> String {
	const BORDERS: &[char] = &['│', '┃', '─', '━', '╭', '╮', '╰', '╯', '┏', '┓', '┗', '┛'];
	let mut lines: Vec<String> = Vec::new();
	let mut row = None;
	let mut skip = 0;
	for position in selected_cells(selection, bounds.intersection(buffer.area)) {
		if row != Some(position.y) {
			row = Some(position.y);
			skip = 0;
			lines.push(String::new());
		}
		if skip > 0 {
			skip -= 1;
			continue;
		}
		let symbol = buffer[position].symbol();
		skip = Span::raw(symbol).width().saturating_sub(1);
		if let Some(line) = lines.last_mut() {
			line.push_str(symbol);
		}
	}
	lines
		.iter()
		.map(|line| line.trim_matches(|c: char| BORDERS.contains(&c) || c == ' '))
		.collect::<Vec<_>>()
		.join("\n")
		.trim_matches('\n')
		.to_string()
}

fn draw_build_prompt(frame: &mut Frame<'_>, app: &App) {
	let Some(args) = app.data_build_args() else {
		return;
	};
	let command = std::iter::once("foch".to_string())
		.chain(args.command_line())
		.collect::<Vec<_>>()
		.join(" ");
	let lines = vec![
		Line::from(Span::styled(
			"Build EU4 base data from your game installation?",
			Style::new().bold(),
		)),
		Line::from(Span::styled(
			format!("Reads {}", display(&args.from_game_path)),
			Style::new().fg(DIM),
		)),
		Line::from(Span::styled(
			"Parses the whole game (several minutes) and installs the result into Foch's data directory. Game files are not changed.",
			Style::new().fg(DIM),
		)),
		Line::from(""),
		Line::from(vec![
			Span::styled("CLI  ", Style::new().fg(DIM)),
			Span::styled(command, Style::new().fg(Color::Gray)),
		]),
		Line::from(""),
		Line::from(vec![
			key("y"),
			Span::raw(" build   "),
			key("Esc"),
			Span::raw(" cancel"),
		]),
	];
	let height = lines.len() as u16 + 4;
	let popup = centered(frame.area(), 100, height);
	frame.render_widget(Clear, popup);
	frame.render_widget(
		Paragraph::new(lines)
			.wrap(Wrap { trim: false })
			.block(panel(" Build base data ", WARN)),
		popup,
	);
}

fn draw_building(
	frame: &mut Frame<'_>,
	app: &App,
	area: Rect,
	elapsed: std::time::Duration,
	stage: Option<&str>,
	finished: usize,
) {
	let popup = centered(area, 80, BUILD_STAGES.len() as u16 + 8);
	let block = panel(" Building EU4 base data ", ACCENT);
	let inner = block.inner(popup);
	frame.render_widget(Clear, popup);
	frame.render_widget(block, popup);
	let [from_area, gauge_area, _, stages_area, note_area] = Layout::vertical([
		Constraint::Length(1),
		Constraint::Length(1),
		Constraint::Length(1),
		Constraint::Length(BUILD_STAGES.len() as u16),
		Constraint::Min(1),
	])
	.areas(inner);
	let from = app
		.data_build_args()
		.map(|args| display(&args.from_game_path))
		.unwrap_or_default();
	frame.render_widget(
		Paragraph::new(Span::styled(format!("  from {from}"), Style::new().fg(DIM))),
		from_area,
	);
	let total = BUILD_STAGES.len();
	frame.render_widget(
		Gauge::default()
			.gauge_style(Style::new().fg(ACCENT).bg(Color::Rgb(30, 36, 44)))
			.ratio((finished as f64 / total as f64).clamp(0.0, 1.0))
			.label(format!(
				"stage {}/{total}  {}s",
				(finished + 1).min(total),
				elapsed.as_secs()
			)),
		gauge_area,
	);
	let spinner = ["◐", "◓", "◑", "◒"][(elapsed.as_millis() / 250 % 4) as usize];
	let current = stage.and_then(|name| BUILD_STAGES.iter().position(|(key, _)| *key == name));
	let lines = BUILD_STAGES
		.iter()
		.enumerate()
		.map(|(index, (_, label))| {
			let (mark, style) = if index < finished {
				("✔", Style::new().fg(OK))
			} else if Some(index) == current || (current.is_none() && index == finished) {
				(spinner, Style::new().fg(ACCENT).bold())
			} else {
				("○", Style::new().fg(DIM))
			};
			Line::from(Span::styled(format!("  {mark} {label}"), style))
		})
		.collect::<Vec<_>>();
	frame.render_widget(Paragraph::new(lines), stages_area);
	frame.render_widget(
		Paragraph::new(Span::styled(
			"  Parsing takes most of the time. The input is inspected again when the build finishes.",
			Style::new().fg(DIM),
		))
		.wrap(Wrap { trim: false }),
		note_area,
	);
}

fn draw_help(frame: &mut Frame<'_>, app: &App) {
	let mut lines = vec![
		Line::from(Span::styled(
			"Every browser action is also a foch command:",
			Style::new().fg(DIM),
		)),
		Line::from(""),
	];
	for (name, action, command) in KEY_HELP {
		// Merge Review and Network Settings both bind `t`. Localize only the
		// resolution-template row; the connectivity row keeps its own label.
		let action = if *name == "t" && *command == "paste [[resolutions]] into foch.toml" {
			text::resolution_help_action(app.ui_language)
		} else {
			action
		};
		lines.push(Line::from(vec![
			Span::raw(format!("{:>6} ", "")),
			key(name),
			Span::raw(format!(" {action:<46}")),
			Span::styled(command.to_string(), Style::new().fg(Color::Gray)),
		]));
	}
	lines.push(Line::from(""));
	lines.push(Line::from(Span::styled(
		"Press any key to close.",
		Style::new().fg(DIM),
	)));
	let height = lines.len() as u16 + 2;
	let popup = centered(frame.area(), 110, height);
	frame.render_widget(Clear, popup);
	frame.render_widget(Paragraph::new(lines).block(panel(" Keys ", ACCENT)), popup);
}

fn draw_merge(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let [header, body] = Layout::vertical([Constraint::Length(5), Constraint::Min(1)]).areas(area);
	draw_header(frame, app, header);
	match (&app.phase, app.screen) {
		(
			Phase::Building {
				started,
				stage,
				finished,
			},
			_,
		) => draw_building(
			frame,
			app,
			body,
			started.elapsed(),
			stage.as_deref(),
			*finished,
		),
		(Phase::Analyzing { started, progress }, _) => {
			let elapsed = progress
				.map(|progress| progress.elapsed)
				.unwrap_or_else(|| started.elapsed());
			draw_progress(frame, body, *progress, elapsed);
		}
		(Phase::Reviewed(_), Screen::Review) => draw_review(frame, app, body),
		_ => draw_input(frame, app, body),
	}
}

fn draw_tab_bar(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let labels = [
		text::tab_launch(app.ui_language),
		text::tab_merge(app.ui_language, app.review_attention_count()),
		text::tab_settings(app.ui_language),
	];
	let tabs = [
		TopTab::LaunchPlugins,
		TopTab::MergeReview,
		TopTab::NetworkSettings,
	];
	let mut spans = vec![Span::styled(
		" foch ",
		Style::new().fg(Color::Black).bg(ACCENT).bold(),
	)];
	let mut x = area.x.saturating_add(Span::raw(" foch ").width() as u16);
	let mut rects = [Rect::default(); 3];
	for (index, label) in labels.iter().enumerate() {
		let shown = format!(" {label} ");
		let width = Span::raw(&shown).width() as u16;
		rects[index] = Rect::new(x, area.y, width.max(1), 1);
		x = x.saturating_add(width);
		let style = if app.top_tab == tabs[index] {
			Style::new().fg(Color::Black).bg(ACCENT).bold()
		} else {
			Style::new().fg(DIM)
		};
		spans.push(Span::styled(shown, style));
	}
	frame.render_widget(Paragraph::new(Line::from(spans)), area);
	record(app, |hit| {
		hit.tab_launch = rects[0];
		hit.tab_merge = rects[1];
		hit.tab_settings = rects[2];
	});
}

fn draw_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let lines = match &app.input {
		None => vec![Line::from(Span::styled(
			"◌ Inspecting the installed EU4 game and launcher playset…",
			Style::new().fg(ACCENT),
		))],
		Some(input) => header_lines(input),
	};
	frame.render_widget(Paragraph::new(lines).block(panel(" Input ", DIM)), area);
	record(app, |hit| hit.header = area);
}

fn header_lines(input: &CurrentEu4Input) -> Vec<Line<'static>> {
	let game = Line::from(vec![
		field("Game"),
		Span::raw(format!(
			"{} {}",
			input.game.name,
			input.game.version.as_deref().unwrap_or("(version unknown)")
		)),
		Span::styled(
			input.game.install_path.as_ref().map_or_else(
				|| "  not found".to_string(),
				|path| format!("  {}", display(path)),
			),
			Style::new().fg(DIM),
		),
	]);
	let (base_state, base_color) = match input.base_data.state {
		BaseDataState::Ready => ("ready", OK),
		BaseDataState::Missing => ("missing", BAD),
		BaseDataState::Stale => ("stale", WARN),
	};
	let base = Line::from(vec![
		field("Base data"),
		badge(base_state, base_color),
		Span::raw(format!(
			" {}",
			input.base_data.version.as_deref().unwrap_or_default()
		)),
		Span::styled(
			format!("  {}", input.base_data.detail),
			Style::new().fg(DIM),
		),
	]);
	let playset = match &input.playset {
		None => Line::from(vec![
			field("Playset"),
			Span::styled("no launcher playset found", Style::new().fg(BAD)),
		]),
		Some(playset) => Line::from(vec![
			field("Playset"),
			Span::raw(format!("{} · {} mods", playset.name, playset.mods.len())),
			Span::styled(
				format!("  {}", display(&playset.source_path)),
				Style::new().fg(DIM),
			),
		]),
	};
	vec![game, base, playset]
}

/// What the user can do next, stated before anything else on the screen.
fn next_step(app: &App) -> (Line<'static>, Color) {
	let mod_count = app
		.input
		.as_ref()
		.and_then(|input| input.playset.as_ref())
		.map_or(0, |playset| playset.mods.len());
	let issue_count = app.input.as_ref().map_or(0, |input| input.issues.len());
	let mut step = match &app.phase {
		Phase::Inspecting => (
			Line::from("◌ Inspecting the installed EU4 game and launcher playset…"),
			ACCENT,
		),
		Phase::Failed(_) => (
			Line::from(vec![
				Span::raw("✖ The analysis failed; the reason is under Issues. Press "),
				key("r"),
				Span::raw(" to inspect and try again."),
			]),
			BAD,
		),
		Phase::Reviewed(_) => (
			Line::from(vec![
				Span::raw("✔ Analysis complete. Press "),
				key("i"),
				Span::raw(" to return to the review."),
			]),
			OK,
		),
		_ if app.selectable && app.needs_base_data() => (
			Line::from(vec![
				Span::raw("⚠ The EU4 base data is not ready. Press "),
				key("B"),
				Span::raw(" to build it here, or "),
				key("o"),
				Span::raw(" to turn off the EU4 base as ancestor (--no-game-base)."),
			]),
			WARN,
		),
		_ if app.selectable && !app.problem_mods().is_empty() => {
			let problems = app.problem_mods().len();
			(
				Line::from(vec![
					Span::raw(format!(
						"⚠ {problems} of {mod_count} mods cannot be analyzed. Press "
					)),
					key("X"),
					Span::raw(" to exclude them (or "),
					key("x"),
					Span::raw(" on one), then "),
					key("a"),
					Span::raw("."),
				]),
				WARN,
			)
		}
		_ if app.can_analyze() && !app.effective_exclusions().is_empty() => (
			Line::from(vec![
				Span::raw(format!(
					"✔ Ready without {} excluded mods. Press ",
					app.effective_exclusions().len()
				)),
				key("a"),
				Span::raw(format!(" to analyze the other {}.", app.included_count())),
			]),
			OK,
		),
		_ if app.can_analyze() => (
			Line::from(vec![
				Span::raw("✔ Ready. Press "),
				key("a"),
				Span::raw(format!(" to analyze all {mod_count} mods.")),
			]),
			OK,
		),
		_ if app.selectable => (
			Line::from(vec![
				Span::raw("⚠ Every mod is excluded. Press "),
				key("x"),
				Span::raw(" on a mod to include it again."),
			]),
			WARN,
		),
		_ => (
			Line::from(vec![
				Span::raw(format!(
					"✖ Analysis is blocked by {issue_count} issue{} listed below. Fix {}, then press ",
					if issue_count == 1 { "" } else { "s" },
					if issue_count == 1 { "it" } else { "them" }
				)),
				key("r"),
				Span::raw("."),
			]),
			BAD,
		),
	};
	if app.settings_changed() {
		step.0.spans.push(Span::styled(
			"   Options changed: press r to re-analyze.",
			Style::new().fg(WARN),
		));
	}
	step
}

fn draw_input(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let [banner_area, main] =
		Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).areas(area);
	let (step, color) = match (&app.refusal, &app.notice) {
		(Some(refusal), _) => (Line::from(format!("✖ {refusal}")), BAD),
		(None, Some(notice)) => (Line::from(format!("✔ {notice}")), ACCENT),
		(None, None) => next_step(app),
	};
	let command = Line::from(vec![
		Span::styled("CLI  ", Style::new().fg(DIM)),
		Span::styled(app.cli_command(), Style::new().fg(Color::Gray)),
	]);
	frame.render_widget(
		Paragraph::new(vec![step.style(Style::new().fg(color).bold()), command])
			.wrap(Wrap { trim: false })
			.block(panel(" Next step ", color)),
		banner_area,
	);
	record(app, |hit| hit.banner = banner_area);

	let [mods_area, side] =
		Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(main);
	let [selected_area, issues_area] =
		Layout::vertical([Constraint::Length(9), Constraint::Min(3)]).areas(side);

	let mods = app
		.input
		.as_ref()
		.and_then(|input| input.playset.as_ref())
		.map_or(&[][..], |playset| &playset.mods);
	draw_mod_table(frame, app, mods, mods_area);
	draw_selected_mod(frame, app, mods.get(app.mod_scroll), selected_area);
	draw_issues(frame, app, issues_area);
}

fn draw_mod_table(frame: &mut Frame<'_>, app: &App, mods: &[DetectedPlaysetMod], area: Rect) {
	// The name and id always show; the version gives way on narrow screens.
	let with_version = area.width >= 90;
	let rows = mods
		.iter()
		.map(|playset_mod| {
			let (status, color) = mod_status(app, playset_mod);
			let name_style = if app.is_excluded(playset_mod) {
				Style::new().fg(DIM).crossed_out()
			} else {
				Style::new()
			};
			let mut cells = vec![
				Cell::from(Span::styled(
					format!("{:>3}", playset_mod.position),
					Style::new().fg(DIM),
				)),
				Cell::from(Span::styled(playset_mod.name.clone(), name_style)),
				Cell::from(Span::styled(playset_mod.id.clone(), Style::new().fg(DIM))),
			];
			if with_version {
				cells.push(Cell::from(Span::styled(
					playset_mod.version.clone().unwrap_or_default(),
					Style::new().fg(DIM),
				)));
			}
			cells.push(Cell::from(Span::styled(
				format!("● {status}"),
				Style::new().fg(color),
			)));
			Row::new(cells)
		})
		.collect::<Vec<_>>();
	let (header, widths) = if with_version {
		(
			Row::new(["  #", "Mod", "Workshop id", "Version", "Status"]),
			vec![
				Constraint::Length(3),
				Constraint::Min(12),
				Constraint::Length(10),
				Constraint::Length(9),
				Constraint::Length(16),
			],
		)
	} else {
		(
			Row::new(["  #", "Mod", "Workshop id", "Status"]),
			vec![
				Constraint::Length(3),
				Constraint::Min(12),
				Constraint::Length(10),
				Constraint::Length(16),
			],
		)
	};
	let table = Table::new(rows, widths)
		.header(header.style(Style::new().fg(ACCENT).bold()))
		.column_spacing(1)
		.row_highlight_style(Style::new().bg(Color::Rgb(40, 52, 64)).bold())
		.highlight_symbol("▶ ")
		.block(focus_panel(
			format!(" Mods · {} in load order (later wins) ", mods.len()),
			DIM,
			app.input_pane == InputPane::Mods,
		));
	let mut state =
		TableState::default().with_selected((!mods.is_empty()).then_some(app.mod_scroll));
	frame.render_stateful_widget(table, area, &mut state);
	record(app, |hit| {
		hit.mods = area;
		hit.mods_offset = state.offset();
	});
}

fn mod_status(app: &App, playset_mod: &DetectedPlaysetMod) -> (&'static str, Color) {
	if app.is_excluded(playset_mod) {
		("excluded", DIM)
	} else if !playset_mod.enabled {
		("disabled", DIM)
	} else if playset_mod.source_error.is_some() {
		("cannot analyze", BAD)
	} else {
		("ok", OK)
	}
}

fn draw_selected_mod(
	frame: &mut Frame<'_>,
	app: &App,
	playset_mod: Option<&DetectedPlaysetMod>,
	area: Rect,
) {
	let Some(playset_mod) = playset_mod else {
		frame.render_widget(
			Paragraph::new(Span::styled("No mod selected.", Style::new().fg(DIM)))
				.block(panel(" Selected mod ", DIM)),
			area,
		);
		return;
	};
	let (status, color) = mod_status(app, playset_mod);
	let mut lines = vec![
		Line::from(Span::styled(playset_mod.name.clone(), Style::new().bold())),
		Line::from(vec![
			badge(status, color),
			Span::styled(
				format!(
					"  #{} · {}{}",
					playset_mod.position,
					playset_mod.id,
					playset_mod
						.version
						.as_deref()
						.map(|version| format!(" · v{version}"))
						.unwrap_or_default()
				),
				Style::new().fg(DIM),
			),
		]),
	];
	if playset_mod.declared_dependencies.is_empty() {
		lines.push(Line::from(Span::styled(
			"No declared dependencies",
			Style::new().fg(DIM),
		)));
	} else {
		lines.push(Line::from(vec![
			Span::styled("depends on: ", Style::new().fg(ACCENT)),
			Span::raw(playset_mod.declared_dependencies.join(", ")),
		]));
	}
	if let Some(error) = &playset_mod.source_error {
		lines.push(Line::from(Span::styled(
			clean(error),
			Style::new().fg(color),
		)));
	}
	lines.push(Line::from(vec![
		key("x"),
		Span::styled(
			if app.is_excluded(playset_mod) {
				" include in the analysis"
			} else {
				" exclude from the analysis"
			},
			Style::new().fg(DIM),
		),
	]));
	frame.render_widget(
		Paragraph::new(lines)
			.wrap(Wrap { trim: false })
			.scroll((app.mod_detail_scroll, 0))
			.block(focus_panel(
				" Selected mod ",
				DIM,
				app.input_pane == InputPane::Mod,
			)),
		area,
	);
	record(app, |hit| hit.selected_mod = area);
}

fn draw_issues(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let mut lines = Vec::new();
	if let Phase::Failed(error) = &app.phase {
		lines.push(Line::from(Span::styled(
			"✖ Analysis failed",
			Style::new().fg(BAD).bold(),
		)));
		lines.push(Line::from(Span::styled(clean(error), Style::new().fg(BAD))));
		lines.push(Line::from(""));
	}
	let issues = app
		.input
		.as_ref()
		.map_or(&[][..], |input| &input.issues[..]);
	for issue in issues {
		lines.push(Line::from(Span::styled(
			format!("⚠ {}", issue.title),
			Style::new().fg(WARN).bold(),
		)));
		lines.push(Line::from(Span::styled(
			clean(&issue.detail),
			Style::new().fg(DIM),
		)));
		if let Some(action) = &issue.action {
			lines.push(Line::from(Span::styled(
				format!("→ {action}"),
				Style::new().fg(ACCENT),
			)));
		}
		lines.push(Line::from(""));
	}
	let color = if lines.is_empty() {
		lines.push(Line::from(Span::styled("✔ No issues", Style::new().fg(OK))));
		DIM
	} else {
		WARN
	};
	frame.render_widget(
		Paragraph::new(lines)
			.wrap(Wrap { trim: false })
			.scroll((app.issues_scroll, 0))
			.block(focus_panel(
				format!(" Issues · {} ", issues.len()),
				color,
				app.input_pane == InputPane::Issues,
			)),
		area,
	);
	record(app, |hit| hit.issues = area);
}

fn draw_progress(
	frame: &mut Frame<'_>,
	area: Rect,
	progress: Option<MergeProgress>,
	elapsed: std::time::Duration,
) {
	let popup = centered(area, 72, 13);
	let block = panel(" Analyzing every contributor ", ACCENT);
	let inner = block.inner(popup);
	frame.render_widget(block, popup);
	let [gauge_area, _, stages_area, note_area] = Layout::vertical([
		Constraint::Length(1),
		Constraint::Length(1),
		Constraint::Length(STAGES.len() as u16),
		Constraint::Min(1),
	])
	.areas(inner);

	let (ratio, units) = match progress.and_then(|p| p.completed_units.zip(p.total_units)) {
		Some((done, total)) if total > 0 => (
			(done as f64 / total as f64).clamp(0.0, 1.0),
			format!("{done}/{total} units"),
		),
		_ => (0.0, String::new()),
	};
	frame.render_widget(
		Gauge::default()
			.gauge_style(Style::new().fg(ACCENT).bg(Color::Rgb(30, 36, 44)))
			.ratio(ratio)
			.label(format!("{units}  {}s", elapsed.as_secs())),
		gauge_area,
	);

	let current = progress.map(|progress| {
		let index = STAGES
			.iter()
			.position(|(stage, _)| *stage == progress.stage)
			.unwrap_or(0);
		(index, progress.completed)
	});
	let stages = STAGES
		.iter()
		.enumerate()
		.map(|(index, (_, name))| {
			let (mark, style) = match current {
				Some((at, completed)) if index < at || (index == at && completed) => {
					("✔", Style::new().fg(OK))
				}
				Some((at, _)) if index == at => ("◐", Style::new().fg(ACCENT).bold()),
				_ => ("○", Style::new().fg(DIM)),
			};
			Line::from(Span::styled(format!("  {mark} {name}"), style))
		})
		.collect::<Vec<_>>();
	frame.render_widget(Paragraph::new(stages), stages_area);
	frame.render_widget(
		Paragraph::new(Line::from(vec![
			Span::styled(
				"  Runs to completion over the frozen input. Press ",
				Style::new().fg(DIM),
			),
			key("q"),
			Span::styled(" to cancel and quit.", Style::new().fg(DIM)),
		]))
		.wrap(Wrap { trim: false }),
		note_area,
	);
}

fn draw_review(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let Some(view) = app.analysis() else {
		return;
	};
	let [summary_area, main] =
		Layout::vertical([Constraint::Length(4), Constraint::Min(3)]).areas(area);

	let mut chips = vec![chip(
		format!("0 all {}", view.summary.total),
		Color::White,
		app.filter.is_none(),
	)];
	for (index, disposition) in DISPOSITIONS.iter().enumerate() {
		chips.push(Span::raw(" "));
		chips.push(chip(
			format!(
				"{} {} {}",
				index + 1,
				disposition_label(*disposition),
				view.count(*disposition)
			),
			disposition_color(*disposition),
			app.filter == Some(*disposition),
		));
	}
	let (status, color) = match view.status {
		MergeAnalysisStatus::ReadyToCommit => ("ready to commit", OK),
		MergeAnalysisStatus::CommittableWithDeferrals => ("committable with deferrals", WARN),
		MergeAnalysisStatus::Blocked => ("blocked", BAD),
	};
	let mut search = match (app.focus, app.query.is_empty()) {
		(Focus::Search, _) => vec![
			Span::styled("search ", Style::new().fg(ACCENT)),
			Span::raw(format!("{}▏", app.query)),
		],
		(_, false) => vec![
			Span::styled("search ", Style::new().fg(ACCENT)),
			Span::raw(app.query.clone()),
		],
		(_, true) => vec![Span::styled(
			"press / to search paths",
			Style::new().fg(DIM),
		)],
	};
	if let Some(refusal) = &app.refusal {
		search = vec![Span::styled(
			format!("✖ {refusal}"),
			Style::new().fg(BAD).bold(),
		)];
	} else if let Some(notice) = &app.notice {
		search = vec![Span::styled(
			format!("✔ {notice}"),
			Style::new().fg(ACCENT).bold(),
		)];
	}
	if app.settings_changed() {
		search.push(Span::styled(
			"   Options changed: press r to re-analyze.",
			Style::new().fg(WARN),
		));
	}
	frame.render_widget(
		Paragraph::new(vec![Line::from(chips), Line::from(search)])
			.block(panel(format!(" Analysis · {status} "), color)),
		summary_area,
	);
	record(app, |hit| hit.summary = summary_area);

	let [list_area, detail_area] =
		Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(main);
	let visible = app.visible_units();
	let items = visible
		.iter()
		.map(|index| {
			let unit = &view.units[*index];
			ListItem::new(Line::from(vec![
				Span::styled(
					format!("{:<6}", short_disposition(unit.disposition)),
					Style::new().fg(disposition_color(unit.disposition)).bold(),
				),
				Span::raw(" "),
				Span::raw(unit.path.as_str().to_owned()),
			]))
		})
		.collect::<Vec<_>>();
	let mut state =
		ListState::default().with_selected((!visible.is_empty()).then_some(app.selected));
	frame.render_stateful_widget(
		List::new(items)
			.block(focus_panel(
				format!(" Units · {}/{} ", visible.len(), view.units.len()),
				DIM,
				app.focus == Focus::Units,
			))
			.highlight_style(Style::new().bg(Color::Rgb(40, 52, 64)).bold())
			.highlight_symbol("▶ "),
		list_area,
		&mut state,
	);
	record(app, |hit| {
		hit.units = list_area;
		hit.units_offset = state.offset();
		hit.detail = detail_area;
	});

	let detail = app.selected_unit().map_or_else(
		|| {
			Text::from(Span::styled(
				"No unit matches this filter.",
				Style::new().fg(DIM),
			))
		},
		|unit| detail_for(app, unit),
	);
	frame.render_widget(
		Paragraph::new(detail)
			.wrap(Wrap { trim: false })
			.scroll((app.detail_scroll, 0))
			.block(focus_panel(" Detail ", DIM, app.focus == Focus::Detail)),
		detail_area,
	);
}

/// How many leaf conflicts the detail pane lists before the overflow footer.
pub(super) const MAX_LEAF_CONFLICTS: usize = 8;
/// Lines of a vanilla or candidate snippet kept before `+N more lines`.
pub(super) const MAX_SNIPPET_LINES: usize = 6;
const MAX_SNIPPET_CHARS: usize = 100;
const MAX_SUMMARY_LINES: usize = 4;

pub(super) fn detail_for(app: &App, unit: &MergeUnitOutcome) -> Text<'static> {
	match app.analysis() {
		Some(view) => detail_from_analysis(unit, view, app.ui_language),
		None => unit_detail(unit, None, &[], app.ui_language),
	}
}

fn detail_from_analysis(
	unit: &MergeUnitOutcome,
	view: &super::app::AnalysisView,
	language: UiLanguage,
) -> Text<'static> {
	unit_detail(
		unit,
		view.conflicts.get(&unit.id),
		&view.decisions,
		language,
	)
}

/// Everything the analysis knows about one unit: the vanilla ancestor, every
/// contributor in precedence order with its source files, the outputs the
/// analysis would commit, and that unit's leaf conflicts when it has them.
pub fn unit_detail(
	unit: &MergeUnitOutcome,
	conflicts: Option<&UnitConflicts>,
	decisions: &[DecisionPoint],
	language: UiLanguage,
) -> Text<'static> {
	let mut lines = vec![
		Line::from(Span::styled(
			unit.path.as_str().to_owned(),
			Style::new().bold(),
		)),
		Line::from(vec![
			badge(
				disposition_label(unit.disposition),
				disposition_color(unit.disposition),
			),
			Span::styled(
				format!(
					"  {} · {} · {}",
					unit.family,
					match unit.kind {
						MergeUnitKind::File => "file",
						MergeUnitKind::DefinitionModule => "definition module",
					},
					unit.strategy
				),
				Style::new().fg(DIM),
			),
		]),
	];
	if !unit.summary.is_empty() {
		lines.push(Line::from(""));
		lines.push(Line::from(unit.summary.clone()));
	}

	push_section(&mut lines, "Ancestor");
	let ancestors = unit
		.contributors
		.iter()
		.filter(|contributor| contributor.is_base_game)
		.collect::<Vec<_>>();
	if ancestors.is_empty() {
		lines.push(Line::from(Span::styled(
			"  none: no vanilla definition at this unit",
			Style::new().fg(DIM),
		)));
	}
	for ancestor in ancestors {
		lines.push(Line::from(format!("  {}", ancestor.name)));
		for path in &ancestor.source_paths {
			lines.push(source_line(path));
		}
	}

	push_section(&mut lines, "Contributors (precedence order)");
	let mut contributors = unit
		.contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game)
		.collect::<Vec<_>>();
	contributors.sort_by_key(|contributor| contributor.precedence);
	for contributor in contributors {
		lines.push(Line::from(vec![
			Span::styled("  ▸ ", Style::new().fg(ACCENT)),
			Span::raw(contributor.name.clone()),
			Span::styled(
				format!(
					"  {} · precedence {}",
					contributor.mod_id, contributor.precedence
				),
				Style::new().fg(DIM),
			),
		]));
		for path in &contributor.source_paths {
			lines.push(source_line(path));
		}
	}

	push_section(&mut lines, "Result");
	if unit.output_paths.is_empty() {
		lines.push(Line::from(Span::styled(
			"  no output: this unit would be deferred",
			Style::new().fg(DIM),
		)));
	}
	for path in &unit.output_paths {
		lines.push(Line::from(format!("  → {}", path.as_str())));
	}

	if !unit.notes.is_empty() {
		push_section(&mut lines, "Notes");
		for note in &unit.notes {
			lines.push(Line::from(format!("  {note}")));
		}
	}
	if let Some(conflicts) = conflicts {
		let before = lines.len();
		push_leaf_conflicts(&mut lines, conflicts, language);
		if lines.len() > before && super::app::offers_resolution_template(decisions, &unit.id) {
			lines.push(Line::from(""));
			lines.push(Line::from(Span::styled(
				text::resolution_detail_hint(language).to_string(),
				Style::new().fg(DIM),
			)));
		}
	}
	Text::from(lines)
}

fn push_leaf_conflicts(
	lines: &mut Vec<Line<'static>>,
	conflicts: &UnitConflicts,
	language: UiLanguage,
) {
	let rows = conflict_rows(conflicts);
	let total = rows.len();
	if total == 0 {
		return;
	}
	push_section_owned(lines, text::leaf_conflicts_heading(language, total));
	let shown = total.min(MAX_LEAF_CONFLICTS);
	for (address, leaf) in rows.iter().take(shown) {
		push_one_conflict(lines, address, leaf, language);
	}
	let hidden = total - shown;
	if hidden > 0 {
		lines.push(Line::from(Span::styled(
			text::more_conflicts(language, hidden),
			Style::new().fg(WARN),
		)));
	}
}

fn conflict_rows(conflicts: &UnitConflicts) -> Vec<(&str, &ConflictLeaf)> {
	let mut rows = Vec::new();
	for node in &conflicts.nodes {
		for leaf in &node.conflicts {
			rows.push((node.address.as_str(), leaf));
		}
	}
	rows
}

fn push_one_conflict(
	lines: &mut Vec<Line<'static>>,
	address: &str,
	leaf: &ConflictLeaf,
	language: UiLanguage,
) {
	lines.push(Line::from(Span::styled(
		format!("  {address}"),
		Style::new().bold(),
	)));
	lines.push(Line::from(text::conflict_kind_line(
		language,
		leaf.kind.map(|kind| kind.as_str()),
		leaf.reason.as_deref(),
	)));
	if let Some(vanilla) = &leaf.vanilla_snippet {
		lines.push(Line::from(Span::styled(
			format!("  {}", text::vanilla_heading()),
			Style::new().fg(OK).bold(),
		)));
		push_bounded_block(lines, vanilla, language);
	}
	for candidate in &leaf.candidates {
		lines.push(Line::from(vec![
			Span::styled("  ▸ ", Style::new().fg(ACCENT)),
			Span::raw(candidate.mod_display_name.clone()),
			Span::styled(
				text::candidate_meta(language, &candidate.mod_id, candidate.precedence),
				Style::new().fg(DIM),
			),
		]));
		for summary in candidate.change_summary.iter().take(MAX_SUMMARY_LINES) {
			lines.push(Line::from(format!("    {}", clip_line(summary))));
		}
		push_bounded_block(lines, &candidate.rendered, language);
	}
}

fn push_bounded_block(lines: &mut Vec<Line<'static>>, text: &str, language: UiLanguage) {
	if text.is_empty() {
		return;
	}
	let raw = text.lines().collect::<Vec<_>>();
	let shown = raw.len().min(MAX_SNIPPET_LINES);
	for line in raw.iter().take(shown) {
		lines.push(Line::from(format!("    {}", clip_line(line))));
	}
	let hidden = raw.len() - shown;
	if hidden > 0 {
		lines.push(Line::from(Span::styled(
			text::more_lines(language, hidden),
			Style::new().fg(DIM),
		)));
	}
}

fn clip_line(line: &str) -> String {
	let mut chars = line.chars();
	let clipped = chars.by_ref().take(MAX_SNIPPET_CHARS).collect::<String>();
	if chars.next().is_some() {
		format!("{clipped}…")
	} else {
		clipped
	}
}

fn push_section_owned(lines: &mut Vec<Line<'static>>, text: String) {
	lines.push(Line::from(""));
	lines.push(Line::from(Span::styled(
		format!("── {text} "),
		Style::new().fg(ACCENT).bold(),
	)));
}

fn draw_exclusion_prompt(frame: &mut Frame<'_>, app: &App) {
	let excluded = app
		.mods()
		.iter()
		.filter(|playset_mod| app.is_excluded(playset_mod))
		.collect::<Vec<_>>();
	let mut lines = vec![
		Line::from(Span::styled(
			format!(
				"Analyze {} of {} mods, leaving these {} out?",
				app.included_count(),
				app.mods().len(),
				excluded.len()
			),
			Style::new().bold(),
		)),
		Line::from(Span::styled(
			"The result will not represent your full playset. Nothing is changed in the launcher.",
			Style::new().fg(DIM),
		)),
		Line::from(""),
	];
	for playset_mod in &excluded {
		lines.push(Line::from(vec![
			Span::styled(
				format!("  #{} {}", playset_mod.position, playset_mod.name),
				Style::new().fg(WARN),
			),
			Span::styled(format!("  {}", playset_mod.id), Style::new().fg(DIM)),
		]));
		lines.push(Line::from(Span::styled(
			format!(
				"     {}",
				playset_mod
					.source_error
					.as_deref()
					.map_or_else(|| "excluded by you".to_string(), clean)
			),
			Style::new().fg(DIM),
		)));
	}
	lines.push(Line::from(""));
	lines.push(Line::from(vec![
		key("y"),
		Span::raw(" analyze without them   "),
		key("Esc"),
		Span::raw(" cancel"),
	]));
	let height = lines.len() as u16 + 2;
	let popup = centered(frame.area(), 100, height);
	frame.render_widget(Clear, popup);
	frame.render_widget(
		Paragraph::new(lines)
			.wrap(Wrap { trim: false })
			.block(panel(" Leave mods out? ", WARN)),
		popup,
	);
}

fn draw_options(frame: &mut Frame<'_>, app: &App, cursor: usize) {
	let settings = &app.settings;
	let check = |on: bool| if on { "■" } else { "□" };
	let game_base = format!(
		"{} Use the EU4 base as ancestor{}",
		check(settings.game_base),
		if app.game_base_available {
			""
		} else {
			" (base data not ready)"
		}
	);
	let rows: [(String, &str); OPTION_COUNT] = [
		(game_base, "off = --no-game-base"),
		(
			format!("{} GUI scroll merge", check(settings.gui_scroll_merge)),
			"--gui-scroll-merge",
		),
		(
			format!(
				"{} Ignore replace_path",
				check(settings.ignore_replace_path)
			),
			"--ignore-replace-path",
		),
		(
			format!("{} Use supported fallbacks", check(settings.force)),
			"--force",
		),
		(
			format!("  Workers  ◀ {} ▶", settings.merge_workers),
			"speed only; same output",
		),
	];
	let mut lines = rows
		.iter()
		.enumerate()
		.map(|(index, (text, hint))| {
			let selected = index == cursor;
			let style = if selected {
				Style::new().fg(Color::Black).bg(ACCENT).bold()
			} else {
				Style::new()
			};
			Line::from(vec![
				Span::styled(if selected { "▶ " } else { "  " }, Style::new().fg(ACCENT)),
				Span::styled(format!("{text:<58}"), style),
				Span::styled(format!("  {hint}"), Style::new().fg(DIM)),
			])
		})
		.collect::<Vec<_>>();
	lines.push(Line::from(""));
	lines.push(Line::from(Span::styled(
		"  Options apply to the next analysis and are not saved.",
		Style::new().fg(DIM),
	)));
	lines.push(Line::from(vec![
		Span::styled("  CLI  ", Style::new().fg(DIM)),
		Span::styled(app.cli_command(), Style::new().fg(Color::Gray)),
	]));
	lines.push(Line::from(vec![
		Span::raw("  "),
		key("↑↓"),
		Span::raw(" select  "),
		key("Space"),
		Span::raw(" toggle  "),
		key("←→"),
		Span::raw(" workers  "),
		key("Esc"),
		Span::raw(" close"),
	]));
	let height = lines.len() as u16 + 2;
	let popup = centered(frame.area(), 90, height);
	frame.render_widget(Clear, popup);
	frame.render_widget(
		Paragraph::new(lines).block(panel(" Analysis options ", ACCENT)),
		popup,
	);
}

fn footer_pairs(pairs: &[(&str, &str)]) -> Line<'static> {
	let mut spans = vec![Span::raw(" ")];
	for (name, action) in pairs {
		spans.push(key(name));
		spans.push(Span::styled(format!(" {action}  "), Style::new().fg(DIM)));
	}
	Line::from(spans)
}

fn draw_launch(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let [strip, alert, body] = Layout::vertical([
		Constraint::Length(4),
		Constraint::Length(3),
		Constraint::Min(4),
	])
	.areas(area);
	let input = app.input.as_ref();
	let game = input.map(|input| input.game.name.as_str()).unwrap_or("EU4");
	let version = input
		.and_then(|input| input.game.version.as_deref())
		.unwrap_or("unknown");
	let install = input
		.and_then(|input| input.game.install_path.as_ref())
		.map(|path| display(path))
		.unwrap_or_else(|| "not found".to_string());
	let playset = input
		.and_then(|input| input.playset.as_ref())
		.map(|playset| playset.name.as_str())
		.unwrap_or(app.playset_name.as_str());
	let strip_lines = vec![
		Line::from(vec![
			field("EU4"),
			Span::raw(format!("{game} {version}")),
			Span::styled(format!("  {install}"), Style::new().fg(DIM)),
		]),
		Line::from(vec![
			field("Host DLL"),
			Span::raw(if app.host_label.is_empty() {
				"unknown".to_string()
			} else {
				app.host_label.clone()
			}),
		]),
		Line::from(vec![field("Playset"), Span::raw(playset.to_string())]),
	];
	frame.render_widget(
		Paragraph::new(strip_lines).block(panel(" Environment ", DIM)),
		strip,
	);
	let mut alerts = Vec::new();
	if let Some(overview) = &app.plugin_overview {
		for plugin in &overview.plugins {
			if plugin.declared_by_mods.is_empty() {
				continue;
			}
			if plugin.enabled && !plugin.installed_versions.is_empty() {
				continue;
			}
			for source in &plugin.declared_by_mods {
				alerts.push(Line::from(Span::styled(
					text::companion_alert(app.ui_language, source, &plugin.id),
					Style::new().fg(WARN).bold(),
				)));
			}
		}
	}
	if alerts.is_empty() {
		alerts.push(Line::from(Span::styled(
			app.banner.clone().unwrap_or_default(),
			Style::new().fg(ACCENT),
		)));
	}
	frame.render_widget(
		Paragraph::new(alerts).block(panel(" Companions ", WARN)),
		alert,
	);
	let [list_area, detail_area] =
		Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(body);
	let plugins = app
		.plugin_overview
		.as_ref()
		.map(|overview| overview.plugins.as_slice())
		.unwrap_or(&[]);
	let items: Vec<ListItem> = plugins
		.iter()
		.enumerate()
		.map(|(index, plugin)| {
			let marker = if index == app.plugin_cursor { ">" } else { " " };
			let badge = text::badge(app.ui_language, plugin.badge);
			ListItem::new(format!("{marker} {badge} {}", plugin.name))
		})
		.collect();
	let mut state = ListState::default();
	state.select(plugins.is_empty().then_some(0).or(Some(app.plugin_cursor)));
	frame.render_stateful_widget(
		List::new(items)
			.block(panel(" Plugins ", ACCENT))
			.highlight_style(Style::new().fg(ACCENT).bold()),
		list_area,
		&mut state,
	);
	let detail = plugins
		.get(app.plugin_cursor)
		.map(|plugin| plugin_detail(app, plugin))
		.unwrap_or_default();
	frame.render_widget(
		Paragraph::new(detail)
			.wrap(Wrap { trim: false })
			.block(panel(" Detail ", DIM)),
		detail_area,
	);
}

fn plugin_detail(app: &App, plugin: &foch::plugin::PluginEntrySummary) -> Text<'static> {
	let mut lines = vec![
		Line::from(plugin.name.clone()),
		Line::from(Span::styled(plugin.id.clone(), Style::new().fg(DIM))),
		Line::from(text::badge(app.ui_language, plugin.badge).to_string()),
	];
	if let Some(description) = &plugin.description {
		lines.push(Line::from(description.clone()));
	}
	if !plugin.installed_versions.is_empty() {
		lines.push(Line::from(format!(
			"installed {}",
			plugin.installed_versions.join(", ")
		)));
	}
	if let Some(spec) = &plugin.install_spec {
		lines.push(Line::from(Span::styled(spec.clone(), Style::new().fg(DIM))));
	}
	for (id, state) in &app.runtime_states {
		if id == &plugin.id {
			lines.push(Line::from(Span::styled(
				format!("runtime {state}"),
				Style::new().fg(OK),
			)));
		}
	}
	for field in &plugin.config_fields {
		lines.push(Line::from(format!("{} = {}", field.key, field.value)));
	}
	if let Some(plan) = app
		.plugin_overview
		.as_ref()
		.and_then(|overview| overview.plan.as_ref())
	{
		lines.push(Line::from(if plan.launchable {
			"plan: launchable".to_string()
		} else {
			format!(
				"plan: {}",
				plan.errors
					.first()
					.cloned()
					.unwrap_or_else(|| "blocked".into())
			)
		}));
	}
	Text::from(lines)
}

fn draw_settings(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let rows = [
		(
			text::transport_label(app.ui_language),
			text::transport_value(app.ui_language, app.preferences.transport),
		),
		(
			text::mirror_label(app.ui_language),
			text::mirror_value(app.ui_language, app.preferences.mirror),
		),
		(
			text::language_label(app.ui_language),
			text::language_value(app.ui_language, app.preferences.language),
		),
	];
	let mut lines: Vec<Line> = rows
		.iter()
		.enumerate()
		.map(|(index, (label, value))| {
			let style = if index == app.settings_cursor {
				Style::new().fg(Color::Black).bg(ACCENT).bold()
			} else {
				Style::new()
			};
			Line::from(Span::styled(format!("  {label:<22} {value}"), style))
		})
		.collect();
	if let Some(report) = &app.connectivity {
		lines.push(Line::from(""));
		lines.push(Line::from(text::sha256_reminder(app.ui_language)));
		for route in &report.routes {
			let style = if route.reachable {
				Style::new().fg(OK)
			} else {
				Style::new().fg(BAD)
			};
			lines.push(Line::from(Span::styled(
				text::route_line(
					app.ui_language,
					&route.label,
					route.reachable,
					route.http_status,
					route.latency_ms,
					&route.detail,
				),
				style,
			)));
		}
	}
	frame.render_widget(
		Paragraph::new(lines).block(panel(
			match app.ui_language {
				foch::plugin::UiLanguage::ZhCn => " 网络与设置 ",
				foch::plugin::UiLanguage::En => " Network & Settings ",
			},
			ACCENT,
		)),
		area,
	);
}

fn draw_config_editor(frame: &mut Frame<'_>, app: &App) {
	let Some(editor) = &app.config_editor else {
		return;
	};
	let lines: Vec<Line> = editor
		.fields
		.iter()
		.enumerate()
		.map(|(index, field)| {
			let marker = if index == editor.cursor { ">" } else { " " };
			let bounds = match (field.min, field.max) {
				(Some(min), Some(max)) => format!("  {min}..={max}"),
				_ => String::new(),
			};
			Line::from(format!(
				"{marker} {} ({}) = {}{bounds}",
				field.key, field.kind, field.value
			))
		})
		.collect();
	let height = lines.len() as u16 + 4;
	let popup = centered(frame.area(), 72, height.max(6));
	frame.render_widget(Clear, popup);
	frame.render_widget(
		Paragraph::new(lines).block(panel(" Config ", ACCENT)),
		popup,
	);
}

fn draw_install_input(frame: &mut Frame<'_>, app: &App) {
	let spec = app.install_input.clone().unwrap_or_default();
	let popup = centered(frame.area(), 72, 5);
	frame.render_widget(Clear, popup);
	frame.render_widget(
		Paragraph::new(format!("owner/repo  {spec}_")).block(panel(" Install ", ACCENT)),
		popup,
	);
}

fn draw_launch_prompt(frame: &mut Frame<'_>, _app: &App) {
	let popup = centered(frame.area(), 64, 6);
	frame.render_widget(Clear, popup);
	frame.render_widget(
		Paragraph::new(vec![
			Line::from("Launch EU4 with the isolated runtime layer?"),
			Line::from(""),
			Line::from("y launch    Esc cancel"),
		])
		.block(panel(" Launch ", WARN)),
		popup,
	);
}

fn footer_keys(app: &App) -> Line<'static> {
	if app.top_tab == TopTab::LaunchPlugins {
		return footer_pairs(if app.config_editor.is_some() {
			&[
				("↑↓", "field"),
				("Space", "bool"),
				("←→", "int"),
				("Esc", "close"),
			]
		} else if app.confirming_launch {
			&[("y", "launch EU4"), ("Esc", "cancel")]
		} else {
			&[
				("↑↓", "plugin"),
				("Space", "enable"),
				("i", "install"),
				("c", "config"),
				("p", "prepare"),
				("L", "launch"),
				("r", "refresh"),
				("q", "quit"),
			]
		});
	}
	if app.top_tab == TopTab::NetworkSettings {
		let test = text::connectivity_action(app.ui_language);
		return match app.ui_language {
			foch::plugin::UiLanguage::ZhCn => footer_pairs(&[
				("↑↓", "设置"),
				("←→", "更改"),
				("t", test),
				("1-3", "页"),
				("q", "退出"),
			]),
			foch::plugin::UiLanguage::En => footer_pairs(&[
				("↑↓", "setting"),
				("←→", "change"),
				("t", test),
				("1-3", "tab"),
				("q", "quit"),
			]),
		};
	}
	let resolution_action = text::resolution_footer_action(app.ui_language);
	let review_pairs = [
		("↑↓", "move"),
		("Tab", "panel"),
		("0-6", "filter"),
		("/", "search"),
		("c", "copy"),
		("t", resolution_action),
		("i", "playset"),
		("r", "refresh"),
		("?", "keys"),
		("q", "quit"),
	];
	let pairs: &[(&str, &str)] = match (&app.phase, app.screen, app.focus) {
		_ if app.help => &[("any key", "close")],
		_ if app.confirming_exclusions => &[("y", "analyze without them"), ("Esc", "cancel")],
		_ if app.confirming_build => &[("y", "build"), ("Esc", "cancel")],
		(Phase::Building { .. }, _, _) => &[("q", "quit")],
		_ if app.options.is_some() => &[("Space", "toggle"), ("Esc", "close")],
		(Phase::Inspecting, _, _) => &[("?", "keys"), ("q", "quit")],
		(Phase::Analyzing { .. }, _, _) => &[("?", "keys"), ("q", "cancel and quit")],
		(_, _, Focus::Search) => &[
			("type", "filter paths"),
			("Enter", "keep"),
			("Esc", "clear"),
		],
		(Phase::Reviewed(_), Screen::Review, _) => &review_pairs,
		_ if app.can_analyze() => &[
			("a", "analyze"),
			("x", "exclude"),
			("R", "repair"),
			("Tab", "panel"),
			("c", "copy"),
			("r", "refresh"),
			("?", "keys"),
			("q", "quit"),
		],
		_ if app.selectable => &[
			("X", "exclude broken"),
			("R", "repair all"),
			("x", "exclude"),
			("Tab", "panel"),
			("c", "copy"),
			("r", "refresh"),
			("?", "keys"),
			("q", "quit"),
		],
		_ => &[
			("R", "repair"),
			("Tab", "panel"),
			("c", "copy"),
			("r", "refresh"),
			("?", "keys"),
			("q", "quit"),
		],
	};
	let mut pairs = pairs.to_vec();
	let offers_build = matches!(
		app.phase,
		Phase::Inspected | Phase::Reviewed(_) | Phase::Failed(_)
	) && !app.game_base_available
		&& app.options.is_none()
		&& !app.confirming_exclusions
		&& !app.confirming_build
		&& !app.help
		&& app.focus != Focus::Search
		&& app.data_build_args().is_some();
	if offers_build {
		pairs.insert(0, ("B", "build base data"));
	}
	let mut spans = vec![Span::raw(" ")];
	for (name, action) in pairs {
		spans.push(key(name));
		spans.push(Span::styled(format!(" {action}  "), Style::new().fg(DIM)));
	}
	if !app.mouse_capture {
		spans.push(Span::styled("mouse released (m)", Style::new().fg(WARN)));
	}
	Line::from(spans)
}

fn panel(title: impl Into<Line<'static>>, color: Color) -> Block<'static> {
	Block::new()
		.borders(Borders::ALL)
		.border_type(BorderType::Rounded)
		.border_style(Style::new().fg(color))
		.title(title)
		.title_style(Style::new().fg(color).bold())
}

/// A panel that takes the keyboard when focused: thick accent border, and a
/// `▸` before the title.
fn focus_panel(title: impl Into<String>, color: Color, focused: bool) -> Block<'static> {
	let title = title.into();
	if focused {
		panel(format!(" ▸{title}"), ACCENT).border_type(BorderType::Thick)
	} else {
		panel(title, color)
	}
}

/// Remember where this frame drew a panel, for mouse handling.
fn record(app: &App, update: impl FnOnce(&mut HitAreas)) {
	let mut hit = app.hit.get();
	update(&mut hit);
	app.hit.set(hit);
}

fn key(name: &str) -> Span<'static> {
	Span::styled(
		format!(" {name} "),
		Style::new().fg(Color::Black).bg(ACCENT).bold(),
	)
}

fn chip(text: String, color: Color, active: bool) -> Span<'static> {
	if active {
		Span::styled(
			format!(" {text} "),
			Style::new().fg(Color::Black).bg(color).bold(),
		)
	} else {
		Span::styled(format!(" {text} "), Style::new().fg(color))
	}
}

fn badge(text: &str, color: Color) -> Span<'static> {
	Span::styled(format!("● {text}"), Style::new().fg(color).bold())
}

fn field(name: &'static str) -> Span<'static> {
	Span::styled(format!("{name:<11}"), Style::new().fg(DIM))
}

fn push_section(lines: &mut Vec<Line<'static>>, text: &'static str) {
	lines.push(Line::from(""));
	lines.push(Line::from(Span::styled(
		format!("── {text} "),
		Style::new().fg(ACCENT).bold(),
	)));
}

fn source_line(path: &str) -> Line<'static> {
	Line::from(Span::styled(format!("       {path}"), Style::new().fg(DIM)))
}

/// Text for display, without Windows' verbatim `\\?\` path prefix.
fn clean(text: &str) -> String {
	text.replace(r"\\?\", "")
}

/// A path for display, without Windows' verbatim `\\?\` prefix.
fn display(path: &std::path::Path) -> String {
	let text = path.display().to_string();
	text.strip_prefix(r"\\?\")
		.map_or(text.clone(), str::to_string)
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
	let width = width.min(area.width);
	let height = height.min(area.height);
	Rect::new(
		area.x + (area.width - width) / 2,
		area.y + (area.height - height) / 2,
		width,
		height,
	)
}

fn short_disposition(disposition: MergeDisposition) -> &'static str {
	match disposition {
		MergeDisposition::Safe => "SAFE",
		MergeDisposition::Copy => "COPY",
		MergeDisposition::NeedsUserChoice => "CHOOSE",
		MergeDisposition::UnsupportedInput => "UNSUP",
		MergeDisposition::EngineFailure => "FAIL",
		MergeDisposition::Deferred => "DEFER",
	}
}

fn disposition_color(disposition: MergeDisposition) -> Color {
	match disposition {
		MergeDisposition::Safe => OK,
		MergeDisposition::Copy => Color::Blue,
		MergeDisposition::NeedsUserChoice => WARN,
		MergeDisposition::UnsupportedInput => Color::Magenta,
		MergeDisposition::EngineFailure => BAD,
		MergeDisposition::Deferred => DIM,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use ratatui::buffer::Buffer;

	#[test]
	fn a_drag_copies_wide_characters_once_without_panel_borders() {
		let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 2));
		buffer.set_string(0, 0, "│欧陆扩展 ok      │", Style::new());
		buffer.set_string(0, 1, "│second line      │", Style::new());

		let all = buffer.area;
		let text = selected_text((Position::new(0, 0), Position::new(19, 1)), all, &buffer);
		assert_eq!(text, "欧陆扩展 ok\nsecond line");
		let partial = selected_text((Position::new(3, 0), Position::new(5, 1)), all, &buffer);
		assert_eq!(partial, "陆扩展 ok\nsecon");
	}

	/// A selection confined to one panel copies only that panel's columns,
	/// row by row, even where a neighbouring panel shares the rows.
	#[test]
	fn a_drag_copies_only_the_panel_it_started_in() {
		let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 2));
		buffer.set_string(0, 0, "│left one  ││right one│", Style::new());
		buffer.set_string(0, 1, "│left two  ││right two│", Style::new());

		let left = Rect::new(1, 0, 10, 2);
		let text = selected_text((Position::new(1, 0), Position::new(10, 1)), left, &buffer);
		assert_eq!(text, "left one\nleft two");
	}
}
