#![allow(dead_code)]

use std::collections::HashMap;

use foch_language::analyzer::content_family::{MergeKeySource, MergePolicies};
use foch_language::analyzer::parser::AstStatement;
use foch_language::analyzer::semantic_index::{ParsedScriptFile, parse_script_file};

use super::dag::{
	BaseResolver, BaseSource, FileDag, IgnoreReplacePath, ModDag, ModId, induced_file_dag,
};
use super::patch::{ClausewitzPatch, diff_ast, fold_renames};
use crate::workspace::ResolvedFileContributor;

#[derive(Clone, Debug)]
pub(crate) struct DagPatchComputation {
	pub mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	pub base_statements: Vec<AstStatement>,
}

/// Compute all patches for a single file using dependency-DAG bases.
///
/// Each active mod contributor is diffed against
/// `recursive_merge(vanilla, transitive_deps_touching_this_file)` rather than
/// against the previous contributor in load order.
pub(crate) fn compute_dag_patches(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	merge_key_source: MergeKeySource,
	policies: &MergePolicies,
	mod_dag: &ModDag,
	ignore_replace_path: &IgnoreReplacePath,
) -> Result<DagPatchComputation, String> {
	let file_dag = induced_file_dag(mod_dag, file_path, contributors, ignore_replace_path);
	let vanilla = parse_vanilla_contributor(file_path, contributors)?;
	let parsed_contributors = parse_active_mod_contributors(file_path, contributors, &file_dag)?;
	compute_dag_patches_from_parsed(
		&file_dag,
		vanilla.as_ref(),
		&parsed_contributors,
		merge_key_source,
		policies,
		ignore_replace_path.clone(),
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
	ignore_replace_path: IgnoreReplacePath,
) -> Result<DagPatchComputation, String> {
	let mut resolver = BaseResolver::new(ignore_replace_path);
	let mut mod_patches = Vec::new();

	for mod_id in file_dag.contributors() {
		let current = contributors.get(mod_id).ok_or_else(|| {
			format!(
				"missing parsed contributor {} for {}",
				mod_id.as_str(),
				file_dag.file_path()
			)
		})?;
		let resolved = resolver.resolve_base(file_dag, mod_id);
		let base_source = resolver
			.resolve_base_source(
				&resolved,
				file_dag,
				vanilla,
				contributors,
				merge_key_source,
				policies,
			)
			.ok_or_else(|| {
				format!(
					"failed to synthesize DAG base for {} in {}",
					mod_id.as_str(),
					file_dag.file_path()
				)
			})?;
		let patches = fold_renames(diff_against_base_source(
			base_source,
			vanilla,
			current,
			merge_key_source,
		));
		mod_patches.push((mod_id.0.clone(), file_dag.precedence_of(mod_id), patches));
	}

	Ok(DagPatchComputation {
		mod_patches,
		base_statements: final_base_statements(file_dag, vanilla),
	})
}

fn diff_against_base_source(
	base_source: BaseSource,
	vanilla: Option<&ParsedScriptFile>,
	current: &ParsedScriptFile,
	merge_key_source: MergeKeySource,
) -> Vec<ClausewitzPatch> {
	match base_source {
		BaseSource::Vanilla => match vanilla {
			Some(base) => diff_ast(base, current, merge_key_source),
			None => diff_ast_as_inserts(current, merge_key_source),
		},
		BaseSource::Empty => diff_ast_as_inserts(current, merge_key_source),
		BaseSource::Synthesized(base) => diff_ast(&base, current, merge_key_source),
	}
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

/// When a file has no prior version, treat every top-level assignment as an
/// `InsertNode` patch.
fn diff_ast_as_inserts(
	parsed: &foch_language::analyzer::semantic_index::ParsedScriptFile,
	_merge_key_source: MergeKeySource,
) -> Vec<ClausewitzPatch> {
	use foch_language::analyzer::parser::AstStatement;

	parsed
		.ast
		.statements
		.iter()
		.filter_map(|stmt| match stmt {
			AstStatement::Assignment { key, .. } => Some(ClausewitzPatch::InsertNode {
				path: vec![],
				key: key.clone(),
				statement: stmt.clone(),
			}),
			_ => None,
		})
		.collect()
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
		let (dag, diags) = super::super::dag::build_mod_dag(&mods);
		assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
		let fdag = induced_file_dag(&dag, "common/foo.txt", &contribs, &ignore);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		compute_dag_patches_from_parsed(
			&fdag,
			vanilla.as_ref(),
			&inventory,
			MergeKeySource::AssignmentKey,
			&MergePolicies::default(),
			ignore,
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
