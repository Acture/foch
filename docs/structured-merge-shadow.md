# Structured Merge Shadow Slice

Status: experimental, quality-harness only. Production `foch merge` continues
to use the legacy patch engine.

## Scope

The first structured vertical slice connects a parser-independent merge kernel
to the real dependency-DAG final join. It currently accepts only a narrow,
auditable shape:

- the content family is ordinary `events`
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
structural conflicts. `foch-engine` owns the Clausewitz AST adapter and event
policy:

- `country_event` and `province_event` are anchored by their inner `id`
- `option` blocks are anchored by their inner `name`
- repeated control-flow keys such as `if`, `after`, and `desc` remain
  unanchored and ordered
- assignment kinds include their key, so unrelated keys cannot recovery-match
- comments and scalar variants survive AST round trips

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

`shadow-corpus` derives the deterministic scorable multi-source denominator and
runs one isolated comparison per scoring unit. Use the expected-unit assertion
for acceptance runs so corpus drift fails instead of silently changing the
denominator:

```fish
cargo run -p foch-merge-quality --bin foch-mq -- \
	--dataset-root crates/foch-merge-quality/dataset \
	--game-root "$EU4_ROOT" \
	shadow-corpus \
	--out-dir /tmp/foch-shadow-corpus \
	--expect-multi-source-units 36
```

Corpus reports include per-arm human scores, human/source resolution,
directional semantic-atom differences, explicit unsupported/conflict/failure
outcomes, and a report-only migration projection that keeps Legacy for
unmigrated families. Event units additionally check parseability,
namespace/event/option preservation, duplicate anchors, orphan control-flow
branches, and ordered control-flow shape. Stable target identities use snapshot
and content identities rather than absolute paths. Complete unit evidence is
resumed only when its paired report, complete output-tree hash, current retained
base files, effective `foch.toml`, and external resolution inputs still validate.

The output root contains `shadow-targets.json`, one directory per stable unit,
`shadow-corpus.json`, and `report.md`. Results remain outside Git by default.
Passing `--record` explicitly appends normalized records to
`dataset/shadow_measurements.jsonl`; it does not change `expected.json`.

## Current Gate

The targeted GE-EE `events/Elections.txt` gate has been run. After adding fixed
assignment-value cardinality and wiring existing event policies, Structured
finishes with valid output, no diagnostics, no duplicate anchors, and no orphan
control-flow nodes. Against human it has 1,210 shared atoms, zero
Structured-only atoms, and seven human-only atoms; Legacy has the same shared
count plus two Legacy-only atoms. Structured took 53,524 ms versus Legacy's
54,397 ms.

The result is still `safety_failed`: an `if`/`else` chain is paired differently
from human and the seven missing atoms remain. No complete 36-unit shadow run
has therefore been recorded, the committed scorer expectations and 7/21
non-GUI baseline are unchanged, and Structured events remain disabled.
Promotion still requires at least one real non-GUI corpus improvement, no
regression among the seven accepted units, no event-safety failure or silent
wrong output, and runtime no worse than 2x Legacy on the promoted cohort.
