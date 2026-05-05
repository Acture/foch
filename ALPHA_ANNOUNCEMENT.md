# foch alpha announcement draft

Hey EU4 modders — foch is ready for alpha testing.

foch is an **EU4 mod merge & static analysis toolkit**. It reads a Paradox Launcher playset, parses Clausewitz script and localisation, builds a cross-mod semantic index, reports risky overlaps, and can emit a deterministic merged mod with auditable `.foch` sidecars.

## Why alpha now?

Eight focused commits landed across the pieces that make real playset testing useful: the `[[resolutions]]` DSL, the leaf-conflict fix, the E2E merge fixture harness, definition-dependency graph artifacts, opt-in NoOp dedup, the C1/C2/C3/C4 cache pipeline, cleanup of old fallback wording, cross-version drift tagging, vanilla-symbol indexing, audit cleanup, and split `merge_status` / `analysis_status` reporting.

The current N=37 EU4 active playset baseline is down to `manual_conflict_count = 9` after the leaf-fix. The repo ships [`examples/eu4-default-foch.toml`](./examples/eu4-default-foch.toml), a narrow per-path resolution template that clears all nine without enabling global last-writer behavior.

## What works

- Static analysis: `foch check dlc_load.json`
- Deterministic merge output: `foch merge dlc_load.json --out merged-mod`
- Merge planning without writing output: `foch merge-plan dlc_load.json`
- Declarative conflict policy: [`docs/foch-toml-resolutions.md`](./docs/foch-toml-resolutions.md)
- Graph artifacts: `foch graph` exports `calls`, `definition-deps`, and `mod-deps` JSON/DOT outputs
- Cache inspection: `foch cache stats`, `foch cache list`, `foch cache where`
- Base-game snapshots: `foch data build eu4 --from-game-path ... --install`
- VS Code/LSP preview: [`packages/vscode-foch`](./packages/vscode-foch)
- Current status notes: [`docs/project-status.md`](./docs/project-status.md)
- CWT/content-family mapping notes: [`docs/cwt-content-family-mapping.md`](./docs/cwt-content-family-mapping.md)

## What does not work yet

Please read [`KNOWN_ISSUES.md`](./KNOWN_ISSUES.md). The short version: this alpha is EU4-only, large playsets can still have real manual conflicts, cold debug runs are slow without cache, the VS Code extension is preview quality, Windows TTY behavior needs more testing, NoOp dedup is intentionally opt-in per content family, and graph artifacts need external viewers such as Graphviz.

## Install

From a checkout:

```bash
git clone https://github.com/Acture/foch.git
cd foch
cargo install --path crates/foch-cli
foch data build eu4 --from-game-path "/path/to/Europa Universalis IV" --game-version auto --install
foch merge dlc_load.json --out merged-mod
```

Release binaries will be attached to the GitHub Release when the alpha tag is cut. macOS Intel builds are manual for this alpha and depend on user-side toolchains.

## Call for testers

If you maintain an EU4 mod stack, please try foch on a copy of your playset and file bugs with:

- your OS and terminal,
- the foch commit or release version,
- the command you ran,
- the relevant `.foch` report or minimized fixture when possible.

GitHub Issues: <https://github.com/Acture/foch/issues>

Feedback channels: GitHub Issues for actionable bugs and feature requests; Discord/forum threads for workflow feedback, screenshots, and larger design discussion.

## Acknowledgments

Thanks to EU4 mod authors, translation-mod maintainers, Paradox tooling communities, CWTools/Irony users, and everyone willing to test messy real playsets. The alpha is intentionally conservative: foch should surface ambiguous conflicts instead of silently losing mod contributions.
