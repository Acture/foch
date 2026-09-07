# foch

Foch is an EU4 mod analysis and merge tool under active development. It takes
an ordered Europa Universalis IV playset, preserves contributions whose loader
semantics are understood, and surfaces genuine ambiguity instead of silently
discarding one mod's work.

> **Unreleased alpha (`0.0.1`), EU4 only.** The implementation passes its
> focused fixture gates, but the current product has no accepted complete
> 14-case Workshop cohort. Do not treat it as a reliable one-click merger for
> arbitrary modlists yet.

## Current boundary

Foch can currently:

- inspect and resolve a Launcher `dlc_load.json` or declarative `foch.toml`
  project while preserving playset order;
- parse Clausewitz script, localisation, CSV, and JSON content;
- build a cross-mod semantic index and report overlap risk;
- analyze a complete deterministic merge result before writing output;
- expose file- and definition-module review units with their disposition and
  contributors;
- commit supported output to a separate merged-mod directory after explicit
  confirmation; and
- record machine-readable artifacts below the output's `.foch/` directory.

What is not established:

- automatic-merge reliability across arbitrary EU4 modlists;
- a complete accepted quality baseline for the current user-facing merge path;
- support for any Paradox game other than EU4; or
- an interactive `MergeSession` API. Session work is deliberately deferred.

Reusable CWT schema machinery lives under `src/game/schema`. That boundary is
intended to support future concrete game implementations, but today only
`src/game/eu4` has verified loader and content-family behavior.

Linear owns active milestones, issues, and dependencies. The repository records
the verified implementation state in [the current checkpoint](./docs/project-status.md)
and the stable execution contract in [the architecture](./docs/architecture.md).

## Build and try it

There is no released binary matching this source line. The crates.io
`foch 0.1.0` package is an older, superseded build; do not use `cargo install
foch` for this repository.

```fish
git clone --recurse-submodules https://github.com/Acture/foch.git
cd foch
cargo install --path apps/foch-cli
```

Build and install the EU4 base-data snapshot, then inspect and merge a Launcher
playset:

```fish
set EU4_ROOT "/path/to/Europa Universalis IV"
set PLAYSET "/path/to/Paradox Interactive/Europa Universalis IV/dlc_load.json"

foch data build eu4 \
	--from-game-path "$EU4_ROOT" \
	--game-version auto \
	--install

foch input inspect "$PLAYSET"
foch check "$PLAYSET"
foch merge "$PLAYSET" --out ./merged-mod --non-interactive  # analyze and review; no write
foch merge "$PLAYSET" --out ./merged-mod --confirm  # commit the reviewed result
foch merge "$PLAYSET" --out ./new-merged-mod --confirm --non-interactive
```

The initial base-data build scans the installed game and can take time. Foch
reads installed Workshop mods in place; it does not copy whole mod trees into
an input CAS.

`foch merge` resolves and freezes the input, computes the semantic result, and
presents its review without touching `--out`. A TTY confirmation or
`--confirm` commits that result. A non-empty output directory still requires a
separate TTY overwrite confirmation, so non-interactive jobs must use a new or
empty path. `--non-interactive` disables prompts and does not imply
`--confirm`.

Unresolved files or complete definition modules are withheld while unrelated
safe units are written. This `partial_success` result is valid. `--force`
applies only to supported `needs_user_choice` fallbacks; it does not turn
unsupported input or engine failures into safe output.

Source mods and the game install are always read-only inputs. Enable the
generated mod only after reviewing its report, and disable its source mods to
avoid loading both copies.

## Conflict policy

Foch does not silently pick a winner for an ambiguous structural conflict.
Reviewed decisions can be recorded as narrow `[[resolutions]]` entries:

```toml
[[resolutions]]
match = "common/ideas/00_country_ideas.txt"
handler = "last_writer"
```

Prefer exact files or conflict IDs over broad policies. See the
[resolution reference](./docs/foch-toml-resolutions.md) and
[project manifest reference](./docs/foch-project-manifest.md).

## CLI surface

| Command | Purpose |
| --- | --- |
| `foch input inspect` | Show the game and ordered mod inputs Foch will use. |
| `foch check` | Parse and analyze an input without writing a merge. |
| `foch merge` | Analyze and review a semantic result; commit only after confirmation. |
| `foch graph` | Write call, definition-dependency, mod-dependency, and semantic graphs. |
| `foch simplify` | Remove target-mod definitions equivalent to effective base definitions. |
| `foch data` | Build, install, and inspect EU4 base-data snapshots. |
| `foch cache` | Inspect and explicitly maintain persistent caches. |
| `foch lsp` | Run the language server used by the VS Code extension. |

Run `foch <command> --help` for authoritative options.

## Repository layout

- `src/` — the root `foch` library: input, project, check, graph, simplify,
  merge, platform, reusable schema machinery, and concrete EU4 behavior
- `apps/foch-cli` — the `foch` executable, LSP, integration tests, and private
  merge-quality harness
- `apps/foch-desktop` — the Tauri desktop product, linked directly to `foch`
- `packages/tree-sitter-paradox` — independently versioned grammar package
- `packages/vscode-foch` — independently versioned VS Code extension

The Rust product is versioned at `0.0.1`, the VS Code extension at `0.1.0`, and
`tree-sitter-paradox` at `0.2.0`. Cache and report schema generations are
versioned independently.

## Development

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

### EU4 database loading rules

Versioned rules live in [`src/game/eu4/content/rules`](./src/game/eu4/content/rules).
Each rule states **which database reads which directory and filename pattern**:

```json
"CStaticModifierDataBase": [
  {"directory": "common/event_modifiers", "files": "*.txt"},
  {"directory": "common/static_modifiers", "files": "*.txt"}
]
```

The file header identifies the game version and inspected executable hash.
Directories are relative to the game/mod root; `files` matches filenames directly
in that directory. For the resolved game version, the path planner groups matching
inputs by database, including files in different directories. Retained-path
selection expands to the same database's available inputs. A file matching more
than one database blocks planning with an explicit error. Unmatched files in a
rule-covered family stay separate; other unmatched resources and versions without
a rule snapshot use the existing family policies.

Structural merge behavior remains in the EU4 content-family descriptors.
Database units with a common definition-module output policy use that policy.
Units spanning different policies retain all inputs in one plan unit and defer
output, including with `--force`; this currently includes a static/event modifier
database containing files from both directories. Base-only units without a mod
contribution or namespace reset remain copy-through paths.

[`tools/eu4-analysis`](./tools/eu4-analysis) extracts these relations from a
symbol-bearing x86_64 Mach-O executable. It inventories loaders, follows each
directory-loading call and its file filter, and resolves directory enum values
from `CDirectorySettings`. Object-key discovery is not required to emit a
loading rule. Unresolved relations remain in the local diagnostic catalog.

It requires Ghidra 12.1.3, JDK 21+, and `uv`. Set `GHIDRA_INSTALL_DIR` and
`EU4_BINARY` to your local installation and executable, then run:

```fish
uv run --directory tools/eu4-analysis python -m eu4_analysis discover \
	--binary "$EU4_BINARY" --game-version 1.37.5
```

A complete scan writes `<version>.json` directly into the rule
directory; `--rules-dir` overrides that destination. Review the resulting diff
before using a new snapshot. `--limit N` runs a bounded probe and preserves
pending inventory entries without replacing repository rules.

Analysis projects, diagnostic catalogs, disassembly, and pseudocode stay under
ignored `target/eu4-analysis` (or `--workspace`). `inspect --symbol '<glob>'`
examines one function using its Mach-O boundary. `--timeout` bounds each
operation; whole-program auto-analysis is disabled. The inventory does not
prove coverage of all indirect loaders. Rule snapshots are embedded in the Foch
library at build time; ordinary merges do not require Python or Ghidra.

Run the module's tests without Ghidra using
`uv run --directory tools/eu4-analysis python -m unittest discover -s tests`.

## Documentation

- [Project status](./docs/project-status.md)
- [Architecture](./docs/architecture.md)
- [Merge design](./docs/merge-design.md)
- [`foch.toml` project manifest](./docs/foch-project-manifest.md)
- [Resolution DSL](./docs/foch-toml-resolutions.md)
- [VS Code/LSP preview](./docs/lsp-0.1-preview.md)
- [Known issues](./KNOWN_ISSUES.md)
- [Release checklist](./docs/RELEASE_CHECKLIST.md)

## License

AGPL-3.0-only. See [LICENSE](./LICENSE).
