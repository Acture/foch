# Structured Merge Shadow Slice

Status: experimental and explicit opt-in. Production `foch merge` still
defaults to Legacy, but selected event files and merge-ready definition modules
can use the Structured final join.

## Scope

The structured vertical slice connects a parser-independent merge kernel to the
real dependency-DAG final join. It accepts only narrow, auditable shapes:

- the content family is ordinary `events`, or it declares a merge-ready
  `DefinitionModule` with `AssignmentKey` identity
- the exact path has exactly two final DAG sinks
- a non-empty real vanilla file, not a synthetic empty base, is available as
  the three-way ancestor
- neither sink nor the shared ancestor carries unresolved legacy conflicts or
  intent-only patches

Any other shape returns an explicit `structured merge unsupported` error.
Intermediate parent joins and a multi-node shared frontier continue to use the
legacy engine; the structured kernel is invoked only at the final two-sink
join. There is no silent fallback after Structured has been selected.

## Architecture

`crates/foch-merge-kernel` owns parser-independent normalized trees,
provenance, matching, revision classes, PCS ordering constraints, and typed
structural conflicts. `foch-engine` owns the Clausewitz AST adapter and
content-family policy:

- `country_event` and `province_event` are anchored by their inner `id`
- `option` blocks are anchored by their inner `name`
- `if`/`else_if`/`else` chains are normalized as guarded control-flow cases
  when their semantics are provable; otherwise they remain opaque and produce
  review findings
- assignment kinds include their key, so unrelated keys cannot recovery-match
- comments are separated as trivia instead of entering positional content
  matching, and scalar variants survive AST round trips
- assignment-key modules merge complete runtime-effective definition views and
  retain inactive definitions in deterministic output

Exact subtree hashes are verified structurally before they become `Exact`
matches. Changed compatible roots remain linked but carry recovery evidence.
Left/right matching is seeded through the common base, and a conflicted merge
exposes only a tentative tree; callers must obtain a conflict-free resolved
tree before materialization. Delete-versus-move, reparent, or ordered-reorder
cases produce an explicit `DeleteModify` conflict and keep the surviving node
in the tentative tree instead of silently treating it as deleted.

Selected matching and amalgamation logic is adapted from Mergiraf 0.18.0 at
revision `e8e13887b85b8cb56b1dc1624c5f94e3d39182b6`. Attribution and the upstream
GPL-3.0 text live under `crates/foch-merge-kernel/`.

## Shadow Compare

`foch-mq shadow-compare` runs Legacy and Structured in separate child
processes and separate output directories. The modset, diff, and DAG-base
cache identities include the selected kernel.

```fish
set EU4_ROOT "$HOME/Library/Application Support/Steam/steamapps/common/Europa Universalis IV"
cargo run -p foch-merge-quality --bin foch-mq -- \
	--game-root "$EU4_ROOT" \
	shadow-compare \
	--playset /path/to/dlc_load.json \
	--out-dir /tmp/foch-shadow \
	--retained-path events/Example.txt
```

The command writes:

- `legacy/` and `structured/`: isolated merged outputs
- `shadow-inputs.json`: the immutable schema-v2 comparison manifest
- `shadow-compare.json`: paired run status, timing, conflict/file counts, and
  hashes for differing user-output files

Both arm records carry the same `comparison_id`. The identity binds the playset,
launcher descriptors, resolved mod-tree contents, retained base-game files,
verified installed base snapshot, effective `foch.toml`, external `use_file`
resolution contents, executable bytes, `force`, and retained paths. Each child
verifies that manifest before and after its run. Preflight clears both old arm
directories before any fallible capture step. A failed, fatal, non-kernel, or
input-drifted arm clears its output and sets `outputs_compared=false`, so stale
files can never become a reported delta. Warnings, unsupported-scope errors,
conflict reasons, and handler resolutions remain structured diagnostics in the
report. `.foch/*` reports are excluded from file hashes so engine metadata does
not masquerade as mod content.

Each arm has a bounded timeout. A timeout, crash, malformed child response, or
comparison-identity mismatch becomes a terminal arm record and clears partial
output instead of aborting before the paired report can be written.

## Corpus Shadow

`shadow-case` restores source mods from the immutable dataset object store,
preserves snapshot mod order, creates an isolated playset, and scores both arms
against the human compatch with the existing base-aware scorer:

```fish
set EU4_ROOT "$HOME/Library/Application Support/Steam/steamapps/common/Europa Universalis IV"
cargo run -p foch-merge-quality --bin foch-mq -- \
	--dataset-root crates/foch-merge-quality/dataset \
	--game-root "$EU4_ROOT" \
	shadow-case \
	--id 3635635014 \
	--retained-path events/Elections.txt \
	--out-dir /tmp/foch-shadow-elections
```

`shadow-corpus` derives the deterministic scorable multi-source denominator,
validates the committed per-file Legacy baseline against scorer expectations,
and runs isolated comparisons only for explicit `--candidate` units. Unselected
units are recorded as `legacy_retained`; this is a rollout disposition, not an
in-engine fallback. Use the expected-unit assertion so corpus drift fails
instead of silently changing the denominator:

```fish
cargo run -p foch-merge-quality --bin foch-mq -- \
	--dataset-root crates/foch-merge-quality/dataset \
	--game-root "$EU4_ROOT" \
	shadow-corpus \
	--out-dir /tmp/foch-shadow-corpus \
	--candidate 3635635014:events/Elections.txt \
	--expect-multi-source-units 36
```

`tests/fixtures/legacy-baseline.json` must cover the exact target set, reproduce
`tests/fixtures/expected.json`, and agree exactly with each selected candidate's
live Legacy arm. Corpus reports embed every Legacy file score and
the complete paired evidence only for evaluated candidates. Event candidates
additionally check parseability,
namespace/event/option preservation, duplicate anchors, orphan control-flow
branches, and ordered control-flow shape. Stable target identities use snapshot
and content identities rather than absolute paths. Complete unit evidence is
resumed only when its paired report, complete output-tree hash, current retained
base files, effective `foch.toml`, and external resolution inputs still validate.

The output root contains `shadow-targets.json`, one directory per evaluated
candidate, `shadow-corpus.json`, and `report.md`. Results remain outside Git by
default. Passing `--record` explicitly appends evaluated candidate records to
`dataset/shadow_measurements.jsonl`; it does not change `expected.json`.

Report schema `2.0.0` separates `candidate_evaluated` from `legacy_retained` and
records the non-GUI denominator separately from the full denominator. Only
`.gui` layout files are excluded; `.gfx` definition files remain in the
order-insensitive non-GUI denominator. Legacy accepted, projected accepted,
Legacy-accepted units lost, candidate outcomes, and candidate runtime are all
recomputable from the 36 report rows.

## Current Gate

The latest 2026-07-22 gate evaluates all 12 fixed `common/**` candidates plus
GE-EE `events/Elections.txt` against the complete 36-unit denominator. It
projects strict and adjudicated acceptance from 7/36 to 12/36, and non-GUI
acceptance from 7/21 to 12/21, with zero Legacy-accepted units lost. Candidate
outcomes are 5 improved, 0 regressed, 1 unchanged accepted, 4 review, 2
structured conflict, and 1 safety failure. Aggregate candidate runtime is
0.960x Legacy.

The five improvements are four scripted-trigger modules and institutions;
religions remains accepted. Buildings and scripted effects withhold output on
explicit conflicts. Elections currently fails the control-flow shape safety
gate, so the event result below is retained as historical evidence rather than
a current rollout claim.

### Historical Elections gate

The GE-EE `events/Elections.txt` gate and complete 36-unit rollout projection
passed on 2026-07-21. The fixed scorer `1.2.0` baseline was reproduced first;
its content identity is
`faf47fd536026b14bae1c1fbd374cf937725ba7026930b0915c1cf8dfef73d38`.
The projection evaluated only Elections and retained Legacy for the other 35
units. It moves both the full and non-GUI accepted counts from 7 to 8 with zero
Legacy-accepted units lost, one improvement, and no regression, review, safety,
unsupported, conflict, or failed candidate outcome.

Structured matches all 1,217 Elections human semantic atoms with zero one-sided
atoms, parses without diagnostics, has no duplicate event IDs, duplicate option
IDs, or orphan control-flow paths, and matches the human control-flow multiset.
Structured took 54,270 ms versus Legacy's 51,229 ms (1.059x). The auditable
report is `/private/tmp/foch-shadow-corpus-elections-projection-v2/` with
comparison ID
`268be78b7d8b1c8fb3fb1ade298ffbb5147753b9ab7ab3590640ca5b634f826e`.

This proves the selected Elections rollout, not the entire event family.
Structured events remain globally disabled; other event units, including
`PriceChanges.txt`, stay explicitly `legacy_retained` until selected and gated
separately.
