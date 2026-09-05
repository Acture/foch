# Project Status

Latest focused verification: 2026-09-05, P-553/P-556 static-modifier product
fixtures and bounded Workshop observation. See
[the verification record](./static-modifiers-verification.md).

Earlier project-wide source verification: 2026-08-25 on branch `refactor/structure-reset` at
`30aa902` (`Update quality harness for merge reviews`).

This page is the repository handoff. Recheck Git and local inputs before using
any checkpoint fact. Linear owns live execution; Notion holds the project
narrative and research record.

## Product goal

Foch takes an ordered EU4 playset, preserves contributions whose loader
semantics it understands, produces a deterministic merged mod, and reports real
ambiguity instead of hiding it behind an arbitrary winner.

The current source line is an unreleased EU4-only alpha at `0.0.1`. It is not a
reliable one-click merger for arbitrary modlists. The active product direction
is one root Rust library with a `foch` CLI and a player-facing Tauri desktop
application. The fixed 14-case Workshop cohort remains the product merge-quality
gate; it does not establish desktop product readiness.

## Static-modifier product checkpoint

P-553 reproduced safe output that summed unchanged vanilla values into independent
mod changes. `common/static_modifiers` now uses conflict semantics for divergent
final values; independent field changes, equivalent contributions and explicit
dependency adapters remain automatically mergeable. P-556 also excludes unchanged
carriers from final value and delete/modify candidates while retaining complete
input evidence, the ancestor and real deletion choices.

The shared synthetic matrix exercises public analyze/commit and CLI
preview/confirmation, reparses actual output and checks ordering, provenance,
omission, stable IDs and input immutability. A bounded installed RCE/EE probe also
preserved selected contributions within the same definition. Commands, precise
observations and limitations are in the verification record above. No new full
Workshop cohort or EU4 runtime acceptance is claimed.

## Structural-reset checkpoint

The committed range from `705a854` through `2870816` established the new source
shape:

- the root `foch` package owns shared models plus input, check, graph, simplify,
  merge, and platform behavior;
- reusable CWT machinery lives under `src/game/schema`;
- concrete loader, parser, content-family, base-data, and editor behavior lives
  under `src/game/eu4`;
- the semantic-tree kernel and higher-level merge orchestration live under
  `src/merge`;
- the full merge-output cache was removed and owner-specific caches moved next
  to the input, schema, parser, or merge behavior that defines their identity;
- merge execution is split into complete read-only analysis and guarded commit;
- the desktop frontend has the typed six-command client, input-readiness view,
  analysis progress/cancellation UI, and searchable paginated review browser;
- the CLI is under `apps/foch-cli`, the desktop under `apps/foch-desktop`, and
  the merge-quality harness under `apps/foch-cli/tests/merge_quality`; and
- the superseded `foch-core`, `foch-syntax`, `foch-cwt`, `foch-language`,
  `foch-engine`, `foch-merge-kernel`, and `foch-merge-quality` packages are gone.

Foch remains EU4-only. The reusable schema boundary is preparation for future
game implementations, not a claim that another Paradox game is supported.

## Product-analysis checkpoint

The committed range from `e4b1a9c` through `30aa902` builds on the structural
reset:

- `inspect_current_eu4_input` reads the current EU4 installation, base-data
  identity, Launcher playset descriptors, paired Workshop ACF identities, and
  Workshop descriptors without initializing configuration or cache state;
- the inspected load order, exact game root, Workshop identities, and base
  snapshot lease are frozen into the later `InputRequest`; missing, invalid, or
  path-escaping Launcher descriptors block readiness;
- every planned merge unit now has exactly one stable review outcome: `safe`,
  `copy`, `needs_user_choice`, `unsupported_input`, `engine_failure`, or
  `deferred`;
- `foch merge` displays that complete review before confirmation; the separate
  `merge-plan` command is gone, and `foch input inspect` is the read-only input
  command;
- normal cache open, lookup, and store paths do not prune generations or apply
  maintenance byte caps. Eviction and cleanup happen only through explicit
  `foch cache clean` or `foch cache clear` operations, including the legacy
  parser-cache root;
- the LSP/VS Code public setting is `fochLsp.projectManifest` with environment
  override `FOCH_LSP_PROJECT_MANIFEST`; no compatibility alias remains; and
- the desktop backend implements the six inspection/analysis/query commands.
  The most recent Ready inspection's exact request is atomically bound to its
  analysis ID, while blocked reinspection and queued cancellation discard the
  token.

The exact commits are:

- `e4b1a9c` — exact current-EU4 input inspection;
- `7567dce` — per-unit merge review ledger;
- `d593699` — strict Launcher descriptors for current-input readiness;
- `b8db81a` — desktop merge-analysis commands;
- `f9041db` — CLI, cache, and project-manifest terminology cleanup; and
- `30aa902` — merge-quality harness alignment with review output.

No desktop commit/export command or durable `MergeSession` has been added.
Session design remains deferred.

## Verification through `30aa902`

The current source checkpoint passed:

```fish
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo check --manifest-path fuzz/Cargo.toml --all-targets --all-features
git diff --check
node --check packages/vscode-foch/extension.js
```

The final full test run passed with these principal counts:

- root library: 1,145 passed / 10 ignored;
- merge E2E: 31 passed / 2 ignored;
- CLI library: 36 passed;
- CLI integration: 41 passed;
- fixed-corpus harness: 110 passed / 2 ignored; and
- desktop backend: 16 passed.

Focused input, review-ledger, materialization, defer, LSP, cache, architecture,
and binary-contract tests also passed. Every source commit above passed the
installed pre-commit hook: Rust format, strict workspace clippy, and workspace
test build.

During the final `2870816` commit, the hook's format and strict clippy phases
passed, but its redundant workspace build exhausted local disk space. The
repository's documented emergency `FOCH_SKIP_PRE_COMMIT=1` switch was used only
after the stronger independent gates above had passed. No hook was bypassed
with `--no-verify`.

The full frontend bundle/type/lint/Vitest gates were not rerun because this
clone has no installed `node_modules`, and dependencies were not installed as
part of this source reset. The packaged Windows application and its CI smoke
have not run. The long fixed 14-case Workshop acceptance cohort was also not
run.

## What works at the current checkpoint

### Merge lifecycle

- `foch merge` analyzes the complete semantic result, prints every review unit,
  and leaves the requested output directory untouched until confirmation.
- The analyzed artifact tree, report, input identity, base snapshot, and any
  reviewed prior-output bytes are frozen before confirmation.
- Commit revalidates those guards and atomically installs the frozen bytes; it
  does not run the semantic backend again.
- Replacing a non-empty target requires separate, fingerprinted authorization.
- Safe files or complete definition modules may commit while unsafe units are
  withheld. `partial_success` is a valid product result.
- `--force` applies only to supported `needs_user_choice` fallbacks.

### Inputs and trust

- Playset order and declared dependencies are semantic inputs.
- Source mods and the game installation are read-only.
- Workshop installation identity comes from paired
  `appworkshop_236850.acf` records; normal product work does not recursively
  hash or copy entire Workshop trees.
- The analyzed EU4 base snapshot is the semantic ancestor for supported
  structural merges.
- Desktop analysis consumes the same exact input token that produced the latest
  Ready inspection instead of silently rediscovering the playset or game root.

### Repository products

| Path | Responsibility |
| --- | --- |
| `src/` | Root `foch` library and concrete EU4 implementation |
| `apps/foch-cli` | `foch`, `foch lsp`, CLI integration tests, merge-quality harness |
| `apps/foch-desktop` | Player-facing Tauri application linked directly to `foch` |
| `packages/tree-sitter-paradox` | Independently versioned grammar |
| `packages/vscode-foch` | Independently versioned VS Code extension using `foch lsp` |

## What is not yet proven

- No complete current 14-case product cohort has been accepted. Product quality
  across the fixed denominator is therefore unknown.
- Product acceptance re-parses and semantically scores generated output, but it
  does not launch EU4 or prove in-game playability.
- Games other than EU4 do not have verified loader, content-family, base-data,
  or merge behavior.
- The desktop source implements input inspection and review browsing, but no
  packaged Windows workflow has been verified.

## Measurement records

The V2 JSONL files under `apps/foch-cli/tests/merge_quality/data/` are
append-only and resumable per case. An interrupted cohort is valid measurement
history but not an accepted baseline. Only a complete cohort for the current
product artifact, runner, kernel, scope, and scorer may support a quality
claim.

Do not stage, restore, truncate, delete, or rewrite dirty measurement records
without first establishing their identity and getting the user's decision.
Installed local availability must not shrink the fixed 14-case, 26-item
denominator.

## Execution tracking

Active milestones, issues, dependencies, and acceptance criteria live in the
Linear `foch` project. This file records verified repository state and evidence;
do not reconstruct an execution backlog here.

## Fresh-agent runbook

1. Read this page, [architecture](./architecture.md), and
   [merge design](./merge-design.md).
2. Inspect `git status --short --branch` and `git log -3 --oneline`. Preserve
   unrelated changes and append-only measurement history.
3. Distinguish committed implementation, local worktree observation, recorded
   verification, and accepted product evidence.
4. Check Linear first for the current issue, dependencies, and blockers. Use
   Notion only for project narrative or research context.
5. Run focused tests before workspace gates. Update this page when a verified
   product fact changes and write execution status back to Linear.
6. Never use `--no-verify`, mutate source mods/game files, or claim a cohort
   passed until the supported wrapper validates it.

## Reading order

1. [README](../README.md)
2. [Architecture](./architecture.md)
3. [Merge design](./merge-design.md)
4. [Merge-quality dataset](./merge-quality-dataset.md)
5. [Cache architecture](./cache-architecture.md)
6. [Project manifest](./foch-project-manifest.md)
7. [Resolution DSL](./foch-toml-resolutions.md)
8. [Known issues](../KNOWN_ISSUES.md)

The structured-merge shadow, common-applicability probe, reviews, and research
notes are historical or auxiliary evidence. They are not the active backlog or
current architecture.
