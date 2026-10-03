use std::borrow::Cow;
use std::fmt::{self, Display, Formatter};
use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::compile::{CwtSchemaGraph, cwt_files};
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

	pub fn from_hex(hex: &str) -> Option<Self> {
		let mut bytes = [0; 32];
		if hex.len() != bytes.len() * 2 {
			return None;
		}
		for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
			*byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
		}
		Some(Self(bytes))
	}
}

impl Display for CwtSchemaId {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(&self.to_hex())
	}
}

#[derive(Clone, Debug)]
pub struct SchemaPack {
	pub id: CwtSchemaId,
	pub graph: Arc<CwtSchemaGraph>,
}

impl SchemaPack {
	pub(crate) fn load_from_dir(root: &Path) -> Result<Self, CwtLoadError> {
		let id = cwt_schema_id_from_dir(root)?;
		Self::load_from_dir_with_id(root, id)
	}

	pub(crate) fn load_from_dir_with_id(
		root: &Path,
		id: CwtSchemaId,
	) -> Result<Self, CwtLoadError> {
		let graph = Arc::new(CwtSchemaGraph::from_directory(root)?);
		Ok(Self { id, graph })
	}
}

pub fn cwt_schema_id_from_dir(root: &Path) -> Result<CwtSchemaId, CwtLoadError> {
	let mut hasher = Sha256::new();
	for path in cwt_files(root)? {
		let bytes = std::fs::read(&path).map_err(|source| CwtLoadError::Io {
			path: path.clone(),
			source,
		})?;
		hasher.update(normalize_line_endings(&bytes));
	}
	Ok(CwtSchemaId(hasher.finalize().into()))
}

pub(crate) fn normalize_line_endings(bytes: &[u8]) -> Cow<'_, [u8]> {
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

#[cfg(test)]
mod tests {
	use std::fs;

	use super::{CwtSchemaId, cwt_schema_id_from_dir};
	use crate::game::schema::error::CwtLoadError;

	#[test]
	fn schema_id_round_trips_through_hex() {
		let dir = tempfile::tempdir().unwrap();
		fs::write(dir.path().join("rules.cwt"), b"types = { }\n").unwrap();
		let id = cwt_schema_id_from_dir(dir.path()).unwrap();

		assert_eq!(CwtSchemaId::from_hex(&id.to_hex()), Some(id));
		assert_eq!(CwtSchemaId::from_hex("abc"), None);
		assert_eq!(CwtSchemaId::from_hex(&"zz".repeat(32)), None);
	}

	#[test]
	fn a_directory_without_rule_files_has_no_schema_id() {
		let empty = tempfile::tempdir().unwrap();
		fs::write(empty.path().join("README.md"), b"not a rule file\n").unwrap();

		assert!(matches!(
			cwt_schema_id_from_dir(empty.path()),
			Err(CwtLoadError::NoRuleFiles { .. })
		));
	}

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
