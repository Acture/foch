use super::*;
use crate::merge::dag::build_mod_dag;
use crate::merge::review::{MergeDisposition, MergeReview, PlaysetProvenance, UnitOutcomeLedger};
use crate::model::{
	HandlerResolutionRecord, LeafConflictDetail, MergePlanContributor, MergePlanEntry,
	MergePlanResult, MergePlanStrategy, MergePlanTarget, MergeReport,
	MergeReportConflictContributor, ModCandidate,
};
use crate::playset::PlaysetEntry;
use crate::playset::descriptor::ModDescriptor;
use crate::project::{ResolutionDecision, ResolutionEntry, ResolutionMap, compute_conflict_id};
use std::collections::{BTreeSet, HashMap};

fn game_path(text: &str) -> GamePathBuf {
	GamePathBuf::parse(text).expect("valid game path")
}

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

fn contributor(mod_id: &str, precedence: usize, is_base_game: bool) -> MergePlanContributor {
	MergePlanContributor {
		mod_id: mod_id.to_string(),
		source_path: "events/a.txt".to_string(),
		precedence,
		is_base_game,
	}
}

fn file_entry(path: &str, contributors: Vec<MergePlanContributor>) -> MergePlanEntry {
	MergePlanEntry {
		target: MergePlanTarget::File {
			path: game_path(path),
		},
		strategy: MergePlanStrategy::StructuralMerge,
		contributors,
		winner: None,
		notes: Vec::new(),
	}
}

fn leaf(file: &str, address_path: &str, key: &str) -> LeafConflictDetail {
	LeafConflictDetail {
		address_path: address_path.to_string(),
		address_key: key.to_string(),
		conflict_id: compute_conflict_id(&game_path(file), address_path, key),
		kind: None,
		contributors: vec![
			MergeReportConflictContributor {
				mod_id: "b".to_string(),
				mod_version: "1".to_string(),
				precedence: 2,
			},
			MergeReportConflictContributor {
				mod_id: "a".to_string(),
				mod_version: "1".to_string(),
				precedence: 1,
			},
		],
	}
}

fn candidate_view(file: &str, address_path: &str, key: &str) -> ConflictView {
	use crate::merge::conflict_view::CandidateView;
	ConflictView {
		file_path: game_path(file),
		address_path: address_path.split('/').map(str::to_string).collect(),
		address_key: key.to_string(),
		conflict_id: compute_conflict_id(&game_path(file), address_path, key),
		reason: "conflicting scalar values".to_string(),
		vanilla_snippet: Some(format!("{key} = base")),
		candidates: ["a", "b"]
			.into_iter()
			.enumerate()
			.map(|(index, mod_id)| CandidateView {
				mod_id: mod_id.to_string(),
				mod_display_name: format!("Mod {}", mod_id.to_uppercase()),
				precedence: index + 1,
				change_summary: vec![format!("set {key}")],
				candidate_rendered: format!("{key} = {mod_id}"),
			})
			.collect(),
	}
}

/// One deferred unit with two conflicts under a shared definition, one of them
/// with rendered candidates, and one safe unit with a configured handler
/// decision.
fn conflicted_review() -> MergeReview {
	let plan = MergePlanResult {
		paths: vec![
			file_entry(
				"events/a.txt",
				vec![
					contributor("base:eu4", 0, true),
					contributor("a", 1, false),
					contributor("b", 2, false),
				],
			),
			file_entry("events/b.txt", vec![contributor("a", 1, false)]),
		],
		..Default::default()
	};
	let mut ledger = UnitOutcomeLedger::from_plan(&plan).unwrap();
	ledger
		.resolve(
			&plan.paths[0],
			MergeDisposition::NeedsUserChoice,
			"conflicts",
			None,
			[],
		)
		.unwrap();
	ledger
		.attach_conflict_views(
			&plan.paths[0],
			vec![candidate_view("events/a.txt", "flavor.1/option", "name")],
		)
		.unwrap();
	ledger
		.resolve(
			&plan.paths[1],
			MergeDisposition::Safe,
			"merged",
			Some(game_path("events/b.txt")),
			[],
		)
		.unwrap();
	let report = MergeReport {
		conflict_resolutions: vec![MergeReportConflictResolution {
			path: game_path("events/a.txt"),
			reason: "structural merge has 2 unresolved conflict(s)".to_string(),
			deferred_reason: DeferredUnitReason::NeedsUserChoice,
			kind: None,
			leaf_conflicts: vec![
				leaf("events/a.txt", "flavor.1/option", "name"),
				leaf("events/a.txt", "flavor.1/option", "ai_chance"),
			],
		}],
		handler_resolutions: vec![HandlerResolutionRecord {
			path: game_path("events/b.txt"),
			action: "last_writer".to_string(),
			source: Some("a".to_string()),
			rationale: None,
		}],
		..MergeReport::default()
	};
	let mods = [
		candidate("a", "Mod A", &[]),
		candidate("b", "Mod B", &["Mod A"]),
	];
	let (dag, diagnostics) = build_mod_dag(&mods);
	let names = HashMap::from([
		("a".to_string(), "Mod A".to_string()),
		("b".to_string(), "Mod B".to_string()),
	]);
	let playset = PlaysetProvenance::new(&mods, &names, &dag, &diagnostics, &[]);
	ledger.finish(&names, &report, playset).unwrap()
}

#[test]
fn units_link_to_contributors_conflicts_and_recorded_handler_decisions() {
	let review = conflicted_review();

	let base = review.mods().last().unwrap();
	assert!(base.is_base_game);
	assert_eq!(base.mod_id, "base:eu4");
	assert_eq!(base.position, None);
	assert_eq!(review.dependencies().len(), 1);

	let [conflicted, safe] = review.units() else {
		panic!("two units");
	};
	assert_eq!(conflicted.id, "file:events/a.txt");
	assert_eq!(
		conflicted
			.contributors
			.iter()
			.map(|contributor| contributor.mod_id.as_str())
			.collect::<Vec<_>>(),
		["base:eu4", "a", "b"]
	);
	assert_eq!(conflicted.conflict_ids.len(), 2);
	assert!(conflicted.handler_resolutions.is_empty());
	assert!(safe.conflict_ids.is_empty());
	assert_eq!(safe.handler_resolutions[0].action, "last_writer");

	let conflict_mods = review
		.conflicts()
		.iter()
		.flat_map(|unit| &unit.nodes)
		.flat_map(|node| &node.conflicts)
		.flat_map(|leaf| &leaf.contributors)
		.map(|contributor| contributor.mod_id.as_str())
		.collect::<BTreeSet<_>>();
	for mod_id in conflict_mods {
		assert!(
			review.mods().iter().any(|node| node.mod_id == mod_id),
			"conflict contributor `{mod_id}` is a review mod"
		);
	}
}

#[test]
fn conflicts_are_an_address_tree_sharing_parents_with_candidates_at_leaves() {
	let review = conflicted_review();
	let [unit] = review.conflicts() else {
		panic!("one conflicted unit");
	};
	assert_eq!(unit.unit_id, "file:events/a.txt");
	let nodes = unit
		.nodes
		.iter()
		.map(|node| {
			(
				node.address.as_str(),
				node.parent.as_deref(),
				node.conflicts.len(),
			)
		})
		.collect::<Vec<_>>();
	assert_eq!(
		nodes,
		[
			("flavor.1", None, 0),
			("flavor.1/option", Some("file:events/a.txt#flavor.1"), 0),
			(
				"flavor.1/option/name",
				Some("file:events/a.txt#flavor.1/option"),
				1
			),
			(
				"flavor.1/option/ai_chance",
				Some("file:events/a.txt#flavor.1/option"),
				1
			),
		]
	);
	let name = &unit.nodes[2].conflicts[0];
	assert_eq!(
		name.contributors
			.iter()
			.map(|contributor| (contributor.mod_id.as_str(), contributor.precedence))
			.collect::<Vec<_>>(),
		[("a", 1), ("b", 2)]
	);
	assert_eq!(name.vanilla_snippet.as_deref(), Some("name = base"));
	assert_eq!(
		name.candidates
			.iter()
			.map(|candidate| (
				candidate.mod_display_name.as_str(),
				candidate.rendered.as_str()
			))
			.collect::<Vec<_>>(),
		[("Mod A", "name = a"), ("Mod B", "name = b")]
	);
	let ai_chance = &unit.nodes[3].conflicts[0];
	assert!(ai_chance.candidates.is_empty());
	assert_eq!(ai_chance.reason, None);
}

/// A conflict-scoped choice is a decision record for exactly that conflict.
/// Every file or directory scope is a valid `[[resolutions]]` entry whose
/// resolution map yields that option's decision for the conflict.
#[test]
fn every_decision_scope_persists_as_a_resolution_that_selects_its_conflict() {
	let review = conflicted_review();
	assert_eq!(review.decisions().len(), 2);
	for point in review.decisions() {
		assert_eq!(point.id, format!("conflict:{}", point.conflict_id));
		assert!(point.node_id.ends_with(&point.address));
		let actions = point
			.options
			.iter()
			.map(|option| option.action.clone())
			.collect::<Vec<_>>();
		assert_eq!(
			actions,
			[
				DecisionAction::PreferMod {
					mod_id: "a".to_string()
				},
				DecisionAction::PreferMod {
					mod_id: "b".to_string()
				},
				DecisionAction::Handler {
					name: "last_writer".to_string()
				},
				DecisionAction::Handler {
					name: "defer".to_string()
				},
			]
		);
		for option in &point.options {
			let expected = match &option.action {
				DecisionAction::PreferMod { mod_id } => {
					ResolutionDecision::PreferMod(mod_id.clone())
				}
				DecisionAction::Handler { name } => ResolutionDecision::Handler(name.clone()),
			};
			for scope in &option.scopes {
				let resolution = match scope {
					DecisionScope::Conflict { decision } => {
						assert_eq!(decision.conflict_id, point.conflict_id);
						assert_eq!(
							ResolutionDecision::PreferMod(decision.prefer_mod.clone()),
							expected
						);
						continue;
					}
					DecisionScope::File { resolution }
					| DecisionScope::Directory { resolution } => resolution,
				};
				let map = ResolutionMap::from_entries(std::slice::from_ref(resolution))
					.unwrap_or_else(|error| panic!("{scope:?}: {error}"));
				assert_eq!(
					map.lookup(&point.file_path, &point.conflict_id, &point.address),
					Some(&expected),
					"{scope:?}"
				);
			}
			let conflict_scopes = option
				.scopes
				.iter()
				.filter(|scope| matches!(scope, DecisionScope::Conflict { .. }))
				.count();
			assert_eq!(
				conflict_scopes,
				usize::from(matches!(option.action, DecisionAction::PreferMod { .. }))
			);
		}
	}
}

#[test]
fn rule_scopes_reach_only_their_file_or_directory() {
	let review = conflicted_review();
	let [name, ai_chance] = review.decisions() else {
		panic!("two decision points");
	};
	let lookup = |resolution: &ResolutionEntry, file: &str, conflict_id: &str, address: &str| {
		ResolutionMap::from_entries(std::slice::from_ref(resolution))
			.unwrap()
			.lookup(&game_path(file), conflict_id, address)
			.is_some()
	};
	let [
		DecisionScope::Conflict { decision },
		DecisionScope::File { resolution: file },
		DecisionScope::Directory {
			resolution: directory,
		},
	] = name.options[0].scopes.as_slice()
	else {
		panic!("conflict, file and directory prefer-mod scopes");
	};
	assert_eq!(decision.conflict_id, name.conflict_id);
	assert_ne!(decision.conflict_id, ai_chance.conflict_id);
	assert!(lookup(file, "events/a.txt", &ai_chance.conflict_id, "x"));
	assert!(!lookup(file, "events/b.txt", "other", "x"));
	assert!(lookup(directory, "events/b.txt", "other", "x"));
	assert!(!lookup(directory, "events/nested/c.txt", "other", "x"));
	assert!(!lookup(directory, "common/events/a.txt", "other", "x"));
}

#[test]
fn review_round_trips_through_json_and_keeps_its_unit_index() {
	let review = conflicted_review();
	let json = serde_json::to_string_pretty(&review).unwrap();
	assert!(json.contains("\"schema\": \"foch.merge_review.v1\""));
	assert!(json.contains("\"disposition\": \"needs_user_choice\""));
	assert!(json.contains("\"kind\": \"prefer_mod\""));
	assert!(!json.contains("by_id"));
	let read = serde_json::from_str::<MergeReview>(&json).unwrap();
	assert_eq!(read, review);
	assert_eq!(
		read.unit("file:events/b.txt")
			.map(|unit| unit.summary.as_str()),
		Some("merged")
	);
}
