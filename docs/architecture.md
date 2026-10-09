# Architecture

This page describes the current source boundaries. Historical documents may
still contain the superseded `crates/foch-*` split; those names are not current
architecture.

## Repository layout

The repository is a Cargo and Bun workspace. The root Cargo manifest only
configures the workspace. All maintained code lives below `src/`; the Rust domain
model and orchestration remain one primary package at `src/packages/foch`.
Its integration tests, independent cargo-fuzz package and build script live
alongside its `src/`. External CWT rules remain in top-level `vendor/`; the
build script compiles them into the binary. Source releases contain the entire
workspace and its public submodules, without private notes.

### Main `foch` library

| Path | Responsibility |
| --- | --- |
| `src/packages/foch/src/model` | Shared reports, findings, identities, and serialization models |
| `src/packages/foch/src/project` | `foch.toml`, dependency overrides, resolution policy, and project fingerprints |
| `src/packages/foch/src/playset` | Launcher playsets, descriptors, dependencies, and Steam installation identity |
| `src/packages/foch/src/input` | Read-only inspection, input resolution, inventories, and mod snapshots |
| `src/packages/foch/src/game/schema` | Reusable CWT loading, compilation, querying, and rule evaluation |
| `src/packages/foch/src/game/eu4` | Concrete EU4 parsing, content families, semantic indexing, base snapshots, and editor behavior |
| `src/packages/foch/src/check` | Semantic checks and runtime overlap analysis |
| `src/packages/foch/src/graph` | Definition, call, module, and mod dependency graph output |
| `src/packages/foch/src/simplify` | Base-equivalent definition removal |
| `src/packages/foch/src/merge` | Path planning, semantic-tree kernel, analysis, review ledger, resolution, and commit |
| `src/packages/foch/src/platform` | Filesystem and cache lifecycle services |

`src/packages/foch/src/game/schema` is reusable infrastructure; it is not a supported game by
itself. Foch remains EU4-only. A future game must add and verify its own loader,
content-family, base-data, and merge behavior instead of treating CWT coverage
as proof of compatibility.

### Applications and packages

- `src/apps/foch-cli` owns the `foch` binary, CLI adapters, terminal conflict
  UI, the read-only analysis browser opened by bare `foch`, integration tests,
  and the test-only merge-quality harness. `foch lsp` starts the server from
  `foch-lsp`; `foch test` plans with `foch-test`, launches the game through
  `foch-runner`, and owns only file discovery, output protection, the isolation
  choice and reporting.
- `src/apps/foch-desktop` owns the Tauri/React player interface and IPC adapters.
  Its Rust backend links `foch` directly; it does not spawn or bundle the CLI.
- `src/packages/foch-lsp` is the language server library linked into `foch`.
  It knows Foch annotations only through `foch-annotation`, never the test
  runner.
- `src/packages/foch-annotation` reads Foch annotations carried in EU4
  comments (`#test(...)`, `#skip`, ...): extraction, schemas, typed values,
  validation and byte-offset completion/hover. An annotated mod stays a valid
  mod that EU4 loads without Foch; this crate never changes loaded script.
  Extensions that must be compiled into native script are not annotations and
  will get their own crate when the first one exists.
- `src/packages/foch-test` gives `#test` annotations and `tests/` blocks their
  meaning: collection, selection, session planning, test-layer compilation,
  judging and reports. It performs no I/O.
- `src/packages/foch-runner` launches a real EU4 install to execute a test
  bundle: it reuses the main library's game detection, builds a short-path
  runtime layer (executable and loose files copied, data directories linked)
  so the install stays read-only, assembles a throwaway user directory, runs
  the game with AI disabled, watches its log, and returns a result for
  `foch-test` to judge. It keeps the player's real profile intact by backing up
  and verifying the few files the game rewrites.
- `src/packages/tree-sitter-paradox` is the independently versioned grammar and a
  Cargo/Bun workspace member.
- `src/apps/vscode-foch` is the independently versioned VS Code extension. It
  launches a bundled `foch lsp` process.

There are no current `foch-core`, `foch-syntax`, `foch-cwt`, `foch-language`,
`foch-engine`, `foch-merge-kernel`, or `foch-merge-quality` packages. Do not
reintroduce package seams merely to recreate those names.

## Dependency direction

```text
foch-cli ----> foch <-------- foch-desktop
  |              ^  +-- game/schema (reusable CWT machinery)
  |              |  +-- game/eu4    (the only concrete game)
  |--> foch-lsp ----> foch-annotation --> foch
  |--> foch-test ---> foch-annotation
  |--> foch-runner --> foch-test, foch

vscode-foch -----> foch lsp
foch -----------> tree-sitter-paradox
```

Applications adapt the main library to a transport or UI. Domain behavior does
not depend on CLI/Tauri types. Merge-quality code is test-only and exercises the
same public product path rather than becoming another engine layer.

## Input flow

1. `src/packages/foch/src/input` inspects a `dlc_load.json` or `[project]` manifest and resolves
   the game root, ordered mod contributors, descriptors, and installed Steam
   identities.
2. Inspection is read-only. It does not initialize configuration, update ACF
   files, copy Workshop trees, or install base data.
3. A ready inspection can produce an `InputRequest` that retains the exact
   inspected playset and base-snapshot identity.
4. Downstream inventory and semantic work preserve playset order. Order is a
   semantic input and is never sorted merely to stabilize a cache key.

The analyzed EU4 base snapshot is the ancestor for supported structural merges.
A missing base file is not an empty ancestor unless that content family has an
explicit verified policy.

## Check, graph, and simplify

`foch check` resolves an input, parses its documents through the EU4 layer,
builds semantic indexes, and adds runtime binding/overlap findings before
rendering a report.

`foch graph` uses the same resolved semantic state to write call,
definition-dependency, module, mod-dependency, or family graph artifacts.

`foch simplify` compares the target mod with effective base definitions and
writes a separate simplified tree. Source mods and game files remain read-only.

## Merge lifecycle

The public lifecycle is **inspect → analyze → review → confirm → commit**.

### Analyze

`src/packages/foch/src/merge/analyze.rs` performs all semantic work before the output target is
modified:

1. resolve and inventory the exact input;
2. classify paths and build definition-module views;
3. run the semantic-tree backend and configured conflict handlers;
4. materialize generated bytes into a Rust-owned artifact tree;
5. re-parse and semantically validate that tree; and
6. freeze artifacts plus input, base-snapshot, and prior-output guards.

Analysis reports structured progress across inventory, input resolution,
semantic merge, validation, and artifact freezing. Cancellation is cooperative
between stages and bounded units.

### Review

Every path-plan entry must resolve exactly once into a stable review unit. A
unit is either a file or a definition module and records:

- stable ID, normalized path, family, and strategy;
- disposition: `safe`, `copy`, `needs_user_choice`, `unsupported_input`,
  `engine_failure`, or explicit `deferred`;
- ordered contributors and base-game participation;
- whether an output path exists; and
- concise notes suitable for a bounded UI projection.

The review ledger rejects duplicate IDs/output paths, double resolution, and
pending units. Cross-file pruning removes only the output path; it does not
rewrite the unit's semantic disposition.

### Commit

`AnalyzedMerge::commit` does not rerun a merge backend. It revalidates the
frozen input identities, installed base snapshot, and any reviewed existing
output bytes, then atomically installs the frozen artifact tree. Replacing a
non-empty directory requires a separately fingerprinted authorization.

`needs_user_choice`, `unsupported_input`, and `engine_failure` units are
withheld while unrelated safe units may still commit. `--force` affects only
supported user-choice fallbacks. A `partial_success` report is therefore a
valid product result, not an implicit global failure.

No `MergeSession` abstraction is implemented in this slice. Long-lived
interactive session ownership remains deferred until the analysis/review
surface proves it is needed.

## Desktop boundary

The desktop backend exposes bounded DTOs rather than serializing `AnalyzedMerge`
or raw artifact/report structures. The first analysis surface contains six
commands:

- `inspect_input`
- `start_merge_analysis`
- `cancel_merge_analysis`
- `get_merge_analysis_summary`
- `list_merge_units`
- `get_merge_unit`

Only one analysis may be queued or running. Terminal analyses are kept in a
small FIFO, and unit listing is filtered and paginated before crossing IPC.
There is intentionally no commit/export command in this checkpoint.

## Cache boundary

Persistent cache formats and payload identities are owned by their domain
modules; `src/packages/foch/src/platform/cache_store` only provides filesystem lifecycle
operations. Opening a current generation must not delete other generations.
Eviction and clearing are explicit maintenance operations. See
[cache-architecture.md](cache-architecture.md).

## Merge-quality acceptance

The test-only harness under `src/apps/foch-cli/tests/merge_quality/` owns the fixed
14-case, 26-item Workshop denominator, append-only V2 records, evidence capture,
scoring, and reports. The supported operator entrypoint is:

```text
cargo acceptance
```

It reads installed Workshop content in place and binds identity to paired Steam
ACF records. It does not recursively hash or copy whole mod trees for normal
acceptance. A complete cohort is required for a product-quality claim; an
interrupted append-only run remains measurement history, not a baseline.

## Editor boundary

The reusable CWT layer compiles schemas, while `src/packages/foch/src/game/eu4/editor` interprets
them for EU4 diagnostics, completion, hover, and navigation. CWT rules remain
evidence: runtime load order and merge semantics stay in concrete EU4 content
families rather than in a generic schema package.
