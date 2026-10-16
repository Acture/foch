# Known issues for the foch alpha

These are user-visible alpha limitations and practical workarounds. They are not release blockers unless a specific workflow depends on them.

## Rustls advisory in 0.0.2

The `0.0.2` CLI includes rustls `0.23.40`, affected by the moderate
[GHSA-2mjx-qc3c-rqvc](https://github.com/rustls/rustls/security/advisories/GHSA-2mjx-qc3c-rqvc)
TLS 1.3 handshake-boundary issue. Upstream states that handshake authentication
remains intact, but messages at the wrong encryption level can be accepted.
Network operations use this dependency; offline merge processing does not use
TLS. The fix is in rustls `0.23.45` and is not included in this release.
The dependency upgrade is tracked separately for a subsequent release.

## EU4 is the only supported game

foch currently implements one concrete game: Europa Universalis IV. CK3,
Victoria 3, Stellaris, Hearts of Iron IV, and other Clausewitz titles need their
own verified loader behavior, content-family registry, base-data model, merge
keys, and resource extraction before they can be trusted. Reusable CWT support
under `src/packages/foch/src/game/schema` is not game support by itself.

**Workaround:** treat foch as EU4-only for this alpha. Contributors adding a new game should start with narrow content-family descriptors, fixtures, and base-data coverage probes.

## Manual conflicts are expected on large EU4 playsets

Ambiguous structural disagreements between mods are expected product outcomes,
not errors that Foch should silently override. Analysis computes structural
leaf outcomes before confirmation, so they can be reviewed without writing the
target directory.

**Workaround:** copy [`examples/eu4-default-foch.toml`](../examples/eu4-default-foch.toml) next to `dlc_load.json`, or write narrow `[[resolutions]]` rules using the DSL in [`foch-toml-resolutions.md`](foch-toml-resolutions.md). Avoid global `last_writer` rules unless you explicitly want load-order semantics everywhere.

## Script text is kept byte-faithful, so editors may show escape bytes

EU4 reads script `.txt` files as windows-1252 bytes, and translation mods
encode CJK text as escape byte sequences inside them for their font patch.
`decode_paradox_bytes` in `src/packages/foch/src/game/eu4/text.rs` reads valid
UTF-8 as UTF-8 and everything else as windows-1252, and generated scripts are
written back as windows-1252 whenever every character has a windows-1252
byte, so merged output keeps the source bytes. Editor views of such files
show those escape bytes as Latin characters rather than the rendered CJK text.
Localisation `.yml` files keep their own UTF-8 reader.

**Workaround:** none needed for merged output; read translated names in game.

## Two total-conversion cases have a cold-path outlier

Most mods in the fixed corpus do not declare `replace_path`. Two total
conversions are outliers: Elder Scrolls Universalis declares 107 paths and World
of Warcraft Universalis declares 108. A bounded `custom_ideas` diagnostic using
them was stopped after more than 6m39s while Foch inspected many base-game
content folders those mods replace. This is not a representative runtime
baseline for ordinary mods.

**Workaround:** use a release binary or `cargo install --path src/apps/foch-cli`,
keep parser and semantic caches enabled, inspect them with `foch cache stats` /
`foch cache list`, and reserve debug cold runs for engine debugging.

Foch no longer has a full merge-output/modset archive cache. Increasing
`FOCH_CACHE_MAX_BYTES` must not be used to relax the fixed acceptance contract.
Optimize the total-conversion path only if a current acceptance result
reproduces the timeout.

## Paths that differ only in letter case are reported, not unified

Foch keeps game paths case-sensitive because EU4's own case behavior has not
been verified. `Common/ideas/x.txt` and `common/ideas/x.txt` therefore stay
separate inputs: the unexpected spelling misses its content family and vanilla
ancestor, and committing both to a case-insensitive filesystem (default NTFS
or APFS) leaves one file for the pair. `foch check` reports each group as
`case-only-path-collision`, and the merge report warns with
`case_only_path_collision`.

**Workaround:** treat the warning as a review item. Check which spelling the
game loads and fix the source mods or the merged output by hand.

## TUI conflict resolver has limited Windows coverage

The terminal conflict resolver works on Linux and macOS in regular TTYs. Windows terminal behavior has had less alpha testing.

**Workaround:** on Windows, prefer prewritten `foch.toml` resolutions or `--non-interactive` batch runs until your terminal path is validated. Please report terminal, shell, and reproduction details for TUI bugs.

## VS Code extension is preview quality

The VS Code extension is bundled and can launch `foch lsp`, but it should be treated as preview software. Diagnostics, completions, and go-to-definition are the intended alpha surface; merge-conflict workflows are still CLI-first.

**Workaround:** use the extension for editing assistance, then run `foch check`
and `foch merge` in a terminal for authoritative analysis and review.

## NoOp dedup policies are opt-in per content family

Per-entry NoOp and cross-file NoOp dedup are only enabled for content families where EU4 load semantics have been verified. Some EU4 roots intentionally remain conservative because duplicate-looking entries can still be meaningful.

**Workaround:** do not assume every apparent duplicate should disappear. If a root is safe, add a focused fixture and opt-in policy through its `ContentFamilyDescriptor`.

## Graph artifacts have no interactive viewer yet

`foch graph` writes `definition-deps.json/.dot` and `mod-deps.json/.dot` for
combined-input and per-mod scopes, including `SymbolKind` filtering for
definition dependencies. There is no interactive graph viewer in the alpha
release.

**Workaround:** open DOT files with Graphviz or another external graph tool, and use the JSON artifacts for scripts and CI checks.
