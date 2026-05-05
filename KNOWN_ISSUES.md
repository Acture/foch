# Known issues for the foch alpha

These are user-visible alpha limitations and practical workarounds. They are not release blockers unless a specific workflow depends on them.

## EU4 is the only supported game profile

foch currently ships one game profile: Europa Universalis IV. CK3, Victoria 3, Stellaris, Hearts of Iron IV, and other Clausewitz titles will require new `GameProfile` and `ContentFamilyDescriptor` registries before their scope rules, symbol kinds, merge keys, and resource extraction can be trusted.

**Workaround:** treat foch as EU4-only for this alpha. Contributors adding a new game should start with narrow content-family descriptors, fixtures, and base-data coverage probes.

## Manual conflicts are expected on large EU4 playsets

The latest N=37 EU4 active playset probe reports `manual_conflict_count = 9` after the leaf-conflict fix. These are expected ambiguous structural disagreements between mods, not errors that foch should silently override.

**Workaround:** copy [`examples/eu4-default-foch.toml`](./examples/eu4-default-foch.toml) next to `dlc_load.json`, or write narrow `[[resolutions]]` rules using the DSL in [`docs/foch-toml-resolutions.md`](./docs/foch-toml-resolutions.md). Avoid global `last_writer` rules unless you explicitly want load-order semantics everywhere.

## Localisation and legacy encodings can still mojibake

The alpha decodes Paradox text through `decode_paradox_bytes`, with UTF-8/BOM handling plus GB18030, GBK, Big5/CJK detection, and Windows-1252 fallback via `chardetng`/`encoding_rs`. That covers common EU4 and translation-mod cases, but exotic or malformed encodings can still display as mojibake.

**Workaround:** keep suspicious localisation files under review, report minimal byte samples, and extend the `decode_paradox_bytes` funnel in `crates/foch-core/src/text.rs` when a repeatable encoding family appears.

## Cold performance is still rough without cache

Large-playset cold runs are expensive, especially in debug builds. The current N=37 baseline is roughly 25-30 minutes cold in debug, while warm cache-backed iterations are seconds and release+cache runs have been observed around 40 seconds.

**Workaround:** use a release binary or `cargo install --path crates/foch-cli`, keep cache enabled, inspect it with `foch cache stats` / `foch cache list`, and reserve debug cold runs for engine debugging.

## TUI conflict resolver has limited Windows coverage

The terminal conflict resolver works on Linux and macOS in regular TTYs. Windows terminal behavior has had less alpha testing.

**Workaround:** on Windows, prefer prewritten `foch.toml` resolutions or `--non-interactive` batch runs until your terminal path is validated. Please report terminal, shell, and reproduction details for TUI bugs.

## VS Code extension is preview quality

The VS Code extension is bundled and can launch `foch lsp`, but it should be treated as preview software. Diagnostics, completions, and go-to-definition are the intended alpha surface; merge-conflict workflows are still CLI-first.

**Workaround:** use the extension for editing assistance, then run `foch check`, `foch merge-plan`, and `foch merge` in a terminal for authoritative reports.

## NoOp dedup policies are opt-in per content family

Per-entry NoOp and cross-file NoOp dedup are only enabled for content families where EU4 load semantics have been verified. Some EU4 roots intentionally remain conservative because duplicate-looking entries can still be meaningful.

**Workaround:** do not assume every apparent duplicate should disappear. If a root is safe, add a focused fixture and opt-in policy through its `ContentFamilyDescriptor`.

## Graph artifacts have no interactive viewer yet

`foch graph` writes `definition-deps.json/.dot` and `mod-deps.json/.dot` for workspace and per-mod scopes, including `SymbolKind` filtering for definition dependencies. There is no interactive graph viewer in the alpha release.

**Workaround:** open DOT files with Graphviz or another external graph tool, and use the JSON artifacts for scripts and CI checks.
