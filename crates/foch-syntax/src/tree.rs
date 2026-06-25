use std::borrow::Cow;
use std::sync::Arc;

use tree_sitter::{Node, Parser, Tree};

use crate::error::{ParseError, ProjectionError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteSpan {
	pub start: usize,
	pub end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommentKind {
	Line,
	DocAttribute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CwtMarkerKind {
	Type,
	Subtype,
	AliasName,
	AliasMatchLeft,
	Enum,
	Value,
	ValueSet,
	Scalar,
	Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParadoxScalar<'source> {
	Identifier(&'source str),
	String(&'source str),
	Number(&'source str),
	Bool(bool),
	Variable(&'source str),
	Template(&'source str),
}

impl<'source> ParadoxScalar<'source> {
	pub fn as_text(&self) -> Cow<'source, str> {
		match self {
			Self::Identifier(value)
			| Self::String(value)
			| Self::Number(value)
			| Self::Variable(value)
			| Self::Template(value) => Cow::Borrowed(value),
			Self::Bool(true) => Cow::Borrowed("yes"),
			Self::Bool(false) => Cow::Borrowed("no"),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParadoxNode<'source> {
	Assignment {
		key: ParadoxScalar<'source>,
		key_span: ByteSpan,
		value: Box<ParadoxNode<'source>>,
		span: ByteSpan,
	},
	Block {
		items: Vec<ParadoxNode<'source>>,
		span: ByteSpan,
	},
	Array {
		items: Vec<ParadoxNode<'source>>,
		span: ByteSpan,
	},
	Scalar(ParadoxScalar<'source>),
	Item {
		value: Box<ParadoxNode<'source>>,
		span: ByteSpan,
	},
	Comment {
		text: &'source str,
		kind: CommentKind,
		span: ByteSpan,
	},
	Condition {
		keyword: &'source str,
		body: Box<ParadoxNode<'source>>,
		span: ByteSpan,
	},
	Logical {
		keyword: &'source str,
		body: Box<ParadoxNode<'source>>,
		span: ByteSpan,
	},
	Scope {
		keyword: &'source str,
		body: Box<ParadoxNode<'source>>,
		span: ByteSpan,
	},
	MacroMap {
		key: ParadoxScalar<'source>,
		items: Vec<ParadoxNode<'source>>,
		span: ByteSpan,
	},
	CwtMarker {
		kind: CwtMarkerKind,
		payload: &'source str,
		span: ByteSpan,
	},
}

impl<'source> ParadoxNode<'source> {
	pub fn span(&self) -> Option<ByteSpan> {
		match self {
			Self::Assignment { span, .. }
			| Self::Block { span, .. }
			| Self::Array { span, .. }
			| Self::Item { span, .. }
			| Self::Comment { span, .. }
			| Self::Condition { span, .. }
			| Self::Logical { span, .. }
			| Self::Scope { span, .. }
			| Self::MacroMap { span, .. }
			| Self::CwtMarker { span, .. } => Some(*span),
			Self::Scalar(_) => None,
		}
	}
}

pub struct ParadoxTree {
	pub source: Arc<str>,
	pub tree: Tree,
}

impl ParadoxTree {
	pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
		let source = Arc::<str>::from(foch_core::decode_paradox_bytes(bytes).into_owned());
		let mut parser = Parser::new();
		parser
			.set_language(&tree_sitter_paradox::language())
			.map_err(|error| ParseError::Language(error.to_string()))?;
		let tree = parser
			.parse(source.as_ref(), None)
			.ok_or(ParseError::ParseReturnedNone)?;
		Ok(Self { source, tree })
	}

	pub fn nodes(&self) -> Result<Vec<ParadoxNode<'_>>, ProjectionError> {
		project_root(self.tree.root_node(), self.source.as_ref())
	}

	pub fn has_error(&self) -> bool {
		self.tree.root_node().has_error()
	}
}

fn project_root<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<Vec<ParadoxNode<'source>>, ProjectionError> {
	let mut cursor = node.walk();
	let mut items = Vec::new();
	for child in node.children(&mut cursor) {
		if !child.is_named() {
			continue;
		}
		match child.kind() {
			"statement" => items.push(project_statement(child, source)?),
			"comment" | "doc_attribute_comment" => items.push(project_comment(child, source)),
			_ => {
				return Err(ProjectionError::UnexpectedNode {
					kind: child.kind().to_string(),
					span: span_of(child),
				});
			}
		}
	}
	Ok(items)
}

fn project_statement<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<ParadoxNode<'source>, ProjectionError> {
	let child = first_named_child(node).ok_or_else(|| ProjectionError::UnexpectedNode {
		kind: node.kind().to_string(),
		span: span_of(node),
	})?;
	match child.kind() {
		"assignment" => project_assignment(child, source),
		"condition_statement" => project_condition_like(child, source),
		"logical_statement" => project_logical(child, source),
		"scope_statement" => project_scope(child, source),
		"macro_map" => project_macro_map(child, source),
		_ => Ok(ParadoxNode::Item {
			value: Box::new(project_value_node(child, source)?),
			span: span_of(node),
		}),
	}
}

fn project_assignment<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<ParadoxNode<'source>, ProjectionError> {
	let key_node = required_field(node, "assignment", "key")?;
	let value_node = required_field(node, "assignment", "value")?;
	Ok(ParadoxNode::Assignment {
		key: scalar_from_node(key_node, source),
		key_span: span_of(key_node),
		value: Box::new(project_value_node(value_node, source)?),
		span: span_of(node),
	})
}

fn project_condition_like<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<ParadoxNode<'source>, ProjectionError> {
	let keyword = text_of(
		required_field(node, "condition_statement", "keyword")?,
		source,
	);
	let body = Box::new(project_map(
		required_field(node, "condition_statement", "body")?,
		source,
	)?);
	Ok(ParadoxNode::Condition {
		keyword,
		body,
		span: span_of(node),
	})
}

fn project_logical<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<ParadoxNode<'source>, ProjectionError> {
	let keyword = text_of(
		required_field(node, "logical_statement", "keyword")?,
		source,
	);
	let body = Box::new(project_map(
		required_field(node, "logical_statement", "body")?,
		source,
	)?);
	Ok(ParadoxNode::Logical {
		keyword,
		body,
		span: span_of(node),
	})
}

fn project_scope<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<ParadoxNode<'source>, ProjectionError> {
	let keyword = text_of(required_field(node, "scope_statement", "keyword")?, source);
	let body = Box::new(project_map(
		required_field(node, "scope_statement", "body")?,
		source,
	)?);
	Ok(ParadoxNode::Scope {
		keyword,
		body,
		span: span_of(node),
	})
}

fn project_macro_map<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<ParadoxNode<'source>, ProjectionError> {
	let key = scalar_from_node(required_field(node, "macro_map", "key")?, source);
	let items = project_container_items(node, source)?;
	Ok(ParadoxNode::MacroMap {
		key,
		items,
		span: span_of(node),
	})
}

fn project_map<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<ParadoxNode<'source>, ProjectionError> {
	Ok(ParadoxNode::Block {
		items: project_container_items(node, source)?,
		span: span_of(node),
	})
}

fn project_array<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<ParadoxNode<'source>, ProjectionError> {
	let mut cursor = node.walk();
	let mut items = Vec::new();
	for child in node.children(&mut cursor) {
		if !child.is_named() {
			continue;
		}
		match child.kind() {
			"comment" | "doc_attribute_comment" => items.push(project_comment(child, source)),
			_ => items.push(ParadoxNode::Item {
				value: Box::new(project_value_node(child, source)?),
				span: span_of(child),
			}),
		}
	}
	Ok(ParadoxNode::Array {
		items,
		span: span_of(node),
	})
}

fn project_container_items<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<Vec<ParadoxNode<'source>>, ProjectionError> {
	let mut cursor = node.walk();
	let mut items = Vec::new();
	for child in node.children(&mut cursor) {
		if !child.is_named() {
			continue;
		}
		match child.kind() {
			"statement" => items.push(project_statement(child, source)?),
			"comment" | "doc_attribute_comment" => items.push(project_comment(child, source)),
			_ => {
				return Err(ProjectionError::UnexpectedNode {
					kind: child.kind().to_string(),
					span: span_of(child),
				});
			}
		}
	}
	Ok(items)
}

fn project_value_node<'source>(
	node: Node<'_>,
	source: &'source str,
) -> Result<ParadoxNode<'source>, ProjectionError> {
	match node.kind() {
		"simple_value" => {
			let child = first_named_child(node).ok_or_else(|| ProjectionError::UnexpectedNode {
				kind: node.kind().to_string(),
				span: span_of(node),
			})?;
			project_value_node(child, source)
		}
		"identifier"
		| "string"
		| "number"
		| "boolean"
		| "placeholder_value"
		| "variable"
		| "variable_embedded_identifier"
		| "template_string"
		| "scalar_keyword"
		| "cwt_value_ref" => Ok(ParadoxNode::Scalar(scalar_from_node(node, source))),
		"cwt_type_marker" => Ok(ParadoxNode::CwtMarker {
			kind: classify_cwt_marker(text_of(node, source)),
			payload: text_of(node, source),
			span: span_of(node),
		}),
		"map" => project_map(node, source),
		"array" => project_array(node, source),
		"comment" | "doc_attribute_comment" => Ok(project_comment(node, source)),
		_ => Err(ProjectionError::UnexpectedNode {
			kind: node.kind().to_string(),
			span: span_of(node),
		}),
	}
}

fn project_comment<'source>(node: Node<'_>, source: &'source str) -> ParadoxNode<'source> {
	let raw = text_of(node, source);
	let kind = if node.kind() == "doc_attribute_comment" {
		CommentKind::DocAttribute
	} else {
		CommentKind::Line
	};
	ParadoxNode::Comment {
		text: trim_comment_text(raw),
		kind,
		span: span_of(node),
	}
}

fn scalar_from_node<'source>(node: Node<'_>, source: &'source str) -> ParadoxScalar<'source> {
	let text = text_of(node, source);
	match node.kind() {
		"identifier" | "placeholder_value" | "scalar_keyword" | "cwt_type_marker"
		| "cwt_value_ref" => ParadoxScalar::Identifier(text),
		"string" => ParadoxScalar::String(strip_enclosure(text, 1, 1)),
		"number" => ParadoxScalar::Number(text),
		"boolean" => ParadoxScalar::Bool(matches!(text, "yes" | "true")),
		"variable" => ParadoxScalar::Variable(strip_enclosure(text, 1, 1)),
		"variable_embedded_identifier" => ParadoxScalar::Template(text),
		"template_string" => ParadoxScalar::Template(strip_enclosure(text, 1, 1)),
		_ => ParadoxScalar::Identifier(text),
	}
}

fn classify_cwt_marker(text: &str) -> CwtMarkerKind {
	let head = text.split_once('[').map(|(head, _)| head).unwrap_or(text);
	match head {
		"type" => CwtMarkerKind::Type,
		"subtype" => CwtMarkerKind::Subtype,
		"alias_name" => CwtMarkerKind::AliasName,
		"alias_match_left" => CwtMarkerKind::AliasMatchLeft,
		"enum" => CwtMarkerKind::Enum,
		"value" => CwtMarkerKind::Value,
		"value_set" => CwtMarkerKind::ValueSet,
		"scalar" | "bool" | "int" | "float" | "colour" | "alias_keys_field" => {
			CwtMarkerKind::Scalar
		}
		_ => CwtMarkerKind::Other,
	}
}

fn trim_comment_text(text: &str) -> &str {
	if let Some(rest) = text.strip_prefix("--[[")
		&& let Some(rest) = rest.strip_suffix("]]")
	{
		return rest.trim();
	}
	if let Some(rest) = text.strip_prefix("--") {
		return rest.trim();
	}
	text.trim_start_matches('#').trim()
}

fn strip_enclosure(text: &str, prefix: usize, suffix: usize) -> &str {
	if text.len() >= prefix + suffix {
		&text[prefix..text.len() - suffix]
	} else {
		text
	}
}

fn first_named_child(node: Node<'_>) -> Option<Node<'_>> {
	let mut cursor = node.walk();
	node.children(&mut cursor).find(Node::is_named)
}

fn required_field<'tree>(
	node: Node<'tree>,
	node_kind: &'static str,
	field: &'static str,
) -> Result<Node<'tree>, ProjectionError> {
	node.child_by_field_name(field)
		.ok_or(ProjectionError::MissingField { node_kind, field })
}

fn text_of<'source>(node: Node<'_>, source: &'source str) -> &'source str {
	&source[node.start_byte()..node.end_byte()]
}

fn span_of(node: Node<'_>) -> ByteSpan {
	ByteSpan {
		start: node.start_byte(),
		end: node.end_byte(),
	}
}
