# Product merge acceptance

This directory is test-only support for the public `foch` executable. It is
compiled only by `tests/merge_quality_corpus.rs`; there is no merge-quality
library or product binary.

The fixed denominator is `fixtures/workshop-product-cases-v2.json`: 14 logical
cases and 26 unique Steam Workshop items. `fixtures/CREDITS.md` records their
provenance. The non-ignored contract test pins both counts and the manifest's
exact digest, so unavailable local Workshop items cannot silently reduce the
cohort.

The files below `data/` are append-only evidence streams:

- `input_versions.jsonl`
- `observations.jsonl`
- `measurements.jsonl`
- `file_results.jsonl`

Never truncate, reorder, normalize, or rewrite their existing bytes. An
interrupted cohort is valid history, but it is not an accepted baseline.
Generated compact evidence objects and work directories are ignored by Git.

The maintainer entrypoint is:

```fish
scripts/merge-quality/acceptance.fish
```

It first runs the cache-residency gate, then the complete fixed cohort. Both are
long, real-Workshop tests and should be launched manually.
