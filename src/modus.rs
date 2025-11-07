use jomini::JominiDeserialize;
use std::path::Path;

#[derive(JominiDeserialize, Debug)]
struct ModDescriptor {
	name: String,
	path: Option<String>,
	#[jomini(default)]
	tags: Vec<String>,
	#[jomini(default)]
	dependencies: Vec<String>,
	version: Option<String>,
	remote_file_id: Option<String>,
}

impl TryFrom<&Path> for ModDescriptor {

	type Error = Box<dyn std::error::Error>;

	fn try_from(p: &Path) -> Result<Self, Self::Error> {
		let data = std::fs::read(p)?;

		let descriptor = jomini::text::de::from_windows1252_slice(&data)?;

		Ok(descriptor)
	}
}

impl TryFrom<&str> for ModDescriptor {

	type Error = Box<dyn std::error::Error>;

	fn try_from(data: &str) -> Result<Self, Self::Error> {
		let descriptor = jomini::text::de::from_windows1252_slice(data.as_bytes())?;

		Ok(descriptor)
	}
}

impl TryFrom<&[u8]> for ModDescriptor {

	type Error = Box<dyn std::error::Error>;

	fn try_from(data: &[u8]) -> Result<Self, Self::Error> {
		let descriptor = jomini::text::de::from_windows1252_slice(data)?;

		Ok(descriptor)
	}
}

impl<const N: usize> TryFrom<&[u8; N]> for ModDescriptor {
	type Error = Box<dyn std::error::Error>;
	fn try_from(data: &[u8; N]) -> Result<Self, Self::Error> {
		let descriptor = jomini::text::de::from_windows1252_slice(data)?;
		Ok(descriptor)
	}
}


mod tests {
	use super::*;


	#[test]
	fn test_mod_descriptor_from_data() {
		let data = br#"version="0.0.1"
		tags={
			"Utilities"
		}
		name="defines"
		supported_version="1.34.4"
		remote_file_id="2887527268""#;

		let descriptor = ModDescriptor::try_from(data).unwrap();

		assert_eq!(descriptor.name, "defines");
		assert_eq!(descriptor.version.unwrap(), "0.0.1");
		assert_eq!(descriptor.tags, vec!["Utilities"]);
		assert_eq!(descriptor.remote_file_id.unwrap(), "2887527268");



	}
}
