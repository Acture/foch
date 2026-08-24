use crate::model::{
	AliasUsage, CsvRow, DocumentFamily, DocumentRecord, JsonProperty, KeyUsage,
	LocalisationDefinition, LocalisationDuplicate, MaybeScope, ParamBinding, ParamContract,
	ParseIssue, ResourceReference, ScalarAssignment, ScopeKind, ScopeNode, ScopeSet, SemanticIndex,
	SourceSpan, SymbolDefinition, SymbolKind, SymbolReference, UiDefinition,
};
use crate::platform::cache_store::{CacheError, default_foch_cache_dir};
use flate2::Compression;
use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;
use rkyv::ser::{Positional, writer::IoWriter};
use rkyv::util::AlignedVec;
use std::fs::{self, File, OpenOptions};
#[cfg(test)]
use std::io::Cursor;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// Bump when the mod-level cached payload becomes wire-incompatible or parser /
/// semantic-index behavior changes in a way that should invalidate old entries.
pub const MOD_SNAPSHOT_CACHE_VERSION: &str = "10.0.0";
const DEFAULT_CACHE_DIR_NAME: &str = "mods";
const MOD_SNAPSHOT_CACHE_MAGIC: &[u8; 8] = b"FOCHMOD\0";
const MOD_SNAPSHOT_CACHE_HEADER_BYTES: usize = MOD_SNAPSHOT_CACHE_MAGIC.len() + size_of::<u64>();
const MAX_UNCOMPRESSED_MOD_SNAPSHOT_CACHE_BYTES: u64 = 4_u64 << 30;
static MOD_SNAPSHOT_CACHE_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct SizeLimitedWriter<W> {
	inner: W,
	written: u64,
	limit: u64,
}

impl<W> SizeLimitedWriter<W> {
	fn new(inner: W, limit: u64) -> Self {
		Self {
			inner,
			written: 0,
			limit,
		}
	}

	fn into_inner(self) -> W {
		self.inner
	}
}

impl<W: Write> Write for SizeLimitedWriter<W> {
	fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
		let requested = u64::try_from(bytes.len())
			.map_err(|_| io::Error::other("mod snapshot write does not fit u64"))?;
		if self.written.saturating_add(requested) > self.limit {
			return Err(io::Error::new(
				io::ErrorKind::FileTooLarge,
				format!(
					"uncompressed mod snapshot exceeds the {} byte decode limit",
					self.limit
				),
			));
		}
		let written = self.inner.write(bytes)?;
		self.written = self.written.saturating_add(written as u64);
		Ok(written)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.inner.flush()
	}
}

#[derive(Clone, Debug)]
pub struct CachedModData {
	pub semantic_index: SemanticIndex,
	/// Strictly sorted, unique, normalized relative paths for every file in the
	/// source mod inventory, including files outside semantic analysis.
	pub inventory_paths: Vec<String>,
	/// One compact flag per `semantic_index.documents` entry. `true` means the
	/// document parsed cleanly and contains no non-comment AST content.
	pub document_noop_hints: Vec<bool>,
	/// One raw-input identity per `semantic_index.documents` entry. Clausewitz
	/// documents carry an identity; formats that do not need lazy AST reloads
	/// use `None`.
	pub document_input_identities: Vec<Option<CachedDocumentInputIdentity>>,
}

#[derive(Clone, Debug, Eq, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub(crate) struct CachedDocumentInputIdentity {
	pub size_bytes: u64,
	pub content_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ModSnapshotCacheEntryProfile {
	pub compressed_bytes: u64,
	pub uncompressed_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModSnapshotCacheStoreOutcome {
	Stored(ModSnapshotCacheEntryProfile),
	RejectedTooLarge {
		compressed_bytes: u64,
		cap_bytes: u64,
	},
}

#[derive(Clone, Debug)]
pub struct ModSnapshotCache {
	root: PathBuf,
}

#[derive(Clone, Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct StoredCachedModData {
	cache_version: String,
	mod_hash: String,
	foch_version: String,
	game_key: String,
	semantic_index: StoredSemanticIndex,
	inventory_paths: Vec<String>,
	document_noop_hints: Vec<bool>,
	document_input_identities: Vec<Option<CachedDocumentInputIdentity>>,
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
	this_type: MaybeScope,
	aliases: std::collections::HashMap<String, MaybeScope>,
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
	declared_this_type: MaybeScope,
	inferred_this_type: MaybeScope,
	inferred_this_mask: ScopeSet,
	inferred_from_mask: ScopeSet,
	inferred_root_mask: ScopeSet,
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
	this_type: MaybeScope,
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

impl ModSnapshotCache {
	pub fn open(cache_dir: &Path) -> Self {
		let _ = fs::create_dir_all(cache_dir);
		Self {
			root: cache_dir.to_path_buf(),
		}
	}

	pub fn open_default() -> Self {
		Self::open(&default_mod_snapshot_cache_dir())
	}

	pub fn lookup(
		&self,
		mod_hash: &str,
		foch_version: &str,
		game_key: &str,
	) -> Option<CachedModData> {
		self.lookup_with_cache_version(MOD_SNAPSHOT_CACHE_VERSION, mod_hash, foch_version, game_key)
	}

	pub(crate) fn store_owned(
		&self,
		mod_hash: &str,
		foch_version: &str,
		game_key: &str,
		data: CachedModData,
	) -> (
		CachedModData,
		Result<ModSnapshotCacheStoreOutcome, CacheError>,
	) {
		self.store_owned_with_cache_version_and_cap(
			MOD_SNAPSHOT_CACHE_VERSION,
			mod_hash,
			foch_version,
			game_key,
			data,
			crate::platform::cache_store::cache_cap_bytes(),
		)
	}

	#[cfg(test)]
	fn store(
		&self,
		mod_hash: &str,
		foch_version: &str,
		game_key: &str,
		data: &CachedModData,
	) -> Result<(), CacheError> {
		let (_, result) = self.store_owned_with_cache_version_and_cap(
			MOD_SNAPSHOT_CACHE_VERSION,
			mod_hash,
			foch_version,
			game_key,
			data.clone(),
			u64::MAX,
		);
		match result? {
			ModSnapshotCacheStoreOutcome::Stored(_) => Ok(()),
			ModSnapshotCacheStoreOutcome::RejectedTooLarge {
				compressed_bytes,
				cap_bytes,
			} => Err(CacheError::encode(format!(
				"compressed mod snapshot is {compressed_bytes} bytes, exceeding the {cap_bytes} byte cache-layer cap"
			))),
		}
	}

	pub(crate) fn entry_profile(
		&self,
		mod_hash: &str,
		foch_version: &str,
		game_key: &str,
	) -> Option<ModSnapshotCacheEntryProfile> {
		let path = self.cache_file(MOD_SNAPSHOT_CACHE_VERSION, mod_hash, foch_version, game_key);
		let compressed_bytes = fs::metadata(&path).ok()?.len();
		let mut header = [0_u8; MOD_SNAPSHOT_CACHE_HEADER_BYTES];
		fs::File::open(path).ok()?.read_exact(&mut header).ok()?;
		let uncompressed_bytes = declared_uncompressed_bytes(&header).ok()?;
		Some(ModSnapshotCacheEntryProfile {
			compressed_bytes,
			uncompressed_bytes,
		})
	}

	pub(crate) fn touch_entry(&self, mod_hash: &str, foch_version: &str, game_key: &str) {
		touch_cache_file(&self.cache_file(
			MOD_SNAPSHOT_CACHE_VERSION,
			mod_hash,
			foch_version,
			game_key,
		));
	}

	fn lookup_with_cache_version(
		&self,
		cache_version: &str,
		mod_hash: &str,
		foch_version: &str,
		game_key: &str,
	) -> Option<CachedModData> {
		self.lookup_with_cache_version_and_cap(
			cache_version,
			mod_hash,
			foch_version,
			game_key,
			crate::platform::cache_store::cache_cap_bytes(),
		)
	}

	fn lookup_with_cache_version_and_cap(
		&self,
		cache_version: &str,
		mod_hash: &str,
		foch_version: &str,
		game_key: &str,
		cap_bytes: u64,
	) -> Option<CachedModData> {
		let path = self.cache_file(cache_version, mod_hash, foch_version, game_key);
		let compressed_bytes = fs::metadata(&path).ok()?.len();
		if compressed_bytes > cap_bytes {
			tracing::warn!(
				target: "foch::input::mod_snapshot",
				path = %path.display(),
				compressed_bytes,
				cap_bytes,
				"discarding mod snapshot larger than the cache-layer byte cap"
			);
			let _ = fs::remove_file(path);
			return None;
		}
		let stored = decode_payload_from_file(&path).ok()?;
		if stored.cache_version != cache_version
			|| stored.mod_hash != mod_hash
			|| stored.foch_version != foch_version
			|| stored.game_key != game_key
		{
			return None;
		}
		stored.validate_document_metadata().ok()?;
		let data = stored.into_cached_mod_data();
		touch_cache_file(&path);
		Some(data)
	}

	fn store_owned_with_cache_version_and_cap(
		&self,
		cache_version: &str,
		mod_hash: &str,
		foch_version: &str,
		game_key: &str,
		data: CachedModData,
		cap_bytes: u64,
	) -> (
		CachedModData,
		Result<ModSnapshotCacheStoreOutcome, CacheError>,
	) {
		if let Err(error) = validate_cached_document_metadata(&data) {
			return (data, Err(error));
		}
		if let Err(error) = fs::create_dir_all(&self.root).map_err(CacheError::Io) {
			return (data, Err(error));
		}
		let payload = StoredCachedModData::from_cached_mod_data_owned(
			cache_version,
			mod_hash,
			foch_version,
			game_key,
			data,
		);
		let path = self.cache_file(cache_version, mod_hash, foch_version, game_key);
		let result = store_payload_streaming(&path, &payload, cap_bytes);
		let data = payload.into_cached_mod_data();
		(data, result)
	}

	fn cache_file(
		&self,
		cache_version: &str,
		mod_hash: &str,
		foch_version: &str,
		game_key: &str,
	) -> PathBuf {
		let filename = cache_filename(cache_version, mod_hash, foch_version, game_key);
		self.root.join(filename)
	}
}

fn touch_cache_file(path: &Path) {
	// Windows requires a writable handle for `SetFileTime`.
	if let Ok(file) = OpenOptions::new().write(true).open(path) {
		let _ = file.set_modified(SystemTime::now());
	}
}

#[cfg(test)]
fn is_mod_snapshot_cache_tmp(name: &str) -> bool {
	name.contains("__cv") && name.contains(".rkyv.") && name.ends_with(".tmp")
}

impl StoredCachedModData {
	fn from_cached_mod_data_owned(
		cache_version: &str,
		mod_hash: &str,
		foch_version: &str,
		game_key: &str,
		data: CachedModData,
	) -> Self {
		Self {
			cache_version: cache_version.to_string(),
			mod_hash: mod_hash.to_string(),
			foch_version: foch_version.to_string(),
			game_key: game_key.to_string(),
			semantic_index: StoredSemanticIndex::from_semantic_index_owned(data.semantic_index),
			inventory_paths: data.inventory_paths,
			document_noop_hints: data.document_noop_hints,
			document_input_identities: data.document_input_identities,
		}
	}

	fn validate_document_metadata(&self) -> Result<(), CacheError> {
		let document_count = self.semantic_index.documents.len();
		if self.document_noop_hints.len() != document_count {
			return Err(CacheError::encode(format!(
				"mod snapshot noop hint count {} does not match document count {document_count}",
				self.document_noop_hints.len(),
			)));
		}
		if self.document_input_identities.len() != document_count {
			return Err(CacheError::encode(format!(
				"mod snapshot input identity count {} does not match document count {document_count}",
				self.document_input_identities.len(),
			)));
		}
		validate_inventory_paths(&self.inventory_paths)?;
		Ok(())
	}

	fn into_cached_mod_data(self) -> CachedModData {
		let semantic_index = self.semantic_index.into_semantic_index();
		CachedModData {
			semantic_index,
			inventory_paths: self.inventory_paths,
			document_noop_hints: self.document_noop_hints,
			document_input_identities: self.document_input_identities,
		}
	}
}

fn validate_cached_document_metadata(data: &CachedModData) -> Result<(), CacheError> {
	let document_count = data.semantic_index.documents.len();
	if data.document_noop_hints.len() != document_count {
		return Err(CacheError::encode(format!(
			"mod snapshot noop hint count {} does not match document count {document_count}",
			data.document_noop_hints.len(),
		)));
	}
	if data.document_input_identities.len() != document_count {
		return Err(CacheError::encode(format!(
			"mod snapshot input identity count {} does not match document count {document_count}",
			data.document_input_identities.len(),
		)));
	}
	validate_inventory_paths(&data.inventory_paths)?;
	Ok(())
}

fn validate_inventory_paths(inventory_paths: &[String]) -> Result<(), CacheError> {
	for path in inventory_paths {
		if !is_normalized_relative_inventory_path(path) {
			return Err(CacheError::encode(format!(
				"mod snapshot inventory path {path:?} is not a normalized forward-slash relative path"
			)));
		}
	}
	for pair in inventory_paths.windows(2) {
		if pair[0] >= pair[1] {
			return Err(CacheError::encode(
				"mod snapshot inventory paths must be strictly sorted and unique",
			));
		}
	}
	Ok(())
}

fn is_normalized_relative_inventory_path(path: &str) -> bool {
	let bytes = path.as_bytes();
	if path.is_empty()
		|| path.starts_with('/')
		|| path.contains('\\')
		|| (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
	{
		return false;
	}
	path.split('/')
		.all(|component| !component.is_empty() && component != "." && component != "..")
}

impl StoredSemanticIndex {
	fn from_semantic_index_owned(index: SemanticIndex) -> Self {
		Self {
			documents: index
				.documents
				.into_iter()
				.map(StoredDocumentRecord::from_document_record_owned)
				.collect(),
			scopes: index
				.scopes
				.into_iter()
				.map(StoredScopeNode::from_scope_node_owned)
				.collect(),
			definitions: index
				.definitions
				.into_iter()
				.map(StoredSymbolDefinition::from_symbol_definition_owned)
				.collect(),
			references: index
				.references
				.into_iter()
				.map(StoredSymbolReference::from_symbol_reference_owned)
				.collect(),
			alias_usages: index
				.alias_usages
				.into_iter()
				.map(StoredAliasUsage::from_alias_usage_owned)
				.collect(),
			key_usages: index
				.key_usages
				.into_iter()
				.map(StoredKeyUsage::from_key_usage_owned)
				.collect(),
			scalar_assignments: index
				.scalar_assignments
				.into_iter()
				.map(StoredScalarAssignment::from_scalar_assignment_owned)
				.collect(),
			localisation_definitions: index
				.localisation_definitions
				.into_iter()
				.map(StoredLocalisationDefinition::from_localisation_definition_owned)
				.collect(),
			localisation_duplicates: index
				.localisation_duplicates
				.into_iter()
				.map(StoredLocalisationDuplicate::from_localisation_duplicate_owned)
				.collect(),
			ui_definitions: index
				.ui_definitions
				.into_iter()
				.map(StoredUiDefinition::from_ui_definition_owned)
				.collect(),
			resource_references: index
				.resource_references
				.into_iter()
				.map(StoredResourceReference::from_resource_reference_owned)
				.collect(),
			csv_rows: index
				.csv_rows
				.into_iter()
				.map(StoredCsvRow::from_csv_row_owned)
				.collect(),
			json_properties: index
				.json_properties
				.into_iter()
				.map(StoredJsonProperty::from_json_property_owned)
				.collect(),
			parse_issues: index
				.parse_issues
				.into_iter()
				.map(StoredParseIssue::from_parse_issue_owned)
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
	fn from_document_record_owned(item: DocumentRecord) -> Self {
		Self {
			mod_id: item.mod_id,
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
	fn from_scope_node_owned(item: ScopeNode) -> Self {
		Self {
			id: item.id,
			kind: item.kind,
			parent: item.parent,
			this_type: item.this_type,
			aliases: item.aliases,
			mod_id: item.mod_id,
			path: path_to_string(&item.path),
			span: item.span,
			key: item.key,
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
	fn from_symbol_definition_owned(item: SymbolDefinition) -> Self {
		Self {
			kind: item.kind,
			name: item.name,
			module: item.module,
			local_name: item.local_name,
			mod_id: item.mod_id,
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			scope_id: item.scope_id,
			declared_this_type: item.declared_this_type,
			inferred_this_type: item.inferred_this_type,
			inferred_this_mask: item.inferred_this_mask,
			inferred_from_mask: item.inferred_from_mask,
			inferred_root_mask: item.inferred_root_mask,
			required_params: item.required_params,
			optional_params: item.optional_params,
			param_contract: item.param_contract,
			scope_param_names: item.scope_param_names,
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
	fn from_symbol_reference_owned(item: SymbolReference) -> Self {
		Self {
			kind: item.kind,
			name: item.name,
			module: item.module,
			mod_id: item.mod_id,
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			scope_id: item.scope_id,
			provided_params: item.provided_params,
			param_bindings: item.param_bindings,
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
	fn from_alias_usage_owned(item: AliasUsage) -> Self {
		Self {
			alias: item.alias,
			mod_id: item.mod_id,
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
	fn from_key_usage_owned(item: KeyUsage) -> Self {
		Self {
			key: item.key,
			mod_id: item.mod_id,
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
	fn from_scalar_assignment_owned(item: ScalarAssignment) -> Self {
		Self {
			key: item.key,
			value: item.value,
			mod_id: item.mod_id,
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
	fn from_localisation_definition_owned(item: LocalisationDefinition) -> Self {
		Self {
			key: item.key,
			mod_id: item.mod_id,
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
	fn from_localisation_duplicate_owned(item: LocalisationDuplicate) -> Self {
		Self {
			key: item.key,
			mod_id: item.mod_id,
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
	fn from_ui_definition_owned(item: UiDefinition) -> Self {
		Self {
			name: item.name,
			mod_id: item.mod_id,
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
	fn from_resource_reference_owned(item: ResourceReference) -> Self {
		Self {
			key: item.key,
			value: item.value,
			mod_id: item.mod_id,
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
	fn from_csv_row_owned(item: CsvRow) -> Self {
		Self {
			identity: item.identity,
			mod_id: item.mod_id,
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
	fn from_json_property_owned(item: JsonProperty) -> Self {
		Self {
			key_path: item.key_path,
			mod_id: item.mod_id,
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
	fn from_parse_issue_owned(item: ParseIssue) -> Self {
		Self {
			mod_id: item.mod_id,
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			message: item.message,
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

pub fn default_mod_snapshot_cache_dir() -> PathBuf {
	default_foch_cache_dir().join(DEFAULT_CACHE_DIR_NAME)
}

fn store_payload_streaming(
	path: &Path,
	payload: &StoredCachedModData,
	cap_bytes: u64,
) -> Result<ModSnapshotCacheStoreOutcome, CacheError> {
	let (tmp, file) = create_mod_snapshot_cache_tmp(path)?;
	let encoded = encode_payload_into_file(payload, file);
	let profile = match encoded {
		Ok(profile) => profile,
		Err(error) => {
			let _ = fs::remove_file(&tmp);
			return Err(error);
		}
	};
	if profile.compressed_bytes > cap_bytes {
		let _ = fs::remove_file(&tmp);
		tracing::warn!(
			target: "foch::input::mod_snapshot",
			path = %path.display(),
			compressed_bytes = profile.compressed_bytes,
			cap_bytes,
			"rejecting mod snapshot larger than the cache-layer byte cap"
		);
		return Ok(ModSnapshotCacheStoreOutcome::RejectedTooLarge {
			compressed_bytes: profile.compressed_bytes,
			cap_bytes,
		});
	}
	if let Err(error) = fs::rename(&tmp, path) {
		let _ = fs::remove_file(&tmp);
		return Err(CacheError::Io(error));
	}
	Ok(ModSnapshotCacheStoreOutcome::Stored(profile))
}

fn create_mod_snapshot_cache_tmp(path: &Path) -> Result<(PathBuf, File), CacheError> {
	for _ in 0..32 {
		let sequence = MOD_SNAPSHOT_CACHE_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
		let tmp = path.with_extension(format!("rkyv.{}.{}.tmp", std::process::id(), sequence));
		match OpenOptions::new().write(true).create_new(true).open(&tmp) {
			Ok(file) => return Ok((tmp, file)),
			Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
			Err(error) => return Err(CacheError::Io(error)),
		}
	}
	Err(CacheError::Io(io::Error::new(
		io::ErrorKind::AlreadyExists,
		"could not allocate a unique mod snapshot temporary file",
	)))
}

fn encode_payload_into_file(
	payload: &StoredCachedModData,
	mut file: File,
) -> Result<ModSnapshotCacheEntryProfile, CacheError> {
	file.write_all(&[0_u8; MOD_SNAPSHOT_CACHE_HEADER_BYTES])
		.map_err(CacheError::Io)?;
	let encoder = GzEncoder::new(file, Compression::fast());
	let limited = SizeLimitedWriter::new(encoder, MAX_UNCOMPRESSED_MOD_SNAPSHOT_CACHE_BYTES);
	let writer = IoWriter::new(limited);
	let writer = rkyv::api::high::to_bytes_in::<_, rkyv::rancor::Error>(payload, writer)
		.map_err(CacheError::encode)?;
	let uncompressed_bytes = u64::try_from(writer.pos())
		.map_err(|_| CacheError::encode("mod snapshot is too large to encode"))?;
	let encoder = writer.into_inner().into_inner();
	let mut file = encoder.finish().map_err(CacheError::Io)?;
	let compressed_bytes = file.seek(SeekFrom::End(0)).map_err(CacheError::Io)?;
	if uncompressed_bytes > MAX_UNCOMPRESSED_MOD_SNAPSHOT_CACHE_BYTES {
		return Err(CacheError::encode(format!(
			"uncompressed mod snapshot is {uncompressed_bytes} bytes, exceeding the {} byte decode limit",
			MAX_UNCOMPRESSED_MOD_SNAPSHOT_CACHE_BYTES
		)));
	}
	file.seek(SeekFrom::Start(0)).map_err(CacheError::Io)?;
	file.write_all(MOD_SNAPSHOT_CACHE_MAGIC)
		.map_err(CacheError::Io)?;
	file.write_all(&uncompressed_bytes.to_le_bytes())
		.map_err(CacheError::Io)?;
	file.flush().map_err(CacheError::Io)?;
	Ok(ModSnapshotCacheEntryProfile {
		compressed_bytes,
		uncompressed_bytes,
	})
}

fn decode_payload_from_file(path: &Path) -> Result<StoredCachedModData, CacheError> {
	let file = File::open(path).map_err(CacheError::Io)?;
	decode_payload_from_reader(BufReader::new(file))
}

fn decode_payload_from_reader(
	mut reader: impl std::io::BufRead,
) -> Result<StoredCachedModData, CacheError> {
	let mut header = [0_u8; MOD_SNAPSHOT_CACHE_HEADER_BYTES];
	reader.read_exact(&mut header).map_err(CacheError::Io)?;
	let uncompressed_bytes = declared_uncompressed_bytes(&header)?;
	let expected = usize::try_from(uncompressed_bytes)
		.map_err(|_| CacheError::encode("mod snapshot length does not fit this platform"))?;
	let mut decoder = GzDecoder::new(reader);
	let mut aligned = AlignedVec::<16>::with_capacity(expected.min(1 << 20));
	let mut chunk = [0_u8; 64 * 1024];
	loop {
		let read = decoder.read(&mut chunk).map_err(CacheError::Io)?;
		if read == 0 {
			break;
		}
		if aligned.len().saturating_add(read) > expected {
			return Err(CacheError::encode(
				"decompressed mod snapshot exceeds its declared length",
			));
		}
		aligned.extend_from_slice(&chunk[..read]);
	}
	if aligned.len() != expected {
		return Err(CacheError::encode(format!(
			"decompressed mod snapshot length mismatch: expected {expected}, observed {}",
			aligned.len()
		)));
	}
	let mut compressed = decoder.into_inner();
	let mut trailing = [0_u8; 1];
	if compressed.read(&mut trailing).map_err(CacheError::Io)? != 0 {
		return Err(CacheError::encode(
			"trailing bytes after mod snapshot gzip stream",
		));
	}
	let stored = rkyv::from_bytes::<StoredCachedModData, rkyv::rancor::Error>(&aligned)
		.map_err(CacheError::encode)?;
	stored.validate_document_metadata()?;
	Ok(stored)
}

#[cfg(test)]
fn decode_payload(bytes: &[u8]) -> Result<StoredCachedModData, CacheError> {
	decode_payload_from_reader(Cursor::new(bytes))
}

fn declared_uncompressed_bytes(header: &[u8]) -> Result<u64, CacheError> {
	if header.len() != MOD_SNAPSHOT_CACHE_HEADER_BYTES {
		return Err(CacheError::encode("invalid mod snapshot header length"));
	}
	if header.get(..MOD_SNAPSHOT_CACHE_MAGIC.len()) != Some(MOD_SNAPSHOT_CACHE_MAGIC.as_slice()) {
		return Err(CacheError::encode("invalid mod snapshot magic"));
	}
	let length_bytes: [u8; 8] = header[MOD_SNAPSHOT_CACHE_MAGIC.len()..]
		.try_into()
		.map_err(CacheError::encode)?;
	let uncompressed_bytes = u64::from_le_bytes(length_bytes);
	if uncompressed_bytes > MAX_UNCOMPRESSED_MOD_SNAPSHOT_CACHE_BYTES {
		return Err(CacheError::encode(format!(
			"mod snapshot declares {uncompressed_bytes} uncompressed bytes, exceeding the {} byte decode limit",
			MAX_UNCOMPRESSED_MOD_SNAPSHOT_CACHE_BYTES
		)));
	}
	Ok(uncompressed_bytes)
}

fn cache_filename(
	cache_version: &str,
	mod_hash: &str,
	foch_version: &str,
	game_key: &str,
) -> String {
	format!(
		"{}__cv{}__v{}__g{}.rkyv",
		sanitize_component(mod_hash),
		cache_version,
		sanitize_component(foch_version),
		sanitize_component(game_key)
	)
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::{DocumentFamily, DocumentRecord};
	use std::time::Duration;
	use tempfile::TempDir;

	#[test]
	fn cache_lookup_miss_then_store_then_hit() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let mut index = SemanticIndex::default();
		index.documents.push(DocumentRecord {
			mod_id: "mod-a".to_string(),
			path: PathBuf::from("common/countries/A.txt"),
			family: DocumentFamily::Clausewitz,
			parse_ok: true,
		});
		let data = CachedModData {
			semantic_index: index,
			inventory_paths: vec![
				"common/countries/A.txt".to_string(),
				"gfx/flags/A.tga".to_string(),
			],
			document_noop_hints: vec![true],
			document_input_identities: vec![Some(CachedDocumentInputIdentity {
				size_bytes: 17,
				content_digest: "abc123".to_string(),
			})],
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
		assert_eq!(
			hit.inventory_paths,
			vec![
				"common/countries/A.txt".to_string(),
				"gfx/flags/A.tga".to_string(),
			]
		);
		assert_eq!(hit.document_noop_hints, vec![true]);
		assert_eq!(
			hit.document_input_identities,
			vec![Some(CachedDocumentInputIdentity {
				size_bytes: 17,
				content_digest: "abc123".to_string(),
			})]
		);
		let profile = cache
			.entry_profile("abc123", "0.1.0", "eu4 1.37.4")
			.expect("entry profile");
		assert!(profile.compressed_bytes > MOD_SNAPSHOT_CACHE_HEADER_BYTES as u64);
		assert!(profile.uncompressed_bytes > 0);
		let path = cache.cache_file(MOD_SNAPSHOT_CACHE_VERSION, "abc123", "0.1.0", "eu4 1.37.4");
		let raw = fs::read(path).expect("read envelope");
		assert_eq!(
			&raw[..MOD_SNAPSHOT_CACHE_MAGIC.len()],
			MOD_SNAPSHOT_CACHE_MAGIC
		);
	}

	#[test]
	fn oversized_store_returns_payload_and_leaves_no_entry() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let data = CachedModData {
			semantic_index: SemanticIndex::default(),
			inventory_paths: vec!["common/countries/A.txt".to_string()],
			document_noop_hints: Vec::new(),
			document_input_identities: Vec::new(),
		};
		let final_path = cache.cache_file(
			MOD_SNAPSHOT_CACHE_VERSION,
			"oversized",
			"0.1.0",
			"eu4 1.37.4",
		);

		let (returned, result) = cache.store_owned_with_cache_version_and_cap(
			MOD_SNAPSHOT_CACHE_VERSION,
			"oversized",
			"0.1.0",
			"eu4 1.37.4",
			data,
			MOD_SNAPSHOT_CACHE_HEADER_BYTES as u64,
		);

		assert!(returned.semantic_index.documents.is_empty());
		assert_eq!(
			returned.inventory_paths,
			vec!["common/countries/A.txt".to_string()]
		);
		assert!(matches!(
			result,
			Ok(ModSnapshotCacheStoreOutcome::RejectedTooLarge { .. })
		));
		assert!(!final_path.exists());
		assert!(
			fs::read_dir(&cache.root)
				.expect("read cache dir")
				.flatten()
				.all(|entry| !is_mod_snapshot_cache_tmp(&entry.file_name().to_string_lossy()))
		);
	}

	#[test]
	fn store_error_returns_original_payload_without_creating_temp_files() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let mut index = SemanticIndex::default();
		index.documents.push(DocumentRecord {
			mod_id: "mod-a".to_string(),
			path: PathBuf::from("common/countries/A.txt"),
			family: DocumentFamily::Clausewitz,
			parse_ok: true,
		});
		let data = CachedModData {
			semantic_index: index,
			inventory_paths: Vec::new(),
			document_noop_hints: Vec::new(),
			document_input_identities: vec![None],
		};

		let (returned, result) = cache.store_owned("invalid-metadata", "0.1.0", "eu4 1.37.4", data);

		assert!(result.is_err());
		assert_eq!(returned.semantic_index.documents.len(), 1);
		assert!(returned.document_noop_hints.is_empty());
		assert!(
			fs::read_dir(&cache.root)
				.expect("read cache dir")
				.flatten()
				.all(|entry| !is_mod_snapshot_cache_tmp(&entry.file_name().to_string_lossy()))
		);
	}

	#[test]
	fn store_rejects_invalid_inventory_paths() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let invalid_cases = [
			(
				"unsorted",
				vec!["gfx/flags/A.tga", "common/countries/A.txt"],
			),
			(
				"duplicate",
				vec!["common/countries/A.txt", "common/countries/A.txt"],
			),
			("empty", vec![""]),
			("absolute", vec!["/common/countries/A.txt"]),
			("drive-absolute", vec!["C:/common/countries/A.txt"]),
			("empty-component", vec!["common//countries/A.txt"]),
			("dot-component", vec!["common/./countries/A.txt"]),
			("parent-component", vec!["common/../countries/A.txt"]),
			("backslash", vec![r"common\countries\A.txt"]),
		];

		for (case, paths) in invalid_cases {
			let inventory_paths = paths.into_iter().map(str::to_string).collect::<Vec<_>>();
			let data = CachedModData {
				semantic_index: SemanticIndex::default(),
				inventory_paths: inventory_paths.clone(),
				document_noop_hints: Vec::new(),
				document_input_identities: Vec::new(),
			};

			let (returned, result) = cache.store_owned(case, "0.1.0", "eu4 1.37.4", data);
			let error = result.expect_err(case);

			assert_eq!(returned.inventory_paths, inventory_paths, "{case}");
			assert!(
				matches!(error, CacheError::Encode(message) if message.contains("inventory")),
				"{case}"
			);
		}
		assert!(
			fs::read_dir(&cache.root)
				.expect("read cache dir")
				.next()
				.is_none()
		);
	}

	#[test]
	fn decode_rejects_invalid_inventory_paths() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let path = cache.cache_file(
			MOD_SNAPSHOT_CACHE_VERSION,
			"invalid-inventory",
			"0.1.0",
			"eu4 1.37.4",
		);
		let payload = StoredCachedModData::from_cached_mod_data_owned(
			MOD_SNAPSHOT_CACHE_VERSION,
			"invalid-inventory",
			"0.1.0",
			"eu4 1.37.4",
			CachedModData {
				semantic_index: SemanticIndex::default(),
				inventory_paths: vec!["common/../countries/A.txt".to_string()],
				document_noop_hints: Vec::new(),
				document_input_identities: Vec::new(),
			},
		);
		assert!(matches!(
			store_payload_streaming(&path, &payload, u64::MAX).expect("encode invalid payload"),
			ModSnapshotCacheStoreOutcome::Stored(_)
		));

		let error = decode_payload_from_file(&path).expect_err("reject invalid inventory");
		assert!(matches!(
			error,
			CacheError::Encode(message) if message.contains("inventory path")
		));
		assert!(
			cache
				.lookup("invalid-inventory", "0.1.0", "eu4 1.37.4")
				.is_none()
		);
	}

	#[test]
	fn lookup_rejects_compressed_entry_over_cap_before_decode() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let path = cache.cache_file(
			MOD_SNAPSHOT_CACHE_VERSION,
			"over-cap",
			"0.1.0",
			"eu4 1.37.4",
		);
		fs::write(&path, [0_u8; MOD_SNAPSHOT_CACHE_HEADER_BYTES + 1])
			.expect("write oversized entry");

		assert!(
			cache
				.lookup_with_cache_version_and_cap(
					MOD_SNAPSHOT_CACHE_VERSION,
					"over-cap",
					"0.1.0",
					"eu4 1.37.4",
					MOD_SNAPSHOT_CACHE_HEADER_BYTES as u64,
				)
				.is_none()
		);
		assert!(!path.exists());
	}

	#[test]
	fn cache_lookup_treats_corruption_truncation_and_trailing_bytes_as_misses() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let data = CachedModData {
			semantic_index: SemanticIndex::default(),
			inventory_paths: Vec::new(),
			document_noop_hints: Vec::new(),
			document_input_identities: Vec::new(),
		};
		cache
			.store("corrupt", "0.1.0", "eu4 1.37.4", &data)
			.expect("store cache");
		let path = cache.cache_file(MOD_SNAPSHOT_CACHE_VERSION, "corrupt", "0.1.0", "eu4 1.37.4");
		let original = fs::read(&path).expect("read cache entry");

		let mut corrupted = original.clone();
		let payload_middle = MOD_SNAPSHOT_CACHE_HEADER_BYTES
			+ (corrupted.len() - MOD_SNAPSHOT_CACHE_HEADER_BYTES) / 2;
		corrupted[payload_middle] ^= 0xff;
		fs::write(&path, corrupted).expect("write corrupted entry");
		assert!(cache.lookup("corrupt", "0.1.0", "eu4 1.37.4").is_none());

		fs::write(&path, &original[..original.len() - 1]).expect("write truncated entry");
		assert!(cache.lookup("corrupt", "0.1.0", "eu4 1.37.4").is_none());

		let mut trailing = original;
		trailing.push(0);
		fs::write(&path, trailing).expect("write entry with trailing byte");
		assert!(cache.lookup("corrupt", "0.1.0", "eu4 1.37.4").is_none());
	}

	#[test]
	fn cache_decode_rejects_invalid_magic_and_oversized_declared_payload() {
		let mut invalid_magic = vec![0_u8; MOD_SNAPSHOT_CACHE_HEADER_BYTES];
		invalid_magic[MOD_SNAPSHOT_CACHE_MAGIC.len()..].copy_from_slice(&1_u64.to_le_bytes());
		assert!(decode_payload(&invalid_magic).is_err());

		let mut oversized = Vec::from(MOD_SNAPSHOT_CACHE_MAGIC.as_slice());
		oversized.extend_from_slice(&(MAX_UNCOMPRESSED_MOD_SNAPSHOT_CACHE_BYTES + 1).to_le_bytes());
		assert!(decode_payload(&oversized).is_err());
	}

	#[test]
	fn successful_cache_lookup_touches_entry_mtime() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let data = CachedModData {
			semantic_index: SemanticIndex::default(),
			inventory_paths: Vec::new(),
			document_noop_hints: Vec::new(),
			document_input_identities: Vec::new(),
		};
		cache
			.store("touch", "0.1.0", "eu4 1.37.4", &data)
			.expect("store cache");
		let path = cache.cache_file(MOD_SNAPSHOT_CACHE_VERSION, "touch", "0.1.0", "eu4 1.37.4");
		let old_time = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
		filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(old_time))
			.expect("age cache entry");

		assert!(cache.lookup("touch", "0.1.0", "eu4 1.37.4").is_some());
		let touched = fs::metadata(path)
			.expect("cache metadata")
			.modified()
			.expect("cache mtime");
		assert!(touched > old_time);
	}

	#[test]
	fn open_preserves_old_generations_flat_and_temporary_items() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let old_generation = tmp.path().join("v2.0.0");
		fs::create_dir_all(&old_generation).expect("create old generation");
		let old_generation_entry = old_generation.join(cache_filename(
			"2.0.0",
			"old-generation",
			"0.1.0",
			"eu4 1.37.4",
		));
		let flat_entry =
			tmp.path()
				.join(cache_filename("2.0.0", "old-flat", "0.1.0", "eu4 1.37.4"));
		let current =
			cache.cache_file(MOD_SNAPSHOT_CACHE_VERSION, "current", "0.1.0", "eu4 1.37.4");
		let unrelated = tmp.path().join("owner.txt");
		let temporary_entry = current.with_extension("rkyv.123.456.tmp");
		fs::write(&old_generation_entry, "old generation").expect("write old generation entry");
		fs::write(&flat_entry, "old flat").expect("write old flat entry");
		fs::write(&current, "current").expect("write current cache");
		fs::write(&unrelated, "keep").expect("write unrelated file");
		fs::write(&temporary_entry, "temporary").expect("write temporary entry");

		let _cache = ModSnapshotCache::open(tmp.path());

		assert!(old_generation_entry.is_file());
		assert!(flat_entry.is_file());
		assert!(temporary_entry.is_file());
		assert!(current.is_file());
		assert!(unrelated.is_file());
	}

	#[test]
	fn cache_lookup_miss_on_version_bump() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let data = CachedModData {
			semantic_index: SemanticIndex::default(),
			inventory_paths: Vec::new(),
			document_noop_hints: Vec::new(),
			document_input_identities: Vec::new(),
		};
		cache
			.store("abc123", "0.1.0", "eu4 1.37.4", &data)
			.expect("store cache");

		assert!(
			cache
				.lookup_with_cache_version("3.0.1", "abc123", "0.1.0", "eu4 1.37.4",)
				.is_none()
		);
	}

	#[test]
	fn cache_lookup_miss_on_different_game_key() {
		let tmp = TempDir::new().expect("temp dir");
		let cache = ModSnapshotCache::open(tmp.path());
		let data = CachedModData {
			semantic_index: SemanticIndex::default(),
			inventory_paths: Vec::new(),
			document_noop_hints: Vec::new(),
			document_input_identities: Vec::new(),
		};
		cache
			.store("abc123", "0.1.0", "eu4", &data)
			.expect("store cache");

		assert!(cache.lookup("abc123", "0.1.0", "ck3").is_none());
	}
}
