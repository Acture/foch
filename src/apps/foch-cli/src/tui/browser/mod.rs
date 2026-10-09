//! The read-only analysis browser that bare `foch` opens: inspect the current
//! EU4 input, run the complete N-way merge analysis, and browse every review
//! unit. It never records decisions, builds artifacts, or writes output.

mod app;
mod quiet_stderr;
mod render;
mod session;

pub use app::{
	AnalysisSettings, AnalysisView, App, DISPOSITIONS, Effect, Focus, HitAreas, InputPane,
	KEY_HELP, OPTION_COUNT, Phase, Screen, disposition_label,
};
pub use render::{draw, selected_text, unit_detail};
pub use session::{
	AnalysisInput, BrowserSource, CurrentEu4Source, Inspection, Session, analyze_input,
};

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::time::Duration;

use crossterm::cursor;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
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
	execute!(
		stdout,
		EnterAlternateScreen,
		EnableMouseCapture,
		cursor::Hide
	)?;
	let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

	loop {
		let drawn = terminal.draw(|frame| draw(frame, session.app()))?;
		if let Some(text) = session.take_selection(drawn.buffer)
			&& let Err(error) = copy_to_clipboard(&text, terminal.backend_mut())
		{
			session.effect_failed(error);
		}
		if event::poll(TICK)? {
			match event::read()? {
				Event::Key(key) if !session.handle_key(key) => return Ok(0),
				Event::Mouse(mouse) => session.handle_mouse(mouse),
				Event::Resize(..) => terminal.clear()?,
				_ => {}
			}
		}
		if let Some(effect) = session.take_effect()
			&& let Err(error) = carry_out(effect, terminal.backend_mut())
		{
			session.effect_failed(error);
		}
		session.pump(Duration::ZERO);
	}
}

fn carry_out(effect: Effect, out: &mut impl Write) -> Result<(), String> {
	match effect {
		Effect::Copy(text) => copy_to_clipboard(&text, out),
		Effect::OpenWorkshop(targets) => crate::cli::handler::input::open_workshop_pages(&targets)
			.map(|_| ())
			.map_err(|error| format!("could not open Steam: {error}")),
		Effect::MouseCapture(true) => {
			execute!(out, EnableMouseCapture).map_err(|error| error.to_string())
		}
		Effect::MouseCapture(false) => {
			execute!(out, DisableMouseCapture).map_err(|error| error.to_string())
		}
	}
}

/// Copy through the terminal (OSC 52), which also works over SSH. The legacy
/// Windows console has no OSC 52, so Windows falls back to `clip.exe`.
fn copy_to_clipboard(text: &str, out: &mut impl Write) -> Result<(), String> {
	use crossterm::clipboard::CopyToClipboard;
	match execute!(out, CopyToClipboard::to_clipboard_from(text)) {
		Ok(()) => Ok(()),
		Err(_) if cfg!(windows) => copy_with_clip_exe(text),
		Err(error) => Err(format!("could not copy: {error}")),
	}
}

fn copy_with_clip_exe(text: &str) -> Result<(), String> {
	use std::process::{Command, Stdio};
	let mut child = Command::new("clip")
		.stdin(Stdio::piped())
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.spawn()
		.map_err(|error| format!("could not copy: {error}"))?;
	// clip.exe reads UTF-16LE text that starts with a byte order mark.
	let bytes = std::iter::once(0xFEFF_u16)
		.chain(text.encode_utf16())
		.flat_map(u16::to_le_bytes)
		.collect::<Vec<_>>();
	let written = match child.stdin.take() {
		Some(mut stdin) => stdin.write_all(&bytes),
		None => Ok(()),
	};
	let waited = child.wait();
	written
		.and(waited)
		.map(|_| ())
		.map_err(|error| format!("could not copy: {error}"))
}

struct TerminalGuard;

impl Drop for TerminalGuard {
	fn drop(&mut self) {
		let _ = disable_raw_mode();
		let mut stdout = io::stdout();
		let _ = execute!(
			stdout,
			DisableMouseCapture,
			cursor::Show,
			LeaveAlternateScreen
		);
	}
}
