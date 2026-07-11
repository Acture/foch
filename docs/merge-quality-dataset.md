# Merge-quality dataset

The merge-quality corpus is an append-only research dataset built from local
EU4 Workshop compatches and their declared source mods. The JSONL metadata is
repository-visible; full payload objects are repository-local and ignored.

## Identity

The dataset schema is semver `1.0.0`.

- Object identity: BLAKE3 over the sorted full tree, including relative path,
  file kind, executable bit, file bytes, and symlink target. `.DS_Store`, `.git`,
  and the object marker are excluded.
- Snapshot identity: EU4 version + Steam build ID + compatch tree hash + ordered
  source-mod tree hashes. Collection time and Workshop metadata are separate
  observations and do not change snapshot identity.
- Measurement identity: snapshot ID + the actual `foch-mq` executable hash +
  scorer semver + scorer-config hash.

The append-only files under `crates/foch-merge-quality/dataset/` are:

| file | contents |
|---|---|
| `object_records.jsonl` | object hash, role, Workshop identity, and tree statistics |
| `snapshots.jsonl` | immutable game/compatch/ordered-source identities |
| `observations.jsonl` | collection time, titles, Workshop timestamps, author/URL/visibility/rights status, subscriptions, and churn |
| `measurements.jsonl` | terminal case outcomes and aggregate scores |
| `file_results.jsonl` | per-file scorer results keyed by measurement |
| `annotations.jsonl` | reserved append-only annotation records |

## Storage

`dataset/objects/<prefix>/<hash>/tree` is a verified content-addressed object
store. On macOS, collection requires APFS `clonefile`; it fails rather than
silently falling back to a physical copy. Source and staged trees are hashed
independently before the object is committed. Escaping or absolute symlinks are
rejected.

Merged output trees are archived through the same object store. Repeated source
mods, compatches, and identical outputs deduplicate by tree hash.

## Full baseline

Build once in release mode, then run the complete locally available corpus:

```fish
cargo build --release -p foch-merge-quality --bin foch-mq
set -x EU4_ROOT "$HOME/Library/Application Support/Steam/steamapps/common/Europa Universalis IV"
target/release/foch-mq baseline --timeout-secs 600
```

`EU4_ROOT` is optional. Resolution precedence is CLI override, environment,
existing foch config, then `steamlocate`; Workshop items are searched across all
Steam libraries. The command is resumable: existing objects and measurements
with the same identities are cache hits.

For separate phases:

```fish
target/release/foch-mq collect
target/release/foch-mq measure --timeout-secs 600
target/release/foch-mq report
```

Every selected case must end as `completed`, `merge_failed`, `crashed`,
`timed_out`, or `fatal`. A report is baseline-complete only when every latest
case snapshot has a terminal measurement. Failures remain in the case
denominator; they are never skipped.

## Metrics

Reports expose two co-equal views:

- all ground-truth files in each human compatch
- files contributed by at least two source mods

Scoring uses every declared source mod. Dropped definitions are computed from
the union of all source keys. Human-resolution analysis uses AST-derived
semantic atoms for parseable Clausewitz files and subtracts base-game atoms
before labeling contributor retention or human-only content. GUI ordering stays
significant; other Clausewitz families use order-insensitive AST comparison.

## Export

Metadata-only export is the default and is suitable for public review:

```fish
target/release/foch-mq export --out /tmp/foch-mq-export
```

`--profile semantic` and `--profile full` add one deterministic `tar.zst` per
input under `objects/` and per merged result under `outputs/`. `export.json` and
`checksums.txt` bind every archive. Those payload exports are for private
research use unless Workshop redistribution rights have been reviewed
separately; unknown rights never enter the default metadata-only profile.

The first full local 23-case run remains a manual acceptance step because it
archives roughly 13.6 GiB of logical Workshop payload and can run for hours.
