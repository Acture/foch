//! Source edits shared by content-family adapters.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEdit {
	/// UTF-8 byte offsets in the decoded original source, before any edit.
	pub start: usize,
	pub end: usize,
	pub expected: String,
	pub replacement: String,
}
