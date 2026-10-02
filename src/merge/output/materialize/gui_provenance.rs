//! Static GUI localisation fields observed in EU4 1.37.5 base interface files.
//! See docs/provenance-gui-assessment.md for the exact examples and limits.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::game::eu4::content::MergePolicies;
use crate::game::eu4::script::parser::{
	AstFile, AstStatement, AstValue, ScalarValue, Span, SpanRange,
};
use crate::merge::kernel::{NodeId, NormalizedTree};
use crate::merge::model::{
	SemanticMergeComputation, SemanticOrigin, SemanticPartitionId, SemanticPartitionLineage,
	VanillaBaseMode,
};
use crate::merge::structured::{ClausewitzFileAdapter, TreePartitionAdapter};

use super::provenance_tooltip::{
	PROVENANCE_KEY_PREFIX, ProvenanceTooltipOutput, display_name, is_safe_localisation_key,
};

const TOOLTIP_FIELDS: [&str; 5] = [
	"pdx_tooltip",
	"tooltipText",
	"tooltip",
	"pdx_tooltip_delayed",
	"delayedTooltipText",
];

struct WidgetProjection {
	statement_path: Vec<usize>,
	node: NodeId,
}

pub(super) fn materialize_gui_provenance_tooltips(
	enabled: bool,
	vanilla_base_mode: VanillaBaseMode,
	target_path: &str,
	statements: Vec<AstStatement>,
	semantic: &SemanticMergeComputation,
	policies: &MergePolicies,
	display_names: &HashMap<String, String>,
) -> Result<ProvenanceTooltipOutput, String> {
	let mut output = ProvenanceTooltipOutput {
		statements,
		localisation: BTreeMap::new(),
	};
	let target = target_path.replace('\\', "/");
	if !enabled
		|| !target.ends_with(".gui")
		|| !(target.starts_with("interface/") || target.starts_with("common/interface/"))
		|| semantic.partition_lineage.len() != 1
	{
		return Ok(output);
	}
	let Some(lineage) = semantic.partition_lineage.get(&SemanticPartitionId::File) else {
		return Ok(output);
	};
	if lineage.sources.is_empty() {
		return Ok(output);
	}
	let file = AstFile {
		path: PathBuf::from(&target),
		statements: output.statements.clone(),
	};
	let Ok(final_tree) = ClausewitzFileAdapter
		.prepare(&file)
		.normalize(&SemanticPartitionId::File, policies)
	else {
		return Ok(output);
	};
	// Coalescing scroll-stack widgets may synthesize a different final tree.
	// Without a verified projection, leave that file's GUI tooltips unchanged.
	if final_tree != lineage.tree {
		return Ok(output);
	}
	let root = final_tree
		.node(final_tree.root())
		.map_err(|error| error.to_string())?;
	let mut widgets = Vec::new();
	if project_widgets(
		&output.statements,
		&final_tree,
		&root.children,
		false,
		&mut Vec::new(),
		&mut widgets,
	)
	.is_none()
	{
		return Ok(output);
	}
	for widget in widgets {
		let (sources, entirely_mod_created) = widget_sources(lineage, widget.node)?;
		let entirely_mod_created =
			entirely_mod_created && vanilla_base_mode != VanillaBaseMode::ExplicitlyDisabled;
		if sources.is_empty() {
			continue;
		}
		let Some(AstStatement::Assignment {
			key: widget_type,
			value: AstValue::Block { items, .. },
			..
		}) = statement_at_path_mut(&mut output.statements, &widget.statement_path)
		else {
			return Err("GUI provenance projection lost its final widget".to_string());
		};
		let Some((field, index, original)) =
			tooltip_target(widget_type, items, entirely_mod_created)
		else {
			continue;
		};
		let key = wrapper_key(&target, widget.node, field, original.as_deref());
		let names = sources
			.iter()
			.map(|id| display_name(id, display_names))
			.collect::<Vec<_>>();
		let attribution = format!("Merged from {}", names.join(", "));
		let value = match original {
			Some(original) => format!("${original}$\\n\\n{attribution}"),
			None => attribution,
		};
		if let Some(existing) = output.localisation.insert(key.clone(), value.clone())
			&& existing != value
		{
			return Err(format!(
				"GUI provenance localisation key collision for {key}"
			));
		}
		if let Some(index) = index {
			if let AstStatement::Assignment {
				value: AstValue::Scalar { value, .. },
				..
			} = &mut items[index]
			{
				match value {
					ScalarValue::Identifier(value) | ScalarValue::String(value) => *value = key,
					_ => unreachable!("tooltip_target requires a textual scalar"),
				}
			}
		} else {
			let start = Span {
				line: 0,
				column: 0,
				offset: 0,
			};
			let span = SpanRange {
				start: start.clone(),
				end: start,
			};
			items.push(AstStatement::Assignment {
				key: field.to_string(),
				key_span: span.clone(),
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(key),
					span: span.clone(),
				},
				span,
			});
		}
	}
	Ok(output)
}

fn supported_widget(key: &str) -> bool {
	matches!(
		key,
		"iconType" | "instantTextBoxType" | "buttonType" | "guiButtonType"
	)
}

/// A structural zipper, after exact normalized-tree equality, preserves
/// occurrence identity for duplicate names. Never join widgets by name alone.
fn project_widgets(
	statements: &[AstStatement],
	tree: &NormalizedTree,
	siblings: &[NodeId],
	inside_gui: bool,
	path: &mut Vec<usize>,
	widgets: &mut Vec<WidgetProjection>,
) -> Option<()> {
	let mut siblings = siblings.iter();
	for (index, statement) in statements.iter().enumerate() {
		if matches!(statement, AstStatement::Comment { .. }) {
			continue;
		}
		let id = *siblings.next()?;
		let node = tree.node(id).ok()?;
		let (value, nested_gui, eligible) = match statement {
			AstStatement::Assignment { key, value, .. } => {
				if node.kind != format!("clausewitz.assignment:{key}")
					|| node.value.as_deref() != Some(key)
				{
					return None;
				}
				(
					value,
					inside_gui || (path.is_empty() && key == "guiTypes"),
					inside_gui && supported_widget(key),
				)
			}
			AstStatement::Item { value, .. } if node.kind == "clausewitz.item" => {
				(value, inside_gui, false)
			}
			_ => return None,
		};
		let [value_id] = node.children.as_slice() else {
			return None;
		};
		let value_node = tree.node(*value_id).ok()?;
		path.push(index);
		if let AstValue::Block { items, .. } = value {
			if !value_node.kind.starts_with("clausewitz.block") {
				return None;
			}
			if eligible {
				widgets.push(WidgetProjection {
					statement_path: path.clone(),
					node: id,
				});
			}
			project_widgets(items, tree, &value_node.children, nested_gui, path, widgets)?;
		} else if !value_node.kind.starts_with("clausewitz.scalar.")
			|| !value_node.children.is_empty()
		{
			return None;
		}
		path.pop();
	}
	siblings.next().is_none().then_some(())
}

fn statement_at_path_mut<'a>(
	statements: &'a mut [AstStatement],
	path: &[usize],
) -> Option<&'a mut AstStatement> {
	let (&index, rest) = path.split_first()?;
	let statement = statements.get_mut(index)?;
	if rest.is_empty() {
		return Some(statement);
	}
	match statement {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		}
		| AstStatement::Item {
			value: AstValue::Block { items, .. },
			..
		} => statement_at_path_mut(items, rest),
		_ => None,
	}
}

fn widget_sources(
	lineage: &SemanticPartitionLineage,
	root: NodeId,
) -> Result<(Vec<String>, bool), String> {
	let mut pending = vec![root];
	let mut sources = BTreeMap::<String, usize>::new();
	let mut entirely_mod_created = true;
	while let Some(node) = pending.pop() {
		if let Some(adopted) = lineage.sources.get(&node) {
			for source in adopted {
				sources
					.entry(source.source_id.clone())
					.and_modify(|precedence| *precedence = (*precedence).min(source.precedence))
					.or_insert(source.precedence);
			}
		}
		// Missing or empty origin records cannot establish a mod-created widget.
		entirely_mod_created &= lineage.origins.get(&node).is_some_and(|origins| {
			!origins.is_empty() && !origins.contains(&SemanticOrigin::Vanilla)
		});
		pending.extend(
			lineage
				.tree
				.node(node)
				.map_err(|error| error.to_string())?
				.children
				.iter()
				.copied(),
		);
	}
	let mut sources = sources.into_iter().collect::<Vec<_>>();
	sources.sort_by(|(left_id, left_order), (right_id, right_order)| {
		left_order
			.cmp(right_order)
			.then_with(|| left_id.cmp(right_id))
	});
	Ok((
		sources.into_iter().map(|(id, _)| id).collect(),
		entirely_mod_created,
	))
}

/// Returns at most one well-formed textual assignment. Missing and blank are
/// distinct so a blank field can be filled without adding a duplicate field.
fn direct_field<'a>(items: &'a [AstStatement], field: &str) -> Option<Option<(usize, &'a str)>> {
	let mut found = None;
	for (index, item) in items.iter().enumerate() {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != field {
			continue;
		}
		if found.is_some() {
			return None;
		}
		let AstValue::Scalar {
			value: ScalarValue::Identifier(text) | ScalarValue::String(text),
			..
		} = value
		else {
			return None;
		};
		found = Some((index, text.as_str()));
	}
	Some(found)
}

fn tooltip_target(
	widget_type: &str,
	items: &[AstStatement],
	entirely_mod_created: bool,
) -> Option<(&'static str, Option<usize>, Option<String>)> {
	let (_, name) = direct_field(items, "name")??;
	if name.trim().is_empty() {
		return None;
	}
	let fields = TOOLTIP_FIELDS
		.iter()
		.map(|field| direct_field(items, field))
		.collect::<Option<Vec<_>>>()?;
	fn nonempty(field: Option<(usize, &str)>) -> Option<(usize, &str)> {
		field.filter(|(_, text)| !text.trim().is_empty())
	}
	if nonempty(fields[2]).is_some() {
		return None;
	}
	let pdx = nonempty(fields[0]);
	let legacy = nonempty(fields[1]);
	let (field, index, original) = match (pdx, legacy) {
		(Some((index, original)), None) => ("pdx_tooltip", index, original),
		(None, Some((index, original))) if widget_type == "guiButtonType" => {
			("tooltipText", index, original)
		}
		(None, None)
			if entirely_mod_created && fields.iter().all(|field| nonempty(*field).is_none()) =>
		{
			return Some(("pdx_tooltip", fields[0].map(|(index, _)| index), None));
		}
		_ => return None,
	};
	if !is_safe_localisation_key(original) || original.starts_with(PROVENANCE_KEY_PREFIX) {
		return None;
	}
	Some((field, Some(index), Some(original.to_string())))
}

fn wrapper_key(target: &str, node: NodeId, field: &str, original: Option<&str>) -> String {
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-gui-provenance-v1\0");
	for part in [target, field, original.unwrap_or("")] {
		hasher.update(&(part.len() as u64).to_le_bytes());
		hasher.update(part.as_bytes());
	}
	hasher.update(&u64::from(node.get()).to_le_bytes());
	format!("{PROVENANCE_KEY_PREFIX}GUI_{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests;
