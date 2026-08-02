use super::super::super::error::MergeError;
use super::super::super::patch::ClausewitzPatch;
use super::super::super::patch_merge::PatchAddress;
use super::super::stale_vanilla::detect_stale_vanilla_targets;
use super::DepMisuseRemoveCount;
use crate::emit::{EmitOptions, emit_clausewitz_statements_with_options};
use crate::merge::model::{SemanticDeltaPartition, SemanticSourceDelta};
use crate::merge::structured::{normalize_clausewitz_partition, semantic_node_address};
use crate::workspace::{ResolvedFileContributor, WorkspaceScriptCache};
use foch_core::model::{DepMisuseFinding, StaleVanillaTargetDescriptor};
use foch_language::analyzer::content_family::{MergeKeySource, MergePolicies};
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::{ParsedScriptFile, parse_script_file};
use foch_merge_kernel::{DeltaOperation, NodeId, TreeMatcher};
use std::collections::{HashMap, HashSet};

const SEMANTIC_MISSING_PATH_NOTE: &str = "vanilla snapshot for this file does not contain the semantic parent; this remove-style change may be cross-version drift, dependency-targeted, or intentionally guarded";
const SEMANTIC_MISSING_KEY_NOTE: &str = "vanilla snapshot contains the semantic parent but not the target; this remove-style change may be cross-version drift, dependency-targeted, or intentionally guarded";

pub(super) fn vanilla_snippet_for_address(
	vanilla: Option<&ParsedScriptFile>,
	address: &PatchAddress,
	emit_options: &EmitOptions,
) -> Option<String> {
	let vanilla = vanilla?;
	let statements = vanilla_statements_at_address(&vanilla.ast.statements, address);
	Some(match statements {
		Some(statements) if !statements.is_empty() => {
			emit_clausewitz_statements_with_options(&statements, emit_options)
				.unwrap_or_else(|err| format!("(failed to render vanilla snippet: {err})"))
		}
		_ => "(key not present in vanilla)".to_string(),
	})
}

fn vanilla_statements_at_address(
	statements: &[AstStatement],
	address: &PatchAddress,
) -> Option<Vec<AstStatement>> {
	let mut current = statements;
	for segment in &address.path {
		current = current.iter().find_map(|statement| match statement {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} if key == segment => Some(items.as_slice()),
			_ => None,
		})?;
	}

	let Some(key) = vanilla_address_lookup_key(&address.key) else {
		return Some(current.to_vec());
	};
	if key.is_empty() {
		return Some(current.to_vec());
	}

	let matches = current
		.iter()
		.filter(|statement| {
			matches!(statement, AstStatement::Assignment { key: statement_key, .. } if statement_key == key)
		})
		.cloned()
		.collect::<Vec<_>>();
	(!matches.is_empty()).then_some(matches)
}

fn vanilla_address_lookup_key(address_key: &str) -> Option<&str> {
	if let Some(rest) = address_key.strip_prefix("__node__::") {
		return rest.split("::").next();
	}
	if let Some(rest) = address_key.strip_prefix("__list_item__::") {
		return rest.split("::").next();
	}
	if let Some(rest) = address_key.strip_prefix("__rename__::") {
		return Some(rest);
	}
	if address_key.starts_with("__append_block_item__::")
		|| address_key.starts_with("__remove_block_item__::")
	{
		return None;
	}
	Some(address_key)
}

pub(super) fn parse_vanilla_for_stale_detection(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	script_cache: &WorkspaceScriptCache,
) -> Result<Option<ParsedScriptFile>, MergeError> {
	let Some(base) = contributors
		.iter()
		.find(|contributor| contributor.is_base_game)
	else {
		return Ok(None);
	};
	if let Ok(relative) = base.absolute_path.strip_prefix(&base.root_path)
		&& let Some(parsed) = script_cache.get(&base.mod_id, relative)
	{
		return Ok(Some(parsed.clone()));
	}
	parse_script_file(&base.mod_id, &base.root_path, &base.absolute_path)
		.map(Some)
		.ok_or_else(|| MergeError::Validation {
			path: Some(file_path.to_string()),
			message: format!(
				"failed to parse vanilla file {} for stale target detection",
				base.absolute_path.display()
			),
		})
}

pub(super) fn collect_stale_vanilla_targets(
	file_path: &str,
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
	vanilla: Option<&ParsedScriptFile>,
	merge_key_source: MergeKeySource,
	mod_versions: &HashMap<String, String>,
) -> Vec<StaleVanillaTargetDescriptor> {
	mod_patches
		.iter()
		.flat_map(|(mod_id, _, patches)| {
			let mod_version = mod_versions
				.get(mod_id)
				.map(String::as_str)
				.unwrap_or("unknown");
			detect_stale_vanilla_targets(
				patches,
				file_path,
				mod_id,
				mod_version,
				vanilla,
				merge_key_source,
			)
		})
		.collect()
}

pub(super) fn collect_semantic_stale_vanilla_targets(
	file_path: &str,
	source_deltas: &[SemanticSourceDelta],
	vanilla: Option<&ParsedScriptFile>,
	policies: &MergePolicies,
	mod_versions: &HashMap<String, String>,
) -> Result<Vec<StaleVanillaTargetDescriptor>, String> {
	let Some(vanilla) = vanilla else {
		return Ok(Vec::new());
	};
	let mut findings = Vec::new();
	for source_delta in source_deltas {
		let mod_version = mod_versions
			.get(&source_delta.source.source_id)
			.map(String::as_str)
			.unwrap_or("unknown");
		for partition in &source_delta.partitions {
			let vanilla_tree =
				normalize_clausewitz_partition(&vanilla.ast, &partition.partition, policies)
					.map_err(|error| {
						format!(
							"failed to normalize vanilla partition {:?}: {error}",
							partition.partition
						)
					})?;
			let matching = TreeMatcher::default().match_trees(&vanilla_tree, &partition.base_tree);
			for (kind, target) in semantic_remove_targets(partition) {
				if matching.get_from_right(target).is_some() {
					continue;
				}
				let target_node = partition.base_tree.node(target).map_err(|error| {
					format!("semantic remove target is outside its base tree: {error}")
				})?;
				let parent_exists = target_node
					.parent
					.is_none_or(|parent| matching.get_from_right(parent).is_some());
				let address =
					semantic_node_address(&partition.base_tree, target).map_err(|error| {
						format!("failed to project semantic remove target: {error}")
					})?;
				if parent_exists && address.key.is_none() {
					continue;
				}
				findings.push(StaleVanillaTargetDescriptor {
					mod_id: source_delta.source.source_id.clone(),
					mod_version: mod_version.to_string(),
					file_path: file_path.to_string(),
					patch_kind: kind.to_string(),
					target_path: address.path,
					target_key: address.key,
					note: Some(
						if parent_exists {
							SEMANTIC_MISSING_KEY_NOTE
						} else {
							SEMANTIC_MISSING_PATH_NOTE
						}
						.to_string(),
					),
				});
			}
		}
	}
	Ok(findings)
}

fn semantic_remove_targets(
	partition: &SemanticDeltaPartition,
) -> impl Iterator<Item = (&'static str, NodeId)> + '_ {
	partition
		.delta
		.operations
		.iter()
		.filter_map(|operation| match operation {
			DeltaOperation::Delete { tombstone } => Some(("Delete", tombstone.deleted.node)),
			DeltaOperation::Rename { base, .. } => Some(("Rename", base.node)),
			DeltaOperation::Insert { .. }
			| DeltaOperation::Update { .. }
			| DeltaOperation::Move { .. } => None,
		})
}

pub(super) fn collect_dep_misuse_remove_counts(
	findings: &[DepMisuseFinding],
	contributors: &[ResolvedFileContributor],
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
) -> Vec<DepMisuseRemoveCount> {
	if findings.is_empty() {
		return Vec::new();
	}

	let contributor_mods = contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
		.map(|contributor| contributor.mod_id.as_str())
		.collect::<HashSet<_>>();
	let mut counts = Vec::new();
	for finding in findings {
		if !contributor_mods.contains(finding.mod_id.as_str())
			|| !contributor_mods.contains(finding.suspicious_dep_id.as_str())
		{
			continue;
		}

		let count = mod_patches
			.iter()
			.filter(|(mod_id, _, _)| mod_id == &finding.mod_id)
			.flat_map(|(_, _, patches)| patches)
			.filter(|patch| is_remove_patch(patch))
			.count();
		if count == 0 {
			continue;
		}
		counts.push(DepMisuseRemoveCount {
			mod_id: finding.mod_id.clone(),
			dep_id: finding.suspicious_dep_id.clone(),
			count: count.min(u32::MAX as usize) as u32,
		});
	}
	counts
}

pub(super) fn collect_semantic_dep_misuse_remove_counts(
	findings: &[DepMisuseFinding],
	contributors: &[ResolvedFileContributor],
	source_deltas: &[SemanticSourceDelta],
) -> Vec<DepMisuseRemoveCount> {
	if findings.is_empty() {
		return Vec::new();
	}

	let contributor_mods = contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
		.map(|contributor| contributor.mod_id.as_str())
		.collect::<HashSet<_>>();
	findings
		.iter()
		.filter(|finding| {
			contributor_mods.contains(finding.mod_id.as_str())
				&& contributor_mods.contains(finding.suspicious_dep_id.as_str())
		})
		.filter_map(|finding| {
			let count = source_deltas
				.iter()
				.filter(|delta| delta.source.source_id == finding.mod_id)
				.map(semantic_remove_count)
				.sum::<usize>();
			(count != 0).then(|| DepMisuseRemoveCount {
				mod_id: finding.mod_id.clone(),
				dep_id: finding.suspicious_dep_id.clone(),
				count: count.min(u32::MAX as usize) as u32,
			})
		})
		.collect()
}

fn semantic_remove_count(source_delta: &SemanticSourceDelta) -> usize {
	source_delta
		.partitions
		.iter()
		.flat_map(|partition| {
			partition
				.delta
				.operations
				.iter()
				.filter(|operation| match operation {
					DeltaOperation::Delete { tombstone } => partition
						.base_tree
						.node(tombstone.deleted.node)
						.is_ok_and(|node| {
							node.kind.starts_with("clausewitz.assignment:")
								|| node.kind == "clausewitz.item"
						}),
					DeltaOperation::Insert { .. }
					| DeltaOperation::Update { .. }
					| DeltaOperation::Move { .. }
					| DeltaOperation::Rename { .. } => false,
				})
		})
		.count()
}

fn is_remove_patch(patch: &ClausewitzPatch) -> bool {
	matches!(
		patch,
		ClausewitzPatch::RemoveNode { .. }
			| ClausewitzPatch::RemoveListItem { .. }
			| ClausewitzPatch::RemoveBlockItem { .. }
	)
}

pub(super) fn apply_dep_misuse_remove_counts(
	findings: &mut [DepMisuseFinding],
	counts: Vec<DepMisuseRemoveCount>,
) {
	for count in counts {
		if let Some(finding) = findings.iter_mut().find(|finding| {
			finding.mod_id == count.mod_id && finding.suspicious_dep_id == count.dep_id
		}) {
			finding.evidence.false_remove_count = finding
				.evidence
				.false_remove_count
				.saturating_add(count.count);
		}
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use foch_language::analyzer::content_family::CwtType;
	use foch_language::analyzer::parser::parse_clausewitz_content;
	use foch_merge_kernel::{
		ChildCardinality, NormalizedTree, RevisionDelta, RevisionId, TreeMatcher, TreeNode,
	};

	use super::*;
	use crate::merge::model::{
		SemanticDeltaPartition, SemanticMergeSource, SemanticPartitionId, SemanticSourceDelta,
	};

	#[test]
	fn semantic_remove_count_counts_only_deleted_statement_roots() {
		let base = normalized(vec![
			assignment("removed", "yes"),
			assignment("changed", "old"),
			item("removed-item"),
		]);
		let revision = normalized(vec![
			assignment("changed", "new"),
			assignment("inserted", "yes"),
		]);
		let matching = TreeMatcher::default().match_trees(&base, &revision);
		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);
		assert!(
			delta
				.operations
				.iter()
				.any(|operation| matches!(operation, DeltaOperation::Update { .. }))
		);
		assert!(
			delta
				.operations
				.iter()
				.any(|operation| matches!(operation, DeltaOperation::Insert { .. }))
		);
		let source_delta = SemanticSourceDelta {
			source: SemanticMergeSource {
				source_id: "mod-a".to_string(),
				precedence: 10,
			},
			partitions: vec![SemanticDeltaPartition {
				partition: SemanticPartitionId::File,
				base_tree: base,
				revision_tree: revision,
				delta,
			}],
		};

		assert_eq!(semantic_remove_count(&source_delta), 2);
	}

	#[test]
	fn semantic_stale_detection_compares_remove_targets_with_original_vanilla() {
		let present = source_delta(
			normalized(vec![assignment("present", "yes")]),
			normalized(vec![]),
		);
		let absent = source_delta(
			normalized(vec![assignment("absent", "yes")]),
			normalized(vec![]),
		);
		let vanilla = parsed_vanilla("present = yes\n");
		let versions = HashMap::from([("mod-a".to_string(), "1.0.0".to_string())]);

		let present_findings = collect_semantic_stale_vanilla_targets(
			"common/test.txt",
			&[present],
			Some(&vanilla),
			&MergePolicies::default(),
			&versions,
		)
		.expect("inspect present semantic target");
		let absent_findings = collect_semantic_stale_vanilla_targets(
			"common/test.txt",
			&[absent],
			Some(&vanilla),
			&MergePolicies::default(),
			&versions,
		)
		.expect("inspect absent semantic target");

		assert!(present_findings.is_empty());
		assert_eq!(absent_findings.len(), 1);
		assert_eq!(absent_findings[0].patch_kind, "Delete");
		assert_eq!(absent_findings[0].target_key.as_deref(), Some("absent"));
	}

	fn source_delta(base: NormalizedTree, revision: NormalizedTree) -> SemanticSourceDelta {
		let matching = TreeMatcher::default().match_trees(&base, &revision);
		let delta = RevisionDelta::between(&base, RevisionId::LEFT, &revision, &matching);
		SemanticSourceDelta {
			source: SemanticMergeSource {
				source_id: "mod-a".to_string(),
				precedence: 10,
			},
			partitions: vec![SemanticDeltaPartition {
				partition: SemanticPartitionId::File,
				base_tree: base,
				revision_tree: revision,
				delta,
			}],
		}
	}

	fn parsed_vanilla(source: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/test.txt");
		let parsed = parse_clausewitz_content(path.clone(), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		ParsedScriptFile {
			mod_id: "__game__".to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("other"),
			module_name: "test".to_string(),
			ast: parsed.ast,
			source: source.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn normalized(children: Vec<TreeNode>) -> NormalizedTree {
		NormalizedTree::from_root(TreeNode::branch("clausewitz.file", children))
			.expect("valid normalized fixture")
	}

	fn assignment(key: &str, value: &str) -> TreeNode {
		TreeNode::branch(
			format!("clausewitz.assignment:{key}"),
			vec![TreeNode::leaf("clausewitz.scalar.identifier", value)],
		)
		.with_parent_scoped_anchor("clausewitz.assignment.key", key)
		.with_child_cardinality(ChildCardinality::ExactlyOne)
	}

	fn item(value: &str) -> TreeNode {
		TreeNode::branch(
			"clausewitz.item",
			vec![TreeNode::leaf("clausewitz.scalar.identifier", value)],
		)
		.with_parent_scoped_anchor("clausewitz.item.scalar", value)
		.with_child_cardinality(ChildCardinality::ExactlyOne)
	}
}
