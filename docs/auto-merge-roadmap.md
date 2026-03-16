# Auto-Merge Roadmap

Last updated: 2026-03-16

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

### Milestone 5: Revalidation and contributor workflow

Goal:

- make generated output trustworthy enough for repeated use

Deliverables:

- automatic post-merge validation
- dry-run and review-friendly reports
- regression tests covering supported merge classes

Why this matters:

- the static-analysis approach only pays off if the generated output is checked using the same semantic machinery that justified the merge

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
