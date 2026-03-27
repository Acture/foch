use crate::check::analyzer::parser::{AstStatement, AstValue, SpanRange};
use crate::check::analyzer::semantic_index::{
	ParsedScriptFile, ScriptFileKind, classify_script_file, is_decision_container_key,
	parse_script_file,
};
use crate::check::merge::normalize::normalize_defines_file;
use crate::check::merge::plan::build_merge_plan_from_workspace;
use crate::check::model::{
	CheckRequest, MergePlanContributor, MergePlanEntry, MergePlanOptions, MergePlanResult,
	MergePlanStrategy,
};
use crate::check::workspace::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveErrorKind, resolve_workspace,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeIrStructuralKind {
	Events,
	Decisions,
	ScriptedEffects,
	DiplomaticActions,
	TriggeredModifiers,
	Defines,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeIrResult {
	pub game: String,
	pub playset_name: String,
	pub include_game_base: bool,
	pub copy_through_files: Vec<MergeIrCopyThroughFile>,
	pub structural_files: Vec<MergeIrStructuralFile>,
	pub deferred_structural_paths: Vec<MergeIrDeferredStructuralPath>,
	pub fatal_errors: Vec<String>,
}

impl MergeIrResult {
	pub fn has_fatal_errors(&self) -> bool {
		!self.fatal_errors.is_empty()
	}

	pub fn push_fatal_error(&mut self, message: impl Into<String>) {
		self.fatal_errors.push(message.into());
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrCopyThroughFile {
	pub target_path: String,
	pub winner: MergePlanContributor,
	pub source_mod_order: Vec<MergePlanContributor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrStructuralFile {
	pub target_path: String,
	pub kind: MergeIrStructuralKind,
	pub nodes: Vec<MergeIrNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrNode {
	pub target_path: String,
	pub merge_key: String,
	pub path_segments: Vec<String>,
	pub statement_key: String,
	pub container_key: Option<String>,
	pub winner: MergePlanContributor,
	pub overridden_contributors: Vec<MergePlanContributor>,
	pub source_mod_order: Vec<MergePlanContributor>,
	pub winning_statement: AstStatement,
	pub source_fragments: Vec<MergeIrSourceFragment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrSourceFragment {
	pub contributor: MergePlanContributor,
	pub statement_span: SpanRange,
	pub statement_key: String,
	pub container_key: Option<String>,
	pub statement: AstStatement,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrDeferredStructuralPath {
	pub target_path: String,
	pub reason: String,
	pub contributors: Vec<MergePlanContributor>,
}

#[derive(Clone, Debug)]
struct ExtractedFragment {
	merge_key: String,
	path_segments: Vec<String>,
	statement_key: String,
	container_key: Option<String>,
	statement: AstStatement,
	statement_span: SpanRange,
}

#[derive(Clone, Debug)]
struct NodeAccumulator {
	target_path: String,
	merge_key: String,
	path_segments: Vec<String>,
	statement_key: String,
	container_key: Option<String>,
	winner: MergePlanContributor,
	source_mod_order: Vec<MergePlanContributor>,
	winning_statement: AstStatement,
	source_fragments: Vec<MergeIrSourceFragment>,
}

pub fn run_merge_ir(request: CheckRequest) -> MergeIrResult {
	run_merge_ir_with_options(request, MergePlanOptions::default())
}

pub fn run_merge_ir_with_options(
	request: CheckRequest,
	options: MergePlanOptions,
) -> MergeIrResult {
	let workspace = match resolve_workspace(&request, options.include_game_base) {
		Ok(workspace) => workspace,
		Err(err) => {
			let mut result = MergeIrResult {
				include_game_base: options.include_game_base,
				..MergeIrResult::default()
			};
			if err.kind == WorkspaceResolveErrorKind::PlaylistFormat {
				result.push_fatal_error("无法解析 Playset JSON");
			} else {
				result.push_fatal_error(err.message);
			}
			return result;
		}
	};

	let plan = build_merge_plan_from_workspace(&workspace, options.include_game_base);
	build_merge_ir_from_workspace_and_plan(&workspace, &plan)
}

pub(crate) fn build_merge_ir_from_workspace_and_plan(
	workspace: &ResolvedWorkspace,
	plan: &MergePlanResult,
) -> MergeIrResult {
	let mut result = MergeIrResult {
		game: plan.game.clone(),
		playset_name: plan.playset_name.clone(),
		include_game_base: plan.include_game_base,
		..MergeIrResult::default()
	};

	if !plan.fatal_errors.is_empty() {
		result.fatal_errors = plan.fatal_errors.clone();
		return result;
	}

	for path_entry in &plan.paths {
		match path_entry.strategy {
			MergePlanStrategy::CopyThrough => append_copy_through_file(&mut result, path_entry),
			MergePlanStrategy::StructuralMerge => {
				append_structural_file(&mut result, workspace, path_entry)
			}
			MergePlanStrategy::LastWriterOverlay | MergePlanStrategy::ManualConflict => {}
		}
	}

	result
		.copy_through_files
		.sort_by(|left, right| left.target_path.cmp(&right.target_path));
	result
		.structural_files
		.sort_by(|left, right| left.target_path.cmp(&right.target_path));
	result
		.deferred_structural_paths
		.sort_by(|left, right| left.target_path.cmp(&right.target_path));
	result
}

fn append_copy_through_file(result: &mut MergeIrResult, path_entry: &MergePlanEntry) {
	let Some(winner) = path_entry.winner.clone() else {
		result.push_fatal_error(format!(
			"copy-through path {} is missing a winner contributor",
			path_entry.path
		));
		return;
	};

	result.copy_through_files.push(MergeIrCopyThroughFile {
		target_path: path_entry.path.clone(),
		winner,
		source_mod_order: path_entry.contributors.clone(),
	});
}

fn append_structural_file(
	result: &mut MergeIrResult,
	workspace: &ResolvedWorkspace,
	path_entry: &MergePlanEntry,
) {
	let contributors = match workspace.file_inventory.get(&path_entry.path) {
		Some(contributors) => contributors,
		None => {
			result.push_fatal_error(format!(
				"merge IR could not locate contributors for {}",
				path_entry.path
			));
			return;
		}
	};

	let kind = classify_script_file(Path::new(&path_entry.path));
	match structural_kind_for_file_kind(kind) {
		Some(structural_kind) => {
			match build_structural_file(&path_entry.path, structural_kind, contributors) {
				Ok(file) => result.structural_files.push(file),
				Err(message) => result.push_fatal_error(message),
			}
		}
		None => result.push_fatal_error(format!(
			"merge IR does not support structural root {} for {}",
			describe_file_kind(kind),
			path_entry.path
		)),
	}
}

fn build_structural_file(
	target_path: &str,
	kind: MergeIrStructuralKind,
	contributors: &[ResolvedFileContributor],
) -> Result<MergeIrStructuralFile, String> {
	let mut nodes = BTreeMap::<String, NodeAccumulator>::new();

	for contributor in contributors {
		let parsed = parse_script_file(
			&contributor.mod_id,
			&contributor.root_path,
			&contributor.absolute_path,
		)
		.ok_or_else(|| {
			format!(
				"merge IR could not parse {} from {}",
				target_path,
				contributor.absolute_path.display()
			)
		})?;
		if !parsed.parse_issues.is_empty() {
			return Err(format!(
				"merge IR requires parse-clean contributors for {} but {} still has parse issues",
				target_path, contributor.mod_id
			));
		}

		let fragments = extract_fragments(&parsed)?;
		if fragments.is_empty() {
			return Err(format!(
				"merge IR found no mergeable blocks in {} for {}",
				target_path, contributor.mod_id
			));
		}

		let contributor_meta = to_merge_contributor(contributor);
		for fragment in fragments {
			let entry =
				nodes
					.entry(fragment.merge_key.clone())
					.or_insert_with(|| NodeAccumulator {
						target_path: target_path.to_string(),
						merge_key: fragment.merge_key.clone(),
						path_segments: fragment.path_segments.clone(),
						statement_key: fragment.statement_key.clone(),
						container_key: fragment.container_key.clone(),
						winner: contributor_meta.clone(),
						source_mod_order: Vec::new(),
						winning_statement: fragment.statement.clone(),
						source_fragments: Vec::new(),
					});

			push_unique_contributor(&mut entry.source_mod_order, &contributor_meta);
			entry.source_fragments.push(MergeIrSourceFragment {
				contributor: contributor_meta.clone(),
				statement_span: fragment.statement_span.clone(),
				statement_key: fragment.statement_key.clone(),
				container_key: fragment.container_key.clone(),
				statement: fragment.statement.clone(),
			});

			if contributor_meta.precedence > entry.winner.precedence
				|| contributor_meta.precedence == entry.winner.precedence
					&& contributor_meta.source_path == entry.winner.source_path
			{
				entry.winner = contributor_meta.clone();
				entry.path_segments = fragment.path_segments.clone();
				entry.statement_key = fragment.statement_key.clone();
				entry.container_key = fragment.container_key.clone();
				entry.winning_statement = fragment.statement.clone();
			}
		}
	}

	let nodes = nodes
		.into_values()
		.map(|accumulator| MergeIrNode {
			target_path: accumulator.target_path,
			merge_key: accumulator.merge_key,
			path_segments: accumulator.path_segments,
			statement_key: accumulator.statement_key,
			container_key: accumulator.container_key,
			overridden_contributors: accumulator
				.source_mod_order
				.iter()
				.filter(|item| item.source_path != accumulator.winner.source_path)
				.cloned()
				.collect(),
			source_mod_order: accumulator.source_mod_order,
			winner: accumulator.winner,
			winning_statement: accumulator.winning_statement,
			source_fragments: accumulator.source_fragments,
		})
		.collect();

	Ok(MergeIrStructuralFile {
		target_path: target_path.to_string(),
		kind,
		nodes,
	})
}

fn extract_fragments(parsed: &ParsedScriptFile) -> Result<Vec<ExtractedFragment>, String> {
	match parsed.file_kind {
		ScriptFileKind::Events => extract_event_fragments(parsed),
		ScriptFileKind::Decisions => Ok(extract_decision_fragments(parsed)),
		ScriptFileKind::ScriptedEffects
		| ScriptFileKind::DiplomaticActions
		| ScriptFileKind::TriggeredModifiers => Ok(extract_assignment_fragments(parsed)),
		ScriptFileKind::Defines => extract_defines_fragments(parsed),
		other => Err(format!(
			"merge IR does not support extracting {} from {}",
			describe_file_kind(other),
			parsed.relative_path.display()
		)),
	}
}

fn extract_event_fragments(parsed: &ParsedScriptFile) -> Result<Vec<ExtractedFragment>, String> {
	let mut fragments = Vec::new();

	for statement in &parsed.ast.statements {
		let AstStatement::Assignment {
			key, value, span, ..
		} = statement
		else {
			continue;
		};
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		let Some(merge_key) = scalar_assignment_value(items, "id") else {
			return Err(format!(
				"merge IR requires event id keys in {} but found a {} block without id",
				parsed.relative_path.display(),
				key
			));
		};
		fragments.push(ExtractedFragment {
			merge_key,
			path_segments: Vec::new(),
			statement_key: key.clone(),
			container_key: None,
			statement: statement.clone(),
			statement_span: span.clone(),
		});
	}

	Ok(fragments)
}

fn extract_decision_fragments(parsed: &ParsedScriptFile) -> Vec<ExtractedFragment> {
	let mut fragments = Vec::new();

	for statement in &parsed.ast.statements {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		if !is_decision_container_key(key) {
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		for item in items {
			let AstStatement::Assignment {
				key: decision_key,
				value: AstValue::Block { .. },
				span,
				..
			} = item
			else {
				continue;
			};
			fragments.push(ExtractedFragment {
				merge_key: decision_key.clone(),
				path_segments: Vec::new(),
				statement_key: decision_key.clone(),
				container_key: Some(key.clone()),
				statement: item.clone(),
				statement_span: span.clone(),
			});
		}
	}

	fragments
}

fn extract_assignment_fragments(parsed: &ParsedScriptFile) -> Vec<ExtractedFragment> {
	let mut fragments = Vec::new();

	for statement in &parsed.ast.statements {
		let AstStatement::Assignment {
			key, value, span, ..
		} = statement
		else {
			continue;
		};
		let AstValue::Block { .. } = value else {
			continue;
		};
		fragments.push(ExtractedFragment {
			merge_key: key.clone(),
			path_segments: Vec::new(),
			statement_key: key.clone(),
			container_key: None,
			statement: statement.clone(),
			statement_span: span.clone(),
		});
	}

	fragments
}

fn extract_defines_fragments(parsed: &ParsedScriptFile) -> Result<Vec<ExtractedFragment>, String> {
	Ok(normalize_defines_file(parsed)?
		.into_iter()
		.map(|fragment| ExtractedFragment {
			merge_key: fragment.merge_key,
			path_segments: fragment.path_segments,
			statement_key: fragment.statement_key,
			container_key: None,
			statement: fragment.statement,
			statement_span: fragment.statement_span,
		})
		.collect())
}

fn scalar_assignment_value(items: &[AstStatement], expected_key: &str) -> Option<String> {
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != expected_key {
			continue;
		}
		if let AstValue::Scalar { value, .. } = value {
			return Some(value.as_text());
		}
	}
	None
}

fn push_unique_contributor(
	contributors: &mut Vec<MergePlanContributor>,
	contributor: &MergePlanContributor,
) {
	if contributors
		.last()
		.is_some_and(|item| item.source_path == contributor.source_path)
	{
		return;
	}
	contributors.push(contributor.clone());
}

fn to_merge_contributor(contributor: &ResolvedFileContributor) -> MergePlanContributor {
	MergePlanContributor {
		mod_id: contributor.mod_id.clone(),
		source_path: contributor
			.absolute_path
			.to_string_lossy()
			.replace('\\', "/"),
		precedence: contributor.precedence,
		is_base_game: contributor.is_base_game,
	}
}

fn structural_kind_for_file_kind(kind: ScriptFileKind) -> Option<MergeIrStructuralKind> {
	match kind {
		ScriptFileKind::Events => Some(MergeIrStructuralKind::Events),
		ScriptFileKind::Decisions => Some(MergeIrStructuralKind::Decisions),
		ScriptFileKind::ScriptedEffects => Some(MergeIrStructuralKind::ScriptedEffects),
		ScriptFileKind::DiplomaticActions => Some(MergeIrStructuralKind::DiplomaticActions),
		ScriptFileKind::TriggeredModifiers => Some(MergeIrStructuralKind::TriggeredModifiers),
		ScriptFileKind::Defines => Some(MergeIrStructuralKind::Defines),
		_ => None,
	}
}

fn describe_file_kind(kind: ScriptFileKind) -> &'static str {
	match kind {
		ScriptFileKind::Events => "events",
		ScriptFileKind::OnActions => "on_actions",
		ScriptFileKind::Decisions => "decisions",
		ScriptFileKind::ScriptedEffects => "scripted_effects",
		ScriptFileKind::ScriptedTriggers => "scripted_triggers",
		ScriptFileKind::DiplomaticActions => "diplomatic_actions",
		ScriptFileKind::TriggeredModifiers => "triggered_modifiers",
		ScriptFileKind::Defines => "defines",
		ScriptFileKind::Achievements => "achievements",
		ScriptFileKind::Ages => "ages",
		ScriptFileKind::Buildings => "buildings",
		ScriptFileKind::Institutions => "institutions",
		ScriptFileKind::ProvinceTriggeredModifiers => "province_triggered_modifiers",
		ScriptFileKind::Ideas => "ideas",
		ScriptFileKind::GreatProjects => "great_projects",
		ScriptFileKind::GovernmentReforms => "government_reforms",
		ScriptFileKind::Cultures => "cultures",
		ScriptFileKind::CustomGui => "custom_gui",
		ScriptFileKind::AdvisorTypes => "advisortypes",
		ScriptFileKind::EventModifiers => "event_modifiers",
		ScriptFileKind::CbTypes => "cb_types",
		ScriptFileKind::GovernmentNames => "government_names",
		ScriptFileKind::CustomizableLocalization => "customizable_localization",
		ScriptFileKind::Missions => "missions",
		ScriptFileKind::NewDiplomaticActions => "new_diplomatic_actions",
		ScriptFileKind::Countries => "countries",
		ScriptFileKind::CountryHistory => "history_countries",
		ScriptFileKind::ProvinceHistory => "history_provinces",
		ScriptFileKind::Wars => "history_wars",
		ScriptFileKind::Units => "units",
		ScriptFileKind::Ui => "ui",
		ScriptFileKind::Other => "other",
	}
}

#[cfg(test)]
mod tests {
	use super::{MergeIrStructuralKind, run_merge_ir_with_options};
	use crate::check::analyzer::parser::{AstStatement, AstValue, ScalarValue};
	use crate::check::model::{CheckRequest, MergePlanOptions};
	use crate::cli::config::Config;
	use serde_json::json;
	use std::fs;
	use std::path::Path;
	use tempfile::TempDir;

	fn write_playlist(path: &Path, mods: serde_json::Value) {
		let playlist = json!({
			"game": "eu4",
			"name": "merge-ir-playset",
			"mods": mods,
		});
		fs::write(
			path,
			serde_json::to_string_pretty(&playlist).expect("serialize playlist"),
		)
		.expect("write playlist");
	}

	fn write_descriptor(mod_root: &Path, name: &str) {
		fs::create_dir_all(mod_root).expect("create mod root");
		fs::write(
			mod_root.join("descriptor.mod"),
			format!("name=\"{name}\"\nversion=\"1.0.0\"\n"),
		)
		.expect("write descriptor");
	}

	fn write_script_file(mod_root: &Path, relative: &str, content: &str) {
		let script_path = mod_root.join(relative);
		if let Some(parent) = script_path.parent() {
			fs::create_dir_all(parent).expect("create script parent");
		}
		fs::write(script_path, content).expect("write script file");
	}

	fn request_for(playlist_path: &Path) -> CheckRequest {
		let game_root = playlist_path
			.parent()
			.expect("playlist parent")
			.join("eu4-game");
		fs::create_dir_all(&game_root).expect("create default game root");
		let mut game_path = std::collections::HashMap::new();
		game_path.insert("eu4".to_string(), game_root);
		CheckRequest {
			playset_path: playlist_path.to_path_buf(),
			config: Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
			},
		}
	}

	fn run_merge_ir_no_base(request: CheckRequest) -> super::MergeIrResult {
		run_merge_ir_with_options(
			request,
			MergePlanOptions {
				include_game_base: false,
			},
		)
	}

	#[test]
	fn copy_through_single_contributor_file_is_preserved() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_root = temp.path().join("9401");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9401"}
			]),
		);
		write_descriptor(&mod_root, "mod-a");
		write_script_file(
			&mod_root,
			"events/a.txt",
			"namespace = test\ncountry_event = { id = test.1 }\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		assert_eq!(result.copy_through_files.len(), 1);
		let file = &result.copy_through_files[0];
		assert_eq!(file.target_path, "events/a.txt");
		assert_eq!(file.winner.mod_id, "9401");
		assert_eq!(file.source_mod_order.len(), 1);
	}

	#[test]
	fn event_ir_uses_block_id_as_merge_key_and_tracks_overrides() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9501");
		let mod_b = temp.path().join("9502");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9501"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9502"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"events/shared.txt",
			"namespace = test\ncountry_event = {\n\tid = test.1\n\ttitle = title_a\n}\ncountry_event = {\n\tid = test.2\n\ttitle = title_b\n}\n",
		);
		write_script_file(
			&mod_b,
			"events/shared.txt",
			"namespace = test\ncountry_event = {\n\tid = test.1\n\ttitle = title_override\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		assert_eq!(result.structural_files.len(), 1);
		let file = &result.structural_files[0];
		assert_eq!(file.kind, MergeIrStructuralKind::Events);
		assert_eq!(file.nodes.len(), 2);
		let shared = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test.1")
			.expect("shared event");
		assert_eq!(shared.statement_key, "country_event");
		assert_eq!(shared.winner.mod_id, "9502");
		assert_eq!(shared.overridden_contributors.len(), 1);
		assert_eq!(shared.overridden_contributors[0].mod_id, "9501");
		assert_eq!(shared.source_mod_order.len(), 2);
		assert_eq!(shared.source_fragments.len(), 2);
	}

	#[test]
	fn decision_ir_merges_nested_entries_by_decision_key() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9601");
		let mod_b = temp.path().join("9602");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9601"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9602"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"decisions/decisions.txt",
			"country_decisions = {\n\ttest_decision = {\n\t\teffect = { log = a }\n\t}\n\tunique_decision = {\n\t\teffect = { log = b }\n\t}\n}\n",
		);
		write_script_file(
			&mod_b,
			"decisions/decisions.txt",
			"country_decisions = {\n\ttest_decision = {\n\t\teffect = { log = override }\n\t}\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		let file = &result.structural_files[0];
		assert_eq!(file.kind, MergeIrStructuralKind::Decisions);
		let node = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test_decision")
			.expect("merged decision");
		assert_eq!(node.container_key.as_deref(), Some("country_decisions"));
		assert_eq!(node.winner.mod_id, "9602");
		assert_eq!(node.source_mod_order.len(), 2);
	}

	#[test]
	fn scripted_effect_ir_merges_top_level_assignment_keys() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9701");
		let mod_b = temp.path().join("9702");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9701"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9702"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"common/scripted_effects/effects.txt",
			"shared_effect = {\n\tlog = a\n}\nunique_effect = {\n\tlog = only_a\n}\n",
		);
		write_script_file(
			&mod_b,
			"common/scripted_effects/effects.txt",
			"shared_effect = {\n\tlog = b\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		let file = &result.structural_files[0];
		assert_eq!(file.kind, MergeIrStructuralKind::ScriptedEffects);
		let shared = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "shared_effect")
			.expect("shared effect");
		let unique = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "unique_effect")
			.expect("unique effect");
		assert_eq!(shared.winner.mod_id, "9702");
		assert_eq!(shared.source_fragments.len(), 2);
		assert_eq!(unique.winner.mod_id, "9701");
		assert!(unique.overridden_contributors.is_empty());
	}

	#[test]
	fn defines_ir_merges_leaf_assignment_paths_and_preserves_segments() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9801");
		let mod_b = temp.path().join("9802");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9801"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9802"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"common/defines/test.txt",
			"NGame = {\n\tSTART_YEAR = 1444\n\tEND_YEAR = 1821\n\tNCountry = {\n\t\tMAX_IDEA_GROUPS = 8\n\t}\n}\n",
		);
		write_script_file(
			&mod_b,
			"common/defines/test.txt",
			"NGame = {\n\tSTART_YEAR = 1500\n}\nNGame = {\n\tSTART_YEAR = 1600\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		assert!(result.deferred_structural_paths.is_empty());
		assert_eq!(result.structural_files.len(), 1);
		let file = &result.structural_files[0];
		assert_eq!(file.kind, MergeIrStructuralKind::Defines);
		let start_year = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "NGame.START_YEAR")
			.expect("start year node");
		let end_year = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "NGame.END_YEAR")
			.expect("end year node");
		let idea_groups = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "NGame.NCountry.MAX_IDEA_GROUPS")
			.expect("nested defines node");
		assert_eq!(start_year.path_segments, ["NGame", "START_YEAR"]);
		assert_eq!(end_year.path_segments, ["NGame", "END_YEAR"]);
		assert_eq!(
			idea_groups.path_segments,
			["NGame", "NCountry", "MAX_IDEA_GROUPS"]
		);
		assert_eq!(start_year.winner.mod_id, "9802");
		assert_eq!(start_year.source_mod_order.len(), 2);
		assert_eq!(start_year.source_fragments.len(), 3);
		assert_eq!(start_year.overridden_contributors.len(), 1);
		assert_eq!(start_year.overridden_contributors[0].mod_id, "9801");
		let AstStatement::Assignment {
			value: AstValue::Scalar { value, .. },
			..
		} = &start_year.winning_statement
		else {
			panic!("winning defines statement should remain a scalar assignment");
		};
		assert_eq!(value, &ScalarValue::Number("1600".to_string()));
		assert_eq!(end_year.winner.mod_id, "9801");
		assert_eq!(idea_groups.winner.mod_id, "9801");
	}
}
