use crate::check::documents::{build_semantic_index_from_documents, parse_text_documents};
use crate::check::model::{
	AliasUsage, CsvRow, DocumentFamily, DocumentRecord, JsonProperty, KeyUsage,
	LocalisationDefinition, LocalisationDuplicate, ParseIssue, ResourceReference, ScalarAssignment,
	ScopeKind, ScopeNode, ScopeType, SemanticIndex, SourceSpan, SymbolDefinition, SymbolKind,
	SymbolReference, UiDefinition,
};
use crate::cli::config::Config;
use crate::domain::game::Game;
use crate::utils::steam::steam_game_install_path;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const BASE_GAME_MOD_ID_PREFIX: &str = "__game__";
pub const BASE_DATA_DIR_ENV: &str = "FOCH_DATA_DIR";
pub const BASE_DATA_RELEASE_BASE_URL_ENV: &str = "FOCH_DATA_RELEASE_BASE_URL";
pub const BASE_DATA_SCHEMA_VERSION: u32 = 1;
pub const RELEASE_MANIFEST_FILE_NAME: &str = "foch-data-manifest.json";
pub const INSTALLED_SNAPSHOT_FILE_NAME: &str = "snapshot.bin.gz";
pub const INSTALLED_METADATA_FILE_NAME: &str = "metadata.json";

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
	pub snapshot_asset_name: String,
	pub snapshot_sha256: String,
}

#[derive(Clone, Debug)]
pub struct ReleaseArtifactOutput {
	pub snapshot_path: PathBuf,
	pub manifest_path: PathBuf,
	pub asset_name: String,
	pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct SnapshotBundleOutput {
	pub snapshot_path: PathBuf,
	pub metadata_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseAnalysisSnapshot {
	pub schema_version: u32,
	pub game: String,
	pub game_version: String,
	pub generated_by_cli_version: String,
	pub inventory_paths: Vec<String>,
	pub documents: Vec<BaseDocumentRecord>,
	pub parse_error_count: usize,
	pub parsed_files: usize,
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

impl BaseAnalysisSnapshot {
	pub fn from_semantic_index(
		game: &Game,
		game_version: &str,
		inventory_paths: Vec<String>,
		index: &SemanticIndex,
	) -> Self {
		Self {
			schema_version: BASE_DATA_SCHEMA_VERSION,
			game: game.key().to_string(),
			game_version: game_version.to_string(),
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
			parse_error_count: index.parse_issues.len(),
			parsed_files: index.documents.len(),
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
					required_params: item.required_params.clone(),
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
		SemanticIndex {
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
					required_params: item.required_params.clone(),
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
		}
	}

	pub fn document_lookup(&self) -> HashMap<&str, (&DocumentFamily, bool)> {
		self.documents
			.iter()
			.map(|item| (item.path.as_str(), (&item.family, item.parse_ok)))
			.collect()
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
	pub required_params: Vec<String>,
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
	pub param_bindings: Vec<crate::check::model::ParamBinding>,
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
	let snapshot_path = install_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	if !metadata_path.is_file() || !snapshot_path.is_file() {
		return Ok(None);
	}

	let metadata_raw = fs::read_to_string(&metadata_path)
		.map_err(|err| format!("无法读取基础数据元数据 {}: {err}", metadata_path.display()))?;
	let metadata: InstalledBaseDataMetadata = serde_json::from_str(&metadata_raw)
		.map_err(|err| format!("无法解析基础数据元数据 {}: {err}", metadata_path.display()))?;
	if metadata.schema_version != BASE_DATA_SCHEMA_VERSION {
		return Err(format!(
			"基础数据 schema 不匹配: expected {}, found {}",
			BASE_DATA_SCHEMA_VERSION, metadata.schema_version
		));
	}

	let snapshot = load_snapshot_from_file(&snapshot_path)?;
	if snapshot.schema_version != BASE_DATA_SCHEMA_VERSION {
		return Err(format!(
			"基础数据 snapshot schema 不匹配: expected {}, found {}",
			BASE_DATA_SCHEMA_VERSION, snapshot.schema_version
		));
	}
	if snapshot.game != game_key || snapshot.game_version != game_version {
		return Err(format!(
			"基础数据内容与请求不匹配: requested {game_key}@{game_version}, found {}@{}",
			snapshot.game, snapshot.game_version
		));
	}

	Ok(Some(InstalledBaseSnapshot {
		install_dir,
		metadata,
		snapshot,
	}))
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
	let resolved_version = match game_version {
		Some(version) => version.to_string(),
		None => detect_game_version(game_root).ok_or_else(|| {
			format!(
				"无法检测 {} 版本；请提供 --game-version 或确认 {} 下存在版本文件",
				game.key(),
				game_root.display()
			)
		})?,
	};
	let inventory_paths = collect_relative_files(game_root)
		.into_iter()
		.map(|path| normalize_path(&path))
		.collect();
	let documents = parse_text_documents(&base_game_mod_id(game.key()), game_root);
	let index = build_semantic_index_from_documents(&documents);
	let snapshot =
		BaseAnalysisSnapshot::from_semantic_index(game, &resolved_version, inventory_paths, &index);
	let encoded = encode_snapshot_to_bytes(&snapshot)?;
	let sha256 = sha256_hex(&encoded);
	let asset_name = snapshot_asset_name(game.key(), &resolved_version);
	Ok(BaseSnapshotBuildResult {
		snapshot,
		snapshot_asset_name: asset_name,
		snapshot_sha256: sha256,
	})
}

pub fn install_built_snapshot(
	snapshot: &BaseAnalysisSnapshot,
	source: BaseDataSource,
	asset_name: Option<String>,
	sha256: Option<String>,
) -> Result<InstalledBaseSnapshot, String> {
	let encoded = encode_snapshot_to_bytes(snapshot)?;
	let metadata = InstalledBaseDataMetadata {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
		source,
		asset_name,
		sha256,
	};
	write_installed_snapshot(snapshot, &metadata, &encoded)
}

pub fn write_release_artifacts(
	snapshot: &BaseAnalysisSnapshot,
	output_dir: &Path,
	release_tag: &str,
) -> Result<ReleaseArtifactOutput, String> {
	fs::create_dir_all(output_dir)
		.map_err(|err| format!("无法创建输出目录 {}: {err}", output_dir.display()))?;
	let encoded = encode_snapshot_to_bytes(snapshot)?;
	let sha256 = sha256_hex(&encoded);
	let asset_name = snapshot_asset_name(&snapshot.game, &snapshot.game_version);
	let snapshot_path = output_dir.join(&asset_name);
	fs::write(&snapshot_path, &encoded).map_err(|err| {
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

	Ok(ReleaseArtifactOutput {
		snapshot_path,
		manifest_path,
		asset_name,
		sha256,
	})
}

pub fn write_snapshot_bundle(
	snapshot: &BaseAnalysisSnapshot,
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
		generated_by_cli_version: snapshot.generated_by_cli_version.clone(),
		source,
		asset_name,
		sha256,
	};
	let metadata_raw = serde_json::to_string_pretty(&metadata)
		.map_err(|err| format!("无法序列化基础数据元数据: {err}"))?;
	let snapshot_bytes = encode_snapshot_to_bytes(snapshot)?;
	let snapshot_path = output_dir.join(INSTALLED_SNAPSHOT_FILE_NAME);
	let metadata_path = output_dir.join(INSTALLED_METADATA_FILE_NAME);
	fs::write(&snapshot_path, snapshot_bytes).map_err(|err| {
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
	Ok(SnapshotBundleOutput {
		snapshot_path,
		metadata_path,
	})
}

pub fn install_snapshot_from_release(
	game: &Game,
	game_version: &str,
	release_tag: Option<&str>,
) -> Result<InstalledBaseSnapshot, String> {
	let release_tag = release_tag.map_or_else(default_release_tag, ToString::to_string);
	let base_url = release_base_url(&release_tag);
	let client = Client::builder()
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

	let metadata = InstalledBaseDataMetadata {
		schema_version: snapshot.schema_version,
		game: snapshot.game.clone(),
		game_version: snapshot.game_version.clone(),
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
	Ok(InstalledBaseSnapshot {
		install_dir,
		metadata: metadata.clone(),
		snapshot: snapshot.clone(),
	})
}

fn load_snapshot_from_file(path: &Path) -> Result<BaseAnalysisSnapshot, String> {
	let file = fs::File::open(path)
		.map_err(|err| format!("无法打开基础数据 snapshot {}: {err}", path.display()))?;
	let reader = BufReader::new(file);
	let decoder = GzDecoder::new(reader);
	let snapshot = bincode::deserialize_from(decoder)
		.map_err(|err| format!("无法解析基础数据 snapshot {}: {err}", path.display()))?;
	Ok(snapshot)
}

fn encode_snapshot_to_bytes(snapshot: &BaseAnalysisSnapshot) -> Result<Vec<u8>, String> {
	let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
	bincode::serialize_into(&mut encoder, snapshot)
		.map_err(|err| format!("无法序列化基础数据 snapshot: {err}"))?;
	encoder
		.finish()
		.map_err(|err| format!("无法完成基础数据 snapshot 压缩: {err}"))
}

fn decode_snapshot_from_bytes(bytes: &[u8]) -> Result<BaseAnalysisSnapshot, String> {
	let cursor = Cursor::new(bytes);
	let decoder = GzDecoder::new(cursor);
	bincode::deserialize_from(decoder).map_err(|err| format!("无法解析基础数据 snapshot: {err}"))
}

fn release_base_url(release_tag: &str) -> String {
	if let Ok(url) = std::env::var(BASE_DATA_RELEASE_BASE_URL_ENV) {
		return url.trim_end_matches('/').to_string();
	}
	format!("https://github.com/Acture/foch/releases/download/{release_tag}")
}

fn snapshot_asset_name(game_key: &str, game_version: &str) -> String {
	format!(
		"foch-{game_key}-base-snapshot-{}.bin.gz",
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
