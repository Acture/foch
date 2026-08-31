//! High-level EU4 editor views backed by the concrete CWT schema.
//!
//! CWT's compiled rule graph stays private to `foch`. Consumers provide a
//! document and receive editor-oriented hover, completion, and diagnostic
//! DTOs instead of inspecting compiled rules themselves.

mod interpret;
mod workspace;

#[cfg(test)]
mod tests;

use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::game::schema::{CwtLoadStatus, CwtSchema, CwtSource};
use crate::model::{LocalisationDefinition, Severity};

pub use workspace::{SchemaDocument, SchemaWorkspace};

/// A zero-based source position suitable for editor protocols.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct EditorPosition {
	pub line: u32,
	pub character: u32,
}

/// A half-open zero-based source range.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct EditorRange {
	pub start: EditorPosition,
	pub end: EditorPosition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaHover {
	pub markdown: String,
	pub range: EditorRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaCompletionKind {
	Field,
	Function,
	EnumMember,
	Value,
	Reference,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaCompletion {
	pub label: String,
	pub insert_text: String,
	pub kind: SchemaCompletionKind,
	pub detail: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SchemaDiagnostic {
	pub range: EditorRange,
	pub severity: Option<Severity>,
	pub code: Option<String>,
	pub source: Option<String>,
	pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaLoadStatus {
	CacheHit,
	CompiledFromSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchemaLoadTimings {
	pub source_hash: Duration,
	pub cache_read: Option<Duration>,
	pub source_compile: Option<Duration>,
	pub total: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaInfo {
	pub root_count: usize,
	pub alias_count: usize,
	pub source_id: String,
	pub status: SchemaLoadStatus,
	pub cache_path: Option<PathBuf>,
	pub timings: SchemaLoadTimings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaLoadError {
	message: String,
}

impl Display for SchemaLoadError {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.message)
	}
}

impl std::error::Error for SchemaLoadError {}

/// Opaque handle to the concrete EU4 editor schema.
#[derive(Clone)]
pub struct EditorSchema {
	schema: Arc<CwtSchema>,
}

impl EditorSchema {
	/// Loads the discovered EU4 schema, if one is installed.
	pub fn active() -> Option<Self> {
		super::super::cwt::active_schema().map(|schema| Self { schema })
	}

	/// Loads an explicit EU4 CWT schema directory using the default cache.
	pub fn load_from_directory(root: &Path) -> Result<Self, SchemaLoadError> {
		let schema = CwtSchema::load(
			root,
			CwtSource::UserProvided {
				path: root.to_path_buf(),
			},
		)
		.map_err(|error| SchemaLoadError {
			message: error.to_string(),
		})?;
		Ok(Self {
			schema: Arc::new(schema),
		})
	}

	/// Loads an explicit schema with a caller-selected compiled-schema cache.
	pub fn load_from_directory_with_cache(
		root: &Path,
		cache_dir: Option<&Path>,
	) -> Result<Self, SchemaLoadError> {
		let schema = CwtSchema::load_with_cache(
			root,
			CwtSource::UserProvided {
				path: root.to_path_buf(),
			},
			cache_dir,
		)
		.map_err(|error| SchemaLoadError {
			message: error.to_string(),
		})?;
		Ok(Self {
			schema: Arc::new(schema),
		})
	}

	pub fn info(&self) -> SchemaInfo {
		let timings = self.schema.timings();
		SchemaInfo {
			root_count: self.facts().root_count(),
			alias_count: self.facts().alias_count(),
			source_id: self.schema.source_id().to_hex(),
			status: match self.schema.cache_status() {
				CwtLoadStatus::CacheHit => SchemaLoadStatus::CacheHit,
				CwtLoadStatus::CompiledFromSource => SchemaLoadStatus::CompiledFromSource,
			},
			cache_path: self.schema.cache_path().map(Path::to_path_buf),
			timings: SchemaLoadTimings {
				source_hash: timings.source_hash,
				cache_read: timings.cache_read,
				source_compile: timings.source_compile,
				total: timings.total,
			},
		}
	}

	pub fn workspace(&self, documents: &[SchemaDocument<'_>]) -> SchemaWorkspace {
		workspace::build(self.facts(), documents)
	}

	pub fn hover(
		&self,
		file_path: &Path,
		text: &str,
		position: EditorPosition,
		workspace: Option<&SchemaWorkspace>,
	) -> Option<SchemaHover> {
		interpret::schema_hover(self.facts(), file_path, text, position, workspace)
	}

	pub fn completions(
		&self,
		file_path: &Path,
		text: &str,
		position: EditorPosition,
		prefix_lower: &str,
		workspace: Option<&SchemaWorkspace>,
	) -> Option<Vec<SchemaCompletion>> {
		match workspace {
			Some(workspace) => interpret::schema_completion_candidates_with_index(
				self.facts(),
				file_path,
				text,
				position,
				prefix_lower,
				Some(workspace),
			),
			None => interpret::schema_completion_candidates(
				self.facts(),
				file_path,
				text,
				position,
				prefix_lower,
			),
		}
	}

	pub fn diagnostics(
		&self,
		file_path: &Path,
		text: &str,
		workspace: Option<&SchemaWorkspace>,
	) -> Vec<SchemaDiagnostic> {
		match workspace {
			Some(workspace) => interpret::schema_diagnostics_for_text_with_index(
				self.facts(),
				file_path,
				text,
				Some(workspace),
			),
			None => interpret::schema_diagnostics_for_text(self.facts(), file_path, text),
		}
	}

	pub fn localisation_diagnostics(
		&self,
		file_path: &Path,
		text: &str,
		definitions: &[LocalisationDefinition],
	) -> Vec<SchemaDiagnostic> {
		interpret::schema_localisation_diagnostics_for_text(
			self.facts(),
			file_path,
			text,
			definitions,
		)
	}

	fn facts(&self) -> &crate::game::schema::CwtQuery {
		self.schema.facts()
	}
}
