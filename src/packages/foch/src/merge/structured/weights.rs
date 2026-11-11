//! The identity of a weight block: a block the schema reads as numbers and
//! conditions, such as `modifier = { factor = 2 is_at_war = yes }` inside an
//! `ai_will_do`. The game combines every such block whose conditions hold, so
//! the conditions say which block it is and the numbers are its value: two
//! mods that change one block's factor change the same block, and two that add
//! blocks with other conditions add two.

use std::collections::{HashMap, HashSet};

use crate::game::eu4::cwt::Eu4Dialect;
use crate::game::eu4::script::emit::emit_clausewitz_statements;
use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue, ScalarValue};
use crate::game::schema::bind::{BoundContext, SchemaBinder};
use crate::game::schema::query::{CompiledAliasCategory, CwtQuery, SchemaScalarType};
use crate::merge::kernel::SemanticKey;

/// What the schema says about the statements of one bound file, by the
/// address of each statement's value. A reading belongs to the exact tree it
/// was bound from: a clone of the file is a different tree whose addresses
/// mean nothing here, so the reading is handed to whoever walks that tree
/// instead of being kept where a walk of another tree could find it.
#[derive(Default, Debug)]
pub(super) struct FileReading {
	/// Weight blocks' identities.
	identities: HashMap<usize, SemanticKey>,
	/// Statements a block may repeat: triggers, effects, scope switches and
	/// fields the schema allows more than once, rather than fields a block
	/// holds once.
	repeatable: HashSet<usize>,
	/// Blocks that hold conditions and no effects, read together.
	conditions: HashSet<usize>,
	/// Parameterized calls: a trigger context calls a name no rule declares
	/// with a block of arguments (such as `num_of_ruler_traits = { amount =
	/// 3 }`). Keyed by the call block's address, with the block's emitted
	/// text as the distinguishing value.
	parameterized: HashMap<usize, String>,
}

/// Bind `file` and note what the schema says about its statements, for the
/// [`FileReading`] queries to answer while that exact tree is walked.
pub(super) fn read(schema: &CwtQuery, file: &AstFile) -> FileReading {
	let mut reading = FileReading::default();
	let binder = SchemaBinder::new(schema, &Eu4Dialect, None, &file.path);
	let context = binder.file(&file.statements);
	collect(&binder, &context, &file.statements, &mut reading);
	reading
}

impl FileReading {
	/// The identity [`read`] found for the block whose value is `value`.
	pub(super) fn identity(&self, value: &AstValue) -> Option<SemanticKey> {
		self.identities
			.get(&(std::ptr::from_ref(value) as usize))
			.cloned()
	}

	/// Whether [`read`] read the block `value` as one of conditions.
	pub(super) fn is_condition_block(&self, value: &AstValue) -> bool {
		self.conditions
			.contains(&(std::ptr::from_ref(value) as usize))
	}

	/// Whether [`read`] read the statement whose value is `value` as one its
	/// block may repeat.
	pub(super) fn is_repeatable(&self, value: &AstValue) -> bool {
		self.repeatable
			.contains(&(std::ptr::from_ref(value) as usize))
	}

	/// The emitted argument text of the parameterized call whose block value
	/// is `value`, if it is one. An exact address lookup, so the answer is
	/// the same in every process.
	pub(super) fn parameterized_call(&self, value: &AstValue) -> Option<String> {
		self.parameterized
			.get(&(std::ptr::from_ref(value) as usize))
			.cloned()
	}

	/// Whether the block `value` is a parameterized call, by its content:
	/// a boolean answer, so any iteration order gives the same result and
	/// the check works on any clone of the file the reading bound.
	pub(super) fn is_parameterized_block(&self, value: &AstValue) -> bool {
		let Some(key) = block_key(value) else {
			return false;
		};
		self.parameterized.values().any(|text| text.ends_with(&key))
	}
}

/// The text a parameterized call's block is recorded under: the block's
/// statements emitted bare, so `{ amount = 3 }` keys the call whatever
/// name called it.
fn block_key(value: &AstValue) -> Option<String> {
	fn zero_span() -> crate::game::eu4::script::parser::Span {
		crate::game::eu4::script::parser::Span {
			line: 0,
			column: 0,
			offset: 0,
		}
	}
	let call = AstStatement::Item {
		value: value.clone(),
		span: crate::game::eu4::script::parser::SpanRange {
			start: zero_span(),
			end: zero_span(),
		},
	};
	let text = emit_clausewitz_statements(std::slice::from_ref(&call)).ok()?;
	Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn collect<'a>(
	binder: &SchemaBinder<'a>,
	context: &BoundContext<'a, AstStatement>,
	statements: &'a [AstStatement],
	reading: &mut FileReading,
) {
	for statement in statements {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		let binding = binder.bind(context, key, value);
		// The boolean operators and the control-flow branch keys are no
		// calls: the binder declares none of them, but each has machinery of
		// its own (the DNF reader, the chain normalizer), and recording their
		// blocks as calls would pull `OR` and `if` blocks out of it.
		let has_own_machinery = super::merge::is_boolean_operator(key)
			|| matches!(key.as_str(), "if" | "else_if" | "else");
		if binding.matched.is_none()
			&& !has_own_machinery
			&& context.admits(&CompiledAliasCategory::Trigger)
			&& matches!(value, AstValue::Block { .. })
			&& let Ok(text) = emit_clausewitz_statements(std::slice::from_ref(statement))
		{
			reading.parameterized.insert(
				std::ptr::from_ref(value) as usize,
				text.split_whitespace().collect::<Vec<_>>().join(" "),
			);
		}
		if binding.matched.is_some_and(|matched| {
			matched.alias().is_some_and(|alias| {
				matches!(
					alias.category,
					CompiledAliasCategory::Trigger
						| CompiledAliasCategory::Effect
						| CompiledAliasCategory::Link
				)
			}) || matched
				.field()
				.attributes
				.cardinality
				.is_some_and(|(_, most)| most.is_none_or(|most| most > 1))
		}) {
			reading
				.repeatable
				.insert(std::ptr::from_ref(value) as usize);
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		let child = binder.child(context, statement, key, &binding);
		if child.admits(&CompiledAliasCategory::Trigger)
			&& !child.admits(&CompiledAliasCategory::Effect)
		{
			reading
				.conditions
				.insert(std::ptr::from_ref(value) as usize);
		}
		if let Some(identity) = weight_identity(binder, &child, key, items) {
			reading
				.identities
				.insert(std::ptr::from_ref(value) as usize, identity);
		}
		collect(binder, &child, items, reading);
	}
}

/// The conditions of a block whose every field the schema reads as a number
/// or a condition, with at least one of each: each written out and sorted,
/// since the game reads them together.
fn weight_identity<'a>(
	binder: &SchemaBinder<'a>,
	context: &BoundContext<'a, AstStatement>,
	key: &str,
	items: &[AstStatement],
) -> Option<SemanticKey> {
	if !is_weight_shape(items) || !context.admits(&CompiledAliasCategory::Trigger) {
		return None;
	}
	let mut numbers = 0;
	let mut conditions = Vec::new();
	for item in items {
		match item {
			AstStatement::Comment { .. } => {}
			AstStatement::Item { .. } => return None,
			AstStatement::Assignment {
				key: field, value, ..
			} => {
				let Some(matched) = binder.bind(context, field, value).matched else {
					conditions.push(
						emit_clausewitz_statements(std::slice::from_ref(item))
							.ok()?
							.split_whitespace()
							.collect::<Vec<_>>()
							.join(" "),
					);
					continue;
				};
				match matched.alias() {
					Some(alias)
						if matches!(
							alias.category,
							CompiledAliasCategory::Trigger | CompiledAliasCategory::Link
						) =>
					{
						conditions.push(
							emit_clausewitz_statements(std::slice::from_ref(item))
								.ok()?
								.split_whitespace()
								.collect::<Vec<_>>()
								.join(" "),
						);
					}
					Some(_) => return None,
					None => match (value, SchemaScalarType::from_rule_value(matched.value())) {
						(
							AstValue::Scalar { .. },
							Some(SchemaScalarType::Float { .. } | SchemaScalarType::Int { .. }),
						) => numbers += 1,
						_ => return None,
					},
				}
			}
		}
	}
	if numbers == 0 || conditions.is_empty() {
		return None;
	}
	conditions.sort();
	Some(SemanticKey::parent_scoped(
		"clausewitz.weight.conditions",
		format!("{key}:{}", conditions.join(" ")),
	))
}

/// A block holding a number and something else, the shape every weight
/// block has: checked before any binding.
fn is_weight_shape(items: &[AstStatement]) -> bool {
	items.len() >= 2
		&& items.iter().any(|item| {
			matches!(
				item,
				AstStatement::Assignment {
					value: AstValue::Scalar {
						value: ScalarValue::Number(_),
						..
					},
					..
				}
			)
		})
}
