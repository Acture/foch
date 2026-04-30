use std::path::PathBuf;

use foch_core::config::{ResolutionDecision, ResolutionMap, compute_conflict_id};

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

pub struct LookupHandler<'a> {
	pub map: &'a ResolutionMap,
	pub current_file: PathBuf,
}

impl<'a> LookupHandler<'a> {
	pub fn new(map: &'a ResolutionMap, file: PathBuf) -> Self {
		Self {
			map,
			current_file: file,
		}
	}
}

impl<'a> ConflictHandler for LookupHandler<'a> {
	fn on_conflict(
		&mut self,
		_: &str,
		address: &PatchAddress,
		_: &PatchConflict,
	) -> ConflictDecision {
		let address_path = address.path.join("/");
		let conflict_id = compute_conflict_id(&self.current_file, &address_path, &address.key);
		match self.map.lookup(&self.current_file, &conflict_id) {
			Some(ResolutionDecision::PreferMod(mod_id)) => {
				ConflictDecision::PickMod(mod_id.clone())
			}
			Some(ResolutionDecision::UseFile(path)) => ConflictDecision::UseFile(path.clone()),
			Some(ResolutionDecision::KeepExisting) => ConflictDecision::KeepExisting,
			None => ConflictDecision::Defer,
		}
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

#[cfg(test)]
mod tests {
	use std::collections::HashMap;
	use std::path::PathBuf;

	use super::*;

	fn address() -> PatchAddress {
		PatchAddress {
			path: vec!["root".to_string(), "event".to_string()],
			key: "id".to_string(),
		}
	}

	fn conflict() -> PatchConflict {
		PatchConflict {
			patches: Vec::new(),
			reason: "test conflict".to_string(),
		}
	}

	#[test]
	fn lookup_handler_returns_pick_mod_when_resolution_map_has_entry() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let mut by_conflict_id = HashMap::new();
		by_conflict_id.insert(
			conflict_id,
			ResolutionDecision::PreferMod("mod-a".to_string()),
		);
		let map = ResolutionMap {
			by_conflict_id,
			..ResolutionMap::default()
		};
		let mut handler = LookupHandler::new(&map, current_file);

		let decision = handler.on_conflict("root/event/id", &address(), &conflict());

		assert_eq!(decision, ConflictDecision::PickMod("mod-a".to_string()));
	}

	#[test]
	fn lookup_handler_returns_defer_on_miss() {
		let map = ResolutionMap::default();
		let mut handler = LookupHandler::new(&map, PathBuf::from("events/PirateEvents.txt"));

		let decision = handler.on_conflict("root/event/id", &address(), &conflict());

		assert_eq!(decision, ConflictDecision::Defer);
	}

	#[test]
	fn lookup_handler_chained_with_defer_uses_resolution_then_defers() {
		let current_file = PathBuf::from("events/PirateEvents.txt");
		let conflict_id = compute_conflict_id(&current_file, "root/event", "id");
		let mut by_conflict_id = HashMap::new();
		by_conflict_id.insert(
			conflict_id,
			ResolutionDecision::PreferMod("mod-a".to_string()),
		);
		let map = ResolutionMap {
			by_conflict_id,
			..ResolutionMap::default()
		};
		let mut handler = ChainHandler {
			first: LookupHandler::new(&map, current_file),
			second: DeferHandler,
		};
		let miss = PatchAddress {
			path: vec!["root".to_string(), "event".to_string()],
			key: "other".to_string(),
		};

		let resolved = handler.on_conflict("root/event/id", &address(), &conflict());
		let deferred = handler.on_conflict("root/event/other", &miss, &conflict());

		assert_eq!(resolved, ConflictDecision::PickMod("mod-a".to_string()));
		assert_eq!(deferred, ConflictDecision::Defer);
	}
}
