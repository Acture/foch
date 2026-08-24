use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;

use super::syntax::ByteSpan;

#[derive(Debug)]
pub(crate) enum ParseError {
	Language(String),
	ParseReturnedNone,
}

impl Display for ParseError {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::Language(message) => write!(f, "failed to load paradox language: {message}"),
			Self::ParseReturnedNone => write!(f, "tree-sitter returned no parse tree"),
		}
	}
}

impl Error for ParseError {}

#[derive(Debug)]
pub(crate) enum ProjectionError {
	MissingField {
		node_kind: &'static str,
		field: &'static str,
	},
	UnexpectedNode {
		kind: String,
		span: ByteSpan,
	},
}

impl Display for ProjectionError {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::MissingField { node_kind, field } => {
				write!(f, "missing field `{field}` on `{node_kind}` node")
			}
			Self::UnexpectedNode { kind, span } => write!(
				f,
				"unexpected node `{kind}` at byte span {}..{}",
				span.start, span.end,
			),
		}
	}
}

impl Error for ProjectionError {}

#[derive(Debug)]
pub(crate) enum CwtLoadError {
	Io {
		path: PathBuf,
		source: std::io::Error,
	},
	Syntax(ParseError),
	Projection(ProjectionError),
	InvalidSchema {
		path: Option<PathBuf>,
		message: String,
	},
	Codec {
		message: String,
	},
}

impl Display for CwtLoadError {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io { path, source } => {
				write!(f, "failed to read `{}`: {source}", path.display())
			}
			Self::Syntax(error) => write!(f, "syntax parse failed: {error}"),
			Self::Projection(error) => write!(f, "syntax projection failed: {error}"),
			Self::InvalidSchema { path, message } => {
				if let Some(path) = path {
					write!(f, "invalid schema `{}`: {message}", path.display())
				} else {
					write!(f, "invalid schema: {message}")
				}
			}
			Self::Codec { message } => write!(f, "compiled schema codec failed: {message}"),
		}
	}
}

impl Error for CwtLoadError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Io { source, .. } => Some(source),
			Self::Syntax(error) => Some(error),
			Self::Projection(error) => Some(error),
			Self::InvalidSchema { .. } | Self::Codec { .. } => None,
		}
	}
}

impl From<ParseError> for CwtLoadError {
	fn from(value: ParseError) -> Self {
		Self::Syntax(value)
	}
}

impl From<ProjectionError> for CwtLoadError {
	fn from(value: ProjectionError) -> Self {
		Self::Projection(value)
	}
}
