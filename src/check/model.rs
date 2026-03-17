use crate::cli::config::Config;
use crate::domain::descriptor::ModDescriptor;
use crate::domain::playlist::{Playlist, PlaylistEntry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Severity {
	Error,
	Warning,
	Info,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FindingChannel {
	Strict,
	Advisory,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AnalysisMode {
	Basic,
	#[default]
	Semantic,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChannelMode {
	Strict,
	#[default]
	All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphFormat {
	Json,
	Dot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFamily {
	Clausewitz,
	Localisation,
	Csv,
	Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergePlanFormat {
	Text,
	Json,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergePlanStrategy {
	#[default]
	CopyThrough,
	LastWriterOverlay,
	StructuralMerge,
	ManualConflict,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
	pub rule_id: String,
	pub severity: Severity,
	pub channel: FindingChannel,
	pub message: String,
	pub mod_id: Option<String>,
	pub path: Option<PathBuf>,
	pub evidence: Option<String>,
	pub line: Option<usize>,
	pub column: Option<usize>,
	pub confidence: Option<f32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalysisMeta {
	pub text_documents: usize,
	pub parsed_files: usize,
	pub parse_errors: usize,
	pub parse_stats: ParseFamilyStats,
	pub scopes: usize,
	pub symbol_definitions: usize,
	pub symbol_references: usize,
	pub alias_usages: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ParseFamilyStats {
	pub clausewitz_mainline: FamilyParseStats,
	pub localisation: FamilyParseStats,
	pub csv: FamilyParseStats,
	pub json: FamilyParseStats,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FamilyParseStats {
	pub documents: usize,
	pub parse_failed_documents: usize,
	pub parse_issue_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CheckResult {
	pub findings: Vec<Finding>,
	pub strict_findings: Vec<Finding>,
	pub advisory_findings: Vec<Finding>,
	pub fatal_errors: Vec<String>,
	pub analysis_meta: AnalysisMeta,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub graph_output: Option<String>,
}

impl CheckResult {
	pub fn has_findings(&self) -> bool {
		!self.findings.is_empty()
	}

	pub fn has_strict_findings(&self) -> bool {
		!self.strict_findings.is_empty()
	}

	pub fn has_fatal_errors(&self) -> bool {
		!self.fatal_errors.is_empty()
	}

	pub fn push_fatal_error(&mut self, message: impl Into<String>) {
		self.fatal_errors.push(message.into());
	}

	pub fn recompute_channels(&mut self) {
		self.strict_findings = self
			.findings
			.iter()
			.filter(|item| item.channel == FindingChannel::Strict)
			.cloned()
			.collect();
		self.advisory_findings = self
			.findings
			.iter()
			.filter(|item| item.channel == FindingChannel::Advisory)
			.cloned()
			.collect();
	}

	pub fn filtered_findings(&self, mode: ChannelMode) -> Vec<Finding> {
		match mode {
			ChannelMode::Strict => self.strict_findings.clone(),
			ChannelMode::All => self.findings.clone(),
		}
	}
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanContributor {
	pub mod_id: String,
	pub source_path: String,
	pub precedence: usize,
	pub is_base_game: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanEntry {
	pub path: String,
	pub strategy: MergePlanStrategy,
	pub contributors: Vec<MergePlanContributor>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub winner: Option<MergePlanContributor>,
	#[serde(default)]
	pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanSummary {
	pub total_paths: usize,
	pub copy_through: usize,
	pub last_writer_overlay: usize,
	pub structural_merge: usize,
	pub manual_conflict: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanResult {
	pub game: String,
	pub playset_name: String,
	pub include_game_base: bool,
	pub entries: Vec<MergePlanEntry>,
	pub summary: MergePlanSummary,
	pub fatal_errors: Vec<String>,
}

impl MergePlanResult {
	pub fn has_fatal_errors(&self) -> bool {
		!self.fatal_errors.is_empty()
	}

	pub fn has_manual_conflicts(&self) -> bool {
		self.summary.manual_conflict > 0
	}

	pub fn push_fatal_error(&mut self, message: impl Into<String>) {
		self.fatal_errors.push(message.into());
	}
}

#[derive(Clone, Debug)]
pub struct CheckRequest {
	pub playset_path: PathBuf,
	pub config: Config,
}

#[derive(Clone, Debug)]
pub struct RunOptions {
	pub analysis_mode: AnalysisMode,
	pub channel_mode: ChannelMode,
	pub graph_format: Option<GraphFormat>,
	pub include_game_base: bool,
}

impl Default for RunOptions {
	fn default() -> Self {
		Self {
			analysis_mode: AnalysisMode::default(),
			channel_mode: ChannelMode::default(),
			graph_format: None,
			include_game_base: true,
		}
	}
}

#[derive(Clone, Debug)]
pub struct MergePlanOptions {
	pub include_game_base: bool,
}

impl Default for MergePlanOptions {
	fn default() -> Self {
		Self {
			include_game_base: true,
		}
	}
}

#[derive(Clone, Debug)]
pub struct ModCandidate {
	pub entry: PlaylistEntry,
	pub mod_id: String,
	pub root_path: Option<PathBuf>,
	pub descriptor_path: Option<PathBuf>,
	pub descriptor: Option<ModDescriptor>,
	pub descriptor_error: Option<String>,
	pub files: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
	ScriptedEffect,
	Event,
	Decision,
	DiplomaticAction,
	TriggeredModifier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ScopeType {
	Country,
	Province,
	Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
	pub required_params: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParamBinding {
	pub name: String,
	pub value: String,
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
pub struct DocumentRecord {
	pub mod_id: String,
	pub path: PathBuf,
	pub family: DocumentFamily,
	pub parse_ok: bool,
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

#[derive(Clone, Debug)]
pub struct CheckContext {
	pub playlist_path: PathBuf,
	pub playlist: Playlist,
	pub mods: Vec<ModCandidate>,
	pub semantic_index: SemanticIndex,
}

#[derive(Clone, Debug, Default)]
pub struct SemanticDiagnostics {
	pub strict: Vec<Finding>,
	pub advisory: Vec<Finding>,
}
