# Merge-quality dataset

The merge-quality dataset has two deliberately separate layers:

- current V2 product measurements over a fixed 14-case logical corpus resolved
  directly from installed Steam Workshop content; and
- frozen V1 JSONL and object payloads retained as historical evidence.

The current product path never restores or archives complete mod trees.

## Current product acceptance

The only product acceptance workflow is
`scripts/merge-quality/acceptance.fish`. It runs the exact ignored
`workshop_product_corpus_acceptance` test against the committed
`workshop-product-cases-v2.json` manifest.

The denominator is always 14 logical cases containing 26 unique Workshop
items. The nine excluded cases are part of the committed policy, not dynamic
skips. Missing items, `manifest = -1`, malformed ACF data, ambiguous
cross-library installs, or input drift are prerequisite failures.

For every Steam library, discovery pairs
`steamapps/workshop/content/236850` with the same library's
`steamapps/workshop/appworkshop_236850.acf`. Both are read-only. Each case reads
the installed mod directories in place and records:

- the ordered Workshop item and ACF manifest IDs;
- the game version and mandatory Steam build ID;
- an ACF-only product manifest/attestation over the ordered source-mod
  identities; and
- the public `foch` artifact, runner protocol, scorer configuration, kernel,
  scope, and base-game identity.

The manifest digest is a canonical digest of identity metadata, not a digest of
Workshop files. Local byte changes that do not change an ACF manifest ID do not
create a new input version; Steam's ACF is deliberately the version authority.
The product merge reads required files directly from the installed directories.
When Clausewitz scripts are parsed, their raw size and BLAKE3 identity are
derived from the same read used by the parser so later lazy AST loads cannot be
silently mixed with a different semantic snapshot. This is not a full-tree
preflight.

The runner re-reads the relevant ACF entries before publication and rejects a
merge report whose product-authored ACF attestation differs from the prepared
case. Mid-run ACF drift produces no measurement or evidence row.

Run the fixed wrapper from the repository root:

```fish
scripts/merge-quality/acceptance.fish
```

The wrapper runs Cargo under macOS Seatbelt while denying all reads and writes
to the frozen `dataset/objects/` and `dataset/.work/` trees. The ignored test
requires `FOCH_LEGACY_CAS_GUARD=seatbelt-v1` and actively confirms that both
paths return `PermissionDenied` (or `NotFound` after a future cleanup) before
discovering Steam or starting a merge. A raw Cargo invocation lacks the guard
marker and fails closed; manually setting the marker still cannot bypass the
live denial probe while either legacy tree exists.

Steam discovery is automatic. To select one installation explicitly, provide
both members of the read-only pair:

```fish
set -x EU4_ROOT "$HOME/Library/Application Support/Steam/steamapps/common/Europa Universalis IV"
set -x STEAM_WORKSHOP_DIR "$HOME/Library/Application Support/Steam/steamapps/workshop/content/236850"
set -x STEAM_WORKSHOP_ACF "$HOME/Library/Application Support/Steam/steamapps/workshop/appworkshop_236850.acf"
scripts/merge-quality/acceptance.fish
```

The first fixed 14-case Workshop cohort has not yet been run or accepted.

## Records and identity

The tracked files under `crates/foch-merge-quality/dataset/` are append-only
metadata:

| file | contents |
|---|---|
| `object_records.jsonl` | frozen historical object metadata |
| `snapshots.jsonl` | frozen historical V1 game/compatch/source identities |
| `input_versions.jsonl` | V2 game/build and ordered ACF manifest identities |
| `observations.jsonl` | frozen V1 observations plus V2 read-only Workshop observations |
| `measurements.jsonl` | terminal V1 and V2 measurement records |
| `file_results.jsonl` | per-file scorer results keyed by measurement |
| `shadow_measurements.jsonl` | frozen Legacy/Structured rollout evidence |
| `annotations.jsonl` | frozen historical review records |

Historical object identity is BLAKE3 over a sorted full tree. A historical V1
snapshot binds the EU4 version, Steam build, compatch object, and ordered source
objects. V1 measurement identity binds that snapshot to the historical
`foch-mq` artifact and scorer configuration. Both tracked V1 cohorts used
`legacy_address_patch_reference`, including scorer `1.3.0`.

V2 input identity instead binds the logical case, game/build, and ordered
Workshop item and ACF manifest IDs. The product manifest uses profile
`steam-workshop-acf-v1` and contains the ordered source-mod ACF identities with
mod ID and precedence; the compatch ACF identity remains in the case input
version. Absolute paths, ACF timestamps, file paths, file bytes, and observation
time are not identity fields. V2 measurement identity additionally binds the
public `foch` digest, runner protocol, scorer `2.0.0`, scorer configuration,
`semantic_tree`, and `full_product_merge`.

The scorer configuration binds scorer policy and the installed base-snapshot
identity. A newly executed case discovers its scoring units and base-game
scoring closure once, then stores both inside the immutable evidence bundle.
That case-specific closure is evidence, not cohort identity, and is not rebuilt
to decide whether a stored measurement is reusable.

V1 cache entries cannot satisfy V2 requests. A cached V2 result must match the
current ACF input version and pass internal evidence-bundle validation. Cache
reuse does not enumerate or hash the live Workshop tree. Terminal states are
`completed`, `merge_failed`, `crashed`, `timed_out`, and `fatal`; failed
terminal outcomes stay in the fixed denominator. Reports select one complete
cohort and use report schema `3.0.0`.

## Storage boundary

Completed V2 measurements retain compact content-addressed evidence under
`dataset/evidence_objects/`. A bundle contains the merge report, product-input
manifest, scorer configuration, file results, and the explicit source,
compatch, base, and merged-output closure visible to scoring. Scoring runs from
one immutable compact capture, and that same capture is stored with an exact
per-unit evidence index. Cache reuse and standalone reporting validate the
bundle internally, match it to the current ACF identity, and check its file
results and merge-report attestation. They do not reconstruct the closure from
live Workshop or base-game files. The bundle is not a recursive copy of a mod
or generated output tree.

`dataset/objects/` is frozen V1 history. Current code has no `ObjectStore`
workflow, recursive tree packer, or snapshot builder, and no current
acceptance, report, probe, or export opens that directory. Raw V1 replay is no
longer supported.

The physical historical object directory has not yet been cleaned. Separating
input-only objects from retained output evidence and removing the former is a
later, user-operated storage migration. No repository script deletes or
rewrites it.

The V1 JSONL prefixes, `objects/`, `shadow_measurements.jsonl`, review fixtures,
and annotations remain frozen. There is no supported review-pack builder,
verifier, or annotation workflow.

### Concurrency boundary

The operational drift model is Steam or local files changing normally while a
measurement runs. ACF reloads and pre-publication guards fail closed when the
declared Workshop version changes. Required scripts are read in place, and a
lazy script read must match the raw identity captured by the semantic parse that
produced its cached metadata. Foch neither scans unrelated files nor claims to
detect local content edits that preserve the same ACF manifest ID. A recursive
content digest belongs only in an explicit integrity audit, never this
acceptance path. A fresh case does not enumerate the compatch scoring closure
until the product merge has returned non-fatal, so product cache lookup precedes
all Workshop tree traversal performed by the scorer.

## Supported repository workflows

The scripts below are the complete supported merge-quality operator surface.
Each invokes only named exact ignored tests and contains no scoring logic.

| purpose | entrypoint | exact test |
|---|---|---|
| Product acceptance: cache-residency pre-gate, then fixed 14-case Workshop cohort | `scripts/merge-quality/acceptance.fish` | `workshop_product_cache_residency_gate`, then `workshop_product_corpus_acceptance` |
| Deterministic metadata-only export | `scripts/merge-quality/export.fish` | `export_dataset_metadata` |
| Full-local symbol research evidence | `scripts/merge-quality/symbol-evidence.fish` | `symbol_evidence` |
| Fixed 12-unit Common applicability probe | `scripts/merge-quality/common-module.fish` | `common_module_acceptance` |
| Compare a complete V2 cohort with pinned Legacy metadata | `scripts/merge-quality/structured-rollout.fish` | `structured_rollout_acceptance` |

The Common and structured-rollout scripts are auxiliary analysis gates; they
are not alternative product acceptance denominators.

The following workflows are retired and have no script or supported library
entrypoint: Workshop acquisition, corpus refresh, fixture archive refresh,
six-case fixture acceptance, review-pack build/verification, and semantic or
full payload export. Installed Workshop content is acquired and updated by
Steam, outside foch.

### Metadata export

`scripts/merge-quality/export.fish` writes two independent exports under
`dataset/.maintenance-work/export/` and requires byte-identical `export.json`
and `checksums.txt` output. The export contains tracked metadata files only. It
never copies `objects/`, `evidence_objects/`, Workshop content, base-game files,
or generated output payloads. Semantic and full CAS export profiles no longer
exist.

## Historical Legacy baselines

The two tracked 23-case V1 cohorts are immutable research history. Scorers
`1.0.0` and `1.3.0` both executed the evaluation-only
`legacy_address_patch_reference` kernel, not the public product. There is no
supported command for rerunning, extending, or relabeling them.

The scorer `1.3.0` all-candidate view accepted 83/29,481 reference-output files
and 25/217 multi-source files. Its six-case scorable view accepted 11/39 and
11/36 respectively. These figures must not be quoted as `semantic_tree` or
current product quality.

The old review-pack and live dual-kernel surfaces are also retired. Their
fixtures and JSONL streams are frozen evidence only; they do not define a
current gate.

## Metrics

Current reports expose two co-equal views over the selected V2 cohort:

- all files in each human reference output; and
- files attributable to at least two referenced mods.

Exact-path collisions count as contributions even when files define different
keys. Static `AssignmentKey` families also compare definitions that move
between sibling filenames at module scope. VFS path masking remains
significant. Human-resolution analysis uses AST-derived semantic atoms for
parseable Clausewitz files and subtracts base-game atoms before classifying
contributor retention or human-only content. GUI ordering remains significant;
other Clausewitz families use order-insensitive AST comparison.

Historical V1 metrics and frozen rollout projections remain available for
audit, but they are not the current product baseline.
