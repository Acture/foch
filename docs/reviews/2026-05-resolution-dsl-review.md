# Code Review: Resolution DSL (commit 7d7e19a)

**Reviewer:** Copilot CLI  
**Date:** 2025-01-XX  
**Commit:** 7d7e19a (merge: add resolution pattern DSL, Handler decision, handler registry)  
**Scope:** First production-path review of `[[resolutions]]` DSL surface

## Summary

Reviewed 5 implementation files and 1 documentation file (~3248 LOC) for schema correctness, pattern parsing, lookup precedence, handler dispatch, and documentation accuracy. Found **0 BUGs**, **2 RISKs**, and **2 test coverage gaps**.

**Top 3 issues:**
1. HashMap iteration in `ResolutionMap` violates documented declaration-order guarantee for `by_file` lookups (RISK)
2. No e2e test coverage for `defer` and `keep_existing` handlers in real merge flows (coverage gap)
3. Missing test for `priority_boost` selector end-to-end behavior (coverage gap)

## Findings

### RISK

#### RISK-1: HashMap violates declaration-order guarantee
**File:** crates/foch-core/src/config.rs:115-116  
**Problem:** `by_file` and `by_conflict_id` use `HashMap<PathBuf, _>` and `HashMap<String, _>`, which have **non-deterministic iteration order**. The documentation at line 244-250 and 308-310 claims lookup precedence is `conflict_id > file > pattern_rules`, which is correct for the single-lookup case. However, if the implementation ever needs to iterate over `by_file` entries (e.g., for diagnostics, serialization, or batch operations), the order will be arbitrary.

**Evidence:** 
```rust
// Line 115-117:
pub by_file: HashMap<PathBuf, ResolutionDecision>,
pub by_conflict_id: HashMap<String, ResolutionDecision>,
```
The documentation at docs/foch-toml-resolutions.md:244-250 explicitly states "declaration order" for pattern rules but doesn't mention that `by_file` has arbitrary ordering. The lookup implementation (lines 316-332) correctly prioritizes `by_conflict_id` → `by_file` → `pattern_rules` for single queries, but any future iteration over `by_file` will be non-deterministic.

**Impact:** Currently no bug because `lookup()` only reads single entries. Future code that iterates (e.g., debug dumps, validation passes, or merge report generation) may produce non-reproducible results.

**Suggested fix:** Change `by_file` and `by_conflict_id` to `BTreeMap` or `IndexMap` if declaration order matters for iteration. If single-key lookup is the only use case, document that iteration order is undefined.

---

#### RISK-2: Missing thread-safety audit for `LookupHandler`
**File:** crates/foch-engine/src/merge/conflict_handler.rs:296-359  
**Problem:** `LookupHandler` is `&mut self` in the `ConflictHandler::on_conflict` trait method (line 323). The comment at handler_registry.rs:18 notes "Handlers must never resort to silent last-writer choices for ambiguous cases," but doesn't document thread-safety requirements. If the merge engine ever parallelizes conflict resolution (e.g., per-file parallel merge), concurrent calls to `on_conflict` would race on the `current_conflict_index` and `total_conflicts` fields (lines 299-300).

**Evidence:**
```rust
// Line 296-300:
pub struct LookupHandler<'a> {
    pub map: &'a ResolutionMap,
    pub current_file: PathBuf,
    current_conflict_index: usize,
    total_conflicts: usize,
}
```
The `set_conflict_progress` method (lines 355-358) mutates these fields. If called from multiple threads, this would be undefined behavior.

**Impact:** Currently safe because merge is single-threaded (verified by checking materialize.rs:1001-1021 builds a single `LookupHandler` per file). Risk emerges if future optimizations add per-file parallelism.

**Suggested fix:** Document that `LookupHandler` is **not** `Sync` and requires single-threaded use, or refactor progress tracking to use `Arc<AtomicUsize>` if parallelism is planned.

---

### SMELL

#### SMELL-1: Handler registry case-insensitivity undocumented in error path
**File:** crates/foch-engine/src/merge/handler_registry.rs:38-49  
**Problem:** The dispatch function (line 38) lowercases handler names (`name.to_ascii_lowercase()`) for case-insensitive matching, but the unknown-handler error message at line 44 prints the **original** name (`other`), not the lowercased version. This could confuse users who write `handler = "Last_Writer"` and see `unknown merge handler 'Last_Writer'` instead of `'last_writer'`.

**Evidence:**
```rust
// Line 42-49:
other => {
    eprintln!(
        "[foch] unknown merge handler `{other}`; deferring conflict at {}::{}",
        current_file.display(),
        address.key
    );
    ConflictDecision::Defer
}
```

**Impact:** Minor confusion in error messages. The handler still defers correctly.

**Suggested fix:** Print the lowercased name in the error: `unknown merge handler '{}'` (formatting `name.to_ascii_lowercase()` instead of `other`).

---

#### SMELL-2: Documentation claims regex is case-sensitive but doesn't test it
**File:** docs/foch-toml-resolutions.md:126  
**Problem:** The doc says `"re:" non-empty Rust regex` (line 126) but never documents or tests whether `re:` prefix matching is case-sensitive. The implementation at config.rs:221 uses `strip_prefix("re:")`, which **is** case-sensitive. A user writing `RE:^events/` or `Re:^events/` would silently fall through to glob parsing and get unexpected results.

**Evidence:**
```rust
// config.rs:221
if let Some(re) = side.strip_prefix("re:") {
```
No test exercises `RE:`, `Re:`, or `rE:` variants. The documentation at line 126 doesn't say "case-sensitive prefix."

**Impact:** Low — most users will write lowercase `re:` by convention. Risk is silent misbehavior if they use uppercase.

**Suggested fix:** Add a sentence to docs/foch-toml-resolutions.md:126 clarifying "`re:` prefix is case-sensitive; `RE:` or `Re:` are treated as glob patterns." Add a test case rejecting or warning on uppercase variants.

---

### Test Coverage Gaps

#### GAP-1: No e2e test for `defer` or `keep_existing` handlers in real merge flow
**Evidence:** 
- `crates/foch-engine/tests/fixtures/playsets/eu4_two_mod_conflict_resolved/foch.toml` only tests `last_writer` handler (line 6).
- Handler registry unit tests cover `defer` and `keep_existing` in isolation (handler_registry.rs:224-240), but no end-to-end fixture exercises them in a full merge pipeline.
- Search for `handler.*defer` in e2e tests yields 0 results.

**Suggested fix:** Add two new fixtures:
1. `eu4_defer_to_interactive/` — uses `handler = "defer"` to verify it passes through to the next handler in the chain
2. `eu4_keep_existing_handler/` — uses `handler = "keep_existing"` with a pre-existing output file to verify materialization preserves it

---

#### GAP-2: No e2e test for `priority_boost` selector
**Evidence:**
- Unit test at config.rs:710-738 verifies `mod` + `priority_boost` parses and stores correctly.
- No test verifies that a boosted mod actually wins conflicts in the merge engine.
- The `mod_priority_boost` map is read by the merge engine (confirmed by grepping for `mod_priority_boost`), but no e2e fixture demonstrates the behavior.

**Suggested fix:** Add fixture `eu4_priority_boost/` with two mods at equal precedence, one boosted via `[[resolutions]] mod = "X" priority_boost = 100`, and verify the boosted mod wins.

---

## Schema Correctness (validated ✓)

- **Selector XOR enforcement:** Lines 336-342 correctly reject missing or multiple selectors. Test at line 745-750 passes.
- **Action XOR enforcement:** Lines 376-380 correctly reject multiple actions. Test at line 771-778 passes.
- **`mod` requires `priority_boost`:** Lines 349-354 enforce. Test at line 781-787 passes.
- **`priority_boost` requires `mod`:** Lines 364-368 enforce. No conflicting logic found.
- **`keep_existing` requires `file`:** Lines 382-386 enforce. Test at line 790-796 passes.
- **`handler` requires `match`:** Lines 370-374 enforce. Test at line 1039-1051 passes.
- **Empty selector/action case:** Covered by XOR validation (lines 337-341, 376-380). Missing-selector test at line 745-750 correctly rejects.

---

## Pattern DSL Parsing (validated ✓)

All advertised semantics have test coverage:
- `pure_glob` (line 902-908): ✓
- `pure_regex` (line 921-927): ✓
- `mixed_glob_file_regex_addr` (line 930-937): ✓
- `empty_addr_side` (line 940-944): ✓
- `empty_input` (line 948-950): ✓
- `empty_file_side` (line 954-956): ✓
- `empty_regex` (line 960-962): ✓
- `invalid_regex` (line 966-968): ✓

**Escape semantics:** Documentation at line 137 correctly explains TOML basic-string escaping (`\\.` for literal dot). Regex tests at lines 923, 932 use proper escaping. No mismatch found.

---

## Lookup Precedence (validated ✓)

**3-layer order `by_conflict_id > by_file > pattern_rules` is stable:**
- Implementation at lines 316-332 short-circuits correctly: conflict_id check at 322-324, file check at 325-327, pattern iteration at 328-331.
- Test at line 835-875 verifies conflict_id trumps file.
- No evidence of HashMap iteration order affecting single-key lookups (RISK-1 applies only to future iteration use cases).

**Pattern rules evaluated in declaration order:**
- `pattern_rules` is a `Vec<PatternRule>` (line 118), preserving insertion order.
- `.iter().find()` at line 328-330 returns first match, honoring declaration order.
- No bug found.

---

## Handler Registry (validated ✓)

- **`last_writer` tie-break:** Lines 72-79 implement `precedence.cmp().then_with(|| mod_id.cmp())`. Test at line 191-209 verifies lexicographic tie-break. Documentation at docs/foch-toml-resolutions.md:197 matches implementation.
- **Unknown handler defers:** Line 42-49 returns `ConflictDecision::Defer` and logs to stderr. Test at line 243-251 passes.
- **Case-insensitive dispatch:** Line 38 lowercases before match. Test at line 255-270 passes.

---

## LookupHandler Dispatch (validated ✓)

**Handler(name) forwards all 3 args:**
- Line 345-350 calls `handler_registry::dispatch(name, &self.current_file, address, conflict)`.
- Handler registry receives `current_file: &Path, address: &PatchAddress, conflict: &PatchConflict` (registry.rs:32-37).
- No arguments dropped. ✓

**HandlerResolutionRecord lands in report:**
- `PickModWithRecord` case at patch_merge.rs:522-540 pushes `record` to `result.handler_resolutions` (line 540).
- Wiring verified. ✓

---

## Documentation Honesty (3 spot-checks)

1. **Glob example** `common/ideas/**` (doc line 144):
   - Parser test at config.rs:903 validates this pattern.
   - Matches implementation. ✓

2. **Regex example** `re:^events/.*\.txt$` (doc line 163):
   - Parser test at config.rs:923 validates this pattern with proper escaping.
   - Matches implementation. ✓

3. **Mixed example** `common/**::re:^test\..*` (doc line 177):
   - Parser test at config.rs:932 validates this pattern.
   - Matches implementation. ✓

**No documentation drift detected.**

---

## EU4 Default Template vs Probe Rollup

The example at `examples/eu4-default-foch.toml` claims N=37 probe at commit `054af69` (line 9) surfaced 9 distinct conflict files (line 11). The template lists 9 resolution entries (lines 27-65): `00_country_ideas.txt`, `zzz_00_governments.txt`, `AngevinEmpire.txt`, `Portugal.txt`, `war_of_the_roses.txt`, `hre.gui`, `countrysubjectsview.gui`, `countrystabilityview.gui`, `countryestatesview.gui`.

**Cannot verify probe rollup** without access to `~/.copilot/session-state/.../files/probe-rollup-054af69.md`. Spot-check on template claims deferred to repository maintainer.

---

## Actionable Todos

1. **review-dsl-hashmap-order** — Decide whether `by_file` / `by_conflict_id` need deterministic iteration; migrate to `BTreeMap` or document arbitrary order.
2. **review-dsl-thread-safety** — Document `LookupHandler` single-threaded requirement or add `Arc<AtomicUsize>` for progress if parallelism is planned.
3. **review-dsl-defer-e2e** — Add e2e fixture for `handler = "defer"` to verify chain fallthrough.
4. **review-dsl-keep-existing-e2e** — Add e2e fixture for `handler = "keep_existing"` with pre-existing output file.
5. **review-dsl-priority-boost-e2e** — Add e2e fixture demonstrating `priority_boost` selector wins conflicts.
