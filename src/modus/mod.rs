use crate::filesystem::{FileWatcher, FS};
use crate::utils::strip_quotes;
use std::collections::HashMap;
use std::path::PathBuf;

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
		let _ = parser.parse(&descriptor_text);
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
		let path = path.parent().expect("Failed to get parent directory of descriptor file").to_path_buf();
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
	pub fw: Box<dyn FileWatcher>,
}

impl From<ModEntry> for Mod {
	fn from(entry: ModEntry) -> Self {
		Mod {
			name: entry.name,
			version: entry.version,
			fw: FS::new_file_watcher(entry.path).expect("Failed to create file watcher"),
		}
	}
}

impl Mod {
	pub fn find_conflict_files(&mut self, other: &mut Self) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
		let file_snapshot = self.fw.file_snapshot()?;
		let other_file_snapshot = other.fw.file_snapshot()?;
		let mut conflicts = Vec::new();
		for (path, hash) in file_snapshot {
			if let Some(other_hash) = other_file_snapshot.get(&path) {
				let self_hash = match hash {
					Some(h) => h,
					None => {
						self.fw.update_hash(&path)
							.map_err(|e| format!("Failed to get fingerprint for {:?}: {}", path, e))?
					}
				};
				let other_hash = match other_hash {
					Some(h) => h.clone(),
					None => {
						other.fw.update_hash(&path).map_err(|e| format!("Failed to get fingerprint for {:?}: {}", path, e))?
					}
				};
				if self_hash != other_hash {
					conflicts.push(path);
				}
			}
		}
		Ok(conflicts)
	}

	pub fn merge(&self, other: &Self) -> Self {
		todo!("Implement merge logic for mods")
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

	#[test]
	fn test_find_conflict_files() {
		let path1 = get_corpus_path().join("defines").join("descriptor.mod");
		let path2 = get_corpus_path().join("control_military_access").join("descriptor.mod");
		let mut mod1 = Mod::from(ModEntry::from_mod_descriptor(&path1).unwrap());
		let mut mod2 = Mod::from(ModEntry::from_mod_descriptor(&path2).unwrap());
		println!("Mod1: {:?}, Mod2: {:?}", mod1.name, mod2.name);
		mod1.fw.collect_files().unwrap();
		mod2.fw.collect_files().unwrap();

		let conflicts = mod1.find_conflict_files(&mut mod2).unwrap();
		let files1 = mod1.fw.file_snapshot().unwrap();
		let files2 = mod2.fw.file_snapshot().unwrap();
		println!("Files in mod1: {:?}", files1);
		println!("Files in mod2: {:?}", files2);
		assert!(conflicts.len() == 1, "Expected conflicts of mod descriptor");
	}
}
