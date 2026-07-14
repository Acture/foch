use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use foch_core::config::DepOverride;
use foch_core::model::{MergePlanEntry, MergePlanTarget};
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, ContentLoadPolicy, DefinitionModulePolicy, MergeKeySource,
	module_name_for_descriptor,
};
use foch_language::analyzer::definition_module::{DefinitionModuleInput, load_definition_module};
use foch_language::analyzer::semantic_index::{ParsedScriptFile, parse_script_file};

use super::dag::{FileDag, IgnoreReplacePath, ModDag, ModId, induced_file_dag_with_overrides};
use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace, WorkspaceScriptCache};

#[derive(Clone, Debug)]
pub(crate) struct CrossFileModuleViews {
	pub aggregate_contributors: Vec<ResolvedFileContributor>,
	pub file_dag: FileDag,
	pub vanilla: Option<ParsedScriptFile>,
	pub contributors: HashMap<ModId, ParsedScriptFile>,
}

pub(crate) fn build_cross_file_module_views(
	entry: &MergePlanEntry,
	workspace: &ResolvedWorkspace,
	descriptor: &ContentFamilyDescriptor,
	mod_dag: &ModDag,
	ignore_replace_path: &IgnoreReplacePath,
	dep_overrides: &[DepOverride],
) -> Result<CrossFileModuleViews, String> {
	let (merge_unit, input_paths, module_policy) = validate_module_target(entry, descriptor)?;

	let mut base_files = BTreeMap::new();
	let mut files_by_mod: HashMap<ModId, BTreeMap<String, ParsedScriptFile>> = HashMap::new();
	let mut representatives: HashMap<ModId, ResolvedFileContributor> = HashMap::new();
	let mut base_representative = None;

	for input_path in input_paths {
		let contributors = workspace
			.file_inventory
			.get(input_path)
			.ok_or_else(|| format!("missing module input {input_path}"))?;
		for contributor in contributors {
			if contributor.is_synthetic_base {
				continue;
			}
			let parsed = parse_contributor(contributor, &workspace.script_cache)?;
			if contributor.is_base_game {
				base_files.insert(input_path.clone(), parsed);
				base_representative.get_or_insert_with(|| contributor.clone());
				continue;
			}
			let mod_id = ModId(contributor.mod_id.clone());
			files_by_mod
				.entry(mod_id.clone())
				.or_default()
				.insert(input_path.clone(), parsed);
			representatives
				.entry(mod_id)
				.and_modify(|current| {
					if contributor.absolute_path < current.absolute_path {
						*current = contributor.clone();
					}
				})
				.or_insert_with(|| contributor.clone());
		}
	}

	include_reset_only_module_participants(
		workspace,
		module_policy,
		ignore_replace_path,
		&mut representatives,
	);
	let mut aggregate_contributors = base_representative.into_iter().collect::<Vec<_>>();
	aggregate_contributors.extend(representatives.values().cloned());
	aggregate_contributors.sort_by(|left, right| {
		left.precedence
			.cmp(&right.precedence)
			.then_with(|| left.mod_id.cmp(&right.mod_id))
	});

	let file_dag = induced_file_dag_with_overrides(
		mod_dag,
		entry.output_path(),
		&aggregate_contributors,
		ignore_replace_path,
		dep_overrides,
	);
	let vanilla = if base_files.is_empty() {
		None
	} else {
		Some(fold_visible_module_files(
			"__base_game__",
			&merge_unit.module_name,
			module_policy,
			&base_files,
		)?)
	};

	let mut effective_views = HashMap::new();
	for mod_id in file_dag.contributors() {
		let ancestors = effective_ancestors(mod_dag, mod_id, dep_overrides);
		let mut visible = base_files.clone();
		if file_dag.contributors().iter().any(|candidate| {
			file_dag.replaces_path(candidate)
				&& file_dag.precedence_of(candidate) <= file_dag.precedence_of(mod_id)
		}) {
			visible.clear();
		}
		for candidate in mod_dag.topo() {
			if candidate != mod_id && (!ancestors.contains(candidate) || !file_dag.ships(candidate))
			{
				continue;
			}
			if module_is_reset_by(candidate, module_policy, mod_dag, ignore_replace_path) {
				visible.clear();
			}
			if let Some(owned_files) = files_by_mod.get(candidate) {
				for (path, parsed) in owned_files {
					visible.insert(path.clone(), parsed.clone());
				}
			}
		}
		effective_views.insert(
			mod_id.clone(),
			fold_visible_module_files(
				mod_id.as_str(),
				&merge_unit.module_name,
				module_policy,
				&visible,
			)?,
		);
	}

	Ok(CrossFileModuleViews {
		aggregate_contributors,
		file_dag,
		vanilla,
		contributors: effective_views,
	})
}

fn validate_module_target<'a>(
	entry: &'a MergePlanEntry,
	descriptor: &ContentFamilyDescriptor,
) -> Result<
	(
		&'a foch_core::model::MergeUnitId,
		&'a [String],
		DefinitionModulePolicy,
	),
	String,
> {
	let MergePlanTarget::Module {
		id: merge_unit,
		input_paths,
		replace_prefix,
		..
	} = &entry.target
	else {
		return Err(format!(
			"{} is not a cross-file merge unit",
			entry.output_path()
		));
	};
	if merge_unit.family_id != descriptor.id.as_str() {
		return Err(format!(
			"merge unit family {} does not match descriptor {}",
			merge_unit.family_id,
			descriptor.id.as_str()
		));
	}
	if descriptor.merge_key_source != Some(MergeKeySource::AssignmentKey) {
		return Err(format!(
			"cross-file module {} requires assignment-key merge semantics",
			merge_unit.module_name
		));
	}
	let ContentLoadPolicy::DefinitionModule(module_policy) = descriptor.load_policy else {
		return Err(format!(
			"cross-file module {} is missing a definition-module load policy",
			merge_unit.module_name
		));
	};
	if module_policy.full_output_path != entry.output_path() {
		return Err(format!(
			"module output {} does not match policy output {}",
			entry.output_path(),
			module_policy.full_output_path
		));
	}
	if replace_prefix != module_policy.replacement_prefix {
		return Err(format!(
			"module replacement prefix {replace_prefix} does not match policy prefix {}",
			module_policy.replacement_prefix
		));
	}
	if input_paths.is_empty() {
		return Err(format!(
			"definition module {} has no input paths",
			merge_unit.module_name
		));
	}
	for input_path in input_paths {
		if !module_input_is_within_prefix(input_path, module_policy.replacement_prefix) {
			return Err(format!(
				"module input {input_path} is outside replacement prefix {}",
				module_policy.replacement_prefix
			));
		}
		let expected_module_name = module_name_for_descriptor(Path::new(input_path), descriptor);
		if merge_unit.module_name != expected_module_name {
			return Err(format!(
				"merge unit module {} does not match input module {expected_module_name} for {input_path}",
				merge_unit.module_name
			));
		}
	}
	Ok((merge_unit, input_paths, module_policy))
}

fn module_input_is_within_prefix(path: &str, prefix: &str) -> bool {
	let path = path.replace('\\', "/");
	let prefix = prefix.trim_matches('/').replace('\\', "/");
	path.strip_prefix(&prefix)
		.is_some_and(|suffix| suffix.starts_with('/'))
}

fn parse_contributor(
	contributor: &ResolvedFileContributor,
	script_cache: &WorkspaceScriptCache,
) -> Result<ParsedScriptFile, String> {
	let relative = contributor
		.absolute_path
		.strip_prefix(&contributor.root_path)
		.map_err(|_| {
			format!(
				"{} is outside contributor root {}",
				contributor.absolute_path.display(),
				contributor.root_path.display()
			)
		})?;
	if let Some(parsed) = script_cache.get(&contributor.mod_id, relative) {
		return Ok(parsed.clone());
	}
	parse_script_file(
		&contributor.mod_id,
		&contributor.root_path,
		&contributor.absolute_path,
	)
	.ok_or_else(|| {
		format!(
			"failed to parse module input {}",
			contributor.absolute_path.display()
		)
	})
}

fn include_reset_only_module_participants(
	workspace: &ResolvedWorkspace,
	policy: DefinitionModulePolicy,
	ignore_replace_path: &IgnoreReplacePath,
	representatives: &mut HashMap<ModId, ResolvedFileContributor>,
) {
	let base_offset = usize::from(workspace.installed_base_snapshot.is_some());
	for (index, mod_item) in workspace.mods.iter().enumerate() {
		let mod_id = ModId(mod_item.mod_id.clone());
		let owns_reset = !replace_path_is_ignored(ignore_replace_path, &mod_id)
			&& mod_item.descriptor.as_ref().is_some_and(|descriptor| {
				descriptor
					.replace_path
					.iter()
					.any(|prefix| path_is_covered(policy.replacement_prefix, prefix))
			});
		if !owns_reset && !representatives.contains_key(&mod_id) {
			continue;
		}
		let precedence = base_offset + index;
		if let Some(representative) = representatives.get_mut(&mod_id) {
			representative.precedence = precedence;
			continue;
		}
		let Some(root_path) = mod_item.root_path.clone() else {
			continue;
		};
		let mod_hash = workspace
			.mod_snapshots
			.get(index)
			.and_then(|snapshot| snapshot.as_ref())
			.and_then(|snapshot| snapshot.mod_hash.clone());
		representatives.insert(
			mod_id,
			ResolvedFileContributor {
				mod_id: mod_item.mod_id.clone(),
				absolute_path: root_path.join(policy.full_output_path),
				root_path,
				precedence,
				is_base_game: false,
				is_synthetic_base: false,
				parse_ok_hint: Some(true),
				mod_hash,
			},
		);
	}
}

fn replace_path_is_ignored(ignore: &IgnoreReplacePath, mod_id: &ModId) -> bool {
	match ignore {
		IgnoreReplacePath::None => false,
		IgnoreReplacePath::Mods(mods) => mods.contains(mod_id),
		IgnoreReplacePath::All => true,
	}
}

fn path_is_covered(path: &str, prefix: &str) -> bool {
	let path = path.trim_matches('/').replace('\\', "/");
	let prefix = prefix.trim_matches('/').replace('\\', "/");
	path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn module_is_reset_by(
	mod_id: &ModId,
	policy: DefinitionModulePolicy,
	mod_dag: &ModDag,
	ignore_replace_path: &IgnoreReplacePath,
) -> bool {
	!replace_path_is_ignored(ignore_replace_path, mod_id)
		&& mod_dag
			.replace_paths(mod_id)
			.iter()
			.any(|prefix| path_is_covered(policy.replacement_prefix, prefix))
}

fn effective_ancestors(
	mod_dag: &ModDag,
	mod_id: &ModId,
	dep_overrides: &[DepOverride],
) -> HashSet<ModId> {
	let ignored = dep_overrides
		.iter()
		.map(|item| (ModId(item.mod_id.clone()), ModId(item.dep_id.clone())))
		.collect::<HashSet<_>>();
	let mut ancestors = HashSet::new();
	let mut stack = mod_dag
		.parents_of(mod_id)
		.iter()
		.filter(|parent| !ignored.contains(&(mod_id.clone(), (*parent).clone())))
		.cloned()
		.map(|parent| (mod_id.clone(), parent))
		.collect::<Vec<_>>();
	while let Some((child, parent)) = stack.pop() {
		if ignored.contains(&(child.clone(), parent.clone())) || !ancestors.insert(parent.clone()) {
			continue;
		}
		stack.extend(
			mod_dag
				.parents_of(&parent)
				.iter()
				.cloned()
				.map(|grandparent| (parent.clone(), grandparent)),
		);
	}
	ancestors
}

fn fold_visible_module_files(
	mod_id: &str,
	module_name: &str,
	policy: DefinitionModulePolicy,
	visible_files: &BTreeMap<String, ParsedScriptFile>,
) -> Result<ParsedScriptFile, String> {
	let inputs = visible_files
		.iter()
		.map(|(path, file)| DefinitionModuleInput::new(Path::new(path), file))
		.collect::<Vec<_>>();
	let canonical = load_definition_module(&inputs, policy)
		.map_err(|error| format!("failed to load definition module: {error:?}"))?;
	let output_path = PathBuf::from(policy.full_output_path);
	let mut parsed = visible_files
		.values()
		.next()
		.cloned()
		.unwrap_or_else(|| ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: output_path.clone(),
			relative_path: output_path.clone(),
			content_family: None,
			file_kind: foch_language::analyzer::content_family::CwtType::new("other"),
			module_name: module_name.to_string(),
			ast: canonical.ast.clone(),
			source: String::new(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		});
	parsed.mod_id = mod_id.to_string();
	parsed.path = output_path.clone();
	parsed.relative_path = output_path.clone();
	parsed.module_name = module_name.to_string();
	parsed.ast = canonical.ast;
	parsed.source.clear();
	parsed.parse_issues.clear();
	parsed.parse_cache_hit = false;
	Ok(parsed)
}

#[cfg(test)]
mod tests {
	use super::{fold_visible_module_files, validate_module_target};
	use foch_core::model::{MergePlanEntry, MergePlanStrategy, MergePlanTarget, MergeUnitId};
	use foch_language::analyzer::content_family::{
		ContentFamilyDescriptor, DefinitionFileOrder, DefinitionKeyPolicy, DefinitionModulePolicy,
		DuplicateDefinitionPolicy, GameProfile,
	};
	use foch_language::analyzer::eu4_profile::eu4_profile;
	use foch_language::analyzer::parser::{AstStatement, AstValue};
	use foch_language::analyzer::semantic_index::parse_script_file;
	use std::collections::BTreeMap;
	use std::fs;
	use std::path::Path;
	use tempfile::TempDir;

	fn module_entry(input_path: &str, module_name: &str, replace_prefix: &str) -> MergePlanEntry {
		MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: "common/governments".to_string(),
					module_name: module_name.to_string(),
				},
				input_paths: vec![input_path.to_string()],
				output_path: "common/governments/zzz_foch_governments.txt".to_string(),
				replace_prefix: replace_prefix.to_string(),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: Vec::new(),
			winner: None,
			generated: false,
			notes: Vec::new(),
		}
	}

	fn governments_descriptor() -> &'static ContentFamilyDescriptor {
		eu4_profile()
			.classify_content_family(Path::new("common/governments/example.txt"))
			.expect("governments descriptor")
	}

	#[test]
	fn module_target_rejects_a_different_replacement_prefix() {
		let entry = module_entry(
			"common/governments/example.txt",
			"governments",
			"common/ideas",
		);

		let error = validate_module_target(&entry, governments_descriptor())
			.expect_err("target prefix must match the load policy");

		assert!(error.contains("common/ideas"), "error: {error}");
		assert!(error.contains("common/governments"), "error: {error}");
	}

	#[test]
	fn module_target_rejects_inputs_outside_the_replacement_prefix() {
		let entry = module_entry(
			"events/not_governments.txt",
			"governments",
			"common/governments",
		);

		let error = validate_module_target(&entry, governments_descriptor())
			.expect_err("module input must stay within its runtime prefix");

		assert!(
			error.contains("events/not_governments.txt"),
			"error: {error}"
		);
	}

	#[test]
	fn module_target_rejects_a_mismatched_module_name() {
		let entry = module_entry(
			"common/governments/example.txt",
			"ideas",
			"common/governments",
		);

		let error = validate_module_target(&entry, governments_descriptor())
			.expect_err("module id must match the descriptor's module rule");

		assert!(error.contains("ideas"), "error: {error}");
		assert!(error.contains("governments"), "error: {error}");
	}

	#[test]
	fn module_target_rejects_an_empty_input_set() {
		let mut entry = module_entry(
			"common/governments/example.txt",
			"governments",
			"common/governments",
		);
		let MergePlanTarget::Module { input_paths, .. } = &mut entry.target else {
			unreachable!();
		};
		input_paths.clear();

		let error = validate_module_target(&entry, governments_descriptor())
			.expect_err("module target must have at least one input");

		assert!(error.contains("no input paths"), "error: {error}");
	}

	#[test]
	fn later_filename_wins_same_top_level_key() {
		let temp = TempDir::new().expect("temp dir");
		let early = temp.path().join("common/governments/00_governments.txt");
		let late = temp.path().join("common/governments/zzz_governments.txt");
		fs::create_dir_all(early.parent().expect("parent")).expect("create parent");
		fs::write(&early, "shared = old\nearly_only = yes\n").expect("write early");
		fs::write(&late, "shared = new\nlate_only = yes\n").expect("write late");
		let mut files = BTreeMap::new();
		for path in [&early, &late] {
			let parsed = parse_script_file("mod", temp.path(), path).expect("parse");
			files.insert(
				parsed.relative_path.to_string_lossy().replace('\\', "/"),
				parsed,
			);
		}

		let folded = fold_visible_module_files(
			"mod",
			"governments",
			DefinitionModulePolicy {
				definition_key: DefinitionKeyPolicy::AssignmentKey,
				file_order: DefinitionFileOrder::NormalizedPathAscending,
				duplicate_definitions: DuplicateDefinitionPolicy::LaterDefinitionWins,
				full_output_path: "common/governments/zzz_foch_governments.txt",
				replacement_prefix: "common/governments",
				policy_version: 1,
			},
			&files,
		)
		.expect("fold module files");
		let shared = folded
			.ast
			.statements
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment { key, value, .. } if key == "shared" => Some(value),
				_ => None,
			})
			.collect::<Vec<_>>();
		assert_eq!(shared.len(), 1);
		assert!(matches!(
			shared[0],
			AstValue::Scalar { value, .. } if value.as_text() == "new"
		));
		assert_eq!(
			folded
				.ast
				.statements
				.iter()
				.filter(|statement| matches!(statement, AstStatement::Assignment { .. }))
				.count(),
			3
		);
	}
}
