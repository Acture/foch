# Merge-quality dataset

The merge-quality corpus is an append-only research dataset built from broad
EU4 Workshop compatibility candidates and the mod items they reference. Steam
child relationships are discovery evidence, not proof that an item is a
compatch or that every child is a merge input. The JSONL metadata is
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

The candidate corpus also has its own semver schema. Oracle-policy semver is
separate: changing candidate eligibility does not rewrite immutable snapshots
or measurements.

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
Steam libraries. Collection and measurement always cover the full locally
available candidate corpus. The command is resumable: existing objects and
measurements with the same identities are cache hits.

For separate phases:

```fish
target/release/foch-mq collect
target/release/foch-mq measure --timeout-secs 600
target/release/foch-mq report
target/release/foch-mq report --cohort all-candidates
```

The committed six-case fixture is a smaller, network-free regression gate. Its
private base-game archive is required locally. Set an artifact parent to retain
each merged tree and refresh detailed results after every completed case:

```fish
set -x FOCH_MQ_FIXTURE_ARTIFACT_DIR /tmp/foch-mq-fixture-runs
cargo test --release -p foch-merge-quality --test scoring \
  committed_corpus_reproduces_base_aware_baseline -- --ignored --nocapture
```

Each invocation creates a unique child directory containing `merged/<case-id>`,
`results.json`, `report.md`, and `run.json`. The artifacts are written before
the expected-verdict assertion, so an intentional baseline-drift failure still
leaves the complete per-file evidence for adjudication.

The default report is the scorable oracle cohort. `--cohort all-candidates`
keeps broad-search false positives visible for discovery and audit without
mixing them into the quality denominator. The current automatic policy marks a
case `proposed` only when its title states compatibility intent, it references
exactly two mods, and neither referenced mod is newer than the candidate.
Other cases remain collected and are labeled `excluded`; proposed evidence is
not silently upgraded to accepted oracle evidence.

Every measured case must end as `completed`, `merge_failed`, `crashed`,
`timed_out`, or `fatal`. A report is baseline-complete only when every latest
snapshot in the selected report cohort has a terminal measurement for the
current scorer semver. Failures remain in the denominator; they are never
skipped, and measurements from an older scorer are never relabeled as current.

## Metrics

Reports expose two co-equal views over the selected oracle cohort:

- all files in each human reference output
- files attributable to at least two referenced mods

Exact-path collisions count as contributions even when the two files define
different keys. For static `AssignmentKey` content families, definitions that
move between sibling filenames are also attributed and compared at module
scope. VFS path masking still matters: same-path source definitions omitted by
the later reference file are treated as a real human choice. Human-resolution
analysis uses AST-derived semantic atoms for parseable Clausewitz files and
subtracts base-game atoms before labeling contributor retention or human-only
content. GUI ordering stays significant; other Clausewitz families use
order-insensitive AST comparison.

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

The first full local 23-candidate run remains preserved as scorer `1.0.0`
history. Scorer `1.1.0` changes cross-file module attribution and therefore
requires a fresh measurement pass rather than reusing those scores. Full local
measurement remains a manual acceptance step because the dataset archives
roughly 13.6 GiB of logical Workshop payload and a cold run can take hours.
