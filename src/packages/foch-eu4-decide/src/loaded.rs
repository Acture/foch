//! Names in a database after its load function has returned.
//!
//! The loader has already merged every file into one object per key. A name
//! here is the string on that object. The files are not read.

use std::collections::BTreeMap;

use crate::residual::DecisionForward;

/// Engine databases named in the 1.37.5 load rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadedDatabase {
	/// `CIdeaDataBase`. The stored object's name is at `+0x158`.
	Ideas,
	/// `CRulerPersonalityDatabase`
	RulerPersonalities,
	/// `CLeaderPersonalityDatabase`. The stored object's name is at `+0x158`.
	LeaderPersonalities,
	/// `CGovernmentReformDatabase`. The stored object's name is at `+0x10`.
	GovernmentReforms,
}

/// What remains in the database after the loader returns.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadedSnapshot {
	ideas: BTreeMap<String, ()>,
	personalities: BTreeMap<String, ()>,
	leaders: BTreeMap<String, ()>,
	reforms: BTreeMap<String, ()>,
}

impl LoadedSnapshot {
	/// One name still stored on a database object. The same key replaces the previous object.
	pub fn note_definition(&mut self, database: LoadedDatabase, name: &str) {
		if name.is_empty() {
			return;
		}
		match database {
			LoadedDatabase::Ideas => {
				self.ideas.insert(name.to_string(), ());
			}
			LoadedDatabase::RulerPersonalities => {
				self.personalities.insert(name.to_string(), ());
			}
			LoadedDatabase::LeaderPersonalities => {
				self.leaders.insert(name.to_string(), ());
			}
			LoadedDatabase::GovernmentReforms => {
				self.reforms.insert(name.to_string(), ());
			}
		}
	}

	pub fn idea_groups(&self) -> impl Iterator<Item = &str> {
		self.ideas.keys().map(String::as_str)
	}

	pub fn personalities(&self) -> impl Iterator<Item = &str> {
		self.personalities.keys().map(String::as_str)
	}

	pub fn leader_personalities(&self) -> impl Iterator<Item = &str> {
		self.leaders.keys().map(String::as_str)
	}

	pub fn reform_names(&self) -> impl Iterator<Item = &str> {
		self.reforms.keys().map(String::as_str)
	}
}

/// Residual heads loaded beside the name databases.
///
/// `load` accepts the joint document or a terrain-only `residual-category`
/// export. No `cb` object means the casus-belli head stays unloaded.
#[derive(Clone, Debug, Default)]
pub struct ResidualHeads {
	forward: Option<DecisionForward>,
}

impl ResidualHeads {
	pub fn load(text: &str) -> Option<Self> {
		Some(Self {
			forward: Some(DecisionForward::from_json(text)?),
		})
	}

	pub fn forward(&self) -> Option<&DecisionForward> {
		self.forward.as_ref()
	}

	pub fn cb_loaded(&self) -> bool {
		self.forward
			.as_ref()
			.is_some_and(DecisionForward::cb_loaded)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_database_keeps_one_object_per_key() {
		let mut loaded = LoadedSnapshot::default();
		loaded.note_definition(LoadedDatabase::Ideas, "trade_ideas");
		loaded.note_definition(LoadedDatabase::Ideas, "FRA_ideas");
		loaded.note_definition(LoadedDatabase::Ideas, "trade_ideas");
		loaded.note_definition(
			LoadedDatabase::LeaderPersonalities,
			"glory_seeker_personality",
		);
		loaded.note_definition(LoadedDatabase::GovernmentReforms, "merchant_republic");
		assert_eq!(
			loaded.idea_groups().collect::<Vec<_>>(),
			vec!["FRA_ideas", "trade_ideas"]
		);
		assert_eq!(
			loaded.leader_personalities().collect::<Vec<_>>(),
			vec!["glory_seeker_personality"]
		);
		assert_eq!(
			loaded.reform_names().collect::<Vec<_>>(),
			vec!["merchant_republic"]
		);
	}

	#[test]
	fn a_terrain_export_loads_without_a_casus_belli_head() {
		let heads = ResidualHeads::load(
			r#"{
				"kind": "residual-category",
				"names": ["plains"],
				"modifiers": [[1.0, 0.0, 0.0, 4.0, 0.0]],
				"f0_weight": [[1, 0, 0, 0, 0], [0, 1, 0, 0, 0]],
				"f0_bias": [0, 0],
				"f2_weight": [[1, 0], [0, 1]],
				"f2_bias": [0, 0],
				"embedding": [[0, 0], [1, 0]]
			}"#,
		)
		.expect("terrain export");
		assert!(!heads.cb_loaded());
		assert!(ResidualHeads::load(r#"{"cb": {"kind": "nope"}}"#).is_none());
	}
}
