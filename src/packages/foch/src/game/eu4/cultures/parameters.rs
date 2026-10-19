//! Culture references that travel through script parameters. Parameters are
//! text substitution, so a call argument naming a renamed culture is rewritten
//! where every use of the parameter, through the scripts it is passed to, is a
//! culture field or a name of a value set composed from it, such as the flag
//! `$CULTURE$_released`. Such a name is renamed with the culture wherever its
//! value set holds it. A use that is neither is rewritten by the load-order
//! default with a note, and `--strict` defers the closure for review.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::{DerivedNames, reference_kinds, script_context, value_set_fields, value_sets};
use crate::game::eu4::script::parser::{AstStatement, AstValue};
use crate::game::schema::query::CompiledAliasCategory;
use crate::input::ResolvedInput;
use crate::model::{GamePath, GamePathBuf};

#[derive(Default)]
pub(super) struct ParameterReview {
	pub paths: BTreeSet<GamePathBuf>,
	pub findings: Vec<String>,
	/// Call arguments to rewrite as culture values, by mod and file, at where
	/// their keys start.
	pub arguments: BTreeMap<(String, GamePathBuf), BTreeSet<usize>>,
	/// Names composed from a rewritten argument, renamed with it.
	pub derived: DerivedNames,
	/// Arguments the load-order default rewrote although their parameter has
	/// other uses.
	pub notes: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CallableKind {
	Effect,
	Trigger,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Callable {
	kind: CallableKind,
	name: String,
}

type ParameterPaths = BTreeMap<(Callable, String), BTreeSet<GamePathBuf>>;

/// Where a script uses a parameter other than as a culture: by mod, file and
/// where the statement's key starts, with the statement for the report.
type OtherUses = BTreeMap<(Callable, String), Vec<(String, GamePathBuf, usize, String)>>;

/// The names of a value set a script composes from its parameters, as
/// `(set, template)`.
type ComposedNames = BTreeMap<Callable, BTreeSet<(String, String)>>;

struct Binding {
	key: String,
	value: String,
	offset: usize,
}

struct Call {
	owner: Option<Callable>,
	callee: Callable,
	bindings: Vec<Binding>,
	mod_id: String,
	path: GamePathBuf,
	location: String,
}

/// With `load_order_default`, an argument whose parameter has other uses is
/// rewritten too, with a note, instead of deferred.
pub(super) fn review_parameter_flows(
	input: &ResolvedInput,
	mappings: &BTreeMap<String, String>,
	load_order_default: bool,
) -> Result<ParameterReview, String> {
	if mappings.is_empty() {
		return Ok(ParameterReview::default());
	}
	let mut documents = Vec::new();
	let mut names = BTreeSet::new();
	let mut definitions = BTreeSet::new();
	let mut sensitive = ParameterPaths::new();
	let mut hazardous = BTreeMap::<Callable, BTreeSet<GamePathBuf>>::new();
	let mut other = OtherUses::new();
	let mut composed = ComposedNames::new();
	let mut review = ParameterReview::default();
	for (path, contributors) in &input.file_inventory {
		if !is_callable_document(path) {
			continue;
		}
		for contributor in contributors.iter().filter(|item| !item.is_synthetic_base) {
			let parsed = input.script_cache.load(contributor)?;
			let fields = value_set_fields(&parsed.ast.statements, path);
			for statement in &parsed.ast.statements {
				if let AstStatement::Assignment {
					key,
					value: AstValue::Block { items, .. },
					..
				} = statement
				{
					names.insert(key.clone());
					let owner = Callable {
						kind: callable_kind(path).expect("callable document"),
						name: key.clone(),
					};
					definitions.insert(owner.clone());
					collect_parameters(
						items,
						path,
						&contributor.mod_id,
						&mut vec![key.clone()],
						&owner,
						mappings,
						&mut sensitive,
						&mut hazardous,
						&fields,
						&mut other,
						&mut composed,
					);
				}
			}
			documents.push((contributor.mod_id.clone(), parsed));
		}
	}
	if sensitive.is_empty() && hazardous.is_empty() {
		return Ok(review);
	}
	// The semantic indexes select callers; inspect the frozen ASTs so accepted
	// source repairs and forwarded parameter values are included in the review.
	let mut caller_paths = BTreeSet::new();
	for snapshot in input.mod_snapshots.iter().flatten() {
		caller_paths.extend(
			snapshot
				.semantic_index
				.references
				.iter()
				.filter(|reference| names.contains(&reference.name))
				.map(|reference| reference.path.clone()),
		);
		caller_paths.extend(
			snapshot
				.semantic_index
				.scalar_assignments
				.iter()
				.filter(|assignment| names.contains(&assignment.key))
				.map(|assignment| assignment.path.clone()),
		);
	}
	if let Some(base) = &input.installed_base_snapshot {
		caller_paths.extend(
			base.snapshot
				.symbol_references
				.iter()
				.filter(|reference| names.contains(&reference.name))
				.map(|reference| reference.path.clone()),
		);
		caller_paths.extend(
			base.snapshot
				.scalar_assignments
				.iter()
				.filter(|assignment| names.contains(&assignment.key))
				.map(|assignment| assignment.path.clone()),
		);
	}
	for path in caller_paths
		.into_iter()
		.filter(|path| !is_callable_document(path))
	{
		if let Some(contributors) = input.file_inventory.get(&path) {
			for contributor in contributors.iter().filter(|item| !item.is_synthetic_base) {
				documents.push((
					contributor.mod_id.clone(),
					input.script_cache.load(contributor)?,
				));
			}
		}
	}
	let mut calls = Vec::new();
	for (mod_id, parsed) in &documents {
		let path = &parsed.ast.path;
		for statement in &parsed.ast.statements {
			match statement {
				AstStatement::Assignment {
					key,
					value: AstValue::Block { items, .. },
					..
				} if is_callable_document(path) => {
					let owner = Callable {
						kind: callable_kind(path).expect("callable document"),
						name: key.clone(),
					};
					collect_calls(
						items,
						path,
						mod_id,
						Some(&owner),
						&mut vec![key.clone()],
						&definitions,
						&mut calls,
					);
				}
				_ => collect_calls(
					std::slice::from_ref(statement),
					path,
					mod_id,
					None,
					&mut Vec::new(),
					&definitions,
					&mut calls,
				),
			}
		}
	}
	// A parameter passed to another script is used as that script uses it,
	// so an argument is read there rather than as a use of its own.
	let arguments = calls
		.iter()
		.filter(|call| call.owner.is_some())
		.flat_map(|call| {
			call.bindings
				.iter()
				.map(|binding| (call.mod_id.clone(), call.path.clone(), binding.offset))
		})
		.collect::<BTreeSet<_>>();
	let mut impure = BTreeMap::<(Callable, String), String>::new();
	for (parameter, uses) in &other {
		if let Some((_, _, _, statement)) = uses.iter().find(|(mod_id, path, offset, _)| {
			!arguments.contains(&(mod_id.clone(), path.clone(), *offset))
		}) {
			impure.insert(parameter.clone(), statement.clone());
		}
	}
	loop {
		let mut changed = false;
		for call in &calls {
			let Some(owner) = &call.owner else { continue };
			for binding in &call.bindings {
				let flow = (call.callee.clone(), binding.key.clone());
				for parameter in parameter_names(&binding.value) {
					let target = (owner.clone(), parameter.to_owned());
					if impure.contains_key(&target) {
						continue;
					}
					let reason = if let Some(reason) = impure.get(&flow) {
						format!(
							"passed to `{}` as `{}`, which has `{reason}`",
							call.callee.name, binding.key
						)
					} else if sensitive.contains_key(&flow)
						&& forwarded_name(&binding.value) != Some(parameter)
					{
						format!(
							"composed into the culture `{} = {}` of `{}`",
							binding.key, binding.value, call.callee.name
						)
					} else {
						continue;
					};
					impure.insert(target, reason);
					changed = true;
				}
			}
		}
		if !changed {
			break;
		}
	}
	// A name a called script composes reads, in its caller, with the call's
	// arguments substituted. Bounded, as a cycle can compose without end.
	for _ in 0..16 {
		let mut changed = false;
		for call in &calls {
			let Some(owner) = &call.owner else { continue };
			let Some(names) = composed.get(&call.callee).cloned() else {
				continue;
			};
			for (set, template) in names {
				if let Some(name) = substitute(&template, |parameter| {
					call.bindings
						.iter()
						.find(|binding| binding.key == parameter)
						.map(|binding| binding.value.clone())
				}) && name.contains('$')
				{
					changed |= composed
						.entry(owner.clone())
						.or_default()
						.insert((set, name));
				}
			}
		}
		if !changed {
			break;
		}
	}
	// A parameter passed to another script inherits that script's culture use.
	// Union paths until stable, including cycles and multiple definitions.
	loop {
		let mut changed = false;
		for call in &calls {
			let Some(owner) = &call.owner else { continue };
			for binding in &call.bindings {
				let Some(mut paths) = sensitive
					.get(&(call.callee.clone(), binding.key.clone()))
					.cloned()
				else {
					continue;
				};
				paths.insert(call.path.clone());
				for forwarded in parameter_names(&binding.value) {
					let target = sensitive
						.entry((owner.clone(), forwarded.to_owned()))
						.or_default();
					let count = target.len();
					target.extend(paths.iter().cloned());
					changed |= count != target.len();
				}
			}
		}
		if !changed {
			break;
		}
	}
	for (name, paths) in &hazardous {
		review.paths.extend(paths.iter().cloned());
		review.findings.push(format!("script `{}` has a culture parameter default referring to a changed culture; review parameter substitution before accepting the transformation", name.name));
	}
	for call in &calls {
		for Binding {
			key: parameter,
			value,
			offset,
		} in &call.bindings
		{
			if !refers_to_changed(value, mappings) {
				continue;
			}
			let flow = (call.callee.clone(), parameter.clone());
			let Some(paths) = sensitive.get(&flow) else {
				continue;
			};
			let mut other_use = impure.get(&flow).cloned();
			if let Some(new) = mappings.get(value) {
				let derived = derive_names(composed.get(&call.callee), call, parameter, value, new);
				let names = match derived {
					Ok(names) => names,
					Err(template) => {
						other_use.get_or_insert(template);
						Vec::new()
					}
				};
				if other_use.is_none() || load_order_default {
					review
						.arguments
						.entry((call.mod_id.clone(), call.path.clone()))
						.or_default()
						.insert(*offset);
					for (set, old, renamed) in names {
						let held = review
							.derived
							.entry(set.clone())
							.or_default()
							.entry(old.clone())
							.or_insert_with(|| renamed.clone());
						if *held != renamed {
							review.paths.insert(call.path.clone());
							review.findings.push(format!(
								"{}: {set} `{old}` would be renamed to both `{held}` and `{renamed}`",
								call.location
							));
						}
					}
					if let Some(other_use) = other_use {
						review.notes.push(format!("{}: parameter `{parameter} = {value}` rewritten to `{new}` for `{}`, which also uses it outside culture fields (`{other_use}`): the load-order default", call.location, call.callee.name));
					}
					continue;
				}
			}
			let other_use = other_use.as_ref();
			let mut paths = paths.clone();
			paths.insert(call.path.clone());
			review.paths.extend(paths.iter().cloned());
			let also = other_use
				.map(|other_use| format!(" and is also used as `{other_use}`"))
				.unwrap_or_default();
			review.findings.push(format!("{}: parameter `{parameter} = {value}` reaches a culture field through `{}`{also}; review parameter substitution before accepting the transformation", call.location, call.callee.name));
			if let Some(owner) = &call.owner {
				hazardous.entry(owner.clone()).or_default().extend(paths);
			}
		}
	}
	// Callers of a withheld script must join the same review group, even when
	// they invoke a default or a script that supplies the changed ID itself.
	loop {
		let mut changed = false;
		for call in &calls {
			let Some(mut paths) = hazardous.get(&call.callee).cloned() else {
				continue;
			};
			paths.insert(call.path.clone());
			review.paths.extend(paths.iter().cloned());
			if let Some(owner) = &call.owner {
				let target = hazardous.entry(owner.clone()).or_default();
				let count = target.len();
				target.extend(paths);
				changed |= count != target.len();
			}
		}
		if !changed {
			break;
		}
	}
	Ok(review)
}

fn is_callable_document(path: &GamePath) -> bool {
	callable_kind(path).is_some()
}

fn callable_kind(path: &GamePath) -> Option<CallableKind> {
	// The game loads only `.txt` scripts here; a mod's `x.txt.bak` is not one.
	if path
		.extension()
		.is_none_or(|extension| !extension.eq_ignore_ascii_case("txt"))
	{
		None
	} else if path.is_inside(&["common", "scripted_effects"], str::eq) {
		Some(CallableKind::Effect)
	} else if path.is_inside(&["common", "scripted_triggers"], str::eq) {
		Some(CallableKind::Trigger)
	} else {
		None
	}
}

fn call_kinds(path: &GamePath, parents: &[String]) -> Vec<CallableKind> {
	let schema = super::super::cwt::rule_engine();
	let context = script_context(schema, path, parents);
	let mut kinds = Vec::new();
	for (kind, key, category) in [
		(
			CallableKind::Effect,
			"change_culture",
			CompiledAliasCategory::Effect,
		),
		(
			CallableKind::Trigger,
			"culture",
			CompiledAliasCategory::Trigger,
		),
	] {
		if context.is_some_and(|context| {
			schema.bind_field_matches(context, key).iter().any(|field| {
				field
					.alias()
					.is_some_and(|alias| alias.category == category)
			})
		}) {
			kinds.push(kind);
		}
	}
	// Unknown CWT contexts cannot establish a namespace. Audit both rather
	// than silently omit a possibly affected reference.
	if kinds.is_empty() {
		kinds.extend([CallableKind::Effect, CallableKind::Trigger]);
	}
	kinds
}

#[allow(clippy::too_many_arguments)]
fn collect_parameters(
	statements: &[AstStatement],
	path: &GamePath,
	mod_id: &str,
	parents: &mut Vec<String>,
	owner: &Callable,
	mappings: &BTreeMap<String, String>,
	sensitive: &mut ParameterPaths,
	hazardous: &mut BTreeMap<Callable, BTreeSet<GamePathBuf>>,
	fields: &HashMap<usize, BTreeSet<String>>,
	other: &mut OtherUses,
	composed: &mut ComposedNames,
) {
	let schema = super::super::cwt::rule_engine();
	let context = script_context(schema, path, parents);
	for statement in statements {
		let AstStatement::Assignment {
			key,
			key_span,
			value,
			..
		} = statement
		else {
			continue;
		};
		let alternatives = context
			.map(|context| schema.bind_field_matches(context, key))
			.unwrap_or_default();
		let kinds = reference_kinds(&alternatives, key);
		let mut uses = Vec::new();
		let mut others = Vec::new();
		if kinds.culture_key {
			uses.push(key.clone());
		} else {
			others.push(key.clone());
		}
		let statement_text = match value {
			AstValue::Scalar { value, .. } => {
				let text = value.as_text();
				// The binder reads a script's body in its definition, where the
				// path alone may not.
				let mut sets = value_sets(&alternatives);
				sets.extend(
					fields
						.get(&key_span.start.offset)
						.into_iter()
						.flatten()
						.cloned(),
				);
				if kinds.culture_value || (!kinds.group_value && key.contains("culture")) {
					uses.push(text.clone());
				} else if let [set] = sets.into_iter().collect::<Vec<_>>().as_slice()
					&& text.contains('$')
				{
					composed
						.entry(owner.clone())
						.or_default()
						.insert((set.clone(), text.clone()));
				} else {
					others.push(text.clone());
				}
				format!("{key} = {text}")
			}
			AstValue::Block { items, .. } => {
				parents.push(key.clone());
				collect_parameters(
					items, path, mod_id, parents, owner, mappings, sensitive, hazardous, fields,
					other, composed,
				);
				parents.pop();
				format!("{key} = {{ ... }}")
			}
		};
		for text in others {
			for name in parameter_names(&text) {
				other
					.entry((owner.clone(), name.to_owned()))
					.or_default()
					.push((
						mod_id.to_owned(),
						path.to_owned(),
						key_span.start.offset,
						statement_text.clone(),
					));
			}
		}
		for value in uses {
			for name in parameter_names(&value) {
				sensitive
					.entry((owner.clone(), name.to_owned()))
					.or_default()
					.insert(path.to_owned());
				if refers_to_changed(&value, mappings) {
					hazardous
						.entry(owner.clone())
						.or_default()
						.insert(path.to_owned());
				}
			}
		}
	}
}

fn collect_calls(
	statements: &[AstStatement],
	path: &GamePath,
	mod_id: &str,
	owner: Option<&Callable>,
	parents: &mut Vec<String>,
	names: &BTreeSet<Callable>,
	calls: &mut Vec<Call>,
) {
	let kinds = call_kinds(path, parents);
	for statement in statements {
		let AstStatement::Assignment {
			key,
			value,
			key_span,
			..
		} = statement
		else {
			continue;
		};
		for &kind in &kinds {
			let callee = Callable {
				kind,
				name: key.clone(),
			};
			if !names.contains(&callee) {
				continue;
			}
			let bindings = match value {
				AstValue::Block { items, .. } => items
					.iter()
					.filter_map(|item| match item {
						AstStatement::Assignment {
							key,
							key_span,
							value: AstValue::Scalar { value, .. },
							..
						} => Some(Binding {
							key: key.clone(),
							value: value.as_text(),
							offset: key_span.start.offset,
						}),
						_ => None,
					})
					.collect(),
				_ => Vec::new(),
			};
			calls.push(Call {
				owner: owner.cloned(),
				callee,
				bindings,
				mod_id: mod_id.to_owned(),
				path: path.to_owned(),
				location: format!(
					"{mod_id}:{path}:{}:{}",
					key_span.start.line, key_span.start.column
				),
			});
		}
		if let AstValue::Block { items, .. } = value {
			parents.push(key.clone());
			collect_calls(items, path, mod_id, owner, parents, names, calls);
			parents.pop();
		}
	}
}

fn parameter_tokens(value: &str) -> impl Iterator<Item = &str> {
	value
		.split('$')
		.enumerate()
		.filter_map(|(index, part)| (index % 2 == 1).then_some(part))
}

fn parameter_names(value: &str) -> impl Iterator<Item = &str> {
	parameter_tokens(value)
		.map(|part| part.split('|').next().unwrap_or_default())
		.filter(|name| {
			!name.is_empty()
				&& name
					.bytes()
					.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
		})
}

/// The names a call composes from `old` passed as `parameter`, as `(set, old
/// name, new name)`, or the first composed name the call's own arguments
/// leave unresolved.
fn derive_names(
	composed: Option<&BTreeSet<(String, String)>>,
	call: &Call,
	parameter: &str,
	old: &str,
	new: &str,
) -> Result<Vec<(String, String, String)>, String> {
	let mut names = Vec::new();
	for (set, template) in composed.into_iter().flatten() {
		if !parameter_names(template).any(|name| name == parameter) {
			continue;
		}
		let name = |culture: &str| {
			substitute(template, |name| {
				if name == parameter {
					Some(culture.to_owned())
				} else {
					call.bindings
						.iter()
						.find(|binding| binding.key == name)
						.map(|binding| binding.value.clone())
				}
			})
			.filter(|name| !name.contains('$'))
			.map(|name| name.split('@').next().unwrap_or_default().to_owned())
		};
		match (name(old), name(new)) {
			(Some(old_name), Some(new_name)) if old_name != new_name => {
				names.push((set.clone(), old_name, new_name))
			}
			(Some(_), Some(_)) => {}
			_ => return Err(format!("{set} `{template}`")),
		}
	}
	Ok(names)
}

/// A template with each `$NAME$` or `$NAME|default$` replaced by its value,
/// or `None` where a parameter has neither value nor default.
fn substitute(template: &str, value: impl Fn(&str) -> Option<String>) -> Option<String> {
	let mut result = String::new();
	for (index, part) in template.split('$').enumerate() {
		if index % 2 == 0 {
			result.push_str(part);
			continue;
		}
		let (name, default) = match part.split_once('|') {
			Some((name, default)) => (name, Some(default)),
			None => (part, None),
		};
		result.push_str(&value(name).or_else(|| default.map(str::to_owned))?);
	}
	Some(result)
}

/// The parameter a value passes on whole, as `$NAME$`.
fn forwarded_name(value: &str) -> Option<&str> {
	let name = value.strip_prefix('$')?.strip_suffix('$')?;
	parameter_names(value)
		.next()
		.filter(|forwarded| *forwarded == name)
}

fn refers_to_changed(value: &str, mappings: &BTreeMap<String, String>) -> bool {
	mappings.contains_key(value)
		|| parameter_tokens(value).any(|part| {
			part.split_once('|')
				.is_some_and(|(_, default)| mappings.contains_key(default))
		})
}

#[cfg(test)]
mod tests {
	use super::{CallableKind, callable_kind};
	use crate::model::GamePath;

	#[test]
	fn only_txt_files_define_callables() {
		let path = |text| GamePath::new(text).expect("valid game path");
		assert!(matches!(
			callable_kind(path("common/scripted_effects/00_effects.txt")),
			Some(CallableKind::Effect)
		));
		assert!(callable_kind(path("common/scripted_effects/00_effects.txt.bak")).is_none());
		assert!(callable_kind(path("common/scripted_triggers/readme.md")).is_none());
	}
}
