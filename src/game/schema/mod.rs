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
use std::time::Instant;

pub(crate) use cache::{CwtLoad, CwtLoadStatus, CwtLoadTimings};
pub(crate) use query::CwtQuery;
pub(crate) use source::CwtSchemaId;

use error::CwtLoadError;
use query::CompiledRulePack;

pub(crate) type CwtFacts = CwtQuery;

pub(crate) struct CwtSchema {
	facts: Arc<CwtFacts>,
	source_id: CwtSchemaId,
	cache_status: CwtLoadStatus,
	cache_path: Option<PathBuf>,
	timings: CwtLoadTimings,
}

impl CwtSchema {
	/// Decodes a rule pack compiled ahead of time, such as the one a build
	/// script embeds. The pack must carry the source identity it was compiled
	/// from, because that identity is what caches and reports key on.
	pub(crate) fn from_compiled_bytes(bytes: &[u8]) -> Result<Self, CwtLoadError> {
		let started = Instant::now();
		let pack = CompiledRulePack::from_bytes(bytes)?;
		let source_id = pack
			.source_id
			.as_deref()
			.and_then(CwtSchemaId::from_hex)
			.ok_or_else(|| CwtLoadError::Codec {
				message: "compiled pack carries no valid source id".to_string(),
			})?;
		Ok(Self {
			facts: Arc::new(CwtQuery::new(pack)),
			source_id,
			cache_status: CwtLoadStatus::Embedded,
			cache_path: None,
			timings: CwtLoadTimings {
				source_hash: None,
				cache_read: None,
				source_compile: None,
				total: started.elapsed(),
			},
		})
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
		cache_dir: Option<&Path>,
	) -> Result<Self, CwtLoadError> {
		let loaded: CwtLoad = cache::load_cwt_from_dir(root, cache_dir)?;
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
