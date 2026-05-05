# foch

**EU4 mod merge & static analysis toolkit.**

foch analyzes Europa Universalis IV mod playsets, builds a semantic index across vanilla and enabled mods, reports risky overlaps, and can materialize a deterministic merged mod. It is built for reproducible power-user workflows: the same playset plus the same `foch.toml` should produce the same output and the same audit artifacts.

> **Status:** alpha. The shipped game profile is EU4 only; other Paradox games need their own `GameProfile` and `ContentFamilyDescriptor` coverage before they should be treated as supported.

## Install + quickstart

Current alpha install from a checkout:

```bash
git clone https://github.com/Acture/foch.git
cd foch
cargo install --path crates/foch-cli
foch data build eu4 --from-game-path "/path/to/Europa Universalis IV" --game-version auto --install
foch merge dlc_load.json --out merged-mod
```

When a crates.io package is published, the install line becomes `cargo install foch`. Until then, use the path install above or a release binary.

## What works in the alpha

| Command | Purpose |
| --- | --- |
| `foch check` | Parse a playset, build the semantic index, and report diagnostics, dependency drift, and cross-mod overlap risk. |
| `foch merge` | Build a deterministic merged mod directory, apply configured conflict resolutions, and write `.foch` report sidecars. |
| `foch merge-plan` | Produce the merge strategy and conflict inventory without materializing the merged mod. |
| `foch graph` | Export `calls`, `definition-deps`, and `mod-deps` graph artifacts as JSON/DOT, with workspace/base/per-mod scopes and `SymbolKind` filters. |
| `foch simplify` | Remove definitions from a target mod when they are equivalent to the effective base-game definition. |
| `foch lsp` | Run the language server used by the bundled VS Code extension preview. |
| `foch cache` | Inspect, list, locate, clean, and clear local cache layers used by the C1/C2/C3/C4 cache pipeline. |
| `foch data` | Build, install, and list EU4 base-game data snapshots. |

Supporting docs:

- [`docs/foch-toml-resolutions.md`](./docs/foch-toml-resolutions.md) — `[[resolutions]]` DSL reference.
- [`KNOWN_ISSUES.md`](./KNOWN_ISSUES.md) — user-visible alpha limitations and workarounds.
- [`docs/project-status.md`](./docs/project-status.md) — rolling implementation status and probe baselines.

## Conflict resolutions

foch does not silently pick winners for ambiguous structural conflicts. The alpha ships a declarative `foch.toml` resolution layer:

```toml
[[resolutions]]
match = "common/ideas/00_country_ideas.txt"
handler = "last_writer"
```

The current handler registry includes `last_writer`, `defer`, and `keep_existing`; exact-file and exact-conflict selectors can also use actions such as `prefer_mod`, `use_file`, and `keep_existing = true`. See the full [`foch.toml` `[[resolutions]]` guide](./docs/foch-toml-resolutions.md) before applying broad rules.

## Current EU4 baseline

The latest N=37 active EU4 playset probe reported `manual_conflict_count = 9` after the leaf-conflict fix. Those nine conflicts are expected ambiguous cross-mod disagreements, not parser fallout. [`examples/eu4-default-foch.toml`](./examples/eu4-default-foch.toml) ships narrow per-path `last_writer` rules that clear all nine while avoiding a global `match = "**"` policy.

## Performance posture

- Cold debug runs over the N=37 playset are still expensive: about 25-30 minutes without useful cache warmth.
- Warm runs with the cache pipeline are seconds for iterative checks and merge planning.
- A release build plus cache has been observed around 40 seconds for the N=37 baseline.

For normal use, prefer release binaries or `cargo install --path crates/foch-cli` over `cargo run` debug builds, and keep the cache enabled unless you are debugging cache correctness.

## VS Code preview

[`packages/vscode-foch`](./packages/vscode-foch) bundles the `foch lsp` server. It is useful for parse diagnostics, semantic findings, completions, and go-to-definition, but it is still a preview surface; expect bugs and report them.

## Release and project status

- Alpha release prep: [`ALPHA_ANNOUNCEMENT.md`](./ALPHA_ANNOUNCEMENT.md), [`docs/RELEASE_CHECKLIST.md`](./docs/RELEASE_CHECKLIST.md).
- Known limitations: [`KNOWN_ISSUES.md`](./KNOWN_ISSUES.md).
- Current engineering status: [`docs/project-status.md`](./docs/project-status.md).
- Resolution DSL: [`docs/foch-toml-resolutions.md`](./docs/foch-toml-resolutions.md).
- CWT mapping notes: [`docs/cwt-content-family-mapping.md`](./docs/cwt-content-family-mapping.md).

## License

AGPL-3.0-only. See [`LICENSE`](./LICENSE).
