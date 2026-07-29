---
status: accepted
date: 2026-07-29
---

# Unify semantic merging behind TreeMergeKernel

## Decision

Foch's production merge path uses one semantic-tree pipeline:

1. A merge-unit adapter normalizes Clausewitz ASTs into semantic trees.
2. `ContentFamilyMergePolicy` supplies both normalization identity and merge decisions.
3. `TreeMergeKernel` folds all source revisions in precedence order.
4. N-source reducers finalize order-independent scalar results from the original revisions.
5. The adapter emits the resolved semantic tree only after conflicts are resolved.

The DAG layer owns topology, source identity, and conflict-handler orchestration. It does not
implement a second merge algorithm.

`run_merge_with_options` always uses the semantic-tree pipeline. The address-patch engine remains
available only through the explicit `run_merge_for_evaluation` API as
`MergeEvaluationKernel::AddressPatchReference`. Existing `legacy` and `structured` strings remain
stable in stored merge-quality artifacts.

Manual source selection retains every original source ID. Selecting A, B, or C reruns the tree
kernel with a semantic-path selection, including when the selected source deleted the subtree.
Unresolved tree conflicts are converted to patch-shaped records only at the reporting and manual
resolution boundary; those records never drive the output AST.

## Consequences

- Production output and conflict resolution share one semantic state.
- File, event, and definition-module merging differ through adapters and content-family policy,
  not separate kernels.
- Numeric reducers receive all original contributors once and exclude unchanged base copies.
- A conflict-free tentative tree is publishable; unresolved conflicts withhold publication.
- Address-patch behavior can still be compared in the corpus without being a silent fallback.

The current N-source implementation is a sequence of three-way tree folds plus an N-source reducer
finalizer. A first-class N-way ChangeSet algebra remains future work. Source selection currently
requires an unambiguous semantic assignment path; ambiguous matching and control-flow conflicts
remain manual unless a whole-file resolution is chosen.
