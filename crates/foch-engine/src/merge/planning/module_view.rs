use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use foch::game::eu4::content::{
	ContentFamilyDescriptor, ContentLoadPolicy, DefinitionModuleOutput, DefinitionModulePolicy,
	DuplicateDefinitionPolicy, MergeKeySource,
};
use foch::game::eu4::script::ParsedScriptFile;
use foch::game::eu4::script::definition_module::{DefinitionModuleInput, load_definition_module};
use foch::model::{MergePlanEntry, MergePlanTarget};
use foch::project::DepOverride;

use super::dag::{FileDag, IgnoreReplacePath, ModDag, ModId, induced_file_dag_with_overrides};
use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace, WorkspaceScriptCache};

#[derive(Clone, Debug)]
pub(crate) struct CrossFileModuleViews {
	pub aggregate_contributors: Vec<ResolvedFileContributor>,
	pub file_dag: FileDag,
	pub vanilla: Option<ParsedScriptFile>,
	pub contributors: HashMap<ModId, ParsedScriptFile>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CrossFileModuleViewError {
	UnsupportedInput(String),
	EngineFailure(String),
}

impl CrossFileModuleViewError {
	fn engine_failure(reason: impl Into<String>) -> Self {
		Self::EngineFailure(reason.into())
	}

	fn unsupported_input(reason: impl Into<String>) -> Self {
		Self::UnsupportedInput(reason.into())
	}
}

#[derive(Clone, Debug)]
struct VisibleModuleFile {
	layer_ordinal: usize,
	parsed: ParsedScriptFile,
}

pub(crate) fn build_cross_file_module_views(
	entry: &MergePlanEntry,
	workspace: &ResolvedWorkspace,
	descriptor: &ContentFamilyDescriptor,
	mod_dag: &ModDag,
	ignore_replace_path: &IgnoreReplacePath,
	dep_overrides: &[DepOverride],
	duplicate_definitions: Option<DuplicateDefinitionPolicy>,
) -> Result<CrossFileModuleViews, CrossFileModuleViewError> {
	let has_covering_reset_participant =
		definition_module_has_covering_reset_participant(workspace, descriptor);
	let (merge_unit, input_paths, module_policy) =
		validate_module_target(entry, descriptor, has_covering_reset_participant)
			.map_err(CrossFileModuleViewError::engine_failure)?;
	let module_policy = apply_duplicate_definition_override(
		module_policy,
		duplicate_definitions,
		descriptor.merge_key_source,
	);

	let mut base_files = BTreeMap::new();
	let mut files_by_mod: HashMap<ModId, BTreeMap<String, ParsedScriptFile>> = HashMap::new();
	let mut representatives: HashMap<ModId, ResolvedFileContributor> = HashMap::new();
	let mut base_representative = None;

	for input_path in input_paths {
		let contributors = workspace.file_inventory.get(input_path).ok_or_else(|| {
			CrossFileModuleViewError::engine_failure(format!("missing module input {input_path}"))
		})?;
		for contributor in contributors {
			if contributor.is_synthetic_base {
				continue;
			}
			let parsed = parse_contributor(contributor, &workspace.script_cache)
				.map_err(CrossFileModuleViewError::engine_failure)?;
			if contributor.is_base_game {
				base_files.insert(
					input_path.clone(),
					VisibleModuleFile {
						layer_ordinal: 0,
						parsed,
					},
				);
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
		for (layer_ordinal, candidate) in mod_dag.topo().iter().enumerate() {
			if candidate != mod_id && (!ancestors.contains(candidate) || !file_dag.ships(candidate))
			{
				continue;
			}
			if module_is_reset_by(candidate, module_policy, mod_dag, ignore_replace_path) {
				visible.clear();
			}
			if let Some(owned_files) = files_by_mod.get(candidate) {
				for (path, parsed) in owned_files {
					visible.insert(
						path.clone(),
						VisibleModuleFile {
							layer_ordinal: layer_ordinal + 1,
							parsed: parsed.clone(),
						},
					);
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

fn apply_duplicate_definition_override(
	mut policy: DefinitionModulePolicy,
	duplicate_definitions: Option<DuplicateDefinitionPolicy>,
	merge_key_source: Option<MergeKeySource>,
) -> DefinitionModulePolicy {
	if merge_key_source == Some(MergeKeySource::AssignmentKey)
		&& let Some(duplicate_definitions) = duplicate_definitions
	{
		policy.duplicate_definitions = duplicate_definitions;
	}
	policy
}

fn validate_module_target<'a>(
	entry: &'a MergePlanEntry,
	descriptor: &ContentFamilyDescriptor,
	has_covering_reset_participant: bool,
) -> Result<
	(
		&'a foch::model::MergeUnitId,
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
	if !matches!(
		descriptor.merge_key_source,
		Some(
			MergeKeySource::AssignmentKey
				| MergeKeySource::FieldValue(_)
				| MergeKeySource::ChildFieldValue { .. }
		)
	) {
		return Err(format!(
			"cross-file module {} requires a top-level definition merge key",
			merge_unit.module_name
		));
	}
	let ContentLoadPolicy::DefinitionModule(module_policy) = descriptor.load_policy else {
		return Err(format!(
			"cross-file module {} is missing a definition-module load policy",
			merge_unit.module_name
		));
	};
	if module_policy.output_path != entry.output_path() {
		return Err(format!(
			"module output {} does not match policy output {}",
			entry.output_path(),
			module_policy.output_path
		));
	}
	let statically_replaces_namespace =
		module_policy.output_mode == DefinitionModuleOutput::ReplaceNamespace;
	let replacement_prefix_is_valid = match replace_prefix.as_deref() {
		Some(prefix) => {
			prefix == module_policy.namespace_prefix
				&& (statically_replaces_namespace || has_covering_reset_participant)
		}
		None => !statically_replaces_namespace,
	};
	if !replacement_prefix_is_valid {
		return Err(format!(
			"module replacement prefix {:?} does not match policy prefix {:?}; static replacement: {statically_replaces_namespace}, covering reset participant: {has_covering_reset_participant}",
			replace_prefix, module_policy.namespace_prefix
		));
	}
	if input_paths.is_empty() {
		return Err(format!(
			"definition module {} has no input paths",
			merge_unit.module_name
		));
	}
	for input_path in input_paths {
		if !module_input_is_within_prefix(input_path, module_policy.namespace_prefix) {
			return Err(format!(
				"module input {input_path} is outside namespace prefix {}",
				module_policy.namespace_prefix
			));
		}
		let expected_module_name = module_policy
			.namespace_prefix
			.rsplit('/')
			.next()
			.unwrap_or(descriptor.id.as_str());
		if merge_unit.module_name != expected_module_name {
			return Err(format!(
				"merge unit module {} does not match input module {expected_module_name} for {input_path}",
				merge_unit.module_name
			));
		}
	}
	Ok((merge_unit, input_paths, module_policy))
}

fn definition_module_has_covering_reset_participant(
	workspace: &ResolvedWorkspace,
	descriptor: &ContentFamilyDescriptor,
) -> bool {
	let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
		return false;
	};
	workspace.mods.iter().any(|mod_item| {
		mod_item.root_path.is_some()
			&& mod_item.descriptor.as_ref().is_some_and(|mod_descriptor| {
				mod_descriptor
					.replace_path
					.iter()
					.any(|prefix| path_is_covered(policy.namespace_prefix, prefix))
			})
	})
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
	script_cache
		.load(contributor)
		.map(|parsed| (*parsed).clone())
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
					.any(|prefix| path_is_covered(policy.namespace_prefix, prefix))
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
				absolute_path: root_path.join(policy.output_path),
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
			.any(|prefix| path_is_covered(policy.namespace_prefix, prefix))
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
	visible_files: &BTreeMap<String, VisibleModuleFile>,
) -> Result<ParsedScriptFile, CrossFileModuleViewError> {
	let inputs = visible_files
		.iter()
		.map(|(path, file)| {
			DefinitionModuleInput::new(Path::new(path), &file.parsed)
				.with_layer_ordinal(file.layer_ordinal)
		})
		.collect::<Vec<_>>();
	let canonical = load_definition_module(&inputs, policy).map_err(|error| {
		CrossFileModuleViewError::unsupported_input(format!(
			"failed to load definition module: {error:?}"
		))
	})?;
	let output_path = PathBuf::from(policy.output_path);
	let mut parsed = visible_files
		.values()
		.next()
		.map(|file| file.parsed.clone())
		.unwrap_or_else(|| ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: output_path.clone(),
			relative_path: output_path.clone(),
			content_family: None,
			file_kind: foch::game::eu4::content::ScriptFileKind::new("other"),
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
	use super::{
		VisibleModuleFile, apply_duplicate_definition_override, fold_visible_module_files,
		parse_contributor, validate_module_target,
	};
	use crate::workspace::{ResolvedFileContributor, WorkspaceScriptCache};
	use foch::game::eu4::content::eu4;
	use foch::game::eu4::content::{
		ContentFamilyDescriptor, ContentLoadPolicy, DefinitionFileOrder, DefinitionKeyPolicy,
		DefinitionModuleOutput, DefinitionModulePolicy, DuplicateDefinitionPolicy,
	};
	use foch::game::eu4::script::parse_script_file;
	use foch::game::eu4::script::parser::{AstStatement, AstValue};
	use foch::model::{MergePlanEntry, MergePlanStrategy, MergePlanTarget, MergeUnitId};
	use std::collections::BTreeMap;
	use std::fs;
	use std::path::Path;
	use tempfile::TempDir;

	#[test]
	fn module_view_never_bypasses_a_missing_verified_ast() {
		let temp = TempDir::new().expect("temp dir");
		let relative = "common/governments/test.txt";
		fs::create_dir_all(temp.path().join("common/governments")).expect("create governments dir");
		fs::write(temp.path().join(relative), "government = { rank = 1 }\n")
			.expect("write module script");
		let contributor = ResolvedFileContributor {
			mod_id: "mod-a".to_string(),
			root_path: temp.path().to_path_buf(),
			absolute_path: temp.path().join(relative),
			precedence: 1,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: Some(true),
			mod_hash: Some("hash-a".to_string()),
		};

		let error = parse_contributor(&contributor, &WorkspaceScriptCache::default())
			.expect_err("missing verified AST must fail closed");

		assert!(error.contains("no semantic-snapshot input"));
	}

	fn module_entry(input_path: &str, module_name: &str, replace_prefix: &str) -> MergePlanEntry {
		MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: "common/governments".to_string(),
					module_name: module_name.to_string(),
				},
				input_paths: vec![input_path.to_string()],
				output_path: "common/governments/zzz_foch_governments.txt".to_string(),
				replace_prefix: Some(replace_prefix.to_string()),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: Vec::new(),
			winner: None,
			notes: Vec::new(),
		}
	}

	fn governments_descriptor() -> &'static ContentFamilyDescriptor {
		eu4()
			.classify_content_family(Path::new("common/governments/example.txt"))
			.expect("governments descriptor")
	}

	fn powerprojection_entry(replace_prefix: Option<&str>) -> MergePlanEntry {
		MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: "common/powerprojection".to_string(),
					module_name: "powerprojection".to_string(),
				},
				input_paths: vec!["common/powerprojection/example.txt".to_string()],
				output_path: "common/powerprojection/zzz_foch_powerprojection.txt".to_string(),
				replace_prefix: replace_prefix.map(str::to_string),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: Vec::new(),
			winner: None,
			notes: Vec::new(),
		}
	}

	fn powerprojection_descriptor() -> &'static ContentFamilyDescriptor {
		eu4()
			.classify_content_family(Path::new("common/powerprojection/example.txt"))
			.expect("powerprojection descriptor")
	}

	#[test]
	fn module_target_rejects_a_different_replacement_prefix() {
		let entry = module_entry(
			"common/governments/example.txt",
			"governments",
			"common/ideas",
		);

		let error = validate_module_target(&entry, governments_descriptor(), false)
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

		let error = validate_module_target(&entry, governments_descriptor(), false)
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

		let error = validate_module_target(&entry, governments_descriptor(), false)
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

		let error = validate_module_target(&entry, governments_descriptor(), false)
			.expect_err("module target must have at least one input");

		assert!(error.contains("no input paths"), "error: {error}");
	}

	#[test]
	fn overlay_module_replacement_requires_a_covering_reset_participant() {
		let entry = powerprojection_entry(Some("common/powerprojection"));

		let error = validate_module_target(&entry, powerprojection_descriptor(), false)
			.expect_err("overlay module cannot replace its namespace without a reset participant");
		assert!(error.contains("covering reset participant: false"));

		validate_module_target(&entry, powerprojection_descriptor(), true)
			.expect("covering reset participant permits dynamic namespace replacement");
	}

	#[test]
	fn structured_module_views_use_runtime_effective_duplicate_definitions() {
		let descriptor = eu4()
			.classify_content_family(Path::new("common/scripted_triggers/example.txt"))
			.expect("scripted triggers descriptor");
		let ContentLoadPolicy::DefinitionModule(mut policy) = descriptor.load_policy else {
			panic!("scripted triggers must be a definition module");
		};
		assert_eq!(
			policy.duplicate_definitions,
			DuplicateDefinitionPolicy::PreserveAll
		);

		assert_eq!(
			apply_duplicate_definition_override(
				policy,
				Some(DuplicateDefinitionPolicy::LaterDefinitionWins),
				descriptor.merge_key_source,
			)
			.duplicate_definitions,
			DuplicateDefinitionPolicy::LaterDefinitionWins
		);
		policy = apply_duplicate_definition_override(policy, None, descriptor.merge_key_source);
		assert_eq!(
			policy.duplicate_definitions,
			DuplicateDefinitionPolicy::PreserveAll
		);
	}

	#[test]
	fn nested_identity_modules_preserve_repeated_top_level_assignments() {
		let descriptor = eu4()
			.classify_content_family(Path::new("common/estates_preload/example.txt"))
			.expect("estates preload descriptor");
		let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
			panic!("estates preload must be a definition module");
		};

		assert_eq!(
			apply_duplicate_definition_override(
				policy,
				Some(DuplicateDefinitionPolicy::LaterDefinitionWins),
				descriptor.merge_key_source,
			)
			.duplicate_definitions,
			DuplicateDefinitionPolicy::PreserveAll
		);
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
				VisibleModuleFile {
					layer_ordinal: 1,
					parsed,
				},
			);
		}

		let folded = fold_visible_module_files(
			"mod",
			"governments",
			DefinitionModulePolicy {
				definition_key: DefinitionKeyPolicy::AssignmentKey,
				file_order: DefinitionFileOrder::NormalizedPathAscending,
				duplicate_definitions: DuplicateDefinitionPolicy::LaterDefinitionWins,
				output_path: "common/governments/zzz_foch_governments.txt",
				namespace_prefix: "common/governments",
				output_mode: DefinitionModuleOutput::ReplaceNamespace,
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

	#[test]
	fn later_layer_wins_over_earlier_lexically_later_file() {
		let temp = TempDir::new().expect("temp dir");
		let earlier = temp.path().join("common/governments/zzz_source.txt");
		let later = temp.path().join("common/governments/00_compatch.txt");
		fs::create_dir_all(earlier.parent().expect("parent")).expect("create parent");
		fs::write(&earlier, "shared = source\n").expect("write source");
		fs::write(&later, "shared = compatch\n").expect("write compatch");
		let earlier = parse_script_file("source", temp.path(), &earlier).expect("parse source");
		let later = parse_script_file("compatch", temp.path(), &later).expect("parse compatch");
		let mut files = BTreeMap::new();
		files.insert(
			earlier.relative_path.to_string_lossy().replace('\\', "/"),
			VisibleModuleFile {
				layer_ordinal: 1,
				parsed: earlier,
			},
		);
		files.insert(
			later.relative_path.to_string_lossy().replace('\\', "/"),
			VisibleModuleFile {
				layer_ordinal: 2,
				parsed: later,
			},
		);

		let folded = fold_visible_module_files(
			"compatch",
			"governments",
			DefinitionModulePolicy {
				definition_key: DefinitionKeyPolicy::AssignmentKey,
				file_order: DefinitionFileOrder::NormalizedPathAscending,
				duplicate_definitions: DuplicateDefinitionPolicy::LaterDefinitionWins,
				output_path: "common/governments/zzz_foch_governments.txt",
				namespace_prefix: "common/governments",
				output_mode: DefinitionModuleOutput::ReplaceNamespace,
				policy_version: 1,
			},
			&files,
		)
		.expect("fold module files");

		assert!(matches!(
			folded.ast.statements.as_slice(),
			[AstStatement::Assignment {
				key,
				value: AstValue::Scalar { value, .. },
				..
			}] if key == "shared" && value.as_text() == "compatch"
		));
	}
}
