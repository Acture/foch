mod coverage;

pub use coverage::{
	BaseCoverageReport, CoverageClass, RootCoverageEntry, build_coverage_report,
	coverage_class_name, document_family_name, script_file_kind_name, write_coverage_report,
};

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
use foch_language::analyzer::documents::{
	DiscoveredTextDocument, build_semantic_index_from_documents, discover_text_documents,
	parse_discovered_text_documents,
};
use foch_language::analyzer::param_contracts::apply_registered_param_contracts;
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

#[cfg(test)]
mod tests;
