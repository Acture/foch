//! Derived localisation: effective text copied and rebound for a new context.
//!
//! A transformation that moves script into a different scope context, such
//! as a GUI action into a decision, also moves the text that script shows.
//! Text reading `[Root.…]` or `[From.…]`, directly, through `$KEY$`, or
//! through a customizable localisation command, would show a different
//! object there. [`Derivation`] reads each language's effective text, and
//! when anything in a key needs rebinding emits a renamed copy of the key,
//! and of every command it uses, rebound by a [`Rebinding`]. Keys that need
//! nothing keep their original name. Reviewed [`TextOverrides`] replace the
//! source text before rebinding, so a reviewer writes text as the source mod
//! would have.

use std::collections::{BTreeMap, BTreeSet};

use crate::game::eu4::script::localisation::parse_localisation_values;
use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue, ScalarValue};

/// Each language's effective text of a playset.
#[derive(Clone, Debug, Default)]
pub struct EffectiveLocalisation {
	languages: BTreeMap<String, BTreeMap<String, String>>,
}

impl EffectiveLocalisation {
	/// Add one localisation file. Add files from the highest loader
	/// precedence down: the first definition of a key in a language wins, as
	/// Foch's localisation merge resolves duplicates.
	pub fn add_file(&mut self, raw: &[u8]) {
		for (language, key, value) in parse_localisation_values(raw) {
			self.insert(&language, &key, &value);
		}
	}

	/// Define `key` in `language` unless a higher-precedence text already did.
	pub fn insert(&mut self, language: &str, key: &str, value: &str) {
		self.languages
			.entry(language.to_owned())
			.or_default()
			.entry(key.to_owned())
			.or_insert_with(|| value.to_owned());
	}

	pub fn get(&self, language: &str, key: &str) -> Option<&str> {
		self.languages.get(language)?.get(key).map(String::as_str)
	}

	pub fn defines(&self, key: &str) -> bool {
		self.languages.values().any(|keys| keys.contains_key(key))
	}
}

/// Effective customizable localisation commands (`defined_text`) by name.
#[derive(Clone, Debug, Default)]
pub struct CustomLocalisation {
	definitions: BTreeMap<String, Vec<Vec<AstStatement>>>,
}

impl CustomLocalisation {
	pub fn new(files: &[AstFile]) -> Self {
		let mut definitions: BTreeMap<String, Vec<Vec<AstStatement>>> = BTreeMap::new();
		for file in files {
			for statement in &file.statements {
				let AstStatement::Assignment {
					key,
					value: AstValue::Block { items, .. },
					..
				} = statement
				else {
					continue;
				};
				if key != "defined_text" {
					continue;
				}
				if let Some(name) = scalar(items, "name") {
					definitions.entry(name).or_default().push(items.clone());
				}
			}
		}
		Self { definitions }
	}
}

/// Reviewed replacement text, written in the source context.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TextOverrides {
	/// `key` → language (`None` for every language) → text.
	texts: BTreeMap<String, BTreeMap<Option<String>, String>>,
}

impl TextOverrides {
	pub fn insert(&mut self, language: Option<&str>, key: &str, text: &str) {
		self.texts
			.entry(key.to_owned())
			.or_default()
			.insert(language.map(str::to_owned), text.to_owned());
	}

	fn get(&self, language: &str, key: &str) -> Option<&str> {
		let texts = self.texts.get(key)?;
		texts
			.get(&Some(language.to_owned()))
			.or_else(|| texts.get(&None))
			.map(String::as_str)
	}

	fn has(&self, key: &str) -> bool {
		self.texts.contains_key(key)
	}

	pub fn is_empty(&self) -> bool {
		self.texts.is_empty()
	}
}

/// How text written for the source context is rebound for a new one.
pub trait Rebinding {
	/// The replacement of a scope head such as `Root` in `[Root.GetName]`,
	/// or `None` to keep it.
	fn head(&self, head: &str) -> Option<String>;
	/// A command's triggers rewritten for the new context.
	fn triggers(&self, statements: &[AstStatement]) -> Vec<AstStatement>;
	/// Distinguishes the derived names of different contexts; empty when
	/// there is only one.
	fn tag(&self) -> &str;
}

/// What a derivation adds to the output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DerivedLocalisation {
	/// Language → derived `(key, text)` in derivation order.
	pub entries: BTreeMap<String, Vec<(String, String)>>,
	/// Derived `defined_text` commands.
	pub commands: Vec<AstStatement>,
	/// Original keys the output still references unchanged.
	pub originals: BTreeSet<String>,
	/// Text the derivation could not establish.
	pub problems: Vec<String>,
}

pub struct Derivation<'a> {
	prefix: String,
	languages: Vec<String>,
	localisation: &'a EffectiveLocalisation,
	custom: &'a CustomLocalisation,
	overrides: &'a TextOverrides,
	keys: BTreeMap<(String, String), String>,
	commands: BTreeMap<(String, String), Option<String>>,
	output: DerivedLocalisation,
}

impl<'a> Derivation<'a> {
	/// Derived names start with `prefix` and text is emitted for `languages`.
	pub fn new(
		prefix: &str,
		languages: &[&str],
		localisation: &'a EffectiveLocalisation,
		custom: &'a CustomLocalisation,
		overrides: &'a TextOverrides,
	) -> Self {
		Self {
			prefix: prefix.to_owned(),
			languages: languages
				.iter()
				.map(|language| (*language).to_owned())
				.collect(),
			localisation,
			custom,
			overrides,
			keys: BTreeMap::new(),
			commands: BTreeMap::new(),
			output: DerivedLocalisation::default(),
		}
	}

	/// The key to reference for `key` in the context of `rebinding`.
	pub fn key(&mut self, key: &str, rebinding: &dyn Rebinding) -> String {
		self.derive(key, rebinding, false)
	}

	/// The key to reference from script whose context is chosen by a
	/// parameter: `parameter` stands for each tag of `instances`, which are
	/// all derived when any one needs it.
	pub fn parameterized_key(
		&mut self,
		key: &str,
		instances: &[&dyn Rebinding],
		parameter: &str,
	) -> String {
		let needed = instances
			.iter()
			.any(|rebinding| self.derive(key, *rebinding, false) != key);
		if !needed {
			return key.to_owned();
		}
		for rebinding in instances {
			self.derive(key, *rebinding, true);
		}
		self.name(parameter, key)
	}

	pub fn finish(self) -> DerivedLocalisation {
		self.output
	}

	fn name(&self, tag: &str, key: &str) -> String {
		if tag.is_empty() {
			format!("{}_{key}", self.prefix)
		} else {
			format!("{}_{tag}_{key}", self.prefix)
		}
	}

	fn derive(&mut self, key: &str, rebinding: &dyn Rebinding, force: bool) -> String {
		let memo = (rebinding.tag().to_owned(), key.to_owned());
		if let Some(done) = self.keys.get(&memo)
			&& (!force || *done != key)
		{
			return done.clone();
		}
		// Break `$KEY$` cycles: a key referencing itself keeps its name.
		self.keys.insert(memo.clone(), key.to_owned());
		let sources = self
			.languages
			.clone()
			.into_iter()
			.map(|language| {
				let text = self
					.overrides
					.get(&language, key)
					.or_else(|| self.localisation.get(&language, key))
					.map(str::to_owned);
				(language, text)
			})
			.collect::<Vec<_>>();
		if sources.iter().all(|(_, text)| text.is_none()) {
			self.output
				.problems
				.push(format!("localisation key `{key}` is not defined"));
			self.output.originals.insert(key.to_owned());
			return key.to_owned();
		}
		let mut changed = force || self.overrides.has(key);
		let mut derived = Vec::new();
		for (language, text) in sources {
			let rebound = text.as_deref().map(|text| self.text(text, rebinding));
			changed |= rebound.is_some() && rebound != text;
			derived.push((language, rebound));
		}
		if !changed {
			self.output.originals.insert(key.to_owned());
			return key.to_owned();
		}
		let name = self.name(rebinding.tag(), key);
		for (language, text) in derived {
			match text {
				Some(text) => self
					.output
					.entries
					.entry(language)
					.or_default()
					.push((name.clone(), text)),
				None => self
					.output
					.problems
					.push(format!("localisation key `{key}` has no {language} text")),
			}
		}
		self.keys.insert(memo, name.clone());
		name
	}

	/// Rebind `[Head.…]`, `$KEY$` and command references in one value.
	fn text(&mut self, text: &str, rebinding: &dyn Rebinding) -> String {
		let mut output = String::new();
		let mut rest = text;
		loop {
			let next = rest.find(['[', '$']);
			let Some(start) = next else {
				output.push_str(rest);
				return output;
			};
			output.push_str(&rest[..start]);
			let opener = rest.as_bytes()[start];
			let closer = if opener == b'[' { ']' } else { '$' };
			let Some(length) = rest[start + 1..].find(closer) else {
				output.push_str(&rest[start..]);
				return output;
			};
			let inner = &rest[start + 1..start + 1 + length];
			let replaced = if opener == b'[' {
				self.command(inner, rebinding)
			} else {
				self.reference(inner, rebinding)
			};
			output.push(opener as char);
			output.push_str(&replaced);
			output.push(closer);
			rest = &rest[start + 1 + length + 1..];
		}
	}

	/// `Root.GetName`, `GetCustomText`, `Root.Monarch.GetName`.
	fn command(&mut self, inner: &str, rebinding: &dyn Rebinding) -> String {
		inner
			.split('.')
			.enumerate()
			.map(|(index, part)| {
				if index == 0
					&& let Some(head) = rebinding.head(part)
				{
					return head;
				}
				self.command_name(part, rebinding)
					.unwrap_or_else(|| part.to_owned())
			})
			.collect::<Vec<_>>()
			.join(".")
	}

	/// `KEY` or `KEY|Y` inside `$…$`; game values such as `$VALUE$` are not
	/// localisation keys and stay.
	fn reference(&mut self, inner: &str, rebinding: &dyn Rebinding) -> String {
		let (name, format) = match inner.split_once('|') {
			Some((name, format)) => (name, Some(format)),
			None => (inner, None),
		};
		if !self.localisation.defines(name) && !self.overrides.has(name) {
			return inner.to_owned();
		}
		let derived = self.derive(name, rebinding, false);
		match format {
			Some(format) => format!("{derived}|{format}"),
			None => derived,
		}
	}

	/// The derived name of a `defined_text` that needs rebinding.
	fn command_name(&mut self, name: &str, rebinding: &dyn Rebinding) -> Option<String> {
		let memo = (rebinding.tag().to_owned(), name.to_owned());
		if let Some(done) = self.commands.get(&memo) {
			return done.clone();
		}
		let definitions = self.custom.definitions.get(name)?.clone();
		self.commands.insert(memo.clone(), None);
		let [definition] = definitions.as_slice() else {
			self.output.problems.push(format!(
				"customizable localisation `{name}` has {} definitions",
				definitions.len()
			));
			return None;
		};
		let derived = self.name(rebinding.tag(), name);
		let mut changed = false;
		let mut items = Vec::new();
		for statement in definition {
			match statement {
				AstStatement::Assignment {
					key,
					key_span,
					value: AstValue::Block {
						items: text,
						span: block_span,
					},
					span,
				} if key == "text" => {
					let mut rebound = Vec::new();
					for entry in text {
						rebound.push(match entry {
							AstStatement::Assignment {
								key,
								key_span,
								value:
									AstValue::Scalar {
										value,
										span: value_span,
									},
								span,
							} if key == "localisation_key" => {
								let original = value.as_text();
								let new = self.derive(&original, rebinding, false);
								changed |= new != original;
								AstStatement::Assignment {
									key: key.clone(),
									key_span: key_span.clone(),
									value: AstValue::Scalar {
										value: ScalarValue::Identifier(new),
										span: value_span.clone(),
									},
									span: span.clone(),
								}
							}
							AstStatement::Assignment {
								key,
								key_span,
								value: AstValue::Block { items, span: inner },
								span,
							} if key == "trigger" => {
								let new = rebinding.triggers(items);
								changed |= new != *items;
								AstStatement::Assignment {
									key: key.clone(),
									key_span: key_span.clone(),
									value: AstValue::Block {
										items: new,
										span: inner.clone(),
									},
									span: span.clone(),
								}
							}
							other => other.clone(),
						});
					}
					items.push(AstStatement::Assignment {
						key: key.clone(),
						key_span: key_span.clone(),
						value: AstValue::Block {
							items: rebound,
							span: block_span.clone(),
						},
						span: span.clone(),
					});
				}
				AstStatement::Assignment {
					key,
					key_span,
					value: AstValue::Scalar {
						span: value_span, ..
					},
					span,
				} if key == "name" => items.push(AstStatement::Assignment {
					key: key.clone(),
					key_span: key_span.clone(),
					value: AstValue::Scalar {
						value: ScalarValue::Identifier(derived.clone()),
						span: value_span.clone(),
					},
					span: span.clone(),
				}),
				other => items.push(other.clone()),
			}
		}
		if !changed {
			return None;
		}
		let span = match &definition.first() {
			Some(AstStatement::Assignment { span, .. }) => span.clone(),
			_ => return None,
		};
		self.output.commands.push(AstStatement::Assignment {
			key: "defined_text".into(),
			key_span: span.clone(),
			value: AstValue::Block {
				items,
				span: span.clone(),
			},
			span,
		});
		self.commands.insert(memo, Some(derived.clone()));
		Some(derived)
	}
}

fn scalar(items: &[AstStatement], key: &str) -> Option<String> {
	items.iter().find_map(|statement| match statement {
		AstStatement::Assignment {
			key: found,
			value: AstValue::Scalar { value, .. },
			..
		} if found == key => Some(value.as_text()),
		_ => None,
	})
}

#[cfg(test)]
mod tests {

	use super::*;
	use crate::game::eu4::script::parser::parse_clausewitz_content;

	/// `Root` becomes the event target `province`, `From` becomes `Root`, and
	/// command triggers have `ROOT` replaced the same way.
	struct ToProvince(&'static str);

	impl Rebinding for ToProvince {
		fn head(&self, head: &str) -> Option<String> {
			match head.to_ascii_uppercase().as_str() {
				"ROOT" => Some(self.0.to_owned()),
				"FROM" => Some("Root".to_owned()),
				_ => None,
			}
		}
		fn triggers(&self, statements: &[AstStatement]) -> Vec<AstStatement> {
			statements
				.iter()
				.map(|statement| match statement {
					AstStatement::Assignment {
						key,
						key_span,
						value,
						span,
					} if key == "ROOT" => AstStatement::Assignment {
						key: format!("event_target:{}", self.0),
						key_span: key_span.clone(),
						value: value.clone(),
						span: span.clone(),
					},
					other => other.clone(),
				})
				.collect()
		}
		fn tag(&self) -> &str {
			self.0
		}
	}

	fn localisation() -> EffectiveLocalisation {
		let mut texts = EffectiveLocalisation::default();
		// Higher precedence first: a translation mod overrides the source.
		texts.add_file(
			"\u{feff}l_english:\n tip:0 \"[Root.GetName] for [From.GetName]: $plain$ [Root.GetHolder]\"\n"
				.as_bytes(),
		);
		texts.add_file(
			"\u{feff}l_english:\n tip:0 \"overridden source text\"\n plain:0 \"No scope here\"\n holder_yes:0 \"held by [Root.Owner.GetName]\"\n holder_no:0 \"unheld\"\n same:0 \"[This.GetName]\"\nl_german:\n tip:0 \"[Root.GetName] fuer [From.GetName]\"\n plain:0 \"Nichts\"\n"
				.as_bytes(),
		);
		texts
	}

	fn commands() -> CustomLocalisation {
		CustomLocalisation::new(&[parse_clausewitz_content(
			&crate::model::GamePathBuf::parse("customizable_localization/x.txt").expect("test game path"),
			"defined_text = { name = GetHolder text = { localisation_key = holder_yes trigger = { ROOT = { has_owner = yes } } } text = { localisation_key = holder_no } }\ndefined_text = { name = GetPlain text = { localisation_key = plain } }",
		)
		.ast])
	}

	#[test]
	fn rebinds_keys_references_and_commands_that_name_root_or_from() {
		let texts = localisation();
		let custom = commands();
		let overrides = TextOverrides::default();
		let mut derivation =
			Derivation::new("foch", &["english", "german"], &texts, &custom, &overrides);
		let key = derivation.key("tip", &ToProvince("slot_1"));
		assert_eq!(key, "foch_slot_1_tip");
		// Text without scope references keeps its original key.
		assert_eq!(derivation.key("same", &ToProvince("slot_1")), "same");
		assert_eq!(derivation.key("plain", &ToProvince("slot_1")), "plain");
		let output = derivation.finish();
		assert_eq!(
			output.entries["english"],
			[
				(
					"foch_slot_1_holder_yes".to_owned(),
					"held by [slot_1.Owner.GetName]".to_owned()
				),
				(
					"foch_slot_1_tip".to_owned(),
					"[slot_1.GetName] for [Root.GetName]: $plain$ [slot_1.foch_slot_1_GetHolder]"
						.to_owned()
				),
			]
		);
		assert_eq!(
			output.entries["german"],
			[(
				"foch_slot_1_tip".to_owned(),
				"[slot_1.GetName] fuer [Root.GetName]".to_owned()
			)]
		);
		assert_eq!(
			output.problems,
			["localisation key `holder_yes` has no german text"]
		);
		assert!(output.originals.contains("plain") && output.originals.contains("same"));
		// The command is copied under a new name with its trigger and key rebound.
		let [command] = output.commands.as_slice() else {
			panic!("{:?}", output.commands)
		};
		let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = command
		else {
			panic!()
		};
		assert_eq!(
			scalar(items, "name").as_deref(),
			Some("foch_slot_1_GetHolder")
		);
		let text = crate::game::eu4::script::emit::emit_clausewitz_statements(items).unwrap();
		assert!(text.contains("event_target:slot_1 = {"), "{text}");
		assert!(
			text.contains("localisation_key = foch_slot_1_holder_yes"),
			"{text}"
		);
		assert!(text.contains("localisation_key = holder_no"), "{text}");
	}

	#[test]
	fn parameterized_keys_derive_every_instance_under_one_pattern() {
		let texts = localisation();
		let custom = commands();
		let overrides = TextOverrides::default();
		let mut derivation = Derivation::new("foch", &["english"], &texts, &custom, &overrides);
		let instances: [&dyn Rebinding; 2] = [&ToProvince("slot_1"), &ToProvince("target")];
		assert_eq!(
			derivation.parameterized_key("tip", &instances, "$target$"),
			"foch_$target$_tip"
		);
		assert_eq!(
			derivation.parameterized_key("plain", &instances, "$target$"),
			"plain"
		);
		let keys = derivation.finish().entries["english"]
			.iter()
			.map(|(key, _)| key.clone())
			.collect::<Vec<_>>();
		assert!(keys.contains(&"foch_slot_1_tip".to_owned()));
		assert!(keys.contains(&"foch_target_tip".to_owned()));
	}

	#[test]
	fn overrides_replace_source_text_before_rebinding() {
		let texts = localisation();
		let custom = commands();
		let mut overrides = TextOverrides::default();
		overrides.insert(None, "plain", "Now about [Root.GetName]");
		overrides.insert(Some("german"), "plain", "Jetzt [Root.GetName]");
		overrides.insert(None, "missing", "Only here");
		let mut derivation =
			Derivation::new("foch", &["english", "german"], &texts, &custom, &overrides);
		assert_eq!(derivation.key("plain", &ToProvince("t")), "foch_t_plain");
		// An override also defines a key the playset lacks.
		assert_eq!(
			derivation.key("missing", &ToProvince("t")),
			"foch_t_missing"
		);
		let output = derivation.finish();
		assert!(
			output.entries["english"]
				.contains(&("foch_t_plain".into(), "Now about [t.GetName]".into()))
		);
		assert!(
			output.entries["german"].contains(&("foch_t_plain".into(), "Jetzt [t.GetName]".into()))
		);
		assert!(output.entries["german"].contains(&("foch_t_missing".into(), "Only here".into())));
		assert!(output.problems.is_empty(), "{:?}", output.problems);
	}

	#[test]
	fn undefined_keys_are_reported_and_kept() {
		let texts = EffectiveLocalisation::default();
		let custom = CustomLocalisation::default();
		let overrides = TextOverrides::default();
		let mut derivation = Derivation::new("foch", &["english"], &texts, &custom, &overrides);
		assert_eq!(derivation.key("nowhere", &ToProvince("t")), "nowhere");
		assert_eq!(
			derivation.finish().problems,
			["localisation key `nowhere` is not defined"]
		);
	}
}
