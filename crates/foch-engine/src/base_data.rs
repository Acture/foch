use crate::config::Config;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use foch_core::domain::game::Game;
use foch_core::model::{
	AliasUsage, CsvRow, DocumentFamily, DocumentRecord, JsonProperty, KeyUsage,
	LocalisationDefinition, LocalisationDuplicate, ParamBinding, ParamContract, ParseFamilyStats,
	ParseIssue, ResourceReference, ScalarAssignment, ScopeKind, ScopeNode, ScopeType,
	SemanticIndex, SourceSpan, SymbolDefinition, SymbolKind, SymbolReference, UiDefinition,
};
use foch_core::utils::steam::steam_game_install_path;
use foch_language::analysis_version::analysis_rules_version;
use foch_language::analyzer::content_family::ScriptFileKind;
use foch_language::analyzer::documents::{
	DiscoveredTextDocument, build_semantic_index_from_documents, discover_text_documents,
	parse_discovered_text_documents,
};
use foch_language::analyzer::eu4_profile::eu4_content_family_for_root_family;
use foch_language::analyzer::param_contracts::apply_registered_param_contracts;
use foch_language::analyzer::semantic_index::classify_script_file;
use rayon::join;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const BASE_GAME_MOD_ID_PREFIX: &str = "__game__";
pub const BASE_DATA_DIR_ENV: &str = "FOCH_DATA_DIR";
pub const BASE_DATA_RELEASE_BASE_URL_ENV: &str = "FOCH_DATA_RELEASE_BASE_URL";
// Bump when any serialized snapshot section becomes wire-incompatible.
pub const BASE_DATA_SCHEMA_VERSION: u32 = 5;
pub const RELEASE_MANIFEST_FILE_NAME: &str = "foch-data-manifest.json";
pub const INSTALLED_SNAPSHOT_FILE_NAME: &str = "snapshot.bin";
pub const INSTALLED_METADATA_FILE_NAME: &str = "metadata.json";
pub const INSTALLED_COVERAGE_FILE_NAME: &str = "coverage.json";
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
	pub snapshot: BaseAnalysisSnapshot,
}

#[derive(Clone, Debug)]
pub struct BaseSnapshotBuildResult {
	pub snapshot: BaseAnalysisSnapshot,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageClass {
	ExcludedNonGameplay,
	ParseOnly,
	SemanticComplete,
	GraphReady,
	MergeReady,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RootCoverageEntry {
	pub root_family: String,
	pub coverage_class: CoverageClass,
	pub inventory_file_count: usize,
	pub document_count: usize,
	pub parse_failed_documents: usize,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub document_families: Vec<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub script_file_kinds: Vec<String>,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub semantic_counts: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BaseCoverageReport {
	pub schema_version: u32,
	pub game: String,
	pub game_version: String,
	pub analysis_rules_version: String,
	pub generated_by_cli_version: String,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub class_counts: BTreeMap<String, usize>,
	pub roots: Vec<RootCoverageEntry>,
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
					required_params: item.required_params.clone(),
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
					inferred_this_mask: if item.inferred_this_mask != 0 {
						item.inferred_this_mask
					} else {
						scope_type_mask(item.inferred_this_type)
					},
					required_params: item.required_params.clone(),
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
		]
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseDocumentRecord {
	pub path: String,
	pub family: DocumentFamily,
	pub parse_ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseScopeNode {
	pub kind: ScopeKind,
	pub parent: Option<usize>,
	pub this_type: ScopeType,
	pub aliases: HashMap<String, ScopeType>,
	pub path: String,
	pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseSymbolDefinition {
	pub kind: SymbolKind,
	pub name: String,
	pub module: String,
	pub local_name: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
	pub declared_this_type: ScopeType,
	pub inferred_this_type: ScopeType,
	#[serde(default)]
	pub inferred_this_mask: u8,
	pub required_params: Vec<String>,
	#[serde(default)]
	pub param_contract: Option<ParamContract>,
	#[serde(default)]
	pub scope_param_names: Vec<String>,
}

fn scope_type_mask(scope_type: ScopeType) -> u8 {
	match scope_type {
		ScopeType::Country => 0b01,
		ScopeType::Province => 0b10,
		ScopeType::Unknown => 0,
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseAliasUsage {
	pub alias: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseKeyUsage {
	pub key: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
	pub this_type: ScopeType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseScalarAssignment {
	pub key: String,
	pub value: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub scope_id: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseLocalisationDefinition {
	pub key: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseLocalisationDuplicate {
	pub key: String,
	pub path: String,
	pub first_line: usize,
	pub duplicate_line: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseUiDefinition {
	pub name: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseResourceReference {
	pub key: String,
	pub value: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseCsvRow {
	pub identity: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotMetadataSection {
	schema_version: u32,
	game: String,
	game_version: String,
	analysis_rules_version: String,
	generated_by_cli_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotInventoryDocumentsSection {
	inventory_paths: Vec<String>,
	documents: Vec<BaseDocumentRecord>,
	parse_error_count: usize,
	parsed_files: usize,
	parse_stats: ParseFamilyStats,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LegacySnapshotInventoryDocumentsSection {
	inventory_paths: Vec<String>,
	documents: Vec<BaseDocumentRecord>,
	parse_error_count: usize,
	parsed_files: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotSymbolScopeSection {
	scopes: Vec<BaseScopeNode>,
	symbol_definitions: Vec<BaseSymbolDefinition>,
	symbol_references: Vec<BaseSymbolReference>,
	alias_usages: Vec<BaseAliasUsage>,
	key_usages: Vec<BaseKeyUsage>,
	scalar_assignments: Vec<BaseScalarAssignment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotLocalisationUiResourcesSection {
	localisation_definitions: Vec<BaseLocalisationDefinition>,
	localisation_duplicates: Vec<BaseLocalisationDuplicate>,
	ui_definitions: Vec<BaseUiDefinition>,
	resource_references: Vec<BaseResourceReference>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotStructuredDataSection {
	csv_rows: Vec<BaseCsvRow>,
	json_properties: Vec<BaseJsonProperty>,
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

#[derive(Serialize)]
struct SnapshotMetadataSectionRef<'a> {
	schema_version: u32,
	game: &'a str,
	game_version: &'a str,
	analysis_rules_version: &'a str,
	generated_by_cli_version: &'a str,
}

#[derive(Serialize)]
struct SnapshotInventoryDocumentsSectionRef<'a> {
	inventory_paths: &'a [String],
	documents: &'a [BaseDocumentRecord],
	parse_error_count: usize,
	parsed_files: usize,
	parse_stats: &'a ParseFamilyStats,
}

#[derive(Serialize)]
struct SnapshotSymbolScopeSectionRef<'a> {
	scopes: &'a [BaseScopeNode],
	symbol_definitions: &'a [BaseSymbolDefinition],
	symbol_references: &'a [BaseSymbolReference],
	alias_usages: &'a [BaseAliasUsage],
	key_usages: &'a [BaseKeyUsage],
	scalar_assignments: &'a [BaseScalarAssignment],
}

#[derive(Serialize)]
struct SnapshotLocalisationUiResourcesSectionRef<'a> {
	localisation_definitions: &'a [BaseLocalisationDefinition],
	localisation_duplicates: &'a [BaseLocalisationDuplicate],
	ui_definitions: &'a [BaseUiDefinition],
	resource_references: &'a [BaseResourceReference],
}

#[derive(Serialize)]
struct SnapshotStructuredDataSectionRef<'a> {
	csv_rows: &'a [BaseCsvRow],
	json_properties: &'a [BaseJsonProperty],
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
		if candidate.file_name().and_then(|value| value.to_str()) == Some("version.txt") {
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
			"无法定位 {} 基础游戏目录；请配置 game_path.{} 或 Steam 路径，或使用 --no-game-base",
			game.key(),
			game.key()
		)
	})?;
	let version = detect_game_version(&game_root).ok_or_else(|| {
		format!(
			"无法检测 {} 版本；请确认 {} 下存在 launcher-settings.json 或 version.txt",
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

pub fn load_installed_base_snapshot(
	game_key: &str,
	game_version: &str,
) -> Result<Option<InstalledBaseSnapshot>, String> {
	let install_dir = installed_data_dir(game_key, game_version);
	let metadata_path = install_dir.join(INSTALLED_METADATA_FILE_NAME);
	let snapshot_path = resolve_installed_snapshot_path(&install_dir);
	if !metadata_path.is_file() || snapshot_path.is_none() {
		return Ok(None);
	}
	let snapshot_path = snapshot_path.expect("checked snapshot path");

	let metadata_raw = fs::read_to_string(&metadata_path)
		.map_err(|err| format!("无法读取基础数据元数据 {}: {err}", metadata_path.display()))?;
	let metadata: InstalledBaseDataMetadata = serde_json::from_str(&metadata_raw)
		.map_err(|err| format!("无法解析基础数据元数据 {}: {err}", metadata_path.display()))?;
	if metadata.schema_version != BASE_DATA_SCHEMA_VERSION {
		return Err(stale_installed_base_data_message(
			game_key,
			game_version,
			&format!(
				"基础数据 schema 不匹配: expected {}, found {}",
				BASE_DATA_SCHEMA_VERSION, metadata.schema_version
			),
		));
	}
	if metadata.analysis_rules_version != analysis_rules_version() {
		return Err(stale_installed_base_data_message(
			game_key,
			game_version,
			&format!(
				"基础数据分析规则版本不匹配: expected {}, found {}",
				analysis_rules_version(),
				metadata.analysis_rules_version
			),
		));
	}

	let snapshot = load_snapshot_from_file(&snapshot_path).map_err(|message| {
		stale_installed_base_data_message(
			game_key,
			game_version,
			&format!(
				"无法解析基础数据 snapshot {}: {message}",
				snapshot_path.display()
			),
		)
	})?;
	if snapshot.schema_version != BASE_DATA_SCHEMA_VERSION {
		return Err(stale_installed_base_data_message(
			game_key,
			game_version,
			&format!(
				"基础数据 snapshot schema 不匹配: expected {}, found {}",
				BASE_DATA_SCHEMA_VERSION, snapshot.schema_version
			),
		));
	}
	if snapshot.game != game_key || snapshot.game_version != game_version {
		return Err(format!(
			"基础数据内容与请求不匹配: requested {game_key}@{game_version}, found {}@{}",
			snapshot.game, snapshot.game_version
		));
	}
	if snapshot.analysis_rules_version != analysis_rules_version() {
		return Err(stale_installed_base_data_message(
			game_key,
			game_version,
			&format!(
				"基础数据 snapshot 分析规则版本不匹配: expected {}, found {}",
				analysis_rules_version(),
				snapshot.analysis_rules_version
			),
		));
	}

	Ok(Some(InstalledBaseSnapshot {
		install_dir,
		metadata,
		snapshot,
	}))
}

fn stale_installed_base_data_message(game_key: &str, game_version: &str, reason: &str) -> String {
	format!(
		"{reason}；已安装基础数据已过期，请重新运行 `foch data install {game_key} --game-version {game_version}`，或重新执行 `foch data build {game_key} --from-game-path <game_root> --game-version {game_version} --install`"
	)
}

pub fn list_installed_base_data() -> Result<Vec<InstalledBaseDataEntry>, String> {
	let root = data_root();
	if !root.exists() {
		return Ok(Vec::new());
	}

	let mut entries = Vec::new();
	for game_dir in fs::read_dir(&root)
		.map_err(|err| format!("无法读取基础数据目录 {}: {err}", root.display()))?
	{
		let Ok(game_dir) = game_dir else {
			continue;
		};
		if !game_dir.path().is_dir() {
			continue;
		}
		for version_dir in fs::read_dir(game_dir.path())
			.map_err(|err| format!("无法读取基础数据目录 {}: {err}", game_dir.path().display()))?
		{
			let Ok(version_dir) = version_dir else {
				continue;
			};
			let metadata_path = version_dir.path().join(INSTALLED_METADATA_FILE_NAME);
			if !metadata_path.is_file() {
				continue;
			}
			let raw = fs::read_to_string(&metadata_path).map_err(|err| {
				format!("无法读取基础数据元数据 {}: {err}", metadata_path.display())
			})?;
			let metadata: InstalledBaseDataMetadata =
				serde_json::from_str(&raw).map_err(|err| {
					format!("无法解析基础数据元数据 {}: {err}", metadata_path.display())
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
) -> Result<BaseSnapshotBuildResult, String> {
	let mut observer = BaseBuildObserver::silent(game.key());
	build_base_snapshot_with_observer(game, game_root, game_version, &mut observer)
}

pub fn build_base_snapshot_with_observer(
	game: &Game,
	game_root: &Path,
	game_version: Option<&str>,
	observer: &mut BaseBuildObserver,
) -> Result<BaseSnapshotBuildResult, String> {
	let resolved_version = observer.run_stage("detect_version", |counts| {
		let version = match game_version {
			Some(version) => version.to_string(),
			None => detect_game_version(game_root).ok_or_else(|| {
				format!(
					"无法检测 {} 版本；请提供 --game-version 或确认 {} 下存在版本文件",
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
		let paths: Vec<String> = collect_relative_files(game_root)
			.into_iter()
			.map(|path| normalize_path(&path))
			.collect();
		counts.insert("file_count".to_string(), paths.len() as u64);
		Ok(paths)
	})?;
	observer.set_inventory_file_count(inventory_paths.len());

	let discovered_documents: Vec<DiscoveredTextDocument> =
		observer.run_stage("discover_documents", |counts| {
			let docs = discover_text_documents(game_root);
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
		let snapshot = BaseAnalysisSnapshot::from_semantic_index(
			game,
			&resolved_version,
			inventory_paths,
			&index,
			parsed_batch.parse_stats.clone(),
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
		snapshot,
		encoded_snapshot: encoded.bytes,
		snapshot_asset_name: asset_name,
		snapshot_sha256: sha256,
	})
}

#[derive(Default)]
struct CoverageAccumulator {
	inventory_file_count: usize,
	document_count: usize,
	parse_failed_documents: usize,
	document_families: BTreeMap<String, usize>,
	script_file_kinds: BTreeMap<String, usize>,
	semantic_counts: BTreeMap<String, usize>,
}

impl CoverageAccumulator {
	fn increment_document_family(&mut self, family: DocumentFamily) {
		let key = document_family_name(family).to_string();
		*self.document_families.entry(key).or_insert(0) += 1;
	}

	fn increment_script_file_kind(&mut self, kind: &str) {
		*self.script_file_kinds.entry(kind.to_string()).or_insert(0) += 1;
	}

	fn increment_semantic_count(&mut self, key: &str) {
		*self.semantic_counts.entry(key.to_string()).or_insert(0) += 1;
	}

	fn has_only_other_clausewitz(&self) -> bool {
		!self.script_file_kinds.is_empty()
			&& self
				.script_file_kinds
				.keys()
				.all(|item| item.as_str() == "other")
	}

	fn has_script_file_kind(&self, kind: &str) -> bool {
		self.script_file_kinds.contains_key(kind)
	}

	fn has_semantic_count(&self, key: &str) -> bool {
		self.semantic_counts.contains_key(key)
	}

	fn has_graph_semantics(&self) -> bool {
		self.semantic_counts.contains_key("symbol_definitions")
			|| self.semantic_counts.contains_key("symbol_references")
			|| self.semantic_counts.contains_key("alias_usages")
			|| self.semantic_counts.contains_key("key_usages")
			|| self.semantic_counts.contains_key("scalar_assignments")
	}

	fn has_semantic_surface(&self) -> bool {
		self.has_graph_semantics()
			|| self
				.semantic_counts
				.contains_key("localisation_definitions")
			|| self.semantic_counts.contains_key("localisation_duplicates")
			|| self.semantic_counts.contains_key("ui_definitions")
			|| self.semantic_counts.contains_key("resource_references")
			|| self.semantic_counts.contains_key("csv_rows")
			|| self.semantic_counts.contains_key("json_properties")
			|| self
				.script_file_kinds
				.keys()
				.any(|item| item.as_str() != "other")
	}
}

pub fn build_coverage_report(snapshot: &BaseAnalysisSnapshot) -> BaseCoverageReport {
	let mut roots: BTreeMap<String, CoverageAccumulator> = BTreeMap::new();
	for path in &snapshot.inventory_paths {
		let Some(root_family) = coverage_root_family(path) else {
			continue;
		};
		roots.entry(root_family).or_default().inventory_file_count += 1;
	}
	for document in &snapshot.documents {
		let Some(root_family) = coverage_root_family(&document.path) else {
			continue;
		};
		let entry = roots.entry(root_family).or_default();
		entry.document_count += 1;
		if !document.parse_ok {
			entry.parse_failed_documents += 1;
		}
		entry.increment_document_family(document.family);
		if document.family == DocumentFamily::Clausewitz {
			let kind = classify_script_file(Path::new(&document.path));
			entry.increment_script_file_kind(script_file_kind_name(kind));
		}
	}
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.symbol_definitions,
		"symbol_definitions",
	);
	accumulate_semantic_counts(&mut roots, &snapshot.symbol_references, "symbol_references");
	accumulate_semantic_counts(&mut roots, &snapshot.alias_usages, "alias_usages");
	accumulate_semantic_counts(&mut roots, &snapshot.key_usages, "key_usages");
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.scalar_assignments,
		"scalar_assignments",
	);
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.localisation_definitions,
		"localisation_definitions",
	);
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.localisation_duplicates,
		"localisation_duplicates",
	);
	accumulate_semantic_counts(&mut roots, &snapshot.ui_definitions, "ui_definitions");
	accumulate_semantic_counts(
		&mut roots,
		&snapshot.resource_references,
		"resource_references",
	);
	accumulate_semantic_counts(&mut roots, &snapshot.csv_rows, "csv_rows");
	accumulate_semantic_counts(&mut roots, &snapshot.json_properties, "json_properties");

	let mut class_counts: BTreeMap<String, usize> = BTreeMap::new();
	let entries: Vec<RootCoverageEntry> = roots
		.into_iter()
		.map(|(root_family, accumulator)| {
			let coverage_class = coverage_class_for_root(&root_family, &accumulator);
			*class_counts
				.entry(coverage_class_name(coverage_class).to_string())
				.or_insert(0) += 1;
			RootCoverageEntry {
				root_family,
				coverage_class,
				inventory_file_count: accumulator.inventory_file_count,
				document_count: accumulator.document_count,
				parse_failed_documents: accumulator.parse_failed_documents,
				document_families: accumulator.document_families.into_keys().collect(),
				script_file_kinds: accumulator.script_file_kinds.into_keys().collect(),
				semantic_counts: accumulator.semantic_counts,
			}
		})
		.collect();

	BaseCoverageReport {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: snapshot.analysis_rules_version.clone(),
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
		class_counts,
		roots: entries,
	}
}

fn coverage_class_for_root(root_family: &str, accumulator: &CoverageAccumulator) -> CoverageClass {
	if is_excluded_non_gameplay_root(root_family) {
		CoverageClass::ExcludedNonGameplay
	} else if let Some(classification) = content_family_coverage_class(root_family, accumulator) {
		classification
	} else if accumulator.has_only_other_clausewitz() {
		CoverageClass::ParseOnly
	} else if accumulator.has_graph_semantics() {
		CoverageClass::GraphReady
	} else if accumulator.has_semantic_surface() {
		CoverageClass::SemanticComplete
	} else {
		CoverageClass::ParseOnly
	}
}

fn content_family_coverage_class(
	root_family: &str,
	accumulator: &CoverageAccumulator,
) -> Option<CoverageClass> {
	let descriptor = eu4_content_family_for_root_family(root_family)?;
	if descriptor.capabilities.merge_ready {
		return Some(CoverageClass::MergeReady);
	}
	if descriptor.capabilities.graph_ready {
		return Some(if accumulator.has_graph_semantics() {
			CoverageClass::GraphReady
		} else {
			CoverageClass::ParseOnly
		});
	}
	if descriptor.capabilities.semantic_complete {
		return Some(
			if accumulator.has_semantic_count("resource_references")
				&& accumulator
					.has_script_file_kind(script_file_kind_name(descriptor.script_file_kind))
			{
				CoverageClass::SemanticComplete
			} else {
				CoverageClass::ParseOnly
			},
		);
	}
	Some(CoverageClass::ParseOnly)
}

fn coverage_class_name(classification: CoverageClass) -> &'static str {
	match classification {
		CoverageClass::ExcludedNonGameplay => "excluded_non_gameplay",
		CoverageClass::ParseOnly => "parse_only",
		CoverageClass::SemanticComplete => "semantic_complete",
		CoverageClass::GraphReady => "graph_ready",
		CoverageClass::MergeReady => "merge_ready",
	}
}

fn script_file_kind_name(kind: ScriptFileKind) -> &'static str {
	match kind {
		ScriptFileKind::Events => "events",
		ScriptFileKind::OnActions => "on_actions",
		ScriptFileKind::Decisions => "decisions",
		ScriptFileKind::ScriptedEffects => "scripted_effects",
		ScriptFileKind::ScriptedTriggers => "scripted_triggers",
		ScriptFileKind::DiplomaticActions => "diplomatic_actions",
		ScriptFileKind::TriggeredModifiers => "triggered_modifiers",
		ScriptFileKind::Defines => "defines",
		ScriptFileKind::Achievements => "achievements",
		ScriptFileKind::Ages => "ages",
		ScriptFileKind::Buildings => "buildings",
		ScriptFileKind::Institutions => "institutions",
		ScriptFileKind::ProvinceTriggeredModifiers => "province_triggered_modifiers",
		ScriptFileKind::Ideas => "ideas",
		ScriptFileKind::GreatProjects => "great_projects",
		ScriptFileKind::GovernmentReforms => "government_reforms",
		ScriptFileKind::Cultures => "cultures",
		ScriptFileKind::CustomGui => "custom_gui",
		ScriptFileKind::AdvisorTypes => "advisortypes",
		ScriptFileKind::EventModifiers => "event_modifiers",
		ScriptFileKind::CbTypes => "cb_types",
		ScriptFileKind::GovernmentNames => "government_names",
		ScriptFileKind::CustomizableLocalization => "customizable_localization",
		ScriptFileKind::Missions => "missions",
		ScriptFileKind::NewDiplomaticActions => "new_diplomatic_actions",
		ScriptFileKind::CountryTags => "country_tags",
		ScriptFileKind::Countries => "countries",
		ScriptFileKind::CountryHistory => "country_history",
		ScriptFileKind::ProvinceHistory => "province_history",
		ScriptFileKind::ProvinceNames => "province_names",
		ScriptFileKind::DiplomacyHistory => "diplomacy_history",
		ScriptFileKind::AdvisorHistory => "advisor_history",
		ScriptFileKind::Wars => "wars",
		ScriptFileKind::Units => "units",
		ScriptFileKind::Religions => "religions",
		ScriptFileKind::SubjectTypes => "subject_types",
		ScriptFileKind::RebelTypes => "rebel_types",
		ScriptFileKind::Disasters => "disasters",
		ScriptFileKind::GovernmentMechanics => "government_mechanics",
		ScriptFileKind::ChurchAspects => "church_aspects",
		ScriptFileKind::Factions => "factions",
		ScriptFileKind::Hegemons => "hegemons",
		ScriptFileKind::PersonalDeities => "personal_deities",
		ScriptFileKind::FetishistCults => "fetishist_cults",
		ScriptFileKind::PeaceTreaties => "peace_treaties",
		ScriptFileKind::Bookmarks => "bookmarks",
		ScriptFileKind::Policies => "policies",
		ScriptFileKind::MercenaryCompanies => "mercenary_companies",
		ScriptFileKind::Technologies => "technologies",
		ScriptFileKind::TechnologyGroups => "technology_groups",
		ScriptFileKind::EstateAgendas => "estate_agendas",
		ScriptFileKind::EstatePrivileges => "estate_privileges",
		ScriptFileKind::Estates => "estates",
		ScriptFileKind::ParliamentBribes => "parliament_bribes",
		ScriptFileKind::ParliamentIssues => "parliament_issues",
		ScriptFileKind::StateEdicts => "state_edicts",
		ScriptFileKind::Ui => "ui",
		ScriptFileKind::Other => "other",
	}
}

fn accumulate_semantic_counts<T>(
	roots: &mut BTreeMap<String, CoverageAccumulator>,
	items: &[T],
	metric_name: &str,
) where
	T: CoveragePath,
{
	for item in items {
		let Some(root_family) = coverage_root_family(item.coverage_path()) else {
			continue;
		};
		roots
			.entry(root_family)
			.or_default()
			.increment_semantic_count(metric_name);
	}
}

trait CoveragePath {
	fn coverage_path(&self) -> &str;
}

impl CoveragePath for BaseSymbolDefinition {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseSymbolReference {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseAliasUsage {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseKeyUsage {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseScalarAssignment {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseLocalisationDefinition {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseLocalisationDuplicate {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseUiDefinition {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseResourceReference {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseCsvRow {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

impl CoveragePath for BaseJsonProperty {
	fn coverage_path(&self) -> &str {
		&self.path
	}
}

fn coverage_root_family(path: &str) -> Option<String> {
	let normalized = path.replace('\\', "/");
	if !is_tracked_non_binary_path(&normalized) {
		return None;
	}
	let parts: Vec<&str> = normalized
		.split('/')
		.filter(|item| !item.is_empty())
		.collect();
	if parts.is_empty() {
		return None;
	}
	if parts[0] == "events"
		&& parts.get(1) == Some(&"common")
		&& let Some(group) = parts.get(2)
	{
		return Some(format!("events/common/{}", strip_extension(group)));
	}
	if parts[0] == "events" && parts.get(1) == Some(&"decisions") {
		return Some("events/decisions".to_string());
	}
	match parts[0] {
		"common" | "history" | "map" => {
			let group = parts.get(1)?;
			let family = if parts.len() == 2 {
				strip_extension(group)
			} else {
				group
			};
			Some(format!("{}/{}", parts[0], family))
		}
		_ if parts.len() == 1 => Some(parts[0].to_string()),
		_ => Some(parts[0].to_string()),
	}
}

fn strip_extension(value: &str) -> &str {
	value.rsplit_once('.').map_or(value, |(stem, _)| stem)
}

fn is_tracked_non_binary_path(path: &str) -> bool {
	let normalized = path.to_ascii_lowercase();
	matches!(
		normalized.rsplit('.').next(),
		Some("txt" | "gui" | "gfx" | "asset" | "mod" | "lua" | "yml" | "yaml" | "csv" | "json")
	)
}

fn is_excluded_non_gameplay_root(root_family: &str) -> bool {
	matches!(
		root_family,
		"licenses"
			| "patchnotes"
			| "ebook" | "legal_notes"
			| "builtin_dlc"
			| "dlc_metadata"
			| "hints" | "tools"
			| "tests" | "ThirdPartyLicenses.txt"
			| "checksum_manifest.txt"
			| "clausewitz_branch.txt"
			| "clausewitz_rev.txt"
			| "eu4_branch.txt"
			| "eu4_rev.txt"
			| "steam.txt"
			| "描述.txt"
			| "launcher-settings.json"
			| "settings-layout.json"
	)
}

fn write_coverage_report(path: &Path, snapshot: &BaseAnalysisSnapshot) -> Result<(), String> {
	let coverage = build_coverage_report(snapshot);
	let raw = serde_json::to_string_pretty(&coverage)
		.map_err(|err| format!("无法序列化基础数据 coverage: {err}"))?;
	fs::write(path, raw)
		.map_err(|err| format!("无法写入基础数据 coverage {}: {err}", path.display()))
}

pub fn install_built_snapshot(
	snapshot: &BaseAnalysisSnapshot,
	encoded_snapshot: &[u8],
	source: BaseDataSource,
	asset_name: Option<String>,
	sha256: Option<String>,
) -> Result<InstalledBaseSnapshot, String> {
	let metadata = InstalledBaseDataMetadata {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: snapshot.analysis_rules_version.clone(),
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
		source,
		asset_name,
		sha256,
	};
	write_installed_snapshot(snapshot, &metadata, encoded_snapshot)
}

pub fn write_release_artifacts(
	snapshot: &BaseAnalysisSnapshot,
	encoded_snapshot: &[u8],
	output_dir: &Path,
	release_tag: &str,
) -> Result<ReleaseArtifactOutput, String> {
	fs::create_dir_all(output_dir)
		.map_err(|err| format!("无法创建输出目录 {}: {err}", output_dir.display()))?;
	let sha256 = sha256_hex(encoded_snapshot);
	let asset_name = snapshot_asset_name(&snapshot.game, &snapshot.game_version);
	let snapshot_path = output_dir.join(&asset_name);
	fs::write(&snapshot_path, encoded_snapshot).map_err(|err| {
		format!(
			"无法写入 release snapshot {}: {err}",
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
		.map_err(|err| format!("无法序列化 release manifest: {err}"))?;
	fs::write(&manifest_path, manifest_raw).map_err(|err| {
		format!(
			"无法写入 release manifest {}: {err}",
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

pub fn write_snapshot_bundle(
	snapshot: &BaseAnalysisSnapshot,
	encoded_snapshot: &[u8],
	output_dir: &Path,
	source: BaseDataSource,
	asset_name: Option<String>,
	sha256: Option<String>,
) -> Result<SnapshotBundleOutput, String> {
	fs::create_dir_all(output_dir)
		.map_err(|err| format!("无法创建输出目录 {}: {err}", output_dir.display()))?;
	let metadata = InstalledBaseDataMetadata {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: snapshot.analysis_rules_version.clone(),
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
		source,
		asset_name,
		sha256,
	};
	let metadata_raw = serde_json::to_string_pretty(&metadata)
		.map_err(|err| format!("无法序列化基础数据元数据: {err}"))?;
	let snapshot_path = output_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	let metadata_path = output_dir.join(INSTALLED_METADATA_FILE_NAME);
	let coverage_path = output_dir.join(INSTALLED_COVERAGE_FILE_NAME);
	fs::write(&snapshot_path, encoded_snapshot).map_err(|err| {
		format!(
			"无法写入 snapshot bundle {}: {err}",
			snapshot_path.display()
		)
	})?;
	fs::write(&metadata_path, metadata_raw).map_err(|err| {
		format!(
			"无法写入 snapshot metadata {}: {err}",
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
		.map_err(|err| format!("无法初始化下载客户端: {err}"))?;
	let manifest_url = format!("{base_url}/{}", RELEASE_MANIFEST_FILE_NAME);
	let manifest = client
		.get(&manifest_url)
		.send()
		.and_then(|resp| resp.error_for_status())
		.map_err(|err| format!("无法下载 release manifest {manifest_url}: {err}"))?
		.json::<ReleaseDataManifest>()
		.map_err(|err| format!("无法解析 release manifest {manifest_url}: {err}"))?;
	if manifest.schema_version != BASE_DATA_SCHEMA_VERSION {
		return Err(format!(
			"release manifest schema 不匹配: expected {}, found {}",
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
				"release {release_tag} 中找不到 {} {} 的基础数据资产",
				game.key(),
				game_version
			)
		})?;
	if asset.analysis_rules_version != analysis_rules_version() {
		return Err(format!(
			"release 基础数据分析规则版本不匹配: expected {}, found {}",
			analysis_rules_version(),
			asset.analysis_rules_version
		));
	}
	let asset_url = format!("{base_url}/{}", asset.asset_name);
	let asset_bytes = client
		.get(&asset_url)
		.send()
		.and_then(|resp| resp.error_for_status())
		.map_err(|err| format!("无法下载基础数据资产 {asset_url}: {err}"))?
		.bytes()
		.map_err(|err| format!("无法读取基础数据资产响应 {asset_url}: {err}"))?
		.to_vec();
	let digest = sha256_hex(&asset_bytes);
	if digest != asset.sha256 {
		return Err(format!(
			"基础数据资产 SHA256 校验失败: expected {}, found {}",
			asset.sha256, digest
		));
	}

	let snapshot = decode_snapshot_from_bytes(&asset_bytes)?;
	if snapshot.game != game.key() || snapshot.game_version != game_version {
		return Err(format!(
			"下载的基础数据资产内容不匹配: expected {}@{}, found {}@{}",
			game.key(),
			game_version,
			snapshot.game,
			snapshot.game_version
		));
	}
	if snapshot.analysis_rules_version != asset.analysis_rules_version {
		return Err(format!(
			"下载的基础数据分析规则版本不匹配: manifest {}, snapshot {}",
			asset.analysis_rules_version, snapshot.analysis_rules_version
		));
	}

	let metadata = InstalledBaseDataMetadata {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		analysis_rules_version: snapshot.analysis_rules_version.clone(),
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
		source: BaseDataSource::Download,
		asset_name: Some(asset.asset_name),
		sha256: Some(asset.sha256),
	};
	write_installed_snapshot(&snapshot, &metadata, &asset_bytes)
}

fn write_installed_snapshot(
	snapshot: &BaseAnalysisSnapshot,
	metadata: &InstalledBaseDataMetadata,
	encoded_snapshot: &[u8],
) -> Result<InstalledBaseSnapshot, String> {
	let install_dir = installed_data_dir(&snapshot.game, &snapshot.game_version);
	fs::create_dir_all(&install_dir)
		.map_err(|err| format!("无法创建基础数据安装目录 {}: {err}", install_dir.display()))?;
	let metadata_path = install_dir.join(INSTALLED_METADATA_FILE_NAME);
	let snapshot_path = install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	let metadata_raw = serde_json::to_string_pretty(metadata)
		.map_err(|err| format!("无法序列化基础数据元数据: {err}"))?;
	fs::write(&metadata_path, metadata_raw)
		.map_err(|err| format!("无法写入基础数据元数据 {}: {err}", metadata_path.display()))?;
	fs::write(&snapshot_path, encoded_snapshot).map_err(|err| {
		format!(
			"无法写入基础数据 snapshot {}: {err}",
			snapshot_path.display()
		)
	})?;
	let coverage_path = install_dir.join(INSTALLED_COVERAGE_FILE_NAME);
	write_coverage_report(&coverage_path, snapshot)?;
	Ok(InstalledBaseSnapshot {
		install_dir,
		metadata: metadata.clone(),
		snapshot: snapshot.clone(),
	})
}

fn load_snapshot_from_file(path: &Path) -> Result<BaseAnalysisSnapshot, String> {
	let raw = fs::read(path)
		.map_err(|err| format!("无法打开基础数据 snapshot {}: {err}", path.display()))?;
	decode_snapshot_from_bytes(&raw)
		.map_err(|err| format!("无法解析基础数据 snapshot {}: {err}", path.display()))
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

	let section_results = vec![
		metadata_res?,
		inventory_res?,
		symbol_res?,
		localisation_res?,
		structured_res?,
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
		.map_err(|err| format!("无法序列化基础数据 snapshot bundle: {err}"))?;
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
		.map_err(|err| format!("无法解析基础数据 snapshot bundle: {err}"))?;
	if bundle.format_version != SNAPSHOT_WIRE_FORMAT_VERSION {
		return Err(format!(
			"基础数据 bundle 版本不匹配: expected {}, found {}",
			SNAPSHOT_WIRE_FORMAT_VERSION, bundle.format_version
		));
	}
	let mut metadata = None;
	let mut inventory = None;
	let mut symbol_scope = None;
	let mut localisation = None;
	let mut structured = None;

	for section in bundle.sections {
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
		}
	}

	let metadata = metadata.ok_or_else(|| "基础数据 snapshot 缺少 metadata section".to_string())?;
	let inventory = inventory
		.ok_or_else(|| "基础数据 snapshot 缺少 inventory_documents section".to_string())?;
	let symbol_scope =
		symbol_scope.ok_or_else(|| "基础数据 snapshot 缺少 symbol_scope section".to_string())?;
	let localisation = localisation
		.ok_or_else(|| "基础数据 snapshot 缺少 localisation_ui_resources section".to_string())?;
	let structured =
		structured.ok_or_else(|| "基础数据 snapshot 缺少 structured_data section".to_string())?;

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
	let digest = Sha256::digest(bytes);
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

fn document_family_name(family: DocumentFamily) -> &'static str {
	match family {
		DocumentFamily::Clausewitz => "clausewitz",
		DocumentFamily::Localisation => "localisation",
		DocumentFamily::Csv => "csv",
		DocumentFamily::Json => "json",
	}
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
	let section = SnapshotMetadataSectionRef {
		schema_version: snapshot.schema_version,
		game: &snapshot.game,
		game_version: &snapshot.game_version,
		analysis_rules_version: &snapshot.analysis_rules_version,
		generated_by_cli_version: &snapshot.generated_by_cli_version,
	};
	encode_section_payload(SnapshotWireSectionName::Metadata, "metadata", &section)
}

fn encode_inventory_documents_section(
	snapshot: &BaseAnalysisSnapshot,
) -> Result<SectionEncodeResult, String> {
	let section = SnapshotInventoryDocumentsSectionRef {
		inventory_paths: &snapshot.inventory_paths,
		documents: &snapshot.documents,
		parse_error_count: snapshot.parse_error_count,
		parsed_files: snapshot.parsed_files,
		parse_stats: &snapshot.parse_stats,
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
	let section = SnapshotSymbolScopeSectionRef {
		scopes: &snapshot.scopes,
		symbol_definitions: &snapshot.symbol_definitions,
		symbol_references: &snapshot.symbol_references,
		alias_usages: &snapshot.alias_usages,
		key_usages: &snapshot.key_usages,
		scalar_assignments: &snapshot.scalar_assignments,
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
	let section = SnapshotLocalisationUiResourcesSectionRef {
		localisation_definitions: &snapshot.localisation_definitions,
		localisation_duplicates: &snapshot.localisation_duplicates,
		ui_definitions: &snapshot.ui_definitions,
		resource_references: &snapshot.resource_references,
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
	let section = SnapshotStructuredDataSectionRef {
		csv_rows: &snapshot.csv_rows,
		json_properties: &snapshot.json_properties,
	};
	encode_section_payload(
		SnapshotWireSectionName::StructuredData,
		"structured_data",
		&section,
	)
}

fn encode_section_payload<T: Serialize>(
	name: SnapshotWireSectionName,
	display_name: &str,
	payload: &T,
) -> Result<SectionEncodeResult, String> {
	let started = Instant::now();
	let raw = bincode::serialize(payload)
		.map_err(|err| format!("无法序列化基础数据 section {display_name}: {err}"))?;
	let payload = gzip_bytes(&raw)
		.map_err(|err| format!("无法压缩基础数据 section {display_name}: {err}"))?;
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

fn decode_section_payload<T: for<'de> Deserialize<'de>>(
	section: &SnapshotWireSection,
) -> Result<T, String> {
	if sha256_hex(&section.payload) != section.sha256 {
		return Err(format!(
			"基础数据 section 校验失败: {}",
			sanitize_metric_key(snapshot_section_display_name(section.name))
		));
	}
	let cursor = Cursor::new(&section.payload);
	let decoder = GzDecoder::new(cursor);
	bincode::deserialize_from(decoder).map_err(|err| {
		format!(
			"无法解析基础数据 section {}: {err}",
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
				.map_err(|err| format!("无法解析 legacy 基础数据 snapshot: {err}"))?;
			Ok(snapshot.into())
		}
	}
}

fn collect_relative_files(root: &Path) -> Vec<PathBuf> {
	let mut files = Vec::new();

	for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
		if !entry.file_type().is_file() {
			continue;
		}

		let path = entry.path();
		if path.file_name().and_then(|name| name.to_str()) == Some("descriptor.mod") {
			continue;
		}

		if let Ok(relative) = path.strip_prefix(root) {
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

#[allow(dead_code)]
fn modified_nanos(metadata: &fs::Metadata) -> u128 {
	metadata
		.modified()
		.ok()
		.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
		.map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
	use super::{
		BASE_DATA_DIR_ENV, BASE_DATA_SCHEMA_VERSION, BaseAnalysisSnapshot, BaseDataSource,
		BaseSymbolDefinition, CoverageClass, INSTALLED_COVERAGE_FILE_NAME,
		INSTALLED_SNAPSHOT_FILE_NAME, InstalledBaseDataMetadata, build_coverage_report,
		decode_snapshot_from_bytes, encode_snapshot_to_bytes, load_installed_base_snapshot,
		write_installed_snapshot, write_snapshot_bundle,
	};
	use foch_core::domain::game::Game;
	use foch_core::model::{
		DocumentFamily, DocumentRecord, LocalisationDefinition, ParamContract, ResourceReference,
		ScopeType, SemanticIndex, SymbolDefinition, SymbolKind,
	};
	use foch_language::analysis_version::analysis_rules_version;
	use std::path::PathBuf;
	use std::sync::Mutex;
	use tempfile::TempDir;

	static BASE_DATA_ENV_LOCK: Mutex<()> = Mutex::new(());

	fn sample_snapshot_with_contract() -> BaseAnalysisSnapshot {
		let mut index = SemanticIndex::default();
		index.definitions.push(SymbolDefinition {
			kind: SymbolKind::ScriptedEffect,
			name: "test.effect".to_string(),
			module: "test".to_string(),
			local_name: "add_age_modifier".to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/scripted_effects/test.txt"),
			line: 1,
			column: 1,
			scope_id: 0,
			declared_this_type: ScopeType::Country,
			inferred_this_type: ScopeType::Country,
			inferred_this_mask: 0b01,
			required_params: vec![
				"age".to_string(),
				"name".to_string(),
				"duration".to_string(),
			],
			param_contract: Some(ParamContract {
				required_all: vec![
					"age".to_string(),
					"name".to_string(),
					"duration".to_string(),
				],
				optional: vec!["else".to_string()],
				one_of_groups: Vec::new(),
				conditional_required: Vec::new(),
			}),
			scope_param_names: Vec::new(),
		});
		BaseAnalysisSnapshot::from_semantic_index(
			&Game::EuropaUniversalis4,
			"schema-test",
			vec!["common/scripted_effects/test.txt".to_string()],
			&index,
			Default::default(),
		)
	}

	fn sample_coverage_snapshot() -> BaseAnalysisSnapshot {
		let mod_id = "__game__eu4".to_string();
		let mut index = SemanticIndex {
			documents: vec![
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/country_tags/00_countries.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/countries/Sweden.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/units/swedish_tercio.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/religions/00_religion.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/subject_types/00_subject_types.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/rebel_types/independence_rebels.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/disasters/civil_war.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from(
						"common/government_mechanics/18_parliament_vs_monarchy.txt",
					),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/peace_treaties/00_peace_treaties.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/bookmarks/a_new_world.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/policies/00_adm.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/mercenary_companies/00_mercenaries.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/technologies/adm.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/technology.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/estate_agendas/00_generic_agendas.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/estate_privileges/01_church_privileges.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/estates/01_church.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/parliament_bribes/administrative_support.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/parliament_issues/00_adm_parliament_issues.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/state_edicts/edict_of_governance.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/church_aspects/00_church_aspects.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/factions/00_factions.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/hegemons/0_economic_hegemon.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/personal_deities/00_hindu_deities.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/fetishist_cults/00_fetishist_cults.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/scripted_effects/test.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("history/countries/SWE - Sweden.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("history/provinces/1 - Stockholm.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("common/province_names/sorbian.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("history/diplomacy/hre.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("history/advisors/00_england.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("history/wars/sample.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				},
				DocumentRecord {
					mod_id: mod_id.clone(),
					path: PathBuf::from("localisation/english/test_l_english.yml"),
					family: DocumentFamily::Localisation,
					parse_ok: true,
				},
			],
			..Default::default()
		};
		index.definitions.push(SymbolDefinition {
			kind: SymbolKind::ScriptedEffect,
			name: "test.effect".to_string(),
			module: "test".to_string(),
			local_name: "test_effect".to_string(),
			mod_id: mod_id.clone(),
			path: PathBuf::from("common/scripted_effects/test.txt"),
			line: 1,
			column: 1,
			scope_id: 0,
			declared_this_type: ScopeType::Country,
			inferred_this_type: ScopeType::Country,
			inferred_this_mask: 0b01,
			required_params: vec!["value".to_string()],
			param_contract: None,
			scope_param_names: Vec::new(),
		});
		index.localisation_definitions.push(LocalisationDefinition {
			key: "test_key".to_string(),
			mod_id,
			path: PathBuf::from("localisation/english/test_l_english.yml"),
			line: 1,
			column: 1,
		});
		index.resource_references.extend([
			ResourceReference {
				key: "country_tag:SWE".to_string(),
				value: "countries/Sweden.txt".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/country_tags/00_countries.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "graphical_culture".to_string(),
				value: "scandinaviangfx".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/countries/Sweden.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "historical_units".to_string(),
				value: "western_medieval_infantry".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/countries/Sweden.txt"),
				line: 3,
				column: 1,
			},
			ResourceReference {
				key: "capital".to_string(),
				value: "1".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/countries/SWE - Sweden.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "owner".to_string(),
				value: "SWE".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/provinces/1 - Stockholm.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "province_name_table".to_string(),
				value: "sorbian".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/province_names/sorbian.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "province_id".to_string(),
				value: "4778".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/province_names/sorbian.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "province_name_literal".to_string(),
				value: "Zhorjelc".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/province_names/sorbian.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "relation_type".to_string(),
				value: "alliance".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/diplomacy/hre.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "first".to_string(),
				value: "FRA".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/diplomacy/hre.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "second".to_string(),
				value: "SCO".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/diplomacy/hre.txt"),
				line: 3,
				column: 1,
			},
			ResourceReference {
				key: "emperor".to_string(),
				value: "BOH".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/diplomacy/hre.txt"),
				line: 4,
				column: 1,
			},
			ResourceReference {
				key: "advisor_definition".to_string(),
				value: "advisor_216".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/advisors/00_england.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "location".to_string(),
				value: "236".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/advisors/00_england.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "type".to_string(),
				value: "theologian".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/advisors/00_england.txt"),
				line: 3,
				column: 1,
			},
			ResourceReference {
				key: "unit_type".to_string(),
				value: "western".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/units/swedish_tercio.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "center_of_religion".to_string(),
				value: "118".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/religions/00_religion.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "copy_from".to_string(),
				value: "default".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/subject_types/00_subject_types.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "demands_description".to_string(),
				value: "independence_rebels_demands".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/rebel_types/independence_rebels.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "on_start".to_string(),
				value: "civil_war.1".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/disasters/civil_war.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "gui".to_string(),
				value: "parliament_vs_monarchy_gov_mech".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/government_mechanics/18_parliament_vs_monarchy.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "localisation_desc".to_string(),
				value: "spread_dynasty_desc".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/peace_treaties/00_peace_treaties.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "country".to_string(),
				value: "CAS".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/bookmarks/a_new_world.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "localisation".to_string(),
				value: "the_combination_act".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/policies/00_adm.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "monarch_power".to_string(),
				value: "ADM".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/policies/00_adm.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "localisation".to_string(),
				value: "merc_black_army".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/mercenary_companies/00_mercenaries.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "mercenary_desc_key".to_string(),
				value: "FREE_OF_ARMY_PROFESSIONALISM_COST".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/mercenary_companies/00_mercenaries.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "monarch_power".to_string(),
				value: "ADM".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/technologies/adm.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "technology_definition".to_string(),
				value: "adm_tech_0".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/technologies/adm.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "expects_institution".to_string(),
				value: "feudalism".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/technologies/adm.txt"),
				line: 3,
				column: 1,
			},
			ResourceReference {
				key: "enable".to_string(),
				value: "temple".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/technologies/adm.txt"),
				line: 4,
				column: 1,
			},
			ResourceReference {
				key: "technology_group".to_string(),
				value: "western".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/technology.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "nation_designer_unit_type".to_string(),
				value: "western".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/technology.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "nation_designer_cost_value".to_string(),
				value: "25".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/technology.txt"),
				line: 3,
				column: 1,
			},
			ResourceReference {
				key: "estate".to_string(),
				value: "clergy".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/estate_agendas/00_generic_agendas.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "custom_tooltip".to_string(),
				value: "agenda_done_tt".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/estate_agendas/00_generic_agendas.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "icon".to_string(),
				value: "privilege_religious_diplomats".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/estate_privileges/01_church_privileges.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "mechanics".to_string(),
				value: "papal_influence".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/estate_privileges/01_church_privileges.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "custom_name".to_string(),
				value: "estate_clergy_custom_name".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/estates/01_church.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "privileges".to_string(),
				value: "religious_diplomats".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/estates/01_church.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "mechanic_type".to_string(),
				value: "parliament_vs_monarchy_mechanic".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/parliament_bribes/administrative_support.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "parliament_action".to_string(),
				value: "strengthen_government".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/parliament_issues/00_adm_parliament_issues.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "tooltip".to_string(),
				value: "edict_of_governance_tt".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/state_edicts/edict_of_governance.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "has_state_edict".to_string(),
				value: "encourage_development_edict".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/state_edicts/edict_of_governance.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "localisation".to_string(),
				value: "organised_through_bishops_aspect".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/church_aspects/00_church_aspects.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "localisation_desc".to_string(),
				value: "desc_organised_through_bishops_aspect".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/church_aspects/00_church_aspects.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "localisation_modifier".to_string(),
				value: "organised_through_bishops_aspect_modifier".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/church_aspects/00_church_aspects.txt"),
				line: 3,
				column: 1,
			},
			ResourceReference {
				key: "localisation".to_string(),
				value: "rr_jacobins".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/factions/00_factions.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "localisation_influence".to_string(),
				value: "rr_jacobins_influence".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/factions/00_factions.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "monarch_power".to_string(),
				value: "ADM".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/factions/00_factions.txt"),
				line: 3,
				column: 1,
			},
			ResourceReference {
				key: "localisation".to_string(),
				value: "economic_hegemon".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/hegemons/0_economic_hegemon.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "localisation".to_string(),
				value: "shiva".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/personal_deities/00_hindu_deities.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "localisation_desc".to_string(),
				value: "shiva_desc".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/personal_deities/00_hindu_deities.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "localisation".to_string(),
				value: "yemoja_cult".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/fetishist_cults/00_fetishist_cults.txt"),
				line: 1,
				column: 1,
			},
			ResourceReference {
				key: "localisation_desc".to_string(),
				value: "yemoja_cult_desc".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("common/fetishist_cults/00_fetishist_cults.txt"),
				line: 2,
				column: 1,
			},
			ResourceReference {
				key: "add_attacker".to_string(),
				value: "SWE".to_string(),
				mod_id: "__game__eu4".to_string(),
				path: PathBuf::from("history/wars/sample.txt"),
				line: 1,
				column: 1,
			},
		]);
		BaseAnalysisSnapshot::from_semantic_index(
			&Game::EuropaUniversalis4,
			"coverage-test",
			vec![
				"common/country_tags/00_countries.txt".to_string(),
				"common/countries/Sweden.txt".to_string(),
				"common/units/swedish_tercio.txt".to_string(),
				"common/religions/00_religion.txt".to_string(),
				"common/subject_types/00_subject_types.txt".to_string(),
				"common/rebel_types/independence_rebels.txt".to_string(),
				"common/disasters/civil_war.txt".to_string(),
				"common/government_mechanics/18_parliament_vs_monarchy.txt".to_string(),
				"common/peace_treaties/00_peace_treaties.txt".to_string(),
				"common/bookmarks/a_new_world.txt".to_string(),
				"common/policies/00_adm.txt".to_string(),
				"common/mercenary_companies/00_mercenaries.txt".to_string(),
				"common/technologies/adm.txt".to_string(),
				"common/technology.txt".to_string(),
				"common/estate_agendas/00_generic_agendas.txt".to_string(),
				"common/estate_privileges/01_church_privileges.txt".to_string(),
				"common/estates/01_church.txt".to_string(),
				"common/parliament_bribes/administrative_support.txt".to_string(),
				"common/parliament_issues/00_adm_parliament_issues.txt".to_string(),
				"common/state_edicts/edict_of_governance.txt".to_string(),
				"common/church_aspects/00_church_aspects.txt".to_string(),
				"common/factions/00_factions.txt".to_string(),
				"common/hegemons/0_economic_hegemon.txt".to_string(),
				"common/personal_deities/00_hindu_deities.txt".to_string(),
				"common/fetishist_cults/00_fetishist_cults.txt".to_string(),
				"common/scripted_effects/test.txt".to_string(),
				"history/countries/SWE - Sweden.txt".to_string(),
				"history/provinces/1 - Stockholm.txt".to_string(),
				"common/province_names/sorbian.txt".to_string(),
				"history/diplomacy/hre.txt".to_string(),
				"history/advisors/00_england.txt".to_string(),
				"history/wars/sample.txt".to_string(),
				"localisation/english/test_l_english.yml".to_string(),
				"patchnotes/1_36.txt".to_string(),
				"builtin_dlc/builtin_dlc.txt".to_string(),
				"checksum_manifest.txt".to_string(),
			],
			&index,
			Default::default(),
		)
	}

	#[test]
	fn base_snapshot_round_trip_preserves_param_contracts() {
		let snapshot = sample_snapshot_with_contract();
		let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
		let decoded = decode_snapshot_from_bytes(&encoded.bytes).expect("decode snapshot");
		let contract = decoded
			.symbol_definitions
			.first()
			.and_then(|definition| definition.param_contract.as_ref())
			.expect("serialized param contract");
		assert_eq!(contract.required_all, vec!["age", "name", "duration"]);
		assert_eq!(contract.optional, vec!["else"]);
		assert_eq!(
			decoded
				.symbol_definitions
				.first()
				.map(|definition| definition.inferred_this_mask),
			Some(0b01)
		);

		let rehydrated = decoded.to_semantic_index();
		let contract = rehydrated
			.definitions
			.first()
			.and_then(|definition| definition.param_contract.as_ref())
			.expect("rehydrated param contract");
		assert_eq!(contract.required_all, vec!["age", "name", "duration"]);
		assert_eq!(contract.optional, vec!["else"]);
		assert_eq!(
			rehydrated
				.definitions
				.first()
				.map(|definition| definition.inferred_this_mask),
			Some(0b01)
		);
	}

	#[test]
	fn build_coverage_report_classifies_foundation_and_excluded_roots() {
		let snapshot = sample_coverage_snapshot();
		let report = build_coverage_report(&snapshot);
		let scripted_effects = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/scripted_effects")
			.expect("scripted effects coverage");
		assert_eq!(scripted_effects.coverage_class, CoverageClass::MergeReady);

		let provinces = report
			.roots
			.iter()
			.find(|item| item.root_family == "history/provinces")
			.expect("province history coverage");
		assert_eq!(provinces.coverage_class, CoverageClass::SemanticComplete);

		let province_names = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/province_names")
			.expect("province names coverage");
		assert_eq!(
			province_names.coverage_class,
			CoverageClass::SemanticComplete
		);

		let diplomacy = report
			.roots
			.iter()
			.find(|item| item.root_family == "history/diplomacy")
			.expect("diplomacy history coverage");
		assert_eq!(diplomacy.coverage_class, CoverageClass::SemanticComplete);

		let advisors = report
			.roots
			.iter()
			.find(|item| item.root_family == "history/advisors")
			.expect("advisor history coverage");
		assert_eq!(advisors.coverage_class, CoverageClass::SemanticComplete);

		let country_tags = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/country_tags")
			.expect("country tags coverage");
		assert_eq!(country_tags.coverage_class, CoverageClass::SemanticComplete);

		let countries = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/countries")
			.expect("countries coverage");
		assert_eq!(countries.coverage_class, CoverageClass::SemanticComplete);

		let units = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/units")
			.expect("units coverage");
		assert_eq!(units.coverage_class, CoverageClass::SemanticComplete);

		let religions = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/religions")
			.expect("religions coverage");
		assert_eq!(religions.coverage_class, CoverageClass::SemanticComplete);

		let subject_types = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/subject_types")
			.expect("subject types coverage");
		assert_eq!(
			subject_types.coverage_class,
			CoverageClass::SemanticComplete
		);

		let rebel_types = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/rebel_types")
			.expect("rebel types coverage");
		assert_eq!(rebel_types.coverage_class, CoverageClass::SemanticComplete);

		let disasters = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/disasters")
			.expect("disasters coverage");
		assert_eq!(disasters.coverage_class, CoverageClass::SemanticComplete);

		let government_mechanics = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/government_mechanics")
			.expect("government mechanics coverage");
		assert_eq!(
			government_mechanics.coverage_class,
			CoverageClass::SemanticComplete
		);

		let peace_treaties = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/peace_treaties")
			.expect("peace treaties coverage");
		assert_eq!(
			peace_treaties.coverage_class,
			CoverageClass::SemanticComplete
		);

		let bookmarks = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/bookmarks")
			.expect("bookmarks coverage");
		assert_eq!(bookmarks.coverage_class, CoverageClass::SemanticComplete);

		let policies = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/policies")
			.expect("policies coverage");
		assert_eq!(policies.coverage_class, CoverageClass::SemanticComplete);

		let mercenary_companies = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/mercenary_companies")
			.expect("mercenary companies coverage");
		assert_eq!(
			mercenary_companies.coverage_class,
			CoverageClass::SemanticComplete
		);

		let technologies = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/technologies")
			.expect("technologies coverage");
		assert_eq!(technologies.coverage_class, CoverageClass::SemanticComplete);

		let technology_groups = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/technology")
			.expect("technology groups coverage");
		assert_eq!(
			technology_groups.coverage_class,
			CoverageClass::SemanticComplete
		);

		let estate_agendas = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/estate_agendas")
			.expect("estate agendas coverage");
		assert_eq!(
			estate_agendas.coverage_class,
			CoverageClass::SemanticComplete
		);

		let estate_privileges = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/estate_privileges")
			.expect("estate privileges coverage");
		assert_eq!(
			estate_privileges.coverage_class,
			CoverageClass::SemanticComplete
		);

		let estates = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/estates")
			.expect("estates coverage");
		assert_eq!(estates.coverage_class, CoverageClass::SemanticComplete);

		let parliament_bribes = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/parliament_bribes")
			.expect("parliament bribes coverage");
		assert_eq!(
			parliament_bribes.coverage_class,
			CoverageClass::SemanticComplete
		);

		let parliament_issues = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/parliament_issues")
			.expect("parliament issues coverage");
		assert_eq!(
			parliament_issues.coverage_class,
			CoverageClass::SemanticComplete
		);

		let state_edicts = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/state_edicts")
			.expect("state edicts coverage");
		assert_eq!(state_edicts.coverage_class, CoverageClass::SemanticComplete);

		let church_aspects = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/church_aspects")
			.expect("church aspects coverage");
		assert_eq!(
			church_aspects.coverage_class,
			CoverageClass::SemanticComplete
		);

		let factions = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/factions")
			.expect("factions coverage");
		assert_eq!(factions.coverage_class, CoverageClass::SemanticComplete);

		let hegemons = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/hegemons")
			.expect("hegemons coverage");
		assert_eq!(hegemons.coverage_class, CoverageClass::SemanticComplete);

		let personal_deities = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/personal_deities")
			.expect("personal deities coverage");
		assert_eq!(
			personal_deities.coverage_class,
			CoverageClass::SemanticComplete
		);

		let fetishist_cults = report
			.roots
			.iter()
			.find(|item| item.root_family == "common/fetishist_cults")
			.expect("fetishist cults coverage");
		assert_eq!(
			fetishist_cults.coverage_class,
			CoverageClass::SemanticComplete
		);

		let localisation = report
			.roots
			.iter()
			.find(|item| item.root_family == "localisation")
			.expect("localisation coverage");
		assert_eq!(localisation.coverage_class, CoverageClass::SemanticComplete);

		let patchnotes = report
			.roots
			.iter()
			.find(|item| item.root_family == "patchnotes")
			.expect("patchnotes coverage");
		assert_eq!(
			patchnotes.coverage_class,
			CoverageClass::ExcludedNonGameplay
		);

		let builtin_dlc = report
			.roots
			.iter()
			.find(|item| item.root_family == "builtin_dlc")
			.expect("builtin dlc coverage");
		assert_eq!(
			builtin_dlc.coverage_class,
			CoverageClass::ExcludedNonGameplay
		);

		let checksum_manifest = report
			.roots
			.iter()
			.find(|item| item.root_family == "checksum_manifest.txt")
			.expect("checksum manifest coverage");
		assert_eq!(
			checksum_manifest.coverage_class,
			CoverageClass::ExcludedNonGameplay
		);
	}

	#[test]
	fn write_snapshot_bundle_emits_coverage_report() {
		let temp = TempDir::new().expect("temp dir");
		let snapshot = sample_coverage_snapshot();
		let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
		let bundle = write_snapshot_bundle(
			&snapshot,
			&encoded.bytes,
			temp.path(),
			BaseDataSource::Build,
			None,
			None,
		)
		.expect("write bundle");
		assert!(bundle.coverage_path.is_file());
		let coverage_path = temp.path().join(INSTALLED_COVERAGE_FILE_NAME);
		assert_eq!(bundle.coverage_path, coverage_path);
	}

	#[test]
	fn load_installed_base_snapshot_rejects_old_schema_version() {
		let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
		let temp = TempDir::new().expect("temp dir");
		unsafe {
			std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
		}

		let snapshot = sample_snapshot_with_contract();
		let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
		let metadata = InstalledBaseDataMetadata {
			schema_version: BASE_DATA_SCHEMA_VERSION,
			game: snapshot.game.clone(),
			game_version: snapshot.game_version.clone(),
			analysis_rules_version: analysis_rules_version().to_string(),
			generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
			source: BaseDataSource::Build,
			asset_name: None,
			sha256: None,
		};
		let installed = write_installed_snapshot(&snapshot, &metadata, &encoded.bytes)
			.expect("install snapshot");
		assert!(
			installed
				.install_dir
				.join(INSTALLED_COVERAGE_FILE_NAME)
				.is_file()
		);
		let metadata_path = installed
			.install_dir
			.join(super::INSTALLED_METADATA_FILE_NAME);
		let old_metadata = InstalledBaseDataMetadata {
			schema_version: BASE_DATA_SCHEMA_VERSION - 1,
			..metadata
		};
		std::fs::write(
			&metadata_path,
			serde_json::to_string_pretty(&old_metadata).expect("serialize metadata"),
		)
		.expect("write metadata");

		let err = load_installed_base_snapshot("eu4", "schema-test")
			.expect_err("old schema should be rejected");
		assert!(err.contains("基础数据 schema 不匹配"));

		unsafe {
			std::env::remove_var(BASE_DATA_DIR_ENV);
		}
	}

	#[test]
	fn load_installed_base_snapshot_rejects_stale_metadata_before_decoding_snapshot() {
		let _guard = BASE_DATA_ENV_LOCK.lock().expect("env lock");
		let temp = TempDir::new().expect("temp dir");
		unsafe {
			std::env::set_var(BASE_DATA_DIR_ENV, temp.path());
		}

		let snapshot = sample_snapshot_with_contract();
		let encoded = encode_snapshot_to_bytes(&snapshot).expect("encode snapshot");
		let metadata = InstalledBaseDataMetadata {
			schema_version: BASE_DATA_SCHEMA_VERSION,
			game: snapshot.game.clone(),
			game_version: snapshot.game_version.clone(),
			analysis_rules_version: analysis_rules_version().to_string(),
			generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
			source: BaseDataSource::Build,
			asset_name: None,
			sha256: None,
		};
		let installed = write_installed_snapshot(&snapshot, &metadata, &encoded.bytes)
			.expect("install snapshot");
		assert!(
			installed
				.install_dir
				.join(INSTALLED_COVERAGE_FILE_NAME)
				.is_file()
		);
		let metadata_path = installed
			.install_dir
			.join(super::INSTALLED_METADATA_FILE_NAME);
		let old_metadata = InstalledBaseDataMetadata {
			schema_version: BASE_DATA_SCHEMA_VERSION - 1,
			..metadata
		};
		std::fs::write(
			&metadata_path,
			serde_json::to_string_pretty(&old_metadata).expect("serialize metadata"),
		)
		.expect("write metadata");
		let snapshot_path = installed.install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
		std::fs::write(&snapshot_path, b"definitely-not-a-valid-snapshot")
			.expect("write corrupt snapshot");

		let err = load_installed_base_snapshot("eu4", "schema-test")
			.expect_err("stale metadata should short-circuit before decode");
		assert!(err.contains("基础数据 schema 不匹配"));
		assert!(!err.contains("无法解析基础数据 snapshot"));

		unsafe {
			std::env::remove_var(BASE_DATA_DIR_ENV);
		}
	}

	#[test]
	fn base_symbol_definition_defaults_missing_param_contract() {
		let raw = serde_json::json!({
			"kind": "ScriptedEffect",
			"name": "test.effect",
			"module": "test",
			"local_name": "test_effect",
			"path": "common/scripted_effects/test.txt",
			"line": 1,
			"column": 1,
			"scope_id": 0,
			"declared_this_type": "Country",
			"inferred_this_type": "Country",
			"inferred_this_mask": 1,
			"required_params": []
		});
		let decoded: BaseSymbolDefinition =
			serde_json::from_value(raw).expect("deserialize legacy base symbol definition");
		assert!(decoded.param_contract.is_none());
		assert_eq!(decoded.inferred_this_mask, 1);
	}
}
