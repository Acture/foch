#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use foch_core::config::DepOverride;
use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::content_family::{MergeKeySource, MergePolicies, ScriptFileKind};
use foch_language::analyzer::parser::{AstFile, AstStatement};
use foch_language::analyzer::semantic_index::{ParsedScriptFile, parse_script_file};

use super::conflict_handler::{ConflictHandler, DeferHandler};
use super::dag::{
	FileDag, IgnoreReplacePath, ModDag, ModId, induced_file_dag_with_overrides, topo_levels,
};
use super::patch::{ClausewitzPatch, diff_ast, fold_renames};
use super::patch_apply::apply_patches;
use super::patch_merge::{PatchMergeResult, PatchResolution, merge_patch_sets};
use crate::workspace::ResolvedFileContributor;

#[derive(Clone, Debug)]
pub(crate) struct DagPatchComputation {
	pub mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	pub base_statements: Vec<AstStatement>,
	pub merged_statements: Vec<AstStatement>,
	pub merge_result: PatchMergeResult,
}

/// Compute all patches for a single file using dependency-DAG topo levels.
///
/// Each level is diffed against the running merged state, sibling patch sets are
/// merged together, then their resolved patches advance the running state.
pub(crate) fn compute_dag_patches(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	merge_key_source: MergeKeySource,
	policies: &MergePolicies,
	mod_dag: &ModDag,
	ignore_replace_path: &IgnoreReplacePath,
	dep_overrides: &[DepOverride],
) -> Result<DagPatchComputation, String> {
	let mut handler = DeferHandler;
	compute_dag_patches_with_handler(
		file_path,
		contributors,
		merge_key_source,
		policies,
		mod_dag,
		ignore_replace_path,
		dep_overrides,
		&mut handler,
	)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_dag_patches_with_handler(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	merge_key_source: MergeKeySource,
	policies: &MergePolicies,
	mod_dag: &ModDag,
	ignore_replace_path: &IgnoreReplacePath,
	dep_overrides: &[DepOverride],
	handler: &mut dyn ConflictHandler,
) -> Result<DagPatchComputation, String> {
	let file_dag = induced_file_dag_with_overrides(
		mod_dag,
		file_path,
		contributors,
		ignore_replace_path,
		dep_overrides,
	);
	let vanilla = parse_vanilla_contributor(file_path, contributors)?;
	let parsed_contributors = parse_active_mod_contributors(file_path, contributors, &file_dag)?;
	compute_dag_patches_from_parsed(
		&file_dag,
		vanilla.as_ref(),
		&parsed_contributors,
		merge_key_source,
		policies,
		handler,
	)
}

fn parse_vanilla_contributor(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
) -> Result<Option<ParsedScriptFile>, String> {
	let Some(base) = contributors.iter().find(|c| c.is_base_game) else {
		return Ok(None);
	};
	parse_script_file(&base.mod_id, &base.root_path, &base.absolute_path)
		.map(Some)
		.ok_or_else(|| {
			format!(
				"failed to parse vanilla file {} for {file_path}",
				base.absolute_path.display()
			)
		})
}

fn parse_active_mod_contributors(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	file_dag: &FileDag,
) -> Result<HashMap<ModId, ParsedScriptFile>, String> {
	let by_mod: HashMap<ModId, &ResolvedFileContributor> = contributors
		.iter()
		.filter(|c| !c.is_base_game && !c.is_synthetic_base)
		.map(|c| (ModId(c.mod_id.clone()), c))
		.collect();
	let mut parsed = HashMap::new();
	for mod_id in file_dag.contributors() {
		let contributor = by_mod
			.get(mod_id)
			.ok_or_else(|| format!("missing contributor {} for {file_path}", mod_id.as_str()))?;
		let parsed_file = parse_script_file(
			&contributor.mod_id,
			&contributor.root_path,
			&contributor.absolute_path,
		)
		.ok_or_else(|| {
			format!(
				"failed to parse mod file {} for {}",
				contributor.absolute_path.display(),
				contributor.mod_id,
			)
		})?;
		parsed.insert(mod_id.clone(), parsed_file);
	}
	Ok(parsed)
}

fn compute_dag_patches_from_parsed(
	file_dag: &FileDag,
	vanilla: Option<&ParsedScriptFile>,
	contributors: &HashMap<ModId, ParsedScriptFile>,
	merge_key_source: MergeKeySource,
	policies: &MergePolicies,
	handler: &mut dyn ConflictHandler,
) -> Result<DagPatchComputation, String> {
	let base_statements = final_base_statements(file_dag, vanilla);
	let mut current_statements = base_statements.clone();
	let mut mod_patches = Vec::new();
	let mut merge_result = PatchMergeResult::default();
	let all_contributors: BTreeSet<ModId> = file_dag.contributors().iter().cloned().collect();

	// Track per-level overwrite addresses so a later level can override
	// an earlier level's pending conflict at the same leaf address.
	let mut level_addresses: Vec<HashSet<(Vec<String>, String)>> = Vec::new();
	// Conflicts deferred to post-pass, paired with the originating level index.
	let mut pending_conflicts: Vec<(usize, PatchResolution)> = Vec::new();

	for (level_idx, level) in topo_levels(&all_contributors, file_dag)
		.into_iter()
		.enumerate()
	{
		let current_base = synthesized_parsed_file(
			file_dag.file_path(),
			template_for(file_dag, vanilla, contributors),
			current_statements.clone(),
		);
		let mut level_patches = Vec::new();
		let mut addresses_in_level: HashSet<(Vec<String>, String)> = HashSet::new();
		for mod_id in level {
			let current = contributors.get(&mod_id).ok_or_else(|| {
				format!(
					"missing parsed contributor {} for {}",
					mod_id.as_str(),
					file_dag.file_path()
				)
			})?;
			let patches = fold_renames(diff_ast(&current_base, current, merge_key_source));
			for patch in &patches {
				if let Some(addr) = overwrite_address(patch) {
					addresses_in_level.insert(addr);
				}
			}
			level_patches.push((mod_id.0.clone(), file_dag.precedence_of(&mod_id), patches));
		}
		level_addresses.push(addresses_in_level);

		mod_patches.extend(level_patches.clone());
		let mut level_result =
			merge_patch_sets(level_patches, policies, handler).map_err(|err| err.to_string())?;

		// Detach this level's conflicts; they go through the post-pass to be
		// either confirmed real or dropped because a downstream level overrode them.
		let level_conflicts = std::mem::take(&mut level_result.conflicts);
		for conflict in level_conflicts {
			pending_conflicts.push((level_idx, conflict));
		}

		let level_resolved_patches = resolved_patches(&level_result);
		extend_merge_result(&mut merge_result, level_result);
		// Always advance the running state with this level's resolved patches so the
		// next level diffs against post-merge content. Conflicting leaves stay at
		// their pre-level value, allowing a downstream mod's diff to produce a
		// fresh patch at that address.
		current_statements = apply_patches(
			&current_statements,
			&level_resolved_patches,
			merge_key_source,
		);
	}

	// Post-pass: any pending conflict whose address shows up in a strictly later
	// level's overwrite set is already resolved by that downstream contributor —
	// drop the conflict and record the override. Whatever remains is a true
	// conflict (no downstream resolution available).
	for (level_idx, conflict) in pending_conflicts {
		match conflict {
			PatchResolution::Conflict {
				address,
				patches,
				reason,
			} => {
				let key = (address.path.clone(), address.key.clone());
				let overridden_at = level_addresses
					.iter()
					.enumerate()
					.skip(level_idx + 1)
					.find_map(|(li, addrs)| if addrs.contains(&key) { Some(li) } else { None });
				if overridden_at.is_some() {
					let contributor_summary: Vec<String> =
						patches.iter().map(|p| p.mod_id.clone()).collect();
					merge_result
						.handler_resolutions
						.push(HandlerResolutionRecord {
							path: file_dag.file_path().to_string(),
							action: "downstream_override".to_string(),
							source: Some(format!("{}::{}", address.path.join("/"), address.key)),
							rationale: Some(format!(
								"upstream conflict between {} resolved by downstream contributor at level {} > {}",
								contributor_summary.join(", "),
								overridden_at.unwrap_or(level_idx + 1),
								level_idx,
							)),
						});
					merge_result.handler_resolved_count += 1;
					// Account for the conflict as resolved by reducing the residual count.
					if merge_result.stats.conflict_patches > 0 {
						merge_result.stats.conflict_patches -= 1;
					}
				} else {
					merge_result.conflicts.push(PatchResolution::Conflict {
						address,
						patches,
						reason,
					});
				}
			}
			other => merge_result.conflicts.push(other),
		}
	}

	Ok(DagPatchComputation {
		mod_patches,
		base_statements,
		merged_statements: current_statements,
		merge_result,
	})
}

fn final_base_statements(
	file_dag: &FileDag,
	vanilla: Option<&ParsedScriptFile>,
) -> Vec<AstStatement> {
	if file_dag
		.contributors()
		.iter()
		.any(|mod_id| file_dag.replaces_path(mod_id))
	{
		Vec::new()
	} else {
		vanilla
			.map(|base| base.ast.statements.clone())
			.unwrap_or_default()
	}
}

fn resolved_patches(merge_result: &PatchMergeResult) -> Vec<ClausewitzPatch> {
	merge_result
		.resolved
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Resolved(patch) => Some(patch.clone()),
			PatchResolution::AutoMerged { result, .. } => Some(result.clone()),
			PatchResolution::Conflict { .. } => None,
		})
		.collect()
}

fn overwrite_address(patch: &ClausewitzPatch) -> Option<(Vec<String>, String)> {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. }
		| ClausewitzPatch::ReplaceBlock { path, key, .. } => Some((path.clone(), key.clone())),
		_ => None,
	}
}

fn extend_merge_result(target: &mut PatchMergeResult, source: PatchMergeResult) {
	target.resolved.extend(source.resolved);
	target.conflicts.extend(source.conflicts);
	target.stats.total_patches += source.stats.total_patches;
	target.stats.single_mod_patches += source.stats.single_mod_patches;
	target.stats.convergent_patches += source.stats.convergent_patches;
	target.stats.auto_merged_patches += source.stats.auto_merged_patches;
	target.stats.conflict_patches += source.stats.conflict_patches;
	target.handler_resolved_count += source.handler_resolved_count;
	target
		.handler_resolutions
		.extend(source.handler_resolutions);
	target
		.external_file_resolutions
		.extend(source.external_file_resolutions);
	target
		.keep_existing_paths
		.extend(source.keep_existing_paths);
}

fn template_for<'a>(
	file_dag: &FileDag,
	vanilla: Option<&'a ParsedScriptFile>,
	contributors: &'a HashMap<ModId, ParsedScriptFile>,
) -> Option<&'a ParsedScriptFile> {
	vanilla.or_else(|| {
		file_dag
			.contributors()
			.iter()
			.find_map(|mod_id| contributors.get(mod_id))
	})
}

fn synthesized_parsed_file(
	file_path: &str,
	template: Option<&ParsedScriptFile>,
	statements: Vec<AstStatement>,
) -> ParsedScriptFile {
	let path = PathBuf::from(file_path);
	let mut parsed = template.cloned().unwrap_or_else(|| ParsedScriptFile {
		mod_id: "__foch_running_base__".to_string(),
		path: path.clone(),
		relative_path: path.clone(),
		content_family: None,
		file_kind: ScriptFileKind::Other,
		module_name: "running_base".to_string(),
		ast: AstFile {
			path: path.clone(),
			statements: Vec::new(),
		},
		source: String::new(),
		parse_issues: Vec::new(),
		parse_cache_hit: false,
	});
	parsed.mod_id = "__foch_running_base__".to_string();
	parsed.path = path.clone();
	parsed.relative_path = path.clone();
	parsed.ast.path = path;
	parsed.ast.statements = statements;
	parsed.source.clear();
	parsed.parse_issues.clear();
	parsed.parse_cache_hit = false;
	parsed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::playlist::PlaylistEntry;
	use foch_core::model::ModCandidate;
	use foch_language::analyzer::content_family::ScriptFileKind;
	use std::path::PathBuf;

	fn mod_with(
		mod_id: &str,
		name: &str,
		deps: Vec<&str>,
		replace_path: Vec<&str>,
	) -> ModCandidate {
		ModCandidate {
			entry: PlaylistEntry {
				steam_id: Some(mod_id.to_string()),
				..PlaylistEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: name.to_string(),
				dependencies: deps.into_iter().map(str::to_string).collect(),
				replace_path: replace_path.into_iter().map(str::to_string).collect(),
				..ModDescriptor::default()
			}),
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn mid(s: &str) -> ModId {
		ModId(s.to_string())
	}

	fn file_contributor(mod_id: &str, precedence: usize) -> ResolvedFileContributor {
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(format!("/mods/{mod_id}")),
			absolute_path: PathBuf::from(format!("/mods/{mod_id}/common/foo.txt")),
			precedence,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: None,
		}
	}

	fn parsed_file(mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/foo.txt");
		let parsed =
			foch_language::analyzer::parser::parse_clausewitz_content(path.clone(), source);
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: ScriptFileKind::Other,
			module_name: "test".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn parsed_inventory(entries: &[(&str, &str)]) -> HashMap<ModId, ParsedScriptFile> {
		entries
			.iter()
			.map(|(mod_id, source)| (mid(mod_id), parsed_file(mod_id, source)))
			.collect()
	}

	fn compute(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
	) -> DagPatchComputation {
		compute_with_overrides(mods, contribs, vanilla_source, inventory, ignore, &[])
	}

	fn compute_with_overrides(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
		dep_overrides: &[DepOverride],
	) -> DagPatchComputation {
		compute_with_merge_key(
			mods,
			contribs,
			vanilla_source,
			inventory,
			ignore,
			dep_overrides,
			MergeKeySource::AssignmentKey,
		)
	}

	fn compute_with_merge_key(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
		dep_overrides: &[DepOverride],
		merge_key_source: MergeKeySource,
	) -> DagPatchComputation {
		let (dag, diags) = super::super::dag::build_mod_dag(&mods);
		assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
		let fdag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contribs,
			&ignore,
			dep_overrides,
		);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		let mut handler = DeferHandler;
		compute_dag_patches_from_parsed(
			&fdag,
			vanilla.as_ref(),
			&inventory,
			merge_key_source,
			&MergePolicies::default(),
			&mut handler,
		)
		.expect("compute DAG patches")
	}

	fn patches_for<'a>(result: &'a DagPatchComputation, mod_id: &str) -> &'a Vec<ClausewitzPatch> {
		&result
			.mod_patches
			.iter()
			.find(|(id, _, _)| id == mod_id)
			.unwrap_or_else(|| panic!("missing patches for {mod_id}"))
			.2
	}

	fn inserted_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::InsertNode { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn removed_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::RemoveNode { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn set_value_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::SetValue { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn base_keys(result: &DagPatchComputation) -> Vec<String> {
		let mut keys: Vec<_> = result
			.base_statements
			.iter()
			.filter_map(|stmt| match stmt {
				AstStatement::Assignment { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn rendered(statements: &[AstStatement]) -> String {
		super::super::emit::emit_clausewitz_statements(statements).expect("emit statements")
	}

	fn append_list_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::AppendListItem { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn remove_list_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::RemoveListItem { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	fn replace_block_keys(patches: &[ClausewitzPatch]) -> Vec<String> {
		let mut keys: Vec<_> = patches
			.iter()
			.filter_map(|patch| match patch {
				ClausewitzPatch::ReplaceBlock { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect();
		keys.sort();
		keys
	}

	#[test]
	fn single_mod_no_deps_preserves_direct_output() {
		let mod_source = "root = yes\nnew_block = {\n\tvalue = 1\n}\n";
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some("root = no\n"),
			parsed_inventory(&[("a", mod_source)]),
			IgnoreReplacePath::None,
		);
		let direct = parsed_file("a", mod_source);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			rendered(&result.merged_statements),
			rendered(&direct.ast.statements)
		);
	}

	#[test]
	fn dependency_chain_applies_parent_before_child_without_mixed_kind_conflict() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("tag = ROOT\n"),
			parsed_inventory(&[
				("a", "tag = ROOT\ntag = AAA\n"),
				("b", "tag = ROOT\ntag = AAA\ntag = BBB\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(append_list_keys(patches_for(&result, "a")), vec!["tag"]);
		assert_eq!(append_list_keys(patches_for(&result, "b")), vec!["tag"]);
		assert!(remove_list_keys(patches_for(&result, "b")).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("tag = AAA"));
		assert!(output.contains("tag = BBB"));
	}

	#[test]
	fn sibling_fork_merges_same_base_patches_in_one_level() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("flag = no\n"),
			parsed_inventory(&[("a", "flag = yes\n"), ("b", "flag = yes\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(set_value_keys(patches_for(&result, "a")), vec!["flag"]);
		assert_eq!(set_value_keys(patches_for(&result, "b")), vec!["flag"]);
		assert_eq!(result.merge_result.stats.convergent_patches, 1);
		assert!(rendered(&result.merged_statements).contains("flag = yes"));
	}

	#[test]
	fn three_level_translation_chain_replaces_inherited_block_without_remove_append() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\npirate = {\n\tname = \"Pirates\"\n}\n"),
				(
					"b",
					"root = yes\npirate = {\n\tname = \"海盗\"\n\tflag = yes\n}\n",
				),
				(
					"c",
					"root = yes\npirate = {\n\tname = \"海盗\"\n\tflag = yes\n}\nc = yes\n",
				),
			]),
			IgnoreReplacePath::None,
		);

		let b_patches = patches_for(&result, "b");
		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(replace_block_keys(b_patches), vec!["pirate"]);
		assert!(append_list_keys(b_patches).is_empty());
		assert!(remove_list_keys(b_patches).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("name = \"海盗\""));
		assert!(output.contains("c = yes"));
	}

	#[test]
	fn independent_mods_diff_against_vanilla_not_previous_mod() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("flag = no\n"),
			parsed_inventory(&[("a", "flag = yes\n"), ("b", "flag = no\n")]),
			IgnoreReplacePath::None,
		);

		assert_eq!(set_value_keys(patches_for(&result, "a")), vec!["flag"]);
		assert!(
			patches_for(&result, "b").is_empty(),
			"independent vanilla-equivalent mod must not remove mod A's changes"
		);
	}

	#[test]
	fn gui_named_children_let_sibling_mods_edit_different_widgets() {
		const GUI_CHILD_TYPES: &[&str] = &["windowType"];
		let key_source = MergeKeySource::ContainerChildFieldValue {
			container: "guiTypes",
			child_key_field: "name",
			child_types: GUI_CHILD_TYPES,
		};
		let vanilla = r#"
			guiTypes = {
				windowType = { name = "left_widget" position = { x = 0 y = 0 } }
				windowType = { name = "right_widget" position = { x = 0 y = 0 } }
			}
		"#;
		let result = compute_with_merge_key(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(vanilla),
			parsed_inventory(&[
				(
					"a",
					r#"guiTypes = {
						windowType = { name = "left_widget" position = { x = 1 y = 0 } }
						windowType = { name = "right_widget" position = { x = 0 y = 0 } }
					}"#,
				),
				(
					"b",
					r#"guiTypes = {
						windowType = { name = "left_widget" position = { x = 0 y = 0 } }
						windowType = { name = "right_widget" position = { x = 2 y = 0 } }
					}"#,
				),
			]),
			IgnoreReplacePath::None,
			&[],
			key_source,
		);

		assert!(result.merge_result.conflicts.is_empty());
		let addresses = result
			.mod_patches
			.iter()
			.flat_map(|(_, _, patches)| patches)
			.filter_map(|patch| match patch {
				ClausewitzPatch::SetValue { path, key, .. } => Some((path.clone(), key.clone())),
				_ => None,
			})
			.collect::<Vec<_>>();
		assert!(addresses.contains(&(
			vec![
				"guiTypes".to_string(),
				"windowType:left_widget".to_string(),
				"position".to_string(),
			],
			"x".to_string(),
		)));
		assert!(addresses.contains(&(
			vec![
				"guiTypes".to_string(),
				"windowType:right_widget".to_string(),
				"position".to_string(),
			],
			"x".to_string(),
		)));
	}

	#[test]
	fn gui_named_children_conflict_same_widget_sibling_overwrites() {
		const GUI_CHILD_TYPES: &[&str] = &["windowType"];
		let key_source = MergeKeySource::ContainerChildFieldValue {
			container: "guiTypes",
			child_key_field: "name",
			child_types: GUI_CHILD_TYPES,
		};
		let vanilla = r#"
			guiTypes = {
				windowType = { name = "left_widget" position = { x = 0 y = 0 } }
			}
		"#;
		let result = compute_with_merge_key(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(vanilla),
			parsed_inventory(&[
				(
					"a",
					r#"guiTypes = {
						windowType = { name = "left_widget" position = { x = 1 y = 0 } }
					}"#,
				),
				(
					"b",
					r#"guiTypes = {
						windowType = { name = "left_widget" position = { x = 2 y = 0 } }
					}"#,
				),
			]),
			IgnoreReplacePath::None,
			&[],
			key_source,
		);

		assert_eq!(result.merge_result.conflicts.len(), 1);
		match &result.merge_result.conflicts[0] {
			PatchResolution::Conflict {
				address, reason, ..
			} => {
				assert_eq!(
					address.path,
					vec![
						"guiTypes".to_string(),
						"windowType:left_widget".to_string(),
						"position".to_string(),
					]
				);
				assert_eq!(address.key, "x");
				assert!(
					reason.contains("sibling mods set the same scalar to divergent values"),
					"unexpected reason: {reason}"
				);
			}
			other => panic!("expected sibling scalar conflict, got {other:?}"),
		}
	}

	#[test]
	fn declared_dep_uses_synthesized_parent_base() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\na = yes\nb = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "a")), vec!["a"]);
		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
	}

	#[test]
	fn dep_override_diffs_child_against_vanilla_not_declared_parent() {
		let result = compute_with_overrides(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\nb = yes\n"),
			]),
			IgnoreReplacePath::None,
			&[DepOverride::new("b", "a")],
		);

		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
		assert!(removed_keys(patches_for(&result, "b")).is_empty());
	}

	#[test]
	fn transitive_chain_base_contains_all_ancestors() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\na = yes\nb = yes\n"),
				("c", "root = yes\na = yes\nb = yes\nc = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "c")), vec!["c"]);
	}

	#[test]
	fn diamond_base_merges_both_branches() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
				mod_with("d", "D", vec!["B", "C"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
				file_contributor("d", 4),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\na = yes\nb = yes\n"),
				("c", "root = yes\na = yes\nc = yes\n"),
				("d", "root = yes\na = yes\nb = yes\nc = yes\nd = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "d")), vec!["d"]);
	}

	#[test]
	fn missing_intermediate_file_dep_lifts_to_shipping_ancestor() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("c", 3)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("c", "root = yes\na = yes\nc = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "c")), vec!["c"]);
	}

	#[test]
	fn replace_path_drops_prior_contributors_and_uses_empty_base() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec!["common"]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[("b", "b = yes\n"), ("c", "b = yes\nc = yes\n")]),
			IgnoreReplacePath::None,
		);

		assert!(
			result
				.mod_patches
				.iter()
				.all(|(mod_id, _, _)| mod_id != "a")
		);
		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
		assert_eq!(inserted_keys(patches_for(&result, "c")), vec!["c"]);
		assert!(base_keys(&result).is_empty());
	}

	#[test]
	fn ignore_replace_path_keeps_prior_contributors() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec!["common"]),
				mod_with("c", "C", vec!["B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\na = yes\nb = yes\n"),
				("c", "root = yes\na = yes\nb = yes\nc = yes\n"),
			]),
			IgnoreReplacePath::All,
		);

		assert_eq!(result.mod_patches.len(), 3);
		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
		assert_eq!(base_keys(&result), vec!["root"]);
	}

	#[test]
	fn no_vanilla_file_diffs_each_mod_against_empty() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			None,
			parsed_inventory(&[("a", "a = yes\n"), ("b", "b = yes\n")]),
			IgnoreReplacePath::None,
		);

		assert_eq!(inserted_keys(patches_for(&result, "a")), vec!["a"]);
		assert_eq!(inserted_keys(patches_for(&result, "b")), vec!["b"]);
		assert!(base_keys(&result).is_empty());
	}
}
