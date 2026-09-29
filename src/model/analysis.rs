use super::GamePathBuf;
use super::candidate::ModCandidate;
use super::document::DocumentFamily;
use super::semantic::{SemanticIndex, SymbolKind};
use crate::playset::Playset;
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

pub const STALE_VANILLA_FALLBACK_RULE_ID: &str = "stale-vanilla-fallback";

/// One check result. A finding about a script names its file by game path in
/// `path`, relative to the root of the mod in `mod_id`; a finding about an
/// input file itself (the playset, a descriptor, a mod root) names that file
/// in `source_file` instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
	pub rule_id: String,
	pub severity: Severity,
	pub channel: FindingChannel,
	pub message: String,
	pub mod_id: Option<String>,
	pub path: Option<GamePathBuf>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub source_file: Option<PathBuf>,
	pub evidence: Option<String>,
	pub line: Option<usize>,
	pub column: Option<usize>,
	pub confidence: Option<f32>,
}

impl Finding {
	pub fn stale_vanilla_fallback(
		mod_id: String,
		file_path: GamePathBuf,
		reference_kind: SymbolKind,
		reference_name: String,
		line: usize,
		column: usize,
		rationale: impl Into<String>,
	) -> Self {
		let rationale = rationale.into();
		Self {
			rule_id: STALE_VANILLA_FALLBACK_RULE_ID.to_string(),
			severity: Severity::Error,
			channel: FindingChannel::Strict,
			message: format!(
				"stale vanilla fallback: {} {}",
				reference_kind.as_str(),
				reference_name
			),
			mod_id: Some(mod_id),
			path: Some(file_path),
			source_file: None,
			evidence: Some(format!(
				"reference_kind={} reference_name={} rationale={}",
				reference_kind.as_str(),
				reference_name,
				rationale
			)),
			line: Some(line),
			column: Some(column),
			confidence: Some(0.95),
		}
	}
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

#[derive(
	Clone, Debug, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct ParseFamilyStats {
	pub clausewitz_mainline: FamilyParseStats,
	pub localisation: FamilyParseStats,
	pub csv: FamilyParseStats,
	pub json: FamilyParseStats,
}

#[derive(
	Clone, Debug, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
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
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
	pub message: String,
}

#[derive(Clone, Debug)]
pub struct CheckContext {
	pub playlist_path: PathBuf,
	pub playlist: Playset,
	pub mods: Vec<ModCandidate>,
	pub semantic_index: SemanticIndex,
}

#[derive(Clone, Debug, Default)]
pub struct SemanticDiagnostics {
	pub strict: Vec<Finding>,
	pub advisory: Vec<Finding>,
}

#[cfg(test)]
mod tests {
	use super::{Finding, FindingChannel, Severity};
	use crate::model::GamePathBuf;
	use std::path::PathBuf;

	fn finding(path: Option<GamePathBuf>, source_file: Option<PathBuf>) -> Finding {
		Finding {
			rule_id: "rule".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: "message".to_string(),
			mod_id: Some("mod".to_string()),
			path,
			source_file,
			evidence: None,
			line: Some(3),
			column: None,
			confidence: None,
		}
	}

	#[test]
	fn a_script_finding_serializes_its_game_path_under_the_path_key_as_before() {
		let game_path = GamePathBuf::parse("events/Flavor FRA.txt").expect("valid game path");
		let json = serde_json::to_value(finding(Some(game_path.clone()), None)).expect("serialize");
		assert_eq!(json["path"], "events/Flavor FRA.txt");
		assert!(json.get("source_file").is_none(), "{json}");

		// The exact bytes a finding with this path serialized to before the
		// split: same keys, same order, no `source_file`.
		let text = r#"{"rule_id":"rule","severity":"Warning","channel":"Advisory","message":"message","mod_id":"mod","path":"events/Flavor FRA.txt","evidence":null,"line":3,"column":null,"confidence":null}"#;
		assert_eq!(
			serde_json::to_string(&finding(Some(game_path.clone()), None)).expect("serialize"),
			text
		);
		let read: Finding = serde_json::from_str(text).expect("a finding written before the split");
		assert_eq!(read.path, Some(game_path));
		assert_eq!(read.source_file, None);
	}

	#[test]
	fn an_input_file_finding_names_its_file_in_source_file_and_leaves_path_null() {
		let playlist = std::env::temp_dir().join("playlist.json");
		let json = serde_json::to_value(finding(None, Some(playlist.clone()))).expect("serialize");
		assert!(json["path"].is_null(), "{json}");
		assert_eq!(
			json["source_file"],
			playlist.to_str().expect("UTF-8 temp dir")
		);
	}

	#[test]
	fn a_finding_path_that_is_not_a_game_path_fails_to_read() {
		let text = r#"{"rule_id":"rule","severity":"Warning","channel":"Advisory","message":"m","mod_id":null,"path":"/abs/playlist.json","evidence":null,"line":null,"column":null,"confidence":null}"#;
		let error = serde_json::from_str::<Finding>(text).expect_err("an absolute path");
		assert!(error.to_string().contains("invalid game path"), "{error}");
	}
}
