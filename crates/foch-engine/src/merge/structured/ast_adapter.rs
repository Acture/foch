use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use foch_language::analyzer::parser::{
	AstFile, AstStatement, AstValue, ScalarValue, Span, SpanRange,
};
use foch_merge_kernel::{
	ChildOrder, NodeId, NormalizedNode, NormalizedTree, SemanticKey, TreeError, TreeNode,
};

use super::policy::ClausewitzTreePolicy;

const FILE_KIND: &str = "clausewitz.file";
const ASSIGNMENT_KIND_PREFIX: &str = "clausewitz.assignment:";
const ITEM_KIND: &str = "clausewitz.item";
const COMMENT_KIND: &str = "clausewitz.comment";
const BLOCK_KIND: &str = "clausewitz.block";
const IDENTIFIER_KIND: &str = "clausewitz.scalar.identifier";
const STRING_KIND: &str = "clausewitz.scalar.string";
const NUMBER_KIND: &str = "clausewitz.scalar.number";
const BOOL_KIND: &str = "clausewitz.scalar.bool";

#[derive(Debug)]
pub(crate) enum AstAdapterError {
	Kernel(TreeError),
	InvalidTree(String),
}

impl fmt::Display for AstAdapterError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Kernel(error) => write!(formatter, "normalized tree error: {error}"),
			Self::InvalidTree(message) => {
				write!(formatter, "invalid Clausewitz merge tree: {message}")
			}
		}
	}
}

impl Error for AstAdapterError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Kernel(error) => Some(error),
			Self::InvalidTree(_) => None,
		}
	}
}

impl From<TreeError> for AstAdapterError {
	fn from(error: TreeError) -> Self {
		Self::Kernel(error)
	}
}

pub(crate) fn normalize_ast(
	file: &AstFile,
	policy: &impl ClausewitzTreePolicy,
) -> Result<NormalizedTree, AstAdapterError> {
	let children = file
		.statements
		.iter()
		.map(|statement| normalize_statement(statement, policy))
		.collect();
	NormalizedTree::from_root(branch(FILE_KIND, None, None, ChildOrder::Ordered, children))
		.map_err(AstAdapterError::from)
}

pub(crate) fn denormalize_ast(
	path: PathBuf,
	tree: &NormalizedTree,
) -> Result<AstFile, AstAdapterError> {
	let root = tree.node(tree.root())?;
	if root.kind != FILE_KIND {
		return Err(AstAdapterError::InvalidTree(format!(
			"root kind is `{}`, expected `{FILE_KIND}`",
			root.kind
		)));
	}
	let statements = root
		.children
		.iter()
		.map(|child| denormalize_statement(tree, *child))
		.collect::<Result<Vec<_>, _>>()?;
	Ok(AstFile { path, statements })
}

fn normalize_statement(statement: &AstStatement, policy: &impl ClausewitzTreePolicy) -> TreeNode {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			let kind = format!("{ASSIGNMENT_KIND_PREFIX}{key}");
			branch(
				&kind,
				Some(key.clone()),
				policy.assignment_anchor(key, value),
				ChildOrder::Ordered,
				vec![normalize_value(value, Some(key), policy)],
			)
		}
		AstStatement::Item { value, .. } => branch(
			ITEM_KIND,
			None,
			None,
			ChildOrder::Ordered,
			vec![normalize_value(value, None, policy)],
		),
		AstStatement::Comment { text, .. } => leaf(COMMENT_KIND, text.clone()),
	}
}

fn normalize_value(
	value: &AstValue,
	assignment_key: Option<&str>,
	policy: &impl ClausewitzTreePolicy,
) -> TreeNode {
	match value {
		AstValue::Scalar { value, .. } => match value {
			ScalarValue::Identifier(value) => leaf(IDENTIFIER_KIND, value.clone()),
			ScalarValue::String(value) => leaf(STRING_KIND, value.clone()),
			ScalarValue::Number(value) => leaf(NUMBER_KIND, value.clone()),
			ScalarValue::Bool(value) => leaf(BOOL_KIND, value.to_string()),
		},
		AstValue::Block { items, .. } => branch(
			BLOCK_KIND,
			None,
			None,
			policy.block_child_order(assignment_key),
			items
				.iter()
				.map(|statement| normalize_statement(statement, policy))
				.collect(),
		),
	}
}

fn denormalize_statement(
	tree: &NormalizedTree,
	id: NodeId,
) -> Result<AstStatement, AstAdapterError> {
	let node = tree.node(id)?;
	if node.kind.starts_with(ASSIGNMENT_KIND_PREFIX) {
		let key = required_value(node, "assignment key")?.to_string();
		if node.kind != format!("{ASSIGNMENT_KIND_PREFIX}{key}") {
			return Err(AstAdapterError::InvalidTree(format!(
				"node {} assignment kind and key disagree",
				id.get()
			)));
		}
		let value = denormalize_only_value_child(tree, node)?;
		return Ok(AstStatement::Assignment {
			key,
			key_span: synthetic_span(),
			value,
			span: synthetic_span(),
		});
	}
	match node.kind.as_str() {
		ITEM_KIND => Ok(AstStatement::Item {
			value: denormalize_only_value_child(tree, node)?,
			span: synthetic_span(),
		}),
		COMMENT_KIND => {
			require_leaf(node)?;
			Ok(AstStatement::Comment {
				text: required_value(node, "comment text")?.to_string(),
				span: synthetic_span(),
			})
		}
		other => Err(AstAdapterError::InvalidTree(format!(
			"node {} has non-statement kind `{other}`",
			id.get()
		))),
	}
}

fn denormalize_only_value_child(
	tree: &NormalizedTree,
	node: &NormalizedNode,
) -> Result<AstValue, AstAdapterError> {
	let [child] = node.children.as_slice() else {
		return Err(AstAdapterError::InvalidTree(format!(
			"`{}` must contain exactly one value child",
			node.kind
		)));
	};
	denormalize_value(tree, *child)
}

fn denormalize_value(tree: &NormalizedTree, id: NodeId) -> Result<AstValue, AstAdapterError> {
	let node = tree.node(id)?;
	let scalar = match node.kind.as_str() {
		IDENTIFIER_KIND => Some(ScalarValue::Identifier(
			required_leaf_value(node, "identifier")?.to_string(),
		)),
		STRING_KIND => Some(ScalarValue::String(
			required_leaf_value(node, "string")?.to_string(),
		)),
		NUMBER_KIND => Some(ScalarValue::Number(
			required_leaf_value(node, "number")?.to_string(),
		)),
		BOOL_KIND => Some(ScalarValue::Bool(
			required_leaf_value(node, "boolean")?
				.parse::<bool>()
				.map_err(|_| {
					AstAdapterError::InvalidTree(format!(
						"node {} has an invalid boolean value",
						id.get()
					))
				})?,
		)),
		BLOCK_KIND => {
			let items = node
				.children
				.iter()
				.map(|child| denormalize_statement(tree, *child))
				.collect::<Result<Vec<_>, _>>()?;
			return Ok(AstValue::Block {
				items,
				span: synthetic_span(),
			});
		}
		other => {
			return Err(AstAdapterError::InvalidTree(format!(
				"node {} has non-value kind `{other}`",
				id.get()
			)));
		}
	};
	Ok(AstValue::Scalar {
		value: scalar.expect("scalar kinds construct a scalar value"),
		span: synthetic_span(),
	})
}

fn branch(
	kind: &str,
	value: Option<String>,
	anchor: Option<SemanticKey>,
	child_order: ChildOrder,
	children: Vec<TreeNode>,
) -> TreeNode {
	TreeNode {
		kind: kind.to_string(),
		value,
		anchor,
		signature: None,
		child_order,
		children,
	}
}

fn leaf(kind: &str, value: String) -> TreeNode {
	TreeNode::leaf(kind, value)
}

fn required_leaf_value<'a>(
	node: &'a NormalizedNode,
	description: &str,
) -> Result<&'a str, AstAdapterError> {
	require_leaf(node)?;
	required_value(node, description)
}

fn required_value<'a>(
	node: &'a NormalizedNode,
	description: &str,
) -> Result<&'a str, AstAdapterError> {
	node.value.as_deref().ok_or_else(|| {
		AstAdapterError::InvalidTree(format!("`{}` is missing {description}", node.kind))
	})
}

fn require_leaf(node: &NormalizedNode) -> Result<(), AstAdapterError> {
	if node.children.is_empty() {
		Ok(())
	} else {
		Err(AstAdapterError::InvalidTree(format!(
			"`{}` must not contain children",
			node.kind
		)))
	}
}

fn synthetic_span() -> SpanRange {
	let point = Span {
		line: 0,
		column: 0,
		offset: 0,
	};
	SpanRange {
		start: point.clone(),
		end: point,
	}
}
