# foch

Foch is an EU4 mod analysis and merge tool under active development.

Its core goal is narrow: take an ordered Europa Universalis IV mod playset,
identify interactions between mods, merge the content families whose load
semantics are understood, and surface ambiguous conflicts instead of silently
discarding contributions.

> **Unreleased alpha (workspace version `0.0.1`), EU4 only.** The CLI and merge
> pipeline work on tested fixtures, but the current product kernel has not yet
> completed the fixed real-Workshop acceptance cohort. Do not treat Foch as a
> reliable one-click merger for arbitrary modlists yet.

## Current boundary

Foch can currently:

- resolve a Paradox Launcher `dlc_load.json` or a declarative `foch.toml`
  workspace;
- parse Clausewitz script, localisation, CSV, and JSON content;
- build a cross-mod semantic index and report overlap risk;
- generate a deterministic merge plan;
- materialize a separate merged-mod directory for supported structural roots;
- stop or report when a conflict needs an explicit decision;
- write machine-readable plan and report artifacts under the output's `.foch/`
  directory.

What is not established yet:

- automatic-merge reliability across ordinary, arbitrary EU4 modlists;
- a completed quality baseline for the current user-facing `foch merge` path;
- support for Paradox games other than EU4.

The next product milestone is the fixed 14-case Workshop acceptance run and
classification of every non-accepted result. See
[`docs/project-status.md`](./docs/project-status.md) for current evidence rather
than relying on historical headline numbers.

## Build and try it

There is no published binary matching the current source line. The existing
crates.io `foch 0.1.0` package is an older, superseded build; the current
workspace is back at `0.0.1` while its product contract is being established.
Do not use `cargo install foch` for this codebase. Build current Foch from the
repository:

```fish
git clone --recurse-submodules https://github.com/Acture/foch.git
cd foch
cargo install --path crates/foch-cli
```

Build and install the EU4 base-data snapshot, then inspect and merge a launcher
playset:

```fish
set EU4_ROOT "/path/to/Europa Universalis IV"
set PLAYSET "/path/to/Paradox Interactive/Europa Universalis IV/dlc_load.json"

foch data build eu4 \
	--from-game-path "$EU4_ROOT" \
	--game-version auto \
	--install

foch workspace resolve "$PLAYSET"
foch check "$PLAYSET"
foch merge-plan "$PLAYSET"
foch merge "$PLAYSET" --out ./merged-mod
```

The initial base-data build scans the installed game and can take time. It
produces an analyzed snapshot for later runs; Foch does not copy installed
Workshop mods into an input CAS.

Foch reads installed source mods in place and writes the result to the explicit
`--out` directory. Review `merged-mod/.foch/foch-merge-plan.json` and
`merged-mod/.foch/foch-merge-report.json` before enabling the generated mod. A
merge with unresolved conflicts exits with status 2 and leaves the plan/report
for review without presenting a usable merged mod. In a terminal, Foch can ask
for narrow resolutions interactively; use `--non-interactive` for CI or batch
runs. When a merge is ready, enable its launcher entry and disable the source
mods to avoid loading both copies. Use a copy of your playset and keep normal
game saves backed up while evaluating development builds.

## Conflict policy

Foch does not silently choose a winner for an ambiguous structural conflict.
Reviewed decisions can be recorded as narrow `[[resolutions]]` entries in
`foch.toml`:

```toml
[[resolutions]]
match = "common/ideas/00_country_ideas.txt"
handler = "last_writer"
```

Prefer exact files or conflicts over global policies. The available selectors,
handlers, and safety rules are documented in
[`docs/foch-toml-resolutions.md`](./docs/foch-toml-resolutions.md). Workspace
composition is documented in
[`docs/foch-workspace-manifest.md`](./docs/foch-workspace-manifest.md).

## CLI surface

| Command | Purpose |
| --- | --- |
| `foch workspace resolve` | Show the game and mod inputs Foch will use. |
| `foch check` | Parse and analyze a workspace without writing a merge. |
| `foch merge-plan` | Produce the merge strategy and conflict inventory. |
| `foch merge` | Materialize and revalidate a merged mod directory. |
| `foch graph` | Export call, definition-dependency, mod-dependency, and semantic graphs. |
| `foch simplify` | Remove target-mod definitions equivalent to effective base definitions. |
| `foch data` | Build, install, and inspect EU4 base-data snapshots. |
| `foch cache` | Inspect and maintain persistent Foch caches. |
| `foch lsp` | Run the language server used by the VS Code extension. |

Run `foch <command> --help` for the authoritative options.

## Independent versions

The repository contains three separately versioned products:

- Foch CLI, engine, and Rust libraries: `0.0.1`, tracking product merge
  maturity;
- VS Code extension: `0.1.0`, tracking editor usability;
- `tree-sitter-paradox`: `0.2.0`, tracking the independently published grammar.

Cache generations, report schemas, and dataset schemas have their own versions
and do not track the CLI release number.

## VS Code preview

The current source tree under
[`packages/vscode-foch`](./packages/vscode-foch) bundles `foch lsp` for EU4
script diagnostics, schema-aware completion and hover, navigation, symbols, and
a focused missing-localisation quick fix. The Marketplace `0.1.0` build predates
these source-tree capabilities and remains a stale preview pending republish.
Neither editor surface is a claim that automatic merge is ready for arbitrary
modlists. See
[`docs/lsp-0.1-preview.md`](./docs/lsp-0.1-preview.md).

## Development

The main local gates are:

```fish
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
bun install --frozen-lockfile
bun run --cwd packages/tree-sitter-paradox test
bun run --cwd packages/vscode-foch smoke
```

EU4 CWT schemas are vendored at `vendor/cwtools-eu4-config`. Refreshing that
submodule and its recorded snapshot hash is an explicit maintenance operation,
not part of a normal build.

## Documentation

- [`KNOWN_ISSUES.md`](./KNOWN_ISSUES.md) — user-visible limitations and workarounds.
- [`docs/project-status.md`](./docs/project-status.md) — current engineering and acceptance status.
- [`docs/architecture.md`](./docs/architecture.md) — package and execution boundaries.
- [`docs/foch-workspace-manifest.md`](./docs/foch-workspace-manifest.md) — workspace configuration.
- [`docs/foch-toml-resolutions.md`](./docs/foch-toml-resolutions.md) — conflict-resolution DSL.
- [`docs/RELEASE_CHECKLIST.md`](./docs/RELEASE_CHECKLIST.md) — release gates.

## License

AGPL-3.0-only. See [`LICENSE`](./LICENSE).
