use std::io::{self, IsTerminal, Stdout};
use std::path::PathBuf;

use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
	EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Widget, Wrap};

use foch_engine::{ConflictDecision, ConflictHandler, ConflictView};

const ACTION_COUNT: usize = 4;
const MAX_RENDERED_SUMMARY_LINES: usize = 4;
const MAX_SNIPPET_LINES: usize = 8;
const MAX_SNIPPET_LINE_CHARS: usize = 80;
const MAX_SNIPPET_SECTION_HEIGHT: u16 = 10;
const MIN_SNIPPET_SECTION_HEIGHT: u16 = 3;
const NO_VANILLA_SNIPPET: &str = "(no vanilla file at this address)";
/// Cap how many wrapped lines `reason: …` may consume in the conflict header.
/// Long sibling-conflict messages that enumerate every divergent value can run
/// hundreds of characters; capping keeps the candidate list visible on small
/// terminals while still giving short reasons their natural single-line look.
const MAX_REASON_HEIGHT: usize = 4;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ConflictAction {
	PickCandidate(usize),
	Defer,
	ExternalFile(PathBuf),
	KeepExisting,
	Abort,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ConflictCandidate {
	mod_id: String,
	display_name: String,
	precedence: usize,
	summary: Vec<String>,
	projected_snippet: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConflictResolver {
	pub current_conflict_index: usize,
	pub total_conflicts: usize,
	pub deferred_so_far: usize,
	file_path: PathBuf,
	address_path: Vec<String>,
	reason: String,
	conflict_id: String,
	vanilla_snippet: Option<String>,
	candidates: Vec<ConflictCandidate>,
	selected_index: usize,
}

pub struct ConflictResolverArgs<'a> {
	pub view: &'a ConflictView,
	pub current_idx: usize,
	pub total: usize,
	pub deferred_so_far: usize,
}

impl ConflictResolver {
	pub fn new(args: ConflictResolverArgs<'_>) -> Self {
		let ConflictResolverArgs {
			view,
			current_idx,
			total,
			deferred_so_far,
		} = args;
		let candidates: Vec<ConflictCandidate> = view
			.candidates
			.iter()
			.map(|candidate| ConflictCandidate {
				mod_id: candidate.mod_id.clone(),
				display_name: candidate.mod_display_name.clone(),
				precedence: candidate.precedence,
				summary: candidate.patch_summary.clone(),
				projected_snippet: truncate_rendered_snippet(
					&candidate.patch_rendered,
					MAX_SNIPPET_LINES,
					MAX_SNIPPET_LINE_CHARS,
				),
			})
			.collect();
		let mut address_path = view.address_path.clone();
		if !view.address_key.is_empty() {
			address_path.push(view.address_key.clone());
		}
		// Default the cursor to "Defer" (the first action right after the
		// candidate list) so an accidental Enter doesn't pick an arbitrary
		// mod's patch. Users can still arrow up to candidates explicitly.
		let default_selected = candidates.len();
		Self {
			current_conflict_index: current_idx,
			total_conflicts: total,
			deferred_so_far,
			file_path: view.file_path.clone(),
			address_path,
			reason: view.reason.clone(),
			conflict_id: view.conflict_id.clone(),
			vanilla_snippet: view.vanilla_snippet.clone().map(|snippet| {
				truncate_rendered_snippet(&snippet, MAX_SNIPPET_LINES, MAX_SNIPPET_LINE_CHARS)
			}),
			candidates,
			selected_index: default_selected,
		}
	}

	pub fn render(&self, frame: &mut Frame<'_>, area: Rect) {
		frame.render_widget(self, area);
	}

	pub fn handle_key(&mut self, key: KeyEvent) -> Option<ConflictAction> {
		match key.code {
			KeyCode::Up => {
				self.selected_index = self.selected_index.saturating_sub(1);
				None
			}
			KeyCode::Down => {
				let last = self.item_count().saturating_sub(1);
				self.selected_index = self.selected_index.saturating_add(1).min(last);
				None
			}
			KeyCode::Home => {
				self.selected_index = 0;
				None
			}
			KeyCode::End => {
				self.selected_index = self.item_count().saturating_sub(1);
				None
			}
			KeyCode::Enter => Some(self.action_for_index(self.selected_index)),
			KeyCode::Esc => Some(ConflictAction::Defer),
			KeyCode::Char(value) => self.handle_char(value),
			_ => None,
		}
	}

	fn handle_char(&mut self, value: char) -> Option<ConflictAction> {
		match value {
			'q' | 'Q' => Some(ConflictAction::Abort),
			'd' | 'D' => Some(ConflictAction::Defer),
			's' | 'S' => Some(ConflictAction::ExternalFile(PathBuf::new())),
			'k' | 'K' => Some(ConflictAction::KeepExisting),
			'1'..='9' => {
				let index = value.to_digit(10).expect("digit matched") as usize - 1;
				(index < self.candidates.len()).then_some(ConflictAction::PickCandidate(index))
			}
			_ => None,
		}
	}

	fn item_count(&self) -> usize {
		self.candidates.len() + ACTION_COUNT
	}

	fn action_for_index(&self, index: usize) -> ConflictAction {
		if index < self.candidates.len() {
			return ConflictAction::PickCandidate(index);
		}
		match index - self.candidates.len() {
			0 => ConflictAction::Defer,
			1 => ConflictAction::ExternalFile(PathBuf::new()),
			2 => ConflictAction::KeepExisting,
			_ => ConflictAction::Abort,
		}
	}
}

impl Widget for &ConflictResolver {
	fn render(self, area: Rect, buf: &mut Buffer) {
		Clear.render(area, buf);
		let title = format!(
			" foch merge: conflict {}/{} ",
			self.current_conflict_index, self.total_conflicts
		);
		Block::default()
			.title(title)
			.borders(Borders::ALL)
			.render(area, buf);
		if area.width < 4 || area.height < 4 {
			return;
		}

		let inner = Rect {
			x: area.x + 1,
			y: area.y + 1,
			width: area.width.saturating_sub(2),
			height: area.height.saturating_sub(2),
		};
		let hint_y = inner.y + inner.height.saturating_sub(1);
		let bottom_separator_y = hint_y.saturating_sub(1);

		// Header rows: progress gauge (1 row), file path (1 row), address
		// path (1 row), blank (1 row), reason (variable rows — word-wrapped
		// so long messages don't get truncated by the right border). Compute
		// reason height from the available inner width with a hard cap so the
		// choice list always has at least a couple of rows on small terminals.
		let reason_text = format!("reason: {}", self.reason);
		let reason_paragraph = Paragraph::new(Text::raw(reason_text.clone()))
			.style(Style::default().fg(Color::Yellow))
			.wrap(Wrap { trim: false });
		let reason_height =
			estimate_wrapped_line_count(&reason_text, inner.width).clamp(1, MAX_REASON_HEIGHT);
		let header_height = 4u16.saturating_add(reason_height as u16);
		let top_separator_y = inner
			.y
			.saturating_add(header_height)
			.min(bottom_separator_y);

		let gauge_rect = Rect {
			x: inner.x,
			y: inner.y,
			width: inner.width,
			height: 1,
		};
		render_progress_gauge(
			gauge_rect,
			buf,
			self.current_conflict_index,
			self.total_conflicts,
			self.deferred_so_far,
		);
		write_line(
			buf,
			inner,
			inner.y.saturating_add(1),
			&self.file_path.to_string_lossy(),
			Style::default(),
		);
		write_line(
			buf,
			inner,
			inner.y.saturating_add(2),
			&format!("  {}", self.address_path.join(" / ")),
			Style::default().fg(Color::Cyan),
		);
		// reason starts at inner.y + 4, claims `reason_height` rows of the
		// header band so the rest of the layout flows below it.
		let reason_y = inner.y.saturating_add(4);
		let reason_rect = Rect {
			x: inner.x,
			y: reason_y,
			width: inner.width,
			height: top_separator_y.saturating_sub(reason_y),
		};
		if reason_rect.height > 0 {
			reason_paragraph.render(reason_rect, buf);
		}
		draw_separator(buf, area, top_separator_y);

		if bottom_separator_y > top_separator_y {
			let list_area = Rect {
				x: inner.x,
				y: top_separator_y + 1,
				width: inner.width,
				height: bottom_separator_y.saturating_sub(top_separator_y + 1),
			};
			render_context_sections_and_actions(self, buf, list_area);
		}

		draw_separator(buf, area, bottom_separator_y);
		write_line(
			buf,
			inner,
			hint_y,
			"↑↓ select  Enter confirm  Esc/d defer  Q abort  S file  K keep",
			Style::default().fg(Color::DarkGray),
		);
	}
}

pub struct InteractiveTuiHandler {
	current_conflict_index: usize,
	total_conflicts: usize,
	deferred_so_far: usize,
}

impl InteractiveTuiHandler {
	pub fn new() -> Self {
		Self {
			current_conflict_index: 1,
			total_conflicts: 1,
			deferred_so_far: 0,
		}
	}

	fn stdin_stdout_are_tty(&self) -> bool {
		io::stdin().is_terminal() && io::stdout().is_terminal()
	}
}

impl Default for InteractiveTuiHandler {
	fn default() -> Self {
		Self::new()
	}
}

impl ConflictHandler for InteractiveTuiHandler {
	fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
		if !self.stdin_stdout_are_tty() {
			eprintln!(
				"[foch] interactive TUI could not be entered because stdin/stdout is not a TTY; downgrading to defer"
			);
			return ConflictDecision::Defer { record: None };
		}

		let mut resolver = ConflictResolver::new(ConflictResolverArgs {
			view,
			current_idx: self.current_conflict_index,
			total: self.total_conflicts,
			deferred_so_far: self.deferred_so_far,
		});

		let action = match run_resolver(&mut resolver) {
			Ok(action) => action,
			Err(err) => {
				eprintln!("[foch] interactive TUI failed: {err}; downgrading to defer");
				return ConflictDecision::Defer { record: None };
			}
		};

		match action {
			ConflictAction::PickCandidate(index) => view
				.candidates
				.get(index)
				.map(|candidate| ConflictDecision::PickMod {
					mod_id: candidate.mod_id.clone(),
					record: None,
				})
				.unwrap_or(ConflictDecision::Defer { record: None }),
			ConflictAction::Defer => ConflictDecision::Defer { record: None },
			ConflictAction::ExternalFile(path) => {
				if path.as_os_str().is_empty() {
					ConflictDecision::Defer { record: None }
				} else {
					ConflictDecision::UseFile(path)
				}
			}
			ConflictAction::KeepExisting => ConflictDecision::KeepExisting,
			ConflictAction::Abort => ConflictDecision::Abort,
		}
	}

	fn set_conflict_progress(&mut self, current: usize, total: usize) {
		self.current_conflict_index = current;
		self.total_conflicts = total;
	}

	fn set_deferred_so_far(&mut self, count: usize) {
		self.deferred_so_far = count;
	}
}

fn run_resolver(resolver: &mut ConflictResolver) -> io::Result<ConflictAction> {
	enable_raw_mode()?;
	let _guard = TerminalGuard;
	let mut stdout = io::stdout();
	execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
	let backend = CrosstermBackend::new(stdout);
	let mut terminal = Terminal::new(backend)?;

	// Drain any stale events queued before we entered raw mode (typically a
	// stray Enter left over from the shell-cooked overwrite prompt earlier
	// in the merge command, which would otherwise be read instantly as the
	// first keypress and dismiss the TUI before it's even visible).
	while event::poll(std::time::Duration::ZERO)? {
		let _ = event::read()?;
	}

	loop {
		terminal.draw(|frame| resolver.render(frame, frame.area()))?;
		if let Event::Key(key) = event::read()? {
			let Some(action) = resolver.handle_key(key) else {
				continue;
			};
			if matches!(action, ConflictAction::ExternalFile(ref path) if path.as_os_str().is_empty())
			{
				if let Some(path) = prompt_external_file(&mut terminal, resolver)? {
					return Ok(ConflictAction::ExternalFile(path));
				}
				continue;
			}
			return Ok(action);
		}
	}
}

struct TerminalGuard;

impl Drop for TerminalGuard {
	fn drop(&mut self) {
		let _ = disable_raw_mode();
		let mut stdout = io::stdout();
		let _ = execute!(stdout, cursor::Show, LeaveAlternateScreen);
	}
}

fn prompt_external_file(
	terminal: &mut Terminal<CrosstermBackend<Stdout>>,
	resolver: &ConflictResolver,
) -> io::Result<Option<PathBuf>> {
	let mut input = String::new();
	loop {
		terminal.draw(|frame| {
			resolver.render(frame, frame.area());
			render_external_file_dialog(frame, &input);
		})?;
		if let Event::Key(key) = event::read()? {
			match key.code {
				KeyCode::Enter => {
					let path = input.trim();
					if !path.is_empty() {
						return Ok(Some(PathBuf::from(path)));
					}
				}
				KeyCode::Esc => return Ok(None),
				KeyCode::Backspace => {
					input.pop();
				}
				KeyCode::Char(value)
					if !key
						.modifiers
						.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
				{
					input.push(value);
				}
				_ => {}
			}
		}
	}
}

fn render_external_file_dialog(frame: &mut Frame<'_>, input: &str) {
	let area = centered_rect(70, 30, frame.area());
	frame.render_widget(Clear, area);
	let block = Block::default()
		.title(" external resolution file ")
		.borders(Borders::ALL);
	let chunks = Layout::default()
		.direction(Direction::Vertical)
		.constraints([
			Constraint::Length(1),
			Constraint::Length(1),
			Constraint::Length(1),
		])
		.margin(1)
		.split(area);
	frame.render_widget(block, area);
	frame.render_widget(
		Paragraph::new("Path to a complete replacement file (Esc cancels):"),
		chunks[0],
	);
	frame.render_widget(
		Paragraph::new(input.to_string()).style(Style::default().fg(Color::Cyan)),
		chunks[1],
	);
	frame.render_widget(
		Paragraph::new("Enter confirm  Esc back").style(Style::default().fg(Color::DarkGray)),
		chunks[2],
	);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
	let vertical = Layout::default()
		.direction(Direction::Vertical)
		.constraints([
			Constraint::Percentage((100 - percent_y) / 2),
			Constraint::Percentage(percent_y),
			Constraint::Percentage((100 - percent_y) / 2),
		])
		.split(area);
	let horizontal = Layout::default()
		.direction(Direction::Horizontal)
		.constraints([
			Constraint::Percentage((100 - percent_x) / 2),
			Constraint::Percentage(percent_x),
			Constraint::Percentage((100 - percent_x) / 2),
		])
		.split(vertical[1]);
	horizontal[1]
}

fn render_context_sections_and_actions(resolver: &ConflictResolver, buf: &mut Buffer, area: Rect) {
	if area.height == 0 {
		return;
	}

	let action_height = (ACTION_COUNT as u16 + 1).min(area.height);
	let sections_height = area.height.saturating_sub(action_height);
	if sections_height > 0 {
		let sections_area = Rect {
			x: area.x,
			y: area.y,
			width: area.width,
			height: sections_height,
		};
		render_snippet_sections(resolver, buf, sections_area);
	}

	if action_height > 0 {
		let action_area = Rect {
			x: area.x,
			y: area.y + sections_height,
			width: area.width,
			height: action_height,
		};
		render_action_rows(resolver, buf, action_area);
	}
}

#[derive(Debug)]
struct SnippetSection {
	title: String,
	body: String,
	body_style: Style,
	border_style: Style,
	height: u16,
}

fn render_snippet_sections(resolver: &ConflictResolver, buf: &mut Buffer, area: Rect) {
	if area.height == 0 {
		return;
	}

	let mut sections = Vec::new();
	let mut remaining = area.height;
	let mut hidden_candidates = 0usize;

	if remaining >= MIN_SNIPPET_SECTION_HEIGHT {
		let vanilla_absent = resolver.vanilla_snippet.is_none();
		let body = resolver
			.vanilla_snippet
			.clone()
			.unwrap_or_else(|| NO_VANILLA_SNIPPET.to_string());
		let height = snippet_section_height(&body).min(remaining);
		sections.push(SnippetSection {
			title: "vanilla baseline".to_string(),
			body,
			body_style: if vanilla_absent {
				Style::default().fg(Color::DarkGray)
			} else {
				Style::default()
			},
			border_style: Style::default(),
			height,
		});
		remaining = remaining.saturating_sub(height);
	} else {
		hidden_candidates = resolver.candidates.len();
	}

	if hidden_candidates == 0 {
		for (index, candidate) in resolver.candidates.iter().enumerate() {
			if remaining < MIN_SNIPPET_SECTION_HEIGHT {
				hidden_candidates = resolver.candidates.len().saturating_sub(index);
				break;
			}

			let desired_height = snippet_section_height(&candidate.projected_snippet);
			if index + 1 < resolver.candidates.len() && remaining <= desired_height {
				hidden_candidates = resolver.candidates.len().saturating_sub(index);
				break;
			}

			let selected = resolver.selected_index == index;
			let height = desired_height.min(remaining);
			sections.push(SnippetSection {
				title: format!(
					"[{}] {} ({}, prec {})",
					index + 1,
					candidate.display_name,
					candidate.mod_id,
					candidate.precedence
				),
				body: candidate.projected_snippet.clone(),
				body_style: Style::default(),
				border_style: if selected {
					Style::default()
						.fg(Color::Cyan)
						.add_modifier(Modifier::BOLD)
				} else {
					Style::default()
				},
				height,
			});
			remaining = remaining.saturating_sub(height);
		}
	}

	let hidden_line_height = u16::from(hidden_candidates > 0 && remaining > 0);
	let layout_height = sections
		.iter()
		.map(|section| section.height)
		.sum::<u16>()
		.saturating_add(hidden_line_height);
	if layout_height == 0 {
		return;
	}

	let constraints = sections
		.iter()
		.map(|section| Constraint::Min(section.height))
		.chain((hidden_line_height > 0).then_some(Constraint::Length(1)))
		.collect::<Vec<_>>();
	let layout_area = Rect {
		x: area.x,
		y: area.y,
		width: area.width,
		height: layout_height.min(area.height),
	};
	let chunks = Layout::default()
		.direction(Direction::Vertical)
		.constraints(constraints)
		.split(layout_area);

	for (section, chunk) in sections.iter().zip(chunks.iter()) {
		render_snippet_section(section, buf, *chunk);
	}
	if hidden_candidates > 0
		&& hidden_line_height > 0
		&& let Some(chunk) = chunks.get(sections.len())
	{
		write_line(
			buf,
			*chunk,
			chunk.y,
			&format!("({hidden_candidates} more candidates not shown — resize terminal)"),
			Style::default().fg(Color::DarkGray),
		);
	}
}

fn snippet_section_height(snippet: &str) -> u16 {
	let line_count = snippet.lines().count().max(1) as u16;
	line_count
		.saturating_add(2)
		.clamp(MIN_SNIPPET_SECTION_HEIGHT, MAX_SNIPPET_SECTION_HEIGHT)
}

fn render_snippet_section(section: &SnippetSection, buf: &mut Buffer, area: Rect) {
	if area.width == 0 || area.height == 0 {
		return;
	}
	let block = Block::default()
		.title(format!(" {} ", section.title))
		.borders(Borders::ALL)
		.border_style(section.border_style);
	let inner = block.inner(area);
	block.render(area, buf);
	if inner.width == 0 || inner.height == 0 {
		return;
	}
	Paragraph::new(section.body.clone())
		.style(section.body_style)
		.render(inner, buf);
}

fn render_action_rows(resolver: &ConflictResolver, buf: &mut Buffer, area: Rect) {
	for (row, line) in action_choice_lines(resolver)
		.into_iter()
		.take(area.height as usize)
		.enumerate()
	{
		write_line(buf, area, area.y + row as u16, &line.text, line.style);
	}
}

#[allow(dead_code)]
fn render_choice_list(resolver: &ConflictResolver, buf: &mut Buffer, area: Rect) {
	let lines = choice_lines(resolver);
	let selected_line = lines
		.iter()
		.position(|line| line.item_index == Some(resolver.selected_index))
		.unwrap_or(0);
	let scroll = selected_line.saturating_sub(area.height.saturating_sub(1) as usize);
	for (row, line) in lines
		.iter()
		.skip(scroll)
		.take(area.height as usize)
		.enumerate()
	{
		write_line(buf, area, area.y + row as u16, &line.text, line.style);
	}
}

struct ChoiceLine {
	item_index: Option<usize>,
	text: String,
	style: Style,
}

fn choice_lines(resolver: &ConflictResolver) -> Vec<ChoiceLine> {
	let selected_style = Style::default()
		.fg(Color::Yellow)
		.add_modifier(Modifier::BOLD);
	let mut lines = Vec::new();
	for (index, candidate) in resolver.candidates.iter().enumerate() {
		let selected = resolver.selected_index == index;
		let marker = if selected { "❯" } else { " " };
		lines.push(ChoiceLine {
			item_index: Some(index),
			text: format!(
				"{marker} [{}] {} ({}, prec {})",
				index + 1,
				candidate.display_name,
				candidate.mod_id,
				candidate.precedence
			),
			style: if selected {
				selected_style
			} else {
				Style::default()
			},
		});
		for summary_line in rendered_summary_lines(&candidate.summary) {
			lines.push(ChoiceLine {
				item_index: None,
				text: format!("      {summary_line}"),
				style: Style::default().fg(Color::Gray),
			});
		}
	}
	lines.extend(action_choice_lines(resolver));
	lines
}

fn action_choice_lines(resolver: &ConflictResolver) -> Vec<ChoiceLine> {
	let selected_style = Style::default()
		.fg(Color::Yellow)
		.add_modifier(Modifier::BOLD);
	let mut lines = vec![ChoiceLine {
		item_index: None,
		text: "  ─────".to_string(),
		style: Style::default().fg(Color::DarkGray),
	}];
	for (offset, label) in [
		"[d] defer (skip; record in report)",
		"[s] use external file (paste a path)",
		"[k] keep existing file in out dir (don't overwrite)",
		"[q] abort merge",
	]
	.iter()
	.enumerate()
	{
		let index = resolver.candidates.len() + offset;
		let selected = resolver.selected_index == index;
		let marker = if selected { "❯" } else { " " };
		lines.push(ChoiceLine {
			item_index: Some(index),
			text: format!("{marker} {label}"),
			style: if selected {
				selected_style
			} else {
				Style::default()
			},
		});
	}
	lines
}

fn rendered_summary_lines(summary: &[String]) -> Vec<String> {
	if summary.len() <= MAX_RENDERED_SUMMARY_LINES {
		return summary.to_vec();
	}

	let keep_prefix = MAX_RENDERED_SUMMARY_LINES.saturating_sub(1);
	let mut rendered: Vec<String> = summary.iter().take(keep_prefix).cloned().collect();
	let hidden_between = summary.len().saturating_sub(keep_prefix + 1);
	let overflow_line = summary
		.last()
		.and_then(|line| adjusted_more_line(line, hidden_between))
		.unwrap_or_else(|| "  …".to_string());
	rendered.push(overflow_line);
	rendered
}

fn adjusted_more_line(line: &str, hidden_between: usize) -> Option<String> {
	for prefix in ["  + … (", "  - … ("] {
		let Some(rest) = line.strip_prefix(prefix) else {
			continue;
		};
		let Some(count) = rest.strip_suffix(" more)") else {
			continue;
		};
		let count = count.parse::<usize>().ok()?;
		return Some(format!("{prefix}{} more)", count + hidden_between));
	}
	None
}

fn draw_separator(buf: &mut Buffer, area: Rect, y: u16) {
	if y <= area.y || y >= area.y.saturating_add(area.height).saturating_sub(1) || area.width < 2 {
		return;
	}
	let right = area.x + area.width - 1;
	buf[(area.x, y)].set_symbol("├");
	for x in area.x + 1..right {
		buf[(x, y)].set_symbol("─");
	}
	buf[(right, y)].set_symbol("┤");
}

fn write_line(buf: &mut Buffer, area: Rect, y: u16, text: &str, style: Style) {
	if y < area.y || y >= area.y.saturating_add(area.height) || area.width == 0 {
		return;
	}
	buf.set_stringn(area.x, y, text, area.width as usize, style);
}

fn render_progress_gauge(
	area: Rect,
	buf: &mut Buffer,
	current: usize,
	total: usize,
	deferred_so_far: usize,
) {
	if area.width == 0 || area.height == 0 {
		return;
	}
	let total = total.max(1);
	let current = current.min(total);
	let ratio = current as f64 / total as f64;
	let percent = (ratio * 100.0).round() as u16;
	let label = format!("conflict {current}/{total}  ({percent}%, {deferred_so_far} deferred)");
	Gauge::default()
		.gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
		.ratio(ratio.clamp(0.0, 1.0))
		.label(label)
		.render(area, buf);
}

/// Conservative estimate of how many terminal rows a `Paragraph::wrap`
/// rendering of `text` will consume at `width` columns. Counts each `\n`
/// as a hard break and divides each segment by `width` rounded up. Uses
/// `chars().count()` for the per-segment length: this overcounts CJK
/// (full-width) glyphs that ratatui would render as 2 cells, which is the
/// safe direction — we'd rather reserve a row that ends up blank than
/// truncate the wrap.
fn estimate_wrapped_line_count(text: &str, width: u16) -> usize {
	if width == 0 {
		return 1;
	}
	let width = width as usize;
	let mut total = 0usize;
	for segment in text.split('\n') {
		let chars = segment.chars().count();
		let segment_lines = if chars == 0 { 1 } else { chars.div_ceil(width) };
		total = total.saturating_add(segment_lines);
	}
	total.max(1)
}

fn truncate_rendered_snippet(input: &str, max_lines: usize, max_chars: usize) -> String {
	if max_lines == 0 {
		return String::new();
	}
	// ratatui's Paragraph widget renders tab characters as zero-width on most
	// terminals, so engine-rendered Clausewitz text with tab indentation shows
	// up flat. Expand tabs to 2 spaces so the snippet structure is preserved.
	let expanded = input.replace('\t', "  ");
	let trimmed = expanded.trim_end_matches(['\r', '\n']);
	let lines = trimmed.lines().collect::<Vec<_>>();
	if lines.is_empty() {
		return String::new();
	}

	let exceeds_lines = lines.len() > max_lines;
	let keep_lines = if exceeds_lines {
		max_lines.saturating_sub(1)
	} else {
		lines.len()
	};
	let mut rendered = lines
		.iter()
		.take(keep_lines)
		.map(|line| truncate_snippet_line(line, max_chars))
		.collect::<Vec<_>>();
	if exceeds_lines {
		rendered.push(format!(
			"… ({} more lines)",
			lines.len().saturating_sub(keep_lines)
		));
	}
	rendered.join("\n")
}

fn truncate_snippet_line(line: &str, max_chars: usize) -> String {
	if max_chars == 0 {
		return "…".to_string();
	}
	if line.chars().count() <= max_chars {
		return line.to_string();
	}
	let truncated = line
		.chars()
		.take(max_chars.saturating_sub(1))
		.collect::<String>();
	format!("{truncated}…")
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_engine::CandidateView;

	fn sample_view() -> ConflictView {
		ConflictView {
			file_path: PathBuf::from("events/FlavorFRA.txt"),
			address_path: vec![
				"flavor_fra.3135".to_string(),
				"option".to_string(),
				"define_advisor".to_string(),
			],
			address_key: "name".to_string(),
			conflict_id: "conflict-id".to_string(),
			reason: "sibling mods set the same scalar to divergent values".to_string(),
			vanilla_snippet: Some("name = \"old\"\n".to_string()),
			candidates: vec![
				CandidateView {
					mod_id: "2164202838".to_string(),
					mod_display_name: "Europa Expanded".to_string(),
					precedence: 0,
					patch_summary: vec![
						"set \"name\": \"old\" → \"Charles-Francois de Broglie\"".to_string(),
					],
					patch_rendered: "name = \"Charles-Francois de Broglie\"\n".to_string(),
				},
				CandidateView {
					mod_id: "1999055990".to_string(),
					mod_display_name: "Chinese Language Supp.".to_string(),
					precedence: 2,
					patch_summary: vec![
						"set \"name\": \"old\" → \"Chinese localization\"".to_string(),
					],
					patch_rendered: "name = \"Chinese localization\"\n".to_string(),
				},
			],
		}
	}

	fn buffer_lines(buffer: &Buffer) -> Vec<String> {
		(0..buffer.area.height)
			.map(|y| {
				(0..buffer.area.width)
					.map(|x| buffer[(buffer.area.x + x, buffer.area.y + y)].symbol())
					.collect::<String>()
			})
			.collect()
	}

	fn assert_line_contains(lines: &[String], expected: &str) {
		assert!(
			lines.iter().any(|line| line.contains(expected)),
			"expected a rendered line to contain {expected:?}; got:\n{}",
			lines.join("\n")
		);
	}

	#[test]
	fn render_conflict_resolver_into_buffer() {
		let view = sample_view();
		let resolver = ConflictResolver::new(ConflictResolverArgs {
			view: &view,
			current_idx: 1,
			total: 30,
			deferred_so_far: 0,
		});
		let area = Rect::new(0, 0, 76, 30);
		let mut actual = Buffer::empty(area);
		Widget::render(&resolver, area, &mut actual);
		let actual_lines = buffer_lines(&actual);

		assert_eq!(
			actual_lines[0],
			"┌ foch merge: conflict 1/30 ───────────────────────────────────────────────┐"
		);
		let gauge_line = actual_lines[1].as_str();
		assert!(
			gauge_line.contains("conflict 1/30"),
			"gauge missing label: {gauge_line:?}"
		);
		assert_line_contains(&actual_lines, "name = \"old\"");
		assert_line_contains(&actual_lines, "[1] Europa Expanded");
		assert_line_contains(&actual_lines, "[2] Chinese Language Supp.");
		assert_line_contains(&actual_lines, "name = \"Charles-Francois de Broglie\"");
		assert_line_contains(&actual_lines, "name = \"Chinese localization\"");
		assert_line_contains(&actual_lines, "[d] defer (skip; record in report)");
		assert_line_contains(&actual_lines, "[s] use external file (paste a path)");
		assert_line_contains(
			&actual_lines,
			"[k] keep existing file in out dir (don't overwrite)",
		);
		assert_line_contains(&actual_lines, "[q] abort merge");
	}

	#[test]
	fn render_conflict_resolver_without_vanilla_shows_placeholder() {
		let mut view = sample_view();
		view.vanilla_snippet = None;
		let resolver = ConflictResolver::new(ConflictResolverArgs {
			view: &view,
			current_idx: 1,
			total: 2,
			deferred_so_far: 0,
		});
		let area = Rect::new(0, 0, 76, 30);
		let mut actual = Buffer::empty(area);
		Widget::render(&resolver, area, &mut actual);

		assert_line_contains(&buffer_lines(&actual), NO_VANILLA_SNIPPET);
	}

	#[test]
	fn rendered_summary_lines_collapses_middle_overflow() {
		let lines = rendered_summary_lines(&[
			"one".to_string(),
			"two".to_string(),
			"three".to_string(),
			"  + … (2 more)".to_string(),
		]);

		assert_eq!(
			lines,
			vec![
				"one".to_string(),
				"two".to_string(),
				"three".to_string(),
				"  + … (2 more)".to_string(),
			]
		);
	}
}
