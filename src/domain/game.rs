use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default, Eq, PartialEq)]
pub enum Game {
	#[serde(alias = "eu4")]
	EuropaUniversalis4,
	#[serde(alias = "ck3")]
	CrusaderKings3,
	#[serde(alias = "vic3")]
	Victoria3,
	Stellaris,
	#[serde(alias = "hoi4")]
	HeartsOfIron4,
	#[serde(other)]
	#[default]
	Unknown,
}

impl Game {
	pub fn steam_app_ids(&self) -> &'static [u32] {
		match self {
			Self::EuropaUniversalis4 => &[236850],
			Self::CrusaderKings3 => &[1158310],
			Self::Victoria3 => &[529340],
			Self::Stellaris => &[281990],
			Self::HeartsOfIron4 => &[394360],
			Self::Unknown => &[],
		}
	}

	pub fn key(&self) -> &'static str {
		match self {
			Self::EuropaUniversalis4 => "eu4",
			Self::CrusaderKings3 => "ck3",
			Self::Victoria3 => "vic3",
			Self::Stellaris => "stellaris",
			Self::HeartsOfIron4 => "hoi4",
			Self::Unknown => "unknown",
		}
	}
}
