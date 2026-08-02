use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use foch_language::analyzer::parser::{
	AstFile, AstStatement, AstValue, ScalarValue, Span, SpanRange,
};
use foch_merge_kernel::{
	ChildCardinality, ChildOrder, NWayMergeError, NodeId, NormalizedNode, NormalizedTree,
	SemanticKey, TreeError, TreeNode,
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
	Merge(NWayMergeError),
	InvalidTree(String),
	DuplicateControlFlowGuard(String),
	UnprovableControlFlow(String),
}

impl fmt::Display for AstAdapterError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Kernel(error) => write!(formatter, "normalized tree error: {error}"),
			Self::Merge(error) => write!(formatter, "normalized tree merge error: {error}"),
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
			Self::Merge(error) => Some(error),
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

impl From<NWayMergeError> for AstAdapterError {
	fn from(error: NWayMergeError) -> Self {
		Self::Merge(error)
	}
}

pub(crate) fn normalize_ast(
	file: &AstFile,
	policy: &impl ClausewitzTreePolicy,
) -> Result<NormalizedTree, AstAdapterError> {
	let (tree, _) = normalize_ast_with_findings(file, policy)?;
	Ok(tree)
}

pub(crate) fn normalize_ast_with_findings(
	file: &AstFile,
	policy: &impl ClausewitzTreePolicy,
) -> Result<(NormalizedTree, Vec<String>), AstAdapterError> {
	let mut findings = Vec::new();
	let children = normalize_statements(&file.statements, None, policy, &mut findings)?;
	NormalizedTree::from_root(branch(
		FILE_KIND,
		None,
		None,
		ChildOrder::Ordered,
		ChildCardinality::Many,
		children,
	))
	.map(|tree| (tree, findings))
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
	parent_assignment_key: Option<&str>,
	policy: &impl ClausewitzTreePolicy,
	control_flow_findings: &mut Vec<String>,
) -> Result<Vec<TreeNode>, AstAdapterError> {
	let mut children = Vec::with_capacity(statements.len());
	let mut scalar_item_occurrences = BTreeMap::new();
	let mut numeric_item_position = 0;
	let mut index = 0;
	while index < statements.len() {
		if super::control_flow::starts_chain(&statements[index]) {
			let (chain, next) = super::control_flow::normalize_chain(
				statements,
				index,
				policy,
				control_flow_findings,
			)?;
			children.push(chain);
			index = next;
		} else {
			let item_anchor = match &statements[index] {
				AstStatement::Item { value, .. } => policy
					.item_anchor(parent_assignment_key, value)
					.or_else(|| {
						scalar_item_anchor(
							&statements[index],
							&mut scalar_item_occurrences,
							&mut numeric_item_position,
						)
					}),
				AstStatement::Assignment { .. } | AstStatement::Comment { .. } => None,
			};
			children.push(normalize_statement_with_item_anchor(
				&statements[index],
				parent_assignment_key,
				policy,
				item_anchor,
				control_flow_findings,
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

pub(super) fn normalize_statement_with_findings(
	statement: &AstStatement,
	policy: &impl ClausewitzTreePolicy,
	control_flow_findings: &mut Vec<String>,
) -> Result<TreeNode, AstAdapterError> {
	normalize_statement_with_item_anchor(statement, None, policy, None, control_flow_findings)
}

fn normalize_statement_with_item_anchor(
	statement: &AstStatement,
	parent_assignment_key: Option<&str>,
	policy: &impl ClausewitzTreePolicy,
	item_anchor: Option<SemanticKey>,
	control_flow_findings: &mut Vec<String>,
) -> Result<TreeNode, AstAdapterError> {
	Ok(match statement {
		AstStatement::Assignment { key, value, .. } => {
			let kind = format!("{ASSIGNMENT_KIND_PREFIX}{key}");
			let mut node = branch(
				&kind,
				Some(key.clone()),
				policy.assignment_anchor(parent_assignment_key, key, value),
				ChildOrder::Ordered,
				ChildCardinality::ExactlyOne,
				vec![normalize_value_with_findings(
					value,
					Some(key),
					policy,
					control_flow_findings,
				)?],
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
			vec![normalize_value_with_findings(
				value,
				None,
				policy,
				control_flow_findings,
			)?],
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

pub(super) fn normalize_value_with_findings(
	value: &AstValue,
	assignment_key: Option<&str>,
	policy: &impl ClausewitzTreePolicy,
	control_flow_findings: &mut Vec<String>,
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
			normalize_statements(items, assignment_key, policy, control_flow_findings)?,
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

pub(crate) fn top_level_assignment_key(
	tree: &NormalizedTree,
	mut id: NodeId,
) -> Result<Option<&str>, TreeError> {
	loop {
		let node = tree.node(id)?;
		let Some(parent) = node.parent else {
			return Ok(None);
		};
		if parent == tree.root() {
			return Ok(node
				.kind
				.starts_with(ASSIGNMENT_KIND_PREFIX)
				.then_some(node.value.as_deref())
				.flatten());
		}
		id = parent;
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticNodeAddress {
	pub path: Vec<String>,
	pub key: Option<String>,
}

pub(crate) fn semantic_node_address(
	tree: &NormalizedTree,
	node: NodeId,
) -> Result<SemanticNodeAddress, TreeError> {
	let original = tree.node(node)?;
	let target = if is_statement_node(original) {
		Some(node)
	} else {
		original
			.parent
			.filter(|parent| tree.node(*parent).is_ok_and(is_statement_node))
	};
	let Some(target) = target else {
		return Ok(SemanticNodeAddress {
			path: original
				.policy_path
				.split_last()
				.map_or_else(Vec::new, |(_, path)| path.to_vec()),
			key: original.policy_path.last().cloned(),
		});
	};

	let mut path = Vec::new();
	let mut current = tree.node(target)?.parent;
	while let Some(node) = current {
		let normalized = tree.node(node)?;
		if is_statement_node(normalized)
			&& let Some(label) = semantic_statement_label(normalized)
		{
			path.push(label.to_string());
		}
		current = normalized.parent;
	}
	path.reverse();
	Ok(SemanticNodeAddress {
		path,
		key: semantic_statement_label(tree.node(target)?).map(str::to_string),
	})
}

fn is_statement_node(node: &NormalizedNode) -> bool {
	node.kind.starts_with(ASSIGNMENT_KIND_PREFIX) || node.kind == ITEM_KIND
}

fn semantic_statement_label(node: &NormalizedNode) -> Option<&str> {
	node.anchor
		.as_ref()
		.map(|anchor| anchor.value.as_str())
		.or(node.value.as_deref())
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
