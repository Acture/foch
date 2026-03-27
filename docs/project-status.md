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

## Current Coverage Reset Loop

The shipped merge-capable surface does not mean the EU4 base game is semantically covered.

The current near-term execution loop is now driven by the base snapshot coverage matrix rather than only by finding buckets:

- `ACT-126`: coverage reset foundation wave for base-game roots
- phase A: remove non-gameplay metadata/noise roots from `parse_only`
- phase B: promote foundation gameplay roots from `parse_only` to explicit root-specific semantics
- `ACT-127`: first common-data wave for rule-bearing roots beyond the foundation family

The current semantic-complete gameplay roots now include:

- `common/country_tags`
- `common/countries`
- `common/disasters`
- `common/government_mechanics`
- `common/rebel_types`
- `common/religions`
- `common/subject_types`
- `common/units`
- `history/countries`
- `history/provinces`
- `history/wars`

The next wave should move into the remaining common-data roots that still dominate `parse_only`, especially `estate_*`, `parliament_*`, `peace_treaties`, `bookmarks`, and `state_edicts`.

Finding-bucket tracks such as `ACT-32`, `ACT-31`, and `ACT-28` are now secondary observability loops. They are useful for regression signals, but they no longer define the main plan.

## Verification

Verified locally during the latest coverage wave:

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
- real `foch data build eu4 ...` probe confirmed `parse_only` moved from `98` to `93` and `semantic_complete` moved from `11` to `16`

## Practical Reading Order

1. [architecture.md](./architecture.md)
2. [merge-design.md](./merge-design.md)
3. [auto-merge-roadmap.md](./auto-merge-roadmap.md)
