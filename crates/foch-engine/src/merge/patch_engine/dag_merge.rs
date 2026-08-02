//! Address-patch reference merge over a neutral per-file DAG.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use foch_core::model::MergeTraceContributor;
use foch_language::analyzer::content_family::{MergeKeySource, MergePolicies};
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::ParsedScriptFile;

use crate::merge::planning::dag::{FileDag, ModId};
use crate::merge::planning::dag_input::{
	DagMergeInputRequest, contributor_mod_hashes, final_base_statements, prepare_dag_merge_input,
	template_for,
};
use crate::merge::planning::dag_join::sink_mods;
use crate::merge::planning::definition_trace::compute_definition_participants;
use crate::merge::resolution::conflict_handler::ConflictHandler;

use super::dag_protocol::{
	ReferenceDagCaches, ReferenceDagProtocolOutput, ReferenceDagProtocolRequest,
	execute_reference_dag, execute_reference_dag_with_caches,
};
use super::patch::ClausewitzPatch;
use super::patch_merge::{PatchMergeResult, semantic_statement_identity, semantic_value_identity};

#[derive(Clone, Debug)]
pub(crate) struct ReferenceDagMergeComputation {
	pub mod_patches: Vec<(String, usize, Vec<ClausewitzPatch>)>,
	#[cfg(test)]
	pub base_statements: Vec<AstStatement>,
	pub merged_statements: Vec<AstStatement>,
	pub merge_result: PatchMergeResult,
	pub definition_provenance: BTreeMap<String, Vec<String>>,
	pub definition_participants: BTreeMap<String, Vec<MergeTraceContributor>>,
}

pub(crate) struct ReferenceDagMergeRequest<'a> {
	pub input: DagMergeInputRequest<'a>,
	pub merge_key_source: MergeKeySource,
	pub policies: &'a MergePolicies,
	pub game_version: &'a str,
}

pub(crate) struct ReferenceParsedDagMergeRequest<'a> {
	pub file_dag: &'a FileDag,
	pub base_statements: &'a [AstStatement],
	pub template: Option<&'a ParsedScriptFile>,
	pub contributors: &'a HashMap<ModId, ParsedScriptFile>,
	pub merge_key_source: MergeKeySource,
	pub policies: &'a MergePolicies,
	pub handler: &'a mut dyn ConflictHandler,
	pub mod_hashes: Option<&'a HashMap<ModId, String>>,
	pub game_version: &'a str,
}

pub(crate) fn compute_reference_dag_merge(
	request: ReferenceDagMergeRequest<'_>,
	handler: &mut dyn ConflictHandler,
) -> Result<ReferenceDagMergeComputation, String> {
	let contributors = request.input.contributors;
	let prepared = prepare_dag_merge_input(request.input)?;
	let mod_hashes = contributor_mod_hashes(contributors, &prepared.file_dag);
	let base_statements = final_base_statements(&prepared.file_dag, prepared.vanilla.as_ref());
	compute_reference_dag_merge_from_parsed(ReferenceParsedDagMergeRequest {
		file_dag: &prepared.file_dag,
		base_statements: &base_statements,
		template: template_for(
			&prepared.file_dag,
			prepared.vanilla.as_ref(),
			&prepared.contributors,
		),
		contributors: &prepared.contributors,
		merge_key_source: request.merge_key_source,
		policies: request.policies,
		handler,
		mod_hashes: Some(&mod_hashes),
		game_version: request.game_version,
	})
}

pub(crate) fn compute_reference_dag_merge_from_parsed(
	request: ReferenceParsedDagMergeRequest<'_>,
) -> Result<ReferenceDagMergeComputation, String> {
	compute_reference_dag_merge_inner(request, None)
}

#[cfg(test)]
pub(crate) fn compute_reference_dag_merge_from_parsed_with_caches(
	request: ReferenceParsedDagMergeRequest<'_>,
	caches: ReferenceDagCaches<'_>,
) -> Result<ReferenceDagMergeComputation, String> {
	compute_reference_dag_merge_inner(request, Some(caches))
}

fn compute_reference_dag_merge_inner(
	request: ReferenceParsedDagMergeRequest<'_>,
	caches: Option<ReferenceDagCaches<'_>>,
) -> Result<ReferenceDagMergeComputation, String> {
	let ReferenceParsedDagMergeRequest {
		file_dag,
		base_statements,
		template,
		contributors,
		merge_key_source,
		policies,
		handler,
		mod_hashes,
		game_version,
	} = request;
	let protocol_request = ReferenceDagProtocolRequest {
		file_dag,
		base_statements,
		template,
		contributors,
		merge_key_source,
		policies,
		handler,
		mod_hashes,
		game_version,
	};
	let ReferenceDagProtocolOutput {
		mod_patches,
		merged_statements,
		merge_result,
		parent_statements,
	} = match caches {
		Some(caches) => execute_reference_dag_with_caches(protocol_request, caches)?,
		None => execute_reference_dag(protocol_request)?,
	};
	let direct_definition_keys = compute_direct_definition_keys(&mod_patches);
	let definition_provenance = compute_definition_provenance(
		&merged_statements,
		contributors,
		file_dag,
		&mod_patches,
		&direct_definition_keys,
		&parent_statements,
	);
	let definition_participants =
		compute_definition_participants(&direct_definition_keys, file_dag);
	Ok(ReferenceDagMergeComputation {
		mod_patches,
		#[cfg(test)]
		base_statements: base_statements.to_vec(),
		merged_statements,
		merge_result,
		definition_provenance,
		definition_participants,
	})
}

pub(crate) fn statement_signature(statement: &AstStatement) -> String {
	semantic_statement_identity(statement)
}

fn block_child_signatures(statement: &AstStatement) -> BTreeSet<String> {
	match statement {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => items.iter().map(statement_signature).collect(),
		_ => BTreeSet::new(),
	}
}

pub(crate) fn same_key_statements<'a>(
	statements: &'a [AstStatement],
	key: &str,
) -> Vec<&'a AstStatement> {
	statements
		.iter()
		.filter(|statement| {
			matches!(statement, AstStatement::Assignment { key: candidate, .. } if candidate == key)
		})
		.collect()
}

fn whole_signature_counts(statements: &[&AstStatement]) -> BTreeMap<String, usize> {
	let mut counts = BTreeMap::new();
	for statement in statements {
		*counts.entry(statement_signature(statement)).or_default() += 1;
	}
	counts
}

fn union_child_signatures(statements: &[&AstStatement]) -> BTreeSet<String> {
	statements
		.iter()
		.flat_map(|statement| block_child_signatures(statement))
		.collect()
}

pub(crate) fn direct_definition_contribution_survives(
	parent_statements: &[AstStatement],
	mod_statements: &[AstStatement],
	merged_statements: &[AstStatement],
	key: &str,
) -> bool {
	let parent_blocks = same_key_statements(parent_statements, key);
	let mod_blocks = same_key_statements(mod_statements, key);
	let final_blocks = same_key_statements(merged_statements, key);
	let parent_whole = whole_signature_counts(&parent_blocks);
	let mod_whole = whole_signature_counts(&mod_blocks);
	let final_whole = whole_signature_counts(&final_blocks);

	if parent_blocks.len() > 1 || mod_blocks.len() > 1 || final_blocks.len() > 1 {
		let added_survives = mod_whole.iter().any(|(signature, mod_count)| {
			let parent_count = parent_whole.get(signature).copied().unwrap_or_default();
			let final_count = final_whole.get(signature).copied().unwrap_or_default();
			*mod_count > parent_count && final_count > parent_count
		});
		if added_survives {
			return true;
		}
		return parent_whole.iter().any(|(signature, parent_count)| {
			let mod_count = mod_whole.get(signature).copied().unwrap_or_default();
			let final_count = final_whole.get(signature).copied().unwrap_or_default();
			mod_count < *parent_count && final_count < *parent_count
		});
	}

	if mod_whole
		.keys()
		.any(|signature| final_whole.contains_key(signature))
	{
		return true;
	}
	let parent_children = union_child_signatures(&parent_blocks);
	let mod_children = union_child_signatures(&mod_blocks);
	let final_children = union_child_signatures(&final_blocks);
	let added_children = mod_children
		.difference(&parent_children)
		.cloned()
		.collect::<BTreeSet<_>>();
	added_children
		.intersection(&final_children)
		.next()
		.is_some()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootListOperationKind {
	Append,
	Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RootListOperation {
	key: String,
	value_identity: String,
	occurrence: usize,
	kind: RootListOperationKind,
}

struct RootListHistory {
	operations: HashMap<ModId, Vec<RootListOperation>>,
	children: HashMap<ModId, Vec<ModId>>,
	sinks: BTreeSet<ModId>,
}

impl RootListHistory {
	fn new(mod_patches: &[(String, usize, Vec<ClausewitzPatch>)], file_dag: &FileDag) -> Self {
		let operations = mod_patches
			.iter()
			.map(|(mod_id, _, patches)| {
				let operations = patches
					.iter()
					.filter_map(|patch| match patch {
						ClausewitzPatch::AppendListItem {
							path,
							key,
							value,
							target_occurrence,
						} if path.is_empty() => Some(RootListOperation {
							key: key.clone(),
							value_identity: semantic_value_identity(value),
							occurrence: target_occurrence.identity_ordinal(),
							kind: RootListOperationKind::Append,
						}),
						ClausewitzPatch::RemoveListItem {
							path,
							key,
							value,
							source_occurrence,
						} if path.is_empty() => Some(RootListOperation {
							key: key.clone(),
							value_identity: semantic_value_identity(value),
							occurrence: source_occurrence.identity_ordinal(),
							kind: RootListOperationKind::Remove,
						}),
						_ => None,
					})
					.collect::<Vec<_>>();
				(ModId(mod_id.clone()), operations)
			})
			.collect::<HashMap<_, _>>();

		let mut children = file_dag
			.contributors()
			.iter()
			.cloned()
			.map(|mod_id| (mod_id, Vec::new()))
			.collect::<HashMap<_, _>>();
		for child in file_dag.contributors() {
			for parent in file_dag.parents_of(child) {
				children
					.entry(parent.clone())
					.or_default()
					.push(child.clone());
			}
		}
		for descendants in children.values_mut() {
			descendants.sort_by(|left, right| {
				file_dag
					.precedence_of(left)
					.cmp(&file_dag.precedence_of(right))
					.then_with(|| left.cmp(right))
			});
			descendants.dedup();
		}

		Self {
			operations,
			children,
			sinks: sink_mods(file_dag).into_iter().collect(),
		}
	}

	fn surviving_direct_intent(&self, mod_id: &ModId, key: &str) -> Option<bool> {
		let direct = self
			.operations
			.get(mod_id)?
			.iter()
			.filter(|operation| operation.key == key)
			.collect::<Vec<_>>();
		if direct.is_empty() {
			return None;
		}
		Some(
			direct
				.into_iter()
				.any(|operation| self.intent_reaches_final_sink(mod_id, operation)),
		)
	}

	fn intent_reaches_final_sink(&self, origin: &ModId, intent: &RootListOperation) -> bool {
		let opposing = match intent.kind {
			RootListOperationKind::Append => RootListOperationKind::Remove,
			RootListOperationKind::Remove => RootListOperationKind::Append,
		};
		let mut stack = vec![origin.clone()];
		let mut visited = HashSet::new();
		while let Some(candidate) = stack.pop() {
			if !visited.insert(candidate.clone()) {
				continue;
			}
			if candidate != *origin
				&& self.operations.get(&candidate).is_some_and(|operations| {
					operations.iter().any(|operation| {
						operation.key == intent.key
							&& operation.value_identity == intent.value_identity
							&& operation.occurrence == intent.occurrence
							&& operation.kind == opposing
					})
				}) {
				continue;
			}
			if self.sinks.contains(&candidate) {
				return true;
			}
			if let Some(children) = self.children.get(&candidate) {
				stack.extend(children.iter().rev().cloned());
			}
		}
		false
	}
}

fn compute_direct_definition_keys(
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
) -> HashMap<ModId, BTreeSet<String>> {
	mod_patches
		.iter()
		.map(|(mod_id, _, patches)| {
			let keys = patches
				.iter()
				.flat_map(patch_top_level_keys)
				.collect::<BTreeSet<_>>();
			(ModId(mod_id.clone()), keys)
		})
		.collect()
}

fn patch_top_level_keys(patch: &ClausewitzPatch) -> Vec<String> {
	match patch {
		ClausewitzPatch::SetValue { path, key, .. }
		| ClausewitzPatch::RemoveNode { path, key, .. }
		| ClausewitzPatch::InsertNode { path, key, .. }
		| ClausewitzPatch::AppendListItem { path, key, .. }
		| ClausewitzPatch::RemoveListItem { path, key, .. }
		| ClausewitzPatch::ReplaceBlock { path, key, .. } => {
			vec![path.first().cloned().unwrap_or_else(|| key.clone())]
		}
		ClausewitzPatch::AppendBlockItem { path, .. }
		| ClausewitzPatch::RemoveBlockItem { path, .. } => path.first().cloned().into_iter().collect(),
		ClausewitzPatch::Rename {
			path,
			old_key,
			new_key,
		} => match path.first() {
			Some(top_level) => vec![top_level.clone()],
			None => vec![old_key.clone(), new_key.clone()],
		},
	}
}

fn compute_definition_provenance(
	merged_statements: &[AstStatement],
	contributors: &HashMap<ModId, ParsedScriptFile>,
	file_dag: &FileDag,
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
	direct_definition_keys: &HashMap<ModId, BTreeSet<String>>,
	parent_statements: &HashMap<ModId, Vec<AstStatement>>,
) -> BTreeMap<String, Vec<String>> {
	let mut ordered = file_dag.contributors().to_vec();
	ordered.sort_by_key(|mod_id| file_dag.precedence_of(mod_id));

	let keys = merged_statements
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment { key, .. } => Some(key.as_str()),
			_ => None,
		})
		.collect::<BTreeSet<_>>();

	let list_history = RootListHistory::new(mod_patches, file_dag);
	let mut provenance = BTreeMap::new();
	for key in keys {
		let mut adopted = Vec::new();
		for mod_id in &ordered {
			if !direct_definition_keys
				.get(mod_id)
				.is_some_and(|keys| keys.contains(key))
			{
				continue;
			}
			let Some(parsed) = contributors.get(mod_id) else {
				continue;
			};
			let Some(parent) = parent_statements.get(mod_id) else {
				continue;
			};
			let direct_survives = direct_definition_contribution_survives(
				parent,
				&parsed.ast.statements,
				merged_statements,
				key,
			);
			let survives = list_history
				.surviving_direct_intent(mod_id, key)
				.map_or(direct_survives, |reaches_sink| {
					reaches_sink && direct_survives
				});
			if survives {
				adopted.push(mod_id.0.clone());
			}
		}
		if !adopted.is_empty() {
			provenance.insert(key.to_string(), adopted);
		}
	}
	provenance
}
