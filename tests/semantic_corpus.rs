use foch::check::analysis::{AnalyzeOptions, analyze_visibility};
use foch::check::graph::export_graph;
use foch::check::model::{AnalysisMode, GraphFormat, ScopeType, SymbolKind};
use foch::check::semantic_index::{
	build_semantic_index, collect_localisation_definitions, parse_script_file,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn corpus_root(mod_name: &str) -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests")
		.join("corpus")
		.join(mod_name)
}

fn parsed(
	mod_name: &str,
	mod_id: &str,
	relative: &str,
) -> foch::check::semantic_index::ParsedScriptFile {
	let root = corpus_root(mod_name);
	let file = root.join(relative);
	parse_script_file(mod_id, &root, &file).expect("parse corpus file")
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
fn graph_export_supports_json_and_dot() {
	let player = parsed("defines", "defines", "common/scripted_effects/player.txt");
	let index = build_semantic_index(&[player]);

	let json = export_graph(&index, GraphFormat::Json);
	let decoded: serde_json::Value = serde_json::from_str(&json).expect("graph json should parse");
	assert!(decoded.get("scopes").is_some());

	let dot = export_graph(&index, GraphFormat::Dot);
	assert!(dot.contains("digraph foch_semantic"));
	assert!(dot.contains("scope_0"));
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
