use crate::model::{Finding, GamePath, GamePathBuf, SemanticIndex, SymbolDefinition, SymbolKind};
use crate::project::{ConfigError, Project, ResolutionMap};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;
use std::path::{Path, PathBuf};

/// The physical file behind each file of a workspace, by the mod that loads
/// it and its game path.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceFiles(HashMap<String, HashMap<GamePathBuf, PathBuf>>);

/// Two physical files claimed one (mod, game path).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceFileConflict {
	pub mod_id: String,
	pub path: GamePathBuf,
	pub existing: PathBuf,
	pub rejected: PathBuf,
}

impl fmt::Display for WorkspaceFileConflict {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"{} of {} is both {} and {}",
			self.path,
			self.mod_id,
			self.existing.display(),
			self.rejected.display()
		)
	}
}

impl std::error::Error for WorkspaceFileConflict {}

impl WorkspaceFiles {
	/// Records the physical file of `path` in `mod_id`. Recording the same
	/// file again is a no-op; a different file for the same key is an error,
	/// never a silent replacement.
	pub fn insert(
		&mut self,
		mod_id: &str,
		path: GamePathBuf,
		physical: PathBuf,
	) -> Result<(), WorkspaceFileConflict> {
		let files = self.0.entry(mod_id.to_string()).or_default();
		match files.entry(path) {
			Entry::Vacant(entry) => {
				entry.insert(physical);
				Ok(())
			}
			Entry::Occupied(entry) if *entry.get() == physical => Ok(()),
			Entry::Occupied(entry) => Err(WorkspaceFileConflict {
				mod_id: mod_id.to_string(),
				path: entry.key().clone(),
				existing: entry.get().clone(),
				rejected: physical,
			}),
		}
	}

	pub fn get(&self, mod_id: &str, path: &GamePath) -> Option<&Path> {
		self.0.get(mod_id)?.get(path).map(PathBuf::as_path)
	}
}

/// A workspace session holding analysis results ready for consumption by CLI or LSP.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceSession {
	pub index: SemanticIndex,
	pub file_paths: Vec<PathBuf>,
	pub files: WorkspaceFiles,
	pub findings: Vec<Finding>,
	resolution_map: ResolutionMap,
}

impl WorkspaceSession {
	/// Build a session from parsed analysis results. `files` resolves each
	/// indexed (mod, game path) to the file it was read from; the caller
	/// builds it, so both playlist-based engine resolution and the LSP's own
	/// file discovery can feed this type.
	pub fn from_analysis(
		index: SemanticIndex,
		file_paths: Vec<PathBuf>,
		files: WorkspaceFiles,
		findings: Vec<Finding>,
	) -> Self {
		Self::from_analysis_with_resolution_map(
			index,
			file_paths,
			files,
			findings,
			ResolutionMap::default(),
		)
	}

	pub fn from_analysis_with_config(
		index: SemanticIndex,
		file_paths: Vec<PathBuf>,
		files: WorkspaceFiles,
		findings: Vec<Finding>,
		config: &Project,
	) -> Result<Self, ConfigError> {
		let resolution_map = ResolutionMap::from_entries(&config.resolutions)?;
		Ok(Self::from_analysis_with_resolution_map(
			index,
			file_paths,
			files,
			findings,
			resolution_map,
		))
	}

	pub fn from_analysis_with_resolution_map(
		index: SemanticIndex,
		file_paths: Vec<PathBuf>,
		files: WorkspaceFiles,
		findings: Vec<Finding>,
		resolution_map: ResolutionMap,
	) -> Self {
		Self {
			index,
			file_paths,
			files,
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

	/// The physical file of `path` as loaded by `mod_id`.
	pub fn resolve_path(&self, mod_id: &str, path: &GamePath) -> Option<&Path> {
		self.files.get(mod_id, path)
	}
}

#[cfg(test)]
mod tests {
	use std::path::{Path, PathBuf};

	use crate::model::{GamePath, GamePathBuf, SemanticIndex};
	use crate::project::{Project, ResolutionDecision};

	use super::{WorkspaceFileConflict, WorkspaceFiles, WorkspaceSession};

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("valid game path")
	}

	#[test]
	fn files_are_resolved_by_mod_and_game_path() {
		let mut files = WorkspaceFiles::default();
		files
			.insert(
				"mod:a",
				game_path("events/a.txt"),
				PathBuf::from("/a/events/a.txt"),
			)
			.expect("first file");
		files
			.insert(
				"mod:b",
				game_path("events/a.txt"),
				PathBuf::from("/b/events/a.txt"),
			)
			.expect("same game path in another mod");
		let session = WorkspaceSession::from_analysis(
			SemanticIndex::default(),
			Vec::new(),
			files,
			Vec::new(),
		);
		let path = GamePath::new("events/a.txt").expect("valid game path");
		assert_eq!(
			session.resolve_path("mod:a", path),
			Some(Path::new("/a/events/a.txt"))
		);
		assert_eq!(
			session.resolve_path("mod:b", path),
			Some(Path::new("/b/events/a.txt"))
		);
		assert_eq!(session.resolve_path("mod:c", path), None);
		assert_eq!(
			session.resolve_path("mod:a", GamePath::new("events/b.txt").expect("valid")),
			None
		);
	}

	#[test]
	fn a_second_physical_file_for_one_game_path_is_rejected() {
		let mut files = WorkspaceFiles::default();
		let path = game_path("localisation/a_l_english.yml");
		files
			.insert(
				"mod:a",
				path.clone(),
				PathBuf::from("/a/localisation/a_l_english.yml"),
			)
			.expect("first file");
		files
			.insert(
				"mod:a",
				path.clone(),
				PathBuf::from("/a/localisation/a_l_english.yml"),
			)
			.expect("the same file again");
		assert_eq!(
			files.insert("mod:a", path.clone(), PathBuf::from("/elsewhere.yml")),
			Err(WorkspaceFileConflict {
				mod_id: "mod:a".to_string(),
				path: path.clone(),
				existing: PathBuf::from("/a/localisation/a_l_english.yml"),
				rejected: PathBuf::from("/elsewhere.yml"),
			})
		);
		assert_eq!(
			files.get("mod:a", &path),
			Some(Path::new("/a/localisation/a_l_english.yml"))
		);
	}

	#[test]
	fn workspace_session_loads_foch_toml_resolutions() {
		let config = Project::from_toml_str(
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
			WorkspaceFiles::default(),
			Vec::new(),
			&config,
		)
		.expect("build session");

		assert_eq!(
			session.resolution_map().lookup(
				crate::model::GamePath::new("common/ideas/resolved.txt").expect("valid game path"),
				"missing",
				""
			),
			Some(&ResolutionDecision::PreferMod("mod-a".to_string()))
		);
	}
}
