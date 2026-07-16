use super::content_family::{
	DefinitionFileOrder, DefinitionKeyPolicy, DefinitionModulePolicy, DuplicateDefinitionPolicy,
};
use super::parser::{AstFile, AstStatement, SpanRange};
use super::semantic_index::ParsedScriptFile;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct DefinitionModuleInput<'a> {
	pub path: &'a Path,
	pub file: &'a ParsedScriptFile,
}

impl<'a> DefinitionModuleInput<'a> {
	pub fn new(path: &'a Path, file: &'a ParsedScriptFile) -> Self {
		Self { path, file }
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionSource {
	pub path: String,
	/// Zero-based position in the source file's top-level AST statement list.
	pub statement_ordinal: usize,
	pub span: SpanRange,
}

/// One deterministic overwrite event in module load order.
///
/// For three definitions `A`, `B`, and `C`, diagnostics are `A -> B` and
/// `B -> C`; `current_source` is the winner at that specific load step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateDefinitionDiagnostic {
	pub definition_key: String,
	pub previous_source: DefinitionSource,
	pub current_source: DefinitionSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalDefinitionModule {
	pub ast: AstFile,
	pub definition_sources: BTreeMap<String, DefinitionSource>,
	pub duplicate_diagnostics: Vec<DuplicateDefinitionDiagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopLevelStatementKind {
	Item,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidDefinitionModulePathReason {
	Empty,
	ParentTraversal,
	Absolute,
	Prefix,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefinitionModuleLoadError {
	NonUtf8Path {
		path: PathBuf,
	},
	InvalidRelativePath {
		path: PathBuf,
		reason: InvalidDefinitionModulePathReason,
	},
	InputPathMismatch {
		input_path: String,
		file_relative_path: String,
	},
	OutsideReplacementPrefix {
		path: String,
		replacement_prefix: String,
	},
	DuplicateInputPath {
		path: String,
	},
	ParseIssues {
		path: String,
		issue_count: usize,
	},
	UnsupportedTopLevelStatement {
		path: String,
		statement_ordinal: usize,
		kind: TopLevelStatementKind,
	},
	MissingDefinitionKey {
		path: String,
		statement_ordinal: usize,
	},
}

#[derive(Clone, Debug)]
struct NormalizedInput<'a> {
	path: String,
	file: &'a ParsedScriptFile,
}

#[derive(Clone, Debug)]
struct WinningDefinition {
	output_statement_index: usize,
	source: DefinitionSource,
}

pub fn load_definition_module(
	inputs: &[DefinitionModuleInput<'_>],
	policy: DefinitionModulePolicy,
) -> Result<CanonicalDefinitionModule, DefinitionModuleLoadError> {
	let namespace_prefix = normalize_relative_path(Path::new(policy.namespace_prefix))?;
	let mut ordered_inputs = inputs
		.iter()
		.map(|input| {
			let input_path = normalize_relative_path(input.path)?;
			let file_relative_path = normalize_relative_path(&input.file.relative_path)?;
			if input_path != file_relative_path {
				return Err(DefinitionModuleLoadError::InputPathMismatch {
					input_path,
					file_relative_path,
				});
			}
			if !path_is_within_prefix(&input_path, &namespace_prefix) {
				return Err(DefinitionModuleLoadError::OutsideReplacementPrefix {
					path: input_path,
					replacement_prefix: namespace_prefix.clone(),
				});
			}
			Ok(NormalizedInput {
				path: input_path,
				file: input.file,
			})
		})
		.collect::<Result<Vec<_>, DefinitionModuleLoadError>>()?;

	match policy.file_order {
		DefinitionFileOrder::NormalizedPathAscending => {
			ordered_inputs.sort_by(|left, right| left.path.cmp(&right.path));
		}
	}
	for adjacent in ordered_inputs.windows(2) {
		if adjacent[0].path == adjacent[1].path {
			return Err(DefinitionModuleLoadError::DuplicateInputPath {
				path: adjacent[0].path.clone(),
			});
		}
	}

	let mut output_statements = Vec::<Option<AstStatement>>::new();
	let mut winners = BTreeMap::<String, WinningDefinition>::new();
	let mut duplicate_diagnostics = Vec::new();

	for input in ordered_inputs {
		if !input.file.parse_issues.is_empty() {
			return Err(DefinitionModuleLoadError::ParseIssues {
				path: input.path,
				issue_count: input.file.parse_issues.len(),
			});
		}

		for (statement_ordinal, statement) in input.file.ast.statements.iter().enumerate() {
			let AstStatement::Assignment { key, span, .. } = statement else {
				match statement {
					AstStatement::Comment { .. } => continue,
					AstStatement::Item { .. } => {
						return Err(DefinitionModuleLoadError::UnsupportedTopLevelStatement {
							path: input.path,
							statement_ordinal,
							kind: TopLevelStatementKind::Item,
						});
					}
					AstStatement::Assignment { .. } => unreachable!(),
				}
			};

			let definition_key = match policy.definition_key {
				DefinitionKeyPolicy::AssignmentKey => key,
			};
			if definition_key.trim().is_empty() {
				return Err(DefinitionModuleLoadError::MissingDefinitionKey {
					path: input.path,
					statement_ordinal,
				});
			}
			let source = DefinitionSource {
				path: input.path.clone(),
				statement_ordinal,
				span: span.clone(),
			};

			match policy.duplicate_definitions {
				DuplicateDefinitionPolicy::LaterDefinitionWins => {
					if let Some(previous) = winners.get(definition_key) {
						output_statements[previous.output_statement_index] = None;
						duplicate_diagnostics.push(DuplicateDefinitionDiagnostic {
							definition_key: definition_key.clone(),
							previous_source: previous.source.clone(),
							current_source: source.clone(),
						});
					}
				}
				DuplicateDefinitionPolicy::PreserveAll => {}
			}

			let output_statement_index = output_statements.len();
			output_statements.push(Some(statement.clone()));
			winners.insert(
				definition_key.clone(),
				WinningDefinition {
					output_statement_index,
					source,
				},
			);
		}
	}

	Ok(CanonicalDefinitionModule {
		ast: AstFile {
			path: PathBuf::from(policy.output_path),
			statements: output_statements.into_iter().flatten().collect(),
		},
		definition_sources: winners
			.into_iter()
			.map(|(key, winner)| (key, winner.source))
			.collect(),
		duplicate_diagnostics,
	})
}

fn path_is_within_prefix(path: &str, prefix: &str) -> bool {
	path.strip_prefix(prefix)
		.is_some_and(|suffix| suffix.starts_with('/'))
}

fn normalize_relative_path(path: &Path) -> Result<String, DefinitionModuleLoadError> {
	let Some(raw_path) = path.to_str() else {
		return Err(DefinitionModuleLoadError::NonUtf8Path {
			path: path.to_path_buf(),
		});
	};
	let slash_normalized = raw_path.replace('\\', "/");
	if has_platform_prefix(path, &slash_normalized) {
		return Err(DefinitionModuleLoadError::InvalidRelativePath {
			path: path.to_path_buf(),
			reason: InvalidDefinitionModulePathReason::Prefix,
		});
	}
	if path.has_root() || slash_normalized.starts_with('/') {
		return Err(DefinitionModuleLoadError::InvalidRelativePath {
			path: path.to_path_buf(),
			reason: InvalidDefinitionModulePathReason::Absolute,
		});
	}

	let mut components = Vec::new();
	for component in slash_normalized.split('/') {
		match component {
			"" | "." => {}
			".." => {
				return Err(DefinitionModuleLoadError::InvalidRelativePath {
					path: path.to_path_buf(),
					reason: InvalidDefinitionModulePathReason::ParentTraversal,
				});
			}
			_ => components.push(component),
		}
	}
	if components.is_empty() {
		return Err(DefinitionModuleLoadError::InvalidRelativePath {
			path: path.to_path_buf(),
			reason: InvalidDefinitionModulePathReason::Empty,
		});
	}
	Ok(components.join("/"))
}

fn has_platform_prefix(path: &Path, slash_normalized: &str) -> bool {
	use std::path::Component;

	path.components()
		.any(|component| matches!(component, Component::Prefix(_)))
		|| matches!(
			slash_normalized.as_bytes(),
			[first, b':', ..] if first.is_ascii_alphabetic()
		)
}

#[cfg(test)]
mod tests {
	use super::{
		DefinitionModuleInput, DefinitionModuleLoadError, DefinitionSource,
		InvalidDefinitionModulePathReason, TopLevelStatementKind, load_definition_module,
	};
	use crate::analyzer::content_family::CwtType;
	use crate::analyzer::content_family::{
		DefinitionFileOrder, DefinitionKeyPolicy, DefinitionModuleOutput, DefinitionModulePolicy,
		DuplicateDefinitionPolicy,
	};
	use crate::analyzer::parser::{
		AstFile, AstStatement, AstValue, SpanRange, parse_clausewitz_content,
	};
	use crate::analyzer::semantic_index::ParsedScriptFile;
	use foch_core::model::ParseIssue;
	use std::path::{Path, PathBuf};

	const POLICY: DefinitionModulePolicy = DefinitionModulePolicy {
		definition_key: DefinitionKeyPolicy::AssignmentKey,
		file_order: DefinitionFileOrder::NormalizedPathAscending,
		duplicate_definitions: DuplicateDefinitionPolicy::LaterDefinitionWins,
		output_path: "common/governments/00_foch_governments.txt",
		namespace_prefix: "common/governments",
		output_mode: DefinitionModuleOutput::ReplaceNamespace,
		policy_version: 1,
	};

	const PRESERVE_DUPLICATES_POLICY: DefinitionModulePolicy = DefinitionModulePolicy {
		duplicate_definitions: DuplicateDefinitionPolicy::PreserveAll,
		..POLICY
	};

	fn parsed_file(path: impl AsRef<Path>, source: &str) -> ParsedScriptFile {
		let path = path.as_ref().to_path_buf();
		let parsed = parse_clausewitz_content(path.clone(), source);
		assert!(
			parsed.diagnostics.is_empty(),
			"test fixture must parse cleanly: {:?}",
			parsed.diagnostics
		);
		ParsedScriptFile {
			mod_id: "test".to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("governments"),
			module_name: "governments".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn assignment_keys(ast: &AstFile) -> Vec<&str> {
		ast.statements
			.iter()
			.map(|statement| match statement {
				AstStatement::Assignment { key, .. } => key.as_str(),
				other => panic!("canonical module contains non-definition: {other:?}"),
			})
			.collect()
	}

	fn marker_for(ast: &AstFile, definition_key: &str) -> String {
		let definition = ast
			.statements
			.iter()
			.find(
				|statement| matches!(statement, AstStatement::Assignment { key, .. } if key == definition_key),
			)
			.unwrap_or_else(|| panic!("missing definition {definition_key}"));
		let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = definition
		else {
			panic!("definition {definition_key} must be a block");
		};
		let marker = items
			.iter()
			.find(
				|statement| matches!(statement, AstStatement::Assignment { key, .. } if key == "marker"),
			)
			.unwrap_or_else(|| panic!("definition {definition_key} is missing marker"));
		let AstStatement::Assignment {
			value: AstValue::Scalar { value, .. },
			..
		} = marker
		else {
			panic!("marker must be scalar");
		};
		value.as_text()
	}

	fn statement_span(statement: &AstStatement) -> &SpanRange {
		match statement {
			AstStatement::Assignment { span, .. }
			| AstStatement::Item { span, .. }
			| AstStatement::Comment { span, .. } => span,
		}
	}

	#[test]
	fn normalized_path_ordering_is_deterministic() {
		let z_path = PathBuf::from(r"common\governments\z.txt");
		let a_path = PathBuf::from("common/governments/a.txt");
		let z_file = parsed_file(&z_path, "z_government = { marker = z }");
		let a_file = parsed_file(&a_path, "a_government = { marker = a }");

		let loaded = load_definition_module(
			&[
				DefinitionModuleInput::new(&z_path, &z_file),
				DefinitionModuleInput::new(&a_path, &a_file),
			],
			POLICY,
		)
		.expect("module should load");

		assert_eq!(
			assignment_keys(&loaded.ast),
			vec!["a_government", "z_government"]
		);
		assert_eq!(loaded.ast.path, PathBuf::from(POLICY.output_path));
	}

	#[test]
	fn lexical_path_normalization_collapses_separators_and_dot_components() {
		let input_path = PathBuf::from("common//./governments///definitions.txt");
		let file_path = PathBuf::from("common/governments/definitions.txt");
		let file = parsed_file(&file_path, "shared = { marker = normalized }");

		let loaded =
			load_definition_module(&[DefinitionModuleInput::new(&input_path, &file)], POLICY)
				.expect("lexical aliases should normalize to the same relative path");

		assert_eq!(
			loaded.definition_sources["shared"].path,
			"common/governments/definitions.txt"
		);
	}

	#[test]
	fn normalized_input_path_must_match_file_relative_path() {
		let input_path = PathBuf::from("common/governments/input.txt");
		let file_path = PathBuf::from("common/governments/file.txt");
		let file = parsed_file(&file_path, "shared = { marker = value }");

		let error =
			load_definition_module(&[DefinitionModuleInput::new(&input_path, &file)], POLICY)
				.expect_err("mismatched caller and parsed-file paths must be rejected");

		assert_eq!(
			error,
			DefinitionModuleLoadError::InputPathMismatch {
				input_path: "common/governments/input.txt".to_string(),
				file_relative_path: "common/governments/file.txt".to_string(),
			}
		);
	}

	#[test]
	fn input_must_belong_to_the_policy_replacement_prefix() {
		let path = PathBuf::from("events/not_a_government.txt");
		let file = parsed_file(&path, "event_definition = { marker = value }");

		let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], POLICY)
			.expect_err("definition modules must not absorb files from another runtime prefix");

		assert_eq!(
			error,
			DefinitionModuleLoadError::OutsideReplacementPrefix {
				path: "events/not_a_government.txt".to_string(),
				replacement_prefix: "common/governments".to_string(),
			}
		);
	}

	#[test]
	fn lexical_path_aliases_are_duplicate_inputs() {
		let alias_path = PathBuf::from("common//governments/./definitions.txt");
		let canonical_path = PathBuf::from("common/governments/definitions.txt");
		let alias_file = parsed_file(&alias_path, "first = { marker = first }");
		let canonical_file = parsed_file(&canonical_path, "second = { marker = second }");

		let error = load_definition_module(
			&[
				DefinitionModuleInput::new(&alias_path, &alias_file),
				DefinitionModuleInput::new(&canonical_path, &canonical_file),
			],
			POLICY,
		)
		.expect_err("lexical aliases must not create two module inputs");

		assert_eq!(
			error,
			DefinitionModuleLoadError::DuplicateInputPath {
				path: "common/governments/definitions.txt".to_string(),
			}
		);
	}

	#[test]
	fn invalid_relative_paths_are_rejected() {
		let cases = [
			(PathBuf::new(), InvalidDefinitionModulePathReason::Empty),
			(
				PathBuf::from("common/governments/../definitions.txt"),
				InvalidDefinitionModulePathReason::ParentTraversal,
			),
			(
				PathBuf::from("/common/governments/definitions.txt"),
				InvalidDefinitionModulePathReason::Absolute,
			),
			(
				PathBuf::from(r"C:\common\governments\definitions.txt"),
				InvalidDefinitionModulePathReason::Prefix,
			),
		];

		for (path, reason) in cases {
			let file = parsed_file(&path, "shared = { marker = value }");
			let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], POLICY)
				.expect_err("invalid relative path must be rejected");

			assert_eq!(
				error,
				DefinitionModuleLoadError::InvalidRelativePath { path, reason }
			);
		}
	}

	#[test]
	fn same_file_duplicate_later_definition_wins() {
		let path = PathBuf::from("common/governments/definitions.txt");
		let file = parsed_file(
			&path,
			"shared = { marker = first }\nshared = { marker = second }",
		);

		let loaded = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], POLICY)
			.expect("module should load");

		assert_eq!(assignment_keys(&loaded.ast), vec!["shared"]);
		assert_eq!(marker_for(&loaded.ast, "shared"), "second");
		assert_eq!(loaded.definition_sources["shared"].statement_ordinal, 1);
	}

	#[test]
	fn preserve_all_keeps_repeated_wrapper_assignments_in_source_order() {
		let path = PathBuf::from("common/governments/definitions.txt");
		let file = parsed_file(
			&path,
			"modifier = { marker = first }\nmodifier = { marker = second }",
		);

		let loaded = load_definition_module(
			&[DefinitionModuleInput::new(&path, &file)],
			PRESERVE_DUPLICATES_POLICY,
		)
		.expect("module should preserve repeated wrappers");

		assert_eq!(assignment_keys(&loaded.ast), vec!["modifier", "modifier"]);
		assert!(loaded.duplicate_diagnostics.is_empty());
		assert_eq!(loaded.definition_sources["modifier"].statement_ordinal, 1);
	}

	#[test]
	fn cross_file_duplicate_later_path_wins() {
		let late_path = PathBuf::from("common/governments/20_late.txt");
		let early_path = PathBuf::from("common/governments/10_early.txt");
		let late_file = parsed_file(&late_path, "shared = { marker = late }");
		let early_file = parsed_file(&early_path, "shared = { marker = early }");

		let loaded = load_definition_module(
			&[
				DefinitionModuleInput::new(&late_path, &late_file),
				DefinitionModuleInput::new(&early_path, &early_file),
			],
			POLICY,
		)
		.expect("module should load");

		assert_eq!(marker_for(&loaded.ast, "shared"), "late");
		assert_eq!(
			loaded.definition_sources["shared"].path,
			"common/governments/20_late.txt"
		);
	}

	#[test]
	fn comments_do_not_become_definitions() {
		let path = PathBuf::from("common/governments/comments.txt");
		let file = parsed_file(
			&path,
			"# module comment\nalpha = { marker = kept }\n# trailing comment",
		);

		let loaded = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], POLICY)
			.expect("module should load");

		assert_eq!(assignment_keys(&loaded.ast), vec!["alpha"]);
		assert_eq!(loaded.definition_sources.len(), 1);
		assert!(loaded.definition_sources.contains_key("alpha"));
		assert!(loaded.duplicate_diagnostics.is_empty());
	}

	#[test]
	fn unsupported_top_level_content_fails_conservatively() {
		let path = PathBuf::from("common/governments/unsupported.txt");
		let file = parsed_file(&path, "standalone_item");

		let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], POLICY)
			.expect_err("bare top-level items must not be guessed into definitions");

		assert_eq!(
			error,
			DefinitionModuleLoadError::UnsupportedTopLevelStatement {
				path: "common/governments/unsupported.txt".to_string(),
				statement_ordinal: 0,
				kind: TopLevelStatementKind::Item,
			}
		);
	}

	#[test]
	fn source_mapping_and_duplicate_diagnostic_record_overwrite_event() {
		let previous_path = PathBuf::from("common/governments/01_previous.txt");
		let current_path = PathBuf::from("common/governments/02_current.txt");
		let previous_file = parsed_file(&previous_path, "shared = { marker = previous }");
		let current_file = parsed_file(
			&current_path,
			"other = { marker = other }\nshared = { marker = winner }",
		);

		let loaded = load_definition_module(
			&[
				DefinitionModuleInput::new(&current_path, &current_file),
				DefinitionModuleInput::new(&previous_path, &previous_file),
			],
			POLICY,
		)
		.expect("module should load");

		let current_source = DefinitionSource {
			path: "common/governments/02_current.txt".to_string(),
			statement_ordinal: 1,
			span: statement_span(&current_file.ast.statements[1]).clone(),
		};
		let previous_source = DefinitionSource {
			path: "common/governments/01_previous.txt".to_string(),
			statement_ordinal: 0,
			span: statement_span(&previous_file.ast.statements[0]).clone(),
		};
		assert_eq!(loaded.definition_sources["shared"], current_source);
		assert_eq!(loaded.duplicate_diagnostics.len(), 1);
		assert_eq!(loaded.duplicate_diagnostics[0].definition_key, "shared");
		assert_eq!(
			loaded.duplicate_diagnostics[0].previous_source,
			previous_source
		);
		assert_eq!(
			loaded.duplicate_diagnostics[0].current_source,
			current_source
		);
	}

	#[test]
	fn three_way_duplicate_diagnostics_record_each_overwrite_event() {
		let first_path = PathBuf::from("common/governments/01_first.txt");
		let second_path = PathBuf::from("common/governments/02_second.txt");
		let final_path = PathBuf::from("common/governments/03_final.txt");
		let first_file = parsed_file(&first_path, "shared = { marker = first }");
		let second_file = parsed_file(&second_path, "shared = { marker = second }");
		let final_file = parsed_file(&final_path, "shared = { marker = final }");

		let loaded = load_definition_module(
			&[
				DefinitionModuleInput::new(&final_path, &final_file),
				DefinitionModuleInput::new(&first_path, &first_file),
				DefinitionModuleInput::new(&second_path, &second_file),
			],
			POLICY,
		)
		.expect("module should load");

		assert_eq!(marker_for(&loaded.ast, "shared"), "final");
		assert_eq!(
			loaded.definition_sources["shared"].path,
			"common/governments/03_final.txt"
		);
		let overwrite_paths = loaded
			.duplicate_diagnostics
			.iter()
			.map(|diagnostic| {
				(
					diagnostic.previous_source.path.as_str(),
					diagnostic.current_source.path.as_str(),
				)
			})
			.collect::<Vec<_>>();
		assert_eq!(
			overwrite_paths,
			vec![
				(
					"common/governments/01_first.txt",
					"common/governments/02_second.txt",
				),
				(
					"common/governments/02_second.txt",
					"common/governments/03_final.txt",
				),
			]
		);
	}

	#[test]
	fn missing_assignment_key_fails_conservatively() {
		let path = PathBuf::from("common/governments/missing_key.txt");
		let mut file = parsed_file(&path, "placeholder = { marker = value }");
		let AstStatement::Assignment { key, .. } = &mut file.ast.statements[0] else {
			panic!("fixture must contain an assignment");
		};
		key.clear();

		let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], POLICY)
			.expect_err("missing keys must not be merged");

		assert_eq!(
			error,
			DefinitionModuleLoadError::MissingDefinitionKey {
				path: "common/governments/missing_key.txt".to_string(),
				statement_ordinal: 0,
			}
		);
	}

	#[test]
	fn parse_issues_are_loader_errors() {
		let path = PathBuf::from("common/governments/invalid.txt");
		let mut file = parsed_file(&path, "valid = { marker = value }");
		file.parse_issues.push(ParseIssue {
			mod_id: "test".to_string(),
			path: path.clone(),
			line: 1,
			column: 1,
			message: "synthetic parse issue".to_string(),
		});

		let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], POLICY)
			.expect_err("parse issues must stop module loading");

		assert_eq!(
			error,
			DefinitionModuleLoadError::ParseIssues {
				path: "common/governments/invalid.txt".to_string(),
				issue_count: 1,
			}
		);
	}
}
