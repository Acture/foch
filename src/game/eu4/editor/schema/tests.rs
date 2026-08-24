use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;

struct WorkspaceFixture {
	schema_workspace: SchemaWorkspace,
}

fn repository_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lsp_fixture_dir() -> PathBuf {
	repository_root()
		.join("apps")
		.join("foch-cli")
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
			Path::new("common/country_tags/00_countries.txt"),
			"SWE = \"countries/Sweden.txt\"\nFRA = \"countries/France.txt\"\n",
		),
		SchemaDocument::new(
			Path::new("common/graphicalculturetype.txt"),
			"westerngfx = {}\neasterngfx = {}\n",
		),
		SchemaDocument::new(
			Path::new("customizable_localization/sample_custom_locs.txt"),
			"defined_text = {\n  name = sample_defined_text\n  text = { localisation_key = sample_defined_text_key }\n}\n",
		),
		SchemaDocument::new(
			Path::new("common/cultures/00_cultures.txt"),
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
	file_path: &Path,
	text: &str,
	position: EditorPosition,
	workspace: Option<&SchemaWorkspace>,
) -> Option<SchemaHover> {
	schema.hover(file_path, text, position, workspace)
}

fn schema_completion_candidates(
	schema: &EditorSchema,
	file_path: &Path,
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
) -> Option<Vec<SchemaCompletion>> {
	schema.completions(file_path, text, position, prefix_lower, None)
}

fn schema_completion_candidates_with_index(
	schema: &EditorSchema,
	file_path: &Path,
	text: &str,
	position: EditorPosition,
	prefix_lower: &str,
	workspace: Option<&SchemaWorkspace>,
) -> Option<Vec<SchemaCompletion>> {
	schema.completions(file_path, text, position, prefix_lower, workspace)
}

fn schema_diagnostics_for_text(
	schema: &EditorSchema,
	file_path: &Path,
	text: &str,
) -> Vec<SchemaDiagnostic> {
	schema.diagnostics(file_path, text, None)
}

fn schema_diagnostics_for_text_with_index(
	schema: &EditorSchema,
	file_path: &Path,
	text: &str,
	workspace: Option<&SchemaWorkspace>,
) -> Vec<SchemaDiagnostic> {
	schema.diagnostics(file_path, text, workspace)
}

#[test]
fn active_schema_discovers_vendored_pack() {
	assert!(EditorSchema::active().is_some());
}

#[test]
fn schema_loader_reads_existing_fixture_pack() {
	let fixture_dir = repository_root()
		.join("crates")
		.join("foch-cwt")
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
fn hover_renders_event_field_from_schema() {
	let engine = load_lsp_schema();
	let text = fixture_text("events/sample.txt");
	let hover = schema_hover(
		&engine,
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("missions/sample.txt"),
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
		Path::new("common/scripted_effects/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("common/scripted_effects/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("missions/sample.txt"),
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
		Path::new("common/ideas/sample.txt"),
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
		Path::new("common/ages/sample.txt"),
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
		Path::new("map/area.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("missions/sample.txt"),
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
		Path::new("missions/sample.txt"),
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("localisation")
	}));
}

#[test]
fn diagnostics_use_replace_scope_this_for_alias_mismatches() {
	let engine = load_lsp_schema();
	let text = "demo_mission = {\n  provinces_to_highlight = {\n    has_country_flag = demo_flag\n    has_province_flag = demo_flag\n    has_sea_flag = demo_flag\n  }\n}\n";
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("missions/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("missions/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
		schema_diagnostics_for_text(&engine, Path::new("events/diagnostics.txt"), &text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
		Path::new("events/sample.txt"),
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
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
	let diagnostics = schema_diagnostics_for_text(&engine, Path::new("events/sample.txt"), text);
	assert!(!diagnostics.iter().any(|diagnostic| {
		diagnostic.code == Some("V001".to_string()) && diagnostic.message.contains("mystery_key")
	}));
}
