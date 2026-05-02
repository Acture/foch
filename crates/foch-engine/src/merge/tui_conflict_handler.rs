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
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};
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
use super::patch::ClausewitzPatch;
use super::patch_merge::{PatchAddress, PatchConflict};

const ACTION_COUNT: usize = 4;
const MAX_SUMMARY_CHARS: usize = 80;
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
	summary: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConflictResolver {
	pub current_conflict_index: usize,
	pub total_conflicts: usize,
	file_path: PathBuf,
	address_path: Vec<String>,
	reason: String,
	conflict_id: String,
	candidates: Vec<ConflictCandidate>,
	selected_index: usize,
}

impl ConflictResolver {
	pub fn new(
		conflict: &PatchConflict,
		file: &Path,
		address: &PatchAddress,
		conflict_id: &str,
		mod_displayname_lookup: &HashMap<String, String>,
		current_idx: usize,
		total: usize,
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
			file_path: file.to_path_buf(),
			address_path,
			reason: conflict.reason.clone(),
			conflict_id: conflict_id.to_string(),
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
			render_choice_list(self, buf, list_area);
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
}

impl InteractiveTuiHandler {
	pub fn new(
		current_file: PathBuf,
		config_writer: Box<dyn ConfigWriter>,
		mod_displayname_lookup: HashMap<String, String>,
		current_conflict_index: usize,
		total_conflicts: usize,
	) -> Self {
		Self {
			current_file,
			config_writer,
			mod_displayname_lookup,
			current_conflict_index,
			total_conflicts,
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
}

fn run_resolver(resolver: &mut ConflictResolver) -> io::Result<ConflictAction> {
	enable_raw_mode()?;
	let _guard = TerminalGuard;
	let mut stdout = io::stdout();
	execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
	let backend = CrosstermBackend::new(stdout);
	let mut terminal = Terminal::new(backend)?;

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
		lines.push(ChoiceLine {
			item_index: None,
			text: format!("      {}", candidate.summary),
			style: Style::default().fg(Color::Gray),
		});
	}
	lines.push(ChoiceLine {
		item_index: None,
		text: "  ─────".to_string(),
		style: Style::default().fg(Color::DarkGray),
	});
	for (offset, label) in [
		"[d] defer",
		"[s] external file",
		"[k] keep existing",
		"[q] abort",
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

fn render_progress_gauge(area: Rect, buf: &mut Buffer, current: usize, total: usize) {
	if area.width == 0 || area.height == 0 {
		return;
	}
	let total = total.max(1);
	let current = current.min(total);
	let ratio = current as f64 / total as f64;
	let percent = (ratio * 100.0).round() as u16;
	let label = format!("conflict {current}/{total}  ({percent}%)");
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

fn concise_patch_summary(patch: &ClausewitzPatch) -> String {
	match patch {
		ClausewitzPatch::SetValue { key, new_value, .. } => {
			format!("set \"{key}\" = {}", value_summary(new_value))
		}
		ClausewitzPatch::RemoveNode { key, .. } => format!("remove \"{key}\""),
		ClausewitzPatch::InsertNode { key, statement, .. } => {
			format!("insert \"{key}\" = {}", statement_value_summary(statement))
		}
		ClausewitzPatch::ReplaceBlock {
			key, new_statement, ..
		} => format!(
			"replace block \"{key}\" with {} entries",
			statement_entry_count(new_statement)
		),
		ClausewitzPatch::AppendListItem { key, value, .. } => {
			format!("append to list \"{key}\": {}", value_summary(value))
		}
		ClausewitzPatch::RemoveListItem { key, value, .. } => {
			format!("remove from list \"{key}\": {}", value_summary(value))
		}
		ClausewitzPatch::AppendBlockItem { value, .. } => {
			format!("append item: {}", value_summary(value))
		}
		ClausewitzPatch::RemoveBlockItem { value, .. } => {
			format!("remove item: {}", value_summary(value))
		}
		ClausewitzPatch::Rename {
			old_key, new_key, ..
		} => format!("rename \"{old_key}\" → \"{new_key}\""),
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
		);
		let area = Rect::new(0, 0, 76, 19);
		let mut actual = Buffer::empty(area);
		Widget::render(&resolver, area, &mut actual);
		let actual_lines = buffer_lines(&actual);

		// Gauge content (line 1) is checked separately via render_progress_gauge_*
		// tests because its exact block-fill symbols are sensitive to ratatui's
		// internal partial-cell glyph table. Confirm the structural rest.
		let rest = [
			actual_lines[0].as_str(),
			actual_lines[2].as_str(),
			actual_lines[3].as_str(),
			actual_lines[4].as_str(),
			actual_lines[5].as_str(),
			actual_lines[6].as_str(),
			actual_lines[7].as_str(),
			actual_lines[8].as_str(),
			actual_lines[9].as_str(),
			actual_lines[10].as_str(),
			actual_lines[11].as_str(),
			actual_lines[12].as_str(),
			actual_lines[13].as_str(),
			actual_lines[14].as_str(),
			actual_lines[15].as_str(),
			actual_lines[16].as_str(),
			actual_lines[17].as_str(),
			actual_lines[18].as_str(),
		];
		let expected = [
			"┌ foch merge: conflict 1/30 ───────────────────────────────────────────────┐",
			"│events/FlavorFRA.txt                                                      │",
			"│  flavor_fra.3135 / option / define_advisor / name                        │",
			"│                                                                          │",
			"│reason: sibling mods set the same scalar to divergent values              │",
			"├──────────────────────────────────────────────────────────────────────────┤",
			"│  [1] Europa Expanded (2164202838, prec 0)                                │",
			"│      set \"name\" = \"Charles-Francois de Broglie\"                          │",
			"│  [2] Chinese Language Supp. (1999055990, prec 2)                         │",
			"│      set \"name\" = \"Chinese localization\"                                 │",
			"│  ─────                                                                   │",
			"│❯ [d] defer                                                               │",
			"│  [s] external file                                                       │",
			"│  [k] keep existing                                                       │",
			"│  [q] abort                                                               │",
			"├──────────────────────────────────────────────────────────────────────────┤",
			"│↑↓ select  Enter confirm  Esc/d defer  Q abort  S file  K keep            │",
			"└──────────────────────────────────────────────────────────────────────────┘",
		];
		assert_eq!(rest, expected);

		// Gauge line shape: starts with │, ends with │, contains the textual label.
		let gauge_line = actual_lines[1].as_str();
		assert!(
			gauge_line.starts_with('│') && gauge_line.ends_with('│'),
			"gauge line not bordered: {gauge_line:?}"
		);
		assert!(
			gauge_line.contains("conflict 1/30"),
			"gauge missing label: {gauge_line:?}"
		);
		assert!(
			gauge_line.contains("(3%)"),
			"gauge missing percent: {gauge_line:?}"
		);
	}

	#[test]
	fn render_progress_gauge_renders_empty_at_zero() {
		let area = Rect::new(0, 0, 30, 1);
		let mut buf = Buffer::empty(area);
		render_progress_gauge(area, &mut buf, 0, 10);
		let line: String = (0..area.width)
			.map(|x| buf[(area.x + x, area.y)].symbol())
			.collect::<String>();
		assert!(
			line.contains("conflict 0/10") && line.contains("(0%)"),
			"unexpected gauge line: {line:?}"
		);
	}

	#[test]
	fn render_progress_gauge_handles_zero_total_safely() {
		let area = Rect::new(0, 0, 30, 1);
		let mut buf = Buffer::empty(area);
		render_progress_gauge(area, &mut buf, 0, 0);
		let line: String = (0..area.width)
			.map(|x| buf[(area.x + x, area.y)].symbol())
			.collect::<String>();
		assert!(
			line.contains("conflict 0/1") && line.contains("(0%)"),
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
				"set \"name\" = \"new\\nvalue\"",
			),
			(
				ClausewitzPatch::RemoveNode {
					path: vec![],
					key: "owner".to_string(),
					removed: assignment("owner", ident("FRA")),
				},
				"remove \"owner\"",
			),
			(
				ClausewitzPatch::InsertNode {
					path: vec![],
					key: "owner".to_string(),
					statement: assignment("owner", ident("FRA")),
				},
				"insert \"owner\" = FRA",
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
				"replace block \"option\" with 2 entries",
			),
			(
				ClausewitzPatch::AppendListItem {
					path: vec![],
					key: "tag".to_string(),
					value: ident("FRA"),
				},
				"append to list \"tag\": FRA",
			),
			(
				ClausewitzPatch::RemoveListItem {
					path: vec![],
					key: "tag".to_string(),
					value: ident("ENG"),
				},
				"remove from list \"tag\": ENG",
			),
			(
				ClausewitzPatch::AppendBlockItem {
					path: vec![],
					value: ident("FRA"),
				},
				"append item: FRA",
			),
			(
				ClausewitzPatch::RemoveBlockItem {
					path: vec![],
					value: ident("ENG"),
				},
				"remove item: ENG",
			),
			(
				ClausewitzPatch::Rename {
					path: vec![],
					old_key: "old".to_string(),
					new_key: "new".to_string(),
				},
				"rename \"old\" → \"new\"",
			),
		];

		for (patch, expected) in patches {
			assert_eq!(concise_patch_summary(&patch), expected);
		}
		Ok(())
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

		assert!(summary.ends_with('…'));
		assert!(!summary.contains('\n'));
	}

	#[test]
	fn item_helper_builds_block_items() {
		let statement = item(ident("FRA"));
		assert_eq!(statement_value_summary(&statement), "FRA");
	}
}
