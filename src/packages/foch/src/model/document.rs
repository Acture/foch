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
}
