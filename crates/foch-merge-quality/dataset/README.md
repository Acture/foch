# foch merge-quality dataset

Tracked JSONL files contain append-only metadata. The content-addressed
`objects/`, transient `.work/`, and lock directories are intentionally ignored.
Corpus shadow runs remain external by default; an explicit `--record` appends
their normalized per-unit evidence to `shadow_measurements.jsonl`.

Schema and operating instructions: [`../../../docs/merge-quality-dataset.md`](../../../docs/merge-quality-dataset.md).
