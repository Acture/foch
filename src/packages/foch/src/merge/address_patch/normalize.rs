use super::super::error::MergeError;
use crate::game::eu4::script::ParsedScriptFile;
use crate::game::eu4::script::parser::{AstStatement, AstValue, ScalarValue, SpanRange};
use std::collections::BTreeSet;

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

	let mut seen_merge_keys: BTreeSet<&str> = BTreeSet::new();
	for fragment in &fragments {
		if !seen_merge_keys.insert(&fragment.merge_key) {
			return Err(MergeError::Parse {
				path: Some(parsed.relative_path.display().to_string()),
				message: format!(
					"defines merge cannot safely normalize duplicate leaf path `{}` in {} because Lua uses last-assignment-wins semantics within a file",
					fragment.merge_key,
					parsed.relative_path.display()
				),
			});
		}
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
		AstStatement::Item {
			value: AstValue::Scalar {
				value: ScalarValue::Identifier(separator),
				..
			},
			..
		} if separator == "," => Ok(()),
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
	use crate::game::eu4::script::ParsedScriptFile;
	use crate::game::eu4::script::parser::parse_clausewitz_content;
	use std::path::PathBuf;

	fn parsed(path: &str, content: &str) -> ParsedScriptFile {
		let path_buf = PathBuf::from(path);
		let parse_result = parse_clausewitz_content(path_buf.clone(), content);
		ParsedScriptFile {
			mod_id: "test_mod".to_string(),
			path: path_buf.clone(),
			relative_path: path_buf,
			content_family: None,
			file_kind: crate::game::eu4::content::ScriptFileKind::new("other"),
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
	fn nested_lua_table_commas_are_separators_not_unnamed_values() {
		let file = parsed(
			"common/defines.lua",
			concat!(
				"NDefines = {\n",
				"\tNGame = {\n",
				"\t\tSTART_DATE = \"1444.11.11\",\n",
				"\t\tMAX_CLIENT_STATES = 75,\n",
				"\t}\n",
				"}\n",
			),
		);

		let fragments = normalize_defines_file(&file).expect("Lua separators are valid");
		assert_eq!(fragments.len(), 2);
		assert_eq!(fragments[0].merge_key, "NDefines.NGame.START_DATE");
		assert_eq!(fragments[1].merge_key, "NDefines.NGame.MAX_CLIENT_STATES");
	}

	#[test]
	fn unnamed_defines_values_other_than_lua_commas_still_fail_closed() {
		let file = parsed(
			"common/defines.lua",
			"NDefines = { NGame = { stray_value } }\n",
		);

		let err = normalize_defines_file(&file).expect_err("unnamed value must be rejected");
		assert!(err.to_string().contains("at NDefines.NGame"), "{err}");
	}

	#[test]
	fn exact_defines_root_rejects_duplicate_full_leaf_path() {
		let file = parsed(
			"common/defines.lua",
			concat!(
				"NDefines = {\n",
				"\tNGame = {\n",
				"\t\tMAX_CLIENT_STATES = 100\n",
				"\t\tMAX_CLIENT_STATES = 10\n",
				"\t}\n",
				"}\n",
			),
		);

		let err = normalize_defines_file(&file)
			.expect_err("Lua last-assignment-wins duplicates must fail closed");
		let MergeError::Parse { path, message } = err else {
			panic!("expected parse error for duplicate defines leaf");
		};
		assert_eq!(path.as_deref(), Some("common/defines.lua"));
		assert!(
			message.contains("duplicate leaf path `NDefines.NGame.MAX_CLIENT_STATES`"),
			"{message}"
		);
		assert!(
			message.contains("Lua uses last-assignment-wins semantics within a file"),
			"{message}"
		);
	}

	#[test]
	fn repeated_leaf_name_under_distinct_full_paths_normalizes() {
		let file = parsed(
			"common/defines.lua",
			concat!(
				"NDefines.NGame.LIMIT = 1\n",
				"NDefines.NCountry.LIMIT = 2\n",
			),
		);

		let fragments = normalize_defines_file(&file).expect("full leaf paths are distinct");
		assert_eq!(fragments.len(), 2);
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
