use super::GamePathBuf;
use serde::{Deserialize, Serialize};

#[derive(
	Clone,
	Copy,
	Debug,
	Eq,
	PartialEq,
	Serialize,
	Deserialize,
	rkyv::Archive,
	rkyv::Serialize,
	rkyv::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFamily {
	Clausewitz,
	Localisation,
	Csv,
	Json,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentRecord {
	pub mod_id: String,
	pub path: GamePathBuf,
	pub family: DocumentFamily,
	pub parse_ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalisationDefinition {
	pub key: String,
	pub mod_id: String,
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalisationDuplicate {
	pub key: String,
	pub mod_id: String,
	pub path: GamePathBuf,
	pub first_line: usize,
	pub duplicate_line: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiDefinition {
	pub name: String,
	pub mod_id: String,
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceReference {
	pub key: String,
	pub value: String,
	pub mod_id: String,
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CsvRow {
	pub identity: String,
	pub mod_id: String,
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonProperty {
	pub key_path: String,
	pub mod_id: String,
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseIssue {
	pub mod_id: String,
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
	pub message: String,
	/// How the parsed document already accounts for this issue the way the
	/// game's loader does, or `None` when the document cannot be trusted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub repair: Option<SourceRepair>,
}

impl ParseIssue {
	/// Whether the document carrying this issue cannot be used as parsed.
	pub fn is_fatal(&self) -> bool {
		self.repair.is_none()
	}
}

/// Whether a document with these issues cannot be used as parsed: an issue
/// that is a recorded repair leaves the document usable.
pub fn has_fatal_parse_issue(issues: &[ParseIssue]) -> bool {
	issues.iter().any(ParseIssue::is_fatal)
}

/// A minimal compatibility repair of a source file's syntax, applied only to
/// Foch's parsed copy. The source file itself is never changed.
#[derive(
	Clone,
	Copy,
	Debug,
	Eq,
	Hash,
	Ord,
	PartialEq,
	PartialOrd,
	Serialize,
	Deserialize,
	rkyv::Archive,
	rkyv::Serialize,
	rkyv::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SourceRepair {
	/// A `}` at the outermost level of a Clausewitz script that no block
	/// opened was ignored, keeping every statement around it.
	IgnoredUnmatchedClosingBrace,
}

impl SourceRepair {
	pub fn description(self) -> &'static str {
		match self {
			Self::IgnoredUnmatchedClosingBrace => "ignored an unmatched top-level `}`",
		}
	}
}
