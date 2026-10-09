//! Symbolic scope bindings and script calls along one feature's behaviour.
//!
//! The walk follows the actual nested scope stack and each call's parameters.
//! It records which object every scope reference denotes, symbolically, not
//! which scope type it has: two country scopes can be different countries.

use std::collections::BTreeMap;

use super::host::HostRoot;
use crate::game::eu4::base::builtin::{
	is_builtin_effect, is_builtin_iterator, is_builtin_scope_changer, is_builtin_trigger,
};
use crate::game::eu4::cwt::{is_country_tag_selector, is_province_id_selector};
use crate::game::eu4::derived_localisation::Rebinding;
use crate::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, SpanRange, parse_clausewitz_content,
};

/// The object a scope denotes when the feature runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Binding {
	/// The clicking country (`FROM`).
	Actor,
	/// The object the host selects when it is not the clicking country.
	Target(HostRoot),
	/// An object reached from another through a link or iterator.
	Link {
		from: Box<Binding>,
		link: String,
	},
	EventTarget(String),
	/// A country tag or province id written in the script.
	Literal(String),
	Unknown(String),
}

/// When a statement is evaluated relative to the click.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
	Potential,
	Trigger,
	Effect,
}

/// One scope reference or switch and the object it denotes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeUse {
	pub stage: Stage,
	pub path: Vec<String>,
	pub token: String,
	pub binding: Binding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptKind {
	Effect,
	Trigger,
}

/// A scripted trigger or effect invocation, after parameter substitution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptCall {
	pub stage: Stage,
	pub kind: ScriptKind,
	pub name: String,
	pub params: Vec<(String, String)>,
	pub path: Vec<String>,
}

/// A scripted trigger or effect source file, decoded to text. Parameters are
/// substituted in the text before parsing, as the game does, so a body such
/// as `add_adm_power = -$amount$` keeps its meaning.
#[derive(Clone, Debug)]
pub struct ScriptSource {
	pub path: crate::model::GamePathBuf,
	pub text: String,
}

/// Effective scripted trigger and effect definitions, each kept as the source
/// text of its body. A name defined more than once is kept as ambiguous
/// rather than resolved to an arbitrary winner.
#[derive(Default)]
pub(super) struct ScriptDefinitions {
	effects: BTreeMap<String, Vec<String>>,
	triggers: BTreeMap<String, Vec<String>>,
}

impl ScriptDefinitions {
	pub(super) fn new(effects: &[ScriptSource], triggers: &[ScriptSource]) -> Self {
		Self {
			effects: top_level_bodies(effects),
			triggers: top_level_bodies(triggers),
		}
	}

	fn find(&self, stage: Stage, key: &str) -> Option<(ScriptKind, &[String])> {
		let effect = self
			.effects
			.get(key)
			.map(|bodies| (ScriptKind::Effect, bodies.as_slice()));
		let trigger = self
			.triggers
			.get(key)
			.map(|bodies| (ScriptKind::Trigger, bodies.as_slice()));
		match stage {
			Stage::Effect => effect.or(trigger),
			Stage::Potential | Stage::Trigger => trigger.or(effect),
		}
	}
}

fn top_level_bodies(sources: &[ScriptSource]) -> BTreeMap<String, Vec<String>> {
	let mut bodies: BTreeMap<String, Vec<String>> = BTreeMap::new();
	for source in sources {
		let parsed = parse_clausewitz_content(&source.path, &source.text);
		for statement in &parsed.ast.statements {
			if let AstStatement::Assignment {
				key,
				value: AstValue::Block { span, .. },
				..
			} = statement && let Some(text) = source.text.get(span.start.offset..span.end.offset)
			{
				bodies.entry(key.clone()).or_default().push(text.to_owned());
			}
		}
	}
	bodies
}

/// Blocks that evaluate their children in the enclosing scope.
const SAME_SCOPE_CONTAINERS: &[&str] = &[
	"and",
	"or",
	"not",
	"nor",
	"nand",
	"if",
	"else_if",
	"else",
	"limit",
	"hidden_effect",
	"hidden_trigger",
	"custom_trigger_tooltip",
	"random",
	"random_list",
	"while",
	// Shows its effects in the tooltip without running them.
	"tooltip",
];

/// Iterators the builtin catalog lacks; the semantic index builder in
/// `script/mod.rs` recognizes the same keys explicitly.
const EFFECT_ITERATORS: &[&str] = &[
	"every_owned_province",
	"every_country",
	"every_subject_country",
	"every_known_country",
	"every_province",
	"random_country",
	"random_owned_province",
	"random_province",
];

pub(super) struct ContextWalk<'a> {
	definitions: &'a ScriptDefinitions,
	root: Binding,
	stage: Stage,
	call_stack: Vec<String>,
	pub uses: Vec<ScopeUse>,
	pub calls: Vec<ScriptCall>,
	pub unresolved: Vec<String>,
}

impl<'a> ContextWalk<'a> {
	pub(super) fn new(definitions: &'a ScriptDefinitions, root: Binding) -> Self {
		Self {
			definitions,
			root,
			stage: Stage::Effect,
			call_stack: Vec::new(),
			uses: Vec::new(),
			calls: Vec::new(),
			unresolved: Vec::new(),
		}
	}

	/// Walk one condition or effect block, which runs with `THIS = ROOT`.
	pub(super) fn walk(&mut self, stage: Stage, origin: &str, statements: &[AstStatement]) {
		self.stage = stage;
		let mut stack = vec![self.root.clone()];
		let mut path = vec![origin.to_owned()];
		self.commands(statements, &mut stack, &mut path, "");
	}

	fn commands(
		&mut self,
		statements: &[AstStatement],
		stack: &mut Vec<Binding>,
		path: &mut Vec<String>,
		parent: &str,
	) {
		for statement in statements {
			let AstStatement::Assignment { key, value, .. } = statement else {
				self.loose_item(statement, path);
				continue;
			};
			path.push(key.clone());
			self.flag_parameters(key, path);
			match value {
				AstValue::Scalar { value, .. } => {
					self.command_scalar(key, value, stack, path, parent)
				}
				AstValue::Block { items, .. } => {
					self.command_block(key, items, stack, path, parent)
				}
			}
			path.pop();
		}
	}

	fn command_scalar(
		&mut self,
		key: &str,
		value: &ScalarValue,
		stack: &mut Vec<Binding>,
		path: &mut Vec<String>,
		parent: &str,
	) {
		let text = value.as_text();
		if is_block_argument(parent, key) {
			self.reference(&text, stack, path);
			return;
		}
		if let Some((kind, bodies)) = self.definitions.find(self.stage, key) {
			// `name = yes` invokes without parameters; `= no` negates a trigger.
			self.call(kind, key, bodies, Vec::new(), stack, path);
			return;
		}
		let lower = key.to_ascii_lowercase();
		if lower == "save_event_target_as" || lower == "save_global_event_target_as" {
			self.record(path, format!("{key} = {text}"), current(stack));
			return;
		}
		// A link with a scalar value, such as `controller = FROM`, compares
		// the linked object instead of switching to it.
		let comparison = is_builtin_scope_changer(key);
		if !is_builtin_trigger(key) && !is_builtin_effect(key) && !comparison {
			self.unresolved
				.push(format!("unknown command `{key}` at {}", path.join(" > ")));
			return;
		}
		self.reference(&text, stack, path);
	}

	fn command_block(
		&mut self,
		key: &str,
		items: &[AstStatement],
		stack: &mut Vec<Binding>,
		path: &mut Vec<String>,
		parent: &str,
	) {
		let lower = key.to_ascii_lowercase();
		let weighted_option = parent == "random_list" && key.chars().all(|c| c.is_ascii_digit());
		if SAME_SCOPE_CONTAINERS.contains(&lower.as_str()) || weighted_option {
			self.commands(items, stack, path, &lower);
			return;
		}
		if let Some((kind, bodies)) = self.definitions.find(self.stage, key) {
			let params = self.parameters(key, items, path);
			self.call(kind, key, bodies, params, stack, path);
			return;
		}
		if let Some(binding) = self.switch(key, stack, path) {
			self.record(path, key.to_owned(), binding.clone());
			stack.push(binding);
			self.commands(items, stack, path, &lower);
			stack.pop();
			return;
		}
		if is_builtin_trigger(key) || is_builtin_effect(key) {
			self.arguments(items, stack, path);
			return;
		}
		self.unresolved
			.push(format!("unknown block `{key}` at {}", path.join(" > ")));
	}

	/// The object a scope-switching block key denotes, if it is one.
	fn switch(&mut self, key: &str, stack: &[Binding], path: &[String]) -> Option<Binding> {
		if let Some(binding) = self.alias(key, stack, path) {
			return Some(binding);
		}
		if is_builtin_iterator(key)
			|| is_builtin_scope_changer(key)
			|| EFFECT_ITERATORS.contains(&key)
		{
			return Some(Binding::Link {
				from: Box::new(current(stack)),
				link: key.to_owned(),
			});
		}
		if is_country_tag_selector(key) || is_province_id_selector(key) {
			return Some(Binding::Literal(key.to_owned()));
		}
		None
	}

	/// `ROOT`, `FROM`, `THIS`, `PREV` and `event_target:` references.
	fn alias(&mut self, token: &str, stack: &[Binding], path: &[String]) -> Option<Binding> {
		if let Some(name) = token.strip_prefix("event_target:") {
			return Some(Binding::EventTarget(name.to_owned()));
		}
		match token.to_ascii_uppercase().as_str() {
			"ROOT" => Some(self.root.clone()),
			"FROM" => Some(Binding::Actor),
			"THIS" => Some(current(stack)),
			"PREV" => Some(match stack.len().checked_sub(2) {
				Some(index) => stack[index].clone(),
				None => {
					self.unresolved.push(format!(
						"`PREV` has no enclosing scope at {}",
						path.join(" > ")
					));
					Binding::Unknown("PREV".into())
				}
			}),
			"PREVPREV" | "FROMFROM" | "ROOTFROM" => {
				self.unresolved
					.push(format!("`{token}` is not modelled at {}", path.join(" > ")));
				Some(Binding::Unknown(token.to_owned()))
			}
			_ => None,
		}
	}

	/// Arguments of a builtin trigger or effect, such as `who = FROM`. Their
	/// keys are parameters, not commands, and they keep the enclosing scope.
	fn arguments(
		&mut self,
		items: &[AstStatement],
		stack: &mut Vec<Binding>,
		path: &mut Vec<String>,
	) {
		for statement in items {
			let AstStatement::Assignment { key, value, .. } = statement else {
				self.loose_item(statement, path);
				continue;
			};
			path.push(key.clone());
			self.flag_parameters(key, path);
			match value {
				AstValue::Scalar { value, .. } => self.reference(&value.as_text(), stack, path),
				AstValue::Block { items, .. } => self.arguments(items, stack, path),
			}
			path.pop();
		}
	}

	fn reference(&mut self, text: &str, stack: &[Binding], path: &[String]) {
		self.flag_parameters(text, path);
		if let Some(binding) = self.alias(text, stack, path) {
			self.record(path, text.to_owned(), binding);
		}
	}

	fn parameters(
		&mut self,
		name: &str,
		items: &[AstStatement],
		path: &[String],
	) -> Vec<(String, String)> {
		let mut params = Vec::new();
		for statement in items {
			match statement {
				AstStatement::Assignment {
					key,
					value: AstValue::Scalar { value, .. },
					..
				} => params.push((key.clone(), value.as_text())),
				AstStatement::Comment { .. } => {}
				_ => self.unresolved.push(format!(
					"non-scalar argument to `{name}` at {}",
					path.join(" > ")
				)),
			}
		}
		params
	}

	fn call(
		&mut self,
		kind: ScriptKind,
		name: &str,
		bodies: &[String],
		params: Vec<(String, String)>,
		stack: &mut Vec<Binding>,
		path: &mut Vec<String>,
	) {
		self.calls.push(ScriptCall {
			stage: self.stage,
			kind,
			name: name.to_owned(),
			params: params.clone(),
			path: path.clone(),
		});
		if bodies.len() != 1 {
			self.unresolved.push(format!(
				"`{name}` has {} effective definitions",
				bodies.len()
			));
			return;
		}
		if self.call_stack.iter().any(|caller| caller == name) {
			self.unresolved.push(format!(
				"recursive call to `{name}` at {}",
				path.join(" > ")
			));
			return;
		}
		let Some(body) = expand_body(name, &bodies[0], &params) else {
			self.unresolved.push(format!(
				"`{name}` does not parse after parameter substitution at {}",
				path.join(" > ")
			));
			return;
		};
		// A scripted trigger or effect runs in its caller's scope.
		self.call_stack.push(name.to_owned());
		self.commands(&body, stack, path, "");
		self.call_stack.pop();
	}

	/// A scalar without a key, such as an unbound `$name$` left by substitution.
	fn loose_item(&mut self, statement: &AstStatement, path: &[String]) {
		if let AstStatement::Item {
			value: AstValue::Scalar { value, .. },
			..
		} = statement
		{
			self.flag_parameters(&value.as_text(), path);
		}
	}

	fn flag_parameters(&mut self, text: &str, path: &[String]) {
		if let Some(start) = text.find('$') {
			let rest = &text[start + 1..];
			let name = rest.split('$').next().unwrap_or(rest);
			self.unresolved.push(format!(
				"unbound parameter `${name}$` at {}",
				path.join(" > ")
			));
		}
	}

	fn record(&mut self, path: &[String], token: String, binding: Binding) {
		self.uses.push(ScopeUse {
			stage: self.stage,
			path: path.to_vec(),
			token,
			binding,
		});
	}
}

fn current(stack: &[Binding]) -> Binding {
	stack
		.last()
		.cloned()
		.unwrap_or_else(|| Binding::Unknown("THIS".into()))
}

/// Scalar keys that configure their enclosing block rather than run as
/// commands: a tooltip's localisation key, or an iterator's `type = all`.
fn is_block_argument(parent: &str, key: &str) -> bool {
	match key {
		"tooltip" => parent == "custom_trigger_tooltip",
		"type" => {
			is_builtin_iterator(parent)
				|| is_builtin_scope_changer(parent)
				|| EFFECT_ITERATORS.contains(&parent)
		}
		_ => false,
	}
}

/// Keep `[[name] ... ]` only when `name` is passed and `[[!name] ... ]` only
/// when it is not, as the game does before substituting parameters.
fn expand_conditionals(text: &str, params: &BTreeMap<String, String>) -> String {
	let mut output = String::new();
	let mut rest = text;
	while let Some(start) = rest.find("[[") {
		let Some(header) = rest[start + 2..].find(']') else {
			break;
		};
		let condition = &rest[start + 2..start + 2 + header];
		let body_start = start + 2 + header + 1;
		let Some(body_length) = conditional_body_length(&rest[body_start..]) else {
			break;
		};
		output.push_str(&rest[..start]);
		let (negated, name) = match condition.strip_prefix('!') {
			Some(name) => (true, name),
			None => (false, condition),
		};
		if params.contains_key(name) != negated {
			output.push_str(&expand_conditionals(
				&rest[body_start..body_start + body_length],
				params,
			));
		}
		rest = &rest[body_start + body_length + 1..];
	}
	output.push_str(rest);
	output
}

/// Length of a conditional block's body up to its closing `]`, skipping nested
/// `[[name] ... ]` blocks.
fn conditional_body_length(text: &str) -> Option<usize> {
	let mut depth = 1;
	let mut index = 0;
	while index < text.len() {
		if text[index..].starts_with("[[") {
			index += text[index..].find(']')? + 1;
			depth += 1;
			continue;
		}
		if text[index..].starts_with(']') {
			depth -= 1;
			if depth == 0 {
				return Some(index);
			}
		}
		index += text[index..].chars().next()?.len_utf8();
	}
	None
}

/// A scripted body with `params` substituted in its text and parsed, as the
/// game expands it at each call.
fn expand_body(name: &str, body: &str, params: &[(String, String)]) -> Option<Vec<AstStatement>> {
	let params = params.iter().cloned().collect::<BTreeMap<_, _>>();
	let text = format!("{name} = {}", substitute(body, &params));
	let parsed = parse_clausewitz_content(
		crate::model::GamePath::new("scripted_expansion.txt").expect("synthetic game path"),
		&text,
	);
	if !parsed.diagnostics.is_empty() {
		return None;
	}
	match parsed.ast.statements.into_iter().next() {
		Some(AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		}) => Some(items),
		_ => Some(Vec::new()),
	}
}

/// Rebinds an action's absolute scope references for a new entry point.
///
/// Inside a documented host `ROOT` and `FROM` never change with nesting, so
/// the extraction's bindings justify replacing each by name: `FROM` (the
/// clicking country) becomes `ROOT`, the deciding country of a decision or
/// event chain, and a host target `ROOT` becomes `event_target:<target>`.
/// Relative `THIS` and `PREV` keep their meaning when the caller wraps the
/// result in the target's scope. A scripted call whose expansion mentions
/// either name is inlined with its parameters, as the game expands it; other
/// calls stay calls.
pub(super) struct Rewrite<'a> {
	pub(super) definitions: &'a ScriptDefinitions,
	/// The saved event target that replaces a host-target `ROOT`, if any.
	pub(super) target: Option<&'a str>,
	/// Rebinds localisation keys the script shows, when text is derived.
	pub(super) texts: Option<&'a Localise<'a>>,
	/// The event target names [`TARGET_PARAMETER`] can stand for.
	pub(super) instances: &'a [String],
}

/// The scripted-effect parameter naming a saved province.
pub(super) const TARGET_PARAMETER: &str = "$target$";

/// Maps a localisation key shown by script with `target` (and, for
/// [`TARGET_PARAMETER`], its `instances`) to the key to reference.
pub(super) type Localise<'a> = dyn Fn(&str, Option<&str>, &[String]) -> String + 'a;

/// Text rebinding for one entry-point context: `From` becomes `Root`, and a
/// host-target `Root` becomes the saved province.
pub(super) struct HostText<'a> {
	definitions: &'a ScriptDefinitions,
	target: Option<String>,
	tag: String,
}

impl<'a> HostText<'a> {
	pub(super) fn new(definitions: &'a ScriptDefinitions, target: Option<&str>) -> Self {
		Self {
			definitions,
			target: target.map(str::to_owned),
			tag: target.unwrap_or_default().to_owned(),
		}
	}
}

impl Rebinding for HostText<'_> {
	fn head(&self, head: &str) -> Option<String> {
		match head.to_ascii_uppercase().as_str() {
			"FROM" => Some("Root".to_owned()),
			"ROOT" => self.target.clone(),
			_ => None,
		}
	}

	fn triggers(&self, statements: &[AstStatement]) -> Vec<AstStatement> {
		Rewrite {
			definitions: self.definitions,
			target: self.target.as_deref(),
			texts: None,
			instances: &[],
		}
		.statements(Stage::Potential, statements)
	}

	fn tag(&self) -> &str {
		&self.tag
	}
}

impl Rewrite<'_> {
	pub(super) fn statements(
		&self,
		stage: Stage,
		statements: &[AstStatement],
	) -> Vec<AstStatement> {
		let mut output = Vec::new();
		for statement in statements {
			let AstStatement::Assignment {
				key,
				key_span,
				value,
				span,
			} = statement
			else {
				output.push(statement.clone());
				continue;
			};
			if let Some(inlined) = self.inline(stage, key, value) {
				output.extend(inlined);
				continue;
			}
			output.push(AstStatement::Assignment {
				key: self.token(key),
				key_span: key_span.clone(),
				value: match value {
					AstValue::Scalar { value, span } => AstValue::Scalar {
						value: match value {
							ScalarValue::Identifier(text)
								if matches!(key.as_str(), "custom_tooltip" | "tooltip") =>
							{
								ScalarValue::Identifier(self.localised(text))
							}
							ScalarValue::Identifier(text) => {
								ScalarValue::Identifier(self.token(text))
							}
							ScalarValue::String(text) => ScalarValue::String(self.text(text)),
							other => other.clone(),
						},
						span: span.clone(),
					},
					AstValue::Block { items, span } => AstValue::Block {
						items: self.statements(stage, items),
						span: span.clone(),
					},
				},
				span: span.clone(),
			});
		}
		output
	}

	/// The localisation key to show for `key` from this script's context.
	pub(super) fn localised(&self, key: &str) -> String {
		match self.texts {
			Some(texts) => texts(key, self.target, self.instances),
			None => key.to_owned(),
		}
	}

	fn token(&self, text: &str) -> String {
		match text.to_ascii_uppercase().as_str() {
			"FROM" => "ROOT".to_owned(),
			"ROOT" => match self.target {
				Some(target) => format!("event_target:{target}"),
				None => text.to_owned(),
			},
			_ => text.to_owned(),
		}
	}

	/// Rebind `[Root.…]` and `[From.…]` in text written inline in the script,
	/// such as a `custom_trigger_tooltip`, by the same rule as the script.
	/// Localisation keys are text of other files and are not touched here.
	fn text(&self, text: &str) -> String {
		let mut output = String::new();
		let mut rest = text;
		while let Some(start) = rest.find('[') {
			output.push_str(&rest[..=start]);
			rest = &rest[start + 1..];
			let end = rest.find(['.', ']']).unwrap_or(rest.len());
			let head = &rest[..end];
			match head.to_ascii_uppercase().as_str() {
				"FROM" => output.push_str("Root"),
				"ROOT" => output.push_str(self.target.unwrap_or(head)),
				_ => output.push_str(head),
			}
			rest = &rest[end..];
		}
		output.push_str(rest);
		output
	}

	fn renames(&self, text: &str) -> bool {
		self.token(text) != text || self.text(text) != text
	}

	/// The expanded statements of a scripted call that needs rebinding.
	fn inline(&self, stage: Stage, key: &str, value: &AstValue) -> Option<Vec<AstStatement>> {
		let body = self.expansion(stage, key, value, 0)?;
		if !self.mentions(stage, &body, 0) {
			return None;
		}
		let rewritten = self.statements(stage, &body);
		// `trigger = no` negates the scripted trigger.
		let negated = matches!(value, AstValue::Scalar { value, .. } if value.as_text() == "no");
		Some(if negated {
			vec![AstStatement::Assignment {
				key: "NOT".into(),
				key_span: SpanRange {
					start: span_start(),
					end: span_start(),
				},
				value: AstValue::Block {
					items: rewritten,
					span: SpanRange {
						start: span_start(),
						end: span_start(),
					},
				},
				span: SpanRange {
					start: span_start(),
					end: span_start(),
				},
			}]
		} else {
			rewritten
		})
	}

	fn expansion(
		&self,
		stage: Stage,
		key: &str,
		value: &AstValue,
		depth: usize,
	) -> Option<Vec<AstStatement>> {
		let (_, bodies) = self.definitions.find(stage, key)?;
		let [body] = bodies else {
			return None;
		};
		if depth > 32 {
			return None;
		}
		let params = match value {
			AstValue::Block { items, .. } => items
				.iter()
				.filter_map(|statement| match statement {
					AstStatement::Assignment {
						key,
						value: AstValue::Scalar { value, .. },
						..
					} => Some((key.clone(), value.as_text())),
					_ => None,
				})
				.collect(),
			AstValue::Scalar { .. } => Vec::new(),
		};
		expand_body(key, body, &params)
	}

	fn mentions(&self, stage: Stage, statements: &[AstStatement], depth: usize) -> bool {
		statements.iter().any(|statement| {
			let AstStatement::Assignment { key, value, .. } = statement else {
				return false;
			};
			if self.renames(key) {
				return true;
			}
			if let Some(body) = self.expansion(stage, key, value, depth + 1) {
				return self.mentions(stage, &body, depth + 1);
			}
			match value {
				AstValue::Scalar { value, .. } => self.renames(&value.as_text()),
				AstValue::Block { items, .. } => self.mentions(stage, items, depth),
			}
		})
	}
}

impl Rewrite<'_> {
	/// The checks of a province-host action for a trigger that iterates
	/// provinces from the deciding country, such as
	/// `any_province = { ... }`: `THIS` is the candidate province and `ROOT`
	/// the deciding country. A host-target `ROOT` becomes `THIS` where no
	/// scope has been entered and `PREV` one scope deeper; a trigger cannot
	/// save the province, so a deeper reference has no equivalent and the
	/// result is `None`. `FROM` becomes `ROOT` as everywhere.
	pub(super) fn relative(
		&self,
		stage: Stage,
		statements: &[AstStatement],
	) -> Option<Vec<AstStatement>> {
		self.relative_at(stage, statements, 0)
	}

	fn relative_at(
		&self,
		stage: Stage,
		statements: &[AstStatement],
		depth: usize,
	) -> Option<Vec<AstStatement>> {
		let target = |depth: usize| match depth {
			0 => Some("THIS"),
			1 => Some("PREV"),
			_ => None,
		};
		let mut output = Vec::new();
		for statement in statements {
			let AstStatement::Assignment {
				key,
				key_span,
				value,
				span,
			} = statement
			else {
				output.push(statement.clone());
				continue;
			};
			if let Some(body) = self.expansion(stage, key, value, 0)
				&& self.mentions(stage, &body, 0)
			{
				let body = self.relative_at(stage, &body, depth)?;
				let negated =
					matches!(value, AstValue::Scalar { value, .. } if value.as_text() == "no");
				if negated {
					output.push(AstStatement::Assignment {
						key: "NOT".into(),
						key_span: key_span.clone(),
						value: AstValue::Block {
							items: body,
							span: span.clone(),
						},
						span: span.clone(),
					});
				} else {
					output.extend(body);
				}
				continue;
			}
			// `ROOT = { ... }` with no scope entered is the candidate itself.
			if depth == 0
				&& key.eq_ignore_ascii_case("ROOT")
				&& let AstValue::Block { items, .. } = value
			{
				output.extend(self.relative_at(stage, items, depth)?);
				continue;
			}
			let new_key = match key.to_ascii_uppercase().as_str() {
				"ROOT" => target(depth)?.to_owned(),
				"FROM" => "ROOT".to_owned(),
				_ => key.clone(),
			};
			let value = match value {
				AstValue::Scalar { value, span } => AstValue::Scalar {
					value: match value {
						ScalarValue::Identifier(text) => {
							ScalarValue::Identifier(match text.to_ascii_uppercase().as_str() {
								"ROOT" => target(depth)?.to_owned(),
								"FROM" => "ROOT".to_owned(),
								_ => text.clone(),
							})
						}
						other => other.clone(),
					},
					span: span.clone(),
				},
				AstValue::Block { items, span } => {
					let inner = if switches_scope(key) {
						depth + 1
					} else {
						depth
					};
					AstValue::Block {
						items: self.relative_at(stage, items, inner)?,
						span: span.clone(),
					}
				}
			};
			output.push(AstStatement::Assignment {
				key: new_key,
				key_span: key_span.clone(),
				value,
				span: span.clone(),
			});
		}
		Some(output)
	}
}

/// Whether a block key enters another scope, as [`ContextWalk`] models it.
fn switches_scope(key: &str) -> bool {
	matches!(
		key.to_ascii_uppercase().as_str(),
		"ROOT" | "FROM" | "THIS" | "PREV"
	) || key.starts_with("event_target:")
		|| is_builtin_iterator(key)
		|| is_builtin_scope_changer(key)
		|| EFFECT_ITERATORS.contains(&key)
		|| is_country_tag_selector(key)
		|| is_province_id_selector(key)
}

fn span_start() -> crate::game::eu4::script::parser::Span {
	crate::game::eu4::script::parser::Span {
		line: 0,
		column: 0,
		offset: 0,
	}
}

/// Replace `$name$` and `$name|default$`. An unbound parameter without a
/// default is left in place for the caller to report.
fn substitute(text: &str, params: &BTreeMap<String, String>) -> String {
	let expanded = expand_conditionals(text, params);
	let mut output = String::new();
	let mut rest = expanded.as_str();
	while let Some(start) = rest.find('$') {
		let Some(length) = rest[start + 1..].find('$') else {
			break;
		};
		let inner = &rest[start + 1..start + 1 + length];
		let (name, default) = match inner.split_once('|') {
			Some((name, default)) => (name, Some(default)),
			None => (inner, None),
		};
		output.push_str(&rest[..start]);
		match params.get(name).map(String::as_str).or(default) {
			Some(value) => output.push_str(value),
			None => output.push_str(&rest[start..start + length + 2]),
		}
		rest = &rest[start + length + 2..];
	}
	output.push_str(rest);
	output
}

#[cfg(test)]
pub(super) fn substitute_for_test(text: &str, params: &[(&str, &str)]) -> String {
	let params = params
		.iter()
		.map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
		.collect();
	substitute(text, &params)
}
