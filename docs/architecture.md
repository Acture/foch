# Architecture

Last updated: 2026-03-25

## Summary

`foch` is no longer just a flat static-analysis crate. The repository now has six stable product-line subsystems under `src/check/`:

- `workspace`: playset loading, mod/base discovery, file inventory, snapshot-aware workspace resolution
- `analyzer`: parser, documents, semantic index, semantic analysis, rule execution, check runner
- `runtime`: runtime symbol binding, precedence winner selection, dependency hints, overlap classification
- `merge`: merge plan, merge IR, normalization, deterministic emission, materialization, post-merge revalidation
- `graph`: runtime call graph and mod-dependency graph export
- `simplify`: base-equivalent definition cleanup for a target mod

The public CLI commands map onto these layers instead of reaching into ad hoc internal files.

## Data Flow

### `foch check`

1. `workspace::resolve` builds the effective workspace from playset, config, mod roots, and optional base snapshot.
2. `analyzer::run` merges semantic snapshots, runs semantic analysis and repository rules.
3. `runtime::overlap` contributes the final `S001` / `A003` overlap diagnostics.
4. `report` renders the filtered result.

### `foch merge-plan`

1. `workspace::resolve` builds the effective file inventory.
2. `merge::plan` classifies every effective path into `copy_through`, `last_writer_overlay`, `structural_merge`, or `manual_conflict`.

### `foch merge`

1. `merge::plan` builds the frozen plan.
2. `merge::ir` lifts supported structural paths into merge IR.
3. `merge::emit` generates deterministic Clausewitz output.
4. `merge::materialize` writes `descriptor.mod`, `.foch/*`, copied files, overlays, placeholders, and structural outputs.
5. `merge::execute` revalidates the generated tree with the normal analyzer pipeline and backfills the final merge report.

### `foch graph`

1. `runtime::binding` resolves actual runtime winners and reference targets.
2. `runtime::overlap` annotates discardable / mergeable / conflicting overlap states.
3. `graph::export` writes `calls` and `mod-deps` artifacts as `json`, `dot`, or both.

### `foch simplify`

1. `runtime::overlap` identifies `discardable_base_copy` definitions.
2. `simplify::execute` removes only those top-level definitions, rewrites affected files, deletes empty files, and emits `simplify-report.json`.

## Shared Kernel

Two shared internal kernels now sit between raw analysis and downstream products:

- `workspace::resolve`: authoritative source for the effective workspace
- `runtime::{binding, overlap}`: authoritative source for runtime winner selection, dependency hints, and overlap classification

This avoids each product line inventing its own precedence or overlap rules.

## Public Entry Points

The stable check-layer façade is exposed from `src/check/mod.rs`:

- `run_checks_with_options`
- `run_merge_plan_with_options`
- `run_merge_with_options`
- `run_graph_with_options`
- `run_simplify_with_options`

CLI handlers are expected to call these façade functions only.

## Legacy Interfaces Removed

The repository no longer supports graph export through `foch check`.

- removed: `check --graph-out`
- removed: `check --graph-format`
- supported path: `foch graph`

This keeps graph generation on its own product line instead of treating it as a side effect of `check`.
