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

/// Checks that each edit's range matches its expected text and that no two
/// edits overlap or insert at the same position.
pub(super) fn validate_edits(edits: &[SourceEdit]) -> Result<(), String> {
	if edits.is_empty() {
		return Err("a repair must contain at least one edit".into());
	}
	let mut ordered = edits.iter().collect::<Vec<_>>();
	ordered.sort_by_key(|edit| (edit.start, edit.end));
	for edit in &ordered {
		if edit.end.checked_sub(edit.start) != Some(edit.expected.len()) {
			return Err("repair range must match the UTF-8 byte length of expected text".into());
		}
	}
	for pair in ordered.windows(2) {
		if pair[1].start < pair[0].end || pair[1].start == pair[0].start {
			return Err("repair edits overlap or share an insertion position".into());
		}
	}
	Ok(())
}
