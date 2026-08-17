use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use foch_language::analyzer::content_family::MergePolicies;
use foch_language::analyzer::eu4_profile::EU4_LOCALISATION_LANGUAGE_HEADERS;
use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue, ScalarValue};
use foch_merge_kernel::NodeId;

use crate::merge::error::MergeError;
use crate::merge::model::{
	SemanticMergeComputation, SemanticOrigin, SemanticPartitionId, SemanticPartitionLineage,
};
use crate::merge::structured::{DefinitionModuleAdapter, TreePartitionAdapter};

const DIPLOMATIC_ACTIONS_PREFIX: &str = "common/diplomatic_actions/";
pub(super) const PROVENANCE_KEY_PREFIX: &str = "FOCH_PROVENANCE_";
const BASE_GAME_DISPLAY_NAME: &str = "Europa Universalis IV";
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ProvenanceTooltipOutput {
	pub statements: Vec<AstStatement>,
	pub localisation: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FinalConditionCoordinate {
	definition_key: String,
	definition_occurrence: usize,
	condition_occurrence: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FinalConditionProjection {
	partition: SemanticPartitionId,
	node: NodeId,
	origins: BTreeSet<SemanticOrigin>,
}

/// Scoped identity boundary for the final diplomatic-actions AST.
///
/// The semantic lineage describes the DAG output, while materialization can
/// transform statements before emitting them. Diplomatic actions currently
/// have no GUI or per-entry transform, but that profile fact is not sufficient
/// evidence on its own: every final partition is normalized again through the
/// same adapter and must exactly equal its lineage tree before the structural
/// zipper below is allowed to address AST conditions by occurrence.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FinalSemanticProjection {
	conditions: BTreeMap<FinalConditionCoordinate, FinalConditionProjection>,
}

impl FinalSemanticProjection {
	fn build(
		target_path: &str,
		statements: &[AstStatement],
		semantic: &SemanticMergeComputation,
		policies: &MergePolicies,
	) -> Result<Self, String> {
		let adapter = DefinitionModuleAdapter;
		let final_file = AstFile {
			path: PathBuf::from(target_path),
			statements: statements.to_vec(),
		};
		let final_partitions = adapter
			.normalization_partitions(&final_file, &final_file)
			.into_iter()
			.collect::<BTreeSet<_>>();
		let mut lineage_partitions = BTreeSet::new();
		for (partition, lineage) in &semantic.partition_lineage {
			let root = lineage
				.tree
				.node(lineage.tree.root())
				.map_err(|error| error.to_string())?;
			// A deleted definition can retain an empty lineage partition for audit
			// continuity. It has no final AST node and therefore no projection slot.
			if !root.children.is_empty() {
				lineage_partitions.insert(partition.clone());
			}
		}
		if final_partitions != lineage_partitions {
			return Err(format!(
				"final diplomatic-actions partitions diverged from semantic lineage: final={final_partitions:?} lineage={lineage_partitions:?}",
			));
		}

		let mut conditions = BTreeMap::new();
		for partition in final_partitions {
			let lineage = semantic
				.partition_lineage
				.get(&partition)
				.ok_or_else(|| format!("missing final lineage partition {partition:?}"))?;
			let final_tree = adapter
				.normalize_partition(&final_file, &partition, policies)
				.map_err(|error| {
					format!("failed to normalize final diplomatic-actions partition: {error}")
				})?;
			if final_tree != lineage.tree {
				return Err(format!(
					"final diplomatic-actions AST diverged from semantic lineage for partition {partition:?}",
				));
			}
			for (coordinate, projection) in condition_projections(&partition, lineage)? {
				if conditions.insert(coordinate.clone(), projection).is_some() {
					return Err(format!(
						"final semantic projection repeated condition {coordinate:?}",
					));
				}
			}
		}
		Ok(Self { conditions })
	}
}

pub(super) fn materialize_condition_provenance_tooltips(
	enabled: bool,
	target_path: &str,
	mut statements: Vec<AstStatement>,
	semantic: &SemanticMergeComputation,
	policies: &MergePolicies,
	display_names: &HashMap<String, String>,
) -> Result<ProvenanceTooltipOutput, String> {
	let normalized_target = target_path.replace('\\', "/");
	if !enabled || !normalized_target.starts_with(DIPLOMATIC_ACTIONS_PREFIX) {
		return Ok(ProvenanceTooltipOutput {
			statements,
			localisation: BTreeMap::new(),
		});
	}

	let mut projection =
		FinalSemanticProjection::build(&normalized_target, &statements, semantic, policies)?;
	let mut localisation = BTreeMap::new();
	let mut definition_occurrences = HashMap::<String, usize>::new();

	for statement in &mut statements {
		let AstStatement::Assignment {
			key: definition_key,
			value: AstValue::Block { items, .. },
			..
		} = statement
		else {
			continue;
		};
		let definition_occurrence = definition_occurrences
			.entry(definition_key.clone())
			.or_default();
		let current_definition_occurrence = *definition_occurrence;
		*definition_occurrence += 1;
		let mut condition_occurrence = 0usize;

		for item in items {
			let AstStatement::Assignment {
				key,
				value: AstValue::Block {
					items: condition_items,
					..
				},
				..
			} = item
			else {
				continue;
			};
			if key != "condition" {
				continue;
			}
			let coordinate = FinalConditionCoordinate {
				definition_key: definition_key.clone(),
				definition_occurrence: current_definition_occurrence,
				condition_occurrence,
			};
			condition_occurrence += 1;
			let condition = projection.conditions.remove(&coordinate).ok_or_else(|| {
				format!("final AST condition is absent from semantic projection: {coordinate:?}")
			})?;
			let Some((tooltip_value, original_key)) = explicit_tooltip(condition_items) else {
				continue;
			};
			if original_key.starts_with(PROVENANCE_KEY_PREFIX)
				|| !is_safe_localisation_key(&original_key)
			{
				continue;
			}

			let (base, modifiers) = provenance_from_origins(
				&condition.origins,
				display_names,
				&condition.partition,
				condition.node,
			)?;
			let wrapper_key = provenance_wrapper_key(
				&normalized_target,
				&condition.partition,
				condition.node,
				&original_key,
			);
			let value = provenance_localisation_value(&original_key, &base, &modifiers);
			if let Some(existing) = localisation.insert(wrapper_key.clone(), value.clone())
				&& existing != value
			{
				return Err(format!(
					"provenance localisation key collision for {wrapper_key}"
				));
			}
			match tooltip_value {
				ScalarValue::Identifier(value) | ScalarValue::String(value) => *value = wrapper_key,
				ScalarValue::Number(_) | ScalarValue::Bool(_) => {
					unreachable!("explicit_tooltip accepts only textual scalar values")
				}
			}
		}
	}
	if !projection.conditions.is_empty() {
		return Err(format!(
			"semantic projection contains {} final condition(s) absent from the AST zipper",
			projection.conditions.len(),
		));
	}

	Ok(ProvenanceTooltipOutput {
		statements,
		localisation,
	})
}

pub(super) fn write_surviving_provenance_localisation(
	out_dir: &Path,
	module_entries: &BTreeMap<String, BTreeMap<String, String>>,
	surviving_script_paths: &BTreeSet<String>,
) -> Result<Option<String>, MergeError> {
	let mut entries = BTreeMap::<String, String>::new();
	for (script_path, script_entries) in module_entries {
		if !surviving_script_paths.contains(script_path) {
			continue;
		}
		for (key, value) in script_entries {
			if let Some(existing) = entries.insert(key.clone(), value.clone())
				&& existing != *value
			{
				return Err(MergeError::Validation {
					path: Some(script_path.clone()),
					message: format!("provenance localisation key collision for {key}"),
				});
			}
		}
	}
	if entries.is_empty() {
		return Ok(None);
	}

	let bytes = render_localisation_file(&entries);
	let digest = blake3::hash(&bytes).to_hex();
	let relative_path = format!("localisation/foch_provenance_{digest}.yml");
	let target = out_dir.join(&relative_path);
	if target.exists() {
		let existing = fs::read(&target)?;
		if existing != bytes {
			return Err(MergeError::Validation {
				path: Some(relative_path),
				message: "generated provenance localisation path collides with existing content"
					.to_string(),
			});
		}
		return Ok(Some(relative_path));
	}
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::write(target, bytes)?;
	Ok(Some(relative_path))
}

fn condition_projections(
	partition: &SemanticPartitionId,
	lineage: &SemanticPartitionLineage,
) -> Result<Vec<(FinalConditionCoordinate, FinalConditionProjection)>, String> {
	let tree = &lineage.tree;
	let root = tree.node(tree.root()).map_err(|error| error.to_string())?;
	let mut definition_occurrences = HashMap::<String, usize>::new();
	let mut conditions = Vec::new();
	for definition_id in &root.children {
		let definition = tree
			.node(*definition_id)
			.map_err(|error| error.to_string())?;
		if !definition.kind.starts_with("clausewitz.assignment:") {
			continue;
		}
		let Some(definition_key) = definition.value.as_ref() else {
			continue;
		};
		if let SemanticPartitionId::Definition(expected) = partition
			&& definition_key != expected
		{
			return Err(format!(
				"definition partition {expected} contains definition {definition_key}",
			));
		}
		let definition_occurrence = definition_occurrences
			.entry(definition_key.clone())
			.or_default();
		let current_definition_occurrence = *definition_occurrence;
		*definition_occurrence += 1;
		let [block_id] = definition.children.as_slice() else {
			continue;
		};
		let block = tree.node(*block_id).map_err(|error| error.to_string())?;
		let mut condition_occurrence = 0usize;
		for condition_id in &block.children {
			let condition = tree
				.node(*condition_id)
				.map_err(|error| error.to_string())?;
			if condition.kind != "clausewitz.assignment:condition"
				|| condition.value.as_deref() != Some("condition")
			{
				continue;
			}
			let coordinate = FinalConditionCoordinate {
				definition_key: definition_key.clone(),
				definition_occurrence: current_definition_occurrence,
				condition_occurrence,
			};
			condition_occurrence += 1;
			conditions.push((
				coordinate,
				FinalConditionProjection {
					partition: partition.clone(),
					node: *condition_id,
					origins: subtree_origins(lineage, *condition_id)?,
				},
			));
		}
	}
	Ok(conditions)
}

fn subtree_origins(
	lineage: &SemanticPartitionLineage,
	root: NodeId,
) -> Result<BTreeSet<SemanticOrigin>, String> {
	let mut origins = BTreeSet::new();
	let mut pending = vec![root];
	while let Some(node) = pending.pop() {
		let node_origins = lineage.origins.get(&node).ok_or_else(|| {
			format!(
				"semantic lineage is missing origins for node {}",
				node.get(),
			)
		})?;
		origins.extend(node_origins.iter().cloned());
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
	Ok(origins)
}

fn provenance_from_origins(
	origins: &BTreeSet<SemanticOrigin>,
	display_names: &HashMap<String, String>,
	partition: &SemanticPartitionId,
	node: NodeId,
) -> Result<(String, Vec<String>), String> {
	let has_vanilla = origins.contains(&SemanticOrigin::Vanilla);
	let mut mods = origins
		.iter()
		.filter_map(|origin| match origin {
			SemanticOrigin::Vanilla => None,
			SemanticOrigin::Mod(source) => Some(source.clone()),
		})
		.collect::<Vec<_>>();
	mods.sort_by(|left, right| {
		left.precedence
			.cmp(&right.precedence)
			.then_with(|| left.source_id.cmp(&right.source_id))
	});
	mods.dedup_by(|left, right| left.source_id == right.source_id);
	if has_vanilla {
		return Ok((
			BASE_GAME_DISPLAY_NAME.to_string(),
			mods.iter()
				.map(|source| display_name(&source.source_id, display_names))
				.collect(),
		));
	}
	let Some(base) = mods.first() else {
		return Err(format!(
			"condition node {} in partition {partition:?} has no semantic origin",
			node.get(),
		));
	};
	Ok((
		display_name(&base.source_id, display_names),
		mods[1..]
			.iter()
			.map(|source| display_name(&source.source_id, display_names))
			.collect(),
	))
}

fn explicit_tooltip(items: &mut [AstStatement]) -> Option<(&mut ScalarValue, String)> {
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != "tooltip" {
			continue;
		}
		let AstValue::Scalar { value, .. } = value else {
			return None;
		};
		let text = match value {
			ScalarValue::Identifier(value) | ScalarValue::String(value) => value.trim().to_string(),
			ScalarValue::Number(_) | ScalarValue::Bool(_) => return None,
		};
		return (!text.is_empty()).then_some((value, text));
	}
	None
}

fn explicit_tooltip_ref(items: &[AstStatement]) -> Option<(&ScalarValue, String)> {
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != "tooltip" {
			continue;
		}
		let AstValue::Scalar { value, .. } = value else {
			return None;
		};
		let text = match value {
			ScalarValue::Identifier(value) | ScalarValue::String(value) => value.trim().to_string(),
			ScalarValue::Number(_) | ScalarValue::Bool(_) => return None,
		};
		return (!text.is_empty()).then_some((value, text));
	}
	None
}

fn is_safe_localisation_key(key: &str) -> bool {
	!key.is_empty()
		&& key
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn provenance_wrapper_key(
	target_path: &str,
	partition: &SemanticPartitionId,
	condition_node: NodeId,
	original_key: &str,
) -> String {
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-provenance-tooltip-v1\0");
	for component in [target_path, original_key] {
		hasher.update(&(component.len() as u64).to_le_bytes());
		hasher.update(component.as_bytes());
	}
	match partition {
		SemanticPartitionId::File => {
			hasher.update(b"file\0");
		}
		SemanticPartitionId::Definition(key) => {
			hasher.update(b"definition\0");
			hasher.update(&(key.len() as u64).to_le_bytes());
			hasher.update(key.as_bytes());
		}
	}
	hasher.update(&u64::from(condition_node.get()).to_le_bytes());
	format!("{PROVENANCE_KEY_PREFIX}{}", hasher.finalize().to_hex())
}

fn display_name(source_id: &str, display_names: &HashMap<String, String>) -> String {
	let display = display_names
		.get(source_id)
		.map(String::as_str)
		.unwrap_or(source_id);
	let sanitized = sanitize_display_name(display);
	if sanitized.is_empty() {
		sanitize_display_name(source_id)
	} else {
		sanitized
	}
}

fn sanitize_display_name(display_name: &str) -> String {
	let mut sanitized = String::with_capacity(display_name.len());
	let mut preceding_space = false;
	for character in display_name.trim().chars() {
		let character = match character {
			'"' => '”',
			'\\' => '／',
			'$' => '＄',
			character if character.is_control() => ' ',
			character => character,
		};
		if character.is_whitespace() {
			if !preceding_space {
				sanitized.push(' ');
			}
			preceding_space = true;
		} else {
			sanitized.push(character);
			preceding_space = false;
		}
	}
	sanitized
}

fn provenance_localisation_value(
	original_key: &str,
	base: &str,
	modifier_names: &[String],
) -> String {
	let mut value = format!("${original_key}$\\n\\nBase: {base}");
	if !modifier_names.is_empty() {
		value.push_str("\\nModified by: ");
		value.push_str(&modifier_names.join(", "));
	}
	value
}

fn render_localisation_file(entries: &BTreeMap<String, String>) -> Vec<u8> {
	let mut bytes = Vec::with_capacity(
		UTF8_BOM.len() + EU4_LOCALISATION_LANGUAGE_HEADERS.len() * entries.len() * 160,
	);
	bytes.extend_from_slice(UTF8_BOM);
	for language in EU4_LOCALISATION_LANGUAGE_HEADERS {
		bytes.extend_from_slice(format!("{language}:\n").as_bytes());
		for (key, value) in entries {
			bytes.extend_from_slice(format!(" {key}:0 \"{value}\"\n").as_bytes());
		}
	}
	bytes
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::{BTreeMap, BTreeSet};
	use std::path::PathBuf;

	use foch_language::analyzer::content_family::{CwtType, MergePolicies};
	use foch_language::analyzer::parser::parse_clausewitz_content;
	use foch_language::analyzer::semantic_index::ParsedScriptFile;
	use foch_merge_kernel::NodeId;

	use crate::emit::emit_clausewitz_statements;
	use crate::merge::model::{
		SemanticMergeSource, SemanticOrigin, SemanticPartitionId, SemanticPartitionLineage,
	};
	use crate::merge::structured::normalize_clausewitz_partition;

	fn parsed(source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/diplomatic_actions/00_actions.txt");
		let parsed = parse_clausewitz_content(path.clone(), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		ParsedScriptFile {
			mod_id: "fixture".to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("diplomatic_actions"),
			module_name: "diplomatic_actions".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn source(source_id: &str, precedence: usize) -> SemanticMergeSource {
		SemanticMergeSource {
			source_id: source_id.to_string(),
			precedence,
		}
	}

	fn semantic_with_marker_origins(
		final_file: &ParsedScriptFile,
		marker_origins: &[(&str, Vec<SemanticOrigin>)],
	) -> SemanticMergeComputation {
		let tree = normalize_clausewitz_partition(
			&final_file.ast,
			&SemanticPartitionId::Definition("send_warning".to_string()),
			&MergePolicies::default(),
		)
		.expect("normalize final fixture");
		let mut origins = tree
			.nodes()
			.map(|(node, _)| (node, BTreeSet::new()))
			.collect::<BTreeMap<NodeId, BTreeSet<SemanticOrigin>>>();
		for (node_id, node) in tree.nodes() {
			let adopted = marker_origins
				.iter()
				.find(|(marker, _)| node.value.as_deref() == Some(*marker))
				.map(|(_, origins)| origins);
			if let Some(adopted) = adopted {
				origins
					.get_mut(&node_id)
					.expect("total fixture origin map")
					.extend(adopted.iter().cloned());
			}
		}
		let sources = origins
			.iter()
			.filter_map(|(node, origins)| {
				let sources = origins
					.iter()
					.filter_map(|origin| match origin {
						SemanticOrigin::Vanilla => None,
						SemanticOrigin::Mod(source) => Some(source.clone()),
					})
					.collect::<BTreeSet<_>>();
				(!sources.is_empty()).then_some((*node, sources))
			})
			.collect();
		SemanticMergeComputation {
			statements: final_file.ast.statements.clone(),
			source_deltas: Vec::new(),
			merge_facts: Vec::new(),
			partition_lineage: BTreeMap::from([(
				SemanticPartitionId::Definition("send_warning".to_string()),
				SemanticPartitionLineage {
					tree,
					sources,
					origins,
				},
			)]),
			unresolved_conflicts: Vec::new(),
			handler_resolutions: Vec::new(),
			resolved_conflict_ids: Vec::new(),
			conflict_resolutions: Vec::new(),
			output_directives: Vec::new(),
		}
	}

	fn semantic_with_condition_sources(final_file: &ParsedScriptFile) -> SemanticMergeComputation {
		semantic_with_marker_origins(
			final_file,
			&[
				(
					"base_mod_marker",
					vec![
						SemanticOrigin::Vanilla,
						SemanticOrigin::Mod(source("expanded", 10)),
					],
				),
				(
					"MOD_ADDED_TT",
					vec![SemanticOrigin::Mod(source("expanded", 10))],
				),
				(
					"dependency_marker",
					vec![SemanticOrigin::Mod(source("imperialism", 20))],
				),
			],
		)
	}

	fn projected_localisation_values(output: &ProvenanceTooltipOutput) -> Vec<String> {
		let mut values = Vec::new();
		for statement in &output.statements {
			let AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} = statement
			else {
				continue;
			};
			for item in items {
				let AstStatement::Assignment {
					key,
					value: AstValue::Block {
						items: condition_items,
						..
					},
					..
				} = item
				else {
					continue;
				};
				if key != "condition" {
					continue;
				}
				let (_, wrapper_key) = explicit_tooltip_ref(condition_items)
					.expect("projected condition has a tooltip");
				values.push(
					output
						.localisation
						.get(&wrapper_key)
						.unwrap_or_else(|| panic!("missing localisation for {wrapper_key}"))
						.clone(),
				);
			}
		}
		values
	}

	#[test]
	fn wraps_only_explicit_surviving_condition_tooltips_with_lineage() {
		let final_file = parsed(
			"send_warning = {\n\
			\tcondition = { tooltip = BASE_TT allow = { marker = base_mod_marker } }\n\
			\t# retained trivia must not invalidate the semantic projection\n\
			\tcondition = { tooltip = MOD_ADDED_TT allow = { marker = dependency_marker } }\n\
			\tcondition = { tooltip = \" \" allow = { always = no } }\n\
			\tcondition = { allow = { always = no } }\n\
			\tcondition = { tooltip = FOCH_PROVENANCE_existing allow = { always = no } }\n\
			}",
		);
		let semantic = semantic_with_condition_sources(&final_file);
		let display_names = HashMap::from([
			("expanded".to_string(), "Europa Expanded".to_string()),
			(
				"imperialism".to_string(),
				"Imperialism Reinvigorated".to_string(),
			),
		]);

		let output = materialize_condition_provenance_tooltips(
			true,
			"common/diplomatic_actions/foch_merged.txt",
			final_file.ast.statements.clone(),
			&semantic,
			&MergePolicies::default(),
			&display_names,
		)
		.expect("materialize provenance tooltips");
		let repeated = materialize_condition_provenance_tooltips(
			true,
			"common/diplomatic_actions/foch_merged.txt",
			final_file.ast.statements.clone(),
			&semantic,
			&MergePolicies::default(),
			&display_names,
		)
		.expect("repeat provenance tooltip materialization");
		assert_eq!(output, repeated, "wrapper keys must be deterministic");
		let rendered = emit_clausewitz_statements(&output.statements).expect("emit statements");

		assert_eq!(
			rendered.matches("tooltip = FOCH_PROVENANCE_").count(),
			3,
			"rendered={rendered}\nlocalisation={:?}",
			output.localisation
		);
		assert!(rendered.contains("tooltip = \" \""), "{rendered}");
		assert_eq!(output.localisation.len(), 2);
		assert!(output.localisation.values().any(|value| {
			value == "$BASE_TT$\\n\\nBase: Europa Universalis IV\\nModified by: Europa Expanded"
		}));
		assert!(output.localisation.values().any(|value| {
			value
				== "$MOD_ADDED_TT$\\n\\nBase: Europa Expanded\\nModified by: Imperialism Reinvigorated"
		}));
	}

	#[test]
	fn isolates_vanilla_and_mod_added_conditions_that_share_a_tooltip_key() {
		let final_file = parsed(
			"send_warning = {\n\
			\tcondition = { tooltip = SHARED_TT allow = { marker = vanilla_tweak } }\n\
			\tcondition = { tooltip = SHARED_TT allow = { marker = mod_added } }\n\
			}",
		);
		let semantic = semantic_with_marker_origins(
			&final_file,
			&[
				(
					"vanilla_tweak",
					vec![
						SemanticOrigin::Vanilla,
						SemanticOrigin::Mod(source("tweaks", 10)),
					],
				),
				(
					"mod_added",
					vec![SemanticOrigin::Mod(source("new_conditions", 20))],
				),
			],
		);
		let display_names = HashMap::from([
			("tweaks".to_string(), "Vanilla Tweaks".to_string()),
			("new_conditions".to_string(), "New Conditions".to_string()),
		]);

		let output = materialize_condition_provenance_tooltips(
			true,
			"common/diplomatic_actions/foch_merged.txt",
			final_file.ast.statements.clone(),
			&semantic,
			&MergePolicies::default(),
			&display_names,
		)
		.expect("materialize isolated provenance");

		assert_eq!(
			projected_localisation_values(&output),
			vec![
				"$SHARED_TT$\\n\\nBase: Europa Universalis IV\\nModified by: Vanilla Tweaks",
				"$SHARED_TT$\\n\\nBase: New Conditions",
			],
		);
	}

	#[test]
	fn isolates_same_tooltip_across_duplicate_definition_occurrences() {
		let final_file = parsed(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = from_a } } }\n\
			send_warning = { condition = { tooltip = SHARED_TT allow = { marker = from_b } } }",
		);
		let semantic = semantic_with_marker_origins(
			&final_file,
			&[
				("from_a", vec![SemanticOrigin::Mod(source("mod_a", 10))]),
				("from_b", vec![SemanticOrigin::Mod(source("mod_b", 20))]),
			],
		);
		let display_names = HashMap::from([
			("mod_a".to_string(), "Mod A".to_string()),
			("mod_b".to_string(), "Mod B".to_string()),
		]);

		let output = materialize_condition_provenance_tooltips(
			true,
			"common/diplomatic_actions/foch_merged.txt",
			final_file.ast.statements.clone(),
			&semantic,
			&MergePolicies::default(),
			&display_names,
		)
		.expect("materialize duplicate definition provenance");

		assert_eq!(
			projected_localisation_values(&output),
			vec![
				"$SHARED_TT$\\n\\nBase: Mod A",
				"$SHARED_TT$\\n\\nBase: Mod B",
			],
		);
	}

	#[test]
	fn final_projection_fails_closed_when_materialized_ast_drifted() {
		let semantic_file = parsed(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = original } } }",
		);
		let semantic = semantic_with_marker_origins(
			&semantic_file,
			&[("original", vec![SemanticOrigin::Mod(source("mod_a", 10))])],
		);
		let drifted = parsed(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = drifted } } }",
		);

		let error = materialize_condition_provenance_tooltips(
			true,
			"common/diplomatic_actions/foch_merged.txt",
			drifted.ast.statements,
			&semantic,
			&MergePolicies::default(),
			&HashMap::new(),
		)
		.expect_err("drifted final AST must fail closed");

		assert!(
			error.contains("final diplomatic-actions AST diverged from semantic lineage"),
			"{error}",
		);
	}

	#[test]
	fn final_projection_ignores_only_an_empty_deleted_partition() {
		let final_file = parsed(
			"send_warning = { condition = { tooltip = SURVIVING_TT allow = { marker = surviving } } }",
		);
		let mut semantic = semantic_with_marker_origins(
			&final_file,
			&[("surviving", vec![SemanticOrigin::Mod(source("mod_a", 10))])],
		);
		let deleted_partition = SemanticPartitionId::Definition("deleted_action".to_string());
		let deleted_tree = normalize_clausewitz_partition(
			&final_file.ast,
			&deleted_partition,
			&MergePolicies::default(),
		)
		.expect("normalize absent deleted partition");
		assert!(
			deleted_tree
				.node(deleted_tree.root())
				.expect("deleted root")
				.children
				.is_empty(),
		);
		let deleted_origins = deleted_tree
			.nodes()
			.map(|(node, _)| (node, BTreeSet::new()))
			.collect();
		semantic.partition_lineage.insert(
			deleted_partition,
			SemanticPartitionLineage {
				tree: deleted_tree,
				sources: BTreeMap::new(),
				origins: deleted_origins,
			},
		);

		let output = materialize_condition_provenance_tooltips(
			true,
			"common/diplomatic_actions/foch_merged.txt",
			final_file.ast.statements,
			&semantic,
			&MergePolicies::default(),
			&HashMap::from([("mod_a".to_string(), "Mod A".to_string())]),
		)
		.expect("empty deleted partition is not a final projection slot");

		assert_eq!(
			projected_localisation_values(&output),
			vec!["$SURVIVING_TT$\\n\\nBase: Mod A"],
		);
	}

	#[test]
	fn final_projection_rejects_a_missing_nonempty_partition() {
		let final_file = parsed(
			"send_warning = { condition = { tooltip = SURVIVING_TT allow = { marker = surviving } } }",
		);
		let mut semantic = semantic_with_marker_origins(
			&final_file,
			&[("surviving", vec![SemanticOrigin::Mod(source("mod_a", 10))])],
		);
		let missing_file = parsed(
			"deleted_action = { condition = { tooltip = DELETED_TT allow = { marker = deleted } } }",
		);
		let missing_partition = SemanticPartitionId::Definition("deleted_action".to_string());
		let missing_tree = normalize_clausewitz_partition(
			&missing_file.ast,
			&missing_partition,
			&MergePolicies::default(),
		)
		.expect("normalize nonempty missing partition");
		let missing_origins = missing_tree
			.nodes()
			.map(|(node, _)| {
				(
					node,
					BTreeSet::from([SemanticOrigin::Mod(source("mod_a", 10))]),
				)
			})
			.collect();
		semantic.partition_lineage.insert(
			missing_partition,
			SemanticPartitionLineage {
				tree: missing_tree,
				sources: BTreeMap::new(),
				origins: missing_origins,
			},
		);

		let error = materialize_condition_provenance_tooltips(
			true,
			"common/diplomatic_actions/foch_merged.txt",
			final_file.ast.statements,
			&semantic,
			&MergePolicies::default(),
			&HashMap::new(),
		)
		.expect_err("a nonempty missing partition must fail closed");

		assert!(error.contains("partitions diverged"), "{error}");
	}

	#[test]
	fn provenance_off_is_ast_and_byte_identical() {
		let final_file = parsed(
			"send_warning = { condition = { tooltip = SHARED_TT allow = { marker = from_a } } }",
		);
		let semantic = semantic_with_marker_origins(
			&final_file,
			&[("from_a", vec![SemanticOrigin::Mod(source("mod_a", 10))])],
		);
		let original = final_file.ast.statements.clone();
		let original_bytes = emit_clausewitz_statements(&original).expect("emit original bytes");

		let output = materialize_condition_provenance_tooltips(
			false,
			"common/diplomatic_actions/foch_merged.txt",
			original.clone(),
			&semantic,
			&MergePolicies::default(),
			&HashMap::new(),
		)
		.expect("disabled provenance projection");

		assert_eq!(output.statements, original);
		assert!(output.localisation.is_empty());
		assert_eq!(
			emit_clausewitz_statements(&output.statements).expect("emit disabled output"),
			original_bytes,
		);
	}

	#[test]
	fn writes_bom_prefixed_localisation_only_for_surviving_scripts() {
		let temp = tempfile::tempdir().expect("tempdir");
		let module_entries = BTreeMap::from([
			(
				"common/diplomatic_actions/kept.txt".to_string(),
				BTreeMap::from([(
					"FOCH_PROVENANCE_aaa".to_string(),
					"$ORIGINAL$\\n\\nBase: Europa Universalis IV".to_string(),
				)]),
			),
			(
				"common/diplomatic_actions/pruned.txt".to_string(),
				BTreeMap::from([(
					"FOCH_PROVENANCE_bbb".to_string(),
					"$REMOVED$\\n\\nBase: Removed Mod".to_string(),
				)]),
			),
		]);
		let surviving = BTreeSet::from(["common/diplomatic_actions/kept.txt".to_string()]);

		let relative =
			write_surviving_provenance_localisation(temp.path(), &module_entries, &surviving)
				.expect("write provenance localisation")
				.expect("localisation path");
		assert!(!relative.contains("_l_english"), "{relative}");
		let bytes = fs::read(temp.path().join(&relative)).expect("read localisation");

		assert!(bytes.starts_with(UTF8_BOM));
		assert_eq!(
			bytes
				.windows(UTF8_BOM.len())
				.filter(|window| *window == UTF8_BOM)
				.count(),
			1,
		);
		let text = std::str::from_utf8(&bytes[UTF8_BOM.len()..]).expect("utf8 localisation");
		assert_eq!(
			EU4_LOCALISATION_LANGUAGE_HEADERS,
			["l_english", "l_german", "l_french", "l_spanish"],
		);
		for language in EU4_LOCALISATION_LANGUAGE_HEADERS {
			assert!(text.contains(&format!("{language}:\n")), "{text}");
		}
		assert_eq!(
			text.matches(" FOCH_PROVENANCE_aaa:0 \"$ORIGINAL$").count(),
			EU4_LOCALISATION_LANGUAGE_HEADERS.len(),
			"every supported language must resolve the generated key: {text}",
		);
		assert!(!text.contains("FOCH_PROVENANCE_bbb"), "{text}");
		assert_eq!(
			write_surviving_provenance_localisation(temp.path(), &module_entries, &surviving,)
				.expect("repeat localisation write"),
			Some(relative),
			"the generated path must be content-stable"
		);
	}
}
