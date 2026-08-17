# Project Status

Last verified: 2026-08-17 against source commit `3924bc3`

This is the repository handoff for contributors and fresh agents. It is
self-contained: Notion page **foch — Merge Corpus & Game Semantics** tracks live
ownership when available, but access to Notion is not required to understand
the product state or choose work.

## Product Goal

Foch takes an ordered EU4 mod playset, preserves contributions whose load
semantics it understands, produces a deterministic merged mod, and reports real
ambiguity instead of silently choosing a winner.

The current source line is an unreleased EU4-only alpha at `0.0.1`. It is not a
reliable one-click merger for arbitrary modlists. The next product proof is a
complete run of the fixed 14-case Workshop acceptance workflow with the current
public `foch` binary and scorer.

## Current Checkpoint

| Area | Verified fact |
| --- | --- |
| Source | `master` was one commit ahead of `origin/master`; HEAD was `3924bc3` and had not been pushed |
| Normal gates | Format, strict workspace clippy, build, and workspace tests passed for `3924bc3` |
| Product acceptance | No complete current runner-v5 / scorer-2.1 14-case cohort has been accepted |
| Partial records | Four tracked JSONL files contain older incomplete runs: 8 unique cases and 9 measurements, all with merge summary `blocked` |
| Current local inputs | A directory-only check found 24 of the fixed 26 Workshop items; `1596815683` and `2172666098` were absent and must be resolved before the official gate can pass preflight |
| Focused merge evidence | Bounded `state_edicts` export is `ready`; this is a regression check, not cohort acceptance |
| Analyzer coverage | Last recorded real-game analyzer probe was `parse_only=60`, `semantic_complete=69`; this is separate from merge quality |

Re-run `git status --short --branch` and `git log -3 --oneline` before relying
on source or worktree observations. Re-run the official Workshop preflight
instead of treating the directory-only availability check as authoritative.

## What Works Today

### Merge behavior

- `foch merge` is preview-first. By default it prints a path-level plan, does
  not touch `--out`, and exits successfully.
- `--confirm` authorizes export from that prepared session. It does not
  authorize replacing a non-empty output directory; that requires a separate
  TTY confirmation. Batch jobs must use a new or empty output path.
- `--non-interactive` disables prompts but does not imply `--confirm`.
- Confirmed export writes every safe file or definition module and withholds
  unsafe units. `partial_success` is a successful product result.
- Deferred reasons are `needs_user_choice`, `unsupported_input`, and
  `engine_failure`. `--force` applies only to supported user-choice fallbacks.
- The production merge kernel is the semantic tree. The legacy address-patch
  path remains evaluation history, not the public product.

### Inputs, cache, and product reports

- Workshop mods are read directly from their installed directories. Foch does
  not copy them into an input CAS.
- Workshop version identity comes from the paired Steam
  `appworkshop_236850.acf` `(app_id, workshop_id, manifest_id)` records. The
  acceptance workflow revalidates those identities; it does not recursively
  hash every mod tree.
- Full-product output currently bypasses the retained-path modset output cache.
  Parser and semantic snapshot caches still operate.
- Confirmed output is re-parsed and semantically checked before publication.
  Reports record the kernel, input, base snapshot, deferred units, and scoring
  evidence.

### Repository layout

The repository has one normal executable, `foch`. The language server is the
`foch lsp` subcommand. Parser-maintainer tools are feature-gated examples, and
the merge-quality harness is a library/test package rather than another
product binary.

| Path | Responsibility |
| --- | --- |
| `crates/foch-cli` | CLI, `foch`, `foch lsp`, product integration tests |
| `crates/foch-core` | Shared domain models, reports, and utilities |
| `crates/foch-syntax` | Shared syntax-tree types used by schema tooling |
| `crates/foch-cwt` | CWT schema loading, compilation, and rule evaluation |
| `crates/foch-language` | Parsing, semantic indexing, `ContentFamily`, EU4 behavior |
| `crates/foch-engine` | Workspace/cache and merge/check/graph/simplify orchestration |
| `crates/foch-merge-kernel` | Game-independent semantic-tree matching and N-way merge |
| `crates/foch-merge-quality` | Private fixed-corpus measurement, scoring, and evidence |
| `packages/tree-sitter-paradox` | Independently versioned grammar package |
| `packages/vscode-foch` | Independently versioned VS Code extension using `foch lsp` |

## What Is Not Yet Proven

- The current public merge path has no complete accepted 14-case product
  baseline, so its quality across the fixed corpus is unknown.
- The preview is a path-level plan. Some leaf conflicts and deferred units are
  discovered only during confirmed export, so preview does not yet show every
  semantic outcome that may appear in the final report.
- Acceptance re-parses and scores the generated mod, but it does not launch EU4
  and enter a game. Runtime playability remains a separate manual check.
- Games other than EU4 do not have trusted `GameProfile` and `ContentFamily`
  coverage.

## `replace_path` Outlier

`replace_path` is uncommon in the fixed corpus. Of the 24 currently present
Workshop item directories, 22 descriptors contain none. The two exceptions are
total conversions:

| Workshop item | Mod | `replace_path` lines |
| --- | --- | ---: |
| `1449952810` | Elder Scrolls Universalis | 107 |
| `1796527319` | World of Warcraft Universalis | 108 |

They appear in two of the fourteen logical cases. A focused `custom_ideas`
diagnostic involving those mods was stopped after more than 6m39s while Foch
inspected many base-game content folders replaced by the total conversions.
This is a real outlier, but it is not evidence that ordinary mods commonly use
`replace_path` or that this is the repository's primary architecture problem.

Do not redesign the merge pipeline around this observation. If a current
acceptance run times out in either affected case, optimize that measured path
with a focused regression and verify that the other cases are unchanged.

## Measurement Records and Interrupted Runs

The V2 JSONL files are append-only measurement logs. Completing one case writes
its stable input, observation, file results, and terminal measurement so an
interrupted expensive run can resume without repeating valid work.

The measurement identity includes the input version, `foch` artifact, runner
protocol, kernel, scope, scorer version, and scorer configuration. A rerun
reuses only an exact matching measurement and executes missing cases. A report
sets `baseline_complete` only when every selected logical case has a terminal
record. Therefore a partial cohort is not corruption and cannot be mistaken for
an accepted complete baseline.

The current uncommitted additions are:

- `input_versions.jsonl`: 8
- `observations.jsonl`: 9
- `measurements.jsonl`: 9
- `file_results.jsonl`: 223

They came from incomplete 1/14 and 8/14 runs using the older runner protocol
and scorer 2.0. They cannot satisfy the current runner-v5/scorer-2.1 cohort.
They remain an explicit worktree decision: do not stage, restore, truncate, or
delete them unless the user chooses whether to retain those partial logs.

## Next Work

1. Restore or otherwise resolve the two missing fixed Workshop items. The
   official discovery/ACF preflight, not the directory count above, decides
   whether the 26-item input contract is satisfied.
2. The user runs the only product acceptance entrypoint:

   ```fish
   scripts/merge-quality/acceptance.fish
   ```

   The workflow is intentionally resumable and may append one case at a time.
   Do not replace it with a raw `cargo test -- --ignored` invocation.
3. Classify every non-accepted result from the complete current cohort. Fix one
   reproducible cause at a time with a focused fixture and a bounded real case.
4. If either total-conversion case times out, address that specific
   `replace_path` performance path. Do not generalize it to ordinary mods
   without further evidence.
5. Separately improve the preview so users can see more semantic conflicts
   before confirming export. Use existing plan/report terminology; no internal
   representation has been selected for that work.

## Fresh-Agent Runbook

1. Read this page and the task-specific reference below. Check Notion for live
   ownership if access is available.
2. Inspect `git status --short --branch` and `git log -3 --oneline`. Preserve
   unrelated changes, especially the four measurement logs.
3. Distinguish committed implementation, recorded verification, current
   worktree observations, and accepted product evidence.
4. Choose work from a fresh acceptance failure or a clearly reproduced focused
   issue. Do not promote a historical probe into the active roadmap.
5. Run focused tests first, then the normal Rust gates. Update this page and
   Notion when a product gate or decision changes.
6. Commit or push only when the user asks. Never bypass repository hooks.

Do not:

- describe partial append-only measurements as dataset corruption;
- invent architecture names in the status document before corresponding code
  and a demonstrated need exist;
- treat the two total-conversion descriptors as representative of normal mods;
- quote frozen V1 or incomplete V2 rows as current product quality;
- shrink the fixed 14-case denominator to installed local inputs;
- make a 14 GB CAS scrub part of normal acceptance; or
- claim the full cohort passed until the wrapper validates it.

## Verification

The recorded `3924bc3` checkpoint passed:

```fish
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --tests
cargo test --workspace
fish -n scripts/merge-quality/acceptance.fish
git diff --check
```

These are recorded results for the source checkpoint, not a claim that a fresh
agent reran them after documentation-only edits.

## Reading Order

1. [README](../README.md) — user-facing product and CLI boundary.
2. [Architecture](./architecture.md) — package and execution layout.
3. [Merge design](./merge-design.md) — merge strategies, artifacts, and conflict
   policy.
4. [Merge-quality dataset](./merge-quality-dataset.md) — fixed cohort,
   identity, resume behavior, evidence, and acceptance contract.
5. [Cache architecture](./cache-architecture.md) — cache and trust boundaries.
6. [Workspace manifest](./foch-workspace-manifest.md) and
   [resolution DSL](./foch-toml-resolutions.md) — user configuration.
7. [Known issues](../KNOWN_ISSUES.md) and
   [release checklist](./RELEASE_CHECKLIST.md) — current limits and release
   gates.

[Auto-merge roadmap](./auto-merge-roadmap.md),
[structured merge shadow](./structured-merge-shadow.md), and
[common applicability](./common-applicability-probe.md) are historical or
auxiliary records. They are not the active task queue or product acceptance
gate.
