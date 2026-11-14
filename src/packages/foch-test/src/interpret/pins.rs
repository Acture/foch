//! The semantics catalog: which primitives the offline model may execute,
//! pinned to the decompiled handler functions that define them.
//!
//! `semantics.json` is the single source shared by the interpreter and the
//! `eu4_analysis semantics` extractor. A primitive entry carries the facts
//! the model implements and, once a decompilation batch has run against the
//! symbol-bearing EU4 executable, the handler pin (address, symbol,
//! pseudocode digest). Until then the catalog is `unpinned`: the model still
//! interprets, but every result records that its semantics are not yet
//! evidence-backed. In a `pinned` catalog a primitive without its handler is
//! refused, never guessed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The embedded catalog seed. Replace it with a `pinned` file produced by
/// `python -m eu4_analysis semantics` after review.
pub const EMBEDDED: &str = include_str!("semantics.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveKind {
	Effect,
	Trigger,
}

/// One decompiled handler a primitive is pinned to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HandlerPin {
	/// A symbol-bearing name of the handler function, as the extractor saw it.
	pub symbol: String,
	/// Handler range in the analyzed executable's address space.
	pub address: u64,
	pub end: u64,
	/// SHA-256 of the decompiled pseudocode the facts were read from.
	pub pseudocode_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Primitive {
	pub kind: PrimitiveKind,
	/// Facts the model implements, stated so tests can audit them against the
	/// pinned pseudocode.
	pub facts: BTreeMap<String, String>,
	/// The decompiled handler, once a batch has pinned this primitive.
	pub handler: Option<HandlerPin>,
}

/// Whether the catalog's primitives are backed by decompiled handlers.
/// `game_version` and `binary_sha256` name the analyzed build when pinned.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
	/// No decompilation batch has run: the model interprets, and every result
	/// records that its semantics are not evidence-backed.
	Unpinned,
	/// Handler pins are active; a primitive without one is refused.
	Pinned,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticsCatalog {
	pub format: u32,
	pub status: EvidenceStatus,
	/// The analyzed game build, once a decompilation batch has pinned the
	/// catalog.
	pub game_version: Option<String>,
	pub binary_sha256: Option<String>,
	pub primitives: BTreeMap<String, Primitive>,
}

impl SemanticsCatalog {
	pub fn parse(text: &str) -> Result<Self, String> {
		let catalog: Self = serde_json::from_str(text).map_err(|error| format!("{error}"))?;
		if catalog.format != 1 {
			return Err(format!("unsupported semantics format {}", catalog.format));
		}
		let pinned = catalog.status == EvidenceStatus::Pinned;
		if pinned && (catalog.game_version.is_none() || catalog.binary_sha256.is_none()) {
			return Err("a pinned catalog must name its game version and binary".into());
		}
		for (name, primitive) in &catalog.primitives {
			if let Some(handler) = &primitive.handler
				&& (handler.address >= handler.end || handler.pseudocode_sha256.len() != 64)
			{
				return Err(format!("primitive {name} has an incomplete handler pin"));
			}
		}
		Ok(catalog)
	}

	/// The catalog embedded in this build.
	pub fn embedded() -> Self {
		Self::parse(EMBEDDED).expect("embedded semantics catalog is valid")
	}

	/// Whether the model may execute `name`. A pinned catalog refuses
	/// primitives whose handler evidence is missing.
	pub fn permits(&self, name: &str) -> Result<(), String> {
		let Some(primitive) = self.primitives.get(name) else {
			return Err(format!("{name} is not in the semantics catalog"));
		};
		if self.status == EvidenceStatus::Pinned && primitive.handler.is_none() {
			return Err(format!(
				"{name} has no decompiled handler evidence in the pinned catalog"
			));
		}
		Ok(())
	}

	/// A fact recorded for `name`, for tests that audit implemented semantics.
	pub fn fact(&self, name: &str, key: &str) -> Option<&str> {
		self.primitives
			.get(name)?
			.facts
			.get(key)
			.map(String::as_str)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_embedded_catalog_parses_and_matches_the_model() {
		let catalog = SemanticsCatalog::embedded();
		assert!(matches!(catalog.status, EvidenceStatus::Unpinned));
		let modeled: Vec<_> = catalog.primitives.keys().cloned().collect();
		assert_eq!(
			modeled,
			[
				"absolutism",
				"add_absolutism",
				"add_adm_power",
				"add_army_tradition",
				"add_devotion",
				"add_dip_power",
				"add_horde_unity",
				"add_legitimacy",
				"add_manpower",
				"add_mil_power",
				"add_navy_tradition",
				"add_prestige",
				"add_republican_tradition",
				"add_stability",
				"add_treasury",
				"adm_power",
				"alliance_with",
				"always",
				"army_tradition",
				"break_alliance",
				"change_government",
				"change_primary_culture",
				"change_religion",
				"change_variable",
				"check_variable",
				"clr_country_flag",
				"clr_global_flag",
				"country_event",
				"create_alliance",
				"declare_war_with_cb",
				"defensive_war_with",
				"devotion",
				"dip_power",
				"every_country",
				"government",
				"government_rank",
				"has_country_flag",
				"has_global_flag",
				"has_variable",
				"horde_unity",
				"is_at_war",
				"legitimacy",
				"log",
				"manpower",
				"mil_power",
				"navy_tradition",
				"offensive_war_with",
				"prestige",
				"primary_culture",
				"random",
				"random_list",
				"religion",
				"republican_tradition",
				"set_country_flag",
				"set_global_flag",
				"set_government_rank",
				"set_variable",
				"stability",
				"tag",
				"treasury",
				"white_peace",
			]
		);
		assert_eq!(catalog.fact("check_variable", "comparison"), Some(">="));
	}

	#[test]
	fn a_pinned_catalog_refuses_primitives_without_handlers() {
		let catalog = SemanticsCatalog::embedded();
		assert!(catalog.permits("set_country_flag").is_ok());
		let text = r#"{
			"format": 1,
			"status": "pinned",
			"game_version": "1.37.5",
			"binary_sha256": "abc",
			"primitives": {
				"set_country_flag": {
					"kind": "effect",
					"facts": {},
					"handler": {
						"symbol": "CCountry::SetFlag",
						"address": 16,
						"end": 64,
						"pseudocode_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
					}
				},
				"clr_country_flag": {"kind": "effect", "facts": {}, "handler": null}
			}
		}"#;
		let pinned = SemanticsCatalog::parse(text).unwrap();
		assert_eq!(
			pinned.permits("set_country_flag"),
			Ok(()),
			"a pinned primitive is permitted"
		);
		let refused = pinned.permits("clr_country_flag").unwrap_err();
		assert!(
			refused.contains("no decompiled handler evidence"),
			"{refused}"
		);
	}

	#[test]
	fn malformed_pins_are_rejected_at_parse_time() {
		let text = r#"{
			"format": 1,
			"status": "unpinned",
			"primitives": {
				"log": {
					"kind": "effect",
					"facts": {},
					"handler": {
						"symbol": "x",
						"address": 64,
						"end": 16,
						"pseudocode_sha256": "short"
					}
				}
			}
		}"#;
		assert!(SemanticsCatalog::parse(text).is_err());
	}
}
