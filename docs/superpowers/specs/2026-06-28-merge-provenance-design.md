# Merge Provenance — per-definition source attribution (Slice A)

Status: implemented
Date: 2026-06-28
Scope: foundation + two non-invasive channels (sidecar, comments). Opt-in.
Deferred to Slice B: `gui_tooltip`, `lsp_hover`.

## Implementation notes (as built)

- The adopted-set is computed over **all DAG contributors** (not only mods that
  produced a patch). Iterating every contributor is what credits the
  lowest-precedence mod when it acts as the synthetic base under `--no-game-base`
  (it produces no patch yet founds the merge). A contributor is credited when one
  of its blocks is emitted verbatim, or a child it added/changed vs **vanilla**
  survives in the output; mods that only re-ship vanilla, and overridden losers,
  are excluded.
- Top-level keys can **repeat** at file root (a scripted-effect union emits
  several `key = { … }` blocks that the game runs in sequence). Provenance
  aggregates across all same-key blocks rather than keying off a single block.
- The inline comment is ASCII (`# foch: <key> from <names>`) to stay safe across
  Clausewitz file encodings; names are mod display names in precedence order.
- `MODSET_CACHE_FORMAT_VERSION` bumped to `…-provenance-v6`; the modset key also
  encodes `provenance={bool}`. Per-file `DAG_BASE`/`MOD_DIFF` caches unchanged
  (provenance is derived after, from in-memory contributors).

## Problem

When `foch merge` structurally combines a definition from several mods, the
emitted output erases which mod each piece came from. The user wants every
merged "part" to carry a "this is from `<mod>`" annotation so a human can see,
per definition, which mod(s) contributed it.

## Goal

For each emitted file, attribute every top-level definition to the mod(s) that
contributed it, and surface that attribution through two non-invasive channels,
**off by default**:

1. **sidecar** — `.foch/foch-provenance.json`: `path -> { definition_key -> [mod_id...] }`.
2. **comments** — a `# foch: <key> — from <mod display names>` line emitted
   immediately before each top-level definition in the merged text.

Both are gated behind a single opt-in `--provenance` flag. With the flag off,
output is byte-identical to today.

## Non-goals (this slice)

- In-game tooltips (`gui_tooltip`) and LSP hover (`lsp_hover`) — Slice B, they
  consume the same foundation/sidecar.
- Per-leaf (sub-definition) provenance, and descent into single-wrapper
  container families (`guiTypes`/`spriteTypes`) for per-widget/per-sprite
  granularity. Granularity is the **top-level block** the user named ("top
  block"); container descent is a later refinement.
- Provenance for copy-through / overlay (whole-file) outputs beyond what the
  merge report already records (single contributor). The value is in
  structural-merge files where multiple mods combine; this slice scopes the
  sidecar + comments to **structural-merge files**.

## Foundation — deriving per-definition provenance (adopted-only)

Granularity is the **top-level block** (the named definition at file root —
`scripted_trigger`/`effect`/event/etc.). A block is annotated **only when a
mod's content is actually adopted into the final merged output** — mods whose
version is identical to base (no change) or was fully overridden by a
higher-precedence mod are **excluded**. This is computed in
`compute_dag_patches_from_parsed_with_cache`
(`crates/foch-engine/src/merge/planning/patch_deps.rs`) at the point it returns
`DagPatchComputation`, where the final `merged_statements`, the `base_statements`,
and every parsed contributor (`HashMap<ModId, ParsedScriptFile>`) are all in
scope — independent of caching (caches only affect how the final statements are
built, not their value).

The adopted test uses **canonical text signatures** — `signature(stmt) =
emit_clausewitz_statements(&[stmt])` (span-free, deterministic). For each
top-level key `K` present in `merged_statements`:

```
final_K = merged statement for K
base_K  = base statement for K (None if K is mod-introduced)
adopted: Vec<mod_id> = []   // precedence order, low → high
for M in contributors-with-K, ordered by file_dag precedence:
    m_K = M's statement for K
    if base_K is Some and signature(m_K) == signature(base_K):
        continue                         // M did not change K
    if signature(m_K) == signature(final_K):
        adopted.push(M)                  // M's version is the output (sole winner / identical)
    else:
        // block case: did any child M added/changed (vs base) survive into final?
        m_new   = children_sigs(m_K) - children_sigs(base_K)
        if !(m_new ∩ children_sigs(final_K)).is_empty():
            adopted.push(M)              // M contributed a surviving piece (union/merge)
        // else: M's contribution was fully overridden → NOT adopted
if !adopted.is_empty():
    definition_provenance.insert(K, adopted)
```

`children_sigs(stmt)` = the set of `signature(child)` for each child of a
`Block` value (empty for scalars; the scalar case is covered by the exact-match
branch). `definition_provenance: BTreeMap<String, Vec<String>>` — `BTreeMap` key
ordering + precedence-ordered `Vec` ⇒ deterministic. It is added to
`DagPatchComputation` and consumed downstream.

This correctly credits: a block added by one mod (that mod), a union where
several mods' children coexist (all of them), and an OverlayWins/last-writer
replacement (only the winner — the loser's unique children are absent from
`final_K`). It needs **no re-parsing** and **no provenance sink threaded through
the merge functions**.

## Channels

### Sidecar (`.foch/foch-provenance.json`)

- New field on `MergeReport` (`crates/foch-core/src/model/merge.rs`):
  `definition_provenance: BTreeMap<String /*path*/, BTreeMap<String /*key*/, Vec<String /*mod_id*/>>>`
  (BTree everywhere → deterministic serialization).
- Populated in `materialize.rs` from each `PatchBasedMergeOutput` (extended to
  carry the per-file map).
- Written by `execute.rs` next to `write_merge_report_artifact`, only when the
  flag is on, as `foch-provenance.json`. mod_ids are mapped to display names via
  the existing `mod_display_names`.

### Comments (inline `# foch: …`)

- In `patch_based_structural_merge`, after `dag_patches` is computed and **after**
  the noop/dedup step, if provenance is enabled, walk `merged_statements` and for
  each top-level `Assignment` whose key has an **adopted** provenance entry,
  insert an `AstStatement::Comment { text: "foch: <key> — from <display names>" }`
  immediately before it. Top-level blocks with no adopted-mod entry (pure
  vanilla, unchanged) get no comment.
- Mod ids are rendered as **display names** (via `mod_display_names`), joined by
  `, ` in precedence order.
- The emitter already renders `Comment` as `# <text>` (`emit.rs:66`). No emitter
  change needed.
- Comments are injected **after** the DAG/diff caches (they live inside
  `compute_dag_patches`), so the per-file merge cache stays comment-free and is
  unaffected.

## Opt-in flag threading

`provenance: bool` added to, in order:

1. CLI `MergeArgs` → `--provenance` (`crates/foch-cli`).
2. `MergeExecuteOptions` (`execute.rs:26`).
3. `MergeMaterializeOptions` (`materialize.rs:52`, default `false`).
4. `PatchBasedMergeContext` (`materialize.rs:929`); set at construction
   (`materialize.rs:320`).
5. Read in `patch_based_structural_merge` to gate comment injection and
   provenance collection.

## Cache correctness

Comment injection changes emitted bytes **only when the flag is on**. The
whole-output **modset tarball cache key** (`execute.rs:186`,
`MODSET_CACHE_FORMAT_VERSION`) currently encodes `include_base`; add
`provenance={provenance}` to the key so a `--provenance` run never reuses a
non-provenance cached tarball (and vice-versa). The per-file `DAG_BASE_CACHE`
and `MOD_DIFF_CACHE` are **not** bumped — comments are post-cache.

## Testing

- **Adopted union** (engine unit): mods A and B each add a distinct child to the
  same top-level block (Union policy) → `definition_provenance[K] == [A, B]`, and
  (flag on) a `# foch: K — from <A>, <B>` line sits directly above the block.
- **Overridden loser excluded**: mod A and higher-precedence mod B both define K
  with incompatible bodies, resolved OverlayWins (B wins) → provenance[K] == [B]
  only; A is **not** listed (its children are absent from the output).
- **No-op vs base excluded**: a mod that ships K byte-identical to vanilla
  produces no provenance entry / no comment for K.
- **Added-by-one**: K introduced by a single mod → provenance[K] == [that mod].
- **Determinism**: re-run the same fixture twice (flag on) → byte-identical
  output and identical `foch-provenance.json`.
- **Off-by-default**: flag off → output byte-identical to pre-change goldens
  (existing `merge_e2e` goldens must stay green).

## Verification gates

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features
-- -D warnings`, `cargo test --workspace`, and
`cargo test -p foch-engine --test merge_e2e`.

## Slice B (follow-up, not this PR)

- `gui_tooltip`: for GUI families, inject `tooltip = <loc_key>` into merged
  widgets + emit a localisation entry naming the source mod. Invasive (overrides
  existing tooltips, needs a loc file) → GUI-scoped, separate flag value.
- `lsp_hover`: the LSP reads `foch-provenance.json` and shows "merged from
  `<mods>`" on hover over a definition — the cleanest true "tooltip", no
  game-file pollution.
