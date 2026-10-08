//! Source text handed to the library by its caller.

pub use foch_annotation::source::{RelPath, SourcePosition, SourceSpan};
use serde::{Deserialize, Serialize};

/// Whether EU4 itself loads the file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
	/// A file the game loads. Tests may only appear in `#` comments.
	Inline,
	/// A file below the mod's `tests/` directory, which the game does not load.
	/// It may contain plain `test = { ... }` and `fixture = { ... }` blocks;
	/// files below `tests/events/` are helper events for the test layer only.
	TestsDir,
}

/// The caller reads files; the library only receives their text.
#[derive(Clone, Debug)]
pub struct SourceFile {
	pub mod_name: String,
	pub path: RelPath,
	pub kind: SourceKind,
	pub text: String,
}
