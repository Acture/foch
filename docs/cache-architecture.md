# Cache architecture

`foch` keeps every user-visible cache under one root. `FOCH_CACHE_ROOT` is the
only override. Without it, foch uses `dirs::cache_dir()/foch`; if that location
is unavailable or unwritable, it falls back to `target/foch-cache` in the
workspace.

Current layers:

| Layer | Format generation | Address |
|---|---:|---|
| `mods/` | `10.0.0` | Workshop ACF identity, mod ID, game/filter behavior, and foch version |
| `diffs/v6.0.0/` | `6.0.0` | Target, mod semantic key, vanilla, foch, and game hashes |
| `dag-base/v12.0.0/` | `12.0.0` | Dependency set, file, foch, and game hashes |
| `modsets/v14.2.0/` | `14.2.0` | Ordered mod semantic keys, resolutions, merge behavior, foch, and game/base versions |
| `cwt-rules/v0.11.0/` | `0.11.0` | CWT source-pack hash |
| `parse/v10.0.0/` | `10.0.0` | Parser mode plus source bytes |

Versioned cache format generations are SemVer strings. Opening one of those
layers creates its active generation and deletes every obsolete recognized
generation, including old integer namespaces such as `v9`. Obsolete payloads
are never decoded or migrated. The mod snapshot cache keeps its generation in
each flat filename and deletes files whose embedded generation is not current.

Workshop installation version is ACF-only: `(app_id, workshop_id, manifest_id)`
is the authority. The ACF-backed semantic key also binds the mod ID, game, and
normalized ignore-pattern set. The on-disk envelope separately binds the foch
version and repeats the stable game key; it never binds the detected installed
game version. Source-root paths and retained-path selections are deliberately
not part of this full-mod snapshot key. No key construction inventories or
hashes the Workshop tree. Mods without a trusted Workshop identity do not
receive a persistent semantic-snapshot key.

The mod snapshot stores a compressed rkyv semantic index, the complete sorted
loadable-file inventory discovered by its cold parse, compact no-op hints, and
one raw-input identity for each Clausewitz document needed by lazy AST loading.
A warm lookup happens before any source-root walk and restores that inventory
from the snapshot. On a cold miss, traversal is pruned to game-loadable roots;
foch then reads each script once, derives its size and BLAKE3 digest from those
already-loaded bytes, and passes the same bytes to the parser cache and parser.
Descriptor parsing is also deferred until after semantic-cache lookup; it reads
the descriptor once for dependency and `replace_path` semantics and computes no
separate content digest.
It does not retain raw source or parsed ASTs in the mod snapshot. On a warm
snapshot hit, only a script actually requested by merge planning is reopened;
that read is checked against the stored raw identity before the AST is accepted.
These per-document checks keep a lazy AST aligned with its semantic snapshot.
They are not a second installation-version model.

The parser cache is content-addressed and stores bincode payloads. It reads a
source file once on a miss, hashes the parser mode and raw bytes, and reuses the
same parse result across paths. A cache hit rebases the stored AST path to the
requested file. Lua and Clausewitz inputs cannot share an entry.

Retained-path runs first load the same complete semantic snapshot and inventory.
After module closure is expanded from those cached inventories, a smaller
transient snapshot may be built for the selected paths; that transient view is
never persisted as the complete ACF-keyed snapshot. Modset identities bind the
effective retained-path selection but deliberately exclude the destination
directory: cache restoration is transactional, while resolutions that depend
on prior output bypass the modset cache entirely.

## Integrity-audit boundary

Normal `check`, `merge`, cache lifecycle, and merge-quality acceptance/report
paths must not recursively read or hash a Workshop tree to establish its
version. A full-tree digest is permitted only in a separately invoked integrity
audit whose cost is explicit to the operator; it must not become a prerequisite
for normal cache hits or product acceptance. No current cache layer stores a
whole-tree input digest.

## Lifecycle seam

The layers do not share a useful key/value interface. Their validation inputs
and payloads are intentionally different: mod snapshots contain semantic
indexes, diffs contain patches, DAG bases contain AST statements, modsets contain
an archive plus report, and CWT/parser caches contain compiled or parsed forms.

`foch-engine::cache::CacheLayerOps` therefore exposes only filesystem lifecycle
operations: path, entry listing, total size, age purge, byte-cap eviction, and
clear. Each implementation operates on its own configured path; it never falls
back to a process-global default. The CLI uses this seam for `foch cache` and
automatic garbage collection.

Automatic GC runs after successful `check`, `merge`, and `data build` commands.
It applies a byte cap independently to every layer, retaining the newest entries
that fit. The default is 1 GiB per layer and `FOCH_CACHE_MAX_BYTES` overrides it.
The mod-snapshot writer rejects an entry whose compressed size exceeds its
layer cap; lifecycle GC handles older and other-layer entries.
