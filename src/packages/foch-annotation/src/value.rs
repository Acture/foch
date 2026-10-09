//! Typed annotation values.
//!
//! Newtypes such as [`GameDate`] and [`Tag`] can only be built through their
//! parsers, so consumers never re-validate them.

use crate::source::{RelPath, SourceSpan};
use foch::game::eu4::script::parser::{AstStatement, AstValue, ScalarValue};
use foch::model::GamePath;
use jomini::common::{Date, PdsDate};
use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! string_newtype {
	($name:ident) => {
		impl $name {
			pub fn as_str(&self) -> &str {
				&self.0
			}
		}

		impl TryFrom<String> for $name {
			type Error = String;

			fn try_from(value: String) -> Result<Self, String> {
				Self::parse(&value)
			}
		}

		impl From<$name> for String {
			fn from(value: $name) -> Self {
				value.0
			}
		}

		impl fmt::Display for $name {
			fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
				formatter.write_str(&self.0)
			}
		}
	};
}

/// A full EU4 calendar date (no leap years), formatted `YYYY.M.D`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GameDate(Date);

impl GameDate {
	pub fn parse(value: &str) -> Result<Self, String> {
		let parts: Vec<_> = value.split('.').collect();
		if parts.len() != 3
			|| parts
				.iter()
				.any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
		{
			return Err("date must be a full date in YYYY.M.D format".into());
		}
		let date = Date::parse(value).map_err(|_| "invalid EU4 calendar date")?;
		if !(1..=9999).contains(&date.year()) {
			return Err("date year must be in 1..=9999".into());
		}
		Ok(Self(date))
	}

	pub fn add_days(self, days: u32) -> Result<Self, String> {
		let last = Date::from_ymd(9999, 12, 31);
		if i64::from(days) > i64::from(self.0.days_until(&last)) {
			return Err("date would exceed year 9999".into());
		}
		Ok(Self(self.0.add_days(days as i32)))
	}
}

impl TryFrom<String> for GameDate {
	type Error = String;

	fn try_from(value: String) -> Result<Self, String> {
		Self::parse(&value)
	}
}

impl From<GameDate> for String {
	fn from(value: GameDate) -> Self {
		value.to_string()
	}
}

impl fmt::Display for GameDate {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{}", self.0.game_fmt())
	}
}

/// Three uppercase ASCII letters/digits starting with a letter, such as `SWE`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Tag(String);

impl Tag {
	pub fn parse(value: &str) -> Result<Self, String> {
		if value.len() == 3
			&& value.as_bytes()[0].is_ascii_uppercase()
			&& value
				.bytes()
				.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
		{
			Ok(Self(value.into()))
		} else {
			Err("tag must be three uppercase ASCII letters/digits, starting with a letter".into())
		}
	}
}
string_newtype!(Tag);

/// `123` or `namespace.123`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EventId(String);

impl EventId {
	pub fn parse(value: &str) -> Result<Self, String> {
		let numeric = |part: &str| {
			!part.is_empty()
				&& part.bytes().all(|byte| byte.is_ascii_digit())
				&& part.parse::<u32>().is_ok()
		};
		let valid = value.len() <= 128
			&& (numeric(value)
				|| value
					.split_once('.')
					.is_some_and(|(namespace, number)| identifier(namespace) && numeric(number)));
		if valid {
			Ok(Self(value.into()))
		} else {
			Err(format!(
				"invalid event ID {value:?}; expected numeric or namespace.numeric"
			))
		}
	}
}
string_newtype!(EventId);

/// An ASCII identifier: a letter or `_`, then letters, digits or `_`.
pub fn identifier(value: &str) -> bool {
	let mut bytes = value.bytes();
	bytes
		.next()
		.is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
		&& bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Native script, without its outer braces.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeBlock {
	pub text: String,
	pub span: SourceSpan,
}

/// One top-level condition of a condition block. Top-level conditions are
/// conjunctive and side-effect free, so evaluating them separately at the
/// same moment gives the same verdict as the whole block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Clause {
	pub text: String,
	pub span: SourceSpan,
}

/// The type of an annotation parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ValueType {
	Date,
	Tag,
	EventId,
	/// Any scalar; quote text containing spaces.
	Text,
	Integer {
		min: u32,
		max: u32,
	},
	Bool,
	Choice {
		options: &'static [&'static str],
	},
	/// A block of identifiers, such as `{ a b }`.
	Identifiers,
	/// A block of native effects.
	Effects,
	/// A block of native conditions, kept clause by clause.
	Conditions,
	/// A block of scalar values each of one element type, such as the
	/// `{ SWE DAN }` of a parametrized `tag`. Only scalar element types are
	/// supported.
	List {
		element: &'static ValueType,
	},
}

impl ValueType {
	/// Short human description, as shown in completions and hovers.
	pub fn describe(&self) -> String {
		match self {
			Self::Date => "date".into(),
			Self::Tag => "country tag".into(),
			Self::EventId => "event ID".into(),
			Self::Text => "string".into(),
			Self::Integer { min, max } => format!("integer ({min}..={max})"),
			Self::Bool => "yes | no".into(),
			Self::Choice { options } => options.join(" | "),
			Self::Identifiers => "names".into(),
			Self::Effects => "effect block".into(),
			Self::Conditions => "trigger block".into(),
			Self::List { element } => format!("{{ {} ... }}", element.describe()),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
	Date(GameDate),
	Tag(Tag),
	EventId(EventId),
	Text(String),
	Integer(u32),
	Bool(bool),
	Choice(String),
	Identifiers(Vec<String>),
	Effects(NativeBlock),
	Conditions(Vec<Clause>),
	List(Vec<Value>),
}

/// Parses a script value as `value_type`. `source` must have the same byte
/// offsets as the file named by `path`, so spans point at the original text.
pub fn parse_value(
	path: &RelPath,
	source: &str,
	value: &AstValue,
	value_type: ValueType,
) -> Result<Value, String> {
	Ok(match value_type {
		ValueType::Date => Value::Date(GameDate::parse(&scalar(value)?)?),
		ValueType::Tag => Value::Tag(Tag::parse(&scalar(value)?)?),
		ValueType::EventId => Value::EventId(EventId::parse(&scalar(value)?)?),
		ValueType::Text => Value::Text(scalar(value)?),
		ValueType::Integer { min, max } => Value::Integer(
			scalar(value)?
				.parse::<u32>()
				.ok()
				.filter(|number| (min..=max).contains(number))
				.ok_or_else(|| format!("expected an integer in {min}..={max}"))?,
		),
		ValueType::Bool => Value::Bool(boolean(value)?),
		ValueType::Choice { options } => {
			let text = scalar(value)?;
			if !options.contains(&text.as_str()) {
				return Err(format!("expected {}, not {text:?}", options.join(" or ")));
			}
			Value::Choice(text)
		}
		ValueType::Identifiers => Value::Identifiers(identifiers(value)?),
		ValueType::Effects => Value::Effects(native_block(path, source, value)?),
		ValueType::Conditions => Value::Conditions(clauses(path, source, value)?),
		ValueType::List { element } => Value::List(list(path, source, value, element)?),
	})
}

/// A block of scalar items, each parsed as `element`. Items must be bare
/// scalars, such as the `{ SWE DAN }` of a parametrized `tag`.
fn list(
	path: &RelPath,
	source: &str,
	value: &AstValue,
	element: &ValueType,
) -> Result<Vec<Value>, String> {
	let AstValue::Block { items, .. } = value else {
		return Err("expected a block of values, such as { a b }".into());
	};
	let mut values = Vec::new();
	for item in items {
		match item {
			AstStatement::Comment { .. } => {}
			AstStatement::Item { value, .. } => {
				values.push(parse_value(path, source, value, *element)?)
			}
			AstStatement::Assignment { .. } => {
				return Err("expected bare values, not assignments".into());
			}
		}
	}
	if values.is_empty() {
		return Err("list must contain at least one value".into());
	}
	Ok(values)
}

pub fn scalar(value: &AstValue) -> Result<String, String> {
	match value {
		AstValue::Scalar {
			value:
				ScalarValue::Identifier(value) | ScalarValue::String(value) | ScalarValue::Number(value),
			..
		} => Ok(value.clone()),
		AstValue::Scalar {
			value: ScalarValue::Bool(value),
			..
		} => Ok(if *value { "yes" } else { "no" }.into()),
		AstValue::Block { .. } => Err("expected a scalar value".into()),
	}
}

pub fn boolean(value: &AstValue) -> Result<bool, String> {
	match value {
		AstValue::Scalar {
			value: ScalarValue::Bool(value),
			..
		} => Ok(*value),
		_ => Err("expected yes or no".into()),
	}
}

fn identifiers(value: &AstValue) -> Result<Vec<String>, String> {
	let AstValue::Block { items, .. } = value else {
		return Err("expected a block of names, such as { a b }".into());
	};
	let mut names = Vec::new();
	for item in items {
		match item {
			AstStatement::Comment { .. } => {}
			AstStatement::Item { value, .. } => {
				let name = scalar(value)?;
				if !identifier(&name) {
					return Err(format!("invalid name {name:?}; expected an identifier"));
				}
				if names.contains(&name) {
					return Err(format!("{name} is listed twice"));
				}
				names.push(name);
			}
			AstStatement::Assignment { .. } => {
				return Err("expected names, not assignments".into());
			}
		}
	}
	Ok(names)
}

fn native_block(path: &RelPath, source: &str, value: &AstValue) -> Result<NativeBlock, String> {
	let AstValue::Block { items, span } = value else {
		return Err("expected a native EU4 block".into());
	};
	if !items
		.iter()
		.any(|item| matches!(item, AstStatement::Assignment { .. }))
	{
		return Err("block must contain native EU4 statements".into());
	}
	Ok(NativeBlock {
		text: source[span.start.offset + 1..span.end.offset - 1]
			.trim()
			.into(),
		span: SourceSpan::from_parser(path, span),
	})
}

fn clauses(path: &RelPath, source: &str, value: &AstValue) -> Result<Vec<Clause>, String> {
	let AstValue::Block { items, .. } = value else {
		return Err("expected a native EU4 block".into());
	};
	let mut clauses = Vec::new();
	for item in items {
		match item {
			AstStatement::Comment { .. } => {}
			AstStatement::Assignment { span, .. } => clauses.push(Clause {
				text: source[span.start.offset..span.end.offset].trim().into(),
				span: SourceSpan::from_parser(path, span),
			}),
			AstStatement::Item { .. } => {
				return Err("block must contain native key = value conditions".into());
			}
		}
	}
	if clauses.is_empty() {
		return Err("block must contain native EU4 conditions".into());
	}
	Ok(clauses)
}

/// A parser path label for internal parsing; inputs are literals or already
/// validated relative paths, so construction does not fail in practice.
pub(crate) fn game_path(text: &str) -> &GamePath {
	GamePath::new(text).expect("valid game path label")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn dates_use_the_nonleap_eu4_calendar() {
		let date = GameDate::parse("1444.2.28").unwrap();
		assert_eq!(date.add_days(1).unwrap().to_string(), "1444.3.1");
		assert!(GameDate::parse("1444.2.29").is_err());
		assert!(GameDate::parse("1444.11").is_err());
		assert!(GameDate::parse("0.1.1").is_err());
		assert!(GameDate::parse("9999.12.31").unwrap().add_days(1).is_err());
	}

	#[test]
	fn identifiers_reject_script_injection() {
		assert!(Tag::parse("SWE").is_ok());
		for bad in ["swe", "SW", "1SW", "SWE }"] {
			assert!(Tag::parse(bad).is_err(), "{bad:?}");
		}
		assert!(EventId::parse("reforms.1").is_ok());
		assert!(EventId::parse("7").is_ok());
		for bad in ["", "a.b", "a.1\nlog=x", "a/1", "1a.1"] {
			assert!(EventId::parse(bad).is_err(), "{bad:?}");
		}
	}
}
