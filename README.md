# Foch

**Foch is a static-analysis and structural-merge toolkit for Paradox mod playsets.**
It parses every script file across your enabled mods, builds a cross-mod semantic graph,
and produces a single deterministic merged mod that you can drop into `mod/` — without
the silent overrides that ad-hoc load-order workflows leave behind.

EU4 is the first fully-supported game; the architecture is game-agnostic via the
`GameProfile` + `ContentFamilyDescriptor` abstraction, with other Paradox titles
slotting in behind the same pipeline.

> Status: alpha, but actively shipping. The merge pipeline produced **36 885 files**
> from a real 118-mod EU4 playset on the latest probe. Roughly 20 unresolved
> structural conflicts per such playset are real cross-mod disagreements that the
> interactive arbitration UI surfaces explicitly — never silently dropped.

Additional documentation lives in [`docs/`](./docs/README.md):

- [`docs/project-status.md`](./docs/project-status.md) — current shipped surface and ongoing work
- [`docs/auto-merge-roadmap.md`](./docs/auto-merge-roadmap.md) — milestones from analyzer foundation to full auto-merge
- [`docs/merge-design.md`](./docs/merge-design.md) — implementation-grade merge specification
- [`docs/architecture.md`](./docs/architecture.md) — subsystem layout and internals

## Why Foch

- **Structural merge, not patchwork overlay.** Foch reads each contributing mod's
  AST, resolves the mod-dependency DAG, and applies patches level-by-level. The
  output is a real merged mod — not a stack of `replace_path` overlays praying that
  load order doesn't reorder under you.
- **Honest about real conflicts.** When two mods make incompatible structural
  changes Foch refuses to silently pick a winner. You get a TTY arbitration UI,
  a deterministic `[[resolutions]]` file format, or an explicit `--fallback` flag
  — and the choice is recorded in the merge report.
- **Zero hidden work.** Every file in the output is traceable back to the
  contributing mod via `.foch/foch-merge-report.json`. Same playset + same
  `foch.toml` ⇒ byte-identical output.
- **One toolchain end-to-end.** A single `foch` binary delivers `check`,
  `merge-plan`, `merge`, `graph`, `simplify`, `data`, `config`, and `lsp` — no
  separate analyzer / formatter / language-server processes.

## Install

```bash
# From crates.io (when published)
cargo install foch

# From a checkout (current alpha — recommended today)
cargo install --path crates/foch-cli
```

Homebrew tap is wired up via `release.yml`; once a tag is pushed the formula is
synced automatically.

## Quick start

The example assumes you have a Paradox Launcher–exported `dlc_load.json` (the
launcher's currently-active playset) and a local EU4 install.

```bash
# 1. Tell foch where EU4 lives
foch config set game-path eu4 "/path/to/Europa Universalis IV"

# 2. (One-time) build the base-game data snapshot
foch data build eu4 --from-game-path "/path/to/Europa Universalis IV" \
    --game-version auto --install

# 3. Analyze a playset: parse, semantic index, cross-mod overlap / dep / version checks
foch check ./playlist.json

# 4. Produce a deterministic merged mod
foch merge ./playlist.json --out ./merged
```

A clean run looks like:

> `Foch Check Report`  
> `fatal_errors: 0`  
> `strict_findings: 0`

> `Foch Merge Report`  
> `status: READY`  
> `manual_conflict_count: 0`

## Command surface

| Command       | Purpose                                                                  |
|---------------|--------------------------------------------------------------------------|
| `check`       | Static analysis: parse errors, unresolved references, cross-mod overlap  |
| `merge-plan`  | Compute the per-path merge strategy; write artifacts without touching FS |
| `merge`       | Materialize the merged mod tree and revalidate it                        |
| `graph`       | Export runtime call graph and content-family semantic graph reports      |
| `simplify`    | Drop base-game-equivalent definitions from a target mod                  |
| `data`        | Build, install, and list distributable base-game data snapshots          |
| `config`      | Inspect and manage `~/.config/foch/config.toml` (engine config)          |
| `lsp`         | Start the language server on stdio (used by the VS Code extension)       |

## Structural merge coverage

Every content family registered in `crates/foch-language/src/analyzer/eu4_profile.rs`
(`EU4_CONTENT_FAMILIES`) goes through the structural pipeline by default. Parsing,
the semantic index, DAG ordering, and level-by-level patch application share a
single mod → vanilla dependency graph; cross-base diff artifacts are no longer
flattened into one layer.

Special handling worth calling out:

- **`common/defines/*.lua`** — the lexer recognizes Lua `--` line comments and
  `--[[ ... ]]` block comments; empty defines files are treated as zero-contribution
  contributors (matching Lua runtime semantics).
- **Localisation YAML** (`localisation/**.yml`) — merged per-key, not per-file.
- **Sibling overwrite / replace-block / true list-item rename** — these remain
  user-arbitrated. Foch does not silently drop them; non-TTY runs explicitly skip
  the affected paths and report them.

## Conflict arbitration workflow

By default, `foch merge ./playlist.json --out ./merged` opens a ratatui TUI for
each unresolvable conflict; non-TTY environments (CI) skip and report. From there:

```bash
# Re-run interactively in a TTY
foch merge ./playlist.json --out ./merged

# CI / batch — skip prompts, write conflicts to the report
foch merge ./playlist.json --out ./merged --non-interactive

# Accept last-writer-wins for any unresolved conflict (writes a marker into the file)
foch merge ./playlist.json --out ./merged --fallback
```

### `foch.toml` `[[resolutions]]`

Each entry has exactly one selector and one action. Selectors:

| Field         | Match by                                                       |
|---------------|----------------------------------------------------------------|
| `file`        | Output path (e.g., `events/PirateEvents.txt`)                  |
| `conflict_id` | Stable hash of `(output path, structural address)`             |
| `mod`         | Mod id (only valid with the `priority_boost` action)           |

Actions:

| Field             | Effect                                                              |
|-------------------|---------------------------------------------------------------------|
| `prefer_mod`      | Pick this mod's contribution                                        |
| `use_file`        | Replace the merged output with an external file                     |
| `keep_existing`   | Preserve whatever is already at the destination path                |
| `priority_boost`  | Bump a mod's local precedence by `n`                                |

```toml
[[resolutions]]
conflict_id = "ab12cd34"
prefer_mod = "3378403419"

[[resolutions]]
file = "events/PirateEvents.txt"
use_file = "manual/events/PirateEvents.txt"

[[resolutions]]
file = "common/ideas/00_country_ideas.txt"
keep_existing = true

[[resolutions]]
mod = "3378403419"
priority_boost = 100
```

The TUI's per-conflict choices (mod number, `d` defer, `s` use file path,
`k` keep existing, `q` abort) are persisted back into `foch.toml` on confirmation.
Non-TTY runs auto-defer.

`conflict_id` is a deterministic hash of the merge report's `path` plus the
structural address printed in the TUI; pasting it into `foch.toml` resolves the
same conflict on every subsequent run.

## Configuration

Two TOML schemas, two purposes:

- `~/.config/foch/config.toml` — `foch_engine::Config`: `steam_root_path`,
  `paradox_data_path`, the per-game `game_path` map, `extra_ignore_patterns`.
  Managed by `foch config set` / `show` / `validate`. You usually don't hand-edit
  this.
- A `foch.toml` next to your playset (or at `~/.config/foch/foch.toml`) —
  `foch_core::FochConfig`: `[[overrides]]` (silence specific dependency edges in
  the local DAG), `[[resolutions]]` (conflict arbitration), and `[emit] indent`
  (merged-output indentation, default tab). Pass `--config PATH` to point at a
  specific file.

Override the engine config directory via:

```bash
export FOCH_CONFIG_DIR="$HOME/.config/foch-alpha"
```

Common config commands:

```bash
foch config show
foch config show --json
foch config validate
foch config set steam-path /path/to/steam
foch config set paradox-data-path "/path/to/Paradox Interactive/Europa Universalis IV"
foch config set game-path eu4 "/path/to/Europa Universalis IV"
```

> `paradox-data-path` should point at the **game-specific** subdirectory under
> `Documents/Paradox Interactive/`, not the parent folder — that's where the
> launcher writes `dlc_load.json`.

## Current alpha noise floor

On the latest 118-mod EU4 probe, `foch merge` writes 36 885 files and surfaces
roughly 20 residual structural conflicts: cross-mod sibling overwrites,
replace-blocks, and the occasional true list-item rename. These are real disagreements
between mods, not parser false positives. The full known-limitations inventory lives
in [`KNOWN_LIMITATIONS.md`](./KNOWN_LIMITATIONS.md).

## Exit codes

- `0` — success (in non-strict mode, findings do not affect the exit code)
- `1` — system error (e.g., unreadable file)
- `2` — `--strict` and at least one strict finding present
- `3` — `merge` produced a `BLOCKED` or `FATAL` report (unresolved conflicts)

## Local parse cache

`check` and `merge-plan` keep two layers of local cache:

- File-level parser cache (game + mod, in the system cache directory)
- Mod semantic snapshot cache (keyed by `game + mod identity + manifest hash`)

The base game is no longer scanned implicitly; install the data snapshot first:

```bash
foch data install eu4 --game-version auto
# or, from a local install:
foch data build eu4 --from-game-path "/path/to/eu4" --game-version auto --install
```

Optional environment overrides:

```bash
export FOCH_PARSE_CACHE_DIR=/tmp/foch-parse-cache
export FOCH_MOD_SNAPSHOT_CACHE_DIR=/tmp/foch-mod-snapshot-cache
export FOCH_DATA_DIR=/tmp/foch-data
```

## VS Code & LSP

The `foch lsp` subcommand is a stdio language server. The
[`packages/vscode-foch`](./packages/vscode-foch) extension bundles it; install the
preview build from the VS Code Marketplace, or build locally:

```bash
cd packages/vscode-foch
bun install
bun run prepare:server   # cargo build --release --bin foch + copy
code .
```

Currently shipped LSP features: parse diagnostics, workspace semantic findings,
builtin trigger / effect completion, workspace symbol completion, go-to-definition
for scripted effects / event ids / flag values / localisation keys. `hover`,
`find references`, `rename`, and code actions are tracked but not yet shipped.

## Real-corpus tooling

For maintainers running probes against a real EU4 install:

```bash
# Parse-success rate for a directory tree
cargo run --bin parse_stats -- "/path/to/eu4" --exts txt --features dev-tools

# A/B comparison between two probe outputs
python3 scripts/eu4_real_smoke.py --playset /path/to/playset.json \
    --out-dir target/eu4-real-smoke/baseline
python3 scripts/eu4_real_smoke.py --playset /path/to/playset.json \
    --out-dir target/eu4-real-smoke/post-change

python3 scripts/eu4_real_smoke_compare.py \
    target/eu4-real-smoke/baseline/<slug>-summary.json \
    target/eu4-real-smoke/post-change/<slug>-summary.json \
    --rule missing-effect-parameter \
    --gate-rule missing-effect-parameter \
    --min-absolute-drop 250 \
    --min-relative-drop 0.08
```

`parse_stats` and `symbol_dump` live behind the `dev-tools` feature so they don't
pollute `~/.cargo/bin/` for normal users.

## EU4 builtin catalog

`crates/foch-language/src/data/eu4_builtin_catalog.json` lists every engine
trigger / effect Foch knows about, used to suppress false-positive
"unresolved scripted effect" findings. Rebuild with:

```bash
python3 scripts/build_eu4_builtin_catalog.py
```

The script reads cached source material from `/tmp/foch-sources` and auto-detects
the local EU4 install; override with `FOCH_EU4_PATH=...`.

## Quality gates

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

The JS workspace (`packages/tree-sitter-paradox`, `packages/vscode-foch`) needs
Bun 1.2+ and Node `>=22 <25`. The `.envrc` at the repo root prepends Homebrew's
`node@22` to `PATH`; run `direnv allow` after `brew install node@22`. Node 25 is
not supported because `tree-sitter`'s native build is currently unstable on it.

## Release automation

Four GitHub Actions workflows ship the project:

- `ci.yml` — Rust quality gates plus `tree-sitter-paradox` and VS Code extension smoke tests
- `release.yml` — on tag: builds the CLI tarball, the VSIX, a source tarball that
  embeds the `tree-sitter-paradox` submodule contents, publishes the GitHub
  Release, and syncs the Homebrew tap
- `publish.yml` — manual re-sync of the Homebrew tap from an existing release asset
- `publish-vscode-preview.yml` — manual VS Code preview-channel publish

Required secrets / variables:

- `VSCE_PAT` — VS Code Marketplace token
- `HOMEBREW_TAP_TOKEN` — push token for the tap repo
- `HOMEBREW_TAP_REPO` — repository variable, e.g. `Acture/homebrew-tap`

The Homebrew formula uses the source tarball produced by `release.yml` rather
than GitHub's auto-generated tag archive, so the source bundle includes the
`packages/tree-sitter-paradox` submodule contents.

## License

AGPL-3.0-only. See [`LICENSE`](./LICENSE).
