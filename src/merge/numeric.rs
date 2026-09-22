//! Rewrite script numbers into the spelling EU4's own reader implies.
//!
//! EU4 reads a script number far more coarsely than byte comparison: three
//! truncated decimals for a `float` field, `atoi` for an `int` one (the
//! evidence is in [`crate::game::eu4::coercion`]). So mods can write one value
//! several ways — `0.5`, `0.50` and `0.500` are the same number to the game —
//! and comparing the text reports a difference that does not exist.
//!
//! This runs on the AST before the merge, beside
//! `canonicalize_boolean_or_definitions`, rather than inside tree
//! normalization or at conflict resolution. The AST is what every identity in
//! the merge is ultimately derived from: `assignment_anchor` and
//! `value_fingerprint` read `AstValue` directly, and the normalized tree's
//! hashes are computed from the values it carries. Canonicalizing here means
//! the matcher, the hashes, the anchors, definition provenance and the
//! semantic-equivalence check all agree by construction, instead of each
//! needing to learn the same rule.
//!
//! Which reader applies is a property of the *field*, not of the text, so this
//! acts only where the CWT schema states the field's type. Where the schema is
//! silent the text is left exactly as written — the tree then says "I make no
//! claim about this value" rather than asserting one from a token's shape.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use crate::game::eu4::coercion::{canonical_float_text, canonical_int_text};
use crate::game::eu4::script::parser::{
	AstFile, AstStatement, AstValue, ScalarValue, parse_clausewitz_content,
};
use crate::game::schema::query::{CwtQuery, RuleContext, SchemaScalarType};

/// Rewrite every schema-typed number in `file` under the installed schema.
///
/// The merge-quality harness scores generated output against a human
/// compatibility patch, and must compare the representation the merge
/// produces rather than a second rule of its own, or a compatch that wrote
/// `0.50` would count as differing from output that says `0.500`.
///
/// `relative_path` is a semantic input, not a label: the schema binds a root
/// type by matching a game-relative prefix such as `common/static_modifiers`,
/// so an absolute path on disk matches nothing and every number is left as
/// written. Callers that hold a scratch path must pass the game-relative one.
pub fn canonicalize_numeric_values_with_active_schema(
	relative_path: &Path,
	file: &AstFile,
) -> AstFile {
	canonicalize_numeric_values_at(relative_path, file, crate::game::eu4::cwt::rule_engine())
}

/// Rewrite every schema-typed number in `file` into its canonical spelling.
///
/// A file whose path binds no root type, or a build with no schema installed,
/// comes back unchanged.
pub(crate) fn canonicalize_numeric_values(file: &AstFile, schema: Option<&CwtQuery>) -> AstFile {
	canonicalize_numeric_values_at(&file.path.clone(), file, schema)
}

fn canonicalize_numeric_values_at(
	relative_path: &Path,
	file: &AstFile,
	schema: Option<&CwtQuery>,
) -> AstFile {
	let Some(schema) = schema else {
		return file.clone();
	};
	let mut file = file.clone();
	let mut walker = NumericWalker {
		schema,
		file_path: relative_path,
		contexts: HashMap::new(),
		edits: Vec::new(),
	};
	walker.visit(&mut file.statements, &mut Vec::new());
	file
}

/// Rewrite the numbers in `source` and leave every other byte untouched.
///
/// Re-emitting the parsed file would work for the values but would also
/// reformat the comments and whitespace around them, and the merge-quality
/// harness measures text similarity against a human patch — reformatting would
/// swamp the signal it is trying to read. Replacing byte ranges keeps the
/// comparison about content.
pub fn canonicalize_numeric_text(relative_path: &Path, source: &str) -> String {
	canonicalize_numeric_text_with(relative_path, source, crate::game::eu4::cwt::rule_engine())
}

fn canonicalize_numeric_text_with(
	relative_path: &Path,
	source: &str,
	schema: Option<&CwtQuery>,
) -> String {
	let Some(schema) = schema else {
		return source.to_string();
	};
	let parsed = parse_clausewitz_content(relative_path.to_path_buf(), source);
	if !parsed.diagnostics.is_empty() {
		return source.to_string();
	}
	let mut file = parsed.ast;
	let mut walker = NumericWalker {
		schema,
		file_path: relative_path,
		contexts: HashMap::new(),
		edits: Vec::new(),
	};
	walker.visit(&mut file.statements, &mut Vec::new());
	let mut edits = walker.edits;
	// Apply from the end so earlier offsets stay valid.
	edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
	let mut source = source.to_string();
	for (range, canonical) in edits {
		if source.get(range.clone()).is_some() {
			source.replace_range(range, &canonical);
		}
	}
	source
}

struct NumericWalker<'a> {
	schema: &'a CwtQuery,
	file_path: &'a Path,
	/// Resolved rule contexts by ancestor key chain.
	///
	/// One entry per block the walk enters, not one per number: a definition
	/// with fifty modifier fields costs one binding, then fifty cheap field
	/// lookups inside it.
	contexts: HashMap<Vec<String>, Vec<RuleContext<'a>>>,
	/// Byte ranges rewritten, for callers that edit the source text rather
	/// than the tree.
	edits: Vec<(Range<usize>, String)>,
}

impl<'a> NumericWalker<'a> {
	fn visit(&mut self, statements: &mut [AstStatement], chain: &mut Vec<String>) {
		for statement in statements {
			let AstStatement::Assignment { key, value, .. } = statement else {
				// A list item is matched by position and its text is not this
				// field's value, so the field's declared type says nothing
				// about it.
				continue;
			};
			match value {
				AstValue::Block { items, .. } => {
					chain.push(key.clone());
					self.visit(items, chain);
					chain.pop();
				}
				AstValue::Scalar {
					value: ScalarValue::Number(text),
					span,
				} => {
					if let Some(canonical) = self.canonical_text(chain, key, text) {
						self.edits
							.push((span.start.offset..span.end.offset, canonical.clone()));
						*text = canonical;
					}
				}
				AstValue::Scalar { .. } => {}
			}
		}
	}

	fn canonical_text(&mut self, chain: &[String], key: &str, text: &str) -> Option<String> {
		let scalar_type = self.scalar_type(chain, key)?;
		match scalar_type {
			SchemaScalarType::Float { .. } => canonical_float_text(text),
			SchemaScalarType::Int { .. } => canonical_int_text(text),
			// `CToken::GetBool` compares against the exact lowercase `yes`, so
			// every other spelling reads as false and they would all collapse
			// together — including the mis-spellings the editor reports as
			// errors. A boolean is never rewritten here.
			SchemaScalarType::Bool => None,
		}
		.filter(|canonical| canonical != text)
	}

	/// The primitive the schema declares for `key` inside `chain`, or `None`
	/// wherever the schema is not explicit.
	fn scalar_type(&mut self, chain: &[String], key: &str) -> Option<SchemaScalarType> {
		let contexts = self.contexts_for(chain);
		let mut resolved: Option<SchemaScalarType> = None;
		for context in contexts {
			for field_match in self.schema.bind_field_matches(context, key) {
				// The type has to come from the match, not from its field: an
				// alias-bound field's wildcard carries the
				// `alias_match_left[...]` marker rather than a type.
				let Some(scalar_type) = SchemaScalarType::from_rule_value(field_match.value())
				else {
					// A matching rule that is not a primitive scalar means the
					// key is overloaded here; the schema is not explicit.
					return None;
				};
				match resolved {
					None => resolved = Some(scalar_type),
					Some(previous) if previous.is_same_primitive(scalar_type) => {}
					Some(_) => return None,
				}
			}
		}
		resolved
	}

	/// Every reading of `chain` the schema accepts.
	///
	/// A definition's own instance key leads the chain under most root types
	/// and is absorbed by `skip_root_key` under others, and which applies is
	/// not recoverable from the chain alone, so both readings are kept and
	/// `scalar_type` requires them to agree.
	fn contexts_for(&mut self, chain: &[String]) -> Vec<RuleContext<'a>> {
		if !self.contexts.contains_key(chain) {
			let mut readings: Vec<&[String]> = vec![chain];
			if let Some((_, rest)) = chain.split_first() {
				readings.push(rest);
			}
			let contexts = readings
				.into_iter()
				.filter_map(|reading| {
					let reading = reading.iter().map(String::as_str).collect::<Vec<_>>();
					self.schema.bind_context(self.file_path, &reading)
				})
				.collect::<Vec<_>>();
			self.contexts.insert(chain.to_vec(), contexts);
		}
		self.contexts[chain].clone()
	}
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::path::PathBuf;

	use tempfile::TempDir;

	use super::*;
	use crate::game::eu4::script::emit::emit_clausewitz_statements;
	use crate::game::eu4::script::parser::parse_clausewitz_content;
	use crate::game::schema::{CwtSchema, CwtSource};

	const SCHEMA: &str = r#"
		types = {
			type[thing] = { path = "game/common/things" }
		}

		thing = {
			alias_name[modifier] = alias_match_left[modifier]
			untyped = scalar
			nested = { depth = float }
		}

		alias[modifier:upkeep] = float
		alias[modifier:slots] = int
		alias[modifier:enabled] = bool
	"#;

	fn schema() -> CwtSchema {
		let root = TempDir::new().expect("create schema directory");
		fs::write(root.path().join("things.cwt"), SCHEMA).expect("write schema");
		CwtSchema::load_with_cache(
			root.path(),
			CwtSource::UserProvided {
				path: root.path().to_path_buf(),
			},
			None,
		)
		.expect("load schema")
	}

	fn canonicalize(path: &str, source: &str, schema: Option<&CwtQuery>) -> String {
		let parsed = parse_clausewitz_content(PathBuf::from(path), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		let canonical = canonicalize_numeric_values(&parsed.ast, schema);
		emit_clausewitz_statements(&canonical.statements).expect("emit")
	}

	#[test]
	fn spellings_of_one_value_become_one_string() {
		let schema = schema();
		for written in ["0.5", "0.50", "0.500"] {
			assert!(
				canonicalize(
					"common/things/example.txt",
					&format!("a_thing = {{ upkeep = {written} }}\n"),
					Some(schema.facts()),
				)
				.contains("upkeep = 0.500"),
				"`{written}` should canonicalize"
			);
		}
	}

	#[test]
	fn a_fourth_decimal_is_truncated_the_way_the_reader_truncates_it() {
		let schema = schema();
		for written in ["0.1234", "0.1239"] {
			assert!(
				canonicalize(
					"common/things/example.txt",
					&format!("a_thing = {{ upkeep = {written} }}\n"),
					Some(schema.facts()),
				)
				.contains("upkeep = 0.123"),
				"`{written}` should truncate"
			);
		}
	}

	#[test]
	fn an_int_field_uses_the_integer_reader() {
		let schema = schema();
		let output = canonicalize(
			"common/things/example.txt",
			"a_thing = { slots = 3.4 }\n",
			Some(schema.facts()),
		);

		assert!(output.contains("slots = 3"), "{output}");
		assert!(!output.contains("3.4"), "{output}");
	}

	#[test]
	fn a_distinct_value_keeps_its_own_spelling() {
		let schema = schema();
		let output = canonicalize(
			"common/things/example.txt",
			"a_thing = { upkeep = 0.6 }\n",
			Some(schema.facts()),
		);

		assert!(output.contains("upkeep = 0.600"), "{output}");
	}

	#[test]
	fn nested_blocks_are_reached() {
		let schema = schema();
		let output = canonicalize(
			"common/things/example.txt",
			"a_thing = { nested = { depth = 2.5 } }\n",
			Some(schema.facts()),
		);

		assert!(output.contains("depth = 2.500"), "{output}");
	}

	#[test]
	fn a_field_the_schema_does_not_type_is_left_exactly_as_written() {
		let schema = schema();
		for source in [
			"a_thing = { untyped = 0.50 }\n",
			"a_thing = { not_in_the_schema = 0.50 }\n",
			// A boolean is never rewritten: the game reads only the exact
			// lowercase `yes` as true.
			"a_thing = { enabled = yes }\n",
		] {
			assert_eq!(
				canonicalize("common/things/example.txt", source, Some(schema.facts())),
				canonicalize("common/things/example.txt", source, None),
				"{source}"
			);
		}
	}

	#[test]
	fn a_path_the_schema_does_not_cover_is_left_alone() {
		let schema = schema();
		let source = "a_thing = { upkeep = 0.50 }\n";

		assert!(
			canonicalize("common/elsewhere/example.txt", source, Some(schema.facts()))
				.contains("upkeep = 0.50"),
		);
	}

	#[test]
	fn a_list_item_is_not_a_field_value() {
		// Items are matched by position, so the enclosing field's type says
		// nothing about their text.
		let schema = schema();
		let output = canonicalize(
			"common/things/example.txt",
			"a_thing = { upkeep = { 0.50 0.1239 } }\n",
			Some(schema.facts()),
		);

		assert!(output.contains("0.50"), "{output}");
		assert!(output.contains("0.1239"), "{output}");
	}

	#[test]
	fn no_schema_leaves_every_number_as_written() {
		let source = "a_thing = { upkeep = 0.50 slots = 3.4 }\n";

		assert!(canonicalize("common/things/example.txt", source, None).contains("upkeep = 0.50"));
	}

	#[test]
	fn rewriting_text_touches_only_the_numbers() {
		let source = "# a leading note\n\
			a_thing = {\n\
			\t# why this value\n\
			\tupkeep    =    0.5   # trailing note\n\
			\tuntyped = 0.50\n\
			}\n";

		let schema = schema();
		let rewritten = canonicalize_numeric_text_with(
			Path::new("common/things/example.txt"),
			source,
			Some(schema.facts()),
		);

		// Only the schema-typed number moves; the comments, the odd spacing
		// and the untyped value are byte-for-byte what they were.
		assert_eq!(
			rewritten,
			source.replace("0.5   #", "0.500   #"),
			"{rewritten}"
		);
	}

	#[test]
	fn rewriting_text_is_a_no_op_where_nothing_is_typed() {
		for source in ["a_thing = { untyped = 0.50 }\n", "not_parseable = {\n"] {
			assert_eq!(
				canonicalize_numeric_text_with(
					Path::new("common/things/example.txt"),
					source,
					Some(schema().facts())
				),
				source
			);
		}
	}

	#[test]
	fn canonicalizing_twice_changes_nothing_the_second_time() {
		let schema = schema();
		let once = canonicalize(
			"common/things/example.txt",
			"a_thing = { upkeep = 0.5 slots = 3.4 }\n",
			Some(schema.facts()),
		);

		assert_eq!(
			canonicalize("common/things/example.txt", &once, Some(schema.facts())),
			once
		);
	}
}

#[cfg(test)]
mod coverage_probe {
	use std::path::{Path, PathBuf};

	use super::*;
	use crate::game::eu4::script::parser::parse_clausewitz_content;

	fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
		let Ok(entries) = std::fs::read_dir(dir) else {
			return;
		};
		for entry in entries.flatten() {
			let path = entry.path();
			if path.is_dir() {
				walk(&path, out);
			} else if path.extension().is_some_and(|e| e == "txt") {
				out.push(path);
			}
		}
	}

	fn redundant(text: &str) -> bool {
		match text.split_once('.') {
			Some((_, fraction)) => {
				!fraction.is_empty()
					&& fraction.bytes().all(|b| b.is_ascii_digit())
					&& (fraction.ends_with('0') || fraction.len() > 3)
			}
			None => false,
		}
	}

	/// Count redundantly spelled numbers, and how many of them the transform
	/// actually rewrote. The canonical form itself ends in zeros, so the count
	/// has to come from comparing the two trees rather than re-testing shape.
	fn count(
		before: &[AstStatement],
		after: &[AstStatement],
		redundant_total: &mut usize,
		changed: &mut usize,
		rewritten: &mut usize,
	) {
		for (before, after) in before.iter().zip(after) {
			let (
				AstStatement::Assignment { value: before, .. },
				AstStatement::Assignment { value: after, .. },
			) = (before, after)
			else {
				continue;
			};
			match (before, after) {
				(AstValue::Block { items: before, .. }, AstValue::Block { items: after, .. }) => {
					count(before, after, redundant_total, changed, rewritten);
				}
				(
					AstValue::Scalar {
						value: ScalarValue::Number(before),
						..
					},
					AstValue::Scalar {
						value: ScalarValue::Number(after),
						..
					},
				) => {
					if redundant(before) {
						*redundant_total += 1;
						if before != after {
							*changed += 1;
						}
					}
					if before != after {
						*rewritten += 1;
					}
				}
				_ => {}
			}
		}
	}

	#[test]
	#[ignore = "P-695 coverage probe: needs EU4_ROOT and the vendored CWT schema"]
	fn measure_coverage_against_vanilla() {
		let schema = crate::game::eu4::cwt::rule_engine().expect("schema");
		let root = PathBuf::from(std::env::var("EU4_ROOT").expect("EU4_ROOT"));
		let mut files = Vec::new();
		for directory in ["common", "events", "decisions", "missions"] {
			walk(&root.join(directory), &mut files);
		}
		files.sort();

		let (mut parsed_files, mut redundant_total, mut changed, mut rewritten) =
			(0usize, 0usize, 0usize, 0usize);
		for file in &files {
			let Ok(source) = std::fs::read_to_string(file) else {
				continue;
			};
			let relative = file.strip_prefix(&root).unwrap().to_path_buf();
			let parsed = parse_clausewitz_content(relative, &source);
			if !parsed.diagnostics.is_empty() {
				continue;
			}
			parsed_files += 1;
			let canonical = canonicalize_numeric_values(&parsed.ast, Some(schema));
			count(
				&parsed.ast.statements,
				&canonical.statements,
				&mut redundant_total,
				&mut changed,
				&mut rewritten,
			);
		}
		eprintln!(
			"P695 COVERAGE files={parsed_files} redundantly_spelled={redundant_total} \
			 of_those_covered={changed} numbers_rewritten_total={rewritten}"
		);
	}
}
