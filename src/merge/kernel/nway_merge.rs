use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::merge::kernel::nway::{NWayExactSelection, NWaySelectionOverrides};
use crate::merge::kernel::{
	ChildCardinality, ChildOrder, ClassId, ConflictKind, ConflictNodeId, ConflictResolution,
	ConservativeMergePolicy, MergeInputId, MergeOutcome, MergePolicy, MergeRevision, MergeTimings,
	NWayClassSelection, NWayCorrespondence, NWayInputError, NWayScalarSynthesis, NodeId,
	NormalizedNode, NormalizedTree, RevisionId, RevisionNode, RevisionSourceRef, SourceNodeRef,
	SourceSet, StructuralConflict, StructuralConflictDraft, TreeError, TreeNode,
};

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum NWayMergeError {
	#[error(transparent)]
	Input(#[from] NWayInputError),
	#[error("the merge base root has no live N-way correspondence")]
	MissingRoot,
	#[error("conflict resolution {conflict} references stale merge input {input:?}")]
	StaleResolutionInput {
		conflict: ConflictNodeId,
		input: MergeInputId,
	},
	#[error("conflict resolution {conflict} references unknown source node {selected:?}")]
	UnknownResolutionSource {
		conflict: ConflictNodeId,
		selected: SourceNodeRef,
	},
	#[error("conflict {conflict} has incompatible exact source selections")]
	ConflictingResolutions { conflict: ConflictNodeId },
	#[error(transparent)]
	InvalidOutput(#[from] TreeError),
}

pub fn n_way_merge(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
) -> Result<MergeOutcome, NWayMergeError> {
	n_way_merge_with_policy(base, revisions, &ConservativeMergePolicy)
}

pub fn n_way_merge_with_policy(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	policy: &dyn MergePolicy,
) -> Result<MergeOutcome, NWayMergeError> {
	n_way_merge_with_policy_and_resolutions(base, revisions, policy, &[])
}

pub fn n_way_merge_with_policy_and_resolutions(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	policy: &dyn MergePolicy,
	resolutions: &[ConflictResolution],
) -> Result<MergeOutcome, NWayMergeError> {
	let (correspondence, timings) = NWayCorrespondence::build_profiled(base, revisions)?;
	let structural_started = Instant::now();
	let (overrides, resolved_conflicts) =
		resolution_overrides(base, revisions, &correspondence, resolutions)?;
	let (mut plan, policy_ns) = correspondence
		.selection_with_policy_and_overrides_profiled(base, revisions, policy, &overrides);
	record_match_ambiguities(base, revisions, &correspondence, &mut plan.conflicts);
	record_rejected_links(base, revisions, &correspondence, &mut plan.conflicts);

	let root_class = correspondence
		.classes
		.class_of(RevisionNode::new(RevisionId::BASE, base.root()));
	if plan.classes[&root_class].selected.is_none() {
		return Err(NWayMergeError::MissingRoot);
	}
	if let Some(parent) = plan
		.classes
		.get_mut(&root_class)
		.expect("the base root has a selection record")
		.parent
		.take()
	{
		plan.conflicts.push(class_conflict(
			ConflictKind::MoveMove,
			root_class,
			Some(parent),
			base,
			revisions,
			&correspondence,
			"the merged root was assigned a parent; root placement was restored".to_string(),
		));
	}

	let mut builder = NWayTreeBuilder::new(
		base,
		revisions,
		&correspondence,
		&plan.classes,
		&plan.policy_excluded,
		&mut plan.conflicts,
	);
	let root = builder.build(root_class);
	builder.record_unreachable();
	let sources_preorder = std::mem::take(&mut builder.sources_preorder);
	let syntheses_preorder = std::mem::take(&mut builder.syntheses_preorder);
	drop(builder);

	let mut tree = NormalizedTree::from_root(root)?;
	debug_assert_eq!(tree.len(), sources_preorder.len());
	debug_assert_eq!(tree.len(), syntheses_preorder.len());
	for (index, synthesis) in syntheses_preorder.into_iter().enumerate() {
		let Some(synthesis) = synthesis else {
			continue;
		};
		tree.record_scalar_synthesis(
			NodeId::new(index as u32),
			synthesis.reducer_path,
			synthesis.inputs,
			synthesis.output,
		)?;
	}
	let provenance = sources_preorder
		.into_iter()
		.enumerate()
		.map(|(index, sources)| (NodeId::new(index as u32), sources))
		.collect();
	plan.decisions.retain(|decision| {
		!is_policy_excluded(
			decision.affected_class,
			&plan.classes,
			&plan.policy_excluded,
		)
	});
	plan.conflicts.retain(|conflict| {
		let classes = conflict
			.revisions
			.iter()
			.map(|source| correspondence.classes.class_of(*source))
			.collect::<BTreeSet<_>>();
		classes.is_empty()
			|| classes
				.iter()
				.any(|class| !is_policy_excluded(*class, &plan.classes, &plan.policy_excluded))
	});
	suppress_required_slot_subconflicts(&mut plan.conflicts, &correspondence);
	suppress_descendant_delete_modify_conflicts(&mut plan.conflicts, base);
	let decisions = plan.decisions;
	let mut conflicts = plan
		.conflicts
		.into_iter()
		.map(|conflict| {
			StructuralConflict::identify(conflict, correspondence.inputs.values().copied())
		})
		.collect::<Vec<_>>();
	conflicts.retain(|conflict| !resolved_conflicts.contains(&conflict.id));
	conflicts.sort_by_key(|conflict| conflict.id);
	conflicts.dedup_by_key(|conflict| conflict.id);
	let pcs_ns = timings
		.mapping_ns
		.saturating_add(nanos(structural_started.elapsed()))
		.saturating_sub(policy_ns);

	Ok(MergeOutcome {
		tentative_tree: tree,
		inputs: correspondence.inputs,
		provenance,
		revision_deltas: correspondence.revision_deltas,
		decisions,
		conflicts,
		timings: MergeTimings {
			matcher_ns: timings.matcher_ns,
			delta_ns: timings.delta_ns,
			pcs_ns,
			policy_ns,
		},
	})
}

fn suppress_required_slot_subconflicts(
	conflicts: &mut Vec<StructuralConflictDraft>,
	correspondence: &NWayCorrespondence,
) {
	let slots = conflicts
		.iter()
		.filter(|conflict| conflict.kind == ConflictKind::ValueSlot)
		.map(|conflict| {
			(
				conflict.semantic_path.clone(),
				conflict
					.candidates
					.iter()
					.filter_map(|candidate| conflict_candidate_class(*candidate, correspondence))
					.collect::<BTreeSet<_>>(),
			)
		})
		.collect::<Vec<_>>();
	conflicts.retain(|conflict| {
		if conflict.kind == ConflictKind::ValueSlot {
			return true;
		}
		let affected = conflict
			.base
			.and_then(|base| correspondence.classes.class_for(base))
			.into_iter()
			.chain(
				conflict
					.candidates
					.iter()
					.filter_map(|candidate| conflict_candidate_class(*candidate, correspondence)),
			)
			.collect::<BTreeSet<_>>();
		!slots.iter().any(|(path, slot_classes)| {
			*path == conflict.semantic_path
				&& affected.iter().any(|class| slot_classes.contains(class))
		})
	});
}

fn suppress_descendant_delete_modify_conflicts(
	conflicts: &mut Vec<StructuralConflictDraft>,
	base: &NormalizedTree,
) {
	let delete_modify = conflicts
		.iter()
		.filter(|conflict| conflict.kind == ConflictKind::DeleteModify)
		.filter_map(|conflict| {
			Some((
				conflict.base?,
				conflict
					.candidates
					.iter()
					.filter_map(|candidate| match candidate {
						RevisionSourceRef::Tombstone { revision, .. } => Some(*revision),
						RevisionSourceRef::Node(_) => None,
					})
					.collect::<BTreeSet<_>>(),
			))
		})
		.collect::<Vec<_>>();
	conflicts.retain(|conflict| {
		if conflict.kind != ConflictKind::DeleteModify {
			return true;
		}
		let Some(descendant) = conflict.base else {
			return true;
		};
		let deleted_by = conflict
			.candidates
			.iter()
			.filter_map(|candidate| match candidate {
				RevisionSourceRef::Tombstone { revision, .. } => Some(*revision),
				RevisionSourceRef::Node(_) => None,
			})
			.collect::<BTreeSet<_>>();
		!delete_modify.iter().any(|(ancestor, ancestor_deleted_by)| {
			ancestor.node != descendant.node
				&& *ancestor_deleted_by == deleted_by
				&& base_node_is_ancestor(base, ancestor.node, descendant.node)
		})
	});
}

fn base_node_is_ancestor(base: &NormalizedTree, ancestor: NodeId, mut node: NodeId) -> bool {
	while let Some(parent) = base.node(node).unwrap().parent {
		if parent == ancestor {
			return true;
		}
		node = parent;
	}
	false
}

fn conflict_candidate_class(
	candidate: RevisionSourceRef,
	correspondence: &NWayCorrespondence,
) -> Option<ClassId> {
	match candidate {
		RevisionSourceRef::Node(source) => correspondence.classes.class_for(source),
		RevisionSourceRef::Tombstone { base_node, .. } => correspondence
			.classes
			.class_for(RevisionNode::new(RevisionId::BASE, base_node)),
	}
}

fn resolution_overrides(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	resolutions: &[ConflictResolution],
) -> Result<(NWaySelectionOverrides, BTreeSet<ConflictNodeId>), NWayMergeError> {
	let mut overrides = NWaySelectionOverrides::default();
	let mut resolved = BTreeMap::<ConflictNodeId, SourceNodeRef>::new();
	for resolution in resolutions {
		let conflict = &resolution.conflict;
		if !conflict.candidates.contains(&resolution.selected) {
			return Err(NWayMergeError::UnknownResolutionSource {
				conflict: conflict.id,
				selected: resolution.selected,
			});
		}
		validate_conflict_identity(correspondence, conflict)?;
		if let Some(previous) = resolved.insert(conflict.id, resolution.selected) {
			if previous != resolution.selected {
				return Err(NWayMergeError::ConflictingResolutions {
					conflict: conflict.id,
				});
			}
			continue;
		}

		let selected = bind_resolution_source(
			base,
			revisions,
			correspondence,
			conflict.id,
			resolution.selected,
		)?;
		if conflict.kind == ConflictKind::Ordering {
			let BoundResolutionSource::Node(source) = selected else {
				return Err(NWayMergeError::UnknownResolutionSource {
					conflict: conflict.id,
					selected: resolution.selected,
				});
			};
			let parent = conflict
				.parent
				.or_else(|| correspondence.classes.class_for(source))
				.ok_or(NWayMergeError::UnknownResolutionSource {
					conflict: conflict.id,
					selected: resolution.selected,
				})?;
			insert_child_revision(&mut overrides, parent, source.revision, conflict.id)?;
			continue;
		}

		let selected_class = bound_source_class(correspondence, selected).ok_or(
			NWayMergeError::UnknownResolutionSource {
				conflict: conflict.id,
				selected: resolution.selected,
			},
		)?;
		let base_class = conflict
			.base
			.and_then(|source| correspondence.classes.class_for(source));
		let target = if conflict.kind == ConflictKind::ValueSlot {
			selected_class
		} else {
			base_class.unwrap_or(selected_class)
		};
		let contributors = conflict.revisions.clone();
		let exact = match selected {
			BoundResolutionSource::Node(source) => NWayExactSelection::Subtree {
				source,
				contributors,
			},
			BoundResolutionSource::Tombstone {
				deleted_by,
				base_node,
			} => NWayExactSelection::Delete {
				base: RevisionNode::new(RevisionId::BASE, base_node),
				deleted_by,
				contributors,
			},
		};
		insert_exact_selection(&mut overrides, target, exact, conflict.id)?;
		let subtree_root = match selected {
			BoundResolutionSource::Node(source) => source,
			BoundResolutionSource::Tombstone { base_node, .. } => {
				RevisionNode::new(RevisionId::BASE, base_node)
			}
		};
		overrides
			.excluded
			.extend(correspondence.subtree_variant_descendants(subtree_root, base, revisions));

		for candidate in &conflict.candidates {
			let bound =
				bind_resolution_source(base, revisions, correspondence, conflict.id, *candidate)?;
			if let Some(class) = bound_source_class(correspondence, bound)
				&& class != target
			{
				overrides.excluded.insert(class);
			}
		}
		if let BoundResolutionSource::Node(source) = selected {
			let selected_tree = tree_for_revision(base, revisions, source.revision);
			let parent = selected_tree
				.node(source.node)
				.expect("validated resolution source belongs to its tree")
				.parent
				.and_then(|node| {
					correspondence
						.classes
						.class_for(RevisionNode::new(source.revision, node))
				});
			overrides.parents.insert(
				target,
				if conflict.kind == ConflictKind::ValueSlot {
					conflict.parent.or(parent)
				} else {
					parent
				},
			);
			restore_resolution_ancestors(
				base,
				revisions,
				correspondence,
				conflict.id,
				source,
				target,
				&mut overrides,
			)?;
		}
	}
	Ok((overrides, resolved.into_keys().collect()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundResolutionSource {
	Node(RevisionNode),
	Tombstone {
		deleted_by: RevisionId,
		base_node: NodeId,
	},
}

fn bind_resolution_source(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	conflict: ConflictNodeId,
	source: SourceNodeRef,
) -> Result<BoundResolutionSource, NWayMergeError> {
	let input = source.input();
	if correspondence.inputs.get(&input.revision) != Some(&input) {
		return Err(NWayMergeError::StaleResolutionInput { conflict, input });
	}
	match source {
		SourceNodeRef::Node { input, node } => {
			let tree = tree_for_revision(base, revisions, input.revision);
			let revision_node = RevisionNode::new(input.revision, node);
			if tree.node(node).is_err() || correspondence.classes.class_for(revision_node).is_none()
			{
				return Err(NWayMergeError::UnknownResolutionSource {
					conflict,
					selected: source,
				});
			}
			Ok(BoundResolutionSource::Node(revision_node))
		}
		SourceNodeRef::Tombstone { input, base_node } => {
			let base_source = RevisionNode::new(RevisionId::BASE, base_node);
			let valid = base.node(base_node).is_ok()
				&& correspondence
					.classes
					.class_for(base_source)
					.is_some_and(|class| {
						correspondence
							.classes
							.class(class)
							.get(input.revision)
							.is_none()
					});
			if !valid {
				return Err(NWayMergeError::UnknownResolutionSource {
					conflict,
					selected: source,
				});
			}
			Ok(BoundResolutionSource::Tombstone {
				deleted_by: input.revision,
				base_node,
			})
		}
	}
}

fn bound_source_class(
	correspondence: &NWayCorrespondence,
	source: BoundResolutionSource,
) -> Option<ClassId> {
	match source {
		BoundResolutionSource::Node(source) => correspondence.classes.class_for(source),
		BoundResolutionSource::Tombstone { base_node, .. } => correspondence
			.classes
			.class_for(RevisionNode::new(RevisionId::BASE, base_node)),
	}
}

fn validate_conflict_identity(
	correspondence: &NWayCorrespondence,
	conflict: &StructuralConflict,
) -> Result<(), NWayMergeError> {
	for candidate in &conflict.candidates {
		let input = candidate.input();
		if correspondence.inputs.get(&input.revision) != Some(&input) {
			return Err(NWayMergeError::StaleResolutionInput {
				conflict: conflict.id,
				input,
			});
		}
	}
	let expected = ConflictNodeId::derive(
		correspondence.inputs.values().copied(),
		conflict.kind,
		conflict.parent.map(ClassId::get),
		conflict.base,
		&conflict.candidates,
		&conflict.semantic_path,
	);
	if expected != conflict.id {
		let input = conflict
			.candidates
			.first()
			.map(|candidate| candidate.input())
			.unwrap_or(correspondence.inputs[&RevisionId::BASE]);
		return Err(NWayMergeError::StaleResolutionInput {
			conflict: conflict.id,
			input,
		});
	}
	Ok(())
}

fn insert_exact_selection(
	overrides: &mut NWaySelectionOverrides,
	class: ClassId,
	selection: NWayExactSelection,
	conflict: ConflictNodeId,
) -> Result<(), NWayMergeError> {
	let Some(previous) = overrides.selections.get_mut(&class) else {
		overrides.selections.insert(class, selection);
		return Ok(());
	};
	match (previous, selection) {
		(
			NWayExactSelection::Subtree {
				source: previous_source,
				contributors: previous_contributors,
			},
			NWayExactSelection::Subtree {
				source,
				contributors,
			},
		) if *previous_source == source => {
			for contributor in contributors.iter().copied() {
				previous_contributors.insert(contributor);
			}
		}
		(
			NWayExactSelection::Delete {
				base: previous_base,
				deleted_by: previous_deleted_by,
				contributors: previous_contributors,
			},
			NWayExactSelection::Delete {
				base,
				deleted_by,
				contributors,
			},
		) if *previous_base == base && *previous_deleted_by == deleted_by => {
			for contributor in contributors.iter().copied() {
				previous_contributors.insert(contributor);
			}
		}
		_ => return Err(NWayMergeError::ConflictingResolutions { conflict }),
	}
	Ok(())
}

fn insert_child_revision(
	overrides: &mut NWaySelectionOverrides,
	class: ClassId,
	revision: RevisionId,
	conflict: ConflictNodeId,
) -> Result<(), NWayMergeError> {
	if let Some(previous) = overrides.child_revisions.insert(class, revision)
		&& previous != revision
	{
		return Err(NWayMergeError::ConflictingResolutions { conflict });
	}
	Ok(())
}

fn restore_resolution_ancestors(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	conflict: ConflictNodeId,
	source: RevisionNode,
	target: ClassId,
	overrides: &mut NWaySelectionOverrides,
) -> Result<(), NWayMergeError> {
	let tree = tree_for_revision(base, revisions, source.revision);
	let mut current = tree
		.node(source.node)
		.expect("validated resolution source belongs to its tree")
		.parent;
	while let Some(node) = current {
		let source = RevisionNode::new(source.revision, node);
		let class = correspondence.classes.class_for(source).ok_or(
			NWayMergeError::UnknownResolutionSource {
				conflict,
				selected: SourceNodeRef::Node {
					input: correspondence.inputs[&source.revision],
					node,
				},
			},
		)?;
		if class != target {
			overrides.restored_ancestors.entry(class).or_insert(source);
		}
		let parent = tree
			.node(node)
			.expect("validated ancestor belongs to its tree")
			.parent;
		overrides.parents.entry(class).or_insert_with(|| {
			parent.and_then(|parent| {
				correspondence
					.classes
					.class_for(RevisionNode::new(source.revision, parent))
			})
		});
		current = parent;
	}
	Ok(())
}

struct NWayTreeBuilder<'a, 'tree> {
	base: &'tree NormalizedTree,
	revisions: &'a [MergeRevision<'tree>],
	correspondence: &'a NWayCorrespondence,
	selections: &'a BTreeMap<ClassId, NWayClassSelection>,
	policy_excluded: &'a BTreeSet<ClassId>,
	conflicts: &'a mut Vec<StructuralConflictDraft>,
	sources_preorder: Vec<SourceSet>,
	syntheses_preorder: Vec<Option<NWayScalarSynthesis>>,
	visiting: BTreeSet<ClassId>,
	emitted: BTreeSet<ClassId>,
}

impl<'a, 'tree> NWayTreeBuilder<'a, 'tree> {
	fn new(
		base: &'tree NormalizedTree,
		revisions: &'a [MergeRevision<'tree>],
		correspondence: &'a NWayCorrespondence,
		selections: &'a BTreeMap<ClassId, NWayClassSelection>,
		policy_excluded: &'a BTreeSet<ClassId>,
		conflicts: &'a mut Vec<StructuralConflictDraft>,
	) -> Self {
		Self {
			base,
			revisions,
			correspondence,
			selections,
			policy_excluded,
			conflicts,
			sources_preorder: Vec::new(),
			syntheses_preorder: Vec::new(),
			visiting: BTreeSet::new(),
			emitted: BTreeSet::new(),
		}
	}

	fn build(&mut self, class: ClassId) -> TreeNode {
		let selection = self.selections[&class].clone();
		let selected = selection
			.selected
			.expect("only live classes occur in an N-way child sequence");
		if selection.subtree_selected {
			return self.clone_source_subtree(selected, Some(selection.sources), Some(class));
		}
		if !self.visiting.insert(class) {
			self.conflicts.push(class_conflict(
				ConflictKind::MoveMove,
				class,
				selection.parent,
				self.base,
				self.revisions,
				self.correspondence,
				"the merged parent relation contains a cycle".to_string(),
			));
			return self.clone_source_subtree(selected, Some(selection.sources), Some(class));
		}
		if !self.emitted.insert(class) {
			self.conflicts.push(class_conflict(
				ConflictKind::MoveMove,
				class,
				selection.parent,
				self.base,
				self.revisions,
				self.correspondence,
				"the same live class is reachable through more than one parent".to_string(),
			));
			self.visiting.remove(&class);
			return self.clone_source_subtree(selected, Some(selection.sources), Some(class));
		}

		let selected_node = self.node(selected).clone();
		let child_classes = selection
			.children
			.into_iter()
			.filter(|child| {
				self.selections.get(child).is_some_and(|candidate| {
					candidate.selected.is_some() && candidate.parent == Some(class)
				})
			})
			.collect::<Vec<_>>();
		if selected_node.child_cardinality == ChildCardinality::ExactlyOne
			&& child_classes.len() != 1
		{
			self.conflicts.push(class_conflict(
				ConflictKind::ValueSlot,
				class,
				selection.parent,
				self.base,
				self.revisions,
				self.correspondence,
				format!(
					"required child slot materialized {} children; selected source subtree retained tentatively",
					child_classes.len(),
				),
			));
			self.visiting.remove(&class);
			return self.clone_source_subtree(selected, Some(selection.sources), Some(class));
		}
		if selected_node.child_order == ChildOrder::Commutative {
			self.record_duplicate_signatures(class, &child_classes);
		}

		self.sources_preorder.push(selection.sources);
		self.syntheses_preorder
			.push(selection.scalar_synthesis.clone());
		let children = child_classes
			.into_iter()
			.map(|child| self.build(child))
			.collect();
		self.visiting.remove(&class);
		TreeNode {
			kind: selected_node.kind,
			value: selection
				.scalar_synthesis
				.map(|synthesis| synthesis.output)
				.or(selected_node.value),
			anchor: selected_node.anchor,
			signature: selected_node.signature,
			child_order: selected_node.child_order,
			child_cardinality: selected_node.child_cardinality,
			children,
		}
	}

	fn clone_source_subtree(
		&mut self,
		source: RevisionNode,
		root_sources: Option<SourceSet>,
		resolution_class: Option<ClassId>,
	) -> TreeNode {
		let node = self.node(source).clone();
		let class = self.correspondence.classes.class_of(source);
		if let Some(resolution_class) = resolution_class {
			self.emitted.insert(resolution_class);
		}
		self.emitted.insert(class);
		self.sources_preorder
			.push(root_sources.unwrap_or_else(|| SourceSet::new([source])));
		self.syntheses_preorder.push(None);
		let children = node
			.children
			.iter()
			.map(|child| {
				self.clone_source_subtree(RevisionNode::new(source.revision, *child), None, None)
			})
			.collect();
		TreeNode {
			kind: node.kind,
			value: node.value,
			anchor: node.anchor,
			signature: node.signature,
			child_order: node.child_order,
			child_cardinality: node.child_cardinality,
			children,
		}
	}

	fn record_duplicate_signatures(&mut self, parent: ClassId, children: &[ClassId]) {
		let mut signatures = BTreeMap::new();
		for child in children {
			let selected = self.selections[child]
				.selected
				.expect("a materialized child is live");
			let Some(signature) = self.node(selected).signature.clone() else {
				continue;
			};
			if let Some(previous) = signatures.insert(signature.clone(), *child)
				&& previous != *child
			{
				self.conflicts.push(class_conflict(
					ConflictKind::DuplicateSignature,
					*child,
					Some(parent),
					self.base,
					self.revisions,
					self.correspondence,
					format!("duplicate commutative signature `{signature}`"),
				));
			}
		}
	}

	fn record_unreachable(&mut self) {
		for (class, selection) in self.selections {
			if selection.selected.is_none()
				|| self.emitted.contains(class)
				|| self.is_policy_excluded(*class)
			{
				continue;
			}
			self.conflicts.push(class_conflict(
				ConflictKind::Policy,
				*class,
				selection.parent,
				self.base,
				self.revisions,
				self.correspondence,
				format!(
					"live class {} is unreachable from the merged root",
					class.get()
				),
			));
		}
	}

	fn is_policy_excluded(&self, class: ClassId) -> bool {
		let mut current = Some(class);
		let mut visited = BTreeSet::new();
		while let Some(candidate) = current {
			if self.policy_excluded.contains(&candidate) {
				return true;
			}
			if !visited.insert(candidate) {
				return false;
			}
			current = self
				.selections
				.get(&candidate)
				.and_then(|selection| selection.parent);
		}
		false
	}

	fn node(&self, source: RevisionNode) -> &NormalizedNode {
		tree_for_revision(self.base, self.revisions, source.revision)
			.node(source.node)
			.unwrap()
	}
}

fn record_match_ambiguities(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	conflicts: &mut Vec<StructuralConflictDraft>,
) {
	for (&(left_revision, right_revision), matching) in &correspondence.matchings {
		for ambiguity in matching.ambiguities() {
			let sources = std::iter::once(RevisionNode::new(left_revision, ambiguity.left))
				.chain(
					ambiguity
						.candidates
						.iter()
						.copied()
						.map(|node| RevisionNode::new(right_revision, node)),
				)
				.collect::<Vec<_>>();
			let mut conflict = StructuralConflictDraft::new(
				ConflictKind::AmbiguousMatch,
				common_parent_class(base, revisions, correspondence, &sources),
				(left_revision == RevisionId::BASE)
					.then_some(RevisionNode::new(left_revision, ambiguity.left)),
				SourceSet::new(sources.iter().copied()),
				Vec::new(),
				format!(
					"revision {} node {} has {} equally ranked revision {} candidates at score {}",
					left_revision.get(),
					ambiguity.left.get(),
					ambiguity.candidates.len(),
					right_revision.get(),
					ambiguity.score,
				),
			);
			conflict.semantic_path = common_policy_path(sources.iter().map(|source| {
				tree_for_revision(base, revisions, source.revision)
					.node(source.node)
					.unwrap()
			}));
			conflicts.push(conflict);
		}
	}
}

fn record_rejected_links(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	conflicts: &mut Vec<StructuralConflictDraft>,
) {
	for rejected in correspondence.classes.rejected_links() {
		let left_class = correspondence.classes.class_of(rejected.left);
		let right_class = correspondence.classes.class_of(rejected.right);
		let distinct_base_anchors = correspondence
			.classes
			.class(left_class)
			.get(RevisionId::BASE)
			.zip(
				correspondence
					.classes
					.class(right_class)
					.get(RevisionId::BASE),
			)
			.is_some_and(|(left, right)| left != right);
		if distinct_base_anchors {
			continue;
		}
		let sources = [rejected.left, rejected.right];
		let base_source = sources
			.iter()
			.copied()
			.find(|source| source.revision == RevisionId::BASE);
		let mut conflict = StructuralConflictDraft::new(
			ConflictKind::AmbiguousMatch,
			common_parent_class(base, revisions, correspondence, &sources),
			base_source,
			SourceSet::new(sources),
			Vec::new(),
			format!(
				"matching revision {} node {} with revision {} node {} would put two nodes from one revision in the same class",
				rejected.left.revision.get(),
				rejected.left.node.get(),
				rejected.right.revision.get(),
				rejected.right.node.get(),
			),
		);
		conflict.semantic_path = common_policy_path(sources.iter().map(|source| {
			tree_for_revision(base, revisions, source.revision)
				.node(source.node)
				.unwrap()
		}));
		conflicts.push(conflict);
	}
}

fn common_parent_class(
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	sources: &[RevisionNode],
) -> Option<ClassId> {
	let mut parents = sources.iter().map(|source| {
		tree_for_revision(base, revisions, source.revision)
			.node(source.node)
			.unwrap()
			.parent
			.map(|parent| {
				correspondence
					.classes
					.class_of(RevisionNode::new(source.revision, parent))
			})
	});
	let first = parents.next()?;
	parents
		.all(|parent| parent == first)
		.then_some(first)
		.flatten()
}

fn class_conflict(
	kind: ConflictKind,
	class: ClassId,
	parent: Option<ClassId>,
	base: &NormalizedTree,
	revisions: &[MergeRevision<'_>],
	correspondence: &NWayCorrespondence,
	detail: String,
) -> StructuralConflictDraft {
	let revision_class = correspondence.classes.class(class);
	let mut conflict = StructuralConflictDraft::new(
		kind,
		parent,
		revision_class
			.get(RevisionId::BASE)
			.map(|node| RevisionNode::new(RevisionId::BASE, node)),
		SourceSet::new(
			revision_class
				.members
				.iter()
				.map(|(revision, node)| RevisionNode::new(*revision, *node)),
		),
		Vec::new(),
		detail,
	);
	conflict.semantic_path =
		common_policy_path(revision_class.members.iter().map(|(revision, node)| {
			tree_for_revision(base, revisions, *revision)
				.node(*node)
				.unwrap()
		}));
	conflict
}

fn common_policy_path<'a>(nodes: impl IntoIterator<Item = &'a NormalizedNode>) -> Vec<String> {
	let mut paths = nodes.into_iter().map(|node| node.policy_path.as_slice());
	let Some(first) = paths.next() else {
		return Vec::new();
	};
	let mut common = first.to_vec();
	for path in paths {
		common.truncate(
			common
				.iter()
				.zip(path)
				.take_while(|(left, right)| left == right)
				.count(),
		);
		if common.is_empty() {
			break;
		}
	}
	common
}

fn is_policy_excluded(
	class: ClassId,
	selections: &BTreeMap<ClassId, NWayClassSelection>,
	excluded: &BTreeSet<ClassId>,
) -> bool {
	let mut current = Some(class);
	let mut visited = BTreeSet::new();
	while let Some(candidate) = current {
		if excluded.contains(&candidate) {
			return true;
		}
		if !visited.insert(candidate) {
			return false;
		}
		current = selections
			.get(&candidate)
			.and_then(|selection| selection.parent);
	}
	false
}

fn tree_for_revision<'tree>(
	base: &'tree NormalizedTree,
	revisions: &[MergeRevision<'tree>],
	revision: RevisionId,
) -> &'tree NormalizedTree {
	if revision == RevisionId::BASE {
		return base;
	}
	revisions
		.iter()
		.find(|candidate| candidate.id == revision)
		.map(|candidate| candidate.tree)
		.unwrap_or_else(|| panic!("unknown revision {}", revision.get()))
}

fn nanos(duration: Duration) -> u64 {
	u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn compatible_exact_selections_union_their_contributors() {
		let conflict = ConflictNodeId::derive([], ConflictKind::Policy, None, None, &[], &[]);
		let class = ClassId::new(4);
		let selected = RevisionNode::new(RevisionId::new(1), NodeId::new(7));
		let mut overrides = NWaySelectionOverrides::default();
		insert_exact_selection(
			&mut overrides,
			class,
			NWayExactSelection::Subtree {
				source: selected,
				contributors: SourceSet::new([RevisionNode::new(RevisionId::BASE, NodeId::new(7))]),
			},
			conflict,
		)
		.unwrap();
		insert_exact_selection(
			&mut overrides,
			class,
			NWayExactSelection::Subtree {
				source: selected,
				contributors: SourceSet::new([RevisionNode::new(
					RevisionId::new(2),
					NodeId::new(7),
				)]),
			},
			conflict,
		)
		.unwrap();

		let NWayExactSelection::Subtree { contributors, .. } = &overrides.selections[&class] else {
			panic!("expected subtree selection");
		};
		assert_eq!(contributors.len(), 2);

		let error = insert_exact_selection(
			&mut overrides,
			class,
			NWayExactSelection::Subtree {
				source: RevisionNode::new(RevisionId::new(2), NodeId::new(7)),
				contributors: SourceSet::default(),
			},
			conflict,
		)
		.expect_err("different source selections conflict");
		assert_eq!(error, NWayMergeError::ConflictingResolutions { conflict });
	}
}
