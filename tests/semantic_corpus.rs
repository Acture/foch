use foch::check::analysis::{AnalyzeOptions, analyze_visibility};
use foch::check::graph::export_graph;
use foch::check::model::{AnalysisMode, GraphFormat, ScopeType, SymbolKind};
use foch::check::semantic_index::{build_semantic_index, parse_script_file};
use std::path::{Path, PathBuf};

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
