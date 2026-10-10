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
- record machine-readable artifacts below the output's `.foch/` directory;
- collect and compile native comment-based event tests, expose annotation
  help in the CLI/LSP, and judge an explicitly configured runtime runner.

What is not established:

- automatic-merge reliability across arbitrary EU4 modlists;
- a complete accepted quality baseline for the current user-facing merge path;
- support for any Paradox game other than EU4; or
- an interactive `MergeSession` API. Session work is deliberately deferred.

Reusable CWT schema machinery lives under `src/packages/foch/src/game/schema`. That boundary is
intended to support future concrete game implementations, but today only
`src/packages/foch/src/game/eu4` has verified loader and content-family behavior.

Linear owns active milestones, issues, and dependencies. The repository records
the verified implementation state in [the current checkpoint](./docs/project-status.md)
and the stable execution contract in [the architecture](docs/architecture.md).

## Install

> **Not available yet.** No channel below has published Foch. Each becomes
> usable only once the first release has been installed and verified from it,
> which [the project status](./docs/project-status.md#distribution) records.
> Until then, build from source as described in the next section.

Every channel installs the same `foch` program, including `foch lsp`, for
Linux x64, macOS arm64 and Windows x64:

| Channel | Command, once released |
| --- | --- |
| WinGet (Windows x64) | `winget install --id Acture.Foch -e` |
| Homebrew (macOS arm64, Linux x64) | `brew install acture/ac/foch` |
| crates.io | `cargo install foch-cli --locked` |
| PyPI through uv | `uvx foch <command>` to run it once, or `uv tool install foch` |

All of them report one identity: `foch --version` prints `foch-cli <version>`
and the `cwt-schema` id of the rules embedded at that release.

The crates.io `foch 0.1.0` package is an older, superseded product, not this
source line. If you installed it, remove it with `cargo uninstall foch` before
installing `foch-cli`; both provide a `foch` executable.

An installed program is not yet ready to merge. Its first run needs an EU4
base-data snapshot built from your own game installation (`foch data build eu4
... --install`, below), and merge quality is a separate acceptance gate, not a
property of any install channel; see the [current boundary](#current-boundary).

## Build and try it

Building from source is the current way to run Foch:

```fish
git clone https://github.com/Acture/foch.git
cd foch
git submodule update --init --recursive src/packages/tree-sitter-paradox src/packages/foch/vendor/cwtools-eu4-config
cargo install --path src/apps/foch-cli
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

The terminal review shows the first 20 units of each disposition, the complete
totals, and explicit counts of omitted units. Add `--review-all` to inspect
every unit before committing; this changes presentation only.
`--review-json PATH` also writes the complete review as JSON before
confirmation: the mods, their dependency edges and each unit's ordered
contributors; each deferred unit's conflict address tree with the competing
candidates' text; and every decision point. A choice for just one conflict is
a decision record keyed by its conflict id, kept apart from `foch.toml`; a
choice for its file or directory is the `[[resolutions]]` rule that would
persist it.

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

## How numbers are written

EU4 reads a script number far more coarsely than a text comparison does. The
script surface reaches `CToken::ReadValue(CFixedPoint&)`, which takes the
integer part and at most **three** fraction digits, truncated rather than
rounded, and scales by 1000; `CToken::GetInt` is plain `atoi`. So `0.5`, `0.50`
and `0.500` are one value to the game, and `0.1234` and `0.1239` are both
`0.123`.

Foch therefore writes numbers in that representation — a `float` field with
three decimals, an `int` field as the integer the game reads:

```text
land_morale = 0.50     ->  land_morale = 0.500
add_prestige = 1       ->  add_prestige = 1.000
slots = 3.4            ->  slots = 3
```

The point is not tidiness. Two mods writing one value in two spellings used to
be reported as a content conflict for you to adjudicate, and there was nothing
to adjudicate. Canonicalizing before the merge makes the spelling invisible to
every part of it at once.

This applies **only where the CWT schema states the field's type**, because
which reader the game uses is a property of the field, not of the text. Where
the schema is silent the number is left exactly as written, rather than
asserting a meaning from how a token happens to look. EU4's schema coverage is
partial, so a good deal of real content is left untouched.

Source mods are never modified — this affects the separate merged mod foch
writes. The rule is pinned to the engine build it was read from; a patch that
changes a reader invalidates the equivalences, not just their precision.

## CLI surface

| Command | Purpose |
| --- | --- |
| `foch` | Open a read-only terminal browser over the current EU4 playset and its full merge analysis. |
| `foch input inspect` | Show the game and ordered mod inputs Foch will use; without a path, the current EU4 input bare `foch` shows (`--format json` for scripts). |
| `foch input repair` | List current-playset mods that cannot be analyzed and, with `--open`, open their Workshop pages in Steam to resubscribe. |
| `foch check` | Parse and analyze an input without writing a merge. With `--fix` (or `--diff` to preview), fix syntax errors as a linter does: a mod directory in place, a playset as a patch mod (`--patch-mod`) or in the mods' own files (`--in-place`, backed up; `--restore` undoes it). `--unsafe-fixes` also settles what a merge holds for review. |
| `foch merge` | Analyze and review a semantic result; commit only after confirmation. |
| `foch graph` | Write call, definition-dependency, mod-dependency, and semantic graphs. |
| `foch simplify` | Remove target-mod definitions equivalent to effective base definitions. |
| `foch data` | Build, install, and inspect EU4 base-data snapshots. |
| `foch cache` | Inspect and explicitly maintain persistent caches. |
| `foch lsp` | Run the language server used by the VS Code extension. |

Every analysis the browser runs is a `foch merge` it displays: without
INPUT_SOURCE, `foch merge` analyzes the current EU4 playset exactly as the
browser does, and `--exclude <WORKSHOP_ID|#POSITION>` leaves a mod out as the
browser's `x` does; `foch input repair --open` opens the same Workshop pages as
the browser's `R`, and `foch data build eu4 --from-game-path <GAME> --install`
builds base data as the browser's `B` does. The browser's `?` panel lists
every key with its command.
Agents and scripts can therefore reproduce any browser analysis from the
command it shows.

Run `foch <command> --help` for authoritative options.

## Repository layout

- `src/packages/foch` — the main Rust library, with its own `src/`, `tests/`,
  `fuzz/`, `build.rs`, and `vendor/` (the externally maintained CWT rules,
  pinned as a build submodule)
- `src/apps/foch-cli` — the `foch` executable, LSP, integration tests, and test-only
  merge-quality harness
- `src/packages/foch-annotation`, `foch-test`, `foch-runner` and `foch-lsp` —
  libraries the `foch` executable links: Foch annotations, in-game test
  planning and judging, the game runner, and the language server
- `src/apps/foch-desktop` — the Tauri desktop product, linked directly to `foch`
- `src/packages/tree-sitter-paradox` — independently versioned grammar package
- `src/apps/vscode-foch` — independently versioned VS Code extension
- `src/tools/eu4-analysis` — maintainer tooling for extracting EU4 loading rules
- `src/tools/foch-dev` — reusable repository checks and diagnostic workflows
- `scripts/` — build, release, and repository maintenance workflows
- `docs/` — current public usage, contributor guides, architecture, and status
- `notes/research/` — private research, experimental interpretation, and design history
  in the [unified notes repository](https://github.com/Acture/obsidian-vault/tree/project/foch)

The root Cargo manifest is a virtual workspace. The `foch` library package
carries its CWT build input, so the packaged crates build without the rest of
the checkout; `python -m foch_dev crate-smoke` packages them with the pinned
grammar and installs `foch` from those crates outside the repository. A
release publishes `foch-cli` to crates.io with every workspace crate it builds
from (`foch`, `foch-annotation`, `foch-lsp`, `foch-runner` and `foch-test`),
against `tree-sitter-paradox` released from its own repository; none is
published yet. Release source archives carry both public submodules and
exclude private notes.

The Rust product is versioned at `0.0.1`, the VS Code extension at `0.1.0`, and
`tree-sitter-paradox` at `0.3.0`. Cache and report schema generations are
versioned independently.

## Development

```fish
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
bun install --frozen-lockfile
bun run --cwd src/packages/tree-sitter-paradox test
bun run --cwd src/apps/vscode-foch smoke
```

EU4 CWT schemas are vendored at `src/packages/foch/vendor/cwtools-eu4-config`. The build
compiles that directory into a rule pack embedded in the binary, so the
submodule must be checked out to build at all; `foch --version` names the
embedded pack's `cwt-schema` identity. Refreshing that submodule and its
recorded snapshot hash is an explicit maintenance operation, not part of a
normal build.

### EU4 database loading rules

Versioned rules live in [`src/packages/foch/src/game/eu4/content/rules`](./src/packages/foch/src/game/eu4/content/rules).
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

[`src/tools/eu4-analysis`](./src/tools/eu4-analysis) extracts these relations from a
symbol-bearing x86_64 Mach-O executable. It inventories loaders, follows each
directory-loading call and its file filter, and resolves directory enum values
from `CDirectorySettings`. Object-key discovery is not required to emit a
loading rule. Unresolved relations remain in the local diagnostic catalog.

It requires Ghidra 12.1.3, JDK 21+, and `uv`. Set `GHIDRA_INSTALL_DIR` and
`EU4_BINARY` to your local installation and executable, then run:

```fish
uv run --directory src/tools/eu4-analysis python -m eu4_analysis discover \
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
`uv run --directory src/tools/eu4-analysis python -m unittest discover -s tests`.

The same module's `builtins` command builds a candidate builtin symbol catalog
from local CWT and wiki snapshots. It requires no Ghidra session. Supply explicit
wiki files and an output destination; the CWT input defaults to the vendored
snapshot. Relative arguments below are resolved from the tool directory:

```fish
uv run --directory src/tools/eu4-analysis python -m eu4_analysis builtins \
	--wiki-effects /path/to/effects.md --wiki-conditions /path/to/conditions.md \
	--wiki-scope /path/to/scope.md --output /tmp/eu4_builtin_catalog.json
```

`--irony-readme` adds optional source context. Game scanning is opt-in through
`--game-root`; `--max-game-files` bounds that scan. Review the candidate before
replacing `src/packages/foch/src/game/eu4/base/data/eu4_builtin_catalog.json`.
Generation does not download sources or modify installed game files.

## Repository maintenance tools

[`foch-dev`](src/tools/foch-dev/README.md) provides importable Python modules and
one maintenance command entrypoint:

```fish
uv run --locked --project src/tools/foch-dev python -m foch_dev check
uv run --locked --project src/tools/foch-dev python -m foch_dev schema-hash
```

Its `smoke` and `compare` subcommands run diagnostic checks and compare summaries;
`crate-smoke`, `dist`, `winget` and `release preflight` build and verify the
release channels. Use `--help` for their inputs. These diagnostics are separate from `cargo acceptance`.
Root `scripts/` holds hooks and release wrappers; the desktop app owns its
Windows installer smoke script. Ordinary Foch users do not need these Python tools.

## Documentation

- [Public documentation](./docs/README.md)
- [Project status](./docs/project-status.md)
- [Architecture](docs/architecture.md)
- [Merge design](docs/merge-design.md)
- [Mod authoring product design (in progress)](docs/foch-authoring-design.md)
- [Inline EU4 runtime tests and runner contract](docs/foch-runtime-tests.md)
- [`foch.toml` project manifest](./docs/foch-project-manifest.md)
- [Resolution DSL](./docs/foch-toml-resolutions.md)
- [VS Code/LSP preview](./docs/lsp-0.1-preview.md)
- [Known issues](./docs/known-issues.md)
- [Release checklist](./docs/RELEASE_CHECKLIST.md)

### Private research notes

`notes/` is a submodule of `https://github.com/Acture/obsidian-vault.git`,
configured for `project/foch`. Its Foch entry is
[`notes/首页.md`](./notes/首页.md). The project branch root is the notes root;
research lives under `notes/research/`. Public usage, contributor and architecture
documents and current measured status remain in Foch. Numeric records and raw
evidence stay with their producing code or test harness. Superseded reports,
release drafts and development history belong in notes, outside public docs.
Reading public documentation, building and testing do not require private access.

Use the latest published `project/foch` notes by default. After cloning,
creating a worktree, or pulling Foch, refresh a clean notes worktree with:

```fish
git submodule update --init --remote --checkout -- notes
```

Preserve local edits and unpublished commits before refreshing; do not force
the checkout. If refresh fails, report that notes are stale instead of treating
the cached checkout as current.

Git still records a fixed commit in the parent. Plain `git clone`, `git pull`,
and `git submodule update --init` do not fetch the latest notes automatically;
even `git clone --recurse-submodules` needs the refresh above. The branch setting
selects what `--remote` follows. Restoring the recorded commit is an explicit
historical-reproduction operation: `git submodule update --init --checkout -- notes`.

The refresh may leave a detached HEAD. Before editing, switch to the project
branch and fast-forward it to the published notes:

```fish
git -C notes fetch origin
git -C notes switch project/foch
git -C notes pull --ff-only origin project/foch
```

The first `switch` creates a local tracking branch when only
`origin/project/foch` exists. Preserve local changes and resolve divergence
before continuing. Edit only this project's branch; integration with the vault's
`master` is managed in that repository.

Install or refresh the notes repository's existing push checker once per clone
using Git, Python 3.10+ and authenticated `gh`:

```fish
git -C notes fetch origin refs/heads/master:refs/remotes/origin/master
and set notes_common_gitdir (git -C notes rev-parse --path-format=absolute --git-common-dir)
and git -C notes show origin/master:.github/scripts/install_push_hook.py > "$notes_common_gitdir/install_push_hook.py"
and python3 "$notes_common_gitdir/install_push_hook.py" --repo notes --source-ref origin/master
```

After editing `notes/`, publish notes before updating Foch's reference:

```fish
git -C notes add -- research 首页.md
git -C notes commit -m "Update Foch research notes"
set notes_common_gitdir (git -C notes rev-parse --path-format=absolute --git-common-dir)
python3 "$notes_common_gitdir/hooks/notes-boundary/submit_project.py" --repo notes --branch project/foch
# Continue only after the notes push succeeds.
git add notes
git commit -m "Update Foch notes reference"
git push
```

The submission tool runs the vault's required remote boundary check before
updating `project/foch`. A direct push of a new commit lacks that check. Follow
the vault's [project integration guide](https://github.com/Acture/obsidian-vault/blob/master/项目接入.md)
if the checker changes; do not disable hooks or branch protection.

A refresh can change the gitlink shown by `git status`; review and commit that
reference separately when adopting the update in Foch. Earlier document paths
and content remain available in each repository's Git history.

## License

Foch's own code is AGPL-3.0-only ([LICENSE](./LICENSE)). The `foch` library,
and every distributed `foch` program, also contains an adaptation of Mergiraf
(GPL-3.0-only, [LICENSE-MERGIRAF.txt](./LICENSE-MERGIRAF.txt)) and embeds a rule
pack compiled from the CWTools EU4 config (MIT), so both are distributed as
`AGPL-3.0-only AND GPL-3.0-only AND MIT`; the own source of `foch-cli` and of
its internal libraries `foch-annotation`, `foch-lsp`, `foch-runner` and
`foch-test` is AGPL-3.0-only. Each program also statically links Rust crates
under their own licenses, given in
[THIRD-PARTY-LICENSES.txt](./THIRD-PARTY-LICENSES.txt). See
[NOTICE.md](./NOTICE.md).
