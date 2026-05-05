# P5 audit: compensation hacks after recent merge fixes

Context audited from `master` at `a5d441a`, after the leaf-conflict fix (`212d2df`), fingerprint scope tightening, fallback-surface purge, and c1/c2/c3/c4 cache work.

## Summary

| Category | Findings | Safe delete / simplify now | Keep / review first |
| --- | ---: | ---: | ---: |
| A. Recurse policy emulation | 4 | 1 | 3 |
| B. Insert/remove fingerprint hacks | 5 | 4 | 1 |
| C. CLI tip + report rendering | 0 obsolete | 0 | 0 |
| D. `MergeReportConflictKind` removal leftovers | 0 obsolete | 0 | 0 |
| E. Cache-conditional logic | 0 obsolete | 0 | 2 kept |

`Safe delete / simplify now` means the hunk is either unreachable under current address fingerprinting or only stale explanatory text. Anything that changes recursive/named-container merge semantics should stay out of p5 cleanup and get design review.

## A. Recurse policy emulation in `patch_merge.rs`

### A1. Duplicate semantic-equivalence guard in `resolve_replace_blocks`

- **Location:** `crates/foch-engine/src/merge/patch_merge.rs:980-994`
- **Description:** `resolve_replace_blocks` repeats the semantic-equality convergence check already performed by `resolve_address` before dispatch (`patch_merge.rs:614-622`).
- **Disposition:** **Keep for now / review.** It is redundant in the normal call path, but it is a defensive direct-call guard inside a large test module. Deleting it is low semantic risk, but it changes stats (`auto_merged_patches` vs `convergent_patches`) if a test or future helper calls the resolver directly.
- **Estimated LoC delta if deleted:** ~15 LoC.

### A2. Recursive block re-diff helper

- **Location:** `crates/foch-engine/src/merge/patch_merge.rs:1021-1025`, `1133-1279`
- **Description:** `try_recursive_block_merge` handles `BlockPatchPolicy::Recurse` by selecting an ancestor body, re-diffing each replacement body, recursively calling `merge_patch_sets`, and applying nested resolved patches.
- **Disposition:** **Keep.** This is the current implementation of Recurse for sibling `ReplaceBlock` patches, not an obsolete compensation path. It delegates sub-conflict detection to the leaf resolvers instead of manually picking winners. Substantial changes here are outside p5 and need review.
- **Estimated LoC delta if deleted:** ~145 LoC, but deletion would break Recurse behavior.

### A3. Named-container 3-way merge helper

- **Location:** `crates/foch-engine/src/merge/patch_merge.rs:1027-1035`, `1281-1705`
- **Description:** `try_replace_block_named_container_merge` and `merge_named_container_bodies` manually index child identities and apply `NamedContainerPolicy` behavior.
- **Disposition:** **Keep / review.** This is separate named-container behavior, not Recurse fallthrough emulation. It can still resolve GUI-style containers when recursive diffing is not applicable. Any simplification needs a focused named-container design pass.
- **Estimated LoC delta if deleted:** ~400 LoC, but deletion would remove named-container merge support.

### A4. Stale `BlockPatchPolicy` overview text

- **Location:** `crates/foch-language/src/analyzer/content_family.rs:204-208`
- **Description:** The enum-level docs still say block patch policy decides whether “the last writer wins (default)”. The actual default baseline is `Recurse` (`content_family.rs:638-645`) and the `LastWriter` variant docs already say it is now explicit/rare.
- **Disposition:** **Safe simplify.** Documentation-only cleanup to avoid reintroducing fallback-era assumptions.
- **Estimated LoC delta if deleted/simplified:** ~2-4 LoC.

## B. Insert/remove fingerprint hacks

### B1. Cross-kind sibling pre-check rationale is broader than current behavior

- **Location:** `crates/foch-engine/src/merge/patch_merge.rs:347-363`, `431-492`
- **Description:** `detect_cross_kind_sibling_conflicts` groups by raw `(path, key)` so a fingerprinted `InsertNode`/`RemoveNode` and an unfingerprinted `SetValue`/`ReplaceBlock` still surface as one mixed-kind conflict.
- **Disposition:** **Keep, but simplify comments.** Since InsertNode/RemoveNode are now fingerprinted only for `BlockPatchPolicy::Union`, default Recurse collisions already meet at `(path, key)` and the normal mixed-kind check fires. The pre-check is still needed for Union-key mixed-kind collisions, where fingerprinted node patches would otherwise split away from scalar/block patches.
- **Estimated LoC delta if deleted:** ~65 LoC, but deletion would regress Union mixed-kind conflicts. Comment simplification is ~8-12 LoC.

### B2. `statement_fingerprint` docs still describe fingerprint-everywhere compensation

- **Location:** `crates/foch-engine/src/merge/patch_merge.rs:196-211`
- **Description:** The doc comment says statement fingerprinting preserves intent for repeated-key parents and “genuinely-unique keys”, and references the old `compatible_inserts` highest-precedence path.
- **Disposition:** **Safe simplify.** The function is still needed for Union, but the docs should state that the caller only uses it for Union-scoped node addresses.
- **Estimated LoC delta if deleted/simplified:** ~8-12 LoC.

### B3. Dead contributor list in `resolve_insert_nodes`

- **Location:** `crates/foch-engine/src/merge/patch_merge.rs:713-726`
- **Description:** The divergent-insert conflict branch builds `mods` and then discards it with `let _ = mods;`. This appears to be residue from the removed compatible-insert/last-writer-style auto-merge branch.
- **Disposition:** **Safe delete.** It has no effect on conflict reporting or stats.
- **Estimated LoC delta if deleted:** 2 LoC.

### B4. Unreachable distinct-value branch in `resolve_append_list_items`

- **Location:** `crates/foch-engine/src/merge/patch_merge.rs:737-773`
- **Description:** `PatchAddress` fingerprints `AppendListItem` by value (`patch_merge.rs:139-142`), so distinct appended values no longer share an address. The “Different values → union” branch should be unreachable in the normal merge path.
- **Disposition:** **Safe delete/simplify.** The resolver can mirror `resolve_append_block_items`: same-address appends are convergent because the address already contains the value fingerprint.
- **Estimated LoC delta if deleted/simplified:** ~25-30 LoC.

### B5. Unreachable distinct-value branch in `resolve_remove_list_items`

- **Location:** `crates/foch-engine/src/merge/patch_merge.rs:912-945`
- **Description:** `PatchAddress` fingerprints `RemoveListItem` by value (`patch_merge.rs:143-146`), so distinct removed values no longer share an address. The `compatible_removals` branch is therefore stale in the normal merge path.
- **Disposition:** **Safe delete/simplify.** The resolver can mirror `resolve_remove_block_items`: same-address removals are convergent because the address already contains the value fingerprint.
- **Estimated LoC delta if deleted/simplified:** ~25-30 LoC.

## C. CLI tip text + report rendering

- **Location checked:** `crates/foch-cli/src/cli/handler/merge.rs:75-106`
- **Result:** No obsolete fallback surface found. `render_unresolved_conflict_tip` points users to `foch.toml [[resolutions]]` with `handler = "last_writer"` and does not mention a deprecated CLI flag.
- **Other checks:** `crates/foch-language/src/analyzer/report.rs`, `crates/foch-engine/tests/check_engine.rs`, and `crates/foch-engine/tests/patch_real_mods.rs` had no `fallback`, `fallback_resolved_count`, `last_writer_fallback`, `MergeReportConflictKind`, `LastWriterFallback`, or `TrueConflictSkipped` matches in the audited fallback-purge search. Generic analyzer terms such as `last_writer_overlay` are unrelated to the removed merge fallback surface.
- **Estimated LoC delta:** 0.

## D. Dead code from `MergeReportConflictKind` enum removal

- **Locations checked:** `crates/foch-core/src/model/merge.rs:132-138`, `crates/foch-engine/src/merge/materialize.rs:1132-1152`, `crates/foch-engine/tests/check_engine.rs:808-860`
- **Result:** No old enum constructors or helper paths remain. Current code constructs `MergeReportConflictResolution { path, reason, leaf_conflicts }` only.
- **Estimated LoC delta:** 0.

## E. Cache-conditional logic

### E1. Workspace script snapshot reuse in runtime binding

- **Location:** `crates/foch-engine/src/runtime/binding.rs:210-269`
- **Description:** `collect_workspace_scripts` tracks `cached_mod_ids` and `seen` while combining workspace snapshots with inventory fallback parsing.
- **Disposition:** **Keep.** This is deduplication/control flow, not a redundant c1-c4 memo cache. It avoids re-parsing mods that already have snapshots while still parsing contributors absent from snapshots.
- **Estimated LoC delta if deleted:** 0 safe; deleting would risk duplicate or missing parsed files.

### E2. In-run hash/key state around DAG base cache

- **Location:** `crates/foch-engine/src/merge/patch_deps.rs:31`, `161-169`, `273-385`
- **Description:** `MOD_ROOT_HASHES`, `cached_mod_root_hash`, and `processed_mod_hashes`/`deps_hash` support c2/c3 cache keys and avoid repeated root hashing inside the process.
- **Disposition:** **Keep.** c3 persists synthesized DAG bases, but `processed_mod_hashes` is still needed to form the cumulative dependency-set key. `MOD_ROOT_HASHES` avoids repeated `compute_mod_hash(root)` work before persistent cache lookup.
- **Estimated LoC delta if deleted:** 0 safe; deleting would either remove key construction or add repeated hashing.
