mod coverage;

#[cfg(test)]
pub(crate) use coverage::{CoverageClass, build_coverage_report};
pub(crate) use coverage::{document_family_name, write_coverage_report};

use crate::config::Config;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use foch_core::domain::game::Game;
use foch_core::model::{
	AliasUsage, CsvRow, DocumentFamily, DocumentRecord, JsonProperty, KeyUsage,
	LocalisationDefinition, LocalisationDuplicate, MaybeScope, ParamBinding, ParamContract,
	ParseFamilyStats, ParseIssue, ResourceReference, ScalarAssignment, ScopeKind, ScopeNode,
	ScopeSet, SemanticIndex, SourceSpan, SymbolDefinition, SymbolKind, SymbolReference,
	UiDefinition,
};
use foch_core::utils::steam::steam_game_install_path;
use foch_language::analysis_version::analysis_rules_version;
use foch_language::analyzer::documents::{
	DiscoveredTextDocument, ParsedTextDocument, build_semantic_index_from_documents,
	discover_text_documents, parse_discovered_text_documents,
};
use foch_language::analyzer::param_contracts::apply_registered_param_contracts;
use foch_language::analyzer::semantic_index::ParsedScriptFile;
use rayon::join;
use reqwest::blocking::Client;
use same_file::Handle as SameFileHandle;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Condvar;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const BASE_GAME_MOD_ID_PREFIX: &str = "__game__";
pub const BASE_DATA_DIR_ENV: &str = "FOCH_DATA_DIR";
pub const BASE_DATA_RELEASE_BASE_URL_ENV: &str = "FOCH_DATA_RELEASE_BASE_URL";
// Bump when any serialized snapshot section becomes wire-incompatible.
pub const BASE_DATA_SCHEMA_VERSION: u32 = 13;
pub const RELEASE_MANIFEST_FILE_NAME: &str = "foch-data-manifest.json";
pub const INSTALLED_SNAPSHOT_FILE_NAME: &str = "snapshot.bin";
pub const INSTALLED_METADATA_FILE_NAME: &str = "metadata.json";
pub const INSTALLED_COVERAGE_FILE_NAME: &str = "coverage.json";
pub const INSTALLED_VOCABULARY_MANIFEST_FILE_NAME: &str = "vocabulary-manifest.json";
const LEGACY_INSTALLED_SNAPSHOT_FILE_NAME: &str = "snapshot.bin.gz";
const SNAPSHOT_WIRE_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseDataSource {
	Build,
	Download,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledBaseDataMetadata {
	pub schema_version: u32,
	pub game: String,
	pub game_version: String,
	pub analysis_rules_version: String,
	pub generated_by_cli_version: String,
	pub source: BaseDataSource,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub asset_name: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub sha256: Option<String>,
	/// SHA-256 of the vocabulary-manifest sidecar emitted alongside this
	/// snapshot, for cross-version existence-drift provenance (GH #35).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub vocabulary_manifest_sha256: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledBaseDataEntry {
	pub schema_version: u32,
	pub game: String,
	pub game_version: String,
	pub analysis_rules_version: String,
	pub generated_by_cli_version: String,
	pub source: BaseDataSource,
	pub install_path: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub asset_name: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub sha256: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseDataAsset {
	pub game: String,
	pub game_version: String,
	pub analysis_rules_version: String,
	pub asset_name: String,
	pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseDataManifest {
	pub schema_version: u32,
	pub cli_tag: String,
	pub assets: Vec<ReleaseDataAsset>,
}

#[derive(Clone, Debug)]
pub struct InstalledBaseSnapshot {
	pub install_dir: PathBuf,
	pub metadata: InstalledBaseDataMetadata,
	pub snapshot: Arc<BaseAnalysisSnapshot>,
}

/// Holds the shared advisory lock protecting one installed snapshot while an
/// output derived from its verified identity is published.
#[must_use = "dropping the guard allows the installed snapshot to be replaced"]
#[derive(Debug)]
pub(crate) struct InstalledBaseSnapshotPublicationGuard {
	_lock_file: fs::File,
}

#[must_use = "dropping the guard allows another snapshot installation to start"]
#[derive(Debug)]
struct InstalledBaseSnapshotInstallGuard {
	_lock_file: fs::File,
}

#[derive(Clone, Copy, Debug)]
enum InstalledBaseSnapshotLockMode {
	Shared,
	Exclusive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BaseSnapshotCurrentValidation {
	Immediate,
	Deferred,
}

/// A verified selection of one installed base snapshot.
///
/// The token owns the exact bytes read during identity computation and records
/// the selected path and file identity. Passing it to
/// [`load_installed_base_snapshot`] makes the identity/load boundary explicit
/// and allows the load to reject content or path replacement.
#[derive(Clone)]
pub struct InstalledBaseSnapshotIdentity {
	install_dir: PathBuf,
	metadata: InstalledBaseDataMetadata,
	verified: VerifiedSnapshotBytes,
}

impl std::fmt::Debug for InstalledBaseSnapshotIdentity {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("InstalledBaseSnapshotIdentity")
			.field("install_dir", &self.install_dir)
			.field("path", &self.verified.identity.path)
			.field("sha256", &self.verified.identity.sha256)
			.field("file_state", &self.verified.file_state)
			.finish_non_exhaustive()
	}
}

impl PartialEq for InstalledBaseSnapshotIdentity {
	fn eq(&self, other: &Self) -> bool {
		self.verified.identity == other.verified.identity
			&& self.verified.file_state == other.verified.file_state
			&& *self.verified.file_handle == *other.verified.file_handle
	}
}

impl Eq for InstalledBaseSnapshotIdentity {}

impl InstalledBaseSnapshotIdentity {
	pub fn as_label(&self) -> String {
		snapshot_identity_label(&self.verified.identity)
	}

	pub fn sha256(&self) -> &str {
		&self.verified.identity.sha256
	}
}

impl std::fmt::Display for InstalledBaseSnapshotIdentity {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter.write_str(&self.as_label())
	}
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SnapshotContentIdentity {
	path: PathBuf,
	sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SnapshotFileState {
	len: u64,
	modified: Option<SystemTime>,
	#[cfg(unix)]
	device: u64,
	#[cfg(unix)]
	inode: u64,
	#[cfg(unix)]
	changed_seconds: i64,
	#[cfg(unix)]
	changed_nanoseconds: i64,
}

#[derive(Clone, Debug)]
struct VerifiedSnapshotBytes {
	identity: SnapshotContentIdentity,
	file_handle: Arc<SameFileHandle>,
	file_state: SnapshotFileState,
	bytes: Arc<[u8]>,
}

#[derive(Clone, Debug)]
struct StableSnapshotBytes {
	file_handle: Arc<SameFileHandle>,
	file_state: SnapshotFileState,
	bytes: Arc<[u8]>,
}

type LoadedBaseSnapshotResult = Result<Arc<BaseAnalysisSnapshot>, String>;
type LoadedBaseSnapshotCell = OnceLock<LoadedBaseSnapshotResult>;

#[derive(Debug)]
struct LoadedBaseSnapshotCacheEntry {
	cell: Arc<LoadedBaseSnapshotCell>,
	last_used: u64,
}

#[derive(Debug, Default)]
struct LoadedBaseSnapshotCache {
	entries: HashMap<String, LoadedBaseSnapshotCacheEntry>,
	clock: u64,
}

// Keep only the most recently used completed decode. Initializations are never
// evicted, so distinct concurrent digests can temporarily exceed this bound.
const COMPLETED_BASE_SNAPSHOT_CACHE_LIMIT: usize = 1;
static LOADED_BASE_SNAPSHOTS: OnceLock<Mutex<LoadedBaseSnapshotCache>> = OnceLock::new();

#[cfg(test)]
static INSTALLED_SNAPSHOT_FILE_READ_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static INSTALLED_SNAPSHOT_CURRENT_DIGEST_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static INSTALLED_SNAPSHOT_CURRENT_VALIDATION_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static INSTALLED_SNAPSHOT_COLD_DECODE_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static BASE_DATA_ENV_LOCK: Mutex<()> = Mutex::new(());
#[cfg(test)]
static INSTALLED_SNAPSHOT_DECODE_GATE: OnceLock<Mutex<Option<Arc<InstalledSnapshotDecodeGate>>>> =
	OnceLock::new();

#[cfg(test)]
#[derive(Debug, Default)]
struct InstalledSnapshotDecodeGateState {
	entered: usize,
	released: bool,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct InstalledSnapshotDecodeGate {
	state: Mutex<InstalledSnapshotDecodeGateState>,
	entered: Condvar,
	release: Condvar,
}

#[cfg(test)]
impl InstalledSnapshotDecodeGate {
	fn enter(&self) {
		let mut state = self
			.state
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		state.entered += 1;
		self.entered.notify_all();
		while !state.released {
			state = self
				.release
				.wait(state)
				.unwrap_or_else(std::sync::PoisonError::into_inner);
		}
	}

	fn wait_until_entered(&self, expected: usize) {
		let mut state = self
			.state
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		while state.entered < expected {
			state = self
				.entered
				.wait(state)
				.unwrap_or_else(std::sync::PoisonError::into_inner);
		}
	}

	fn release(&self) {
		let mut state = self
			.state
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		state.released = true;
		self.release.notify_all();
	}
}

#[derive(Clone, Debug)]
pub struct BaseSnapshotBuildResult {
	pub encoded_snapshot: Vec<u8>,
	pub snapshot_asset_name: String,
	pub snapshot_sha256: String,
}

#[derive(Clone, Debug)]
pub struct ReleaseArtifactOutput {
	pub snapshot_path: PathBuf,
	pub manifest_path: PathBuf,
	pub coverage_path: PathBuf,
	pub asset_name: String,
	pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct SnapshotBundleOutput {
	pub snapshot_path: PathBuf,
	pub metadata_path: PathBuf,
	pub coverage_path: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BaseBuildProfile {
	pub game: String,
	pub game_version: String,
	pub started_at: u64,
	pub finished_at: u64,
	pub total_elapsed_ms: u64,
	pub stages: Vec<BaseBuildStageProfile>,
	pub counts: BTreeMap<String, u64>,
	pub encoded_size_bytes: u64,
	pub inventory_file_count: usize,
	pub document_count: usize,
	pub parse_stats: ParseFamilyStats,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub encoded_sections: Vec<BaseEncodedSectionProfile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseBuildStageProfile {
	pub name: String,
	pub started_at: u64,
	pub finished_at: u64,
	pub elapsed_ms: u64,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub counts: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseEncodedSectionProfile {
	pub name: String,
	pub elapsed_ms: u64,
	pub uncompressed_bytes: u64,
	pub compressed_bytes: u64,
	pub sha256: String,
}

#[derive(Debug)]
pub struct BaseBuildObserver {
	emit_progress: bool,
	started_at: u64,
	started_instant: Instant,
	profile: BaseBuildProfile,
}

impl BaseBuildObserver {
	pub fn stderr(game_key: &str) -> Self {
		Self::new(game_key, true)
	}

	pub fn silent(game_key: &str) -> Self {
		Self::new(game_key, false)
	}

	fn new(game_key: &str, emit_progress: bool) -> Self {
		let started_at = unix_timestamp_millis();
		Self {
			emit_progress,
			started_at,
			started_instant: Instant::now(),
			profile: BaseBuildProfile {
				game: game_key.to_string(),
				game_version: String::new(),
				started_at,
				..BaseBuildProfile::default()
			},
		}
	}

	pub fn set_game_version(&mut self, game_version: &str) {
		self.profile.game_version = game_version.to_string();
	}

	pub fn set_count(&mut self, key: impl Into<String>, value: usize) {
		self.profile.counts.insert(key.into(), value as u64);
	}

	pub fn set_inventory_file_count(&mut self, count: usize) {
		self.profile.inventory_file_count = count;
		self.set_count("inventory_file_count", count);
	}

	pub fn set_document_count(&mut self, count: usize) {
		self.profile.document_count = count;
		self.set_count("document_count", count);
	}

	pub fn set_parse_stats(&mut self, parse_stats: ParseFamilyStats) {
		self.profile.parse_stats = parse_stats;
	}

	pub fn set_encoded_size_bytes(&mut self, size: usize) {
		self.profile.encoded_size_bytes = size as u64;
		self.profile
			.counts
			.insert("encoded_size_bytes".to_string(), size as u64);
	}

	pub fn set_encoded_sections(&mut self, sections: Vec<BaseEncodedSectionProfile>) {
		self.profile.encoded_sections = sections;
	}

	pub fn run_stage<T, F>(&mut self, name: &str, f: F) -> Result<T, String>
	where
		F: FnOnce(&mut BTreeMap<String, u64>) -> Result<T, String>,
	{
		if self.emit_progress {
			eprintln!("[data build] {name}: start");
		}
		let stage_started_at = unix_timestamp_millis();
		let stage_started = Instant::now();
		let mut counts = BTreeMap::new();
		let result = f(&mut counts);
		let elapsed_ms = stage_started.elapsed().as_millis() as u64;
		let stage_finished_at = unix_timestamp_millis();
		self.profile.stages.push(BaseBuildStageProfile {
			name: name.to_string(),
			started_at: stage_started_at,
			finished_at: stage_finished_at,
			elapsed_ms,
			counts: counts.clone(),
		});
		if self.emit_progress {
			match &result {
				Ok(_) => {
					let summary = format_stage_counts(&counts);
					if summary.is_empty() {
						eprintln!("[data build] {name}: done elapsed_ms={elapsed_ms}");
					} else {
						eprintln!("[data build] {name}: done elapsed_ms={elapsed_ms} {summary}");
					}
				}
				Err(err) => {
					eprintln!("[data build] {name}: failed elapsed_ms={elapsed_ms} error={err}");
				}
			}
		}
		result
	}

	pub fn finish(mut self) -> BaseBuildProfile {
		self.profile.finished_at = unix_timestamp_millis();
		self.profile.total_elapsed_ms = self.started_instant.elapsed().as_millis() as u64;
		self.profile.started_at = self.started_at;
		self.profile
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseAnalysisSnapshot {
	pub schema_version: u32,
	pub game: String,
	pub game_version: String,
	pub analysis_rules_version: String,
	pub generated_by_cli_version: String,
	pub inventory_paths: Vec<String>,
	pub documents: Vec<BaseDocumentRecord>,
	pub parse_error_count: usize,
	pub parsed_files: usize,
	pub parse_stats: ParseFamilyStats,
	pub parsed_scripts: Vec<u8>,
	pub scopes: Vec<BaseScopeNode>,
	pub symbol_definitions: Vec<BaseSymbolDefinition>,
	pub symbol_references: Vec<BaseSymbolReference>,
	pub alias_usages: Vec<BaseAliasUsage>,
	pub key_usages: Vec<BaseKeyUsage>,
	pub scalar_assignments: Vec<BaseScalarAssignment>,
	pub localisation_definitions: Vec<BaseLocalisationDefinition>,
	pub localisation_duplicates: Vec<BaseLocalisationDuplicate>,
	pub ui_definitions: Vec<BaseUiDefinition>,
	pub resource_references: Vec<BaseResourceReference>,
	pub csv_rows: Vec<BaseCsvRow>,
	pub json_properties: Vec<BaseJsonProperty>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LegacyBaseAnalysisSnapshot {
	schema_version: u32,
	game: String,
	game_version: String,
	generated_by_cli_version: String,
	inventory_paths: Vec<String>,
	documents: Vec<BaseDocumentRecord>,
	parse_error_count: usize,
	parsed_files: usize,
	scopes: Vec<BaseScopeNode>,
	symbol_definitions: Vec<BaseSymbolDefinition>,
	symbol_references: Vec<BaseSymbolReference>,
	alias_usages: Vec<BaseAliasUsage>,
	key_usages: Vec<BaseKeyUsage>,
	scalar_assignments: Vec<BaseScalarAssignment>,
	localisation_definitions: Vec<BaseLocalisationDefinition>,
	localisation_duplicates: Vec<BaseLocalisationDuplicate>,
	ui_definitions: Vec<BaseUiDefinition>,
	resource_references: Vec<BaseResourceReference>,
	csv_rows: Vec<BaseCsvRow>,
	json_properties: Vec<BaseJsonProperty>,
}

impl From<LegacyBaseAnalysisSnapshot> for BaseAnalysisSnapshot {
	fn from(value: LegacyBaseAnalysisSnapshot) -> Self {
		Self {
			schema_version: value.schema_version,
			game: value.game,
			game_version: value.game_version,
			analysis_rules_version: analysis_rules_version().to_string(),
			generated_by_cli_version: value.generated_by_cli_version,
			inventory_paths: value.inventory_paths,
			documents: value.documents,
			parse_error_count: value.parse_error_count,
			parsed_files: value.parsed_files,
			parse_stats: ParseFamilyStats::default(),
			parsed_scripts: Vec::new(),
			scopes: value.scopes,
			symbol_definitions: value.symbol_definitions,
			symbol_references: value.symbol_references,
			alias_usages: value.alias_usages,
			key_usages: value.key_usages,
			scalar_assignments: value.scalar_assignments,
			localisation_definitions: value.localisation_definitions,
			localisation_duplicates: value.localisation_duplicates,
			ui_definitions: value.ui_definitions,
			resource_references: value.resource_references,
			csv_rows: value.csv_rows,
			json_properties: value.json_properties,
		}
	}
}

impl BaseAnalysisSnapshot {
	pub fn from_semantic_index(
		game: &Game,
		game_version: &str,
		inventory_paths: Vec<String>,
		index: &SemanticIndex,
		parse_stats: ParseFamilyStats,
	) -> Self {
		Self::from_semantic_index_with_parsed_scripts(
			game,
			game_version,
			inventory_paths,
			index,
			parse_stats,
			Vec::new(),
		)
	}

	pub fn from_semantic_index_with_parsed_scripts(
		game: &Game,
		game_version: &str,
		inventory_paths: Vec<String>,
		index: &SemanticIndex,
		parse_stats: ParseFamilyStats,
		parsed_scripts: Vec<u8>,
	) -> Self {
		Self {
			schema_version: BASE_DATA_SCHEMA_VERSION,
			game: game.key().to_string(),
			game_version: game_version.to_string(),
			analysis_rules_version: analysis_rules_version().to_string(),
			generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
			inventory_paths,
			documents: index
				.documents
				.iter()
				.map(|item| BaseDocumentRecord {
					path: normalize_path_str(&item.path),
					family: item.family,
					parse_ok: item.parse_ok,
				})
				.collect(),
			parse_error_count: parse_stats.clausewitz_mainline.parse_issue_count,
			parsed_files: index.documents.len(),
			parse_stats,
			parsed_scripts,
			scopes: index
				.scopes
				.iter()
				.map(|item| BaseScopeNode {
					kind: item.kind,
					parent: item.parent,
					this_type: item.this_type,
					aliases: item.aliases.clone(),
					path: normalize_path_str(&item.path),
					span: item.span.clone(),
					key: item.key.clone(),
				})
				.collect(),
			symbol_definitions: index
				.definitions
				.iter()
				.map(|item| BaseSymbolDefinition {
					kind: item.kind,
					name: item.name.clone(),
					module: item.module.clone(),
					local_name: item.local_name.clone(),
					path: normalize_path_str(&item.path),
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
				})
				.collect(),
			symbol_references: index
				.references
				.iter()
				.map(|item| BaseSymbolReference {
					kind: item.kind,
					name: item.name.clone(),
					module: item.module.clone(),
					path: normalize_path_str(&item.path),
					line: item.line,
					column: item.column,
					scope_id: item.scope_id,
					provided_params: item.provided_params.clone(),
					param_bindings: item.param_bindings.clone(),
				})
				.collect(),
			alias_usages: index
				.alias_usages
				.iter()
				.map(|item| BaseAliasUsage {
					alias: item.alias.clone(),
					path: normalize_path_str(&item.path),
					line: item.line,
					column: item.column,
					scope_id: item.scope_id,
				})
				.collect(),
			key_usages: index
				.key_usages
				.iter()
				.map(|item| BaseKeyUsage {
					key: item.key.clone(),
					path: normalize_path_str(&item.path),
					line: item.line,
					column: item.column,
					scope_id: item.scope_id,
					this_type: item.this_type,
				})
				.collect(),
			scalar_assignments: index
				.scalar_assignments
				.iter()
				.map(|item| BaseScalarAssignment {
					key: item.key.clone(),
					value: item.value.clone(),
					path: normalize_path_str(&item.path),
					line: item.line,
					column: item.column,
					scope_id: item.scope_id,
				})
				.collect(),
			localisation_definitions: index
				.localisation_definitions
				.iter()
				.map(|item| BaseLocalisationDefinition {
					key: item.key.clone(),
					path: normalize_path_str(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
			localisation_duplicates: index
				.localisation_duplicates
				.iter()
				.map(|item| BaseLocalisationDuplicate {
					key: item.key.clone(),
					path: normalize_path_str(&item.path),
					first_line: item.first_line,
					duplicate_line: item.duplicate_line,
				})
				.collect(),
			ui_definitions: index
				.ui_definitions
				.iter()
				.map(|item| BaseUiDefinition {
					name: item.name.clone(),
					path: normalize_path_str(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
			resource_references: index
				.resource_references
				.iter()
				.map(|item| BaseResourceReference {
					key: item.key.clone(),
					value: item.value.clone(),
					path: normalize_path_str(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
			csv_rows: index
				.csv_rows
				.iter()
				.map(|item| BaseCsvRow {
					identity: item.identity.clone(),
					path: normalize_path_str(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
			json_properties: index
				.json_properties
				.iter()
				.map(|item| BaseJsonProperty {
					key_path: item.key_path.clone(),
					path: normalize_path_str(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
		}
	}

	pub fn to_semantic_index(&self) -> SemanticIndex {
		let mod_id = base_game_mod_id(&self.game);
		let mut index = SemanticIndex {
			documents: self
				.documents
				.iter()
				.map(|item| DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					family: item.family,
					parse_ok: item.parse_ok,
				})
				.collect(),
			scopes: self
				.scopes
				.iter()
				.enumerate()
				.map(|(id, item)| ScopeNode {
					id,
					kind: item.kind,
					parent: item.parent,
					this_type: item.this_type,
					aliases: item.aliases.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					span: item.span.clone(),
					key: item.key.clone(),
				})
				.collect(),
			definitions: self
				.symbol_definitions
				.iter()
				.map(|item| SymbolDefinition {
					kind: item.kind,
					name: item.name.clone(),
					module: item.module.clone(),
					local_name: item.local_name.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
					scope_id: item.scope_id,
					declared_this_type: item.declared_this_type,
					inferred_this_type: item.inferred_this_type,
					inferred_this_mask: if !item.inferred_this_mask.is_empty() {
						item.inferred_this_mask
					} else {
						scope_mask(item.inferred_this_type)
					},
					inferred_from_mask: item.inferred_from_mask,
					inferred_root_mask: item.inferred_root_mask,
					required_params: item.required_params.clone(),
					optional_params: item.optional_params.clone(),
					param_contract: item.param_contract.clone(),
					scope_param_names: item.scope_param_names.clone(),
				})
				.collect(),
			references: self
				.symbol_references
				.iter()
				.map(|item| SymbolReference {
					kind: item.kind,
					name: item.name.clone(),
					module: item.module.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
					scope_id: item.scope_id,
					provided_params: item.provided_params.clone(),
					param_bindings: item.param_bindings.clone(),
				})
				.collect(),
			alias_usages: self
				.alias_usages
				.iter()
				.map(|item| AliasUsage {
					alias: item.alias.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
					scope_id: item.scope_id,
				})
				.collect(),
			key_usages: self
				.key_usages
				.iter()
				.map(|item| KeyUsage {
					key: item.key.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
					scope_id: item.scope_id,
					this_type: item.this_type,
				})
				.collect(),
			scalar_assignments: self
				.scalar_assignments
				.iter()
				.map(|item| ScalarAssignment {
					key: item.key.clone(),
					value: item.value.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
					scope_id: item.scope_id,
				})
				.collect(),
			localisation_definitions: self
				.localisation_definitions
				.iter()
				.map(|item| LocalisationDefinition {
					key: item.key.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
			localisation_duplicates: self
				.localisation_duplicates
				.iter()
				.map(|item| LocalisationDuplicate {
					key: item.key.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					first_line: item.first_line,
					duplicate_line: item.duplicate_line,
				})
				.collect(),
			ui_definitions: self
				.ui_definitions
				.iter()
				.map(|item| UiDefinition {
					name: item.name.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
			resource_references: self
				.resource_references
				.iter()
				.map(|item| ResourceReference {
					key: item.key.clone(),
					value: item.value.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
			csv_rows: self
				.csv_rows
				.iter()
				.map(|item| CsvRow {
					identity: item.identity.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
			json_properties: self
				.json_properties
				.iter()
				.map(|item| JsonProperty {
					key_path: item.key_path.clone(),
					mod_id: mod_id.clone(),
					path: PathBuf::from(&item.path),
					line: item.line,
					column: item.column,
				})
				.collect(),
			parse_issues: Vec::<ParseIssue>::new(),
		};
		apply_registered_param_contracts(&mut index);
		index
	}

	pub(crate) fn parsed_script_files(
		&self,
		game_root: &Path,
	) -> Result<Vec<ParsedScriptFile>, String> {
		if self.parsed_scripts.is_empty() {
			return Ok(Vec::new());
		}
		let mut parsed =
			crate::cache::parsed_scripts::decode_parsed_documents(&self.parsed_scripts)?;
		crate::cache::parsed_scripts::rebase_parsed_documents(game_root, &mut parsed);
		Ok(parsed)
	}

	pub fn document_lookup(&self) -> HashMap<&str, (&DocumentFamily, bool)> {
		self.documents
			.iter()
			.map(|item| (item.path.as_str(), (&item.family, item.parse_ok)))
			.collect()
	}

	fn section_item_counts(&self) -> Vec<(String, usize)> {
		vec![
			("metadata_fields".to_string(), 4),
			(
				"inventory_documents_items".to_string(),
				self.inventory_paths.len()
					+ self.documents.len()
					+ 2 + parse_stats_item_count(&self.parse_stats),
			),
			(
				"symbol_scope_items".to_string(),
				self.scopes.len()
					+ self.symbol_definitions.len()
					+ self.symbol_references.len()
					+ self.alias_usages.len()
					+ self.key_usages.len()
					+ self.scalar_assignments.len(),
			),
			(
				"localisation_ui_resources_items".to_string(),
				self.localisation_definitions.len()
					+ self.localisation_duplicates.len()
					+ self.ui_definitions.len()
					+ self.resource_references.len(),
			),
			(
				"structured_data_items".to_string(),
				self.csv_rows.len() + self.json_properties.len(),
			),
			(
				"parsed_scripts_bytes".to_string(),
				self.parsed_scripts.len(),
			),
		]
	}
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseDocumentRecord {
	pub path: String,
	pub family: DocumentFamily,
	pub parse_ok: bool,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseScopeNode {
	pub kind: ScopeKind,
	pub parent: Option<usize>,
	pub this_type: MaybeScope,
	pub aliases: HashMap<String, MaybeScope>,
	pub path: String,
	pub span: SourceSpan,
	#[serde(default)]
	pub key: String,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseSymbolDefinition {
	pub kind: SymbolKind,
	pub name: String,
	pub module: String,
	pub local_name: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
	pub declared_this_type: MaybeScope,
	pub inferred_this_type: MaybeScope,
	#[serde(default)]
	pub inferred_this_mask: ScopeSet,
	#[serde(default)]
	pub inferred_from_mask: ScopeSet,
	#[serde(default)]
	pub inferred_root_mask: ScopeSet,
	pub required_params: Vec<String>,
	#[serde(default)]
	pub optional_params: Vec<String>,
	#[serde(default)]
	pub param_contract: Option<ParamContract>,
	#[serde(default)]
	pub scope_param_names: Vec<String>,
}

fn scope_mask(scope_type: impl Into<ScopeSet>) -> ScopeSet {
	scope_type.into()
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseSymbolReference {
	pub kind: SymbolKind,
	pub name: String,
	pub module: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
	pub provided_params: Vec<String>,
	pub param_bindings: Vec<ParamBinding>,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseAliasUsage {
	pub alias: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseKeyUsage {
	pub key: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
	pub this_type: MaybeScope,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseScalarAssignment {
	pub key: String,
	pub value: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseLocalisationDefinition {
	pub key: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseLocalisationDuplicate {
	pub key: String,
	pub path: String,
	pub first_line: usize,
	pub duplicate_line: usize,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseUiDefinition {
	pub name: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseResourceReference {
	pub key: String,
	pub value: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseCsvRow {
	pub identity: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BaseJsonProperty {
	pub key_path: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotWireBundle {
	format_version: u32,
	schema_version: u32,
	game: String,
	game_version: String,
	sections: Vec<SnapshotWireSection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotWireSection {
	name: SnapshotWireSectionName,
	sha256: String,
	uncompressed_bytes: u64,
	compressed_bytes: u64,
	payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotWireSectionName {
	Metadata,
	InventoryDocuments,
	SymbolScope,
	LocalisationUiResources,
	StructuredData,
	ParsedScripts,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
struct SnapshotMetadataSection {
	schema_version: u32,
	game: String,
	game_version: String,
	analysis_rules_version: String,
	generated_by_cli_version: String,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
struct SnapshotInventoryDocumentsSection {
	inventory_paths: Vec<String>,
	documents: Vec<BaseDocumentRecord>,
	parse_error_count: usize,
	parsed_files: usize,
	parse_stats: ParseFamilyStats,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
struct LegacySnapshotInventoryDocumentsSection {
	inventory_paths: Vec<String>,
	documents: Vec<BaseDocumentRecord>,
	parse_error_count: usize,
	parsed_files: usize,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
struct SnapshotSymbolScopeSection {
	scopes: Vec<BaseScopeNode>,
	symbol_definitions: Vec<BaseSymbolDefinition>,
	symbol_references: Vec<BaseSymbolReference>,
	alias_usages: Vec<BaseAliasUsage>,
	key_usages: Vec<BaseKeyUsage>,
	scalar_assignments: Vec<BaseScalarAssignment>,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
struct SnapshotLocalisationUiResourcesSection {
	localisation_definitions: Vec<BaseLocalisationDefinition>,
	localisation_duplicates: Vec<BaseLocalisationDuplicate>,
	ui_definitions: Vec<BaseUiDefinition>,
	resource_references: Vec<BaseResourceReference>,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
struct SnapshotStructuredDataSection {
	csv_rows: Vec<BaseCsvRow>,
	json_properties: Vec<BaseJsonProperty>,
}

#[derive(
	Clone, Debug, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
struct SnapshotParsedScriptsSection {
	parsed_scripts: Vec<u8>,
}

#[derive(Clone, Debug)]
struct EncodedSnapshotBundle {
	bytes: Vec<u8>,
	sections: Vec<BaseEncodedSectionProfile>,
}

#[derive(Clone, Debug)]
struct SectionEncodeResult {
	wire: SnapshotWireSection,
	profile: BaseEncodedSectionProfile,
}

pub fn default_release_tag() -> String {
	format!("v{}", env!("CARGO_PKG_VERSION"))
}

pub fn base_game_mod_id(game_key: &str) -> String {
	format!("{BASE_GAME_MOD_ID_PREFIX}{game_key}")
}

pub fn resolve_game_root(config: &Config, game: &Game) -> Option<PathBuf> {
	let mut candidates = Vec::new();
	if let Some(path) = config.game_path.get(game.key()) {
		candidates.push(path.clone());
	}
	if let Some(steam_root) = config.steam_root_path.as_ref() {
		for app_id in game.steam_app_ids() {
			if let Some(path) = steam_game_install_path(steam_root, *app_id) {
				candidates.push(path);
			}
		}
	}
	dedup_candidates(candidates)
		.into_iter()
		.find(|candidate| candidate.is_dir())
}

pub fn detect_game_version(game_root: &Path) -> Option<String> {
	for candidate in [
		game_root.join("launcher-settings.json"),
		game_root.join("launcher").join("launcher-settings.json"),
		game_root.join("version.txt"),
	] {
		if !candidate.is_file() {
			continue;
		}
		if candidate.file_name() == Some(OsStr::new("version.txt")) {
			let version = fs::read_to_string(&candidate).ok()?;
			let version = version.lines().next()?.trim();
			if !version.is_empty() {
				return Some(version.to_string());
			}
			continue;
		}
		let raw = fs::read_to_string(&candidate).ok()?;
		let json = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
		for key in ["rawVersion", "version", "gameVersion"] {
			if let Some(value) = json.get(key).and_then(|value| value.as_str())
				&& !value.trim().is_empty()
			{
				return Some(value.trim().to_string());
			}
		}
	}
	None
}

pub fn resolve_game_root_and_version(
	config: &Config,
	game: &Game,
) -> Result<(PathBuf, String), String> {
	let game_root = resolve_game_root(config, game).ok_or_else(|| {
		format!(
			"failed to locate {} base game directory; configure game_path.{} or the Steam path, or use --no-game-base",
			game.key(),
			game.key()
		)
	})?;
	let version = detect_game_version(&game_root).ok_or_else(|| {
		format!(
			"failed to detect {} version; make sure launcher-settings.json or version.txt exists under {}",
			game.key(),
			game_root.display()
		)
	})?;
	Ok((game_root, version))
}

pub fn data_root() -> PathBuf {
	if let Ok(override_dir) = std::env::var(BASE_DATA_DIR_ENV) {
		return PathBuf::from(override_dir);
	}
	dirs::data_local_dir()
		.unwrap_or_else(std::env::temp_dir)
		.join("foch")
		.join("data")
}

pub fn installed_data_dir(game_key: &str, game_version: &str) -> PathBuf {
	data_root()
		.join(game_key)
		.join(sanitize_component(game_version))
}

fn installed_base_snapshot_lock_path(game_key: &str, game_version: &str) -> PathBuf {
	// Keep the persistent lock beside the replaceable version bundle. Installed
	// bundle enumeration ignores this regular file.
	data_root().join(game_key).join(format!(
		".snapshot-{}.lock",
		sanitize_component(game_version)
	))
}

fn acquire_installed_base_snapshot_lock(
	game_key: &str,
	game_version: &str,
	mode: InstalledBaseSnapshotLockMode,
) -> Result<fs::File, String> {
	let lock_path = installed_base_snapshot_lock_path(game_key, game_version);
	let parent = lock_path
		.parent()
		.expect("installed base snapshot lock path has a parent");
	fs::create_dir_all(parent).map_err(|err| {
		format!(
			"failed to create base data lock directory {}: {err}",
			parent.display()
		)
	})?;
	let lock_file = fs::OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(false)
		.open(&lock_path)
		.map_err(|err| {
			format!(
				"failed to open base data snapshot lock {}: {err}",
				lock_path.display()
			)
		})?;
	let lock_result = match mode {
		InstalledBaseSnapshotLockMode::Shared => lock_file.lock_shared(),
		InstalledBaseSnapshotLockMode::Exclusive => lock_file.lock(),
	};
	lock_result.map_err(|err| {
		format!(
			"failed to lock base data snapshot {}: {err}",
			lock_path.display()
		)
	})?;
	Ok(lock_file)
}

fn lock_installed_base_snapshot_for_install(
	game_key: &str,
	game_version: &str,
) -> Result<InstalledBaseSnapshotInstallGuard, String> {
	Ok(InstalledBaseSnapshotInstallGuard {
		_lock_file: acquire_installed_base_snapshot_lock(
			game_key,
			game_version,
			InstalledBaseSnapshotLockMode::Exclusive,
		)?,
	})
}

/// Locks one installed snapshot against replacement, then validates the exact
/// identity captured by the caller. The returned guard keeps that validation
/// current until it is dropped.
pub(crate) fn lock_and_validate_installed_base_snapshot_identity(
	game_key: &str,
	game_version: &str,
	identity: &InstalledBaseSnapshotIdentity,
) -> Result<InstalledBaseSnapshotPublicationGuard, String> {
	let guard = InstalledBaseSnapshotPublicationGuard {
		_lock_file: acquire_installed_base_snapshot_lock(
			game_key,
			game_version,
			InstalledBaseSnapshotLockMode::Shared,
		)?,
	};
	validate_installed_base_snapshot_identity(game_key, game_version, identity)?;
	Ok(guard)
}

/// Reads and verifies the selected installed snapshot, returning an explicit
/// token that can be transferred to the later load stage or another thread.
pub fn installed_base_snapshot_identity(
	game_key: &str,
	game_version: &str,
) -> Result<Option<InstalledBaseSnapshotIdentity>, String> {
	let install_dir = installed_data_dir(game_key, game_version);
	let metadata_path = install_dir.join(INSTALLED_METADATA_FILE_NAME);
	if !metadata_path.is_file() {
		return Ok(None);
	}
	let metadata = read_installed_base_metadata(&metadata_path)?;
	validate_installed_base_metadata(game_key, game_version, &metadata)?;
	let Some(snapshot_path) = resolve_installed_snapshot_path(&install_dir) else {
		return Ok(None);
	};
	let verified = read_verified_snapshot_bytes(&snapshot_path, metadata.sha256.as_deref())?;
	Ok(Some(InstalledBaseSnapshotIdentity {
		install_dir,
		metadata,
		verified,
	}))
}

/// Validates that a previously captured snapshot identity still selects the
/// installed snapshot bytes for `game_key` and `game_version`.
pub fn validate_installed_base_snapshot_identity(
	game_key: &str,
	game_version: &str,
	identity: &InstalledBaseSnapshotIdentity,
) -> Result<(), String> {
	let install_dir = installed_data_dir(game_key, game_version);
	if install_dir != identity.install_dir {
		return Err(snapshot_changed_retry_message(
			game_key,
			game_version,
			&identity.verified.identity,
		));
	}
	let metadata_path = install_dir.join(INSTALLED_METADATA_FILE_NAME);
	if !metadata_path.is_file() {
		return Err(snapshot_changed_retry_message(
			game_key,
			game_version,
			&identity.verified.identity,
		));
	}
	let metadata = read_installed_base_metadata(&metadata_path)?;
	validate_installed_base_metadata(game_key, game_version, &metadata)?;
	let Some(snapshot_path) = resolve_installed_snapshot_path(&install_dir) else {
		return Err(snapshot_changed_retry_message(
			game_key,
			game_version,
			&identity.verified.identity,
		));
	};
	if snapshot_path != identity.verified.identity.path {
		return Err(snapshot_changed_retry_message(
			game_key,
			game_version,
			&identity.verified.identity,
		));
	}
	let map_stale =
		|message: String| stale_installed_base_data_message(game_key, game_version, &message);
	verify_snapshot_sha256(
		&snapshot_path,
		metadata.sha256.as_deref(),
		&identity.verified.identity.sha256,
	)
	.map_err(&map_stale)?;
	if !verified_snapshot_is_current(&snapshot_path, &identity.verified).map_err(&map_stale)? {
		return Err(snapshot_changed_retry_message(
			game_key,
			game_version,
			&identity.verified.identity,
		));
	}
	Ok(())
}

fn read_installed_base_metadata(path: &Path) -> Result<InstalledBaseDataMetadata, String> {
	let metadata_raw = fs::read_to_string(path).map_err(|err| {
		format!(
			"failed to read base data metadata {}: {err}",
			path.display()
		)
	})?;
	serde_json::from_str(&metadata_raw).map_err(|err| {
		format!(
			"failed to parse base data metadata {}: {err}",
			path.display()
		)
	})
}

fn validate_installed_base_metadata(
	game_key: &str,
	game_version: &str,
	metadata: &InstalledBaseDataMetadata,
) -> Result<(), String> {
	if metadata.schema_version != BASE_DATA_SCHEMA_VERSION {
		return Err(stale_installed_base_data_message(
			game_key,
			game_version,
			&format!(
				"base data schema mismatch: expected {}, found {}",
				BASE_DATA_SCHEMA_VERSION, metadata.schema_version
			),
		));
	}
	if metadata.analysis_rules_version != analysis_rules_version() {
		return Err(stale_installed_base_data_message(
			game_key,
			game_version,
			&format!(
				"base data analysis rules version mismatch: expected {}, found {}",
				analysis_rules_version(),
				metadata.analysis_rules_version
			),
		));
	}
	Ok(())
}

fn read_verified_snapshot_bytes(
	path: &Path,
	expected_sha256: Option<&str>,
) -> Result<VerifiedSnapshotBytes, String> {
	const MAX_ATTEMPTS: usize = 2;
	for _ in 0..MAX_ATTEMPTS {
		let Some(stable) = read_stable_snapshot_bytes(path)? else {
			continue;
		};
		let sha256 = sha256_hex(&stable.bytes);
		let verified = VerifiedSnapshotBytes {
			identity: SnapshotContentIdentity {
				path: path.to_path_buf(),
				sha256,
			},
			file_handle: stable.file_handle,
			file_state: stable.file_state,
			bytes: stable.bytes,
		};
		// The stable read already checks the open handle and path state before
		// and after reading. Its exact bytes define the token; consumers perform
		// one final current-content check after decoding instead of rehashing here.
		verify_snapshot_sha256(path, expected_sha256, &verified.identity.sha256)?;
		return Ok(verified);
	}
	Err(format!(
		"base data snapshot changed while reading {}",
		path.display()
	))
}

fn read_stable_snapshot_bytes(path: &Path) -> Result<Option<StableSnapshotBytes>, String> {
	read_stable_snapshot_bytes_with_counter(path, true)
}

fn read_stable_snapshot_bytes_with_counter(
	path: &Path,
	count_identity_read: bool,
) -> Result<Option<StableSnapshotBytes>, String> {
	let mut file = fs::File::open(path).map_err(|err| {
		format!(
			"failed to open base data snapshot {}: {err}",
			path.display()
		)
	})?;
	#[cfg(test)]
	if count_identity_read {
		INSTALLED_SNAPSHOT_FILE_READ_COUNT.fetch_add(1, Ordering::Relaxed);
	}
	#[cfg(not(test))]
	let _ = count_identity_read;
	let before = file.metadata().map_err(|err| {
		format!(
			"failed to inspect base data snapshot {}: {err}",
			path.display()
		)
	})?;
	let file_handle = SameFileHandle::from_file(file.try_clone().map_err(|err| {
		format!(
			"failed to clone base data snapshot handle {}: {err}",
			path.display()
		)
	})?)
	.map_err(|err| {
		format!(
			"failed to identify base data snapshot {}: {err}",
			path.display()
		)
	})?;
	let mut bytes = Vec::new();
	file.read_to_end(&mut bytes).map_err(|err| {
		format!(
			"failed to read base data snapshot {}: {err}",
			path.display()
		)
	})?;
	let after = file.metadata().map_err(|err| {
		format!(
			"failed to inspect base data snapshot {}: {err}",
			path.display()
		)
	})?;
	let current = match fs::metadata(path) {
		Ok(metadata) => metadata,
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
		Err(err) => {
			return Err(format!(
				"failed to inspect base data snapshot {}: {err}",
				path.display()
			));
		}
	};
	let current_handle = match SameFileHandle::from_path(path) {
		Ok(handle) => handle,
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
		Err(err) => {
			return Err(format!(
				"failed to identify base data snapshot {}: {err}",
				path.display()
			));
		}
	};
	let before_state = snapshot_file_state(&before);
	let after_state = snapshot_file_state(&after);
	let current_state = snapshot_file_state(&current);
	if file_handle != current_handle || before_state != after_state || after_state != current_state
	{
		return Ok(None);
	}
	Ok(Some(StableSnapshotBytes {
		file_handle: Arc::new(file_handle),
		file_state: after_state,
		bytes: bytes.into(),
	}))
}

fn snapshot_file_state(metadata: &fs::Metadata) -> SnapshotFileState {
	#[cfg(unix)]
	use std::os::unix::fs::MetadataExt;

	SnapshotFileState {
		len: metadata.len(),
		modified: metadata.modified().ok(),
		#[cfg(unix)]
		device: metadata.dev(),
		#[cfg(unix)]
		inode: metadata.ino(),
		#[cfg(unix)]
		changed_seconds: metadata.ctime(),
		#[cfg(unix)]
		changed_nanoseconds: metadata.ctime_nsec(),
	}
}

fn snapshot_identity_label(identity: &SnapshotContentIdentity) -> String {
	format!("sha256:{}", identity.sha256)
}

fn snapshot_changed_retry_message(
	game_key: &str,
	game_version: &str,
	expected: &SnapshotContentIdentity,
) -> String {
	format!(
		"installed base data snapshot changed after identity verification for {game_key}@{game_version}: expected {}; retry the operation so its cache identity is rebuilt",
		snapshot_identity_label(expected)
	)
}

fn verified_snapshot_is_current(
	path: &Path,
	verified: &VerifiedSnapshotBytes,
) -> Result<bool, String> {
	#[cfg(test)]
	INSTALLED_SNAPSHOT_CURRENT_VALIDATION_COUNT.fetch_add(1, Ordering::Relaxed);
	let current_handle = match SameFileHandle::from_path(path) {
		Ok(handle) => handle,
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
		Err(err) => {
			return Err(format!(
				"failed to identify base data snapshot {}: {err}",
				path.display()
			));
		}
	};
	if current_handle != *verified.file_handle {
		return Ok(false);
	}
	let current = match fs::metadata(path) {
		Ok(metadata) => metadata,
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
		Err(err) => {
			return Err(format!(
				"failed to inspect base data snapshot {}: {err}",
				path.display()
			));
		}
	};
	if snapshot_file_state(&current) != verified.file_state {
		return Ok(false);
	}
	#[cfg(unix)]
	{
		Ok(true)
	}
	#[cfg(not(unix))]
	{
		verified_snapshot_content_matches(path, verified)
	}
}

#[cfg(any(not(unix), test))]
fn verified_snapshot_content_matches(
	path: &Path,
	verified: &VerifiedSnapshotBytes,
) -> Result<bool, String> {
	#[cfg(test)]
	INSTALLED_SNAPSHOT_CURRENT_DIGEST_COUNT.fetch_add(1, Ordering::Relaxed);
	let Some(current) = read_stable_snapshot_bytes_with_counter(path, false)? else {
		return Ok(false);
	};
	Ok(*current.file_handle == *verified.file_handle
		&& current.file_state == verified.file_state
		&& sha256_hex(&current.bytes) == verified.identity.sha256)
}

fn verify_snapshot_sha256(
	path: &Path,
	expected_sha256: Option<&str>,
	actual_sha256: &str,
) -> Result<(), String> {
	if let Some(expected_sha256) = expected_sha256
		&& actual_sha256 != expected_sha256
	{
		return Err(format!(
			"base data snapshot SHA256 verification failed for {}: expected {}, found {}",
			path.display(),
			expected_sha256,
			actual_sha256
		));
	}
	Ok(())
}

fn prune_completed_base_snapshot_cache(cache: &mut LoadedBaseSnapshotCache) {
	let mut completed = cache
		.entries
		.iter()
		.filter(|(_, entry)| entry.cell.get().is_some())
		.map(|(sha256, entry)| (sha256.clone(), entry.last_used))
		.collect::<Vec<_>>();
	if completed.len() <= COMPLETED_BASE_SNAPSHOT_CACHE_LIMIT {
		return;
	}
	completed.sort_by_key(|(_, last_used)| *last_used);
	let remove_count = completed.len() - COMPLETED_BASE_SNAPSHOT_CACHE_LIMIT;
	for (sha256, _) in completed.into_iter().take(remove_count) {
		cache.entries.remove(&sha256);
	}
}

fn loaded_base_snapshot_cell(sha256: &str) -> Arc<LoadedBaseSnapshotCell> {
	let mut cache = LOADED_BASE_SNAPSHOTS
		.get_or_init(|| Mutex::new(LoadedBaseSnapshotCache::default()))
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	cache.clock = cache.clock.wrapping_add(1);
	let last_used = cache.clock;
	if let Some(entry) = cache.entries.get_mut(sha256) {
		entry.last_used = last_used;
		return Arc::clone(&entry.cell);
	}
	let cell = Arc::new(OnceLock::new());
	cache.entries.insert(
		sha256.to_string(),
		LoadedBaseSnapshotCacheEntry {
			cell: Arc::clone(&cell),
			last_used,
		},
	);
	prune_completed_base_snapshot_cache(&mut cache);
	cell
}

fn touch_loaded_base_snapshot_cache(sha256: &str) {
	let mut cache = LOADED_BASE_SNAPSHOTS
		.get_or_init(|| Mutex::new(LoadedBaseSnapshotCache::default()))
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	cache.clock = cache.clock.wrapping_add(1);
	let last_used = cache.clock;
	if let Some(entry) = cache.entries.get_mut(sha256) {
		entry.last_used = last_used;
	}
	prune_completed_base_snapshot_cache(&mut cache);
}

fn decode_cached_base_snapshot(sha256: &str, bytes: &[u8]) -> LoadedBaseSnapshotResult {
	let result = loaded_base_snapshot_cell(sha256)
		.get_or_init(|| decode_installed_snapshot_from_bytes(bytes).map(Arc::new))
		.clone();
	touch_loaded_base_snapshot_cache(sha256);
	result
}

#[cfg(test)]
pub(crate) fn reset_installed_snapshot_test_counters() {
	INSTALLED_SNAPSHOT_FILE_READ_COUNT.store(0, Ordering::Relaxed);
	INSTALLED_SNAPSHOT_CURRENT_DIGEST_COUNT.store(0, Ordering::Relaxed);
	INSTALLED_SNAPSHOT_CURRENT_VALIDATION_COUNT.store(0, Ordering::Relaxed);
	INSTALLED_SNAPSHOT_COLD_DECODE_COUNT.store(0, Ordering::Relaxed);
	*INSTALLED_SNAPSHOT_DECODE_GATE
		.get_or_init(|| Mutex::new(None))
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

#[cfg(test)]
pub(crate) fn installed_snapshot_file_read_count() -> usize {
	INSTALLED_SNAPSHOT_FILE_READ_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn installed_snapshot_current_digest_count() -> usize {
	INSTALLED_SNAPSHOT_CURRENT_DIGEST_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn installed_snapshot_current_validation_count() -> usize {
	INSTALLED_SNAPSHOT_CURRENT_VALIDATION_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn installed_snapshot_cold_decode_count() -> usize {
	INSTALLED_SNAPSHOT_COLD_DECODE_COUNT.load(Ordering::Relaxed)
}

#[cfg(test)]
fn install_installed_snapshot_decode_gate() -> Arc<InstalledSnapshotDecodeGate> {
	let gate = Arc::new(InstalledSnapshotDecodeGate::default());
	*INSTALLED_SNAPSHOT_DECODE_GATE
		.get_or_init(|| Mutex::new(None))
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
	gate
}

#[cfg(test)]
pub(crate) fn clear_cached_loaded_base_snapshot(_path: &Path) {
	LOADED_BASE_SNAPSHOTS
		.get_or_init(|| Mutex::new(LoadedBaseSnapshotCache::default()))
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.entries
		.clear();
}

#[cfg(test)]
fn loaded_base_snapshot_cache_completed_count() -> usize {
	LOADED_BASE_SNAPSHOTS
		.get_or_init(|| Mutex::new(LoadedBaseSnapshotCache::default()))
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.entries
		.values()
		.filter(|entry| entry.cell.get().is_some())
		.count()
}

fn decode_installed_snapshot_from_bytes(bytes: &[u8]) -> Result<BaseAnalysisSnapshot, String> {
	#[cfg(test)]
	{
		INSTALLED_SNAPSHOT_COLD_DECODE_COUNT.fetch_add(1, Ordering::Relaxed);
		let gate = INSTALLED_SNAPSHOT_DECODE_GATE
			.get_or_init(|| Mutex::new(None))
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.clone();
		if let Some(gate) = gate {
			gate.enter();
		}
	}
	decode_snapshot_from_bytes(bytes)
}

/// Loads the installed snapshot, optionally bound to an explicit identity-stage token.
pub fn load_installed_base_snapshot(
	game_key: &str,
	game_version: &str,
	expected_identity: Option<&InstalledBaseSnapshotIdentity>,
) -> Result<Option<InstalledBaseSnapshot>, String> {
	let identity = match expected_identity {
		Some(identity) => identity.clone(),
		None => {
			let Some(identity) = installed_base_snapshot_identity(game_key, game_version)? else {
				return Ok(None);
			};
			identity
		}
	};
	load_installed_base_snapshot_from_identity(
		game_key,
		game_version,
		&identity,
		BaseSnapshotCurrentValidation::Immediate,
	)
	.map(Some)
}

pub(crate) fn load_installed_base_snapshot_from_identity(
	game_key: &str,
	game_version: &str,
	identity: &InstalledBaseSnapshotIdentity,
	current_validation: BaseSnapshotCurrentValidation,
) -> Result<InstalledBaseSnapshot, String> {
	let expected_install_dir = installed_data_dir(game_key, game_version);
	if identity.install_dir != expected_install_dir {
		return Err(snapshot_changed_retry_message(
			game_key,
			game_version,
			&identity.verified.identity,
		));
	}
	validate_installed_base_metadata(game_key, game_version, &identity.metadata)?;
	let map_stale =
		|message: String| stale_installed_base_data_message(game_key, game_version, &message);
	verify_snapshot_sha256(
		&identity.verified.identity.path,
		identity.metadata.sha256.as_deref(),
		&identity.verified.identity.sha256,
	)
	.map_err(&map_stale)?;
	let snapshot =
		decode_cached_base_snapshot(&identity.verified.identity.sha256, &identity.verified.bytes)
			.map_err(|message| {
			stale_installed_base_data_message(
				game_key,
				game_version,
				&format!(
					"failed to parse base data snapshot {}: {message}",
					identity.verified.identity.path.display()
				),
			)
		})?;
	validate_loaded_base_snapshot(game_key, game_version, &snapshot)?;
	if current_validation == BaseSnapshotCurrentValidation::Immediate {
		validate_installed_base_snapshot_identity(game_key, game_version, identity)?;
	}
	Ok(InstalledBaseSnapshot {
		install_dir: identity.install_dir.clone(),
		metadata: identity.metadata.clone(),
		snapshot,
	})
}

fn validate_loaded_base_snapshot(
	game_key: &str,
	game_version: &str,
	snapshot: &BaseAnalysisSnapshot,
) -> Result<(), String> {
	if snapshot.schema_version != BASE_DATA_SCHEMA_VERSION {
		return Err(stale_installed_base_data_message(
			game_key,
			game_version,
			&format!(
				"base data snapshot schema mismatch: expected {}, found {}",
				BASE_DATA_SCHEMA_VERSION, snapshot.schema_version
			),
		));
	}
	if snapshot.game != game_key || snapshot.game_version != game_version {
		return Err(format!(
			"base data content does not match request: requested {game_key}@{game_version}, found {}@{}",
			snapshot.game, snapshot.game_version
		));
	}
	if snapshot.analysis_rules_version != analysis_rules_version() {
		return Err(stale_installed_base_data_message(
			game_key,
			game_version,
			&format!(
				"base data snapshot analysis rules version mismatch: expected {}, found {}",
				analysis_rules_version(),
				snapshot.analysis_rules_version
			),
		));
	}
	Ok(())
}

fn stale_installed_base_data_message(game_key: &str, game_version: &str, reason: &str) -> String {
	format!(
		"{reason}; installed base data is stale, rerun `foch data install {game_key} --game-version {game_version}` or `foch data build {game_key} --from-game-path <game_root> --game-version {game_version} --install`"
	)
}

pub fn list_installed_base_data() -> Result<Vec<InstalledBaseDataEntry>, String> {
	let root = data_root();
	if !root.exists() {
		return Ok(Vec::new());
	}

	let mut entries = Vec::new();
	for game_dir in fs::read_dir(&root).map_err(|err| {
		format!(
			"failed to read base data directory {}: {err}",
			root.display()
		)
	})? {
		let Ok(game_dir) = game_dir else {
			continue;
		};
		if !game_dir.path().is_dir() {
			continue;
		}
		for version_dir in fs::read_dir(game_dir.path()).map_err(|err| {
			format!(
				"failed to read base data directory {}: {err}",
				game_dir.path().display()
			)
		})? {
			let Ok(version_dir) = version_dir else {
				continue;
			};
			let metadata_path = version_dir.path().join(INSTALLED_METADATA_FILE_NAME);
			if !metadata_path.is_file() {
				continue;
			}
			let raw = fs::read_to_string(&metadata_path).map_err(|err| {
				format!(
					"failed to read base data metadata {}: {err}",
					metadata_path.display()
				)
			})?;
			let metadata: InstalledBaseDataMetadata =
				serde_json::from_str(&raw).map_err(|err| {
					format!(
						"failed to parse base data metadata {}: {err}",
						metadata_path.display()
					)
				})?;
			entries.push(InstalledBaseDataEntry {
				schema_version: metadata.schema_version,
				game: metadata.game,
				game_version: metadata.game_version,
				analysis_rules_version: metadata.analysis_rules_version,
				generated_by_cli_version: metadata.generated_by_cli_version,
				source: metadata.source,
				install_path: version_dir.path().to_string_lossy().replace('\\', "/"),
				asset_name: metadata.asset_name,
				sha256: metadata.sha256,
			});
		}
	}

	entries.sort_by(|lhs, rhs| {
		(lhs.game.clone(), lhs.game_version.clone())
			.cmp(&(rhs.game.clone(), rhs.game_version.clone()))
	});
	Ok(entries)
}

pub fn build_base_snapshot(
	game: &Game,
	game_root: &Path,
	game_version: Option<&str>,
	filter: &crate::workspace::FileFilter,
) -> Result<BaseSnapshotBuildResult, String> {
	let mut observer = BaseBuildObserver::silent(game.key());
	build_base_snapshot_with_observer(game, game_root, game_version, filter, &mut observer)
}

pub fn build_base_snapshot_with_observer(
	game: &Game,
	game_root: &Path,
	game_version: Option<&str>,
	filter: &crate::workspace::FileFilter,
	observer: &mut BaseBuildObserver,
) -> Result<BaseSnapshotBuildResult, String> {
	let resolved_version = observer.run_stage("detect_version", |counts| {
		let version = match game_version {
			Some(version) => version.to_string(),
			None => detect_game_version(game_root).ok_or_else(|| {
				format!(
					"failed to detect {} version; provide --game-version or make sure a version file exists under {}",
					game.key(),
					game_root.display()
				)
			})?,
		};
		counts.insert("version_length".to_string(), version.len() as u64);
		Ok(version)
	})?;
	observer.set_game_version(&resolved_version);

	let inventory_paths = observer.run_stage("collect_inventory", |counts| {
		let paths: Vec<String> = collect_relative_files(game_root, filter)
			.into_iter()
			.map(|path| normalize_path(&path))
			.collect();
		counts.insert("file_count".to_string(), paths.len() as u64);
		Ok(paths)
	})?;
	observer.set_inventory_file_count(inventory_paths.len());

	let discovered_documents: Vec<DiscoveredTextDocument> =
		observer.run_stage("discover_documents", |counts| {
			let docs: Vec<DiscoveredTextDocument> = discover_text_documents(game_root)
				.into_iter()
				.filter(|doc| filter.accepts(&doc.relative_path))
				.collect();
			counts.insert("document_count".to_string(), docs.len() as u64);
			for (key, value) in discover_family_counts(&docs) {
				counts.insert(key, value);
			}
			Ok(docs)
		})?;
	observer.set_document_count(discovered_documents.len());

	let parsed_batch = observer.run_stage("parse_documents", |counts| {
		let batch = parse_discovered_text_documents(
			&base_game_mod_id(game.key()),
			game_root,
			&discovered_documents,
		);
		counts.insert("parsed_documents".to_string(), batch.documents.len() as u64);
		for (key, value) in parse_stats_counts(&batch.parse_stats) {
			counts.insert(key, value);
		}
		counts.insert(
			"parse_cache_hits".to_string(),
			batch.clausewitz_cache_hits as u64,
		);
		counts.insert(
			"parse_cache_misses".to_string(),
			batch.clausewitz_cache_misses as u64,
		);
		Ok(batch)
	})?;
	observer.set_parse_stats(parsed_batch.parse_stats.clone());
	for (key, value) in parse_stats_counts(&parsed_batch.parse_stats) {
		observer.profile.counts.insert(key, value);
	}
	observer.set_count("parse_cache_hits", parsed_batch.clausewitz_cache_hits);
	observer.set_count("parse_cache_misses", parsed_batch.clausewitz_cache_misses);

	let index = observer.run_stage("build_semantic_index", |counts| {
		let index = build_semantic_index_from_documents(&parsed_batch.documents);
		counts.insert("scopes".to_string(), index.scopes.len() as u64);
		counts.insert(
			"symbol_definitions".to_string(),
			index.definitions.len() as u64,
		);
		counts.insert(
			"symbol_references".to_string(),
			index.references.len() as u64,
		);
		counts.insert(
			"ui_definitions".to_string(),
			index.ui_definitions.len() as u64,
		);
		counts.insert(
			"localisation_definitions".to_string(),
			index.localisation_definitions.len() as u64,
		);
		Ok(index)
	})?;
	observer.set_count("scopes", index.scopes.len());
	observer.set_count("symbol_definitions", index.definitions.len());
	observer.set_count("symbol_references", index.references.len());
	observer.set_count("ui_definitions", index.ui_definitions.len());
	observer.set_count(
		"localisation_definitions",
		index.localisation_definitions.len(),
	);

	let snapshot = observer.run_stage("materialize_snapshot", |counts| {
		let parsed_scripts = parsed_batch
			.documents
			.iter()
			.filter_map(|document| match document {
				ParsedTextDocument::Clausewitz(file) => Some(file.clone()),
				_ => None,
			})
			.collect::<Vec<_>>();
		let encoded_parsed_scripts =
			crate::cache::parsed_scripts::encode_parsed_documents(&parsed_scripts)?;
		counts.insert("parsed_scripts".to_string(), parsed_scripts.len() as u64);
		counts.insert(
			"parsed_scripts_bytes".to_string(),
			encoded_parsed_scripts.len() as u64,
		);
		let snapshot = BaseAnalysisSnapshot::from_semantic_index_with_parsed_scripts(
			game,
			&resolved_version,
			inventory_paths,
			&index,
			parsed_batch.parse_stats.clone(),
			encoded_parsed_scripts,
		);
		for (key, value) in snapshot.section_item_counts() {
			counts.insert(key, value as u64);
		}
		Ok(snapshot)
	})?;

	let encoded = observer.run_stage("encode_snapshot", |counts| {
		let encoded = encode_snapshot_to_bytes(&snapshot)?;
		let uncompressed_bytes: u64 = encoded
			.sections
			.iter()
			.map(|item| item.uncompressed_bytes)
			.sum();
		counts.insert("encoded_size_bytes".to_string(), encoded.bytes.len() as u64);
		counts.insert("uncompressed_size_bytes".to_string(), uncompressed_bytes);
		counts.insert("section_count".to_string(), encoded.sections.len() as u64);
		for section in &encoded.sections {
			counts.insert(
				format!("{}_compressed_bytes", sanitize_metric_key(&section.name)),
				section.compressed_bytes,
			);
		}
		Ok(encoded)
	})?;
	observer.set_encoded_size_bytes(encoded.bytes.len());
	observer.set_encoded_sections(encoded.sections.clone());

	let sha256 = sha256_hex(&encoded.bytes);
	let asset_name = snapshot_asset_name(game.key(), &resolved_version);
	Ok(BaseSnapshotBuildResult {
		encoded_snapshot: encoded.bytes,
		snapshot_asset_name: asset_name,
		snapshot_sha256: sha256,
	})
}

pub fn install_built_snapshot(
	encoded_snapshot: &[u8],
	source: BaseDataSource,
	asset_name: Option<String>,
	sha256: Option<String>,
) -> Result<InstalledBaseSnapshot, String> {
	install_encoded_snapshot(
		encoded_snapshot,
		source,
		asset_name,
		EncodedSnapshotExpectations {
			sha256: sha256.as_deref(),
			..EncodedSnapshotExpectations::default()
		},
	)
}

pub fn write_release_artifacts(
	encoded_snapshot: &[u8],
	output_dir: &Path,
	release_tag: &str,
) -> Result<ReleaseArtifactOutput, String> {
	let verified =
		verify_encoded_snapshot(encoded_snapshot, EncodedSnapshotExpectations::default())?;
	let snapshot = &verified.snapshot;
	fs::create_dir_all(output_dir).map_err(|err| {
		format!(
			"failed to create output directory {}: {err}",
			output_dir.display()
		)
	})?;
	let sha256 = verified.sha256.clone();
	let asset_name = snapshot_asset_name(&snapshot.game, &snapshot.game_version);
	let snapshot_path = output_dir.join(&asset_name);
	fs::write(&snapshot_path, verified.bytes).map_err(|err| {
		format!(
			"failed to write release snapshot {}: {err}",
			snapshot_path.display()
		)
	})?;

	let manifest = ReleaseDataManifest {
		schema_version: BASE_DATA_SCHEMA_VERSION,
		cli_tag: release_tag.to_string(),
		assets: vec![ReleaseDataAsset {
			game: snapshot.game.clone(),
			game_version: snapshot.game_version.clone(),
			analysis_rules_version: snapshot.analysis_rules_version.clone(),
			asset_name: asset_name.clone(),
			sha256: sha256.clone(),
		}],
	};
	let manifest_path = output_dir.join(RELEASE_MANIFEST_FILE_NAME);
	let manifest_raw = serde_json::to_string_pretty(&manifest)
		.map_err(|err| format!("failed to serialize release manifest: {err}"))?;
	fs::write(&manifest_path, manifest_raw).map_err(|err| {
		format!(
			"failed to write release manifest {}: {err}",
			manifest_path.display()
		)
	})?;
	let coverage_path = output_dir.join(INSTALLED_COVERAGE_FILE_NAME);
	write_coverage_report(&coverage_path, snapshot)?;

	Ok(ReleaseArtifactOutput {
		snapshot_path,
		manifest_path,
		coverage_path,
		asset_name,
		sha256,
	})
}

/// Schema version of the vocabulary-manifest sidecar.
pub const VOCABULARY_MANIFEST_VERSION: u32 = 1;

/// One entry in the base-game vocabulary manifest: a defined symbol identified
/// by kind, name, and originating module.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VocabularyManifestEntry {
	pub kind: SymbolKind,
	pub name: String,
	pub module: String,
}

/// Deterministic, version-stamped manifest of the symbol vocabulary a base
/// snapshot defines. Emitted as a sidecar so cross-version existence-drift can
/// be diffed cheaply without deserializing the full snapshot (GH #35) — the
/// induced-symbol-spec counterpart to the CWT declared spec.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VocabularyManifest {
	pub manifest_version: u32,
	pub game: String,
	pub game_version: String,
	pub analysis_rules_version: String,
	pub entries: Vec<VocabularyManifestEntry>,
}

fn sorted_vocabulary_entries(snapshot: &BaseAnalysisSnapshot) -> Vec<VocabularyManifestEntry> {
	let mut entries: Vec<VocabularyManifestEntry> = snapshot
		.symbol_definitions
		.iter()
		.map(|def| VocabularyManifestEntry {
			kind: def.kind,
			name: def.name.clone(),
			module: def.module.clone(),
		})
		.collect();
	entries.sort_by(|a, b| {
		a.kind
			.as_str()
			.cmp(b.kind.as_str())
			.then_with(|| a.name.cmp(&b.name))
			.then_with(|| a.module.cmp(&b.module))
	});
	entries.dedup_by(|a, b| a.kind == b.kind && a.name == b.name && a.module == b.module);
	entries
}

/// Build the deterministic vocabulary manifest for a base snapshot.
pub fn build_vocabulary_manifest(snapshot: &BaseAnalysisSnapshot) -> VocabularyManifest {
	VocabularyManifest {
		manifest_version: VOCABULARY_MANIFEST_VERSION,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: snapshot.analysis_rules_version.clone(),
		entries: sorted_vocabulary_entries(snapshot),
	}
}

/// Write the vocabulary-manifest sidecar to `path` and return its SHA-256 digest.
fn write_vocabulary_manifest(
	path: &Path,
	snapshot: &BaseAnalysisSnapshot,
) -> Result<String, String> {
	let manifest = build_vocabulary_manifest(snapshot);
	let raw = serde_json::to_string_pretty(&manifest)
		.map_err(|err| format!("failed to serialize vocabulary manifest: {err}"))?;
	let digest = sha256_hex(raw.as_bytes());
	fs::write(path, raw).map_err(|err| {
		format!(
			"failed to write vocabulary manifest {}: {err}",
			path.display()
		)
	})?;
	Ok(digest)
}

pub fn write_snapshot_bundle(
	encoded_snapshot: &[u8],
	output_dir: &Path,
	source: BaseDataSource,
	asset_name: Option<String>,
	expected_sha256: Option<String>,
) -> Result<SnapshotBundleOutput, String> {
	let verified = verify_encoded_snapshot(
		encoded_snapshot,
		EncodedSnapshotExpectations {
			sha256: expected_sha256.as_deref(),
			..EncodedSnapshotExpectations::default()
		},
	)?;
	let snapshot = &verified.snapshot;
	fs::create_dir_all(output_dir).map_err(|err| {
		format!(
			"failed to create output directory {}: {err}",
			output_dir.display()
		)
	})?;
	let vocabulary_manifest_path = output_dir.join(INSTALLED_VOCABULARY_MANIFEST_FILE_NAME);
	let vocabulary_manifest_sha256 =
		write_vocabulary_manifest(&vocabulary_manifest_path, snapshot)?;
	let metadata = InstalledBaseDataMetadata {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: snapshot.analysis_rules_version.clone(),
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
		source,
		asset_name,
		sha256: Some(verified.sha256.clone()),
		vocabulary_manifest_sha256: Some(vocabulary_manifest_sha256),
	};
	let metadata_raw = serde_json::to_string_pretty(&metadata)
		.map_err(|err| format!("failed to serialize base data metadata: {err}"))?;
	let snapshot_path = output_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	let metadata_path = output_dir.join(INSTALLED_METADATA_FILE_NAME);
	let coverage_path = output_dir.join(INSTALLED_COVERAGE_FILE_NAME);
	fs::write(&snapshot_path, verified.bytes).map_err(|err| {
		format!(
			"failed to write snapshot bundle {}: {err}",
			snapshot_path.display()
		)
	})?;
	fs::write(&metadata_path, metadata_raw).map_err(|err| {
		format!(
			"failed to write snapshot metadata {}: {err}",
			metadata_path.display()
		)
	})?;
	write_coverage_report(&coverage_path, snapshot)?;
	Ok(SnapshotBundleOutput {
		snapshot_path,
		metadata_path,
		coverage_path,
	})
}

pub fn install_snapshot_from_release(
	game: &Game,
	game_version: &str,
	release_tag: Option<&str>,
) -> Result<InstalledBaseSnapshot, String> {
	let release_tag = release_tag.map_or_else(default_release_tag, ToString::to_string);
	let base_url = release_base_url(&release_tag);
	let mut client_builder = Client::builder();
	if base_url.contains("127.0.0.1") || base_url.contains("localhost") {
		client_builder = client_builder.no_proxy();
	}
	let client = client_builder
		.build()
		.map_err(|err| format!("failed to initialize download client: {err}"))?;
	let manifest_url = format!("{base_url}/{}", RELEASE_MANIFEST_FILE_NAME);
	let manifest = client
		.get(&manifest_url)
		.send()
		.and_then(|resp| resp.error_for_status())
		.map_err(|err| format!("failed to download release manifest {manifest_url}: {err}"))?
		.json::<ReleaseDataManifest>()
		.map_err(|err| format!("failed to parse release manifest {manifest_url}: {err}"))?;
	if manifest.schema_version != BASE_DATA_SCHEMA_VERSION {
		return Err(format!(
			"release manifest schema mismatch: expected {}, found {}",
			BASE_DATA_SCHEMA_VERSION, manifest.schema_version
		));
	}

	let asset = manifest
		.assets
		.iter()
		.find(|item| item.game == game.key() && item.game_version == game_version)
		.cloned()
		.ok_or_else(|| {
			format!(
				"release {release_tag} does not contain a base data asset for {} {}",
				game.key(),
				game_version
			)
		})?;
	let asset_url = format!("{base_url}/{}", asset.asset_name);
	let asset_bytes = client
		.get(&asset_url)
		.send()
		.and_then(|resp| resp.error_for_status())
		.map_err(|err| format!("failed to download base data asset {asset_url}: {err}"))?
		.bytes()
		.map_err(|err| format!("failed to read base data asset response {asset_url}: {err}"))?
		.to_vec();
	install_encoded_snapshot(
		&asset_bytes,
		BaseDataSource::Download,
		Some(asset.asset_name),
		EncodedSnapshotExpectations {
			game: Some(game.key()),
			game_version: Some(game_version),
			analysis_rules_version: Some(&asset.analysis_rules_version),
			sha256: Some(&asset.sha256),
		},
	)
}

struct VerifiedEncodedSnapshot<'a> {
	bytes: &'a [u8],
	sha256: String,
	snapshot: Arc<BaseAnalysisSnapshot>,
}

#[derive(Clone, Copy, Debug, Default)]
struct EncodedSnapshotExpectations<'a> {
	game: Option<&'a str>,
	game_version: Option<&'a str>,
	analysis_rules_version: Option<&'a str>,
	sha256: Option<&'a str>,
}

fn install_encoded_snapshot(
	encoded_snapshot: &[u8],
	source: BaseDataSource,
	asset_name: Option<String>,
	expected: EncodedSnapshotExpectations<'_>,
) -> Result<InstalledBaseSnapshot, String> {
	let verified = verify_encoded_snapshot(encoded_snapshot, expected)?;
	let metadata = InstalledBaseDataMetadata {
		schema_version: verified.snapshot.schema_version,
		game: verified.snapshot.game.clone(),
		game_version: verified.snapshot.game_version.clone(),
		analysis_rules_version: verified.snapshot.analysis_rules_version.clone(),
		generated_by_cli_version: verified.snapshot.generated_by_cli_version.clone(),
		source,
		asset_name,
		sha256: Some(verified.sha256.clone()),
		vocabulary_manifest_sha256: None,
	};
	write_installed_snapshot(verified, &metadata)
}

fn verify_encoded_snapshot<'a>(
	encoded_snapshot: &'a [u8],
	expected: EncodedSnapshotExpectations<'_>,
) -> Result<VerifiedEncodedSnapshot<'a>, String> {
	let sha256 = sha256_hex(encoded_snapshot);
	let snapshot = decode_cached_base_snapshot(&sha256, encoded_snapshot)
		.map_err(|err| format!("failed to verify encoded base data snapshot: {err}"))?;
	let snapshot_path = installed_data_dir(&snapshot.game, &snapshot.game_version)
		.join(INSTALLED_SNAPSHOT_FILE_NAME);
	verify_snapshot_sha256(&snapshot_path, expected.sha256, &sha256)?;
	validate_loaded_base_snapshot(&snapshot.game, &snapshot.game_version, &snapshot)?;
	if expected.game.is_some_and(|game| snapshot.game != game)
		|| expected
			.game_version
			.is_some_and(|game_version| snapshot.game_version != game_version)
	{
		return Err(format!(
			"encoded base data content mismatch: expected {}@{}, found {}@{}",
			expected.game.unwrap_or("*"),
			expected.game_version.unwrap_or("*"),
			snapshot.game,
			snapshot.game_version
		));
	}
	if let Some(expected_rules) = expected.analysis_rules_version
		&& snapshot.analysis_rules_version != expected_rules
	{
		return Err(format!(
			"encoded base data analysis rules version mismatch: expected {expected_rules}, found {}",
			snapshot.analysis_rules_version
		));
	}
	Ok(VerifiedEncodedSnapshot {
		bytes: encoded_snapshot,
		sha256,
		snapshot,
	})
}

fn write_installed_snapshot(
	verified: VerifiedEncodedSnapshot<'_>,
	metadata: &InstalledBaseDataMetadata,
) -> Result<InstalledBaseSnapshot, String> {
	let VerifiedEncodedSnapshot {
		bytes: encoded_snapshot,
		sha256: _,
		snapshot,
	} = verified;
	validate_install_metadata(metadata, &snapshot)?;
	let _install_guard =
		lock_installed_base_snapshot_for_install(&snapshot.game, &snapshot.game_version)?;
	let install_dir = installed_data_dir(&snapshot.game, &snapshot.game_version);
	let snapshot_path = install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	fs::create_dir_all(&install_dir).map_err(|err| {
		format!(
			"failed to create base data install directory {}: {err}",
			install_dir.display()
		)
	})?;
	let metadata_path = install_dir.join(INSTALLED_METADATA_FILE_NAME);
	let vocabulary_manifest_path = install_dir.join(INSTALLED_VOCABULARY_MANIFEST_FILE_NAME);
	let vocabulary_manifest_sha256 =
		write_vocabulary_manifest(&vocabulary_manifest_path, &snapshot)?;
	let mut metadata = metadata.clone();
	metadata.vocabulary_manifest_sha256 = Some(vocabulary_manifest_sha256);
	let metadata_raw = serde_json::to_string_pretty(&metadata)
		.map_err(|err| format!("failed to serialize base data metadata: {err}"))?;
	fs::write(&metadata_path, metadata_raw).map_err(|err| {
		format!(
			"failed to write base data metadata {}: {err}",
			metadata_path.display()
		)
	})?;
	fs::write(&snapshot_path, encoded_snapshot).map_err(|err| {
		format!(
			"failed to write base data snapshot {}: {err}",
			snapshot_path.display()
		)
	})?;
	let coverage_path = install_dir.join(INSTALLED_COVERAGE_FILE_NAME);
	write_coverage_report(&coverage_path, &snapshot)?;
	Ok(InstalledBaseSnapshot {
		install_dir,
		metadata,
		snapshot,
	})
}

fn validate_install_metadata(
	metadata: &InstalledBaseDataMetadata,
	snapshot: &BaseAnalysisSnapshot,
) -> Result<(), String> {
	if metadata.schema_version != snapshot.schema_version
		|| metadata.game != snapshot.game
		|| metadata.game_version != snapshot.game_version
		|| metadata.analysis_rules_version != snapshot.analysis_rules_version
		|| metadata.generated_by_cli_version != snapshot.generated_by_cli_version
	{
		return Err(format!(
			"base data install metadata does not match encoded snapshot {}@{}",
			snapshot.game, snapshot.game_version
		));
	}
	Ok(())
}

#[cfg(test)]
fn write_test_installed_snapshot(
	metadata: &InstalledBaseDataMetadata,
	encoded_snapshot: &[u8],
) -> Result<InstalledBaseSnapshot, String> {
	let verified = verify_encoded_snapshot(
		encoded_snapshot,
		EncodedSnapshotExpectations {
			game: Some(&metadata.game),
			game_version: Some(&metadata.game_version),
			analysis_rules_version: Some(&metadata.analysis_rules_version),
			sha256: metadata.sha256.as_deref(),
		},
	)?;
	write_installed_snapshot(verified, metadata)
}

fn encode_snapshot_to_bytes(
	snapshot: &BaseAnalysisSnapshot,
) -> Result<EncodedSnapshotBundle, String> {
	let ((metadata_res, inventory_res), (symbol_res, (localisation_res, structured_res))) = join(
		|| {
			join(
				|| encode_metadata_section(snapshot),
				|| encode_inventory_documents_section(snapshot),
			)
		},
		|| {
			join(
				|| encode_symbol_scope_section(snapshot),
				|| {
					join(
						|| encode_localisation_ui_resources_section(snapshot),
						|| encode_structured_data_section(snapshot),
					)
				},
			)
		},
	);
	let parsed_scripts_res = encode_parsed_scripts_section(snapshot);

	let section_results = vec![
		metadata_res?,
		inventory_res?,
		symbol_res?,
		localisation_res?,
		structured_res?,
		parsed_scripts_res?,
	];
	let bundle = SnapshotWireBundle {
		format_version: SNAPSHOT_WIRE_FORMAT_VERSION,
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		sections: section_results
			.iter()
			.map(|item| item.wire.clone())
			.collect(),
	};
	let bytes = bincode::serialize(&bundle)
		.map_err(|err| format!("failed to serialize base data snapshot bundle: {err}"))?;
	Ok(EncodedSnapshotBundle {
		bytes,
		sections: section_results
			.into_iter()
			.map(|item| item.profile)
			.collect(),
	})
}

fn decode_snapshot_from_bytes(bytes: &[u8]) -> Result<BaseAnalysisSnapshot, String> {
	if bytes.starts_with(&[0x1f, 0x8b]) {
		return decode_legacy_snapshot_from_bytes(bytes);
	}

	let bundle: SnapshotWireBundle = bincode::deserialize(bytes)
		.map_err(|err| format!("failed to parse base data snapshot bundle: {err}"))?;
	if bundle.format_version != SNAPSHOT_WIRE_FORMAT_VERSION {
		return Err(format!(
			"base data bundle version mismatch: expected {}, found {}",
			SNAPSHOT_WIRE_FORMAT_VERSION, bundle.format_version
		));
	}
	if bundle.schema_version != BASE_DATA_SCHEMA_VERSION {
		return Err(format!(
			"base data snapshot schema mismatch: expected {}, found {}",
			BASE_DATA_SCHEMA_VERSION, bundle.schema_version
		));
	}
	let mut metadata = None;
	let mut inventory = None;
	let mut symbol_scope = None;
	let mut localisation = None;
	let mut structured = None;
	let mut parsed_scripts = None;

	for section in bundle.sections.into_iter() {
		match section.name {
			SnapshotWireSectionName::Metadata => {
				metadata = Some(decode_section_payload::<SnapshotMetadataSection>(&section)?);
			}
			SnapshotWireSectionName::InventoryDocuments => {
				inventory = Some(decode_inventory_documents_section(&section)?);
			}
			SnapshotWireSectionName::SymbolScope => {
				symbol_scope = Some(decode_section_payload::<SnapshotSymbolScopeSection>(
					&section,
				)?);
			}
			SnapshotWireSectionName::LocalisationUiResources => {
				localisation = Some(decode_section_payload::<
					SnapshotLocalisationUiResourcesSection,
				>(&section)?);
			}
			SnapshotWireSectionName::StructuredData => {
				structured = Some(decode_section_payload::<SnapshotStructuredDataSection>(
					&section,
				)?);
			}
			SnapshotWireSectionName::ParsedScripts => {
				parsed_scripts = Some(decode_section_payload::<SnapshotParsedScriptsSection>(
					&section,
				)?);
			}
		}
	}

	let metadata =
		metadata.ok_or_else(|| "base data snapshot is missing metadata section".to_string())?;
	let inventory = inventory
		.ok_or_else(|| "base data snapshot is missing inventory_documents section".to_string())?;
	let symbol_scope = symbol_scope
		.ok_or_else(|| "base data snapshot is missing symbol_scope section".to_string())?;
	let localisation = localisation.ok_or_else(|| {
		"base data snapshot is missing localisation_ui_resources section".to_string()
	})?;
	let structured = structured
		.ok_or_else(|| "base data snapshot is missing structured_data section".to_string())?;
	let parsed_scripts = parsed_scripts
		.ok_or_else(|| "base data snapshot is missing parsed_scripts section".to_string())?;

	Ok(BaseAnalysisSnapshot {
		schema_version: metadata.schema_version,
		game: metadata.game,
		game_version: metadata.game_version,
		analysis_rules_version: metadata.analysis_rules_version,
		generated_by_cli_version: metadata.generated_by_cli_version,
		inventory_paths: inventory.inventory_paths,
		documents: inventory.documents,
		parse_error_count: inventory.parse_error_count,
		parsed_files: inventory.parsed_files,
		parse_stats: inventory.parse_stats,
		parsed_scripts: parsed_scripts.parsed_scripts,
		scopes: symbol_scope.scopes,
		symbol_definitions: symbol_scope.symbol_definitions,
		symbol_references: symbol_scope.symbol_references,
		alias_usages: symbol_scope.alias_usages,
		key_usages: symbol_scope.key_usages,
		scalar_assignments: symbol_scope.scalar_assignments,
		localisation_definitions: localisation.localisation_definitions,
		localisation_duplicates: localisation.localisation_duplicates,
		ui_definitions: localisation.ui_definitions,
		resource_references: localisation.resource_references,
		csv_rows: structured.csv_rows,
		json_properties: structured.json_properties,
	})
}

fn release_base_url(release_tag: &str) -> String {
	if let Ok(url) = std::env::var(BASE_DATA_RELEASE_BASE_URL_ENV) {
		return url.trim_end_matches('/').to_string();
	}
	format!("https://github.com/Acture/foch/releases/download/{release_tag}")
}

fn snapshot_asset_name(game_key: &str, game_version: &str) -> String {
	format!(
		"foch-{game_key}-base-snapshot-{}.bin",
		sanitize_component(game_version)
	)
}

fn sha256_hex(bytes: &[u8]) -> String {
	sha256_digest_hex(&Sha256::digest(bytes))
}

fn sha256_digest_hex(digest: &[u8]) -> String {
	let mut out = String::with_capacity(digest.len() * 2);
	for byte in digest {
		out.push_str(&format!("{byte:02x}"));
	}
	out
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

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

fn normalize_path_str(path: &Path) -> String {
	normalize_path(path)
}

fn discover_family_counts(docs: &[DiscoveredTextDocument]) -> BTreeMap<String, u64> {
	let mut counts = BTreeMap::new();
	for doc in docs {
		let key = format!(
			"{}_documents",
			sanitize_metric_key(document_family_name(doc.family))
		);
		*counts.entry(key).or_insert(0) += 1;
	}
	counts
}

fn parse_stats_counts(parse_stats: &ParseFamilyStats) -> BTreeMap<String, u64> {
	let mut counts = BTreeMap::new();
	for (name, stats) in [
		("clausewitz_mainline", &parse_stats.clausewitz_mainline),
		("localisation", &parse_stats.localisation),
		("csv", &parse_stats.csv),
		("json", &parse_stats.json),
	] {
		counts.insert(format!("{name}_documents"), stats.documents as u64);
		counts.insert(
			format!("{name}_parse_failed_documents"),
			stats.parse_failed_documents as u64,
		);
		counts.insert(
			format!("{name}_parse_issue_count"),
			stats.parse_issue_count as u64,
		);
	}
	counts
}

fn parse_stats_item_count(parse_stats: &ParseFamilyStats) -> usize {
	let all = [
		&parse_stats.clausewitz_mainline,
		&parse_stats.localisation,
		&parse_stats.csv,
		&parse_stats.json,
	];
	all.len() * 3
}

fn sanitize_metric_key(value: &str) -> String {
	value
		.chars()
		.map(|ch| {
			if ch.is_ascii_alphanumeric() {
				ch.to_ascii_lowercase()
			} else {
				'_'
			}
		})
		.collect()
}

fn format_stage_counts(counts: &BTreeMap<String, u64>) -> String {
	counts
		.iter()
		.map(|(key, value)| format!("{key}={value}"))
		.collect::<Vec<_>>()
		.join(" ")
}

fn unix_timestamp_millis() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |duration| duration.as_millis() as u64)
}

fn resolve_installed_snapshot_path(install_dir: &Path) -> Option<PathBuf> {
	let current = install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	if current.is_file() {
		return Some(current);
	}
	let legacy = install_dir.join(LEGACY_INSTALLED_SNAPSHOT_FILE_NAME);
	if legacy.is_file() {
		return Some(legacy);
	}
	None
}

fn encode_metadata_section(snapshot: &BaseAnalysisSnapshot) -> Result<SectionEncodeResult, String> {
	let section = SnapshotMetadataSection {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: snapshot.analysis_rules_version.clone(),
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
	};
	encode_section_payload(SnapshotWireSectionName::Metadata, "metadata", &section)
}

fn encode_inventory_documents_section(
	snapshot: &BaseAnalysisSnapshot,
) -> Result<SectionEncodeResult, String> {
	let section = SnapshotInventoryDocumentsSection {
		inventory_paths: snapshot.inventory_paths.clone(),
		documents: snapshot.documents.clone(),
		parse_error_count: snapshot.parse_error_count,
		parsed_files: snapshot.parsed_files,
		parse_stats: snapshot.parse_stats.clone(),
	};
	encode_section_payload(
		SnapshotWireSectionName::InventoryDocuments,
		"inventory_documents",
		&section,
	)
}

fn encode_symbol_scope_section(
	snapshot: &BaseAnalysisSnapshot,
) -> Result<SectionEncodeResult, String> {
	let section = SnapshotSymbolScopeSection {
		scopes: snapshot.scopes.clone(),
		symbol_definitions: snapshot.symbol_definitions.clone(),
		symbol_references: snapshot.symbol_references.clone(),
		alias_usages: snapshot.alias_usages.clone(),
		key_usages: snapshot.key_usages.clone(),
		scalar_assignments: snapshot.scalar_assignments.clone(),
	};
	encode_section_payload(
		SnapshotWireSectionName::SymbolScope,
		"symbol_scope",
		&section,
	)
}

fn encode_localisation_ui_resources_section(
	snapshot: &BaseAnalysisSnapshot,
) -> Result<SectionEncodeResult, String> {
	let section = SnapshotLocalisationUiResourcesSection {
		localisation_definitions: snapshot.localisation_definitions.clone(),
		localisation_duplicates: snapshot.localisation_duplicates.clone(),
		ui_definitions: snapshot.ui_definitions.clone(),
		resource_references: snapshot.resource_references.clone(),
	};
	encode_section_payload(
		SnapshotWireSectionName::LocalisationUiResources,
		"localisation_ui_resources",
		&section,
	)
}

fn encode_structured_data_section(
	snapshot: &BaseAnalysisSnapshot,
) -> Result<SectionEncodeResult, String> {
	let section = SnapshotStructuredDataSection {
		csv_rows: snapshot.csv_rows.clone(),
		json_properties: snapshot.json_properties.clone(),
	};
	encode_section_payload(
		SnapshotWireSectionName::StructuredData,
		"structured_data",
		&section,
	)
}

fn encode_parsed_scripts_section(
	snapshot: &BaseAnalysisSnapshot,
) -> Result<SectionEncodeResult, String> {
	let section = SnapshotParsedScriptsSection {
		parsed_scripts: snapshot.parsed_scripts.clone(),
	};
	encode_section_payload(
		SnapshotWireSectionName::ParsedScripts,
		"parsed_scripts",
		&section,
	)
}

fn encode_section_payload<T>(
	name: SnapshotWireSectionName,
	display_name: &str,
	payload: &T,
) -> Result<SectionEncodeResult, String>
where
	T: for<'a> rkyv::Serialize<
			rkyv::api::high::HighSerializer<
				rkyv::util::AlignedVec,
				rkyv::ser::allocator::ArenaHandle<'a>,
				rkyv::rancor::Error,
			>,
		>,
{
	let started = Instant::now();
	let aligned = rkyv::to_bytes::<rkyv::rancor::Error>(payload)
		.map_err(|err| format!("failed to serialize base data section {display_name}: {err}"))?;
	let raw: &[u8] = &aligned;
	let payload = gzip_bytes(raw)
		.map_err(|err| format!("failed to compress base data section {display_name}: {err}"))?;
	let profile = BaseEncodedSectionProfile {
		name: display_name.to_string(),
		elapsed_ms: started.elapsed().as_millis() as u64,
		uncompressed_bytes: raw.len() as u64,
		compressed_bytes: payload.len() as u64,
		sha256: sha256_hex(&payload),
	};
	Ok(SectionEncodeResult {
		wire: SnapshotWireSection {
			name,
			sha256: profile.sha256.clone(),
			uncompressed_bytes: profile.uncompressed_bytes,
			compressed_bytes: profile.compressed_bytes,
			payload,
		},
		profile,
	})
}

fn gzip_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
	let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
	encoder
		.write_all(bytes)
		.map_err(|err| format!("gzip write failed: {err}"))?;
	encoder
		.finish()
		.map_err(|err| format!("gzip finish failed: {err}"))
}

fn decode_section_payload<T>(section: &SnapshotWireSection) -> Result<T, String>
where
	T: rkyv::Archive,
	<T as rkyv::Archive>::Archived:
		rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>,
{
	if sha256_hex(&section.payload) != section.sha256 {
		return Err(format!(
			"base data section verification failed: {}",
			sanitize_metric_key(snapshot_section_display_name(section.name))
		));
	}
	let cursor = Cursor::new(&section.payload);
	let mut decoder = std::io::BufReader::new(GzDecoder::new(cursor));
	let expected = section.uncompressed_bytes as usize;
	let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(expected);
	let mut chunk = [0u8; 65536];
	loop {
		let n = std::io::Read::read(&mut decoder, &mut chunk).map_err(|err| {
			format!(
				"failed to decompress base data section {}: {err}",
				snapshot_section_display_name(section.name)
			)
		})?;
		if n == 0 {
			break;
		}
		aligned.extend_from_slice(&chunk[..n]);
	}
	// SAFETY: We trust our own serialized data; the byte stream was produced by rkyv::to_bytes.
	unsafe { rkyv::from_bytes_unchecked::<T, rkyv::rancor::Error>(&aligned) }.map_err(|err| {
		format!(
			"failed to parse base data section {}: {err}",
			snapshot_section_display_name(section.name)
		)
	})
}

fn decode_inventory_documents_section(
	section: &SnapshotWireSection,
) -> Result<SnapshotInventoryDocumentsSection, String> {
	match decode_section_payload::<SnapshotInventoryDocumentsSection>(section) {
		Ok(decoded) => Ok(decoded),
		Err(_) => {
			let legacy =
				decode_section_payload::<LegacySnapshotInventoryDocumentsSection>(section)?;
			Ok(SnapshotInventoryDocumentsSection {
				inventory_paths: legacy.inventory_paths,
				documents: legacy.documents,
				parse_error_count: legacy.parse_error_count,
				parsed_files: legacy.parsed_files,
				parse_stats: ParseFamilyStats::default(),
			})
		}
	}
}

fn snapshot_section_display_name(name: SnapshotWireSectionName) -> &'static str {
	match name {
		SnapshotWireSectionName::Metadata => "metadata",
		SnapshotWireSectionName::InventoryDocuments => "inventory_documents",
		SnapshotWireSectionName::SymbolScope => "symbol_scope",
		SnapshotWireSectionName::LocalisationUiResources => "localisation_ui_resources",
		SnapshotWireSectionName::StructuredData => "structured_data",
		SnapshotWireSectionName::ParsedScripts => "parsed_scripts",
	}
}

fn decode_legacy_snapshot_from_bytes(bytes: &[u8]) -> Result<BaseAnalysisSnapshot, String> {
	let cursor = Cursor::new(bytes);
	let decoder = GzDecoder::new(cursor);
	match bincode::deserialize_from::<_, BaseAnalysisSnapshot>(decoder) {
		Ok(snapshot) => Ok(snapshot),
		Err(_) => {
			let cursor = Cursor::new(bytes);
			let decoder = GzDecoder::new(cursor);
			let snapshot = bincode::deserialize_from::<_, LegacyBaseAnalysisSnapshot>(decoder)
				.map_err(|err| format!("failed to parse legacy base data snapshot: {err}"))?;
			Ok(snapshot.into())
		}
	}
}

fn collect_relative_files(root: &Path, filter: &crate::workspace::FileFilter) -> Vec<PathBuf> {
	let mut files = Vec::new();

	for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
		if !entry.file_type().is_file() {
			continue;
		}

		let path = entry.path();
		if path.file_name() == Some(OsStr::new("descriptor.mod")) {
			continue;
		}

		if let Ok(relative) = path.strip_prefix(root) {
			if !filter.accepts(relative) {
				continue;
			}
			files.push(relative.to_path_buf());
		}
	}

	files.sort();
	files
}

fn dedup_candidates(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
	let mut seen = HashSet::new();
	let mut result = Vec::new();
	for candidate in candidates {
		let key = candidate.to_string_lossy().replace('\\', "/");
		if !seen.insert(key) {
			continue;
		}
		result.push(candidate);
	}
	result
}

#[cfg(test)]
mod tests;
