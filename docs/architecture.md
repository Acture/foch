# Architecture

Last updated: 2026-03-31

## Summary

`foch` is now a workspace-based monorepo. The repository root is coordination only:

- Cargo workspace manifest
- Bun workspace manifest
- shared CI
- docs and scripts

The buildable products live under `apps/`, `crates/`, and `packages/`.

## Workspace Layout

### `apps/`

- `crates/foch-cli`
  - owns the repository's only normal Cargo binary, `foch`; the `lsp`
    subcommand runs the language server on stdio
  - provides `parse_stats` and `symbol_dump` only as `dev-tools`-gated Cargo
    examples for parser / semantic-index maintainers
  - owns CLI parsing, command dispatch, and the product entrypoint

### `crates/`

- `crates/foch-core`
  - shared domain types
  - diagnostics/model payloads
  - generic utilities
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
- `crates/foch-merge-quality`
  - private, library-only product evaluation and immutable dataset contracts
  - pure scoring over an existing output tree and product `MergeReport`
  - injected measurement/review runners and exact cohort reporting
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

- `crates/foch-cli -> foch-engine`
- `foch-engine -> foch-language + foch-core`
- `foch-language -> foch-core`
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

1. `foch-engine::merge::plan` freezes the merge plan.
2. `foch-engine::merge::ir` lifts supported roots into merge IR.
3. `foch-engine::merge::emit` produces deterministic Clausewitz output.
4. `foch-engine::merge::materialize` writes the merged tree and `.foch/*` sidecars.
5. `foch-engine::merge::execute` revalidates the output using the normal analyzer pipeline and records kernel, scope, and base-snapshot attestation in the merge report.

### Product merge-quality acceptance

1. An ignored `foch-cli` integration test selects exact immutable snapshot IDs.
2. The library materializes one content-bound base view from the installed
   inventory; both product execution and scoring use that view instead of the
   mutable game tree.
3. Each case launches the public `foch merge --non-interactive` artifact in an independent bounded child process.
4. The runner verifies the product-authored execution attestation; the library
   rechecks the pinned base and archives the emitted tree.
5. `foch-merge-quality` scores that existing tree without selecting or executing a merge kernel.
6. Exact V2 cohort reports and assertions remain separate from frozen V1 Legacy evidence.

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
