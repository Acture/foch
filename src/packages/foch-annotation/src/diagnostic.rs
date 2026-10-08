//! Problems in annotations, located in the original source.

use crate::source::SourceSpan;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
	Error,
	Warning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Code {
	/// The surrounding script does not parse.
	Parse,
	/// Malformed annotation syntax, such as an unterminated `#test(`.
	Malformed,
	/// A comment shaped like an annotation with an unknown name.
	UnknownAnnotation,
	UnknownParameter,
	MissingParameter,
	DuplicateParameter,
	InvalidValue,
	/// Not followed by what it applies to.
	Orphan,
	/// Inside a definition rather than before it.
	Nested,
	/// Attached to a definition the annotation does not support.
	UnsupportedTarget,
	DuplicateModifier,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
	pub code: Code,
	pub severity: Severity,
	pub span: SourceSpan,
	pub message: String,
}

impl Diagnostic {
	pub(crate) fn error(code: Code, span: SourceSpan, message: impl Into<String>) -> Self {
		Self {
			code,
			severity: Severity::Error,
			span,
			message: message.into(),
		}
	}
}

impl fmt::Display for Diagnostic {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		let severity = match self.severity {
			Severity::Error => "error",
			Severity::Warning => "warning",
		};
		write!(formatter, "{}: {severity}: {}", self.span, self.message)
	}
}
