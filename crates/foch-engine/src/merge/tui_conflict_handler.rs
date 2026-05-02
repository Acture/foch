use std::collections::HashMap;
use std::io::{self, IsTerminal, Stdout};
use std::path::{Path, PathBuf};

use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
	EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use foch_core::config::compute_conflict_id;
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Widget, Wrap};

use super::conflict_handler::{
	ConfigWriter, ConflictDecision, ConflictHandler, resolution_entry_for_decision,
};
use super::emit::{EmitOptions, emit_clausewitz_statements_with_options};
use super::patch::ClausewitzPatch;
use super::patch_merge::{PatchAddress, PatchConflict};

const ACTION_COUNT: usize = 4;
const MAX_SUMMARY_CHARS: usize = 80;
const MAX_CHILD_PREVIEW_ENTRIES: usize = 3;
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

impl ConflictResolver {
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		conflict: &PatchConflict,
		file: &Path,
		address: &PatchAddress,
		conflict_id: &str,
		mod_displayname_lookup: &HashMap<String, String>,
		current_idx: usize,
		total: usize,
		deferred_so_far: usize,
		vanilla_snippet: Option<String>,
	) -> Self {
		let candidates: Vec<ConflictCandidate> = conflict
			.patches
			.iter()
			.map(|patch| ConflictCandidate {
				mod_id: patch.mod_id.clone(),
				display_name: mod_displayname_lookup
					.get(&patch.mod_id)
					.filter(|name| !name.trim().is_empty())
					.cloned()
					.unwrap_or_else(|| patch.mod_id.clone()),
				precedence: patch.precedence,
				summary: concise_patch_summary(&patch.patch),
				projected_snippet: projected_patch_snippet(&patch.patch),
			})
			.collect();
		let mut address_path = address.path.clone();
		if !address.key.is_empty() {
			address_path.push(address.key.clone());
		}
		// Default the cursor to "Defer" (the first action right after the
		// candidate list) so an accidental Enter doesn't pick an arbitrary
		// mod's patch. Users can still arrow up to candidates explicitly.
		let default_selected = candidates.len();
		Self {
			current_conflict_index: current_idx,
			total_conflicts: total,
			deferred_so_far,
			file_path: file.to_path_buf(),
			address_path,
			reason: conflict.reason.clone(),
			conflict_id: conflict_id.to_string(),
			vanilla_snippet: vanilla_snippet.map(|snippet| {
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
	pub current_file: PathBuf,
	pub config_writer: Box<dyn ConfigWriter>,
	mod_displayname_lookup: HashMap<String, String>,
	current_conflict_index: usize,
	total_conflicts: usize,
	deferred_so_far: usize,
	vanilla_snippet: Option<String>,
}

impl InteractiveTuiHandler {
	pub fn new(
		current_file: PathBuf,
		config_writer: Box<dyn ConfigWriter>,
		mod_displayname_lookup: HashMap<String, String>,
		current_conflict_index: usize,
		total_conflicts: usize,
		deferred_so_far: usize,
		vanilla_snippet: Option<String>,
	) -> Self {
		Self {
			current_file,
			config_writer,
			mod_displayname_lookup,
			current_conflict_index,
			total_conflicts,
			deferred_so_far,
			vanilla_snippet,
		}
	}

	fn persist_decision(
		&mut self,
		address: &PatchAddress,
		decision: ConflictDecision,
	) -> ConflictDecision {
		let Some(entry) = resolution_entry_for_decision(&self.current_file, address, &decision)
		else {
			return decision;
		};
		match self.config_writer.append_resolution(entry) {
			Ok(()) => decision,
			Err(err) => {
				eprintln!("[foch] failed to persist interactive resolution: {err}");
				ConflictDecision::Abort
			}
		}
	}

	fn stdin_stdout_are_tty(&self) -> bool {
		io::stdin().is_terminal() && io::stdout().is_terminal()
	}
}

impl ConflictHandler for InteractiveTuiHandler {
	fn on_conflict(
		&mut self,
		_path: &str,
		address: &PatchAddress,
		conflict: &PatchConflict,
	) -> ConflictDecision {
		if !self.stdin_stdout_are_tty() {
			eprintln!(
				"[foch] interactive TUI could not be entered because stdin/stdout is not a TTY; downgrading to defer"
			);
			return ConflictDecision::Defer;
		}

		let address_path = address.path.join("/");
		let conflict_id = compute_conflict_id(&self.current_file, &address_path, &address.key);
		let mut resolver = ConflictResolver::new(
			conflict,
			&self.current_file,
			address,
			&conflict_id,
			&self.mod_displayname_lookup,
			self.current_conflict_index,
			self.total_conflicts,
			self.deferred_so_far,
			self.vanilla_snippet.clone(),
		);

		let action = match run_resolver(&mut resolver) {
			Ok(action) => action,
			Err(err) => {
				eprintln!("[foch] interactive TUI failed: {err}; downgrading to defer");
				return ConflictDecision::Defer;
			}
		};

		let decision = match action {
			ConflictAction::PickCandidate(index) => conflict
				.patches
				.get(index)
				.map(|patch| ConflictDecision::PickMod(patch.mod_id.clone()))
				.unwrap_or(ConflictDecision::Defer),
			ConflictAction::Defer => ConflictDecision::Defer,
			ConflictAction::ExternalFile(path) => {
				if path.as_os_str().is_empty() {
					ConflictDecision::Defer
				} else {
					ConflictDecision::UseFile(path)
				}
			}
			ConflictAction::KeepExisting => ConflictDecision::KeepExisting,
			ConflictAction::Abort => ConflictDecision::Abort,
		};
		self.persist_decision(address, decision)
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

fn projected_patch_snippet(patch: &ClausewitzPatch) -> String {
	let rendered = match render_projected_patch_snippet(patch) {
		Ok(rendered) => rendered,
		Err(err) => format!("(failed to render patch: {err})"),
	};
	truncate_rendered_snippet(&rendered, MAX_SNIPPET_LINES, MAX_SNIPPET_LINE_CHARS)
}

fn render_projected_patch_snippet(
	patch: &ClausewitzPatch,
) -> Result<String, super::error::MergeError> {
	match patch {
		ClausewitzPatch::SetValue { key, new_value, .. } => {
			emit_statement_snippet(&assignment(key, new_value.clone()))
		}
		ClausewitzPatch::ReplaceBlock { new_statement, .. } => {
			emit_statement_snippet(new_statement)
		}
		ClausewitzPatch::InsertNode { statement, .. } => emit_statement_snippet(statement),
		ClausewitzPatch::RemoveNode { .. } => Ok("(removed)".to_string()),
		ClausewitzPatch::AppendListItem { value, .. }
		| ClausewitzPatch::AppendBlockItem { value, .. } => render_prefixed_value("(appends)", value),
		ClausewitzPatch::RemoveListItem { value, .. }
		| ClausewitzPatch::RemoveBlockItem { value, .. } => render_prefixed_value("(removes)", value),
		ClausewitzPatch::Rename {
			old_key, new_key, ..
		} => Ok(format!("(renames \"{old_key}\" -> \"{new_key}\")")),
	}
}

fn emit_statement_snippet(statement: &AstStatement) -> Result<String, super::error::MergeError> {
	emit_clausewitz_statements_with_options(
		std::slice::from_ref(statement),
		&EmitOptions::default(),
	)
}

fn render_prefixed_value(
	prefix: &str,
	value: &AstValue,
) -> Result<String, super::error::MergeError> {
	let rendered = emit_clausewitz_statements_with_options(
		&[AstStatement::Item {
			value: value.clone(),
			span: synthetic_span(),
		}],
		&EmitOptions::default(),
	)?;
	Ok(format!("{prefix} {}", rendered.trim_end()))
}

fn assignment(key: &str, value: AstValue) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: synthetic_span(),
		value,
		span: synthetic_span(),
	}
}

fn synthetic_span() -> SpanRange {
	SpanRange {
		start: Span {
			line: 0,
			column: 0,
			offset: 0,
		},
		end: Span {
			line: 0,
			column: 0,
			offset: 0,
		},
	}
}

fn truncate_rendered_snippet(input: &str, max_lines: usize, max_chars: usize) -> String {
	if max_lines == 0 {
		return String::new();
	}
	// ratatui's Paragraph widget renders tab characters as zero-width on most
	// terminals, so the emit_clausewitz_statements_with_options output (which
	// uses tab indentation by default) shows up flat. Expand tabs to 2 spaces
	// so the structure of the snippet is preserved on screen.
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

fn concise_patch_summary(patch: &ClausewitzPatch) -> Vec<String> {
	match patch {
		ClausewitzPatch::SetValue {
			key,
			old_value,
			new_value,
			..
		} => vec![format!(
			"set \"{key}\": {} → {}",
			value_summary(old_value),
			value_summary(new_value)
		)],
		ClausewitzPatch::RemoveNode { key, removed, .. } => remove_node_summary(key, removed),
		ClausewitzPatch::InsertNode { key, statement, .. } => insert_node_summary(key, statement),
		ClausewitzPatch::ReplaceBlock {
			key, new_statement, ..
		} => block_patch_summary(format!("replace block \"{key}\""), new_statement, '+'),
		ClausewitzPatch::AppendListItem { key, value, .. } => {
			vec![format!(
				"append to list \"{key}\": {}",
				value_summary(value)
			)]
		}
		ClausewitzPatch::RemoveListItem { key, value, .. } => {
			vec![format!(
				"remove from list \"{key}\": {}",
				value_summary(value)
			)]
		}
		ClausewitzPatch::AppendBlockItem { value, .. } => {
			vec![format!("append item: {}", value_summary(value))]
		}
		ClausewitzPatch::RemoveBlockItem { value, .. } => {
			vec![format!("remove item: {}", value_summary(value))]
		}
		ClausewitzPatch::Rename {
			old_key, new_key, ..
		} => vec![format!("rename \"{old_key}\" → \"{new_key}\"")],
	}
}

fn remove_node_summary(key: &str, removed: &AstStatement) -> Vec<String> {
	let mut lines = vec![format!(
		"remove \"{key}\" (was: {})",
		statement_value_summary(removed)
	)];
	if let AstStatement::Assignment {
		value: AstValue::Block { items, .. },
		..
	} = removed
	{
		lines.extend(child_preview_lines(items, '-'));
	}
	lines
}

fn insert_node_summary(key: &str, statement: &AstStatement) -> Vec<String> {
	if statement_block_items(statement).is_some() {
		return block_patch_summary(format!("insert \"{key}\""), statement, '+');
	}
	vec![format!(
		"insert \"{key}\" = {}",
		statement_value_summary(statement)
	)]
}

fn block_patch_summary(prefix: String, statement: &AstStatement, marker: char) -> Vec<String> {
	let mut lines = vec![format!(
		"{prefix} ({} entries)",
		statement_entry_count(statement)
	)];
	if let Some(items) = statement_block_items(statement) {
		lines.extend(child_preview_lines(items, marker));
	}
	lines
}

fn statement_block_items(statement: &AstStatement) -> Option<&[AstStatement]> {
	match statement {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		}
		| AstStatement::Item {
			value: AstValue::Block { items, .. },
			..
		} => Some(items),
		AstStatement::Assignment { .. }
		| AstStatement::Item { .. }
		| AstStatement::Comment { .. } => None,
	}
}

fn child_preview_lines(items: &[AstStatement], marker: char) -> Vec<String> {
	let entries: Vec<String> = items
		.iter()
		.filter_map(|statement| child_preview_line(statement, marker))
		.collect();
	let mut lines: Vec<String> = entries
		.iter()
		.take(MAX_CHILD_PREVIEW_ENTRIES)
		.cloned()
		.collect();
	let remaining = entries.len().saturating_sub(MAX_CHILD_PREVIEW_ENTRIES);
	if remaining > 0 {
		lines.push(format!("  {marker} … ({remaining} more)"));
	}
	lines
}

fn child_preview_line(statement: &AstStatement, marker: char) -> Option<String> {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			Some(format!("  {marker} {key} = {}", value_summary(value)))
		}
		AstStatement::Item { value, .. } => Some(format!("  {marker} {}", value_summary(value))),
		AstStatement::Comment { .. } => None,
	}
}

fn statement_value_summary(statement: &AstStatement) -> String {
	match statement {
		AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
			value_summary(value)
		}
		AstStatement::Comment { text, .. } => format!("# {}", sanitize_summary(text)),
	}
}

fn statement_entry_count(statement: &AstStatement) -> usize {
	match statement {
		AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
			value_entry_count(value)
		}
		AstStatement::Comment { .. } => 0,
	}
}

fn value_entry_count(value: &AstValue) -> usize {
	match value {
		AstValue::Block { items, .. } => items.len(),
		AstValue::Scalar { .. } => 1,
	}
}

fn value_summary(value: &AstValue) -> String {
	match value {
		AstValue::Scalar { value, .. } => {
			truncate_summary(&sanitize_summary(&scalar_summary(value)))
		}
		AstValue::Block { items, .. } => format!("{{ {} entries }}", items.len()),
	}
}

fn scalar_summary(value: &ScalarValue) -> String {
	match value {
		ScalarValue::Identifier(value) | ScalarValue::Number(value) => value.clone(),
		ScalarValue::String(value) => format!("\"{}\"", escape_string(value)),
		ScalarValue::Bool(value) => {
			if *value {
				"yes".to_string()
			} else {
				"no".to_string()
			}
		}
	}
}

fn escape_string(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn sanitize_summary(value: &str) -> String {
	let mut out = String::new();
	for c in value.chars() {
		match c {
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
			c => out.push(c),
		}
	}
	out
}

fn truncate_summary(value: &str) -> String {
	let mut chars = value.chars();
	let truncated: String = chars.by_ref().take(MAX_SUMMARY_CHARS).collect();
	if chars.next().is_some() {
		format!("{truncated}…")
	} else {
		truncated
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::parser::{Span, SpanRange};
	type TestResult = Result<(), Box<dyn std::error::Error>>;

	fn span() -> SpanRange {
		SpanRange {
			start: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
			end: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
		}
	}

	fn scalar(value: ScalarValue) -> AstValue {
		AstValue::Scalar {
			value,
			span: span(),
		}
	}

	fn ident(value: &str) -> AstValue {
		scalar(ScalarValue::Identifier(value.to_string()))
	}

	fn string(value: &str) -> AstValue {
		scalar(ScalarValue::String(value.to_string()))
	}

	fn block(items: Vec<AstStatement>) -> AstValue {
		AstValue::Block {
			items,
			span: span(),
		}
	}

	fn assignment(key: &str, value: AstValue) -> AstStatement {
		AstStatement::Assignment {
			key: key.to_string(),
			key_span: span(),
			value,
			span: span(),
		}
	}

	fn item(value: AstValue) -> AstStatement {
		AstStatement::Item {
			value,
			span: span(),
		}
	}

	fn sample_conflict() -> PatchConflict {
		PatchConflict {
			patches: vec![
				super::super::patch_merge::AttributedPatch {
					mod_id: "2164202838".to_string(),
					precedence: 0,
					patch: ClausewitzPatch::SetValue {
						path: vec!["flavor_fra.3135".to_string(), "option".to_string()],
						key: "name".to_string(),
						old_value: string("old"),
						new_value: string("Charles-Francois de Broglie"),
					},
				},
				super::super::patch_merge::AttributedPatch {
					mod_id: "1999055990".to_string(),
					precedence: 2,
					patch: ClausewitzPatch::SetValue {
						path: vec!["flavor_fra.3135".to_string(), "option".to_string()],
						key: "name".to_string(),
						old_value: string("old"),
						new_value: string("Chinese localization"),
					},
				},
			],
			reason: "sibling mods set the same scalar to divergent values".to_string(),
		}
	}

	fn sample_address() -> PatchAddress {
		PatchAddress {
			path: vec![
				"flavor_fra.3135".to_string(),
				"option".to_string(),
				"define_advisor".to_string(),
			],
			key: "name".to_string(),
		}
	}

	fn display_names() -> HashMap<String, String> {
		HashMap::from([
			("2164202838".to_string(), "Europa Expanded".to_string()),
			(
				"1999055990".to_string(),
				"Chinese Language Supp.".to_string(),
			),
		])
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
		let resolver = ConflictResolver::new(
			&sample_conflict(),
			Path::new("events/FlavorFRA.txt"),
			&sample_address(),
			"conflict-id",
			&display_names(),
			1,
			30,
			0,
			Some("name = \"old\"\n".to_string()),
		);
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
		let resolver = ConflictResolver::new(
			&sample_conflict(),
			Path::new("events/FlavorFRA.txt"),
			&sample_address(),
			"conflict-id",
			&display_names(),
			1,
			2,
			0,
			None,
		);
		let area = Rect::new(0, 0, 76, 30);
		let mut actual = Buffer::empty(area);
		Widget::render(&resolver, area, &mut actual);

		assert_line_contains(&buffer_lines(&actual), NO_VANILLA_SNIPPET);
	}

	#[test]
	fn projected_patch_snippet_covers_patch_variants() {
		let replace_statement = assignment(
			"option",
			block(vec![
				assignment("name", string("A")),
				assignment("ai_chance", ident("B")),
			]),
		);
		let variants = vec![
			(
				ClausewitzPatch::SetValue {
					path: vec![],
					key: "name".to_string(),
					old_value: string("old"),
					new_value: string("new"),
				},
				vec!["name = \"new\""],
			),
			(
				ClausewitzPatch::RemoveNode {
					path: vec![],
					key: "owner".to_string(),
					removed: assignment("owner", ident("FRA")),
				},
				vec!["(removed)"],
			),
			(
				ClausewitzPatch::InsertNode {
					path: vec![],
					key: "owner".to_string(),
					statement: assignment("owner", ident("FRA")),
				},
				vec!["owner = FRA"],
			),
			(
				ClausewitzPatch::ReplaceBlock {
					path: vec![],
					key: "option".to_string(),
					old_statement: assignment("option", block(vec![])),
					new_statement: replace_statement,
				},
				vec!["option = {", "name = \"A\"", "ai_chance = B"],
			),
			(
				ClausewitzPatch::AppendListItem {
					path: vec![],
					key: "tag".to_string(),
					value: ident("FRA"),
				},
				vec!["(appends) FRA"],
			),
			(
				ClausewitzPatch::RemoveListItem {
					path: vec![],
					key: "tag".to_string(),
					value: ident("ENG"),
				},
				vec!["(removes) ENG"],
			),
			(
				ClausewitzPatch::AppendBlockItem {
					path: vec![],
					value: ident("FRA"),
				},
				vec!["(appends) FRA"],
			),
			(
				ClausewitzPatch::RemoveBlockItem {
					path: vec![],
					value: ident("ENG"),
				},
				vec!["(removes) ENG"],
			),
			(
				ClausewitzPatch::Rename {
					path: vec![],
					old_key: "old".to_string(),
					new_key: "new".to_string(),
				},
				vec!["(renames \"old\" -> \"new\")"],
			),
		];

		for (patch, expected_parts) in variants {
			let snippet = projected_patch_snippet(&patch);
			for expected in expected_parts {
				assert!(
					snippet.contains(expected),
					"snippet {snippet:?} should contain {expected:?}"
				);
			}
		}
	}

	#[test]
	fn projected_patch_snippet_truncates_long_blocks() {
		let items = (0..12)
			.map(|index| assignment(&format!("entry_{index}"), ident("yes")))
			.collect::<Vec<_>>();
		let patch = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "root".to_string(),
			old_statement: assignment("root", block(vec![])),
			new_statement: assignment("root", block(items)),
		};

		let snippet = projected_patch_snippet(&patch);
		let lines = snippet.lines().collect::<Vec<_>>();

		assert_eq!(lines.len(), MAX_SNIPPET_LINES);
		assert!(
			lines
				.last()
				.is_some_and(|line| line.contains("… (") && line.contains("more lines)")),
			"long snippet should end with a hidden-line count: {snippet:?}"
		);
	}

	#[test]
	fn render_progress_gauge_renders_empty_at_zero() {
		let area = Rect::new(0, 0, 50, 1);
		let mut buf = Buffer::empty(area);
		render_progress_gauge(area, &mut buf, 0, 10, 2);
		let line: String = (0..area.width)
			.map(|x| buf[(area.x + x, area.y)].symbol())
			.collect::<String>();
		assert!(
			line.contains("conflict 0/10") && line.contains("(0%, 2 deferred)"),
			"unexpected gauge line: {line:?}"
		);
	}

	#[test]
	fn render_progress_gauge_handles_zero_total_safely() {
		let area = Rect::new(0, 0, 50, 1);
		let mut buf = Buffer::empty(area);
		render_progress_gauge(area, &mut buf, 0, 0, 0);
		let line: String = (0..area.width)
			.map(|x| buf[(area.x + x, area.y)].symbol())
			.collect::<String>();
		assert!(
			line.contains("conflict 0/1") && line.contains("(0%, 0 deferred)"),
			"gauge must clamp zero total to 1 to avoid div-by-zero: {line:?}"
		);
	}

	#[test]
	fn handle_key_selects_candidates_and_actions() {
		let mut resolver = ConflictResolver::new(
			&sample_conflict(),
			Path::new("events/FlavorFRA.txt"),
			&sample_address(),
			"conflict-id",
			&display_names(),
			1,
			2,
			0,
			None,
		);
		let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
		let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
		let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

		// Cursor defaults to "[d] defer" so accidental Enter never picks
		// an arbitrary mod's patch.
		assert_eq!(resolver.handle_key(enter), Some(ConflictAction::Defer));
		// Up moves into the candidate list (last candidate first).
		assert_eq!(resolver.handle_key(up), None);
		assert_eq!(
			resolver.handle_key(enter),
			Some(ConflictAction::PickCandidate(1))
		);
		assert_eq!(resolver.handle_key(up), None);
		assert_eq!(
			resolver.handle_key(enter),
			Some(ConflictAction::PickCandidate(0))
		);
		// Now navigate down past the candidate list and through the action
		// rows to the final Abort entry.
		for _ in 0..5 {
			resolver.handle_key(down);
		}
		assert_eq!(resolver.handle_key(enter), Some(ConflictAction::Abort));
	}

	#[test]
	fn handle_key_supports_action_shortcuts() {
		let mut resolver = ConflictResolver::new(
			&sample_conflict(),
			Path::new("events/FlavorFRA.txt"),
			&sample_address(),
			"conflict-id",
			&display_names(),
			1,
			2,
			0,
			None,
		);
		let rendered_actions: Vec<String> = choice_lines(&resolver)
			.into_iter()
			.filter_map(|line| line.item_index.map(|_| line.text))
			.skip(resolver.candidates.len())
			.collect();
		assert_eq!(
			rendered_actions,
			vec![
				"❯ [d] defer (skip; record in report)",
				"  [s] use external file (paste a path)",
				"  [k] keep existing file in out dir (don't overwrite)",
				"  [q] abort merge",
			]
		);

		assert_eq!(
			resolver.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
			Some(ConflictAction::Defer)
		);
		assert_eq!(
			resolver.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
			Some(ConflictAction::ExternalFile(PathBuf::new()))
		);
		assert_eq!(
			resolver.handle_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT)),
			Some(ConflictAction::KeepExisting)
		);
		assert_eq!(
			resolver.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
			Some(ConflictAction::Defer)
		);
	}

	#[test]
	fn concise_patch_summary_covers_patch_variants() -> TestResult {
		let patches = vec![
			(
				ClausewitzPatch::SetValue {
					path: vec![],
					key: "name".to_string(),
					old_value: ident("old"),
					new_value: string("new\nvalue"),
				},
				vec!["set \"name\": old → \"new\\nvalue\""],
			),
			(
				ClausewitzPatch::RemoveNode {
					path: vec![],
					key: "owner".to_string(),
					removed: assignment("owner", ident("FRA")),
				},
				vec!["remove \"owner\" (was: FRA)"],
			),
			(
				ClausewitzPatch::InsertNode {
					path: vec![],
					key: "owner".to_string(),
					statement: assignment("owner", ident("FRA")),
				},
				vec!["insert \"owner\" = FRA"],
			),
			(
				ClausewitzPatch::ReplaceBlock {
					path: vec![],
					key: "option".to_string(),
					old_statement: assignment("option", block(vec![])),
					new_statement: assignment(
						"option",
						block(vec![
							assignment("name", string("A")),
							assignment("ai_chance", ident("B")),
						]),
					),
				},
				vec![
					"replace block \"option\" (2 entries)",
					"  + name = \"A\"",
					"  + ai_chance = B",
				],
			),
			(
				ClausewitzPatch::AppendListItem {
					path: vec![],
					key: "tag".to_string(),
					value: ident("FRA"),
				},
				vec!["append to list \"tag\": FRA"],
			),
			(
				ClausewitzPatch::RemoveListItem {
					path: vec![],
					key: "tag".to_string(),
					value: ident("ENG"),
				},
				vec!["remove from list \"tag\": ENG"],
			),
			(
				ClausewitzPatch::AppendBlockItem {
					path: vec![],
					value: ident("FRA"),
				},
				vec!["append item: FRA"],
			),
			(
				ClausewitzPatch::RemoveBlockItem {
					path: vec![],
					value: ident("ENG"),
				},
				vec!["remove item: ENG"],
			),
			(
				ClausewitzPatch::Rename {
					path: vec![],
					old_key: "old".to_string(),
					new_key: "new".to_string(),
				},
				vec!["rename \"old\" → \"new\""],
			),
		];

		for (patch, expected) in patches {
			let expected = expected.into_iter().map(str::to_string).collect::<Vec<_>>();
			assert_eq!(concise_patch_summary(&patch), expected);
		}
		Ok(())
	}

	#[test]
	fn concise_patch_summary_replace_block_lists_first_three_children() {
		let patch = ClausewitzPatch::ReplaceBlock {
			path: vec![],
			key: "X".to_string(),
			old_statement: assignment("X", block(vec![])),
			new_statement: assignment(
				"X",
				block(vec![
					assignment("a", ident("A")),
					assignment("b", ident("B")),
					assignment("c", ident("C")),
					assignment("d", ident("D")),
					assignment("e", ident("E")),
				]),
			),
		};

		assert_eq!(
			concise_patch_summary(&patch),
			vec![
				"replace block \"X\" (5 entries)",
				"  + a = A",
				"  + b = B",
				"  + c = C",
				"  + … (2 more)",
			]
		);
	}

	#[test]
	fn set_value_summary_shows_old_arrow_new() {
		let patch = ClausewitzPatch::SetValue {
			path: vec![],
			key: "key".to_string(),
			old_value: ident("old"),
			new_value: string("new"),
		};

		assert_eq!(
			concise_patch_summary(&patch),
			vec!["set \"key\": old → \"new\""]
		);
	}

	#[test]
	fn concise_patch_summary_truncates_long_values() {
		let patch = ClausewitzPatch::SetValue {
			path: vec![],
			key: "name".to_string(),
			old_value: string("old"),
			new_value: string(&"x".repeat(100)),
		};

		let summary = concise_patch_summary(&patch);

		assert_eq!(summary.len(), 1);
		assert!(summary[0].ends_with('…'));
		assert!(!summary[0].contains('\n'));
	}

	#[test]
	fn item_helper_builds_block_items() {
		let statement = item(ident("FRA"));
		assert_eq!(statement_value_summary(&statement), "FRA");
	}
}
