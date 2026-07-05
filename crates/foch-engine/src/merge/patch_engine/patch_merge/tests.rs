use super::block_merge::NamedContainerMergeError;
use super::*;
use crate::merge::conflict_handler::{ChainHandler, LookupHandler};
use foch_core::config::{ResolutionDecision, ResolutionMap, compute_conflict_id};
use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::content_family::MergePolicies;
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};

fn span() -> SpanRange {
	SpanRange {
		start: Span {
			line: 0,
			column: 0,
			offset: 0,
		},
		end: Span {
			line: 0,
			column: 0,
			offset: 0,
		},
	}
}

fn scalar(s: &str) -> AstValue {
	AstValue::Scalar {
		value: ScalarValue::Identifier(s.to_string()),
		span: span(),
	}
}

fn number(n: &str) -> AstValue {
	AstValue::Scalar {
		value: ScalarValue::Number(n.to_string()),
		span: span(),
	}
}

fn assignment(key: &str, val: AstValue) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: span(),
		value: val,
		span: span(),
	}
}

fn default_policies() -> MergePolicies {
	MergePolicies::default()
}

fn edit_wins_policies() -> MergePolicies {
	MergePolicies {
		edit_wins_over_remove: true,
		..Default::default()
	}
}

fn scalar_last_writer_policies() -> MergePolicies {
	MergePolicies {
		scalar: ScalarMergePolicy::LastWriter,
		..Default::default()
	}
}

fn coordinate_first_writer_policies() -> MergePolicies {
	MergePolicies {
		scalar: ScalarMergePolicy::CoordinateFirstWriter,
		..Default::default()
	}
}

fn gui_widget_policies() -> MergePolicies {
	MergePolicies {
		scalar: ScalarMergePolicy::GuiWidget,
		..Default::default()
	}
}

fn merge_patch_sets_with_defer(
	mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	policies: &MergePolicies,
) -> PatchMergeResult {
	let mut handler = DeferHandler;
	merge_patch_sets(mod_patches, policies, &mut handler).expect("defer handler should not abort")
}

#[test]
fn merge_patch_sets_with_defer_handler_preserves_current_behavior() {
	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("alpha")),
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("beta")),
	};

	let mut handler = DeferHandler;
	let result = merge_patch_sets(
		vec![
			("mod_a".into(), 1, vec![patch_a.clone()]),
			("mod_b".into(), 2, vec![patch_b.clone()]),
		],
		&default_policies(),
		&mut handler,
	)
	.expect("defer handler should not abort");

	let expected = PatchMergeResult {
		conflicts: vec![PatchResolution::Conflict {
			address: PatchAddress {
				path: vec!["root".into()],
				key: "decisions".into(),
			},
			patches: vec![
				AttributedPatch {
					mod_id: "mod_a".into(),
					precedence: 1,
					patch: patch_a,
				},
				AttributedPatch {
					mod_id: "mod_b".into(),
					precedence: 2,
					patch: patch_b,
				},
			],
			reason: "multiple mods replace the same block with different content".into(),
		}],
		stats: PatchMergeStats {
			total_patches: 2,
			conflict_patches: 1,
			..PatchMergeStats::default()
		},
		..PatchMergeResult::default()
	};
	assert_eq!(result, expected);
}

#[test]
fn merge_patch_sets_with_resolution_picks_correct_mod_patch() {
	let current_file = PathBuf::from("common/ideas/resolved.txt");
	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("alpha")),
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("beta")),
	};
	let conflict_id = compute_conflict_id(&current_file, "root", "decisions");
	let mut resolution_map = ResolutionMap::default();
	resolution_map.by_conflict_id.insert(
		conflict_id,
		ResolutionDecision::PreferMod("mod_a".to_string()),
	);
	let mut handler = LookupHandler::new(&resolution_map, current_file);

	let result = merge_patch_sets(
		vec![
			("mod_a".into(), 1, vec![patch_a.clone()]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&default_policies(),
		&mut handler,
	)
	.expect("resolution map should pick mod_a");

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.handler_resolved_count, 1);
	assert_eq!(result.resolved, vec![PatchResolution::Resolved(patch_a)]);
	assert_eq!(result.stats.conflict_patches, 1);
}

#[test]
fn merge_patch_sets_records_pick_mod_handler_metadata() {
	struct MockRecordedPickHandler;

	impl ConflictHandler for MockRecordedPickHandler {
		fn on_conflict(
			&mut self,
			_: &crate::merge::conflict_view::ConflictView,
		) -> ConflictDecision {
			ConflictDecision::PickMod {
				mod_id: "mod_b".to_string(),
				record: Some(HandlerResolutionRecord {
					path: "common/ideas/dep.txt".to_string(),
					action: "dep_implied".to_string(),
					source: Some("mod_b".to_string()),
					rationale: Some("mod mod_b declares dep on mod_a".to_string()),
				}),
			}
		}
	}

	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("alpha")),
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("beta")),
	};
	let mut handler = MockRecordedPickHandler;

	let result = merge_patch_sets(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b.clone()]),
		],
		&default_policies(),
		&mut handler,
	)
	.expect("mock recorded pick should not abort");

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.handler_resolved_count, 1);
	assert_eq!(result.resolved, vec![PatchResolution::Resolved(patch_b)]);
	assert_eq!(result.handler_resolutions.len(), 1);
	assert_eq!(result.handler_resolutions[0].action, "dep_implied");
	assert_eq!(
		result.handler_resolutions[0].rationale.as_deref(),
		Some("mod mod_b declares dep on mod_a")
	);
}

#[test]
fn chain_handler_falls_through_to_second_on_defer() {
	struct MockPickHandler;

	impl ConflictHandler for MockPickHandler {
		fn on_conflict(
			&mut self,
			_: &crate::merge::conflict_view::ConflictView,
		) -> ConflictDecision {
			ConflictDecision::PickMod {
				mod_id: "mod_b".to_string(),
				record: None,
			}
		}
	}

	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("alpha")),
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("beta")),
	};

	let mut handler = ChainHandler {
		first: DeferHandler,
		second: MockPickHandler,
	};
	let result = merge_patch_sets(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b.clone()]),
		],
		&default_policies(),
		&mut handler,
	)
	.expect("mock pick handler should not abort");

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.handler_resolved_count, 1);
	assert_eq!(result.resolved, vec![PatchResolution::Resolved(patch_b)]);
}

#[test]
fn file_level_conflict_decisions_are_keyed_by_current_file() {
	struct MockFileDecisionHandler {
		decision: ConflictDecision,
	}

	impl ConflictHandler for MockFileDecisionHandler {
		fn on_conflict(
			&mut self,
			_: &crate::merge::conflict_view::ConflictView,
		) -> ConflictDecision {
			self.decision.clone()
		}
	}

	let current_file = PathBuf::from("common/ideas/resolved.txt");
	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("alpha")),
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("beta")),
	};
	let mod_patches = vec![
		("mod_a".into(), 1, vec![patch_a]),
		("mod_b".into(), 2, vec![patch_b]),
	];

	let mut keep_handler = MockFileDecisionHandler {
		decision: ConflictDecision::KeepExisting,
	};
	let keep_result = merge_patch_sets_for_file(
		mod_patches.clone(),
		&default_policies(),
		&mut keep_handler,
		Some(&current_file),
	)
	.expect("keep-existing handler should not abort");

	assert!(keep_result.keep_existing_paths.contains(&current_file));
	assert!(
		!keep_result
			.keep_existing_paths
			.contains(&PathBuf::from("root/decisions"))
	);

	let external_file = PathBuf::from("resolutions/resolved.txt");
	let mut file_handler = MockFileDecisionHandler {
		decision: ConflictDecision::UseFile(external_file.clone()),
	};
	let file_result = merge_patch_sets_for_file(
		mod_patches,
		&default_policies(),
		&mut file_handler,
		Some(&current_file),
	)
	.expect("use-file handler should not abort");

	assert_eq!(
		file_result.external_file_resolutions.get(&current_file),
		Some(&external_file)
	);
	assert!(
		!file_result
			.external_file_resolutions
			.contains_key(&PathBuf::from("root/decisions"))
	);
}

#[test]
fn single_mod_patches_all_resolved() {
	let patches = vec![
		ClausewitzPatch::SetValue {
			path: vec!["root".into()],
			key: "tax".into(),
			old_value: number("5"),
			new_value: number("10"),
		},
		ClausewitzPatch::InsertNode {
			path: vec!["root".into()],
			key: "new_key".into(),
			statement: assignment("new_key", scalar("val")),
		},
	];

	let result =
		merge_patch_sets_with_defer(vec![("mod_a".into(), 1, patches)], &default_policies());

	assert_eq!(result.resolved.len(), 2);
	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.stats.total_patches, 2);
	assert_eq!(result.stats.single_mod_patches, 2);
}

#[test]
fn identical_patches_convergent() {
	let patch = ClausewitzPatch::SetValue {
		path: vec!["root".into()],
		key: "tax".into(),
		old_value: number("5"),
		new_value: number("10"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch.clone()]),
			("mod_b".into(), 2, vec![patch]),
		],
		&default_policies(),
	);

	assert_eq!(result.resolved.len(), 1);
	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.stats.convergent_patches, 1);
}

#[test]
fn different_insert_nodes_under_union_policy_coexist() {
	// Two mods inserting the same key with different bodies under a
	// list-like Union policy (e.g. `monarch_names = "..."` lines inside
	// a country history names block) get distinct addresses via the
	// body fingerprint and apply independently. This is the only
	// policy that opts into list-like coexistence; Recurse / LastWriter
	// keep the (path, key) collision so the resolver can escalate it.
	let patch_a = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "ideas".into(),
		statement: assignment("ideas", scalar("alpha")),
	};
	let patch_b = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "ideas".into(),
		statement: assignment("ideas", scalar("beta")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&union_policies(),
	);

	assert_eq!(result.resolved.len(), 2);
	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.stats.single_mod_patches, 2);
}

#[test]
fn different_insert_nodes_under_recurse_policy_emit_conflict() {
	// Same shape as the Union case above, but under the default Recurse
	// policy the engine treats `(path, "ideas")` as unique-key and
	// surfaces the divergent inserts as a sibling conflict instead of
	// silently picking the highest-precedence patch.
	let patch_a = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "ideas".into(),
		statement: assignment("ideas", scalar("alpha")),
	};
	let patch_b = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "ideas".into(),
		statement: assignment("ideas", scalar("beta")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&default_policies(),
	);

	assert_eq!(result.resolved.len(), 0);
	assert_eq!(result.conflicts.len(), 1);
	match &result.conflicts[0] {
		PatchResolution::Conflict { reason, .. } => {
			assert!(
				reason.contains("sibling mods inserted divergent statements"),
				"unexpected conflict reason: {reason}"
			);
		}
		other => panic!("expected Conflict, got {other:?}"),
	}
}

fn rebel_insert_patch(key: &str, prestige: &str, demand_key: &str) -> ClausewitzPatch {
	ClausewitzPatch::InsertNode {
		path: vec![],
		key: key.to_string(),
		statement: assignment_block(
			key,
			vec![
				assignment("gfx_type", scalar("culture_province")),
				assignment("morale", number("1.1")),
				assignment("demands_description", scalar(demand_key)),
				assignment("add_prestige", number(prestige)),
			],
		),
	}
}

#[test]
fn prefixed_single_root_insert_rename_keeps_highest_precedence() {
	let old = rebel_insert_patch("ita_monarchy_rebels", "-10", "ita_monarchy_rebels_demand");
	let new = rebel_insert_patch(
		"fee_ita_monarchy_rebels",
		"-25",
		"fee_ita_monarchy_rebels_demand",
	);

	let result = merge_patch_sets_with_defer(
		vec![
			("expanded".into(), 0, vec![old]),
			("family".into(), 1, vec![new.clone()]),
		],
		&default_policies(),
	);

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::Resolved(ClausewitzPatch::InsertNode { key, statement, .. }) => {
			assert_eq!(key, "fee_ita_monarchy_rebels");
			assert_eq!(
				statement,
				match &new {
					ClausewitzPatch::InsertNode { statement, .. } => statement,
					_ => unreachable!(),
				}
			);
		}
		other => panic!("expected single retained InsertNode, got: {other:?}"),
	}
}

#[test]
fn single_root_insert_dedup_requires_prefixed_key_pair() {
	let alpha = rebel_insert_patch("alpha_monarchy_rebels", "-10", "alpha_rebels_demand");
	let beta = rebel_insert_patch("beta_monarchy_rebels", "-25", "beta_rebels_demand");

	let result = merge_patch_sets_with_defer(
		vec![
			("alpha".into(), 0, vec![alpha]),
			("beta".into(), 1, vec![beta]),
		],
		&default_policies(),
	);

	let mut keys: Vec<String> = result
		.resolved
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Resolved(ClausewitzPatch::InsertNode { key, .. }) => Some(key.clone()),
			_ => None,
		})
		.collect();
	keys.sort();
	assert_eq!(
		keys,
		vec![
			"alpha_monarchy_rebels".to_string(),
			"beta_monarchy_rebels".to_string()
		]
	);
	assert_eq!(result.conflicts.len(), 0);
}

#[test]
fn same_append_list_item_deduplicated() {
	let patch = ClausewitzPatch::AppendListItem {
		path: vec!["root".into(), "or".into()],
		key: "tag".into(),
		value: scalar("ERS"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch.clone()]),
			("mod_b".into(), 2, vec![patch]),
		],
		&default_policies(),
	);

	assert_eq!(result.resolved.len(), 1);
	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.stats.convergent_patches, 1);
}

#[test]
fn different_append_list_items_independent_addresses() {
	let patch_a = ClausewitzPatch::AppendListItem {
		path: vec!["root".into(), "or".into()],
		key: "tag".into(),
		value: scalar("ERS"),
	};
	let patch_b = ClausewitzPatch::AppendListItem {
		path: vec!["root".into(), "or".into()],
		key: "tag".into(),
		value: scalar("FRA"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&default_policies(),
	);

	// Distinct values land in independent address buckets and apply as
	// single-mod patches; no conflict and no auto-merge needed.
	assert_eq!(result.resolved.len(), 2);
	assert_eq!(result.conflicts.len(), 0);
}

#[test]
fn different_set_value_sibling_conflict() {
	// Sibling mods (no dependency edge) writing the same scalar leaf to
	// divergent values must surface a manual conflict — there is no
	// dependency-graph signal saying which value should win.
	let patch_a = ClausewitzPatch::SetValue {
		path: vec!["root".into()],
		key: "tax".into(),
		old_value: number("5"),
		new_value: number("10"),
	};
	let patch_b = ClausewitzPatch::SetValue {
		path: vec!["root".into()],
		key: "tax".into(),
		old_value: number("5"),
		new_value: number("15"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&default_policies(),
	);

	assert_eq!(result.resolved.len(), 0);
	assert_eq!(result.conflicts.len(), 1);
	assert_eq!(result.stats.conflict_patches, 1);
	match &result.conflicts[0] {
		PatchResolution::Conflict { reason, .. } => {
			assert!(
				reason.contains("sibling mods set the same scalar"),
				"unexpected reason: {reason}"
			);
			assert!(reason.contains("10"));
			assert!(reason.contains("15"));
		}
		other => panic!("expected Conflict, got: {other:?}"),
	}
}

#[test]
fn last_writer_set_value_policy_picks_highest_precedence() {
	let patch_a = ClausewitzPatch::SetValue {
		path: vec!["root".into()],
		key: "tax".into(),
		old_value: number("5"),
		new_value: number("10"),
	};
	let patch_b = ClausewitzPatch::SetValue {
		path: vec!["root".into()],
		key: "tax".into(),
		old_value: number("5"),
		new_value: number("15"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b.clone()]),
		],
		&scalar_last_writer_policies(),
	);

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: patch,
			strategy,
			contributing_mods,
		} => {
			assert_eq!(strategy, "LastWriter");
			assert_eq!(
				contributing_mods,
				&vec!["mod_a".to_string(), "mod_b".to_string()]
			);
			assert_eq!(patch, &patch_b);
		}
		other => panic!("expected LastWriter AutoMerged, got: {other:?}"),
	}
}

#[test]
fn coordinate_first_writer_set_value_policy_picks_lowest_precedence_for_xy() {
	let patch_a = ClausewitzPatch::SetValue {
		path: vec![
			"guiTypes".into(),
			"windowType:provinceview".into(),
			"hide_position".into(),
		],
		key: "x".into(),
		old_value: number("0"),
		new_value: number("-1000"),
	};
	let patch_b = ClausewitzPatch::SetValue {
		path: vec![
			"guiTypes".into(),
			"windowType:provinceview".into(),
			"hide_position".into(),
		],
		key: "x".into(),
		old_value: number("0"),
		new_value: number("-8"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a.clone()]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&coordinate_first_writer_policies(),
	);

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: patch,
			strategy,
			..
		} => {
			assert_eq!(strategy, "CoordinateFirstWriter");
			assert_eq!(patch, &patch_a);
		}
		other => panic!("expected CoordinateFirstWriter AutoMerged, got: {other:?}"),
	}
}

#[test]
fn coordinate_first_writer_policy_keeps_non_coordinate_scalars_conflicting() {
	let patch_a = ClausewitzPatch::SetValue {
		path: vec!["guiTypes".into(), "windowType:provinceview".into()],
		key: "orientation".into(),
		old_value: scalar("LOWER_LEFT"),
		new_value: scalar("LOWER_LEFT"),
	};
	let patch_b = ClausewitzPatch::SetValue {
		path: vec!["guiTypes".into(), "windowType:provinceview".into()],
		key: "orientation".into(),
		old_value: scalar("LOWER_LEFT"),
		new_value: scalar("CENTER"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&coordinate_first_writer_policies(),
	);

	assert_eq!(result.resolved.len(), 0);
	assert_eq!(result.conflicts.len(), 1);
}

#[test]
fn gui_widget_policy_picks_lowest_precedence_for_layout_bounds() {
	let patch_a = ClausewitzPatch::SetValue {
		path: vec![
			"guiTypes".into(),
			"windowType:provinceview".into(),
			"instantTextBoxType:income_label".into(),
		],
		key: "maxWidth".into(),
		old_value: number("100"),
		new_value: number("150"),
	};
	let patch_b = ClausewitzPatch::SetValue {
		path: vec![
			"guiTypes".into(),
			"windowType:provinceview".into(),
			"instantTextBoxType:income_label".into(),
		],
		key: "maxWidth".into(),
		old_value: number("100"),
		new_value: number("200"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a.clone()]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&gui_widget_policies(),
	);

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: patch,
			strategy,
			..
		} => {
			assert_eq!(strategy, "GuiLayoutFirstWriter");
			assert_eq!(patch, &patch_a);
		}
		other => panic!("expected GuiLayoutFirstWriter AutoMerged, got: {other:?}"),
	}
}

#[test]
fn gui_widget_policy_picks_highest_precedence_for_sprite_refs() {
	let patch_a = ClausewitzPatch::InsertNode {
		path: vec![
			"guiTypes".into(),
			"windowType:dlc_entry".into(),
			"iconType:dlc_icon_bg".into(),
		],
		key: "spriteType".into(),
		statement: assignment("spriteType", scalar("GFX_dlc_icon_even_bg_flip")),
	};
	let patch_b = ClausewitzPatch::InsertNode {
		path: vec![
			"guiTypes".into(),
			"windowType:dlc_entry".into(),
			"iconType:dlc_icon_bg".into(),
		],
		key: "spriteType".into(),
		statement: assignment("spriteType", scalar("gfx_emptyness")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b.clone()]),
		],
		&gui_widget_policies(),
	);

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: patch,
			strategy,
			..
		} => {
			assert_eq!(strategy, "GuiReferenceLastWriter");
			assert_eq!(patch, &patch_b);
		}
		other => panic!("expected GuiReferenceLastWriter AutoMerged, got: {other:?}"),
	}
}

#[test]
fn gui_widget_policy_keeps_unlisted_scalars_conflicting() {
	let patch_a = ClausewitzPatch::SetValue {
		path: vec!["guiTypes".into(), "windowType:provinceview".into()],
		key: "orientation".into(),
		old_value: scalar("LOWER_LEFT"),
		new_value: scalar("LOWER_LEFT"),
	};
	let patch_b = ClausewitzPatch::SetValue {
		path: vec!["guiTypes".into(), "windowType:provinceview".into()],
		key: "orientation".into(),
		old_value: scalar("LOWER_LEFT"),
		new_value: scalar("CENTER"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&gui_widget_policies(),
	);

	assert_eq!(result.resolved.len(), 0);
	assert_eq!(result.conflicts.len(), 1);
}

#[test]
fn last_writer_scalar_insert_policy_picks_highest_precedence() {
	let patch_a = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "factor".into(),
		statement: assignment("factor", number("1")),
	};
	let patch_b = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "factor".into(),
		statement: assignment("factor", number("1000")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b.clone()]),
		],
		&scalar_last_writer_policies(),
	);

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: patch,
			strategy,
			..
		} => {
			assert_eq!(strategy, "LastWriter");
			assert_eq!(patch, &patch_b);
		}
		other => panic!("expected LastWriter AutoMerged, got: {other:?}"),
	}
}

#[test]
fn coordinate_first_writer_scalar_insert_policy_picks_lowest_precedence_for_xy() {
	let patch_a = ClausewitzPatch::InsertNode {
		path: vec![
			"guiTypes".into(),
			"windowType:provinceview".into(),
			"hide_position".into(),
		],
		key: "x".into(),
		statement: assignment("x", number("-1000")),
	};
	let patch_b = ClausewitzPatch::InsertNode {
		path: vec![
			"guiTypes".into(),
			"windowType:provinceview".into(),
			"hide_position".into(),
		],
		key: "x".into(),
		statement: assignment("x", number("-8")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a.clone()]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&coordinate_first_writer_policies(),
	);

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: patch,
			strategy,
			..
		} => {
			assert_eq!(strategy, "CoordinateFirstWriter");
			assert_eq!(patch, &patch_a);
		}
		other => panic!("expected CoordinateFirstWriter AutoMerged, got: {other:?}"),
	}
}

#[test]
fn last_writer_block_insert_policy_picks_highest_precedence() {
	let patch_a = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "effect_tooltip".into(),
		statement: assignment_block(
			"effect_tooltip",
			vec![assignment("add_prestige", number("5"))],
		),
	};
	let patch_b = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "effect_tooltip".into(),
		statement: assignment_block(
			"effect_tooltip",
			vec![assignment("add_prestige", number("10"))],
		),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b.clone()]),
		],
		&scalar_last_writer_policies(),
	);

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: patch,
			strategy,
			..
		} => {
			assert_eq!(strategy, "LastWriter");
			assert_eq!(patch, &patch_b);
		}
		other => panic!("expected LastWriter AutoMerged, got: {other:?}"),
	}
}

#[test]
fn conflicting_replace_blocks() {
	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("alpha")),
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "decisions".into(),
		old_statement: assignment("decisions", scalar("old")),
		new_statement: assignment("decisions", scalar("beta")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&default_policies(),
	);

	assert_eq!(result.resolved.len(), 0);
	assert_eq!(result.conflicts.len(), 1);
	assert_eq!(result.stats.conflict_patches, 1);
	match &result.conflicts[0] {
		PatchResolution::Conflict {
			reason, patches, ..
		} => {
			assert!(reason.contains("replace the same block"));
			assert_eq!(patches.len(), 2);
		}
		other => panic!("expected Conflict, got: {other:?}"),
	}
}

#[test]
fn stats_are_correctly_tracked() {
	// Mix of single, convergent, auto-merged, and conflict patches.
	let single = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "unique".into(),
		statement: assignment("unique", scalar("only_a")),
	};
	let convergent = ClausewitzPatch::RemoveNode {
		path: vec!["root".into()],
		key: "old_key".into(),
		removed: assignment("old_key", scalar("gone")),
	};
	let conflict_a = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "block".into(),
		old_statement: assignment("block", scalar("old")),
		new_statement: assignment("block", scalar("a_ver")),
	};
	let conflict_b = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "block".into(),
		old_statement: assignment("block", scalar("old")),
		new_statement: assignment("block", scalar("b_ver")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			(
				"mod_a".into(),
				1,
				vec![single, convergent.clone(), conflict_a],
			),
			("mod_b".into(), 2, vec![convergent, conflict_b]),
		],
		&default_policies(),
	);

	assert_eq!(result.stats.total_patches, 5);
	assert_eq!(result.stats.single_mod_patches, 1);
	assert_eq!(result.stats.convergent_patches, 1);
	assert_eq!(result.stats.conflict_patches, 1);
	assert_eq!(result.resolved.len(), 2); // single + convergent
	assert_eq!(result.conflicts.len(), 1);
}

#[test]
fn mixed_kinds_at_same_address_conflict() {
	// Insert + Remove at the same `(path, key)` AND same body still
	// share a `PatchAddress` (the body fingerprint is included for
	// both kinds), so a real Insert-X / Remove-X collision continues
	// to surface as a mixed-kind conflict.
	let insert = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "thing".into(),
		statement: assignment("thing", scalar("same")),
	};
	let remove = ClausewitzPatch::RemoveNode {
		path: vec!["root".into()],
		key: "thing".into(),
		removed: assignment("thing", scalar("same")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![insert]),
			("mod_b".into(), 2, vec![remove]),
		],
		&default_policies(),
	);

	assert_eq!(result.conflicts.len(), 1);
	assert_eq!(result.stats.conflict_patches, 1);
	match &result.conflicts[0] {
		PatchResolution::Conflict { reason, .. } => {
			assert!(reason.contains("mixed patch kinds"));
		}
		other => panic!("expected Conflict, got: {other:?}"),
	}
}

#[test]
fn set_value_wins_over_remove_edit_wins() {
	// One mod edits a scalar property (Orientation); another removes it.
	// edit-wins: keep the edit, drop the remove, no conflict.
	let set = ClausewitzPatch::SetValue {
		path: vec!["widget".into()],
		key: "orientation".into(),
		old_value: scalar("CENTER"),
		new_value: scalar("UPPER_LEFT"),
	};
	let remove = ClausewitzPatch::RemoveNode {
		path: vec!["widget".into()],
		key: "orientation".into(),
		removed: assignment("orientation", scalar("CENTER")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![set]),
			("mod_b".into(), 2, vec![remove]),
		],
		&edit_wins_policies(),
	);

	assert_eq!(result.conflicts.len(), 0, "edit must win over remove");
	assert_eq!(result.stats.conflict_patches, 0);
	assert_eq!(result.stats.edit_over_remove_resolved, 1);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::Resolved(ClausewitzPatch::SetValue { key, .. }) => {
			assert_eq!(key, "orientation");
		}
		other => panic!("expected resolved SetValue, got: {other:?}"),
	}
}

#[test]
fn replace_block_wins_over_remove_edit_wins() {
	// One mod replaces a block property (position); another removes it.
	let replace = ClausewitzPatch::ReplaceBlock {
		path: vec!["widget".into()],
		key: "position".into(),
		old_statement: assignment("position", scalar("old")),
		new_statement: assignment("position", scalar("moved")),
	};
	let remove = ClausewitzPatch::RemoveNode {
		path: vec!["widget".into()],
		key: "position".into(),
		removed: assignment("position", scalar("old")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![replace]),
			("mod_b".into(), 2, vec![remove]),
		],
		&edit_wins_policies(),
	);

	assert_eq!(result.conflicts.len(), 0, "block edit must win over remove");
	assert_eq!(result.stats.edit_over_remove_resolved, 1);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::Resolved(ClausewitzPatch::ReplaceBlock { key, .. }) => {
			assert_eq!(key, "position");
		}
		other => panic!("expected resolved ReplaceBlock, got: {other:?}"),
	}
}

#[test]
fn edit_vs_edit_still_conflicts_even_with_a_remove() {
	// edit-wins drops removes, but two genuine divergent edits at the same leaf
	// must STILL conflict — dropping the remove must not silently pick a winner.
	let set_a = ClausewitzPatch::SetValue {
		path: vec!["root".into()],
		key: "tax".into(),
		old_value: number("5"),
		new_value: number("10"),
	};
	let set_b = ClausewitzPatch::SetValue {
		path: vec!["root".into()],
		key: "tax".into(),
		old_value: number("5"),
		new_value: number("15"),
	};
	let remove = ClausewitzPatch::RemoveNode {
		path: vec!["root".into()],
		key: "tax".into(),
		removed: assignment("tax", number("5")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![set_a]),
			("mod_b".into(), 2, vec![set_b]),
			("mod_c".into(), 3, vec![remove]),
		],
		&edit_wins_policies(),
	);

	assert_eq!(
		result.stats.edit_over_remove_resolved, 1,
		"remove was dropped"
	);
	assert_eq!(result.conflicts.len(), 1, "divergent edits still conflict");
	assert_eq!(result.stats.conflict_patches, 1);
}

#[test]
fn rename_for_conflict_assignment_key_appends_mod_suffix() {
	let stmt = assignment(
		"pragmatic_sanction",
		AstValue::Block {
			items: vec![assignment("potential", scalar("yes"))],
			span: span(),
		},
	);

	let renamed = rename_for_conflict(&stmt, MergeKeySource::AssignmentKey, "mod_a");

	match renamed {
		AstStatement::Assignment { key, value, .. } => {
			assert_eq!(key, "pragmatic_sanction_mod_a");
			// Body is preserved as-is.
			match value {
				AstValue::Block { items, .. } => {
					assert_eq!(items.len(), 1);
					assert!(matches!(
						&items[0],
						AstStatement::Assignment { key, .. } if key == "potential"
					));
				}
				_ => panic!("expected block body"),
			}
		}
		_ => panic!("expected Assignment"),
	}

	// ContainerChildKey behaves identically (renames the top-level key).
	let renamed_container = rename_for_conflict(&stmt, MergeKeySource::ContainerChildKey, "mod_b");
	match renamed_container {
		AstStatement::Assignment { key, .. } => {
			assert_eq!(key, "pragmatic_sanction_mod_b");
		}
		_ => panic!("expected Assignment"),
	}
}

#[test]
fn rename_for_conflict_field_value_renames_inner_id() {
	let stmt = assignment(
		"country_event",
		AstValue::Block {
			items: vec![
				assignment("id", scalar("test.1")),
				assignment("title", scalar("evt_title")),
			],
			span: span(),
		},
	);

	let renamed = rename_for_conflict(&stmt, MergeKeySource::FieldValue("id"), "mod_a");

	match renamed {
		AstStatement::Assignment { key, value, .. } => {
			// Outer key is unchanged.
			assert_eq!(key, "country_event");
			match value {
				AstValue::Block { items, .. } => {
					assert_eq!(items.len(), 2);
					// The `id` field has been renamed.
					match &items[0] {
						AstStatement::Assignment {
							key: ikey,
							value:
								AstValue::Scalar {
									value: ScalarValue::Identifier(v),
									..
								},
							..
						} => {
							assert_eq!(ikey, "id");
							assert_eq!(v, "test.1_mod_a");
						}
						other => panic!("expected scalar id field, got {other:?}"),
					}
					// Other fields are untouched.
					match &items[1] {
						AstStatement::Assignment {
							key: ikey,
							value:
								AstValue::Scalar {
									value: ScalarValue::Identifier(v),
									..
								},
							..
						} => {
							assert_eq!(ikey, "title");
							assert_eq!(v, "evt_title");
						}
						other => panic!("expected scalar title field, got {other:?}"),
					}
				}
				_ => panic!("expected block body"),
			}
		}
		_ => panic!("expected Assignment"),
	}
}

#[test]
fn rename_for_conflict_leaf_path_returns_unchanged_or_lastwriter() {
	// LeafPath identities are the dotted path itself, which cannot be
	// suffix-renamed without changing semantics. The helper must return
	// the statement unchanged so callers fall back to last-writer.
	let stmt = assignment("NGame.START_YEAR", scalar("1444"));
	let renamed = rename_for_conflict(&stmt, MergeKeySource::LeafPath, "mod_a");
	assert_eq!(renamed, stmt);

	// Comments and items are similarly left alone for any key source.
	let comment = AstStatement::Comment {
		text: "# header".to_string(),
		span: span(),
	};
	assert_eq!(
		rename_for_conflict(&comment, MergeKeySource::AssignmentKey, "mod_a"),
		comment
	);
}

#[test]
fn numeric_sum_policy() {
	let policies = MergePolicies {
		scalar: ScalarMergePolicy::Sum,
		..Default::default()
	};
	let patch_a = ClausewitzPatch::SetValue {
		path: vec!["root".into()],
		key: "bonus".into(),
		old_value: number("0"),
		new_value: number("5"),
	};
	let patch_b = ClausewitzPatch::SetValue {
		path: vec!["root".into()],
		key: "bonus".into(),
		old_value: number("0"),
		new_value: number("3"),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&policies,
	);

	assert_eq!(result.resolved.len(), 1);
	assert_eq!(result.stats.auto_merged_patches, 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: patch,
			strategy,
			..
		} => {
			assert_eq!(strategy, "Sum");
			match patch {
				ClausewitzPatch::SetValue { new_value, .. } => {
					assert_eq!(*new_value, number("8"));
				}
				_ => panic!("expected SetValue"),
			}
		}
		other => panic!("expected AutoMerged, got: {other:?}"),
	}
}

#[test]
fn numeric_sum_policy_applies_to_scalar_inserts() {
	let policies = MergePolicies {
		scalar: ScalarMergePolicy::Sum,
		..Default::default()
	};
	let patch_a = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "bonus".into(),
		statement: assignment("bonus", number("5")),
	};
	let patch_b = ClausewitzPatch::InsertNode {
		path: vec!["root".into()],
		key: "bonus".into(),
		statement: assignment("bonus", number("3")),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&policies,
	);

	assert_eq!(result.resolved.len(), 1);
	assert_eq!(result.stats.auto_merged_patches, 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: ClausewitzPatch::InsertNode { statement, .. },
			strategy,
			..
		} => {
			assert_eq!(strategy, "Sum");
			assert_eq!(statement, &assignment("bonus", number("8")));
		}
		other => panic!("expected AutoMerged scalar InsertNode, got: {other:?}"),
	}
}

fn block_value(items: Vec<AstStatement>) -> AstValue {
	AstValue::Block {
		items,
		span: span(),
	}
}

fn assignment_block(key: &str, items: Vec<AstStatement>) -> AstStatement {
	AstStatement::Assignment {
		key: key.to_string(),
		key_span: span(),
		value: block_value(items),
		span: span(),
	}
}

fn boolean_or_policies() -> MergePolicies {
	MergePolicies {
		block_patch: BlockPatchPolicy::BooleanOr,
		..Default::default()
	}
}

const COUNTRY_NAME_BLOCK_POLICIES: &[(&str, BlockPatchPolicy)] = &[
	("monarch_names", BlockPatchPolicy::Union),
	("leader_names", BlockPatchPolicy::Union),
	("ship_names", BlockPatchPolicy::Union),
	("army_names", BlockPatchPolicy::Union),
];

fn union_policies() -> MergePolicies {
	MergePolicies {
		block_patch: BlockPatchPolicy::Union,
		..Default::default()
	}
}

fn country_history_name_union_policies() -> MergePolicies {
	MergePolicies {
		block_patch: BlockPatchPolicy::Recurse,
		block_patch_policies: COUNTRY_NAME_BLOCK_POLICIES,
		..Default::default()
	}
}

fn bare_item(value: &str) -> AstStatement {
	AstStatement::Item {
		value: scalar(value),
		span: span(),
	}
}

fn replace_block_patch(
	key: &str,
	old_items: Vec<AstStatement>,
	new_items: Vec<AstStatement>,
) -> ClausewitzPatch {
	ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: key.to_string(),
		old_statement: assignment_block(key, old_items),
		new_statement: assignment_block(key, new_items),
	}
}

fn block_items(stmt: &AstStatement) -> &[AstStatement] {
	match stmt {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => items,
		other => panic!("expected block assignment, got {other:?}"),
	}
}

fn item_texts(items: &[AstStatement]) -> Vec<String> {
	items
		.iter()
		.map(|item| match item {
			AstStatement::Item {
				value: AstValue::Scalar { value, .. },
				..
			} => value.as_text(),
			other => panic!("expected scalar item, got {other:?}"),
		})
		.collect()
}

fn assignment_keys(items: &[AstStatement]) -> Vec<String> {
	items
		.iter()
		.map(|item| match item {
			AstStatement::Assignment { key, .. } => key.clone(),
			other => panic!("expected assignment item, got {other:?}"),
		})
		.collect()
}

/// Helper: assert `stmt` is `key = { OR = { <d_0> <d_1> ... } }` — a single
/// shared `OR` whose disjuncts are each contributor's body (inlined when the
/// body is one statement, else wrapped in `AND = { ... }`). Returns the disjunct
/// bodies in order. Enforces the single-OR structure so a regression back to
/// sibling `OR` blocks (which would mean an implicit AND / intersection) fails.
fn assert_or_wrapped(stmt: &AstStatement, expected_key: &str) -> Vec<Vec<AstStatement>> {
	let (key, items) = match stmt {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} => (key.as_str(), items.as_slice()),
		other => panic!("expected Assignment with Block value, got: {other:?}"),
	};
	assert_eq!(key, expected_key, "outer key mismatch");
	assert_eq!(
		items.len(),
		1,
		"expected exactly one shared OR wrapper (OR of disjuncts), got {} children: {items:?}",
		items.len()
	);
	let or_items = match &items[0] {
		AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} => {
			assert_eq!(key, "OR", "expected single OR wrapper, got key={key}");
			items.as_slice()
		}
		other => panic!("expected `OR = {{ ... }}`, got: {other:?}"),
	};
	or_items
		.iter()
		.map(|disjunct| match disjunct {
			// Multi-statement bodies are wrapped in `AND = { ... }`.
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} if key == "AND" => items.clone(),
			// Single-statement bodies are inlined verbatim.
			other => vec![other.clone()],
		})
		.collect()
}

#[test]
fn boolean_or_two_mods_modify_same_block_produces_single_or_of_disjuncts() {
	let body_a = vec![assignment("tag", scalar("ABC"))];
	let body_b = vec![assignment("culture", scalar("french"))];
	let old = assignment_block("is_great_power", vec![assignment("tag", scalar("OLD"))]);

	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "is_great_power".into(),
		old_statement: old.clone(),
		new_statement: assignment_block("is_great_power", body_a.clone()),
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "is_great_power".into(),
		old_statement: old,
		new_statement: assignment_block("is_great_power", body_b.clone()),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&boolean_or_policies(),
	);

	assert_eq!(result.resolved.len(), 1);
	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.stats.auto_merged_patches, 1);

	let merged_stmt = match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
			strategy,
			contributing_mods,
		} => {
			assert_eq!(strategy, "boolean_or");
			assert_eq!(contributing_mods.len(), 2);
			new_statement
		}
		other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
	};

	let or_bodies = assert_or_wrapped(merged_stmt, "is_great_power");
	assert_eq!(or_bodies.len(), 2);
	assert_eq!(or_bodies[0], body_a);
	assert_eq!(or_bodies[1], body_b);
}

#[test]
fn boolean_or_three_mods_produce_single_or_of_three_disjuncts() {
	let body_a = vec![assignment("tag", scalar("AAA"))];
	let body_b = vec![assignment("tag", scalar("BBB"))];
	let body_c = vec![assignment("tag", scalar("CCC"))];

	let mk = |body: &[AstStatement]| ClausewitzPatch::InsertNode {
		path: vec![],
		key: "is_powerful".into(),
		statement: assignment_block("is_powerful", body.to_vec()),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![mk(&body_a)]),
			("mod_b".into(), 2, vec![mk(&body_b)]),
			("mod_c".into(), 3, vec![mk(&body_c)]),
		],
		&boolean_or_policies(),
	);

	assert_eq!(result.resolved.len(), 1);
	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.stats.auto_merged_patches, 1);

	let merged_stmt = match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: ClausewitzPatch::InsertNode { statement, .. },
			strategy,
			..
		} => {
			assert_eq!(strategy, "boolean_or");
			statement
		}
		other => panic!("expected AutoMerged InsertNode, got: {other:?}"),
	};

	let or_bodies = assert_or_wrapped(merged_stmt, "is_powerful");
	assert_eq!(or_bodies.len(), 3);
	assert_eq!(or_bodies[0], body_a);
	assert_eq!(or_bodies[1], body_b);
	assert_eq!(or_bodies[2], body_c);
}

#[test]
fn boolean_or_preserves_disjunction_semantics_and_conjunctive_bodies() {
	// Regression for the semantic-inversion bug: two divergent same-key trigger
	// definitions must merge to OR(body_a, body_b) — "holds if EITHER mod's body
	// holds" — NOT to sibling `OR` blocks (which a trigger block reads as an
	// implicit AND, i.e. the *intersection*, the opposite of BooleanOr's intent).
	//
	// mod_a contributes a MULTI-statement (conjunctive) body, so it must be
	// `AND`-wrapped to stay a single disjunct; mod_b's single-statement body is
	// inlined. Real-world shape: Europa Expanded + Flavour & Events Expanded both
	// fully (re)define `is_expanded_mod_active`.
	let body_a = vec![
		assignment("has_global_flag", scalar("ee_active")),
		assignment("has_global_flag", scalar("ee_active_typo")),
	];
	let body_b = vec![assignment("has_global_flag", scalar("fee_active"))];

	let mk = |body: &[AstStatement]| ClausewitzPatch::InsertNode {
		path: vec![],
		key: "is_expanded_mod_active".into(),
		statement: assignment_block("is_expanded_mod_active", body.to_vec()),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![mk(&body_a)]),
			("mod_b".into(), 2, vec![mk(&body_b)]),
		],
		&boolean_or_policies(),
	);

	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.stats.auto_merged_patches, 1);

	let merged_stmt = match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: ClausewitzPatch::InsertNode { statement, .. },
			strategy,
			..
		} => {
			assert_eq!(strategy, "boolean_or");
			statement
		}
		other => panic!("expected AutoMerged InsertNode, got: {other:?}"),
	};

	// Exactly one shared OR (asserted by the helper) with two disjuncts that
	// recover each contributor's original body.
	let or_bodies = assert_or_wrapped(merged_stmt, "is_expanded_mod_active");
	assert_eq!(or_bodies.len(), 2, "expected exactly two OR disjuncts");
	assert_eq!(
		or_bodies[0], body_a,
		"mod_a's conjunctive body must survive"
	);
	assert_eq!(or_bodies[1], body_b);

	// The multi-statement disjunct must be `AND`-wrapped (not flattened into the
	// OR, which would change AND(a1,a2) into OR(a1,a2)).
	let or_children = match merged_stmt {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => match &items[0] {
			AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} => items,
			other => panic!("expected OR block, got: {other:?}"),
		},
		other => panic!("expected Assignment block, got: {other:?}"),
	};
	match &or_children[0] {
		AstStatement::Assignment { key, .. } => {
			assert_eq!(key, "AND", "multi-statement body must be AND-wrapped")
		}
		other => panic!("expected AND-wrapped disjunct, got: {other:?}"),
	}
}

#[test]
fn boolean_or_single_modification_no_or_wrap() {
	let body = vec![assignment("tag", scalar("XYZ"))];
	let patch = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "is_lonely".into(),
		old_statement: assignment_block("is_lonely", vec![]),
		new_statement: assignment_block("is_lonely", body.clone()),
	};

	let result = merge_patch_sets_with_defer(
		vec![("mod_a".into(), 1, vec![patch])],
		&boolean_or_policies(),
	);

	assert_eq!(result.resolved.len(), 1);
	assert_eq!(result.stats.single_mod_patches, 1);
	assert_eq!(result.stats.auto_merged_patches, 0);

	match &result.resolved[0] {
		PatchResolution::Resolved(ClausewitzPatch::ReplaceBlock { new_statement, .. }) => {
			let items = match new_statement {
				AstStatement::Assignment {
					value: AstValue::Block { items, .. },
					..
				} => items,
				other => panic!("expected Assignment block, got: {other:?}"),
			};
			assert_eq!(*items, body);
			for child in items {
				if let AstStatement::Assignment { key, .. } = child {
					assert_ne!(key, "OR", "single-mod path must not introduce OR wrappers");
				}
			}
		}
		other => panic!("expected single-mod Resolved ReplaceBlock, got: {other:?}"),
	}
}

#[test]
fn explicit_last_writer_block_policy_falls_through_to_conflict() {
	// `BlockPatchPolicy::LastWriter` is a deliberate escape hatch: it does
	// not actually silently pick a winner — it just sidesteps Recurse /
	// Union / BooleanOr / named-container so the final branch in
	// `resolve_replace_blocks` reports the divergent ReplaceBlock as a
	// manual conflict. This keeps families that explicitly opt into
	// LastWriter from getting auto-merged behind their backs.
	let old = assignment_block("is_great_power", vec![]);
	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "is_great_power".into(),
		old_statement: old.clone(),
		new_statement: assignment_block("is_great_power", vec![assignment("tag", scalar("A"))]),
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "is_great_power".into(),
		old_statement: old,
		new_statement: assignment_block("is_great_power", vec![assignment("tag", scalar("B"))]),
	};

	let mut policies = default_policies();
	policies.block_patch = BlockPatchPolicy::LastWriter;
	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&policies,
	);

	assert_eq!(result.resolved.len(), 0);
	assert_eq!(result.conflicts.len(), 1);
	assert_eq!(result.stats.conflict_patches, 1);
	assert_eq!(
		MergePolicies::default().block_patch,
		BlockPatchPolicy::Recurse,
		"BlockPatchPolicy::default() must be Recurse"
	);
}

#[test]
fn union_block_collects_two_overlay_items_via_fingerprint() {
	let base = vec![bare_item("Base")];
	let patch_a = replace_block_patch(
		"leader_names",
		base.clone(),
		vec![bare_item("Base"), bare_item("Afonso")],
	);
	let patch_b = replace_block_patch(
		"leader_names",
		base,
		vec![bare_item("Base"), bare_item("Bernat")],
	);

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&union_policies(),
	);

	assert_eq!(
		result.conflicts.len(),
		0,
		"got conflicts: {:?}",
		result.conflicts
	);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
			strategy,
			..
		} => {
			assert_eq!(strategy, "union_block");
			assert_eq!(
				item_texts(block_items(new_statement)),
				vec!["Base", "Afonso", "Bernat"]
			);
		}
		other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
	}
}

#[test]
fn union_block_collects_three_overlay_items_and_dedups_assignments() {
	let base = vec![assignment("Base #0", number("10"))];
	let patch_a = replace_block_patch(
		"monarch_names",
		base.clone(),
		vec![
			assignment("Base #0", number("10")),
			assignment("Afonso #0", number("0")),
		],
	);
	let patch_b = replace_block_patch(
		"monarch_names",
		base.clone(),
		vec![
			assignment("Base #0", number("10")),
			assignment("Bernat #0", number("0")),
		],
	);
	let patch_c = replace_block_patch(
		"monarch_names",
		base,
		vec![
			assignment("Base #0", number("10")),
			assignment("Afonso #0", number("0")),
			assignment("Carles #0", number("0")),
		],
	);

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
			("mod_c".into(), 3, vec![patch_c]),
		],
		&union_policies(),
	);

	assert_eq!(
		result.conflicts.len(),
		0,
		"got conflicts: {:?}",
		result.conflicts
	);
	assert_eq!(result.resolved.len(), 1);
	let new_statement = match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
			strategy,
			..
		} => {
			assert_eq!(strategy, "union_block");
			new_statement
		}
		other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
	};
	assert_eq!(
		assignment_keys(block_items(new_statement)),
		vec!["Base #0", "Afonso #0", "Bernat #0", "Carles #0"]
	);
}

#[test]
fn union_block_policy_coexists_with_recurse_for_other_blocks() {
	let vanilla_date = assignment_block("1444.11.11", vec![assignment("owner", scalar("BYZ"))]);
	let date_a = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "1444.11.11".into(),
		old_statement: vanilla_date.clone(),
		new_statement: assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]),
	};
	let date_b = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "1444.11.11".into(),
		old_statement: assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]),
		new_statement: assignment_block(
			"1444.11.11",
			vec![
				assignment("owner", scalar("BYZ")),
				assignment("controller", scalar("OTT")),
			],
		),
	};
	let base_names = vec![bare_item("Base")];
	let names_a = replace_block_patch(
		"leader_names",
		base_names.clone(),
		vec![bare_item("Base"), bare_item("Afonso")],
	);
	let names_b = replace_block_patch(
		"leader_names",
		base_names,
		vec![bare_item("Base"), bare_item("Bernat")],
	);

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![date_a, names_a]),
			("mod_b".into(), 2, vec![date_b, names_b]),
		],
		&country_history_name_union_policies(),
	);

	assert_eq!(
		result.conflicts.len(),
		0,
		"got conflicts: {:?}",
		result.conflicts
	);
	let mut strategies: Vec<&str> = result
		.resolved
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::AutoMerged { strategy, .. } => Some(strategy.as_str()),
			_ => None,
		})
		.collect();
	strategies.sort_unstable();
	assert_eq!(strategies, vec!["recursive_block_merge", "union_block"]);
}

#[test]
fn default_recurse_policy_unions_compatible_appends() {
	// Two sibling mods append distinct bare items to the same list-shaped
	// block. Recurse re-diffs the bodies and produces independent
	// `AppendBlockItem` patches at fingerprinted addresses; both apply
	// without a conflict (the user explicitly endorsed list-append
	// coexistence: "list 追加合并我觉得没什么问题").
	let base = vec![bare_item("Base")];
	let patch_a = replace_block_patch(
		"leader_names",
		base.clone(),
		vec![bare_item("Base"), bare_item("Afonso")],
	);
	let patch_b = replace_block_patch(
		"leader_names",
		base,
		vec![bare_item("Base"), bare_item("Bernat")],
	);

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&default_policies(),
	);

	assert_eq!(
		result.conflicts.len(),
		0,
		"got conflicts: {:?}",
		result.conflicts
	);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
			strategy,
			..
		} => {
			assert_eq!(strategy, "recursive_block_merge");
			let names = item_texts(block_items(new_statement));
			assert!(
				names.contains(&"Afonso".to_string()),
				"missing Afonso: {names:?}"
			);
			assert!(
				names.contains(&"Bernat".to_string()),
				"missing Bernat: {names:?}"
			);
		}
		other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
	}
}

// -----------------------------------------------------------------------
// Named-container merge tests
// -----------------------------------------------------------------------

fn string_val(s: &str) -> AstValue {
	AstValue::Scalar {
		value: ScalarValue::String(s.to_string()),
		span: span(),
	}
}

fn block(items: Vec<AstStatement>) -> AstValue {
	AstValue::Block {
		items,
		span: span(),
	}
}

fn named_block(key: &str, name: &str, extras: Vec<AstStatement>) -> AstStatement {
	let mut items = vec![assignment("name", string_val(name))];
	items.extend(extras);
	assignment(key, block(items))
}

#[test]
fn child_identity_named_block_returns_key_and_name() {
	let stmt = named_block(
		"windowType",
		"hre_window",
		vec![assignment("position", scalar("center"))],
	);
	let id = child_identity(&stmt).expect("identity");
	assert_eq!(id.key, "windowType");
	assert_eq!(id.name.as_deref(), Some("hre_window"));
}

#[test]
fn child_identity_block_without_name_returns_key_only() {
	let stmt = assignment("position", block(vec![assignment("x", number("1"))]));
	let id = child_identity(&stmt).expect("identity");
	assert_eq!(id.key, "position");
	assert_eq!(id.name, None);
}

#[test]
fn items_are_named_container_pure_blocks_true() {
	let body = vec![
		named_block("windowType", "a", vec![]),
		named_block("windowType", "b", vec![]),
	];
	assert!(items_are_named_container(&body, false));
	assert!(items_are_named_container(&body, true));
}

#[test]
fn items_are_named_container_mixed_with_scalars_strict_false_lenient_true() {
	let body = vec![
		assignment("position", scalar("center")), // bare scalar field
		named_block("iconType", "icon_a", vec![]),
	];
	assert!(!items_are_named_container(&body, false));
	assert!(items_are_named_container(&body, true));
}

#[test]
fn ast_equal_ignoring_spans_handles_different_filenames() {
	// Two structurally identical statements with different spans (here we
	// can only differ on offset/line/column) must compare equal.
	let s1 = named_block(
		"iconType",
		"icon_a",
		vec![assignment("texture", scalar("a.dds"))],
	);
	let mut s2 = s1.clone();
	// Mutate inner spans to simulate a different parse origin.
	if let AstStatement::Assignment { span, .. } = &mut s2 {
		span.start.line = 42;
		span.start.column = 7;
		span.end.line = 99;
	}
	assert!(ast_equal_ignoring_spans(&s1, &s2));
	assert_ne!(s1, s2, "raw PartialEq must differ — spans differ");
}

fn body_to_window_type_block(name: &str, body: Vec<AstStatement>) -> AstStatement {
	named_block("windowType", name, body)
}

#[test]
fn merge_two_modded_windowtypes_unions_inner_icon_types() {
	// Base: empty windowType "hre"
	let base = vec![body_to_window_type_block("hre", vec![])];
	// Mod A adds iconType "ico_a" inside windowType
	let mod_a = vec![body_to_window_type_block(
		"hre",
		vec![named_block("iconType", "ico_a", vec![])],
	)];
	// Mod B adds iconType "ico_b" inside windowType
	let mod_b = vec![body_to_window_type_block(
		"hre",
		vec![named_block("iconType", "ico_b", vec![])],
	)];

	let merged = merge_named_container_bodies(
		&base,
		&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
		&default_policies(),
	)
	.expect("merge");

	assert_eq!(merged.len(), 1);
	// Inspect inner body: should now have both iconType ico_a and ico_b.
	let inner = match &merged[0] {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => items,
		other => panic!("expected windowType block, got {other:?}"),
	};
	// Filter only iconType children.
	let icons: Vec<_> = inner
		.iter()
		.filter_map(child_identity)
		.filter(|id| id.key == "iconType")
		.map(|id| id.name.unwrap_or_default())
		.collect();
	assert!(
		icons.contains(&"ico_a".to_string()),
		"missing ico_a: {icons:?}"
	);
	assert!(
		icons.contains(&"ico_b".to_string()),
		"missing ico_b: {icons:?}"
	);
}

#[test]
fn merge_two_modded_windowtypes_recursive_into_named_subblock() {
	// Both mods modify the same iconType "ico_x", each adding distinct grandchild.
	let base = vec![body_to_window_type_block(
		"hre",
		vec![named_block("iconType", "ico_x", vec![])],
	)];
	let mod_a = vec![body_to_window_type_block(
		"hre",
		vec![named_block(
			"iconType",
			"ico_x",
			vec![named_block("hover", "h_a", vec![])],
		)],
	)];
	let mod_b = vec![body_to_window_type_block(
		"hre",
		vec![named_block(
			"iconType",
			"ico_x",
			vec![named_block("hover", "h_b", vec![])],
		)],
	)];

	let merged = merge_named_container_bodies(
		&base,
		&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
		&default_policies(),
	)
	.expect("merge");

	// Drill down: windowType.hre -> iconType.ico_x -> body should contain
	// both hover.h_a and hover.h_b.
	let window_body = match &merged[0] {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => items,
		_ => panic!("expected windowType block"),
	};
	let icon = window_body
		.iter()
		.find(|s| {
			child_identity(s)
				.map(|i| i.name.as_deref() == Some("ico_x"))
				.unwrap_or(false)
		})
		.expect("ico_x present");
	let icon_body = match icon {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => items,
		_ => panic!("ico_x should be a block"),
	};
	let hovers: Vec<_> = icon_body
		.iter()
		.filter_map(child_identity)
		.filter(|id| id.key == "hover")
		.map(|id| id.name.unwrap_or_default())
		.collect();
	assert!(
		hovers.contains(&"h_a".to_string()),
		"missing h_a: {hovers:?}"
	);
	assert!(
		hovers.contains(&"h_b".to_string()),
		"missing h_b: {hovers:?}"
	);
}

#[test]
fn merge_conflict_suffix_renames_under_lenient() {
	// Same identity, both leaves (no nested named-container body) → cannot
	// recurse → SuffixRename keeps both via rename.
	let base: Vec<AstStatement> = vec![named_block("iconType", "icon_x", vec![])];
	let mod_a = vec![named_block(
		"iconType",
		"icon_x",
		vec![assignment("texture", string_val("a.dds"))],
	)];
	let mod_b = vec![named_block(
		"iconType",
		"icon_x",
		vec![assignment("texture", string_val("b.dds"))],
	)];

	let policies = MergePolicies {
		named_container: NamedContainerPolicy::SuffixRename,
		..Default::default()
	};
	let merged = merge_named_container_bodies(
		&base,
		&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
		&policies,
	)
	.expect("merge");

	// First candidate replaced base via recursive (texture is a scalar
	// passthrough — single-mod merge succeeds). Second candidate conflicts
	// with the same texture key → SuffixRename appends a renamed copy.
	let names: Vec<_> = merged
		.iter()
		.filter_map(child_identity)
		.filter(|id| id.key == "iconType")
		.map(|id| id.name.unwrap_or_default())
		.collect();
	assert!(names.iter().any(|n| n == "icon_x"), "names={names:?}");
	assert!(
		names
			.iter()
			.any(|n| n.starts_with("icon_x_") && n.contains("mod_b")),
		"expected suffix-renamed icon_x_mod_b, got names={names:?}"
	);
}

#[test]
fn merge_conflict_overlay_wins_under_overlay_policy() {
	let base: Vec<AstStatement> = vec![named_block("iconType", "icon_x", vec![])];
	let mod_a = vec![named_block(
		"iconType",
		"icon_x",
		vec![assignment("texture", string_val("a.dds"))],
	)];
	let mod_b = vec![named_block(
		"iconType",
		"icon_x",
		vec![assignment("texture", string_val("b.dds"))],
	)];

	let policies = MergePolicies {
		named_container: NamedContainerPolicy::OverlayWins,
		..Default::default()
	};
	let merged = merge_named_container_bodies(
		&base,
		&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
		&policies,
	)
	.expect("merge");

	// Only one icon_x kept; its texture is mod_b's value (last in the list).
	let icons: Vec<_> = merged
		.iter()
		.filter(|s| {
			child_identity(s)
				.map(|i| i.key == "iconType")
				.unwrap_or(false)
		})
		.collect();
	assert_eq!(icons.len(), 1, "OverlayWins must keep only one entry");
	let inner = match icons[0] {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => items,
		_ => panic!("expected block"),
	};
	let texture = inner.iter().find_map(|s| match s {
		AstStatement::Assignment {
			key,
			value: AstValue::Scalar { value: sv, .. },
			..
		} if key == "texture" => Some(sv.as_text()),
		_ => None,
	});
	assert_eq!(texture.as_deref(), Some("b.dds"));
}

#[test]
fn merge_conflict_policy_unions_sibling_blocks_that_share_non_empty_base() {
	let base = vec![named_block(
		"government",
		"shared_reform",
		vec![assignment("monarchy", scalar("yes"))],
	)];
	let mod_a = vec![named_block(
		"government",
		"shared_reform",
		vec![
			assignment("monarchy", scalar("yes")),
			assignment("has_parliament", scalar("yes")),
		],
	)];
	let mod_b = vec![named_block(
		"government",
		"shared_reform",
		vec![
			assignment("monarchy", scalar("yes")),
			assignment("has_states_general", scalar("yes")),
		],
	)];

	let merged = merge_named_container_bodies(
		&base,
		&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
		&default_policies(),
	)
	.expect("shared-base extensions should union");

	let body = block_items(&merged[0]);
	for key in ["monarchy", "has_parliament", "has_states_general"] {
		assert!(
			body.iter().any(
				|stmt| matches!(stmt, AstStatement::Assignment { key: found, .. } if found == key)
			),
			"missing {key}: {body:#?}"
		);
	}
}

#[test]
fn merge_conflict_policy_takes_superset_sibling_block() {
	let base = vec![named_block(
		"government",
		"shared_reform",
		vec![assignment("monarchy", scalar("yes"))],
	)];
	let mod_a = vec![named_block(
		"government",
		"shared_reform",
		vec![
			assignment("monarchy", scalar("yes")),
			assignment("has_parliament", scalar("yes")),
		],
	)];
	let mod_b = vec![named_block(
		"government",
		"shared_reform",
		vec![
			assignment("monarchy", scalar("yes")),
			assignment("has_parliament", scalar("yes")),
			assignment("has_states_general", scalar("yes")),
		],
	)];

	let merged = merge_named_container_bodies(
		&base,
		&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
		&default_policies(),
	)
	.expect("strict superset should win");

	let body = block_items(&merged[0]);
	assert!(
		body.iter().any(
			|stmt| matches!(stmt, AstStatement::Assignment { key, .. } if key == "has_states_general")
		),
		"superset-only child should survive: {body:#?}"
	);
}

#[test]
fn merge_conflict_policy_keeps_disjoint_sibling_blocks_conflicting() {
	let mod_a = vec![named_block(
		"government",
		"shared_reform",
		vec![assignment("has_parliament", scalar("yes"))],
	)];
	let mod_b = vec![named_block(
		"government",
		"shared_reform",
		vec![assignment("has_states_general", scalar("yes"))],
	)];

	let result = merge_named_container_bodies(
		&[],
		&[("mod_a", mod_a.as_slice()), ("mod_b", mod_b.as_slice())],
		&default_policies(),
	);

	assert_eq!(result, Err(NamedContainerMergeError::UnresolvableConflict));
}

#[test]
fn replace_block_named_container_resolves_via_merge() {
	// End-to-end: two mods produce ReplaceBlock for the same windowType
	// with different inner additions. Default Recurse re-diffs each body
	// and the additive iconType inserts at distinct identities coexist —
	// no conflict, single auto-merged ReplaceBlock.
	let base_stmt =
		body_to_window_type_block("hre", vec![named_block("iconType", "ico_x", vec![])]);
	let mod_a_stmt = body_to_window_type_block(
		"hre",
		vec![
			named_block("iconType", "ico_x", vec![]),
			named_block("iconType", "ico_a", vec![]),
		],
	);
	let mod_b_stmt = body_to_window_type_block(
		"hre",
		vec![
			named_block("iconType", "ico_x", vec![]),
			named_block("iconType", "ico_b", vec![]),
		],
	);

	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "windowType".into(),
		old_statement: base_stmt.clone(),
		new_statement: mod_a_stmt,
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec!["root".into()],
		key: "windowType".into(),
		old_statement: base_stmt,
		new_statement: mod_b_stmt,
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&default_policies(),
	);

	assert_eq!(
		result.conflicts.len(),
		0,
		"expected merge, got conflicts: {:?}",
		result.conflicts
	);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged { strategy, .. } => {
			assert!(
				matches!(
					strategy.as_str(),
					"recursive_block_merge" | "named_container_union"
				),
				"unexpected strategy: {strategy}"
			);
		}
		other => panic!("expected AutoMerged, got: {other:?}"),
	}
}

// ---- Rename cross-mod resolution ---------------------------------------

#[test]
fn rename_rewrites_set_value_at_old_key() {
	// mod_a renames X→Y; mod_b sets a value at X.
	// Expected: mod_b's SetValue is rewritten to address Y, no conflict.
	let rename = ClausewitzPatch::Rename {
		path: vec![],
		old_key: "feudalism_reform".into(),
		new_key: "EE_feudalism_reform".into(),
	};
	let set = ClausewitzPatch::SetValue {
		path: vec![],
		key: "feudalism_reform".into(),
		old_value: scalar("a"),
		new_value: scalar("b"),
	};
	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![rename]),
			("mod_b".into(), 2, vec![set]),
		],
		&default_policies(),
	);
	assert_eq!(
		result.conflicts.len(),
		0,
		"expected no conflicts, got: {:?}",
		result.conflicts
	);
	// Both the Rename and the rewritten SetValue should be resolved.
	let has_rename = result
		.resolved
		.iter()
		.any(|r| matches!(r, PatchResolution::Resolved(ClausewitzPatch::Rename { .. })));
	let rewritten_set = result.resolved.iter().any(|r| match r {
		PatchResolution::Resolved(ClausewitzPatch::SetValue { key, .. }) => {
			key == "EE_feudalism_reform"
		}
		_ => false,
	});
	assert!(has_rename, "expected Rename in resolved");
	assert!(rewritten_set, "expected SetValue rewritten to new key");
}

#[test]
fn rename_rewrites_nested_path_segment() {
	// mod_a renames X→Y at root; mod_b inserts a node at path [X].
	let rename = ClausewitzPatch::Rename {
		path: vec![],
		old_key: "feudalism_reform".into(),
		new_key: "EE_feudalism_reform".into(),
	};
	let insert = ClausewitzPatch::InsertNode {
		path: vec!["feudalism_reform".into()],
		key: "modifier".into(),
		statement: assignment("modifier", scalar("centralization_modifier")),
	};
	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![rename]),
			("mod_b".into(), 2, vec![insert]),
		],
		&default_policies(),
	);
	assert_eq!(
		result.conflicts.len(),
		0,
		"expected no conflicts, got: {:?}",
		result.conflicts
	);
	let rewritten = result.resolved.iter().any(|r| match r {
		PatchResolution::Resolved(ClausewitzPatch::InsertNode { path, .. }) => {
			path == &vec!["EE_feudalism_reform".to_string()]
		}
		_ => false,
	});
	assert!(rewritten, "expected nested InsertNode path rewritten");
}

#[test]
fn conflicting_renames_emit_conflict() {
	// Two mods rename the same (path, X) to different new keys.
	let rename_a = ClausewitzPatch::Rename {
		path: vec![],
		old_key: "X".into(),
		new_key: "Y1".into(),
	};
	let rename_b = ClausewitzPatch::Rename {
		path: vec![],
		old_key: "X".into(),
		new_key: "Y2".into(),
	};
	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![rename_a]),
			("mod_b".into(), 2, vec![rename_b]),
		],
		&default_policies(),
	);
	assert_eq!(
		result.conflicts.len(),
		1,
		"expected one conflict, got: {:?}",
		result.conflicts
	);
	match &result.conflicts[0] {
		PatchResolution::Conflict { reason, .. } => {
			assert!(reason.contains("conflicting renames"));
		}
		other => panic!("expected Conflict, got: {other:?}"),
	}
}

#[test]
fn convergent_renames_resolve() {
	// Two mods rename the same (path, X) to the same new key.
	let mk = || ClausewitzPatch::Rename {
		path: vec![],
		old_key: "X".into(),
		new_key: "Y".into(),
	};
	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![mk()]),
			("mod_b".into(), 2, vec![mk()]),
		],
		&default_policies(),
	);
	assert_eq!(result.conflicts.len(), 0);
	assert_eq!(result.resolved.len(), 1);
}

// -----------------------------------------------------------------------
// BlockPatchPolicy::Recurse — date-keyed history deep merge
// -----------------------------------------------------------------------

fn recurse_policies() -> MergePolicies {
	MergePolicies {
		block_patch: BlockPatchPolicy::Recurse,
		..Default::default()
	}
}

#[test]
fn recurse_merges_disjoint_inserted_block_bodies() {
	let body_a = vec![
		assignment("start", number("1530")),
		assignment_block("objective_a", vec![assignment("type", scalar("alpha"))]),
	];
	let body_b = vec![
		assignment("start", number("1530")),
		assignment_block("objective_b", vec![assignment("type", scalar("beta"))]),
	];

	let mk = |body: Vec<AstStatement>| ClausewitzPatch::InsertNode {
		path: vec![],
		key: "age_of_reformation".into(),
		statement: assignment_block("age_of_reformation", body),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![mk(body_a)]),
			("mod_b".into(), 2, vec![mk(body_b)]),
		],
		&recurse_policies(),
	);

	assert_eq!(
		result.conflicts.len(),
		0,
		"got conflicts: {:?}",
		result.conflicts
	);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: ClausewitzPatch::InsertNode { statement, .. },
			strategy,
			..
		} => {
			assert_eq!(strategy, "recursive_insert_merge");
			let body = match statement {
				AstStatement::Assignment {
					value: AstValue::Block { items, .. },
					..
				} => items,
				other => panic!("expected block, got {other:?}"),
			};
			let has_start = body.iter().any(|s| matches!(s,
				AstStatement::Assignment { key, value: AstValue::Scalar { value: ScalarValue::Number(v), .. }, .. }
				if key == "start" && v == "1530"));
			let has_objective_a = body
				.iter()
				.any(|s| matches!(s, AstStatement::Assignment { key, .. } if key == "objective_a"));
			let has_objective_b = body
				.iter()
				.any(|s| matches!(s, AstStatement::Assignment { key, .. } if key == "objective_b"));
			assert!(has_start, "expected shared scalar to survive, got {body:?}");
			assert!(
				has_objective_a && has_objective_b,
				"expected both disjoint block children, got {body:?}"
			);
		}
		other => panic!("expected AutoMerged InsertNode, got: {other:?}"),
	}
}

#[test]
fn recurse_insert_scalar_conflict_bubbles_up() {
	let mk = |start: &str| ClausewitzPatch::InsertNode {
		path: vec![],
		key: "age_of_reformation".into(),
		statement: assignment_block(
			"age_of_reformation",
			vec![assignment("start", number(start))],
		),
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![mk("1530")]),
			("mod_b".into(), 2, vec![mk("1540")]),
		],
		&recurse_policies(),
	);

	assert_eq!(
		result.conflicts.len(),
		1,
		"expected one bubbled sub-conflict, got resolved={:?} conflicts={:?}",
		result.resolved,
		result.conflicts,
	);
	match &result.conflicts[0] {
		PatchResolution::Conflict { reason, .. } => {
			assert!(
				reason.contains("deep merge of inserted block")
					&& reason.contains("sibling mods inserted divergent statements"),
				"unexpected reason: {reason}"
			);
		}
		other => panic!("expected Conflict, got: {other:?}"),
	}
}

#[test]
fn recurse_merges_disjoint_date_block_changes() {
	// Vanilla:    1444.11.11 = { owner = BYZ }
	// mod_a:      1444.11.11 = { owner = OTT }                        (changes owner)
	// mod_b:      1444.11.11 = { owner = BYZ controller = OTT }       (adds controller)
	//
	// Each mod's diff is against vanilla (chained predecessor at this
	// address only has vanilla). Expected: deep-merge produces
	// `{ owner = OTT controller = OTT }`.
	let vanilla_body = vec![assignment("owner", scalar("BYZ"))];
	let a_body = vec![assignment("owner", scalar("OTT"))];
	let b_body = vec![
		assignment("owner", scalar("BYZ")),
		assignment("controller", scalar("OTT")),
	];

	let vanilla_stmt = assignment_block("1444.11.11", vanilla_body);
	let a_stmt = assignment_block("1444.11.11", a_body);
	let b_stmt = assignment_block("1444.11.11", b_body);

	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "1444.11.11".into(),
		old_statement: vanilla_stmt.clone(),
		new_statement: a_stmt,
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "1444.11.11".into(),
		// mod_b's chained diff base is mod_a's new content; carry that
		// to mirror how `compute_chained_patches` produces patches.
		old_statement: assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]),
		new_statement: b_stmt,
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&recurse_policies(),
	);

	assert_eq!(
		result.conflicts.len(),
		0,
		"got conflicts: {:?}",
		result.conflicts
	);
	assert_eq!(result.resolved.len(), 1);
	match &result.resolved[0] {
		PatchResolution::AutoMerged {
			result: ClausewitzPatch::ReplaceBlock { new_statement, .. },
			strategy,
			..
		} => {
			assert_eq!(strategy, "recursive_block_merge");
			let body = match new_statement {
				AstStatement::Assignment {
					value: AstValue::Block { items, .. },
					..
				} => items,
				other => panic!("expected block, got {other:?}"),
			};
			// mod_a contributes owner=OTT (vs vanilla owner=BYZ).
			// mod_b contributes controller=OTT addition.
			let has_owner_ott = body.iter().any(|s| matches!(s,
					AstStatement::Assignment { key, value: AstValue::Scalar { value: ScalarValue::Identifier(v), .. }, .. }
					if key == "owner" && v == "OTT"));
			let has_controller_ott = body.iter().any(|s| matches!(s,
					AstStatement::Assignment { key, value: AstValue::Scalar { value: ScalarValue::Identifier(v), .. }, .. }
					if key == "controller" && v == "OTT"));
			assert!(
				has_owner_ott,
				"expected merged body to keep owner=OTT, got {body:?}"
			);
			assert!(
				has_controller_ott,
				"expected merged body to add controller=OTT, got {body:?}"
			);
		}
		other => panic!("expected AutoMerged ReplaceBlock, got: {other:?}"),
	}
}

#[test]
fn recurse_sibling_scalar_conflict_bubbles_up() {
	// Both sibling mods change the same `owner` scalar inside the same
	// date block to *different* tags. Recurse re-diffs each body against
	// the common ancestor, producing two SetValues at the nested address
	// — and per `ScalarMergePolicy::Conflict` the engine must surface a
	// conflict instead of silently choosing the highest-precedence value.
	let vanilla_body = vec![assignment("owner", scalar("BYZ"))];
	let vanilla_stmt = assignment_block("1444.11.11", vanilla_body);
	let a_stmt = assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]);
	let b_stmt = assignment_block("1444.11.11", vec![assignment("owner", scalar("MAM"))]);

	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "1444.11.11".into(),
		old_statement: vanilla_stmt.clone(),
		new_statement: a_stmt.clone(),
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "1444.11.11".into(),
		old_statement: a_stmt, // chained diff base
		new_statement: b_stmt,
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&recurse_policies(),
	);

	assert_eq!(
		result.conflicts.len(),
		1,
		"expected one bubbled sub-conflict, got resolved={:?} conflicts={:?}",
		result.resolved,
		result.conflicts,
	);
	match &result.conflicts[0] {
		PatchResolution::Conflict { reason, .. } => {
			assert!(
				reason.contains("unresolved sub-conflict")
					|| reason.contains("sibling mods set the same scalar"),
				"unexpected reason: {reason}"
			);
		}
		other => panic!("expected Conflict, got: {other:?}"),
	}
}

#[test]
fn recurse_default_policy_emits_cross_kind_conflict_on_owner() {
	// mod_a sets `owner` to a new value while mod_b removes `owner`
	// entirely and adds a sibling `controller`. Recurse re-diffs each
	// body and the cross-kind detector surfaces the conflicting intent
	// at `owner` as a sub-conflict (SetValue vs RemoveNode at the same
	// raw key) which bubbles up to the outer ReplaceBlock.
	let vanilla_stmt = assignment_block("1444.11.11", vec![assignment("owner", scalar("BYZ"))]);
	let a_stmt = assignment_block("1444.11.11", vec![assignment("owner", scalar("OTT"))]);
	let b_stmt = assignment_block("1444.11.11", vec![assignment("controller", scalar("OTT"))]);

	let patch_a = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "1444.11.11".into(),
		old_statement: vanilla_stmt.clone(),
		new_statement: a_stmt,
	};
	let patch_b = ClausewitzPatch::ReplaceBlock {
		path: vec![],
		key: "1444.11.11".into(),
		old_statement: vanilla_stmt,
		new_statement: b_stmt,
	};

	let result = merge_patch_sets_with_defer(
		vec![
			("mod_a".into(), 1, vec![patch_a]),
			("mod_b".into(), 2, vec![patch_b]),
		],
		&default_policies(),
	);

	assert_eq!(result.conflicts.len(), 1);
	match &result.conflicts[0] {
		PatchResolution::Conflict { reason, .. } => {
			assert!(
				reason.contains("incompatible patch kinds")
					|| reason.contains("unresolved sub-conflict"),
				"unexpected reason: {reason}"
			);
			assert!(reason.contains("owner"), "reason missing key: {reason}");
		}
		other => panic!("expected Conflict, got: {other:?}"),
	}
}
