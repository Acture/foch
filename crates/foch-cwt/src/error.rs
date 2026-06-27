use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;

use foch_syntax::{ParseError, ProjectionError};

#[derive(Debug)]
pub enum CwtLoadError {
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
		}
	}
}

impl Error for CwtLoadError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Io { source, .. } => Some(source),
			Self::Syntax(error) => Some(error),
			Self::Projection(error) => Some(error),
			Self::InvalidSchema { .. } => None,
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
