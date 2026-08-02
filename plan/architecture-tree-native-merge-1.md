---
goal: Decouple production semantic merging from the address-patch reference implementation
version: 1.0
date_created: 2026-07-29
last_updated: 2026-08-02
owner: foch
status: In Progress
tags: [architecture, merge, refactor, gui, cache]
---

# Introduction

![Status: In Progress](https://img.shields.io/badge/status-In_Progress-yellow)

Replace every production dependency on the address-patch representation with tree-native contracts. The parser-independent `foch-merge-kernel` owns structural identity, revision deltas, N-way merge, provenance, conflicts, and decision evidence. `foch-language` supplies game and content-family semantics. `foch-engine` owns DAG execution, persistence, handlers, and materialization. The historical address-patch implementation remains executable only as an isolated merge-quality reference until parity gates permit its deletion.

Current checkpoint (2026-07-30): the kernel now exposes deterministic revision
deltas, tombstones, ordering facts, source/conflict identities, and decision
evidence contracts. Structured DAG conflicts and output directives use
`SemanticMergeComputation`; the synthetic tree-conflict-to-patch bridge has
been deleted. Runtime conflict decisions select an exact candidate in the
current `ConflictView`, and the tree path replays the corresponding
`ConflictNodeId + SourceNodeRef`, including original-source tombstones.
`resolution/` no longer imports patch types; the historical patch renderer is
isolated under `patch_engine`. The Clausewitz integration is no longer exposed
as one all-purpose adapter: `TreePartitionAdapter` supplies normalized
partitions, `TreeJoinProtocol` invokes N-way joins, `TreeSourceObserver`
records parent-relative deltas and lineage, and the definition-level trace
observer projects kernel `MergeDecisionEvidence`. File, event, and
definition-module adapters and joins are distinct concrete types. The main
structural materializer imports no address-patch type; reference conflict
rendering is isolated in `materialize/structural/reference.rs`. Exact
decisions are persisted as one-based `prefer_candidate` values bound to stable
conflict IDs. Each effective DAG node records a parent-relative
`SemanticSourceDelta`, and Structured stale-vanilla and dependency-misuse
observers consume those deltas. The mixed `DagMergeComputation` has been
deleted: production APIs return `SemanticDagMergeComputation`, reference-only
APIs return `ReferenceDagMergeComputation`, and materialization dispatches once
into typed semantic/reference paths. Structured execution no longer opens the
address-patch caches. Neutral planning now owns only DAG topology, the generic
effective-node walker, and definition-participant projection. The unused
patch-based `BaseResolver` has been deleted. Address-patch DAG state, cache
access, intent preservation, conflict replay, result construction, and
patch-derived provenance now live under `patch_engine/dag_protocol.rs` and
`patch_engine/dag_merge.rs`. `planning/dag_input.rs` is now the shared,
kernel-neutral source-preparation module. The planning-layer
`DagMergeEvaluation`, `MergeKernelMode` switch, and reference facade have been
deleted; `planning/dag_merge.rs` has a semantic-only production path.
Reference structural execution, interactive replay, observers, and
materialization are isolated in `materialize/structural/reference.rs`. A
source-boundary architecture test covers these neutral planning modules
together with the semantic model, policies, control flow, DAG join/protocol,
observers, resolution, and main materializer. Report field rename and
reference isolation in stale reporting, extraction of `foch-merge-reference`,
engine-level evaluation-mode removal, semantic caching, and remaining
domain/GUI policy parity remain open. The old binary three-way kernel has now
been deleted: two-revision joins and arbitrary revision sets use the same N-way
entrypoint and policy surface. CWT runtime consumers share the compiled
`RuleEngine`; the duplicate raw binder and unwired parser bridge are gone.
Persisted caches now share one root, use SemVer generations, delete obsolete
formats without migration reads, and avoid nesting the per-file parser cache
inside a mod-snapshot build.

N-way production checkpoint: `ClassMapping` accepts arbitrary revision
sets, rejected cross-revision links remain explicit, `NWayCorrespondence`
builds all base/revision and cross-revision matchings plus every original
revision delta. Conservative class selection now considers every original
source and tombstone at once, selects parents across all revisions, and feeds
all child sequences into one PCS computation. The kernel materializes one valid
tentative `NormalizedTree` with provenance, decisions, and typed conflicts.
Content-family policy receives all original contributors in one callback for
deletion, subtree/source selection, child-set selection, and scalar synthesis.
`TreeMergeKernel` now invokes this N-way computation once for file, event, and
definition-module units; its sequential three-way fold and post-hoc scalar
finalizer have been deleted.

Verified focused checkpoint: all 67 DAG-merge tests pass, covering semantic
tree joins and the isolated address-patch reference behavior. The expanded
semantic-pipeline architecture guard, `cargo fmt --all --check`, strict
`foch-engine` library/test Clippy, and `git diff --check` pass. Earlier kernel,
tree-protocol, semantic-observer, resolution, and neutral DAG topology
checkpoints remain unchanged. No workspace-wide or corpus run was used for
this architecture slice.

## 0. Target Boundaries and Critical Path

The production runtime pipeline is:

```text
Clausewitz source
       |
       v
foch-language::clausewitz_adapter
AST <-> NormalizedTree, trivia, source map
       |
       v
foch-merge-kernel
identity -> correspondence -> RevisionDelta -> N-way class state -> PCS
       |
       +<---- foch-language::semantic_policy
       |      ContentFamily, condition/control-flow semantics,
       |      reducers, GUI identity/fields/layout synthesis
       v
selection + subtree synthesis + conflicts + provenance + evidence
       |
       v
foch-engine
DAG protocol
  TreePartitionAdapter -> normalized partitions
  TreeSourceObserver   -> parent-relative SemanticSourceDelta + lineage
  TreeJoinProtocol     -> TreeMergeKernel -> partition-scoped SemanticMergeFacts
       |
       v
exact conflict replay
       |
       v
stale/no-op/provenance/trace observers
       |
       v
materializer/reporting
       |
       v
merged Clausewitz source
```

The historical implementation is outside that production chain:

```text
neutral evaluation input
       |
       +------> production pipeline
       |
       +------> foch-merge-reference
                historical address-patch implementation
                         |
                         v
                 foch-merge-quality
```

The capability ownership is explicit:

| Capability | Owner | Not owned by |
|---|---|---|
| AST/tree conversion, source spans, comments/trivia | `foch-language::clausewitz_adapter` | merge policy, patch engine |
| Node identity, matching, class mapping, deltas, N-way state, PCS | `foch-merge-kernel` | Clausewitz adapter, GUI code |
| Conditions, control flow, reducers, module semantics, GUI identity/layout/reference rules | `foch-language::semantic_policy` through `ContentFamily` | adapter, patch engine |
| Source selection, synthesized subtree contracts, conflicts, provenance, evidence | `foch-merge-kernel` protocols and algorithms | address patches |
| DAG execution, decision persistence/replay, observers, publication | `foch-engine` | parser adapter, reference engine |
| Terminal/editor/TUI rendering and source navigation | presentation modules | GUI layout semantics |
| Historical address-patch behavior | `foch-merge-reference` | every production crate |
| Human/reference/semantic comparison | `foch-merge-quality` | production merge path |

The boundaries are behavioral, not just directories:

- `foch-merge-kernel` owns node identity, matching, revision deltas, N-way
  merge, conflicts, source selection, provenance, and decision evidence. It
  never sees Clausewitz ASTs, files, GUI widgets, or patches.
- `foch-language::clausewitz_adapter` maps Clausewitz ASTs to and from
  normalized trees. It contains no merge strategy.
- `foch-language::semantic_policy` owns `ContentFamily` semantics, including
  condition and control-flow classification, reducers, module boundaries, GUI
  widget identity and hierarchy, coordinate/bounds/reference fields, layout
  anchors, and the configured policy for each field or container. These facts
  feed tree matching and synthesis and are neither adapter behavior nor
  presentation data.
- The Clausewitz adapter separately produces a source map from `SourceNodeRef` to
  AST node, file span, semantic path, and optional display label. Only this
  editor/TUI navigation metadata is presentation data; it must not be confused
  with GUI positioning or layout semantics.
- `foch-engine` walks the file/module DAG and passes only semantic contracts
  between merge, handler, observer, and materializer stages. `ConflictView` is
  a disposable presentation value; its display path or source range must never
  identify or replay a decision.
- The adapter does not own orchestration, conflict policy, observer logic, or
  materialization. Its only runtime products are normalized partitions,
  AST/source projection, trivia, and logical-equivalence services.
- `SemanticSourceDelta` records one effective contributor relative to its
  resolved parent state. `SemanticMergeFacts` records one N-way join outcome.
  Both are scoped by `SemanticPartitionId`; local `NodeId`, `RevisionId`,
  `ClassId`, provenance, and evidence values must never be compared across
  partitions or join events.
- Engine lineage composes immediate join sources back to original mods. A
  branch-tip source ID alone is not provenance for inherited ancestor content.
- `foch-merge-reference` owns every address-patch type, renderer, cache, and
  historical merge protocol. It consumes neutral evaluation inputs but cannot
  be reached from the production CLI path.
- `foch-merge-quality` is the only dual-run owner. It compares semantic output
  with archived human output and, temporarily, the executable reference.

Do not introduce a shared enum that contains both semantic and patch results.
During migration, the top-level evaluator may dispatch to two implementations,
but their internal state, conflicts, caches, and materializers remain separate.

Execute the work packages in this order:

1. Establish tree-native contracts. This checkpoint is substantially complete.
2. Implement first-class N-way correspondence, selection, ordering, and tree
   construction over every original revision.
3. Complete exact node-keyed conflict selection, stable decision persistence,
   source presentation, replay, and semantic materialization against that
   N-way outcome.
4. Port stale-vanilla, dependency-misuse, provenance, trace, and no-op
   observers to `RevisionDelta` and kernel evidence.
5. Extract the address-patch implementation into `foch-merge-reference`
   immediately after the production dependency is gone.
6. Port remaining domain and GUI policies through tree synthesis.
7. Add the unified semantic cache, parity gates, and final corpus run.

## 1. Requirements & Constraints

- **REQ-001**: Production code must not import or expose `ClausewitzPatch`, `PatchAddress`, `PatchResolution`, `PatchMergeResult`, `PatchMergeStats`, or any module under `merge/patch_engine`.
- **REQ-002**: `foch-merge-kernel` must remain parser-independent and must not depend on `foch-language`, `foch-engine`, tree-sitter, filesystem I/O, or game-specific types.
- **REQ-003**: The kernel must expose a first-class `RevisionDelta` that represents insert, delete, update, move, rename, tombstone, and ordering facts using revision-local nodes and merge-local equivalence classes.
- **REQ-004**: N-source merge must consume the base and all original revisions in one merge computation. Sequential three-way folds must not define N-way semantics.
- **REQ-005**: Conflict selection must identify an exact conflict node and exact source node. It must support repeated keys, ambiguous matches, control-flow nodes, moved nodes, and source deletions without relying on a string assignment path.
- **REQ-006**: Every automatic structural or policy decision must emit deterministic `MergeDecisionEvidence` with policy, reason, affected node, contributing sources, and selected or synthesized result.
- **REQ-007**: Every useful behavior currently specified only by address-patch tests must receive a tree-native implementation and regression test. This includes GUI coordinate/reference policies, `ScrollStack`, recursive/named-container merging, Boolean OR, union, numeric reducers, last-writer behavior, deletion handling, and rename/move correspondence.
- **REQ-008**: Tree normalization, matching, delta construction, and join results must use one content-addressed `SemanticMergeCache`; none of its keys or payloads may contain address-patch types.
- **REQ-009**: Stale-vanilla detection, dependency-misuse detection, provenance, merge trace, no-op detection, and conflict reporting must consume normalized trees, `RevisionDelta`, provenance, and decision evidence directly.
- **REQ-010**: The address-patch implementation must move out of `foch-engine` into `foch-merge-reference`. Only `foch-merge-quality` may depend on that crate.
- **REQ-011**: GUI files must be included in final architecture and corpus gates. Excluding GUI may remain a reporting stratum but must not be an implementation exemption.
- **REQ-012**: Comments and trivia must remain adapter-owned data and must not participate in node identity unless a content-family policy explicitly assigns semantic meaning.
- **CON-001**: `ContentFamily` and `MergePolicies` remain the game/domain behavior boundary.
- **CON-002**: Do not add compatibility aliases for removed internal patch names. Bump research artifact schemas when names or shapes change and preserve historical JSONL rows as immutable data.
- **CON-003**: Do not rerun the full corpus during ordinary implementation. Use focused unit/fixture tests per task and run the complete corpus only at declared phase gates.
- **CON-004**: Do not cache a temporary bridge representation. Implement the semantic cache only after `RevisionDelta`, conflict identity, and N-way outcome schemas are stable.
- **GUD-001**: Use regression-first TDD. Port the behavioral assertion, observe it fail on the tree path, then implement the tree-native capability.
- **GUD-002**: Prefer typed enums and structs over stringly typed paths, policy names, and decision reasons.
- **PAT-001**: Keep algorithms pure and composable. Filesystem persistence implements a cache interface outside `foch-merge-kernel`.
- **PAT-002**: Keep dependency direction one-way: `foch-merge-kernel` <- `foch-language`/`foch-engine` <- `foch-merge-reference`/`foch-merge-quality`.

## 2. Implementation Steps

### Implementation Phase 1: Establish tree-native contracts

- **GOAL-001**: Introduce the final vocabulary and result contracts without changing production merge behavior.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-001 | Add `delta.rs`, `decision.rs`, and `selection.rs` to `crates/foch-merge-kernel/src/`. Define `MergeInputId`, `ConflictNodeId`, `SourceNodeRef`, `RevisionDelta`, `DeltaOperation`, `Tombstone`, `OrderingFact`, `MergeDecisionEvidence`, and typed policy/decision enums. All types must derive deterministic comparison and serde traits where persisted. | Yes | 2026-07-29 |
| TASK-002 | Extend `MergeOutcome` in `crates/foch-merge-kernel/src/conflict.rs` with revision deltas and decision evidence. Keep `SourceSet` as the provenance primitive; do not add Clausewitz payloads. | Yes | 2026-07-29 |
| TASK-003 | Rename `BlockPatchPolicy` to `DivergentBlockPolicy`, `MergePolicies.block_patch` to `divergent_block`, and `block_patch_policies` to `divergent_block_rules` in `crates/foch-language/src/analyzer/content_family.rs`. Update profile declarations and remove patch-engine wording from policy documentation. | Yes | 2026-07-29 |
| TASK-004 | Add `crates/foch-engine/src/merge/model.rs` containing `SemanticMergeComputation`, `SemanticMergeConflict`, `SemanticConflictCandidate`, `MergeOutputDirective`, and source-level diagnostic summaries. These types may contain `AstStatement` only at the adapter/rendering boundary and must not contain patch types. | Yes | 2026-07-30 |
| TASK-005 | Add kernel tests proving deterministic delta serialization, deletion tombstones, move versus delete distinction, rename versus insert/delete distinction, and stable conflict IDs for identical input hashes. | Yes | 2026-07-29 |

Completion criteria:

- `foch-merge-kernel` remains dependency-minimal.
- Existing tree output is unchanged on focused file, event, and definition-module fixtures.
- New model modules contain no `patch` identifier except documentation that names the migration target.

### Implementation Phase 2: Remove patch-shaped conflict and handler state

- **GOAL-002**: Make tree conflicts, manual decisions, and materialization consume semantic node identities directly.
- **Dependency**: Complete the N-way outcome in GOAL-004 first. A decision
  recorded against a sequential fold identifies a synthetic intermediate tree,
  not an original source. Do not add another path-keyed replay bridge.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-006 | Replace path-keyed `TreeSourceSelections` in `structured/tree_kernel.rs` with selections keyed by `ConflictNodeId` and valued by `SourceNodeRef`. Replaying a decision must select the exact original subtree or tombstone. | Yes | 2026-07-30 |
| TASK-007 | Replace `TreeConflictRecord.address_path/address_key` with `conflict_node`, optional display path, typed kind, base source node, and complete source candidates. Display paths remain diagnostics and never participate in lookup. | Yes | 2026-07-30 |
| TASK-008 | Replace `CandidateView.patch_summary` and `patch_rendered` with `change_summary` and `candidate_rendered` in `resolution/conflict_view.rs`. Build views from `SemanticMergeConflict`; move the legacy patch renderer into the reference implementation. | Yes: neutral resolution model; renderer isolated under `patch_engine` pending TASK-042 | 2026-07-30 |
| TASK-009 | Change `ConflictHandler` and `handler_registry.rs` to consume neutral `ConflictView` values. Use exact `PickCandidate`, `Defer`, `UseFile`, `KeepExisting`, and `Abort` decisions, and persist exact conflict/source identities alongside human-readable paths. | Yes: runtime and persisted `prefer_candidate` replay select the exact current candidate | 2026-07-30 |
| TASK-010 | Delete `tree_merge_result` and `tree_conflict_candidate_patch` from `planning/dag_merge.rs`. Return `SemanticMergeComputation` directly from the semantic-tree path. | Yes | 2026-07-29 |
| TASK-011 | Refactor `output/materialize/structural.rs` to inspect `SemanticMergeConflict` and `MergeOutputDirective` directly. Remove every `PatchAddress`, `PatchConflict`, `PatchResolution`, and `PatchMergeResult` import from materialization. | Yes: semantic/reference finish paths are typed separately; patch rendering is isolated in the temporary reference submodule pending TASK-042 | 2026-07-30 |
| TASK-012 | Add repeated-key, ambiguous control-flow, moved-node, and deleted-source handler regressions. Each test must prove exact A/B/C selection and byte-identical replay. | Partial: A/B/C source, deletion, and duplicate display-mod identity covered; repeated control-flow and move/order cases remain | 2026-07-30 |

Completion criteria:

- No tree conflict is converted into a synthetic patch.
- Manual selection works for assignment, item, control-flow, move, and deletion conflicts.
- The handler and materializer compile without patch imports.

### Implementation Phase 3: Replace patch observers and post-hoc provenance

- **GOAL-003**: Remove the last production reason to compute address patches after a semantic merge.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-013 | During each effective DAG-node evaluation, compute and retain its parent-relative `RevisionDelta`. Add it to semantic DAG state instead of recomputing `compute_tree_diagnostic_patches`. | Yes: `TreeSourceObserver` stores typed file/definition deltas; no post-hoc semantic AST-to-patch diff remains | 2026-07-30 |
| TASK-014 | Rewrite stale-vanilla detection in `output/stale_vanilla.rs` and `output/materialize/stale_detect.rs` over typed delta operations. Replace `StaleVanillaTargetDescriptor.patch_kind` with `change_kind` and bump the merge-report schema. | Partial: Structured detection consumes typed deltas; report field/schema rename and reference isolation remain | 2026-07-30 |
| TASK-015 | Count dependency-misuse removals from delta tombstones/delete operations, including removed list items and moved-away nodes without confusing moves with deletions. | Yes: Structured counts assignment/item delete roots and excludes insert/update/move/rename; reference mode retains its historical collector | 2026-07-30 |
| TASK-016 | Build definition provenance from kernel `SourceSet` values and build merge trace from `MergeDecisionEvidence`. Remove post-hoc top-level patch-key reconstruction from `planning/dag_merge.rs`. | Partial: Structured provenance composes kernel lineage and trace projection consumes kernel evidence; reference-only patch reconstruction is isolated in `patch_engine/dag_merge.rs` pending crate extraction | 2026-07-30 |
| TASK-017 | Replace patch semantic-equality helpers in no-op publication checks with normalized-tree equality plus adapter-level logical equivalence where configured. | Yes for Structured; the reference path retains historical patch equality | 2026-07-30 |
| TASK-018 | Replace `DagMergeComputation.mod_patches` with `source_deltas`; delete `compute_tree_diagnostic_patches` and remove `ModDiffCache` use from the semantic path. | Yes: typed semantic/reference results replace the mixed result and caches open only for the reference kernel | 2026-07-30 |
| TASK-019 | Add an architecture test that fails if production modules reference the forbidden patch types from REQ-001. The allowlist is limited to the temporary reference module and merge-quality fixtures. | Partial: guard now covers neutral input preparation and the semantic-only `planning/dag_merge.rs` production path in addition to the prior semantic pipeline; stale-report reference code remains outside it | 2026-07-30 |

Completion criteria:

- `rg` finds no forbidden patch type in semantic DAG execution, resolution, output, reporting, or cache code.
- Stale-vanilla, dependency-misuse, provenance, trace, and no-op focused tests retain their prior behavior.
- Semantic merge performs no AST-to-patch diff.

### Implementation Phase 4: Implement first-class N-way merge state

- **GOAL-004**: Replace ordered three-way folding with one N-source structural computation that preserves intent across DAG joins.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-020 | Generalize `ClassMapping` from fixed base/left/right inputs to an ordered set of arbitrary `RevisionId` values and pairwise/seeded matchings. Preserve at most one node from each revision per class. | Yes | 2026-07-29 |
| TASK-021 | Add `nway.rs` to `foch-merge-kernel`. Compute base-to-revision correspondences, cross-revision recovery matches, per-revision deltas, tombstones, conservative class/parent selection, and PCS constraints before applying policy. | Yes | 2026-07-29 |
| TASK-022 | Extend `MergePolicy` contexts to expose every changed contributor for a class. Replace binary-only policy callbacks with N-source callbacks for node selection, scalar/subtree synthesis, deletion, child-set merge, and ordering. | Partial: deletion, source/subtree, scalar, and child-set callbacks are live; dedicated policy ordering/synthesis remains | 2026-07-30 |
| TASK-023 | Add tree-native subtree synthesis so domain policy can implement Boolean OR, union, and GUI `ScrollStack` without constructing an address patch. | | |
| TASK-024 | Replace `TreeMergeKernel`'s `for revisions.skip(1)` fold and N-source scalar finalizer with the N-way kernel entrypoint. Delete binary-fold-only state after focused parity passes. | Yes | 2026-07-30 |
| TASK-025 | Store effective normalized tree, surviving tombstones, order facts, provenance, decisions, and unresolved conflicts in `TreeDagState`. Propagate these facts through intermediate and final DAG joins. | Partial: partition-scoped source deltas and join outcomes propagate; original-source lineage and a canonical current normalized state remain open | 2026-07-30 |
| TASK-026 | Add N-way tests for three and four siblings, a dependency diamond, delete-after-move, move-plus-modify, rename-plus-modify, unanimous deletion, one-sided deletion, conflicting moves, and order cycles. | | |
| TASK-027 | Add permutation tests: commutative policies must be invariant to contributor order; precedence policies must be deterministic and must record the selected precedence. | | |

Completion criteria:

- The semantic kernel calls no three-way fold loop.
- A tombstone remains observable after at least two intermediate joins.
- All original source candidates and provenance survive an N-way conflict.

### Implementation Phase 5: Port all domain and GUI policies

- **GOAL-005**: Reimplement every retained legacy resolver behavior as a tree/domain policy and make GUI a first-class merge family.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-028 | Port scalar policy tests for `LastWriter`, numeric Sum/Avg/Max/Min, coordinate first-writer for numeric `x`/`y`, GUI first-writer for `maxWidth`/`maxHeight`, and GUI last-writer for `spriteType`. Unlisted GUI scalars must remain conflicts. | | |
| TASK-029 | Port recursive block, recursive remove/replace, recursive insert, union block, named-container union, and semantic-equivalence regressions to `structured/` or `foch-merge-kernel` tests according to ownership. | | |
| TASK-030 | Implement GUI widget identity from `name` and configured child type, including repeated widgets, nested containers, reparenting, sibling ordering, and position blocks. Identity must come from `ContentFamily`, not filenames or patch addresses. | | |
| TASK-031 | Implement `NamedContainerPolicy::SuffixRename`, `OverlayWins`, and `ScrollStack` through subtree synthesis. `ScrollStack` must preserve both complete source widget bodies, emit deterministic offsets/order, parse after emission, and retain source provenance. | | |
| TASK-032 | Replace legacy fuzzy-rename behavior with policy-configured tree matching thresholds and evidence. Exact/semantic matches take precedence; ambiguous fuzzy matches remain typed conflicts. | | |
| TASK-033 | Build a parity matrix mapping every address-patch strategy test to a tree-native test and one of `equivalent`, `strictly safer`, or `superseded by N-way semantics`. No row may remain untested. | | |
| TASK-034 | Run focused real GUI cases from the committed corpus. Verify terminal output, parse validity, stable widget inventory, no duplicate semantic IDs, source-preserving layout synthesis, and deterministic output. | | |

Completion criteria:

- All GUI policy variants execute through `TreeMergeKernel`.
- Every legacy strategy has a tree-native behavioral specification.
- GUI remains included in the final corpus denominator.

### Implementation Phase 6: Add one semantic merge cache

- **GOAL-006**: Cache stable semantic artifacts without recreating per-stage patch caches.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-035 | Add `cache/semantic_merge_cache.rs` using existing `CacheLayerOps`, atomic writes, content addressing, and bounded eviction. Use one cache layer with typed artifact tags for normalized trees, matchings, deltas, and N-way outcomes. | | |
| TASK-036 | Define a stable `MergePolicyFingerprint` that includes content-family identity, owned scalar/block/list/boolean/GUI rules, parser analysis version, adapter schema, matcher configuration, and game/profile identity. Do not hash Rust `Debug` output. | | |
| TASK-037 | Use semantic cache schema `0.1.0` and directory `semantic-merge/v0.1.0`. Implement exact-version reads initially and automatic cleanup of obsolete semantic-cache generations without touching sibling layers. | | |
| TASK-038 | Key normalized trees by source bytes plus adapter/policy identity; key matchings by normalized tree hashes plus matcher identity; key deltas by base/revision tree hashes; key outcomes by base plus ordered source identities, policy fingerprint, and resolution map hash. | | |
| TASK-039 | Record normalization, matching, delta, policy, and cache hit/miss timings in merge evidence/reporting. A cache hit must return the same outcome bytes and evidence as a cold computation. | | |
| TASK-040 | Delete semantic use of `ModDiffCache` and `DagBaseCache`. Leave them temporarily owned only by the address-patch reference crate. | Partial: semantic execution no longer opens either cache; physical ownership moves with TASK-042 | 2026-07-30 |
| TASK-041 | Add corruption, stale-policy, reordered-source, schema-upgrade, concurrent single-flight, warm-hit, and deterministic-round-trip cache tests. | | |

Completion criteria:

- One semantic cache root serves all semantic merge stages.
- Warm and cold results are byte-identical.
- A warm focused probe avoids normalization and matching work and reports the avoided timing.

### Implementation Phase 7: Isolate and retire the address-patch implementation

- **GOAL-007**: Make the old implementation an external research reference and remove it from production dependency graphs.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-042 | Create `crates/foch-merge-reference` and move `patch_engine`, its DAG protocol, `ModDiffCache`, `DagBaseCache`, and patch-only tests into it. The crate may depend on public neutral planning/input APIs from `foch-engine`; `foch-engine` must not depend on it. | | |
| TASK-043 | Remove `MergeKernelMode::Legacy`, `MergeEvaluationKernel::AddressPatchReference`, and `run_merge_for_evaluation` from `foch-engine`. Make `foch-merge-quality` call `foch-engine` for semantic output and `foch-merge-reference` for historical baseline output. | Partial: planning-level kernel dispatch and `DagMergeEvaluation` are deleted; engine/materialization evaluation mode remains until TASK-042 provides the external reference entrypoint | 2026-07-30 |
| TASK-044 | Move historical `legacy`/`structured` artifact naming into merge-quality schema code. Bump the scorer schema and write explicit `semantic_tree` and `address_patch_reference` names for new measurements without rewriting old JSONL rows. | | |
| TASK-045 | Split neutral graph and DAG-walker code from patch-only `planning/dag.rs` and `planning/dag_merge.rs`; delete unused `BaseResolver` and patch apply helpers from `foch-engine`. | Partial: `dag.rs`, `dag_input.rs`, `dag_pipeline.rs`, `definition_trace.rs`, and the production portion of `dag_merge.rs` are neutral/semantic-only; `BaseResolver` and the planning evaluation facade are deleted; reference protocol/result/materialization moved under reference modules; final crate extraction remains | 2026-07-30 |
| TASK-046 | Enforce dependency boundaries with Cargo metadata tests: CLI and engine cannot reach `foch-merge-reference`; only merge-quality and reference-specific tests can. | | |
| TASK-047 | Run focused parity, strict workspace gates, the complete six-case/full-file fixture, and then one fresh full corpus measurement including GUI. Preserve every previously accepted semantic result and adjudicate every changed result before updating published quality numbers. | | |
| TASK-048 | Delete `foch-merge-reference` after the parity matrix is complete, archived reference outputs are sufficient for reproducible scoring, and no active experiment requires executing the old engine. Record the deletion decision in ADR and Notion. | | |

Completion criteria:

- Production Cargo dependency graphs contain no address-patch crate or type.
- `foch-engine` contains no patch engine, patch cache, patch conflict model, or patch observer.
- The full corpus has terminal outcomes for every selected unit, including GUI.
- Legacy execution can be deleted without losing baseline reproducibility.

## 3. Alternatives

- **ALT-001**: Keep both engines inside `foch-engine` behind a mode enum. Rejected because shared result, conflict, cache, and reporting types would continue to bias production architecture toward address patches.
- **ALT-002**: Convert tree conflicts and provenance to synthetic patches only at output time. Rejected because the synthetic representation loses ambiguous-node identity, moves, tombstones, N-way provenance, and exact source selection.
- **ALT-003**: Port each legacy resolver directly into the current sequential three-way fold. Rejected because GUI synthesis and policy parity would then need to be rewritten after N-way state is introduced.
- **ALT-004**: Add separate normalization, matching, delta, and join cache directories. Rejected in favor of one typed content-addressed semantic cache with one semver lifecycle.

## 4. Dependencies

- **DEP-001**: The existing `foch-merge-kernel` normalized tree, GumTree-derived matching, class mapping, PCS, structural conflicts, and provenance primitives.
- **DEP-002**: `ContentFamily`, `MergePolicies`, EU4 profile rules, Clausewitz AST parser, and adapters in `foch-language`.
- **DEP-003**: Kernel-neutral DAG topology and pipeline protocols in `foch-engine/src/merge/planning/`.
- **DEP-004**: Existing cache layer utilities for atomic persistence, generation cleanup, size accounting, and eviction.
- **DEP-005**: Committed merge-quality fixtures, archived address-patch outputs, review packs, and full local corpus.

## 5. Files

- **FILE-001**: `crates/foch-merge-kernel/src/{delta,decision,selection,nway}.rs` - new parser-independent IR and N-way algorithms.
- **FILE-002**: `crates/foch-merge-kernel/src/{class_mapping,conflict,matching,merge,policy,provenance,tree}.rs` - generalized identities, contexts, outcomes, and evidence.
- **FILE-003**: `crates/foch-language/src/analyzer/{content_family,eu4_profile}.rs` - patch-neutral policy vocabulary and GUI/domain configuration.
- **FILE-004**: `crates/foch-engine/src/merge/model.rs` - semantic computation, conflict, source delta, and output directive contracts.
- **FILE-005**: `crates/foch-engine/src/merge/structured/{tree_kernel,policy,ast_adapter,merge,control_flow}.rs` - adapter integration, exact selections, and tree policies.
- **FILE-006**: `crates/foch-engine/src/merge/planning/{dag,dag_merge,dag_pipeline,dag_join}.rs` - neutral DAG state and removal of legacy execution.
- **FILE-007**: `crates/foch-engine/src/merge/resolution/{conflict_view,conflict_handler,handler_registry}.rs` - patch-neutral manual resolution.
- **FILE-008**: `crates/foch-engine/src/merge/output/materialize/{structural,stale_detect}.rs` and `merge/output/stale_vanilla.rs` - tree-native publication and observers.
- **FILE-009**: `crates/foch-engine/src/cache/semantic_merge_cache.rs` and `cache/mod.rs` - unified semantic cache.
- **FILE-010**: `crates/foch-core/src/model/merge.rs` - versioned report, conflict, trace, and stale-change schemas.
- **FILE-011**: `crates/foch-merge-reference/` - temporary isolated historical address-patch implementation.
- **FILE-012**: `crates/foch-merge-quality/` - dual-run evaluation orchestration and parity matrix.
- **FILE-013**: `docs/adr/0002-unify-tree-merge-kernel.md`, a follow-up ADR, and `docs/project-status.md` - accepted boundaries and verified checkpoints.

## 6. Testing

- **TEST-001**: Kernel delta and identity tests for every operation and deterministic serialization.
- **TEST-002**: N-way structural tests for multiple siblings, dependency diamonds, moves, renames, deletions, tombstones, and PCS cycles.
- **TEST-003**: Exact source-selection tests for repeated assignments, items, control-flow regions, moves, and deletions.
- **TEST-004**: Tree-native observer tests for stale vanilla, dependency misuse, provenance, merge trace, and no-op detection.
- **TEST-005**: GUI policy tests for coordinates, bounds, references, named widgets, nested/reparented widgets, ordering, suffix rename, overlay, and scroll-stack synthesis.
- **TEST-006**: Semantic cache tests for identity, corruption, invalidation, semver cleanup, concurrency, and warm/cold parity.
- **TEST-007**: Architecture tests for forbidden imports and Cargo dependency direction.
- **TEST-008**: Legacy-to-tree parity matrix generated from existing address-patch strategy tests.
- **TEST-009**: Focused real file/event/definition-module/GUI probes after their owning phase.
- **TEST-010**: Final strict gates: `cargo fmt --all --check`, strict workspace Clippy, `cargo test --workspace`, merge-quality focused tests, full six-case fixture, and one full corpus run.

## 7. Risks & Assumptions

- **RISK-001**: A merge-local class ID can become unstable if matching order changes. Bind conflict IDs to the complete merge-input hash and deterministic class construction, and test replay across serialization.
- **RISK-002**: Tombstones can incorrectly suppress independent insertions if identity is too broad. Require source-node and equivalence-class evidence for every delete.
- **RISK-003**: N-way matching can become quadratic on large modules. Reuse base-revision matchings, bound cross-revision recovery, expose timings, and cache stable artifacts.
- **RISK-004**: GUI `ScrollStack` can be lossless syntactically but unusable visually. Validate emitted widget inventory and layout invariants, then inspect real GUI cases before accepting the policy.
- **RISK-005**: Porting tests mechanically can preserve old address errors. Each parity row must state the semantic invariant, not merely the old output bytes.
- **RISK-006**: Cache entries can become semantically stale when profile rules change. Include explicit policy, adapter, parser, matcher, and schema identities in every key.
- **ASSUMPTION-001**: A non-empty, version-bound vanilla base remains mandatory for multi-source semantic joins unless an explicit no-base evaluation mode is selected.
- **ASSUMPTION-002**: Historical reference outputs plus immutable corpus inputs are sufficient to retire executable address-patch code after the final parity checkpoint.

## 8. Related Specifications / Further Reading

- `docs/adr/0002-unify-tree-merge-kernel.md`
- `docs/merge-design.md`
- `docs/merge-quality-dataset.md`
- `docs/superpowers/specs/2026-06-28-merge-provenance-design.md`
- `docs/superpowers/specs/2026-06-28-merge-trace-design.md`
- `crates/foch-merge-kernel/NOTICE.md`
