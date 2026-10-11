//! Product regressions for transformations spanning definitions and their uses.
use foch::game::eu4::Eu4;
use foch::game::eu4::base::snapshot::{
	BASE_DATA_DIR_ENV, BaseDataSource, build_base_snapshot, install_built_snapshot,
};
use foch::input::request::InputRequest;
use foch::input::{Config, FileFilter};
use foch::merge::*;
use foch::model::MergeReportStatus;
use std::fs;
use std::path::{Path, PathBuf};

static BASE_DATA_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write(root: &Path, relative: &str, text: &str) {
	let path = root.join(relative);
	fs::create_dir_all(path.parent().unwrap()).unwrap();
	fs::write(path, text).unwrap();
}

struct BaseEnvironment(Option<std::ffi::OsString>);

impl Drop for BaseEnvironment {
	fn drop(&mut self) {
		unsafe {
			match &self.0 {
				Some(value) => std::env::set_var(BASE_DATA_DIR_ENV, value),
				None => std::env::remove_var(BASE_DATA_DIR_ENV),
			}
		}
	}
}

fn options(out_dir: PathBuf) -> MergeAnalysisOptions {
	MergeAnalysisOptions {
		out_dir,
		include_game_base: true,
		include_base: false,
		gui_scroll_merge: false,
		force: false,
		ignore_replace_path: false,
		dep_overrides: Vec::new(),
		resolution_config_path: None,
		interactive_conflict_handler: None,
		interactive_resolution_config_path: None,
		playset_fingerprint: None,
		provenance: false,
		merge_workers: std::num::NonZeroUsize::new(2).unwrap(),
		retained_paths: None,
	}
}

#[test]
fn culture_rename_preserves_independent_edit_and_all_bound_references() {
	assert_culture_adaptation(CultureCase::Compatible);
}

#[test]
fn culture_modifier_and_mechanism_compose_with_a_dependent_translation() {
	assert_culture_adaptation(CultureCase::DependentTranslation);
}

#[test]
fn culture_conflict_withholds_related_references_but_keeps_unrelated_outputs() {
	assert_culture_adaptation(CultureCase::Conflict);
}

#[test]
fn unsupported_culture_use_is_reviewable_and_cannot_partly_emit() {
	assert_culture_adaptation(CultureCase::UnsupportedUse);
}

#[test]
fn force_cannot_emit_an_incomplete_culture_adaptation() {
	assert_culture_adaptation(CultureCase::ForceUnsupportedUse);
}

#[test]
fn ambiguous_culture_identity_is_reviewable_and_cannot_partly_emit() {
	assert_culture_adaptation(CultureCase::Ambiguous);
}

#[test]
fn culture_rename_from_later_filename_adapts_vanilla_references() {
	assert_culture_adaptation(CultureCase::LaterFilename);
}

#[test]
fn culture_move_checks_group_predicates_even_when_identifiers_resolve() {
	assert_culture_adaptation(CultureCase::GroupPredicate);
}

#[test]
fn independent_rename_and_group_move_still_check_group_predicates() {
	assert_culture_adaptation(CultureCase::SiblingGroupMove);
}

#[test]
fn culture_move_with_equivalent_groups_preserves_independent_edit() {
	assert_culture_adaptation(CultureCase::EquivalentGroup);
}

#[test]
fn culture_move_checks_independent_changes_to_source_group_modifiers() {
	assert_culture_adaptation(CultureCase::GroupModifier);
}

#[test]
fn culture_move_checks_dynamic_group_predicates() {
	assert_culture_adaptation(CultureCase::DynamicGroup);
}

#[test]
fn culture_move_checks_group_keys_in_government_reforms() {
	assert_culture_adaptation(CultureCase::GroupKey);
}

#[test]
fn culture_move_does_not_bind_an_unrelated_symbol_with_the_group_name() {
	assert_culture_adaptation(CultureCase::UnrelatedGroupKey);
}

#[test]
fn unchanged_identity_group_move_needs_complete_reference_inventory() {
	assert_culture_adaptation(CultureCase::RetainedGroupMove);
}

#[test]
fn culture_rename_cannot_claim_closed_references_with_user_ignored_scripts() {
	assert_culture_adaptation(CultureCase::IgnoredReferences);
}

#[test]
fn culture_group_move_cannot_skip_ignored_group_predicates() {
	assert_culture_adaptation(CultureCase::IgnoredGroupPredicate);
}

#[test]
fn dependency_introduced_culture_move_checks_group_behavior() {
	assert_culture_adaptation(CultureCase::IntroducedGroupMove);
}

#[test]
fn accepted_culture_repair_is_an_analysis_overlay_and_preserves_original_bytes() {
	assert_culture_adaptation(CultureCase::Repair);
}

#[test]
fn stale_culture_repair_is_rejected_before_any_output_is_emitted() {
	assert_culture_adaptation(CultureCase::StaleRepair);
}

#[test]
fn culture_repair_that_leaves_a_block_open_is_rejected() {
	assert_culture_adaptation(CultureCase::IncompleteRepair);
}

#[test]
fn accepted_culture_repair_source_drift_invalidates_commit() {
	assert_culture_adaptation(CultureCase::RepairDrift);
}

#[test]
fn reviewed_rename_with_changed_fields_composes_with_independent_modifier() {
	assert_culture_adaptation(CultureCase::ReviewedRename);
}

#[test]
fn reviewed_culture_mapping_requires_a_verified_vanilla_culture_ancestor() {
	assert_culture_adaptation(CultureCase::MissingReviewedAncestor);
}

#[test]
fn culture_adaptation_preserves_conditional_split_fusion_and_character_updates() {
	assert_culture_adaptation(CultureCase::ConditionalMechanism);
}

#[test]
fn culture_rename_withholds_unverified_script_parameter_flows() {
	assert_culture_adaptation(CultureCase::ParameterizedReference);
}

#[test]
fn culture_rename_withholds_forwarded_script_parameters_even_with_force() {
	assert_culture_adaptation(CultureCase::ForwardedParameter);
}

#[test]
fn culture_rename_does_not_bind_parameters_used_only_as_flags() {
	assert_culture_adaptation(CultureCase::FlagParameter);
}

#[test]
fn culture_rename_withholds_parameter_defaults_and_their_callers() {
	assert_culture_adaptation(CultureCase::ParameterDefault);
}

#[test]
fn culture_rename_audits_vanilla_script_parameters() {
	assert_culture_adaptation(CultureCase::BaseParameter);
}

#[test]
fn culture_rename_audits_trigger_parameters_separately_from_effects() {
	assert_culture_adaptation(CultureCase::TriggerParameter);
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CultureCase {
	Compatible,
	DependentTranslation,
	Conflict,
	UnsupportedUse,
	ForceUnsupportedUse,
	Ambiguous,
	LaterFilename,
	GroupPredicate,
	SiblingGroupMove,
	EquivalentGroup,
	GroupModifier,
	DynamicGroup,
	GroupKey,
	UnrelatedGroupKey,
	RetainedGroupMove,
	IgnoredReferences,
	IgnoredGroupPredicate,
	IntroducedGroupMove,
	Repair,
	StaleRepair,
	IncompleteRepair,
	RepairDrift,
	ReviewedRename,
	MissingReviewedAncestor,
	ConditionalMechanism,
	ParameterizedReference,
	ForwardedParameter,
	FlagParameter,
	ParameterDefault,
	BaseParameter,
	TriggerParameter,
}

fn assert_culture_adaptation(case: CultureCase) {
	let _lock = BASE_DATA_ENV_LOCK.lock().unwrap();
	let temp = tempfile::tempdir().unwrap();
	let root = temp.path();
	let _environment = BaseEnvironment(std::env::var_os(BASE_DATA_DIR_ENV));
	unsafe { std::env::set_var(BASE_DATA_DIR_ENV, root.join("base-data")) };
	let game = root.join("game");
	let base = "group = { graphical_culture = westerngfx old = { primary = AAA } destination = { primary = BBB } }\nother_group = { graphical_culture = westerngfx }\n";
	let with_destinations = format!(
		"{base} transformations = {{ split_a = {{ primary = CCC }} split_b = {{ primary = DDD }} fused = {{ primary = EEE }} }}\n"
	);
	let base = if case == CultureCase::ConditionalMechanism {
		with_destinations.as_str()
	} else {
		base
	};
	write(&game, "version.txt", "1.37.5\n");
	let vanilla = if case == CultureCase::IntroducedGroupMove {
		base.replace("old = { primary = AAA }", "")
	} else {
		base.to_owned()
	};
	if case != CultureCase::MissingReviewedAncestor {
		write(&game, "common/cultures/base.txt", &vanilla);
	}
	write(
		&game,
		"common/scripted_effects/base.txt",
		"base_effect = { add_prestige = 1 }\n",
	);
	write(
		&game,
		"common/scripted_triggers/base.txt",
		"base_culture_check = { primary_culture = old }\n",
	);
	if case == CultureCase::BaseParameter {
		write(
			&game,
			"common/scripted_effects/parameter.txt",
			"apply_c = { change_primary_culture = $CULTURE$ }\n",
		);
		write(
			&game,
			"events/parameter.txt",
			"country_event = { id = parameter.1 immediate = { apply_c = { CULTURE = old } } }\n",
		);
	}
	let built = build_base_snapshot(
		&Eu4,
		&game,
		Some("1.37.5"),
		&FileFilter::new(Eu4, &[]).unwrap(),
	)
	.unwrap();
	install_built_snapshot(
		&built.encoded_snapshot,
		BaseDataSource::Build,
		Some(built.snapshot_asset_name),
		Some(built.snapshot_sha256),
	)
	.unwrap();

	let mut renamed = base.replace("old =", "renamed =");
	let mut modified = base.replace(
		"primary = AAA",
		"primary = AAA country = { production_efficiency = 0.1 }",
	);
	if matches!(
		case,
		CultureCase::GroupPredicate
			| CultureCase::EquivalentGroup
			| CultureCase::GroupModifier
			| CultureCase::DynamicGroup
			| CultureCase::GroupKey
			| CultureCase::UnrelatedGroupKey
			| CultureCase::RetainedGroupMove
			| CultureCase::IgnoredGroupPredicate
	) {
		renamed = renamed.replace("renamed = { primary = AAA }", "").replace(
			"other_group = {",
			"other_group = { renamed = { primary = AAA }",
		);
	}
	if matches!(
		case,
		CultureCase::RetainedGroupMove | CultureCase::IgnoredGroupPredicate
	) {
		renamed = renamed.replace("renamed =", "old =");
	}
	if case == CultureCase::IntroducedGroupMove {
		renamed = modified
			.replace(
				"old = { primary = AAA country = { production_efficiency = 0.1 } }",
				"",
			)
			.replace(
				"other_group = {",
				"other_group = { old = { primary = AAA country = { production_efficiency = 0.1 } }",
			);
	}
	if case == CultureCase::GroupModifier {
		modified = modified.replacen(
			"group = { graphical_culture",
			"group = { country = { discipline = 0.1 } graphical_culture",
			1,
		);
	}
	if case == CultureCase::SiblingGroupMove {
		let moved = "old = { primary = AAA country = { production_efficiency = 0.1 } }";
		modified = modified
			.replace(moved, "")
			.replace("other_group = {", &format!("other_group = {{ {moved}"));
	}
	if case == CultureCase::Conflict {
		renamed = renamed.replace("primary = BBB", "primary = CCC");
		modified = modified.replace("primary = BBB", "primary = DDD");
	}
	if case == CultureCase::Ambiguous {
		renamed = renamed.replace(
			"renamed = { primary = AAA }",
			"renamed = { primary = AAA } other = { primary = AAA }",
		);
	}
	if matches!(
		case,
		CultureCase::Repair
			| CultureCase::StaleRepair
			| CultureCase::RepairDrift
			| CultureCase::IncompleteRepair
	) {
		renamed = renamed.replacen("group = {", "group", 1);
	}
	if case == CultureCase::IncompleteRepair {
		// The reviewed edit opens `group` again but leaves it unclosed; the
		// automatic repair that would close it is in the reviewed definition.
		renamed.remove(renamed.find("}\nother_group").unwrap());
	}
	if case == CultureCase::ReviewedRename {
		renamed = renamed.replace("primary = AAA", "primary = AAA male_names = { NewName }");
	}
	let effect = if case == CultureCase::ConditionalMechanism {
		r#"culture_mechanic = {
		if = { limit = { culture = old } change_culture = destination }
		every_owned_province = {
			if = { limit = { culture = old region = france_region } change_culture = split_a }
			else_if = { limit = { culture = old region = germany_region } change_culture = split_b }
		}
		if = { limit = { accepted_culture = old }
			remove_accepted_culture = old add_accepted_culture = split_a add_accepted_culture = split_b }
		if = { limit = { OR = { primary_culture = split_a primary_culture = split_b } }
			change_primary_culture = fused add_accepted_culture = fused
			set_ruler_culture = fused set_heir_culture = fused set_consort_culture = fused }
		if = { limit = { ruler_culture = old } set_ruler_culture = old }
		set_country_flag = old
	}"#
	} else if matches!(
		case,
		CultureCase::UnsupportedUse | CultureCase::ForceUnsupportedUse
	) {
		"culture_mechanic = { unknown_culture_operation = old }\n"
	} else if matches!(
		case,
		CultureCase::GroupPredicate
			| CultureCase::SiblingGroupMove
			| CultureCase::IntroducedGroupMove
			| CultureCase::IgnoredGroupPredicate
	) {
		"culture_mechanic = { if = { limit = { culture = old culture_group = group } change_culture = destination } set_country_flag = old }\n"
	} else if case == CultureCase::DynamicGroup {
		"culture_mechanic = { if = { limit = { culture = old culture_group = ROOT } change_culture = destination } set_country_flag = old }\n"
	} else {
		"culture_mechanic = { if = { limit = { culture = old } change_culture = destination } set_country_flag = old }\n"
	};
	write(root, "mods/rename/descriptor.mod", "name=\"Rename\"\n");
	write(root, "mods/bonus/descriptor.mod", "name=\"Bonus\"\n");
	if case == CultureCase::IntroducedGroupMove {
		write(
			root,
			"mods/rename/descriptor.mod",
			"name=\"Rename\"\ndependencies={\"Bonus\"}\n",
		);
	}
	if case == CultureCase::GroupKey {
		write(
			root,
			"mods/bonus/common/government_reforms/assimilation.txt",
			"reform = { assimilation_cultures = { group = { discipline = 0.1 } } }\n",
		);
	}
	if case == CultureCase::UnrelatedGroupKey {
		write(
			root,
			"mods/bonus/common/scripted_effects/util.txt",
			"group = { add_prestige = 1 }\n",
		);
	}
	let renamed_path = if case == CultureCase::LaterFilename {
		"mods/rename/common/cultures/zzz.txt"
	} else {
		"mods/rename/common/cultures/base.txt"
	};
	write(root, renamed_path, &renamed);
	write(root, "mods/bonus/common/cultures/base.txt", &modified);
	write(
		root,
		"mods/bonus/common/scripted_effects/mechanic.txt",
		effect,
	);
	write(
		root,
		"mods/bonus/gfx/unrelated.dds",
		"unrelated binary passthrough",
	);
	write(
		root,
		"mods/bonus/events/unrelated.txt",
		"country_event = { id = unrelated.1 immediate = { set_country_flag = old } }\n",
	);
	if matches!(
		case,
		CultureCase::ParameterizedReference
			| CultureCase::ForwardedParameter
			| CultureCase::FlagParameter
			| CultureCase::ParameterDefault
			| CultureCase::TriggerParameter
	) {
		let effect = if matches!(
			case,
			CultureCase::FlagParameter | CultureCase::TriggerParameter
		) {
			"apply_c = { set_country_flag = $CULTURE$ }\nunused_c = { change_primary_culture = $CULTURE$ }\n"
		} else if case == CultureCase::ParameterDefault {
			"apply_c = { change_primary_culture = $CULTURE|old$ }\n"
		} else {
			"apply_c = { change_primary_culture = $CULTURE$ }\n"
		};
		write(
			root,
			"mods/bonus/common/scripted_effects/parameter.txt",
			effect,
		);
		if matches!(
			case,
			CultureCase::FlagParameter | CultureCase::TriggerParameter
		) {
			write(
				root,
				"mods/bonus/common/scripted_triggers/parameter.txt",
				"apply_c = { primary_culture = $CULTURE$ }\n",
			);
		}
		let caller = if case == CultureCase::ForwardedParameter {
			write(
				root,
				"mods/bonus/common/scripted_effects/relay.txt",
				"relay_c = { apply_c = { CULTURE = $VALUE$ } }\n",
			);
			"country_event = { id = parameter.1 immediate = { relay_c = { VALUE = old } } }\n"
		} else if case == CultureCase::ParameterDefault {
			"country_event = { id = parameter.1 immediate = { apply_c = yes } }\n"
		} else if case == CultureCase::TriggerParameter {
			"country_event = { id = parameter.1 trigger = { apply_c = { CULTURE = old } } immediate = { add_prestige = 1 } }\n"
		} else {
			"country_event = { id = parameter.1 immediate = { apply_c = { CULTURE = old } } }\n"
		};
		write(root, "mods/bonus/events/parameter.txt", caller);
	}
	let mut project = format!(
		"[project]\ngame=\"eu4\"\ngame_path='{}'\n[[project.mods]]\nid=\"rename\"\npath=\"mods/rename\"\n[[project.mods]]\nid=\"bonus\"\npath=\"mods/bonus\"\n",
		game.display()
	);
	let translated = renamed.replace("primary = AAA", "primary = AAA male_names = { \"译名\" }");
	if case == CultureCase::DependentTranslation {
		write(
			root,
			"mods/translation/descriptor.mod",
			"name=\"Translation\"\ndependencies={\"Rename\"}\n",
		);
		write(
			root,
			"mods/translation/common/cultures/base.txt",
			&translated,
		);
		project.push_str("[[project.mods]]\nid=\"translation\"\npath=\"mods/translation\"\n");
	}
	if case == CultureCase::IntroducedGroupMove {
		project = project.replace(
			"id=\"rename\"\npath=\"mods/rename\"\n[[project.mods]]\nid=\"bonus\"\npath=\"mods/bonus\"",
			"id=\"bonus\"\npath=\"mods/bonus\"\n[[project.mods]]\nid=\"rename\"\npath=\"mods/rename\"",
		);
	}
	if matches!(
		case,
		CultureCase::Repair
			| CultureCase::StaleRepair
			| CultureCase::RepairDrift
			| CultureCase::IncompleteRepair
			| CultureCase::ReviewedRename
			| CultureCase::MissingReviewedAncestor
	) {
		use foch::project::{CultureRenameEntry, CultureRepairEntry, Project, SourceEdit};
		use sha2::{Digest, Sha256};
		let mut manifest: Project = toml::from_str(&project).unwrap();
		let sha256 = format!("{:x}", Sha256::digest(renamed.as_bytes()));
		if matches!(
			case,
			CultureCase::ReviewedRename | CultureCase::MissingReviewedAncestor
		) {
			manifest.cultures.renames.push(CultureRenameEntry {
				from: "old".into(),
				to: "renamed".into(),
				mod_id: "rename".into(),
				file: "common/cultures/base.txt".into(),
				sha256,
			});
		} else {
			manifest.cultures.repairs.push(CultureRepairEntry {
				mod_id: "rename".into(),
				file: "common/cultures/base.txt".into(),
				sha256,
				edits: vec![SourceEdit {
					start: 5,
					end: 5,
					expected: "".into(),
					replacement: " = {".into(),
				}],
			});
		}
		project = toml::to_string(&manifest).unwrap();
	}
	write(root, "foch.toml", &project);
	if case == CultureCase::StaleRepair {
		write(
			root,
			renamed_path,
			&format!("{renamed}\n# Updated by author\n"),
		);
	}
	let out = root.join("output");
	let mut analysis_options = options(out.clone());
	analysis_options.force = matches!(
		case,
		CultureCase::ForceUnsupportedUse | CultureCase::ForwardedParameter
	);
	if case == CultureCase::RetainedGroupMove {
		analysis_options.retained_paths = Some(
			["common/cultures/base.txt"]
				.map(|path| foch::model::GamePathBuf::parse(path).expect("game path"))
				.into_iter()
				.collect(),
		);
	}
	let mut config = Config::default();
	if matches!(
		case,
		CultureCase::IgnoredReferences | CultureCase::IgnoredGroupPredicate
	) {
		config
			.extra_ignore_patterns
			.push("common/scripted_effects/mechanic.txt".into());
	}
	let result = analyze_merge(
		InputRequest::from_manifest_path(root.join("foch.toml"), config),
		analysis_options,
		&NoopProgressObserver,
		&CancellationToken::new(),
	);
	if case == CultureCase::MissingReviewedAncestor {
		assert!(
			matches!(result, Err(ref error) if error.to_string().contains("cannot verify reviewed culture mappings")),
			"missing ancestor must reject the reviewed mapping"
		);
		assert!(!out.exists());
		return;
	}
	if case == CultureCase::StaleRepair {
		assert!(
			matches!(result, Err(ref error) if error.to_string().contains("stale culture decision"))
		);
		assert!(!out.exists());
		return;
	}
	if case == CultureCase::IncompleteRepair {
		assert!(
			matches!(result, Err(ref error) if error.to_string().contains("culture repair still has parse errors"))
		);
		assert!(!out.exists());
		assert_eq!(
			fs::read_to_string(root.join(renamed_path)).unwrap(),
			renamed
		);
		return;
	}
	let analyzed = result.unwrap();
	if case == CultureCase::RepairDrift {
		assert_eq!(
			analyzed.analysis().report().status,
			MergeReportStatus::Ready
		);
		write(
			root,
			renamed_path,
			&format!("{renamed}\n# Updated after review\n"),
		);
		assert!(
			analyzed
				.commit(CommitAuthorization::EmptyTargetOnly)
				.is_err()
		);
		assert!(!out.exists());
		return;
	}
	if case == CultureCase::RetainedGroupMove {
		assert_eq!(analyzed.review_summary().engine_failure, 0);
		assert!(analyzed.review_summary().needs_user_choice > 0);
		assert!(
			analyzed
				.list_units()
				.iter()
				.all(|unit| unit.output_paths.is_empty())
		);
		return;
	}
	if matches!(
		case,
		CultureCase::Conflict
			| CultureCase::UnsupportedUse
			| CultureCase::ForceUnsupportedUse
			| CultureCase::Ambiguous
			| CultureCase::GroupPredicate
			| CultureCase::SiblingGroupMove
			| CultureCase::GroupModifier
			| CultureCase::DynamicGroup
			| CultureCase::GroupKey
			| CultureCase::RetainedGroupMove
			| CultureCase::IntroducedGroupMove
			| CultureCase::IgnoredReferences
			| CultureCase::IgnoredGroupPredicate
			| CultureCase::ParameterizedReference
			| CultureCase::ForwardedParameter
			| CultureCase::ParameterDefault
			| CultureCase::BaseParameter
			| CultureCase::TriggerParameter
	) {
		assert_eq!(
			analyzed.analysis().report().status,
			MergeReportStatus::PartialSuccess
		);
		assert_eq!(analyzed.review_summary().engine_failure, 0);
		assert!(analyzed.review_summary().needs_user_choice > 0);
		if matches!(
			case,
			CultureCase::UnsupportedUse | CultureCase::ForceUnsupportedUse
		) {
			assert!(
				analyzed
					.list_units()
					.iter()
					.flat_map(|unit| &unit.notes)
					.any(|note| note.contains("bonus:common/scripted_effects/mechanic.txt:1:"))
			);
		}
		for unit in analyzed.list_units().iter().filter(|unit| {
			unit.path.as_str().starts_with("common/")
				&& unit.path.as_str() != "common/scripted_effects/base.txt"
		}) {
			assert!(
				unit.output_paths.is_empty(),
				"incomplete adaptation emitted: {unit:?}"
			);
			assert!(
				!matches!(
					unit.disposition,
					MergeDisposition::Safe | MergeDisposition::Copy
				),
				"{unit:?}"
			);
		}
		if matches!(
			case,
			CultureCase::ParameterizedReference
				| CultureCase::ForwardedParameter
				| CultureCase::ParameterDefault
				| CultureCase::BaseParameter
				| CultureCase::TriggerParameter
		) {
			assert!(
				analyzed.list_units().iter().any(|unit| {
					unit.path.as_str() == "events/parameter.txt" && unit.output_paths.is_empty()
				}),
				"parameter caller must be withheld with the renamed definition"
			);
		}
		analyzed
			.commit(CommitAuthorization::EmptyTargetOnly)
			.unwrap();
		if matches!(
			case,
			CultureCase::ParameterizedReference
				| CultureCase::ForwardedParameter
				| CultureCase::ParameterDefault
				| CultureCase::BaseParameter
				| CultureCase::TriggerParameter
		) {
			assert!(!out.join("events/parameter.txt").exists());
		}
		assert_eq!(
			fs::read_to_string(out.join("gfx/unrelated.dds")).unwrap(),
			"unrelated binary passthrough"
		);
		assert!(
			fs::read_to_string(out.join("events/unrelated.txt"))
				.unwrap()
				.contains("set_country_flag = old")
		);
		assert!(!out.join("common/scripted_triggers/base.txt").exists());
		return;
	}
	assert_eq!(
		analyzed.analysis().report().status,
		MergeReportStatus::Ready,
		"a culture identity rename must compose with the independent modifier: {:?}",
		analyzed.analysis().report()
	);
	let culture_output = analyzed
		.list_units()
		.iter()
		.find(|unit| unit.path.as_str().starts_with("common/cultures/"))
		.and_then(|unit| unit.output_path.clone())
		.expect("culture output");
	let effect_output = analyzed
		.list_units()
		.iter()
		.find(|unit| unit.path.as_str().starts_with("common/scripted_effects/"))
		.and_then(|unit| unit.output_path.clone())
		.expect("effect output");
	let base_reference_output = analyzed
		.list_units()
		.iter()
		.find(|unit| unit.path.as_str().starts_with("common/scripted_triggers/"))
		.and_then(|unit| unit.output_path.clone())
		.expect("rewritten vanilla trigger output");
	let review_decisions = analyzed
		.list_units()
		.iter()
		.map(|unit| (unit.path.clone(), unit.disposition, unit.notes.clone()))
		.collect::<Vec<_>>();
	analyzed
		.commit(CommitAuthorization::EmptyTargetOnly)
		.unwrap();
	let cultures = fs::read_to_string(culture_output.to_path(&out)).unwrap();
	assert!(cultures.contains("renamed ="), "{cultures}");
	assert!(!cultures.contains("old ="), "{cultures}");
	assert!(
		cultures.contains("production_efficiency = 0.1"),
		"{cultures}"
	);
	if case == CultureCase::DependentTranslation {
		assert!(cultures.contains("译名"), "{cultures}");
		assert_eq!(
			fs::read_to_string(root.join("mods/translation/common/cultures/base.txt")).unwrap(),
			translated
		);
	}
	let emitted = fs::read_to_string(effect_output.to_path(&out)).unwrap();
	assert!(emitted.contains("culture = renamed"), "{emitted}");
	assert!(
		emitted.contains("change_culture = destination"),
		"{emitted}"
	);
	assert!(emitted.contains("set_country_flag = old"), "{emitted}");
	if case == CultureCase::FlagParameter {
		assert!(
			fs::read_to_string(out.join("events/parameter.txt"))
				.unwrap()
				.contains("CULTURE = old")
		);
		assert!(emitted.contains("set_country_flag = $CULTURE$"));
	}
	if case == CultureCase::ConditionalMechanism {
		for text in [
			"region = france_region",
			"region = germany_region",
			"else_if =",
			"OR =",
			"change_culture = split_a",
			"change_culture = split_b",
			"accepted_culture = renamed",
			"remove_accepted_culture = renamed",
			"add_accepted_culture = split_a",
			"add_accepted_culture = split_b",
			"change_primary_culture = fused",
			"set_ruler_culture = fused",
			"set_heir_culture = fused",
			"set_consort_culture = fused",
			"set_ruler_culture = renamed",
		] {
			assert!(
				emitted.contains(text),
				"lost transformation operation {text}: {emitted}"
			);
		}
		for destination in ["split_a =", "split_b =", "fused ="] {
			assert!(
				cultures.contains(destination),
				"lost distinct transformation destination: {cultures}"
			);
		}
	}
	let base_reference = fs::read_to_string(base_reference_output.to_path(&out))
		.expect("rewritten vanilla reference must be emitted even without include_base");
	assert!(
		base_reference.contains("primary_culture = renamed"),
		"{base_reference}"
	);
	assert_eq!(
		fs::read_to_string(game.join("common/cultures/base.txt")).unwrap(),
		base
	);
	assert_eq!(
		fs::read_to_string(root.join(renamed_path)).unwrap(),
		renamed
	);
	assert_eq!(
		fs::read_to_string(root.join("mods/bonus/common/cultures/base.txt")).unwrap(),
		modified
	);
	assert_eq!(
		fs::read_to_string(root.join("mods/bonus/common/scripted_effects/mechanic.txt")).unwrap(),
		effect
	);
	if case == CultureCase::Compatible {
		let second_out = root.join("output-one-worker");
		let mut second_options = options(second_out.clone());
		second_options.merge_workers = std::num::NonZeroUsize::new(1).unwrap();
		let second = analyze_merge(
			InputRequest::from_manifest_path(root.join("foch.toml"), Config::default()),
			second_options,
			&NoopProgressObserver,
			&CancellationToken::new(),
		)
		.unwrap();
		assert_eq!(second.analysis().report().status, MergeReportStatus::Ready);
		assert_eq!(
			second
				.list_units()
				.iter()
				.map(|unit| (unit.path.clone(), unit.disposition, unit.notes.clone()))
				.collect::<Vec<_>>(),
			review_decisions
		);
		let paths = second
			.list_units()
			.iter()
			.flat_map(|unit| unit.output_paths.iter().cloned())
			.collect::<Vec<_>>();
		assert!(
			paths
				.iter()
				.any(|path| path.as_str().starts_with("common/cultures/"))
		);
		second.commit(CommitAuthorization::EmptyTargetOnly).unwrap();
		for path in paths {
			assert_eq!(
				fs::read(path.to_path(&out)).unwrap(),
				fs::read(path.to_path(&second_out)).unwrap(),
				"worker/cache-dependent output: {path}"
			);
		}
	}
}
