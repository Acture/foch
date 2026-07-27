use std::path::PathBuf;

use foch_language::analyzer::content_family::MergePolicies;
use foch_language::analyzer::parser::{AstFile, AstStatement};

use crate::merge::kernel::{KernelMergeInput, KernelRevision};
use crate::merge::planning::dag::{FileDag, ModId};
use crate::merge::planning::dag_join::DagJoinScope;
use crate::merge::planning::dag_pipeline::{
	DagJoinProtocol, DagJoinRequest, EffectiveNodeProtocol, EffectiveNodeRequest,
};

use super::{merge_clausewitz_definition_module, merge_event_files};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StructuredJoinKind {
	Event,
	DefinitionModule,
}

#[derive(Clone, Debug)]
pub(crate) struct StructuredDagState {
	pub statements: Vec<AstStatement>,
}

pub(crate) struct StructuredDagProtocol<'a> {
	kind: StructuredJoinKind,
	policies: &'a MergePolicies,
	has_vanilla_base: bool,
}

impl<'a> StructuredDagProtocol<'a> {
	pub(crate) fn new(
		kind: StructuredJoinKind,
		policies: &'a MergePolicies,
		has_vanilla_base: bool,
	) -> Self {
		Self {
			kind,
			policies,
			has_vanilla_base,
		}
	}
}

impl EffectiveNodeProtocol<StructuredDagState> for StructuredDagProtocol<'_> {
	fn effective_node(
		&mut self,
		request: EffectiveNodeRequest<'_, StructuredDagState>,
	) -> Result<StructuredDagState, String> {
		// A same-path mod file is a complete overlay. Definition-module callers
		// have already folded each mod and its ancestors into the supplied view.
		Ok(StructuredDagState {
			statements: request.source.ast.statements.clone(),
		})
	}
}

impl DagJoinProtocol<StructuredDagState> for StructuredDagProtocol<'_> {
	fn validate_final_frontier(
		&self,
		file_dag: &FileDag,
		root: &StructuredDagState,
		sinks: &[ModId],
	) -> Result<(), String> {
		if !self.has_vanilla_base || root.statements.is_empty() {
			return Err(format!(
				"structured merge unsupported for {}: a non-empty vanilla base is required",
				file_dag.file_path()
			));
		}
		if sinks.len() != 2 {
			return Err(format!(
				"structured merge unsupported for {}: expected exactly two final sinks, found {}",
				file_dag.file_path(),
				sinks.len()
			));
		}
		Ok(())
	}

	fn join(
		&mut self,
		request: DagJoinRequest<'_, StructuredDagState>,
	) -> Result<StructuredDagState, String> {
		if request.plan.scope() == DagJoinScope::Final && request.base.statements.is_empty() {
			return Err(format!(
				"structured merge unsupported for {}: a non-empty shared base is required",
				request.file_dag.file_path()
			));
		}
		let path = PathBuf::from(request.file_dag.file_path());
		let revisions = request
			.revisions
			.into_iter()
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
		let statements = merge_structured_dag_join(&input, self.kind, self.policies)?;
		Ok(StructuredDagState { statements })
	}
}

pub(crate) fn merge_structured_dag_join(
	input: &KernelMergeInput,
	kind: StructuredJoinKind,
	policies: &MergePolicies,
) -> Result<Vec<AstStatement>, String> {
	let [left, right] = input
		.exactly_two_revisions()
		.map_err(|error| format!("structured merge unsupported: {error}"))?;
	let statements = match kind {
		StructuredJoinKind::Event => {
			let outcome = merge_event_files(&input.base, &left.ast, &right.ast, policies)
				.map_err(|error| format!("structured merge adapter failed: {error}"))?;
			if !outcome.conflicts().is_empty() {
				let conflicts = serde_json::to_string(outcome.conflicts())
					.unwrap_or_else(|_| format!("{:?}", outcome.conflicts()));
				return Err(format!(
					"structured merge conflict for {}: {conflicts}",
					input.base.path.display()
				));
			}
			outcome
				.resolved_ast()
				.expect("conflict-free structured event outcome exposes an AST")
				.statements
				.clone()
		}
		StructuredJoinKind::DefinitionModule => {
			let outcome =
				merge_clausewitz_definition_module(&input.base, &left.ast, &right.ast, policies)
					.map_err(|error| format!("structured module adapter failed: {error}"))?;
			eprintln!(
				"[structured-module] final join {} base_definitions={} active_definitions={} copy_through_definitions={} structured_definitions={}",
				input.base.path.display(),
				outcome.base_definitions(),
				outcome.active_definitions(),
				outcome.copy_through_definitions(),
				outcome.structured_definitions(),
			);
			if !outcome.conflicts().is_empty() {
				return Err(format!(
					"structured merge conflict for {}: {:?}",
					input.base.path.display(),
					outcome.conflicts(),
				));
			}
			outcome
				.resolved_ast()
				.expect("conflict-free structured module outcome exposes an AST")
				.statements
				.clone()
		}
	};
	Ok(statements)
}
