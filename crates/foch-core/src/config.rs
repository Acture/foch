use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FochConfig {
	#[serde(default)]
	pub overrides: Vec<DepOverride>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DepOverride {
	#[serde(rename = "mod", alias = "mod_id")]
	pub mod_id: String,
	#[serde(rename = "dep", alias = "dep_id")]
	pub dep_id: String,
	#[serde(default)]
	pub note: Option<String>,
}

impl DepOverride {
	pub fn new(mod_id: impl Into<String>, dep_id: impl Into<String>) -> Self {
		Self {
			mod_id: mod_id.into(),
			dep_id: dep_id.into(),
			note: None,
		}
	}

	pub fn matches(&self, mod_id: &str, dep_id: &str) -> bool {
		self.mod_id == mod_id && self.dep_id == dep_id
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DepOverrideSource {
	Config,
	Cli,
}

impl fmt::Display for DepOverrideSource {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Config => f.write_str("config"),
			Self::Cli => f.write_str("cli"),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct AppliedDepOverride {
	pub mod_id: String,
	pub dep_id: String,
	pub source: DepOverrideSource,
}

impl AppliedDepOverride {
	pub fn config(dep_override: &DepOverride) -> Self {
		Self {
			mod_id: dep_override.mod_id.clone(),
			dep_id: dep_override.dep_id.clone(),
			source: DepOverrideSource::Config,
		}
	}

	pub fn cli(mod_id: impl Into<String>, dep_id: impl Into<String>) -> Self {
		Self {
			mod_id: mod_id.into(),
			dep_id: dep_id.into(),
			source: DepOverrideSource::Cli,
		}
	}
}

impl From<&AppliedDepOverride> for DepOverride {
	fn from(value: &AppliedDepOverride) -> Self {
		Self::new(value.mod_id.clone(), value.dep_id.clone())
	}
}

#[derive(Debug)]
pub enum FochConfigLoadError {
	Io {
		path: PathBuf,
		source: std::io::Error,
	},
	Parse {
		path: PathBuf,
		source: toml::de::Error,
	},
}

impl fmt::Display for FochConfigLoadError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io { path, source } => {
				write!(f, "failed to read foch config {}: {source}", path.display())
			}
			Self::Parse { path, source } => {
				write!(
					f,
					"failed to parse foch config {}: {source}",
					path.display()
				)
			}
		}
	}
}

impl Error for FochConfigLoadError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Io { source, .. } => Some(source),
			Self::Parse { source, .. } => Some(source),
		}
	}
}

impl FochConfig {
	pub fn from_toml_str(content: &str) -> Result<Self, toml::de::Error> {
		toml::from_str(content)
	}

	pub fn load(playset_root: &Path) -> Self {
		Self::try_load(playset_root).unwrap_or_default()
	}

	pub fn try_load(playset_root: &Path) -> Result<Self, FochConfigLoadError> {
		Self::try_load_from_paths(Self::search_paths(playset_root))
	}

	pub fn load_from_path(path: &Path) -> Result<Self, FochConfigLoadError> {
		Self::load_file(path)
	}

	fn try_load_from_paths(paths: Vec<PathBuf>) -> Result<Self, FochConfigLoadError> {
		let mut merged = Self::default();
		for path in paths {
			if path.is_file() {
				let config = Self::load_file(&path)?;
				merged.overrides.extend(config.overrides);
			}
		}
		Ok(merged)
	}

	fn load_file(path: &Path) -> Result<Self, FochConfigLoadError> {
		let content = fs::read_to_string(path).map_err(|source| FochConfigLoadError::Io {
			path: path.to_path_buf(),
			source,
		})?;
		if content.trim().is_empty() {
			return Ok(Self::default());
		}
		Self::from_toml_str(&content).map_err(|source| FochConfigLoadError::Parse {
			path: path.to_path_buf(),
			source,
		})
	}

	fn search_paths(playset_root: &Path) -> Vec<PathBuf> {
		let mut paths = Vec::new();
		let mut seen = HashSet::new();
		if let Ok(cwd) = std::env::current_dir() {
			push_unique_path(&mut paths, &mut seen, cwd.join("foch.toml"));
		}
		push_unique_path(&mut paths, &mut seen, playset_root.join("foch.toml"));
		if let Some(home) = dirs::home_dir() {
			push_unique_path(
				&mut paths,
				&mut seen,
				home.join(".config").join("foch").join("config.toml"),
			);
		}
		paths
	}
}

fn push_unique_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
	if seen.insert(path.clone()) {
		paths.push(path);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn parses_valid_toml_with_multiple_overrides() {
		let config = FochConfig::from_toml_str(
			r#"
[[overrides]]
mod = "3378403419"
dep = "1999055990"
note = "not a git parent"

[[overrides]]
mod = "abc"
dep = "def"
"#,
		)
		.expect("parse config");

		assert_eq!(config.overrides.len(), 2);
		assert_eq!(config.overrides[0].mod_id, "3378403419");
		assert_eq!(config.overrides[0].dep_id, "1999055990");
		assert_eq!(
			config.overrides[0].note.as_deref(),
			Some("not a git parent")
		);
		assert_eq!(config.overrides[1], DepOverride::new("abc", "def"));
	}

	#[test]
	fn rejects_override_missing_dep_field() {
		let err = FochConfig::from_toml_str(
			r#"
[[overrides]]
mod = "3378403419"
"#,
		)
		.expect_err("missing dep must be invalid");

		assert!(err.to_string().contains("dep"), "error was: {err}");
	}

	#[test]
	fn load_merges_multiple_existing_sources() {
		let temp = TempDir::new().expect("temp dir");
		let first = temp.path().join("first.toml");
		let second = temp.path().join("second.toml");
		fs::write(&first, "[[overrides]]\nmod = \"a\"\ndep = \"b\"\n").expect("write first");
		fs::write(&second, "[[overrides]]\nmod = \"c\"\ndep = \"d\"\n").expect("write second");

		let config = FochConfig::try_load_from_paths(vec![first, second]).expect("load configs");

		assert_eq!(
			config.overrides,
			vec![DepOverride::new("a", "b"), DepOverride::new("c", "d")]
		);
	}
}
