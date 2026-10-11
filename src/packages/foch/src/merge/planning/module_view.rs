use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::game::eu4::content::load_rules::load_rules_for_version;
use crate::game::eu4::content::{
	ContentFamilyDescriptor, ContentLoadPolicy, DefinitionModuleOutput, DefinitionModulePolicy,
	DuplicateDefinitionPolicy, MergeKeySource,
};
use crate::game::eu4::script::ParsedScriptFile;
use crate::game::eu4::script::definition_module::{
	DefinitionModuleInput, DefinitionSource, load_definition_module,
};
use crate::game::eu4::script::parser::AstStatement;
use crate::model::{GamePath, GamePathBuf, MergeModuleOutput, MergePlanEntry, MergePlanTarget};
use crate::project::DepOverride;

use super::dag::{FileDag, IgnoreReplacePath, ModDag, ModId, induced_file_dag_with_overrides};
use crate::input::{InputScriptCache, ResolvedInput, ResolvedInputContributor};

#[derive(Clone, Debug)]
pub(crate) struct CrossFileModuleViews {
	pub aggregate_contributors: Vec<ResolvedInputContributor>,
	pub file_dag: FileDag,
	pub vanilla: Option<ParsedScriptFile>,
	pub contributors: HashMap<ModId, ParsedScriptFile>,
	pub definition_sources: HashMap<ModId, BTreeMap<String, ModuleDefinitionSource>>,
}

#[derive(Clone, Debug)]
pub(crate) struct ModuleDefinitionSource {
	pub mod_id: String,
	pub source: DefinitionSource,
}

struct FoldedModule {
	parsed: ParsedScriptFile,
	sources: BTreeMap<String, ModuleDefinitionSource>,
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_cross_file_module_views(
	entry: &MergePlanEntry,
	namespace: &MergeModuleOutput,
	input: &ResolvedInput,
	descriptor: &ContentFamilyDescriptor,
	mod_dag: &ModDag,
	ignore_replace_path: &IgnoreReplacePath,
	dep_overrides: &[DepOverride],
	duplicate_definitions: Option<DuplicateDefinitionPolicy>,
) -> Result<CrossFileModuleViews, CrossFileModuleViewError> {
	let has_covering_reset_participant =
		definition_module_has_covering_reset_participant(input, descriptor);
	let (merge_unit, input_paths, module_policy) = validate_module_target(
		entry,
		namespace,
		descriptor,
		has_covering_reset_participant,
		input.game_version.as_deref(),
	)
	.map_err(CrossFileModuleViewError::engine_failure)?;
	let module_policy = apply_duplicate_definition_override(
		module_policy,
		duplicate_definitions,
		descriptor.merge_key_source,
	);

	let mut base_files = BTreeMap::new();
	let mut files_by_mod: HashMap<ModId, BTreeMap<GamePathBuf, ParsedScriptFile>> = HashMap::new();
	let mut representatives: HashMap<ModId, ResolvedInputContributor> = HashMap::new();
	let mut base_representative = None;

	for input_path in input_paths {
		let contributors = input.file_inventory.get(input_path).ok_or_else(|| {
			CrossFileModuleViewError::engine_failure(format!("missing module input {input_path}"))
		})?;
		for contributor in contributors {
			if contributor.is_synthetic_base {
				continue;
			}
			let parsed = parse_contributor(contributor, &input.script_cache)
				.map_err(CrossFileModuleViewError::engine_failure)?;
			if contributor.is_base_game {
				base_files.insert(
					input_path.to_owned(),
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
				.insert(input_path.to_owned(), parsed);
			// One contributor stands for each mod: the one with the first
			// game path. Module inputs are direct children of one directory,
			// so this is also the first physical file under the mod's root.
			representatives
				.entry(mod_id)
				.and_modify(|current| {
					if contributor.relative_path < current.relative_path {
						*current = contributor.clone();
					}
				})
				.or_insert_with(|| contributor.clone());
		}
	}

	include_reset_only_module_participants(
		input,
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

	// `replace_path` is declared per directory, so the visibility DAG is induced
	// for this namespace's own output path and never the unit's primary one.
	let file_dag = induced_file_dag_with_overrides(
		mod_dag,
		namespace.output_path(),
		&aggregate_contributors,
		ignore_replace_path,
		dep_overrides,
	);
	let vanilla = if base_files.is_empty() {
		None
	} else {
		Some(
			fold_visible_module_files(
				"__base_game__",
				&merge_unit.module_name,
				module_policy,
				&base_files,
			)?
			.parsed,
		)
	};

	let mut effective_views = HashMap::new();
	let mut definition_sources = HashMap::new();
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
		let folded = fold_visible_module_files(
			mod_id.as_str(),
			&merge_unit.module_name,
			module_policy,
			&visible,
		)?;
		definition_sources.insert(mod_id.clone(), folded.sources);
		effective_views.insert(mod_id.clone(), folded.parsed);
	}

	Ok(CrossFileModuleViews {
		aggregate_contributors,
		file_dag,
		vanilla,
		contributors: effective_views,
		definition_sources,
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
	namespace: &MergeModuleOutput,
	descriptor: &ContentFamilyDescriptor,
	has_covering_reset_participant: bool,
	game_version: Option<&str>,
) -> Result<
	(
		&'a crate::model::MergeUnitId,
		Vec<&'a GamePath>,
		DefinitionModulePolicy,
	),
	String,
> {
	let MergePlanTarget::Module { id: merge_unit, .. } = &entry.target else {
		return Err(format!(
			"{} is not a cross-file merge unit",
			entry.output_path()
		));
	};
	let replace_prefix: Option<&GamePath> = namespace.replace_prefix();
	// Each namespace of a database unit keeps only its own inputs: the
	// directory decides the output file, the `replace_path` prefix and the
	// extractor, so inputs must not cross namespaces. An input claimed by no
	// namespace would otherwise be dropped from every merge without a word.
	let namespaces: &[MergeModuleOutput] = entry.target.module_outputs();
	if let Some(orphan) = entry.target.input_paths().iter().find(|path| {
		!namespaces
			.iter()
			.any(|namespace| path.is_child_of(namespace.namespace_prefix()))
	}) {
		return Err(format!(
			"module input {orphan} is outside every output namespace of {}",
			merge_unit.module_name
		));
	}
	let input_paths: Vec<&GamePath> = entry
		.target
		.namespace_input_paths(namespace.namespace_prefix());
	let output_path: &GamePath = namespace.output_path();
	let database: Option<&str> = game_version
		.and_then(load_rules_for_version)
		.map(|rules| rules.database_for(output_path))
		.transpose()?
		.flatten();
	let expected_family: &str = database.unwrap_or(descriptor.id.as_str());
	if merge_unit.family_id != expected_family {
		return Err(format!(
			"merge unit family {} does not match expected family {}",
			merge_unit.family_id, expected_family
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
	if module_policy.output_path != output_path {
		return Err(format!(
			"module output {} does not match policy output {}",
			output_path, module_policy.output_path
		));
	}
	let statically_replaces_namespace =
		module_policy.output_mode == DefinitionModuleOutput::ReplaceNamespace;
	let replacement_prefix_is_valid = match replace_prefix {
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
			"definition module {} has no input paths in namespace {}",
			merge_unit.module_name,
			namespace.namespace_prefix()
		));
	}
	for input_path in &input_paths {
		if !module_input_is_within_prefix(input_path, module_policy.namespace_prefix) {
			return Err(format!(
				"module input {input_path} is outside namespace prefix {}",
				module_policy.namespace_prefix
			));
		}
		let expected_module_name = module_policy.namespace_prefix.file_name();
		let database: Option<&str> = game_version
			.and_then(load_rules_for_version)
			.map(|rules| rules.database_for(input_path))
			.transpose()?
			.flatten();
		let expected_module_name: &str = database.unwrap_or(expected_module_name);
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
	input: &ResolvedInput,
	descriptor: &ContentFamilyDescriptor,
) -> bool {
	let ContentLoadPolicy::DefinitionModule(policy) = descriptor.load_policy else {
		return false;
	};
	input.mods.iter().any(|mod_item| {
		mod_item.root_path.is_some()
			&& mod_item.descriptor.as_ref().is_some_and(|mod_descriptor| {
				replace_paths_reset(&mod_descriptor.replace_path, policy)
			})
	})
}

/// Whether one of `replace_paths` resets `policy`'s namespace: a
/// `replace_path` names the namespace itself or a directory above it, compared
/// by whole components (`common/ideas` resets `common/ideas`, not
/// `common/ideas_extra`).
fn replace_paths_reset(replace_paths: &[GamePathBuf], policy: DefinitionModulePolicy) -> bool {
	replace_paths
		.iter()
		.any(|prefix| policy.namespace_prefix.starts_with(prefix))
}

/// Whether `path` lies anywhere below the `prefix` directory. Unlike a
/// namespace's inputs, which are its direct children, this admits nested
/// files; it guards that no input escapes its policy's directory.
fn module_input_is_within_prefix(path: &GamePath, prefix: &GamePath) -> bool {
	path.strip_prefix(prefix).is_some()
}

fn parse_contributor(
	contributor: &ResolvedInputContributor,
	script_cache: &InputScriptCache,
) -> Result<ParsedScriptFile, String> {
	script_cache
		.load(contributor)
		.map(|parsed| (*parsed).clone())
}

/// Top-level definition names every contributor declares under `input_paths`.
///
/// Used to detect one name defined in two directories of the same database.
/// Files that fail to parse are skipped: a parse failure is reported by the
/// merge itself, and this check must not turn it into a name collision.
///
/// `base_game_only` restricts the scan to the analyzed vanilla snapshot, whose
/// own arrangement is the evidence for how a database registers a repeated name.
pub(crate) fn declared_definition_keys<'a>(
	input_paths: impl IntoIterator<Item = &'a GamePath>,
	input: &ResolvedInput,
	base_game_only: bool,
) -> BTreeSet<String> {
	let mut keys: BTreeSet<String> = BTreeSet::new();
	for input_path in input_paths {
		let Some(contributors) = input.file_inventory.get(input_path) else {
			continue;
		};
		for contributor in contributors {
			if contributor.is_synthetic_base || (base_game_only && !contributor.is_base_game) {
				continue;
			}
			let Ok(parsed) = parse_contributor(contributor, &input.script_cache) else {
				continue;
			};
			keys.extend(
				parsed
					.ast
					.statements
					.iter()
					.filter_map(|statement| match statement {
						AstStatement::Assignment { key, .. } if !key.trim().is_empty() => {
							Some(key.clone())
						}
						_ => None,
					}),
			);
		}
	}
	keys
}

fn include_reset_only_module_participants(
	input: &ResolvedInput,
	policy: DefinitionModulePolicy,
	ignore_replace_path: &IgnoreReplacePath,
	representatives: &mut HashMap<ModId, ResolvedInputContributor>,
) {
	let base_offset = usize::from(input.installed_base_snapshot.is_some());
	for (index, mod_item) in input.mods.iter().enumerate() {
		let mod_id = ModId(mod_item.mod_id.clone());
		let owns_reset = !replace_path_is_ignored(ignore_replace_path, &mod_id)
			&& mod_item
				.descriptor
				.as_ref()
				.is_some_and(|descriptor| replace_paths_reset(&descriptor.replace_path, policy));
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
		let mod_hash = input
			.mod_snapshots
			.get(index)
			.and_then(|snapshot| snapshot.as_ref())
			.and_then(|snapshot| snapshot.mod_hash.clone());
		representatives.insert(
			mod_id,
			ResolvedInputContributor {
				mod_id: mod_item.mod_id.clone(),
				root_path,
				relative_path: policy.output_path.to_owned(),
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

fn module_is_reset_by(
	mod_id: &ModId,
	policy: DefinitionModulePolicy,
	mod_dag: &ModDag,
	ignore_replace_path: &IgnoreReplacePath,
) -> bool {
	!replace_path_is_ignored(ignore_replace_path, mod_id)
		&& replace_paths_reset(mod_dag.replace_paths(mod_id), policy)
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
	visible_files: &BTreeMap<GamePathBuf, VisibleModuleFile>,
) -> Result<FoldedModule, CrossFileModuleViewError> {
	let inputs = visible_files
		.iter()
		.map(|(path, file)| {
			DefinitionModuleInput::new(path, &file.parsed).with_layer_ordinal(file.layer_ordinal)
		})
		.collect::<Vec<_>>();
	let canonical = load_definition_module(&inputs, policy).map_err(|error| {
		CrossFileModuleViewError::unsupported_input(format!(
			"failed to load definition module: {error:?}"
		))
	})?;
	// The folded module exists only in memory, at the policy's output path.
	let output_path = canonical.ast.path.clone();
	let mut parsed = visible_files
		.values()
		.next()
		.map(|file| file.parsed.clone())
		.unwrap_or_else(|| ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: None,
			relative_path: output_path.clone(),
			content_family: None,
			file_kind: crate::game::eu4::content::ScriptFileKind::new("other"),
			module_name: module_name.to_string(),
			ast: canonical.ast.clone(),
			source: String::new(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		});
	parsed.mod_id = mod_id.to_string();
	parsed.path = None;
	parsed.relative_path = output_path;
	parsed.module_name = module_name.to_string();
	parsed.ast = canonical.ast;
	parsed.source.clear();
	// A definition isolated in any folded file is unknown in the folded view
	// too, so the view keeps those issues and no others.
	parsed.parse_issues = visible_files
		.values()
		.flat_map(|file| &file.parsed.parse_issues)
		.filter(|issue| issue.isolation.is_some())
		.cloned()
		.collect();
	parsed.parse_cache_hit = false;
	let sources = canonical
		.definition_sources
		.into_iter()
		.map(|(key, source)| {
			let mod_id = visible_files[&source.path].parsed.mod_id.clone();
			(key, ModuleDefinitionSource { mod_id, source })
		})
		.collect();
	Ok(FoldedModule { parsed, sources })
}

#[cfg(test)]
mod tests {
	use super::{
		VisibleModuleFile, apply_duplicate_definition_override, fold_visible_module_files,
		parse_contributor, validate_module_target,
	};
	use crate::game::eu4::content::eu4;
	use crate::game::eu4::content::{
		ContentFamilyDescriptor, ContentLoadPolicy, DefinitionFileOrder, DefinitionKeyPolicy,
		DefinitionModuleOutput, DefinitionModulePolicy, DuplicateDefinitionPolicy,
	};
	use crate::game::eu4::script::parse_script_file;
	use crate::game::eu4::script::parser::{AstStatement, AstValue};
	use crate::input::{InputScriptCache, ResolvedInputContributor};
	use crate::merge::planning::dag::{IgnoreReplacePath, ModId, build_mod_dag};
	use crate::model::ModCandidate;
	use crate::model::{
		GamePathBuf, MergeModuleOutput, MergeModuleOutputs, MergePlanEntry, MergePlanStrategy,
		MergePlanTarget, MergeUnitId,
	};
	use crate::playset::PlaysetEntry;
	use crate::playset::descriptor::ModDescriptor;
	use std::collections::BTreeMap;
	use std::fs;
	use tempfile::TempDir;

	#[test]
	fn module_view_never_bypasses_a_missing_verified_ast() {
		let temp = TempDir::new().expect("temp dir");
		let relative = "common/governments/test.txt";
		fs::create_dir_all(temp.path().join("common/governments")).expect("create governments dir");
		fs::write(temp.path().join(relative), "government = { rank = 1 }\n")
			.expect("write module script");
		let contributor = ResolvedInputContributor {
			mod_id: "mod-a".to_string(),
			root_path: temp.path().to_path_buf(),
			relative_path: crate::model::GamePathBuf::parse(relative).expect("valid game path"),
			precedence: 1,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: Some(true),
			mod_hash: Some("hash-a".to_string()),
		};

		let error = parse_contributor(&contributor, &InputScriptCache::default())
			.expect_err("missing verified AST must fail closed");

		assert!(error.contains("no semantic-snapshot input"));
	}

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("valid game path")
	}

	fn outputs(output_path: &str, replace_prefix: Option<&str>) -> MergeModuleOutputs {
		MergeModuleOutputs::new(vec![
			MergeModuleOutput::new(game_path(output_path), replace_prefix.map(game_path))
				.expect("output inside a namespace directory"),
		])
		.expect("one output")
	}

	fn module_entry(input_path: &str, module_name: &str, replace_prefix: &str) -> MergePlanEntry {
		MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: "common/governments".to_string(),
					module_name: module_name.to_string(),
				},
				input_paths: vec![game_path(input_path)],
				outputs: outputs(
					"common/governments/zzz_foch_governments.txt",
					Some(replace_prefix),
				),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: Vec::new(),
			winner: None,
			notes: Vec::new(),
		}
	}

	fn governments_descriptor() -> &'static ContentFamilyDescriptor {
		eu4()
			.classify_content_family(
				crate::model::GamePath::new("common/governments/example.txt")
					.expect("valid game path"),
			)
			.expect("governments descriptor")
	}

	fn powerprojection_entry(replace_prefix: Option<&str>) -> MergePlanEntry {
		MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: "common/powerprojection".to_string(),
					module_name: "powerprojection".to_string(),
				},
				input_paths: vec![game_path("common/powerprojection/example.txt")],
				outputs: outputs(
					"common/powerprojection/zzz_foch_powerprojection.txt",
					replace_prefix,
				),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: Vec::new(),
			winner: None,
			notes: Vec::new(),
		}
	}

	fn powerprojection_descriptor() -> &'static ContentFamilyDescriptor {
		eu4()
			.classify_content_family(
				crate::model::GamePath::new("common/powerprojection/example.txt")
					.expect("valid game path"),
			)
			.expect("powerprojection descriptor")
	}

	#[test]
	fn module_target_uses_the_database_name_from_the_selected_game_version() {
		let mut entry = powerprojection_entry(None);
		let MergePlanTarget::Module { id, .. } = &mut entry.target else {
			unreachable!();
		};
		id.module_name = "CPowerProjectionDatabase".to_string();
		id.family_id = "CPowerProjectionDatabase".to_string();
		validate_module_target(
			&entry,
			&entry.target.module_outputs()[0].clone(),
			powerprojection_descriptor(),
			false,
			Some("1.37.5"),
		)
		.expect("database unit keeps the existing output policy");
		assert!(
			validate_module_target(
				&entry,
				&entry.target.module_outputs()[0].clone(),
				powerprojection_descriptor(),
				false,
				Some("1.37.4")
			)
			.is_err()
		);
	}

	#[test]
	fn module_target_rejects_a_different_replacement_prefix() {
		let entry = module_entry(
			"common/governments/example.txt",
			"governments",
			"common/ideas",
		);

		let error = validate_module_target(
			&entry,
			&entry.target.module_outputs()[0].clone(),
			governments_descriptor(),
			false,
			None,
		)
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

		let error = validate_module_target(
			&entry,
			&entry.target.module_outputs()[0].clone(),
			governments_descriptor(),
			false,
			None,
		)
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

		let error = validate_module_target(
			&entry,
			&entry.target.module_outputs()[0].clone(),
			governments_descriptor(),
			false,
			None,
		)
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

		let error = validate_module_target(
			&entry,
			&entry.target.module_outputs()[0].clone(),
			governments_descriptor(),
			false,
			None,
		)
		.expect_err("module target must have at least one input");

		assert!(error.contains("no input paths"), "error: {error}");
	}

	/// Every reset decision here (a covering reset participant, a reset-only
	/// participant, a mod resetting a module) asks one question: does a
	/// `replace_path` name the namespace or a directory above it, by whole
	/// components.
	#[test]
	fn a_replace_path_resets_its_directory_and_what_lies_below_it() {
		let ContentLoadPolicy::DefinitionModule(policy) = governments_descriptor().load_policy
		else {
			panic!("governments must be a definition module");
		};
		assert_eq!(policy.namespace_prefix.as_str(), "common/governments");
		for (replace_path, resets) in [
			("common/governments", true),
			("common", true),
			("common/governments_extra", false),
			("common/governments/nested", false),
			("common/govern", false),
		] {
			let replace_paths = vec![game_path(replace_path)];
			assert_eq!(
				super::replace_paths_reset(&replace_paths, policy),
				resets,
				"{replace_path}"
			);
			let mods = vec![ModCandidate {
				entry: PlaysetEntry::default(),
				mod_id: "reset".to_string(),
				root_path: None,
				descriptor_path: None,
				descriptor: Some(ModDescriptor {
					name: "reset".to_string(),
					replace_path: replace_paths,
					..ModDescriptor::default()
				}),
				workshop_identity: None,
				descriptor_error: None,
				files: Vec::new(),
			}];
			let (mod_dag, _) = build_mod_dag(&mods);
			assert_eq!(
				super::module_is_reset_by(
					&ModId("reset".to_string()),
					policy,
					&mod_dag,
					&IgnoreReplacePath::None,
				),
				resets,
				"{replace_path}"
			);
		}
	}

	#[test]
	fn overlay_module_replacement_requires_a_covering_reset_participant() {
		let entry = powerprojection_entry(Some("common/powerprojection"));

		let error = validate_module_target(
			&entry,
			&entry.target.module_outputs()[0].clone(),
			powerprojection_descriptor(),
			false,
			None,
		)
		.expect_err("overlay module cannot replace its namespace without a reset participant");
		assert!(error.contains("covering reset participant: false"));

		validate_module_target(
			&entry,
			&entry.target.module_outputs()[0].clone(),
			powerprojection_descriptor(),
			true,
			None,
		)
		.expect("covering reset participant permits dynamic namespace replacement");
	}

	#[test]
	fn structured_module_views_use_runtime_effective_duplicate_definitions() {
		let descriptor = eu4()
			.classify_content_family(
				crate::model::GamePath::new("common/scripted_triggers/example.txt")
					.expect("valid game path"),
			)
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
			.classify_content_family(
				crate::model::GamePath::new("common/estates_preload/example.txt")
					.expect("valid game path"),
			)
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
	fn reviewed_culture_mapping_requires_the_named_winning_file_and_owner() {
		use crate::game::eu4::cultures::dag::ReviewedCultureMappings;
		use crate::merge::planning::dag::{
			IgnoreReplacePath, ModId, build_mod_dag, induced_file_dag_with_overrides,
		};
		use crate::project::CultureRenameEntry;

		let temp = TempDir::new().unwrap();
		let early = temp.path().join("common/cultures/00_source.txt");
		let late = temp.path().join("common/cultures/zz_patch.txt");
		fs::create_dir_all(early.parent().unwrap()).unwrap();
		let mut files = BTreeMap::new();
		for (ordinal, path) in [&early, &late].into_iter().enumerate() {
			fs::write(path, "g = { new_culture = { male_names = { Otto } } }").unwrap();
			let relative =
				crate::model::GamePathBuf::from_physical(temp.path(), path).expect("game path");
			let parsed = parse_script_file("patch", temp.path(), &relative);
			files.insert(
				parsed.relative_path.clone(),
				VisibleModuleFile {
					layer_ordinal: ordinal,
					parsed,
				},
			);
		}
		let descriptor = eu4()
			.classify_content_family(&game_path("common/cultures/test.txt"))
			.unwrap();
		let ContentLoadPolicy::DefinitionModule(mut policy) = descriptor.load_policy else {
			panic!("culture module")
		};
		policy.duplicate_definitions = DuplicateDefinitionPolicy::LaterDefinitionWins;
		let folded = fold_visible_module_files("patch", "cultures", policy, &files).unwrap();
		let dag = build_mod_dag(&[]).0;
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			policy.output_path,
			&[],
			&IgnoreReplacePath::None,
			&[],
		);
		let mod_id = ModId("patch".into());
		let mut views = super::CrossFileModuleViews {
			aggregate_contributors: vec![],
			file_dag,
			vanilla: None,
			contributors: std::collections::HashMap::from([(mod_id.clone(), folded.parsed)]),
			definition_sources: std::collections::HashMap::from([(mod_id.clone(), folded.sources)]),
		};
		let mut entry = CultureRenameEntry {
			from: "old_culture".into(),
			to: "new_culture".into(),
			mod_id: "patch".into(),
			file: "common/cultures/00_source.txt".into(),
			sha256: "a".repeat(64),
		};
		assert!(
			ReviewedCultureMappings::from_verified_entries(&[entry.clone()], &views).is_err(),
			"overridden source must not authorize the winning target"
		);
		entry.file = "common/cultures/zz_patch.txt".into();
		assert!(ReviewedCultureMappings::from_verified_entries(&[entry.clone()], &views).is_ok());
		views
			.definition_sources
			.get_mut(&mod_id)
			.unwrap()
			.get_mut("g")
			.unwrap()
			.mod_id = "parent".into();
		assert!(
			ReviewedCultureMappings::from_verified_entries(&[entry], &views).is_err(),
			"inherited file must not count as this contributor's reviewed edit"
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
			let relative = crate::model::GamePathBuf::from_physical(temp.path(), path)
				.expect("file under the mod root");
			let parsed = parse_script_file("mod", temp.path(), &relative);
			files.insert(
				parsed.relative_path.clone(),
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
				output_path: crate::model::GamePath::new(
					"common/governments/zzz_foch_governments.txt",
				)
				.expect("valid game path"),
				namespace_prefix: crate::model::GamePath::new("common/governments")
					.expect("valid game path"),
				output_mode: DefinitionModuleOutput::ReplaceNamespace,
				policy_version: 1,
			},
			&files,
		)
		.expect("fold module files");
		let shared = folded
			.parsed
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
				.parsed
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
		let game_path = |physical: &std::path::Path| {
			crate::model::GamePathBuf::from_physical(temp.path(), physical)
				.expect("file under the mod root")
		};
		let earlier = parse_script_file("source", temp.path(), &game_path(&earlier));
		let later = parse_script_file("compatch", temp.path(), &game_path(&later));
		let mut files = BTreeMap::new();
		files.insert(
			earlier.relative_path.clone(),
			VisibleModuleFile {
				layer_ordinal: 1,
				parsed: earlier,
			},
		);
		files.insert(
			later.relative_path.clone(),
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
				output_path: crate::model::GamePath::new(
					"common/governments/zzz_foch_governments.txt",
				)
				.expect("valid game path"),
				namespace_prefix: crate::model::GamePath::new("common/governments")
					.expect("valid game path"),
				output_mode: DefinitionModuleOutput::ReplaceNamespace,
				policy_version: 1,
			},
			&files,
		)
		.expect("fold module files");

		assert!(matches!(
			folded.parsed.ast.statements.as_slice(),
			[AstStatement::Assignment {
				key,
				value: AstValue::Scalar { value, .. },
				..
			}] if key == "shared" && value.as_text() == "compatch"
		));
	}
}
