use std::path::PathBuf;

use crate::merge::patch_merge::{PatchAddress, PatchConflict};

pub trait ConflictHandler {
	fn on_conflict(
		&mut self,
		path: &str,
		address: &PatchAddress,
		conflict: &PatchConflict,
	) -> ConflictDecision;
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ConflictDecision {
	/// Pick this mod's patch only; drop the others.
	PickMod(String),
	/// Use this external file's content (handled at materialize time).
	UseFile(PathBuf),
	/// Keep whatever already exists at output dir (handled at materialize time).
	KeepExisting,
	/// Defer — log to report, leave for later resolution.
	Defer,
	/// Abort the merge.
	Abort,
}

/// Default handler: always defer, reproducing the current behavior.
pub struct DeferHandler;

impl ConflictHandler for DeferHandler {
	fn on_conflict(&mut self, _: &str, _: &PatchAddress, _: &PatchConflict) -> ConflictDecision {
		ConflictDecision::Defer
	}
}

/// Chain combinator: returns the second handler's decision when the first defers.
pub struct ChainHandler<H1: ConflictHandler, H2: ConflictHandler> {
	pub first: H1,
	pub second: H2,
}

impl<H1: ConflictHandler, H2: ConflictHandler> ConflictHandler for ChainHandler<H1, H2> {
	fn on_conflict(
		&mut self,
		path: &str,
		address: &PatchAddress,
		conflict: &PatchConflict,
	) -> ConflictDecision {
		match self.first.on_conflict(path, address, conflict) {
			ConflictDecision::Defer => self.second.on_conflict(path, address, conflict),
			other => other,
		}
	}
}
