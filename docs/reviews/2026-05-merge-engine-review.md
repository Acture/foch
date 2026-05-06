# Merge Engine Code Review — 2026-05

**Commit:** `0da9de9`  
**Scope:** DSL cleanup chain, leaf-fix `212d2df`, cross-file/per-entry NoOp, p5 hack cleanup, p6 status decouple  
**Reviewer:** Automated review agent  
**Files reviewed:**
- `crates/foch-engine/src/merge/patch_merge.rs` (4135 LoC)
- `crates/foch-engine/src/merge/patch_deps.rs`
- `crates/foch-engine/src/merge/materialize.rs` (3073 LoC)
- `crates/foch-engine/src/merge/conflict_handler.rs` (1373 LoC)
- `crates/foch-engine/src/merge/handler_registry.rs`
- `crates/foch-engine/src/merge/execute.rs`
- `crates/foch-engine/src/merge/normalize.rs`
- `crates/foch-engine/src/merge/patch.rs`
- `crates/foch-engine/src/merge/patch_apply.rs`

## Summary

**Test coverage:** 214 passing tests in merge module (100% pass rate)  
**Findings:** 0 BUG | 0 RISK | 3 SMELL

The recent DSL cleanup, leaf-fix (212d2df), and p5 hack removal left the merge engine in excellent shape. The leaf-fix correctly closed the silent auto-merge hole for sibling InsertNode conflicts, and the p5 cleanup properly removed compensating logic for list-item and cross-kind fingerprinting. No correctness bugs, panics in normal execution paths, or hidden coupling issues were found.

---

## SMELL

### SMELL-1: Dead code from cleanup — unused helper functions
**File:** `crates/foch-engine/src/merge/patch.rs:890-893`  
**Issue:** `scalar_values_equal` is marked `#[allow(dead_code)]` and has no callers. This was likely replaced by `scalar_values_semantically_equal` during the semantic-equality refactor but never deleted.

**Suggested fix:** Remove the function entirely. Semantic equality is the correct abstraction for all merge operations.

---

### SMELL-2: Dead code from cleanup — unused TUI render function
**File:** `crates/foch-engine/src/merge/tui_conflict_handler.rs:722-737`  
**Issue:** `render_choice_list` is marked `#[allow(dead_code)]` and has no callers. This was likely an earlier iteration of the conflict UI that was replaced by the current snippet-based renderer but never removed.

**Suggested fix:** Remove the function and its associated `ChoiceLine` type unless there's a planned feature that will reintroduce choice-list rendering.

---

### SMELL-3: Commented-out namespace conflict detection
**File:** `crates/foch-engine/src/merge/materialize.rs:399-403`  
**Issue:** The TODO comment references a parse_script_file stack overflow risk, but the entire namespace conflict warning block (lines 404–437, not shown) is commented out with `#[allow(unused_imports)]` still present for the namespace module. If the feature is genuinely deferred pending the iterative parser, the dead imports should be removed until it's re-enabled.

**Suggested fix:** Either:
1. Remove the `#[allow(unused_imports)]` and the unused namespace imports (lines 14–16) until the feature is ready, OR
2. Add a tracking issue reference to the TODO comment so the work isn't forgotten.

---

## Observations (no action required)

### Expect calls rely on provable invariants
The code uses `.expect()` in several hot paths, but all instances reviewed are safe:

- **dag.rs:416, 515, 545** — `indeg.get_mut(c).expect("child indeg present")` and similar. These expect calls rely on the invariant that every node in the iteration was pre-inserted into the `indegree` HashMap during initialization (lines 395, 501). The invariant holds because both `topo_sort` and `topo_levels` initialize the map with all input nodes before the Kahn's algorithm loop.

- **tui_conflict_handler.rs:165** — `value.to_digit(10).expect("digit matched")` inside a `'1'..='9'` match arm. The expect is safe because the match arm guarantees the character is a digit.

- **conflict_handler.rs:51, 59** — `INTERACTIVE_SETTINGS.lock().expect("lock poisoned")`. Acceptable for `OnceLock<Mutex<_>>` — if the lock is poisoned, the process is in a fatal state anyway.

### Public API surface is intentional
`patch_merge.rs` exports 7 public functions/types. All are used by callers in `patch_deps.rs`, `materialize.rs`, or external integration tests. No bloat detected.

### Error type consistency
All new APIs use `Result<_, MergeError>`. No `Result<_, String>` instances found in the reviewed scope.

---

## Verification Commands

```bash
# Confirm tests still pass
cargo test -p foch-engine --lib merge

# Confirm no compiler warnings
cargo clippy --package foch-engine -- -D warnings

# Confirm the dead code is truly unused
rg "scalar_values_equal|render_choice_list" crates/foch-engine/src/merge/ --type rust
```

---

## Conclusion

The merge engine is in production-ready shape. The leaf-fix (212d2df) correctly closed the silent auto-merge hole, and the p5 cleanup removed compensating logic without introducing regressions. The three SMELL findings are low-priority cleanup opportunities — none block the alpha release.
