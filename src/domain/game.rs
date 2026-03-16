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
	pub fn from_key(value: &str) -> Option<Self> {
		match value.trim().to_ascii_lowercase().as_str() {
			"eu4" | "europauniversalis4" | "europa-universalis-4" => Some(Self::EuropaUniversalis4),
			"ck3" | "crusaderkings3" | "crusader-kings-3" => Some(Self::CrusaderKings3),
			"vic3" | "victoria3" | "victoria-3" => Some(Self::Victoria3),
			"stellaris" => Some(Self::Stellaris),
			"hoi4" | "heartsofiron4" | "hearts-of-iron-4" => Some(Self::HeartsOfIron4),
			_ => None,
		}
	}

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

	pub fn paradox_data_dir_name(&self) -> Option<&'static str> {
		match self {
			Self::EuropaUniversalis4 => Some("Europa Universalis IV"),
			Self::CrusaderKings3 => Some("Crusader Kings III"),
			Self::Victoria3 => Some("Victoria 3"),
			Self::Stellaris => Some("Stellaris"),
			Self::HeartsOfIron4 => Some("Hearts of Iron IV"),
			Self::Unknown => None,
		}
	}
}
