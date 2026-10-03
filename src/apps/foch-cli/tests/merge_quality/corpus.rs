//! Corpus model for broad Steam Workshop compatibility-patch candidates.
//!
//! Steam child relationships are discovery evidence, not proof that an item is
//! a compatch or that every referenced mod is one of its merge inputs. The
//! broad candidate corpus is retained for recall; [`OracleAssessment`] controls
//! which cases may enter merge-quality reporting.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub const CORPUS_SCHEMA: &str = "1.0.0";
pub const ORACLE_POLICY_VERSION: &str = "1.1.0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleStatus {
	Accepted,
	Proposed,
	Excluded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleEvidence {
	ExplicitCompatibilityIntentInTitle,
	ExactlyTwoReferencedMods,
	NoReferencedModChurn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleExclusionReason {
	InsufficientReferencedMods,
	MissingExplicitCompatibilityIntent,
	AmbiguousReferencedMods,
	ReferencedModUpdatedAfterCandidate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OracleAssessment {
	pub policy_version: String,
	pub status: OracleStatus,
	pub evidence: Vec<OracleEvidence>,
	pub exclusion_reason: Option<OracleExclusionReason>,
}

impl OracleAssessment {
	pub fn is_scorable(&self) -> bool {
		matches!(self.status, OracleStatus::Accepted | OracleStatus::Proposed)
	}
}

pub fn assess_oracle_candidate(
	title: &str,
	referenced_mod_count: usize,
	mod_churned: bool,
) -> OracleAssessment {
	static EXPLICIT_INTENT: OnceLock<Regex> = OnceLock::new();
	let explicit_intent = EXPLICIT_INTENT
		.get_or_init(|| {
			Regex::new(r"(?i)\b(?:compatch|compat(?:ibility)?(?:\s+patch)?|patch)\b")
				.expect("oracle intent regex is valid")
		})
		.is_match(title);
	let mut evidence = Vec::new();
	if explicit_intent {
		evidence.push(OracleEvidence::ExplicitCompatibilityIntentInTitle);
	}
	if referenced_mod_count == 2 {
		evidence.push(OracleEvidence::ExactlyTwoReferencedMods);
	}
	if !mod_churned {
		evidence.push(OracleEvidence::NoReferencedModChurn);
	}
	let exclusion_reason = if referenced_mod_count < 2 {
		Some(OracleExclusionReason::InsufficientReferencedMods)
	} else if !explicit_intent {
		Some(OracleExclusionReason::MissingExplicitCompatibilityIntent)
	} else if referenced_mod_count != 2 {
		Some(OracleExclusionReason::AmbiguousReferencedMods)
	} else if mod_churned {
		Some(OracleExclusionReason::ReferencedModUpdatedAfterCandidate)
	} else {
		None
	};

	OracleAssessment {
		policy_version: ORACLE_POLICY_VERSION.to_string(),
		status: if exclusion_reason.is_some() {
			OracleStatus::Excluded
		} else {
			OracleStatus::Proposed
		},
		evidence,
		exclusion_reason,
	}
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedistributionStatus {
	#[default]
	Unknown,
	Permitted,
	Restricted,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkshopAvailability {
	#[default]
	Unknown,
	Active,
	Unavailable,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkshopProvenance {
	#[serde(default)]
	pub creator_steam_id: String,
	#[serde(default)]
	pub url: String,
	#[serde(default)]
	pub visibility: Option<i64>,
	#[serde(default)]
	pub detected_license: Option<String>,
	#[serde(default)]
	pub redistribution_status: RedistributionStatus,
	#[serde(default)]
	pub availability: WorkshopAvailability,
}

/// Per-referenced-mod metadata captured at discovery time for version
/// provenance. Keyed by Steam id in [`Case::referenced_mod_meta`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReferencedModMeta {
	#[serde(default)]
	pub title: String,
	/// Unix timestamp of the referenced mod's Workshop creation.
	#[serde(default)]
	pub time_created: i64,
	/// Unix timestamp of the referenced mod's last Workshop update.
	#[serde(default)]
	pub time_updated: i64,
	#[serde(default)]
	pub workshop: WorkshopProvenance,
}

/// One Workshop candidate and the mod items it references as children.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Case {
	pub compatch_id: String,
	#[serde(default)]
	pub title: String,
	/// Referenced mod Steam ids in Workshop declaration order. These become
	/// merge-input candidates only after oracle eligibility is established.
	pub referenced_mods: Vec<String>,
	#[serde(default)]
	pub time_created: i64,
	#[serde(default)]
	pub time_updated: i64,
	#[serde(default)]
	pub subscriptions: i64,
	/// Per-referenced-mod metadata, keyed by Steam id.
	#[serde(default)]
	pub referenced_mod_meta: BTreeMap<String, ReferencedModMeta>,
	#[serde(default)]
	pub workshop: WorkshopProvenance,
}

impl Case {
	pub fn oracle_assessment(&self) -> OracleAssessment {
		assess_oracle_candidate(&self.title, self.referenced_mods.len(), self.mod_churned())
	}

	/// A referenced mod was updated after the candidate reference output.
	pub fn mod_churned(&self) -> bool {
		self.referenced_mod_meta
			.values()
			.any(|m| m.time_updated > self.time_updated)
	}
}

/// The full discovered corpus, matching `corpus.json`'s envelope.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Corpus {
	pub schema: String,
	/// Unix timestamp the corpus was generated.
	#[serde(default)]
	pub generated_at: i64,
	/// Short git SHA of the tool that produced the corpus.
	#[serde(default)]
	pub tool_commit: String,
	/// Workshop search terms used during discovery.
	#[serde(default)]
	pub search_terms: Vec<String>,
	pub cases: Vec<Case>,
}

impl Default for Corpus {
	fn default() -> Self {
		Self {
			schema: CORPUS_SCHEMA.to_string(),
			generated_at: 0,
			tool_commit: String::new(),
			search_terms: Vec::new(),
			cases: Vec::new(),
		}
	}
}

impl Corpus {
	pub fn from_json(text: &str) -> serde_json::Result<Self> {
		let corpus: Self = serde_json::from_str(text)?;
		if corpus.schema != CORPUS_SCHEMA {
			return Err(<serde_json::Error as serde::de::Error>::custom(format!(
				"unsupported corpus schema {}; expected {CORPUS_SCHEMA}",
				corpus.schema
			)));
		}
		Ok(corpus)
	}

	pub fn to_json_pretty(&self) -> serde_json::Result<String> {
		serde_json::to_string_pretty(self)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn explicit_two_mod_compatch_is_proposed_for_scoring() {
		let assessment = assess_oracle_candidate("Imperial Circles - Europa Patch", 2, false);
		assert_eq!(assessment.status, OracleStatus::Proposed);
		assert!(assessment.is_scorable());
		assert_eq!(assessment.exclusion_reason, None);
	}

	#[test]
	fn broad_search_false_positive_is_excluded() {
		let assessment = assess_oracle_candidate("Elder Scrolls Universalis", 5, false);
		assert_eq!(assessment.status, OracleStatus::Excluded);
		assert_eq!(
			assessment.exclusion_reason,
			Some(OracleExclusionReason::MissingExplicitCompatibilityIntent)
		);
	}

	#[test]
	fn explicit_patch_with_ambiguous_references_is_excluded() {
		let assessment = assess_oracle_candidate("Overhaul Compatibility Patch", 3, false);
		assert_eq!(assessment.status, OracleStatus::Excluded);
		assert_eq!(
			assessment.exclusion_reason,
			Some(OracleExclusionReason::AmbiguousReferencedMods)
		);
	}

	#[test]
	fn candidate_older_than_a_referenced_mod_is_excluded() {
		let assessment = assess_oracle_candidate("Overhaul Compatibility Patch", 2, true);
		assert_eq!(assessment.status, OracleStatus::Excluded);
		assert_eq!(
			assessment.exclusion_reason,
			Some(OracleExclusionReason::ReferencedModUpdatedAfterCandidate)
		);
	}

	#[test]
	fn corpus_rejects_an_unsupported_schema() {
		let error = Corpus::from_json(r#"{"schema":"2.0.0","cases":[]}"#).unwrap_err();
		assert!(
			error
				.to_string()
				.contains("unsupported corpus schema 2.0.0")
		);
	}
}
