use crate::domain::ParseError;
use jomini::JominiDeserialize;
use std::path::Path;

#[derive(JominiDeserialize, Debug, Clone, Default)]
pub struct ModDescriptor {
	#[jomini(default)]
	pub name: String,
	pub path: Option<String>,
	#[jomini(default)]
	pub tags: Vec<String>,
	#[jomini(default)]
	pub dependencies: Vec<String>,
	pub version: Option<String>,
	pub remote_file_id: Option<String>,
	pub supported_version: Option<String>,
}

pub fn load_descriptor(path: &Path) -> Result<ModDescriptor, ParseError> {
	let data = std::fs::read(path).map_err(|err| ParseError::io(path.to_path_buf(), err))?;
	jomini::text::de::from_windows1252_slice::<ModDescriptor>(&data)
		.map_err(|err| ParseError::format(path.to_path_buf(), err.to_string()))
}

#[cfg(test)]
mod tests {
	use super::load_descriptor;
	use std::path::Path;

	#[test]
	fn parses_descriptor_from_corpus() {
		let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
		let test_file = manifest_dir
			.join("tests")
			.join("corpus")
			.join("defines")
			.join("descriptor.mod");

		let descriptor = load_descriptor(&test_file).expect("failed to parse descriptor");
		assert_eq!(descriptor.name, "defines");
		assert_eq!(descriptor.version.as_deref(), Some("0.0.1"));
		assert_eq!(descriptor.remote_file_id.as_deref(), Some("2887527268"));
	}
}
