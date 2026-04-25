#![allow(dead_code)]

use super::error::MergeError;
use super::ir::{MergeIrNode, MergeIrStructuralFile};
use foch_language::analyzer::content_family::MergeKeySource;
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
struct DefinesEmitNode {
	leaf_value: Option<ScalarValue>,
	children: BTreeMap<String, DefinesEmitNode>,
}

pub(crate) fn emit_structural_file(file: &MergeIrStructuralFile) -> Result<String, MergeError> {
	match file.merge_key_source {
		MergeKeySource::AssignmentKey | MergeKeySource::FieldValue(_) => {
			emit_top_level_nodes(&file.passthrough_statements, &file.nodes)
		}
		MergeKeySource::ContainerChildKey => emit_decision_nodes(&file.nodes),
		MergeKeySource::LeafPath => emit_defines_nodes(&file.nodes),
	}
}

pub(crate) fn emit_clausewitz_statements(
	statements: &[AstStatement],
) -> Result<String, MergeError> {
	let mut out = String::new();
	for statement in statements {
		emit_statement(statement, 0, &mut out)?;
	}
	Ok(out)
}

fn emit_top_level_nodes(
	passthrough: &[AstStatement],
	nodes: &[MergeIrNode],
) -> Result<String, MergeError> {
	let mut out = String::new();
	for stmt in passthrough {
		emit_statement(stmt, 0, &mut out)?;
	}
	for node in nodes {
		emit_statement(&node.merged_statement, 0, &mut out)?;
	}
	Ok(out)
}

fn emit_decision_nodes(nodes: &[MergeIrNode]) -> Result<String, MergeError> {
	// Group by container_key, preserving first-appearance order
	let mut group_order: Vec<String> = Vec::new();
	let mut grouped: HashMap<String, Vec<&MergeIrNode>> = HashMap::new();
	for node in nodes {
		let Some(container_key) = node.container_key.clone() else {
			return Err(MergeError::Emit {
				path: None,
				message: format!(
					"decision node {} is missing a container key",
					node.merge_key
				),
			});
		};
		if !grouped.contains_key(&container_key) {
			group_order.push(container_key.clone());
		}
		grouped.entry(container_key).or_default().push(node);
	}

	let mut out = String::new();
	for container_key in &group_order {
		let group_nodes = &grouped[container_key];
		out.push_str(container_key);
		out.push_str(" = {\n");
		for node in group_nodes {
			emit_statement(&node.merged_statement, 1, &mut out)?;
		}
		out.push_str("}\n");
	}
	Ok(out)
}

fn emit_defines_nodes(nodes: &[MergeIrNode]) -> Result<String, MergeError> {
	let mut root = DefinesEmitNode::default();
	for node in nodes {
		insert_defines_node(&mut root, node)?;
	}

	let mut out = String::new();
	for (key, child) in &root.children {
		emit_defines_branch(key, child, 0, &mut out)?;
	}
	Ok(out)
}

fn insert_defines_node(root: &mut DefinesEmitNode, node: &MergeIrNode) -> Result<(), MergeError> {
	if node.path_segments.is_empty() {
		return Err(MergeError::Emit {
			path: None,
			message: format!(
				"defines node {} is missing assignment path segments",
				node.merge_key
			),
		});
	}

	let AstStatement::Assignment {
		key,
		value: AstValue::Scalar { value, .. },
		..
	} = &node.merged_statement
	else {
		return Err(MergeError::Emit {
			path: None,
			message: format!(
				"defines node {} must use a scalar merged assignment",
				node.merge_key
			),
		});
	};
	if key != node.path_segments.last().expect("path segments checked") {
		return Err(MergeError::Emit {
			path: None,
			message: format!(
				"defines node {} has mismatched statement key {}",
				node.merge_key, key
			),
		});
	}

	let mut current = root;
	for segment in &node.path_segments[..node.path_segments.len() - 1] {
		if current.leaf_value.is_some() {
			return Err(MergeError::Emit {
				path: None,
				message: format!(
					"defines emission found leaf-vs-branch conflict at {}",
					node.merge_key
				),
			});
		}
		current = current.children.entry(segment.clone()).or_default();
	}

	let leaf_key = node.path_segments.last().expect("path segments checked");
	let leaf = current.children.entry(leaf_key.clone()).or_default();
	if !leaf.children.is_empty() {
		return Err(MergeError::Emit {
			path: None,
			message: format!(
				"defines emission found branch-vs-leaf conflict at {}",
				node.merge_key
			),
		});
	}
	leaf.leaf_value = Some(value.clone());
	Ok(())
}

fn emit_defines_branch(
	key: &str,
	node: &DefinesEmitNode,
	indent: usize,
	out: &mut String,
) -> Result<(), MergeError> {
	indent_into(out, indent);
	out.push_str(key);
	out.push_str(" = ");
	if let Some(value) = node.leaf_value.as_ref() {
		out.push_str(&render_scalar(value));
		out.push('\n');
		return Ok(());
	}

	out.push_str("{\n");
	for (child_key, child_node) in &node.children {
		emit_defines_branch(child_key, child_node, indent + 1, out)?;
	}
	indent_into(out, indent);
	out.push_str("}\n");
	Ok(())
}

fn emit_statement(
	statement: &AstStatement,
	indent: usize,
	out: &mut String,
) -> Result<(), MergeError> {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			indent_into(out, indent);
			out.push_str(key);
			out.push_str(" = ");
			emit_value(value, indent, out)?;
			out.push('\n');
			Ok(())
		}
		AstStatement::Item { value, .. } => {
			indent_into(out, indent);
			emit_value(value, indent, out)?;
			out.push('\n');
			Ok(())
		}
		AstStatement::Comment { text, .. } => {
			indent_into(out, indent);
			out.push_str("# ");
			out.push_str(text);
			out.push('\n');
			Ok(())
		}
	}
}

fn emit_value(value: &AstValue, indent: usize, out: &mut String) -> Result<(), MergeError> {
	match value {
		AstValue::Scalar { value, .. } => {
			out.push_str(&render_scalar(value));
			Ok(())
		}
		AstValue::Block { items, .. } => {
			out.push_str("{\n");
			for item in items {
				emit_statement(item, indent + 1, out)?;
			}
			indent_into(out, indent);
			out.push('}');
			Ok(())
		}
	}
}

fn render_scalar(value: &ScalarValue) -> String {
	match value {
		ScalarValue::Identifier(value) => value.clone(),
		ScalarValue::String(value) => format!("\"{}\"", escape_string(value)),
		ScalarValue::Number(value) => value.clone(),
		ScalarValue::Bool(value) => {
			if *value {
				"yes".to_string()
			} else {
				"no".to_string()
			}
		}
	}
}

fn escape_string(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn indent_into(out: &mut String, indent: usize) {
	for _ in 0..indent {
		out.push('\t');
	}
}
