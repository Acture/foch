//! Typed EU4 war boundary for native plugins and the test runner.
//! No game pointers, offsets, or guessed engine rules live here.
//!
//! A provider must produce one coherent game-thread snapshot and stable campaign,
//! country and war identities across saves. It must revalidate engine eligibility
//! inside `join_specific_war`, then acknowledge that exact request after entry.
//! These identities are a provider contract, not verified EU4 field offsets:
//! the current host does not yet implement a native war provider.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Compatibility re-export of the EU4 live war reader, which lives under
/// [`crate::dataplane::war`].
pub use crate::dataplane::war as eu4;

/// Provider-assigned country identity; never an ASCII tag encoded as an integer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct CountryId(pub u32);

/// The original declaration distinguishes wars even if an engine serial is reused.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarId {
	pub serial: u32,
	pub started: i32,
	pub declarer: CountryId,
	pub declared_on: CountryId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
	Attacker,
	Defender,
}

impl Side {
	pub fn opposite(self) -> Self {
		match self {
			Self::Attacker => Self::Defender,
			Self::Defender => Self::Attacker,
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Country {
	pub tag: String,
	/// Unknown subject status is not permission to enter a pact or a war.
	pub independent: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct War {
	pub id: WarId,
	pub attackers: Vec<CountryId>,
	pub defenders: Vec<CountryId>,
}

impl War {
	pub fn side(&self, country: CountryId) -> Option<Side> {
		if self.attackers.contains(&country) {
			Some(Side::Attacker)
		} else if self.defenders.contains(&country) {
			Some(Side::Defender)
		} else {
			None
		}
	}

	pub fn participants(&self, side: Side) -> &[CountryId] {
		match side {
			Side::Attacker => &self.attackers,
			Side::Defender => &self.defenders,
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct World {
	pub campaign: String,
	/// Engine calendar days, with one provider-owned, stable epoch.
	pub day: i32,
	pub revision: u64,
	pub countries: BTreeMap<CountryId, Country>,
	pub wars: Vec<War>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WarSelectionError {
	InvalidCountries,
	InvalidSnapshot(String),
	Ambiguous(Vec<WarId>),
	NotFound(WarId),
}

impl std::fmt::Display for WarSelectionError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::InvalidCountries => formatter.write_str("select two distinct existing countries"),
			Self::InvalidSnapshot(reason) => write!(formatter, "invalid war snapshot: {reason}"),
			Self::Ambiguous(wars) => write!(
				formatter,
				"{} wars match these countries; select an explicit war identity",
				wars.len()
			),
			Self::NotFound(war) => write!(
				formatter,
				"selected war {war:?} does not place these countries on opposing sides"
			),
		}
	}
}

impl std::error::Error for WarSelectionError {}

impl World {
	fn check_pair(&self, a: CountryId, b: CountryId) -> Result<(), WarSelectionError> {
		self.validate()
			.map_err(WarSelectionError::InvalidSnapshot)?;
		if a == b || !self.countries.contains_key(&a) || !self.countries.contains_key(&b) {
			return Err(WarSelectionError::InvalidCountries);
		}
		Ok(())
	}

	/// Select by actual opposing participation, never by the first matching
	/// leader or vector position. Multiple matching wars require an explicit ID.
	pub fn war_between(
		&self,
		a: CountryId,
		b: CountryId,
	) -> Result<Option<&War>, WarSelectionError> {
		self.check_pair(a, b)?;
		let matching: Vec<_> = self
			.wars
			.iter()
			.filter(
				|war| matches!((war.side(a), war.side(b)), (Some(left), Some(right)) if left != right),
			)
			.collect();
		match matching.as_slice() {
			[] => Ok(None),
			[war] => Ok(Some(war)),
			_ => {
				let mut ids: Vec<_> = matching.iter().map(|war| war.id).collect();
				ids.sort_unstable();
				Err(WarSelectionError::Ambiguous(ids))
			}
		}
	}

	pub fn select_war(
		&self,
		a: CountryId,
		b: CountryId,
		selected: WarId,
	) -> Result<&War, WarSelectionError> {
		self.check_pair(a, b)?;
		self.wars
			.iter()
			.find(|war| {
				war.id == selected
					&& matches!((war.side(a), war.side(b)), (Some(left), Some(right)) if left != right)
			})
			.ok_or(WarSelectionError::NotFound(selected))
	}

	pub fn validate(&self) -> Result<(), String> {
		if self.campaign.is_empty() || self.campaign.len() > 128 {
			return Err("missing or oversized campaign identity".into());
		}
		let mut tags = BTreeSet::new();
		for country in self.countries.values() {
			if country.tag.len() != 3
				|| !country
					.tag
					.bytes()
					.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
				|| !tags.insert(&country.tag)
			{
				return Err("invalid or duplicate country tag".into());
			}
		}
		let mut wars = BTreeSet::new();
		for war in &self.wars {
			let mut members = BTreeSet::new();
			if !wars.insert(war.id)
				|| war.id.declarer == war.id.declared_on
				|| war.id.started > self.day
				|| war.attackers.is_empty()
				|| war.defenders.is_empty()
			{
				return Err("invalid war identity or empty side".into());
			}
			for member in war.attackers.iter().chain(&war.defenders) {
				if !members.insert(*member) || !self.countries.contains_key(member) {
					return Err("duplicate, contradictory or absent war participant".into());
				}
			}
		}
		Ok(())
	}

	pub fn enemies(&self, a: CountryId, b: CountryId) -> bool {
		self.wars
			.iter()
			.any(|war| matches!((war.side(a), war.side(b)), (Some(sa), Some(sb)) if sa != sb))
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinRequest {
	pub war: WarId,
	pub country: CountryId,
	pub side: Side,
	pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JoinReceipt {
	pub request: JoinRequest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinRefusal {
	Truce(CountryId),
	Subject,
	OpposingWar,
	AlreadyEnemy,
	InconsistentReceipt,
	StaleSnapshot,
	Unsupported(String),
	EngineRejected(String),
}

impl JoinRefusal {
	pub fn explanation(&self) -> String {
		match self {
			Self::Truce(country) => format!(
				"A truce with country {} prevents this war entry.",
				country.0
			),
			Self::Subject => {
				"Independent foreign policy is required; subject entry is unsupported.".into()
			}
			Self::OpposingWar => {
				"Entry would place existing enemies on the same side or allies on opposite sides."
					.into()
			}
			Self::AlreadyEnemy => {
				"The obligated country is already on the opposing side of this war.".into()
			}
			Self::InconsistentReceipt => {
				"The engine did not acknowledge the requested country, war and side.".into()
			}
			Self::StaleSnapshot => {
				"The game state changed; war eligibility must be checked again.".into()
			}
			Self::Unsupported(reason) => format!("War entry is unavailable: {reason}"),
			Self::EngineRejected(reason) => format!("EU4 refused this war entry: {reason}"),
		}
	}
}

pub trait WarBridge {
	/// Must use actual EU4 conditions, including truces with every enemy,
	/// subject/overlord restrictions and conflicts across concurrent wars.
	fn validate_join(&mut self, request: &JoinRequest) -> Result<(), JoinRefusal>;
	/// Join only this war, rechecking all eligibility on the game thread.
	/// Returning success without observing membership is a provider defect.
	fn join_specific_war(&mut self, request: &JoinRequest) -> Result<JoinReceipt, JoinRefusal>;
}
