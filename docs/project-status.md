# Project Status

Last updated: 2026-04-01

## Summary

`foch` is a shipped EU4 analyzer-plus-merge toolkit, and the repository is now organized as a workspace monorepo instead of a single root crate.

The shipped product surface includes:

- `foch check`
- `foch merge-plan`
- `foch merge`
- `foch graph`
- `foch simplify`
- `foch data`
- `foch config`
- `foch_lsp`

## Current Repository Shape

The repository now has these first-class packages:

- `apps/foch-cli`
- `crates/foch-core`
- `crates/foch-language`
- `crates/foch-engine`
- `packages/tree-sitter-paradox`
- `packages/vscode-foch`

The repository root is coordination-only:

- Cargo workspace manifest
- Bun workspace manifest
- shared CI
- docs and scripts

The old `src/check/` compatibility shell is gone. Internal code now imports directly from `foch_core`, `foch_language`, and `foch_engine`.

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

## Current Coverage Reset Loop

The current semantic-complete gameplay roots in the last verified real probe include:

- `common/country_tags`
- `common/countries`
- `common/bookmarks`
- `common/church_aspects`
- `common/decrees`
- `common/defender_of_faith`
- `common/disasters`
- `common/factions`
- `common/federation_advancements`
- `common/fetishist_cults`
- `common/fervor`
- `common/flagship_modifications`
- `common/golden_bulls`
- `common/government_mechanics`
- `common/hegemons`
- `common/holy_orders`
- `common/isolationism`
- `common/estate_agendas`
- `common/estate_privileges`
- `common/estates`
- `common/naval_doctrines`
- `common/parliament_bribes`
- `common/parliament_issues`
- `common/peace_treaties`
- `common/personal_deities`
- `common/policies`
- `common/professionalism`
- `common/province_names`
- `common/rebel_types`
- `common/religions`
- `common/state_edicts`
- `common/subject_types`
- `common/technologies`
- `common/technology`
- `common/units`
- `common/mercenary_companies`
- `map/random/scenarios`
- `map/random/tiles`
- `map/random_names`
- `history/advisors`
- `history/countries`
- `history/diplomacy`
- `history/provinces`
- `history/wars`

The latest verified real probe is:

- `parse_only = 63`
- `semantic_complete = 49`

`map/random` is now split honestly instead of being treated as one mixed root:

- `map/random/scenarios = semantic_complete`
- `map/random/tiles = semantic_complete`
- `map/random_names = semantic_complete`
- `map/random/tweaks = parse_only`

The mixed `map/random` backlog is now structurally honest and no longer the default next target. The next planning checkpoint should return to the remaining low-risk gameplay `common/*` tails, with `common/powerprojection` the most natural immediate follow-on because it sits next to the just-completed single-file mechanics wave.

Finding-bucket tracks such as `ACT-32`, `ACT-31`, and `ACT-28` are now secondary observability loops. They remain useful for regression signals, but they no longer define the main plan.

## Verification

Verified locally during the completed coverage waves:

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
- real `foch data build eu4 ...` probes confirmed:
  - `parse_only: 85 -> 80`
  - `semantic_complete: 24 -> 29`
  - `parse_only: 80 -> 78`
  - `semantic_complete: 29 -> 31`
  - `parse_only: 78 -> 76`
  - `semantic_complete: 31 -> 33`
  - `parse_only: 76 -> 74`
  - `semantic_complete: 33 -> 35`
  - `parse_only: 74 -> 73`
  - `semantic_complete: 35 -> 36`
  - `parse_only: 73 -> 73`
  - `semantic_complete: 36 -> 39`
  - `parse_only: 73 -> 68`
  - `semantic_complete: 39 -> 44`
  - `parse_only: 68 -> 63`
  - `semantic_complete: 44 -> 49`

Verified locally during the workspace reorganization:

- `cargo check -p foch-language`
- `cargo check -p foch-engine`
- `cargo check -p foch-cli`
- `cargo check --workspace`

## Practical Reading Order

1. [architecture.md](./architecture.md)
2. [merge-design.md](./merge-design.md)
3. [auto-merge-roadmap.md](./auto-merge-roadmap.md)
