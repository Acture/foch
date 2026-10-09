//! Defer culture transformations whose references travel through script parameters.
//! Parameters are text substitution, so rewriting a call argument as a culture
//! could also change its non-culture uses. Track the closure without editing it.

use std::collections::{BTreeMap, BTreeSet};

use super::{reference_kinds, script_context};
use crate::game::eu4::script::parser::{AstStatement, AstValue};
use crate::game::schema::query::CompiledAliasCategory;
use crate::input::ResolvedInput;
use crate::model::{GamePath, GamePathBuf};

#[derive(Default)]
pub(super) struct ParameterReview {
	pub paths: BTreeSet<GamePathBuf>,
	pub findings: Vec<String>,
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

struct Call {
	owner: Option<Callable>,
	callee: Callable,
	bindings: Vec<(String, String)>,
	path: GamePathBuf,
	location: String,
}

pub(super) fn review_parameter_flows(
	input: &ResolvedInput,
	mappings: &BTreeMap<String, String>,
) -> Result<ParameterReview, String> {
	if mappings.is_empty() {
		return Ok(ParameterReview::default());
	}
	let mut documents = Vec::new();
	let mut names = BTreeSet::new();
	let mut definitions = BTreeSet::new();
	let mut sensitive = ParameterPaths::new();
	let mut hazardous = BTreeMap::<Callable, BTreeSet<GamePathBuf>>::new();
	let mut review = ParameterReview::default();
	for (path, contributors) in &input.file_inventory {
		if !is_callable_document(path) {
			continue;
		}
		for contributor in contributors.iter().filter(|item| !item.is_synthetic_base) {
			let parsed = input.script_cache.load(contributor)?;
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
						&mut vec![key.clone()],
						&owner,
						mappings,
						&mut sensitive,
						&mut hazardous,
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
	// A parameter passed to another script inherits that script's culture use.
	// Union paths until stable, including cycles and multiple definitions.
	loop {
		let mut changed = false;
		for call in &calls {
			let Some(owner) = &call.owner else { continue };
			for (parameter, value) in &call.bindings {
				let Some(mut paths) = sensitive
					.get(&(call.callee.clone(), parameter.clone()))
					.cloned()
				else {
					continue;
				};
				paths.insert(call.path.clone());
				for forwarded in parameter_names(value) {
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
		for (parameter, value) in &call.bindings {
			if !refers_to_changed(value, mappings) {
				continue;
			}
			let Some(paths) = sensitive.get(&(call.callee.clone(), parameter.clone())) else {
				continue;
			};
			let mut paths = paths.clone();
			paths.insert(call.path.clone());
			review.paths.extend(paths.iter().cloned());
			review.findings.push(format!("{}: parameter `{parameter} = {value}` reaches a culture field through `{}`; review parameter substitution before accepting the transformation", call.location, call.callee.name));
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
	if path.is_inside(&["common", "scripted_effects"], str::eq) {
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

fn collect_parameters(
	statements: &[AstStatement],
	path: &GamePath,
	parents: &mut Vec<String>,
	owner: &Callable,
	mappings: &BTreeMap<String, String>,
	sensitive: &mut ParameterPaths,
	hazardous: &mut BTreeMap<Callable, BTreeSet<GamePathBuf>>,
) {
	let schema = super::super::cwt::rule_engine();
	let context = script_context(schema, path, parents);
	for statement in statements {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		let kinds = reference_kinds(schema, context, key);
		let mut uses = Vec::new();
		if kinds.culture_key {
			uses.push(key.clone());
		}
		match value {
			AstValue::Scalar { value, .. }
				if kinds.culture_value || (!kinds.group_value && key.contains("culture")) =>
			{
				uses.push(value.as_text())
			}
			AstValue::Block { items, .. } => {
				parents.push(key.clone());
				collect_parameters(items, path, parents, owner, mappings, sensitive, hazardous);
				parents.pop();
			}
			_ => {}
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
							value: AstValue::Scalar { value, .. },
							..
						} => Some((key.clone(), value.as_text())),
						_ => None,
					})
					.collect(),
				_ => Vec::new(),
			};
			calls.push(Call {
				owner: owner.cloned(),
				callee,
				bindings,
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

fn refers_to_changed(value: &str, mappings: &BTreeMap<String, String>) -> bool {
	mappings.contains_key(value)
		|| parameter_tokens(value).any(|part| {
			part.split_once('|')
				.is_some_and(|(_, default)| mappings.contains_key(default))
		})
}
