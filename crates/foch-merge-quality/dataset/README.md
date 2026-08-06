# foch merge-quality dataset

Tracked JSONL files contain append-only metadata. The content-addressed
`objects/`, transient `.work/`, and the advisory `.lock` file are intentionally ignored.
`shadow_measurements.jsonl` is frozen rollout history; there is no live
dual-kernel command that appends to it.

Schema and operating instructions: [`../../../docs/merge-quality-dataset.md`](../../../docs/merge-quality-dataset.md).

The fixed 36-Legacy / 13-Structured / six-case review-pack contract is
documented in
[`../../../docs/merge-quality-review-pack.md`](../../../docs/merge-quality-review-pack.md).
Generated packs belong under ignored `.work/review-packs/`.

The tracked measurement stream contains two immutable V1 cohorts produced by
the historical `foch-mq` artifact. Scorers `1.0.0` and `1.3.0` both used
`legacy_address_patch_reference`. New measurements are V2 only: scorer `2.0.0`,
the public `foch merge` artifact, `semantic_tree`, and
`full_product_merge`, with product-authored report attestation and an exact
base-game content binding.

Use the fixed entrypoints in `scripts/merge-quality/`: `acceptance.fish`,
`fixture-acceptance.fish`, `review-pack.fish`, `refresh-corpus.fish`,
`export.fish`, `refresh-fixtures.fish`, `symbol-evidence.fish`,
`common-module.fish`, `structured-rollout.fish`, and `acquire.fish`. Each
invokes one exact ignored test.
