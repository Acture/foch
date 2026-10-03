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
  `src/packages/foch/src/game/schema`. Concrete EU4 interpretation belongs in `src/packages/foch/src/game/eu4`,
  with `ContentFamilyDescriptor` as the analyzer behavior boundary. CWT
  schemas are useful evidence, but they do not by themselves prove runtime
  load or merge semantics.
- The CWT schema decides output bytes (numeric canonicalization reads its
  field types), so `src/packages/foch/build.rs` compiles `vendor/cwtools-eu4-config` into a rule
  pack the binary embeds, and its `cwt_schema_id` enters
  `analysis_rules_version`, the mod-snapshot cache key, `foch --version` and
  the merge-quality scorer identity. `FOCH_CWTOOLS_SCHEMA_DIR` is a
  parser-maintainer override that compiles another directory, uncached, in
  every process; it changes output bytes, fails loudly when the directory does
  not load, and merge-quality product runners refuse to start under it. It
  also changes `analysis_rules_version`, so pair it with its own
  `FOCH_DATA_DIR` rather than rebuilding the shared base data.
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
  lsp`. The separate `foch-desktop` application links the main `foch` library
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

```text
cargo acceptance
```

That denominator defines the gate; it does not mean a current cohort has
passed, and it is not a statistical claim that every case represents an
ordinary modlist. Read `docs/project-status.md` for current input availability
and accepted evidence, and do not generalize a single corpus outlier into a
project-wide architecture requirement.

This is a long, real-Workshop run for the maintainer to execute manually.
Agents should use focused fixtures and bounded real-case probes while
developing, then hand off the Cargo command instead of launching it
unannounced.

The V2 JSONL streams under `src/apps/foch-cli/tests/merge_quality/data/` are
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
- Check Linear for active work. `docs/` owns public usage, contributor guidance,
  current architecture, measured status and raw evidence. `notes/首页.md` is
  the entry to private research and design history in `notes/research/`.
  Notion-only narrative that has not been
  migrated remains at its original page; do not write to Notion unless asked.
- Distinguish committed implementation, a local worktree observation, a
  recorded test result, and an accepted product cohort. Never promote one into
  another.
- Prefer fixes driven by a fresh acceptance failure: reproduce one cause with
  a focused regression, fix it at the correct EU4 content-family or kernel
  boundary, run focused gates, then re-check a bounded real case.
- Do not invent architectural names in status or planning documents before a
  corresponding code boundary and demonstrated need exist. Use terms already
  present in the source and reports.
- Treat historical roadmaps, reviews, and probes as evidence only. Linear is
  the active backlog.

## Research Notes

`notes/` references the existing `https://github.com/Acture/obsidian-vault.git`
repository on `project/foch`. Use the latest published branch contents by
default. At session start, before reading project notes, preserve any local
edits or unpublished commits, then refresh a clean checkout with
`git submodule update --init --remote --checkout -- notes`. Private repository
access is required; if refresh fails, report the stale/unavailable notes rather
than silently using them as current. Read `notes/首页.md` alongside the
code status and Linear.

Before editing, preserve any dirty notes, then run `git -C notes fetch origin`,
`git -C notes switch project/foch`, and
`git -C notes pull --ff-only origin project/foch`. Initialization may leave a
detached HEAD; `.gitmodules` branch configuration and ordinary clone/pull do
not automatically fetch the latest notes. A plain submodule update restores
the recorded commit and is only for explicit historical reproduction.
The project branch root is the notes root; edit its `research/` and project
entry directly. Master integration belongs to the notes repository. Preserve
both sides of any conflict; do not overwrite or reset unpublished work.

Install or refresh the master-owned notes hook using the README commands.
Submit notes through the installed `hooks/notes-boundary/submit_project.py`:
the vault requires a remote boundary check before a new project commit can be
pushed. Do not bypass hooks or branch protection.
Commit and successfully push the notes branch first. Only then stage `notes`
and commit/push the Foch gitlink. Never point the parent at an unpublished notes
commit. See the README's research-notes section for initialization, latest-branch
refresh, historical reproduction, and editing commands. Keep one canonical
instruction file here: `AGENTS.md` and `.github/copilot-instructions.md` remain
symlinks to `CLAUDE.md`.

## Project Structure & Module Organization

`foch` is a workspace monorepo with one primary Rust library:

- `src/packages/foch/src/` — project/input models, check/graph/simplify orchestration, merge
  analysis/review/commit, platform services, reusable CWT schema machinery, and
  concrete EU4 semantics
- `src/apps/foch-cli` — the `foch` binary, `foch lsp`, CLI integration tests,
  fixed-corpus merge-quality harness, and feature-gated maintainer examples
- `src/apps/foch-desktop` — the player-facing Tauri application, linked directly to
  the main library without a CLI sidecar

- `src/packages/tree-sitter-paradox` — grammar package
- `src/apps/vscode-foch` — VS Code extension
- `src/tools/eu4-analysis` — PyGhidra-based maintainer analysis
- `vendor/cwtools-eu4-config` — external build input, compiled into the binary

The root Cargo manifest only configures the workspace. The main library's
`tests/`, `fuzz/` and `build.rs` live in `src/packages/foch/`. Keep fuzz as an
independent cargo-fuzz workspace. Use package-local test fixtures, `docs/` for
public documentation and evidence, `notes/research/` for private research and
design history, and `scripts/` for operator workflows. Release source archives
must include both public build submodules and work without `.git` or `notes/`.

Public known issues and release guidance belong in `docs/`; historical public
release material belongs in `docs/archive/` with an explicit archive notice.
Keep the README, canonical agent instructions and license/notice files at the
repository root.

## Build, Test, and Development Commands

- `cargo fmt --all --check` — verify Rust formatting
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — strict Rust linting
- `cargo test --workspace` — run the Rust test suite
- `bun install --frozen-lockfile` — install workspace JS dependencies
- `bun run --cwd src/packages/tree-sitter-paradox test` — test the grammar package
- `bun run --cwd src/apps/vscode-foch smoke` — smoke-test the VS Code extension
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

Keep changing coverage status in `docs/project-status.md`; research interpretation belongs in `notes/`. `AGENTS.md` should hold stable execution guidance, not rolling project baselines.

## Environment & Configuration

Use `direnv` in the repo root and keep Node on the supported line: `>=22 <25`. Run `direnv allow` once after cloning. `node@25` is currently not a supported local development environment for `src/packages/tree-sitter-paradox`.

Both build submodules must be checked out before anything builds, and `git worktree add` initializes none of the submodules:

```fish
git submodule update --init src/packages/tree-sitter-paradox vendor/cwtools-eu4-config
```

A missing `src/packages/tree-sitter-paradox` fails at manifest load. A missing or empty `vendor/cwtools-eu4-config` fails the main library's build script, which compiles it into the embedded CWT rule pack; there is no schema-less build.

The private `notes/` submodule is separate and optional for builds. CI and public
source packages initialize only the two build submodules listed above.

Three tests need privileges a restricted sandbox may withhold: `output_transaction_rejects_an_existing_unix_socket` binds a Unix socket, and `data_install_downloads_release_asset_from_manifest` and `page_fetch_is_frozen_and_reused_without_another_network_request` each open a local HTTP server. A sandbox denial there is an environment result, not a defect. Committing an analyzed merge also takes the installed base-snapshot lock under the user data directory; point `FOCH_DATA_DIR` at a cloned data directory when that path is not writable.

## Local quality gates

Run `bash scripts/install-hooks.sh` once after cloning to install the local git hooks. The pre-commit hook runs `cargo fmt --all --check`, strict workspace clippy, and `cargo build --workspace --tests`; the pre-push hook runs the full `cargo test --workspace` suite. Override only in emergencies with `FOCH_SKIP_PRE_COMMIT=1 git commit ...` or `FOCH_SKIP_PRE_PUSH=1 git push ...`, and make the next push without skipping the gate. Agents in autopilot or fleet mode must install hooks before doing work and must not use `--no-verify` to bypass them.

## Commit & Pull Request Guidelines

Follow the existing commit style: short, imperative subjects such as `Promote low-risk common mechanics roots` or `Split and promote random map content families`. Keep one logical change per commit. PRs should include a concise summary, linked Notion task or decision when applicable, validation commands run, and any probe delta (`parse_only` / `semantic_complete`) when analyzer coverage changes. Add screenshots only for `src/apps/vscode-foch` UI changes. Do not add AI co-author trailers.


## Project notes submission

`docs/` is reserved for public documentation. Private research notes live in `notes/`, which tracks `project/foch` in `Acture/obsidian-vault`; start at `notes/首页.md`. This checkout's root contains only this project's notes. Master places these notes under `foch/`. Keep automation and vault configuration on master. Preserve existing local edits when updating a checkout.

Install or refresh the trusted submission tools in Git metadata, including in new clones:

```fish
git -C notes fetch origin refs/heads/master:refs/remotes/origin/master
set notes_common_gitdir (git -C notes rev-parse --path-format=absolute --git-common-dir)
git -C notes show origin/master:.github/scripts/install_push_hook.py > "$notes_common_gitdir/install_push_hook.py"
python3 "$notes_common_gitdir/install_push_hook.py" --repo notes --source-ref origin/master
```

After committing specific note files, submit through `python3 "$notes_common_gitdir/hooks/notes-boundary/submit_project.py" --repo notes`. The remote requires `notes-boundary/root/foch` from GitHub Actions. Only after successful submission should this repository commit and push the `notes` gitlink. See the central repository's `项目接入.md` for initialization, updates and conflict handling.
