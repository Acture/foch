# Cache architecture

`foch` keeps user-visible caches under one root, resolved by `FOCH_CACHE_ROOT`, then the legacy `FOCH_CACHE_DIR`, then `dirs::cache_dir()/foch` with a repository-local fallback when the system cache directory is unavailable.

Current layers:

- `mods/` — engine mod snapshot cache.
- `diffs/` — engine per-mod diff cache.
- `dag-base/` — engine dependency-DAG base cache.
- `modsets/` — engine full playset result cache.
- `parse/v7/` — legacy parser-file cache.

Layer-specific overrides (`FOCH_MOD_PARSE_CACHE_DIR`, `FOCH_MOD_DIFF_CACHE_DIR`, `FOCH_DAG_BASE_CACHE_DIR`, `FOCH_MODSET_CACHE_DIR`, `FOCH_PARSE_CACHE_DIR`) remain supported for tests and advanced workflows. The legacy parser cache also checks the previous `parse_cache/v7/` location on a miss and lazily moves valid entries into `parse/v7/`.

## Why there is no shared `Cache<K, V>` trait yet

The cache layers do not currently share enough key/value shape for a useful generic trait. `mods` keys include mod content hash plus foch/game versions and store a large semantic snapshot; `diffs` and `dag-base` have multi-part keys and store patch or AST statement vectors; `modsets` stores a tarball plus report keyed by a precomputed playset hash; `parse` keys are source-file paths and file signatures with byte-cap GC. A trait with associated `Key` and `Value` types would not let the CLI iterate heterogeneous layers as `Box<dyn Cache>` without adding an erased wrapper, and forcing store/lookup signatures into one shape would hide important validation inputs.

Instead, the CLI uses a small `CacheLayer` enum dispatcher that exposes uniform operational controls: `stats`, `list`, `clean`, `clear`, and `where`. Revisit a shared trait after the parser cache is retired or all layers converge on a common key object and eviction policy.
