# Auto-Merge Roadmap

Last updated: 2026-03-23

## Purpose

This document describes the milestone order for turning `foch` from a static analyzer into an automatic Paradox mod merger. It intentionally stays at the roadmap level. Detailed command and artifact contracts live in [merge-design.md](./merge-design.md).

## Why The Current Work Is Still On-Path

The existing repository already provides prerequisites that a merge engine will need:

- playset resolution
- mod discovery
- script parsing
- semantic indexing
- conflict detection
- structural merge candidate tagging for selected paths

The project is therefore not on the wrong track. It has completed a meaningful prerequisite layer, but it has not yet crossed into generated merge behavior.

## V1 Boundaries

The first merge-capable version should deliberately stay narrow:

- target game: EU4 only
- supported structural merge classes: only roots already covered by current semantic indexing
- unsupported binary or unknown formats: never rewritten; only copied through or flagged as hard conflicts
- UI files: opt-in and limited, not treated as universally safe merge targets

These boundaries keep the first implementation honest and testable.

## Deferred Compatibility Workstream

Localisation compatibility is tracked separately from the merge roadmap.

- It should not be merged into the mainline parser-cleanup track.
- It should not block Milestones 1 through 5.
- It should remain visible as an explicit follow-up workstream for cases such as double-byte / EU4DLL decoding and other localisation-specific compatibility gaps.

The merge roadmap may surface localisation regressions during post-merge validation, but the implementation of localisation compatibility itself belongs to its own workstream.

## Milestone Order

### Milestone 1: Deterministic merge planning

Goal:

- convert current conflict findings into a stable merge planning phase

Deliverables:

- merge classification for every overridden file
- deterministic precedence and conflict rules
- a machine-readable merge plan artifact

Why this comes first:

- it turns current analysis output into a product-facing contract without requiring file generation yet

### Milestone 2: Merge-oriented intermediate representation

Goal:

- represent structurally mergeable content in a form suitable for deterministic rewriting

Deliverables:

- merge-specific IR for supported script roots
- provenance retained at the fragment or node level
- explicit merge policy attached to IR nodes

Why this comes before emission:

- without a merge IR, the project cannot move from "candidate" to "materialized result"

### Milestone 3: Clausewitz emission layer

Goal:

- generate normalized script output from the merge IR

Deliverables:

- stable text emission
- deterministic ordering rules
- preserved provenance through comments or sidecar metadata

Why this is the inflection point:

- this is the first milestone that enables actual merged artifact generation

### Milestone 4: Generated merged mod output

Goal:

- produce a merged mod directory from a playset

Deliverables:

- merged output root
- generated `descriptor.mod`
- merge manifest and validation report
- explicit failure behavior for unresolved hard conflicts

Why this is still not the finish line:

- generation alone is not enough; the generated output must still be revalidated

### Milestone 4 Implementation Slices

The next execution step should be split into isolated work packages so the coordinator can assign them without overlapping ownership:

- Slice A: contract freeze for `merge`/`merge-plan`
- Slice B: merge IR for supported structural roots
- Slice C: materialization and artifact emission
- Slice D: post-merge revalidation and exit-status gating

Recommended dependency order:

- complete Slice A before any worker starts Slice B or Slice C
- let Slice B and Slice C run in parallel only after the shared contract is frozen
- keep Slice D last, because it depends on the emitted tree and metadata files

### Slice A: Contract Freeze

Scope:

- finalize the shared input/output contract for `merge` and `merge-plan`
- lock the plan and report schema fields that downstream code will consume
- lock the output directory layout and the `descriptor.mod` contract
- lock exit-code semantics for success, manual conflict, and validation failure

Success criteria:

- no worker needs to guess the final JSON shape or file layout
- later slices can be implemented without changing the public contract

### Slice B: Merge IR

Scope:

- introduce a merge-specific intermediate representation for supported roots
- preserve provenance at the fragment or node level
- encode precedence and merge policy directly in the IR
- make the IR consumable by later emission code without depending on artifact-writing details

Success criteria:

- supported paths can be converted from plan entries into IR nodes deterministically
- the IR can represent both copy-through and structural-merge cases

### Slice C: Materialization

Scope:

- write `descriptor.mod`
- persist `.foch/foch-merge-plan.json`
- persist `.foch/foch-merge-report.json`
- copy through or overlay non-structural files
- emit generated files for structural-merge paths
- handle conflict placeholders when `--force` is used

Success criteria:

- a merged output tree is produced on disk from the frozen plan and IR
- the written metadata matches the shared contract exactly

### Slice D: Revalidation

Scope:

- parse the generated output tree again
- run semantic checks over the emitted artifacts
- classify fatal errors, warnings, and residual conflicts
- map the validation result back into the documented exit codes

Success criteria:

- the merge command does not report success until the generated tree passes validation
- validation failures are visible in the report, not only in the exit code

### Milestone 5: Revalidation and contributor workflow

Goal:

- make generated output trustworthy enough for repeated use

Deliverables:

- automatic post-merge validation
- dry-run and review-friendly reports
- regression tests covering supported merge classes

Why this matters:

- the static-analysis approach only pays off if the generated output is checked using the same semantic machinery that justified the merge

## Localisation Positioning

Localisation should remain outside the core merge milestone sequence.

- Treat missing or suspect localisation as a separate compatibility concern.
- Preserve localisation diagnostics as validation inputs, not as proof that merge planning or emission is incomplete.
- Revisit localisation once the merge contract is stable and the mainline parser/semantic cleanup work is no longer the active priority.

## Dependency Rules

The milestones above should not be reordered:

- do not implement output generation before merge planning exists
- do not implement broad UI merging before the text script merge path is proven
- do not treat base game support as optional once merge generation is introduced; load order and validation should remain consistent across planning and generation

## Suggested Near-Term Focus

The next practical implementation step should be Milestone 1.

That milestone is small enough to ship incrementally, but important enough to turn the repository from "an analyzer with merge-adjacent hints" into "an analyzer with a defined merge contract."

## Exit Criteria For A First Meaningful Merge Release

Treat the first merge release as complete only when all of the following are true:

- `merge-plan` classifies every file in the playset deterministically
- `merge` generates a merged output tree for supported cases
- unsupported overlaps fail as explicit manual conflicts instead of silent last-writer behavior
- generated output is revalidated automatically
- regression tests cover all supported structural merge roots
