use super::error::MergeError;
use foch_language::analyzer::parser::{AstStatement, AstValue, SpanRange};
use foch_language::analyzer::semantic_index::ParsedScriptFile;

#[derive(Clone, Debug)]
#[allow(dead_code)]
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

	// An empty fragments list at the root is intentionally allowed: a defines
	// file may legitimately consist only of comments, or be a 0-byte placeholder
	// that a downstream mod ships to "no-op" the file. At Lua runtime an empty
	// file does nothing — no NDefines values change. Treating it as a fatal
	// parse error would block merge; we instead surface it as a zero-contribution
	// contributor.
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

#[cfg(test)]
mod tests {
	use super::*;
	use foch_language::analyzer::parser::parse_clausewitz_content;
	use foch_language::analyzer::semantic_index::ParsedScriptFile;
	use std::path::PathBuf;

	fn parsed(path: &str, content: &str) -> ParsedScriptFile {
		let path_buf = PathBuf::from(path);
		let parse_result = parse_clausewitz_content(path_buf.clone(), content);
		ParsedScriptFile {
			mod_id: "test_mod".to_string(),
			path: path_buf.clone(),
			relative_path: path_buf,
			content_family: None,
			file_kind: foch_language::analyzer::content_family::ScriptFileKind::Other,
			module_name: "test".to_string(),
			ast: parse_result.ast,
			source: content.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	#[test]
	fn empty_defines_file_is_zero_contribution() {
		let file = parsed("common/defines/empty.lua", "");
		let fragments = normalize_defines_file(&file).expect("empty file is valid");
		assert!(fragments.is_empty());
	}

	#[test]
	fn comment_only_defines_file_is_zero_contribution() {
		let file = parsed(
			"common/defines/comment_only.lua",
			"-- header comment\n--directly second line\n",
		);
		let fragments = normalize_defines_file(&file).expect("comment-only file is valid");
		assert!(fragments.is_empty());
	}

	#[test]
	fn dotted_defines_with_lua_comments_normalize() {
		let file = parsed(
			"common/defines/idea.lua",
			"--直属州维护费\nNDefines.NCountry.STATE_MAINTENANCE_DEV_FACTOR = 0.012\nNDefines.NCountry.PS_BUY_IDEA = 250 -- inline\n",
		);
		let fragments = normalize_defines_file(&file).expect("file with -- comments is valid");
		assert_eq!(fragments.len(), 2);
		assert_eq!(
			fragments[0].merge_key,
			"NDefines.NCountry.STATE_MAINTENANCE_DEV_FACTOR"
		);
		assert_eq!(fragments[1].merge_key, "NDefines.NCountry.PS_BUY_IDEA");
	}

	#[test]
	fn nested_empty_block_still_errors() {
		// A nested `NDefines.X = {}` is NOT zero contribution — it's an
		// explicit empty-block assignment that the merge cannot represent.
		// The root-level empty allowance must not extend to nested blocks.
		let file = parsed(
			"common/defines/nested_empty.lua",
			"NDefines.NMilitary = {}\n",
		);
		let result = normalize_defines_file(&file);
		assert!(
			result.is_err(),
			"nested empty block should still be rejected, got: {result:?}"
		);
	}
}
