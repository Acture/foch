//! Mod-relative paths and positions inside source text.

use foch::game::eu4::script::parser::SpanRange;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A `/`-separated path relative to the mod root, without `.` or `..`
/// segments. It never names a location outside the mod.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
	pub fn new(value: &str) -> Result<Self, String> {
		let normalized = value.replace('\\', "/");
		if normalized.is_empty()
			|| normalized.starts_with('/')
			|| normalized.contains(':')
			|| normalized
				.split('/')
				.any(|segment| segment.is_empty() || segment == "." || segment == "..")
			|| normalized.chars().any(char::is_control)
		{
			return Err(format!("invalid mod-relative path {value:?}"));
		}
		Ok(Self(normalized))
	}

	pub fn as_str(&self) -> &str {
		&self.0
	}

	/// Whether the path lies below `directory` (given without a trailing `/`).
	pub fn is_below(&self, directory: &str) -> bool {
		self.0
			.strip_prefix(directory)
			.is_some_and(|rest| rest.starts_with('/'))
	}

	pub fn file_stem(&self) -> &str {
		let name = self.0.rsplit('/').next().unwrap_or(&self.0);
		name.rsplit_once('.').map_or(name, |(stem, _)| stem)
	}
}

impl TryFrom<String> for RelPath {
	type Error = String;

	fn try_from(value: String) -> Result<Self, String> {
		Self::new(&value)
	}
}

impl From<RelPath> for String {
	fn from(value: RelPath) -> Self {
		value.0
	}
}

impl fmt::Display for RelPath {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.0)
	}
}

/// UTF-8 byte offset with 1-based line and column, as produced by the parser.
/// Editors convert to their own encoding at their boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct SourcePosition {
	pub offset: usize,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSpan {
	pub path: RelPath,
	pub start: SourcePosition,
	pub end: SourcePosition,
}

impl SourceSpan {
	pub fn from_parser(path: &RelPath, span: &SpanRange) -> Self {
		let position = |point: &foch::game::eu4::script::parser::Span| SourcePosition {
			offset: point.offset,
			line: point.line,
			column: point.column,
		};
		Self {
			path: path.clone(),
			start: position(&span.start),
			end: position(&span.end),
		}
	}
}

impl fmt::Display for SourceSpan {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			formatter,
			"{}:{}:{}",
			self.path, self.start.line, self.start.column
		)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn relative_paths_stay_inside_the_mod() {
		assert_eq!(
			RelPath::new("events\\a.txt").unwrap().as_str(),
			"events/a.txt"
		);
		for bad in ["", "/abs", "a/../b", "./a", "a//b", "C:/x", "a\nb"] {
			assert!(RelPath::new(bad).is_err(), "{bad:?}");
		}
		let path = RelPath::new("tests/events/helpers.txt").unwrap();
		assert!(path.is_below("tests"));
		assert!(path.is_below("tests/events"));
		assert!(!RelPath::new("testsuite/a.txt").unwrap().is_below("tests"));
		assert_eq!(path.file_stem(), "helpers");
	}
}
