use std::borrow::Cow;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::compile::CwtSchemaGraph;
use super::error::CwtLoadError;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CwtSchemaId([u8; 32]);

impl CwtSchemaId {
	pub fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}

	pub fn to_hex(&self) -> String {
		self.0.iter().map(|byte| format!("{byte:02x}")).collect()
	}
}

impl Display for CwtSchemaId {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(&self.to_hex())
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CwtSource {
	Vendored { commit: String },
	UserProvided { path: PathBuf },
}

#[derive(Clone, Debug)]
pub struct SchemaPack {
	pub id: CwtSchemaId,
	pub source: CwtSource,
	pub graph: Arc<CwtSchemaGraph>,
}

impl SchemaPack {
	pub(crate) fn load_from_dir(root: &Path, source: CwtSource) -> Result<Self, CwtLoadError> {
		let id = cwt_schema_id_from_dir(root)?;
		Self::load_from_dir_with_id(root, source, id)
	}

	pub(crate) fn load_from_dir_with_id(
		root: &Path,
		source: CwtSource,
		id: CwtSchemaId,
	) -> Result<Self, CwtLoadError> {
		let graph = Arc::new(CwtSchemaGraph::from_directory(root)?);
		Ok(Self { id, source, graph })
	}
}

pub fn cwt_schema_id_from_dir(root: &Path) -> Result<CwtSchemaId, CwtLoadError> {
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
		hasher.update(normalize_line_endings(&bytes));
	}
	Ok(CwtSchemaId(hasher.finalize().into()))
}

fn normalize_line_endings(bytes: &[u8]) -> Cow<'_, [u8]> {
	if !bytes.contains(&b'\r') {
		return Cow::Borrowed(bytes);
	}

	let mut normalized = Vec::with_capacity(bytes.len());
	let mut index = 0;
	while index < bytes.len() {
		if bytes[index] == b'\r' {
			normalized.push(b'\n');
			index += usize::from(bytes.get(index + 1) == Some(&b'\n'));
		} else {
			normalized.push(bytes[index]);
		}
		index += 1;
	}
	Cow::Owned(normalized)
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy()
		.replace('\\', "/")
		.trim_matches('/')
		.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::cwt_schema_id_from_dir;

	#[test]
	fn schema_pack_id_ignores_text_line_endings() {
		let lf = tempfile::tempdir().unwrap();
		let crlf = tempfile::tempdir().unwrap();
		fs::write(
			lf.path().join("rules.cwt"),
			b"types = {\n  event = { }\n}\n",
		)
		.unwrap();
		fs::write(
			crlf.path().join("rules.cwt"),
			b"types = {\r\n  event = { }\r\n}\r\n",
		)
		.unwrap();

		assert_eq!(
			cwt_schema_id_from_dir(lf.path()).unwrap(),
			cwt_schema_id_from_dir(crlf.path()).unwrap()
		);
	}
}
