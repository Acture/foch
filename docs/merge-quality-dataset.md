# Merge-quality dataset

The current merge-quality harness is private test support for the public
`foch` executable. It lives under `apps/foch-cli/tests/merge_quality/`; there is
no production merge-quality crate or binary.

## Product acceptance

The only supported acceptance entrypoint is:

```text
cargo acceptance
```

The repository Cargo alias selects an explicit ignored Rust orchestrator, which
runs two exact ignored integration tests in separate processes:

1. `workshop_product_cache_residency_gate`; and
2. `workshop_product_corpus_acceptance`.

These are long, real-Workshop runs for the maintainer to launch manually.
Agents should use focused fixtures and bounded real-case probes, then hand off
the Cargo command. No fish, Bash, or additional task runner is required. The
orchestrator removes cache-cap overrides from each child, uses stage-specific
authorization, and stops immediately if a stage fails.

The 2026-09-05 entrypoint change was checked with a temporary Cargo fixture that
loaded the actual Rust orchestrator and replaced the expensive stages with empty
tests. It verified successful stage ordering, first-stage short-circuiting,
second-stage failure propagation, removal of inherited cache-cap overrides, and
rejection of a raw invocation without the alias's authorization. This was an
orchestration check; it did not run or record a Workshop cohort.

The denominator is the committed
`apps/foch-cli/tests/merge_quality/fixtures/workshop-product-cases-v2.json`:
14 logical cases and 26 unique Workshop items. The manifest digest and both
counts are tested. Missing local items, unavailable manifest IDs, malformed ACF
data, ambiguous cross-library installs, or input drift fail the prerequisite;
they never shrink the denominator.

No complete current cohort has been accepted. A complete fixed denominator is
required before making a product-quality claim.

## Input identity

For each Steam library, discovery pairs:

- `steamapps/workshop/content/236850`; and
- that same library's `steamapps/workshop/appworkshop_236850.acf`.

Both are read-only. Each case binds:

- ordered Workshop item and manifest IDs;
- EU4 version and mandatory Steam build ID;
- the product's ordered input attestation, including mod ID and precedence;
- the exact `foch` artifact digest;
- runner protocol `foch-cli-committable-merge-report-v6`;
- scorer `2.1.0` and configuration;
- production backend `gumtree-pcs-nway`;
- scope `full_product_merge`; and
- installed base-snapshot identity.

The installation digest covers identity metadata, not every Workshop byte.
Normal acceptance reads required installed files in place. It does not copy or
recursively hash whole mod trees. Steam ACF is deliberately the normal version
authority; a full-tree integrity audit is a separate operator action.

The runner re-reads relevant ACF identities before accepting a terminal result
and compares them with the product-authored input attestation. Drift fails
closed and produces no completed measurement/evidence pair.

## Append-only records

The tracked streams under `apps/foch-cli/tests/merge_quality/data/` are:

| File | Contents |
| --- | --- |
| `input_versions.jsonl` | Logical case, game/build, and ordered Workshop ACF identities |
| `observations.jsonl` | Read-only installed-Workshop observations |
| `measurements.jsonl` | Terminal measurement identity, status, timing, summary, and evidence reference |
| `file_results.jsonl` | Per-file scorer results keyed to one measurement |

Existing bytes are append-only. Never truncate, reorder, normalize, restore,
or rewrite them merely to make a worktree clean. An interrupted run is valid
measurement history but not an accepted baseline.

One finished case appends stable input, observation, file results, and a
terminal measurement. A later invocation reuses only an exact valid match for
the same artifact/protocol/backend/scope/scorer cohort and runs missing cases.
V1 identities cannot satisfy V2 requests.

Terminal statuses are:

- `completed`
- `merge_failed`
- `crashed`
- `timed_out`
- `fatal`

Failed terminal outcomes remain part of the fixed denominator. A report sets
`baseline_complete` only when all selected logical cases have one valid
terminal record; completion alone does not mean every result passed quality
thresholds.

## Evidence storage

Completed V2 measurements retain a compact content-addressed bundle below the
ignored `data/evidence_objects/` directory. A bundle contains only the exact
closure needed to reproduce scoring:

- merge report and product-input manifest;
- scorer configuration and per-file results;
- explicit source, human-reference, base, and generated-output units visible
  to that score; and
- an exact evidence index.

The scorer reads one immutable compact capture, and cache reuse validates that
same capture. A bundle is not a recursive archive of a Workshop mod, the EU4
installation, or the generated mod tree.

Ignored work paths are test-owned implementation details. Their absence never
alters tracked JSONL history, and current product code does not depend on them.

## Concurrency and drift boundary

Acceptance assumes Steam or local files may change normally while a case runs.
It detects declared Workshop-version drift through ACF reloads and detects a
changed lazily loaded script by comparing its raw byte identity with the
identity captured for the semantic snapshot.

It does not claim to detect a local edit that preserves the same Workshop ACF
manifest ID unless that exact file is later reopened and checked. It also does
not scan unrelated files. Strengthening that trust model requires an explicit,
costed integrity audit rather than hidden work in acceptance.

## Cache-residency gate

The orchestrator clears `FOCH_CACHE_MAX_BYTES` from each child environment.
The cache-residency test exercises analysis without commit and verifies that
the fixed workflow fits the normal per-layer cache contract. The cohort test
then passes `--confirm` explicitly and measures committed product output.

The removed full merge-output/modset cache is not part of either path. Parser,
CWT, mod-snapshot, and evaluation caches may accelerate their owning work, but
cache state cannot change the denominator or semantic acceptance criteria.

## Metrics

Current scoring keeps two co-equal views:

- every file in the human reference output; and
- files attributable to at least two referenced source mods.

Exact-path collision is evidence of contribution, not proof of semantic
conflict. Static assignment-key families also compare definitions that move
between sibling filenames at module scope. Human-resolution analysis uses
semantic atoms for parseable Clausewitz files and subtracts base-game atoms
before classifying retained, dropped, or human-only content. GUI order remains
significant; other Clausewitz families use their configured comparison policy.

Product acceptance re-parses and scores the generated mod. It does not launch
EU4 or prove in-game playability; that remains a separate manual check.

## Historical material

Older V1 object stores, review packs, legacy address-patch scores, rollout
reports, and metadata-export workflows are research history. Current harness
code does not use them as input, cannot relabel them as semantic-tree evidence,
and exposes no operator workflow to rebuild or extend them. Historical files
elsewhere in the repository remain evidence, not current architecture or an
alternative product gate.
