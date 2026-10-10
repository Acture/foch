//! A deferred unit's conflicts and the decisions that would resolve them.
//!
//! Each unit with genuine leaf conflicts gets its address tree down to every
//! conflict, with the competing candidates rendered for review. Every leaf is
//! a decision point whose options carry the exact `foch.toml` resolution entry
//! that would persist them. Nothing here chooses a winner: every option is
//! explicit and maps onto a resolution the handler contract already audits.

use super::MergeUnitOutcome;
use crate::merge::conflict_view::ConflictView;
use crate::model::{
	ConflictKind, DeferredUnitReason, GamePath, GamePathBuf, MergeReportConflictResolution,
};
use crate::project::ResolutionEntry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnitConflicts {
	pub unit_id: String,
	/// The file conflict ids and resolutions are keyed by.
	pub file_path: GamePathBuf,
	pub reason: String,
	pub deferred_reason: DeferredUnitReason,
	/// The address tree down to each leaf conflict, each node listed after
	/// its parent.
	pub nodes: Vec<AddressNode>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AddressNode {
	/// `{unit_id}#{address}`.
	pub id: String,
	pub parent: Option<String>,
	pub segment: String,
	/// The canonical `path/key` leaf address resolutions match against.
	pub address: String,
	pub conflicts: Vec<ConflictLeaf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConflictLeaf {
	pub conflict_id: String,
	pub kind: Option<ConflictKind>,
	/// Contributing mods in ascending precedence; each is a review mod.
	pub contributors: Vec<ConflictContributor>,
	pub reason: Option<String>,
	/// The base game's text at this address, when it has one.
	pub vanilla_snippet: Option<String>,
	/// The competing versions, in the kernel's candidate order. Empty only
	/// when the candidates could not be rendered.
	pub candidates: Vec<ConflictCandidate>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConflictCandidate {
	pub mod_id: String,
	pub mod_display_name: String,
	pub precedence: usize,
	pub change_summary: Vec<String>,
	pub rendered: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConflictContributor {
	pub mod_id: String,
	pub precedence: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DecisionPoint {
	/// `conflict:{conflict_id}`.
	pub id: String,
	pub unit_id: String,
	pub node_id: String,
	pub conflict_id: String,
	pub file_path: GamePathBuf,
	pub address: String,
	pub options: Vec<DecisionOption>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DecisionOption {
	pub action: DecisionAction,
	/// The ways to persist this choice, narrowest first.
	pub scopes: Vec<DecisionScope>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionAction {
	PreferMod { mod_id: String },
	Handler { name: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DecisionScope {
	pub scope: DecisionScopeKind,
	/// The `[[resolutions]]` entry that persists the choice at this scope.
	pub resolution: ResolutionEntry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionScopeKind {
	/// Only this conflict, by its id.
	Conflict,
	/// Every conflict in this file.
	File,
	/// Every conflict in files of this file's directory, as a match rule.
	Directory,
}

pub(super) fn unit_conflicts(
	unit: &MergeUnitOutcome,
	resolution: &MergeReportConflictResolution,
	views: &[ConflictView],
) -> UnitConflicts {
	let mut nodes = Vec::<AddressNode>::new();
	let mut by_address = BTreeMap::<String, usize>::new();
	for leaf in &resolution.leaf_conflicts {
		let mut parent = None::<String>;
		let mut address = String::new();
		let segments = leaf
			.address_path
			.split('/')
			.filter(|segment| !segment.is_empty())
			.chain(std::iter::once(leaf.address_key.as_str()));
		for segment in segments {
			if !address.is_empty() {
				address.push('/');
			}
			address.push_str(segment);
			let index = *by_address.entry(address.clone()).or_insert_with(|| {
				nodes.push(AddressNode {
					id: format!("{}#{address}", unit.id),
					parent: parent.clone(),
					segment: segment.to_string(),
					address: address.clone(),
					conflicts: Vec::new(),
				});
				nodes.len() - 1
			});
			parent = Some(nodes[index].id.clone());
		}
		let index = by_address[&address];
		let mut contributors = leaf
			.contributors
			.iter()
			.map(|contributor| ConflictContributor {
				mod_id: contributor.mod_id.clone(),
				precedence: contributor.precedence,
			})
			.collect::<Vec<_>>();
		contributors.sort_by(|left, right| {
			left.precedence
				.cmp(&right.precedence)
				.then_with(|| left.mod_id.cmp(&right.mod_id))
		});
		let view = views
			.iter()
			.find(|view| view.conflict_id == leaf.conflict_id);
		nodes[index].conflicts.push(ConflictLeaf {
			conflict_id: leaf.conflict_id.clone(),
			kind: leaf.kind,
			contributors,
			reason: view.map(|view| view.reason.clone()),
			vanilla_snippet: view.and_then(|view| view.vanilla_snippet.clone()),
			candidates: view
				.into_iter()
				.flat_map(|view| &view.candidates)
				.map(|candidate| ConflictCandidate {
					mod_id: candidate.mod_id.clone(),
					mod_display_name: candidate.mod_display_name.clone(),
					precedence: candidate.precedence,
					change_summary: candidate.change_summary.clone(),
					rendered: candidate.candidate_rendered.clone(),
				})
				.collect(),
		});
	}
	UnitConflicts {
		unit_id: unit.id.clone(),
		file_path: resolution.path.clone(),
		reason: resolution.reason.clone(),
		deferred_reason: resolution.deferred_reason,
		nodes,
	}
}

pub(super) fn decision_point(
	unit: &MergeUnitOutcome,
	file: &GamePathBuf,
	node: &AddressNode,
	leaf: &ConflictLeaf,
) -> DecisionPoint {
	let mut mod_ids = Vec::<&str>::new();
	for contributor in &leaf.contributors {
		if !mod_ids.contains(&contributor.mod_id.as_str()) {
			mod_ids.push(&contributor.mod_id);
		}
	}
	let mut options = mod_ids
		.into_iter()
		.map(|mod_id| DecisionOption {
			action: DecisionAction::PreferMod {
				mod_id: mod_id.to_string(),
			},
			scopes: vec![
				DecisionScope {
					scope: DecisionScopeKind::Conflict,
					resolution: ResolutionEntry {
						conflict_id: Some(leaf.conflict_id.clone()),
						prefer_mod: Some(mod_id.to_string()),
						..empty_resolution()
					},
				},
				DecisionScope {
					scope: DecisionScopeKind::File,
					resolution: ResolutionEntry {
						file: Some(file.clone()),
						prefer_mod: Some(mod_id.to_string()),
						..empty_resolution()
					},
				},
				DecisionScope {
					scope: DecisionScopeKind::Directory,
					resolution: ResolutionEntry {
						r#match: Some(directory_rule(file)),
						prefer_mod: Some(mod_id.to_string()),
						..empty_resolution()
					},
				},
			],
		})
		.collect::<Vec<_>>();
	// Named handlers apply only to `match` rules.
	for handler in ["last_writer", "defer"] {
		options.push(DecisionOption {
			action: DecisionAction::Handler {
				name: handler.to_string(),
			},
			scopes: [
				(DecisionScopeKind::File, file_rule(file)),
				(DecisionScopeKind::Directory, directory_rule(file)),
			]
			.into_iter()
			.map(|(scope, rule)| DecisionScope {
				scope,
				resolution: ResolutionEntry {
					r#match: Some(rule),
					handler: Some(handler.to_string()),
					..empty_resolution()
				},
			})
			.collect(),
		});
	}
	DecisionPoint {
		id: format!("conflict:{}", leaf.conflict_id),
		unit_id: unit.id.clone(),
		node_id: node.id.clone(),
		conflict_id: leaf.conflict_id.clone(),
		file_path: file.clone(),
		address: node.address.clone(),
		options,
	}
}

/// A rule matching exactly this file, written as a regex so path characters
/// glob syntax would treat as wildcards stay literal.
fn file_rule(file: &GamePath) -> String {
	format!("re:^{}$", regex::escape(file.as_str()))
}

/// A rule matching every file directly in this file's directory.
fn directory_rule(file: &GamePath) -> String {
	let text = file.as_str();
	match text.rsplit_once('/') {
		Some((directory, _)) => format!("re:^{}/[^/]+$", regex::escape(directory)),
		None => "re:^[^/]+$".to_string(),
	}
}

fn empty_resolution() -> ResolutionEntry {
	ResolutionEntry {
		file: None,
		conflict_id: None,
		mod_id: None,
		r#match: None,
		prefer_mod: None,
		prefer_candidate: None,
		use_file: None,
		keep_existing: None,
		priority_boost: None,
		handler: None,
		policy: None,
	}
}

#[cfg(test)]
mod tests;
