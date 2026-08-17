#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use foch_core::model::MergeTraceContributor;
use foch_language::analyzer::content_family::MergePolicies;
use foch_language::analyzer::parser::AstStatement;
use foch_language::analyzer::semantic_index::ParsedScriptFile;
use foch_merge_kernel::{DeltaOperation, RevisionNode};

use super::super::conflict_handler::{ConflictHandler, DeferHandler};
use super::dag::{FileDag, ModId};
use super::dag_input::{
	DagMergeInputRequest, merge_ancestor_statements, prepare_dag_merge_input, template_for,
};
use super::dag_pipeline::{DagPipelineResult, execute_dag_pipeline};
use super::definition_trace::compute_definition_participants;
use crate::merge::model::{
	SemanticDeltaPartition, SemanticMergeComputation, SemanticMergeSource, SemanticPartitionId,
	SemanticPartitionLineage, SemanticSourceDelta, VanillaBaseMode,
};
use crate::merge::structured::{
	ClausewitzFileAdapter, ClausewitzFileJoin, DefinitionModuleAdapter, DefinitionModuleJoin,
	EventFileAdapter, EventFileJoin, TreeDagProtocol, TreeDagState, TreeJoinProtocol,
	TreeMergeUnit, TreePartitionAdapter, top_level_assignment_key,
};

#[derive(Clone, Debug)]
pub(crate) struct SemanticDagMergeComputation {
	pub base_statements: Vec<AstStatement>,
	pub merged_statements: Vec<AstStatement>,
	pub semantic: SemanticMergeComputation,
	/// Per top-level definition key → mods whose content is **adopted** into the
	/// final merged output, in ascending DAG-precedence order. Overridden losers
	/// and no-op-vs-base contributors are excluded. Empty unless a mod changed a
	/// key whose content survives in `merged_statements`.
	pub definition_provenance: BTreeMap<String, Vec<String>>,
	/// Per top-level definition key → all non-base mods that directly changed
	/// that key, in DAG level / precedence order. Inherited no-op content is
	/// excluded.
	pub definition_participants: BTreeMap<String, Vec<MergeTraceContributor>>,
}

pub(crate) struct SemanticDagMergeRequest<'a> {
	pub input: DagMergeInputRequest<'a>,
	pub policies: &'a MergePolicies,
	pub vanilla_base_mode: VanillaBaseMode,
}

struct SemanticDagMergeArgs<'a> {
	file_dag: &'a FileDag,
	base_observation: SemanticBaseObservation<'a>,
	contributors: &'a HashMap<ModId, ParsedScriptFile>,
	policies: &'a MergePolicies,
	handler: &'a mut dyn ConflictHandler,
	tree_unit: TreeMergeUnit,
}

#[derive(Clone, Copy, Debug)]
enum SemanticBaseObservation<'a> {
	Present(&'a ParsedScriptFile),
	KnownAbsent,
	ExplicitlyDisabled,
}

impl<'a> SemanticBaseObservation<'a> {
	fn try_from_parts(
		vanilla: Option<&'a ParsedScriptFile>,
		mode: VanillaBaseMode,
	) -> Result<Self, String> {
		match (mode, vanilla) {
			(VanillaBaseMode::Required, Some(vanilla)) => Ok(Self::Present(vanilla)),
			(VanillaBaseMode::KnownAbsent, None) => Ok(Self::KnownAbsent),
			(VanillaBaseMode::ExplicitlyDisabled, None) => Ok(Self::ExplicitlyDisabled),
			(VanillaBaseMode::Required, None) => {
				Err("required vanilla base is missing its parsed file".to_string())
			}
			(VanillaBaseMode::KnownAbsent, Some(_)) => {
				Err("known-absent vanilla base unexpectedly has a parsed file".to_string())
			}
			(VanillaBaseMode::ExplicitlyDisabled, Some(_)) => {
				Err("explicitly disabled vanilla base unexpectedly has a parsed file".to_string())
			}
		}
	}

	const fn vanilla(self) -> Option<&'a ParsedScriptFile> {
		match self {
			Self::Present(vanilla) => Some(vanilla),
			Self::KnownAbsent | Self::ExplicitlyDisabled => None,
		}
	}

	const fn mode(self) -> VanillaBaseMode {
		match self {
			Self::Present(_) => VanillaBaseMode::Required,
			Self::KnownAbsent => VanillaBaseMode::KnownAbsent,
			Self::ExplicitlyDisabled => VanillaBaseMode::ExplicitlyDisabled,
		}
	}
}

pub(crate) fn compute_dag_merge(
	request: SemanticDagMergeRequest<'_>,
) -> Result<SemanticDagMergeComputation, String> {
	let mut handler = DeferHandler;
	compute_dag_merge_with_handler(request, &mut handler)
}

pub(crate) fn compute_dag_merge_with_handler(
	request: SemanticDagMergeRequest<'_>,
	handler: &mut dyn ConflictHandler,
) -> Result<SemanticDagMergeComputation, String> {
	let prepared = prepare_dag_merge_input(request.input)?;
	let base_observation = SemanticBaseObservation::try_from_parts(
		prepared.vanilla.as_ref(),
		request.vanilla_base_mode,
	)?;
	compute_semantic_dag_merge_from_parsed(SemanticDagMergeArgs {
		file_dag: &prepared.file_dag,
		base_observation,
		contributors: &prepared.contributors,
		policies: request.policies,
		handler,
		tree_unit: TreeMergeUnit::File,
	})
}

pub(crate) fn compute_dag_merge_from_parsed(
	file_dag: &FileDag,
	vanilla: Option<&ParsedScriptFile>,
	contributors: &HashMap<ModId, ParsedScriptFile>,
	policies: &MergePolicies,
	vanilla_base_mode: VanillaBaseMode,
	handler: &mut dyn ConflictHandler,
) -> Result<SemanticDagMergeComputation, String> {
	let base_observation = SemanticBaseObservation::try_from_parts(vanilla, vanilla_base_mode)?;
	compute_semantic_dag_merge_from_parsed(SemanticDagMergeArgs {
		file_dag,
		base_observation,
		contributors,
		policies,
		handler,
		tree_unit: inferred_tree_merge_unit(vanilla, contributors),
	})
}

fn inferred_tree_merge_unit(
	vanilla: Option<&ParsedScriptFile>,
	contributors: &HashMap<ModId, ParsedScriptFile>,
) -> TreeMergeUnit {
	let is_event = vanilla
		.into_iter()
		.chain(contributors.values())
		.any(|file| file.file_kind.as_str() == "events");
	if is_event {
		TreeMergeUnit::File
	} else {
		TreeMergeUnit::DefinitionModule
	}
}

fn compute_semantic_dag_merge_from_parsed(
	args: SemanticDagMergeArgs<'_>,
) -> Result<SemanticDagMergeComputation, String> {
	let SemanticDagMergeArgs {
		file_dag,
		base_observation,
		contributors,
		policies,
		handler,
		tree_unit,
	} = args;
	let vanilla = base_observation.vanilla();
	let base_statements = merge_ancestor_statements(vanilla);
	let template = template_for(file_dag, vanilla, contributors);
	let file_adapter = ClausewitzFileAdapter;
	let file_join = ClausewitzFileJoin;
	let event_adapter = EventFileAdapter;
	let event_join = EventFileJoin;
	let module_adapter = DefinitionModuleAdapter;
	let module_join = DefinitionModuleJoin;
	let (partition_adapter, join): (&dyn TreePartitionAdapter, &dyn TreeJoinProtocol) =
		match tree_unit {
			TreeMergeUnit::File
				if template.is_some_and(|file| file.file_kind.as_str() == "events") =>
			{
				(&event_adapter, &event_join)
			}
			TreeMergeUnit::File => (&file_adapter, &file_join),
			TreeMergeUnit::DefinitionModule => (&module_adapter, &module_join),
		};
	let partition_lineage = seed_vanilla_partition_lineage(vanilla, partition_adapter, policies)?;
	let root = TreeDagState {
		statements: base_statements.clone(),
		source_deltas: Vec::new(),
		merge_facts: Vec::new(),
		partition_lineage,
		unresolved_conflicts: Vec::new(),
		handler_resolutions: Vec::new(),
		resolved_conflict_ids: Vec::new(),
		conflict_resolutions: Vec::new(),
		output_directives: Vec::new(),
	};
	let mut protocol = TreeDagProtocol::new(
		partition_adapter,
		join,
		policies,
		vanilla.is_some(),
		base_observation.mode(),
		handler,
	);
	let pipeline = execute_dag_pipeline(file_dag, contributors, root, &mut protocol)?;
	let DagPipelineResult {
		final_state: semantic,
		..
	} = pipeline;
	let merged_statements = semantic.statements.clone();
	let direct_definition_keys = compute_semantic_direct_definition_keys(&semantic.source_deltas)?;
	let definition_provenance = compute_semantic_definition_provenance(&semantic)?;
	let definition_participants =
		compute_definition_participants(&direct_definition_keys, file_dag);
	Ok(SemanticDagMergeComputation {
		base_statements,
		merged_statements,
		semantic,
		definition_provenance,
		definition_participants,
	})
}

fn seed_vanilla_partition_lineage(
	vanilla: Option<&ParsedScriptFile>,
	partition_adapter: &dyn TreePartitionAdapter,
	policies: &MergePolicies,
) -> Result<BTreeMap<SemanticPartitionId, SemanticPartitionLineage>, String> {
	let Some(vanilla) = vanilla else {
		return Ok(BTreeMap::new());
	};
	let mut lineage = BTreeMap::new();
	for partition in partition_adapter.normalization_partitions(&vanilla.ast, &vanilla.ast) {
		let tree = partition_adapter
			.normalize_partition(&vanilla.ast, &partition, policies)
			.map_err(|error| format!("failed to normalize vanilla root lineage: {error}"))?;
		if lineage
			.insert(partition, SemanticPartitionLineage::vanilla(tree))
			.is_some()
		{
			return Err("vanilla root repeated a semantic partition".to_string());
		}
	}
	Ok(lineage)
}

fn compute_semantic_direct_definition_keys(
	source_deltas: &[SemanticSourceDelta],
) -> Result<HashMap<ModId, BTreeSet<String>>, String> {
	let mut direct = HashMap::<ModId, BTreeSet<String>>::new();
	for source_delta in source_deltas {
		let keys = direct_definition_keys_from_source_delta(source_delta)?;
		direct
			.entry(ModId(source_delta.source.source_id.clone()))
			.or_default()
			.extend(keys);
	}
	Ok(direct)
}

fn direct_definition_keys_from_source_delta(
	source_delta: &SemanticSourceDelta,
) -> Result<BTreeSet<String>, String> {
	let mut keys = BTreeSet::new();
	for partition in &source_delta.partitions {
		if partition.delta.operations.is_empty() && partition.delta.ordering.is_empty() {
			continue;
		}
		if let SemanticPartitionId::Definition(key) = &partition.partition {
			keys.insert(key.clone());
			continue;
		}
		for operation in &partition.delta.operations {
			match operation {
				DeltaOperation::Insert { node, .. } => {
					record_semantic_top_level_key(partition, *node, &mut keys)?;
				}
				DeltaOperation::Delete { tombstone } => {
					record_semantic_top_level_key(partition, tombstone.deleted, &mut keys)?;
				}
				DeltaOperation::Update { base, revision }
				| DeltaOperation::Rename { base, revision, .. } => {
					record_semantic_top_level_key(partition, *base, &mut keys)?;
					record_semantic_top_level_key(partition, *revision, &mut keys)?;
				}
				DeltaOperation::Move {
					base,
					revision,
					from_parent,
					to_parent,
				} => {
					record_semantic_top_level_key(partition, *base, &mut keys)?;
					record_semantic_top_level_key(partition, *revision, &mut keys)?;
					for parent in [*from_parent, *to_parent].into_iter().flatten() {
						record_semantic_top_level_key(partition, parent, &mut keys)?;
					}
				}
			}
		}
		for ordering in &partition.delta.ordering {
			for reference in [&ordering.parent, &ordering.before, &ordering.after] {
				record_semantic_top_level_key(partition, reference.source, &mut keys)?;
				if let Some(base) = reference.base {
					record_semantic_top_level_key(partition, base, &mut keys)?;
				}
			}
		}
	}
	Ok(keys)
}

fn record_semantic_top_level_key(
	partition: &SemanticDeltaPartition,
	node: RevisionNode,
	keys: &mut BTreeSet<String>,
) -> Result<(), String> {
	let tree = if node.revision == foch_merge_kernel::RevisionId::BASE {
		&partition.base_tree
	} else if node.revision == partition.delta.revision.revision {
		&partition.revision_tree
	} else {
		return Err(format!(
			"semantic delta references unknown revision {}",
			node.revision.get()
		));
	};
	if let Some(key) = top_level_assignment_key(tree, node.node)
		.map_err(|error| format!("failed to project semantic delta node: {error}"))?
	{
		keys.insert(key.to_string());
	}
	Ok(())
}

fn compute_semantic_definition_provenance(
	semantic: &SemanticMergeComputation,
) -> Result<BTreeMap<String, Vec<String>>, String> {
	let mut by_key = BTreeMap::<String, BTreeSet<SemanticMergeSource>>::new();
	for (partition, lineage) in &semantic.partition_lineage {
		match partition {
			SemanticPartitionId::Definition(key) => {
				let sources = by_key.entry(key.clone()).or_default();
				for node_sources in lineage.sources.values() {
					sources.extend(node_sources.iter().cloned());
				}
			}
			SemanticPartitionId::File => {
				for (node, node_sources) in &lineage.sources {
					let Some(key) =
						top_level_assignment_key(&lineage.tree, *node).map_err(|error| {
							format!("failed to project semantic provenance node: {error}")
						})?
					else {
						continue;
					};
					by_key
						.entry(key.to_string())
						.or_default()
						.extend(node_sources.iter().cloned());
				}
			}
		}
	}
	Ok(by_key
		.into_iter()
		.filter_map(|(key, sources)| {
			let mut sources = sources.into_iter().collect::<Vec<_>>();
			sources.sort_by(|left, right| {
				left.precedence
					.cmp(&right.precedence)
					.then_with(|| left.source_id.cmp(&right.source_id))
			});
			(!sources.is_empty()).then(|| {
				(
					key,
					sources.into_iter().map(|source| source.source_id).collect(),
				)
			})
		})
		.collect())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::{Path, PathBuf};

	use foch_core::config::DepOverride;
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::playlist::PlaylistEntry;
	use foch_core::model::ModCandidate;
	use foch_language::analyzer::content_family::{
		CwtType, GameProfile, ListMergePolicy, MergeKeySource,
	};
	use foch_language::analyzer::parser::AstValue;

	use crate::cache::{DagBaseCache, ModDiffCache};
	use crate::merge::model::SemanticOrigin;
	use crate::merge::patch_engine::dag_merge::{
		ReferenceDagMergeComputation, ReferenceParsedDagMergeRequest,
		compute_reference_dag_merge_from_parsed as compute_reference_dag_merge_from_parsed_reference,
		compute_reference_dag_merge_from_parsed_with_caches,
		direct_definition_contribution_survives, same_key_statements, statement_signature,
	};
	use crate::merge::patch_engine::dag_protocol::{
		DagApplyCacheEvent, DagApplyCacheScope, ReferenceDagCaches, dag_apply_cache_events,
		extend_merge_result, hash_ast_statements, hash_dag_apply_input, pending_after_direct_delta,
		reset_dag_apply_cache_events,
	};
	use crate::merge::patch_engine::patch::{ClausewitzPatch, diff_ast};
	use crate::merge::patch_engine::patch_merge::{PatchMergeResult, PatchResolution};
	use crate::merge::planning::dag::{IgnoreReplacePath, induced_file_dag_with_overrides};
	use crate::merge::planning::dag_join::{ancestry_metrics, reset_ancestry_metrics};
	use crate::workspace::ResolvedFileContributor;

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
			workshop_identity: None,
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn mid(s: &str) -> ModId {
		ModId(s.to_string())
	}

	fn semantic_source(mod_id: &str, precedence: usize) -> SemanticMergeSource {
		SemanticMergeSource {
			source_id: mod_id.to_string(),
			precedence,
		}
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
			mod_hash: Some(format!("hash-{mod_id}")),
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
			file_kind: CwtType::new("other"),
			module_name: "test".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn parsed_event_file(mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("events/test.txt");
		let parsed =
			foch_language::analyzer::parser::parse_clausewitz_content(path.clone(), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("events"),
			module_name: "events".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn parsed_definition_module_file(mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/institutions/zzz_foch_institutions.txt");
		let parsed =
			foch_language::analyzer::parser::parse_clausewitz_content(path.clone(), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("institutions"),
			module_name: "institutions".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn parsed_diplomatic_actions_file(mod_id: &str, source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/diplomatic_actions/zzz_foch_diplomatic_actions.txt");
		let parsed =
			foch_language::analyzer::parser::parse_clausewitz_content(path.clone(), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("diplomatic_actions"),
			module_name: "diplomatic_actions".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn compute_tree_event_join(
		vanilla_source: Option<&str>,
		left_source: &str,
		right_source: &str,
		vanilla_base_mode: VanillaBaseMode,
	) -> Result<SemanticDagMergeComputation, String> {
		let mods = vec![
			mod_with("left", "Left", vec![], vec![]),
			mod_with("right", "Right", vec![], vec![]),
		];
		let contributors = vec![file_contributor("left", 0), file_contributor("right", 1)];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"events/test.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = vanilla_source.map(|source| parsed_event_file("__game__", source));
		let inventory = HashMap::from([
			(mid("left"), parsed_event_file("left", left_source)),
			(mid("right"), parsed_event_file("right", right_source)),
		]);
		let descriptor = foch_language::analyzer::eu4_profile::eu4_profile()
			.classify_content_family(Path::new("events/test.txt"))
			.expect("events content family");
		let mut handler = DeferHandler;
		compute_dag_merge_from_parsed(
			&file_dag,
			vanilla.as_ref(),
			&inventory,
			&descriptor.merge_policies,
			vanilla_base_mode,
			&mut handler,
		)
	}

	fn compute_single_tree_event_join(
		vanilla_source: Option<&str>,
		source: &str,
		vanilla_base_mode: VanillaBaseMode,
	) -> Result<SemanticDagMergeComputation, String> {
		let mods = vec![mod_with("only", "Only", vec![], vec![])];
		let contributors = vec![file_contributor("only", 0)];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"events/test.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = vanilla_source.map(|source| parsed_event_file("__game__", source));
		let inventory = HashMap::from([(mid("only"), parsed_event_file("only", source))]);
		let descriptor = foch_language::analyzer::eu4_profile::eu4_profile()
			.classify_content_family(Path::new("events/test.txt"))
			.expect("events content family");
		let mut handler = DeferHandler;
		compute_dag_merge_from_parsed(
			&file_dag,
			vanilla.as_ref(),
			&inventory,
			&descriptor.merge_policies,
			vanilla_base_mode,
			&mut handler,
		)
	}

	fn compute_tree_definition_module_join(
		vanilla_source: &str,
		left_source: &str,
		right_source: &str,
	) -> Result<SemanticDagMergeComputation, String> {
		let path = "common/institutions/zzz_foch_institutions.txt";
		let mods = vec![
			mod_with("left", "Left", vec![], vec![]),
			mod_with("right", "Right", vec![], vec![]),
		];
		let contributors = vec![file_contributor("left", 0), file_contributor("right", 1)];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			path,
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = parsed_definition_module_file("__game__", vanilla_source);
		let inventory = HashMap::from([
			(
				mid("left"),
				parsed_definition_module_file("left", left_source),
			),
			(
				mid("right"),
				parsed_definition_module_file("right", right_source),
			),
		]);
		let descriptor = foch_language::analyzer::eu4_profile::eu4_profile()
			.classify_content_family(Path::new(path))
			.expect("institutions content family");
		let mut handler = DeferHandler;
		compute_dag_merge_from_parsed(
			&file_dag,
			Some(&vanilla),
			&inventory,
			&descriptor.merge_policies,
			VanillaBaseMode::Required,
			&mut handler,
		)
	}

	fn compute_tree_diplomatic_actions_join(
		vanilla_source: Option<&str>,
		left_source: &str,
		right_source: &str,
		vanilla_base_mode: VanillaBaseMode,
	) -> Result<SemanticDagMergeComputation, String> {
		let path = "common/diplomatic_actions/zzz_foch_diplomatic_actions.txt";
		let mods = vec![
			mod_with("left", "Left", vec![], vec![]),
			mod_with("right", "Right", vec![], vec![]),
		];
		let contributors = vec![file_contributor("left", 10), file_contributor("right", 20)];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			path,
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla =
			vanilla_source.map(|source| parsed_diplomatic_actions_file("__game__", source));
		let inventory = HashMap::from([
			(
				mid("left"),
				parsed_diplomatic_actions_file("left", left_source),
			),
			(
				mid("right"),
				parsed_diplomatic_actions_file("right", right_source),
			),
		]);
		let descriptor = foch_language::analyzer::eu4_profile::eu4_profile()
			.classify_content_family(Path::new(path))
			.expect("diplomatic actions content family");
		let mut handler = DeferHandler;
		compute_dag_merge_from_parsed(
			&file_dag,
			vanilla.as_ref(),
			&inventory,
			&descriptor.merge_policies,
			vanilla_base_mode,
			&mut handler,
		)
	}

	fn condition_origins_for_marker(
		lineage: &SemanticPartitionLineage,
		marker: &str,
	) -> BTreeSet<SemanticOrigin> {
		let marker_node = lineage
			.tree
			.nodes()
			.find_map(|(node, normalized)| {
				(normalized.value.as_deref() == Some(marker)).then_some(node)
			})
			.unwrap_or_else(|| panic!("missing marker {marker}"));
		let mut current = Some(marker_node);
		let condition = loop {
			let node = current.expect("marker must be nested in a condition");
			let normalized = lineage.tree.node(node).expect("normalized marker ancestor");
			if normalized.kind == "clausewitz.assignment:condition"
				&& normalized.value.as_deref() == Some("condition")
			{
				break node;
			}
			current = normalized.parent;
		};
		let mut pending = vec![condition];
		let mut origins = BTreeSet::new();
		while let Some(node) = pending.pop() {
			origins.extend(
				lineage
					.origins
					.get(&node)
					.unwrap_or_else(|| panic!("missing origins for node {}", node.get()))
					.iter()
					.cloned(),
			);
			pending.extend(
				lineage
					.tree
					.node(node)
					.expect("condition subtree node")
					.children
					.iter()
					.copied(),
			);
		}
		origins
	}

	#[test]
	fn vanilla_root_seeding_marks_every_definition_partition_node() {
		let vanilla = parsed_definition_module_file(
			"__game__",
			"shared = { condition = { tooltip = SHARED_TT } }\n\
			shared = { condition = { tooltip = SHARED_TT } }\n",
		);
		let lineage = seed_vanilla_partition_lineage(
			Some(&vanilla),
			&DefinitionModuleAdapter,
			&MergePolicies::default(),
		)
		.expect("seed vanilla definition lineage");
		let partition = lineage
			.get(&SemanticPartitionId::Definition("shared".to_string()))
			.expect("shared definition partition");

		assert_eq!(partition.origins.len(), partition.tree.nodes().count());
		assert!(
			partition
				.origins
				.values()
				.all(|origins| { origins == &BTreeSet::from([SemanticOrigin::Vanilla]) })
		);
	}

	#[test]
	fn diplomatic_conditions_with_the_same_tooltip_keep_node_isolated_origins() {
		let vanilla =
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = vanilla } } }";
		let left = "send_warning = {\n\
			condition = { tooltip = SHARED_TT allow = { marker = vanilla } }\n\
			condition = { tooltip = SHARED_TT allow = { marker = from_left } }\n\
		}";
		let right = "send_warning = {\n\
			condition = { tooltip = SHARED_TT allow = { marker = vanilla } }\n\
			condition = { tooltip = SHARED_TT allow = { marker = from_right } }\n\
		}";
		let result = compute_tree_diplomatic_actions_join(
			Some(vanilla),
			left,
			right,
			VanillaBaseMode::Required,
		)
		.expect("merge source-isolated diplomatic conditions");
		assert!(result.semantic.unresolved_conflicts.is_empty());
		let lineage = result
			.semantic
			.partition_lineage
			.get(&SemanticPartitionId::Definition("send_warning".to_string()))
			.expect("send_warning lineage");

		assert_eq!(
			condition_origins_for_marker(lineage, "vanilla"),
			BTreeSet::from([SemanticOrigin::Vanilla]),
		);
		assert_eq!(
			condition_origins_for_marker(lineage, "from_left"),
			BTreeSet::from([SemanticOrigin::Mod(semantic_source("left", 10))]),
		);
		assert_eq!(
			condition_origins_for_marker(lineage, "from_right"),
			BTreeSet::from([SemanticOrigin::Mod(semantic_source("right", 20))]),
		);
	}

	#[test]
	fn deleted_definition_retains_only_an_empty_lineage_partition() {
		let vanilla = "removed_action = { condition = { tooltip = REMOVED_TT } }\n\
			surviving_action = { condition = { tooltip = SURVIVING_TT allow = { marker = vanilla } } }";
		let surviving = "surviving_action = { condition = { tooltip = SURVIVING_TT allow = { marker = vanilla } } }";
		let result = compute_tree_diplomatic_actions_join(
			Some(vanilla),
			surviving,
			surviving,
			VanillaBaseMode::Required,
		)
		.expect("merge a definition deleted by every surviving source");
		assert!(result.semantic.unresolved_conflicts.is_empty());

		let removed = result
			.semantic
			.partition_lineage
			.get(&SemanticPartitionId::Definition(
				"removed_action".to_string(),
			))
			.expect("deleted definition retains audit lineage");
		let removed_root = removed
			.tree
			.node(removed.tree.root())
			.expect("deleted partition root");
		assert!(removed_root.children.is_empty());

		let surviving = result
			.semantic
			.partition_lineage
			.get(&SemanticPartitionId::Definition(
				"surviving_action".to_string(),
			))
			.expect("surviving definition lineage");
		assert!(
			!surviving
				.tree
				.node(surviving.tree.root())
				.expect("surviving partition root")
				.children
				.is_empty(),
		);
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
	) -> SemanticDagMergeComputation {
		compute_with_overrides(mods, contribs, vanilla_source, inventory, ignore, &[])
	}

	fn compute_reference(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
	) -> ReferenceDagMergeComputation {
		let mut handler = DeferHandler;
		compute_reference_with_merge_key_and_handler(
			mods,
			contribs,
			vanilla_source,
			inventory,
			ignore,
			&[],
			MergeKeySource::AssignmentKey,
			&mut handler,
		)
	}

	fn compute_with_overrides(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
		dep_overrides: &[DepOverride],
	) -> SemanticDagMergeComputation {
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
	) -> SemanticDagMergeComputation {
		let mut handler = DeferHandler;
		compute_with_merge_key_and_handler(
			mods,
			contribs,
			vanilla_source,
			inventory,
			ignore,
			dep_overrides,
			merge_key_source,
			&mut handler,
		)
	}

	fn compute_reference_with_merge_key(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
		dep_overrides: &[DepOverride],
		merge_key_source: MergeKeySource,
	) -> ReferenceDagMergeComputation {
		let mut handler = DeferHandler;
		compute_reference_with_merge_key_and_handler(
			mods,
			contribs,
			vanilla_source,
			inventory,
			ignore,
			dep_overrides,
			merge_key_source,
			&mut handler,
		)
	}

	fn compute_with_policies(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		policies: &MergePolicies,
	) -> SemanticDagMergeComputation {
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(
			diagnostics.is_empty(),
			"unexpected diagnostics: {diagnostics:?}"
		);
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contribs,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		let mut handler = DeferHandler;
		compute_dag_merge_from_parsed(
			&file_dag,
			vanilla.as_ref(),
			&inventory,
			policies,
			VanillaBaseMode::Required,
			&mut handler,
		)
		.expect("compute semantic DAG merge")
	}

	fn compute_reference_with_policies(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		policies: &MergePolicies,
	) -> ReferenceDagMergeComputation {
		let mut handler = DeferHandler;
		compute_reference_with_policies_and_handler(
			mods,
			contribs,
			vanilla_source,
			inventory,
			policies,
			&mut handler,
		)
	}

	fn compute_reference_with_policies_and_handler(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		policies: &MergePolicies,
		handler: &mut dyn ConflictHandler,
	) -> ReferenceDagMergeComputation {
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(
			diagnostics.is_empty(),
			"unexpected diagnostics: {diagnostics:?}"
		);
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contribs,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		let base_statements = merge_ancestor_statements(vanilla.as_ref());
		compute_reference_dag_merge_from_parsed_reference(ReferenceParsedDagMergeRequest {
			file_dag: &file_dag,
			base_statements: &base_statements,
			template: template_for(&file_dag, vanilla.as_ref(), &inventory),
			contributors: &inventory,
			merge_key_source: policies.merge_key_source,
			policies,
			handler,
			mod_hashes: None,
			game_version: "unknown",
		})
		.expect("compute reference DAG merge")
	}

	#[allow(clippy::too_many_arguments)]
	fn compute_with_merge_key_and_handler(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
		dep_overrides: &[DepOverride],
		_merge_key_source: MergeKeySource,
		handler: &mut dyn ConflictHandler,
	) -> SemanticDagMergeComputation {
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(
			diagnostics.is_empty(),
			"unexpected diagnostics: {diagnostics:?}"
		);
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contribs,
			&ignore,
			dep_overrides,
		);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		compute_dag_merge_from_parsed(
			&file_dag,
			vanilla.as_ref(),
			&inventory,
			&MergePolicies::default(),
			VanillaBaseMode::Required,
			handler,
		)
		.expect("compute semantic DAG merge")
	}

	#[allow(clippy::too_many_arguments)]
	fn compute_reference_with_merge_key_and_handler(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		ignore: IgnoreReplacePath,
		dep_overrides: &[DepOverride],
		merge_key_source: MergeKeySource,
		handler: &mut dyn ConflictHandler,
	) -> ReferenceDagMergeComputation {
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(
			diagnostics.is_empty(),
			"unexpected diagnostics: {diagnostics:?}"
		);
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contribs,
			&ignore,
			dep_overrides,
		);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		let base_statements = merge_ancestor_statements(vanilla.as_ref());
		compute_reference_dag_merge_from_parsed_reference(ReferenceParsedDagMergeRequest {
			file_dag: &file_dag,
			base_statements: &base_statements,
			template: template_for(&file_dag, vanilla.as_ref(), &inventory),
			contributors: &inventory,
			merge_key_source,
			policies: &MergePolicies::default(),
			handler,
			mod_hashes: None,
			game_version: "unknown",
		})
		.expect("compute reference DAG merge")
	}

	#[allow(clippy::too_many_arguments)]
	fn compute_with_test_caches(
		mods: Vec<ModCandidate>,
		contribs: Vec<ResolvedFileContributor>,
		vanilla_source: Option<&str>,
		inventory: HashMap<ModId, ParsedScriptFile>,
		mod_hashes: &HashMap<ModId, String>,
		policies: &MergePolicies,
		handler: &mut dyn ConflictHandler,
		diff_cache: &ModDiffCache,
		dag_base_cache: &DagBaseCache,
	) -> ReferenceDagMergeComputation {
		let (dag, diags) = super::super::dag::build_mod_dag(&mods);
		assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
		let fdag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contribs,
			&IgnoreReplacePath::None,
			&[],
		);
		let vanilla = vanilla_source.map(|source| parsed_file("__game__", source));
		let base_statements = merge_ancestor_statements(vanilla.as_ref());
		compute_reference_dag_merge_from_parsed_with_caches(
			ReferenceParsedDagMergeRequest {
				file_dag: &fdag,
				base_statements: &base_statements,
				template: template_for(&fdag, vanilla.as_ref(), &inventory),
				contributors: &inventory,
				merge_key_source: MergeKeySource::AssignmentKey,
				policies,
				handler,
				mod_hashes: Some(mod_hashes),
				game_version: "cache-regression",
			},
			ReferenceDagCaches {
				diff: Some(diff_cache),
				dag_base: Some(dag_base_cache),
			},
		)
		.expect("compute cached DAG patches")
	}

	fn assert_computation_eq(
		expected: &ReferenceDagMergeComputation,
		actual: &ReferenceDagMergeComputation,
	) {
		assert_eq!(actual.mod_patches, expected.mod_patches);
		assert_eq!(actual.base_statements, expected.base_statements);
		assert_eq!(actual.merged_statements, expected.merged_statements);
		assert_eq!(actual.merge_result, expected.merge_result);
		assert_eq!(actual.definition_provenance, expected.definition_provenance);
		assert_eq!(
			actual.definition_participants,
			expected.definition_participants
		);
	}

	#[test]
	fn tree_kernel_runs_at_the_two_sink_event_final_join() {
		let base =
			"country_event = { id = demo.1 title = demo.title option = { name = demo.accept } }\n";
		let left = "country_event = { id = demo.1 title = demo.title trigger = { has_country_flag = left } option = { name = demo.accept } }\n";
		let right = "country_event = { id = demo.1 title = demo.title option = { name = demo.accept } option = { name = demo.reject } }\n";

		let result = compute_tree_event_join(Some(base), left, right, VanillaBaseMode::Required)
			.expect("tree event final join");
		let emitted = crate::emit::emit_clausewitz_statements(&result.merged_statements)
			.expect("emit merged event");

		assert!(
			result.semantic.unresolved_conflicts.is_empty(),
			"{:?}",
			result.semantic.unresolved_conflicts
		);
		assert_eq!(
			emitted,
			"country_event = {\n\
			\tid = demo.1\n\
			\ttitle = demo.title\n\
			\ttrigger = {\n\
			\t\thas_country_flag = left\n\
			\t}\n\
			\toption = {\n\
			\t\tname = demo.accept\n\
			\t}\n\
			\toption = {\n\
			\t\tname = demo.reject\n\
			\t}\n\
			}\n"
		);
	}

	#[test]
	fn tree_kernel_runs_at_the_two_sink_definition_module_final_join() {
		let base = "institution = { can_embrace = { OR = { trade_goods = ivory trade_goods = cloves } } }\n";
		let right =
			"institution = { can_embrace = { OR = { trade_goods = ivory trade_goods = fur } } }\n";

		let result = compute_tree_definition_module_join(base, base, right)
			.expect("tree definition-module final join");
		let emitted = crate::emit::emit_clausewitz_statements(&result.merged_statements)
			.expect("emit merged definition module");

		assert!(
			result.semantic.unresolved_conflicts.is_empty(),
			"{:?}",
			result.semantic.unresolved_conflicts
		);
		for trade_good in ["ivory", "cloves", "fur"] {
			assert!(
				emitted.contains(&format!("trade_goods = {trade_good}")),
				"{emitted}"
			);
		}
	}

	#[test]
	fn tree_kernel_accepts_three_parent_intermediate_join() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec![], vec![]),
			mod_with("join", "Join", vec!["A", "B", "C"], vec![]),
			mod_with("right", "Right", vec![], vec![]),
		];
		let contributors = vec![
			file_contributor("a", 0),
			file_contributor("b", 1),
			file_contributor("c", 2),
			file_contributor("join", 3),
			file_contributor("right", 4),
		];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"events/test.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let base = parsed_event_file(
			"__game__",
			"country_event = { id = demo.1 title = demo.title }\n",
		);
		let inventory = ["a", "b", "c", "join", "right"]
			.into_iter()
			.map(|mod_id| {
				(
					mid(mod_id),
					parsed_event_file(
						mod_id,
						&format!(
							"country_event = {{ id = demo.1 title = demo.title {mod_id} = yes }}\n"
						),
					),
				)
			})
			.collect::<HashMap<_, _>>();
		let descriptor = foch_language::analyzer::eu4_profile::eu4_profile()
			.classify_content_family(Path::new("events/test.txt"))
			.expect("events content family");
		let mut handler = DeferHandler;

		let result = compute_dag_merge_from_parsed(
			&file_dag,
			Some(&base),
			&inventory,
			&descriptor.merge_policies,
			VanillaBaseMode::Required,
			&mut handler,
		)
		.expect("tree kernel folds all three intermediate revisions");
		let emitted = crate::emit::emit_clausewitz_statements(&result.merged_statements)
			.expect("emit tree result");

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert!(emitted.contains("join = yes"), "{emitted}");
		assert!(emitted.contains("right = yes"), "{emitted}");
	}

	#[test]
	fn tree_handler_can_select_any_of_three_original_sources() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec![], vec![]),
		];
		let contributors = vec![
			file_contributor("a", 0),
			file_contributor("b", 1),
			file_contributor("c", 2),
		];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let base = parsed_file("__game__", "value = 0\n");
		let inventory = parsed_inventory(&[
			("a", "value = 1\n"),
			("b", "value = 2\n"),
			("c", "value = 3\n"),
		]);

		for (winner, expected) in [("a", "1"), ("b", "2"), ("c", "3")] {
			let mut handler = PickWinnerHandler { winner, calls: 0 };
			let result = compute_dag_merge_from_parsed(
				&file_dag,
				Some(&base),
				&inventory,
				&MergePolicies::default(),
				VanillaBaseMode::Required,
				&mut handler,
			)
			.expect("resolve tree conflict by original source");

			assert_eq!(handler.calls, 1);
			assert!(result.semantic.unresolved_conflicts.is_empty());
			assert_eq!(
				root_scalar_values(&result.merged_statements, "value"),
				vec![expected]
			);
		}
	}

	#[test]
	fn unresolved_tree_conflict_reaches_manual_resolution_with_all_sources() {
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec![], vec![]),
		];
		let contributors = vec![
			file_contributor("a", 0),
			file_contributor("b", 1),
			file_contributor("c", 2),
		];
		let (dag, diagnostics) = super::super::dag::build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&dag,
			"common/foo.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);
		let base = parsed_file("__game__", "value = 0\n");
		let inventory = parsed_inventory(&[
			("a", "value = 1\n"),
			("b", "value = 2\n"),
			("c", "value = 3\n"),
		]);
		let mut handler = DeferHandler;

		let result = compute_dag_merge_from_parsed(
			&file_dag,
			Some(&base),
			&inventory,
			&MergePolicies::default(),
			VanillaBaseMode::Required,
			&mut handler,
		)
		.expect("retain unresolved tree conflict");

		let semantic = result.semantic;
		let [conflict] = semantic.unresolved_conflicts.as_slice() else {
			panic!(
				"expected one tree conflict, got {:?}",
				semantic.unresolved_conflicts
			);
		};
		assert_eq!(conflict.conflict.semantic_path, vec!["value"]);
		assert_eq!(
			conflict
				.candidates
				.iter()
				.map(|candidate| candidate.source_id.as_str())
				.collect::<Vec<_>>(),
			vec!["a", "b", "c"]
		);
	}

	#[test]
	fn tree_kernel_rejects_an_implicit_empty_base() {
		let source = "country_event = { id = demo.1 title = demo.title }\n";

		let error = compute_single_tree_event_join(None, source, VanillaBaseMode::Required)
			.expect_err("tree merge requires vanilla");

		assert!(error.contains("required vanilla base"), "{error}");
	}

	#[test]
	fn semantic_merge_rejects_known_absent_with_a_parsed_vanilla_file() {
		let source = "country_event = { id = demo.1 title = demo.title }\n";

		let error =
			compute_single_tree_event_join(Some(source), source, VanillaBaseMode::KnownAbsent)
				.expect_err("known-absent state cannot carry vanilla content");

		assert!(error.contains("known-absent vanilla base"), "{error}");
	}

	#[test]
	fn semantic_merge_rejects_disabled_with_a_parsed_vanilla_file() {
		let source = "country_event = { id = demo.1 title = demo.title }\n";

		let error = compute_single_tree_event_join(
			Some(source),
			source,
			VanillaBaseMode::ExplicitlyDisabled,
		)
		.expect_err("disabled state cannot carry vanilla content");

		assert!(
			error.contains("explicitly disabled vanilla base"),
			"{error}"
		);
	}

	#[test]
	fn semantic_merge_allows_required_with_a_parsed_vanilla_file() {
		let source = "country_event = { id = demo.1 title = demo.title }\n";

		let result =
			compute_single_tree_event_join(Some(source), source, VanillaBaseMode::Required)
				.expect("required state with parsed vanilla is valid");

		assert!(!result.merged_statements.is_empty());
	}

	#[test]
	fn tree_kernel_allows_an_explicitly_disabled_vanilla_base() {
		let source = "country_event = { id = demo.1 title = demo.title }\n";

		let result =
			compute_tree_event_join(None, source, source, VanillaBaseMode::ExplicitlyDisabled)
				.expect("explicit empty base is allowed");

		assert!(!result.merged_statements.is_empty());
	}

	#[test]
	fn tree_kernel_preserves_total_origins_for_known_absent_two_mod_join() {
		let source = "country_event = { id = demo.1 title = demo.title }\n";

		let result = compute_tree_event_join(None, source, source, VanillaBaseMode::KnownAbsent)
			.expect("known-absent empty base is allowed");

		assert!(!result.merged_statements.is_empty());
		assert!(!result.semantic.partition_lineage.is_empty());
		for lineage in result.semantic.partition_lineage.values() {
			assert_eq!(
				lineage.origins.len(),
				lineage.tree.nodes().count(),
				"every normalized node, including a synthetic root, must have an origin entry",
			);
		}
	}

	#[test]
	fn cache_identity_hashes_use_full_blake3_digest() {
		let base = parsed_file("base", "root = yes\n");
		let overlay = parsed_file("overlay", "root = yes\nextra = yes\n");
		let patches = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
		let parent_hash = hash_ast_statements(&base.ast.statements).expect("parent-view hash");
		let apply_hash =
			hash_dag_apply_input(&base.ast.statements, &patches).expect("DAG-application hash");

		assert_eq!(parent_hash.len(), blake3::OUT_LEN * 2);
		assert_eq!(apply_hash.len(), blake3::OUT_LEN * 2);
	}

	#[test]
	fn extend_merge_result_aggregates_edit_over_remove_stats() {
		let mut target = PatchMergeResult::default();
		target.stats.edit_over_remove_resolved = 2;
		let mut source = PatchMergeResult::default();
		source.stats.edit_over_remove_resolved = 3;

		extend_merge_result(&mut target, source);

		assert_eq!(target.stats.edit_over_remove_resolved, 5);
	}

	fn patches_for<'a>(
		result: &'a ReferenceDagMergeComputation,
		mod_id: &str,
	) -> &'a Vec<ClausewitzPatch> {
		&result
			.mod_patches
			.iter()
			.find(|(id, _, _)| id == mod_id)
			.unwrap_or_else(|| panic!("missing patches for {mod_id}"))
			.2
	}

	#[derive(Clone, Copy)]
	enum SemanticOperationKind {
		Insert,
		Delete,
		Update,
		Move,
		Rename,
	}

	fn source_delta_for<'a>(
		result: &'a SemanticDagMergeComputation,
		source_id: &str,
	) -> &'a SemanticSourceDelta {
		result
			.semantic
			.source_deltas
			.iter()
			.find(|delta| delta.source.source_id == source_id)
			.unwrap_or_else(|| panic!("missing source delta for {source_id}"))
	}

	fn source_delta_is_empty(result: &SemanticDagMergeComputation, source_id: &str) -> bool {
		source_delta_for(result, source_id)
			.partitions
			.iter()
			.all(|partition| {
				partition.delta.operations.is_empty() && partition.delta.ordering.is_empty()
			})
	}

	fn semantic_operation_keys(
		result: &SemanticDagMergeComputation,
		source_id: &str,
		expected: SemanticOperationKind,
	) -> Vec<String> {
		let mut keys = source_delta_for(result, source_id)
			.partitions
			.iter()
			.flat_map(|partition| {
				partition
					.delta
					.operations
					.iter()
					.filter(move |operation| semantic_operation_is(operation, expected))
					.filter_map(|operation| semantic_operation_key(partition, operation))
			})
			.collect::<Vec<_>>();
		keys.sort();
		keys.dedup();
		keys
	}

	fn semantic_touched_keys(result: &SemanticDagMergeComputation, source_id: &str) -> Vec<String> {
		let mut keys = source_delta_for(result, source_id)
			.partitions
			.iter()
			.flat_map(|partition| {
				partition
					.delta
					.operations
					.iter()
					.filter_map(|operation| semantic_operation_key(partition, operation))
			})
			.collect::<Vec<_>>();
		keys.sort();
		keys.dedup();
		keys
	}

	fn semantic_operation_is(operation: &DeltaOperation, expected: SemanticOperationKind) -> bool {
		matches!(
			(operation, expected),
			(DeltaOperation::Insert { .. }, SemanticOperationKind::Insert)
				| (DeltaOperation::Delete { .. }, SemanticOperationKind::Delete)
				| (DeltaOperation::Update { .. }, SemanticOperationKind::Update)
				| (DeltaOperation::Move { .. }, SemanticOperationKind::Move)
				| (DeltaOperation::Rename { .. }, SemanticOperationKind::Rename)
		)
	}

	fn semantic_operation_key(
		partition: &SemanticDeltaPartition,
		operation: &DeltaOperation,
	) -> Option<String> {
		if let SemanticPartitionId::Definition(key) = &partition.partition {
			return Some(key.clone());
		}
		let (tree, node) = match operation {
			DeltaOperation::Insert { node, .. } => (&partition.revision_tree, node.node),
			DeltaOperation::Delete { tombstone } => (&partition.base_tree, tombstone.deleted.node),
			DeltaOperation::Update { revision, .. }
			| DeltaOperation::Move { revision, .. }
			| DeltaOperation::Rename { revision, .. } => (&partition.revision_tree, revision.node),
		};
		top_level_assignment_key(tree, node)
			.expect("semantic operation node belongs to its partition")
			.map(str::to_string)
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

	trait DagComputationView {
		fn base_statements(&self) -> &[AstStatement];

		fn definition_provenance(&self) -> &BTreeMap<String, Vec<String>>;
	}

	impl DagComputationView for SemanticDagMergeComputation {
		fn base_statements(&self) -> &[AstStatement] {
			&self.base_statements
		}

		fn definition_provenance(&self) -> &BTreeMap<String, Vec<String>> {
			&self.definition_provenance
		}
	}

	impl DagComputationView for ReferenceDagMergeComputation {
		fn base_statements(&self) -> &[AstStatement] {
			&self.base_statements
		}

		fn definition_provenance(&self) -> &BTreeMap<String, Vec<String>> {
			&self.definition_provenance
		}
	}

	fn base_keys(result: &impl DagComputationView) -> Vec<String> {
		let mut keys: Vec<_> = result
			.base_statements()
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
		crate::emit::emit_clausewitz_statements(statements).expect("emit statements")
	}

	fn root_scalar_values(statements: &[AstStatement], expected_key: &str) -> Vec<String> {
		statements
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment {
					key,
					value: AstValue::Scalar { value, .. },
					..
				} if key == expected_key => Some(value.as_text()),
				_ => None,
			})
			.collect()
	}

	struct PickWinnerHandler {
		winner: &'static str,
		calls: usize,
	}

	impl ConflictHandler for PickWinnerHandler {
		fn on_conflict(
			&mut self,
			view: &crate::merge::conflict_view::ConflictView,
		) -> crate::merge::conflict_handler::ConflictDecision {
			self.calls += 1;
			let candidate = view
				.candidates
				.iter()
				.position(|candidate| candidate.mod_id == self.winner)
				.expect("test winner should be a conflict candidate");
			crate::merge::conflict_handler::ConflictDecision::PickCandidate {
				candidate,
				record: None,
			}
		}
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

		assert!(result.semantic.unresolved_conflicts.is_empty());
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

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(
			semantic_operation_keys(&result, "a", SemanticOperationKind::Insert),
			vec!["tag"]
		);
		assert_eq!(
			semantic_operation_keys(&result, "b", SemanticOperationKind::Insert),
			vec!["tag"]
		);
		assert!(semantic_operation_keys(&result, "b", SemanticOperationKind::Delete).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("tag = AAA"));
		assert!(output.contains("tag = BBB"));
	}

	#[test]
	fn sibling_fork_merges_same_base_patches_in_one_level() {
		let result = compute_reference(
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

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(semantic_touched_keys(&result, "b"), vec!["pirate"]);
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

		assert_eq!(
			semantic_operation_keys(&result, "a", SemanticOperationKind::Update),
			vec!["flag"]
		);
		assert!(
			source_delta_is_empty(&result, "b"),
			"independent vanilla-equivalent mod must not remove mod A's changes"
		);
	}

	#[test]
	fn child_delta_against_parent_preserves_independent_root() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\n"),
				("b", "root = yes\nb = yes\n"),
				("c", "root = yes\na = yes\nc = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(
			semantic_operation_keys(&result, "c", SemanticOperationKind::Insert),
			vec!["c"]
		);
		assert!(semantic_operation_keys(&result, "c", SemanticOperationKind::Delete).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("a = yes"), "{output}");
		assert!(output.contains("b = yes"), "{output}");
		assert!(output.contains("c = yes"), "{output}");
	}

	#[test]
	fn child_deletion_relative_to_parent_preserves_independent_root() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\na = yes\nremoved_by_c = yes\n"),
				("b", "root = yes\nb = yes\n"),
				("c", "root = yes\na = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(
			semantic_operation_keys(&result, "c", SemanticOperationKind::Delete),
			vec!["removed_by_c"]
		);
		let output = rendered(&result.merged_statements);
		assert!(output.contains("b = yes"), "{output}");
		assert!(!output.contains("removed_by_c"), "{output}");
	}

	#[test]
	fn unrelated_deeper_branch_conflicts_with_independent_root() {
		let result = compute_reference(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("flag = no\n"),
			parsed_inventory(&[
				("a", "flag = yes\n"),
				("b", "flag = maybe\n"),
				("c", "flag = forced\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(result.merge_result.conflicts.len(), 1);
		let PatchResolution::Conflict { patches, .. } = &result.merge_result.conflicts[0] else {
			panic!("expected cross-branch conflict");
		};
		let mods = patches
			.iter()
			.map(|patch| patch.mod_id.as_str())
			.collect::<BTreeSet<_>>();
		assert_eq!(mods, BTreeSet::from(["b", "c"]));
		assert!(
			!result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|record| record.action == "downstream_override")
		);
	}

	#[test]
	fn child_restores_vanilla_after_caller_resolves_parent_conflict() {
		let mut handler = PickWinnerHandler {
			winner: "a",
			calls: 0,
		};
		let result = compute_with_merge_key_and_handler(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("flag = no\n"),
			parsed_inventory(&[
				("a", "flag = yes\n"),
				("b", "flag = maybe\n"),
				("c", "flag = no\n"),
			]),
			IgnoreReplacePath::None,
			&[],
			MergeKeySource::AssignmentKey,
			&mut handler,
		);

		assert_eq!(handler.calls, 1);
		assert!(
			result.semantic.unresolved_conflicts.is_empty(),
			"{:?}",
			result.semantic.unresolved_conflicts
		);
		assert_eq!(
			semantic_operation_keys(&result, "c", SemanticOperationKind::Update),
			vec!["flag"]
		);
		assert_eq!(rendered(&result.merged_statements), "flag = no\n");
		assert_eq!(prov(&result, "flag"), vec!["c".to_string()]);
	}

	#[test]
	fn multi_parent_join_merges_only_frontiers_after_shared_ancestor() {
		let joined = "flag = forced\nright = yes\njoined = yes\n";
		let mut handler = PickWinnerHandler {
			winner: "b",
			calls: 0,
		};
		let result = compute_with_merge_key_and_handler(
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
			Some("flag = no\n"),
			parsed_inventory(&[
				("a", "flag = yes\n"),
				("b", "flag = forced\n"),
				("c", "flag = yes\nright = yes\n"),
				("d", joined),
			]),
			IgnoreReplacePath::None,
			&[],
			MergeKeySource::AssignmentKey,
			&mut handler,
		);

		assert_eq!(handler.calls, 0, "shared ancestry is not a sibling edit");
		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(
			semantic_operation_keys(&result, "d", SemanticOperationKind::Insert),
			vec!["joined"]
		);
		assert_eq!(rendered(&result.merged_statements), joined);
	}

	#[test]
	fn cached_and_cold_match_when_handler_resolution_changes_parent_view() {
		let temp = tempfile::TempDir::new().expect("temp cache root");
		let diff_cache = ModDiffCache::open(&temp.path().join("diff"));
		let dag_base_cache = DagBaseCache::open(&temp.path().join("dag"));
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
			mod_with("c", "C", vec!["A", "B"], vec![]),
		];
		let contribs = vec![
			file_contributor("a", 1),
			file_contributor("b", 2),
			file_contributor("c", 3),
		];
		let inventory = parsed_inventory(&[
			("a", "flag = yes\n"),
			("b", "flag = maybe\n"),
			("c", "flag = no\n"),
		]);
		let mod_hashes = HashMap::from([
			(mid("a"), "hash-a".to_string()),
			(mid("b"), "hash-b".to_string()),
			(mid("c"), "hash-c".to_string()),
		]);

		let run_cold = |winner| {
			let mut handler = PickWinnerHandler { winner, calls: 0 };
			compute_reference_with_merge_key_and_handler(
				mods.clone(),
				contribs.clone(),
				Some("flag = no\n"),
				inventory.clone(),
				IgnoreReplacePath::None,
				&[],
				MergeKeySource::AssignmentKey,
				&mut handler,
			)
		};
		let run_cached = |winner| {
			let mut handler = PickWinnerHandler { winner, calls: 0 };
			compute_with_test_caches(
				mods.clone(),
				contribs.clone(),
				Some("flag = no\n"),
				inventory.clone(),
				&mod_hashes,
				&MergePolicies::default(),
				&mut handler,
				&diff_cache,
				&dag_base_cache,
			)
		};

		let cold_a = run_cold("a");
		let cache_miss_a = run_cached("a");
		reset_dag_apply_cache_events();
		let cache_hit_a = run_cached("a");
		let branch_events = dag_apply_cache_events()
			.into_iter()
			.filter(|event| event.scope == DagApplyCacheScope::ResolvedBranchState)
			.collect::<Vec<_>>();
		assert_computation_eq(&cold_a, &cache_miss_a);
		assert_computation_eq(&cold_a, &cache_hit_a);
		assert_eq!(
			branch_events,
			vec![DagApplyCacheEvent {
				scope: DagApplyCacheScope::ResolvedBranchState,
				hit: true,
			}],
			"the repeated run must hit the specific multi-parent resolved branch state"
		);

		let cold_b = run_cold("b");
		let cached_b = run_cached("b");
		assert_computation_eq(&cold_b, &cached_b);
		assert_eq!(rendered(&cached_b.merged_statements), "flag = no\n");
		assert_ne!(patches_for(&cache_hit_a, "c"), patches_for(&cached_b, "c"));
	}

	#[test]
	fn resolved_branch_cache_invalidates_when_merge_policy_changes() {
		use foch_language::analyzer::content_family::ScalarMergePolicy;

		let temp = tempfile::TempDir::new().expect("temp cache root");
		let diff_cache = ModDiffCache::open(&temp.path().join("diff"));
		let dag_base_cache = DagBaseCache::open(&temp.path().join("dag"));
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("b", "B", vec![], vec![]),
		];
		let contribs = vec![file_contributor("a", 1), file_contributor("b", 2)];
		let inventory = parsed_inventory(&[
			("a", "root = yes\na = yes\n"),
			("b", "root = yes\nb = yes\n"),
		]);
		let mod_hashes = HashMap::from([
			(mid("a"), "hash-a".to_string()),
			(mid("b"), "hash-b".to_string()),
		]);
		let last_writer = MergePolicies {
			scalar: ScalarMergePolicy::LastWriter,
			..MergePolicies::default()
		};
		let run = |policies: &MergePolicies| {
			let mut handler = DeferHandler;
			compute_with_test_caches(
				mods.clone(),
				contribs.clone(),
				Some("root = yes\n"),
				inventory.clone(),
				&mod_hashes,
				policies,
				&mut handler,
				&diff_cache,
				&dag_base_cache,
			)
		};

		run(&MergePolicies::default());
		reset_dag_apply_cache_events();
		let policy_miss = run(&last_writer);
		let miss_events = dag_apply_cache_events()
			.into_iter()
			.filter(|event| event.scope == DagApplyCacheScope::ResolvedBranchState)
			.collect::<Vec<_>>();
		assert_eq!(miss_events.len(), 1);
		assert!(
			!miss_events[0].hit,
			"changed policy must invalidate branch state"
		);

		reset_dag_apply_cache_events();
		let policy_hit = run(&last_writer);
		let hit_events = dag_apply_cache_events()
			.into_iter()
			.filter(|event| event.scope == DagApplyCacheScope::ResolvedBranchState)
			.collect::<Vec<_>>();
		assert_eq!(
			hit_events,
			vec![DagApplyCacheEvent {
				scope: DagApplyCacheScope::ResolvedBranchState,
				hit: true,
			}]
		);
		assert_computation_eq(&policy_miss, &policy_hit);
	}

	#[test]
	fn cached_child_delta_invalidates_when_only_parent_view_changes() {
		let temp = tempfile::TempDir::new().expect("temp cache root");
		let diff_cache = ModDiffCache::open(&temp.path().join("diff"));
		let dag_base_cache = DagBaseCache::open(&temp.path().join("dag"));
		let mods = vec![
			mod_with("a", "A", vec![], vec![]),
			mod_with("c", "C", vec!["A"], vec![]),
		];
		let contribs = vec![file_contributor("a", 1), file_contributor("c", 2)];
		let hashes_v1 = HashMap::from([
			(mid("a"), "hash-a-v1".to_string()),
			(mid("c"), "hash-c-stable".to_string()),
		]);
		let hashes_v2 = HashMap::from([
			(mid("a"), "hash-a-v2".to_string()),
			(mid("c"), "hash-c-stable".to_string()),
		]);
		let mut warm_handler = DeferHandler;
		let warm = compute_with_test_caches(
			mods.clone(),
			contribs.clone(),
			Some("flag = no\n"),
			parsed_inventory(&[("a", "flag = yes\n"), ("c", "flag = no\n")]),
			&hashes_v1,
			&MergePolicies::default(),
			&mut warm_handler,
			&diff_cache,
			&dag_base_cache,
		);
		assert_eq!(set_value_keys(patches_for(&warm, "c")), vec!["flag"]);

		let inventory_v2 = parsed_inventory(&[("a", "flag = no\n"), ("c", "flag = no\n")]);
		let mut cold_handler = DeferHandler;
		let cold = compute_reference_with_merge_key_and_handler(
			mods.clone(),
			contribs.clone(),
			Some("flag = no\n"),
			inventory_v2.clone(),
			IgnoreReplacePath::None,
			&[],
			MergeKeySource::AssignmentKey,
			&mut cold_handler,
		);
		let mut cached_handler = DeferHandler;
		let cached = compute_with_test_caches(
			mods,
			contribs,
			Some("flag = no\n"),
			inventory_v2,
			&hashes_v2,
			&MergePolicies::default(),
			&mut cached_handler,
			&diff_cache,
			&dag_base_cache,
		);

		assert_computation_eq(&cold, &cached);
		assert!(patches_for(&cached, "c").is_empty());
		assert!(prov(&cached, "flag").is_empty());
	}

	#[test]
	fn gui_named_children_let_sibling_mods_edit_different_widgets() {
		const GUI_CHILD_TYPES: &[&str] = &["windowType"];
		let key_source = MergeKeySource::ContainerChildFieldValue {
			containers: &["guiTypes"],
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

		assert!(result.semantic.unresolved_conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert_eq!(output.matches("x = 1").count(), 1, "{output}");
		assert_eq!(output.matches("x = 2").count(), 1, "{output}");
		assert_eq!(output.matches("x = 0").count(), 0, "{output}");
	}

	#[test]
	fn gui_named_children_conflict_same_widget_sibling_overwrites() {
		const GUI_CHILD_TYPES: &[&str] = &["windowType"];
		let key_source = MergeKeySource::ContainerChildFieldValue {
			containers: &["guiTypes"],
			child_key_field: "name",
			child_types: GUI_CHILD_TYPES,
		};
		let vanilla = r#"
			guiTypes = {
				windowType = { name = "left_widget" position = { x = 0 y = 0 } }
			}
		"#;
		let result = compute_reference_with_merge_key(
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

		assert_eq!(
			semantic_operation_keys(&result, "a", SemanticOperationKind::Insert),
			vec!["a"]
		);
		assert_eq!(
			semantic_operation_keys(&result, "b", SemanticOperationKind::Insert),
			vec!["b"]
		);
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

		assert_eq!(
			semantic_operation_keys(&result, "b", SemanticOperationKind::Insert),
			vec!["b"]
		);
		assert!(semantic_operation_keys(&result, "b", SemanticOperationKind::Delete).is_empty());
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

		assert_eq!(
			semantic_operation_keys(&result, "c", SemanticOperationKind::Insert),
			vec!["c"]
		);
	}

	#[test]
	fn dependency_chain_reads_only_incremental_parent_states() {
		const NODE_COUNT: usize = 16;
		let ids = (0..NODE_COUNT)
			.map(|index| format!("m{index}"))
			.collect::<Vec<_>>();
		let names = (0..NODE_COUNT)
			.map(|index| format!("M{index}"))
			.collect::<Vec<_>>();
		let mods = (0..NODE_COUNT)
			.map(|index| {
				let deps = if index == 0 {
					Vec::new()
				} else {
					vec![names[index - 1].as_str()]
				};
				mod_with(&ids[index], &names[index], deps, vec![])
			})
			.collect::<Vec<_>>();
		let contributors = ids
			.iter()
			.enumerate()
			.map(|(index, mod_id)| file_contributor(mod_id, index + 1))
			.collect::<Vec<_>>();
		let mut source = "root = yes\n".to_string();
		let mut inventory = HashMap::new();
		for (index, mod_id) in ids.iter().enumerate() {
			source.push_str(&format!("key_{index} = yes\n"));
			inventory.insert(mid(mod_id), parsed_file(mod_id, &source));
		}

		let result = compute(
			mods,
			contributors,
			Some("root = yes\n"),
			inventory,
			IgnoreReplacePath::None,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert!(rendered(&result.merged_statements).contains("key_15 = yes"));
		for (index, mod_id) in ids.iter().enumerate() {
			assert_eq!(
				semantic_operation_keys(&result, mod_id, SemanticOperationKind::Insert),
				vec![format!("key_{index}")]
			);
		}
	}

	#[test]
	fn shared_chain_join_keeps_ancestry_storage_and_work_linear() {
		const NODE_COUNT: usize = 64;
		let ids = (0..NODE_COUNT)
			.map(|index| format!("chain_{index}"))
			.collect::<Vec<_>>();
		let names = (0..NODE_COUNT)
			.map(|index| format!("Chain {index}"))
			.collect::<Vec<_>>();
		let mut mods = (0..NODE_COUNT)
			.map(|index| {
				let deps = if index == 0 {
					Vec::new()
				} else {
					vec![names[index - 1].as_str()]
				};
				mod_with(&ids[index], &names[index], deps, vec![])
			})
			.collect::<Vec<_>>();
		mods.push(mod_with(
			"left",
			"Left",
			vec![names[NODE_COUNT - 1].as_str()],
			vec![],
		));
		mods.push(mod_with(
			"right",
			"Right",
			vec![names[NODE_COUNT - 1].as_str()],
			vec![],
		));
		mods.push(mod_with("join", "Join", vec!["Left", "Right"], vec![]));

		let mut contributors = ids
			.iter()
			.enumerate()
			.map(|(index, mod_id)| file_contributor(mod_id, index + 1))
			.collect::<Vec<_>>();
		contributors.push(file_contributor("left", NODE_COUNT + 1));
		contributors.push(file_contributor("right", NODE_COUNT + 2));
		contributors.push(file_contributor("join", NODE_COUNT + 3));

		let mut source = "root = yes\n".to_string();
		let mut inventory = HashMap::new();
		for (index, mod_id) in ids.iter().enumerate() {
			source.push_str(&format!("chain_value_{index} = yes\n"));
			inventory.insert(mid(mod_id), parsed_file(mod_id, &source));
		}
		let left_source = format!("{source}left = yes\n");
		let right_source = format!("{source}right = yes\n");
		let join_source = format!("{source}left = yes\nright = yes\njoin = yes\n");
		inventory.insert(mid("left"), parsed_file("left", &left_source));
		inventory.insert(mid("right"), parsed_file("right", &right_source));
		inventory.insert(mid("join"), parsed_file("join", &join_source));

		reset_ancestry_metrics();
		let result = compute(
			mods,
			contributors,
			Some("root = yes\n"),
			inventory,
			IgnoreReplacePath::None,
		);
		let metrics = ancestry_metrics();

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert!(rendered(&result.merged_statements).contains("join = yes"));
		assert!(
			metrics.work_units <= NODE_COUNT * 5,
			"shared-frontier search visited {} nodes for {NODE_COUNT} shared ancestors",
			metrics.work_units
		);
		assert!(
			metrics.peak_transient_nodes <= NODE_COUNT * 3,
			"shared-frontier search retained {} transient nodes for {NODE_COUNT} shared ancestors",
			metrics.peak_transient_nodes
		);
	}

	#[test]
	fn high_fan_in_join_measures_word_linear_common_frontier_work() {
		const DEPTH: usize = 32;
		const FAN_IN: usize = 129;
		let chain_ids = (0..DEPTH)
			.map(|index| format!("chain_{index}"))
			.collect::<Vec<_>>();
		let chain_names = (0..DEPTH)
			.map(|index| format!("Chain {index}"))
			.collect::<Vec<_>>();
		let branch_ids = (0..FAN_IN)
			.map(|index| format!("branch_{index}"))
			.collect::<Vec<_>>();
		let branch_names = (0..FAN_IN)
			.map(|index| format!("Branch {index}"))
			.collect::<Vec<_>>();

		let mut mods = (0..DEPTH)
			.map(|index| {
				let deps = if index == 0 {
					Vec::new()
				} else {
					vec![chain_names[index - 1].as_str()]
				};
				mod_with(&chain_ids[index], &chain_names[index], deps, vec![])
			})
			.collect::<Vec<_>>();
		for (branch_id, branch_name) in branch_ids.iter().zip(&branch_names) {
			mods.push(mod_with(
				branch_id,
				branch_name,
				vec![chain_names[DEPTH - 1].as_str()],
				vec![],
			));
		}
		mods.push(mod_with(
			"join",
			"Join",
			branch_names.iter().map(String::as_str).collect(),
			vec![],
		));

		let mut contributors = chain_ids
			.iter()
			.chain(&branch_ids)
			.enumerate()
			.map(|(index, mod_id)| file_contributor(mod_id, index + 1))
			.collect::<Vec<_>>();
		contributors.push(file_contributor("join", DEPTH + FAN_IN + 1));

		let mut source = "root = yes\n".to_string();
		let mut inventory = HashMap::new();
		for (index, mod_id) in chain_ids.iter().enumerate() {
			source.push_str(&format!("chain_value_{index} = yes\n"));
			inventory.insert(mid(mod_id), parsed_file(mod_id, &source));
		}
		let mut join_source = source.clone();
		for (index, mod_id) in branch_ids.iter().enumerate() {
			let branch_line = format!("branch_value_{index} = yes\n");
			inventory.insert(
				mid(mod_id),
				parsed_file(mod_id, &format!("{source}{branch_line}")),
			);
			join_source.push_str(&branch_line);
		}
		join_source.push_str("join = yes\n");
		inventory.insert(mid("join"), parsed_file("join", &join_source));

		let run = || {
			compute(
				mods.clone(),
				contributors.clone(),
				Some("root = yes\n"),
				inventory.clone(),
				IgnoreReplacePath::None,
			)
		};
		reset_ancestry_metrics();
		let result = run();
		let metrics = ancestry_metrics();
		reset_ancestry_metrics();
		let repeated = run();
		let repeated_metrics = ancestry_metrics();
		let graph_nodes = DEPTH + FAN_IN;
		let graph_edges = DEPTH - 1 + FAN_IN;
		let coverage_words = FAN_IN.div_ceil(u64::BITS as usize);
		let expected_word_unions = graph_edges * coverage_words;
		let expected_work = graph_nodes + expected_word_unions + DEPTH + (DEPTH - 1);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert!(rendered(&result.merged_statements).contains("join = yes"));
		assert_eq!(
			rendered(&result.merged_statements),
			rendered(&repeated.merged_statements),
			"high-fan-in merge output changed across identical runs"
		);
		assert_eq!(metrics.work_units, expected_work);
		assert_eq!(metrics.coverage_word_unions, expected_word_unions);
		assert_eq!(metrics.work_units, repeated_metrics.work_units);
		assert_eq!(
			metrics.coverage_word_unions,
			repeated_metrics.coverage_word_unions
		);
		assert!(
			metrics.peak_transient_nodes <= graph_nodes * (2 + coverage_words),
			"high-fan-in frontier retained {} transient units for {graph_nodes} nodes and {coverage_words} coverage words",
			metrics.peak_transient_nodes
		);
		for index in 0..FAN_IN {
			assert!(
				rendered(&result.merged_statements)
					.contains(&format!("branch_value_{index} = yes")),
				"missing branch {index} from high-fan-in output"
			);
		}
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

		assert_eq!(
			semantic_operation_keys(&result, "d", SemanticOperationKind::Insert),
			vec!["d"]
		);
		assert!(semantic_operation_keys(&result, "d", SemanticOperationKind::Delete).is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("b = yes"), "{output}");
		assert!(output.contains("c = yes"), "{output}");
		assert!(output.contains("d = yes"), "{output}");
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

		assert_eq!(
			semantic_operation_keys(&result, "c", SemanticOperationKind::Insert),
			vec!["c"]
		);
	}

	#[test]
	fn replace_path_drops_prior_contributors_but_keeps_vanilla_merge_ancestor() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec!["common"]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[("b", "b = yes\n")]),
			IgnoreReplacePath::None,
		);

		assert!(
			result
				.semantic
				.source_deltas
				.iter()
				.all(|delta| delta.source.source_id != "a")
		);
		assert_eq!(
			semantic_operation_keys(&result, "b", SemanticOperationKind::Insert),
			vec!["b"]
		);
		assert_eq!(
			semantic_operation_keys(&result, "b", SemanticOperationKind::Delete),
			vec!["root"]
		);
		assert_eq!(base_keys(&result), vec!["root"]);
		let output = rendered(&result.merged_statements);
		assert_eq!(output, "b = yes\n");
	}

	#[test]
	fn independent_replace_path_owners_merge_against_vanilla_ancestor() {
		let result = compute(
			vec![
				mod_with("left", "Left", vec![], vec!["common"]),
				mod_with("right", "Right", vec![], vec!["common"]),
			],
			vec![file_contributor("left", 1), file_contributor("right", 2)],
			Some("kept = yes\nremoved_by_both = yes\n"),
			parsed_inventory(&[
				("left", "kept = yes\nleft_only = yes\n"),
				("right", "kept = yes\nright_only = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(base_keys(&result), vec!["kept", "removed_by_both"]);
		assert!(result.semantic.unresolved_conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("kept = yes"), "{output}");
		assert!(output.contains("left_only = yes"), "{output}");
		assert!(output.contains("right_only = yes"), "{output}");
		assert!(!output.contains("removed_by_both"), "{output}");
	}

	#[test]
	fn independent_replace_path_layers_treat_shared_vanilla_absence_as_neutral() {
		let result = compute(
			vec![
				mod_with("left", "Left", vec![], vec!["common"]),
				mod_with("right", "Right", vec![], vec!["common"]),
			],
			vec![file_contributor("left", 1), file_contributor("right", 2)],
			Some(
				"shared = { potential = { always = yes } color = { 1 2 3 } value = vanilla }\n\
				 removed_by_both = { value = vanilla }\n",
			),
			parsed_inventory(&[
				("left", "left_only = { value = left }\n"),
				(
					"right",
					"shared = { potential = { always = yes } color = { 4 5 6 } value = right }\n\
					 right_only = { value = right }\n",
				),
			]),
			IgnoreReplacePath::None,
		);

		assert!(
			result.semantic.unresolved_conflicts.is_empty(),
			"shared vanilla absence in the left reset layer must be neutral: {:#?}",
			result.semantic.unresolved_conflicts,
		);
		let output = rendered(&result.merged_statements);
		assert!(output.contains("left_only ="), "{output}");
		assert!(output.contains("right_only ="), "{output}");
		assert!(output.contains("shared ="), "{output}");
		assert!(output.contains("value = right"), "{output}");
		assert!(!output.contains("removed_by_both"), "{output}");
		assert_eq!(prov(&result, "shared"), vec!["right".to_string()]);
		assert_eq!(
			result.definition_participants["shared"]
				.iter()
				.map(|participant| participant.mod_id.as_str())
				.collect::<Vec<_>>(),
			vec!["right"],
		);
	}

	#[test]
	fn non_reset_revision_absence_remains_an_authoritative_deletion() {
		let result = compute(
			vec![
				mod_with("reset", "Reset", vec![], vec!["common"]),
				mod_with("ordinary", "Ordinary", vec![], vec![]),
			],
			vec![
				file_contributor("reset", 1),
				file_contributor("ordinary", 2),
			],
			Some("shared = { value = vanilla }\n"),
			parsed_inventory(&[
				("reset", "shared = { value = reset }\n"),
				("ordinary", "ordinary_only = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		let [conflict] = result.semantic.unresolved_conflicts.as_slice() else {
			panic!(
				"an ordinary revision's deletion must not be neutralized: {:#?}",
				result.semantic.unresolved_conflicts,
			);
		};
		assert_eq!(conflict.conflict.semantic_path, vec!["shared"]);
	}

	#[test]
	fn reset_descendant_branch_absence_remains_an_authoritative_deletion() {
		let result = compute(
			vec![
				mod_with("reset", "Reset", vec![], vec!["common"]),
				mod_with("remover", "Remover", vec!["Reset"], vec![]),
				mod_with("modifier", "Modifier", vec!["Reset"], vec![]),
			],
			vec![
				file_contributor("reset", 1),
				file_contributor("remover", 2),
				file_contributor("modifier", 3),
			],
			Some("shared = { value = vanilla }\n"),
			parsed_inventory(&[
				("reset", "shared = { value = vanilla }\n"),
				("remover", "remover_only = yes\n"),
				(
					"modifier",
					"shared = { value = modified }\nmodifier_only = yes\n",
				),
			]),
			IgnoreReplacePath::None,
		);

		let [conflict] = result.semantic.unresolved_conflicts.as_slice() else {
			panic!(
				"a descendant snapshot's deletion must not become sparse-neutral: {:#?}",
				result.semantic.unresolved_conflicts,
			);
		};
		assert_eq!(conflict.conflict.semantic_path, vec!["shared"]);
	}

	#[test]
	fn independent_replace_path_owners_defer_overlapping_edits() {
		let result = compute(
			vec![
				mod_with("left", "Left", vec![], vec!["common"]),
				mod_with("right", "Right", vec![], vec!["common"]),
			],
			vec![file_contributor("left", 1), file_contributor("right", 2)],
			Some("value = 0\n"),
			parsed_inventory(&[("left", "value = 1\n"), ("right", "value = 2\n")]),
			IgnoreReplacePath::None,
		);

		assert_eq!(base_keys(&result), vec!["value"]);
		let [conflict] = result.semantic.unresolved_conflicts.as_slice() else {
			panic!(
				"expected one overlapping replacement conflict, got {:?}",
				result.semantic.unresolved_conflicts
			);
		};
		assert_eq!(conflict.conflict.semantic_path, vec!["value"]);
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

		assert_eq!(result.semantic.source_deltas.len(), 3);
		assert_eq!(
			semantic_operation_keys(&result, "b", SemanticOperationKind::Insert),
			vec!["b"]
		);
		assert_eq!(base_keys(&result), vec!["root"]);
	}

	#[test]
	fn no_vanilla_file_diffs_each_mod_against_empty() {
		let result = compute_reference(
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
		let output = rendered(&result.merged_statements);
		assert!(output.contains("a = yes"), "{output}");
		assert!(output.contains("b = yes"), "{output}");
	}

	#[test]
	fn resolved_patch_application_is_deterministic_across_repeated_runs() {
		let mut outputs = BTreeSet::new();
		for _ in 0..64 {
			let result = compute_reference(
				vec![
					mod_with("a", "A", vec![], vec![]),
					mod_with("b", "B", vec![], vec![]),
					mod_with("c", "C", vec![], vec![]),
				],
				vec![
					file_contributor("a", 1),
					file_contributor("b", 2),
					file_contributor("c", 3),
				],
				Some("root = yes\n"),
				parsed_inventory(&[
					("a", "root = yes\nz1 = yes\nz2 = yes\nz3 = yes\n"),
					("b", "root = yes\nb1 = yes\nb2 = yes\nb3 = yes\n"),
					("c", "root = yes\nm1 = yes\nm2 = yes\nm3 = yes\n"),
				]),
				IgnoreReplacePath::None,
			);
			assert!(result.merge_result.conflicts.is_empty());
			outputs.insert(rendered(&result.merged_statements));
		}

		assert_eq!(
			outputs.len(),
			1,
			"identical DAG inputs produced divergent output orders: {outputs:#?}"
		);
		assert_eq!(
			outputs.first().expect("one deterministic output"),
			"root = yes\nz1 = yes\nz2 = yes\nz3 = yes\nb1 = yes\nb2 = yes\nb3 = yes\nm1 = yes\nm2 = yes\nm3 = yes\n",
			"resolved inserts are ordered by contributor precedence, then address"
		);
	}

	#[test]
	fn resolved_patch_application_preserves_contributor_local_source_order() {
		let result = compute_reference(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\nz_first = yes\na_second = yes\n"),
				("b", "root = yes\nmiddle_other_mod = yes\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(
			rendered(&result.merged_statements),
			"root = yes\nz_first = yes\na_second = yes\nmiddle_other_mod = yes\n"
		);
	}

	#[test]
	fn downstream_remove_collapses_sibling_set_value_conflict() {
		// a and b are independent siblings that disagree on `flag`; c declares
		// dependencies on both and removes `flag` outright. The upstream
		// disagreement is moot — the post-pass must collapse it.
		let result = compute_reference(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("flag = no\n"),
			parsed_inventory(&[("a", "flag = yes\n"), ("b", "flag = maybe\n"), ("c", "")]),
			IgnoreReplacePath::None,
		);

		assert!(
			result.merge_result.conflicts.is_empty(),
			"downstream RemoveNode should collapse sibling SetValue/SetValue conflict, got {:?}",
			result.merge_result.conflicts
		);
		assert!(
			result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|r| r.action == "downstream_override"),
			"expected a downstream_override handler resolution"
		);
	}

	#[test]
	fn downstream_insert_collapses_sibling_remove_set_conflict() {
		// a removes `flag`, b sets `flag`; c declares deps on both and re-inserts
		// the key fresh. The upstream RemoveNode/SetValue disagreement is moot.
		let result = compute_reference(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("flag = no\n"),
			parsed_inventory(&[("a", ""), ("b", "flag = yes\n"), ("c", "flag = forced\n")]),
			IgnoreReplacePath::None,
		);

		assert!(
			result.merge_result.conflicts.is_empty(),
			"downstream re-insert should collapse sibling RemoveNode/SetValue conflict, got {:?}",
			result.merge_result.conflicts
		);
		assert!(
			result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|r| r.action == "downstream_override"),
			"expected a downstream_override handler resolution"
		);
	}

	#[test]
	fn descendant_append_list_item_collapses_parent_key_conflict() {
		let result = compute_reference(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("tag = ROOT\n"),
			parsed_inventory(&[
				("a", "tag = A\n"),
				("b", ""),
				("c", "tag = ROOT\ntag = CHILD\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(
			result.merge_result.conflicts.is_empty(),
			"child list intent must settle the parent key conflict: {:?}",
			result.merge_result.conflicts
		);
		assert_eq!(append_list_keys(patches_for(&result, "c")), vec!["tag"]);
		assert!(
			result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|record| record.action == "downstream_override")
		);
	}

	#[test]
	fn append_and_remove_list_deltas_overwrite_pending_logical_address() {
		let pending = vec![PatchResolution::Conflict {
			address: crate::merge::patch_merge::PatchAddress {
				path: Vec::new(),
				key: "tag".to_string(),
			},
			patches: Vec::new(),
			reason: "parent disagreement".to_string(),
		}];
		let base = parsed_file("base", "tag = KEEP\ntag = REMOVE\n");
		let appended = parsed_file("append", "tag = KEEP\ntag = REMOVE\ntag = APPENDED\n");
		let removed = parsed_file("remove", "tag = KEEP\n");
		let append_delta = diff_ast(&base, &appended, MergeKeySource::AssignmentKey);
		let remove_delta = diff_ast(&base, &removed, MergeKeySource::AssignmentKey);

		assert!(matches!(
			append_delta.as_slice(),
			[ClausewitzPatch::AppendListItem { key, .. }] if key == "tag"
		));
		assert!(matches!(
			remove_delta.as_slice(),
			[ClausewitzPatch::RemoveListItem { key, .. }] if key == "tag"
		));
		assert!(pending_after_direct_delta(&pending, &append_delta).is_empty());
		assert!(pending_after_direct_delta(&pending, &remove_delta).is_empty());
	}

	#[test]
	fn descendant_remove_list_item_resolves_dag_join_conflict_in_final_output() {
		let result = compute_reference(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("reset", "Reset", vec![], vec![]),
				mod_with("b", "B", vec!["Reset"], vec![]),
				mod_with("c", "C", vec!["A", "B"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("reset", 2),
				file_contributor("b", 3),
				file_contributor("c", 4),
			],
			Some("tag = ROOT\ntag = X\n"),
			parsed_inventory(&[
				("a", "tag = ROOT\n"),
				("reset", "tag = ROOT\n"),
				("b", "tag = ROOT\ntag = X\n"),
				("c", "tag = ROOT\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(remove_list_keys(patches_for(&result, "c")), vec!["tag"]);
		assert!(
			result.merge_result.conflicts.is_empty(),
			"descendant removal must settle the parent append/remove conflict: {:?}",
			result.merge_result.conflicts
		);
		assert_eq!(rendered(&result.merged_statements), "tag = ROOT\n");
		assert!(
			result
				.merge_result
				.handler_resolutions
				.iter()
				.any(|record| record.action == "downstream_override")
		);
	}

	fn prov(result: &impl DagComputationView, key: &str) -> Vec<String> {
		result
			.definition_provenance()
			.get(key)
			.cloned()
			.unwrap_or_default()
	}

	#[test]
	fn provenance_credits_the_single_mod_that_adds_a_block() {
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some("root = yes\n"),
			parsed_inventory(&[("a", "root = yes\nalpha = {\n\tx = 1\n}\n")]),
			IgnoreReplacePath::None,
		);
		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(prov(&result, "alpha"), vec!["a".to_string()]);
		// Unchanged vanilla key gets no provenance entry.
		assert!(prov(&result, "root").is_empty());
	}

	#[test]
	fn provenance_excludes_a_mod_that_ships_a_block_identical_to_vanilla() {
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some("shared = {\n\tx = 1\n}\n"),
			// `a` re-ships `shared` byte-identical and adds `extra`.
			parsed_inventory(&[("a", "shared = {\n\tx = 1\n}\nextra = yes\n")]),
			IgnoreReplacePath::None,
		);
		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert!(
			prov(&result, "shared").is_empty(),
			"no-op-vs-base must not be credited: {:?}",
			result.definition_provenance
		);
		assert_eq!(prov(&result, "extra"), vec!["a".to_string()]);
	}

	#[test]
	fn provenance_credits_vanilla_identical_reintroduction_after_reset() {
		let result = compute(
			vec![mod_with("reset", "Reset", vec![], vec!["common"])],
			vec![file_contributor("reset", 1)],
			Some("shared = {\n\tx = 1\n}\n"),
			parsed_inventory(&[("reset", "shared = {\n\tx = 1\n}\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(prov(&result, "shared"), vec!["reset".to_string()]);
	}

	#[test]
	fn provenance_credits_a_surviving_identical_duplicate_root_definition() {
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some("shared = {\n\tx = 1\n}\n"),
			parsed_inventory(&[("a", "shared = {\n\tx = 1\n}\nshared = {\n\tx = 1\n}\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(
			same_key_statements(&result.merged_statements, "shared").len(),
			2,
			"source_delta={:?}, output={}",
			source_delta_for(&result, "a"),
			rendered(&result.merged_statements),
		);
		assert_eq!(prov(&result, "shared"), vec!["a".to_string()]);
	}

	#[test]
	fn duplicate_root_multiset_records_one_occurrence_update() {
		let parent = r#"shared = { value = A }
			shared = { value = A }
			shared = { value = B }
		"#;
		let child = r#"shared = { value = A }
			shared = { value = B }
			shared = { value = B }
		"#;
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some(parent),
			parsed_inventory(&[("a", child)]),
			IgnoreReplacePath::None,
		);
		let child_parsed = parsed_file("child", child);
		let partition = source_delta_for(&result, "a")
			.partitions
			.iter()
			.find(|partition| {
				partition.partition == SemanticPartitionId::Definition("shared".to_string())
			})
			.expect("shared semantic partition");
		let delete_count = partition
			.delta
			.operations
			.iter()
			.filter(|operation| matches!(operation, DeltaOperation::Delete { .. }))
			.count();
		let insert_count = partition
			.delta
			.operations
			.iter()
			.filter(|operation| matches!(operation, DeltaOperation::Insert { .. }))
			.count();
		let update_count = partition
			.delta
			.operations
			.iter()
			.filter(|operation| matches!(operation, DeltaOperation::Update { .. }))
			.count();
		let actual = same_key_statements(&result.merged_statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();
		let expected = same_key_statements(&child_parsed.ast.statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(delete_count, 0, "{:?}", partition.delta.operations);
		assert_eq!(insert_count, 0, "{:?}", partition.delta.operations);
		assert_eq!(update_count, 1, "{:?}", partition.delta.operations);
		assert_eq!(
			actual,
			expected,
			"output={}",
			rendered(&result.merged_statements)
		);
		assert_eq!(prov(&result, "shared"), vec!["a".to_string()]);
	}

	#[test]
	fn duplicate_root_occurrence_order_preserves_a_b_a() {
		let parent = "shared = { value = A }\n";
		let child = r#"shared = { value = A }
			shared = { value = B }
			shared = { value = A }
		"#;
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some(parent),
			parsed_inventory(&[("a", child)]),
			IgnoreReplacePath::None,
		);
		let child_parsed = parsed_file("child", child);
		let actual = same_key_statements(&result.merged_statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();
		let expected = same_key_statements(&child_parsed.ast.statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(
			actual,
			expected,
			"output={}",
			rendered(&result.merged_statements)
		);
	}

	#[test]
	fn field_value_root_deltas_do_not_duplicate_id_addressed_events() {
		let base = r#"country_event = { id = base.1 marker = base }
		"#;
		let branch_a = r#"country_event = { id = base.1 marker = base }
			country_event = { id = a.1 marker = a }
		"#;
		let branch_b = r#"country_event = { id = base.1 marker = base }
			country_event = { id = b.1 marker = b }
		"#;
		let result = compute_reference_with_merge_key(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(base),
			parsed_inventory(&[("a", branch_a), ("b", branch_b)]),
			IgnoreReplacePath::None,
			&[],
			MergeKeySource::FieldValue("id"),
		);

		for mod_id in ["a", "b"] {
			assert!(
				patches_for(&result, mod_id).iter().all(|patch| !matches!(
					patch,
					ClausewitzPatch::AppendListItem { key, .. }
						| ClausewitzPatch::RemoveListItem { key, .. }
						if key == "country_event"
				)),
				"{mod_id} patches={:?}",
				patches_for(&result, mod_id)
			);
		}
		let output = rendered(&result.merged_statements);
		for id in ["base.1", "a.1", "b.1"] {
			assert_eq!(
				output.matches(&format!("id = {id}")).count(),
				1,
				"id={id}, output={output}, patches={:?}",
				result.mod_patches
			);
		}
	}

	#[test]
	fn event_options_with_unique_names_merge_by_name() {
		let base = r#"country_event = {
			id = test.1
			option = { name = OPTION_A base_effect = yes }
			option = { name = OPTION_B untouched = yes }
		}
		"#;
		let branch_a = r#"country_event = {
			id = test.1
			option = { name = OPTION_A base_effect = yes from_a = yes }
			option = { name = OPTION_B untouched = yes }
		}
		"#;
		let branch_b = r#"country_event = {
			id = test.1
			option = { name = OPTION_A base_effect = yes from_b = yes }
			option = { name = OPTION_B untouched = yes }
		}
		"#;
		let policies = MergePolicies {
			merge_key_source: MergeKeySource::FieldValue("id"),
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "name",
				child_types: &["option"],
			},
			..MergePolicies::default()
		};
		let result = compute_with_policies(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(base),
			parsed_inventory(&[("a", branch_a), ("b", branch_b)]),
			&policies,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert_eq!(output.matches("name = OPTION_A").count(), 1, "{output}");
		assert_eq!(output.matches("name = OPTION_B").count(), 1, "{output}");
		assert_eq!(output.matches("from_a = yes").count(), 1, "{output}");
		assert_eq!(output.matches("from_b = yes").count(), 1, "{output}");
	}

	#[test]
	fn event_options_with_duplicate_names_keep_source_occurrences() {
		let base = "country_event = { id = test.1 }\n";
		let branch = r#"country_event = {
			id = test.1
			option = { name = OPTION_A marker = first }
			option = { name = OPTION_A marker = second }
		}
		"#;
		let policies = MergePolicies {
			merge_key_source: MergeKeySource::FieldValue("id"),
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "name",
				child_types: &["option"],
			},
			..MergePolicies::default()
		};
		let result = compute_with_policies(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some(base),
			parsed_inventory(&[("a", branch)]),
			&policies,
		);

		let output = rendered(&result.merged_statements);
		assert_eq!(output.matches("name = OPTION_A").count(), 2, "{output}");
		assert_eq!(output.matches("marker = first").count(), 1, "{output}");
		assert_eq!(output.matches("marker = second").count(), 1, "{output}");
	}

	#[test]
	fn union_lists_retain_base_items_inside_matched_repeated_blocks() {
		let base = r#"manufactories = {
			embracement_speed = {
				modifier = {
					factor = 0.8
					potential = { OR = { trade_goods = spices trade_goods = cloves } }
					custom_trigger_tooltip = { tooltip = tooltip_tradecompany }
				}
				modifier = {
					factor = 0.2
					potential = { OR = { trade_goods = fur trade_goods = cloves } }
					custom_trigger_tooltip = { tooltip = tooltip_plantations }
				}
			}
		}
		"#;
		let expanded = r#"manufactories = {
			embracement_speed = {
				modifier = {
					factor = 0.8
					potential = { OR = { trade_goods = spices trade_goods = cocoa } }
					custom_trigger_tooltip = { tooltip = tooltip_tradecompany }
				}
				modifier = {
					factor = 0.2
					potential = { OR = { trade_goods = fur trade_goods = cloves } }
					custom_trigger_tooltip = { tooltip = tooltip_plantations }
				}
			}
		}
		"#;
		let companion = base.replace(
			"embracement_speed = {",
			"compatibility_marker = yes\n\t\t\tembracement_speed = {",
		);
		let policies = MergePolicies {
			merge_key_source: MergeKeySource::AssignmentKey,
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "tooltip",
				child_types: &["modifier"],
			},
			list: ListMergePolicy::Union,
			..MergePolicies::default()
		};
		let result = compute_reference_with_policies(
			vec![
				mod_with("expanded", "Expanded", vec![], vec![]),
				mod_with("companion", "Companion", vec![], vec![]),
			],
			vec![
				file_contributor("expanded", 1),
				file_contributor("companion", 2),
			],
			Some(base),
			parsed_inventory(&[("expanded", expanded), ("companion", &companion)]),
			&policies,
		);

		assert!(result.merge_result.conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert_eq!(
			output.matches("tooltip = tooltip_tradecompany").count(),
			1,
			"{output}"
		);
		assert_eq!(
			output.matches("tooltip = tooltip_plantations").count(),
			1,
			"{output}"
		);
		assert_eq!(
			output.matches("trade_goods = cloves").count(),
			2,
			"{output}\npatches={:?}",
			result.mod_patches
		);
		assert_eq!(output.matches("trade_goods = cocoa").count(), 1, "{output}");
		assert_eq!(
			output.matches("compatibility_marker = yes").count(),
			1,
			"{output}"
		);
	}

	fn event_merge_policies() -> MergePolicies {
		MergePolicies {
			merge_key_source: MergeKeySource::FieldValue("id"),
			nested_merge_key_source: MergeKeySource::ChildFieldValue {
				child_key_field: "name",
				child_types: &["option"],
			},
			scalar: foch_language::analyzer::content_family::ScalarMergePolicy::LastWriter,
			list: ListMergePolicy::UnionWithRename,
			edit_wins_over_remove: true,
			..MergePolicies::default()
		}
	}

	#[test]
	fn descendant_edit_preserves_sibling_removed_ancestor_block() {
		let base = "country_event = { id = test.1 after = { base_cleanup = yes } }\n";
		let removed = "country_event = { id = test.1 }\n";
		let extended = r#"country_event = {
			id = test.1
			after = { base_cleanup = yes mod_cleanup = yes }
		}
		"#;
		let result = compute_reference_with_policies(
			vec![
				mod_with("removed", "Removed", vec![], vec![]),
				mod_with("extended", "Extended", vec![], vec![]),
			],
			vec![
				file_contributor("removed", 1),
				file_contributor("extended", 2),
			],
			Some(base),
			parsed_inventory(&[("removed", removed), ("extended", extended)]),
			&event_merge_policies(),
		);

		assert!(result.merge_result.conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("base_cleanup = yes"), "{output}");
		assert!(output.contains("mod_cleanup = yes"), "{output}");
	}

	#[test]
	fn event_descriptions_merge_by_localisation_identity() {
		let base = r#"country_event = {
			id = test.1
			desc = { trigger = { mode = stable } desc = test.1.da }
			desc = { trigger = { mode = base } desc = test.1.db }
		}
		"#;
		let old_model = base.replace("mode = base", "mode = old_model");
		let new_model = base.replace("mode = base", "mode = new_model");
		let result = compute_with_policies(
			vec![
				mod_with("old", "Old", vec![], vec![]),
				mod_with("new", "New", vec![], vec![]),
			],
			vec![file_contributor("old", 1), file_contributor("new", 2)],
			Some(base),
			parsed_inventory(&[("old", &old_model), ("new", &new_model)]),
			&event_merge_policies(),
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert_eq!(output.matches("desc = test.1.db").count(), 1, "{output}");
		assert!(output.contains("mode = new_model"), "{output}");
		assert!(!output.contains("mode = old_model"), "{output}");
	}

	#[test]
	fn event_control_flow_merges_if_branches_without_reviving_replaced_branch() {
		let base = r#"country_event = {
			id = test.1
			option = {
				name = test.1.a
				if = { limit = { has_government_attribute = republican_virtues } define_ruler = { change_adm = 1 } }
				else = { define_ruler = {} }
				if = { limit = { has_country_flag = old_candidate_flag } add_estate_loyalty = 5 }
			}
		}
		"#;
		let ge = base.replace(
			"name = test.1.a",
			"name = test.1.a\n\t\t\t\tge_marker = yes",
		);
		let ee = r#"country_event = {
			id = test.1
			option = {
				name = test.1.a
				if = { limit = { has_country_flag = upgraded_candidate_flag } define_ruler = { change_mil = 1 } }
				else = { define_ruler = {} }
				if = { limit = { has_saved_event_target = spread_target } add_province_modifier = support }
			}
		}
		"#;
		let result = compute_tree_event_join(Some(base), &ge, ee, VanillaBaseMode::Required)
			.expect("tree control-flow join");

		assert!(result.semantic.unresolved_conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert!(
			output.contains("has_government_attribute = republican_virtues"),
			"{output}",
		);
		assert!(
			output.contains("has_country_flag = upgraded_candidate_flag"),
			"{output}"
		);
		assert!(
			output.contains("has_saved_event_target = spread_target"),
			"{output}"
		);
		assert!(
			!output.contains("has_country_flag = old_candidate_flag"),
			"{output}"
		);
	}

	#[test]
	fn descendant_removal_cancels_only_one_identical_ancestor_occurrence() {
		let result = compute(
			vec![
				mod_with("ancestor", "Ancestor", vec![], vec![]),
				mod_with("descendant", "Descendant", vec!["Ancestor"], vec![]),
			],
			vec![
				file_contributor("ancestor", 1),
				file_contributor("descendant", 2),
			],
			Some(""),
			parsed_inventory(&[
				(
					"ancestor",
					"shared = { value = A }\nshared = { value = A }\n",
				),
				("descendant", "shared = { value = A }\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(
			same_key_statements(&result.merged_statements, "shared").len(),
			1,
			"output={}",
			rendered(&result.merged_statements)
		);
		assert_eq!(prov(&result, "shared"), vec!["ancestor".to_string()]);
		assert_eq!(
			result.definition_participants["shared"]
				.iter()
				.map(|participant| participant.mod_id.as_str())
				.collect::<Vec<_>>(),
			vec!["ancestor", "descendant"]
		);
	}

	#[test]
	fn duplicate_root_removal_preserves_a_b_target_order() {
		let parent = r#"shared = { value = A }
			shared = { value = B }
			shared = { value = A }
		"#;
		let child = r#"shared = { value = A }
			shared = { value = B }
		"#;
		let result = compute(
			vec![mod_with("a", "A", vec![], vec![])],
			vec![file_contributor("a", 1)],
			Some(parent),
			parsed_inventory(&[("a", child)]),
			IgnoreReplacePath::None,
		);
		let child_parsed = parsed_file("child", child);
		let actual = same_key_statements(&result.merged_statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();
		let expected = same_key_statements(&child_parsed.ast.statements, "shared")
			.into_iter()
			.map(statement_signature)
			.collect::<Vec<_>>();

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(
			actual,
			expected,
			"output={}",
			rendered(&result.merged_statements)
		);
	}

	#[test]
	fn sibling_list_insertions_use_branch_local_anchors() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("tag = C\n"),
			parsed_inventory(&[("a", "tag = A\ntag = C\n"), ("b", "tag = C\ntag = B\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(
			root_scalar_values(&result.merged_statements, "tag"),
			vec!["A", "C", "B"]
		);
	}

	#[test]
	fn empty_base_insert_and_append_share_logical_cardinality() {
		let result = compute_reference(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(""),
			parsed_inventory(&[("a", "tag = A\n"), ("b", "tag = A\ntag = B\n")]),
			IgnoreReplacePath::None,
		);

		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			root_scalar_values(&result.merged_statements, "tag"),
			vec!["A", "B"]
		);
	}

	#[test]
	fn independent_contributors_union_one_same_key_root() {
		let result = compute_reference(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some(""),
			parsed_inventory(&[
				("a", "shared = {\n\tleft = yes\n}\n"),
				("b", "shared = {\n\tright = yes\n}\n"),
			]),
			IgnoreReplacePath::None,
		);

		let output = rendered(&result.merged_statements);
		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			same_key_statements(&result.merged_statements, "shared").len(),
			1,
			"independent structural edits must not duplicate their logical root: {output}"
		);
		assert!(output.contains("left = yes"), "{output}");
		assert!(output.contains("right = yes"), "{output}");
	}

	#[test]
	fn independent_sprite_types_branches_union_one_container() {
		const SPRITE_TYPES: MergeKeySource = MergeKeySource::ContainerChildFieldValue {
			containers: &["spriteTypes"],
			child_key_field: "name",
			child_types: &["spriteType"],
		};
		let result = compute_reference_with_merge_key(
			vec![
				mod_with("baseline", "Baseline", vec![], vec![]),
				mod_with("a", "A", vec!["Baseline"], vec![]),
				mod_with("b", "B", vec!["Baseline"], vec![]),
			],
			vec![
				file_contributor("baseline", 1),
				file_contributor("a", 2),
				file_contributor("b", 3),
			],
			None,
			parsed_inventory(&[
				(
					"baseline",
					r#"spriteTypes = {
						spriteType = { name = "GFX_anchor" texturefile = "anchor.dds" }
					}"#,
				),
				(
					"a",
					r#"spriteTypes = {
						spriteType = { name = "GFX_left" texturefile = "left.dds" }
					}"#,
				),
				(
					"b",
					r#"spriteTypes = {
						spriteType = { name = "GFX_right" texturefile = "right.dds" }
					}"#,
				),
			]),
			IgnoreReplacePath::None,
			&[],
			SPRITE_TYPES,
		);

		let output = rendered(&result.merged_statements);
		assert!(result.merge_result.conflicts.is_empty());
		assert_eq!(
			same_key_statements(&result.merged_statements, "spriteTypes").len(),
			1,
			"named child union must not duplicate spriteTypes: {output}"
		);
		assert!(output.contains("GFX_left"), "{output}");
		assert!(output.contains("GFX_right"), "{output}");
	}

	#[test]
	fn duplicate_root_survival_uses_signature_multiplicity() {
		let parent = parsed_file("parent", "shared = {\n\tx = 1\n}\n");
		let contributor = parsed_file("a", "shared = {\n\tx = 1\n}\nshared = {\n\tx = 1\n}\n");

		assert!(direct_definition_contribution_survives(
			&parent.ast.statements,
			&contributor.ast.statements,
			&contributor.ast.statements,
			"shared",
		));
	}

	#[test]
	fn repeated_key_provenance_excludes_overridden_direct_item() {
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
			Some("tag = ROOT\ntag = SHARED\n"),
			parsed_inventory(&[
				("a", "tag = ROOT\ntag = SHARED\ntag = A\n"),
				("b", "tag = ROOT\ntag = SHARED\ntag = A\ntag = B\n"),
				("c", "tag = ROOT\ntag = SHARED\ntag = A\ntag = C\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(prov(&result, "tag"), vec!["a".to_string(), "c".to_string()]);
	}

	#[test]
	fn repeated_key_provenance_credits_only_reappend_after_removal() {
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
			Some("tag = ROOT\n"),
			parsed_inventory(&[
				("a", "tag = ROOT\ntag = X\n"),
				("b", "tag = ROOT\n"),
				("c", "tag = ROOT\ntag = X\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert!(result.semantic.unresolved_conflicts.is_empty());
		assert_eq!(rendered(&result.merged_statements), "tag = ROOT\ntag = X\n");
		assert_eq!(prov(&result, "tag"), vec!["c".to_string()]);
	}

	#[test]
	fn provenance_unions_credit_every_contributing_mod() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("block = {\n\ta = 1\n}\n"),
			parsed_inventory(&[
				("a", "block = {\n\ta = 1\n\tb = 2\n}\n"),
				("b", "block = {\n\ta = 1\n\tc = 3\n}\n"),
			]),
			IgnoreReplacePath::None,
		);
		assert!(result.semantic.unresolved_conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert!(
			output.contains("b = 2") && output.contains("c = 3"),
			"{output}"
		);
		assert_eq!(
			prov(&result, "block"),
			vec!["a".to_string(), "b".to_string()]
		);
	}

	#[test]
	fn provenance_composes_original_sources_across_dependency_and_join_partitions() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec![], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
			],
			vec![
				file_contributor("a", 1),
				file_contributor("b", 2),
				file_contributor("c", 3),
			],
			Some("alpha = { left = no right = no }\nbeta = 0\n"),
			parsed_inventory(&[
				("a", "alpha = { left = yes right = no }\nbeta = 0\n"),
				("b", "alpha = { left = no right = yes }\nbeta = 0\n"),
				("c", "alpha = { left = yes right = no }\nbeta = 1\n"),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(
			prov(&result, "alpha"),
			vec!["a".to_string(), "b".to_string()]
		);
		assert_eq!(prov(&result, "beta"), vec!["c".to_string()]);
		assert_eq!(
			result.definition_participants["alpha"]
				.iter()
				.map(|participant| participant.mod_id.as_str())
				.collect::<Vec<_>>(),
			vec!["a", "b"]
		);
		assert_eq!(
			result.definition_participants["beta"]
				.iter()
				.map(|participant| participant.mod_id.as_str())
				.collect::<Vec<_>>(),
			vec!["c"]
		);
	}

	#[test]
	fn provenance_excludes_the_overridden_loser_in_a_dependency_chain() {
		// `b` depends on `a` and replaces `a`'s block with an incompatible body.
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("b", "B", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("b", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\nthing = {\n\tname = \"old\"\n}\n"),
				("b", "root = yes\nthing = {\n\tname = \"new\"\n}\n"),
			]),
			IgnoreReplacePath::None,
		);
		assert!(result.semantic.unresolved_conflicts.is_empty());
		let output = rendered(&result.merged_statements);
		assert!(output.contains("name = \"new\""), "{output}");
		assert!(!output.contains("name = \"old\""), "{output}");
		// Only the adopted winner is credited; `a`'s overridden body is excluded.
		assert_eq!(prov(&result, "thing"), vec!["b".to_string()]);
	}

	#[test]
	fn provenance_and_participants_exclude_inherited_child_content() {
		let result = compute(
			vec![
				mod_with("a", "A", vec![], vec![]),
				mod_with("c", "C", vec!["A"], vec![]),
			],
			vec![file_contributor("a", 1), file_contributor("c", 2)],
			Some("root = yes\n"),
			parsed_inventory(&[
				("a", "root = yes\nowned_by_a = {\n\tx = 1\n}\n"),
				(
					"c",
					"root = yes\nowned_by_a = {\n\tx = 1\n}\nowned_by_c = yes\n",
				),
			]),
			IgnoreReplacePath::None,
		);

		assert_eq!(prov(&result, "owned_by_a"), vec!["a".to_string()]);
		assert_eq!(prov(&result, "owned_by_c"), vec!["c".to_string()]);
		let participants = result
			.definition_participants
			.get("owned_by_a")
			.expect("A's direct definition is tracked");
		assert_eq!(participants.len(), 1);
		assert_eq!(participants[0].mod_id, "a");
		assert!(
			!result.definition_participants.contains_key("root"),
			"unchanged inherited/base content is not a direct participant"
		);
	}
}
