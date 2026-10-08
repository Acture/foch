//! What each annotation accepts, as plain data shared by validation,
//! completion, hover and command-line help.

use crate::value::ValueType;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ParamSchema {
	pub name: &'static str,
	pub value_type: ValueType,
	pub required: bool,
	pub description: &'static str,
	pub example: &'static str,
}

/// Unnamed arguments, such as the labels of `#mark(war, slow)`.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct PositionalSchema {
	pub description: &'static str,
	pub example: &'static str,
}

/// The definition an annotation may be attached to.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct TargetRule {
	/// Top-level key of the definition, such as `country_event`.
	pub key: &'static str,
	/// Fields the definition must set to `yes`.
	pub required_yes: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnnotationKind {
	/// Attaches to the next top-level definition.
	Primary,
	/// Applies to the next primary annotation named `of`, like a Rust
	/// attribute applies to the next item.
	Modifier { of: &'static str },
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct AnnotationSchema {
	pub name: &'static str,
	pub kind: AnnotationKind,
	pub description: &'static str,
	pub params: &'static [ParamSchema],
	pub positional: Option<PositionalSchema>,
	pub target: Option<TargetRule>,
}

impl AnnotationSchema {
	pub fn param(&self, name: &str) -> Option<&'static ParamSchema> {
		self.params.iter().find(|param| param.name == name)
	}
}

/// The annotations a consumer understands.
#[derive(Clone, Copy, Debug)]
pub struct Registry {
	pub schemas: &'static [AnnotationSchema],
}

impl Registry {
	pub fn get(&self, name: &str) -> Option<&'static AnnotationSchema> {
		self.schemas.iter().find(|schema| schema.name == name)
	}
}
