# foch merge-quality dataset

Tracked JSONL files are append-only metadata. The V1 prefixes and
`shadow_measurements.jsonl` are frozen historical evidence. New measurements
are V2 only: scorer `2.0.0`, the public `foch merge` artifact,
`semantic_tree`, and `full_product_merge`.

The only product acceptance is the fixed 14-case gate:

```fish
scripts/merge-quality/acceptance.fish
```

Its exact ignored test, `workshop_product_corpus_acceptance`, resolves the
committed logical manifest directly from read-only Steam Workshop content and
same-library `appworkshop_236850.acf`. It never reads legacy `objects/`.
Completed V2 runs retain compact scorer evidence under ignored
`evidence_objects/` rather than full input or output trees.

Ignored `objects/` is inert V1 storage. The code has no current object-store,
tree-packing, or snapshot-building workflow. The directory remains on disk
until the user performs a separate reviewed cleanup; no repository script
deletes it.

The remaining supported scripts are `export.fish` (metadata only),
`symbol-evidence.fish`, `common-module.fish`, and
`structured-rollout.fish`. Acquisition, corpus/fixture refresh, fixture
acceptance, review-pack, and semantic/full payload export are retired.

Current temporary output lives under ignored `.maintenance-work/` and
`.evidence-work/`; legacy `.work/` and the advisory `.lock` file also remain
ignored. Full schema and operating details are in
[`../../../docs/merge-quality-dataset.md`](../../../docs/merge-quality-dataset.md).
