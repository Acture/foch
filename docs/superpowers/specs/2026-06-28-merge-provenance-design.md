# Merge Provenance — per-definition source attribution (Slice A)

Status: proposed
Date: 2026-06-28
Scope: foundation + two non-invasive channels (sidecar, comments). Opt-in.
Deferred to Slice B: `gui_tooltip`, `lsp_hover`.

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
- Per-leaf (sub-definition) provenance. Granularity is **top-level definition**
  (the named container / scripted entity), which is the unit users reason about.
- Provenance for copy-through / overlay (whole-file) outputs beyond what the
  merge report already records (single contributor). The value is in
  structural-merge files where multiple mods combine; this slice scopes the
  sidecar + comments to **structural-merge files**.

## Foundation — deriving per-definition provenance

`patch_based_structural_merge` already computes a `DagPatchComputation`
(`crates/foch-engine/src/merge/planning/patch_deps.rs`) carrying:

- `mod_patches: Vec<(String /*mod_id*/, usize /*precedence*/, Vec<ClausewitzPatch>)>`
- `merged_statements: Vec<AstStatement>`

Every `ClausewitzPatch` carries a `path: AstPath` (root→target keys) and, for
node-level ops, a `key`. The **top-level definition key** a patch belongs to is:

```
top_key(patch) = patch.path.first().cloned()
                 .unwrap_or_else(|| patch.key_or_empty())
```

(For a root-level `InsertNode`/`SetValue`/`ReplaceBlock`, `path` is empty and
`key` is the definition name; for nested edits, `path[0]` is the enclosing
definition.)

Algorithm (deterministic, ordered):

```
definition_provenance: BTreeMap<String /*key*/, Vec<String /*mod_id*/>>
// mod_patches is already in ascending DAG-precedence order.
for (mod_id, _prec, patches) in &mod_patches:
    for patch in patches:
        if let Some(k) = top_key(patch):
            let mods = definition_provenance.entry(k).or_default()
            if !mods.contains(mod_id): mods.push(mod_id.clone())  // insertion-order dedup
```

`BTreeMap` key ordering makes the map itself deterministic; the per-key `Vec`
preserves the precedence order `mod_patches` is already sorted in (lowest →
highest precedence), with insertion-order dedup so a mod that patches a key
twice appears once. Base-game-only keys (no mod patch) carry no provenance entry
and get no comment.

This needs **no re-parsing** and **no new tracking through the AST** — it reads
data the patch engine already produced.

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
  each top-level `Assignment`/`Item` whose key has a provenance entry, insert an
  `AstStatement::Comment { text: "foch: <key> — from <display names>" }`
  immediately before it.
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

- Unit (engine): a structural-merge fixture where mods A and B each add distinct
  named definitions to the same file → assert `definition_provenance` maps each
  key to the right mod, and (flag on) the emitted text contains the matching
  `# foch: <key> — from <A|B>` line directly above each definition.
- Determinism: re-run the same fixture twice (flag on) → byte-identical output
  and identical `foch-provenance.json` (guards the BTree ordering).
- Off-by-default: flag off → output byte-identical to a pre-change golden
  (existing `merge_e2e` goldens already assert this — they must stay green).

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
