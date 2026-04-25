# Structured Merge & Patch-Based Merge for Clausewitz Script

Research brief — July 2025

---

## 1. Executive Summary

foch currently performs **overlay merge**: parse each mod's AST, accumulate by merge-key,
fold later contributors onto earlier ones, re-emit. This works but is semantically
coarse — it cannot distinguish "mod A changed field X" from "mod A left field X
untouched." A **patch-based 3-way merge** treats each mod as a set of edits to the
base game AST, enabling precise conflict detection and automatic resolution of
non-overlapping changes.

**Recommended approach**: a hybrid inspired by **FSTMerge's semistructured merge**
and **GumTree-style tree matching**, with domain-specific patch primitives designed
for Clausewitz's key-value structure. This avoids the complexity of full patch
algebra (Darcs/Pijul) while being far more precise than the current overlay system.

---

## 2. Algorithm Families

### 2.1 Semistructured / AST-Based 3-Way Merge

| System | Core Idea | Applicability to Clausewitz |
|--------|-----------|----------------------------|
| **FSTMerge** (Apel et al., ICSE 2011) | Parse into Feature Structure Trees; merge by superimposing nodes matched by key; fall back to line-based merge for method bodies | **High**. Clausewitz blocks map directly to FST nodes. Key-matching is the natural merge strategy. Fallback to text merge handles unparseable edge cases. |
| **JDime** | Java-specific AST 3-way merge with fine/coarse granularity | Medium. The architecture (AST → match → merge) applies, but it is deeply Java-specific. |
| **IntelliMerge** | Uses IDE's PSI model; detects refactorings (renames, moves) | Low direct applicability. Clausewitz doesn't have refactoring patterns. The refactoring detection is unnecessary. |

**Key insight for foch**: FSTMerge's superimposition is almost exactly what
Clausewitz needs. A Clausewitz file is already a feature structure tree — blocks
are nodes, assignment keys are features, values are leaves. The "semistructured"
part (falling back to text diff within leaf values) handles cases where block
contents have no further structure.

**FSTMerge algorithm sketch for Clausewitz**:
```
for each node in union(base, modA, modB) matched by merge_key:
  if only modA changed node relative to base → take modA's version
  if only modB changed node relative to base → take modB's version
  if both changed identically → take either (convergent)
  if both changed differently → CONFLICT (or recurse into children)
  if node is new (not in base) → accept addition
  if node was deleted by one mod → accept deletion (unless other mod also changed it)
```

### 2.2 Tree Differencing Algorithms

| Algorithm | Complexity | Move Detection | Best For |
|-----------|-----------|----------------|----------|
| **GumTree** (Falleri et al., ASE 2014) | O(n log n) heuristic | Yes (strong) | Large ASTs, code diffs. Bottom-up + top-down matching. |
| **ChangeDistiller** (Fluri et al., 2007) | O(n²) | Partial | Change classification with taxonomy of 41 change types. |
| **Zhang-Shasha** (1989) | O(n³) ordered trees | No (generic edit script) | Optimal edit distance, small trees. |
| **RTED** (Pawlik & Augsten, 2011) | O(n³) worst case, robust | No | Optimal for any tree shape. |
| **Difftastic** (Rust) | Heuristic, fast | Partial | Practical code diffing. Uses Tree-sitter parsers. |

**Recommendation for Clausewitz**: GumTree's approach is the best fit. Its
bottom-up matching (match leaf nodes first by content hash, then propagate
matches upward) maps well to Clausewitz's structure:

1. Leaf values (scalars) → match by equality
2. Block nodes → match by merge key (assignment key, field value, etc.)
3. Top-down refinement → match unmatched children within matched parents

Zhang-Shasha/RTED are **overkill** — Clausewitz trees have natural node
identifiers (keys), so we don't need optimal edit distance over arbitrary ordered
trees. We can exploit the key structure for O(n) matching in the common case.

**Handling Clausewitz-specific challenges**:

- **Repeated keys** (list semantics): Clausewitz uses repeated keys like
  `add_accepted_culture = X`. These are **ordered children with the same key**.
  For tree diff, treat them as a list node and use **sequence diffing** (LCS) on
  the values, not tree edit distance.

- **Node reordering**: In most Clausewitz contexts, key order within a block
  doesn't matter semantically (e.g., `tax = 3` before or after `manpower = 5`
  is identical). For trigger/effect blocks where order matters, preserve the
  base game's order and interleave insertions.

- **Value modification**: Trivially detected — same key path, different scalar value.

### 2.3 Patch Theory (Darcs / Pijul)

Darcs and Pijul formalize patches as algebraic objects with:
- **Application**: patch transforms state A → state B
- **Inversion**: patch⁻¹ transforms state B → state A
- **Commutation**: given patches p, q, find p', q' such that p;q = q';p'

**Applicability to Clausewitz**: **Theoretically elegant but practically
excessive.** The algebraic properties (commutation, inversion) are designed for
version control history manipulation (cherry-picking, reordering). Mod merging
doesn't need history manipulation — we have a fixed topology (base → mod₁,
base → mod₂, ..., base → modₙ) and want a single merged result.

However, patch theory provides useful **conceptual vocabulary**:
- A mod IS a patch to the base game
- Two mods commute if they don't touch the same keys
- Conflict = failure to commute
- The merged result = sequential application of all commuting patches + conflict
  markers for non-commuting ones

**Verdict**: Borrow the mental model, not the implementation. The complexity of
implementing a full patch algebra (Pijul uses category theory and graph-based
representations) is not justified when we have a star topology.

### 2.4 CRDT Approaches

Tree-structured CRDTs (Logoot-Undo Tree, Treedoc, FC-Tree) handle concurrent
modifications to ordered trees by giving each node a globally unique identifier
and defining deterministic conflict resolution rules.

**Applicability**: **Low for foch's use case.** CRDTs solve the real-time
collaborative editing problem where operations arrive in arbitrary order and must
converge. Mod merging is a batch operation with full knowledge of all inputs.
CRDTs would add unnecessary overhead (unique IDs, tombstones, vector clocks).

The one useful idea from CRDTs: **LWW-Register** (Last-Writer-Wins) semantics
for scalar values, which foch already uses via `ScalarMergePolicy::LastWriter`.

### 2.5 Operational Transformation (OT)

OT transforms concurrent operations to be compatible. Like CRDTs, it targets
real-time collaboration. **Not applicable** — foch has all inputs available
simultaneously and doesn't need incremental transformation.

---

## 3. Proposed Edit Script / Patch Representation

### 3.1 Patch Primitives for Clausewitz

```rust
/// A path into the Clausewitz AST: sequence of keys from root to target node.
/// Example: ["decisions", "my_decision", "potential"] addresses the potential
/// block inside my_decision inside the decisions file.
type AstPath = Vec<MergeKey>;

enum ClausewitzPatch {
    /// Set or change a scalar value at a path.
    /// diff(base, mod) where mod changed `key = old` to `key = new`
    SetValue {
        path: AstPath,
        key: String,
        old_value: Option<AstValue>,  // None if key didn't exist
        new_value: AstValue,
    },

    /// Remove a key-value pair or block at a path.
    RemoveNode {
        path: AstPath,
        key: String,
        old_value: AstValue,  // preserved for conflict detection
    },

    /// Insert a new key-value pair or block at a path.
    /// Position is relative to the parent block's children.
    InsertNode {
        path: AstPath,
        key: String,
        value: AstValue,
        position: InsertPosition,
    },

    /// Replace an entire block's contents (when the mod rewrites a block
    /// extensively enough that per-field diffing isn't useful).
    ReplaceBlock {
        path: AstPath,
        key: String,
        old_block: AstBlock,
        new_block: AstBlock,
    },

    /// Append to a repeated-key list.
    /// e.g., mod adds `add_accepted_culture = norwegian`
    AppendListItem {
        path: AstPath,
        key: String,
        value: ScalarValue,
    },

    /// Remove from a repeated-key list.
    RemoveListItem {
        path: AstPath,
        key: String,
        value: ScalarValue,
    },
}

enum InsertPosition {
    /// After the node with this key (preserves relative ordering)
    After(String),
    /// Before the node with this key
    Before(String),
    /// At the end of the parent block
    Append,
    /// At the start of the parent block
    Prepend,
}
```

### 3.2 Diff Algorithm: `diff(base, mod) → Vec<ClausewitzPatch>`

```
function diff(base_block, mod_block, current_path):
    patches = []

    // Index both blocks by merge key
    base_index = index_by_merge_key(base_block)
    mod_index  = index_by_merge_key(mod_block)

    // Deletions: in base but not in mod
    for key in base_index - mod_index:
        patches.push(RemoveNode(current_path, key, base_index[key]))

    // Insertions: in mod but not in base
    for key in mod_index - base_index:
        patches.push(InsertNode(current_path, key, mod_index[key], Append))

    // Modifications: in both, but different
    for key in base_index ∩ mod_index:
        base_val = base_index[key]
        mod_val  = mod_index[key]
        if base_val == mod_val:
            continue  // unchanged
        match (base_val, mod_val):
            (Scalar(a), Scalar(b)):
                patches.push(SetValue(current_path, key, Some(a), b))
            (Block(a), Block(b)):
                // Recurse into nested blocks
                sub = diff(a, b, current_path + [key])
                if sub.len() > threshold:
                    patches.push(ReplaceBlock(current_path, key, a, b))
                else:
                    patches.extend(sub)
            _:
                // Type changed (scalar→block or vice versa): treat as replace
                patches.push(RemoveNode(current_path, key, base_val))
                patches.push(InsertNode(current_path, key, mod_val, Append))

    // Handle repeated-key lists separately
    for repeated_key in find_repeated_keys(base_block, mod_block):
        base_list = extract_list(base_block, repeated_key)
        mod_list  = extract_list(mod_block, repeated_key)
        for item in mod_list - base_list:
            patches.push(AppendListItem(current_path, repeated_key, item))
        for item in base_list - mod_list:
            patches.push(RemoveListItem(current_path, repeated_key, item))

    return patches
```

### 3.3 Merge Algorithm: `merge(base, patches_A, patches_B) → Result`

```
function merge(patches_a, patches_b):
    result = []
    conflicts = []

    // Group patches by (path, key) — the "address" of the change
    a_by_addr = group_by_address(patches_a)
    b_by_addr = group_by_address(patches_b)

    // Non-overlapping patches: apply directly
    for addr in a_by_addr.keys() - b_by_addr.keys():
        result.extend(a_by_addr[addr])
    for addr in b_by_addr.keys() - a_by_addr.keys():
        result.extend(b_by_addr[addr])

    // Overlapping patches: check for conflicts
    for addr in a_by_addr.keys() ∩ b_by_addr.keys():
        pa = a_by_addr[addr]
        pb = b_by_addr[addr]
        if pa == pb:
            result.extend(pa)  // convergent change
        else:
            // Apply merge policy from ContentFamily
            resolved = try_auto_resolve(pa, pb, policy_for(addr))
            if resolved.is_some():
                result.extend(resolved)
            else:
                conflicts.push(Conflict(addr, pa, pb))

    return MergeResult { patches: result, conflicts }
```

---

## 4. Comparison: Overlay Merge vs 3-Way Patch Merge

| Dimension | Current Overlay Merge | Proposed 3-Way Patch Merge |
|-----------|----------------------|---------------------------|
| **Granularity** | Per merge-key (top-level block) | Per AST node (any depth) |
| **"Unchanged" detection** | Cannot distinguish "mod kept base value" from "mod intentionally set same value" | Explicit: no patch = no change |
| **Conflict detection** | Only when multiple mods provide different blocks for same merge-key | When patches touch same (path, key) with different effects |
| **Additive changes** | Mod additions always included | Additions are explicit InsertNode patches; can detect if two mods add same block |
| **Deletion support** | No concept of deletion — if mod omits a key, base value persists | RemoveNode patch explicitly models deletion |
| **Nested block merge** | Only at top level; inner structure merged by last-writer overlay | Recursive diff at any depth; per-field conflict detection |
| **List (repeated key) merge** | Union policy exists but applied at top level | AppendListItem / RemoveListItem patches with set-diff semantics |
| **New files** | CopyThrough — works well | Same: new files = InsertNode at root (or CopyThrough shortcut) |
| **Performance** | O(n × m) where n = nodes, m = mods | O(n × m) for diff, O(p) for merge where p = patches; comparable |
| **Merge policies** | ContentFamily policies (Sum, Union, LastWriter, etc.) | Same policies, now applied at patch conflict resolution |
| **Diagnostics** | Limited to "conflict rename" | Rich: "mod A and mod B both change X.Y.Z from different values" |
| **Determinism** | Load order dependent via last-writer | Deterministic merge of non-conflicting patches; explicit conflict report |

### What improves with 3-way merge

1. **Fewer false conflicts**: Two mods that edit different fields within the same
   block (e.g., mod A changes `tax` and mod B changes `manpower` in the same
   province file) currently may trigger a conflict or last-writer-wins at the
   block level. With 3-way merge, they are automatically merged.

2. **Deletion awareness**: If a mod intentionally removes a trigger or effect,
   the overlay system can't express this. Patch-based merge handles it naturally.

3. **Better diagnostics**: Instead of "these mods conflict on ideas_group X",
   the system can report "mod A changes `free_leader_pool = 1` while mod B
   changes `free_leader_pool = 2` in national_ideas.txt::FRA_ideas".

4. **Recursive inner merge**: Country history files, decisions, events — all
   have deeply nested structure where inner blocks often change independently.
   Patch-based merge handles this automatically.

---

## 5. Key Challenges & Open Questions

### 5.1 Merge Key Ambiguity
Clausewitz has no universal key mechanism. The current `MergeKeySource` enum
(`AssignmentKey`, `FieldValue("id")`, `ContainerChildKey`, `LeafPath`) already
handles this, but patch addressing requires a canonical path. **The current
merge-key infrastructure is sufficient** — just extend it to produce paths.

### 5.2 Order Sensitivity
Some blocks (triggers, effects) are order-sensitive. The diff algorithm must
distinguish **order-significant** from **order-insignificant** blocks. This is
already partially encoded in foch's `ContentFamilyDescriptor` — it just needs a
new flag.

**Proposed solution**: In order-significant contexts, use LCS (Longest Common
Subsequence) for child matching instead of key-based matching. Insertions and
deletions are relative to the LCS.

### 5.3 Repeated Keys Without Unique Values
`remove_idea = yes` repeated in multiple places is hard to diff because the
repeated items have no unique identity. **Solution**: positional matching within
the parent block (LCS on values), combined with semantic knowledge from
`ListMergePolicy`.

### 5.4 Files Not In Base Game
Entirely new mod files have no base to diff against. The diff is trivially
"insert everything." **Already handled** by `CopyThrough` in the merge plan.
If multiple mods create the same new file, diff both against an empty base.

### 5.5 Full-File Overwrite Semantics
Some mods intentionally replace entire files. Detecting this vs. "mod just
changed a lot" is a heuristic. **Proposed**: If >80% of a file's top-level
nodes are changed, flag as `ReplaceBlock` at root level and skip per-node merge.

### 5.6 N-Way Merge (Beyond 2 Mods)
The star topology (base → mod₁, ..., base → modₙ) means we compute n
independent diffs against the base. Merging n patch sets is:
1. Group all patches by address
2. For each address, if only one mod touches it → apply
3. If multiple mods touch it → apply merge policy or flag conflict

This reduces to pairwise conflict detection on the patch sets, not n-way tree
merge. **Computationally tractable** for typical mod counts (2–20 mods).

### 5.7 Performance
- ~8,400 base game files, hundreds of mod files
- Diff: per-file, O(n) where n = nodes in file. Clausewitz files are small
  (typically <1000 nodes). Total: O(files × avg_nodes × mods).
- Merge: O(total_patches), dominated by sorting/grouping by address.
- **Estimated**: comparable to current overlay merge. The diff phase adds cost
  but enables skipping unchanged nodes in the merge phase.

### 5.8 Base Game Version Pinning
The patch model requires a specific base game version as the common ancestor.
foch already has `--game-version auto` for base data builds. Patches would be
computed against the base data snapshot, which is already versioned.

---

## 6. Recommended Implementation Path

### Phase 1: Diff Engine (foundation)
- Implement `diff(base_ast, mod_ast) → Vec<ClausewitzPatch>` using key-based
  matching (GumTree-inspired but simpler, exploiting Clausewitz's key structure).
- Wire it into the existing `MergeIr` stage: instead of folding overlays,
  compute diffs and store patches.
- Unit test against known base/mod pairs.

### Phase 2: Patch Merge
- Implement `merge(patches_a, patches_b) → MergeResult` with conflict detection.
- Integrate with `ContentFamilyDescriptor` policies for auto-resolution.
- Extend `MergeReport` with patch-level conflict diagnostics.

### Phase 3: Patch Application
- Implement `apply(base_ast, merged_patches) → AstFile` to produce the final
  merged AST from base + patches.
- Replace the current overlay-based emit path for `StructuralMerge` files.

### Phase 4: Polish
- Add order-sensitivity handling (LCS for trigger/effect blocks).
- Add heuristic for full-file-overwrite detection.
- Benchmark against the current overlay merge on real EU4 mod playsets.
- Report patch-level diagnostics in `foch check` output.

### Compatibility
The existing `ContentFamily` infrastructure (merge keys, scope policies, merge
policies) maps directly to patch operations. The `MergeKeySource` enum provides
addressing. The `MergePolicies` struct provides auto-resolution strategies. No
fundamental restructuring is needed — the patch engine is a **new stage inserted
between parsing and emitting**, replacing the overlay fold.

---

## 7. References

### Semistructured Merge
- Apel, S., Liebig, J., Brandl, B., Lengauer, C., Kästner, C. (2011).
  "Semistructured Merge: Rethinking Merge in Revision Control Systems."
  ICSE 2011. IEEE. https://ieeexplore.ieee.org/document/5970093

### Tree Differencing
- Falleri, J-R., Morandat, F., Blanc, X., Martinez, M., Monperrus, M. (2014).
  "Fine-grained and Accurate Source Code Differencing." ASE 2014.
  https://hal.science/hal-01054552
  GitHub: https://github.com/GumTreeDiff/gumtree
- Fluri, B., Würsch, M., Pinzger, M., Gall, H.C. (2007).
  "Change Distilling: Tree Differencing for Fine-Grained Source Code Change
  Extraction." IEEE TSE 33(11).
  https://dl.acm.org/doi/10.1145/1371739.1371742
- Zhang, K., Shasha, D. (1989). "Simple Fast Algorithms for the Editing Distance
  between Trees and Related Problems." SIAM J. Computing 18(6).
- Pawlik, M., Augsten, N. (2011). "RTED: A Robust Algorithm for the Tree Edit
  Distance." VLDB 2011.

### Difftastic (Rust tree diff)
- Difftastic. Syntax-aware diff tool written in Rust.
  https://github.com/Wilfred/difftastic
  Manual on tree diffing: https://difftastic.wilfred.me.uk/tree_diffing.html

### Patch Theory
- Cretin, J. "A Theory of Patches."
  https://www.irif.fr/~jcretin/publis/patchTheory.pdf
- Darcs patch theory: http://wiki.darcs.net/Theory/Implementation
- Pijul: https://pijul.org/manual/basic-concepts.html

### CRDTs and Collaborative Editing
- Kleppmann, M., Beresford, A.R. (2017). "A Conflict-Free Replicated JSON
  Datatype." IEEE TPDS. https://arxiv.org/abs/1608.03960
- Oster, G., et al. (2006). "Data Consistency for P2P Collaborative Editing."
  CSCW 2006.

### JSON Merge/Patch Standards
- RFC 7396: JSON Merge Patch. https://tools.ietf.org/html/rfc7396
- RFC 6902: JSON Patch (operation-based). https://tools.ietf.org/html/rfc6902

### Rust Libraries
- `treediff` crate: https://github.com/Byron/treediff-rs
- `similar` crate: https://github.com/mitsuhiko/similar
- `tree-ds` crate: https://crates.io/crates/tree-ds

### Existing Paradox Mod Tools
- PdxMerge: https://github.com/mrmenno/PdxMerge
- Irony Mod Manager (community mod merging tool for Paradox games)
