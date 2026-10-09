//! The read-only analysis browser that bare `foch` opens: inspect the current
//! EU4 input, run the complete N-way merge analysis, and browse every review
//! unit. It never records decisions, builds artifacts, or writes output.

mod app;
mod quiet_stderr;
mod render;
mod session;

pub use app::{
	AnalysisSettings, AnalysisView, App, DISPOSITIONS, Focus, OPTION_COUNT, Phase, Screen,
	disposition_label,
};
pub use render::{draw, unit_detail};
pub use session::{BrowserSource, CurrentEu4Source, Inspection, Session, analyze_input};

use std::io::{self, IsTerminal};
use std::sync::Arc;
use std::time::Duration;

use crossterm::cursor;
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
	EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

const TICK: Duration = Duration::from_millis(100);

/// Open the browser on the current EU4 input. Returns the process exit code.
pub fn run() -> io::Result<i32> {
	if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
		eprintln!(
			"error: bare `foch` opens an interactive terminal browser and needs a TTY; run `foch --help` for subcommands"
		);
		return Ok(2);
	}
	// Dropped last, after the terminal is restored.
	let _quiet_stderr = quiet_stderr::QuietStderr::new();
	let mut session = Session::new(Arc::new(CurrentEu4Source));

	enable_raw_mode()?;
	let _guard = TerminalGuard;
	let mut stdout = io::stdout();
	execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
	let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

	loop {
		terminal.draw(|frame| draw(frame, session.app()))?;
		if event::poll(TICK)? {
			match event::read()? {
				Event::Key(key) if !session.handle_key(key) => return Ok(0),
				Event::Resize(..) => terminal.clear()?,
				_ => {}
			}
		}
		session.pump(Duration::ZERO);
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
