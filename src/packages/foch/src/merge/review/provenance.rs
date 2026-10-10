//! Who contributes to a review: the playset's mods and the declared
//! dependency edges that order their contributions.

use crate::merge::dag::{DagDiagnostic, DagDiagnosticKind, ModDag};
use crate::model::ModCandidate;
use crate::project::AppliedDepOverride;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeReviewMod {
	pub mod_id: String,
	pub name: String,
	/// Playset position; `None` for the base game.
	pub position: Option<usize>,
	pub is_base_game: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeReviewDependency {
	pub child: String,
	pub parent: String,
	pub status: DependencyStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyStatus {
	/// The edge orders the child's contributions after the parent's.
	Active,
	/// A local dependency override drops the edge from every file DAG.
	Overridden,
	/// The edge closed a dependency cycle and was removed by playset order.
	BrokenCycle,
}

/// The playset's mods and dependency edges, captured where the mod DAG is
/// built so the review does not need to keep the resolved input.
#[derive(Clone, Debug, Default)]
pub(crate) struct PlaysetProvenance {
	pub(super) mods: Vec<MergeReviewMod>,
	pub(super) dependencies: Vec<MergeReviewDependency>,
}

impl PlaysetProvenance {
	pub(crate) fn new(
		mods: &[ModCandidate],
		mod_display_names: &HashMap<String, String>,
		mod_dag: &ModDag,
		diagnostics: &[DagDiagnostic],
		dep_overrides: &[AppliedDepOverride],
	) -> Self {
		let mut review_mods = mods
			.iter()
			.map(|candidate| MergeReviewMod {
				mod_id: candidate.mod_id.clone(),
				name: mod_display_names
					.get(&candidate.mod_id)
					.cloned()
					.unwrap_or_else(|| candidate.mod_id.clone()),
				position: mod_dag.position(&candidate.mod_id.as_str().into()),
				is_base_game: false,
			})
			.collect::<Vec<_>>();
		review_mods.sort_by_key(|node| node.position);
		let overridden = dep_overrides
			.iter()
			.map(|dep_override| (dep_override.mod_id.as_str(), dep_override.dep_id.as_str()))
			.collect::<BTreeSet<_>>();
		let mut dependencies = Vec::new();
		for node in &review_mods {
			for parent in mod_dag.parents_of(&node.mod_id.as_str().into()) {
				let status = if overridden.contains(&(node.mod_id.as_str(), parent.as_str())) {
					DependencyStatus::Overridden
				} else {
					DependencyStatus::Active
				};
				dependencies.push(MergeReviewDependency {
					child: node.mod_id.clone(),
					parent: parent.as_str().to_string(),
					status,
				});
			}
		}
		for diagnostic in diagnostics {
			if let DagDiagnosticKind::BrokenCycleEdge { child, parent } = &diagnostic.kind {
				dependencies.push(MergeReviewDependency {
					child: child.as_str().to_string(),
					parent: parent.as_str().to_string(),
					status: DependencyStatus::BrokenCycle,
				});
			}
		}
		Self {
			mods: review_mods,
			dependencies,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::merge::dag::build_mod_dag;
	use crate::playset::PlaysetEntry;
	use crate::playset::descriptor::ModDescriptor;
	use crate::project::DepOverride;

	fn candidate(mod_id: &str, name: &str, dependencies: &[&str]) -> ModCandidate {
		ModCandidate {
			entry: PlaysetEntry {
				steam_id: Some(mod_id.to_string()),
				..PlaysetEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: name.to_string(),
				dependencies: dependencies.iter().map(|dep| dep.to_string()).collect(),
				..ModDescriptor::default()
			}),
			workshop_identity: None,
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	#[test]
	fn mods_keep_playset_order_and_each_dependency_edge_has_a_state() {
		let mods = [
			candidate("a", "Mod A", &[]),
			candidate("b", "Mod B", &["Mod A"]),
			candidate("c", "Mod C", &["Mod A", "Mod B"]),
			candidate("d", "Mod D", &["Mod E"]),
			candidate("e", "Mod E", &["Mod D"]),
		];
		let (dag, diagnostics) = build_mod_dag(&mods);
		let names = HashMap::from([("b".to_string(), "Mod B".to_string())]);
		let overrides = [AppliedDepOverride::config(&DepOverride::new("c", "a"))];
		let provenance = PlaysetProvenance::new(&mods, &names, &dag, &diagnostics, &overrides);

		let ids = provenance
			.mods
			.iter()
			.map(|node| (node.mod_id.as_str(), node.position))
			.collect::<Vec<_>>();
		assert_eq!(
			ids,
			[
				("a", Some(0)),
				("b", Some(1)),
				("c", Some(2)),
				("d", Some(3)),
				("e", Some(4))
			]
		);
		assert_eq!(provenance.mods[0].name, "a");
		assert_eq!(provenance.mods[1].name, "Mod B");
		let edges = provenance
			.dependencies
			.iter()
			.map(|edge| (edge.child.as_str(), edge.parent.as_str(), edge.status))
			.collect::<Vec<_>>();
		assert_eq!(
			edges,
			[
				("b", "a", DependencyStatus::Active),
				("c", "a", DependencyStatus::Overridden),
				("c", "b", DependencyStatus::Active),
				("d", "e", DependencyStatus::Active),
				("e", "d", DependencyStatus::BrokenCycle),
			]
		);
	}
}
