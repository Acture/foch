//! Corpus model: the set of community compatibility patches ("compatches")
//! discovered from the Steam Workshop, each pairing the mods it patches.
//!
//! A compatch is human ground truth for "what a good merge of mod A + mod B
//! looks like". `corpus.json` is the serialized form of [`Corpus`] and is the
//! only artifact the network discovery step produces; scoring consumes it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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

/// Per-patched-mod metadata captured at discovery time, used for version
/// provenance: a compatch is ground truth only for the specific
/// (game × modA × modB) version triple it was authored against. Keyed by
/// steam id in [`Case::patched_meta`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PatchedMeta {
	#[serde(default)]
	pub title: String,
	/// Unix timestamp of the patched mod's Workshop creation.
	#[serde(default)]
	pub time_created: i64,
	/// Unix timestamp of the patched mod's last Workshop update.
	#[serde(default)]
	pub time_updated: i64,
	#[serde(default)]
	pub workshop: WorkshopProvenance,
}

/// One compatch and the mods it declares as required items (the mods it patches).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Case {
	pub compatch_id: String,
	#[serde(default)]
	pub title: String,
	/// Steam ids of the patched mods, in declaration order (first = base mod).
	pub patched: Vec<String>,
	#[serde(default)]
	pub time_created: i64,
	#[serde(default)]
	pub time_updated: i64,
	#[serde(default)]
	pub subscriptions: i64,
	/// Per-patched-mod metadata, keyed by steam id.
	#[serde(default)]
	pub patched_meta: BTreeMap<String, PatchedMeta>,
	#[serde(default)]
	pub workshop: WorkshopProvenance,
}

impl Case {
	/// A patched mod was updated after the compatch — a churn signal that the
	/// compatch may no longer match current mod versions. NOT a validity check
	/// (version compatibility is checked post-download via `supported_version`).
	pub fn mod_churned(&self) -> bool {
		self.patched_meta
			.values()
			.any(|m| m.time_updated > self.time_updated)
	}
}

/// The full discovered corpus, matching `corpus.json`'s envelope.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Corpus {
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

impl Corpus {
	pub fn from_json(text: &str) -> serde_json::Result<Self> {
		serde_json::from_str(text)
	}

	pub fn to_json_pretty(&self) -> serde_json::Result<String> {
		serde_json::to_string_pretty(self)
	}
}
