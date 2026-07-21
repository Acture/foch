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
const COMMENT_KIND: &str = "clausewitz.comment";
const BLOCK_KIND_PREFIX: &str = "clausewitz.block";
const CONTROL_FLOW_CHAIN_KIND_PREFIX: &str = "clausewitz.control_flow.chain:";
const CONTROL_FLOW_GUARDED_BRANCH_KIND_PREFIX: &str = "clausewitz.control_flow.guarded_branch:";
const CONTROL_FLOW_ELSE_BRANCH_KIND: &str = "clausewitz.control_flow.else_branch";
const IDENTIFIER_KIND: &str = "clausewitz.scalar.identifier";
const STRING_KIND: &str = "clausewitz.scalar.string";
const NUMBER_KIND: &str = "clausewitz.scalar.number";
const BOOL_KIND: &str = "clausewitz.scalar.bool";

#[derive(Debug)]
pub enum AstAdapterError {
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
	let children = normalize_statements(&file.statements, policy);
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
) -> Vec<TreeNode> {
	let mut children = Vec::with_capacity(statements.len());
	let mut index = 0;
	while index < statements.len() {
		if assignment_key(&statements[index]) == Some("if") {
			let (chain, next) = normalize_control_flow_chain(statements, index, policy);
			children.push(chain);
			index = next;
		} else {
			children.push(normalize_statement(&statements[index], policy));
			index += 1;
		}
	}
	children
}

fn normalize_control_flow_chain(
	statements: &[AstStatement],
	start: usize,
	policy: &impl ClausewitzTreePolicy,
) -> (TreeNode, usize) {
	let first = normalize_guarded_branch(&statements[start], policy);
	let signature = first.signature.clone();
	let mut children = vec![first];
	let mut cursor = start + 1;

	loop {
		let mut branch = cursor;
		while statements
			.get(branch)
			.is_some_and(|statement| matches!(statement, AstStatement::Comment { .. }))
		{
			branch += 1;
		}
		let Some(key @ ("else_if" | "else")) = statements.get(branch).and_then(assignment_key)
		else {
			break;
		};
		children.extend(
			statements[cursor..branch]
				.iter()
				.map(|statement| normalize_statement(statement, policy)),
		);
		children.push(if key == "else_if" {
			normalize_guarded_branch(&statements[branch], policy)
		} else {
			normalize_else_branch(&statements[branch], policy)
		});
		cursor = branch + 1;
		if key == "else" {
			break;
		}
	}

	let kind = control_flow_chain_kind(&children);
	let anchor = signature.as_ref().map(|signature| {
		SemanticKey::parent_scoped("clausewitz.control_flow.chain.guard", signature.clone())
	});
	if let Some(signature) = &signature {
		for child in &mut children {
			if child.kind == CONTROL_FLOW_ELSE_BRANCH_KIND {
				child.anchor = Some(SemanticKey::parent_scoped(
					"clausewitz.control_flow.branch.guard",
					format!("{signature}:else"),
				));
			}
		}
	}
	let mut chain = branch(
		&kind,
		None,
		anchor,
		ChildOrder::Ordered,
		ChildCardinality::Many,
		children,
	);
	chain.signature = signature;
	(chain, cursor)
}

fn normalize_else_branch(statement: &AstStatement, policy: &impl ClausewitzTreePolicy) -> TreeNode {
	let AstStatement::Assignment { key, value, .. } = statement else {
		unreachable!("control-flow else branch is an assignment")
	};
	debug_assert_eq!(key, "else");
	let mut node = branch(
		CONTROL_FLOW_ELSE_BRANCH_KIND,
		None,
		None,
		ChildOrder::Ordered,
		ChildCardinality::ExactlyOne,
		vec![normalize_value(value, Some("else"), policy)],
	);
	node.signature = Some("control_flow:else".to_string());
	node
}

fn normalize_guarded_branch(
	statement: &AstStatement,
	policy: &impl ClausewitzTreePolicy,
) -> TreeNode {
	let AstStatement::Assignment { key, value, .. } = statement else {
		unreachable!("control-flow guarded branch is an assignment")
	};
	debug_assert!(matches!(key.as_str(), "if" | "else_if"));
	let kind = guarded_branch_kind(value);
	let mut node = branch(
		&kind,
		None,
		None,
		ChildOrder::Ordered,
		ChildCardinality::ExactlyOne,
		vec![normalize_value(value, Some("if"), policy)],
	);
	node.signature = control_flow_guard_signature(key, value, policy);
	node.anchor = node.signature.as_ref().map(|signature| {
		SemanticKey::parent_scoped("clausewitz.control_flow.branch.guard", signature.clone())
	});
	node
}

fn guarded_branch_kind(value: &AstValue) -> String {
	let AstValue::Block { items, .. } = value else {
		return format!("{CONTROL_FLOW_GUARDED_BRANCH_KIND_PREFIX}scalar");
	};
	let mut effects = items
		.iter()
		.filter_map(assignment_key)
		.filter(|key| *key != "limit")
		.collect::<Vec<_>>();
	effects.sort_unstable();
	effects.dedup();
	let role = if contains_assignment_key(value, "dynasty") {
		"exclusive:"
	} else {
		""
	};
	format!(
		"{CONTROL_FLOW_GUARDED_BRANCH_KIND_PREFIX}{role}{}",
		if effects.is_empty() {
			"empty".to_string()
		} else {
			effects.join("+")
		}
	)
}

fn contains_assignment_key(value: &AstValue, expected: &str) -> bool {
	let AstValue::Block { items, .. } = value else {
		return false;
	};
	items.iter().any(|statement| match statement {
		AstStatement::Assignment { key, value, .. } => {
			key == expected || contains_assignment_key(value, expected)
		}
		AstStatement::Item { value, .. } => contains_assignment_key(value, expected),
		AstStatement::Comment { .. } => false,
	})
}

fn is_guarded_branch_kind(kind: &str) -> bool {
	kind.starts_with(CONTROL_FLOW_GUARDED_BRANCH_KIND_PREFIX)
}

fn control_flow_chain_kind(children: &[TreeNode]) -> String {
	let effects = children
		.iter()
		.filter_map(|child| {
			child
				.kind
				.strip_prefix(CONTROL_FLOW_GUARDED_BRANCH_KIND_PREFIX)
				.or((child.kind == CONTROL_FLOW_ELSE_BRANCH_KIND).then_some("else"))
		})
		.collect::<Vec<_>>();
	format!("{CONTROL_FLOW_CHAIN_KIND_PREFIX}{}", effects.join(">"))
}

fn is_control_flow_chain_kind(kind: &str) -> bool {
	kind.starts_with(CONTROL_FLOW_CHAIN_KIND_PREFIX)
}

fn assignment_key(statement: &AstStatement) -> Option<&str> {
	match statement {
		AstStatement::Assignment { key, .. } => Some(key),
		AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
	}
}

fn normalize_statement(statement: &AstStatement, policy: &impl ClausewitzTreePolicy) -> TreeNode {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			let kind = format!("{ASSIGNMENT_KIND_PREFIX}{key}");
			let mut node = branch(
				&kind,
				Some(key.clone()),
				policy.assignment_anchor(key, value),
				ChildOrder::Ordered,
				ChildCardinality::ExactlyOne,
				vec![normalize_value(value, Some(key), policy)],
			);
			node.signature = control_flow_guard_signature(key, value, policy)
				.or_else(|| policy.assignment_signature(key, value));
			node
		}
		AstStatement::Item { value, .. } => branch(
			ITEM_KIND,
			None,
			None,
			ChildOrder::Ordered,
			ChildCardinality::ExactlyOne,
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
			&block_kind(assignment_key),
			None,
			None,
			policy.block_child_order(assignment_key),
			ChildCardinality::Many,
			normalize_statements(items, policy),
		),
	}
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

fn control_flow_guard_signature(
	key: &str,
	value: &AstValue,
	policy: &impl ClausewitzTreePolicy,
) -> Option<String> {
	if !matches!(key, "if" | "else_if") {
		return None;
	}
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	let limit = items
		.iter()
		.find(|statement| assignment_key(statement) == Some("limit"))?;
	let tree = NormalizedTree::from_root(normalize_statement(limit, policy)).ok()?;
	Some(format!(
		"limit:{}",
		tree.node(tree.root()).ok()?.subtree_hash
	))
}

fn denormalize_statements(
	tree: &NormalizedTree,
	children: &[NodeId],
) -> Result<Vec<AstStatement>, AstAdapterError> {
	let mut statements = Vec::with_capacity(children.len());
	for child in children {
		let node = tree.node(*child)?;
		if is_control_flow_chain_kind(&node.kind) {
			statements.extend(denormalize_control_flow_chain(tree, *child, node)?);
		} else {
			statements.push(denormalize_statement(tree, *child)?);
		}
	}
	Ok(statements)
}

fn denormalize_control_flow_chain(
	tree: &NormalizedTree,
	chain_id: NodeId,
	chain: &NormalizedNode,
) -> Result<Vec<AstStatement>, AstAdapterError> {
	let mut statements = Vec::with_capacity(chain.children.len());
	let mut branch_count = 0;
	let mut saw_else = false;
	let mut branch_keys = Vec::new();
	for (position, child) in chain.children.iter().enumerate() {
		let node = tree.node(*child)?;
		if node.kind == COMMENT_KIND {
			statements.push(denormalize_statement(tree, *child)?);
			continue;
		}
		if is_guarded_branch_kind(&node.kind) {
			if saw_else {
				return Err(AstAdapterError::InvalidTree(format!(
					"guarded branch at child {position} follows `else` in control-flow chain {}",
					chain_id.get()
				)));
			}
			let key = if branch_count == 0 { "if" } else { "else_if" };
			branch_count += 1;
			branch_keys.push(key.to_string());
			statements.push(AstStatement::Assignment {
				key: key.to_string(),
				key_span: synthetic_span(),
				value: denormalize_only_value_child(tree, node)?,
				span: synthetic_span(),
			});
			continue;
		}
		if node.kind == CONTROL_FLOW_ELSE_BRANCH_KIND {
			if branch_count == 0 || saw_else {
				return Err(AstAdapterError::InvalidTree(format!(
					"invalid `else` placement at child {position} in control-flow chain {} after [{}]",
					chain_id.get(),
					branch_keys.join(", ")
				)));
			}
			saw_else = true;
			branch_keys.push("else".to_string());
			statements.push(AstStatement::Assignment {
				key: "else".to_string(),
				key_span: synthetic_span(),
				value: denormalize_only_value_child(tree, node)?,
				span: synthetic_span(),
			});
			continue;
		}
		let key = node.value.as_deref().ok_or_else(|| {
			AstAdapterError::InvalidTree(
				"control-flow chain contains a non-branch node".to_string(),
			)
		})?;
		let valid = match (branch_count, key, saw_else) {
			(_, "else", false) if branch_count > 0 => {
				saw_else = true;
				true
			}
			_ => false,
		};
		if !valid {
			return Err(AstAdapterError::InvalidTree(format!(
				"invalid `{key}` placement at child {position} in control-flow chain {} after [{}]",
				chain_id.get(),
				branch_keys.join(", ")
			)));
		}
		branch_keys.push(key.to_string());
		statements.push(denormalize_statement(tree, *child)?);
	}
	if branch_count == 0 {
		return Err(AstAdapterError::InvalidTree(
			"control-flow chain contains no branches".to_string(),
		));
	}
	Ok(statements)
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

fn branch(
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
