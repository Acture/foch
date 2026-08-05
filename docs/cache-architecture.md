# Cache architecture

`foch` keeps every user-visible cache under one root. `FOCH_CACHE_ROOT` is the
only override. Without it, foch uses `dirs::cache_dir()/foch`; if that location
is unavailable or unwritable, it falls back to `target/foch-cache` in the
workspace.

Current layers:

| Layer | Format generation | Address |
|---|---:|---|
| `mods/` | `4.0.0` | Selected mod content hash plus foch and game versions |
| `diffs/v6.0.0/` | `6.0.0` | Target, mod, vanilla, foch, and game hashes |
| `dag-base/v12.0.0/` | `12.0.0` | Dependency set, file, foch, and game hashes |
| `modsets/v14.0.1/` | `14.0.1` | Ordered mods, resolutions, merge behavior, foch, and game hashes |
| `cwt-rules/v0.11.0/` | `0.11.0` | CWT source-pack hash |
| `parse/v10.0.0/` | `10.0.0` | Parser mode plus source bytes |

Cache format generations are SemVer strings. Opening a layer creates its active
generation and deletes every obsolete recognized generation, including old
integer namespaces such as `v9`. Obsolete payloads are never decoded or
migrated. The mod snapshot cache keeps its generation in each flat filename and
deletes files whose embedded generation is not current.

Retained-path runs cache the exact selected mod snapshot under the selected
files' content hash. Modset identities deliberately exclude the destination
directory: cache restoration is transactional, while resolutions that depend
on prior output bypass the modset cache entirely.

The parser cache is content-addressed and stores bincode payloads. It reads a
source file once on a miss, hashes the parser mode and raw bytes, and reuses the
same parse result across paths. A cache hit rebases the stored AST path to the
requested file. Lua and Clausewitz inputs cannot share an entry.

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
A newly produced artifact larger than the cap can therefore be evicted at the
end of the command.
