use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
	pub path: PathBuf,
	pub family: DocumentFamily,
	pub parse_ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalisationDefinition {
	pub key: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalisationDuplicate {
	pub key: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub first_line: usize,
	pub duplicate_line: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiDefinition {
	pub name: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceReference {
	pub key: String,
	pub value: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CsvRow {
	pub identity: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonProperty {
	pub key_path: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ParseIssue {
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
	pub message: String,
}
