use crate::workspace::FileFilter;
use foch_core::domain::game::Game;
use foch_core::model::{
	AliasUsage, CsvRow, DocumentFamily, DocumentRecord, JsonProperty, KeyUsage,
	LocalisationDefinition, LocalisationDuplicate, ParamBinding, ParamContract, ParseIssue,
	ResourceReference, ScalarAssignment, ScopeKind, ScopeNode, ScopeType, SemanticIndex,
	SourceSpan, SymbolDefinition, SymbolKind, SymbolReference, UiDefinition,
};
use foch_language::analyzer::content_family::{GameProfile, ScriptFileKind};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstFile, AstStatement};
use foch_language::analyzer::semantic_index::ParsedScriptFile;
use rkyv::util::AlignedVec;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Bump when the mod-level cached payload becomes wire-incompatible or parser /
/// semantic-index behavior changes in a way that should invalidate old entries.
pub const MOD_PARSE_CACHE_VERSION: u32 = 1;
const DEFAULT_CACHE_DIR_NAME: &str = "mods";
const CACHE_ENV: &str = "FOCH_MOD_PARSE_CACHE_DIR";
const ROOT_CACHE_ENV: &str = "FOCH_CACHE_DIR";
const HASH_HEX_LEN: usize = 16;

#[derive(Clone, Debug)]
pub struct CachedModData {
	pub semantic_index: SemanticIndex,
	/// Parsed Clausewitz documents are stored so runtime overlap can normalize
	/// definitions without reparsing unchanged mods. This vector may be empty for
	/// cache files written by older or intentionally semantic-only writers.
	pub parsed_documents: Vec<ParsedScriptFile>,
}

#[derive(Clone, Debug)]
pub struct ModParseCache {
	root: PathBuf,
}

#[derive(Debug)]
pub enum CacheError {
	Io(io::Error),
	Encode(String),
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredCachedModData {
	cache_version: u32,
	mod_hash: String,
	foch_version: String,
	game_version: String,
	semantic_index: StoredSemanticIndex,
	parsed_documents: Vec<u8>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredParsedScriptFile {
	mod_id: String,
	path: String,
	relative_path: String,
	file_kind: ScriptFileKind,
	module_name: String,
	ast: StoredAstFile,
	source: String,
	parse_issues: Vec<StoredParseIssue>,
	parse_cache_hit: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredAstFile {
	path: String,
	statements: Vec<AstStatement>,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredSemanticIndex {
	documents: Vec<StoredDocumentRecord>,
	scopes: Vec<StoredScopeNode>,
	definitions: Vec<StoredSymbolDefinition>,
	references: Vec<StoredSymbolReference>,
	alias_usages: Vec<StoredAliasUsage>,
	key_usages: Vec<StoredKeyUsage>,
	scalar_assignments: Vec<StoredScalarAssignment>,
	localisation_definitions: Vec<StoredLocalisationDefinition>,
	localisation_duplicates: Vec<StoredLocalisationDuplicate>,
	ui_definitions: Vec<StoredUiDefinition>,
	resource_references: Vec<StoredResourceReference>,
	csv_rows: Vec<StoredCsvRow>,
	json_properties: Vec<StoredJsonProperty>,
	parse_issues: Vec<StoredParseIssue>,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredDocumentRecord {
	mod_id: String,
	path: String,
	family: DocumentFamily,
	parse_ok: bool,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredScopeNode {
	id: usize,
	kind: ScopeKind,
	parent: Option<usize>,
	this_type: ScopeType,
	aliases: std::collections::HashMap<String, ScopeType>,
	mod_id: String,
	path: String,
	span: SourceSpan,
	key: String,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredSymbolDefinition {
	kind: SymbolKind,
	name: String,
	module: String,
	local_name: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
	scope_id: usize,
	declared_this_type: ScopeType,
	inferred_this_type: ScopeType,
	inferred_this_mask: u8,
	inferred_from_mask: u8,
	inferred_root_mask: u8,
	required_params: Vec<String>,
	optional_params: Vec<String>,
	param_contract: Option<ParamContract>,
	scope_param_names: Vec<String>,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredSymbolReference {
	kind: SymbolKind,
	name: String,
	module: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
	scope_id: usize,
	provided_params: Vec<String>,
	param_bindings: Vec<ParamBinding>,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredAliasUsage {
	alias: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
	scope_id: usize,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredKeyUsage {
	key: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
	scope_id: usize,
	this_type: ScopeType,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredScalarAssignment {
	key: String,
	value: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
	scope_id: usize,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredLocalisationDefinition {
	key: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredLocalisationDuplicate {
	key: String,
	mod_id: String,
	path: String,
	first_line: usize,
	duplicate_line: usize,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredUiDefinition {
	name: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredResourceReference {
	key: String,
	value: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredCsvRow {
	identity: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredJsonProperty {
	key_path: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
}

#[derive(
	Clone,
	Debug,
	serde::Serialize,
	serde::Deserialize,
	rkyv::Archive,
	rkyv::Serialize,
	rkyv::Deserialize,
)]
struct StoredParseIssue {
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
	message: String,
}

impl ModParseCache {
	pub fn open(cache_dir: &Path) -> Self {
		let _ = fs::create_dir_all(cache_dir);
		Self {
			root: cache_dir.to_path_buf(),
		}
	}

	pub fn open_default() -> Self {
		Self::open(&default_mod_parse_cache_dir())
	}

	pub fn lookup(
		&self,
		mod_hash: &str,
		foch_version: &str,
		game_version: &str,
	) -> Option<CachedModData> {
		self.lookup_with_cache_version(
			MOD_PARSE_CACHE_VERSION,
			mod_hash,
			foch_version,
			game_version,
		)
	}

	pub fn store(
		&self,
		mod_hash: &str,
		foch_version: &str,
		game_version: &str,
		data: &CachedModData,
	) -> Result<(), CacheError> {
		self.store_with_cache_version(
			MOD_PARSE_CACHE_VERSION,
			mod_hash,
			foch_version,
			game_version,
			data,
		)
	}

	fn lookup_with_cache_version(
		&self,
		cache_version: u32,
		mod_hash: &str,
		foch_version: &str,
		game_version: &str,
	) -> Option<CachedModData> {
		let path = self.cache_file(cache_version, mod_hash, foch_version, game_version);
		let raw = fs::read(path).ok()?;
		let stored = decode_payload(&raw).ok()?;
		if stored.cache_version != cache_version
			|| stored.mod_hash != mod_hash
			|| stored.foch_version != foch_version
			|| stored.game_version != game_version
		{
			return None;
		}
		stored.into_cached_mod_data().ok()
	}

	fn store_with_cache_version(
		&self,
		cache_version: u32,
		mod_hash: &str,
		foch_version: &str,
		game_version: &str,
		data: &CachedModData,
	) -> Result<(), CacheError> {
		fs::create_dir_all(&self.root).map_err(CacheError::Io)?;
		let payload = StoredCachedModData::from_cached_mod_data(
			cache_version,
			mod_hash,
			foch_version,
			game_version,
			data,
		)?;
		let encoded = encode_payload(&payload)?;
		let path = self.cache_file(cache_version, mod_hash, foch_version, game_version);
		let tmp = path.with_extension(format!("rkyv.{}.tmp", std::process::id()));
		fs::write(&tmp, encoded.as_slice()).map_err(CacheError::Io)?;
		fs::rename(&tmp, &path).map_err(|err| {
			let _ = fs::remove_file(&tmp);
			CacheError::Io(err)
		})?;
		Ok(())
	}

	fn cache_file(
		&self,
		cache_version: u32,
		mod_hash: &str,
		foch_version: &str,
		game_version: &str,
	) -> PathBuf {
		let filename = cache_filename(cache_version, mod_hash, foch_version, game_version);
		self.root.join(filename)
	}
}

impl CacheError {
	fn encode(err: impl fmt::Display) -> Self {
		Self::Encode(err.to_string())
	}
}

impl fmt::Display for CacheError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io(err) => write!(f, "{err}"),
			Self::Encode(err) => write!(f, "{err}"),
		}
	}
}

impl std::error::Error for CacheError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(err) => Some(err),
			Self::Encode(_) => None,
		}
	}
}

impl From<io::Error> for CacheError {
	fn from(value: io::Error) -> Self {
		Self::Io(value)
	}
}

impl StoredCachedModData {
	fn from_cached_mod_data(
		cache_version: u32,
		mod_hash: &str,
		foch_version: &str,
		game_version: &str,
		data: &CachedModData,
	) -> Result<Self, CacheError> {
		Ok(Self {
			cache_version,
			mod_hash: mod_hash.to_string(),
			foch_version: foch_version.to_string(),
			game_version: game_version.to_string(),
			semantic_index: StoredSemanticIndex::from_semantic_index(&data.semantic_index),
			parsed_documents: encode_parsed_documents(&data.parsed_documents)?,
		})
	}

	fn into_cached_mod_data(self) -> Result<CachedModData, CacheError> {
		Ok(CachedModData {
			semantic_index: self.semantic_index.into_semantic_index(),
			parsed_documents: decode_parsed_documents(&self.parsed_documents)?,
		})
	}
}

fn encode_parsed_documents(documents: &[ParsedScriptFile]) -> Result<Vec<u8>, CacheError> {
	let stored = documents
		.iter()
		.map(StoredParsedScriptFile::from_parsed_script_file)
		.collect::<Vec<_>>();
	bincode::serialize(&stored).map_err(CacheError::encode)
}

fn decode_parsed_documents(bytes: &[u8]) -> Result<Vec<ParsedScriptFile>, CacheError> {
	let stored =
		bincode::deserialize::<Vec<StoredParsedScriptFile>>(bytes).map_err(CacheError::encode)?;
	Ok(stored
		.into_iter()
		.map(StoredParsedScriptFile::into_parsed_script_file)
		.collect())
}

impl StoredParsedScriptFile {
	fn from_parsed_script_file(file: &ParsedScriptFile) -> Self {
		Self {
			mod_id: file.mod_id.clone(),
			path: path_to_string(&file.path),
			relative_path: path_to_string(&file.relative_path),
			file_kind: file.file_kind,
			module_name: file.module_name.clone(),
			ast: StoredAstFile::from_ast_file(&file.ast),
			source: file.source.clone(),
			parse_issues: file
				.parse_issues
				.iter()
				.map(StoredParseIssue::from_parse_issue)
				.collect(),
			parse_cache_hit: true,
		}
	}

	fn into_parsed_script_file(self) -> ParsedScriptFile {
		let relative_path = PathBuf::from(self.relative_path);
		let content_family = eu4_profile().classify_content_family(&relative_path);
		ParsedScriptFile {
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			relative_path,
			content_family,
			file_kind: self.file_kind,
			module_name: self.module_name,
			ast: self.ast.into_ast_file(),
			source: self.source,
			parse_issues: self
				.parse_issues
				.into_iter()
				.map(StoredParseIssue::into_parse_issue)
				.collect(),
			parse_cache_hit: self.parse_cache_hit,
		}
	}
}

impl StoredAstFile {
	fn from_ast_file(file: &AstFile) -> Self {
		Self {
			path: path_to_string(&file.path),
			statements: file.statements.clone(),
		}
	}

	fn into_ast_file(self) -> AstFile {
		AstFile {
			path: PathBuf::from(self.path),
			statements: self.statements,
		}
	}
}

impl StoredSemanticIndex {
	fn from_semantic_index(index: &SemanticIndex) -> Self {
		Self {
			documents: index
				.documents
				.iter()
				.map(StoredDocumentRecord::from_document_record)
				.collect(),
			scopes: index
				.scopes
				.iter()
				.map(StoredScopeNode::from_scope_node)
				.collect(),
			definitions: index
				.definitions
				.iter()
				.map(StoredSymbolDefinition::from_symbol_definition)
				.collect(),
			references: index
				.references
				.iter()
				.map(StoredSymbolReference::from_symbol_reference)
				.collect(),
			alias_usages: index
				.alias_usages
				.iter()
				.map(StoredAliasUsage::from_alias_usage)
				.collect(),
			key_usages: index
				.key_usages
				.iter()
				.map(StoredKeyUsage::from_key_usage)
				.collect(),
			scalar_assignments: index
				.scalar_assignments
				.iter()
				.map(StoredScalarAssignment::from_scalar_assignment)
				.collect(),
			localisation_definitions: index
				.localisation_definitions
				.iter()
				.map(StoredLocalisationDefinition::from_localisation_definition)
				.collect(),
			localisation_duplicates: index
				.localisation_duplicates
				.iter()
				.map(StoredLocalisationDuplicate::from_localisation_duplicate)
				.collect(),
			ui_definitions: index
				.ui_definitions
				.iter()
				.map(StoredUiDefinition::from_ui_definition)
				.collect(),
			resource_references: index
				.resource_references
				.iter()
				.map(StoredResourceReference::from_resource_reference)
				.collect(),
			csv_rows: index
				.csv_rows
				.iter()
				.map(StoredCsvRow::from_csv_row)
				.collect(),
			json_properties: index
				.json_properties
				.iter()
				.map(StoredJsonProperty::from_json_property)
				.collect(),
			parse_issues: index
				.parse_issues
				.iter()
				.map(StoredParseIssue::from_parse_issue)
				.collect(),
		}
	}

	fn into_semantic_index(self) -> SemanticIndex {
		SemanticIndex {
			documents: self
				.documents
				.into_iter()
				.map(StoredDocumentRecord::into_document_record)
				.collect(),
			scopes: self
				.scopes
				.into_iter()
				.map(StoredScopeNode::into_scope_node)
				.collect(),
			definitions: self
				.definitions
				.into_iter()
				.map(StoredSymbolDefinition::into_symbol_definition)
				.collect(),
			references: self
				.references
				.into_iter()
				.map(StoredSymbolReference::into_symbol_reference)
				.collect(),
			alias_usages: self
				.alias_usages
				.into_iter()
				.map(StoredAliasUsage::into_alias_usage)
				.collect(),
			key_usages: self
				.key_usages
				.into_iter()
				.map(StoredKeyUsage::into_key_usage)
				.collect(),
			scalar_assignments: self
				.scalar_assignments
				.into_iter()
				.map(StoredScalarAssignment::into_scalar_assignment)
				.collect(),
			localisation_definitions: self
				.localisation_definitions
				.into_iter()
				.map(StoredLocalisationDefinition::into_localisation_definition)
				.collect(),
			localisation_duplicates: self
				.localisation_duplicates
				.into_iter()
				.map(StoredLocalisationDuplicate::into_localisation_duplicate)
				.collect(),
			ui_definitions: self
				.ui_definitions
				.into_iter()
				.map(StoredUiDefinition::into_ui_definition)
				.collect(),
			resource_references: self
				.resource_references
				.into_iter()
				.map(StoredResourceReference::into_resource_reference)
				.collect(),
			csv_rows: self
				.csv_rows
				.into_iter()
				.map(StoredCsvRow::into_csv_row)
				.collect(),
			json_properties: self
				.json_properties
				.into_iter()
				.map(StoredJsonProperty::into_json_property)
				.collect(),
			parse_issues: self
				.parse_issues
				.into_iter()
				.map(StoredParseIssue::into_parse_issue)
				.collect(),
		}
	}
}

impl StoredDocumentRecord {
	fn from_document_record(item: &DocumentRecord) -> Self {
		Self {
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			family: item.family,
			parse_ok: item.parse_ok,
		}
	}

	fn into_document_record(self) -> DocumentRecord {
		DocumentRecord {
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			family: self.family,
			parse_ok: self.parse_ok,
		}
	}
}

impl StoredScopeNode {
	fn from_scope_node(item: &ScopeNode) -> Self {
		Self {
			id: item.id,
			kind: item.kind,
			parent: item.parent,
			this_type: item.this_type,
			aliases: item.aliases.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			span: item.span.clone(),
			key: item.key.clone(),
		}
	}

	fn into_scope_node(self) -> ScopeNode {
		ScopeNode {
			id: self.id,
			kind: self.kind,
			parent: self.parent,
			this_type: self.this_type,
			aliases: self.aliases,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			span: self.span,
			key: self.key,
		}
	}
}

impl StoredSymbolDefinition {
	fn from_symbol_definition(item: &SymbolDefinition) -> Self {
		Self {
			kind: item.kind,
			name: item.name.clone(),
			module: item.module.clone(),
			local_name: item.local_name.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			scope_id: item.scope_id,
			declared_this_type: item.declared_this_type,
			inferred_this_type: item.inferred_this_type,
			inferred_this_mask: item.inferred_this_mask,
			inferred_from_mask: item.inferred_from_mask,
			inferred_root_mask: item.inferred_root_mask,
			required_params: item.required_params.clone(),
			optional_params: item.optional_params.clone(),
			param_contract: item.param_contract.clone(),
			scope_param_names: item.scope_param_names.clone(),
		}
	}

	fn into_symbol_definition(self) -> SymbolDefinition {
		SymbolDefinition {
			kind: self.kind,
			name: self.name,
			module: self.module,
			local_name: self.local_name,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
			scope_id: self.scope_id,
			declared_this_type: self.declared_this_type,
			inferred_this_type: self.inferred_this_type,
			inferred_this_mask: self.inferred_this_mask,
			inferred_from_mask: self.inferred_from_mask,
			inferred_root_mask: self.inferred_root_mask,
			required_params: self.required_params,
			optional_params: self.optional_params,
			param_contract: self.param_contract,
			scope_param_names: self.scope_param_names,
		}
	}
}

impl StoredSymbolReference {
	fn from_symbol_reference(item: &SymbolReference) -> Self {
		Self {
			kind: item.kind,
			name: item.name.clone(),
			module: item.module.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			scope_id: item.scope_id,
			provided_params: item.provided_params.clone(),
			param_bindings: item.param_bindings.clone(),
		}
	}

	fn into_symbol_reference(self) -> SymbolReference {
		SymbolReference {
			kind: self.kind,
			name: self.name,
			module: self.module,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
			scope_id: self.scope_id,
			provided_params: self.provided_params,
			param_bindings: self.param_bindings,
		}
	}
}

impl StoredAliasUsage {
	fn from_alias_usage(item: &AliasUsage) -> Self {
		Self {
			alias: item.alias.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			scope_id: item.scope_id,
		}
	}

	fn into_alias_usage(self) -> AliasUsage {
		AliasUsage {
			alias: self.alias,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
			scope_id: self.scope_id,
		}
	}
}

impl StoredKeyUsage {
	fn from_key_usage(item: &KeyUsage) -> Self {
		Self {
			key: item.key.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			scope_id: item.scope_id,
			this_type: item.this_type,
		}
	}

	fn into_key_usage(self) -> KeyUsage {
		KeyUsage {
			key: self.key,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
			scope_id: self.scope_id,
			this_type: self.this_type,
		}
	}
}

impl StoredScalarAssignment {
	fn from_scalar_assignment(item: &ScalarAssignment) -> Self {
		Self {
			key: item.key.clone(),
			value: item.value.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			scope_id: item.scope_id,
		}
	}

	fn into_scalar_assignment(self) -> ScalarAssignment {
		ScalarAssignment {
			key: self.key,
			value: self.value,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
			scope_id: self.scope_id,
		}
	}
}

impl StoredLocalisationDefinition {
	fn from_localisation_definition(item: &LocalisationDefinition) -> Self {
		Self {
			key: item.key.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
		}
	}

	fn into_localisation_definition(self) -> LocalisationDefinition {
		LocalisationDefinition {
			key: self.key,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
		}
	}
}

impl StoredLocalisationDuplicate {
	fn from_localisation_duplicate(item: &LocalisationDuplicate) -> Self {
		Self {
			key: item.key.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			first_line: item.first_line,
			duplicate_line: item.duplicate_line,
		}
	}

	fn into_localisation_duplicate(self) -> LocalisationDuplicate {
		LocalisationDuplicate {
			key: self.key,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			first_line: self.first_line,
			duplicate_line: self.duplicate_line,
		}
	}
}

impl StoredUiDefinition {
	fn from_ui_definition(item: &UiDefinition) -> Self {
		Self {
			name: item.name.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
		}
	}

	fn into_ui_definition(self) -> UiDefinition {
		UiDefinition {
			name: self.name,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
		}
	}
}

impl StoredResourceReference {
	fn from_resource_reference(item: &ResourceReference) -> Self {
		Self {
			key: item.key.clone(),
			value: item.value.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
		}
	}

	fn into_resource_reference(self) -> ResourceReference {
		ResourceReference {
			key: self.key,
			value: self.value,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
		}
	}
}

impl StoredCsvRow {
	fn from_csv_row(item: &CsvRow) -> Self {
		Self {
			identity: item.identity.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
		}
	}

	fn into_csv_row(self) -> CsvRow {
		CsvRow {
			identity: self.identity,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
		}
	}
}

impl StoredJsonProperty {
	fn from_json_property(item: &JsonProperty) -> Self {
		Self {
			key_path: item.key_path.clone(),
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
		}
	}

	fn into_json_property(self) -> JsonProperty {
		JsonProperty {
			key_path: self.key_path,
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
		}
	}
}

impl StoredParseIssue {
	fn from_parse_issue(item: &ParseIssue) -> Self {
		Self {
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			message: item.message.clone(),
		}
	}

	fn into_parse_issue(self) -> ParseIssue {
		ParseIssue {
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
			message: self.message,
		}
	}
}

fn path_to_string(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

pub fn default_mod_parse_cache_dir() -> PathBuf {
	if let Ok(override_dir) = std::env::var(CACHE_ENV) {
		return PathBuf::from(override_dir);
	}
	default_foch_cache_dir().join(DEFAULT_CACHE_DIR_NAME)
}

pub fn default_foch_cache_dir() -> PathBuf {
	if let Ok(override_dir) = std::env::var(ROOT_CACHE_ENV) {
		return PathBuf::from(override_dir);
	}

	if let Some(cache_dir) = dirs::cache_dir() {
		let candidate = cache_dir.join("foch");
		if ensure_writable_dir(&candidate) {
			return candidate;
		}
	}

	repo_fallback_cache_root_dir()
}

pub fn compute_mod_hash(mod_root: &Path) -> Result<String, io::Error> {
	let filter = FileFilter::for_game(Game::EuropaUniversalis4);
	compute_mod_hash_with_filter(mod_root, &filter)
}

pub fn compute_mod_hash_with_filter(
	mod_root: &Path,
	filter: &FileFilter,
) -> Result<String, io::Error> {
	let mut files = Vec::new();
	for entry in WalkDir::new(mod_root)
		.into_iter()
		.filter_entry(|entry| should_descend(entry.path(), mod_root))
	{
		let entry = entry.map_err(io::Error::other)?;
		if !entry.file_type().is_file() {
			continue;
		}
		let path = entry.path();
		let Ok(relative) = path.strip_prefix(mod_root) else {
			continue;
		};
		if !filter.accepts(relative) {
			continue;
		}
		files.push(relative.to_path_buf());
	}
	files.sort();

	let mut hasher = blake3::Hasher::new();
	let mut buffer = vec![0_u8; 64 * 1024];
	for relative in files {
		let normalized = normalize_relative_path(&relative);
		hasher.update(&(normalized.len() as u64).to_le_bytes());
		hasher.update(normalized.as_bytes());
		let absolute = mod_root.join(&relative);
		let mut file = fs::File::open(&absolute)?;
		let len = file.metadata()?.len();
		hasher.update(&len.to_le_bytes());
		loop {
			let read = file.read(&mut buffer)?;
			if read == 0 {
				break;
			}
			hasher.update(&buffer[..read]);
		}
	}
	Ok(hasher.finalize().to_hex()[..HASH_HEX_LEN].to_string())
}

fn encode_payload(payload: &StoredCachedModData) -> Result<AlignedVec, CacheError> {
	rkyv::to_bytes::<rkyv::rancor::Error>(payload).map_err(CacheError::encode)
}

fn decode_payload(bytes: &[u8]) -> Result<StoredCachedModData, CacheError> {
	let mut aligned = AlignedVec::<16>::with_capacity(bytes.len());
	aligned.extend_from_slice(bytes);
	// SAFETY: Mod parse cache files are produced by `encode_payload`; corrupt or
	// incompatible files simply fail and are treated as cache misses.
	unsafe { rkyv::from_bytes_unchecked::<StoredCachedModData, rkyv::rancor::Error>(&aligned) }
		.map_err(CacheError::encode)
}

fn cache_filename(
	cache_version: u32,
	mod_hash: &str,
	foch_version: &str,
	game_version: &str,
) -> String {
	format!(
		"{}__cv{}__v{}__g{}.rkyv",
		sanitize_component(mod_hash),
		cache_version,
		sanitize_component(foch_version),
		sanitize_component(game_version)
	)
}

fn should_descend(path: &Path, root: &Path) -> bool {
	if path == root {
		return true;
	}
	let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
		return true;
	};
	!matches!(
		name,
		".git" | ".hg" | ".svn" | ".jj" | ".direnv" | "target" | "node_modules"
	)
}

fn ensure_writable_dir(path: &Path) -> bool {
	if fs::create_dir_all(path).is_err() {
		return false;
	}
	let probe = path.join(".foch-write-test");
	match fs::write(&probe, b"") {
		Ok(()) => {
			let _ = fs::remove_file(probe);
			true
		}
		Err(_) => false,
	}
}

fn repo_fallback_cache_root_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.and_then(Path::parent)
		.map(Path::to_path_buf)
		.unwrap_or_else(|| PathBuf::from("."))
		.join("target")
		.join("foch-cache")
}

fn sanitize_component(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for ch in value.chars() {
		if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
			out.push(ch);
		} else {
			out.push('_');
		}
	}
	if out.is_empty() {
		"unknown".to_string()
	} else {
		out
	}
}

fn normalize_relative_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_core::model::{DocumentFamily, DocumentRecord};
	use std::thread;
	use std::time::Duration;
	use tempfile::TempDir;

	fn write_loadable_file(root: &Path, relative: &str, contents: &str) {
		let path = root.join(relative);
		fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
		fs::write(path, contents).expect("write file");
	}

	#[test]
	fn mod_hash_is_deterministic_across_runs() {
		let tmp = TempDir::new().expect("temp dir");
		write_loadable_file(tmp.path(), "common/countries/A.txt", "color = { 1 2 3 }\n");

		let first = compute_mod_hash(tmp.path()).expect("first hash");
		let second = compute_mod_hash(tmp.path()).expect("second hash");

		assert_eq!(first, second);
	}

	#[test]
	fn mod_hash_changes_when_file_content_changes() {
		let tmp = TempDir::new().expect("temp dir");
		write_loadable_file(tmp.path(), "common/countries/A.txt", "color = { 1 2 3 }\n");
		let first = compute_mod_hash(tmp.path()).expect("first hash");

		write_loadable_file(tmp.path(), "common/countries/A.txt", "color = { 1 2 4 }\n");
		let second = compute_mod_hash(tmp.path()).expect("second hash");

		assert_ne!(first, second);
	}

	#[test]
	fn mod_hash_unaffected_by_mtime() {
		let tmp = TempDir::new().expect("temp dir");
		let relative = "common/countries/A.txt";
		write_loadable_file(tmp.path(), relative, "color = { 1 2 3 }\n");
		let first = compute_mod_hash(tmp.path()).expect("first hash");

		thread::sleep(Duration::from_millis(10));
		write_loadable_file(tmp.path(), relative, "color = { 1 2 3 }\n");
		let second = compute_mod_hash(tmp.path()).expect("second hash");

		assert_eq!(first, second);
	}

	#[test]
	fn mod_hash_excludes_ignored_paths() {
		let tmp = TempDir::new().expect("temp dir");
		write_loadable_file(tmp.path(), "common/countries/A.txt", "color = { 1 2 3 }\n");
		let first = compute_mod_hash(tmp.path()).expect("first hash");

		write_loadable_file(tmp.path(), "target/common/countries/A.txt", "changed\n");
		write_loadable_file(tmp.path(), ".git/common/countries/A.txt", "changed\n");
		write_loadable_file(
			tmp.path(),
			"node_modules/common/countries/A.txt",
			"changed\n",
		);
		let second = compute_mod_hash(tmp.path()).expect("second hash");

		assert_eq!(first, second);
	}

	#[test]
	fn cache_lookup_miss_then_store_then_hit() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModParseCache::open(tmp.path());
		let mut index = SemanticIndex::default();
		index.documents.push(DocumentRecord {
			mod_id: "mod-a".to_string(),
			path: PathBuf::from("common/countries/A.txt"),
			family: DocumentFamily::Clausewitz,
			parse_ok: true,
		});
		let data = CachedModData {
			semantic_index: index,
			parsed_documents: Vec::new(),
		};

		assert!(cache.lookup("abc123", "0.1.0", "eu4 1.37.4").is_none());
		cache
			.store("abc123", "0.1.0", "eu4 1.37.4", &data)
			.expect("store cache");
		let hit = cache
			.lookup("abc123", "0.1.0", "eu4 1.37.4")
			.expect("cache hit");

		assert_eq!(hit.semantic_index.documents.len(), 1);
		assert_eq!(hit.semantic_index.documents[0].mod_id, "mod-a");
	}

	#[test]
	fn cache_lookup_miss_on_version_bump() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModParseCache::open(tmp.path());
		let data = CachedModData {
			semantic_index: SemanticIndex::default(),
			parsed_documents: Vec::new(),
		};
		cache
			.store("abc123", "0.1.0", "eu4 1.37.4", &data)
			.expect("store cache");

		assert!(
			cache
				.lookup_with_cache_version(
					MOD_PARSE_CACHE_VERSION + 1,
					"abc123",
					"0.1.0",
					"eu4 1.37.4",
				)
				.is_none()
		);
	}

	#[test]
	fn cache_lookup_miss_on_different_game_version() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModParseCache::open(tmp.path());
		let data = CachedModData {
			semantic_index: SemanticIndex::default(),
			parsed_documents: Vec::new(),
		};
		cache
			.store("abc123", "0.1.0", "eu4 1.37.4", &data)
			.expect("store cache");

		assert!(cache.lookup("abc123", "0.1.0", "eu4 1.38.0").is_none());
	}
}
