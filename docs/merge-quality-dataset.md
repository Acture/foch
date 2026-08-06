# Merge-quality dataset

The merge-quality corpus is an append-only research dataset built from broad
EU4 Workshop compatibility candidates and the mod items they reference. Steam
child relationships are discovery evidence, not proof that an item is a
compatch or that every child is a merge input. The JSONL metadata is
repository-visible; full payload objects are repository-local and ignored.

## Identity

The dataset manifest and the original measurement wire schema are semver
`1.0.0`. Measurement records are versioned independently so the append-only
stream can retain V1 history while accepting product-bound V2 records.

- Object identity: BLAKE3 over the sorted full tree, including relative path,
  file kind, executable bit, file bytes, and symlink target. `.DS_Store`, `.git`,
  and the object marker are excluded.
- Snapshot identity: EU4 version + Steam build ID + compatch tree hash + ordered
  source-mod tree hashes. Collection time and Workshop metadata are separate
  observations and do not change snapshot identity.
- V1 measurement identity: snapshot ID + the actual historical `foch-mq`
  executable hash + scorer semver + scorer-config hash. Every currently tracked
  V1 cohort used `legacy_address_patch_reference`, even scorer `1.3.0`.
- V2 measurement identity: snapshot ID + the actual `foch` executable artifact
  digest + runner protocol + `semantic_tree` kernel + `full_product_merge`
  scope + scorer semver + scorer-config hash. The scorer configuration also
  binds the installed base snapshot SHA, Steam build, exact timeout, and the
  BLAKE3 digest of a private pinned product-base view. That view contains exactly
  the regular files named by the installed inventory plus detected version
  metadata. Both the product process and scorer receive that same view, never
  the mutable Steam tree; its exact file set and bytes are reverified after each
  case before evidence is persisted. The current product scorer is `2.0.0`; its
  runner invokes public non-interactive `foch merge` and rejects reports whose
  product-authored kernel, scope, or base-snapshot attestation does not match the
  request.
- Historical corpus-shadow target identity: snapshot ID + scoring unit +
  ordered contributors + game version/build + exact base snapshot + evaluator
  artifact + shadow/scorer config. Absolute object-store and output paths were
  excluded. This identity now describes frozen rollout evidence only.

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
| `shadow_measurements.jsonl` | frozen per-unit Legacy/Structured rollout evidence |
| `annotations.jsonl` | append-only, input-bound review proposals and adjudications |

## Storage

`dataset/objects/<prefix>/<hash>/tree` is a verified content-addressed object
store. On macOS, collection requires APFS `clonefile`; it fails rather than
silently falling back to a physical copy. Source and staged trees are hashed
independently before the object is committed. Escaping or absolute symlinks are
rejected.

Merged output trees are archived through the same object store. Repeated source
mods, compatches, and identical outputs deduplicate by tree hash.

Review annotations bind the review-pack identity, snapshot and scoring-unit
identities, selected kernel, and the exact base/source/human/candidate content
hashes. Historical records may retain an opaque optional knowledge-snapshot
binding, but foch no longer acquires, indexes, or serves Wiki content. Proposals remain provisional.
Accepted records must be explicit adjudications; non-identical positive
judgments require family-invariant or runtime evidence, and non-identical GUI
judgments require runtime evidence.

See [`merge-quality-review-pack.md`](./merge-quality-review-pack.md) for the
evidence packaging, verification, and review workflow.

## Historical Legacy baselines

The two tracked 23-case V1 cohorts are immutable historical evidence. Their
artifact identity records the then-current `foch-mq` executable, but both scorer
`1.0.0` and scorer `1.3.0` executed the evaluation-only
`legacy_address_patch_reference` kernel. They do not measure the public product
and there is no supported command for rerunning or extending them.

V2 starts a separate product-bound history. A product measurement must launch
the exact `foch` artifact through the injected runner, parse its
`MergeReport`, score the existing output without selecting another kernel, and
record `semantic_tree` plus `full_product_merge`. V1 cache entries can never
satisfy a V2 request. The fixed acceptance passes the same exact snapshot ID
list through measurement and both reports, so later append-only collection
cannot drift its denominator.

Snapshot IDs are recomputed from every stored payload before selection. V2 file
result IDs bind the complete score and human-resolution payload, while frozen V1
path-only IDs retain their historical validation rule. Resume permits orphaned
file results only for a currently selected V2 identity with no terminal record,
then requires deterministic replay before committing the terminal record.

Every measured case ends as `completed`, `merge_failed`, `crashed`,
`timed_out`, or `fatal`. A parseable non-Fatal product report is completed and
scoreable even when the CLI process exits nonzero; a Fatal report remains
Fatal. Reports select one complete stable cohort ID. A scorer-only selector
fails when it matches more than one cohort, and failed terminal outcomes remain
in the denominator. Resume validates every selected input CAS, the archived
output CAS and object record, payload-bound file-result identities, foreign
keys, and aggregate summary before counting a cache hit. Local prerequisite
failures abort before any immutable terminal record is appended.

## Repository-owned workflows

Use only the fixed Fish entrypoints below. Each maintenance script selects one
exact ignored test and supplies its mutation guard; the scripts contain no
scoring logic.

| goal | entrypoint | exact test |
|---|---|---|
| Run the fixed 23-snapshot product acceptance | `scripts/merge-quality/acceptance.fish` | target `merge_quality_corpus`, test `full_product_corpus_acceptance` |
| Run the isolated six-case product fixture | `scripts/merge-quality/fixture-acceptance.fish` | target `merge_quality_corpus`, test `product_fixture_acceptance` |
| Build and verify the fixed review pack | `scripts/merge-quality/review-pack.fish` | target `merge_quality_corpus`, test `review_pack_acceptance` |
| Refresh immutable snapshots and observations | `scripts/merge-quality/refresh-corpus.fish` | target `maintenance`, test `refresh_corpus` |
| Produce two matching metadata exports | `scripts/merge-quality/export.fish` | target `maintenance`, test `export_dataset_metadata` |
| Stage refreshed corpus/base-game fixture archives | `scripts/merge-quality/refresh-fixtures.fish` | target `maintenance`, test `refresh_fixtures` |
| Generate full-local symbol evidence | `scripts/merge-quality/symbol-evidence.fish` | target `maintenance`, test `symbol_evidence` |
| Run the fixed 12-unit Common gate | `scripts/merge-quality/common-module.fish` | target `maintenance`, test `common_module_acceptance` |
| Compare one complete V2 product cohort with pinned Legacy evidence | `scripts/merge-quality/structured-rollout.fish` | target `maintenance`, test `structured_rollout_acceptance` |
| Acquire Workshop candidates with the `steam` feature | `scripts/merge-quality/acquire.fish` | target `maintenance`, test `acquire_workshop_corpus` |

The acquisition workflow derives one exact typed plan before downloading. Every
newly needed item must be confirmed by SteamCMD; already-present Workshop items
are treated only as local inputs. It writes canonical `manifest.json` and
`checksums.txt`, binds the raw discovered corpus and every selected
`foch-tree-v1` digest, then re-hashes the selected trees before succeeding.
These artifacts establish deterministic local acquisition integrity. They do
not authenticate Steam's remote state or prove that an already-local item is
the latest published version.

The long-running acceptance is manual. It uses the release-built public `foch`
binary, requires the private CAS and installed EU4 base snapshot, asserts the
fixed 23 snapshot IDs, and writes cohort-specific reports only after all 23
terminal records succeed. As of this documentation checkpoint, that first V2
23-case cohort has not been run or accepted.

The six-case fixture and review pack are also manual because both execute real
product merges. They remain isolated from the canonical 23-case denominator and
cannot create or reuse a product cohort accidentally.

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

The supported export workflow is:

```fish
scripts/merge-quality/export.fish
```

It runs `export_dataset_metadata`, writes two independent metadata exports under
`dataset/.work/maintenance/export/`, and requires their `export.json` and
`checksums.txt` bytes to match. The library still supports semantic and full
profiles for repository-owned workflows, but no public Fish entrypoint exposes
payload export. Those payloads remain private unless Workshop redistribution
rights have been reviewed separately.

The two full local 23-candidate V1 runs remain preserved as scorer `1.0.0` and
`1.3.0` history. Both are Legacy AddressPatchReference measurements. Scoring or
execution changes require a fresh cohort rather than relabeling or reusing those
records. Full local product measurement remains a manual acceptance step because
the dataset archives roughly 13.6 GiB of logical Workshop payload and a cold run
can take hours.
