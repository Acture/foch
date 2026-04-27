use foch_core::model::{AnalysisMode, Finding, ScopeType, SymbolKind};
use foch_language::analyzer::analysis::{AnalyzeOptions, analyze_visibility};
use foch_language::analyzer::semantic_index::{
	build_semantic_index, collect_localisation_definitions, parse_script_file,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn corpus_root(mod_name: &str) -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("..")
		.join("..")
		.join("tests")
		.join("corpus")
		.join(mod_name)
}

fn parsed(
	mod_name: &str,
	mod_id: &str,
	relative: &str,
) -> foch_language::analyzer::semantic_index::ParsedScriptFile {
	let root = corpus_root(mod_name);
	let file = root.join(relative);
	parse_script_file(mod_id, &root, &file).expect("parse corpus file")
}

fn parsed_many(
	mod_name: &str,
	mod_id: &str,
	relatives: &[&str],
) -> Vec<foch_language::analyzer::semantic_index::ParsedScriptFile> {
	relatives
		.iter()
		.map(|relative| parsed(mod_name, mod_id, relative))
		.collect()
}

fn is_targeted_noise(finding: &Finding, relative_paths: &[&str], rule_ids: &[&str]) -> bool {
	rule_ids.contains(&finding.rule_id.as_str())
		&& finding
			.path
			.as_ref()
			.map(|path| {
				let rendered = path.to_string_lossy().replace('\\', "/");
				relative_paths
					.iter()
					.any(|relative| rendered.ends_with(relative))
			})
			.unwrap_or(false)
}

#[test]
fn corpus_events_are_indexed_and_calls_resolve() {
	let event = parsed(
		"control_military_access",
		"ctrlma",
		"events/CTRLMA_config_events.txt",
	);
	let effects = parsed(
		"control_military_access",
		"ctrlma",
		"common/scripted_effects/CTRLMA_scripted_effects.txt",
	);
	let index = build_semantic_index(&[event, effects]);

	assert!(
		index
			.definitions
			.iter()
			.any(|def| def.kind == SymbolKind::Event && def.name == "CTRLMA_config_events.0")
	);
	assert!(
		index
			.definitions
			.iter()
			.any(|def| def.kind == SymbolKind::Event && def.name == "CTRLMA_config_events.1")
	);
	assert!(
		index
			.references
			.iter()
			.any(|reference| reference.kind == SymbolKind::Event
				&& reference.name == "CTRLMA_config_events.0")
	);
	assert!(
		index
			.references
			.iter()
			.any(|reference| reference.kind == SymbolKind::Event
				&& reference.name == "CTRLMA_config_events.1")
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(!diagnostics.strict.iter().any(|finding| {
		finding.rule_id == "S002" && finding.message.contains("event CTRLMA_config_events")
	}));
}

#[test]
fn corpus_scripted_effect_param_binding_is_resolved() {
	let event = parsed(
		"control_military_access",
		"ctrlma",
		"events/CTRLMA_config_events.txt",
	);
	let effects = parsed(
		"control_military_access",
		"ctrlma",
		"common/scripted_effects/CTRLMA_scripted_effects.txt",
	);
	let index = build_semantic_index(&[event, effects]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	assert!(!diagnostics.strict.iter().any(|finding| {
		finding.rule_id == "S004" && finding.message.contains("CTRLMA_enable_or_disable_effects")
	}));
}

#[test]
fn corpus_scope_inference_tracks_root_and_province_scope() {
	let player = parsed("defines", "defines", "common/scripted_effects/player.txt");
	let index = build_semantic_index(&[player]);
	assert!(
		index
			.scopes
			.iter()
			.any(|scope| scope.this_type == ScopeType::Province)
	);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(!diagnostics.strict.iter().any(|finding| {
		finding.rule_id == "S003"
			&& finding
				.path
				.as_ref()
				.map(|path| path.ends_with("common/scripted_effects/player.txt"))
				.unwrap_or(false)
	}));
}

#[test]
fn corpus_diplomatic_actions_keep_aliases_visible() {
	let control_action = parsed(
		"control_military_access",
		"ctrlma",
		"common/diplomatic_actions/000_CTRLMA_diplomatic_actions.txt",
	);
	let base_action = parsed(
		"defines",
		"defines",
		"common/diplomatic_actions/00_diplomatic_actions.txt",
	);
	let index = build_semantic_index(&[control_action, base_action]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	assert!(!diagnostics.strict.iter().any(|finding| {
		finding.rule_id == "S003"
			&& finding
				.path
				.as_ref()
				.map(|path| {
					path.ends_with("common/diplomatic_actions/000_CTRLMA_diplomatic_actions.txt")
				})
				.unwrap_or(false)
	}));
}

#[test]
fn decision_keywords_are_not_recorded_as_scripted_effect_references() {
	let decision = parsed("defines", "defines", "decisions/01_player_decision.txt");
	let index = build_semantic_index(&[decision]);

	let reference_names: Vec<&str> = index
		.references
		.iter()
		.filter(|item| item.kind == SymbolKind::ScriptedEffect)
		.map(|item| item.name.as_str())
		.collect();

	assert!(!reference_names.contains(&"country_decisions"));
	assert!(!reference_names.contains(&"potential"));
	assert!(!reference_names.contains(&"allow"));
	assert!(!reference_names.contains(&"add_country_modifier"));
	assert!(!reference_names.contains(&"every_owned_province"));
	assert!(
		index
			.definitions
			.iter()
			.any(|item| item.kind == SymbolKind::Decision
				&& item.local_name == "_player_decision"
				&& item.name.ends_with("::_player_decision"))
	);
}

#[test]
fn corpus_flag_reference_is_resolved_from_param_binding() {
	let event = parsed(
		"control_military_access",
		"ctrlma",
		"events/CTRLMA_config_events.txt",
	);
	let effects = parsed(
		"control_military_access",
		"ctrlma",
		"common/scripted_effects/CTRLMA_scripted_effects.txt",
	);
	let index = build_semantic_index(&[event, effects]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	assert!(!diagnostics.advisory.iter().any(|finding| {
		finding.rule_id == "A004"
			&& finding
				.message
				.contains("CTRLMA_non_war_leader_cannot_ask_military_access_enabled_global_flag")
	}));
}

#[test]
fn direct_country_flag_definition_resolves_cross_file_references() {
	let decision = parsed(
		"control_military_access",
		"ctrlma",
		"decisions/CTRLMA_decisions.txt",
	);
	let event = parsed(
		"control_military_access",
		"ctrlma",
		"events/CTRLMA_config_events.txt",
	);
	let index = build_semantic_index(&[decision, event]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	assert!(!diagnostics.advisory.iter().any(|finding| {
		finding.rule_id == "A004" && finding.message.contains("CTRLMA_open_config_menu_flag")
	}));
}

#[test]
fn corpus_tooltip_key_is_resolved_from_localisation_files() {
	let action = parsed(
		"control_military_access",
		"ctrlma",
		"common/diplomatic_actions/000_CTRLMA_diplomatic_actions.txt",
	);
	let mut index = build_semantic_index(&[action]);
	let root = corpus_root("control_military_access");
	index
		.localisation_definitions
		.extend(collect_localisation_definitions("ctrlma", &root));

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	assert!(!diagnostics.advisory.iter().any(|finding| {
		finding.rule_id == "A005"
			&& finding
				.message
				.contains("CTRLMA_ALLIES_CANNOT_BE_ASKED_MILITARY_ACCESS_BY_OPPOSING_SIDE")
	}));
}

#[test]
fn missing_localisation_definition_creates_a005() {
	let action = parsed(
		"control_military_access",
		"ctrlma",
		"common/diplomatic_actions/000_CTRLMA_diplomatic_actions.txt",
	);
	let index = build_semantic_index(&[action]);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	assert!(diagnostics.advisory.iter().any(|finding| {
		finding.rule_id == "A005"
			&& finding
				.message
				.contains("CTRLMA_ALLIES_CANNOT_BE_ASKED_MILITARY_ACCESS_BY_OPPOSING_SIDE")
	}));
}

#[test]
fn corpus_name_title_desc_keys_are_resolved_from_localisation_files() {
	let event = parsed(
		"control_military_access",
		"ctrlma",
		"events/CTRLMA_config_events.txt",
	);
	let mut index = build_semantic_index(&[event]);
	let root = corpus_root("control_military_access");
	index
		.localisation_definitions
		.extend(collect_localisation_definitions("ctrlma", &root));

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	for key in [
		"CTRLMA_config_events.title",
		"CTRLMA_config_events.desc",
		"CTRLMA.confirm",
	] {
		assert!(
			!diagnostics
				.advisory
				.iter()
				.any(|finding| { finding.rule_id == "A005" && finding.message.contains(key) })
		);
	}
}

#[test]
fn missing_name_title_desc_localisation_creates_a005() {
	let event = parsed(
		"control_military_access",
		"ctrlma",
		"events/CTRLMA_config_events.txt",
	);
	let index = build_semantic_index(&[event]);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	for key in [
		"CTRLMA_config_events.title",
		"CTRLMA_config_events.desc",
		"CTRLMA.confirm",
	] {
		assert!(
			diagnostics
				.advisory
				.iter()
				.any(|finding| { finding.rule_id == "A005" && finding.message.contains(key) })
		);
	}
}

#[test]
fn templated_flag_missing_reports_inference_evidence() {
	let tmp = TempDir::new().expect("temp dir");
	let root = tmp.path().join("mod");
	fs::create_dir_all(root.join("events")).expect("create events");
	fs::create_dir_all(root.join("common").join("scripted_effects")).expect("create effects dir");
	fs::write(
		root.join("events").join("test.txt"),
		"namespace = test\ncountry_event = { id = test.1 option = { toggle_missing_flag = { FLAG = unresolved_global_flag } } }\n",
	)
	.expect("write events");
	fs::write(
		root.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"toggle_missing_flag = { if = { limit = { has_global_flag = $FLAG$ } } }\n",
	)
	.expect("write effects");

	let event = parse_script_file("tmp", &root, &root.join("events").join("test.txt"))
		.expect("parse event");
	let effect = parse_script_file(
		"tmp",
		&root,
		&root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
	)
	.expect("parse effect");
	let index = build_semantic_index(&[event, effect]);

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	let finding = diagnostics
		.advisory
		.iter()
		.find(|finding| {
			finding.rule_id == "A004"
				&& finding.message.contains("unresolved_global_flag")
				&& finding
					.evidence
					.as_ref()
					.map(|value| value.contains("FLAG=unresolved_global_flag"))
					.unwrap_or(false)
		})
		.expect("templated flag reference should report inferred evidence");

	assert!(
		finding
			.evidence
			.as_ref()
			.map(|value| value.contains("has_global_flag = $FLAG$"))
			.unwrap_or(false)
	);
}

#[test]
fn embedded_template_flag_setter_resolves_dynamic_definition() {
	// Validates the fix for A004 false positives where a scripted_effect sets a
	// flag whose name embeds a `$param$` placeholder (e.g. `set_country_flag =
	// is_$tag$_flag`). Calling `effect = { tag = EY0 }` should make
	// `is_EY0_flag` count as defined and suppress the A004 read warning.
	let tmp = TempDir::new().expect("temp dir");
	let root = tmp.path().join("mod");
	fs::create_dir_all(root.join("events")).expect("create events");
	fs::create_dir_all(root.join("common").join("scripted_effects")).expect("create effects dir");
	fs::write(
		root.join("events").join("test.txt"),
		"namespace = test\ncountry_event = { id = test.1 option = { eyalet_effect = { tag = EY0 } limit = { has_country_flag = is_EY0_flag } } }\n",
	)
	.expect("write events");
	fs::write(
		root.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"eyalet_effect = { set_country_flag = is_$tag$_flag }\n",
	)
	.expect("write effects");

	let event = parse_script_file("tmp", &root, &root.join("events").join("test.txt"))
		.expect("parse event");
	let effect = parse_script_file(
		"tmp",
		&root,
		&root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
	)
	.expect("parse effect");
	let index = build_semantic_index(&[event, effect]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics.advisory.iter().any(|finding| {
			finding.rule_id == "A004" && finding.message.contains("is_EY0_flag")
		}),
		"embedded `set_country_flag = is_$tag$_flag` invoked with tag=EY0 should define is_EY0_flag",
	);
}

#[test]
fn engine_set_country_flags_are_pre_seeded() {
	// Vanilla EU4 reads several country flags that are only ever set by the
	// game engine (e.g. on diplomatic annexation, war victory). A004 should
	// pre-seed those flags so reads do not warn.
	let tmp = TempDir::new().expect("temp dir");
	let root = tmp.path().join("mod");
	fs::create_dir_all(root.join("events")).expect("create events");
	fs::write(
		root.join("events").join("test.txt"),
		"namespace = test\ncountry_event = { id = test.1 trigger = { has_country_flag = vanilla_achievements_enabled has_country_flag = have_diploannexed has_country_flag = has_won_war } }\n",
	)
	.expect("write events");
	let event = parse_script_file("tmp", &root, &root.join("events").join("test.txt"))
		.expect("parse event");
	let index = build_semantic_index(&[event]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for engine_flag in [
		"vanilla_achievements_enabled",
		"have_diploannexed",
		"has_won_war",
	] {
		assert!(
			!diagnostics
				.advisory
				.iter()
				.any(|finding| finding.rule_id == "A004" && finding.message.contains(engine_flag)),
			"engine-set flag {engine_flag} should not produce A004",
		);
	}
}

#[test]
fn templated_flag_pattern_allowlists_unbound_reads() {
	let tmp = TempDir::new().expect("temp dir");
	let root = tmp.path().join("mod");
	fs::create_dir_all(root.join("events")).expect("create events");
	fs::create_dir_all(root.join("common").join("scripted_effects")).expect("create effects dir");
	fs::write(
		root.join("events").join("test.txt"),
		"namespace = test\ncountry_event = { id = test.1 trigger = { has_country_flag = is_FOO_flag has_country_flag = is_BAR_flag } }\n",
	)
	.expect("write events");
	fs::write(
		root.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"eyalet_effect = { set_country_flag = is_$tag$_flag }\n",
	)
	.expect("write effects");
	let event = parse_script_file("tmp", &root, &root.join("events").join("test.txt"))
		.expect("parse event");
	let effect = parse_script_file(
		"tmp",
		&root,
		&root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
	)
	.expect("parse effect");
	let index = build_semantic_index(&[event, effect]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	for flag in ["is_FOO_flag", "is_BAR_flag"] {
		assert!(
			!diagnostics
				.advisory
				.iter()
				.any(|finding| finding.rule_id == "A004" && finding.message.contains(flag)),
			"`is_$tag$_flag` template should pattern-suppress reads of {flag}",
		);
	}
}

#[test]
fn unrelated_flag_still_fires_with_template_present() {
	// Validate that the templated-derivation path keeps firing when a setter
	// is expected by parameterization but absent. The scripted_effect reads
	// `has_country_flag = is_$tag$_flag` but never sets it; a caller binds
	// `tag = FOO`, so we know the read resolves to `is_FOO_flag` and that
	// flag has no setter anywhere. A004 must still fire (sites 1/2).
	let tmp = TempDir::new().expect("temp dir");
	let root = tmp.path().join("mod");
	fs::create_dir_all(root.join("events")).expect("create events");
	fs::create_dir_all(root.join("common").join("scripted_effects")).expect("create effects dir");
	fs::write(
		root.join("events").join("test.txt"),
		"namespace = test\ncountry_event = { id = test.1 option = { eyalet_check_effect = { tag = FOO } } }\n",
	)
	.expect("write events");
	fs::write(
		root.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"eyalet_check_effect = { if = { limit = { has_country_flag = is_$tag$_flag } } }\n",
	)
	.expect("write effects");
	let event = parse_script_file("tmp", &root, &root.join("events").join("test.txt"))
		.expect("parse event");
	let effect = parse_script_file(
		"tmp",
		&root,
		&root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
	)
	.expect("parse effect");
	let index = build_semantic_index(&[event, effect]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		diagnostics
			.advisory
			.iter()
			.any(|finding| finding.rule_id == "A004" && finding.message.contains("is_FOO_flag")),
		"templated derivation must keep firing when no setter exists",
	);
}

#[test]
fn literal_unset_flag_in_trigger_position_is_suppressed() {
	// `has_*_flag = X` inside a trigger context where X is never set anywhere
	// is the canonical cross-mod compat gate (e.g. `has_global_flag =
	// extended_timeline_mod`). Reading an unset flag in trigger position is
	// benign — the gate just stays closed. A004 must not fire.
	let tmp = TempDir::new().expect("temp dir");
	let root = tmp.path().join("mod");
	fs::create_dir_all(root.join("events")).expect("create events");
	fs::write(
		root.join("events").join("test.txt"),
		"namespace = test\ncountry_event = { id = test.1 trigger = { has_global_flag = totally_unrelated_flag } }\n",
	)
	.expect("write events");
	let event = parse_script_file("tmp", &root, &root.join("events").join("test.txt"))
		.expect("parse event");
	let index = build_semantic_index(&[event]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics
			.advisory
			.iter()
			.any(|finding| finding.rule_id == "A004"
				&& finding.message.contains("totally_unrelated_flag")),
		"literal `has_*_flag` reads in trigger position must not raise A004",
	);
}

#[test]
fn corpus_priority_eu4_roots_do_not_emit_targeted_noise() {
	let files = parsed_many(
		"eu4_coverage",
		"eu4cov",
		&[
			"common/scripted_effects/00_coverage_effects.txt",
			"common/scripted_triggers/00_coverage_triggers.txt",
			"missions/00_coverage_missions.txt",
			"common/ages/00_coverage_ages.txt",
			"common/buildings/00_coverage_buildings.txt",
			"common/government_reforms/00_coverage_reforms.txt",
			"common/institutions/00_coverage_institutions.txt",
			"common/great_projects/00_coverage_projects.txt",
			"common/cb_types/00_coverage_cb.txt",
			"common/province_triggered_modifiers/00_coverage_ptm.txt",
			"common/ideas/00_coverage_ideas.txt",
			"common/new_diplomatic_actions/00_coverage_actions.txt",
		],
	);
	let index = build_semantic_index(&files);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	let target_paths = [
		"missions/00_coverage_missions.txt",
		"common/ages/00_coverage_ages.txt",
		"common/buildings/00_coverage_buildings.txt",
		"common/government_reforms/00_coverage_reforms.txt",
		"common/institutions/00_coverage_institutions.txt",
		"common/great_projects/00_coverage_projects.txt",
		"common/cb_types/00_coverage_cb.txt",
		"common/province_triggered_modifiers/00_coverage_ptm.txt",
		"common/ideas/00_coverage_ideas.txt",
		"common/new_diplomatic_actions/00_coverage_actions.txt",
	];
	let targeted_rules = ["A001", "S002", "S003", "S004", "A004"];

	assert!(!diagnostics.strict.iter().any(|finding| is_targeted_noise(
		finding,
		&target_paths,
		&targeted_rules
	)));
	assert!(
		!diagnostics.advisory.iter().any(|finding| is_targeted_noise(
			finding,
			&target_paths,
			&targeted_rules
		))
	);
}

#[test]
fn corpus_metadata_eu4_roots_do_not_emit_targeted_noise() {
	let files = parsed_many(
		"eu4_coverage",
		"eu4cov",
		&[
			"common/scripted_effects/00_coverage_effects.txt",
			"common/scripted_triggers/00_coverage_triggers.txt",
			"common/event_modifiers/00_coverage_event_modifiers.txt",
			"common/government_names/00_coverage_government_names.txt",
			"customizable_localization/00_coverage_localization.txt",
			"common/cultures/00_coverage_cultures.txt",
			"common/advisortypes/00_coverage_advisortypes.txt",
			"common/custom_gui/00_coverage_gui.txt",
		],
	);
	let index = build_semantic_index(&files);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	let target_paths = [
		"common/event_modifiers/00_coverage_event_modifiers.txt",
		"common/government_names/00_coverage_government_names.txt",
		"customizable_localization/00_coverage_localization.txt",
		"common/cultures/00_coverage_cultures.txt",
		"common/advisortypes/00_coverage_advisortypes.txt",
		"common/custom_gui/00_coverage_gui.txt",
	];
	let targeted_rules = ["A001", "S002", "S003", "S004", "A004"];

	assert!(!diagnostics.strict.iter().any(|finding| is_targeted_noise(
		finding,
		&target_paths,
		&targeted_rules
	)));
	assert!(
		!diagnostics.advisory.iter().any(|finding| is_targeted_noise(
			finding,
			&target_paths,
			&targeted_rules
		))
	);
}

#[test]
fn corpus_wrapper_heavy_roots_keep_callbacks_and_helpers_clean() {
	let files = parsed_many(
		"eu4_wrappers",
		"eu4wrap",
		&[
			"common/scripted_effects/00_wrappers_effects.txt",
			"common/scripted_triggers/00_wrappers_triggers.txt",
			"missions/00_wrappers_missions.txt",
			"common/government_reforms/00_wrappers_reforms.txt",
			"common/new_diplomatic_actions/00_wrappers_actions.txt",
			"common/cb_types/00_wrappers_cb.txt",
			"common/on_actions/00_wrappers_on_actions.txt",
		],
	);
	let index = build_semantic_index(&files);

	for name in [
		"potential_on_load",
		"ai_weight",
		"static_actions",
		"ai_acceptance",
		"add_entry",
	] {
		assert!(
			!index.references.iter().any(|reference| {
				reference.kind == SymbolKind::ScriptedEffect && reference.name == name
			}),
			"{name} should not be recorded as a scripted effect reference"
		);
	}

	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	let target_paths = [
		"missions/00_wrappers_missions.txt",
		"common/government_reforms/00_wrappers_reforms.txt",
		"common/new_diplomatic_actions/00_wrappers_actions.txt",
		"common/cb_types/00_wrappers_cb.txt",
		"common/on_actions/00_wrappers_on_actions.txt",
	];
	let targeted_rules = ["A001", "S002", "S003", "S004", "A004"];

	assert!(!diagnostics.strict.iter().any(|finding| is_targeted_noise(
		finding,
		&target_paths,
		&targeted_rules
	)));
	assert!(
		!diagnostics.advisory.iter().any(|finding| is_targeted_noise(
			finding,
			&target_paths,
			&targeted_rules
		))
	);
}

#[test]
fn corpus_real_minimized_ages_reformed_patterns_stay_clean() {
	let files = parsed_many(
		"eu4_real_minimized/ages_reformed",
		"2896451151",
		&[
			"common/scripted_effects/00_ages_reformed_effects.txt",
			"common/scripted_triggers/00_ages_reformed_triggers.txt",
			"common/triggered_modifiers/00_ages_reformed_modifiers.txt",
			"common/ages/00_ages_reformed_ages.txt",
		],
	);
	let index = build_semantic_index(&files);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	let target_paths = [
		"common/scripted_triggers/00_ages_reformed_triggers.txt",
		"common/triggered_modifiers/00_ages_reformed_modifiers.txt",
		"common/ages/00_ages_reformed_ages.txt",
	];
	let targeted_rules = ["A001", "S004"];

	assert!(!diagnostics.strict.iter().any(|finding| is_targeted_noise(
		finding,
		&target_paths,
		&targeted_rules
	)));
	assert!(
		!diagnostics.advisory.iter().any(|finding| is_targeted_noise(
			finding,
			&target_paths,
			&targeted_rules
		))
	);
	assert!(index.definitions.iter().any(|definition| {
		definition.kind == SymbolKind::ScriptedEffect
			&& definition.local_name == "se_md_add_or_upgrade_bonus"
	}));
	assert!(index.references.iter().any(|reference| {
		reference.kind == SymbolKind::ScriptedEffect
			&& reference.name == "se_md_add_or_upgrade_bonus"
			&& reference
				.provided_params
				.iter()
				.any(|param| param == "abilityName")
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.key == "age_definition"
			&& reference.value == "age_of_discovery"
			&& reference.path == std::path::Path::new("common/ages/00_ages_reformed_ages.txt")
	}));
}

#[test]
fn corpus_real_minimized_more_favor_actions_patterns_stay_clean() {
	let files = parsed_many(
		"eu4_real_minimized/more_favor_actions",
		"2871630256",
		&[
			"common/scripted_effects/more_favor_actions_scripted_effects.txt",
			"common/new_diplomatic_actions/00_more_favor_actions_actions.txt",
			"common/diplomatic_actions/000_more_favor_actions_diplomatic_actions.txt",
			"events/00_more_favor_actions_events.txt",
		],
	);
	let index = build_semantic_index(&files);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	let target_paths = [
		"common/scripted_effects/more_favor_actions_scripted_effects.txt",
		"common/new_diplomatic_actions/00_more_favor_actions_actions.txt",
		"common/diplomatic_actions/000_more_favor_actions_diplomatic_actions.txt",
		"events/00_more_favor_actions_events.txt",
	];
	let targeted_rules = ["A001", "S002", "S003", "S004", "A004"];
	let strict_noise: Vec<String> = diagnostics
		.strict
		.iter()
		.filter(|finding| is_targeted_noise(finding, &target_paths, &targeted_rules))
		.map(|finding| {
			format!(
				"{}:{}:{}:{}",
				finding.rule_id,
				finding
					.path
					.as_ref()
					.map(|path| path.display().to_string())
					.unwrap_or_else(|| "<none>".to_string()),
				finding.line.unwrap_or_default(),
				finding.message
			)
		})
		.collect();
	let advisory_noise: Vec<String> = diagnostics
		.advisory
		.iter()
		.filter(|finding| is_targeted_noise(finding, &target_paths, &targeted_rules))
		.map(|finding| {
			format!(
				"{}:{}:{}:{}",
				finding.rule_id,
				finding
					.path
					.as_ref()
					.map(|path| path.display().to_string())
					.unwrap_or_else(|| "<none>".to_string()),
				finding.line.unwrap_or_default(),
				finding.message
			)
		})
		.collect();

	assert!(
		strict_noise.is_empty(),
		"strict targeted noise: {strict_noise:#?}"
	);
	assert!(
		advisory_noise.is_empty(),
		"advisory targeted noise: {advisory_noise:#?}"
	);
	assert!(index.resource_references.iter().any(|reference| {
		reference.path
			== std::path::Path::new(
				"common/new_diplomatic_actions/00_more_favor_actions_actions.txt",
			) && reference.key == "new_diplomatic_action_definition"
			&& reference.value == "more_favors_action_request_adm_power"
	}));
	assert!(index.resource_references.iter().any(|reference| {
		reference.path
			== std::path::Path::new(
				"common/diplomatic_actions/000_more_favor_actions_diplomatic_actions.txt",
			) && reference.key == "diplomatic_action_definition"
			&& reference.value == "more_favor_actions_remove_guarantee"
	}));
	assert!(!index.references.iter().any(|reference| {
		reference.kind == SymbolKind::ScriptedEffect && reference.name.starts_with("event_target:")
	}));
}

#[test]
fn corpus_real_minimized_europa_expanded_building_params_stay_clean() {
	let files = parsed_many(
		"eu4_real_minimized/europa_expanded",
		"2164202838",
		&[
			"common/scripted_effects/00_europa_expanded_effects.txt",
			"common/buildings/00_europa_expanded_buildings.txt",
		],
	);
	let index = build_semantic_index(&files);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	let target_paths = [
		"common/scripted_effects/00_europa_expanded_effects.txt",
		"common/buildings/00_europa_expanded_buildings.txt",
	];
	let targeted_rules = ["S004"];
	let strict_noise: Vec<String> = diagnostics
		.strict
		.iter()
		.filter(|finding| is_targeted_noise(finding, &target_paths, &targeted_rules))
		.map(|finding| {
			format!(
				"{}:{}:{}:{}",
				finding.rule_id,
				finding
					.path
					.as_ref()
					.map(|path| path.display().to_string())
					.unwrap_or_else(|| "<none>".to_string()),
				finding.line.unwrap_or_default(),
				finding.message
			)
		})
		.collect();

	assert!(
		strict_noise.is_empty(),
		"strict targeted noise: {strict_noise:#?}"
	);
	assert!(index.references.iter().any(|reference| {
		reference.kind == SymbolKind::ScriptedEffect
			&& reference.name == "update_improved_military_buildings_modifier"
			&& reference
				.provided_params
				.iter()
				.any(|param| param == "building")
	}));
}

#[test]
fn corpus_real_minimized_europa_expanded_complex_effects_stay_clean() {
	let files = parsed_many(
		"eu4_real_minimized/europa_expanded",
		"2164202838",
		&[
			"common/scripted_effects/01_europa_expanded_complex_effects.txt",
			"missions/00_europa_expanded_complex_effects.txt",
		],
	);
	let index = build_semantic_index(&files);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	let target_paths = [
		"common/scripted_effects/01_europa_expanded_complex_effects.txt",
		"missions/00_europa_expanded_complex_effects.txt",
	];
	let targeted_rules = ["S004"];
	let strict_noise: Vec<String> = diagnostics
		.strict
		.iter()
		.filter(|finding| is_targeted_noise(finding, &target_paths, &targeted_rules))
		.map(|finding| {
			format!(
				"{}:{}:{}:{}",
				finding.rule_id,
				finding
					.path
					.as_ref()
					.map(|path| path.display().to_string())
					.unwrap_or_else(|| "<none>".to_string()),
				finding.line.unwrap_or_default(),
				finding.message
			)
		})
		.collect();

	assert!(
		strict_noise.is_empty(),
		"strict targeted noise: {strict_noise:#?}"
	);
	assert!(
		index
			.references
			.iter()
			.filter(|reference| {
				reference.kind == SymbolKind::ScriptedEffect
					&& reference.name == "complex_dynamic_effect_without_alternative"
			})
			.count() >= 2
	);
}

#[test]
fn corpus_real_minimized_base_game_complex_effects_stay_clean() {
	let files = parsed_many(
		"eu4_real_minimized/base_game_complex_effects",
		"__game__eu4",
		&[
			"common/scripted_effects/00_base_game_complex_effects.txt",
			"missions/00_base_game_complex_effects.txt",
			"decisions/00_base_game_complex_effects.txt",
		],
	);
	let index = build_semantic_index(&files);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);

	let target_paths = [
		"common/scripted_effects/00_base_game_complex_effects.txt",
		"missions/00_base_game_complex_effects.txt",
		"decisions/00_base_game_complex_effects.txt",
	];
	let targeted_rules = ["S004"];
	let strict_noise: Vec<String> = diagnostics
		.strict
		.iter()
		.filter(|finding| is_targeted_noise(finding, &target_paths, &targeted_rules))
		.map(|finding| {
			format!(
				"{}:{}:{}:{}",
				finding.rule_id,
				finding
					.path
					.as_ref()
					.map(|path| path.display().to_string())
					.unwrap_or_else(|| "<none>".to_string()),
				finding.line.unwrap_or_default(),
				finding.message
			)
		})
		.collect();

	assert!(
		strict_noise.is_empty(),
		"strict targeted noise: {strict_noise:#?}"
	);
	assert!(index.references.iter().any(|reference| {
		reference.kind == SymbolKind::ScriptedEffect
			&& reference.name == "complex_dynamic_effect"
			&& reference
				.provided_params
				.iter()
				.any(|param| param == "first_custom_tooltip")
	}));
	assert!(index.references.iter().any(|reference| {
		reference.kind == SymbolKind::ScriptedEffect
			&& reference.name == "complex_dynamic_effect_without_alternative"
			&& reference
				.provided_params
				.iter()
				.any(|param| param == "first_custom_tooltip")
	}));
}

#[test]
fn a002_skips_dynamic_scope_content_families() {
	// scripted_effects bodies have no statically-known caller scope. A002
	// should not flag `set_country_flag` / `add_prestige` etc inside such
	// files just because the analyzer happens to infer Province scope from
	// a nested iterator. This mirrors the A001 dynamic_scope skip.
	let tmp = TempDir::new().expect("temp dir");
	let root = tmp.path().join("mod");
	fs::create_dir_all(root.join("common").join("scripted_effects")).expect("create effects dir");
	fs::write(
		root.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
		"sample_effect = { every_owned_province = { set_country_flag = some_flag add_prestige = 1 } }\n",
	)
	.expect("write effects");

	let effect = parse_script_file(
		"tmp",
		&root,
		&root
			.join("common")
			.join("scripted_effects")
			.join("effects.txt"),
	)
	.expect("parse effect");
	let index = build_semantic_index(&[effect]);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
	assert!(
		!diagnostics
			.advisory
			.iter()
			.any(|finding| finding.rule_id == "A002"),
		"A002 must skip dynamic_scope content families: {:#?}",
		diagnostics
			.advisory
			.iter()
			.filter(|finding| finding.rule_id == "A002")
			.collect::<Vec<_>>(),
	);
}
