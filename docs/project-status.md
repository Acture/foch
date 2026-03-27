# Project Status

Last updated: 2026-03-27

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

The `src/check/` layer is organized around six product-line subsystems:

- `workspace/`
- `analyzer/`
- `runtime/`
- `merge/`
- `graph/`
- `simplify/`

Two shared support modules remain at the root on purpose:

- `model`
- `base_data`

The analyzer support files now live physically under `src/check/analyzer/`, while legacy top-level module paths such as `check::analysis` and `check::semantic_index` remain as thin compatibility wrappers for the library surface.

`mod_cache` is no longer a standalone top-level subsystem; it now lives under `workspace/cache`.

The old flat `engine.rs` / `resolution.rs` / `graph_g1.rs` structure has been retired.

## Deferred Workstreams

These remain intentionally separate from the shipped v1 merge path:

- localisation compatibility follow-ups
- Graph G2 fine-grained grouping and richer viewers
- Simplify R2 beyond base-equivalent copy removal

## Current Semantic Cleanup Loop

The shipped merge-capable surface does not mean semantic cleanup is done.

The current near-term execution loop is still driven by real EU4 playset noise reduction:

- `ACT-32`: tighten scripted-effect param contracts and `S004`
- `ACT-31`: model unresolved wrapper semantics for `S002`
- `ACT-28`: reduce shared scripted-effect `A001`

Issue closure for those tracks should be based on saved real-smoke artifacts, not minimized corpus tests alone:

- generate the run summary with `scripts/eu4_real_smoke.py`
- compare baseline versus candidate with `scripts/eu4_real_smoke_compare.py`
- close an issue only after the full-playset counts and hotspot paths move in the intended direction

## Verification

Verified locally during the latest architecture cleanup:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`

## Practical Reading Order

1. [architecture.md](./architecture.md)
2. [merge-design.md](./merge-design.md)
3. [auto-merge-roadmap.md](./auto-merge-roadmap.md)
