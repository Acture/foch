use crate::filesystem::FS;
use std::collections::HashMap;
use std::path::PathBuf;
use crate::utils::strip_quotes;

pub struct ModEntry {
	pub name: String,
	pub path: PathBuf,
	pub version: String,
	pub supported_version: String,
	pub remote_file_id: String,
}

impl ModEntry {
	pub fn from_mod_record(path: &PathBuf) -> Self {
		todo!()
	}

	pub fn from_mod_descriptor(path: &PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
		let descriptor_text = std::fs::read_to_string(path)?;
		let mut parser = crate::parsing::TSParserWrapper::new();
		let _ = parser.parse_as_tree(&descriptor_text);
		let nodes = parser
			.find_nodes(|node| node.kind() == "assignment")
			.expect("Failed to find assignment nodes");
		let hash_map = nodes
			.into_iter()
			.filter_map(|node| {
				let key = node.child_by_field_name("key")?;
				let key_name = key.utf8_text(&descriptor_text.as_ref()).ok()?;
				match key_name {
					"version" | "name" | "supported_version" | "remote_file_id" => {
						let value = node.child_by_field_name("value")?;
						let value_text = value.utf8_text(&descriptor_text.as_ref()).ok()?;
						Some((key_name.to_string(), value_text.to_string()))
					}
					_ => None,
				}
			})
			.collect::<HashMap<String, String>>();

		let name = strip_quotes(&hash_map["name"])?;
		let path = path.to_path_buf();
		let version = strip_quotes(&hash_map["version"])?;
		let supported_version = strip_quotes(&hash_map["supported_version"])?;
		let remote_file_id = strip_quotes(&hash_map["remote_file_id"])?;



		Ok(Self {
			name,
			path,
			version,
			supported_version,
			remote_file_id,
		})
	}
}

pub struct Mod {
	pub name: String,
	pub version: String,
	pub fs: FS,
}

impl From<ModEntry> for Mod {
	fn from(entry: ModEntry) -> Self {
		Mod {
			name: entry.name,
			version: entry.version,
			fs: FS::builder().root(entry.path).build(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::get_corpus_path;

	#[test]
	fn test_mod_entry_from_mod_descriptor() {
		let path = get_corpus_path().join("defines").join("descriptor.mod");
		let entry = ModEntry::from_mod_descriptor(&path).unwrap();
		assert_eq!(entry.name, "defines");
		assert_eq!(entry.version, "0.0.1");
		assert_eq!(entry.supported_version, "1.34.4");
	}
}
