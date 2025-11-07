use serde::{Deserialize, Serialize};

pub mod mod_descriptor;
pub mod playlist;


#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub enum Game{
	#[serde(alias = "eu4")]
	EuropaUniversalis4,
	CrusaderKings3,
	Victoria3,
	Stellaris,
	HeartsOfIron4,
}
