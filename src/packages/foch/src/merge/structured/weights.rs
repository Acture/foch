//! The identity of a weight block: a block the schema reads as numbers and
//! conditions, such as `modifier = { factor = 2 is_at_war = yes }` inside an
//! `ai_will_do`. The game combines every such block whose conditions hold, so
//! the conditions say which block it is and the numbers are its value: two
//! mods that change one block's factor change the same block, and two that add
//! blocks with other conditions add two.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::game::eu4::cwt::Eu4Dialect;
use crate::game::eu4::script::emit::emit_clausewitz_statements;
use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue, ScalarValue};
use crate::game::schema::bind::{BoundContext, SchemaBinder};
use crate::game::schema::query::{CompiledAliasCategory, CwtQuery, SchemaScalarType};
use crate::merge::kernel::SemanticKey;

/// What the schema says about the statements of the file being normalized on
/// this thread, by the address of each statement's value.
#[derive(Default)]
struct FileReading {
	/// Weight blocks' identities.
	identities: HashMap<usize, SemanticKey>,
	/// Statements a block may repeat: triggers, effects, scope switches and
	/// fields the schema allows more than once, rather than fields a block
	/// holds once.
	repeatable: HashSet<usize>,
	/// Blocks that hold conditions and no effects, read together.
	conditions: HashSet<usize>,
}

thread_local! {
	static READING: RefCell<FileReading> = RefCell::new(FileReading::default());
}

/// Bind `file` and note what the schema says about its statements, for
/// [`identity`] and [`is_repeatable`] to answer while `file` is normalized.
/// Subtypes decide what a block holds, as `ai_will_do` belongs to selectable
/// idea groups, so the binding reads the file's own statements rather than
/// key paths alone.
pub(super) fn prepare(schema: &CwtQuery, file: &AstFile) {
	let mut reading = FileReading::default();
	let binder = SchemaBinder::new(schema, &Eu4Dialect, None, &file.path);
	let context = binder.file(&file.statements);
	collect(&binder, &context, &file.statements, &mut reading);
	READING.with(|current| *current.borrow_mut() = reading);
}

/// The identity [`prepare`] found for the block whose value is `value`.
pub(super) fn identity(value: &AstValue) -> Option<SemanticKey> {
	let address = std::ptr::from_ref(value) as usize;
	READING.with(|reading| reading.borrow().identities.get(&address).cloned())
}

/// Whether [`prepare`] read the block `value` as one of conditions.
pub(super) fn is_condition_block(value: &AstValue) -> bool {
	let address = std::ptr::from_ref(value) as usize;
	READING.with(|reading| reading.borrow().conditions.contains(&address))
}

/// Whether [`prepare`] read the statement whose value is `value` as one its
/// block may repeat.
pub(super) fn is_repeatable(value: &AstValue) -> bool {
	let address = std::ptr::from_ref(value) as usize;
	READING.with(|reading| reading.borrow().repeatable.contains(&address))
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
				// A key no rule names here is most often a call of a scripted
				// trigger, which this reading does not know by name: still a
				// condition of the block.
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
