use foch_core::model::{Finding, SemanticIndex, SymbolDefinition, SymbolKind};
use std::collections::HashMap;
use std::path::PathBuf;

/// A workspace session holding analysis results ready for consumption by CLI or LSP.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceSession {
	pub index: SemanticIndex,
	pub file_paths: Vec<PathBuf>,
	pub path_lookup: HashMap<String, PathBuf>,
	pub findings: Vec<Finding>,
}

impl WorkspaceSession {
	/// Build a session from parsed analysis results.
	///
	/// `path_lookup` maps composite keys (e.g. `"{mod_id}|{relative_path}"`) to
	/// absolute file paths. The caller owns the key scheme so both playlist-based
	/// engine resolution and the LSP's own file discovery can feed this type.
	pub fn from_analysis(
		index: SemanticIndex,
		file_paths: Vec<PathBuf>,
		path_lookup: HashMap<String, PathBuf>,
		findings: Vec<Finding>,
	) -> Self {
		Self {
			index,
			file_paths,
			path_lookup,
			findings,
		}
	}

	/// Get symbol definitions matching a name and optional kind filter.
	pub fn find_definitions(&self, name: &str, kind: Option<SymbolKind>) -> Vec<&SymbolDefinition> {
		self.index
			.definitions
			.iter()
			.filter(|d| d.name == name && kind.is_none_or(|k| d.kind == k))
			.collect()
	}

	/// Look up the absolute path for a composite `"{mod_id}|{relative_path}"` key.
	pub fn resolve_path(&self, key: &str) -> Option<&PathBuf> {
		self.path_lookup.get(key)
	}
}
