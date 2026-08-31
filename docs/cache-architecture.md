# Cache architecture

Foch keeps user-visible persistent caches below one root. `FOCH_CACHE_ROOT` is
the only root override. Without it, Foch uses `dirs::cache_dir()/foch`; if that
path cannot be made writable, it falls back to `target/foch-cache` below the
repository root.

## Layers

| Layer | Current generation | Purpose |
| --- | --- | --- |
| `mods/` | `10.0.0` | Complete semantic snapshot and loadable-file inventory for an installed mod |
| `diffs/v6.0.0/` | `6.0.0` | Address-patch evaluation deltas for a target/mod/base identity |
| `dag-base/v12.0.0/` | `12.0.0` | Address-patch evaluation DAG ancestors |
| `cwt-rules/v0.11.0/` | `0.11.0` | Compiled reusable CWT rule pack |
| `parse/v11.0.0/` | `11.0.0` | Parser-mode and source-byte addressed parse result |

There is no full merge-output or modset archive cache. A merge owns its frozen
artifact tree in an analysis-scoped temporary directory until review and
commit. Increasing
`FOCH_CACHE_MAX_BYTES` cannot restore or relax that removed product shortcut.

## Generations and lifecycle

Format generations are SemVer namespaces. Opening a layer creates its current
generation when needed but does not delete other recognized generations. Old
payloads are never decoded as current data and are not migrated implicitly.
Keeping generations on open prevents a normal analysis from turning cache
discovery into an unrelated destructive operation.

Eviction is a lifecycle operation exposed by `src/platform/cache_store` and the
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

The CWT cache stores the compiled reusable rule pack keyed by the vendored
source-pack identity. EU4 interpretation remains in `src/game/eu4`; a compiled
CWT hit does not prove EU4 runtime or merge semantics.

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
