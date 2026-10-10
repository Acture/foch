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
	/// The definition left out of the parsed document because this issue has
	/// no trustworthy repair, when the rest of the file is sound.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub isolation: Option<Isolation>,
}

impl ParseIssue {
	/// Whether the document carrying this issue cannot be used as parsed: it
	/// is neither repaired nor confined to one left-out definition.
	pub fn is_fatal(&self) -> bool {
		self.repair.is_none() && self.isolation.is_none()
	}
}

/// Whether a document with these issues cannot be used as parsed: an issue
/// that is a recorded repair or an isolated definition leaves it usable.
pub fn has_fatal_parse_issue(issues: &[ParseIssue]) -> bool {
	issues.iter().any(ParseIssue::is_fatal)
}

/// Whether a document with these issues holds all of its text: every issue
/// is a repair, and no definition was left out.
pub fn reads_completely(issues: &[ParseIssue]) -> bool {
	issues.iter().all(|issue| issue.repair.is_some())
}

/// The definitions these issues leave out of their document.
pub fn isolated_definitions(issues: &[ParseIssue]) -> impl Iterator<Item = &Isolation> {
	issues.iter().filter_map(|issue| issue.isolation.as_ref())
}

/// A top-level definition left out of a parsed document because an error in
/// it has no trustworthy repair. The rest of the document is unaffected.
#[derive(
	Clone,
	Debug,
	Eq,
	PartialEq,
	Serialize,
	Deserialize,
	rkyv::Archive,
	rkyv::Serialize,
	rkyv::Deserialize,
)]
pub struct Isolation {
	/// The definition's key.
	pub definition: String,
	/// The last line the left-out text reaches.
	pub end_line: usize,
	/// One-token repairs that could be meant, the likeliest first.
	pub proposals: Vec<RepairProposal>,
}

/// A one-token repair that could be meant, offered for review.
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
pub struct RepairProposal {
	pub edit: SourceRepairEdit,
	pub line: usize,
	pub column: usize,
	/// The byte offset in the decoded text where the edit applies.
	pub offset: usize,
}

impl RepairProposal {
	pub fn description(self) -> String {
		let edit = match self.edit {
			SourceRepairEdit::RemovedClosingBrace => "leave out the `}`",
			SourceRepairEdit::RemovedOpeningBrace => "leave out the `{`",
			SourceRepairEdit::InsertedClosingBrace => "add a `}`",
			SourceRepairEdit::InsertedOpeningBrace => "add a `{`",
			SourceRepairEdit::ClosedStringAtLineEnd => "end the string",
			SourceRepairEdit::RemovedEmptyAssignment => "leave out the empty assignment",
		};
		format!("{edit} at {}:{}", self.line, self.column)
	}
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
	/// An assignment whose `=` has no value was left out.
	RemovedEmptyAssignment,
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
	/// What is left out held nothing: an assignment with no value.
	NothingLost,
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
			SourceRepairEdit::RemovedEmptyAssignment => "left out an assignment with no value",
		};
		let evidence = match self.evidence {
			SourceRepairEvidence::OnlyReading => "every one-token repair reads the same",
			SourceRepairEvidence::NothingLost => "it held nothing",
			SourceRepairEvidence::OnlySchemaValid => "the only one-token repair the schema accepts",
			SourceRepairEvidence::SmallestChange => {
				"the one-token repair that moves the fewest statements"
			}
		};
		format!("{edit} ({evidence})")
	}
}
