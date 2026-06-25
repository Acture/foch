use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::ByteSpan;

#[derive(Debug)]
pub enum ParseError {
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
pub enum ProjectionError {
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
