//! Shared CWT schema loading and querying.
//!
//! Game-specific discovery and scope interpretation belong to concrete game
//! modules. This module only parses, compiles, caches, and queries CWT facts.

#![allow(dead_code)]

pub(crate) mod cache;
pub(crate) mod compile;
pub(crate) mod error;
pub(crate) mod query;
pub(crate) mod source;
pub(crate) mod syntax;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) use cache::{CwtLoad, CwtLoadStatus, CwtLoadTimings};
pub(crate) use query::CwtQuery;
pub(crate) use source::{CwtSchemaId, CwtSource};

use error::CwtLoadError;

pub(crate) type CwtFacts = CwtQuery;

pub(crate) struct CwtSchema {
	facts: Arc<CwtFacts>,
	source_id: CwtSchemaId,
	cache_status: CwtLoadStatus,
	cache_path: Option<PathBuf>,
	timings: CwtLoadTimings,
}

impl CwtSchema {
	pub(crate) fn load(root: &Path, source: CwtSource) -> Result<Self, CwtLoadError> {
		let cache_dir = cache::default_cwt_cache_dir();
		Self::load_with_cache(root, source, Some(&cache_dir))
	}

	pub(crate) fn facts(&self) -> &CwtFacts {
		self.facts.as_ref()
	}

	pub(crate) fn source_id(&self) -> &CwtSchemaId {
		&self.source_id
	}

	pub(crate) fn cache_status(&self) -> CwtLoadStatus {
		self.cache_status
	}

	pub(crate) fn cache_path(&self) -> Option<&Path> {
		self.cache_path.as_deref()
	}

	pub(crate) fn timings(&self) -> CwtLoadTimings {
		self.timings
	}

	pub(crate) fn load_with_cache(
		root: &Path,
		source: CwtSource,
		cache_dir: Option<&Path>,
	) -> Result<Self, CwtLoadError> {
		let loaded: CwtLoad = cache::load_cwt_from_dir(root, source, cache_dir)?;
		Ok(Self {
			facts: loaded.facts,
			source_id: loaded.source_id,
			cache_status: loaded.status,
			cache_path: loaded.cache_path,
			timings: loaded.timings,
		})
	}
}

#[cfg(test)]
mod tests;
