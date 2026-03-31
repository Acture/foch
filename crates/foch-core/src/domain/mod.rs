pub mod descriptor;
pub mod game;
pub mod playlist;

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseErrorKind {
	Io,
	Format,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
	pub kind: ParseErrorKind,
	pub path: PathBuf,
	pub message: String,
}

impl ParseError {
	pub fn io(path: PathBuf, source: std::io::Error) -> Self {
		Self {
			kind: ParseErrorKind::Io,
			path,
			message: source.to_string(),
		}
	}

	pub fn format(path: PathBuf, message: impl Into<String>) -> Self {
		Self {
			kind: ParseErrorKind::Format,
			path,
			message: message.into(),
		}
	}
}

impl Display for ParseError {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}: {}", self.path.display(), self.message)
	}
}

impl Error for ParseError {}
