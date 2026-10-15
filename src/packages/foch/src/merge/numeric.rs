//! Give each script number one spelling per value for the merge, then write
//! the result back the way the mods spelled it.
//!
//! The merge compares a schema-typed number by its exact decimal value:
//! `0.5`, `0.50` and `0.500` are one value, while `0.0125` and `0.012` are
//! two, and so are `3` and `3.4` in an `int` field. Which digits the game
//! keeps differs from field to field (see [`crate::game::eu4::coercion`]), so
//! no reader's rounding takes part in deciding whether two mods agree.
//!
//! Three passes share one walk:
//!
//! * Before the merge, every schema-typed number becomes its shortest exact
//!   spelling, beside `canonicalize_boolean_or_definitions` rather than inside
//!   tree normalization or at conflict resolution. The AST is what every
//!   identity in the merge is derived from: `assignment_anchor` and
//!   `value_fingerprint` read `AstValue` directly, and the normalized tree's
//!   hashes come from the values it carries. Rewriting here means the
//!   matcher, the hashes, the anchors, definition provenance and the
//!   semantic-equivalence check all agree by construction. The pass also
//!   remembers the longest spelling each input used at each place, and
//!   [`NumberSpellings::restore`] pads the merged value back to it, so `0.5`
//!   and `0.50` at one place merge to `0.50`.
//! * Before the file is written, [`drop_unread_decimals`] removes digits past
//!   the third where the game was seen to ignore them, unless the project
//!   turns that off.
//! * The merge-quality harness compares in a form that applies both, so a
//!   human patch that spelled a value differently, or kept a digit the game
//!   ignores, is not counted as different content.
//!
//! What a number means is a property of the *field*, not of the text, so the
//! exact-value pass acts only where the CWT schema states the field's type.
//! Where the schema is silent the text is left exactly as written.

use std::collections::HashMap;
use std::ops::Range;

use crate::game::eu4::coercion::{
	alias_reads_three_decimals, alias_value_reads_three_decimals, drop_unread_fraction_digits,
};
use crate::game::eu4::script::parser::{
	AstFile, AstStatement, AstValue, ScalarValue, ScriptSyntax, parse_clausewitz_statements,
};
use crate::game::schema::query::{CompiledAliasCategory, CwtQuery, RuleContext, SchemaScalarType};
use crate::model::GamePath;

/// Bring every number in `file` to the form the merge-quality harness compares.
///
/// The harness scores generated output against a human compatibility patch,
/// and must compare the representation the merge produces rather than a
/// second rule of its own, or a compatch that wrote `0.500` would count as
/// differing from output that says `0.5`, and one that kept a digit the game
/// ignores would count as differing from output that dropped it.
///
/// `relative_path` is a semantic input, not a label: the schema binds a root
/// type by matching a game-relative prefix such as `common/static_modifiers`,
/// so an absolute path on disk matches nothing and every number is left as
/// written. Callers that hold a scratch path must pass the game-relative one.
pub fn canonicalize_numeric_values_with_active_schema(
	relative_path: &GamePath,
	file: &AstFile,
) -> AstFile {
	let mut file = file.clone();
	NumericWalker::new(
		crate::game::eu4::cwt::rule_engine(),
		relative_path,
		Pass::Compare,
	)
	.visit(&mut file.statements, &mut Vec::new());
	file
}

/// Rewrite every schema-typed number in `file` into its shortest exact
/// spelling, and note in `spellings` how each input wrote it.
///
/// A file whose path binds no root type, or a call with no schema, comes back
/// unchanged.
pub(crate) fn canonicalize_numeric_values(
	file: &AstFile,
	schema: Option<&CwtQuery>,
	spellings: &mut NumberSpellings,
) -> AstFile {
	let Some(schema) = schema else {
		return file.clone();
	};
	let mut canonical = file.clone();
	let mut walker = NumericWalker::new(schema, &file.path, Pass::Exact);
	walker.visit(&mut canonical.statements, &mut Vec::new());
	spellings.absorb(walker.spellings);
	canonical
}

/// Drop the fraction digits past the third wherever the game was seen to read
/// only three, leaving every other number as it is.
///
/// This is the last change before a merged file is written. The digits it
/// removes are ones the game discards on load, so the file means the same
/// thing to the game and no longer suggests a precision it does not have.
pub(crate) fn drop_unread_decimals(
	relative_path: &GamePath,
	statements: &mut [AstStatement],
	schema: &CwtQuery,
) {
	NumericWalker::new(schema, relative_path, Pass::DropUnread).visit(statements, &mut Vec::new());
}

/// Rewrite the numbers in `source` into the compared form and leave every
/// other byte untouched.
///
/// Re-emitting the parsed file would work for the values but would also
/// reformat the comments and whitespace around them, and the merge-quality
/// harness measures text similarity against a human patch — reformatting would
/// swamp the signal it is trying to read. Replacing byte ranges keeps the
/// comparison about content.
pub fn canonicalize_numeric_text(relative_path: &GamePath, source: &str) -> String {
	canonicalize_numeric_text_with(
		relative_path,
		source,
		Some(crate::game::eu4::cwt::rule_engine()),
	)
}

fn canonicalize_numeric_text_with(
	relative_path: &GamePath,
	source: &str,
	schema: Option<&CwtQuery>,
) -> String {
	let Some(schema) = schema else {
		return source.to_string();
	};
	let syntax = ScriptSyntax::for_game_path(relative_path);
	let parsed = parse_clausewitz_statements(syntax, source);
	if !parsed.diagnostics.is_empty() {
		return source.to_string();
	}
	let mut statements = parsed.statements;
	let mut walker = NumericWalker::new(schema, relative_path, Pass::Compare);
	walker.visit(&mut statements, &mut Vec::new());
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

/// The most fraction digits any input used for one value at one place.
///
/// A place is the keys of the blocks around a number followed by its own key,
/// so only spellings of the same field in the same definition compete; a
/// `0.50` elsewhere in the file says nothing about how this one was written.
#[derive(Debug, Default)]
pub(crate) struct NumberSpellings {
	digits: HashMap<Vec<String>, HashMap<String, usize>>,
}

impl NumberSpellings {
	fn record(&mut self, chain: &[String], key: &str, exact: &str, written: &str) {
		let written_digits = fraction_digits(written);
		if written_digits <= fraction_digits(exact) {
			return;
		}
		let mut place = chain.to_vec();
		place.push(key.to_string());
		let digits = self
			.digits
			.entry(place)
			.or_default()
			.entry(exact.to_string())
			.or_default();
		*digits = (*digits).max(written_digits);
	}

	fn absorb(&mut self, other: NumberSpellings) {
		for (place, values) in other.digits {
			let known = self.digits.entry(place).or_default();
			for (exact, digits) in values {
				let known = known.entry(exact).or_default();
				*known = (*known).max(digits);
			}
		}
	}

	/// Pad each merged number with the trailing zeros the longest input
	/// spelling of it at the same place had.
	pub(crate) fn restore(&self, statements: &mut [AstStatement]) {
		self.restore_within(&[], statements);
	}

	/// [`Self::restore`] for statements that sit inside the blocks `chain`
	/// names, rather than at the top of a file.
	pub(crate) fn restore_within(&self, chain: &[String], statements: &mut [AstStatement]) {
		if !self.digits.is_empty() {
			self.restore_in(statements, &mut chain.to_vec());
		}
	}

	fn restore_in(&self, statements: &mut [AstStatement], chain: &mut Vec<String>) {
		for statement in statements {
			let AstStatement::Assignment { key, value, .. } = statement else {
				continue;
			};
			chain.push(key.clone());
			match value {
				AstValue::Block { items, .. } => self.restore_in(items, chain),
				AstValue::Scalar {
					value: ScalarValue::Number(text),
					..
				} => {
					// Only an exact spelling is ever recorded, so a text that
					// finds an entry is already the exact form of its value.
					if let Some(padded) = self
						.digits
						.get(chain.as_slice())
						.and_then(|values| values.get(text.as_str()))
						.and_then(|digits| pad_fraction(text, *digits))
					{
						*text = padded;
					}
				}
				AstValue::Scalar { .. } => {}
			}
			chain.pop();
		}
	}
}

/// The shortest spelling of the exact decimal value `text` writes: no leading
/// zeros, no trailing fraction zeros, and no sign on zero.
///
/// Returns `None` for anything that is not a plain decimal numeral, which then
/// stays as written.
fn exact_decimal_text(text: &str) -> Option<String> {
	let (negative, digits) = match text.strip_prefix('-') {
		Some(digits) => (true, digits),
		None => (false, text),
	};
	let (integer, fraction) = digits.split_once('.').unwrap_or((digits, ""));
	if integer.is_empty() && fraction.is_empty() {
		return None;
	}
	if !integer.bytes().all(|byte| byte.is_ascii_digit())
		|| !fraction.bytes().all(|byte| byte.is_ascii_digit())
	{
		return None;
	}
	let integer = match integer.trim_start_matches('0') {
		"" => "0",
		integer => integer,
	};
	let magnitude = match fraction.trim_end_matches('0') {
		"" => integer.to_string(),
		fraction => format!("{integer}.{fraction}"),
	};
	Some(if negative && magnitude != "0" {
		format!("-{magnitude}")
	} else {
		magnitude
	})
}

fn fraction_digits(text: &str) -> usize {
	text.split_once('.')
		.map_or(0, |(_, fraction)| fraction.len())
}

fn pad_fraction(text: &str, digits: usize) -> Option<String> {
	let missing = digits
		.checked_sub(fraction_digits(text))
		.filter(|missing| *missing > 0)?;
	let mut padded = text.to_string();
	if !padded.contains('.') {
		padded.push('.');
	}
	padded.extend(std::iter::repeat_n('0', missing));
	Some(padded)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pass {
	/// Every schema-typed number in its shortest exact spelling.
	Exact,
	/// Digits the game was seen to ignore dropped, nothing else.
	DropUnread,
	/// Both, in that order: the form two written files are compared in.
	Compare,
}

struct NumericWalker<'a> {
	schema: &'a CwtQuery,
	file_path: &'a GamePath,
	pass: Pass,
	/// Resolved rule contexts by ancestor key chain.
	///
	/// One entry per block the walk enters, not one per number: a definition
	/// with fifty modifier fields costs one binding, then fifty cheap field
	/// lookups inside it.
	contexts: HashMap<Vec<String>, Vec<RuleContext<'a>>>,
	/// Byte ranges rewritten, for callers that edit the source text rather
	/// than the tree.
	edits: Vec<(Range<usize>, String)>,
	spellings: NumberSpellings,
}

impl<'a> NumericWalker<'a> {
	fn new(schema: &'a CwtQuery, file_path: &'a GamePath, pass: Pass) -> Self {
		Self {
			schema,
			file_path,
			pass,
			contexts: HashMap::new(),
			edits: Vec::new(),
			spellings: NumberSpellings::default(),
		}
	}

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
					if let Some(rewritten) = self.rewrite(chain, key, text) {
						if self.pass == Pass::Exact {
							self.spellings.record(chain, key, &rewritten, text);
						}
						self.edits
							.push((span.start.offset..span.end.offset, rewritten.clone()));
						*text = rewritten;
					}
				}
				AstValue::Scalar { .. } => {}
			}
		}
	}

	fn rewrite(&mut self, chain: &[String], key: &str, text: &str) -> Option<String> {
		let mut rewritten = None;
		if matches!(self.pass, Pass::DropUnread | Pass::Compare)
			&& self.reads_three_decimals(chain, key)
		{
			rewritten = drop_unread_fraction_digits(text);
		}
		if matches!(self.pass, Pass::Exact | Pass::Compare) && self.is_typed_number(chain, key) {
			let current = rewritten.as_deref().unwrap_or(text);
			if let Some(exact) = exact_decimal_text(current) {
				rewritten = Some(exact);
			}
		}
		rewritten.filter(|rewritten| rewritten != text)
	}

	fn is_typed_number(&mut self, chain: &[String], key: &str) -> bool {
		// `CToken::GetBool` compares against the exact lowercase `yes`, so a
		// boolean is never rewritten here.
		matches!(
			self.scalar_type(chain, key),
			Some(SchemaScalarType::Float { .. } | SchemaScalarType::Int { .. })
		)
	}

	/// Whether the game was seen to read this field at three decimals, either
	/// by its own alias or, for `value`, by the alias of the block around it.
	fn reads_three_decimals(&mut self, chain: &[String], key: &str) -> bool {
		if self.every_alias(chain, key, alias_reads_three_decimals) {
			return true;
		}
		key == "value"
			&& chain.split_last().is_some_and(|(parent, outer)| {
				self.every_alias(outer, parent, alias_value_reads_three_decimals)
			})
	}

	/// Whether `key` binds inside `chain` and every rule it binds to is an
	/// alias `checked` accepts. Any other reading makes the answer no.
	///
	/// Every overload takes part, including the modifiers the schema only
	/// lists by name and scope: most of the modifiers checked in game, such as
	/// `discipline`, have no written alias rule.
	fn every_alias(
		&mut self,
		chain: &[String],
		key: &str,
		checked: fn(&CompiledAliasCategory, &str) -> bool,
	) -> bool {
		let mut bound = false;
		for context in self.contexts_for(chain) {
			for field_match in self.schema.bind_field_overloads(context, key, |_| true) {
				match field_match.alias() {
					Some(alias) if checked(&alias.category, &alias.name) => bound = true,
					_ => return false,
				}
			}
		}
		bound
	}

	/// The primitive the schema declares for `key` inside `chain`, or `None`
	/// wherever the schema is not explicit.
	fn scalar_type(&mut self, chain: &[String], key: &str) -> Option<SchemaScalarType> {
		let contexts = self.contexts_for(chain);
		let mut resolved: Option<SchemaScalarType> = None;
		for context in contexts {
			let mut field_matches = self.schema.bind_field_matches(context, key);
			if field_matches.is_empty() {
				// No written rule binds the key, which leaves a modifier the
				// schema lists only by name and scope, such as `discipline`.
				// Every such modifier is compiled as a `float`.
				field_matches = self.schema.bind_field_overloads(context, key, |_| true);
			}
			for field_match in field_matches {
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

	use tempfile::TempDir;

	use super::*;
	use crate::game::eu4::script::emit::emit_clausewitz_statements;
	use crate::game::eu4::script::parser::parse_clausewitz_content;
	use crate::game::schema::CwtSchema;

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

	const THINGS: &str = "common/things/example.txt";

	fn schema() -> CwtSchema {
		let root = TempDir::new().expect("create schema directory");
		fs::write(root.path().join("things.cwt"), SCHEMA).expect("write schema");
		CwtSchema::load_with_cache(root.path(), None).expect("load schema")
	}

	fn parse(path: &str, source: &str) -> AstFile {
		let parsed = parse_clausewitz_content(
			&crate::model::GamePathBuf::parse(path).expect("valid game path"),
			source,
		);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast
	}

	fn emit(file: &AstFile) -> String {
		emit_clausewitz_statements(&file.statements).expect("emit")
	}

	fn canonicalize(path: &str, source: &str, schema: Option<&CwtQuery>) -> String {
		emit(&canonicalize_numeric_values(
			&parse(path, source),
			schema,
			&mut NumberSpellings::default(),
		))
	}

	fn drop_unread(path: &str, source: &str) -> String {
		let mut file = parse(path, source);
		drop_unread_decimals(
			&file.path.clone(),
			&mut file.statements,
			crate::game::eu4::cwt::rule_engine(),
		);
		emit(&file)
	}

	#[test]
	fn spellings_of_one_value_become_one_string() {
		let schema = schema();
		for written in ["0.5", "0.50", "0.500"] {
			let output = canonicalize(
				THINGS,
				&format!("a_thing = {{ upkeep = {written} }}\n"),
				Some(schema.facts()),
			);
			assert!(output.contains("upkeep = 0.5\n"), "`{written}`: {output}");
		}
	}

	#[test]
	fn a_fourth_decimal_is_a_value_of_its_own() {
		let schema = schema();
		for written in ["0.0125", "0.012", "0.1239"] {
			let output = canonicalize(
				THINGS,
				&format!("a_thing = {{ upkeep = {written} }}\n"),
				Some(schema.facts()),
			);
			assert!(
				output.contains(&format!("upkeep = {written}\n")),
				"`{written}`: {output}"
			);
		}
	}

	#[test]
	fn an_int_field_compares_its_exact_value_too() {
		let schema = schema();
		let whole = canonicalize(THINGS, "a_thing = { slots = 3.0 }\n", Some(schema.facts()));
		let fraction = canonicalize(THINGS, "a_thing = { slots = 3.4 }\n", Some(schema.facts()));

		assert!(whole.contains("slots = 3\n"), "{whole}");
		assert!(fraction.contains("slots = 3.4\n"), "{fraction}");
	}

	#[test]
	fn the_exact_spelling_is_the_shortest_one_for_the_value() {
		for (written, exact) in [
			("0.50", "0.5"),
			("1.000", "1"),
			("007", "7"),
			("5.", "5"),
			("-1.50", "-1.5"),
			("-0.0", "0"),
			("0.0125", "0.0125"),
		] {
			assert_eq!(
				exact_decimal_text(written).as_deref(),
				Some(exact),
				"{written}"
			);
		}
		for shape in ["", ".", "-", "+2", "yes", "1e-3", "1444.11.11", "0x10"] {
			assert_eq!(exact_decimal_text(shape), None, "{shape}");
		}
	}

	#[test]
	fn nested_blocks_are_reached() {
		let schema = schema();
		let output = canonicalize(
			THINGS,
			"a_thing = { nested = { depth = 2.50 } }\n",
			Some(schema.facts()),
		);

		assert!(output.contains("depth = 2.5\n"), "{output}");
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
				canonicalize(THINGS, source, Some(schema.facts())),
				canonicalize(THINGS, source, None),
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
			THINGS,
			"a_thing = { upkeep = { 0.50 0.1239 } }\n",
			Some(schema.facts()),
		);

		assert!(output.contains("0.50"), "{output}");
		assert!(output.contains("0.1239"), "{output}");
	}

	#[test]
	fn no_schema_leaves_every_number_as_written() {
		let source = "a_thing = { upkeep = 0.50 slots = 3.4 }\n";

		assert!(canonicalize(THINGS, source, None).contains("upkeep = 0.50"));
	}

	#[test]
	fn the_longest_spelling_at_a_place_comes_back() {
		let schema = schema();
		let mut spellings = NumberSpellings::default();
		for source in [
			"a_thing = { upkeep = 0.5 }\n",
			"a_thing = { upkeep = 0.50 }\n",
			"a_thing = { slots = 2.00 }\n",
		] {
			canonicalize_numeric_values(
				&parse(THINGS, source),
				Some(schema.facts()),
				&mut spellings,
			);
		}
		let mut merged = canonicalize_numeric_values(
			&parse(THINGS, "a_thing = { upkeep = 0.5 slots = 2 }\n"),
			Some(schema.facts()),
			&mut NumberSpellings::default(),
		);

		spellings.restore(&mut merged.statements);

		let output = emit(&merged);
		assert!(output.contains("upkeep = 0.50\n"), "{output}");
		assert!(output.contains("slots = 2.00\n"), "{output}");
	}

	#[test]
	fn a_spelling_counts_only_at_its_own_place_and_value() {
		let schema = schema();
		let mut spellings = NumberSpellings::default();
		for source in [
			// Another definition, and another value at the same place.
			"b_thing = { upkeep = 0.500 }\n",
			"a_thing = { upkeep = 0.60 }\n",
		] {
			canonicalize_numeric_values(
				&parse(THINGS, source),
				Some(schema.facts()),
				&mut spellings,
			);
		}
		let mut merged = canonicalize_numeric_values(
			&parse(THINGS, "a_thing = { upkeep = 0.5 }\n"),
			Some(schema.facts()),
			&mut NumberSpellings::default(),
		);

		spellings.restore(&mut merged.statements);

		assert!(
			emit(&merged).contains("upkeep = 0.5\n"),
			"{}",
			emit(&merged)
		);
	}

	#[test]
	fn digits_are_dropped_only_where_the_game_was_seen_to_ignore_them() {
		let output = drop_unread(
			"common/event_modifiers/test.txt",
			"sample = {\n\tdiscipline = 0.0125\n\tyearly_army_professionalism = 0.0125\n\tglobal_unrest = 0.0125\n}\n",
		);

		assert!(output.contains("discipline = 0.012\n"), "{output}");
		assert!(
			output.contains("yearly_army_professionalism = 0.012\n"),
			"{output}"
		);
		// Never checked in game, so its digits stay.
		assert!(output.contains("global_unrest = 0.0125\n"), "{output}");
	}

	#[test]
	fn effects_and_triggers_keep_the_digits_the_game_reads() {
		let output = drop_unread(
			"events/test.txt",
			"country_event = {\n\
			\tid = test.1\n\
			\ttrigger = {\n\
			\t\tarmy_professionalism = 0.0125\n\
			\t\tarmy_tradition = 0.0125\n\
			\t\tcheck_variable = { which = counter value = 0.0125 }\n\
			\t}\n\
			\timmediate = {\n\
			\t\tadd_army_professionalism = 0.0125\n\
			\t\tadd_prestige = 0.0125\n\
			\t\tset_variable = { which = counter value = 0.0125 }\n\
			\t}\n\
			}\n",
		);

		// Checked in game and found to keep the fourth decimal.
		assert!(
			output.contains("army_professionalism = 0.0125\n"),
			"{output}"
		);
		assert!(
			output.contains("add_army_professionalism = 0.0125\n"),
			"{output}"
		);
		assert!(output.contains("army_tradition = 0.012\n"), "{output}");
		assert!(output.contains("add_prestige = 0.012\n"), "{output}");
		assert!(!output.contains("value = 0.0125"), "{output}");
	}

	#[test]
	fn rewriting_text_touches_only_the_numbers() {
		let source = "# a leading note\n\
			a_thing = {\n\
			\t# why this value\n\
			\tupkeep    =    0.50   # trailing note\n\
			\tuntyped = 0.50\n\
			}\n";

		let schema = schema();
		let rewritten = canonicalize_numeric_text_with(
			GamePath::new(THINGS).expect("valid game path"),
			source,
			Some(schema.facts()),
		);

		// Only the schema-typed number moves; the comments, the odd spacing
		// and the untyped value are byte-for-byte what they were.
		assert_eq!(
			rewritten,
			source.replace("0.50   #", "0.5   #"),
			"{rewritten}"
		);
	}

	#[test]
	fn the_compared_form_drops_unread_digits_before_spelling_the_value() {
		let path = GamePath::new("common/event_modifiers/test.txt").expect("valid game path");
		let output = canonicalize_numeric_text(
			path,
			"sample = {\n\tdiscipline = 0.01250\n\tmissionary_maintenance_cost = 0.01250\n}\n",
		);

		assert!(output.contains("discipline = 0.012\n"), "{output}");
		assert!(
			output.contains("missionary_maintenance_cost = 0.0125\n"),
			"{output}"
		);
	}

	#[test]
	fn rewriting_text_is_a_no_op_where_nothing_is_typed() {
		for source in ["a_thing = { untyped = 0.50 }\n", "not_parseable = {\n"] {
			assert_eq!(
				canonicalize_numeric_text_with(
					GamePath::new(THINGS).expect("valid game path"),
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
			THINGS,
			"a_thing = { upkeep = 0.50 slots = 3.40 }\n",
			Some(schema.facts()),
		);

		assert_eq!(canonicalize(THINGS, &once, Some(schema.facts())), once);
	}

	/// The vendored rules type `institution_growth` as a whole number, but
	/// the game writes `1.2`; the merge keeps the fraction either way.
	#[test]
	fn a_modifier_the_game_writes_with_decimals_keeps_its_fraction() {
		let path = GamePath::new("common/event_modifiers/00_event_modifiers.txt")
			.expect("valid game path");
		let output =
			canonicalize_numeric_text(path, "sample = {\n\tinstitution_growth = 1.20\n}\n");
		assert!(output.contains("institution_growth = 1.2\n"), "{output}");
	}

	/// Most modifiers, `global_unrest` among them, have no written rule: the
	/// schema only lists them by name and scope. They are numbers all the same.
	#[test]
	fn a_modifier_listed_only_by_name_is_compared_by_value() {
		let output = emit(&canonicalize_numeric_values(
			&parse(
				"common/event_modifiers/test.txt",
				"sample = {\n\tglobal_unrest = 0.050\n\tdiscipline = 0.0125\n}\n",
			),
			Some(crate::game::eu4::cwt::rule_engine()),
			&mut NumberSpellings::default(),
		));

		assert!(output.contains("global_unrest = 0.05\n"), "{output}");
		assert!(output.contains("discipline = 0.0125\n"), "{output}");
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
	#[ignore = "P-695 coverage probe: needs EU4_ROOT"]
	fn measure_coverage_against_vanilla() {
		let schema = crate::game::eu4::cwt::rule_engine();
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
			let relative = crate::model::GamePathBuf::from_physical(&root, file)
				.expect("vanilla file has a game path");
			let parsed = parse_clausewitz_content(&relative, &source);
			if !parsed.diagnostics.is_empty() {
				continue;
			}
			parsed_files += 1;
			let canonical = canonicalize_numeric_values(
				&parsed.ast,
				Some(schema),
				&mut NumberSpellings::default(),
			);
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
