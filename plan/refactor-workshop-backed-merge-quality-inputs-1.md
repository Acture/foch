---
goal: Replace merge-quality input CAS snapshots with read-only Steam Workshop inputs
version: 1.0
date_created: 2026-08-08
last_updated: 2026-08-08
owner: foch
status: Implementation Complete - Manual Acceptance Pending
tags: [architecture, merge-quality, workshop, steam, cas, testing]
---

# Workshop-backed merge-quality inputs

This plan supersedes the input-CAS assumptions in
`refactor-foch-mq-package-integration-1.md`. The merge-quality package remains a
private library used by integration tests; no standalone binary is added.

## Locked decisions

- Steam Workshop directories and `appworkshop_<appid>.acf` are read-only.
- Product acceptance uses a fixed 14-case logical corpus. The nine cases whose
  inputs are absent from `WorkshopItemsInstalled` or have `manifest = -1` are
  excluded deliberately, never dynamically skipped.
- ACF manifest IDs and a mandatory Steam build ID establish installation
  versions. The case input version covers compatch and ordered source ACF
  identities; the product-manifest digest canonicalizes the ordered source
  identities supplied to `foch`. Neither is a digest of Workshop file bytes.
  Local edits under an unchanged ACF manifest deliberately do not create
  another input version.
- Required scripts are read directly from Workshop. A cold parse derives each
  raw script identity from the same bytes supplied to the parser; no separate
  whole-tree preflight or digest cache is allowed.
- A persistent semantic snapshot is looked up from ACF identity before any
  Workshop traversal. A cold miss enumerates only game-loadable roots and
  persists the complete loadable-file inventory with the semantic index.
  Retained-path views are transient and cannot replace that complete snapshot.
- Descriptor parsing happens only after semantic-cache lookup and computes no
  separate content digest. The scorer does not enumerate the compatch until a
  fresh product merge has returned non-fatal.
- New measurements retain a compact scorer evidence bundle, not full input or
  merged-output trees.
- Historical V1 JSONL and output evidence stay byte-identical. Once input-only
  CAS objects are retired, raw V1 input replay is no longer supported. Current
  code already has no object-store, tree-packing, or snapshot-building path;
  the physical `objects/` cleanup is a later user-operated migration.
- This change does not alter full-product merge materialization semantics.

The fixed 14-case Workshop test is the only product acceptance. Acquisition,
corpus/fixture refresh, fixture acceptance, review-pack, and semantic/full
payload export are retired. `export.fish` is metadata-only; Common and
structured-rollout scripts are auxiliary analysis rather than alternate
product gates.

## Requirements

- **REQ-001**: Pair every Workshop content root with the ACF in the same Steam
  library; duplicate item IDs across libraries are errors unless explicitly
  disambiguated.
- **REQ-002**: Treat missing records, invalid manifests, `manifest = -1`, bad
  ACF, or missing item directories as prerequisites failures.
- **REQ-003**: Store Steam 64-bit IDs as canonical decimal strings.
- **REQ-004**: Input identity covers game/build, ordered case topology, and ACF
  manifest IDs only. Paths, descriptors, file metadata, and file digests are
  excluded from installation-version identity.
- **REQ-005**: Compare relevant ACF entries before and after each product run;
  drift produces no terminal measurement or evidence row.
- **REQ-006**: Product acceptance must not open, verify, restore, copy, or
  archive any input-CAS object.
- **REQ-007**: Evidence bundles contain only reports, identities, scorer
  configuration, checksums, and the explicit immutable per-unit closure visible
  to scoring.
- **REQ-008**: Existing V1 streams are append-only historical evidence and must
  not be rewritten during schema migration.
- **REQ-009**: Ordinary CI uses hermetic Workshop/ACF fixtures. The real
  14-case Steam run remains an exact ignored test handed to the user.
- **REQ-010**: Recursive Workshop hashing is allowed only in a separately
  invoked integrity audit with explicit operator-visible cost. It must never be
  required by normal merge, cache reuse, acceptance, or reporting.
- **REQ-011**: A warm semantic-snapshot hit must occur before any Workshop-root
  walk. Cache-key construction may read the ACF and validate the item root but
  must not enumerate that root. Descriptor reads follow semantic-cache lookup,
  and compatch scoring discovery follows the product merge.
- **REQ-012**: Cold inventory traversal must prune non-loadable top-level roots.
  Scoring and evidence capture admit only explicit game-loadable text document
  families and must reject binary assets.

## Tasks

| Task | Description | Status |
|---|---|---|
| TASK-001 | Record historical stream checksums and suspend the legacy CAS-backed acceptance path. | Complete |
| TASK-002 | Add typed, read-only ACF parsing and paired multi-library Workshop discovery in `foch-core`. | Complete |
| TASK-003 | Add a shared ACF-only product manifest and merge-report attestation in `foch-engine`. | Complete |
| TASK-004 | Add V2 input-version, observation, measurement, and typed evidence records without rewriting V1. | Complete |
| TASK-005 | Replace lifecycle input-CAS restoration with a live Workshop resolver and stable ACF checks. | Complete |
| TASK-006 | Migrate Common/structured consumers and retire review, input archive/export, and acquisition paths. | Complete |
| TASK-007 | Commit the fixed 14-case topology and add hermetic/read-only/no-CAS regression tests. | Complete |
| TASK-008 | Run formatting, strict clippy, workspace tests, and a static current-workflow CAS reference audit. | Complete |
| TASK-009 | Run the release 14-case acceptance manually, then update status and Notion. | Pending |
| TASK-010 | Delete the content-digest product-input contract and `input-digests-v1`; derive raw Clausewitz identities only from bytes already read for parsing. | Complete |
| TASK-011 | Make ACF-keyed semantic-cache lookup precede Workshop traversal; persist complete loadable inventories and keep retained-path snapshots transient. | Complete |
| TASK-012 | Restrict reference scoring and evidence capture to game-loadable text document families. | Complete |
| TASK-013 | Re-run focused regressions, full Rust formatting/check/test/clippy gates, and the no-CAS/no-full-tree static audit without starting a real Workshop case. | Complete |

## Historical baseline

The following SHA-256 values were recorded before implementation:

- `snapshots.jsonl`: `6d5ad4c4d8db51d2b00286e9e7894edd473089b257fe2745733953a28f53fd8c`
- `observations.jsonl`: `64d0e7c0829a9598cc5a913a7ab808165f09086565180de58f17bb416e51fa8a`
- `measurements.jsonl`: `a7e2150a61ac191e9558981d22e0b5d5c15a8a27d101bfb642223bd911f956e9`
- `object_records.jsonl`: `8ed93792f20943ef41afab4add817320f2f823f5f7736dfd974787bedc0132e8`
- `file_results.jsonl`: `f9847d08f62524752f524ebbdba4af536a870eda21e947832753d336f4f77d30`

Excluded case IDs: `2095475587`, `788906833`, `1884376477`,
`2804377099`, `253263609`, `1088606279`, `1449952810`, `2469419235`,
and `276456014`.

## Acceptance

The final manual command is:

```fish
scripts/merge-quality/acceptance.fish
```

Acceptance requires 14 unique successful terminal measurements, zero failures,
stable ACF attestations, no Workshop writes, no legacy input-CAS reads,
and one valid compact evidence bundle per measurement.
