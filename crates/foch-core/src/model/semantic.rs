use super::document::{
	CsvRow, DocumentRecord, JsonProperty, LocalisationDefinition, LocalisationDuplicate,
	ParseIssue, ResourceReference, UiDefinition,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(
	Clone,
	Copy,
	Debug,
	Eq,
	PartialEq,
	Hash,
	Serialize,
	Deserialize,
	rkyv::Archive,
	rkyv::Serialize,
	rkyv::Deserialize,
)]
pub enum SymbolKind {
	ScriptedEffect,
	ScriptedTrigger,
	Event,
	Decision,
	DiplomaticAction,
	TriggeredModifier,
}

#[derive(
	Clone,
	Copy,
	Debug,
	Eq,
	PartialEq,
	Hash,
	Serialize,
	Deserialize,
	rkyv::Archive,
	rkyv::Serialize,
	rkyv::Deserialize,
)]
pub enum ScopeType {
	Country,
	Province,
	Unknown,
}

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
pub enum ScopeKind {
	File,
	Event,
	Decision,
	ScriptedEffect,
	Trigger,
	Effect,
	Loop,
	AliasBlock,
	Block,
}

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
pub struct SourceSpan {
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopeNode {
	pub id: usize,
	pub kind: ScopeKind,
	pub parent: Option<usize>,
	pub this_type: ScopeType,
	pub aliases: HashMap<String, ScopeType>,
	pub mod_id: String,
	pub path: PathBuf,
	pub span: SourceSpan,
	/// The block key that created this scope (e.g. "multiply_variable", "OR").
	/// Empty for file-level scopes.
	#[serde(default)]
	pub key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolDefinition {
	pub kind: SymbolKind,
	pub name: String,
	pub module: String,
	pub local_name: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
	pub declared_this_type: ScopeType,
	pub inferred_this_type: ScopeType,
	#[serde(default)]
	pub inferred_this_mask: u8,
	pub required_params: Vec<String>,
	#[serde(default)]
	pub optional_params: Vec<String>,
	#[serde(default)]
	pub param_contract: Option<ParamContract>,
	#[serde(default)]
	pub scope_param_names: Vec<String>,
}

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
pub struct ParamBinding {
	pub name: String,
	pub value: String,
}

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
pub struct ConditionalParamRule {
	pub when_present: String,
	pub requires_any_of: Vec<String>,
}

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
pub struct ParamContract {
	pub required_all: Vec<String>,
	pub optional: Vec<String>,
	pub one_of_groups: Vec<Vec<String>>,
	pub conditional_required: Vec<ConditionalParamRule>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolReference {
	pub kind: SymbolKind,
	pub name: String,
	pub module: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
	pub provided_params: Vec<String>,
	pub param_bindings: Vec<ParamBinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AliasUsage {
	pub alias: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyUsage {
	pub key: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
	pub this_type: ScopeType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScalarAssignment {
	pub key: String,
	pub value: String,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SemanticIndex {
	pub documents: Vec<DocumentRecord>,
	pub scopes: Vec<ScopeNode>,
	pub definitions: Vec<SymbolDefinition>,
	pub references: Vec<SymbolReference>,
	pub alias_usages: Vec<AliasUsage>,
	pub key_usages: Vec<KeyUsage>,
	pub scalar_assignments: Vec<ScalarAssignment>,
	pub localisation_definitions: Vec<LocalisationDefinition>,
	pub localisation_duplicates: Vec<LocalisationDuplicate>,
	pub ui_definitions: Vec<UiDefinition>,
	pub resource_references: Vec<ResourceReference>,
	pub csv_rows: Vec<CsvRow>,
	pub json_properties: Vec<JsonProperty>,
	pub parse_issues: Vec<ParseIssue>,
}
