use super::document::DocumentFamily;
use super::semantic::SemanticIndex;
use super::workspace::ModCandidate;
use crate::domain::playlist::Playlist;
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ParseFamilyStats {
	pub clausewitz_mainline: FamilyParseStats,
	pub localisation: FamilyParseStats,
	pub csv: FamilyParseStats,
	pub json: FamilyParseStats,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
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
	#[serde(skip_serializing, skip_deserializing)]
	pub parse_issue_report: Vec<ParseIssueReportItem>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseIssueReportItem {
	pub family: DocumentFamily,
	pub mod_id: String,
	pub path: PathBuf,
	pub line: usize,
	pub column: usize,
	pub message: String,
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
