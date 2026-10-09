use foch::input::{BaseDataState, CurrentEu4Input, InputReadiness};
use foch::merge::{MergeAnalysisStatus, MergeDisposition, MergeUnitKind, MergeUnitOutcome};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};

use super::app::{App, DISPOSITIONS, Focus, OPTION_COUNT, Phase, Screen, disposition_label};

const TITLE: &str = " foch · EU4 analysis browser (read-only) ";

pub fn draw(frame: &mut Frame<'_>, app: &App) {
	let [header, body, footer] = Layout::vertical([
		Constraint::Length(6),
		Constraint::Min(5),
		Constraint::Length(1),
	])
	.areas(frame.area());
	draw_header(frame, app, header);
	match (&app.phase, app.screen) {
		(Phase::Analyzing { started, progress }, _) => {
			let elapsed = progress
				.map(|progress| progress.elapsed)
				.unwrap_or_else(|| started.elapsed());
			draw_progress(frame, body, *progress, elapsed);
		}
		(Phase::Reviewed(_), Screen::Review) => draw_review(frame, app, body),
		_ => draw_input(frame, app, body),
	}
	frame.render_widget(
		Paragraph::new(footer_text(app)).style(Style::new().fg(Color::DarkGray)),
		footer,
	);
	if let Some(cursor) = app.options {
		draw_options(frame, app, cursor);
	}
	if app.confirming_omissions {
		draw_omission_prompt(frame, app);
	}
}

fn draw_omission_prompt(frame: &mut Frame<'_>, app: &App) {
	let Some(recovery) = app.input.as_ref().and_then(|input| input.recovery.as_ref()) else {
		return;
	};
	let mut lines = vec![
		Line::from(format!(
			" {} of {} playset mods are unavailable. Analyze the other {} without them?",
			recovery.omitted_mods.len(),
			recovery.source_mod_count,
			recovery.included_mod_count
		)),
		Line::from(""),
	];
	for omitted in &recovery.omitted_mods {
		lines.push(Line::from(vec![
			Span::styled(
				format!(" #{} {}", omitted.position, omitted.name),
				Style::new().fg(Color::Yellow),
			),
			Span::styled(
				format!("  {}", omitted.reason),
				Style::new().fg(Color::DarkGray),
			),
		]));
	}
	lines.push(Line::from(""));
	lines.push(Line::from(Span::styled(
		" The analysis will not represent your full playset.  [y] analyze without them  [Esc] cancel",
		Style::new().add_modifier(Modifier::BOLD),
	)));
	let area = frame.area();
	let width = 100.min(area.width);
	let height = (lines.len() as u16 + 2).min(area.height);
	let popup = Rect::new(
		area.x + (area.width - width) / 2,
		area.y + (area.height - height) / 2,
		width,
		height,
	);
	frame.render_widget(Clear, popup);
	frame.render_widget(
		Paragraph::new(lines).wrap(Wrap { trim: false }).block(
			Block::new()
				.borders(Borders::ALL)
				.border_style(Style::new().fg(Color::Yellow))
				.title(" Unavailable mods "),
		),
		popup,
	);
}

fn draw_options(frame: &mut Frame<'_>, app: &App, cursor: usize) {
	let settings = &app.settings;
	let check = |on: bool| if on { "[x]" } else { "[ ]" };
	let game_base = if app.game_base_available {
		format!("{} Use the EU4 base as ancestor", check(settings.game_base))
	} else {
		"[-] Use the EU4 base as ancestor (unavailable for this input)".to_string()
	};
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
			format!("    Workers  ◀ {} ▶", settings.merge_workers),
			"speed only; output is the same",
		),
	];
	let mut lines = rows
		.iter()
		.enumerate()
		.map(|(index, (text, hint))| {
			let style = if index == cursor {
				Style::new().add_modifier(Modifier::REVERSED)
			} else {
				Style::new()
			};
			Line::from(vec![
				Span::styled(format!(" {text:<44}"), style),
				Span::styled(format!(" {hint}"), Style::new().fg(Color::DarkGray)),
			])
		})
		.collect::<Vec<_>>();
	lines.push(Line::from(""));
	lines.push(Line::from(Span::styled(
		" Options apply to the next analysis and are not saved.",
		Style::new().fg(Color::DarkGray),
	)));
	lines.push(Line::from(Span::styled(
		" [↑↓] select  [Space] toggle  [←→] workers  [Esc] close",
		Style::new().fg(Color::DarkGray),
	)));
	let area = frame.area();
	let width = 84.min(area.width);
	let height = (lines.len() as u16 + 2).min(area.height);
	let popup = Rect::new(
		area.x + (area.width - width) / 2,
		area.y + (area.height - height) / 2,
		width,
		height,
	);
	frame.render_widget(Clear, popup);
	frame.render_widget(
		Paragraph::new(lines).block(
			Block::new()
				.borders(Borders::ALL)
				.border_style(Style::new().fg(Color::Cyan))
				.title(" Analysis options "),
		),
		popup,
	);
}

fn draw_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let block = Block::new().borders(Borders::ALL).title(TITLE);
	let lines = match &app.input {
		None => vec![Line::from(
			"Inspecting the installed EU4 game and launcher playset…",
		)],
		Some(input) => header_lines(input, app),
	};
	frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn header_lines(input: &CurrentEu4Input, app: &App) -> Vec<Line<'static>> {
	let game = format!(
		"{} {}  {}",
		input.game.name,
		input.game.version.as_deref().unwrap_or("(unknown version)"),
		input.game.install_path.as_ref().map_or_else(
			|| "(not found)".to_string(),
			|path| path.display().to_string()
		)
	);
	let (base_label, base_color) = match input.base_data.state {
		BaseDataState::Ready => ("ready", Color::Green),
		BaseDataState::Missing => ("missing", Color::Red),
		BaseDataState::Stale => ("stale", Color::Yellow),
	};
	let base = format!(
		" {}  {}",
		input.base_data.version.as_deref().unwrap_or(""),
		input.base_data.detail
	);
	let playset = input.playset.as_ref().map_or_else(
		|| "(no playset detected)".to_string(),
		|playset| {
			format!(
				"{}  {} mods  {}",
				playset.name,
				playset.mods.len(),
				playset.source_path.display()
			)
		},
	);
	let (readiness, readiness_color) = match input.readiness {
		InputReadiness::Ready => ("ready".to_string(), Color::Green),
		InputReadiness::ReadyWithOmissions => (
			format!(
				"ready without {} unavailable mods",
				input
					.recovery
					.as_ref()
					.map_or(0, |recovery| recovery.omitted_mods.len())
			),
			Color::Yellow,
		),
		InputReadiness::Blocked => ("blocked".to_string(), Color::Red),
	};
	let mut status = vec![
		label("Input   "),
		Span::styled(readiness, Style::new().fg(readiness_color)),
	];
	if let Some(view) = app.analysis() {
		let (text, color) = match view.status {
			MergeAnalysisStatus::ReadyToCommit => ("ready to commit", Color::Green),
			MergeAnalysisStatus::CommittableWithDeferrals => {
				("committable with deferrals", Color::Yellow)
			}
			MergeAnalysisStatus::Blocked => ("blocked", Color::Red),
		};
		status.push(Span::raw("   "));
		status.push(label("Analysis "));
		status.push(Span::styled(text, Style::new().fg(color)));
	}
	if app.settings_changed() {
		status.push(Span::styled(
			"   options changed: [r] re-analyzes",
			Style::new().fg(Color::Yellow),
		));
	}
	vec![
		Line::from(vec![label("Game    "), Span::raw(game)]),
		Line::from(vec![
			label("Base    "),
			Span::styled(base_label, Style::new().fg(base_color)),
			Span::raw(base),
		]),
		Line::from(vec![label("Playset "), Span::raw(playset)]),
		Line::from(status),
	]
}

fn draw_input(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let Some(input) = &app.input else {
		let text = match &app.phase {
			Phase::Failed(error) => {
				Line::from(Span::styled(error.clone(), Style::new().fg(Color::Red)))
			}
			_ => Line::from(""),
		};
		frame.render_widget(
			Paragraph::new(text)
				.wrap(Wrap { trim: false })
				.block(Block::new().borders(Borders::ALL)),
			area,
		);
		return;
	};
	let mut notices = Vec::new();
	if let Phase::Failed(error) = &app.phase {
		notices.push(Line::from(Span::styled(
			format!("Analysis failed: {error}"),
			Style::new().fg(Color::Red),
		)));
	}
	for issue in &input.issues {
		notices.push(Line::from(vec![
			Span::styled(issue.title.clone(), Style::new().fg(Color::Yellow)),
			Span::raw(format!(": {}", issue.detail)),
		]));
		if let Some(action) = &issue.action {
			notices.push(Line::from(format!("  → {action}")));
		}
	}
	if let Some(recovery) = &input.recovery {
		for omitted in &recovery.omitted_mods {
			notices.push(Line::from(format!(
				"omitted #{} {}: {}",
				omitted.position, omitted.name, omitted.reason
			)));
		}
	}
	let notice_height = if notices.is_empty() {
		0
	} else {
		(notices.len() as u16 + 2).min(area.height / 3)
	};
	let [mods_area, notices_area] =
		Layout::vertical([Constraint::Min(3), Constraint::Length(notice_height)]).areas(area);

	let mods = input
		.playset
		.as_ref()
		.map_or(&[][..], |playset| &playset.mods);
	let items = mods
		.iter()
		.map(|playset_mod| {
			let mut spans = vec![
				Span::styled(
					format!("{:>3} ", playset_mod.position),
					Style::new().fg(Color::DarkGray),
				),
				Span::raw(playset_mod.name.clone()),
				Span::styled(
					format!(
						"  {}{}",
						playset_mod.id,
						playset_mod
							.version
							.as_deref()
							.map(|version| format!("  v{version}"))
							.unwrap_or_default()
					),
					Style::new().fg(Color::DarkGray),
				),
			];
			if !playset_mod.enabled {
				spans.push(Span::styled("  disabled", Style::new().fg(Color::DarkGray)));
			}
			if let Some(error) = &playset_mod.source_error {
				spans.push(Span::styled(
					format!("  {error}"),
					Style::new().fg(Color::Red),
				));
			}
			let mut lines = vec![Line::from(spans)];
			if !playset_mod.declared_dependencies.is_empty() {
				lines.push(Line::from(Span::styled(
					format!(
						"      depends on: {}",
						playset_mod.declared_dependencies.join(", ")
					),
					Style::new().fg(Color::Cyan),
				)));
			}
			ListItem::new(lines)
		})
		.collect::<Vec<_>>();
	let mut state = ListState::default().with_selected(Some(app.mod_scroll));
	frame.render_stateful_widget(
		List::new(items)
			.block(
				Block::new()
					.borders(Borders::ALL)
					.title(" Ordered mods (load order, last wins) "),
			)
			.highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
		mods_area,
		&mut state,
	);
	if notice_height > 0 {
		frame.render_widget(
			Paragraph::new(notices)
				.wrap(Wrap { trim: false })
				.block(Block::new().borders(Borders::ALL).title(" Notices ")),
			notices_area,
		);
	}
}

fn draw_progress(
	frame: &mut Frame<'_>,
	area: Rect,
	progress: Option<foch::merge::MergeProgress>,
	elapsed: std::time::Duration,
) {
	let block = Block::new()
		.borders(Borders::ALL)
		.title(" Analyzing every contributor ");
	let (ratio, label) = match progress {
		Some(progress) => {
			let stage = format!("{:?}", progress.stage);
			match (progress.completed_units, progress.total_units) {
				(Some(done), Some(total)) if total > 0 => (
					(done as f64 / total as f64).clamp(0.0, 1.0),
					format!("{stage}: {done}/{total} units"),
				),
				_ => (0.0, stage),
			}
		}
		None => (0.0, "starting".to_string()),
	};
	let [gauge_area, _, note_area] = Layout::vertical([
		Constraint::Length(3),
		Constraint::Length(1),
		Constraint::Min(1),
	])
	.areas(block.inner(area));
	frame.render_widget(block, area);
	frame.render_widget(
		Gauge::default()
			.gauge_style(Style::new().fg(Color::Cyan))
			.ratio(ratio)
			.label(format!("{label}  ({}s)", elapsed.as_secs())),
		gauge_area,
	);
	frame.render_widget(
		Paragraph::new(
			"The analysis runs to completion over the frozen input. Nothing is written.",
		)
		.style(Style::new().fg(Color::DarkGray)),
		note_area,
	);
}

fn draw_review(frame: &mut Frame<'_>, app: &App, area: Rect) {
	let Some(view) = app.analysis() else {
		return;
	};
	let [summary_area, main] =
		Layout::vertical([Constraint::Length(4), Constraint::Min(3)]).areas(area);

	let mut spans = vec![Span::styled(
		format!("[0] all {}", view.summary.total),
		filter_style(app.filter.is_none(), Color::White),
	)];
	for (index, disposition) in DISPOSITIONS.iter().enumerate() {
		spans.push(Span::raw("  "));
		spans.push(Span::styled(
			format!(
				"[{}] {} {}",
				index + 1,
				disposition_label(*disposition),
				view.count(*disposition)
			),
			filter_style(
				app.filter == Some(*disposition),
				disposition_color(*disposition),
			),
		));
	}
	let search_title = match (app.focus, app.query.is_empty()) {
		(Focus::Search, _) => format!(" Summary · search: {}▏ ", app.query),
		(_, false) => format!(" Summary · search: {} ", app.query),
		(_, true) => " Summary ".to_string(),
	};
	frame.render_widget(
		Paragraph::new(Line::from(spans))
			.wrap(Wrap { trim: true })
			.block(Block::new().borders(Borders::ALL).title(search_title)),
		summary_area,
	);

	let [list_area, detail_area] =
		Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(main);
	let visible = app.visible_units();
	let items = visible
		.iter()
		.map(|index| {
			let unit = &view.units[*index];
			ListItem::new(Line::from(vec![
				Span::styled(
					format!("{:<6} ", short_disposition(unit.disposition)),
					Style::new().fg(disposition_color(unit.disposition)),
				),
				Span::raw(unit.path.as_str().to_owned()),
			]))
		})
		.collect::<Vec<_>>();
	let selected = (!visible.is_empty()).then_some(app.selected);
	let mut state = ListState::default().with_selected(selected);
	frame.render_stateful_widget(
		List::new(items)
			.block(focus_block(
				format!(" Units {}/{} ", visible.len(), view.units.len()),
				app.focus == Focus::Units,
			))
			.highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
		list_area,
		&mut state,
	);

	let detail = app
		.selected_unit()
		.map_or_else(|| Text::from("No unit matches this filter."), unit_detail);
	frame.render_widget(
		Paragraph::new(detail)
			.wrap(Wrap { trim: false })
			.scroll((app.detail_scroll, 0))
			.block(focus_block(
				" Detail ".to_string(),
				app.focus == Focus::Detail,
			)),
		detail_area,
	);
}

/// Everything the analysis knows about one unit: the vanilla ancestor, every
/// contributor in precedence order with its source files, and the outputs
/// the analysis would commit.
pub fn unit_detail(unit: &MergeUnitOutcome) -> Text<'static> {
	let mut lines = vec![
		Line::from(vec![label("Unit        "), Span::raw(unit.id.clone())]),
		Line::from(vec![
			label("Family      "),
			Span::raw(format!(
				"{} ({})",
				unit.family,
				match unit.kind {
					MergeUnitKind::File => "file",
					MergeUnitKind::DefinitionModule => "definition module",
				}
			)),
		]),
		Line::from(vec![
			label("Disposition "),
			Span::styled(
				disposition_label(unit.disposition),
				Style::new()
					.fg(disposition_color(unit.disposition))
					.add_modifier(Modifier::BOLD),
			),
		]),
		Line::from(vec![
			label("Strategy    "),
			Span::raw(unit.strategy.clone()),
		]),
	];
	if !unit.summary.is_empty() {
		lines.push(Line::from(""));
		lines.push(Line::from(unit.summary.clone()));
	}

	lines.push(Line::from(""));
	lines.push(section("Ancestor"));
	let ancestors = unit
		.contributors
		.iter()
		.filter(|contributor| contributor.is_base_game)
		.collect::<Vec<_>>();
	if ancestors.is_empty() {
		lines.push(Line::from(Span::styled(
			"  none: no vanilla definition at this unit",
			Style::new().fg(Color::DarkGray),
		)));
	}
	for ancestor in ancestors {
		lines.push(Line::from(format!("  {}", ancestor.name)));
		for path in &ancestor.source_paths {
			lines.push(source_line(path));
		}
	}

	lines.push(Line::from(""));
	lines.push(section("Contributors (precedence order)"));
	let mut contributors = unit
		.contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game)
		.collect::<Vec<_>>();
	contributors.sort_by_key(|contributor| contributor.precedence);
	for contributor in contributors {
		lines.push(Line::from(vec![
			Span::styled(
				format!("  #{} ", contributor.precedence),
				Style::new().fg(Color::DarkGray),
			),
			Span::raw(contributor.name.clone()),
			Span::styled(
				format!("  {}", contributor.mod_id),
				Style::new().fg(Color::DarkGray),
			),
		]));
		for path in &contributor.source_paths {
			lines.push(source_line(path));
		}
	}

	lines.push(Line::from(""));
	lines.push(section("Result"));
	if unit.output_paths.is_empty() {
		lines.push(Line::from(Span::styled(
			"  no output: this unit would be deferred",
			Style::new().fg(Color::DarkGray),
		)));
	}
	for path in &unit.output_paths {
		lines.push(Line::from(format!("  → {}", path.as_str())));
	}

	if !unit.notes.is_empty() {
		lines.push(Line::from(""));
		lines.push(section("Notes"));
		for note in &unit.notes {
			lines.push(Line::from(format!("  {note}")));
		}
	}
	Text::from(lines)
}

fn footer_text(app: &App) -> String {
	let keys = match (&app.phase, app.screen, app.focus) {
		(Phase::Inspecting | Phase::Analyzing { .. }, _, _) => "[o] options  [q] quit",
		(_, _, Focus::Search) => "type to filter units  [Enter] keep  [Esc] clear",
		(Phase::Reviewed(_), Screen::Review, Focus::Detail) => {
			"[↑↓] scroll  [Esc] units  [0-6] filter  [i] input  [r] refresh  [o] options  [q] quit"
		}
		(Phase::Reviewed(_), Screen::Review, _) => {
			"[↑↓] select  [Enter] detail  [0-6] filter  [/] search  [i] input  [r] refresh  [o] options  [q] quit"
		}
		(Phase::Reviewed(_), Screen::Input, _) => {
			"[↑↓] scroll  [i] review  [r] refresh  [o] options  [q] quit"
		}
		_ if app.can_analyze => "[a] analyze  [↑↓] scroll  [r] refresh  [o] options  [q] quit",
		_ => "[↑↓] scroll  [r] refresh  [o] options  [q] quit",
	};
	format!(" {keys}")
}

fn focus_block(title: String, focused: bool) -> Block<'static> {
	let style = if focused {
		Style::new().fg(Color::Cyan)
	} else {
		Style::new()
	};
	Block::new()
		.borders(Borders::ALL)
		.border_style(style)
		.title(title)
}

fn filter_style(active: bool, color: Color) -> Style {
	let style = Style::new().fg(color);
	if active {
		style.add_modifier(Modifier::REVERSED | Modifier::BOLD)
	} else {
		style
	}
}

fn label(text: &'static str) -> Span<'static> {
	Span::styled(text, Style::new().add_modifier(Modifier::BOLD))
}

fn section(text: &'static str) -> Line<'static> {
	Line::from(Span::styled(
		text,
		Style::new()
			.add_modifier(Modifier::BOLD)
			.add_modifier(Modifier::UNDERLINED),
	))
}

fn source_line(path: &str) -> Line<'static> {
	Line::from(Span::styled(
		format!("      {path}"),
		Style::new().fg(Color::DarkGray),
	))
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
		MergeDisposition::Safe => Color::Green,
		MergeDisposition::Copy => Color::Blue,
		MergeDisposition::NeedsUserChoice => Color::Yellow,
		MergeDisposition::UnsupportedInput => Color::Magenta,
		MergeDisposition::EngineFailure => Color::Red,
		MergeDisposition::Deferred => Color::DarkGray,
	}
}
