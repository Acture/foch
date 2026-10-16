# Cache architecture

Foch keeps user-visible persistent caches below one root. `FOCH_CACHE_ROOT` is
the only root override. Without it, Foch uses `dirs::cache_dir()/foch`; if that
path cannot be made writable, it falls back to `target/foch-cache` below the
repository root.

## Layers

| Layer | Purpose |
| --- | --- |
| `mods/` | Complete semantic snapshot and loadable-file inventory for an installed mod |
| `diffs/v<DATA_VERSION>/` | Address-patch evaluation deltas for a target/mod/base identity |
| `dag-base/v<DATA_VERSION>/` | Address-patch evaluation DAG ancestors |
| `cwt-rules/v<DATA_VERSION>/` | Compiled CWT rule packs left by earlier builds; no longer written |
| `parse/v<DATA_VERSION>/` | Parser-mode and source-byte addressed parse result |

Every layer, the installed base data and `analysis_rules_version` share one
generation, `DATA_VERSION` in `src/packages/foch/src/data_version.rs`. It is a
`0.0.N` pre-release version raised by one, at most once per pull request, when
a change makes any stored data unreadable or stale; every cache then rebuilds
together. The mod-snapshot layer records it in each entry's file name.

There is no full merge-output or modset archive cache. A merge owns its frozen
artifact tree in an analysis-scoped temporary directory until review and
commit. Increasing
`FOCH_CACHE_MAX_BYTES` cannot restore or relax that removed product shortcut.

## Merge timing history

Each `foch merge` appends one JSON line to `timings/merge-timings.jsonl` below
the cache root. The line records the foch and analysis-rules versions, the
input, the output directory, the exit status, every logged stage with its
start offset and wall-clock time, and the CPU time the tree kernels spent per
step summed across workers. Stages nest: a stage's interval contains the
stages it ran. Timing stays out of the merged mod so identical inputs keep
producing identical output bytes. The history is not a cache layer; cache
maintenance does not read or trim it.

## Generations and lifecycle

Format generations are SemVer namespaces. Opening a layer creates its current
generation when needed but does not delete other recognized generations. Old
payloads are never decoded as current data and are not migrated implicitly.
Keeping generations on open prevents a normal analysis from turning cache
discovery into an unrelated destructive operation.

Eviction is a lifecycle operation exposed by `src/packages/foch/src/platform/cache_store` and the
`foch cache` commands. Operators can inspect stats/listing and explicitly clean
by age, enforce a byte cap, or clear selected layers. Layer implementations own
their payload validation and addresses; the platform layer only owns filesystem
listing, sizing, eviction, and clearing.

`FOCH_CACHE_MAX_BYTES` sets the per-layer byte cap used by maintenance. The
default is 1 GiB. It is a storage policy, not part of merge correctness or the
Workshop acceptance contract.

## Mod snapshots

Trusted Workshop installation identity is the paired Steam ACF tuple
`(app_id, workshop_id, manifest_id)`. The semantic key also binds the mod ID,
game, ignore-pattern behavior, and relevant Foch format/version data. Building
the key does not inventory or recursively hash the Workshop tree.

The snapshot stores:

- a compressed semantic index;
- the complete sorted loadable-file inventory discovered on a cold parse;
- compact no-op hints; and
- a raw-input identity for each Clausewitz document that may later require a
  lazy AST.

A warm hit occurs before a source-root walk and restores the inventory. Only a
document requested by merge analysis is reopened, and its bytes are checked
against the stored raw identity before the AST is accepted. This aligns lazy
syntax with the semantic snapshot without inventing a second Workshop-version
model.

Mods without a trusted Workshop identity do not receive a persistent
installation-keyed semantic snapshot.

## Parser and CWT caches

The parser cache is content-addressed by parser mode and source bytes. The same
bytes may be reused across paths; a hit rebases the stored AST path to the
requested file. Lua and Clausewitz modes never share entries.

The product no longer writes the CWT cache. `build.rs` compiles the vendored
schema, then foch's supplementary rules in `src/packages/foch/src/game/eu4/cwt_supplement`,
and the binary embeds the pack; the `FOCH_CWTOOLS_SCHEMA_DIR`
maintainer override replaces both and is compiled fresh in every process, so an edit to the
compiler cannot be masked by a pack an earlier build stored. `foch cache` still
lists and cleans the `cwt-rules` entries earlier builds left. EU4
interpretation remains in `src/packages/foch/src/game/eu4`; compiled CWT facts do not prove EU4
runtime or merge semantics.

Analysis output cached elsewhere depends on the schema through scope
classification, so the active `cwt_schema_id` is part of
`analysis_rules_version`, which base snapshots are validated against and which
the persistent mod-snapshot key includes.

## Integrity boundary

Normal input inspection, check, merge analysis/commit, cache lifecycle, and
merge-quality acceptance must not recursively read or hash an entire Workshop
tree merely to establish its installation version. A full-tree digest is
permitted only in an explicitly requested integrity audit whose I/O cost is
stated to the operator. No current normal cache layer stores such a digest.

## Commands

```fish
foch cache where
foch cache stats
foch cache list
foch cache clean --help
foch cache clear --help
```

Use `--layer` to restrict maintenance to `parse`, `mods`, `diffs`, `dag-base`,
or `cwt-rules`. Cache maintenance never changes source mods, the EU4 install,
or an analyzed merge artifact waiting for commit.
