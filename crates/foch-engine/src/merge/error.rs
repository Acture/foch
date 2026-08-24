use crate::workspace::resolve::WorkspaceResolveError;
use std::fmt;
use std::path::PathBuf;

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
	/// Workspace resolution failed (playlist, game root, base data, profile)
	WorkspaceResolve { path: PathBuf, message: String },
	/// Parse failure during IR construction
	Parse {
		path: Option<String>,
		message: String,
	},
	/// Validation failure (structural merge inputs, revalidation)
	Validation {
		path: Option<String>,
		message: String,
	},
	/// Emit failure (Clausewitz output generation)
	Emit {
		path: Option<String>,
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
			Self::WorkspaceResolve { message, .. } => {
				write!(f, "workspace resolve: {message}")
			}
			Self::Parse { path, message } => {
				if let Some(p) = path {
					write!(f, "parse error in {p}: {message}")
				} else {
					write!(f, "parse error: {message}")
				}
			}
			Self::Validation { path, message } => {
				if let Some(p) = path {
					write!(f, "validation error in {p}: {message}")
				} else {
					write!(f, "validation error: {message}")
				}
			}
			Self::Emit { path, message } => {
				if let Some(p) = path {
					write!(f, "emit error in {p}: {message}")
				} else {
					write!(f, "emit error: {message}")
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

impl From<WorkspaceResolveError> for MergeError {
	fn from(e: WorkspaceResolveError) -> Self {
		Self::WorkspaceResolve {
			path: e.path,
			message: e.message,
		}
	}
}
