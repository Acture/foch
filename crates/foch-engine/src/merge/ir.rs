use super::error::MergeError;
use super::normalize::normalize_defines_file;
use super::plan::build_merge_plan_from_workspace;
use crate::request::{CheckRequest, MergePlanOptions};
use crate::workspace::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveErrorKind, resolve_workspace,
};
use foch_core::model::{MergePlanContributor, MergePlanEntry, MergePlanResult, MergePlanStrategy};
use foch_language::analyzer::content_family::{
	ConflictPolicy, ContentFamilyDescriptor, GameProfile, MergeKeySource,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstStatement, AstValue, SpanRange};
use foch_language::analyzer::semantic_index::{
	ParsedScriptFile, is_decision_container_key, parse_script_file,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

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
	pub family_id: String,
	pub merge_key_source: MergeKeySource,
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
	/// Set when renamed due to conflict — holds the original merge key.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub original_merge_key: Option<String>,
	/// Whether this node was renamed as part of conflict resolution.
	#[serde(default)]
	pub conflict_rename: bool,
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
	/// Set during conflict rename to track the original merge key.
	original_merge_key: Option<String>,
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

	let profile = eu4_profile();
	let descriptor = profile.classify_content_family(Path::new(&path_entry.path));
	let merge_key_source = descriptor.and_then(|d| d.merge_key_source);
	match (descriptor, merge_key_source) {
		(Some(descriptor), Some(merge_key_source)) => {
			match build_structural_file(
				&path_entry.path,
				descriptor,
				merge_key_source,
				contributors,
			) {
				Ok(file) => result.structural_files.push(file),
				Err(err) => result.push_fatal_error(err.to_string()),
			}
		}
		_ => result.push_fatal_error(format!(
			"merge IR has no merge_key_source for {}",
			path_entry.path
		)),
	}
}

fn build_structural_file(
	target_path: &str,
	descriptor: &ContentFamilyDescriptor,
	merge_key_source: MergeKeySource,
	contributors: &[ResolvedFileContributor],
) -> Result<MergeIrStructuralFile, MergeError> {
	let mut nodes = BTreeMap::<String, NodeAccumulator>::new();

	for contributor in contributors {
		let parsed = parse_script_file(
			&contributor.mod_id,
			&contributor.root_path,
			&contributor.absolute_path,
		)
		.ok_or_else(|| MergeError::Parse {
			path: Some(target_path.to_string()),
			message: format!(
				"merge IR could not parse {} from {}",
				target_path,
				contributor.absolute_path.display()
			),
		})?;
		if !parsed.parse_issues.is_empty() {
			return Err(MergeError::Parse {
				path: Some(target_path.to_string()),
				message: format!(
					"merge IR requires parse-clean contributors for {} but {} still has parse issues",
					target_path, contributor.mod_id
				),
			});
		}

		let fragments = extract_fragments(&parsed, merge_key_source)?;
		if fragments.is_empty() {
			return Err(MergeError::Parse {
				path: Some(target_path.to_string()),
				message: format!(
					"merge IR found no mergeable blocks in {} for {}",
					target_path, contributor.mod_id
				),
			});
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
						original_merge_key: None,
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

	// --- Conflict detection and rename ---
	let conflict_policy = descriptor.conflict_policy;
	if conflict_policy == ConflictPolicy::Rename {
		let conflicting_keys: Vec<String> = nodes
			.iter()
			.filter_map(|(key, accumulator)| {
				let unique_non_base_mods: HashSet<&str> = accumulator
					.source_mod_order
					.iter()
					.filter(|c| !c.is_base_game)
					.map(|c| c.mod_id.as_str())
					.collect();
				if unique_non_base_mods.len() > 1 {
					Some(key.clone())
				} else {
					None
				}
			})
			.collect();

		for key in conflicting_keys {
			if let Some(accumulator) = nodes.remove(&key) {
				// Base game fragments keep the original key
				let base_fragments: Vec<&MergeIrSourceFragment> = accumulator
					.source_fragments
					.iter()
					.filter(|f| f.contributor.is_base_game)
					.collect();
				if !base_fragments.is_empty() {
					let base_contributor = base_fragments[0].contributor.clone();
					let base_node = NodeAccumulator {
						target_path: accumulator.target_path.clone(),
						merge_key: key.clone(),
						path_segments: accumulator.path_segments.clone(),
						statement_key: accumulator.statement_key.clone(),
						container_key: accumulator.container_key.clone(),
						winner: base_contributor.clone(),
						source_mod_order: vec![base_contributor],
						winning_statement: base_fragments.last().unwrap().statement.clone(),
						source_fragments: base_fragments.into_iter().cloned().collect(),
						original_merge_key: None,
					};
					nodes.insert(key.clone(), base_node);
				}

				// Group non-base fragments by mod_id and create renamed nodes
				let mut per_mod: BTreeMap<String, Vec<MergeIrSourceFragment>> = BTreeMap::new();
				for fragment in &accumulator.source_fragments {
					if fragment.contributor.is_base_game {
						continue;
					}
					per_mod
						.entry(fragment.contributor.mod_id.clone())
						.or_default()
						.push(fragment.clone());
				}

				for (mod_id, fragments) in per_mod {
					let mod_suffix = sanitize_mod_name(&mod_id);
					let renamed_key = format!("{}_{}", key, mod_suffix);
					let last_fragment = fragments.last().unwrap();
					let contributor = last_fragment.contributor.clone();
					let renamed_node = NodeAccumulator {
						target_path: accumulator.target_path.clone(),
						merge_key: renamed_key.clone(),
						path_segments: accumulator.path_segments.clone(),
						statement_key: last_fragment.statement_key.clone(),
						container_key: last_fragment.container_key.clone(),
						winner: contributor.clone(),
						source_mod_order: vec![contributor],
						winning_statement: last_fragment.statement.clone(),
						source_fragments: fragments,
						original_merge_key: Some(key.clone()),
					};
					nodes.insert(renamed_key.clone(), renamed_node);
				}
			}
		}
	}
	// For MergeLeaf and LastWriter, the existing accumulation behavior is correct.

	let nodes = nodes
		.into_values()
		.map(|accumulator| {
			let is_renamed = accumulator.original_merge_key.is_some();
			MergeIrNode {
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
				original_merge_key: accumulator.original_merge_key,
				conflict_rename: is_renamed,
			}
		})
		.collect();

	Ok(MergeIrStructuralFile {
		target_path: target_path.to_string(),
		family_id: descriptor.id.to_string(),
		merge_key_source,
		nodes,
	})
}

fn extract_fragments(
	parsed: &ParsedScriptFile,
	merge_key_source: MergeKeySource,
) -> Result<Vec<ExtractedFragment>, MergeError> {
	match merge_key_source {
		MergeKeySource::BlockKey => Ok(extract_assignment_fragments(parsed)),
		MergeKeySource::InnerField(field) => extract_inner_field_fragments(parsed, field),
		MergeKeySource::ContainerChild => Ok(extract_decision_fragments(parsed)),
		MergeKeySource::DefinesPath => extract_defines_fragments(parsed),
	}
}

fn extract_inner_field_fragments(
	parsed: &ParsedScriptFile,
	field: &str,
) -> Result<Vec<ExtractedFragment>, MergeError> {
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
		let Some(merge_key) = scalar_assignment_value(items, field) else {
			return Err(MergeError::Parse {
				path: Some(parsed.relative_path.display().to_string()),
				message: format!(
					"merge IR requires {} keys in {} but found a {} block without {}",
					field,
					parsed.relative_path.display(),
					key,
					field
				),
			});
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

fn extract_defines_fragments(
	parsed: &ParsedScriptFile,
) -> Result<Vec<ExtractedFragment>, MergeError> {
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

/// Sanitize a mod identifier for use as a merge-key suffix.
/// Replaces non-alphanumeric characters with underscores and lowercases.
fn sanitize_mod_name(mod_id: &str) -> String {
	mod_id
		.chars()
		.map(|c| {
			if c.is_alphanumeric() {
				c.to_ascii_lowercase()
			} else {
				'_'
			}
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::run_merge_ir_with_options;
	use crate::config::Config;
	use crate::request::{CheckRequest, MergePlanOptions};
	use foch_language::analyzer::content_family::MergeKeySource;
	use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};
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
		assert_eq!(file.merge_key_source, MergeKeySource::InnerField("id"));
		// Conflict rename splits test.1 into per-mod nodes
		assert_eq!(file.nodes.len(), 3);
		let renamed_a = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test.1_9501")
			.expect("renamed event for mod A");
		assert_eq!(renamed_a.winner.mod_id, "9501");
		assert!(renamed_a.conflict_rename);
		assert_eq!(renamed_a.original_merge_key.as_deref(), Some("test.1"));
		let renamed_b = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test.1_9502")
			.expect("renamed event for mod B");
		assert_eq!(renamed_b.winner.mod_id, "9502");
		assert!(renamed_b.conflict_rename);
		let unique = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test.2")
			.expect("unique event");
		assert!(!unique.conflict_rename);
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
		assert_eq!(file.merge_key_source, MergeKeySource::ContainerChild);
		// Conflict rename splits test_decision into per-mod nodes
		assert_eq!(file.nodes.len(), 3);
		let renamed_a = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test_decision_9601")
			.expect("renamed decision for mod A");
		assert_eq!(
			renamed_a.container_key.as_deref(),
			Some("country_decisions")
		);
		assert_eq!(renamed_a.winner.mod_id, "9601");
		assert!(renamed_a.conflict_rename);
		let renamed_b = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test_decision_9602")
			.expect("renamed decision for mod B");
		assert_eq!(renamed_b.winner.mod_id, "9602");
		assert!(renamed_b.conflict_rename);
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
		assert_eq!(file.merge_key_source, MergeKeySource::BlockKey);
		// Conflict rename splits shared_effect into per-mod nodes
		assert_eq!(file.nodes.len(), 3);
		let renamed_a = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "shared_effect_9701")
			.expect("renamed effect for mod A");
		assert_eq!(renamed_a.winner.mod_id, "9701");
		assert!(renamed_a.conflict_rename);
		let renamed_b = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "shared_effect_9702")
			.expect("renamed effect for mod B");
		assert_eq!(renamed_b.winner.mod_id, "9702");
		assert!(renamed_b.conflict_rename);
		let unique = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "unique_effect")
			.expect("unique effect");
		assert_eq!(unique.winner.mod_id, "9701");
		assert!(unique.overridden_contributors.is_empty());
		assert!(!unique.conflict_rename);
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
		assert_eq!(file.merge_key_source, MergeKeySource::DefinesPath);
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
