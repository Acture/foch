# Cache Pipeline Code Review — 2026-05-XX

**Commit:** `0da9de9`  
**Scope:** c1 (mod_parse_cache), c2 (mod_diff_cache), c3 (dag_base_cache), c4 (modset_cache)  
**Reviewer:** Code Review Agent  

---

## BUG Severity

### 1. Path Traversal in c4 Tarball Extraction
**File:** `crates/foch-engine/src/cache/modset_cache.rs:221-227`  
**Severity:** BUG (ship before alpha)  
**Problem:** `unpack_modset_tarball` calls `archive.unpack(out_dir)` without path sanitization. The Rust `tar` crate (v0.4) does NOT prevent path traversal by default. A malicious or corrupted cache tarball containing entries like `../../etc/sensitive` will write outside `out_dir`, potentially overwriting system files or user data.  
**Evidence:** 
- Line 226: `archive.unpack(out_dir)` has no sanitization wrapper
- Rust tar crate documentation confirms path traversal is possible without explicit checks
- Cache files are writable by the user, so local corruption or malicious modification is a real threat  
**Suggested fix:** Before unpacking, iterate entries with `archive.entries()` and validate each entry's path. Reject any path containing `..`, absolute paths, or symlinks that escape `out_dir`. Alternatively, use `archive.set_preserve_permissions(false)` and manually extract each entry after validation.

---

## RISK Severity

### 2. c2/c3 Missing game_version in Cache Key
**File:** `crates/foch-engine/src/cache/mod_diff_cache.rs:60-74` (c2), `crates/foch-engine/src/cache/dag_base_cache.rs:59-73` (c3)  
**Severity:** RISK  
**Problem:** c2 (mod_diff_cache) and c3 (dag_base_cache) cache keys omit `game_version`. If the same mod content is used with EU4 1.36 vs 1.37, and vanilla game files changed, c2/c3 will incorrectly serve stale patches/DAG-base data. c1 and c4 correctly include `game_version`.  
**Evidence:**
- c2 lookup signature (line 60-66): only `target_path, mod_hash, vanilla_hash, foch_version`
- c3 lookup signature (line 59-64): only `deps_hash, file_path, foch_version`
- c1 (line 274-279) and c4 (line 202-218) both include `game_version`  
**Suggested fix:** Add `game_version: &str` parameter to c2/c3 `lookup` and `store` signatures. Update `cache_key` functions (c2:176, c3:150) to include `game_version` in hash. Bump `MOD_DIFF_CACHE_VERSION` and `DAG_BASE_CACHE_VERSION` to invalidate old entries.

### 3. c2/c3 Cache Version Not Encoded in Filename
**File:** `crates/foch-engine/src/cache/mod_diff_cache.rs:161-174` (c2), `crates/foch-engine/src/cache/dag_base_cache.rs:145-156` (c3)  
**Severity:** RISK  
**Problem:** c2 and c3 encode `cache_version` only in the payload, not the filename. After a version bump, old cache files remain on disk and are read/deserialized before the version check rejects them. This wastes I/O and creates a window where corrupted old files could trigger crashes if payload schema changed incompatibly.  
**Evidence:**
- c2 filename (line 168-173): `{mod_hash}__{vanilla_hash}__{key}.bin` (no version)
- c3 filename (line 147): `{deps_hash}__{key}.bin` (no version)
- c1 filename (line 1127-1133) correctly includes `__cv{cache_version}__`  
**Suggested fix:** Add `__cv{cache_version}__` segment to c2/c3 filenames (matching c1's pattern). This ensures old entries are never read after a version bump.

### 4. File Handle Leak in c1 compute_mod_hash on Read Error
**File:** `crates/foch-engine/src/cache/mod_parse_cache.rs:1094-1103`  
**Severity:** RISK  
**Problem:** If `file.read()` fails mid-loop (e.g., disk I/O error, file deleted by another process), the function propagates `?` without explicitly closing the file handle. While Rust's RAII ensures the handle is dropped eventually, in a long-running process scanning many mods, repeated I/O errors could accumulate open FDs before the GC runs, risking "too many open files" errors.  
**Evidence:**
- Line 1094: `let mut file = fs::File::open(&absolute)?;`
- Line 1098-1102: `file.read(&mut buffer)?;` can fail mid-loop, handle dropped via RAII
- No explicit `Drop` trait or early-close on error path  
**Suggested fix:** Wrap the read loop in a closure or use a `defer`-like pattern to ensure timely FD release. Alternatively, accept that RAII is sufficient here and rely on the existing error propagation (lower priority).

---

## SMELL Severity

### 5. Missing cache_version Bump Tests for c2/c3/c4
**File:** Test suites in `mod_diff_cache.rs`, `dag_base_cache.rs`, `modset_cache.rs`  
**Severity:** SMELL  
**Problem:** c2, c3, and c4 lack tests that verify cache invalidation when `CACHE_VERSION` is bumped. c1 has `cache_lookup_miss_on_version_bump` (line ~1260), but c2/c3/c4 do not.  
**Evidence:**
- c2 tests (line 267-343): no version-bump test
- c3 tests (line 240-299): no version-bump test
- c4 tests (line 381+): no explicit cache_version test (acceptable since c4 has no version constant)
- c1 test exists: `cache_lookup_miss_on_version_bump`  
**Suggested fix:** Add `{layer}_cache_invalidates_when_cache_version_bumps` tests for c2 and c3. Store an entry with version N, re-open cache with version N+1, verify lookup returns None.

---

## Summary

**BUGs:** 1 (path traversal in c4 tarball extraction — ship blocker)  
**RISKs:** 4 (missing game_version in c2/c3, version not in c2/c3 filenames, file handle leak in c1)  
**SMELLs:** 1 (missing test coverage)  

**Atomicity:** All layers use write-then-rename ✅  
**Hash determinism:** All layers sort inputs or use length-prefixed hashing ✅  
**Mod-content hash excludes mtime:** c1 confirmed via test ✅  
**Error handling:** All layers degrade gracefully (cache miss on error) ✅  

**Path traversal concern:** CONFIRMED — c4 unpack must sanitize paths before extraction.
