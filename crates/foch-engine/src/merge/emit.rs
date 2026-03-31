#![allow(dead_code)]

use super::ir::{MergeIrNode, MergeIrStructuralFile, MergeIrStructuralKind};
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};
use std::collections::BTreeMap;

#[derive(Default)]
struct DefinesEmitNode {
	leaf_value: Option<ScalarValue>,
	children: BTreeMap<String, DefinesEmitNode>,
}

pub(crate) fn emit_structural_file(file: &MergeIrStructuralFile) -> Result<String, String> {
	match file.kind {
		MergeIrStructuralKind::Events
		| MergeIrStructuralKind::ScriptedEffects
		| MergeIrStructuralKind::DiplomaticActions
		| MergeIrStructuralKind::TriggeredModifiers => emit_top_level_nodes(&file.nodes),
		MergeIrStructuralKind::Decisions => emit_decision_nodes(&file.nodes),
		MergeIrStructuralKind::Defines => emit_defines_nodes(&file.nodes),
	}
}

pub(crate) fn emit_clausewitz_statements(statements: &[AstStatement]) -> Result<String, String> {
	let mut out = String::new();
	for statement in statements {
		emit_statement(statement, 0, &mut out)?;
	}
	Ok(out)
}

fn emit_top_level_nodes(nodes: &[MergeIrNode]) -> Result<String, String> {
	let mut ordered = nodes.iter().collect::<Vec<_>>();
	ordered.sort_by(|left, right| left.merge_key.cmp(&right.merge_key));

	let mut out = String::new();
	for node in ordered {
		emit_statement(&node.winning_statement, 0, &mut out)?;
	}
	Ok(out)
}

fn emit_decision_nodes(nodes: &[MergeIrNode]) -> Result<String, String> {
	let mut grouped = BTreeMap::<String, Vec<&MergeIrNode>>::new();
	for node in nodes {
		let Some(container_key) = node.container_key.clone() else {
			return Err(format!(
				"decision node {} is missing a container key",
				node.merge_key
			));
		};
		grouped.entry(container_key).or_default().push(node);
	}

	let mut out = String::new();
	for (container_key, mut group_nodes) in grouped {
		group_nodes.sort_by(|left, right| left.merge_key.cmp(&right.merge_key));
		out.push_str(&container_key);
		out.push_str(" = {\n");
		for node in group_nodes {
			emit_statement(&node.winning_statement, 1, &mut out)?;
		}
		out.push_str("}\n");
	}
	Ok(out)
}

fn emit_defines_nodes(nodes: &[MergeIrNode]) -> Result<String, String> {
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

fn insert_defines_node(root: &mut DefinesEmitNode, node: &MergeIrNode) -> Result<(), String> {
	if node.path_segments.is_empty() {
		return Err(format!(
			"defines node {} is missing assignment path segments",
			node.merge_key
		));
	}

	let AstStatement::Assignment {
		key,
		value: AstValue::Scalar { value, .. },
		..
	} = &node.winning_statement
	else {
		return Err(format!(
			"defines node {} must use a scalar winning assignment",
			node.merge_key
		));
	};
	if key != node.path_segments.last().expect("path segments checked") {
		return Err(format!(
			"defines node {} has mismatched statement key {}",
			node.merge_key, key
		));
	}

	let mut current = root;
	for segment in &node.path_segments[..node.path_segments.len() - 1] {
		if current.leaf_value.is_some() {
			return Err(format!(
				"defines emission found leaf-vs-branch conflict at {}",
				node.merge_key
			));
		}
		current = current.children.entry(segment.clone()).or_default();
	}

	let leaf_key = node.path_segments.last().expect("path segments checked");
	let leaf = current.children.entry(leaf_key.clone()).or_default();
	if !leaf.children.is_empty() {
		return Err(format!(
			"defines emission found branch-vs-leaf conflict at {}",
			node.merge_key
		));
	}
	leaf.leaf_value = Some(value.clone());
	Ok(())
}

fn emit_defines_branch(
	key: &str,
	node: &DefinesEmitNode,
	indent: usize,
	out: &mut String,
) -> Result<(), String> {
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

fn emit_statement(statement: &AstStatement, indent: usize, out: &mut String) -> Result<(), String> {
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
		AstStatement::Comment { .. } => Ok(()),
	}
}

fn emit_value(value: &AstValue, indent: usize, out: &mut String) -> Result<(), String> {
	match value {
		AstValue::Scalar { value, .. } => {
			out.push_str(&render_scalar(value));
			Ok(())
		}
		AstValue::Block { items, .. } => {
			out.push_str("{\n");
			for item in items {
				if matches!(item, AstStatement::Comment { .. }) {
					continue;
				}
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
