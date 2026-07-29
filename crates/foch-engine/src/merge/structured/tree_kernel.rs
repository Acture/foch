use std::collections::BTreeMap;
use std::path::PathBuf;

use foch_core::config::compute_conflict_id;
use foch_core::model::HandlerResolutionRecord;
use foch_language::analyzer::content_family::MergePolicies;
use foch_language::analyzer::parser::{AstFile, AstStatement};

use crate::emit::emit_clausewitz_statements;
use crate::merge::conflict_handler::{ConflictDecision, ConflictHandler};
use crate::merge::conflict_view::{CandidateView, ConflictView};
use crate::merge::kernel::{KernelMergeInput, KernelRevision};
use crate::merge::planning::dag::{FileDag, ModId};
use crate::merge::planning::dag_join::DagJoinScope;
use crate::merge::planning::dag_pipeline::{
	DagJoinProtocol, DagJoinRequest, DagJoinRevision, EffectiveNodeProtocol, EffectiveNodeRequest,
};

use super::ast_adapter::{denormalize_statement, normalize_ast};
use super::policy::{ContentFamilyMergePolicy, LocalSourceSelections};
use super::trivia::detach_trivia;
use super::{
	ClausewitzConflictSummary, apply_nary_scalar_reducers,
	merge_clausewitz_definition_module_with_source_selections,
	merge_clausewitz_files_with_source_selections, merge_event_files_with_source_selections,
};

pub(crate) type TreeSourceSelections = BTreeMap<Vec<String>, String>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeConflictCandidate {
	pub source_id: String,
	pub precedence: usize,
	pub statement: Option<AstStatement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeConflictRecord {
	pub address_path: Vec<String>,
	pub address_key: String,
	pub reason: String,
	pub base_statement: Option<AstStatement>,
	pub candidates: Vec<TreeConflictCandidate>,
	pub source_selectable: bool,
}

pub(crate) trait TreeMergeAdapter {
	fn name(&self) -> &'static str;

	fn merge_three_way(
		&self,
		base: &AstFile,
		left: &AstFile,
		right: &AstFile,
		policies: &MergePolicies,
		source_selections: &LocalSourceSelections,
	) -> Result<TreeMergeStep, String>;
}

#[derive(Clone, Debug)]
pub(crate) struct TreeMergeStep {
	pub statements: Vec<AstStatement>,
	pub conflicts: Vec<ClausewitzConflictSummary>,
}

#[derive(Clone, Debug)]
pub(crate) struct TreeKernelOutcome {
	pub statements: Vec<AstStatement>,
	pub conflicts: Vec<ClausewitzConflictSummary>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TreeMergeUnit {
	#[default]
	File,
	DefinitionModule,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ClausewitzFileAdapter;

impl TreeMergeAdapter for ClausewitzFileAdapter {
	fn name(&self) -> &'static str {
		"clausewitz-file"
	}

	fn merge_three_way(
		&self,
		base: &AstFile,
		left: &AstFile,
		right: &AstFile,
		policies: &MergePolicies,
		source_selections: &LocalSourceSelections,
	) -> Result<TreeMergeStep, String> {
		let outcome = merge_clausewitz_files_with_source_selections(
			base,
			left,
			right,
			policies,
			source_selections,
		)
		.map_err(|error| format!("tree merge adapter failed: {error}"))?;
		Ok(clausewitz_step(outcome))
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EventFileAdapter;

impl TreeMergeAdapter for EventFileAdapter {
	fn name(&self) -> &'static str {
		"event-file"
	}

	fn merge_three_way(
		&self,
		base: &AstFile,
		left: &AstFile,
		right: &AstFile,
		policies: &MergePolicies,
		source_selections: &LocalSourceSelections,
	) -> Result<TreeMergeStep, String> {
		let outcome = merge_event_files_with_source_selections(
			base,
			left,
			right,
			policies,
			source_selections,
		)
		.map_err(|error| format!("event tree adapter failed: {error}"))?;
		Ok(clausewitz_step(outcome))
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DefinitionModuleAdapter;

impl TreeMergeAdapter for DefinitionModuleAdapter {
	fn name(&self) -> &'static str {
		"definition-module"
	}

	fn merge_three_way(
		&self,
		base: &AstFile,
		left: &AstFile,
		right: &AstFile,
		policies: &MergePolicies,
		source_selections: &LocalSourceSelections,
	) -> Result<TreeMergeStep, String> {
		let outcome = merge_clausewitz_definition_module_with_source_selections(
			base,
			left,
			right,
			policies,
			source_selections,
		)
		.map_err(|error| format!("definition-module tree adapter failed: {error}"))?;
		eprintln!(
			"[tree-module] join {} base_definitions={} active_definitions={} copy_through_definitions={} tree_definitions={}",
			base.path.display(),
			outcome.base_definitions(),
			outcome.active_definitions(),
			outcome.copy_through_definitions(),
			outcome.structured_definitions(),
		);
		Ok(TreeMergeStep {
			statements: outcome.tentative_ast().statements.clone(),
			conflicts: outcome.conflicts().to_vec(),
		})
	}
}

fn clausewitz_step(outcome: super::ClausewitzMergeOutcome) -> TreeMergeStep {
	TreeMergeStep {
		statements: outcome.tentative_ast().statements.clone(),
		conflicts: outcome.conflict_summaries(),
	}
}

pub(crate) struct TreeMergeKernel<'a> {
	adapter: &'a dyn TreeMergeAdapter,
	policies: &'a MergePolicies,
}

impl<'a> TreeMergeKernel<'a> {
	pub(crate) const fn new(
		adapter: &'a dyn TreeMergeAdapter,
		policies: &'a MergePolicies,
	) -> Self {
		Self { adapter, policies }
	}

	#[cfg(test)]
	pub(crate) fn merge(&self, input: &KernelMergeInput) -> Result<Vec<AstStatement>, String> {
		self.merge_with_source_selections(input, &TreeSourceSelections::new())
	}

	#[cfg(test)]
	pub(crate) fn merge_with_source_selections(
		&self,
		input: &KernelMergeInput,
		source_selections: &TreeSourceSelections,
	) -> Result<Vec<AstStatement>, String> {
		let outcome = self.merge_tentative_with_source_selections(input, source_selections)?;
		if !outcome.conflicts.is_empty() {
			return Err(format!(
				"tree merge conflict for {}: {:?}",
				input.base.path.display(),
				outcome.conflicts,
			));
		}
		Ok(outcome.statements)
	}

	pub(crate) fn merge_tentative(
		&self,
		input: &KernelMergeInput,
	) -> Result<TreeKernelOutcome, String> {
		self.merge_tentative_with_source_selections(input, &TreeSourceSelections::new())
	}

	pub(crate) fn merge_tentative_with_source_selections(
		&self,
		input: &KernelMergeInput,
		source_selections: &TreeSourceSelections,
	) -> Result<TreeKernelOutcome, String> {
		let first = input.revisions.first().ok_or_else(|| {
			format!(
				"tree merge requires at least one revision for {}",
				input.base.path.display(),
			)
		})?;
		let selected_indices = resolve_source_selection_indices(input, source_selections)?;
		let mut merged = first.ast.clone();
		let mut conflicts = Vec::new();
		for (fold_index, revision) in input.revisions.iter().enumerate().skip(1) {
			let local_selections = selected_indices
				.iter()
				.map(|(path, selected_index)| {
					let revision = if *selected_index == fold_index {
						foch_merge_kernel::RevisionId::RIGHT
					} else {
						foch_merge_kernel::RevisionId::LEFT
					};
					(path.clone(), revision)
				})
				.collect();
			let step = self.adapter.merge_three_way(
				&input.base,
				&merged,
				&revision.ast,
				self.policies,
				&local_selections,
			)?;
			merged.statements = step.statements;
			conflicts.extend(step.conflicts);
		}
		let revision_asts = input
			.revisions
			.iter()
			.map(|revision| &revision.ast)
			.collect::<Vec<_>>();
		let reduced =
			apply_nary_scalar_reducers(&input.base, &revision_asts, &merged, self.policies)
				.map_err(|error| format!("N-source scalar finalization failed: {error}"))?;
		Ok(TreeKernelOutcome {
			statements: reduced.statements,
			conflicts,
		})
	}

	pub(crate) fn adapter_name(&self) -> &'static str {
		self.adapter.name()
	}
}

fn resolve_source_selection_indices(
	input: &KernelMergeInput,
	source_selections: &TreeSourceSelections,
) -> Result<BTreeMap<Vec<String>, usize>, String> {
	source_selections
		.iter()
		.map(|(path, selected_source)| {
			let mut matches = input
				.revisions
				.iter()
				.enumerate()
				.filter(|(_, revision)| revision.source_id == *selected_source);
			let Some((index, _)) = matches.next() else {
				return Err(format!(
					"selected source `{selected_source}` is not a revision for {} at {}",
					input.base.path.display(),
					path.join("/"),
				));
			};
			if matches.next().is_some() {
				return Err(format!(
					"selected source `{selected_source}` is ambiguous for {} at {}",
					input.base.path.display(),
					path.join("/"),
				));
			}
			Ok((path.clone(), index))
		})
		.collect()
}

#[derive(Default)]
struct TreeConflictResolution {
	source_selections: TreeSourceSelections,
	resolved_paths: Vec<Vec<String>>,
	unresolved_conflicts: Vec<TreeConflictRecord>,
	handler_resolutions: Vec<HandlerResolutionRecord>,
	resolved_conflict_ids: Vec<String>,
	external_file_resolution: Option<PathBuf>,
	keep_existing: bool,
}

fn resolve_tree_conflicts(
	input: &KernelMergeInput,
	conflicts: &[ClausewitzConflictSummary],
	policies: &MergePolicies,
	handler: &mut dyn ConflictHandler,
) -> Result<TreeConflictResolution, String> {
	let grouped = conflicts.iter().fold(
		BTreeMap::<Vec<String>, Vec<&ClausewitzConflictSummary>>::new(),
		|mut grouped, conflict| {
			grouped
				.entry(conflict.semantic_path.clone())
				.or_default()
				.push(conflict);
			grouped
		},
	);
	let mut resolution = TreeConflictResolution::default();
	let conflict_count = grouped.len();
	handler.set_conflict_progress(0, conflict_count);
	for (index, (semantic_path, conflicts)) in grouped.into_iter().enumerate() {
		handler.set_conflict_progress(index + 1, conflict_count);
		let record = tree_conflict_record(input, &semantic_path, &conflicts, policies)?;
		let view = tree_conflict_view(&input.base.path, &record)?;
		let conflict_id = view.conflict_id.clone();
		match handler.on_conflict(&view) {
			ConflictDecision::PickMod {
				mod_id,
				record: log,
			} if record.source_selectable => {
				if !record
					.candidates
					.iter()
					.any(|candidate| candidate.source_id == mod_id)
				{
					resolution.unresolved_conflicts.push(record);
					continue;
				}
				if let Some(existing) = resolution
					.source_selections
					.insert(semantic_path.clone(), mod_id.clone())
					&& existing != mod_id
				{
					return Err(format!(
						"conflicting source selections for {} at {}",
						input.base.path.display(),
						semantic_path.join("/"),
					));
				}
				resolution.resolved_paths.push(semantic_path);
				resolution.resolved_conflict_ids.push(conflict_id);
				if let Some(log) = log {
					resolution.handler_resolutions.push(log);
				}
			}
			ConflictDecision::PickMod { record: log, .. } => {
				if let Some(log) = log {
					resolution.handler_resolutions.push(log);
				}
				resolution.unresolved_conflicts.push(record);
			}
			ConflictDecision::Defer { record: None } => {
				resolution.unresolved_conflicts.push(record);
			}
			ConflictDecision::Defer { record: Some(log) } => {
				resolution.handler_resolutions.push(log);
				resolution.unresolved_conflicts.push(record);
			}
			ConflictDecision::UseFile(path) => {
				resolution.resolved_conflict_ids.push(conflict_id);
				resolution.external_file_resolution = Some(path);
			}
			ConflictDecision::KeepExisting => {
				resolution.resolved_conflict_ids.push(conflict_id);
				resolution.keep_existing = true;
			}
			ConflictDecision::Abort => {
				return Err(format!(
					"conflict handler aborted tree merge for {} at {}",
					input.base.path.display(),
					semantic_path.join("/"),
				));
			}
		}
	}
	Ok(resolution)
}

fn tree_conflict_record(
	input: &KernelMergeInput,
	semantic_path: &[String],
	conflicts: &[&ClausewitzConflictSummary],
	policies: &MergePolicies,
) -> Result<TreeConflictRecord, String> {
	let base = semantic_candidate(&input.base, semantic_path, policies)?;
	let mut candidates = input
		.revisions
		.iter()
		.map(|revision| {
			let candidate = semantic_candidate(&revision.ast, semantic_path, policies)?;
			Ok((revision, candidate))
		})
		.collect::<Result<Vec<_>, String>>()?;
	let source_selectable = !semantic_path.is_empty()
		&& base.exact
		&& candidates.iter().all(|(_, candidate)| candidate.exact);
	candidates.retain(|(_, candidate)| {
		candidate_fingerprint(candidate.statement.as_ref())
			!= candidate_fingerprint(base.statement.as_ref())
	});
	if candidates.len() < 2 {
		candidates = input
			.revisions
			.iter()
			.map(|revision| {
				Ok((
					revision,
					semantic_candidate(&revision.ast, semantic_path, policies)?,
				))
			})
			.collect::<Result<Vec<_>, String>>()?;
	}
	let candidates = candidates
		.into_iter()
		.map(|(revision, candidate)| TreeConflictCandidate {
			source_id: revision.source_id.clone(),
			precedence: revision.precedence,
			statement: candidate.statement,
		})
		.collect();
	let (address_path, address_key) = split_semantic_address(semantic_path);
	let mut reasons = conflicts
		.iter()
		.map(|conflict| format!("{}: {}", conflict.kind, conflict.detail))
		.collect::<Vec<_>>();
	reasons.sort();
	reasons.dedup();
	Ok(TreeConflictRecord {
		address_path,
		address_key,
		reason: reasons.join("; "),
		base_statement: base.statement,
		candidates,
		source_selectable,
	})
}

struct SemanticCandidate {
	statement: Option<AstStatement>,
	exact: bool,
}

fn semantic_candidate(
	file: &AstFile,
	semantic_path: &[String],
	policies: &MergePolicies,
) -> Result<SemanticCandidate, String> {
	if semantic_path.is_empty() {
		return Ok(SemanticCandidate {
			statement: None,
			exact: false,
		});
	}
	let policy = ContentFamilyMergePolicy::new(policies);
	let (semantic, _) = detach_trivia(file);
	let tree = normalize_ast(&semantic, &policy)
		.map_err(|error| format!("failed to normalize conflict candidate: {error}"))?;
	let matching_nodes = tree
		.nodes()
		.filter(|(_, node)| node.policy_path == semantic_path)
		.collect::<Vec<_>>();
	let assignments = matching_nodes
		.iter()
		.filter(|(_, node)| node.kind.starts_with("clausewitz.assignment:"))
		.map(|(node, _)| *node)
		.collect::<Vec<_>>();
	match assignments.as_slice() {
		[node] => Ok(SemanticCandidate {
			statement: Some(
				denormalize_statement(&tree, *node)
					.map_err(|error| format!("failed to render conflict candidate: {error}"))?,
			),
			exact: true,
		}),
		[] if matching_nodes.is_empty() => Ok(SemanticCandidate {
			statement: None,
			exact: true,
		}),
		[] | [_, _, ..] => Ok(SemanticCandidate {
			statement: None,
			exact: false,
		}),
	}
}

fn candidate_fingerprint(statement: Option<&AstStatement>) -> Option<String> {
	statement.map(crate::merge::semantic_fingerprint::statement_fingerprint)
}

fn split_semantic_address(path: &[String]) -> (Vec<String>, String) {
	match path.split_last() {
		Some((key, parent)) => (parent.to_vec(), key.clone()),
		None => (Vec::new(), "$file".to_string()),
	}
}

fn tree_conflict_view(
	file_path: &std::path::Path,
	record: &TreeConflictRecord,
) -> Result<ConflictView, String> {
	let candidates = record
		.candidates
		.iter()
		.map(|candidate| {
			let patch_rendered = candidate.statement.as_ref().map_or_else(
				|| Ok("(removed)".to_string()),
				|statement| {
					emit_clausewitz_statements(std::slice::from_ref(statement))
						.map(|rendered| rendered.trim_end().to_string())
						.map_err(|error| format!("failed to emit tree conflict candidate: {error}"))
				},
			)?;
			Ok(CandidateView {
				mod_id: candidate.source_id.clone(),
				mod_display_name: candidate.source_id.clone(),
				precedence: candidate.precedence,
				patch_summary: vec![format!(
					"select semantic subtree {}",
					record
						.address_path
						.iter()
						.chain(std::iter::once(&record.address_key))
						.cloned()
						.collect::<Vec<_>>()
						.join("/")
				)],
				patch_rendered,
			})
		})
		.collect::<Result<Vec<_>, String>>()?;
	let vanilla_snippet = record
		.base_statement
		.as_ref()
		.map(|statement| emit_clausewitz_statements(std::slice::from_ref(statement)))
		.transpose()
		.map_err(|error| format!("failed to emit vanilla conflict candidate: {error}"))?
		.map(|rendered| rendered.trim_end().to_string());
	Ok(ConflictView {
		file_path: file_path.to_path_buf(),
		address_path: record.address_path.clone(),
		address_key: record.address_key.clone(),
		conflict_id: compute_conflict_id(
			file_path,
			&record.address_path.join("/"),
			&record.address_key,
		),
		reason: record.reason.clone(),
		vanilla_snippet,
		candidates,
	})
}

fn merge_tree_metadata(
	base: &TreeDagState,
	revisions: &[DagJoinRevision<'_, TreeDagState>],
) -> TreeDagState {
	let mut state = base.clone();
	for revision in revisions {
		for conflict in &revision.state.unresolved_conflicts {
			if !state.unresolved_conflicts.contains(conflict) {
				state.unresolved_conflicts.push(conflict.clone());
			}
		}
		for resolution in &revision.state.handler_resolutions {
			if !state.handler_resolutions.contains(resolution) {
				state.handler_resolutions.push(resolution.clone());
			}
		}
		for conflict_id in &revision.state.resolved_conflict_ids {
			if !state.resolved_conflict_ids.contains(conflict_id) {
				state.resolved_conflict_ids.push(conflict_id.clone());
			}
		}
		if revision.state.external_file_resolution.is_some() {
			state.external_file_resolution = revision.state.external_file_resolution.clone();
		}
		state.keep_existing |= revision.state.keep_existing;
	}
	state
}

#[derive(Clone, Debug)]
pub(crate) struct TreeDagState {
	pub statements: Vec<AstStatement>,
	pub unresolved_conflicts: Vec<TreeConflictRecord>,
	pub handler_resolutions: Vec<HandlerResolutionRecord>,
	pub resolved_conflict_ids: Vec<String>,
	pub external_file_resolution: Option<PathBuf>,
	pub keep_existing: bool,
}

pub(crate) struct TreeDagProtocol<'a> {
	kernel: TreeMergeKernel<'a>,
	has_vanilla_base: bool,
	handler: &'a mut dyn ConflictHandler,
}

impl<'a> TreeDagProtocol<'a> {
	pub(crate) fn new(
		adapter: &'a dyn TreeMergeAdapter,
		policies: &'a MergePolicies,
		has_vanilla_base: bool,
		handler: &'a mut dyn ConflictHandler,
	) -> Self {
		Self {
			kernel: TreeMergeKernel::new(adapter, policies),
			has_vanilla_base,
			handler,
		}
	}
}

impl EffectiveNodeProtocol<TreeDagState> for TreeDagProtocol<'_> {
	fn effective_node(
		&mut self,
		request: EffectiveNodeRequest<'_, TreeDagState>,
	) -> Result<TreeDagState, String> {
		// A same-path mod file is a complete overlay. Definition-module callers
		// have already folded each mod and its ancestors into the supplied view.
		Ok(TreeDagState {
			statements: request.source.ast.statements.clone(),
			unresolved_conflicts: request.parent.unresolved_conflicts.clone(),
			handler_resolutions: request.parent.handler_resolutions.clone(),
			resolved_conflict_ids: request.parent.resolved_conflict_ids.clone(),
			external_file_resolution: request.parent.external_file_resolution.clone(),
			keep_existing: request.parent.keep_existing,
		})
	}
}

impl DagJoinProtocol<TreeDagState> for TreeDagProtocol<'_> {
	fn validate_final_frontier(
		&self,
		file_dag: &FileDag,
		root: &TreeDagState,
		sinks: &[ModId],
	) -> Result<(), String> {
		if sinks.len() > 1 && (!self.has_vanilla_base || root.statements.is_empty()) {
			return Err(format!(
				"tree merge unsupported for {}: a non-empty vanilla base is required",
				file_dag.file_path(),
			));
		}
		Ok(())
	}

	fn join(&mut self, request: DagJoinRequest<'_, TreeDagState>) -> Result<TreeDagState, String> {
		if request.base.statements.is_empty() {
			return Err(format!(
				"tree merge unsupported for {} {} join: a non-empty shared base is required",
				request.file_dag.file_path(),
				match request.plan.scope() {
					DagJoinScope::Intermediate => "intermediate",
					DagJoinScope::Final => "final",
				},
			));
		}
		let path = PathBuf::from(request.file_dag.file_path());
		let mut state = merge_tree_metadata(request.base, &request.revisions);
		let revisions = request
			.revisions
			.iter()
			.map(|revision| KernelRevision {
				source_id: revision.mod_id.0.clone(),
				precedence: revision.precedence,
				ast: AstFile {
					path: path.clone(),
					statements: revision.state.statements.clone(),
				},
			})
			.collect();
		let input = KernelMergeInput::new(
			AstFile {
				path,
				statements: request.base.statements.clone(),
			},
			revisions,
		);
		let probe = self.kernel.merge_tentative(&input).map_err(|error| {
			format!(
				"{} adapter failed for {}: {error}",
				self.kernel.adapter_name(),
				request.file_dag.file_path(),
			)
		})?;
		let resolution =
			resolve_tree_conflicts(&input, &probe.conflicts, self.kernel.policies, self.handler)?;
		let outcome = if resolution.source_selections.is_empty() {
			probe
		} else {
			self.kernel
				.merge_tentative_with_source_selections(&input, &resolution.source_selections)
				.map_err(|error| {
					format!(
						"{} adapter failed while replaying source selections for {}: {error}",
						self.kernel.adapter_name(),
						request.file_dag.file_path(),
					)
				})?
		};
		state.statements = outcome.statements;
		state
			.unresolved_conflicts
			.extend(resolution.unresolved_conflicts);
		state
			.handler_resolutions
			.extend(resolution.handler_resolutions);
		for conflict_id in resolution.resolved_conflict_ids {
			if !state.resolved_conflict_ids.contains(&conflict_id) {
				state.resolved_conflict_ids.push(conflict_id);
			}
		}
		if resolution.external_file_resolution.is_some() {
			state.external_file_resolution = resolution.external_file_resolution;
		}
		state.keep_existing |= resolution.keep_existing;
		for conflict in outcome.conflicts {
			if resolution.resolved_paths.contains(&conflict.semantic_path) {
				return Err(format!(
					"source selection did not resolve {} at {}",
					conflict.kind,
					conflict.semantic_path.join("/"),
				));
			}
			let record = tree_conflict_record(
				&input,
				&conflict.semantic_path,
				&[&conflict],
				self.kernel.policies,
			)?;
			if !state.unresolved_conflicts.iter().any(|existing| {
				existing.address_path == record.address_path
					&& existing.address_key == record.address_key
			}) {
				state.unresolved_conflicts.push(record);
			}
		}
		Ok(state)
	}
}

#[cfg(test)]
mod tests {
	use std::cell::RefCell;
	use std::path::PathBuf;

	use foch_language::analyzer::content_family::{
		MergePolicies, ScalarMergePolicy, ScalarReducerRule,
	};
	use foch_language::analyzer::parser::{
		AstFile, AstStatement, AstValue, ScalarValue, Span, SpanRange, parse_clausewitz_content,
	};

	use super::{
		ClausewitzFileAdapter, LocalSourceSelections, TreeMergeAdapter, TreeMergeKernel,
		TreeMergeStep, TreeSourceSelections,
	};
	use crate::emit::emit_clausewitz_statements;
	use crate::merge::kernel::{KernelMergeInput, KernelRevision};

	struct RecordingAdapter {
		calls: RefCell<Vec<(String, String)>>,
	}

	impl TreeMergeAdapter for RecordingAdapter {
		fn name(&self) -> &'static str {
			"recording"
		}

		fn merge_three_way(
			&self,
			_base: &AstFile,
			left: &AstFile,
			right: &AstFile,
			_policies: &MergePolicies,
			_source_selections: &LocalSourceSelections,
		) -> Result<TreeMergeStep, String> {
			self.calls.borrow_mut().push((
				scalar_value(left).to_string(),
				scalar_value(right).to_string(),
			));
			Ok(TreeMergeStep {
				statements: right.statements.clone(),
				conflicts: Vec::new(),
			})
		}
	}

	#[test]
	fn tree_kernel_folds_all_revisions_in_precedence_order() {
		let adapter = RecordingAdapter {
			calls: RefCell::new(Vec::new()),
		};
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&adapter, &policies);
		let input = KernelMergeInput::new(
			file("base"),
			vec![
				revision("highest", 30, "right"),
				revision("lowest", 10, "left"),
				revision("middle", 20, "middle"),
			],
		);

		let merged = kernel.merge(&input).expect("merge revisions");

		assert_eq!(scalar_statement_value(&merged), "right");
		assert_eq!(
			adapter.calls.into_inner(),
			vec![
				("left".to_string(), "middle".to_string()),
				("middle".to_string(), "right".to_string()),
			],
		);
	}

	#[test]
	fn tree_kernel_reduces_all_changed_numeric_contributors_once() {
		const RULES: &[ScalarReducerRule] = &[ScalarReducerRule::new(
			&["province_trade_power_modifier"],
			ScalarMergePolicy::Avg,
		)];
		let policies = MergePolicies {
			scalar_reducer_rules: RULES,
			..MergePolicies::default()
		};
		let kernel = TreeMergeKernel::new(&ClausewitzFileAdapter, &policies);
		let input = KernelMergeInput::new(
			parsed_file("cloves = { province_trade_power_modifier = 0 }\n"),
			vec![
				parsed_revision("a", 10, "cloves = { province_trade_power_modifier = .2 }\n"),
				parsed_revision("b", 20, "cloves = { province_trade_power_modifier = .1 }\n"),
				parsed_revision("c", 30, "cloves = { province_trade_power_modifier = .3 }\n"),
			],
		);

		let merged = kernel.merge(&input).expect("merge all contributors");
		let output = emit_clausewitz_statements(&merged).expect("emit merged AST");

		assert!(
			output.contains("province_trade_power_modifier = .2"),
			"{output}"
		);
	}

	#[test]
	fn tree_kernel_excludes_unchanged_base_copies_from_numeric_reducer() {
		const RULES: &[ScalarReducerRule] = &[ScalarReducerRule::new(
			&["province_trade_power_modifier"],
			ScalarMergePolicy::Avg,
		)];
		let policies = MergePolicies {
			scalar_reducer_rules: RULES,
			..MergePolicies::default()
		};
		let kernel = TreeMergeKernel::new(&ClausewitzFileAdapter, &policies);
		let input = KernelMergeInput::new(
			parsed_file("cloves = { province_trade_power_modifier = 0 }\n"),
			vec![
				parsed_revision("a", 10, "cloves = { province_trade_power_modifier = .2 }\n"),
				parsed_revision("b", 20, "cloves = { province_trade_power_modifier = .1 }\n"),
				parsed_revision(
					"unchanged",
					30,
					"cloves = { province_trade_power_modifier = 0 }\n",
				),
			],
		);

		let merged = kernel.merge(&input).expect("merge changed contributors");
		let output = emit_clausewitz_statements(&merged).expect("emit merged AST");

		assert!(
			output.contains("province_trade_power_modifier = .15"),
			"{output}"
		);
	}

	#[test]
	fn tree_kernel_can_select_any_original_source_across_three_revisions() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileAdapter, &policies);
		let input = KernelMergeInput::new(
			parsed_file("value = 0\n"),
			vec![
				parsed_revision("a", 10, "value = 1\n"),
				parsed_revision("b", 20, "value = 2\n"),
				parsed_revision("c", 30, "value = 3\n"),
			],
		);

		for (source, expected) in [("a", "1"), ("b", "2"), ("c", "3")] {
			let selections =
				TreeSourceSelections::from([(vec!["value".to_string()], source.to_string())]);
			let merged = kernel
				.merge_with_source_selections(&input, &selections)
				.unwrap_or_else(|error| panic!("select original source {source}: {error}"));
			let output = emit_clausewitz_statements(&merged).expect("emit selected AST");
			assert!(output.contains(&format!("value = {expected}")), "{output}");
		}
	}

	#[test]
	fn tree_kernel_can_select_an_original_source_deletion() {
		let policies = MergePolicies::default();
		let kernel = TreeMergeKernel::new(&ClausewitzFileAdapter, &policies);
		let input = KernelMergeInput::new(
			parsed_file("value = 0\n"),
			vec![
				parsed_revision("a", 10, "value = 1\n"),
				parsed_revision("b", 20, ""),
				parsed_revision("c", 30, "value = 3\n"),
			],
		);

		for (source, expected) in [("a", Some("1")), ("b", None), ("c", Some("3"))] {
			let selections =
				TreeSourceSelections::from([(vec!["value".to_string()], source.to_string())]);
			let merged = kernel
				.merge_with_source_selections(&input, &selections)
				.unwrap_or_else(|error| panic!("select original source {source}: {error}"));
			let output = emit_clausewitz_statements(&merged).expect("emit selected AST");
			match expected {
				Some(value) => assert!(output.contains(&format!("value = {value}")), "{output}"),
				None => assert!(!output.contains("value ="), "{output}"),
			}
		}
	}

	fn revision(source_id: &str, precedence: usize, value: &str) -> KernelRevision {
		KernelRevision {
			source_id: source_id.to_string(),
			precedence,
			ast: file(value),
		}
	}

	fn parsed_revision(source_id: &str, precedence: usize, source: &str) -> KernelRevision {
		KernelRevision {
			source_id: source_id.to_string(),
			precedence,
			ast: parsed_file(source),
		}
	}

	fn parsed_file(source: &str) -> AstFile {
		let parsed = parse_clausewitz_content(PathBuf::from("common/test.txt"), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast
	}

	fn file(value: &str) -> AstFile {
		AstFile {
			path: PathBuf::from("common/test.txt"),
			statements: vec![AstStatement::Assignment {
				key: "value".to_string(),
				key_span: span(),
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(value.to_string()),
					span: span(),
				},
				span: span(),
			}],
		}
	}

	fn scalar_value(file: &AstFile) -> &str {
		scalar_statement_value(&file.statements)
	}

	fn scalar_statement_value(statements: &[AstStatement]) -> &str {
		let [
			AstStatement::Assignment {
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(value),
					..
				},
				..
			},
		] = statements
		else {
			panic!("expected one identifier assignment")
		};
		value
	}

	fn span() -> SpanRange {
		SpanRange {
			start: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
			end: Span {
				line: 0,
				column: 0,
				offset: 0,
			},
		}
	}
}
