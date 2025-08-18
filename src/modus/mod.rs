use std::path::PathBuf;
use crate::filesystem::{WithFileSystem, FS};

pub struct ModEntry {
	pub name: String,
	pub path: PathBuf,
	pub version: String,
}

impl ModEntry {
	pub fn from_mod_record(path: &PathBuf) -> Self {
		todo!()
	}

	pub fn from_mod_descriptor(path: &PathBuf) -> Self {
		todo!()
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
			fs: FS::builder()
				.root(entry.path)
				.build(),
		}
	}
}

impl WithFileSystem for Mod {
	fn fs(&self) -> &FS {
		&self.fs
	}

	fn fs_mut(&mut self) -> &mut FS {
		&mut self.fs
	}
}