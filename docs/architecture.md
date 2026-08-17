# Architecture

Last updated: 2026-08-17

## Summary

`foch` is now a workspace-based monorepo. The repository root is coordination only:

- Cargo workspace manifest
- Bun workspace manifest
- shared CI
- docs and scripts

The buildable products live under `crates/` and `packages/`.

## Workspace Layout

### `crates/`

- `crates/foch-cli`
  - owns the repository's only normal Cargo binary, `foch`; the `lsp`
    subcommand runs the language server on stdio
  - provides `parse_stats` and `symbol_dump` only as `dev-tools`-gated Cargo
    examples for parser / semantic-index maintainers
  - owns CLI parsing, command dispatch, and the product entrypoint

- `crates/foch-core`
  - shared domain types
  - diagnostics/model payloads
  - generic utilities
- `crates/foch-syntax`
  - shared syntax-tree types and source spans
  - consumed by the CWT schema layer
- `crates/foch-cwt`
  - CWT schema loading and compiled rule packs
  - schema binding and rule evaluation used by CLI, language, and engine layers
- `crates/foch-language`
  - parsing
  - document discovery
  - localisation
  - semantic index
  - `ContentFamily`
  - `GameProfile`
  - EU4 builtin/profile/content-family registry
- `crates/foch-engine`
  - workspace resolution/cache
  - base snapshot build/install/load
  - runtime binding and overlap
  - graph export
  - merge planning/execution
  - simplify
  - stable orchestration APIs consumed by the CLI
- `crates/foch-merge-kernel`
  - normalized semantic-tree representation
  - matching, deltas, conflict identities, and N-way merge primitives
  - game-independent kernel consumed by `foch-engine`
- `crates/foch-merge-quality`
  - private, library-only product evaluation and immutable dataset contracts
  - pure scoring over an existing output tree and product `MergeReport`
  - read-only Workshop/ACF input resolution, injected product measurement
    runners, compact evidence, and exact cohort reporting
  - metadata-only dataset export; no `ObjectStore`, recursive tree packer,
    snapshot builder, acquisition path, or review-pack workflow
  - no binary target and no production dependency from `foch-cli`

### `packages/`

- `packages/tree-sitter-paradox`
  - grammar package
  - Cargo workspace member
  - Bun workspace package
- `packages/vscode-foch`
  - VS Code extension
  - bundles `foch` from `crates/foch-cli` and launches it as `foch lsp`

## Dependency Direction

The intended dependency flow is:

- `foch-cli -> foch-engine + foch-language + foch-cwt + foch-core`
- `foch-engine -> foch-language + foch-cwt + foch-core + foch-merge-kernel`
- `foch-language -> foch-cwt + foch-core`
- `foch-cwt -> foch-syntax`
- `foch-cli` tests `-> foch-merge-quality` as a dev-dependency only

`foch-language` is the behavior boundary for game-aware language semantics. `ScriptFileKind` remains a plain compatibility enum and is not the primary extension point.

## Data Flow

### `foch check`

1. `foch-engine::workspace` resolves the effective workspace from config, playset, mod roots, and optional base snapshot.
2. `foch-language` parses documents, builds semantic indexes, and runs semantic analysis.
3. `foch-engine::runtime::overlap` adds final overlap diagnostics.
4. `foch-language::analyzer::report` renders the output.

### `foch merge-plan`

1. `foch-engine::workspace` builds the effective file inventory.
2. `foch-engine::merge::plan` classifies each effective path as copy-through, overlay, structural merge, or manual conflict.

### `foch merge`

1. `foch-engine::merge::execute::prepare_merge_with_options` freezes the
   resolved workspace, path-level plan, and policy without writing an output
   tree.
2. Explicit confirmation calls the prepared session's export path. File- or
   module-level failures are typed and deferred while unrelated safe units
   continue.
3. `foch-engine::merge::planning` builds definition-module views and revision
   DAGs; `merge::structured` adapts supported Clausewitz ASTs to the semantic
   tree in `foch-merge-kernel`.
4. `foch-engine::merge::output::materialize` emits deterministic Clausewitz
   output, withholds deferred units, projects provenance where enabled, and
   stages `.foch/*` sidecars.
5. `foch-engine::merge::execute` revalidates the generated tree and
   records kernel, scope, base, and input attestation in the merge report.
6. `output::materialize::output_transaction` publishes the complete staging
   tree under a same-target lock.

The preview freezes the resolved workspace and path-level plan. Structural
merging still happens during confirmed export, so some leaf conflicts and
deferred units first appear in the final report. Improving what the preview can
show remains open work; no replacement internal representation has been chosen.
See [project-status.md](./project-status.md).

### Product merge-quality acceptance

1. An ignored `foch-cli` integration test loads the committed fixed 14-case
   logical manifest; installed state cannot shrink its denominator.
2. The catalog pairs every discovered
   `steamapps/workshop/content/236850` root with the same library's
   `appworkshop_236850.acf` and strictly resolves all selected manifest IDs.
3. The engine requires the Steam build ID and records the ordered source-mod
   installation identities from ACF `(app_id, workshop_id, manifest_id)` tuples.
   It does not enumerate paths or read file bytes to establish that identity.
   Workshop content and ACF files are read-only and are not copied into an
   input CAS.
4. Each case launches the public `foch merge --confirm --non-interactive` artifact in an
   independent bounded child process and verifies its product-authored kernel,
   scope, base, and input attestation.
   The separate cold/warm cache gate deliberately omits `--confirm`: it validates
   the default read-only plan path and requires no output directory or merge report.
5. Before committing a terminal result, the library re-reads the exact ACF
   pairs and compares the ordered installation identities; any drift invalidates
   the run.
6. Only after a fresh product merge returns non-fatal does the scorer discover
   its scoring-unit set and base scoring closure. It captures only that compact
   closure, scores the capture without selecting another kernel, and stores the
   same bytes with an exact evidence index. The closure is stored evidence, not
   installation or cohort identity.
7. V2 input versions, observations, measurements, reports, and assertions stay
   separate from frozen V1 metadata and its historical `objects/` store.

This fixed 14-case workflow is the only product acceptance denominator.
Common-module and structured-rollout scripts are auxiliary analysis over the
same read-only boundary, not alternative product gates. Historical V1 objects
remain inert on disk pending a separate user-operated cleanup; current code
cannot pack, restore, or export them.

### `foch graph`

1. `foch-engine::runtime::binding` resolves runtime winners and reference targets.
2. `foch-engine::runtime::overlap` classifies overlap states.
3. `foch-engine::graph::export` writes `calls` and `mod-deps` artifacts.

### `foch simplify`

1. `foch-engine::runtime::overlap` identifies base-equivalent definitions.
2. `foch-engine::simplify::execute` rewrites files, drops empty files, and emits `simplify-report.json`.

## Language Layer

The language crate owns:

- parser
- document family discovery
- semantic indexing
- localisation handling
- analyzer reporting
- EU4-specific `ContentFamilyDescriptor` registry
- `Eu4Profile`

Behavior is attached to `ContentFamily`, not to giant central matches. That lets new EU4 roots and future game profiles register semantics without reopening core traversal logic.

## Product Packages

### VS Code

`packages/vscode-foch` is a standalone extension package. It prefers a bundled `foch` binary under `bin/<platform>-<arch>/` and launches it as `foch lsp`; its packaging scripts build that binary from the workspace root before packaging the VSIX.

### Tree-sitter

`packages/tree-sitter-paradox` remains its own grammar package. It is part of the workspace, but it is not folded into the Rust crates.

## Removed Legacy Shape

The old root-library shell and `src/check/` compatibility façade are gone as primary architecture. Internal code should import from workspace crates directly:

- `foch_core`
- `foch_language`
- `foch_engine`

The repository root is no longer a buildable Rust package.
