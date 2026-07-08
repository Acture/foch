use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::{CwtLoadError, CwtSchemaGraph, install_base_scopes};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SchemaPackId([u8; 32]);

impl SchemaPackId {
	pub fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}

	pub fn to_hex(&self) -> String {
		self.0.iter().map(|byte| format!("{byte:02x}")).collect()
	}
}

impl Display for SchemaPackId {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(&self.to_hex())
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaSource {
	Vendored { commit: String },
	UserProvided { path: PathBuf },
}

#[derive(Clone, Debug)]
pub struct SchemaPack {
	pub id: SchemaPackId,
	pub source: SchemaSource,
	pub target_eu4_version: Option<String>,
	pub graph: Arc<CwtSchemaGraph>,
}

impl SchemaPack {
	pub fn load_from_dir(root: &Path, source: SchemaSource) -> Result<Self, CwtLoadError> {
		let id = schema_pack_id_from_dir(root)?;
		Self::load_from_dir_with_id(root, source, id)
	}

	pub(crate) fn load_from_dir_with_id(
		root: &Path,
		source: SchemaSource,
		id: SchemaPackId,
	) -> Result<Self, CwtLoadError> {
		let graph = Arc::new(CwtSchemaGraph::from_directory(root)?);
		install_base_scopes(&graph);
		Ok(Self {
			id,
			source,
			target_eu4_version: None,
			graph,
		})
	}
}

pub fn schema_pack_id_from_dir(root: &Path) -> Result<SchemaPackId, CwtLoadError> {
	let mut files = WalkDir::new(root)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("cwt"))
		.map(|entry| entry.into_path())
		.collect::<Vec<_>>();
	files.sort_by_key(|path| normalize_path(path));
	let mut hasher = Sha256::new();
	for path in files {
		let bytes = std::fs::read(&path).map_err(|source| CwtLoadError::Io {
			path: path.clone(),
			source,
		})?;
		hasher.update(bytes);
	}
	Ok(SchemaPackId(hasher.finalize().into()))
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy()
		.replace('\\', "/")
		.trim_matches('/')
		.to_ascii_lowercase()
}
