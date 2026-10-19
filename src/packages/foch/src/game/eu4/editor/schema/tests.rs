use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::*;

fn game_path(text: &str) -> &GamePath {
	GamePath::new(text).expect("valid game path")
}

struct WorkspaceFixture {
	schema_workspace: SchemaWorkspace,
}

fn repository_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lsp_fixture_dir() -> PathBuf {
	repository_root()
		.join("..")
		.join("foch-lsp")
		.join("tests")
		.join("fixtures")
		.join("lsp")
}

fn load_lsp_schema() -> EditorSchema {
	let cache = TempDir::new().expect("create test CWT rule cache");
	EditorSchema::load_from_directory_with_cache(
		&lsp_fixture_dir().join("schema"),
		Some(cache.path()),
	)
	.expect("load LSP editor schema")
}

fn load_inline_lsp_schema(schema: &str) -> EditorSchema {
	let schema_dir = TempDir::new().expect("create inline CWT schema dir");
	fs::write(schema_dir.path().join("inline.cwt"), schema).expect("write inline CWT schema");
	let cache = TempDir::new().expect("create inline CWT rule cache");
	EditorSchema::load_from_directory_with_cache(schema_dir.path(), Some(cache.path()))
		.expect("load inline LSP editor schema")
}

fn complex_enum_workspace(schema: EditorSchema) -> WorkspaceFixture {
	let documents = [
		SchemaDocument::new(
			game_path("common/country_tags/00_countries.txt"),
			"SWE = \"countries/Sweden.txt\"\nFRA = \"countries/France.txt\"\n",
		),
		SchemaDocument::new(
			game_path("common/graphicalculturetype.txt"),
			"westerngfx = {}\neasterngfx = {}\n",
		),
		SchemaDocument::new(
			game_path("customizable_localization/sample_custom_locs.txt"),
			"defined_text = {\n  name = sample_defined_text\n  text = { localisation_key = sample_defined_text_key }\n}\n",
		),
		SchemaDocument::new(
			game_path("common/cultures/00_cultures.txt"),
			"latin = {\n  dynasty_names = { von_habsburg de_valois }\n  austrian = {\n    name = { dynasty_names = { von_luxembourg } }\n  }\n}\n",
		),
	];
	WorkspaceFixture {
		schema_workspace: schema.workspace(&documents),
	}
}

fn conditional_subtype_schema() -> &'static str {
	r#"
		types = {
			type[event] = {
				path = "game/events"
				## type_key_filter = sample_event
				subtype[sample] = {
				}
				subtype[hidden] = {
					hidden = yes
				}
			}
		}

		event = {
			hidden = bool
			subtype[hidden] = {
				hidden_only = bool
			}
			subtype[!hidden] = {
				visible_only = bool
			}
		}
		"#
}

fn fixture_text(relative_path: &str) -> String {
	fs::read_to_string(lsp_fixture_dir().join(relative_path)).expect("read LSP fixture text")
}

fn position_for_token(text: &str, token: &str) -> EditorPosition {
	for (line_index, line) in text.lines().enumerate() {
		if let Some(character) = line.find(token) {
			return EditorPosition {
				line: line_index as u32,
				character: character as u32,
			};
		}
	}
	panic!("token `{token}` not found");
}

fn position_for_token_offset(text: &str, token: &str, offset: u32) -> EditorPosition {
	let mut position = position_for_token(text, token);
	position.character += offset;
	position
}

fn hover_markdown(hover: SchemaHover) -> String {
	hover.markdown
}

fn schema_hover(
	schema: &EditorSchema,
	file_path: &GamePath,
	text: &str,
	position: EditorPosition,
	workspace: Option<&SchemaWorkspace>,
) -> Option<SchemaHover> {
	schema.hover(file_path, text, position, workspace)
}

fn schema_completion_candidates(
	schema: &EditorSchema,
	file_path: &GamePath,
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
) -> Option<Vec<SchemaCompletion>> {
	schema.completions(file_path, text, position, prefix_lower, None)
}

fn schema_completion_candidates_with_index(
	schema: &EditorSchema,
	file_path: &GamePath,
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
	workspace: Option<&SchemaWorkspace>,
) -> Option<Vec<SchemaCompletion>> {
	schema.completions(file_path, text, position, prefix_lower, workspace)
}

fn schema_diagnostics_for_text(
	schema: &EditorSchema,
	file_path: &GamePath,
	text: &str,
) -> Vec<SchemaDiagnostic> {
	schema.diagnostics(file_path, text, None)
}

fn schema_diagnostics_for_text_with_index(
	schema: &EditorSchema,
	file_path: &GamePath,
	text: &str,
	workspace: Option<&SchemaWorkspace>,
) -> Vec<SchemaDiagnostic> {
	schema.diagnostics(file_path, text, workspace)
}

#[test]
fn active_schema_is_the_embedded_pack_unless_overridden() {
	let info = EditorSchema::active().info();

	assert_eq!(info.source_id, crate::game::eu4::active_cwt_schema_id());
	if crate::game::eu4::cwt_schema_override().is_none() {
		assert_eq!(info.status, SchemaLoadStatus::Embedded);
		assert_eq!(info.source_id, crate::game::eu4::EMBEDDED_CWT_SCHEMA_ID);
	}
	assert!(info.root_count > 0);
	assert!(info.alias_count > 0);
}

#[test]
fn schema_loader_reads_existing_fixture_pack() {
	let fixture_dir = repository_root()
		.join("src")
		.join("game")
		.join("schema")
		.join("tests/fixtures/schema-pack");
	assert!(
		fixture_dir.join("events/events.cwt").is_file(),
		"events schema fixture must exist: {fixture_dir:?}"
	);
	assert!(
		fixture_dir.join("missions/missions.cwt").is_file(),
		"missions schema fixture must exist: {fixture_dir:?}"
	);
	let cache = TempDir::new().expect("create test CWT rule cache");
	let schema = EditorSchema::load_from_directory_with_cache(&fixture_dir, Some(cache.path()))
		.expect("load fixture schema graph");
	assert!(schema.info().root_count >= 2);
}

#[test]
fn hover_uses_utf16_positions_after_unicode_text() {
	let schema = load_lsp_schema();
	let text = "country_event = { title = \"中文😀\" category = ADM }";
	let hover = schema
		.hover(
			GamePath::new("events/sample.txt").unwrap(),
			text,
			EditorPosition {
				line: 0,
				character: 33,
			},
			None,
		)
		.expect("category hover after Chinese and emoji text");
	assert!(hover.markdown.contains("**category**"));
	assert_eq!(
		hover.range,
		EditorRange {
			start: EditorPosition {
				line: 0,
				character: 33,
			},
			end: EditorPosition {
				line: 0,
				character: 41,
			},
		}
	);
}

#[test]
fn hover_uses_utf16_ranges_for_unicode_keys_with_crlf() {
	let schema = load_inline_lsp_schema(
		r#"
		types = {
			type[event] = {
				path = "game/events"
				## type_key_filter = sample_event
			}
		}
		event = { 中文😀 = bool }
		"#,
	);
	let text = "sample_event = {\r\n 中文😀 = yes\r\n}\r\n";
	let expected = EditorRange {
		start: EditorPosition {
			line: 1,
			character: 1,
		},
		end: EditorPosition {
			line: 1,
			character: 5,
		},
	};
	for character in [1, 2, 3] {
		let hover = schema
			.hover(
				GamePath::new("events/sample.txt").unwrap(),
				text,
				EditorPosition { line: 1, character },
				None,
			)
			.expect("hover on Chinese characters and the emoji start");
		assert!(hover.markdown.contains("**中文😀**"));
		assert_eq!(hover.range, expected);
	}
	for character in [4, 5, 12, 13] {
		assert!(
			schema
				.hover(
					GamePath::new("events/sample.txt").unwrap(),
					text,
					EditorPosition { line: 1, character },
					None,
				)
				.is_none(),
			"no hover inside a surrogate pair, at the exclusive end, or beyond line content: {character}"
		);
	}
}

#[test]
fn hover_renders_event_field_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let hover = schema_hover(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token(&text, "immediate"),
		None,
	)
	.expect("event hover");
	let markdown = hover_markdown(hover);
	assert!(markdown.contains("**immediate**"));
	assert!(markdown.contains("Type: `Block`"));
	assert!(markdown.contains("Immediate effects executed when the event fires."));
	assert!(markdown.contains("push_scope=`country`"));
	assert!(markdown.contains("scope=`country`, `province`"));
}

#[test]
fn hover_renders_enum_value_constraints_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let hover = schema_hover(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token(&text, "category"),
		None,
	)
	.expect("category hover");
	let markdown = hover_markdown(hover);
	assert!(markdown.contains("**category**"));
	assert!(markdown.contains("Type: `Scalar`"));
	assert!(markdown.contains("Value set: `enum` `power_categories`"));
	assert!(markdown.contains("`ADM`, `DIP`, `MIL`"));
	assert!(markdown.contains("Cardinality: `1..1`"));
}

#[test]
fn hover_renders_scope_value_constraints_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let hover = schema_hover(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token(&text, "friend_scope"),
		None,
	)
	.expect("scope hover");
	let markdown = hover_markdown(hover);
	assert!(markdown.contains("**friend_scope**"));
	assert!(markdown.contains("Value set: `scope` `country`"));
	assert!(markdown.contains("`country`, `from`, `province`, `root`"));
}

#[test]
fn hover_renders_complex_enum_values_from_workspace() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = fixture_text("events/sample.txt");
	let hover = schema_hover(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token(&text, "gfx"),
		Some(&workspace.schema_workspace),
	)
	.expect("complex enum hover");
	let markdown = hover_markdown(hover);
	assert!(markdown.contains("**gfx**"));
	assert!(markdown.contains("Value set: `complex_enum` `graphical_cultures`"));
	assert!(markdown.contains("`easterngfx`, `westerngfx`"));
}

#[test]
fn hover_renders_workspace_dynamic_key_source_from_schema() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = "\
namespace = sample
country_event = {
  dynamic_fields = {
    SWE = 1
  }
}
";
	let hover = schema_hover(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token(text, "SWE"),
		Some(&workspace.schema_workspace),
	)
	.expect("workspace dynamic key hover");
	let markdown = hover_markdown(hover);
	assert!(markdown.contains("**SWE**"));
	assert!(markdown.contains("Type: `Scalar`"));
	assert!(markdown.contains("Dynamic key: `complex_enum` `country_tags`"));
}

#[test]
fn hover_renders_workspace_dynamic_alias_source_from_schema() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = "\
namespace = sample
country_event = {
  immediate = {
    SWE = {
      add_prestige = 1
    }
  }
}
";
	let hover = schema_hover(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token(text, "SWE"),
		Some(&workspace.schema_workspace),
	)
	.expect("workspace dynamic alias hover");
	let markdown = hover_markdown(hover);
	assert!(markdown.contains("**SWE**"));
	assert!(markdown.contains("Type: `Block`"));
	assert!(markdown.contains("Dynamic alias: `complex_enum` `country_tags`"));
}

#[test]
fn hover_renders_mission_field_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("missions/sample.txt");
	let hover = schema_hover(
		&engine,
		game_path("missions/sample.txt"),
		&text,
		position_for_token(&text, "provinces_to_highlight"),
		None,
	)
	.expect("mission hover");
	let markdown = hover_markdown(hover);
	assert!(markdown.contains("**provinces_to_highlight**"));
	assert!(markdown.contains("Type: `Block`"));
	assert!(markdown.contains("Selects provinces relevant to the mission."));
	assert!(markdown.contains("Cardinality: `0..1`"));
	assert!(markdown.contains("`root`→`country`"));
	assert!(markdown.contains("`this`→`province`"));
}

#[test]
fn hover_renders_scripted_effect_field_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("common/scripted_effects/sample.txt");
	let hover = schema_hover(
		&engine,
		game_path("common/scripted_effects/sample.txt"),
		&text,
		position_for_token(&text, "add_prestige"),
		None,
	)
	.expect("scripted effect hover");
	let markdown = hover_markdown(hover);
	assert!(markdown.contains("**add_prestige**"));
	assert!(markdown.contains("Type: `Scalar`"));
	assert!(markdown.contains("Adds prestige directly from a scripted effect body."));
}

#[test]
fn completion_suggests_event_children_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token(&text, "trigger"),
		"",
	)
	.expect("event schema completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"title"));
	assert!(labels.contains(&"trigger"));
	assert!(labels.contains(&"immediate"));
	assert!(!labels.contains(&"localisation"));
	assert!(candidates.iter().any(|candidate| {
		candidate.label == "immediate"
			&& candidate
				.detail
				.starts_with("cwt: Immediate effects executed")
	}));
}

#[test]
fn completion_expands_trigger_aliases_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token(&text, "has_country_flag"),
		"has",
	)
	.expect("trigger schema completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"has_country_flag"));
	assert!(!labels.contains(&"is_year"));
	assert!(
		candidates
			.iter()
			.all(|candidate| candidate.kind == SchemaCompletionKind::Function)
	);
	assert!(candidates.iter().any(|candidate| {
		candidate.label == "has_country_flag"
			&& candidate
				.detail
				.starts_with("cwt: Checks whether the current country")
	}));
}

#[test]
fn completion_suggests_enum_values_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token_offset(&text, "ADM", 1),
		"a",
	)
	.expect("enum value completion");
	assert_eq!(candidates.len(), 1);
	assert_eq!(candidates[0].label, "ADM");
	assert_eq!(candidates[0].kind, SchemaCompletionKind::EnumMember);
	assert_eq!(candidates[0].detail, "cwt enum power_categories");
}

#[test]
fn completion_suggests_value_set_values_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token_offset(&text, "root", 2),
		"ro",
	)
	.expect("value_set value completion");
	assert_eq!(candidates.len(), 1);
	assert_eq!(candidates[0].label, "root");
	assert_eq!(candidates[0].kind, SchemaCompletionKind::Value);
	assert_eq!(candidates[0].detail, "cwt value_set event_targets");
}

#[test]
fn completion_suggests_value_values_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token_offset(&text, "prev", 2),
		"pr",
	)
	.expect("value completion");
	assert_eq!(candidates.len(), 1);
	assert_eq!(candidates[0].label, "prev");
	assert_eq!(candidates[0].kind, SchemaCompletionKind::Value);
	assert_eq!(candidates[0].detail, "cwt value event_targets");
}

#[test]
fn completion_suggests_scope_values_from_schema() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  friend_scope = ro\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token_offset(text, "ro", 2),
		"ro",
	)
	.expect("scope value completion");
	assert_eq!(candidates.len(), 1);
	assert_eq!(candidates[0].label, "root");
	assert_eq!(candidates[0].kind, SchemaCompletionKind::Reference);
	assert_eq!(candidates[0].detail, "cwt scope country");
}

#[test]
fn completion_expands_static_dynamic_key_markers_from_schema() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  dynamic_fields = {\n    al\n  }\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token_offset(text, "al", 2),
		"al",
	)
	.expect("dynamic key completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"alpha"));
	assert!(!labels.contains(&"enum[dynamic_event_fields]"));
	assert!(candidates.iter().any(|candidate| {
		candidate.label == "alpha"
			&& candidate.kind == SchemaCompletionKind::Field
			&& candidate.detail == "cwt dynamic key enum dynamic_event_fields"
	}));
}

#[test]
fn completion_expands_workspace_dynamic_key_markers_from_schema() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = "namespace = sample\ncountry_event = {\n  dynamic_fields = {\n    SW\n  }\n}\n";
	let candidates = schema_completion_candidates_with_index(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token_offset(text, "SW", 2),
		"sw",
		Some(&workspace.schema_workspace),
	)
	.expect("workspace dynamic key completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert_eq!(labels, vec!["SWE"]);
	assert!(candidates.iter().any(|candidate| {
		candidate.label == "SWE"
			&& candidate.kind == SchemaCompletionKind::Field
			&& candidate.detail == "cwt dynamic key complex_enum country_tags"
	}));
}

#[test]
fn completion_expands_workspace_dynamic_alias_names_from_schema() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = "namespace = sample\ncountry_event = {\n  immediate = {\n    SW\n  }\n}\n";
	let candidates = schema_completion_candidates_with_index(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token_offset(text, "SW", 2),
		"sw",
		Some(&workspace.schema_workspace),
	)
	.expect("workspace dynamic alias completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert_eq!(labels, vec!["SWE"]);
	assert!(!labels.contains(&"enum[country_tags]"));
	assert!(candidates.iter().any(|candidate| {
		candidate.label == "SWE"
			&& candidate.kind == SchemaCompletionKind::Function
			&& candidate.detail == "cwt dynamic alias complex_enum country_tags"
	}));
}

#[test]
fn completion_enters_workspace_dynamic_alias_body() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = "\
namespace = sample
country_event = {
  immediate = {
    SWE = {
      add
    }
  }
}
";
	let candidates = schema_completion_candidates_with_index(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token_offset(text, "add", 3),
		"add",
		Some(&workspace.schema_workspace),
	)
	.expect("dynamic alias body completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"add_prestige"));
	assert!(!labels.contains(&"enum[country_tags]"));
}

#[test]
fn completion_suggests_complex_enum_values_from_workspace() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = fixture_text("events/sample.txt");
	let country_candidates = schema_completion_candidates_with_index(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token_offset(&text, "SWE", 2),
		"sw",
		Some(&workspace.schema_workspace),
	)
	.expect("country tag complex enum completion");
	assert_eq!(country_candidates.len(), 1);
	assert_eq!(country_candidates[0].label, "SWE");
	assert_eq!(country_candidates[0].kind, SchemaCompletionKind::EnumMember);
	assert_eq!(
		country_candidates[0].detail,
		"cwt complex_enum country_tags"
	);
	let graphical_candidates = schema_completion_candidates_with_index(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token_offset(&text, "westerngfx", 4),
		"west",
		Some(&workspace.schema_workspace),
	)
	.expect("graphical culture complex enum completion");
	assert_eq!(graphical_candidates.len(), 1);
	assert_eq!(graphical_candidates[0].label, "westerngfx");
	assert_eq!(
		graphical_candidates[0].detail,
		"cwt complex_enum graphical_cultures"
	);
	let text_command_candidates = schema_completion_candidates_with_index(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token_offset(&text, "sample_defined_text", 6),
		"sample",
		Some(&workspace.schema_workspace),
	)
	.expect("defined text complex enum completion");
	assert_eq!(text_command_candidates.len(), 1);
	assert_eq!(text_command_candidates[0].label, "sample_defined_text");
	let dynasty_candidates = schema_completion_candidates_with_index(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token_offset(&text, "von_habsburg", 5),
		"von_h",
		Some(&workspace.schema_workspace),
	)
	.expect("dynasty complex enum completion");
	assert_eq!(dynasty_candidates.len(), 1);
	assert_eq!(dynasty_candidates[0].label, "von_habsburg");
}

#[test]
fn completion_suggests_scripted_effect_fields_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("common/scripted_effects/sample.txt");
	let candidates = schema_completion_candidates(
		&engine,
		game_path("common/scripted_effects/sample.txt"),
		&text,
		position_for_token(&text, "add_prestige"),
		"add",
	)
	.expect("scripted effect schema completion");
	assert_eq!(candidates.len(), 1);
	assert_eq!(candidates[0].label, "add_prestige");
	assert_eq!(candidates[0].kind, SchemaCompletionKind::Field);
	assert!(
		candidates[0]
			.detail
			.starts_with("cwt: Adds prestige directly from a scripted effect body.")
	);
}

#[test]
fn completion_filters_aliases_by_active_scope_when_known() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		&text,
		position_for_token(&text, "add_prestige"),
		"",
	)
	.expect("scope-filtered effect schema completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"add_prestige"));
	assert!(!labels.contains(&"province_only_effect"));
}

#[test]
fn completion_inherits_subtype_scope_for_alias_filtering() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  trigger = {\n    has\n  }\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token_offset(text, "has", 3),
		"has",
	)
	.expect("subtype-scope-filtered trigger completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"has_country_flag"));
	assert!(!labels.contains(&"has_province_flag"));
	assert!(!labels.contains(&"has_sea_flag"));
}

#[test]
fn completion_accepts_parent_scope_aliases_in_subscope_context() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  province_effects = {\n    country_wide_effect = 1\n  }\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token(text, "country_wide_effect"),
		"",
	)
	.expect("subscope effect schema completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"country_wide_effect"));
	assert!(labels.contains(&"add_prestige"));
	assert!(labels.contains(&"province_only_effect"));
}

#[test]
fn completion_uses_cwt_links_to_transition_active_scope() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  province_effects = {\n    owner = {\n      country_wide_effect = 1\n    }\n  }\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		text,
		position_for_token(text, "country_wide_effect"),
		"",
	)
	.expect("scope-link effect schema completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"country_wide_effect"));
	assert!(labels.contains(&"add_prestige"));
	assert!(!labels.contains(&"province_only_effect"));
}

#[test]
fn completion_uses_replace_scope_this_for_alias_filtering() {
	let engine = load_lsp_schema();
	let text = "demo_mission = {\n  provinces_to_highlight = {\n    has\n  }\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("missions/sample.txt"),
		text,
		position_for_token_offset(text, "has", 3),
		"has",
	)
	.expect("replace_scope-filtered trigger completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"has_country_flag"));
	assert!(labels.contains(&"has_province_flag"));
	assert!(!labels.contains(&"has_sea_flag"));
}

#[test]
fn completion_uses_root_type_key_filter_exclusion_for_schema_context() {
	let schema = r#"
		types = {
			type[idea_group] = {
				path = "game/common/ideas"
				subtype[selectable] = {
					category = scalar
				}
			}
			## type_key_filter <> { start trigger bonus ai_will_do }
			type[idea] = {
				path = "game/common/ideas"
				skip_root_key = any
			}
		}

		idea_group = {
			subtype[selectable] = {
				category = scalar
			}
		}

		idea = {
			idea_only = bool
		}
		"#;
	let schema_dir = TempDir::new().expect("create inline schema dir");
	fs::write(schema_dir.path().join("ideas.cwt"), schema).expect("write inline schema");
	let engine = EditorSchema::load_from_directory_with_cache(schema_dir.path(), None)
		.expect("load inline schema");
	let text = "sample_group = {\n  sample_idea = {\n    \n  }\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("common/ideas/sample.txt"),
		text,
		EditorPosition {
			line: 2,
			character: 4,
		},
		"",
	)
	.expect("schema completion under filtered idea root");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"idea_only"));
	assert!(!labels.contains(&"category"));
}

#[test]
fn completion_uses_ordered_skip_root_key_chain_for_schema_context() {
	let schema = r#"
		types = {
			type[game_age] = {
				path = "game/common/ages"
			}
			type[game_age_ability] = {
				path = "game/common/ages"
				skip_root_key = { any abilities }
			}
		}

		game_age = {
			start = int
		}

		game_age_ability = {
			ability_only = bool
		}
		"#;
	let engine = load_inline_lsp_schema(schema);
	let text =
		"age_of_discovery = {\n  abilities = {\n    free_war_taxes = {\n      \n    }\n  }\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("common/ages/sample.txt"),
		text,
		EditorPosition {
			line: 3,
			character: 6,
		},
		"",
	)
	.expect("schema completion under ordered skip_root_key chain");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"ability_only"));
	assert!(!labels.contains(&"start"));
}

#[test]
fn completion_uses_cwt_path_file_for_root_matching() {
	let schema = r#"
		types = {
			type[map_fallback] = {
				path = "game/map"
			}
			type[area] = {
				path = "game/map"
				path_file = "area.txt"
			}
			type[region] = {
				path = "game/map"
				path_file = "region.txt"
			}
		}

		map_fallback = {
			fallback_only = bool
		}

		area = {
			area_only = bool
		}

		region = {
			region_only = bool
		}
		"#;
	let engine = load_inline_lsp_schema(schema);
	let text = "sample_area = {\n  \n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("map/area.txt"),
		text,
		EditorPosition {
			line: 1,
			character: 2,
		},
		"",
	)
	.expect("path_file-specific area schema completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"area_only"));
	assert!(!labels.contains(&"region_only"));
	assert!(!labels.contains(&"fallback_only"));
}

#[test]
fn completion_filters_cwt_rule_subtype_conditions() {
	let engine = load_inline_lsp_schema(conditional_subtype_schema());
	let hidden_text = "sample_event = {\n  hidden = yes\n  \n}\n";
	let hidden_candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		hidden_text,
		EditorPosition {
			line: 2,
			character: 2,
		},
		"",
	)
	.expect("hidden event conditional completion");
	let hidden_labels = hidden_candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(hidden_labels.contains(&"hidden_only"));
	assert!(!hidden_labels.contains(&"visible_only"));
	assert!(!hidden_labels.contains(&"subtype[hidden]"));

	let visible_text = "sample_event = {\n  hidden = no\n  \n}\n";
	let visible_candidates = schema_completion_candidates(
		&engine,
		game_path("events/sample.txt"),
		visible_text,
		EditorPosition {
			line: 2,
			character: 2,
		},
		"",
	)
	.expect("visible event conditional completion");
	let visible_labels = visible_candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(visible_labels.contains(&"visible_only"));
	assert!(!visible_labels.contains(&"hidden_only"));
}

#[test]
fn completion_binds_dynamic_cwt_marker_fields() {
	let engine = load_lsp_schema();
	let text = "demo_mission = {\n  mission_tree = {\n    conquest = {\n      has\n    }\n  }\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("missions/sample.txt"),
		text,
		position_for_token_offset(text, "has", 3),
		"has",
	)
	.expect("dynamic-field trigger completion");
	let labels = candidates
		.iter()
		.map(|candidate| candidate.label.as_str())
		.collect::<Vec<_>>();
	assert!(labels.contains(&"has_country_flag"));
	assert!(labels.contains(&"has_province_flag"));
	assert!(!labels.contains(&"has_sea_flag"));
}

#[test]
fn completion_does_not_suggest_dynamic_marker_literals() {
	let engine = load_lsp_schema();
	let text = "demo_mission = {\n  mission_tree = {\n    \n  }\n}\n";
	let candidates = schema_completion_candidates(
		&engine,
		game_path("missions/sample.txt"),
		text,
		EditorPosition {
			line: 2,
			character: 4,
		},
		"",
	)
	.expect("mission tree schema completion");
	assert!(
		!candidates
			.iter()
			.any(|candidate| candidate.label == "<mission_stage>")
	);
}

#[test]
fn diagnostics_report_alias_scope_mismatches_when_scope_is_known() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  immediate = {\n    province_only_effect = 1\n  }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("province_only_effect")
			&& diagnostic.message.contains("`province`")
			&& diagnostic.message.contains("`country`")
			&& diagnostic.severity == Some(Severity::Error)
	}));
}

#[test]
fn diagnostics_inherit_subtype_scope_for_alias_mismatches() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  trigger = {\n    has_province_flag = demo_flag\n  }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("has_province_flag")
			&& diagnostic.message.contains("`province`")
			&& diagnostic.message.contains("`country`")
			&& diagnostic.severity == Some(Severity::Error)
	}));
}

#[test]
fn diagnostics_filter_cwt_rule_subtype_conditions() {
	let engine = load_inline_lsp_schema(conditional_subtype_schema());
	let text = "\
sample_event = {
  hidden = yes
  hidden_only = yes
  visible_only = yes
}
";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("hidden_only")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("visible_only")
	}));
}

#[test]
fn diagnostics_do_not_accept_type_localisation_metadata_as_script_field() {
	let engine = load_lsp_schema();
	let text = "\
country_event = {
  id = sample.1
  localisation = bad_key
}
";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("localisation")
	}));
}

#[test]
fn diagnostics_use_replace_scope_this_for_alias_mismatches() {
	let engine = load_lsp_schema();
	let text = "demo_mission = {\n  provinces_to_highlight = {\n    has_country_flag = demo_flag\n    has_province_flag = demo_flag\n    has_sea_flag = demo_flag\n  }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("missions/sample.txt"), text);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("has_country_flag")
	}));
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("has_province_flag")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("has_sea_flag")
			&& diagnostic.message.contains("`sea`")
			&& diagnostic.message.contains("`province`")
	}));
}

#[test]
fn diagnostics_bind_dynamic_cwt_marker_fields() {
	let engine = load_lsp_schema();
	let text = "demo_mission = {\n  mission_tree = {\n    conquest = {\n      has_country_flag = demo_flag\n      has_province_flag = demo_flag\n      has_sea_flag = demo_flag\n    }\n  }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("missions/sample.txt"), text);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("conquest")
	}));
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("has_country_flag")
	}));
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("has_province_flag")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("has_sea_flag")
			&& diagnostic.message.contains("`sea`")
			&& diagnostic.message.contains("`province`")
	}));
}

#[test]
fn diagnostics_accept_parent_scope_aliases_in_subscope_context() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  province_effects = {\n    country_wide_effect = 1\n    province_only_effect = 1\n  }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("country_wide_effect")
	}));
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("province_only_effect")
	}));
}

#[test]
fn diagnostics_use_cwt_links_to_transition_active_scope() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  province_effects = {\n    owner = {\n      country_wide_effect = 1\n      province_only_effect = 1\n    }\n  }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("country_wide_effect")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("province_only_effect")
			&& diagnostic.message.contains("`province`")
			&& diagnostic.message.contains("`country`")
	}));
}

#[test]
fn diagnostics_report_unknown_keys_and_cardinality_violations() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/diagnostics.txt");
	let diagnostics =
		schema_diagnostics_for_text(&engine, game_path("events/diagnostics.txt"), &text);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string())
			&& diagnostic.message.contains("mystery_key")
			&& diagnostic.severity == Some(Severity::Warning)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V002".to_string())
			&& diagnostic.message.contains("title")
			&& diagnostic.severity == Some(Severity::Warning)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V003".to_string())
			&& diagnostic.message.contains("ECO")
			&& diagnostic.message.contains("power_categories")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V003".to_string())
			&& diagnostic.message.contains("elsewhere")
			&& diagnostic.message.contains("event_targets")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V003".to_string())
			&& diagnostic.message.contains("nowhere")
			&& diagnostic.message.contains("goto")
			&& diagnostic.message.contains("schema value `event_targets`")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V005".to_string())
			&& diagnostic.message.contains("many")
			&& diagnostic.message.contains("days")
			&& diagnostic.message.contains("int")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V005".to_string())
			&& diagnostic.message.contains("heavy")
			&& diagnostic.message.contains("chance")
			&& diagnostic.message.contains("float")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V005".to_string())
			&& diagnostic.message.contains("maybe")
			&& diagnostic.message.contains("hidden")
			&& diagnostic.message.contains("bool")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V005".to_string())
			&& diagnostic.message.contains("much")
			&& diagnostic.message.contains("add_prestige")
			&& diagnostic.message.contains("int")
			&& diagnostic.severity == Some(Severity::Error)
	}));
}

#[test]
fn diagnostics_accept_static_dynamic_key_markers_from_schema() {
	let engine = load_lsp_schema();
	let text = "\
country_event = {
  dynamic_fields = {
    alpha = 1
    reusable_token = ok
    gamma = 1
  }
}
";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("alpha")
	}));
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("reusable_token")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("gamma")
	}));
}

#[test]
fn diagnostics_accept_workspace_dynamic_key_markers_from_schema() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = "\
country_event = {
  dynamic_fields = {
    SWE = 1
    XXX = 1
  }
}
";
	let diagnostics = schema_diagnostics_for_text_with_index(
		&engine,
		game_path("events/sample.txt"),
		text,
		Some(&workspace.schema_workspace),
	);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("SWE")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("XXX")
	}));
}

#[test]
fn diagnostics_accept_workspace_dynamic_alias_names_from_schema() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = "\
namespace = sample
country_event = {
  category = ADM
  target = root
  trigger = {
    SWE = {
      has_country_flag = demo_flag
    }
  }
  immediate = {
    SWE = {
      add_prestige = much
    }
    XXX = {}
  }
}
";
	let diagnostics = schema_diagnostics_for_text_with_index(
		&engine,
		game_path("events/sample.txt"),
		text,
		Some(&workspace.schema_workspace),
	);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("SWE")
	}));
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string())
			&& diagnostic.message.contains("has_country_flag")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("XXX")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V005".to_string())
			&& diagnostic.message.contains("add_prestige")
			&& diagnostic.message.contains("int")
	}));
}

#[test]
fn diagnostics_validate_complex_enum_values_from_workspace() {
	let engine = load_lsp_schema();
	let workspace = complex_enum_workspace(engine.clone());
	let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  ally = XXX\n  gfx = missinggfx\n  text_command = missing_text\n  dynasty = missing_dynasty\n}\n";
	let diagnostics = schema_diagnostics_for_text_with_index(
		&engine,
		game_path("events/sample.txt"),
		text,
		Some(&workspace.schema_workspace),
	);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V003".to_string())
			&& diagnostic.message.contains("XXX")
			&& diagnostic.message.contains("ally")
			&& diagnostic
				.message
				.contains("schema complex_enum `country_tags`")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V003".to_string())
			&& diagnostic.message.contains("missinggfx")
			&& diagnostic
				.message
				.contains("schema complex_enum `graphical_cultures`")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V003".to_string())
			&& diagnostic.message.contains("missing_text")
			&& diagnostic
				.message
				.contains("schema complex_enum `defined_text_commands`")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V003".to_string())
			&& diagnostic.message.contains("missing_dynasty")
			&& diagnostic
				.message
				.contains("schema complex_enum `dynasty_name`")
	}));
}

#[test]
fn diagnostics_validate_scope_values_from_schema() {
	let engine = load_lsp_schema();
	let text =
		"namespace = sample\ncountry_event = {\n  friend_scope = root\n  friend_scope = sea\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V003".to_string()) && diagnostic.message.contains("value `root`")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V003".to_string())
			&& diagnostic.message.contains("value `sea`")
			&& diagnostic.message.contains("schema scope `country`")
	}));
}

#[test]
fn diagnostics_use_cwt_severity_for_schema_findings() {
	let schema = r#"
		types = {
			type[event] = {
				path = "game/events"
			}
		}

		event = {
			## severity = warning
			gentle_bool = bool

			## required
			## severity = info
			soft_required = scalar

			## cardinality = 1..1
			## severity = info
			singleton = scalar

			## push_scope = country
			trigger = {
				alias_name[trigger] = alias_match_left[trigger]
			}
		}

		## scope = sea
		## severity = warning
		alias[trigger:sea_only_trigger] = bool

		scopes = {
			country = { aliases = { country } }
			sea = { aliases = { sea } }
		}
		"#;
	let engine = load_inline_lsp_schema(schema);
	let text = "\
sample = {
  gentle_bool = maybe
  singleton = first
  singleton = second
  trigger = {
    sea_only_trigger = yes
  }
}
";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V005".to_string())
			&& diagnostic.message.contains("gentle_bool")
			&& diagnostic.severity == Some(Severity::Warning)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V002".to_string())
			&& diagnostic.message.contains("singleton")
			&& diagnostic.severity == Some(Severity::Info)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V004".to_string())
			&& diagnostic.message.contains("soft_required")
			&& diagnostic.severity == Some(Severity::Info)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V007".to_string())
			&& diagnostic.message.contains("sea_only_trigger")
			&& diagnostic.severity == Some(Severity::Warning)
	}));
}

#[test]
fn diagnostics_validate_cwt_ranged_scalar_types() {
	let schema = r#"
		types = {
			type[event] = {
				path = "game/events"
			}
		}

		event = {
			limited_int = int[1..3]
			limited_float = float[-1.0..1.0]
			open_int = int[0..inf]
		}
		"#;
	let engine = load_inline_lsp_schema(schema);
	let text = "\
sample = {
  limited_int = 4
  limited_float = -2.0
  open_int = 99
}
";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V005".to_string())
			&& diagnostic.message.contains("limited_int")
			&& diagnostic.message.contains("int[1..3]")
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V005".to_string())
			&& diagnostic.message.contains("limited_float")
			&& diagnostic.message.contains("float[-1..1]")
	}));
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V005".to_string()) && diagnostic.message.contains("open_int")
	}));
}

#[test]
fn diagnostics_report_missing_required_schema_keys() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  title = sample_title\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V004".to_string())
			&& diagnostic.message.contains("category")
			&& diagnostic.message.contains("at least 1")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V004".to_string())
			&& diagnostic.message.contains("target")
			&& diagnostic.message.contains("at least 1")
			&& diagnostic.severity == Some(Severity::Error)
	}));
}

#[test]
fn diagnostics_report_schema_value_shape_mismatches() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  trigger = yes\n  days = { value = 1 }\n  immediate = {\n    add_prestige = { amount = 5 }\n  }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V006".to_string())
			&& diagnostic.message.contains("trigger")
			&& diagnostic.message.contains("schema block")
			&& diagnostic.message.contains("scalar")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V006".to_string())
			&& diagnostic.message.contains("days")
			&& diagnostic.message.contains("schema scalar")
			&& diagnostic.message.contains("block")
			&& diagnostic.severity == Some(Severity::Error)
	}));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V006".to_string())
			&& diagnostic.message.contains("add_prestige")
			&& diagnostic.message.contains("schema scalar")
			&& diagnostic.message.contains("block")
			&& diagnostic.severity == Some(Severity::Error)
	}));
}

#[test]
fn diagnostics_skip_unknown_keys_inside_alias_bodies() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  trigger = {\n    custom_trigger = {\n      mystery_key = yes\n    }\n  }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("mystery_key")
	}));
}

fn diagnostics_with_code<'a>(
	diagnostics: &'a [SchemaDiagnostic],
	code: &str,
) -> Vec<&'a SchemaDiagnostic> {
	diagnostics
		.iter()
		.filter(|diagnostic| diagnostic.code.as_deref() == Some(code))
		.collect()
}

/// OSS-407: keys above a definition body name instances or skipped wrappers,
/// so they are not checked as fields of that body.
#[test]
fn diagnostics_bind_top_level_keys_as_definition_instances() {
	let schema = EditorSchema::active();
	let documents = [
		(
			"common/scripted_effects/sample.txt",
			"my_effect = {\n\tadd_prestige = 1\n}\n",
		),
		(
			"events/sample.txt",
			"namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\ttrigger = { always = yes }\n\toption = {\n\t\tname = sample.1.a\n\t\tadd_prestige = 1\n\t}\n}\n",
		),
		(
			"decisions/sample.txt",
			"country_decisions = {\n\tsample_decision = {\n\t\tpotential = { always = yes }\n\t\tallow = { always = yes }\n\t\teffect = { add_prestige = 1 }\n\t}\n}\n",
		),
	];
	for (path, text) in documents {
		let diagnostics = schema.diagnostics(game_path(path), text, None);
		assert!(diagnostics.is_empty(), "{path}: {diagnostics:?}");
	}
}

/// OSS-407: a definition body keeps reporting its own missing keys exactly
/// once, at the definition rather than at the top of the file.
#[test]
fn diagnostics_report_missing_definition_key_once() {
	let schema = EditorSchema::active();
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n}\n";
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), text, None);
	let missing = diagnostics_with_code(&diagnostics, "V004");
	assert_eq!(missing.len(), 1, "{diagnostics:?}");
	assert!(missing[0].message.contains("`option`"), "{diagnostics:?}");
	assert_eq!(missing[0].range.start.line, 1, "{diagnostics:?}");
}

/// OSS-407: scope keywords such as `OR` and `ROOT` reach trigger and effect
/// bodies through the schema's `scope_field` alias, not one alias per name.
#[test]
fn diagnostics_accept_scope_keywords_in_alias_rule_bodies() {
	let schema = EditorSchema::active();
	let documents = [
		(
			"common/scripted_triggers/sample.txt",
			"my_trigger = {\n\tOR = {\n\t\talways = yes\n\t\tis_at_war = yes\n\t}\n\tAND = { always = yes }\n\tNOT = { is_at_war = yes }\n\tROOT = { is_at_war = yes }\n}\n",
		),
		(
			"common/scripted_effects/sample.txt",
			"my_effect = {\n\tROOT = { add_prestige = 1 }\n\tFROM = { add_prestige = 1 }\n}\n",
		),
	];
	for (path, text) in documents {
		let diagnostics = schema.diagnostics(game_path(path), text, None);
		assert!(diagnostics.is_empty(), "{path}: {diagnostics:?}");
	}
}

/// `NOR` is no logical block in EU4: the game writes `NOR = { ... }` for
/// Norway (`peace_treaties/00_claim_norwegian_throne.txt`), so it is the tag
/// the workspace defines, switching to a country.
#[test]
fn nor_is_a_country_tag_not_a_logical_block() {
	let schema = EditorSchema::active();
	let tags_path = game_path("common/country_tags/00_countries.txt");
	let tags = "NOR = \"countries/Norway.txt\"\n";
	let effect_path = game_path("common/scripted_effects/sample.txt");
	let effect = "my_effect = {\n\tNOR = { junior_union_with = ROOT }\n}\n";
	let workspace = schema.workspace(&[
		SchemaDocument::new(tags_path, tags),
		SchemaDocument::new(effect_path, effect),
	]);
	let diagnostics = schema.diagnostics(effect_path, effect, Some(&workspace));
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let without_tags = schema.diagnostics(effect_path, effect, None);
	assert!(
		diagnostics_with_code(&without_tags, "V001")
			.iter()
			.any(|diagnostic| diagnostic.message.contains("`NOR`")),
		"{without_tags:?}"
	);
}

/// The fixes for OSS-407 must not hide genuine findings inside definitions.
#[test]
fn diagnostics_still_report_errors_inside_definition_bodies() {
	let schema = EditorSchema::active();
	let effect = "my_effect = {\n\tnot_a_real_effect_key = 1\n}\n";
	let diagnostics = schema.diagnostics(
		game_path("common/scripted_effects/sample.txt"),
		effect,
		None,
	);
	assert!(
		diagnostics_with_code(&diagnostics, "V001")
			.iter()
			.any(|diagnostic| diagnostic.message.contains("not_a_real_effect_key")),
		"{diagnostics:?}"
	);

	let decision = "country_decisions = {\n\tsample_decision = {\n\t\tpotential = {\n\t\t\thas_port = yes\n\t\t\tOR = { has_port = yes }\n\t\t}\n\t\tprovinces_to_highlight = {\n\t\t\tis_at_war = yes\n\t\t\tROOT = { is_at_war = yes }\n\t\t}\n\t\tallow = { always = yes }\n\t\teffect = { add_prestige = 1 }\n\t}\n}\n";
	let diagnostics = schema.diagnostics(game_path("decisions/sample.txt"), decision, None);
	let scope_mismatches = diagnostics_with_code(&diagnostics, "V007");
	let lines = |key: &str| {
		scope_mismatches
			.iter()
			.filter(|diagnostic| diagnostic.message.contains(key))
			.map(|diagnostic| diagnostic.range.start.line)
			.collect::<Vec<_>>()
	};
	assert_eq!(lines("has_port"), vec![3, 4], "{diagnostics:?}");
	assert_eq!(lines("is_at_war"), vec![7], "{diagnostics:?}");
	assert_eq!(scope_mismatches.len(), 3, "{diagnostics:?}");
}

/// A mission inside its series names a `mission` type that has no body rules
/// of its own; its body is described by the series' `<mission>` field.
#[test]
fn diagnostics_check_missions_through_their_series() {
	let schema = EditorSchema::active();
	let path = game_path("missions/sample.txt");
	let text = "sample_series = {\n\tslot = 1\n\tgeneric = yes\n\tpotential = { always = yes }\n\tsample_mission = {\n\t\ticon = mission_build_up_to_force_limit\n\t\trequired_missions = { }\n\t\tposition = 1\n\t\ttrigger = { always = yes }\n\t\teffect = { add_prestige = 1 }\n\t\tnot_a_mission_key = yes\n\t}\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("not_a_mission_key"),
		"{diagnostics:?}"
	);
}

/// Scope keywords and saved event targets name scopes wherever the schema asks
/// for one, as keys and as values.
#[test]
fn diagnostics_accept_scope_keywords_and_event_targets_as_scopes() {
	let schema = EditorSchema::active();
	let text = "my_effect = {\n\tsave_event_target_as = sample_target\n\tevent_target:sample_target = { add_prestige = 1 }\n\tadd_opinion = { who = ROOT modifier = sample_opinion }\n\tadd_opinion = { who = event_target:sample_target modifier = sample_opinion }\n\tadd_opinion = { who = SWE modifier = sample_opinion }\n\tadd_opinion = { who = not_a_scope modifier = sample_opinion }\n}\n";
	let workspace = complex_enum_workspace(schema.clone());
	let diagnostics = schema.diagnostics(
		game_path("common/scripted_effects/sample.txt"),
		text,
		Some(&workspace.schema_workspace),
	);
	assert!(
		diagnostics_with_code(&diagnostics, "V001").is_empty(),
		"{diagnostics:?}"
	);
	let invalid = diagnostics_with_code(&diagnostics, "V003");
	assert_eq!(invalid.len(), 1, "{diagnostics:?}");
	assert!(
		invalid[0].message.contains("not_a_scope"),
		"{diagnostics:?}"
	);
}

/// When the schema lists several forms for one key, a value is checked against
/// the form that fits it, and a block uses the block form's rules.
#[test]
fn diagnostics_choose_the_matching_alternative_for_a_key() {
	let schema = EditorSchema::active();
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = {\n\t\ttrigger = { always = yes }\n\t\tdesc = sample.1.d\n\t}\n\tdesc = sample.1.d2\n\tpicture = {\n\t\ttrigger = { always = yes }\n\t\tpicture = BAD_WINTER_eventPicture\n\t}\n\toption = { name = sample.1.a }\n}\n";
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let wrong = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = {\n\t\tnot_a_desc_key = yes\n\t}\n\toption = { name = sample.1.a }\n}\n";
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), wrong, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("not_a_desc_key"),
		"{diagnostics:?}"
	);
}

/// Rule keys written as a type accept any key of that type: numbered ranks in
/// government names and weighted entries in on-action random events.
#[test]
fn diagnostics_accept_keys_matching_typed_rule_keys() {
	let schema = EditorSchema::active();
	let names = "czech_localisation = {\n\trank = {\n\t\t1 = KNIZECTVI\n\t\t2 = KRALOVSTVI\n\t\t11 = TOO_HIGH\n\t}\n}\n";
	let diagnostics = schema.diagnostics(
		game_path("common/government_names/00_government_names.txt"),
		names,
		None,
	);
	// A number out of the range is the numbered key, of the wrong value.
	let mistyped = diagnostics_with_code(&diagnostics, "V005");
	assert_eq!(mistyped.len(), 1, "{diagnostics:?}");
	assert!(mistyped[0].message.contains("`11`"), "{diagnostics:?}");
	assert!(
		diagnostics_with_code(&diagnostics, "V001").is_empty(),
		"{diagnostics:?}"
	);

	let on_actions = "on_bi_yearly_pulse = {\n\trandom_events = {\n\t\t1000 = 0\n\t\t100 = sample.6\n\t\tnot_a_weight = 0\n\t}\n}\n";
	let diagnostics = schema.diagnostics(
		game_path("common/on_actions/00_on_actions.txt"),
		on_actions,
		None,
	);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("not_a_weight"),
		"{diagnostics:?}"
	);
}

/// Script keys match the schema regardless of letter case, as the game reads
/// them.
#[test]
fn diagnostics_match_keys_regardless_of_case() {
	let schema = EditorSchema::active();
	let text = "government = monarchy\n1490.1.1 = {\n\tmonarch = {\n\t\tname = \"Ahmad Shah I\"\n\t\tdynasty = \"Nizam Shahi\"\n\t\tADM = 4\n\t\tDIP = 6\n\t\tMIL = 4\n\t\tNOT_A_MONARCH_KEY = 1\n\t}\n}\n";
	let diagnostics = schema.diagnostics(
		game_path("history/countries/AHM - Ahmadnagar.txt"),
		text,
		None,
	);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("NOT_A_MONARCH_KEY"),
		"{diagnostics:?}"
	);

	// The rule files spell the random map tile's region count `Regions`.
	let path = game_path("map/random/tiles/tile0.txt");
	let text =
		"regions = 2\nregion = { 145 203 116 }\nsize = { 4 4 }\nweight = 10\nempty = { 0 0 0 }\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "regions = { 1 2 3 }\nregion = { 145 203 116 }\nsize = { 4 4 }\nweight = 10\nempty = { 0 0 0 }\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(
		diagnostics_with_code(&diagnostics, "V006").len(),
		1,
		"{diagnostics:?}"
	);
}

/// Values match an enum without regard to case, as CWTools matches them;
/// values outside it are still reported.
#[test]
fn diagnostics_match_enum_values_regardless_of_case() {
	let schema = EditorSchema::active();
	let path = game_path("events/sample.txt");
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\toption = {\n\t\tname = sample.1.a\n\t\tremove_advisor_by_category = adm\n\t\tchange_national_focus = Mil\n\t}\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\toption = {\n\t\tname = sample.1.a\n\t\tremove_advisor_by_category = admin\n\t}\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(
		diagnostics_with_code(&diagnostics, "V003").len(),
		1,
		"{diagnostics:?}"
	);
}

/// A leading `+` signs a number, as the game and CWTools read it; text that
/// is not a number after it is still reported.
#[test]
fn diagnostics_read_a_leading_plus_as_a_sign() {
	let schema = EditorSchema::active();
	let path = game_path("common/event_modifiers/sample.txt");
	let text = "sample_modifier = {\n\tyearly_absolutism = +0.5\n\tglobal_unrest = +1\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "sample_modifier = {\n\tyearly_absolutism = +half\n\tglobal_unrest = +-1\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
	assert_eq!(
		diagnostics_with_code(&diagnostics, "V005").len(),
		2,
		"{diagnostics:?}"
	);
}

/// Every event shares the `country_event` key; each one is checked with its
/// own subtypes, not those of the first event in the file. A key with several
/// block forms is checked against the form that fits its contents.
#[test]
fn diagnostics_check_each_same_key_definition_on_its_own() {
	let schema = EditorSchema::active();
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\tis_triggered_only = yes\n\tmean_time_to_happen = {\n\t\tdays = 1\n\t\tmodifier = { factor = 0.5 any_owned_province = { has_province_flag = sample_flag } }\n\t\tnot_an_mtth_key = yes\n\t}\n\toption = { name = sample.1.a }\n}\ncountry_event = {\n\tid = sample.2\n\ttitle = sample.2.t\n\tdesc = sample.2.d\n\tmean_time_to_happen = {\n\t\tmonths = 120\n\t\tmodifier = { factor = 0.5 always = yes }\n\t}\n\toption = { name = sample.2.a }\n}\n";
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("not_an_mtth_key"),
		"{diagnostics:?}"
	);
}

/// Rules nested under a key can depend on the definition's subtype: an event
/// that is not triggered-only takes any `months`/`days` in its mean time to
/// happen.
#[test]
fn diagnostics_follow_subtype_conditions_in_nested_blocks() {
	let schema = EditorSchema::active();
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\tmean_time_to_happen = {\n\t\tmonths = 12\n\t\tnot_an_mtth_key = yes\n\t}\n\toption = { name = sample.1.a }\n}\n";
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("not_an_mtth_key"),
		"{diagnostics:?}"
	);
}

/// A per-file history type picks its subtypes from the whole file: a subtype
/// written as `owner = scalar` holds whenever the file sets any `owner`, and
/// dated blocks use the `date_field` rules.
#[test]
fn diagnostics_activate_per_file_subtypes_from_typed_rules() {
	let schema = EditorSchema::active();
	let text = "add_core = SWE\nowner = SWE\ncontroller = SWE\nculture = swedish\nreligion = catholic\nhre = no\nbase_tax = many\nbase_production = 5\ntrade_goods = grain\nbase_manpower = 3\ncapital = \"Stockholm\"\nis_city = yes\ndiscovered_by = western\n1501.8.1 = { controller = DAN base_manpower = many }\n";
	let diagnostics = schema.diagnostics(game_path("history/provinces/1-Uppland.txt"), text, None);
	let type_errors = diagnostics_with_code(&diagnostics, "V005");
	assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
	assert_eq!(type_errors.len(), 2, "{diagnostics:?}");
	assert!(
		type_errors[0].message.contains("base_tax"),
		"{diagnostics:?}"
	);
	assert!(
		type_errors[1].message.contains("base_manpower"),
		"{diagnostics:?}"
	);
}

/// A scope-switching block such as `random_owned_province` checks its body in
/// the scope the schema pushes for it, not in the scope around it.
#[test]
fn diagnostics_follow_scope_switching_aliases() {
	let schema = EditorSchema::active();
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\timmediate = {\n\t\trandom_owned_province = {\n\t\t\tlimit = { is_capital = no }\n\t\t\tset_province_flag = sample_flag\n\t\t\tadd_prestige = 1\n\t\t}\n\t}\n\toption = { name = sample.1.a }\n}\n";
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), text, None);
	let scope_mismatches = diagnostics_with_code(&diagnostics, "V007");
	assert_eq!(scope_mismatches.len(), 1, "{diagnostics:?}");
	assert!(
		scope_mismatches[0].message.contains("add_prestige"),
		"{diagnostics:?}"
	);
}

/// A key the schema defines both as a value and as a block that switches
/// scope (`home_trade_node = <trade_node>` or `home_trade_node = { ... }` in
/// the country's home trade node) checks its block in the switched scope.
#[test]
fn diagnostics_follow_the_scope_of_a_keys_block_form() {
	let schema = EditorSchema::active();
	let event = |trigger: &str| {
		format!(
			"namespace = sample\ncountry_event = {{\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\ttrigger = {{\n\t\thome_trade_node = {{\n{trigger}\n\t\t}}\n\t}}\n\toption = {{ name = sample.1.a }}\n}}\n"
		)
	};
	let text = event("\t\t\ttrade_share = { country = ROOT share = 75 }");
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), &text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = event("\t\t\tis_at_war = yes");
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), &text, None);
	let scope_mismatches = diagnostics_with_code(&diagnostics, "V007");
	assert_eq!(scope_mismatches.len(), 1, "{diagnostics:?}");
	assert!(
		scope_mismatches[0].message.contains("is_at_war"),
		"{diagnostics:?}"
	);
}

/// CWTools reads a scope name the schema's scope list lacks as any scope. The
/// rule files write `push_scope = { root = province this = province }` for
/// `add_owned_provinces_development_ducats`' trigger and list `capital_scope`
/// among `trade_range`'s scopes; neither is reported as a scope mismatch.
#[test]
fn diagnostics_read_unlisted_schema_scopes_as_any_scope() {
	let schema = EditorSchema::active();
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\ttrigger = {\n\t\tcapital_scope = { trade_range = ROOT }\n\t}\n\timmediate = {\n\t\tadd_owned_provinces_development_ducats = {\n\t\t\tmultiplier = 1\n\t\t\ttrigger = { owner = { ruler_culture = PREV } }\n\t\t}\n\t}\n\toption = { name = sample.1.a }\n}\n";
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// The schema defines some triggers several times, once per value form or
/// scope (`stability` takes a number or a country; `culture` reads a culture
/// in country scope and a culture or scope in province scope). A value that
/// fits any definition allowed in the current scope is accepted; one that
/// fits none, or a trigger none of whose definitions is allowed in the
/// current scope, is still reported.
#[test]
fn diagnostics_accept_any_definition_of_an_overloaded_alias() {
	let schema = EditorSchema::active();
	let workspace = complex_enum_workspace(schema.clone());
	let workspace = Some(&workspace.schema_workspace);
	let event = |trigger: &str| {
		format!(
			"namespace = sample\ncountry_event = {{\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\ttrigger = {{\n{trigger}\n\t}}\n\toption = {{ name = sample.1.a }}\n}}\n"
		)
	};
	let text = event(
		"\t\tstability = 2\n\t\tstability = FRA\n\t\tculture = austrian\n\t\tcapital_scope = { culture = ROOT owned_by = ROOT }",
	);
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), &text, workspace);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = event("\t\tstability = 7");
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), &text, workspace);
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert!(
		diagnostics[0].message.contains("stability"),
		"{diagnostics:?}"
	);

	let text = event("\t\towned_by = ROOT");
	let diagnostics = schema.diagnostics(game_path("events/sample.txt"), &text, workspace);
	let scope_mismatches = diagnostics_with_code(&diagnostics, "V007");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(scope_mismatches.len(), 1, "{diagnostics:?}");
	assert!(
		scope_mismatches[0].message.contains("owned_by"),
		"{diagnostics:?}"
	);
}

/// Most modifiers appear only in the schema's `modifiers` list, which names
/// each with its scope. Any of them is accepted where a modifier is, with a
/// number value; a name the list lacks or a non-number value is reported.
#[test]
fn diagnostics_accept_modifiers_the_schema_lists_by_name() {
	let schema = EditorSchema::active();
	let path = game_path("common/advisortypes/sample.txt");
	let text = "sample_advisor = {\n\tmonarch_power = ADM\n\tglobal_unrest = -1\n\tlocal_unrest = 1\n\tskill_scaled_modifier = { modifier = { global_tax_modifier = 0.1 } }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "sample_advisor = {\n\tmonarch_power = ADM\n\tnot_a_modifier = 1\n\tglobal_unrest = yes\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
	assert_eq!(
		diagnostics_with_code(&diagnostics, "V001").len(),
		1,
		"{diagnostics:?}"
	);
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("not_a_modifier")),
		"{diagnostics:?}"
	);
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("global_unrest")),
		"{diagnostics:?}"
	);
}

/// The scope the `modifiers` list gives a modifier is its category, not a
/// limit on where it is written: a trade good's province modifier block takes
/// country modifiers, as vanilla's `province = { land_forcelimit = 0.5 }`.
#[test]
fn diagnostics_accept_listed_modifiers_in_any_scope() {
	let schema = EditorSchema::active();
	let path = game_path("common/tradegoods/sample.txt");
	let text = "sample_good = {\n\tcolor = { 0.5 0.5 0.5 }\n\tmodifier = { land_forcelimit_modifier = 0.2 }\n\tprovince = { land_forcelimit = 0.5 local_unrest = 1 }\n\tchance = { factor = 1 }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A key the schema writes as a type (`<scripted_effect>`, `<province_id>`)
/// is the name of a workspace definition of that type: a scripted effect or
/// trigger called by name, a province scoped by its id, a province named in
/// `province_names`. A name the workspace does not define is still reported.
#[test]
fn diagnostics_accept_workspace_definitions_named_by_type_keys() {
	let schema = EditorSchema::active();
	let documents = [
		SchemaDocument::new(
			game_path("common/scripted_effects/sample.txt"),
			"sample_reward_effect = { add_prestige = 1 }\n",
		),
		SchemaDocument::new(
			game_path("common/scripted_triggers/sample.txt"),
			"sample_check_trigger = { stability = 1 }\n",
		),
		SchemaDocument::new(
			game_path("map/positions.txt"),
			"151 = { position = { 1.0 2.0 } }\n",
		),
	];
	let workspace = schema.workspace(&documents);
	let event = |trigger: &str, effect: &str| {
		format!(
			"namespace = sample\ncountry_event = {{\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\ttrigger = {{ {trigger} }}\n\timmediate = {{ {effect} }}\n\toption = {{ name = sample.1.a }}\n}}\n"
		)
	};
	let path = game_path("events/sample.txt");
	let text = event(
		"sample_check_trigger = yes",
		"sample_reward_effect = yes 151 = { add_base_tax = 1 }",
	);
	let diagnostics = schema.diagnostics(path, &text, Some(&workspace));
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = event(
		"sample_missing_trigger = yes",
		"sample_missing_effect = yes",
	);
	let diagnostics = schema.diagnostics(path, &text, Some(&workspace));
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
	assert_eq!(unknown.len(), 2, "{diagnostics:?}");

	let path = game_path("common/province_names/sample.txt");
	let diagnostics = schema.diagnostics(path, "151 = \"Sample\"\n", Some(&workspace));
	assert!(diagnostics.is_empty(), "{diagnostics:?}");
	let diagnostics = schema.diagnostics(path, "9999 = \"Nowhere\"\n", Some(&workspace));
	assert_eq!(
		diagnostics_with_code(&diagnostics, "V001").len(),
		1,
		"{diagnostics:?}"
	);
}

/// A cardinality on an alias rule (`## cardinality = 0..1 alias_name[effect]`
/// in an event option) does not limit each effect to one use, matching
/// CWTools; a plain field still keeps its cardinality.
#[test]
fn diagnostics_do_not_apply_cardinality_to_alias_matches() {
	let schema = EditorSchema::active();
	let path = game_path("events/sample.txt");
	let event = |extra: &str| {
		format!(
			"namespace = sample\ncountry_event = {{\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\toption = {{\n\t\tname = sample.1.a\n\t\ttrigger = {{ stability = 1 }}\n{extra}\t\tif = {{ limit = {{ stability = 1 }} add_prestige = 1 }}\n\t\tif = {{ limit = {{ stability = 2 }} add_prestige = 2 }}\n\t\tcustom_tooltip = sample.1.tt\n\t\tcustom_tooltip = sample.1.tt2\n\t}}\n}}\n"
		)
	};
	let diagnostics = schema.diagnostics(path, &event(""), None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let diagnostics = schema.diagnostics(path, &event("\t\ttrigger = { stability = 2 }\n"), None);
	let cardinality = diagnostics_with_code(&diagnostics, "V002");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(cardinality.len(), 1, "{diagnostics:?}");
	assert!(
		cardinality[0].message.contains("trigger"),
		"{diagnostics:?}"
	);
}

/// Files in a folder shared by several types (`common/ideas`) bind each
/// definition to its type before subtypes are chosen, so a national idea
/// group's `start`, `bonus` and `free` are known keys. Any other key is an
/// idea, which must still be a block.
#[test]
fn diagnostics_activate_subtypes_for_shared_folder_definitions() {
	let schema = EditorSchema::active();
	let path = game_path("common/ideas/sample.txt");
	let text = "sample_ideas = {\n\tstart = { land_morale = 0.1 }\n\tbonus = { infantry_power = 0.1 }\n\ttrigger = { tag = FRA }\n\tfree = yes\n\tsample_idea_1 = { land_morale = 0.1 }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "sample_ideas = {\n\tstart = { land_morale = 0.1 }\n\tfree = yes\n\tmystery_key = yes\n\tsample_idea_1 = { land_morale = 0.1 }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let shape = diagnostics_with_code(&diagnostics, "V006");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(shape.len(), 1, "{diagnostics:?}");
	assert!(shape[0].message.contains("mystery_key"), "{diagnostics:?}");
}

/// `history/diplomacy` files are one definition each. A file of dated blocks
/// named by a subtype's key filter (`hre.txt`) takes that subtype; one without
/// dated blocks takes the subtype listing relations (`vassal`, `alliance`).
#[test]
fn diagnostics_select_per_file_subtypes_by_file_name_and_absent_keys() {
	let schema = EditorSchema::active();
	let relations = "alliance = {\n\tfirst = FRA\n\tsecond = SCO\n\tstart_date = 1444.1.1\n\tend_date = 1500.1.1\n}\nroyal_marriage = {\n\tfirst = FRA\n\tsecond = SCO\n\tstart_date = 1444.1.1\n\tend_date = 1500.1.1\n}\n";
	let path = game_path("history/diplomacy/sample_alliances.txt");
	let diagnostics = schema.diagnostics(path, relations, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = format!(
		"{relations}alliance = {{\n\tfirst = FRA\n\tsecond = ENG\n\tmystery_term = yes\n\tstart_date = 1444.1.1\n\tend_date = 1500.1.1\n}}\n"
	);
	let diagnostics = schema.diagnostics(path, &text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("mystery_term"),
		"{diagnostics:?}"
	);

	let emperors = "1399.1.1 = { emperor = BOH }\n1437.12.9 = { emperor = HAB }\n";
	let diagnostics = schema.diagnostics(game_path("history/diplomacy/hre.txt"), emperors, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");
	let diagnostics = schema.diagnostics(
		game_path("history/diplomacy/celestial_empire.txt"),
		"1368.1.23 = { celestial_emperor = MNG }\n",
		None,
	);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// Two forms of one effect can share their keys and differ only in what a
/// value may be (`add_estate_loyalty` takes an estate or `all`). The form
/// whose values the block satisfies is the one checked.
#[test]
fn diagnostics_choose_the_block_form_whose_values_fit() {
	let schema = EditorSchema::active();
	let documents = [SchemaDocument::new(
		game_path("common/estates/sample.txt"),
		"estate_church = { icon = 1 }\n",
	)];
	let workspace = schema.workspace(&documents);
	let event = |estate: &str, loyalty: &str| {
		format!(
			"namespace = sample\ncountry_event = {{\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\toption = {{\n\t\tname = sample.1.a\n\t\tadd_estate_loyalty = {{ estate = {estate} loyalty = {loyalty} }}\n\t}}\n}}\n"
		)
	};
	let path = game_path("events/sample.txt");
	for estate in ["estate_church", "all"] {
		let diagnostics = schema.diagnostics(path, &event(estate, "2"), Some(&workspace));
		assert!(diagnostics.is_empty(), "{estate}: {diagnostics:?}");
	}

	let diagnostics = schema.diagnostics(path, &event("estate_church", "lots"), Some(&workspace));
	let type_mismatches = diagnostics_with_code(&diagnostics, "V005");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(type_mismatches.len(), 1, "{diagnostics:?}");
	assert!(
		type_mismatches[0].message.contains("loyalty"),
		"{diagnostics:?}"
	);
}

/// A trade node is named by its location, so a province id that hosts one
/// also names the trade node; the key is still known.
#[test]
fn diagnostics_accept_keys_naming_several_definitions() {
	let schema = EditorSchema::active();
	let documents = [
		SchemaDocument::new(
			game_path("map/positions.txt"),
			"45 = { position = { 1.0 2.0 } }\n",
		),
		SchemaDocument::new(
			game_path("common/tradenodes/sample.txt"),
			"lubeck = { location = 45 }\n",
		),
	];
	let workspace = schema.workspace(&documents);
	let path = game_path("events/sample.txt");
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\ttrigger = { 45 = { is_state = yes } }\n\toption = { name = sample.1.a 45 = { add_base_tax = 1 } }\n}\n";
	let diagnostics = schema.diagnostics(path, text, Some(&workspace));
	assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A scripted effect's `$parameter$` stands for whatever a call passes, so a
/// key or value holding one is not checked; the rest of the body still is.
#[test]
fn diagnostics_skip_script_parameters_in_scripted_effects() {
	let schema = EditorSchema::active();
	let path = game_path("common/scripted_effects/sample.txt");
	let text = "sample_effect = {\n\t$province$ = { add_base_tax = 1 }\n\t$type$_tradition = 1\n\tadd_years_of_income = $amount$\n\tadd_prestige = $prestige$\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "sample_effect = {\n\t$province$ = { add_base_tax = 1 }\n\tmystery_effect = yes\n\tadd_years_of_income = lots\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
	assert_eq!(
		diagnostics_with_code(&diagnostics, "V001").len(),
		1,
		"{diagnostics:?}"
	);
	assert_eq!(
		diagnostics_with_code(&diagnostics, "V005").len(),
		1,
		"{diagnostics:?}"
	);
}

/// `[[name] ... ]` keeps its body only when a call passes `name`; the body is
/// checked whether or not the markers touch the text inside them. A key that
/// is a whole `$parameter$` with a plain value is whatever call the caller
/// names, so it is not checked as the scope a parameter opens.
#[test]
fn diagnostics_read_through_conditional_parameter_blocks() {
	let schema = EditorSchema::active();
	let path = game_path("common/scripted_effects/sample.txt");
	let text = "sample_effect = {\n\t[[tooltip]custom_tooltip = $tooltip$]\n\t[[prestige] add_prestige = 1]\n\t[[!prestige]add_prestige = 2 ]\n\t[[action]\n\t$action$ = estate_church\n\t]\n\tif = { limit = { NOT = { $type$ = 6 } } add_prestige = 1 }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "sample_effect = {\n\t[[tooltip]mystery_effect = yes]\n\t[[prestige] add_prestige = lots]\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	let invalid = diagnostics_with_code(&diagnostics, "V005");
	assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("`mystery_effect`"),
		"{diagnostics:?}"
	);
	assert_eq!(unknown[0].range.start.line, 1);
	assert_eq!(unknown[0].range.start.character, 11);
	assert_eq!(invalid.len(), 1, "{diagnostics:?}");
	assert!(invalid[0].message.contains("`lots`"), "{diagnostics:?}");
}

/// A modifier named from a definition between fixed text
/// (`monthly_<government_mechanic_power>`) is known for every government
/// mechanic power the workspace defines, and only for those.
#[test]
fn diagnostics_accept_modifiers_named_from_workspace_definitions() {
	let schema = EditorSchema::active();
	let documents = [SchemaDocument::new(
		game_path("common/government_mechanics/sample.txt"),
		"sample_mechanic = {\n\tpowers = {\n\t\tsample_power = { max = 100 }\n\t}\n}\n",
	)];
	let workspace = schema.workspace(&documents);
	let path = game_path("common/event_modifiers/sample.txt");
	let text =
		"sample_modifier = {\n\tmonthly_sample_power = 1\n\tsample_power_gain_modifier = 0.1\n}\n";
	let diagnostics = schema.diagnostics(path, text, Some(&workspace));
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "sample_modifier = {\n\tmonthly_missing_power = 1\n}\n";
	let diagnostics = schema.diagnostics(path, text, Some(&workspace));
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("monthly_missing_power"),
		"{diagnostics:?}"
	);
}

/// The game creates countries such as trading cities (`T00`) and federations
/// (`F00`) at runtime, so no file defines their tags; other unknown tags are
/// still reported.
#[test]
fn diagnostics_accept_country_tags_the_game_creates() {
	let schema = EditorSchema::active();
	let documents = [SchemaDocument::new(
		game_path("common/country_tags/00_countries.txt"),
		"SWE = \"countries/Sweden.txt\"\n",
	)];
	let workspace = schema.workspace(&documents);
	let path = game_path("events/sample.txt");
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\ttrigger = {\n\t\tOR = { tag = T00 tag = T74 tag = F19 tag = C74 tag = K99 tag = S09 tag = SWE }\n\t\tT00 = { exists = yes }\n\t}\n\toption = { name = sample.1.a }\n}\n";
	let diagnostics = schema.diagnostics(path, text, Some(&workspace));
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\ttrigger = { OR = { tag = T75 tag = F20 tag = A00 tag = T0A tag = T100 } }\n\toption = { name = sample.1.a }\n}\n";
	let diagnostics = schema.diagnostics(path, text, Some(&workspace));
	let rejected = diagnostics_with_code(&diagnostics, "V003");
	assert_eq!(diagnostics.len(), 5, "{diagnostics:?}");
	assert_eq!(rejected.len(), 5, "{diagnostics:?}");
}

/// A mod defining a country under a tag the game hands out at runtime is
/// warned; tags outside those ranges are not.
#[test]
fn diagnostics_warn_on_countries_defined_under_reserved_tags() {
	let schema = EditorSchema::active();
	let path = game_path("common/country_tags/01_sample.txt");
	let text = "SWE = \"countries/Sweden.txt\"\nT05 = \"countries/Hansa.txt\"\nT75 = \"countries/Other.txt\"\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let reserved = diagnostics_with_code(&diagnostics, "V010");
	assert_eq!(reserved.len(), 1, "{diagnostics:?}");
	assert!(reserved[0].message.contains("T05"), "{diagnostics:?}");
	assert_eq!(reserved[0].severity, Some(Severity::Warning));
}

/// `trigger_switch` names some cases (`advisor_events_adm = { ... }`) beside
/// a case of any name holding any effect; a named case holding other effects
/// is that general case, and unknown effects are still reported.
#[test]
fn diagnostics_let_named_keys_take_the_general_form_that_fits() {
	let schema = EditorSchema::active();
	let path = game_path("events/sample.txt");
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\toption = {\n\t\tname = sample.1.a\n\t\ttrigger_switch = {\n\t\t\ton_trigger = has_country_flag\n\t\t\tadvisor_events_adm = { remove_advisor_by_category = ADM add_adm_power = -50 }\n\t\t\tadvisor_events_dip = { clr_country_flag = advisor_events_dip }\n\t\t}\n\t}\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\ttitle = sample.1.t\n\tdesc = sample.1.d\n\toption = {\n\t\tname = sample.1.a\n\t\ttrigger_switch = {\n\t\t\ton_trigger = has_country_flag\n\t\t\tadvisor_events_adm = { add_adm_power = -50 mystery_effect = yes }\n\t\t\tadvisor_events_dip = { add_dip_power = -50 }\n\t\t}\n\t}\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("mystery_effect"),
		"{diagnostics:?}"
	);
}

/// A scripted effect defined under a name the schema also lists as a call
/// (`simple_dynamic_effect = { first_limit = ... }`) is checked as a
/// definition of effects, not as that call.
#[test]
fn diagnostics_check_definitions_named_like_listed_calls_as_definitions() {
	let schema = EditorSchema::active();
	let path = game_path("common/scripted_effects/sample.txt");
	let text = "simple_dynamic_effect = {\n\tcustom_tooltip = $first_custom_tooltip$\n\tif = {\n\t\tlimit = { $first_limit$ }\n\t\tcustom_tooltip = BONUS_REWARD_YES\n\t}\n\telse = { custom_tooltip = BONUS_REWARD_NO }\n\ttooltip = { $first_effect$ }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "simple_dynamic_effect = {\n\tif = { limit = { always = yes } add_prestige = 1 }\n\tmystery_effect = yes\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("mystery_effect"),
		"{diagnostics:?}"
	);
}

/// `dlc_metadata/dlc_info` holds two types under different wrappers:
/// categories under `categories` and DLCs under `dlcs`.
#[test]
fn diagnostics_bind_each_wrapper_of_a_shared_file_to_its_own_type() {
	let schema = EditorSchema::active();
	let path = game_path("dlc_metadata/dlc_info/00_dlc_info.txt");
	let text = "dlcs = {\n\tdlc003 = {\n\t\tname = \"Winged Hussars Unit Pack\"\n\t\tid = 625170\n\t\tcategory = unit_pack\n\t}\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "dlcs = {\n\tdlc003 = {\n\t\tname = \"Winged Hussars Unit Pack\"\n\t\tmystery_setting = yes\n\t}\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("mystery_setting"),
		"{diagnostics:?}"
	);
}

/// `defaults_reform` is the default government reform, whose body lists the
/// settings every reform starts from.
#[test]
fn diagnostics_check_the_default_reform_against_its_own_fields() {
	let schema = EditorSchema::active();
	let path = game_path("common/government_reforms/00_government_reforms.txt");
	let text = "defaults_reform = {\n\tvalid_for_new_country = yes\n\tclaim_states = no\n\thas_parliament = no\n\ttrade_city_reform = \"\"\n\tmin_autonomy = 0\n\tassimilation_cultures = { }\n\tstates_general_mechanic = { }\n\tcan_remove_idea_group = yes\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "defaults_reform = {\n\tclaim_states = no\n\tmystery_setting = yes\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("mystery_setting"),
		"{diagnostics:?}"
	);
}

/// A subtype selected by an optional field (`church_aspect`: `is_blessing =
/// no`, at most once) holds for definitions that leave the field out, and not
/// for those that set it otherwise.
#[test]
fn diagnostics_select_subtypes_whose_optional_fields_are_left_out() {
	let schema = EditorSchema::active();
	let path = game_path("common/church_aspects/00_church_aspects.txt");
	let text = "sample_aspect = {\n\tcost = 100\n\tsprite = GFX_sample_aspect\n\tmonarch_power = adm\n\tmodifier = { global_tax_modifier = 0.1 }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "sample_blessing = {\n\tis_blessing = yes\n\tcost = 100\n\tmodifier = { global_tax_modifier = 0.1 }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(unknown[0].message.contains("`cost`"), "{diagnostics:?}");
}

/// A rule stated twice with the same key and value allows the looser repeat
/// count; a rule stated once keeps its own.
#[test]
fn diagnostics_let_a_restated_rule_raise_the_repeat_limit() {
	let engine = load_inline_lsp_schema(
		r#"
		types = {
			type[sample] = { path = "game/common/samples" }
		}
		sample = {
			## cardinality = 0..1
			reform = scalar
			## cardinality = 0..inf
			reform = scalar
			## cardinality = 0..1
			rank = int
		}
		"#,
	);
	let text = "sample = {\n\treform = a\n\treform = b\n\treform = c\n\trank = 1\n\trank = 2\n}\n";
	let diagnostics =
		schema_diagnostics_for_text(&engine, game_path("common/samples/sample.txt"), text);
	let repeated = diagnostics_with_code(&diagnostics, "V002");
	assert_eq!(repeated.len(), 1, "{diagnostics:?}");
	assert!(repeated[0].message.contains("`rank`"), "{diagnostics:?}");
}

/// `## starts_with` selects a subtype by key prefix: every
/// `on_harmonized_<group>` on_action takes the harmonization body.
#[test]
fn diagnostics_select_subtypes_by_key_prefix() {
	let schema = EditorSchema::active();
	let path = game_path("common/on_actions/00_on_actions.txt");
	let text = "on_harmonized_pagan = {\n\tevents = { sample.1 }\n\trandom_events = { 100 = sample.2 }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "on_harmonized_pagan = {\n\tevents = { sample.1 }\n\tmystery_setting = yes\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("mystery_setting"),
		"{diagnostics:?}"
	);
}

/// P-687: the game reading a different value than the file states is
/// reported: extra decimals as a notice, a misspelled boolean as an error.
#[test]
fn value_coercion_reports_where_the_game_reads_a_different_value() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  id = sample.3\n  hidden = YES\n  immediate = { add_prestige = 0.1234 }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);

	assert!(
		diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some("V008".to_string())
				&& diagnostic.message.contains("add_prestige")
				&& diagnostic.message.contains("0.123")
				&& diagnostic.message.contains("discards `4`")
				&& diagnostic.severity == Some(Severity::Info)
		}),
		"{diagnostics:?}"
	);
	assert!(
		diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some("V009".to_string())
				&& diagnostic.message.contains("hidden")
				&& diagnostic.message.contains("YES")
				&& diagnostic.severity == Some(Severity::Error)
		}),
		"{diagnostics:?}"
	);
}

/// Trailing zeros read back as written — `0.5000` is `0.500` — so the notice
/// says the value is unchanged rather than naming discarded digits.
#[test]
fn value_coercion_notes_dropped_zeros_as_unchanged() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  id = sample.4\n  hidden = yes\n  immediate = { add_prestige = 0.5000 }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);

	let truncated = diagnostics_with_code(&diagnostics, "V008");
	assert_eq!(truncated.len(), 1, "{diagnostics:?}");
	assert_eq!(truncated[0].severity, Some(Severity::Info));
	assert!(
		truncated[0].message.contains("unchanged"),
		"{diagnostics:?}"
	);
	assert!(
		!diagnostics
			.iter()
			.any(|diagnostic| diagnostic.code == Some("V009".to_string())),
		"the exact lowercase spelling is the boolean: {diagnostics:?}"
	);
}

/// The game's lexer strips quotes and its numeric readers read the text, so a
/// quoted numeral is the number it spells: vanilla's `duration = "-1"` is a
/// permanent modifier, and a quoted fourth decimal is still discarded where
/// the game drops it.
#[test]
fn diagnostics_read_a_quoted_numeral_as_its_number() {
	let engine = load_inline_lsp_schema(
		r#"
		types = {
			type[sample] = { path = "game/common/samples" }
		}
		sample = {
			## cardinality = 0..1
			duration = int
			## cardinality = 0..1
			chance = float
			## cardinality = 0..1
			modifier = {
				alias_name[modifier] = alias_match_left[modifier]
			}
		}
		alias[modifier:discipline] = float
		"#,
	);
	let path = game_path("common/samples/sample.txt");
	for text in [
		"sample = {\n\tduration = \"7300\"\n}\n",
		"sample = {\n\tduration = \"-1\"\n\tchance = \"0.5\"\n}\n",
	] {
		let diagnostics = schema_diagnostics_for_text(&engine, path, text);
		assert!(diagnostics.is_empty(), "{text}: {diagnostics:?}");
	}

	let text = "sample = {\n\tduration = \"long\"\n\tchance = \"0.0025\"\n\tmodifier = { discipline = \"0.0025\" }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, path, text);
	assert_eq!(
		diagnostics_with_code(&diagnostics, "V005").len(),
		1,
		"{diagnostics:?}"
	);
	let truncated = diagnostics_with_code(&diagnostics, "V008");
	assert_eq!(truncated.len(), 1, "{diagnostics:?}");
	assert_eq!(truncated[0].severity, Some(Severity::Info));
	assert!(
		truncated[0].message.contains("`discipline`"),
		"{diagnostics:?}"
	);
}

/// Writing a one-value field again with the value it already holds changes
/// nothing; writing a different value is the repeat worth reporting. A block
/// written again unchanged is only a notice, as the game may run it twice.
#[test]
fn diagnostics_report_a_repeated_field_by_whether_its_value_changes() {
	let engine = load_inline_lsp_schema(
		r#"
		types = {
			type[sample] = { path = "game/common/samples" }
		}
		sample = {
			## cardinality = 0..1
			rank = int
			## cardinality = 0..1
			school = scalar
			## cardinality = 0..1
			chance = { factor = float }
		}
		"#,
	);
	let path = game_path("common/samples/sample.txt");
	let text = "sample = {\n\tschool = shafii\n\tschool = \"shafii\"\n\trank = 1\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, path, text);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "sample = {\n\trank = 1\n\trank = 1\n\trank = 2\n\tchance = { factor = 2 }\n\tchance = {\n\t\t# the same factor\n\t\tfactor = 2\n\t}\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, path, text);
	let repeated = diagnostics_with_code(&diagnostics, "V002");
	assert_eq!(repeated.len(), 2, "{diagnostics:?}");
	let rank = repeated
		.iter()
		.find(|diagnostic| diagnostic.message.contains("`rank`"))
		.expect("a changed value is reported");
	assert_eq!(rank.severity, Some(Severity::Warning));
	let chance = repeated
		.iter()
		.find(|diagnostic| diagnostic.message.contains("`chance`"))
		.expect("an unchanged block may still be read twice");
	assert_eq!(chance.severity, Some(Severity::Info));
	assert_eq!(chance.range.start.line, 5);

	let text = "sample = {\n\tchance = { factor = 2 }\n\tchance = { factor = 3 }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, path, text);
	let repeated = diagnostics_with_code(&diagnostics, "V002");
	assert_eq!(repeated.len(), 1, "{diagnostics:?}");
	assert_eq!(repeated[0].severity, Some(Severity::Warning));
}

fn diagnostic_source<'a>(diagnostics: &'a [SchemaDiagnostic], code: &str, key: &str) -> &'a str {
	let found = diagnostics_with_code(diagnostics, code)
		.into_iter()
		.filter(|diagnostic| diagnostic.message.contains(&format!("`{key}`")))
		.collect::<Vec<_>>();
	assert_eq!(found.len(), 1, "{code} `{key}`: {diagnostics:?}");
	found[0].source.as_deref().expect("diagnostic source")
}

/// A finding a vendored CWTools rule decides credits CWTools; one foch's
/// supplementary rules or its own refinement of a rule decides is
/// `foch-cwtools`; one from how foch reads the game itself is `foch-detect`.
#[test]
fn diagnostics_name_the_rules_that_decided_them() {
	let engine = load_inline_lsp_schema(
		r#"
		types = {
			type[sample] = { path = "game/common/samples" }
		}
		sample = {
			## cardinality = 0..1
			rank = int
			## cardinality = 0..1
			modifier = {
				alias_name[modifier] = alias_match_left[modifier]
			}
			## cardinality = 0..1
			bonus = { factor = float }
		}
		alias[modifier:discipline] = float
		"#,
	);
	let path = game_path("common/samples/sample.txt");
	let text = "sample = {\n\trank = 1\n\trank = 2\n\tmodifier = { discipline = 0.0025 }\n\tmystery = yes\n\tbonus = { factor = 2 }\n\tbonus = { factor = 2 }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, path, text);
	assert_eq!(
		diagnostic_source(&diagnostics, "V001", "mystery"),
		"cwtools"
	);
	assert_eq!(diagnostic_source(&diagnostics, "V002", "rank"), "cwtools");
	assert_eq!(
		diagnostic_source(&diagnostics, "V002", "bonus"),
		"foch-cwtools"
	);
	assert_eq!(
		diagnostic_source(&diagnostics, "V008", "discipline"),
		"foch-detect"
	);

	let schema = EditorSchema::active();
	let path = game_path("history/provinces/1034 - Kamchatka.txt");
	let text = "owner = KMC\ncontroller = KMC\nnative_size = 50\nnative_size = 60\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert_eq!(
		diagnostic_source(&diagnostics, "V002", "native_size"),
		"foch-cwtools"
	);

	let path = game_path("events/sample.txt");
	let text = "namespace = sample\ncountry_event = {\n\tid = sample.1\n\tmystery = yes\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert_eq!(
		diagnostic_source(&diagnostics, "V001", "mystery"),
		"cwtools"
	);
}

/// A file emptied to nothing but comments is a deliberate blank, as vanilla's
/// commented-out Ottoman–Albanian war is; a war that does exist still needs
/// its dates.
#[test]
fn diagnostics_require_nothing_of_a_blank_file() {
	let schema = EditorSchema::active();
	let path = game_path("history/wars/OttomanAlbanianWar.txt");
	let text = "#name = \"Ottoman-Albanian War\"\n# Commented out\n#1443.3.4 = {\n#\tadd_attacker = TUR\n#}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let diagnostics = schema.diagnostics(path, "name = \"Sample War\"\n", None);
	assert!(
		!diagnostics_with_code(&diagnostics, "V004").is_empty(),
		"{diagnostics:?}"
	);
}

/// Exactly three decimals is what the game keeps, so there is nothing to say.
#[test]
fn value_coercion_diagnostics_stay_quiet_at_the_kept_precision() {
	let engine = load_lsp_schema();
	let text = "namespace = sample\ncountry_event = {\n  id = sample.5\n  immediate = { add_prestige = 0.500 }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, game_path("events/sample.txt"), text);

	assert!(
		!diagnostics
			.iter()
			.any(|diagnostic| diagnostic.code == Some("V008".to_string())),
		"{diagnostics:?}"
	);
}

/// Precision is a property of the field: the game keeps a fourth decimal of
/// army professionalism, and a field nobody checked in game gets no notice.
/// A variable's `value` is read at three decimals, by the effect around it.
#[test]
fn value_coercion_diagnostics_follow_the_checked_fields() {
	let schema = EditorSchema::active();
	let path = game_path("events/sample.txt");
	let text = "namespace = sample\ncountry_event = {\n  id = sample.6\n  chance = 0.1234\n  trigger = { army_professionalism = 0.0125 }\n  immediate = {\n    add_army_professionalism = 0.0125\n    add_prestige = 0.0125\n  }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let truncated = diagnostics_with_code(&diagnostics, "V008");
	assert_eq!(truncated.len(), 1, "{diagnostics:?}");
	assert!(
		truncated[0].message.contains("`add_prestige`"),
		"{diagnostics:?}"
	);

	let text = "namespace = sample\ncountry_event = {\n  id = sample.7\n  immediate = { set_variable = { which = sample_var value = 0.0125 } }\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	let truncated = diagnostics_with_code(&diagnostics, "V008");
	assert_eq!(truncated.len(), 1, "{diagnostics:?}");
	assert!(truncated[0].message.contains("`value`"), "{diagnostics:?}");
}

/// The flag palette lives in `common/custom_country_colors/`; foch's
/// supplementary rules point the vendored rules for it there.
#[test]
fn diagnostics_check_the_flag_palette_where_the_game_keeps_it() {
	let engine = EditorSchema::active();
	let path = game_path("common/custom_country_colors/00_custom_country_colors.txt");
	let texture = "texture = { file = \"gfx\\\\custom_flags\\\\pattern.tga\" size = { x = 10 y = 4 } noOfFrames = 34 color = 2 }";
	let text = format!(
		"num_symbols = 2\ncolor = {{ 235 235 235 }}\ncolor = {{ 40 40 40 }}\nflag_color = {{ 255 255 255 }}\ntextures = {{\n\t{texture}\n\t{texture}\n}}\n"
	);
	let diagnostics = schema_diagnostics_for_text(&engine, path, &text);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let diagnostics =
		schema_diagnostics_for_text(&engine, path, &format!("{text}unknown_palette_key = 1\n"));
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.code == Some("V001".to_string())),
		"{diagnostics:?}"
	);
}
/// Every faction has an influence modifier, `<faction>_influence`, which the
/// vendored rules leave out and foch's supplementary rules add.
#[test]
fn diagnostics_accept_the_influence_modifier_of_each_faction() {
	let schema = EditorSchema::active();
	let workspace = schema.workspace(&[SchemaDocument::new(
		game_path("common/factions/00_factions.txt"),
		"mr_traders = {\n\tmonarch_power = ADM\n\tmodifier = { }\n}\n",
	)]);
	let path = game_path("common/static_modifiers/00_static_modifiers.txt");
	let text = "sample_modifier = {\n\tmr_traders_influence = 1\n}\n";
	let diagnostics = schema.diagnostics(path, text, Some(&workspace));
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let text = "sample_modifier = {\n\tmr_strangers_influence = 1\n}\n";
	let diagnostics = schema.diagnostics(path, text, Some(&workspace));
	let unknown = diagnostics_with_code(&diagnostics, "V001");
	assert_eq!(unknown.len(), 1, "{diagnostics:?}");
	assert!(
		unknown[0].message.contains("mr_strangers_influence"),
		"{diagnostics:?}"
	);
}
/// The game reads every top-level `chance` block of a ruler personality, as
/// vanilla's `charismatic_negotiator_personality` relies on; the vendored rules
/// allow one.
#[test]
fn diagnostics_accept_every_chance_block_of_a_ruler_personality() {
	let schema = EditorSchema::active();
	let path = game_path("common/ruler_personalities/00_core.txt");
	let text = "sample_personality = {\n\tallow = { }\n\tchance = {\n\t\tmodifier = {\n\t\t\tfactor = 2\n\t\t\thas_country_flag = a\n\t\t}\n\t}\n\tchance = {\n\t\tmodifier = {\n\t\t\tfactor = 3\n\t\t\thas_country_flag = b\n\t\t}\n\t}\n\tdiplomatic_reputation = 1\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(
		diagnostics_with_code(&diagnostics, "V002").is_empty(),
		"{diagnostics:?}"
	);
}

/// A province nobody owns at the start can still be a country's core; the
/// vendored rules only allow `add_core` beside an `owner`.
#[test]
fn diagnostics_accept_a_core_on_a_province_nobody_owns() {
	let schema = EditorSchema::active();
	let path = game_path("history/provinces/1138 - Kumasi.txt");
	let text = "culture = aka\nreligion = shamanism\ntrade_goods = gold\nnative_size = 60\nnative_ferocity = 4\nnative_hostileness = 9\nadd_core = ASH\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
/// The game keeps natives on provinces a country owns at the start; the
/// vendored rules allow them only where nobody owns the province.
#[test]
fn diagnostics_accept_natives_on_a_province_a_country_owns() {
	let schema = EditorSchema::active();
	let path = game_path("history/provinces/1034 - Kamchatka.txt");
	let text = "owner = KMC\ncontroller = KMC\nadd_core = KMC\nculture = kamchatkan\nreligion = tengri_pagan_reformed\ntrade_goods = fish\nbase_tax = 1\nbase_production = 1\nbase_manpower = 1\nnative_size = 50\nnative_ferocity = 2\nnative_hostileness = 5\nis_city = yes\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");

	let diagnostics = schema.diagnostics(path, &format!("{text}native_size = 60\n"), None);
	let repeated = diagnostics_with_code(&diagnostics, "V002");
	assert_eq!(repeated.len(), 1, "{diagnostics:?}");
	assert!(
		repeated[0].message.contains("native_size"),
		"{diagnostics:?}"
	);
}
/// The game's own triggered-only events keep a one-day `mean_time_to_happen`,
/// which the vendored rules mark as one too many.
#[test]
fn diagnostics_accept_a_one_day_mean_time_on_a_triggered_only_event() {
	let schema = EditorSchema::active();
	let path = game_path("events/FlavorGBR.txt");
	let text = "namespace = flavor_gbr\ncountry_event = {\n\tid = flavor_gbr.102\n\ttitle = flavor_gbr.102.t\n\tdesc = flavor_gbr.102.desc\n\tpicture = HENRY_eventPicture\n\tis_triggered_only = yes\n\tmean_time_to_happen = {\n\t\tdays = 1\n\t}\n\toption = {\n\t\tname = flavor_gbr.102.a\n\t}\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(
		diagnostics_with_code(&diagnostics, "V002").is_empty(),
		"{diagnostics:?}"
	);
}
/// The game writes `institution_growth` and `movement_speed_in_fleet_modifier`
/// with decimals, which the vendored rules type as whole numbers.
#[test]
fn diagnostics_accept_decimal_values_the_game_writes_for_modifiers() {
	let schema = EditorSchema::active();
	let path = game_path("common/event_modifiers/01_mission_modifiers.txt");
	let text = "sample_modifier = {\n\tinstitution_growth = 1.2\n\tmovement_speed_in_fleet_modifier = 0.25\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
/// A quoted effect the rules list as an allowed value matches the game's
/// text however each indents it.
#[test]
fn diagnostics_compare_quoted_effects_by_their_words() {
	let schema = EditorSchema::active();
	let path = game_path("events/sample.txt");
	let effect = |cost: &str| {
		format!(
			"namespace = sample\ncountry_event = {{\n\tid = sample.1\n\timmediate = {{\n\t\tissue_effect_scaling_with_estate_influence = {{\n\t\t\teffect5 = \"\n\t\t\tdefine_advisor = {{\n\t\t\t\ttype = colonial_governor\n\t\t\t\tskill = 3\n\t\t\t\tcost_multiplier = {cost}\n\t\t\t}}\n\t\t\t\"\n\t\t}}\n\t}}\n}}\n"
		)
	};
	let diagnostics = schema.diagnostics(path, &effect("0.1"), None);
	assert!(
		diagnostics_with_code(&diagnostics, "V003").is_empty(),
		"{diagnostics:?}"
	);

	let diagnostics = schema.diagnostics(path, &effect("0.2"), None);
	assert_eq!(
		diagnostics_with_code(&diagnostics, "V003").len(),
		1,
		"{diagnostics:?}"
	);
}
/// The rules name `clr_ruler_flag` outright in one on_action only; in every
/// other on_action it is still an effect like any other.
#[test]
fn diagnostics_accept_an_effect_another_subtype_names_outright() {
	let schema = EditorSchema::active();
	let path = game_path("common/on_actions/00_on_actions.txt");
	let text = "on_religion_change = {\n\tset_ruler_flag = sample_flag\n\tclr_ruler_flag = sample_flag\n}\non_queen_death = {\n\tclr_ruler_flag = marriage_length\n}\n";
	let diagnostics = schema.diagnostics(path, text, None);
	assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
