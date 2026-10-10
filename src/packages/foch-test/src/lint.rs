//! Checks before launching the game, and what each case may touch.
//!
//! Facts about the project come from the caller, which derives them from
//! Foch's analysis. Missing facts skip the corresponding check rather than
//! guessing. Footprints are deliberately conservative: anything that might
//! reach another country or global state makes a case unsuitable for sharing.

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::model::{CaseIndex, EventId, EventInfo, NativeBlock, Step, Tag};
use crate::plan::Expanded;
use crate::source::SourceSpan;
use foch::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, parse_clausewitz_content,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// What the caller knows about the project and base game. `None` means
/// unknown, which disables the related check.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProjectFacts {
	pub tags: Option<BTreeSet<Tag>>,
	pub events: Option<BTreeSet<EventId>>,
	pub effects: Option<BTreeSet<String>>,
	pub triggers: Option<BTreeSet<String>>,
}

/// An over-approximation of the state a case reads or writes.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
	pub tags: BTreeSet<Tag>,
	pub global: bool,
	/// Why the footprint cannot be bounded, if it cannot.
	pub opaque: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LintResult {
	pub diagnostics: Vec<Diagnostic>,
	pub footprints: BTreeMap<CaseIndex, Footprint>,
}

/// Blocks that only structure conditions or effects in the current scope.
const CONTROL_BLOCKS: &[&str] = &[
	"if",
	"else_if",
	"else",
	"limit",
	"AND",
	"OR",
	"NOT",
	"NOR",
	"NAND",
	"hidden_effect",
	"hidden_trigger",
	"custom_tooltip",
	"tooltip",
	"ai_chance",
	"modifier",
	"mean_time_to_happen",
];

/// Event-level blocks executed or evaluated in the event's own scope.
const EVENT_BLOCKS: &[&str] = &["trigger", "immediate", "option", "after"];

const GLOBAL_KEYS: &[&str] = &["set_global_flag", "clr_global_flag", "has_global_flag"];

pub fn lint(expanded: &Expanded, facts: &ProjectFacts) -> LintResult {
	let events: BTreeMap<&EventId, &EventInfo> = expanded
		.events
		.iter()
		.map(|event| (&event.id, event))
		.collect();
	let mut result = LintResult::default();
	for planned in &expanded.cases {
		let case = &planned.case;
		let mut footprint = Footprint::default();
		footprint.tags.insert(case.start.tag.clone());
		if let Some(tags) = &facts.tags
			&& !tags.contains(&case.start.tag)
		{
			result.diagnostics.push(Diagnostic::model_error(
				DiagnosticCode::UnknownTag,
				Some(case.origin.clone()),
				format!(
					"country {} is not defined by the mod or base game",
					case.start.tag
				),
			));
		}
		let mut effects: Vec<&NativeBlock> = Vec::new();
		let mut conditions = Vec::new();
		for fixture in &planned.fixtures {
			effects.extend(&fixture.effect);
			conditions.extend(&fixture.check);
		}
		for step in &case.steps {
			match step {
				Step::Effect { block } => effects.push(block),
				Step::Expect { clauses } => conditions.extend(clauses),
				Step::Fire { event, span } => {
					fire_target(
						event,
						span,
						&events,
						facts,
						&mut result.diagnostics,
						&mut footprint,
					);
				}
				Step::AdvanceDays { .. } => {}
			}
		}
		for block in effects {
			let statements = parse(&block.text);
			check_keys(
				&statements,
				&block.span,
				facts,
				Context::Effect,
				&mut result.diagnostics,
			);
			touch(&statements, &mut footprint);
		}
		for clause in conditions {
			let statements = parse(&clause.text);
			check_keys(
				&statements,
				&clause.span,
				facts,
				Context::Condition,
				&mut result.diagnostics,
			);
			touch(&statements, &mut footprint);
		}
		result.footprints.insert(planned.index, footprint);
	}
	result
}

fn fire_target(
	event: &EventId,
	span: &SourceSpan,
	events: &BTreeMap<&EventId, &EventInfo>,
	facts: &ProjectFacts,
	diagnostics: &mut Vec<Diagnostic>,
	footprint: &mut Footprint,
) {
	match events.get(event) {
		Some(info) if !info.invocable => diagnostics.push(Diagnostic::error(
			DiagnosticCode::UnsupportedTarget,
			Some(span.clone()),
			format!("{event} must declare hidden = yes and is_triggered_only = yes to be fired"),
		)),
		Some(info) => {
			for statement in parse(&info.body) {
				if let AstStatement::Assignment {
					key,
					value: AstValue::Block { items, .. },
					..
				} = &statement && EVENT_BLOCKS.contains(&key.as_str())
				{
					touch(items, footprint);
				}
			}
		}
		None => {
			footprint
				.opaque
				.get_or_insert_with(|| format!("fires {event}, whose body was not collected"));
			let known = facts.events.as_ref().map(|events| events.contains(event));
			let message = match known {
				Some(false) => format!("event {event} is not defined"),
				_ => format!(
					"cannot verify that {event} is hidden and triggered-only; it was not collected"
				),
			};
			let code = if known == Some(false) {
				DiagnosticCode::UnknownEvent
			} else {
				DiagnosticCode::UnsupportedTarget
			};
			diagnostics.push(Diagnostic::model_error(code, Some(span.clone()), message));
		}
	}
}

fn parse(text: &str) -> Vec<AstStatement> {
	parse_clausewitz_content(crate::game_path("lint.txt"), text)
		.ast
		.statements
}

#[derive(Clone, Copy, PartialEq)]
enum Context {
	Effect,
	Condition,
}

/// Flags top-level keys used in the wrong kind of block. Keys known as both
/// or neither (scopes, scripted items) are left alone.
fn check_keys(
	statements: &[AstStatement],
	span: &SourceSpan,
	facts: &ProjectFacts,
	context: Context,
	diagnostics: &mut Vec<Diagnostic>,
) {
	let (Some(effects), Some(triggers)) = (&facts.effects, &facts.triggers) else {
		return;
	};
	for statement in statements {
		let AstStatement::Assignment { key, .. } = statement else {
			continue;
		};
		let (is_effect, is_trigger) = (effects.contains(key), triggers.contains(key));
		match context {
			Context::Effect if is_trigger && !is_effect => {
				diagnostics.push(Diagnostic::model_error(
					DiagnosticCode::TriggerAsEffect,
					Some(span.clone()),
					format!("{key} is a trigger, but this block runs effects"),
				))
			}
			Context::Condition if is_effect && !is_trigger => {
				diagnostics.push(Diagnostic::model_error(
					DiagnosticCode::EffectAsTrigger,
					Some(span.clone()),
					format!("{key} is an effect, but this block is a condition"),
				))
			}
			_ => {}
		}
	}
}

/// A tag-shaped word that may name a country. `NOT` and `AND` are logic
/// keywords no country uses; `NOR` is also Norway, so it is kept and the
/// over-approximation stays safe.
fn country(word: &str) -> Option<Tag> {
	if matches!(word, "NOT" | "AND") {
		return None;
	}
	Tag::parse(word).ok()
}

fn touch(statements: &[AstStatement], footprint: &mut Footprint) {
	for statement in statements {
		let (key, value) = match statement {
			AstStatement::Assignment { key, value, .. } => (Some(key.as_str()), value),
			AstStatement::Item { value, .. } => (None, value),
			AstStatement::Comment { .. } => continue,
		};
		if let Some(key) = key {
			if GLOBAL_KEYS.contains(&key) {
				footprint.global = true;
			}
			if let Some(tag) = country(key) {
				footprint.tags.insert(tag);
			}
		}
		match value {
			AstValue::Scalar {
				value: ScalarValue::Identifier(text) | ScalarValue::String(text),
				..
			} => {
				if let Some(tag) = country(text) {
					footprint.tags.insert(tag);
				}
			}
			AstValue::Scalar { .. } => {}
			AstValue::Block { items, .. } => {
				let key = key.unwrap_or_default();
				if CONTROL_BLOCKS.contains(&key) || country(key).is_some() {
					touch(items, footprint);
				} else {
					footprint.opaque.get_or_insert_with(|| {
						format!("{key} = {{ ... }} may change scope or reach other countries")
					});
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn footprint(text: &str) -> Footprint {
		let mut footprint = Footprint::default();
		touch(&parse(text), &mut footprint);
		footprint
	}

	#[test]
	fn footprints_are_conservative() {
		let simple = footprint(
			"set_country_flag = a if = { limit = { has_country_flag = b } add_prestige = 1 }",
		);
		assert!(simple.opaque.is_none() && !simple.global && simple.tags.is_empty());
		let other = footprint("DAN = { set_country_flag = a } add_truce_with = NOR");
		assert_eq!(
			other.tags.iter().map(Tag::as_str).collect::<Vec<_>>(),
			["DAN", "NOR"]
		);
		let logic = footprint("NOT = { has_country_flag = a } AND = { OR = { always = yes } }");
		assert!(logic.tags.is_empty() && logic.opaque.is_none(), "{logic:?}");
		assert!(footprint("set_global_flag = x").global);
		assert!(
			footprint("every_country = { add_prestige = 1 }")
				.opaque
				.is_some()
		);
	}
}
