use super::error::MergeError;
use foch_language::analyzer::parser::{AstStatement, AstValue, SpanRange};
use foch_language::analyzer::semantic_index::ParsedScriptFile;

#[derive(Clone, Debug)]
pub(crate) struct DefinesAssignmentFragment {
	pub merge_key: String,
	pub statement_key: String,
	pub path_segments: Vec<String>,
	pub statement: AstStatement,
	pub statement_span: SpanRange,
}

pub(crate) fn normalize_defines_file(
	parsed: &ParsedScriptFile,
) -> Result<Vec<DefinesAssignmentFragment>, MergeError> {
	let mut fragments = Vec::new();

	for statement in &parsed.ast.statements {
		collect_defines_fragments(statement, &[], &mut fragments, parsed)?;
	}

	if fragments.is_empty() {
		return Err(MergeError::Parse {
			path: Some(parsed.relative_path.display().to_string()),
			message: format!(
				"defines merge requires at least one leaf assignment in {}",
				parsed.relative_path.display()
			),
		});
	}

	Ok(fragments)
}

fn collect_defines_fragments(
	statement: &AstStatement,
	parent_segments: &[String],
	fragments: &mut Vec<DefinesAssignmentFragment>,
	parsed: &ParsedScriptFile,
) -> Result<(), MergeError> {
	match statement {
		AstStatement::Comment { .. } => Ok(()),
		AstStatement::Item { .. } => Err(MergeError::Parse {
			path: Some(parsed.relative_path.display().to_string()),
			message: format!(
				"defines merge requires named assignments in {} at {}",
				parsed.relative_path.display(),
				describe_assignment_path(parent_segments)
			),
		}),
		AstStatement::Assignment {
			key, value, span, ..
		} => {
			let mut path_segments = parent_segments.to_vec();
			path_segments.push(key.clone());
			match value {
				AstValue::Scalar { .. } => {
					fragments.push(DefinesAssignmentFragment {
						merge_key: path_segments.join("."),
						statement_key: key.clone(),
						path_segments,
						statement: statement.clone(),
						statement_span: span.clone(),
					});
					Ok(())
				}
				AstValue::Block { items, .. } => {
					let fragment_count = fragments.len();
					for item in items {
						collect_defines_fragments(item, &path_segments, fragments, parsed)?;
					}
					if fragments.len() == fragment_count {
						return Err(MergeError::Parse {
							path: Some(parsed.relative_path.display().to_string()),
							message: format!(
								"defines merge requires leaf assignments below {} in {}",
								describe_assignment_path(&path_segments),
								parsed.relative_path.display()
							),
						});
					}
					Ok(())
				}
			}
		}
	}
}

fn describe_assignment_path(path_segments: &[String]) -> String {
	if path_segments.is_empty() {
		"<root>".to_string()
	} else {
		path_segments.join(".")
	}
}
