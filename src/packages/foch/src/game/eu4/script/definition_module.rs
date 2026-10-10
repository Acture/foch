use super::super::content::{
	DefinitionFileOrder, DefinitionKeyPolicy, DefinitionModulePolicy, DuplicateDefinitionPolicy,
};
use super::ParsedScriptFile;
use super::parser::{AstFile, AstStatement, SpanRange};
use crate::model::{GamePath, GamePathBuf, has_fatal_parse_issue};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug)]
pub struct DefinitionModuleInput<'a> {
	pub path: &'a GamePath,
	pub file: &'a ParsedScriptFile,
	pub layer_ordinal: usize,
}

impl<'a> DefinitionModuleInput<'a> {
	pub fn new(path: &'a GamePath, file: &'a ParsedScriptFile) -> Self {
		Self {
			path,
			file,
			layer_ordinal: 0,
		}
	}

	pub fn with_layer_ordinal(mut self, layer_ordinal: usize) -> Self {
		self.layer_ordinal = layer_ordinal;
		self
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionSource {
	pub path: GamePathBuf,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefinitionModuleLoadError {
	InputPathMismatch {
		input_path: GamePathBuf,
		file_relative_path: GamePathBuf,
	},
	OutsideReplacementPrefix {
		path: GamePathBuf,
		replacement_prefix: GamePathBuf,
	},
	DuplicateInputPath {
		path: GamePathBuf,
	},
	ParseIssues {
		path: GamePathBuf,
		issue_count: usize,
	},
	UnsupportedTopLevelStatement {
		path: GamePathBuf,
		statement_ordinal: usize,
		kind: TopLevelStatementKind,
	},
	MissingDefinitionKey {
		path: GamePathBuf,
		statement_ordinal: usize,
	},
}

#[derive(Clone, Debug)]
struct ModuleInput<'a> {
	path: &'a GamePath,
	file: &'a ParsedScriptFile,
	layer_ordinal: usize,
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
	let namespace_prefix: &GamePath = policy.namespace_prefix;
	let mut ordered_inputs = inputs
		.iter()
		.map(|input| {
			if input.path != input.file.relative_path.as_game_path() {
				return Err(DefinitionModuleLoadError::InputPathMismatch {
					input_path: input.path.to_owned(),
					file_relative_path: input.file.relative_path.clone(),
				});
			}
			if input.path.strip_prefix(namespace_prefix).is_none() {
				return Err(DefinitionModuleLoadError::OutsideReplacementPrefix {
					path: input.path.to_owned(),
					replacement_prefix: namespace_prefix.to_owned(),
				});
			}
			Ok(ModuleInput {
				path: input.path,
				file: input.file,
				layer_ordinal: input.layer_ordinal,
			})
		})
		.collect::<Result<Vec<_>, DefinitionModuleLoadError>>()?;

	match policy.file_order {
		// Byte order of the canonical path text within each layer.
		DefinitionFileOrder::NormalizedPathAscending => {
			ordered_inputs.sort_by(|left, right| {
				(left.layer_ordinal, left.path).cmp(&(right.layer_ordinal, right.path))
			});
		}
	}
	let mut seen_paths = BTreeSet::new();
	for input in &ordered_inputs {
		if !seen_paths.insert(input.path) {
			return Err(DefinitionModuleLoadError::DuplicateInputPath {
				path: input.path.to_owned(),
			});
		}
	}

	let mut output_statements = Vec::<Option<AstStatement>>::new();
	let mut winners = BTreeMap::<String, WinningDefinition>::new();
	let mut duplicate_diagnostics = Vec::new();

	for input in ordered_inputs {
		if has_fatal_parse_issue(&input.file.parse_issues) {
			return Err(DefinitionModuleLoadError::ParseIssues {
				path: input.path.to_owned(),
				issue_count: input
					.file
					.parse_issues
					.iter()
					.filter(|issue| issue.is_fatal())
					.count(),
			});
		}

		for (statement_ordinal, statement) in input.file.ast.statements.iter().enumerate() {
			let AstStatement::Assignment { key, span, .. } = statement else {
				match statement {
					AstStatement::Comment { .. } => continue,
					AstStatement::Item { .. } => {
						return Err(DefinitionModuleLoadError::UnsupportedTopLevelStatement {
							path: input.path.to_owned(),
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
					path: input.path.to_owned(),
					statement_ordinal,
				});
			}
			let source = DefinitionSource {
				path: input.path.to_owned(),
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
			path: policy.output_path.to_owned(),
			statements: output_statements.into_iter().flatten().collect(),
		},
		definition_sources: winners
			.into_iter()
			.map(|(key, winner)| (key, winner.source))
			.collect(),
		duplicate_diagnostics,
	})
}

#[cfg(test)]
mod tests {
	use super::{
		DefinitionModuleInput, DefinitionModuleLoadError, DefinitionSource, TopLevelStatementKind,
		load_definition_module,
	};
	use crate::game::eu4::content::ScriptFileKind;
	use crate::game::eu4::content::{
		DefinitionFileOrder, DefinitionKeyPolicy, DefinitionModuleOutput, DefinitionModulePolicy,
		DuplicateDefinitionPolicy,
	};
	use crate::game::eu4::script::ParsedScriptFile;
	use crate::game::eu4::script::parser::{
		AstFile, AstStatement, AstValue, SpanRange, parse_clausewitz_content,
	};
	use crate::model::{
		GamePath, GamePathBuf, GamePathErrorKind, ParseIssue, SourceRepair, SourceRepairEdit,
		SourceRepairEvidence,
	};

	fn policy() -> DefinitionModulePolicy {
		DefinitionModulePolicy {
			definition_key: DefinitionKeyPolicy::AssignmentKey,
			file_order: DefinitionFileOrder::NormalizedPathAscending,
			duplicate_definitions: DuplicateDefinitionPolicy::LaterDefinitionWins,
			output_path: GamePath::new("common/governments/00_foch_governments.txt")
				.expect("valid game path"),
			namespace_prefix: GamePath::new("common/governments").expect("valid game path"),
			output_mode: DefinitionModuleOutput::ReplaceNamespace,
			policy_version: 1,
		}
	}

	fn preserve_duplicates_policy() -> DefinitionModulePolicy {
		DefinitionModulePolicy {
			duplicate_definitions: DuplicateDefinitionPolicy::PreserveAll,
			..policy()
		}
	}

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("valid game path")
	}

	fn parsed_file(path: &GamePath, source: &str) -> ParsedScriptFile {
		let parsed = parse_clausewitz_content(path, source);
		assert!(
			parsed.diagnostics.is_empty(),
			"test fixture must parse cleanly: {:?}",
			parsed.diagnostics
		);
		ParsedScriptFile {
			mod_id: "test".to_string(),
			path: None,
			relative_path: path.to_owned(),
			content_family: None,
			file_kind: ScriptFileKind::new("governments"),
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
		let z_path = game_path("common/governments/z.txt");
		let a_path = game_path("common/governments/a.txt");
		let z_file = parsed_file(&z_path, "z_government = { marker = z }");
		let a_file = parsed_file(&a_path, "a_government = { marker = a }");

		let loaded = load_definition_module(
			&[
				DefinitionModuleInput::new(&z_path, &z_file),
				DefinitionModuleInput::new(&a_path, &a_file),
			],
			policy(),
		)
		.expect("module should load");

		assert_eq!(
			assignment_keys(&loaded.ast),
			vec!["a_government", "z_government"]
		);
		assert_eq!(loaded.ast.path, policy().output_path);
	}

	#[test]
	fn path_order_is_the_byte_order_of_the_canonical_text() {
		// `-` sorts below `/`, so byte order loads `a-b.txt` before `a/b.txt`
		// where component order would load it after.
		let nested_path = game_path("common/governments/a/b.txt");
		let dashed_path = game_path("common/governments/a-b.txt");
		let nested_file = parsed_file(&nested_path, "shared = { marker = nested }");
		let dashed_file = parsed_file(&dashed_path, "shared = { marker = dashed }");

		let loaded = load_definition_module(
			&[
				DefinitionModuleInput::new(&nested_path, &nested_file),
				DefinitionModuleInput::new(&dashed_path, &dashed_file),
			],
			policy(),
		)
		.expect("module should load");

		assert_eq!(marker_for(&loaded.ast, "shared"), "nested");
		assert_eq!(
			loaded.duplicate_diagnostics[0].previous_source.path,
			dashed_path
		);
	}

	#[test]
	fn backslash_text_is_rejected_as_a_game_path_instead_of_read_as_separators() {
		// A caller holding `common\governments\z.txt` cannot hand it to the
		// loader: the text names one component containing `\`, which a Windows
		// host would split, so it has no portable identity.
		let error = GamePathBuf::parse(r"common\governments\z.txt")
			.expect_err("backslashes are not separators in a game path");
		assert_eq!(
			error.kind,
			GamePathErrorKind::ReservedCharacter {
				component: r"common\governments\z.txt".to_string(),
				character: '\\',
			}
		);
	}

	#[test]
	fn layer_order_precedes_lexical_path_order() {
		let earlier_path = game_path("common/governments/zzz_source.txt");
		let later_path = game_path("common/governments/00_compatch.txt");
		let earlier_file = parsed_file(&earlier_path, "shared = { marker = source }");
		let later_file = parsed_file(&later_path, "shared = { marker = compatch }");

		let loaded = load_definition_module(
			&[
				DefinitionModuleInput::new(&later_path, &later_file).with_layer_ordinal(1),
				DefinitionModuleInput::new(&earlier_path, &earlier_file).with_layer_ordinal(0),
			],
			policy(),
		)
		.expect("layered module should load");

		assert_eq!(marker_for(&loaded.ast, "shared"), "compatch");
		assert_eq!(
			loaded.definition_sources["shared"].path,
			game_path("common/governments/00_compatch.txt")
		);
	}

	#[test]
	fn lexical_aliases_are_rejected_as_game_paths_instead_of_collapsed() {
		// The loader used to collapse `//` and `.` itself. Such text is not a
		// game path, so it is rejected where it would enter the model.
		for (text, kind) in [
			(
				"common//./governments///definitions.txt",
				GamePathErrorKind::EmptyComponent,
			),
			(
				"common/./governments/definitions.txt",
				GamePathErrorKind::CurrentComponent,
			),
		] {
			assert_eq!(
				GamePathBuf::parse(text).expect_err(text).kind,
				kind,
				"{text}"
			);
		}
	}

	#[test]
	fn normalized_input_path_must_match_file_relative_path() {
		let input_path = game_path("common/governments/input.txt");
		let file_path = game_path("common/governments/file.txt");
		let file = parsed_file(&file_path, "shared = { marker = value }");

		let error =
			load_definition_module(&[DefinitionModuleInput::new(&input_path, &file)], policy())
				.expect_err("mismatched caller and parsed-file paths must be rejected");

		assert_eq!(
			error,
			DefinitionModuleLoadError::InputPathMismatch {
				input_path: input_path.clone(),
				file_relative_path: file_path.clone(),
			}
		);
	}

	#[test]
	fn input_must_belong_to_the_policy_replacement_prefix() {
		let path = game_path("events/not_a_government.txt");
		let file = parsed_file(&path, "event_definition = { marker = value }");

		let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], policy())
			.expect_err("definition modules must not absorb files from another runtime prefix");

		assert_eq!(
			error,
			DefinitionModuleLoadError::OutsideReplacementPrefix {
				path: path.clone(),
				replacement_prefix: game_path("common/governments"),
			}
		);
	}

	#[test]
	fn the_prefix_directory_itself_is_outside_the_replacement_prefix() {
		let path = game_path("common/governments");
		let file = parsed_file(&path, "shared = { marker = value }");

		let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], policy())
			.expect_err("only files inside the prefix directory belong to the module");

		assert_eq!(
			error,
			DefinitionModuleLoadError::OutsideReplacementPrefix {
				path: path.clone(),
				replacement_prefix: path,
			}
		);
	}

	#[test]
	fn repeated_paths_are_duplicate_inputs() {
		let path = game_path("common/governments/definitions.txt");
		let first_file = parsed_file(&path, "first = { marker = first }");
		let second_file = parsed_file(&path, "second = { marker = second }");

		let error = load_definition_module(
			&[
				DefinitionModuleInput::new(&path, &first_file),
				DefinitionModuleInput::new(&path, &second_file),
			],
			policy(),
		)
		.expect_err("one path must not create two module inputs");

		assert_eq!(
			error,
			DefinitionModuleLoadError::DuplicateInputPath { path }
		);
	}

	#[test]
	fn invalid_relative_paths_are_rejected_as_game_paths() {
		for (text, kind) in [
			("", GamePathErrorKind::Empty),
			(
				"common/governments/../definitions.txt",
				GamePathErrorKind::ParentComponent,
			),
			(
				"/common/governments/definitions.txt",
				GamePathErrorKind::NotRelative,
			),
			(
				r"C:\common\governments\definitions.txt",
				GamePathErrorKind::ReservedCharacter {
					component: r"C:\common\governments\definitions.txt".to_string(),
					character: ':',
				},
			),
			(
				"C:/common/governments/definitions.txt",
				GamePathErrorKind::ReservedCharacter {
					component: "C:".to_string(),
					character: ':',
				},
			),
		] {
			assert_eq!(
				GamePathBuf::parse(text).expect_err(text).kind,
				kind,
				"{text}"
			);
		}
	}

	#[test]
	fn same_file_duplicate_later_definition_wins() {
		let path = game_path("common/governments/definitions.txt");
		let file = parsed_file(
			&path,
			"shared = { marker = first }\nshared = { marker = second }",
		);

		let loaded = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], policy())
			.expect("module should load");

		assert_eq!(assignment_keys(&loaded.ast), vec!["shared"]);
		assert_eq!(marker_for(&loaded.ast, "shared"), "second");
		assert_eq!(loaded.definition_sources["shared"].statement_ordinal, 1);
	}

	#[test]
	fn preserve_all_keeps_repeated_wrapper_assignments_in_source_order() {
		let path = game_path("common/governments/definitions.txt");
		let file = parsed_file(
			&path,
			"modifier = { marker = first }\nmodifier = { marker = second }",
		);

		let loaded = load_definition_module(
			&[DefinitionModuleInput::new(&path, &file)],
			preserve_duplicates_policy(),
		)
		.expect("module should preserve repeated wrappers");

		assert_eq!(assignment_keys(&loaded.ast), vec!["modifier", "modifier"]);
		assert!(loaded.duplicate_diagnostics.is_empty());
		assert_eq!(loaded.definition_sources["modifier"].statement_ordinal, 1);
	}

	#[test]
	fn cross_file_duplicate_later_path_wins() {
		let late_path = game_path("common/governments/20_late.txt");
		let early_path = game_path("common/governments/10_early.txt");
		let late_file = parsed_file(&late_path, "shared = { marker = late }");
		let early_file = parsed_file(&early_path, "shared = { marker = early }");

		let loaded = load_definition_module(
			&[
				DefinitionModuleInput::new(&late_path, &late_file),
				DefinitionModuleInput::new(&early_path, &early_file),
			],
			policy(),
		)
		.expect("module should load");

		assert_eq!(marker_for(&loaded.ast, "shared"), "late");
		assert_eq!(
			loaded.definition_sources["shared"].path,
			game_path("common/governments/20_late.txt")
		);
	}

	#[test]
	fn comments_do_not_become_definitions() {
		let path = game_path("common/governments/comments.txt");
		let file = parsed_file(
			&path,
			"# module comment\nalpha = { marker = kept }\n# trailing comment",
		);

		let loaded = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], policy())
			.expect("module should load");

		assert_eq!(assignment_keys(&loaded.ast), vec!["alpha"]);
		assert_eq!(loaded.definition_sources.len(), 1);
		assert!(loaded.definition_sources.contains_key("alpha"));
		assert!(loaded.duplicate_diagnostics.is_empty());
	}

	#[test]
	fn unsupported_top_level_content_fails_conservatively() {
		let path = game_path("common/governments/unsupported.txt");
		let file = parsed_file(&path, "standalone_item");

		let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], policy())
			.expect_err("bare top-level items must not be guessed into definitions");

		assert_eq!(
			error,
			DefinitionModuleLoadError::UnsupportedTopLevelStatement {
				path: game_path("common/governments/unsupported.txt"),
				statement_ordinal: 0,
				kind: TopLevelStatementKind::Item,
			}
		);
	}

	#[test]
	fn source_mapping_and_duplicate_diagnostic_record_overwrite_event() {
		let previous_path = game_path("common/governments/01_previous.txt");
		let current_path = game_path("common/governments/02_current.txt");
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
			policy(),
		)
		.expect("module should load");

		let current_source = DefinitionSource {
			path: game_path("common/governments/02_current.txt"),
			statement_ordinal: 1,
			span: statement_span(&current_file.ast.statements[1]).clone(),
		};
		let previous_source = DefinitionSource {
			path: game_path("common/governments/01_previous.txt"),
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
		let first_path = game_path("common/governments/01_first.txt");
		let second_path = game_path("common/governments/02_second.txt");
		let final_path = game_path("common/governments/03_final.txt");
		let first_file = parsed_file(&first_path, "shared = { marker = first }");
		let second_file = parsed_file(&second_path, "shared = { marker = second }");
		let final_file = parsed_file(&final_path, "shared = { marker = final }");

		let loaded = load_definition_module(
			&[
				DefinitionModuleInput::new(&final_path, &final_file),
				DefinitionModuleInput::new(&first_path, &first_file),
				DefinitionModuleInput::new(&second_path, &second_file),
			],
			policy(),
		)
		.expect("module should load");

		assert_eq!(marker_for(&loaded.ast, "shared"), "final");
		assert_eq!(
			loaded.definition_sources["shared"].path,
			game_path("common/governments/03_final.txt")
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
		let path = game_path("common/governments/missing_key.txt");
		let mut file = parsed_file(&path, "placeholder = { marker = value }");
		let AstStatement::Assignment { key, .. } = &mut file.ast.statements[0] else {
			panic!("fixture must contain an assignment");
		};
		key.clear();

		let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], policy())
			.expect_err("missing keys must not be merged");

		assert_eq!(
			error,
			DefinitionModuleLoadError::MissingDefinitionKey {
				path: game_path("common/governments/missing_key.txt"),
				statement_ordinal: 0,
			}
		);
	}

	#[test]
	fn a_repaired_parse_issue_does_not_stop_module_loading() {
		let path = game_path("common/governments/repaired.txt");
		let mut file = parsed_file(&path, "valid = { marker = value }");
		file.parse_issues.push(ParseIssue {
			mod_id: "test".to_string(),
			path: path.clone(),
			line: 2,
			column: 1,
			message: "unexpected closing brace without an opening block".to_string(),
			repair: Some(SourceRepair {
				edit: SourceRepairEdit::RemovedClosingBrace,
				evidence: SourceRepairEvidence::OnlyReading,
			}),
		});

		let module = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], policy())
			.expect("a repaired file loads");

		assert_eq!(module.definition_sources.len(), 1);
	}

	#[test]
	fn parse_issues_are_loader_errors() {
		let path = game_path("common/governments/invalid.txt");
		let mut file = parsed_file(&path, "valid = { marker = value }");
		file.parse_issues.push(ParseIssue {
			mod_id: "test".to_string(),
			path: path.clone(),
			line: 1,
			column: 1,
			message: "synthetic parse issue".to_string(),
			repair: None,
		});

		let error = load_definition_module(&[DefinitionModuleInput::new(&path, &file)], policy())
			.expect_err("parse issues must stop module loading");

		assert_eq!(
			error,
			DefinitionModuleLoadError::ParseIssues {
				path: game_path("common/governments/invalid.txt"),
				issue_count: 1,
			}
		);
	}
}
