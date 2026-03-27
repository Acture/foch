# Architecture

Last updated: 2026-03-27

## Summary

`foch` is no longer just a flat static-analysis crate. The repository now has six stable product-line subsystems under `src/check/`:

- `workspace`: playset loading, mod/base discovery, file inventory, snapshot-aware workspace resolution
- `analyzer`: parser, documents, semantic index, semantic analysis, rule execution, check runner
- `runtime`: runtime symbol binding, precedence winner selection, dependency hints, overlap classification
- `merge`: merge plan, merge IR, normalization, deterministic emission, materialization, post-merge revalidation
- `graph`: runtime call graph and mod-dependency graph export
- `simplify`: base-equivalent definition cleanup for a target mod

There are also two shared support modules at the check-layer root:

- `model`: shared types consumed across products
- `base_data`: installed base-snapshot build/install/load support for both `foch data` and analysis products

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

- `workspace::{resolve, cache}`: authoritative source for the effective workspace and mod snapshot caching
- `runtime::{binding, overlap}`: authoritative source for runtime winner selection, dependency hints, and overlap classification

This avoids each product line inventing its own precedence or overlap rules.

## Internal Module Boundaries

The analyzer implementation is physically grouped under `src/check/analyzer/`:

- `analysis`
- `documents`
- `eu4_builtin`
- `localisation`
- `param_contracts`
- `parser`
- `report`
- `rules`
- `semantic_index`
- `run`

For library stability, the historical top-level module paths such as `check::analysis`, `check::parser`, and `check::semantic_index` remain as thin compatibility wrappers. New internal code should prefer `check::analyzer::*` and `check::workspace::*`.

## Public Entry Points

The stable check-layer façade is exposed from `src/check/mod.rs`:

- `run_checks_with_options`
- `run_merge_plan_with_options`
- `run_merge_with_options`
- `run_graph_with_options`
- `run_simplify_with_options`

CLI handlers are expected to call these façade functions only.

The intended dependency direction is:

- `cli -> check façade`
- `merge/graph/simplify -> workspace/runtime/(when needed) analyzer façade`
- internal analyzer support code stays inside `src/check/analyzer/`

## Legacy Interfaces Removed

The repository no longer supports graph export through `foch check`.

- removed: `check --graph-out`
- removed: `check --graph-format`
- supported path: `foch graph`

This keeps graph generation on its own product line instead of treating it as a side effect of `check`.
