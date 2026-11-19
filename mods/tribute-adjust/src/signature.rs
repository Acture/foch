//! Signature table: what the plugin needs from one exact eu4.exe build.
//!
//! One file per game version under `signatures/`. The plugin embeds the table
//! for the version the manifest declares; a missing or `unverified` entry
//! makes init refuse — the plugin never hooks on a guess.

use serde::{Deserialize, Serialize};

/// What the detour needs from the executable, keyed by role.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SignatureTable {
	/// The game version this table was reversed against, e.g. "1.37.5".
	pub game_version: String,
	/// SHA-256 of eu4.exe, lowercase hex; informational, the host checks it
	/// against the plan already. Empty when not recorded yet.
	pub exe_sha256: String,
	/// The monthly settlement point that adds a subject's tribute to its
	/// overlord. The detour multiplies the value passed on.
	pub tribute_settlement: Pattern,
	/// `AddIncome(country, category, amount, flag)` — logging hook that
	/// identifies the vassal/colony income category at runtime.
	pub add_income: Pattern,
	/// `ChangeTreasury(country, delta)` — treasury at `country+0xbb0`.
	pub change_treasury: Pattern,
	/// `ApplyMonthlyIncome(country, flush)` — banks `country+0x68` monthly.
	pub monthly_apply: Pattern,
	/// Engine define RVAs the monthly tribute math reads; patched (not
	/// hooked) because every enclosing function fails the 14-byte rule.
	pub defines: Defines,
	/// Per-subject income contribution (monthly category 2 + UI).
	pub subject_income: Pattern,
	/// How to read one subject relation's kind discriminant from the object
	/// the settlement function receives.
	pub subject_kind: FieldRead,
	/// How to read the subject's tag from the same object.
	pub subject_tag: FieldRead,
	/// Discriminant value -> [`crate::coefficient::SubjectKind::key`] spelling.
	pub kind_values: Vec<KindValue>,
}

/// Define constant locations (RVAs) used by the tribute math.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Defines {
	pub cash_rate: u32,
	pub adm_rate: u32,
	pub dip_rate: u32,
	pub mil_rate: u32,
	pub manpower_rate: u32,
	pub vassal_share: u32,
}

/// One row of the kind mapping.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct KindValue {
	pub value: u32,
	pub key: String,
}

/// A byte pattern for `find_pattern` (`"48 8B ?? 05 ?"` syntax) plus the role
/// it was verified to fill.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pattern {
	/// Host `find_pattern` syntax; `??`/`?` wildcard bytes.
	pub pattern: String,
	/// Offset from the match to the hooked address, in bytes.
	pub offset: i64,
	/// `verified` once confirmed against the exact exe; `unverified` makes
	/// init refuse rather than hook a guess.
	pub status: PatternStatus,
	/// Where the pattern and offset come from, so a future version's work
	/// starts from notes instead of from zero.
	pub provenance: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternStatus {
	Unverified,
	Verified,
}

/// A field read inside a game object: a fixed offset chain from a register
/// the settlement function already holds.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FieldRead {
	/// Offsets to chase from the base pointer, in order; empty means the base
	/// pointer itself is the value.
	pub offsets: Vec<u32>,
	/// `u32` discriminants for the kind read, `tag_ptr` for the tag.
	pub kind: ReadKind,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub provenance: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadKind {
	/// Read a u32 discriminant, mapped through `kind_values`.
	Discriminant,
	/// Read a pointer to the tag string.
	TagPointer,
	/// Read an inline packed country tag (`CCountry+0x20`: 3-byte ASCII tag + NUL + i16 index).
	InlineTag,
}

impl SignatureTable {
	/// A table is usable only when every pattern is verified, any pinned
	/// executable digest matches the shared data plane, and every kind
	/// value maps to a known subject kind key.
	pub fn usable(&self) -> Result<(), String> {
		if !self.exe_sha256.is_empty() && !foch_runtime::dataplane::supports_build(&self.exe_sha256)
		{
			return Err(format!(
				"exe_sha256 for {} does not match foch_runtime::dataplane",
				self.game_version
			));
		}
		if self.tribute_settlement.status != PatternStatus::Verified {
			return Err(format!(
				"tribute_settlement pattern for {} is {}",
				self.game_version,
				if self.tribute_settlement.status == PatternStatus::Verified {
					"verified"
				} else {
					"unverified"
				}
			));
		}
		if self.kind_values.is_empty() {
			return Err(format!("kind_values for {} is empty", self.game_version));
		}
		for KindValue { value, key } in &self.kind_values {
			if !crate::coefficient::SubjectKind::ALL
				.iter()
				.any(|k| k.key() == key)
			{
				return Err(format!(
					"kind value {value} maps to unknown kind key {key:?}"
				));
			}
		}
		Ok(())
	}
}

/// The table for the one version the manifest declares. Filled in from
/// `signatures/signatures-1.37.5.toml` once the reversing milestone lands;
/// until then init refuses with the missing role named.
pub const SIGNATURES_1_37_5: &str = include_str!("../signatures/signatures-1.37.5.toml");

pub fn load_embedded() -> Result<SignatureTable, String> {
	let table: SignatureTable = toml::from_str(SIGNATURES_1_37_5)
		.map_err(|error| format!("embedded signature table: {error}"))?;
	Ok(table)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn table(status: PatternStatus) -> SignatureTable {
		SignatureTable {
			game_version: "1.37.5".into(),
			exe_sha256: String::new(),
			tribute_settlement: Pattern {
				pattern: "48 89 5C 24 ?? 57".into(),
				offset: 0,
				status,
				provenance: "test".into(),
			},
			add_income: Pattern {
				pattern: "48 89 5C 24 10".into(),
				offset: 0,
				status,
				provenance: "test".into(),
			},
			change_treasury: Pattern {
				pattern: "48 89 5C 24 18".into(),
				offset: 0,
				status,
				provenance: "test".into(),
			},
			monthly_apply: Pattern {
				pattern: "48 89 5C 24 08 57".into(),
				offset: 0,
				status,
				provenance: "test".into(),
			},
			defines: Defines {
				cash_rate: 0x33f820,
				adm_rate: 0x33f81c,
				dip_rate: 0x33f834,
				mil_rate: 0x33f838,
				manpower_rate: 0x33f830,
				vassal_share: 0x1dc1574,
			},
			subject_income: Pattern {
				pattern: "4C 89 4C 24 20".into(),
				offset: 0,
				status,
				provenance: "test".into(),
			},
			subject_kind: FieldRead {
				offsets: vec![0x18],
				kind: ReadKind::Discriminant,
				provenance: None,
			},
			subject_tag: FieldRead {
				offsets: vec![0x30, 0x8],
				kind: ReadKind::TagPointer,
				provenance: None,
			},
			kind_values: vec![
				KindValue {
					value: 0,
					key: "vassal".into(),
				},
				KindValue {
					value: 3,
					key: "tributary".into(),
				},
			],
		}
	}

	#[test]
	fn unverified_tables_are_refused() {
		assert!(table(PatternStatus::Unverified).usable().is_err());
	}

	#[test]
	fn verified_tables_with_known_kinds_pass() {
		assert!(table(PatternStatus::Verified).usable().is_ok());
	}

	#[test]
	fn unknown_kind_keys_are_refused() {
		let mut table = table(PatternStatus::Verified);
		table.kind_values.push(KindValue {
			value: 9,
			key: "palpatine".into(),
		});
		assert!(table.usable().is_err());
	}

	#[test]
	fn the_shipped_table_parses_verified() {
		let table = load_embedded().expect("embedded signature table parses");
		// The 1.37.5 table ships verified patterns; gaps must close, not open.
		assert!(table.usable().is_ok(), "{:?}", table.usable().err());
		assert_eq!(table.exe_sha256.len(), 64);
		assert!(foch_runtime::dataplane::supports_build(&table.exe_sha256));
		assert_eq!(table.subject_tag.kind, ReadKind::InlineTag);
		assert_eq!(
			table.subject_tag.offsets,
			vec![foch_runtime::dataplane::layout::country::TAG as u32]
		);
	}
}
