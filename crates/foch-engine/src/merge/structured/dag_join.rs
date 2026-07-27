use foch_language::analyzer::content_family::MergePolicies;
use foch_language::analyzer::parser::AstStatement;

use crate::merge::kernel::KernelMergeInput;

use super::{merge_clausewitz_definition_module, merge_event_files};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StructuredJoinKind {
	Event,
	DefinitionModule,
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
