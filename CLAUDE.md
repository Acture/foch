# AGENTS.md

## Project Background

Foch is an unreleased, EU4-only mod analysis and merge tool. It consumes an
ordered playset, models the parts of Europa Universalis IV's loader semantics
that the project has verified, preserves compatible contributions from the
source mods, and produces a separate deterministic merged mod. Genuine
ambiguity must be reported for review instead of being hidden behind an
arbitrary winner.

This is not a generic three-way text merger. EU4 resources can be defined
across files, so exact-path overlap is neither necessary nor sufficient for a
semantic conflict. Depending on the content family, the relevant unit may be a
file, a top-level definition, or a folder-wide definition module. Human-made
compatibility patches are practical comparison evidence, not an infallible
specification of every file.

The product goal is real merge correctness on ordinary mod playsets. Parser
coverage, cache speed, research metrics, and editor features support that goal;
none substitutes for product merge evidence.

## Product Contracts

- EU4 is the only supported game. Do not imply support for other
  Paradox games until their loader behavior and content families are verified.
- Reusable CWT parsing, compilation, and rule evaluation belong in
  `src/game/schema`. Concrete EU4 interpretation belongs in `src/game/eu4`,
  with `ContentFamilyDescriptor` as the analyzer behavior boundary. CWT
  schemas are useful evidence, but they do not by themselves prove runtime
  load or merge semantics.
- Playset order and declared mod dependencies are semantic inputs. Preserve
  their precedence in input resolution, merge DAGs, cache identities, and
  tests; never sort mods merely to make a key deterministic.
- The analyzed EU4 base snapshot is the semantic ancestor for supported
  structural merges. Do not treat a missing vanilla file as an empty ancestor
  unless that EU4 content-family descriptor explicitly supports a verified
  empty base.
- `foch merge` is analyze-first. It resolves and freezes the input, computes
  the complete semantic result, and exposes review units before confirmation.
  Commit writes all safe files or complete definition modules and defers only
  unsafe units. `partial_success` is a valid product result; `--force` applies
  only to supported `needs_user_choice` fallbacks.
- `--confirm` does not authorize replacement of a non-empty output directory.
  That has a separate TTY confirmation, so non-interactive jobs must use a new
  or empty output path.
- Source mods and the game installation are read-only inputs. Foch reads
  installed Workshop mods in place and writes a separate output mod; never
  mutate, normalize, or copy source mod trees as an implementation shortcut.
- Workshop version identity comes from the paired Steam
  `appworkshop_236850.acf` records. Normal acceptance does not recursively hash
  or copy entire Workshop trees into an input CAS. A whole-tree integrity scan
  is an explicit audit with its I/O cost stated up front, not a hidden merge or
  acceptance prerequisite.
- The only command-line executable is `foch`; the language server is `foch
  lsp`. The separate `foch-desktop` application links the root `foch` library
  directly and must not spawn or bundle the CLI. Parser-maintainer tools are
  feature-gated examples. Merge-quality workflows belong in the CLI's exact
  integration-test harness, not a production library or separate binary.
- Product acceptance re-parses and semantically scores the generated mod, but
  it does not launch EU4 or prove in-game playability. Runtime playability is a
  separate manual check.

## Merge-Quality Evidence

The current product acceptance denominator is the committed fixed 14-case
Workshop cohort containing 26 unique items. Installed local availability must
not shrink that denominator. The supported entrypoint is:

```fish
scripts/merge-quality/acceptance.fish
```

That denominator defines the gate; it does not mean a current cohort has
passed, and it is not a statistical claim that every case represents an
ordinary modlist. Read `docs/project-status.md` for current input availability
and accepted evidence, and do not generalize a single corpus outlier into a
project-wide architecture requirement.

This is a long, real-Workshop run for the maintainer to execute manually.
Agents should use focused fixtures and bounded real-case probes while
developing, then hand off the wrapper command instead of launching it
unannounced.

The V2 JSONL streams under `apps/foch-cli/tests/merge_quality/data/` are
append-only and intentionally resumable per case. An interrupted partial cohort
is valid measurement history, not corruption, but it is not an accepted
baseline. Only a complete cohort for the current product artifact, runner,
kernel, scope, and scorer may support a product-quality claim. Never restore,
truncate, stage, or rewrite dirty measurement records without first
establishing their identity and getting the user's decision.

Frozen V1 objects, scorers, and historical roadmap numbers are research
history. Do not quote them as current `semantic_tree` quality and do not make
the legacy object store part of the current product path.

## Working Context and Evidence Discipline

- Read `docs/project-status.md` before selecting work. It is the self-contained
  current handoff; re-check Git and local inputs because its checkpoint facts
  can age.
- Check the Notion page **foch — Merge Corpus & Game Semantics** for current
  ownership, decisions, and blockers when access is available. Keep stable
  project background here and rolling results in project status and Notion.
- Distinguish committed implementation, a local worktree observation, a
  recorded test result, and an accepted product cohort. Never promote one into
  another.
- Prefer fixes driven by a fresh acceptance failure: reproduce one cause with
  a focused regression, fix it at the correct EU4 content-family or kernel
  boundary, run focused gates, then re-check a bounded real case.
- Do not invent architectural names in status or planning documents before a
  corresponding code boundary and demonstrated need exist. Use terms already
  present in the source and reports.
- Treat historical roadmaps, reviews, and probes as evidence only. They are not
  the active backlog unless current status or Notion explicitly revives them.

## Project Structure & Module Organization

`foch` is a workspace monorepo with one primary Rust library:

- `src/` — project/input models, check/graph/simplify orchestration, merge
  analysis/review/commit, platform services, reusable CWT schema machinery, and
  concrete EU4 semantics
- `apps/foch-cli` — the `foch` binary, `foch lsp`, CLI integration tests,
  fixed-corpus merge-quality harness, and feature-gated maintainer examples
- `apps/foch-desktop` — the player-facing Tauri application, linked directly to
  the root library without a CLI sidecar

JS packages live under `packages/`:

- `packages/tree-sitter-paradox` — grammar package
- `packages/vscode-foch` — VS Code extension

Use `tests/` and package-local `tests/fixtures/` for integration fixtures and
corpus-style checks, `docs/` for architecture and status docs, and `scripts/`
for operator workflow wrappers.

## Build, Test, and Development Commands

- `cargo fmt --all --check` — verify Rust formatting
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — strict Rust linting
- `cargo test --workspace` — run the Rust test suite
- `bun install --frozen-lockfile` — install workspace JS dependencies
- `bun run --cwd packages/tree-sitter-paradox test` — test the grammar package
- `bun run --cwd packages/vscode-foch smoke` — smoke-test the VS Code extension
- `set EU4_ROOT "$HOME/Library/Application Support/Steam/steamapps/common/Europa Universalis IV"; target/debug/foch data build eu4 --from-game-path "$EU4_ROOT" --game-version auto --output-dir /tmp/foch-probe` — default macOS Steam probe command to hand off after analyzer changes

## Coding Style & Naming Conventions

Use tabs in repo-authored code unless a file already uses another style.
Prefer small composable helpers over deep hierarchies. In Rust, use
`snake_case` for functions/modules and `UpperCamelCase` for types. Keep
`ContentFamilyDescriptor` as the EU4 analyzer behavior boundary;
`ScriptFileKind` is only a compatibility label. Delete dead code instead of
leaving compatibility shims.

## Testing Guidelines

Add unit or regression tests with every semantic-family change. For coverage work, update both semantic-index tests and `base_data` coverage assertions. When changing root classification or extraction, run a real EU4 probe and record the new baseline in `docs/project-status.md`.

Coverage-reset execution should stay narrow: promote one root per issue, keep local validation green first, then treat the manual full EU4 probe as the acceptance gate before calling the slice verified complete.

For merge changes, start with the smallest relevant merge/game/CLI test,
add a regression for the observed semantic cause, then run the owning package and
CLI integration tests. Do not update expected corpus output merely to make a
failure green; adjudicate why the product and the human compatch differ.

Keep changing coverage status in `docs/project-status.md` and Notion. `AGENTS.md` should hold stable execution guidance, not rolling project baselines.

## Environment & Configuration

Use `direnv` in the repo root and keep Node on the supported line: `>=22 <25`. Run `direnv allow` once after cloning. `node@25` is currently not a supported local development environment for `packages/tree-sitter-paradox`.

## Local quality gates

Run `bash scripts/install-hooks.sh` once after cloning to install the local git hooks. The pre-commit hook runs `cargo fmt --all --check`, strict workspace clippy, and `cargo build --workspace --tests`; the pre-push hook runs the full `cargo test --workspace` suite. Override only in emergencies with `FOCH_SKIP_PRE_COMMIT=1 git commit ...` or `FOCH_SKIP_PRE_PUSH=1 git push ...`, and make the next push without skipping the gate. Agents in autopilot or fleet mode must install hooks before doing work and must not use `--no-verify` to bypass them.

## Commit & Pull Request Guidelines

Follow the existing commit style: short, imperative subjects such as `Promote low-risk common mechanics roots` or `Split and promote random map content families`. Keep one logical change per commit. PRs should include a concise summary, linked Notion task or decision when applicable, validation commands run, and any probe delta (`parse_only` / `semantic_complete`) when analyzer coverage changes. Add screenshots only for `packages/vscode-foch` UI changes. Do not add AI co-author trailers.
