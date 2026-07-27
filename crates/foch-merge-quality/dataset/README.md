# foch merge-quality dataset

Tracked JSONL files contain append-only metadata. The content-addressed
`objects/`, transient `.work/`, and lock directories are intentionally ignored.
Corpus shadow runs remain external by default; an explicit `--record` appends
their normalized per-unit evidence to `shadow_measurements.jsonl`.
Frozen Wiki archives and their derived BM25 caches also stay under ignored
`.work/knowledge/`; only acquisition code, hashes, attribution policy, and
accepted research records belong in Git.

Schema and operating instructions: [`../../../docs/merge-quality-dataset.md`](../../../docs/merge-quality-dataset.md).

The fixed 36-Legacy / 13-Structured / six-case review-pack contract is
documented in
[`../../../docs/merge-quality-review-pack.md`](../../../docs/merge-quality-review-pack.md).
Generated packs belong under ignored `.work/review-packs/`.
