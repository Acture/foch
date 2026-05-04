use foch_core::config::{ConfigError, FochConfig, ResolutionMap};
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
	resolution_map: ResolutionMap,
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
		Self::from_analysis_with_resolution_map(
			index,
			file_paths,
			path_lookup,
			findings,
			ResolutionMap::default(),
		)
	}

	pub fn from_analysis_with_config(
		index: SemanticIndex,
		file_paths: Vec<PathBuf>,
		path_lookup: HashMap<String, PathBuf>,
		findings: Vec<Finding>,
		config: &FochConfig,
	) -> Result<Self, ConfigError> {
		let resolution_map = ResolutionMap::from_entries(&config.resolutions)?;
		Ok(Self::from_analysis_with_resolution_map(
			index,
			file_paths,
			path_lookup,
			findings,
			resolution_map,
		))
	}

	pub fn from_analysis_with_resolution_map(
		index: SemanticIndex,
		file_paths: Vec<PathBuf>,
		path_lookup: HashMap<String, PathBuf>,
		findings: Vec<Finding>,
		resolution_map: ResolutionMap,
	) -> Self {
		Self {
			index,
			file_paths,
			path_lookup,
			findings,
			resolution_map,
		}
	}

	pub fn resolution_map(&self) -> &ResolutionMap {
		&self.resolution_map
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

#[cfg(test)]
mod tests {
	use std::collections::HashMap;
	use std::path::Path;

	use foch_core::config::{FochConfig, ResolutionDecision};
	use foch_core::model::SemanticIndex;

	use super::WorkspaceSession;

	#[test]
	fn workspace_session_loads_foch_toml_resolutions() {
		let config = FochConfig::from_toml_str(
			r#"
[[resolutions]]
file = "common/ideas/resolved.txt"
prefer_mod = "mod-a"
"#,
		)
		.expect("parse foch.toml");

		let session = WorkspaceSession::from_analysis_with_config(
			SemanticIndex::default(),
			Vec::new(),
			HashMap::new(),
			Vec::new(),
			&config,
		)
		.expect("build session");

		assert_eq!(
			session
				.resolution_map()
				.lookup(Path::new("common/ideas/resolved.txt"), "missing", ""),
			Some(&ResolutionDecision::PreferMod("mod-a".to_string()))
		);
	}
}
