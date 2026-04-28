use super::error::MergeError;
use super::normalize::normalize_defines_file;
use super::plan::build_merge_plan_from_workspace;
use crate::request::{CheckRequest, MergePlanOptions};
use crate::workspace::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveErrorKind, resolve_workspace,
};
use foch_core::model::{MergePlanContributor, MergePlanEntry, MergePlanResult, MergePlanStrategy};
use foch_language::analyzer::content_family::{
	BlockMergePolicy, ConflictPolicy, ContentFamilyDescriptor, GameProfile, ListMergePolicy,
	MergeKeySource, MergePolicies, ScalarMergePolicy,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, SpanRange};
use foch_language::analyzer::semantic_index::{
	ParsedScriptFile, is_decision_container_key, parse_script_file,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeIrResult {
	pub game: String,
	pub playset_name: String,
	pub include_game_base: bool,
	pub copy_through_files: Vec<MergeIrCopyThroughFile>,
	pub structural_files: Vec<MergeIrStructuralFile>,
	pub deferred_structural_paths: Vec<MergeIrDeferredStructuralPath>,
	pub fatal_errors: Vec<String>,
	pub merge_warnings: Vec<String>,
}

impl MergeIrResult {
	pub fn has_fatal_errors(&self) -> bool {
		!self.fatal_errors.is_empty()
	}

	pub fn push_fatal_error(&mut self, message: impl Into<String>) {
		self.fatal_errors.push(message.into());
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrCopyThroughFile {
	pub target_path: String,
	pub winner: MergePlanContributor,
	pub source_mod_order: Vec<MergePlanContributor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrStructuralFile {
	pub target_path: String,
	pub family_id: String,
	pub merge_key_source: MergeKeySource,
	pub nodes: Vec<MergeIrNode>,
	/// Top-level statements that don't match the merge-key pattern but should
	/// be preserved in output (e.g. `namespace = xxx` in event files).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub passthrough_statements: Vec<AstStatement>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrNode {
	pub target_path: String,
	pub merge_key: String,
	pub path_segments: Vec<String>,
	pub statement_key: String,
	pub container_key: Option<String>,
	pub winner: MergePlanContributor,
	pub overridden_contributors: Vec<MergePlanContributor>,
	pub source_mod_order: Vec<MergePlanContributor>,
	pub merged_statement: AstStatement,
	pub source_fragments: Vec<MergeIrSourceFragment>,
	/// Set when renamed due to conflict — holds the original merge key.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub original_merge_key: Option<String>,
	/// Whether this node was renamed as part of conflict resolution.
	#[serde(default)]
	pub conflict_rename: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrSourceFragment {
	pub contributor: MergePlanContributor,
	pub statement_span: SpanRange,
	pub statement_key: String,
	pub container_key: Option<String>,
	pub statement: AstStatement,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeIrDeferredStructuralPath {
	pub target_path: String,
	pub reason: String,
	pub contributors: Vec<MergePlanContributor>,
}

#[derive(Clone, Debug)]
struct ExtractedFragment {
	merge_key: String,
	path_segments: Vec<String>,
	statement_key: String,
	container_key: Option<String>,
	statement: AstStatement,
	statement_span: SpanRange,
}

#[derive(Clone, Debug)]
struct NodeAccumulator {
	target_path: String,
	merge_key: String,
	path_segments: Vec<String>,
	statement_key: String,
	container_key: Option<String>,
	winner: MergePlanContributor,
	source_mod_order: Vec<MergePlanContributor>,
	merged_statement: AstStatement,
	source_fragments: Vec<MergeIrSourceFragment>,
	/// Set during conflict rename to track the original merge key.
	original_merge_key: Option<String>,
}

pub fn run_merge_ir(request: CheckRequest) -> MergeIrResult {
	run_merge_ir_with_options(request, MergePlanOptions::default())
}

pub fn run_merge_ir_with_options(
	request: CheckRequest,
	options: MergePlanOptions,
) -> MergeIrResult {
	let workspace = match resolve_workspace(&request, options.include_game_base) {
		Ok(workspace) => workspace,
		Err(err) => {
			let mut result = MergeIrResult {
				include_game_base: options.include_game_base,
				..MergeIrResult::default()
			};
			if err.kind == WorkspaceResolveErrorKind::PlaylistFormat {
				result.push_fatal_error("无法解析 Playset JSON");
			} else {
				result.push_fatal_error(err.message);
			}
			return result;
		}
	};

	let plan = build_merge_plan_from_workspace(&workspace, options.include_game_base);
	build_merge_ir_from_workspace_and_plan(&workspace, &plan)
}

pub(crate) fn build_merge_ir_from_workspace_and_plan(
	workspace: &ResolvedWorkspace,
	plan: &MergePlanResult,
) -> MergeIrResult {
	let mut result = MergeIrResult {
		game: plan.game.clone(),
		playset_name: plan.playset_name.clone(),
		include_game_base: plan.include_game_base,
		..MergeIrResult::default()
	};

	if !plan.fatal_errors.is_empty() {
		result.fatal_errors = plan.fatal_errors.clone();
		return result;
	}

	for path_entry in &plan.paths {
		match path_entry.strategy {
			MergePlanStrategy::CopyThrough => append_copy_through_file(&mut result, path_entry),
			MergePlanStrategy::StructuralMerge => {
				append_structural_file(&mut result, workspace, path_entry)
			}
			MergePlanStrategy::LastWriterOverlay | MergePlanStrategy::ManualConflict => {}
		}
	}

	result
		.copy_through_files
		.sort_by(|left, right| left.target_path.cmp(&right.target_path));
	result
		.structural_files
		.sort_by(|left, right| left.target_path.cmp(&right.target_path));
	result
		.deferred_structural_paths
		.sort_by(|left, right| left.target_path.cmp(&right.target_path));
	result
}

fn append_copy_through_file(result: &mut MergeIrResult, path_entry: &MergePlanEntry) {
	let Some(winner) = path_entry.winner.clone() else {
		result.push_fatal_error(format!(
			"copy-through path {} is missing a winner contributor",
			path_entry.path
		));
		return;
	};

	result.copy_through_files.push(MergeIrCopyThroughFile {
		target_path: path_entry.path.clone(),
		winner,
		source_mod_order: path_entry.contributors.clone(),
	});
}

fn append_structural_file(
	result: &mut MergeIrResult,
	workspace: &ResolvedWorkspace,
	path_entry: &MergePlanEntry,
) {
	let contributors = match workspace.file_inventory.get(&path_entry.path) {
		Some(contributors) => contributors,
		None => {
			result.push_fatal_error(format!(
				"merge IR could not locate contributors for {}",
				path_entry.path
			));
			return;
		}
	};

	let profile = eu4_profile();
	let descriptor = profile.classify_content_family(Path::new(&path_entry.path));
	let merge_key_source = descriptor.and_then(|d| d.merge_key_source);
	match (descriptor, merge_key_source) {
		(Some(descriptor), Some(merge_key_source)) => {
			match build_structural_file(
				&path_entry.path,
				descriptor,
				merge_key_source,
				contributors,
			) {
				Ok((file, warnings)) => {
					result.structural_files.push(file);
					result.merge_warnings.extend(warnings);
				}
				Err(err) => result.push_fatal_error(err.to_string()),
			}
		}
		_ => result.push_fatal_error(format!(
			"merge IR has no merge_key_source for {}",
			path_entry.path
		)),
	}
}

fn build_structural_file(
	target_path: &str,
	descriptor: &ContentFamilyDescriptor,
	merge_key_source: MergeKeySource,
	contributors: &[ResolvedFileContributor],
) -> Result<(MergeIrStructuralFile, Vec<String>), MergeError> {
	let mut node_vec: Vec<NodeAccumulator> = Vec::new();
	let mut node_index: HashMap<String, usize> = HashMap::new();
	let mut merge_warnings: Vec<String> = Vec::new();
	let mut passthrough_acc: Vec<AstStatement> = Vec::new();

	for contributor in contributors {
		let parsed = parse_script_file(
			&contributor.mod_id,
			&contributor.root_path,
			&contributor.absolute_path,
		)
		.ok_or_else(|| MergeError::Parse {
			path: Some(target_path.to_string()),
			message: format!(
				"merge IR could not parse {} from {}",
				target_path,
				contributor.absolute_path.display()
			),
		})?;
		if !parsed.parse_issues.is_empty() {
			return Err(MergeError::Parse {
				path: Some(target_path.to_string()),
				message: format!(
					"merge IR requires parse-clean contributors for {} but {} still has parse issues",
					target_path, contributor.mod_id
				),
			});
		}

		let (fragments, passthrough) = extract_fragments(&parsed, merge_key_source)?;

		// Accumulate passthrough statements (last-writer wins per key)
		for stmt in passthrough {
			if let AstStatement::Assignment { key, .. } = &stmt {
				if let Some(pos) = passthrough_acc
					.iter()
					.position(|s| matches!(s, AstStatement::Assignment { key: k, .. } if k == key))
				{
					passthrough_acc[pos] = stmt;
				} else {
					passthrough_acc.push(stmt);
				}
			}
		}

		if fragments.is_empty() {
			if passthrough_acc.is_empty() {
				return Err(MergeError::Parse {
					path: Some(target_path.to_string()),
					message: format!(
						"merge IR found no mergeable blocks in {} for {}",
						target_path, contributor.mod_id
					),
				});
			}
			continue;
		}

		let contributor_meta = to_merge_contributor(contributor);
		for fragment in fragments {
			let is_new = !node_index.contains_key(&fragment.merge_key);
			let acc_idx = if let Some(&idx) = node_index.get(&fragment.merge_key) {
				idx
			} else {
				let idx = node_vec.len();
				node_vec.push(NodeAccumulator {
					target_path: target_path.to_string(),
					merge_key: fragment.merge_key.clone(),
					path_segments: fragment.path_segments.clone(),
					statement_key: fragment.statement_key.clone(),
					container_key: fragment.container_key.clone(),
					winner: contributor_meta.clone(),
					source_mod_order: Vec::new(),
					merged_statement: fragment.statement.clone(),
					source_fragments: Vec::new(),
					original_merge_key: None,
				});
				node_index.insert(fragment.merge_key.clone(), idx);
				idx
			};
			let entry = &mut node_vec[acc_idx];

			push_unique_contributor(&mut entry.source_mod_order, &contributor_meta);
			entry.source_fragments.push(MergeIrSourceFragment {
				contributor: contributor_meta.clone(),
				statement_span: fragment.statement_span.clone(),
				statement_key: fragment.statement_key.clone(),
				container_key: fragment.container_key.clone(),
				statement: fragment.statement.clone(),
			});

			// Deep merge: fold overlay on top of current merged result.
			// Skip for the first fragment — merging a statement with itself
			// would incorrectly apply Sum/Avg policies (e.g. doubling values).
			if !is_new {
				entry.merged_statement = deep_merge(
					&entry.merged_statement,
					&fragment.statement,
					&descriptor.merge_policies,
					&contributor_meta.mod_id,
					&mut merge_warnings,
				);
			}

			// Track the highest-precedence contributor as "winner" for metadata
			if contributor_meta.precedence > entry.winner.precedence
				|| contributor_meta.precedence == entry.winner.precedence
					&& contributor_meta.source_path == entry.winner.source_path
			{
				entry.winner = contributor_meta.clone();
				entry.path_segments = fragment.path_segments.clone();
				entry.statement_key = fragment.statement_key.clone();
				entry.container_key = fragment.container_key.clone();
			}
		}
	}

	// --- Conflict detection and rename ---
	let conflict_policy = descriptor.conflict_policy;
	let node_vec = if conflict_policy == ConflictPolicy::Rename {
		let conflicting_keys: HashSet<String> = node_vec
			.iter()
			.filter_map(|acc| {
				let unique_non_base_mods: HashSet<&str> = acc
					.source_mod_order
					.iter()
					.filter(|c| !c.is_base_game)
					.map(|c| c.mod_id.as_str())
					.collect();
				if unique_non_base_mods.len() > 1 {
					Some(acc.merge_key.clone())
				} else {
					None
				}
			})
			.collect();

		let mut renamed_vec: Vec<NodeAccumulator> = Vec::new();
		for accumulator in node_vec {
			if !conflicting_keys.contains(&accumulator.merge_key) {
				renamed_vec.push(accumulator);
				continue;
			}

			// NamedContainerUnion: when both contributors define the same
			// top-level container key (e.g. `guiTypes`, `spriteTypes`) whose
			// children are homogeneous named items, merge children by their
			// identity (name field or assignment key) instead of suffix-
			// renaming the container itself.
			if merge_key_source == MergeKeySource::AssignmentKey
				&& let Some(merged_statement) =
					try_named_container_union(&accumulator.source_fragments)
			{
				let mut merged_acc = accumulator;
				merged_acc.merged_statement = merged_statement;
				renamed_vec.push(merged_acc);
				continue;
			}

			// Base game fragments keep the original key
			let base_fragments: Vec<&MergeIrSourceFragment> = accumulator
				.source_fragments
				.iter()
				.filter(|f| f.contributor.is_base_game)
				.collect();
			if !base_fragments.is_empty() {
				let base_contributor = base_fragments[0].contributor.clone();
				renamed_vec.push(NodeAccumulator {
					target_path: accumulator.target_path.clone(),
					merge_key: accumulator.merge_key.clone(),
					path_segments: accumulator.path_segments.clone(),
					statement_key: accumulator.statement_key.clone(),
					container_key: accumulator.container_key.clone(),
					winner: base_contributor.clone(),
					source_mod_order: vec![base_contributor],
					merged_statement: base_fragments.last().unwrap().statement.clone(),
					source_fragments: base_fragments.into_iter().cloned().collect(),
					original_merge_key: None,
				});
			}

			// Group non-base fragments by mod_id and create renamed nodes
			let mut per_mod: BTreeMap<String, Vec<MergeIrSourceFragment>> = BTreeMap::new();
			for fragment in &accumulator.source_fragments {
				if fragment.contributor.is_base_game {
					continue;
				}
				per_mod
					.entry(fragment.contributor.mod_id.clone())
					.or_default()
					.push(fragment.clone());
			}

			for (mod_id, fragments) in per_mod {
				let mod_suffix = sanitize_contributor_id(&mod_id);
				let renamed_key = format!("{}_{}", accumulator.merge_key, mod_suffix);
				let last_fragment = fragments.last().unwrap();
				let contributor = last_fragment.contributor.clone();
				let renamed_statement = rename_merged_statement(
					&last_fragment.statement,
					merge_key_source,
					&renamed_key,
				);
				renamed_vec.push(NodeAccumulator {
					target_path: accumulator.target_path.clone(),
					merge_key: renamed_key,
					path_segments: accumulator.path_segments.clone(),
					statement_key: last_fragment.statement_key.clone(),
					container_key: last_fragment.container_key.clone(),
					winner: contributor.clone(),
					source_mod_order: vec![contributor],
					merged_statement: renamed_statement,
					source_fragments: fragments,
					original_merge_key: Some(accumulator.merge_key.clone()),
				});
			}
		}
		renamed_vec
	} else {
		node_vec
	};
	// For MergeLeaf and LastWriter, the existing accumulation behavior is correct.

	let nodes = node_vec
		.into_iter()
		.map(|accumulator| {
			let is_renamed = accumulator.original_merge_key.is_some();
			MergeIrNode {
				target_path: accumulator.target_path,
				merge_key: accumulator.merge_key,
				path_segments: accumulator.path_segments,
				statement_key: accumulator.statement_key,
				container_key: accumulator.container_key,
				overridden_contributors: accumulator
					.source_mod_order
					.iter()
					.filter(|item| item.source_path != accumulator.winner.source_path)
					.cloned()
					.collect(),
				source_mod_order: accumulator.source_mod_order,
				winner: accumulator.winner,
				merged_statement: accumulator.merged_statement,
				source_fragments: accumulator.source_fragments,
				original_merge_key: accumulator.original_merge_key,
				conflict_rename: is_renamed,
			}
		})
		.collect();

	Ok((
		MergeIrStructuralFile {
			target_path: target_path.to_string(),
			family_id: descriptor.id.to_string(),
			merge_key_source,
			nodes,
			passthrough_statements: passthrough_acc,
		},
		merge_warnings,
	))
}

/// Recursively merge two AST blocks.
/// - Sub-blocks with same key: apply `policies.block` (Recursive → recurse, Replace → overlay wins)
/// - Scalars with same key: apply `policies.scalar` (LastWriter/Sum/Avg/Max/Min)
/// - Items (bare list entries): apply `policies.list` (Union/UnionWithRename/Replace)
/// - New keys in overlay: append
/// - Repeated keys (same key appearing multiple times) are treated as list items.
fn deep_merge(
	base: &AstStatement,
	overlay: &AstStatement,
	policies: &MergePolicies,
	overlay_mod_id: &str,
	warnings: &mut Vec<String>,
) -> AstStatement {
	// Both must be assignments with block values to recurse
	let (base_key, base_key_span, base_block, base_span) = match base {
		AstStatement::Assignment {
			key,
			key_span,
			value: AstValue::Block { items, span },
			..
		} => (key, key_span, items, span),
		_ => {
			// Detect type mismatch: base is scalar, overlay is block
			if let (
				AstStatement::Assignment {
					key,
					value: AstValue::Scalar { .. },
					..
				},
				AstStatement::Assignment {
					value: AstValue::Block { .. },
					..
				},
			) = (base, overlay)
			{
				warnings.push(format!(
					"type mismatch for key '{}': base is scalar, overlay is block from mod {}",
					key, overlay_mod_id
				));
			}
			return overlay.clone();
		}
	};
	let overlay_block = match overlay {
		AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} => items,
		_ => {
			// Detect type mismatch: base is block, overlay is scalar
			if let AstStatement::Assignment {
				value: AstValue::Scalar { .. },
				..
			} = overlay
			{
				warnings.push(format!(
					"type mismatch for key '{}': base is block, overlay is scalar from mod {}",
					base_key, overlay_mod_id
				));
			}
			return overlay.clone();
		}
	};

	let mut merged_items: Vec<AstStatement> = Vec::new();
	let mut base_by_key: HashMap<String, Vec<usize>> = HashMap::new();

	// Seed merged_items with base entries; track keyed positions for lookup.
	for (i, stmt) in base_block.iter().enumerate() {
		if let AstStatement::Assignment { key, .. } = stmt {
			base_by_key.entry(key.clone()).or_default().push(i);
		}
		merged_items.push(stmt.clone());
	}

	// When list policy is Replace, drop all non-keyed base items first so
	// overlay's list entries become the sole set.
	if policies.list == ListMergePolicy::Replace {
		merged_items.retain(|stmt| {
			matches!(
				stmt,
				AstStatement::Assignment { .. } | AstStatement::Comment { .. }
			)
		});
		// Rebuild index after removal
		base_by_key.clear();
		for (i, stmt) in merged_items.iter().enumerate() {
			if let AstStatement::Assignment { key, .. } = stmt {
				base_by_key.entry(key.clone()).or_default().push(i);
			}
		}
	}

	for stmt in overlay_block {
		match stmt {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { .. },
				..
			} => {
				let count = base_by_key.get(key).map_or(0, |v| v.len());
				if count == 1 {
					let base_idx = base_by_key[key][0];
					match policies.block {
						BlockMergePolicy::Recursive => {
							merged_items[base_idx] = deep_merge(
								&merged_items[base_idx],
								stmt,
								policies,
								overlay_mod_id,
								warnings,
							);
						}
						BlockMergePolicy::Replace => {
							merged_items[base_idx] = stmt.clone();
						}
					}
				} else if count > 1 {
					// Multiple base entries with same key — treat as list items
					let positions = base_by_key[key].clone();
					let is_dup = positions.iter().any(|&idx| merged_items[idx] == *stmt);
					match policies.list {
						ListMergePolicy::Union | ListMergePolicy::OrderedUnion => {
							if !is_dup {
								base_by_key
									.entry(key.clone())
									.or_default()
									.push(merged_items.len());
								merged_items.push(stmt.clone());
							}
						}
						ListMergePolicy::UnionWithRename => {
							if is_dup {
								merged_items.push(rename_item(stmt, overlay_mod_id));
							} else {
								merged_items.push(stmt.clone());
							}
						}
						ListMergePolicy::Replace => {
							base_by_key
								.entry(key.clone())
								.or_default()
								.push(merged_items.len());
							merged_items.push(stmt.clone());
						}
					}
				} else {
					base_by_key
						.entry(key.clone())
						.or_default()
						.push(merged_items.len());
					merged_items.push(stmt.clone());
				}
			}
			AstStatement::Assignment {
				key,
				key_span: ks,
				value: AstValue::Scalar {
					value: overlay_sv,
					span: sv_span,
				},
				span: s,
			} => {
				let count = base_by_key.get(key).map_or(0, |v| v.len());
				if count == 1 {
					let base_idx = base_by_key[key][0];
					merged_items[base_idx] = resolve_scalar_conflict(
						&merged_items[base_idx],
						overlay_sv,
						ks,
						sv_span,
						s,
						policies.scalar,
					);
				} else if count > 1 {
					// Multiple base entries with same key — treat as list items
					let positions = base_by_key[key].clone();
					let is_dup = positions.iter().any(|&idx| merged_items[idx] == *stmt);
					match policies.list {
						ListMergePolicy::Union | ListMergePolicy::OrderedUnion => {
							if !is_dup {
								base_by_key
									.entry(key.clone())
									.or_default()
									.push(merged_items.len());
								merged_items.push(stmt.clone());
							}
						}
						ListMergePolicy::UnionWithRename => {
							if is_dup {
								merged_items.push(rename_item(stmt, overlay_mod_id));
							} else {
								merged_items.push(stmt.clone());
							}
						}
						ListMergePolicy::Replace => {
							base_by_key
								.entry(key.clone())
								.or_default()
								.push(merged_items.len());
							merged_items.push(stmt.clone());
						}
					}
				} else {
					base_by_key
						.entry(key.clone())
						.or_default()
						.push(merged_items.len());
					merged_items.push(stmt.clone());
				}
			}
			AstStatement::Comment { .. } => {
				// Always append unique overlay comments regardless of list policy
				let is_dup = merged_items.iter().any(|existing| existing == stmt);
				if !is_dup {
					merged_items.push(stmt.clone());
				}
			}
			_ => {
				// Items: behaviour depends on list policy
				let is_dup = merged_items.iter().any(|existing| existing == stmt);
				match policies.list {
					ListMergePolicy::Union | ListMergePolicy::OrderedUnion => {
						if !is_dup {
							merged_items.push(stmt.clone());
						}
					}
					ListMergePolicy::UnionWithRename => {
						if is_dup {
							merged_items.push(rename_item(stmt, overlay_mod_id));
						} else {
							merged_items.push(stmt.clone());
						}
					}
					ListMergePolicy::Replace => {
						// Base non-keyed items were already removed above.
						merged_items.push(stmt.clone());
					}
				}
			}
		}
	}

	AstStatement::Assignment {
		key: base_key.clone(),
		key_span: base_key_span.clone(),
		value: AstValue::Block {
			items: merged_items,
			span: base_span.clone(),
		},
		span: base_span.clone(),
	}
}

/// Identity of a named-container child: the assignment key plus, when present,
/// the scalar value of an inner `name = "..."` field.  Two children share an
/// identity iff their assignment keys match AND their `name` fields agree
/// (or both are absent).
type ChildIdentity = (String, Option<String>);

fn child_identity(stmt: &AstStatement) -> Option<ChildIdentity> {
	let AstStatement::Assignment {
		key,
		value: AstValue::Block { items, .. },
		..
	} = stmt
	else {
		return None;
	};
	let name = scalar_assignment_value(items, "name");
	Some((key.clone(), name))
}

/// Try to merge several `key = { ... }` containers into a single container by
/// unioning their named children.  Returns `None` if the containers don't
/// look like a homogeneous named-container family — in that case the caller
/// falls back to per-mod suffix renaming.
///
/// Accepts containers whose non-comment children are all `Assignment` blocks.
/// Children with conflicting content but matching identity are kept and
/// renamed using the existing per-mod suffix scheme so the EU4 engine still
/// loads each one under a unique handle.
fn try_named_container_union(fragments: &[MergeIrSourceFragment]) -> Option<AstStatement> {
	if fragments.is_empty() {
		return None;
	}

	// All fragments must be `key = { items... }` assignments and share a key.
	let mut container_key: Option<String> = None;
	let mut container_key_span: Option<SpanRange> = None;
	let mut container_block_span: Option<SpanRange> = None;
	let mut container_stmt_span: Option<SpanRange> = None;
	for fragment in fragments {
		let AstStatement::Assignment {
			key,
			key_span,
			value: AstValue::Block { span, .. },
			span: stmt_span,
		} = &fragment.statement
		else {
			return None;
		};
		match &container_key {
			Some(existing) if existing != key => return None,
			Some(_) => {}
			None => {
				container_key = Some(key.clone());
				container_key_span = Some(key_span.clone());
				container_block_span = Some(span.clone());
				container_stmt_span = Some(stmt_span.clone());
			}
		}
	}

	// Every non-comment child of every fragment must be a block assignment
	// with a derivable identity.  Anything else (scalar assignments, bare
	// items) means the container is heterogeneous and we bail out.
	for fragment in fragments {
		let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = &fragment.statement
		else {
			return None;
		};
		for item in items {
			match item {
				AstStatement::Comment { .. } => {}
				other => {
					child_identity(other)?;
				}
			}
		}
	}

	// Walk fragments in their existing precedence order.  First contributor's
	// children seed the merged list; later contributors dedupe by identity
	// and suffix-rename on conflict.
	let mut merged_items: Vec<AstStatement> = Vec::new();
	let mut by_identity: HashMap<ChildIdentity, usize> = HashMap::new();

	for fragment in fragments {
		let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = &fragment.statement
		else {
			continue;
		};
		let mod_id = &fragment.contributor.mod_id;
		for item in items {
			match item {
				AstStatement::Comment { .. } => {
					if !merged_items.iter().any(|existing| existing == item) {
						merged_items.push(item.clone());
					}
				}
				stmt => {
					let identity = child_identity(stmt)?;
					if let Some(&idx) = by_identity.get(&identity) {
						if &merged_items[idx] == stmt {
							continue;
						}
						let renamed = rename_named_child(stmt, &identity, mod_id);
						merged_items.push(renamed);
					} else {
						by_identity.insert(identity, merged_items.len());
						merged_items.push(stmt.clone());
					}
				}
			}
		}
	}

	let key = container_key?;
	let key_span = container_key_span?;
	let block_span = container_block_span?;
	let stmt_span = container_stmt_span?;
	Some(AstStatement::Assignment {
		key,
		key_span,
		value: AstValue::Block {
			items: merged_items,
			span: block_span,
		},
		span: stmt_span,
	})
}

/// Rename a named-container child for conflict resolution.  When the child
/// carries a `name = "..."` field the rename targets that value; otherwise
/// the child's assignment key itself is suffixed.
fn rename_named_child(stmt: &AstStatement, identity: &ChildIdentity, mod_id: &str) -> AstStatement {
	let suffix = sanitize_contributor_id(mod_id);
	let (_key, name) = identity;
	if let Some(name_value) = name {
		let new_name = format!("{name_value}_{suffix}");
		rename_named_child_field(stmt, "name", &new_name)
	} else {
		match stmt {
			AstStatement::Assignment { key, .. } => {
				let renamed_key = format!("{key}_{suffix}");
				rename_statement_key(stmt, &renamed_key)
			}
			other => other.clone(),
		}
	}
}

/// Like `rename_inner_field`, but preserves the original scalar kind so a
/// quoted name stays quoted in the emitted output.
fn rename_named_child_field(
	statement: &AstStatement,
	field: &str,
	new_value: &str,
) -> AstStatement {
	let AstStatement::Assignment {
		key,
		key_span,
		value: AstValue::Block { items, span },
		span: stmt_span,
	} = statement
	else {
		return statement.clone();
	};
	let new_items: Vec<AstStatement> = items
		.iter()
		.map(|item| match item {
			AstStatement::Assignment {
				key: k,
				key_span: ks,
				value: AstValue::Scalar { value, span: vs },
				span: s,
			} if k == field => {
				let renamed = match value {
					ScalarValue::String(_) => ScalarValue::String(new_value.to_string()),
					_ => ScalarValue::Identifier(new_value.to_string()),
				};
				AstStatement::Assignment {
					key: k.clone(),
					key_span: ks.clone(),
					value: AstValue::Scalar {
						value: renamed,
						span: vs.clone(),
					},
					span: s.clone(),
				}
			}
			other => other.clone(),
		})
		.collect();
	AstStatement::Assignment {
		key: key.clone(),
		key_span: key_span.clone(),
		value: AstValue::Block {
			items: new_items,
			span: span.clone(),
		},
		span: stmt_span.clone(),
	}
}

/// Produce a renamed copy of a list item by appending `_{mod_suffix}` to its
/// scalar value text.  Non-scalar items are returned unchanged.
fn rename_item(stmt: &AstStatement, mod_id: &str) -> AstStatement {
	match stmt {
		AstStatement::Item {
			value: AstValue::Scalar { value, span },
			span: item_span,
		} => {
			let suffix = sanitize_contributor_id(mod_id);
			let renamed = match value {
				ScalarValue::Identifier(s) => ScalarValue::Identifier(format!("{s}_{suffix}")),
				ScalarValue::String(s) => ScalarValue::String(format!("{s}_{suffix}")),
				ScalarValue::Number(s) => ScalarValue::Identifier(format!("{s}_{suffix}")),
				ScalarValue::Bool(b) => {
					let text = if *b { "yes" } else { "no" };
					ScalarValue::Identifier(format!("{text}_{suffix}"))
				}
			};
			AstStatement::Item {
				value: AstValue::Scalar {
					value: renamed,
					span: span.clone(),
				},
				span: item_span.clone(),
			}
		}
		_ => stmt.clone(),
	}
}

/// Apply `ScalarMergePolicy` to a same-key scalar conflict.
/// If the policy is non-trivial and both values are numeric, compute the result;
/// otherwise fall back to last-writer (overlay wins).
fn resolve_scalar_conflict(
	base_stmt: &AstStatement,
	overlay_sv: &ScalarValue,
	overlay_ks: &SpanRange,
	overlay_sv_span: &SpanRange,
	overlay_span: &SpanRange,
	policy: ScalarMergePolicy,
) -> AstStatement {
	// Extract base scalar; if base isn't a scalar assignment, overlay wins outright.
	let (base_key, base_sv) = match base_stmt {
		AstStatement::Assignment {
			key,
			value: AstValue::Scalar { value, .. },
			..
		} => (key, value),
		_ => {
			return AstStatement::Assignment {
				key: match base_stmt {
					AstStatement::Assignment { key, .. } => key.clone(),
					_ => String::new(),
				},
				key_span: overlay_ks.clone(),
				value: AstValue::Scalar {
					value: overlay_sv.clone(),
					span: overlay_sv_span.clone(),
				},
				span: overlay_span.clone(),
			};
		}
	};

	if policy == ScalarMergePolicy::LastWriter {
		return AstStatement::Assignment {
			key: base_key.clone(),
			key_span: overlay_ks.clone(),
			value: AstValue::Scalar {
				value: overlay_sv.clone(),
				span: overlay_sv_span.clone(),
			},
			span: overlay_span.clone(),
		};
	}

	// Try numeric merge
	let base_num = match base_sv {
		ScalarValue::Number(s) => s.parse::<f64>().ok(),
		_ => None,
	};
	let overlay_num = match overlay_sv {
		ScalarValue::Number(s) => s.parse::<f64>().ok(),
		_ => None,
	};

	let merged_value = match (base_num, overlay_num) {
		(Some(b), Some(o)) => {
			let result = match policy {
				ScalarMergePolicy::Sum => b + o,
				ScalarMergePolicy::Avg => (b + o) / 2.0,
				ScalarMergePolicy::Max => b.max(o),
				ScalarMergePolicy::Min => b.min(o),
				ScalarMergePolicy::LastWriter => o,
			};
			ScalarValue::Number(format_merged_number(result))
		}
		// Non-numeric: fall back to last-writer
		_ => overlay_sv.clone(),
	};

	AstStatement::Assignment {
		key: base_key.clone(),
		key_span: overlay_ks.clone(),
		value: AstValue::Scalar {
			value: merged_value,
			span: overlay_sv_span.clone(),
		},
		span: overlay_span.clone(),
	}
}

/// Format a merged numeric result, preferring integer notation when the value
/// is exact (no fractional part).
fn format_merged_number(v: f64) -> String {
	if v.fract() == 0.0 && v.abs() < i64::MAX as f64 {
		format!("{}", v as i64)
	} else {
		format!("{v}")
	}
}

fn extract_fragments(
	parsed: &ParsedScriptFile,
	merge_key_source: MergeKeySource,
) -> Result<(Vec<ExtractedFragment>, Vec<AstStatement>), MergeError> {
	match merge_key_source {
		MergeKeySource::AssignmentKey => Ok((extract_assignment_fragments(parsed), Vec::new())),
		MergeKeySource::FieldValue(field) => extract_inner_field_fragments(parsed, field),
		MergeKeySource::ContainerChildKey => Ok((extract_decision_fragments(parsed), Vec::new())),
		MergeKeySource::LeafPath => Ok((extract_defines_fragments(parsed)?, Vec::new())),
	}
}

fn extract_inner_field_fragments(
	parsed: &ParsedScriptFile,
	field: &str,
) -> Result<(Vec<ExtractedFragment>, Vec<AstStatement>), MergeError> {
	let mut fragments = Vec::new();
	let mut passthrough = Vec::new();

	for statement in &parsed.ast.statements {
		let AstStatement::Assignment {
			key, value, span, ..
		} = statement
		else {
			continue;
		};
		match value {
			AstValue::Block { items, .. } => {
				let Some(merge_key) = scalar_assignment_value(items, field) else {
					return Err(MergeError::Parse {
						path: Some(parsed.relative_path.display().to_string()),
						message: format!(
							"merge IR requires {} keys in {} but found a {} block without {}",
							field,
							parsed.relative_path.display(),
							key,
							field
						),
					});
				};
				fragments.push(ExtractedFragment {
					merge_key,
					path_segments: Vec::new(),
					statement_key: key.clone(),
					container_key: None,
					statement: statement.clone(),
					statement_span: span.clone(),
				});
			}
			AstValue::Scalar { .. } => {
				// Preserve non-block top-level assignments (e.g. namespace = xxx)
				passthrough.push(statement.clone());
			}
		}
	}

	Ok((fragments, passthrough))
}

fn extract_decision_fragments(parsed: &ParsedScriptFile) -> Vec<ExtractedFragment> {
	let mut fragments = Vec::new();

	for statement in &parsed.ast.statements {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		if !is_decision_container_key(key) {
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		for item in items {
			let AstStatement::Assignment {
				key: decision_key,
				value: AstValue::Block { .. },
				span,
				..
			} = item
			else {
				continue;
			};
			fragments.push(ExtractedFragment {
				merge_key: decision_key.clone(),
				path_segments: Vec::new(),
				statement_key: decision_key.clone(),
				container_key: Some(key.clone()),
				statement: item.clone(),
				statement_span: span.clone(),
			});
		}
	}

	fragments
}

fn extract_assignment_fragments(parsed: &ParsedScriptFile) -> Vec<ExtractedFragment> {
	let mut fragments = Vec::new();

	for statement in &parsed.ast.statements {
		let AstStatement::Assignment {
			key, value, span, ..
		} = statement
		else {
			continue;
		};
		let AstValue::Block { .. } = value else {
			continue;
		};
		fragments.push(ExtractedFragment {
			merge_key: key.clone(),
			path_segments: Vec::new(),
			statement_key: key.clone(),
			container_key: None,
			statement: statement.clone(),
			statement_span: span.clone(),
		});
	}

	fragments
}

fn extract_defines_fragments(
	parsed: &ParsedScriptFile,
) -> Result<Vec<ExtractedFragment>, MergeError> {
	Ok(normalize_defines_file(parsed)?
		.into_iter()
		.map(|fragment| ExtractedFragment {
			merge_key: fragment.merge_key,
			path_segments: fragment.path_segments,
			statement_key: fragment.statement_key,
			container_key: None,
			statement: fragment.statement,
			statement_span: fragment.statement_span,
		})
		.collect())
}

fn scalar_assignment_value(items: &[AstStatement], expected_key: &str) -> Option<String> {
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != expected_key {
			continue;
		}
		if let AstValue::Scalar { value, .. } = value {
			return Some(value.as_text());
		}
	}
	None
}

/// Rename a statement to reflect a conflict-renamed merge key.
/// For `AssignmentKey` / `ContainerChildKey`, renames the top-level key.
/// For `FieldValue(field)`, updates the inner field value inside the block.
fn rename_merged_statement(
	statement: &AstStatement,
	merge_key_source: MergeKeySource,
	new_key: &str,
) -> AstStatement {
	match merge_key_source {
		MergeKeySource::AssignmentKey | MergeKeySource::ContainerChildKey => {
			rename_statement_key(statement, new_key)
		}
		MergeKeySource::FieldValue(field) => rename_inner_field(statement, field, new_key),
		MergeKeySource::LeafPath => statement.clone(),
	}
}

fn rename_statement_key(statement: &AstStatement, new_key: &str) -> AstStatement {
	match statement {
		AstStatement::Assignment {
			key_span,
			value,
			span,
			..
		} => AstStatement::Assignment {
			key: new_key.to_string(),
			key_span: key_span.clone(),
			value: value.clone(),
			span: span.clone(),
		},
		other => other.clone(),
	}
}

fn rename_inner_field(statement: &AstStatement, field: &str, new_value: &str) -> AstStatement {
	match statement {
		AstStatement::Assignment {
			key,
			key_span,
			value: AstValue::Block { items, span },
			span: stmt_span,
		} => {
			let new_items: Vec<AstStatement> = items
				.iter()
				.map(|item| match item {
					AstStatement::Assignment {
						key: k,
						key_span: ks,
						value: AstValue::Scalar { value: _, span: vs },
						span: s,
					} if k == field => AstStatement::Assignment {
						key: k.clone(),
						key_span: ks.clone(),
						value: AstValue::Scalar {
							value: ScalarValue::Identifier(new_value.to_string()),
							span: vs.clone(),
						},
						span: s.clone(),
					},
					other => other.clone(),
				})
				.collect();
			AstStatement::Assignment {
				key: key.clone(),
				key_span: key_span.clone(),
				value: AstValue::Block {
					items: new_items,
					span: span.clone(),
				},
				span: stmt_span.clone(),
			}
		}
		other => other.clone(),
	}
}

fn push_unique_contributor(
	contributors: &mut Vec<MergePlanContributor>,
	contributor: &MergePlanContributor,
) {
	if contributors
		.last()
		.is_some_and(|item| item.source_path == contributor.source_path)
	{
		return;
	}
	contributors.push(contributor.clone());
}

fn to_merge_contributor(contributor: &ResolvedFileContributor) -> MergePlanContributor {
	MergePlanContributor {
		mod_id: contributor.mod_id.clone(),
		source_path: contributor
			.absolute_path
			.to_string_lossy()
			.replace('\\', "/"),
		precedence: contributor.precedence,
		is_base_game: contributor.is_base_game,
	}
}

/// Sanitize a mod identifier for use as a merge-key suffix.
/// Filters to ASCII alphanumeric and underscores, then lowercases.
fn sanitize_contributor_id(mod_id: &str) -> String {
	mod_id
		.to_lowercase()
		.chars()
		.filter(|c| c.is_ascii_alphanumeric() || *c == '_')
		.collect()
}

#[cfg(test)]
mod tests {
	use super::run_merge_ir_with_options;
	use crate::config::Config;
	use crate::request::{CheckRequest, MergePlanOptions};
	use foch_language::analyzer::content_family::MergeKeySource;
	use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, Span, SpanRange};
	use serde_json::json;
	use std::fs;
	use std::path::Path;
	use tempfile::TempDir;

	fn write_playlist(path: &Path, mods: serde_json::Value) {
		let playlist = json!({
			"game": "eu4",
			"name": "merge-ir-playset",
			"mods": mods,
		});
		fs::write(
			path,
			serde_json::to_string_pretty(&playlist).expect("serialize playlist"),
		)
		.expect("write playlist");
	}

	fn write_descriptor(mod_root: &Path, name: &str) {
		fs::create_dir_all(mod_root).expect("create mod root");
		fs::write(
			mod_root.join("descriptor.mod"),
			format!("name=\"{name}\"\nversion=\"1.0.0\"\n"),
		)
		.expect("write descriptor");
	}

	fn write_script_file(mod_root: &Path, relative: &str, content: &str) {
		let script_path = mod_root.join(relative);
		if let Some(parent) = script_path.parent() {
			fs::create_dir_all(parent).expect("create script parent");
		}
		fs::write(script_path, content).expect("write script file");
	}

	fn request_for(playlist_path: &Path) -> CheckRequest {
		let game_root = playlist_path
			.parent()
			.expect("playlist parent")
			.join("eu4-game");
		fs::create_dir_all(&game_root).expect("create default game root");
		let mut game_path = std::collections::HashMap::new();
		game_path.insert("eu4".to_string(), game_root);
		CheckRequest {
			playset_path: playlist_path.to_path_buf(),
			config: Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		}
	}

	fn run_merge_ir_no_base(request: CheckRequest) -> super::MergeIrResult {
		run_merge_ir_with_options(
			request,
			MergePlanOptions {
				include_game_base: false,
			},
		)
	}

	#[test]
	fn copy_through_single_contributor_file_is_preserved() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_root = temp.path().join("9401");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9401"}
			]),
		);
		write_descriptor(&mod_root, "mod-a");
		write_script_file(
			&mod_root,
			"events/a.txt",
			"namespace = test\ncountry_event = { id = test.1 }\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		assert_eq!(result.copy_through_files.len(), 1);
		let file = &result.copy_through_files[0];
		assert_eq!(file.target_path, "events/a.txt");
		assert_eq!(file.winner.mod_id, "9401");
		assert_eq!(file.source_mod_order.len(), 1);
	}

	#[test]
	fn event_ir_uses_block_id_as_merge_key_and_tracks_overrides() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9501");
		let mod_b = temp.path().join("9502");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9501"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9502"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"events/shared.txt",
			"namespace = test\ncountry_event = {\n\tid = test.1\n\ttitle = title_a\n}\ncountry_event = {\n\tid = test.2\n\ttitle = title_b\n}\n",
		);
		write_script_file(
			&mod_b,
			"events/shared.txt",
			"namespace = test\ncountry_event = {\n\tid = test.1\n\ttitle = title_override\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		assert_eq!(result.structural_files.len(), 1);
		let file = &result.structural_files[0];
		assert_eq!(file.merge_key_source, MergeKeySource::FieldValue("id"));
		// Conflict rename splits test.1 into per-mod nodes
		assert_eq!(file.nodes.len(), 3);
		let renamed_a = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test.1_9501")
			.expect("renamed event for mod A");
		assert_eq!(renamed_a.winner.mod_id, "9501");
		assert!(renamed_a.conflict_rename);
		assert_eq!(renamed_a.original_merge_key.as_deref(), Some("test.1"));
		let renamed_b = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test.1_9502")
			.expect("renamed event for mod B");
		assert_eq!(renamed_b.winner.mod_id, "9502");
		assert!(renamed_b.conflict_rename);
		let unique = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test.2")
			.expect("unique event");
		assert!(!unique.conflict_rename);
	}

	#[test]
	fn decision_ir_merges_nested_entries_by_decision_key() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9601");
		let mod_b = temp.path().join("9602");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9601"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9602"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"decisions/decisions.txt",
			"country_decisions = {\n\ttest_decision = {\n\t\teffect = { log = a }\n\t}\n\tunique_decision = {\n\t\teffect = { log = b }\n\t}\n}\n",
		);
		write_script_file(
			&mod_b,
			"decisions/decisions.txt",
			"country_decisions = {\n\ttest_decision = {\n\t\teffect = { log = override }\n\t}\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		let file = &result.structural_files[0];
		assert_eq!(file.merge_key_source, MergeKeySource::ContainerChildKey);
		// Conflict rename splits test_decision into per-mod nodes
		assert_eq!(file.nodes.len(), 3);
		let renamed_a = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test_decision_9601")
			.expect("renamed decision for mod A");
		assert_eq!(
			renamed_a.container_key.as_deref(),
			Some("country_decisions")
		);
		assert_eq!(renamed_a.winner.mod_id, "9601");
		assert!(renamed_a.conflict_rename);
		let renamed_b = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "test_decision_9602")
			.expect("renamed decision for mod B");
		assert_eq!(renamed_b.winner.mod_id, "9602");
		assert!(renamed_b.conflict_rename);
	}

	#[test]
	fn scripted_effect_ir_merges_top_level_assignment_keys() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9701");
		let mod_b = temp.path().join("9702");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9701"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9702"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"common/scripted_effects/effects.txt",
			"shared_effect = {\n\tlog = a\n}\nunique_effect = {\n\tlog = only_a\n}\n",
		);
		write_script_file(
			&mod_b,
			"common/scripted_effects/effects.txt",
			"shared_effect = {\n\tlog = b\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		let file = &result.structural_files[0];
		assert_eq!(file.merge_key_source, MergeKeySource::AssignmentKey);
		// Conflict rename splits shared_effect into per-mod nodes
		assert_eq!(file.nodes.len(), 3);
		let renamed_a = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "shared_effect_9701")
			.expect("renamed effect for mod A");
		assert_eq!(renamed_a.winner.mod_id, "9701");
		assert!(renamed_a.conflict_rename);
		let renamed_b = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "shared_effect_9702")
			.expect("renamed effect for mod B");
		assert_eq!(renamed_b.winner.mod_id, "9702");
		assert!(renamed_b.conflict_rename);
		let unique = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "unique_effect")
			.expect("unique effect");
		assert_eq!(unique.winner.mod_id, "9701");
		assert!(unique.overridden_contributors.is_empty());
		assert!(!unique.conflict_rename);
	}

	#[test]
	fn defines_ir_merges_leaf_assignment_paths_and_preserves_segments() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9801");
		let mod_b = temp.path().join("9802");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9801"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9802"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"common/defines/test.txt",
			"NGame = {\n\tSTART_YEAR = 1444\n\tEND_YEAR = 1821\n\tNCountry = {\n\t\tMAX_IDEA_GROUPS = 8\n\t}\n}\n",
		);
		write_script_file(
			&mod_b,
			"common/defines/test.txt",
			"NGame = {\n\tSTART_YEAR = 1500\n}\nNGame = {\n\tSTART_YEAR = 1600\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		assert!(result.deferred_structural_paths.is_empty());
		assert_eq!(result.structural_files.len(), 1);
		let file = &result.structural_files[0];
		assert_eq!(file.merge_key_source, MergeKeySource::LeafPath);
		let start_year = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "NGame.START_YEAR")
			.expect("start year node");
		let end_year = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "NGame.END_YEAR")
			.expect("end year node");
		let idea_groups = file
			.nodes
			.iter()
			.find(|node| node.merge_key == "NGame.NCountry.MAX_IDEA_GROUPS")
			.expect("nested defines node");
		assert_eq!(start_year.path_segments, ["NGame", "START_YEAR"]);
		assert_eq!(end_year.path_segments, ["NGame", "END_YEAR"]);
		assert_eq!(
			idea_groups.path_segments,
			["NGame", "NCountry", "MAX_IDEA_GROUPS"]
		);
		assert_eq!(start_year.winner.mod_id, "9802");
		assert_eq!(start_year.source_mod_order.len(), 2);
		assert_eq!(start_year.source_fragments.len(), 3);
		assert_eq!(start_year.overridden_contributors.len(), 1);
		assert_eq!(start_year.overridden_contributors[0].mod_id, "9801");
		let AstStatement::Assignment {
			value: AstValue::Scalar { value, .. },
			..
		} = &start_year.merged_statement
		else {
			panic!("merged defines statement should remain a scalar assignment");
		};
		assert_eq!(value, &ScalarValue::Number("1600".to_string()));
		assert_eq!(end_year.winner.mod_id, "9801");
		assert_eq!(idea_groups.winner.mod_id, "9801");
	}

	fn corpus_playlist(name: &str) -> std::path::PathBuf {
		let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
		manifest_dir
			.join("../../tests/corpus")
			.join(name)
			.join("playlist.json")
	}

	fn request_for_corpus(playlist_path: &Path, temp: &TempDir) -> CheckRequest {
		let game_root = temp.path().join("eu4-game");
		fs::create_dir_all(&game_root).expect("create game root");
		let mut game_path = std::collections::HashMap::new();
		game_path.insert("eu4".to_string(), game_root);
		CheckRequest {
			playset_path: playlist_path.to_path_buf(),
			config: Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		}
	}

	#[test]
	fn multi_mod_corpus_produces_structural_merges() {
		let playlist_path = corpus_playlist("eu4_merge_test");
		assert!(
			playlist_path.exists(),
			"corpus playlist missing: {playlist_path:?}"
		);
		let temp = TempDir::new().expect("temp dir");
		let request = request_for_corpus(&playlist_path, &temp);
		let result = run_merge_ir_no_base(request);

		assert!(
			result.fatal_errors.is_empty(),
			"fatal errors: {:?}",
			result.fatal_errors
		);
		assert!(
			!result.structural_files.is_empty(),
			"expected structural merges but got none"
		);

		// --- scripted_effects: AssignmentKey merge (3-way) ---
		let effects_file = result
			.structural_files
			.iter()
			.find(|f| f.target_path.contains("scripted_effects"))
			.expect("scripted_effects structural file");
		assert_eq!(effects_file.merge_key_source, MergeKeySource::AssignmentKey);
		// effect_alpha contributed by mod-a and mod-b → conflict rename
		let alpha_keys: Vec<&str> = effects_file
			.nodes
			.iter()
			.filter(|n| n.merge_key.starts_with("effect_alpha"))
			.map(|n| n.merge_key.as_str())
			.collect();
		assert!(
			alpha_keys.len() >= 2,
			"effect_alpha should have conflict-renamed nodes, got: {alpha_keys:?}"
		);
		// unique keys from each mod
		assert!(
			effects_file
				.nodes
				.iter()
				.any(|n| n.merge_key == "effect_unique_a"),
			"effect_unique_a should be present"
		);
		assert!(
			effects_file
				.nodes
				.iter()
				.any(|n| n.merge_key == "effect_beta"),
			"effect_beta should be present"
		);
		assert!(
			effects_file
				.nodes
				.iter()
				.any(|n| n.merge_key == "effect_gamma"),
			"effect_gamma should be present"
		);

		// --- events: FieldValue merge (3-way, distinct ids) ---
		let events_file = result
			.structural_files
			.iter()
			.find(|f| f.target_path.contains("events"))
			.expect("events structural file");
		assert_eq!(
			events_file.merge_key_source,
			MergeKeySource::FieldValue("id")
		);
		let event_keys: Vec<&str> = events_file
			.nodes
			.iter()
			.map(|n| n.merge_key.as_str())
			.collect();
		assert!(
			event_keys.contains(&"test.1"),
			"test.1 event should be present, got: {event_keys:?}"
		);
		assert!(
			event_keys.contains(&"test.2"),
			"test.2 event should be present, got: {event_keys:?}"
		);
		assert!(
			event_keys.contains(&"test.3"),
			"test.3 event should be present, got: {event_keys:?}"
		);

		// --- decisions: ContainerChildKey merge ---
		let decisions_file = result
			.structural_files
			.iter()
			.find(|f| f.target_path.contains("decisions"))
			.expect("decisions structural file");
		assert_eq!(
			decisions_file.merge_key_source,
			MergeKeySource::ContainerChildKey
		);
		let decision_keys: Vec<&str> = decisions_file
			.nodes
			.iter()
			.map(|n| n.merge_key.as_str())
			.collect();
		assert!(
			decision_keys.contains(&"decision_a"),
			"decision_a should be present, got: {decision_keys:?}"
		);
		assert!(
			decision_keys.contains(&"decision_b"),
			"decision_b should be present, got: {decision_keys:?}"
		);

		// --- ideas: AssignmentKey merge ---
		let ideas_file = result
			.structural_files
			.iter()
			.find(|f| f.target_path.contains("ideas"))
			.expect("ideas structural file");
		assert_eq!(ideas_file.merge_key_source, MergeKeySource::AssignmentKey);
		let idea_keys: Vec<&str> = ideas_file
			.nodes
			.iter()
			.map(|n| n.merge_key.as_str())
			.collect();
		assert!(
			idea_keys.contains(&"test_idea_group_a"),
			"test_idea_group_a should be present, got: {idea_keys:?}"
		);
		assert!(
			idea_keys.contains(&"test_idea_group_b"),
			"test_idea_group_b should be present, got: {idea_keys:?}"
		);

		// Verify we have at least 4 structural files (one per family)
		assert!(
			result.structural_files.len() >= 4,
			"expected ≥4 structural files, got {}",
			result.structural_files.len()
		);
	}

	// ----------------------------------------------------------------
	// deep_merge unit tests
	// ----------------------------------------------------------------

	mod deep_merge_unit {
		use super::*;
		use foch_language::analyzer::content_family::{
			BlockMergePolicy, BooleanMergePolicy, ListMergePolicy, MergePolicies, ScalarMergePolicy,
		};

		fn dummy_span() -> SpanRange {
			SpanRange {
				start: Span {
					line: 0,
					column: 0,
					offset: 0,
				},
				end: Span {
					line: 0,
					column: 0,
					offset: 0,
				},
			}
		}

		fn scalar_num(key: &str, value: &str) -> AstStatement {
			AstStatement::Assignment {
				key: key.to_string(),
				key_span: dummy_span(),
				value: AstValue::Scalar {
					value: ScalarValue::Number(value.to_string()),
					span: dummy_span(),
				},
				span: dummy_span(),
			}
		}

		fn scalar_ident(key: &str, value: &str) -> AstStatement {
			AstStatement::Assignment {
				key: key.to_string(),
				key_span: dummy_span(),
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(value.to_string()),
					span: dummy_span(),
				},
				span: dummy_span(),
			}
		}

		fn block_stmt(key: &str, items: Vec<AstStatement>) -> AstStatement {
			AstStatement::Assignment {
				key: key.to_string(),
				key_span: dummy_span(),
				value: AstValue::Block {
					items,
					span: dummy_span(),
				},
				span: dummy_span(),
			}
		}

		fn bare_item(value: &str) -> AstStatement {
			AstStatement::Item {
				value: AstValue::Scalar {
					value: ScalarValue::Identifier(value.to_string()),
					span: dummy_span(),
				},
				span: dummy_span(),
			}
		}

		fn policies(
			scalar: ScalarMergePolicy,
			list: ListMergePolicy,
			block: BlockMergePolicy,
		) -> MergePolicies {
			MergePolicies {
				scalar,
				list,
				block,
				boolean: BooleanMergePolicy::default(),
			}
		}

		fn default_policies() -> MergePolicies {
			MergePolicies::default()
		}

		fn merged_items(result: &AstStatement) -> &[AstStatement] {
			match result {
				AstStatement::Assignment {
					value: AstValue::Block { items, .. },
					..
				} => items,
				_ => panic!("expected assignment with block value"),
			}
		}

		fn extract_scalar(stmt: &AstStatement) -> &ScalarValue {
			match stmt {
				AstStatement::Assignment {
					value: AstValue::Scalar { value, .. },
					..
				} => value,
				_ => panic!("expected scalar assignment, got {:?}", stmt),
			}
		}

		fn find_by_key<'a>(items: &'a [AstStatement], key: &str) -> &'a AstStatement {
			items
				.iter()
				.find(|s| matches!(s, AstStatement::Assignment { key: k, .. } if k == key))
				.unwrap_or_else(|| panic!("key '{}' not found in items", key))
		}

		fn call_deep_merge(
			base: &AstStatement,
			overlay: &AstStatement,
			p: &MergePolicies,
			warnings: &mut Vec<String>,
		) -> AstStatement {
			crate::merge::ir::deep_merge(base, overlay, p, "test_mod", warnings)
		}

		// ---- Scalar policies ----

		#[test]
		fn deep_merge_scalar_last_writer() {
			let base = block_stmt("root", vec![scalar_ident("color", "red")]);
			let overlay = block_stmt("root", vec![scalar_ident("color", "blue")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::LastWriter,
				ListMergePolicy::default(),
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			assert_eq!(items.len(), 1);
			assert_eq!(
				extract_scalar(&items[0]),
				&ScalarValue::Identifier("blue".to_string())
			);
			assert!(warnings.is_empty());
		}

		#[test]
		fn deep_merge_scalar_sum() {
			let base = block_stmt("root", vec![scalar_num("bonus", "10")]);
			let overlay = block_stmt("root", vec![scalar_num("bonus", "5")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::Sum,
				ListMergePolicy::default(),
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			assert_eq!(items.len(), 1);
			assert_eq!(
				extract_scalar(&items[0]),
				&ScalarValue::Number("15".to_string())
			);
		}

		#[test]
		fn deep_merge_scalar_max() {
			let base = block_stmt("root", vec![scalar_num("val", "3")]);
			let overlay = block_stmt("root", vec![scalar_num("val", "7")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::Max,
				ListMergePolicy::default(),
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			assert_eq!(
				extract_scalar(&merged_items(&result)[0]),
				&ScalarValue::Number("7".to_string())
			);
		}

		#[test]
		fn deep_merge_scalar_min() {
			let base = block_stmt("root", vec![scalar_num("val", "3")]);
			let overlay = block_stmt("root", vec![scalar_num("val", "7")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::Min,
				ListMergePolicy::default(),
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			assert_eq!(
				extract_scalar(&merged_items(&result)[0]),
				&ScalarValue::Number("3".to_string())
			);
		}

		#[test]
		fn deep_merge_scalar_avg() {
			let base = block_stmt("root", vec![scalar_num("val", "10")]);
			let overlay = block_stmt("root", vec![scalar_num("val", "20")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::Avg,
				ListMergePolicy::default(),
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			assert_eq!(
				extract_scalar(&merged_items(&result)[0]),
				&ScalarValue::Number("15".to_string())
			);
		}

		#[test]
		fn deep_merge_scalar_non_numeric_fallback() {
			let base = block_stmt("root", vec![scalar_ident("tag", "alpha")]);
			let overlay = block_stmt("root", vec![scalar_ident("tag", "beta")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::Sum,
				ListMergePolicy::default(),
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			// Non-numeric identifiers fall back to last-writer
			assert_eq!(
				extract_scalar(&merged_items(&result)[0]),
				&ScalarValue::Identifier("beta".to_string())
			);
		}

		// ---- List policies ----

		#[test]
		fn deep_merge_list_union() {
			let base = block_stmt("root", vec![bare_item("alpha"), bare_item("beta")]);
			let overlay = block_stmt("root", vec![bare_item("beta"), bare_item("gamma")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::default(),
				ListMergePolicy::Union,
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			assert_eq!(items.len(), 3); // alpha, beta, gamma — beta deduped
			assert_eq!(items[0], bare_item("alpha"));
			assert_eq!(items[1], bare_item("beta"));
			assert_eq!(items[2], bare_item("gamma"));
		}

		#[test]
		fn deep_merge_list_union_with_rename() {
			let base = block_stmt("root", vec![bare_item("alpha"), bare_item("beta")]);
			let overlay = block_stmt("root", vec![bare_item("beta"), bare_item("gamma")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::default(),
				ListMergePolicy::UnionWithRename,
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			// alpha, beta (base), beta_test_mod (renamed dup), gamma
			assert_eq!(items.len(), 4);
			assert_eq!(items[0], bare_item("alpha"));
			assert_eq!(items[1], bare_item("beta"));
			let renamed = match &items[2] {
				AstStatement::Item {
					value: AstValue::Scalar { value, .. },
					..
				} => value.clone(),
				_ => panic!("expected renamed item"),
			};
			assert_eq!(
				renamed,
				ScalarValue::Identifier("beta_test_mod".to_string())
			);
			assert_eq!(items[3], bare_item("gamma"));
		}

		#[test]
		fn deep_merge_list_replace() {
			let base = block_stmt("root", vec![bare_item("old_a"), bare_item("old_b")]);
			let overlay = block_stmt("root", vec![bare_item("new_c")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::default(),
				ListMergePolicy::Replace,
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			// Base bare items removed, only overlay items remain
			assert_eq!(items.len(), 1);
			assert_eq!(items[0], bare_item("new_c"));
		}

		#[test]
		fn deep_merge_list_ordered_union() {
			let base = block_stmt("root", vec![bare_item("first"), bare_item("second")]);
			let overlay = block_stmt("root", vec![bare_item("second"), bare_item("third")]);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::default(),
				ListMergePolicy::OrderedUnion,
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			// Base order preserved, overlay new items appended
			assert_eq!(items.len(), 3);
			assert_eq!(items[0], bare_item("first"));
			assert_eq!(items[1], bare_item("second"));
			assert_eq!(items[2], bare_item("third"));
		}

		// ---- Block policies ----

		#[test]
		fn deep_merge_block_recursive() {
			let base = block_stmt(
				"root",
				vec![block_stmt("inner", vec![scalar_ident("a", "1")])],
			);
			let overlay = block_stmt(
				"root",
				vec![block_stmt("inner", vec![scalar_ident("b", "2")])],
			);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::default(),
				ListMergePolicy::default(),
				BlockMergePolicy::Recursive,
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			assert_eq!(items.len(), 1);
			let inner_items = merged_items(&items[0]);
			assert_eq!(inner_items.len(), 2);
			assert_eq!(
				extract_scalar(find_by_key(inner_items, "a")),
				&ScalarValue::Identifier("1".to_string())
			);
			assert_eq!(
				extract_scalar(find_by_key(inner_items, "b")),
				&ScalarValue::Identifier("2".to_string())
			);
		}

		#[test]
		fn deep_merge_block_replace() {
			let base = block_stmt(
				"root",
				vec![block_stmt("inner", vec![scalar_ident("a", "1")])],
			);
			let overlay = block_stmt(
				"root",
				vec![block_stmt("inner", vec![scalar_ident("b", "2")])],
			);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::default(),
				ListMergePolicy::default(),
				BlockMergePolicy::Replace,
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			assert_eq!(items.len(), 1);
			let inner_items = merged_items(&items[0]);
			// Overlay block replaces entirely — only "b" present
			assert_eq!(inner_items.len(), 1);
			assert_eq!(
				extract_scalar(find_by_key(inner_items, "b")),
				&ScalarValue::Identifier("2".to_string())
			);
		}

		// ---- Type mismatch ----

		#[test]
		fn deep_merge_type_mismatch_warns() {
			// base is block, overlay is scalar → overlay wins, warning emitted
			let base = block_stmt("root", vec![scalar_ident("a", "1")]);
			let overlay = scalar_ident("root", "override_value");
			let mut warnings = Vec::new();
			let p = default_policies();
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			// Overlay wins outright
			assert_eq!(result, overlay);
			assert_eq!(warnings.len(), 1);
			assert!(warnings[0].contains("type mismatch"));
			assert!(warnings[0].contains("root"));
		}

		// ---- Repeated keys ----

		#[test]
		fn deep_merge_repeated_keys_treated_as_list() {
			// base has two "option" blocks, overlay adds a third
			let base = block_stmt(
				"root",
				vec![
					block_stmt("option", vec![scalar_ident("a", "1")]),
					block_stmt("option", vec![scalar_ident("b", "2")]),
				],
			);
			let overlay = block_stmt(
				"root",
				vec![block_stmt("option", vec![scalar_ident("c", "3")])],
			);
			let mut warnings = Vec::new();
			let p = policies(
				ScalarMergePolicy::default(),
				ListMergePolicy::Union,
				BlockMergePolicy::default(),
			);
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			// All three "option" blocks present (not a dup, Union appends)
			let option_count = items
				.iter()
				.filter(|s| matches!(s, AstStatement::Assignment { key, .. } if key == "option"))
				.count();
			assert_eq!(option_count, 3);
		}

		// ---- New / base-only keys ----

		#[test]
		fn deep_merge_new_keys_appended() {
			let base = block_stmt("root", vec![scalar_ident("a", "1")]);
			let overlay = block_stmt("root", vec![scalar_ident("a", "1"), scalar_ident("b", "2")]);
			let mut warnings = Vec::new();
			let p = default_policies();
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			assert_eq!(items.len(), 2);
			assert_eq!(
				extract_scalar(find_by_key(items, "b")),
				&ScalarValue::Identifier("2".to_string())
			);
		}

		#[test]
		fn deep_merge_base_only_keys_preserved() {
			let base = block_stmt(
				"root",
				vec![
					scalar_ident("keep", "yes"),
					scalar_ident("also_keep", "yes"),
				],
			);
			let overlay = block_stmt("root", vec![scalar_ident("new_key", "val")]);
			let mut warnings = Vec::new();
			let p = default_policies();
			let result = call_deep_merge(&base, &overlay, &p, &mut warnings);
			let items = merged_items(&result);
			assert_eq!(items.len(), 3);
			find_by_key(items, "keep");
			find_by_key(items, "also_keep");
			find_by_key(items, "new_key");
		}
	}

	// ---- NamedContainerUnion ----

	#[test]
	fn named_container_union_merges_disjoint_gui_window_types() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9801");
		let mod_b = temp.path().join("9802");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9801"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9802"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"interface/hre.gui",
			"guiTypes = {\n\twindowType = {\n\t\tname = \"hre_window_a\"\n\t}\n}\n",
		);
		write_script_file(
			&mod_b,
			"interface/hre.gui",
			"guiTypes = {\n\twindowType = {\n\t\tname = \"hre_window_b\"\n\t}\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		assert_eq!(result.structural_files.len(), 1);
		let file = &result.structural_files[0];
		assert_eq!(file.merge_key_source, MergeKeySource::AssignmentKey);
		assert_eq!(file.nodes.len(), 1, "container should not be split");
		let node = &file.nodes[0];
		assert_eq!(node.merge_key, "guiTypes");
		assert!(!node.conflict_rename);

		let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = &node.merged_statement
		else {
			panic!("merged guiTypes must be a block");
		};
		let window_names: Vec<String> = items
			.iter()
			.filter_map(|item| {
				let AstStatement::Assignment {
					key,
					value: AstValue::Block { items: inner, .. },
					..
				} = item
				else {
					return None;
				};
				if key != "windowType" {
					return None;
				}
				inner.iter().find_map(|inner_item| {
					if let AstStatement::Assignment {
						key: k,
						value: AstValue::Scalar { value, .. },
						..
					} = inner_item && k == "name"
					{
						Some(value.as_text())
					} else {
						None
					}
				})
			})
			.collect();
		assert_eq!(window_names, vec!["hre_window_a", "hre_window_b"]);
	}

	#[test]
	fn named_container_union_renames_conflicting_sprite_types() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9811");
		let mod_b = temp.path().join("9812");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9811"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9812"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"interface/family.gfx",
			"spriteTypes = {\n\tspriteType = {\n\t\tname = \"GFX_shared\"\n\t\ttexturefile = \"a.dds\"\n\t}\n\tspriteType = {\n\t\tname = \"GFX_only_a\"\n\t\ttexturefile = \"only_a.dds\"\n\t}\n}\n",
		);
		write_script_file(
			&mod_b,
			"interface/family.gfx",
			"spriteTypes = {\n\tspriteType = {\n\t\tname = \"GFX_shared\"\n\t\ttexturefile = \"b.dds\"\n\t}\n\tspriteType = {\n\t\tname = \"GFX_only_b\"\n\t\ttexturefile = \"only_b.dds\"\n\t}\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		let file = &result.structural_files[0];
		assert_eq!(file.merge_key_source, MergeKeySource::AssignmentKey);
		assert_eq!(file.nodes.len(), 1);
		let node = &file.nodes[0];
		assert_eq!(node.merge_key, "spriteTypes");
		assert!(!node.conflict_rename);

		let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = &node.merged_statement
		else {
			panic!("merged spriteTypes must be a block");
		};
		let sprite_names: Vec<String> = items
			.iter()
			.filter_map(|item| {
				let AstStatement::Assignment {
					value: AstValue::Block { items: inner, .. },
					..
				} = item
				else {
					return None;
				};
				inner.iter().find_map(|inner_item| {
					if let AstStatement::Assignment {
						key: k,
						value: AstValue::Scalar { value, .. },
						..
					} = inner_item && k == "name"
					{
						Some(value.as_text())
					} else {
						None
					}
				})
			})
			.collect();
		assert!(sprite_names.contains(&"GFX_shared".to_string()));
		assert!(sprite_names.contains(&"GFX_only_a".to_string()));
		assert!(sprite_names.contains(&"GFX_only_b".to_string()));
		assert!(
			sprite_names.iter().any(|n| n == "GFX_shared_9812"),
			"conflicting GFX_shared from mod B should be suffix-renamed; got {sprite_names:?}",
		);
		assert_eq!(sprite_names.len(), 4);
	}

	#[test]
	fn named_container_union_merges_disjoint_country_decisions() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9821");
		let mod_b = temp.path().join("9822");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9821"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9822"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_script_file(
			&mod_a,
			"decisions/PragmaticSanction.txt",
			"country_decisions = {\n\tdecision_a = {\n\t\teffect = { log = a }\n\t}\n}\n",
		);
		write_script_file(
			&mod_b,
			"decisions/PragmaticSanction.txt",
			"country_decisions = {\n\tdecision_b = {\n\t\teffect = { log = b }\n\t}\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		let file = &result.structural_files[0];
		assert_eq!(file.merge_key_source, MergeKeySource::ContainerChildKey);
		// ContainerChildKey already fragments at the decision level so each
		// decision is its own node — no container suffix to worry about.
		let keys: Vec<&str> = file
			.nodes
			.iter()
			.map(|node| node.merge_key.as_str())
			.collect();
		assert!(keys.contains(&"decision_a"));
		assert!(keys.contains(&"decision_b"));
		for node in &file.nodes {
			assert_eq!(node.container_key.as_deref(), Some("country_decisions"));
			assert!(!node.conflict_rename);
		}
	}

	#[test]
	fn named_container_union_skips_heterogeneous_blocks() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("9831");
		let mod_b = temp.path().join("9832");

		write_playlist(
			&playlist_path,
			json!([
				{"displayName":"A", "enabled": true, "position": 0, "steamId":"9831"},
				{"displayName":"B", "enabled": true, "position": 1, "steamId":"9832"}
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		// Top-level container holds a mix of a scalar field and a child block,
		// so we cannot derive a uniform child identity and must fall back to
		// the existing per-contributor suffix rename.
		write_script_file(
			&mod_a,
			"common/scripted_effects/effects.txt",
			"shared_effect = {\n\tversion = 1\n\tinner = {\n\t\tlog = a\n\t}\n}\n",
		);
		write_script_file(
			&mod_b,
			"common/scripted_effects/effects.txt",
			"shared_effect = {\n\tversion = 2\n\tinner = {\n\t\tlog = b\n\t}\n}\n",
		);

		let result = run_merge_ir_no_base(request_for(&playlist_path));
		assert!(result.fatal_errors.is_empty());
		let file = &result.structural_files[0];
		assert_eq!(file.merge_key_source, MergeKeySource::AssignmentKey);
		// Heterogeneous container falls back to suffix rename: two nodes,
		// neither retains the bare "shared_effect" key.
		let keys: Vec<&str> = file
			.nodes
			.iter()
			.map(|node| node.merge_key.as_str())
			.collect();
		assert!(!keys.contains(&"shared_effect"));
		assert!(keys.iter().any(|k| k.starts_with("shared_effect_")));
		assert!(file.nodes.iter().any(|node| node.conflict_rename));
	}
}
