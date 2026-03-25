# Project Status

Last updated: 2026-03-25

## Summary

`foch` is now a shipped analyzer-plus-merge toolkit for EU4 mod playsets.

The repository currently includes:

- `foch check`
- `foch merge-plan`
- `foch merge`
- `foch graph`
- `foch simplify`
- `foch data`
- `foch config`

That means the earlier “analyzer only, merge not yet landed” description is no longer accurate.

## What Exists Today

### Analyzer

The analyzer pipeline can:

- resolve a playset into an effective workspace
- load optional installed base-game snapshots
- parse Clausewitz, localisation, CSV, and JSON families
- build semantic indexes across base game and enabled mods
- emit strict and advisory findings
- surface overlap diagnostics through the shared runtime overlap classifier

### Merge

The merge pipeline can:

- produce deterministic `merge-plan` artifacts
- build merge IR for supported structural roots
- emit normalized Clausewitz output
- materialize a merged output tree with `descriptor.mod` and `.foch/*` sidecars
- revalidate generated output and backfill final validation buckets

### Graph

The graph pipeline can:

- export runtime `calls` graphs
- export descriptor-level `mod-deps` graphs
- annotate cross-mod edges with declared-dependency hints
- write deterministic `json` and `dot` artifacts for workspace/base/per-mod views and optional symbol trees

### Simplify

The simplify pipeline can:

- remove definitions in a target mod that are structurally equivalent to the effective base-game definition
- work either in-place or into an output copy
- write a machine-readable `simplify-report.json`

## Current Internal Shape

The `src/check/` layer is now organized by product line:

- `workspace/`
- `analyzer/`
- `runtime/`
- `merge/`
- `graph/`
- `simplify/`

The old flat `engine.rs` / `resolution.rs` / `graph_g1.rs` structure has been retired.

## Deferred Workstreams

These remain intentionally separate from the shipped v1 merge path:

- localisation compatibility follow-ups
- Graph G2 fine-grained grouping and richer viewers
- Simplify R2 beyond base-equivalent copy removal

## Verification

Verified locally during the latest architecture cleanup:

- `cargo test --offline`
- `cargo clippy --all-targets`

## Practical Reading Order

1. [architecture.md](./architecture.md)
2. [merge-design.md](./merge-design.md)
3. [auto-merge-roadmap.md](./auto-merge-roadmap.md)
