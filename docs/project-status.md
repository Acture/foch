# Project Status

Last updated: 2026-04-02

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
- export family-first semantic graphs with `--mode semantic --family <content-family-id>`
- annotate cross-mod edges with declared-dependency hints
- write deterministic `json` and `dot` artifacts for workspace/base/per-mod views and optional symbol trees
- write deterministic `semantic-graph.json` plus a static `index.html` viewer for a selected family

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
- `common/powerprojection`
- `common/personal_deities`
- `common/policies`
- `common/professionalism`
- `common/province_names`
- `common/rebel_types`
- `common/religions`
- `common/state_edicts`
- `common/subject_types`
- `common/subject_type_upgrades`
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

- `parse_only = 60`
- `semantic_complete = 52`

`map/random` is now split honestly instead of being treated as one mixed root:

- `map/random/scenarios = semantic_complete`
- `map/random/tiles = semantic_complete`
- `map/random_names = semantic_complete`
- `map/random/tweaks = parse_only`

`common/government_ranks` is now complete. The current product line has shifted from another low-risk coverage wave to semantic graphing, with `foch graph --mode semantic --family ...` now serving as the next mainline for visualizing family structure, overlay, and references.

The static semantic viewer had one critical renderer regression immediately after ACT-157 landed: the generated `index.html` escaped CSS and JS braces incorrectly, which left the page shell visible but the graph tree blank. That regression is now fixed and covered by a renderer-level test in `foch-engine`.

Validation now splits into two tracks:

- representative family output is readable again in the static viewer
- real semantic-graph runs are now observable enough to use as a validation loop without falling back to ad hoc `/tmp` slices

A repo-backed bounded validation path now exists under `tests/corpus/eu4_real_minimized/playlist.json`. Semantic graph CLI integration coverage uses that playset to export `common/scripted_effects`, assert default-visible progress output from the `tracing` pipeline, and confirm that the generated graph contains real scripted-effect keys such as `eu4::scripted_effects::se_md_add_or_upgrade_bonus` and `eu4::scripted_effects::complex_dynamic_effect_without_alternative`.

The next execution step is not infrastructure work on graph export itself. It is to use the new observability and repo-backed bounded playset to validate a few representative real families, then decide whether the next mainline issue is a narrow ACT-158-style viewer refinement pass or a return to semantic coverage promotion.

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
  - `parse_only: 63 -> 62`
  - `semantic_complete: 49 -> 50`
  - `parse_only: 62 -> 61`
  - `semantic_complete: 50 -> 51`
  - `parse_only: 61 -> 60`
  - `semantic_complete: 51 -> 52`

Verified locally during the workspace reorganization:

- `cargo check -p foch-language`
- `cargo check -p foch-engine`
- `cargo check -p foch-cli`
- `cargo check --workspace`

Verified locally during the semantic graph mode implementation:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace`

Verified locally during the semantic viewer repair:

- `cargo fmt --all`
- `cargo test -p foch-engine graph::semantic -- --nocapture`
- browser validation against the regenerated `common/holy_orders` semantic viewer confirmed tree rendering and details-panel interaction

Verified locally during semantic graph observability hardening:

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test -p foch-cli semantic_graph -- --nocapture`
- `cargo test --workspace`
- `target/debug/foch graph tests/corpus/eu4_real_minimized/playlist.json --out /tmp/foch-act164-probe --mode semantic --family common/scripted_effects --no-game-base`

## Practical Reading Order

1. [architecture.md](./architecture.md)
2. [merge-design.md](./merge-design.md)
3. [auto-merge-roadmap.md](./auto-merge-roadmap.md)
