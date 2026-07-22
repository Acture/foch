use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use foch_language::analyzer::parser::{
	AstFile, AstStatement, AstValue, ScalarValue, Span, SpanRange,
};
use foch_merge_kernel::{
	ChildCardinality, ChildOrder, NodeId, NormalizedNode, NormalizedTree, SemanticKey, TreeError,
	TreeNode,
};

use super::policy::ClausewitzTreePolicy;

const FILE_KIND: &str = "clausewitz.file";
const ASSIGNMENT_KIND_PREFIX: &str = "clausewitz.assignment:";
const ITEM_KIND: &str = "clausewitz.item";
pub(super) const COMMENT_KIND: &str = "clausewitz.comment";
const BLOCK_KIND_PREFIX: &str = "clausewitz.block";
const IDENTIFIER_KIND: &str = "clausewitz.scalar.identifier";
const STRING_KIND: &str = "clausewitz.scalar.string";
const NUMBER_KIND: &str = "clausewitz.scalar.number";
const BOOL_KIND: &str = "clausewitz.scalar.bool";

#[derive(Debug)]
pub enum AstAdapterError {
	Kernel(TreeError),
	InvalidTree(String),
	DuplicateControlFlowGuard(String),
	UnprovableControlFlow(String),
}

impl fmt::Display for AstAdapterError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Kernel(error) => write!(formatter, "normalized tree error: {error}"),
			Self::InvalidTree(message) => {
				write!(formatter, "invalid Clausewitz merge tree: {message}")
			}
			Self::DuplicateControlFlowGuard(guard) => {
				write!(
					formatter,
					"duplicate Clausewitz control-flow guard: {guard}"
				)
			}
			Self::UnprovableControlFlow(message) => {
				write!(
					formatter,
					"unprovable Clausewitz control-flow structure: {message}"
				)
			}
		}
	}
}

impl Error for AstAdapterError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Kernel(error) => Some(error),
			Self::InvalidTree(_)
			| Self::DuplicateControlFlowGuard(_)
			| Self::UnprovableControlFlow(_) => None,
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
	let children = normalize_statements(&file.statements, policy)?;
	NormalizedTree::from_root(branch(
		FILE_KIND,
		None,
		None,
		ChildOrder::Ordered,
		ChildCardinality::Many,
		children,
	))
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
	let statements = denormalize_statements(tree, &root.children)?;
	Ok(AstFile { path, statements })
}

fn normalize_statements(
	statements: &[AstStatement],
	policy: &impl ClausewitzTreePolicy,
) -> Result<Vec<TreeNode>, AstAdapterError> {
	let mut children = Vec::with_capacity(statements.len());
	let mut scalar_item_occurrences = BTreeMap::new();
	let mut numeric_item_position = 0;
	let mut index = 0;
	while index < statements.len() {
		if super::control_flow::starts_chain(&statements[index]) {
			let (chain, next) = super::control_flow::normalize_chain(statements, index, policy)?;
			children.push(chain);
			index = next;
		} else {
			if matches!(assignment_key(&statements[index]), Some("else_if" | "else")) {
				return Err(AstAdapterError::UnprovableControlFlow(
					"orphan `else_if` or `else` branch".to_string(),
				));
			}
			let item_anchor = scalar_item_anchor(
				&statements[index],
				&mut scalar_item_occurrences,
				&mut numeric_item_position,
			);
			children.push(normalize_statement_with_item_anchor(
				&statements[index],
				policy,
				item_anchor,
			)?);
			index += 1;
		}
	}
	Ok(children)
}

pub(super) fn assignment_key(statement: &AstStatement) -> Option<&str> {
	match statement {
		AstStatement::Assignment { key, .. } => Some(key),
		AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
	}
}

pub(super) fn normalize_statement(
	statement: &AstStatement,
	policy: &impl ClausewitzTreePolicy,
) -> Result<TreeNode, AstAdapterError> {
	normalize_statement_with_item_anchor(statement, policy, None)
}

fn normalize_statement_with_item_anchor(
	statement: &AstStatement,
	policy: &impl ClausewitzTreePolicy,
	item_anchor: Option<SemanticKey>,
) -> Result<TreeNode, AstAdapterError> {
	Ok(match statement {
		AstStatement::Assignment { key, value, .. } => {
			let kind = format!("{ASSIGNMENT_KIND_PREFIX}{key}");
			let mut node = branch(
				&kind,
				Some(key.clone()),
				policy.assignment_anchor(key, value),
				ChildOrder::Ordered,
				ChildCardinality::ExactlyOne,
				vec![normalize_value(value, Some(key), policy)?],
			);
			node.signature = policy.assignment_signature(key, value);
			node
		}
		AstStatement::Item { value, .. } => branch(
			ITEM_KIND,
			None,
			item_anchor,
			ChildOrder::Ordered,
			ChildCardinality::ExactlyOne,
			vec![normalize_value(value, None, policy)?],
		),
		AstStatement::Comment { text, .. } => leaf(COMMENT_KIND, text.clone()),
	})
}

fn scalar_item_anchor(
	statement: &AstStatement,
	occurrences: &mut BTreeMap<(String, String), usize>,
	numeric_position: &mut usize,
) -> Option<SemanticKey> {
	let AstStatement::Item {
		value: AstValue::Scalar { value, .. },
		..
	} = statement
	else {
		return None;
	};
	if matches!(value, ScalarValue::Number(_)) {
		let position = *numeric_position;
		*numeric_position += 1;
		return Some(SemanticKey::parent_scoped(
			"clausewitz.item.number.position",
			position.to_string(),
		));
	}

	let kind = match value {
		ScalarValue::Identifier(_) => "identifier",
		ScalarValue::String(_) => "string",
		ScalarValue::Bool(_) => "bool",
		ScalarValue::Number(_) => unreachable!("numbers handled above"),
	};
	let occurrence_key = (kind.to_string(), value.as_text());
	let occurrence = occurrences.entry(occurrence_key.clone()).or_default();
	let anchor = SemanticKey::parent_scoped(
		"clausewitz.item.scalar",
		format!("{}:{}:{}", occurrence_key.0, occurrence_key.1, *occurrence),
	);
	*occurrence += 1;
	Some(anchor)
}

pub(super) fn normalize_value(
	value: &AstValue,
	assignment_key: Option<&str>,
	policy: &impl ClausewitzTreePolicy,
) -> Result<TreeNode, AstAdapterError> {
	Ok(match value {
		AstValue::Scalar { value, .. } => match value {
			ScalarValue::Identifier(value) => leaf(IDENTIFIER_KIND, value.clone()),
			ScalarValue::String(value) => leaf(STRING_KIND, value.clone()),
			ScalarValue::Number(value) => leaf(NUMBER_KIND, value.clone()),
			ScalarValue::Bool(value) => leaf(BOOL_KIND, value.to_string()),
		},
		AstValue::Block { items, .. } => branch(
			&block_kind(assignment_key),
			None,
			None,
			policy.block_child_order(assignment_key),
			ChildCardinality::Many,
			normalize_statements(items, policy)?,
		),
	})
}

fn block_kind(assignment_key: Option<&str>) -> String {
	assignment_key.map_or_else(
		|| BLOCK_KIND_PREFIX.to_string(),
		|key| format!("{BLOCK_KIND_PREFIX}:{key}"),
	)
}

fn is_block_kind(kind: &str) -> bool {
	kind == BLOCK_KIND_PREFIX || kind.starts_with(&format!("{BLOCK_KIND_PREFIX}:"))
}

fn denormalize_statements(
	tree: &NormalizedTree,
	children: &[NodeId],
) -> Result<Vec<AstStatement>, AstAdapterError> {
	let mut statements = Vec::with_capacity(children.len());
	for child in children {
		let node = tree.node(*child)?;
		if super::control_flow::is_chain_kind(&node.kind) {
			statements.extend(super::control_flow::denormalize_chain(tree, *child, node)?);
		} else {
			statements.push(denormalize_statement(tree, *child)?);
		}
	}
	Ok(statements)
}

pub(super) fn denormalize_statement(
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

pub(super) fn denormalize_only_value_child(
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
		kind if is_block_kind(kind) => {
			let items = denormalize_statements(tree, &node.children)?;
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

pub(super) fn branch(
	kind: &str,
	value: Option<String>,
	anchor: Option<SemanticKey>,
	child_order: ChildOrder,
	child_cardinality: ChildCardinality,
	children: Vec<TreeNode>,
) -> TreeNode {
	TreeNode {
		kind: kind.to_string(),
		value,
		anchor,
		signature: None,
		child_order,
		child_cardinality,
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

pub(super) fn synthetic_span() -> SpanRange {
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
