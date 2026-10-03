use crate::input::InputResolveError;
use crate::model::{GamePath, GamePathBuf};
use std::fmt;
use std::path::{Path, PathBuf};

/// What a [`MergeError`] is about, named for people. It is never parsed,
/// compared or joined; each kind of subject keeps its own type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MergeErrorSubject {
	/// A file or directory of the game's filesystem, such as a merge target.
	Game(GamePathBuf),
	/// A file or directory on this host.
	Host(PathBuf),
	/// Something that is not a path, such as a review unit.
	Named(String),
}

impl fmt::Display for MergeErrorSubject {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Game(path) => fmt::Display::fmt(path, f),
			Self::Host(path) => fmt::Display::fmt(&path.display(), f),
			Self::Named(name) => f.write_str(name),
		}
	}
}

impl From<&GamePath> for MergeErrorSubject {
	fn from(path: &GamePath) -> Self {
		Self::Game(path.to_owned())
	}
}

impl From<&Path> for MergeErrorSubject {
	fn from(path: &Path) -> Self {
		Self::Host(path.to_path_buf())
	}
}

#[derive(Debug)]
pub enum MergeError {
	/// The caller cancelled an in-progress merge analysis.
	Cancelled,
	/// Commit would replace an existing non-empty output without authorization.
	ReplacementAuthorizationRequired { path: PathBuf },
	/// The output changed after the caller confirmed the exact replacement token.
	ReplacementTargetChanged { path: PathBuf },
	/// A frozen analysis artifact was modified before commit.
	AnalyzedArtifactChanged,
	/// Output bytes consumed by an analysis-time keep-existing decision changed.
	AnalyzedOutputChanged { path: PathBuf },
	/// Input resolution failed (playlist, game root, base data, profile)
	InputResolve { path: PathBuf, message: String },
	/// Parse failure during IR construction.
	Parse {
		subject: Option<MergeErrorSubject>,
		message: String,
	},
	/// Validation failure (structural merge inputs, revalidation).
	Validation {
		subject: Option<MergeErrorSubject>,
		message: String,
	},
	/// IO error (file system operations)
	Io(std::io::Error),
}

impl fmt::Display for MergeError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Cancelled => write!(f, "merge analysis cancelled"),
			Self::ReplacementAuthorizationRequired { path } => write!(
				f,
				"commit requires explicit authorization to replace non-empty output {}",
				path.display()
			),
			Self::ReplacementTargetChanged { path } => write!(
				f,
				"merge output changed after replacement confirmation: {}",
				path.display()
			),
			Self::AnalyzedArtifactChanged => {
				write!(f, "frozen merge analysis artifacts changed before commit")
			}
			Self::AnalyzedOutputChanged { path } => write!(
				f,
				"output used by merge analysis changed before commit: {}",
				path.display()
			),
			Self::InputResolve { message, .. } => {
				write!(f, "input resolve: {message}")
			}
			Self::Parse { subject, message } => {
				if let Some(subject) = subject {
					write!(f, "parse error in {subject}: {message}")
				} else {
					write!(f, "parse error: {message}")
				}
			}
			Self::Validation { subject, message } => {
				if let Some(subject) = subject {
					write!(f, "validation error in {subject}: {message}")
				} else {
					write!(f, "validation error: {message}")
				}
			}
			Self::Io(e) => write!(f, "io error: {e}"),
		}
	}
}

impl std::error::Error for MergeError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(e) => Some(e),
			_ => None,
		}
	}
}

impl From<std::io::Error> for MergeError {
	fn from(e: std::io::Error) -> Self {
		Self::Io(e)
	}
}

impl From<InputResolveError> for MergeError {
	fn from(e: InputResolveError) -> Self {
		Self::InputResolve {
			path: e.path,
			message: e.message,
		}
	}
}
