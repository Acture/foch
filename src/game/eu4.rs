use serde::Serialize;
use crate::filesystem::{WithFileSystem, FS};
use crate::game::GameType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EU4 {}


impl GameType for EU4 {
	const NAME: &'static str = "Europa Universalis IV";
}

impl TryFrom<String> for EU4 {
	type Error = ();

	fn try_from(value: String) -> Result<Self, Self::Error> {
		if value == Self::NAME {
			Ok(EU4 {})
		} else {
			Err(())
		}
	}
}