# Cache architecture

`foch` keeps user-visible caches under one root, resolved by `FOCH_CACHE_ROOT`, then the legacy `FOCH_CACHE_DIR`, then `dirs::cache_dir()/foch` with a repository-local fallback when the system cache directory is unavailable.

Current layers:

- `mods/` — engine mod snapshot cache.
- `diffs/` — engine per-mod diff cache.
- `dag-base/` — engine dependency-DAG base cache.
- `modsets/` — engine full playset result cache.
- `parse/v7/` — legacy parser-file cache.

Layer-specific overrides (`FOCH_MOD_PARSE_CACHE_DIR`, `FOCH_MOD_DIFF_CACHE_DIR`, `FOCH_DAG_BASE_CACHE_DIR`, `FOCH_MODSET_CACHE_DIR`, `FOCH_PARSE_CACHE_DIR`) remain supported for tests and advanced workflows. The legacy parser cache also checks the previous `parse_cache/v7/` location on a miss and lazily moves valid entries into `parse/v7/`.

## Lifecycle seam, not a shared key/value cache

The cache layers do not currently share enough key/value shape for a useful generic trait. `mods` keys include mod content hash plus foch/game versions and store a large semantic snapshot; `diffs` and `dag-base` have multi-part keys and store patch or AST statement vectors; `modsets` stores a tarball plus report keyed by a precomputed playset hash; `parse` keys are source-file paths and file signatures with byte-cap GC. A trait with associated `Key` and `Value` types would not let the CLI iterate heterogeneous layers as `Box<dyn Cache>` without adding an erased wrapper, and forcing store/lookup signatures into one shape would hide important validation inputs.

Instead, `foch-engine::cache::CacheLayerOps` is deliberately lifecycle-only: it lists on-disk entries and controls age purge, byte-cap eviction, total size, and clear operations without abstracting lookup or store semantics. The CLI iterates this seam across all five layers, so every layer receives the same lifecycle control plane and byte-cap policy while each cache keeps its existing key format, value format, and validation rules.

The parser cache payloads still live in `foch-language`, including the `parse/v7` on-disk format and legacy `parse_cache/v7` migration path, but parser-cache lifecycle is now owned from the engine seam. `foch-language` exposes only the minimal filesystem lifecycle functions needed by the engine wrapper.

Automatic post-command GC runs after successful `check`, `merge`, and `data build` commands and byte-caps every cache layer independently. The default cap is 1 GiB per layer and can be changed with `FOCH_CACHE_MAX_BYTES`; a freshly-produced artifact larger than the cap, such as a very large modset tarball, may be evicted immediately after the run unless the cap is raised.
