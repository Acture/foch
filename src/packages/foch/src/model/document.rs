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
	/// The edit the parsed document already includes to get past this
	/// issue, at `line`:`column`, or `None` when it cannot be trusted.
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

/// A one-token repair of a source file's syntax, applied only to Foch's
/// parsed copy. The source file itself is never changed.
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
pub struct SourceRepair {
	pub edit: SourceRepairEdit,
	pub evidence: SourceRepairEvidence,
}

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
pub enum SourceRepairEdit {
	/// A `}` that no block opened was left out.
	RemovedClosingBrace,
	/// A `{` that nothing closed was left out.
	RemovedOpeningBrace,
	/// A missing `}` was added.
	InsertedClosingBrace,
	/// A missing `{` was added.
	InsertedOpeningBrace,
	/// A string missing its closing quote was ended at its line's end.
	ClosedStringAtLineEnd,
}

/// Why a one-token repair is the reading of the text.
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
pub enum SourceRepairEvidence {
	/// Every one-token edit that makes the text parse gives the same tree.
	OnlyReading,
	/// Of the trees one-token edits give, one alone has the fewest values
	/// whose block-or-scalar shape the schema for the file rejects.
	OnlySchemaValid,
	/// Of the trees one-token edits give, one moves fewer statements to
	/// another parent than any other, measured from the tree the text gives
	/// unedited, and the indentation does not clearly favour another.
	SmallestChange,
}

impl SourceRepair {
	pub fn description(self) -> String {
		let edit = match self.edit {
			SourceRepairEdit::RemovedClosingBrace => "left out an unmatched `}`",
			SourceRepairEdit::RemovedOpeningBrace => "left out an unclosed `{`",
			SourceRepairEdit::InsertedClosingBrace => "added a missing `}`",
			SourceRepairEdit::InsertedOpeningBrace => "added a missing `{`",
			SourceRepairEdit::ClosedStringAtLineEnd => {
				"ended an unterminated string at the end of its line"
			}
		};
		let evidence = match self.evidence {
			SourceRepairEvidence::OnlyReading => "every one-token repair reads the same",
			SourceRepairEvidence::OnlySchemaValid => "the only one-token repair the schema accepts",
			SourceRepairEvidence::SmallestChange => {
				"the one-token repair that moves the fewest statements"
			}
		};
		format!("{edit} ({evidence})")
	}
}
