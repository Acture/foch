# Merge trace design

## Goal

`--provenance` should produce a first-class audit trail for each structural-merge definition without changing flag-off bytes. The existing provenance map answers **who** was adopted; the trace adds **how** the merge was decided and exposes that information to graph consumers.

## Flag and artifact

Use the existing `foch merge --provenance` flag. Provenance already opts into non-game metadata and inline provenance comments, so adding `.foch/foch-merge-trace.json` under the same flag avoids another partially-overlapping CLI switch. When the flag is off, `definition_provenance`, `merge_trace`, and the new sidecar are empty/absent so normal merge output and report JSON remain byte-identical.

Add `MERGE_TRACE_ARTIFACT_PATH = ".foch/foch-merge-trace.json"` in `foch-core` next to the report and provenance constants. The trace is also embedded in `MergeReport` behind `skip_serializing_if = "BTreeMap::is_empty"` so cached report consumers can inspect it without reading another file.

## Schema

Deterministic shape, using only `BTreeMap` and `Vec`:

```text
MergeReport.merge_trace / .foch/foch-merge-trace.json:
  path -> definition_key -> MergeTraceEntry

MergeTraceEntry:
  contributors: Vec<MergeTraceContributor>
  policy: MergeTracePolicy
  decision: MergeTraceDecision

MergeTraceContributor:
  mod_id: String
  precedence: usize
  dag_level: usize

MergeTracePolicy: copy_through | overlay | union | boolean_or | named_container | conflict
MergeTraceDecision: adopted | overridden | unioned | conflict
```

`contributors` are the adopted mods from `definition_provenance`, augmented with the already-known DAG precedence and topological DAG level from the file contributor set. `policy` is derived at the structural-merge point from the matched `ContentFamilyDescriptor.merge_policies`: `BlockPatchPolicy::Union` => `union`; `BooleanOr` => `boolean_or`; `LastWriter` => `overlay`; recursive merge with `NamedContainerPolicy` other than `Conflict` => `named_container`; otherwise `conflict` as the honest baseline. The copy-through policy is reserved for future non-structural file-level trace entries; this track records per-definition structural output.

`decision` is derived from adopted contributors plus policy: multiple adopted contributors under `union`, `boolean_or`, or `named_container` means `unioned`; one adopted contributor when more than one mod defined the key means `overridden`; unresolved structural failures can record `conflict`; otherwise `adopted`.

## Pipeline changes

At `patch_structural.rs`, after DAG patch computation and before provenance comments are injected, build a per-file trace from `definition_provenance`, the structural contributors, and `ContentFamilyDescriptor`. Return it in `PatchBasedMergeOutput`. `materialize.rs` accumulates the per-file trace into `MergeReport.merge_trace` only when `--provenance` is on and the file is actually materialized as generated output, mirroring the existing provenance accumulation.

`execute.rs` writes `.foch/foch-merge-trace.json` beside `.foch/foch-provenance.json` when the embedded map is non-empty, and removes stale trace sidecars when the flag is off. Bump `MODSET_CACHE_FORMAT_VERSION` so cached provenance runs cannot hide the new artifact.

## Graph surfacing

Choose the lower-risk graph approach: extend `foch graph --modules` output by adding a default-empty `merge_trace_edges` array to `.foch/module-report.json`. This preserves the private `graph` module boundary and does not add a new graph mode. The graph command reads an existing `.foch/foch-merge-trace.json` from the requested `--out` directory, if present, and appends edges:

```text
merged_definition: "<path>::<key>"
source_mod: "<mod_id>"
policy: <MergeTracePolicy>
decision: <MergeTraceDecision>
precedence: <usize>
dag_level: <usize>
```

Edges are sorted by merged definition, source mod, then precedence, then DAG level for deterministic bytes. If no trace artifact exists, the report remains useful with an empty `merge_trace_edges` list.

## Tests

Unit-test trace derivation in-module: union of two mods gives policy `union`, decision `unioned`, and ordered contributors; overlay/last-writer with one adopted winner but multiple definers gives `overridden`. Add an e2e mirroring `eu4_provenance_*` for `eu4_union_scripted_effect` asserting `.foch/foch-merge-trace.json` contains `test_shared_effect`, both mods, policy `union`, and decision `unioned`. Add a graph/module-report test that injects a trace sidecar, verifies edges are included, and serializes twice to identical bytes.
