# Merge Design

Last updated: 2026-03-23

## Summary

This document defines the first implementation-ready design for turning `foch` into a static-analysis-driven Paradox mod merger.

This specification is intentionally narrow:

- target game: EU4 only
- structural merge support: only roots already covered by the current semantic indexing layer
- unsupported binary or unknown assets: copy through when unique, otherwise raise a manual conflict
- generated output is never considered successful until it has been revalidated

## Product Goal

The product goal is to take an ordered playset and produce either:

- a deterministic merge plan
- or a generated merged mod directory plus a validation report

The generated result must preserve load-order semantics where direct copying is sufficient, and use structural merging only where the repository already has enough parser and semantic coverage to do so safely.

## V1 Non-Goals

The first merge-capable version does not attempt to provide:

- universal Paradox game support beyond EU4
- guaranteed safe rewriting of every `.gui`, `.gfx`, or arbitrary text file
- automatic resolution of unsupported overlapping binary assets
- editor-integrated conflict review UX
- semantic equivalence guarantees for paths outside the supported structural classes
- a full localisation compatibility solution

Localisation-specific compatibility remains a separate workstream. The merge engine may report localisation regressions during validation, but v1 merge planning and emission do not depend on solving localisation behavior end to end.

## Target Workflow

The intended workflow is:

1. `foch check <playlist.json>`
2. `foch merge-plan <playlist.json> --format json --output <path>`
3. `foch merge <playlist.json> --out <dir>`
4. automatic post-merge validation run on the generated output

`check` remains the existing diagnostic command.

`merge-plan` becomes the dry-run planning command.

`merge` becomes the artifact-producing command and always runs a validation pass before success is reported.

## Command Surface

### `foch check`

The existing command remains unchanged in v1.

### `foch merge-plan`

Required input:

- `playset_path`

Supported options:

- `--format text|json`
- `--output <path>`
- `--include-game-base`

Behavior:

- resolves the playset and effective load order
- classifies every file in the merged view
- emits a deterministic plan without writing a merged mod tree

Exit codes:

- `0` when the plan is produced successfully
- `1` on system or input errors
- `2` when manual conflicts are present in the computed plan

### `foch merge`

Required input:

- `playset_path`
- `--out <dir>`

Supported options:

- `--include-game-base`
- `--force`

Behavior:

- computes the same plan as `merge-plan`
- aborts on manual conflicts unless `--force` is set
- writes a merged output tree
- writes merge metadata
- runs post-merge validation

Exit codes:

- `0` when generation succeeds and validation reports no fatal errors
- `1` on system or input errors
- `2` when manual conflicts block generation without `--force`
- `3` when generation completes but post-merge validation reports fatal errors

## Merge Strategy Taxonomy

Every effective path in the merged view must be classified into exactly one strategy:

### `copy_through`

Definition:

- the path is provided by exactly one source artifact in the resolved load order

Behavior:

- copy the source file into the generated output without rewriting it

### `last_writer_overlay`

Definition:

- the path is provided by multiple sources, but the path class is not supported for structural merging and is still safe to represent with normal Paradox last-writer semantics

Behavior:

- copy only the highest-precedence source file
- record all overridden contributors in the merge manifest

Allowed for v1:

- unique text or asset classes where direct overlay is faithful to game semantics and the repository is not attempting structural composition

### `structural_merge`

Definition:

- the path is provided by multiple sources and belongs to a supported structural merge class

Behavior:

- parse all contributors
- convert them into merge IR
- merge deterministically by class-specific rules
- emit a generated file instead of copying a source file verbatim

### `manual_conflict`

Definition:

- the path is provided by multiple sources and the repository cannot safely apply either direct overlay or structural merging under v1 rules

Behavior:

- do not silently choose a winner during merge planning
- block `merge` unless `--force` is used
- if `--force` is used, emit a conflict placeholder file only for text formats and omit binary outputs entirely
- always record the conflict in the merge manifest and validation report

## Supported Structural Path Classes In V1

V1 structural merge support is limited to paths already covered by current semantic indexing:

- `events/**`
- `decisions/**`
- `common/scripted_effects/**`
- `common/diplomatic_actions/**`
- `common/triggered_modifiers/**`
- `common/defines/**`

UI-related paths are explicitly limited in v1:

- `interface/**`
- `common/interface/**`
- `gfx/**`

Policy for these UI-related paths:

- they may be classified as structural merge candidates in planning
- they are not rewritten in v1
- overlapping files in these roots become `manual_conflict` unless they can be represented faithfully as `last_writer_overlay`

This keeps the implementation aligned with current parser coverage without overstating safety.

## Deterministic Precedence Rules

All merge behavior must use a single precedence order:

- later enabled mod in the playset overrides earlier enabled mod
- when `--include-game-base` is set, base game content is lower precedence than every mod
- when two contributions originate from the same mod and path, the repository uses the file discovered at that normalized relative path

The merge plan and the generated output must both use this exact order. No command may use a different precedence model.

## Merge Pipeline

### Phase 1: Resolve inputs

- parse the playset
- resolve mod roots
- optionally resolve the base game root
- build the effective ordered contributor list

### Phase 2: Build file inventory

- enumerate all effective relative paths
- normalize path separators and casing rules used by the current analyzer
- collect contributors per path in precedence order

### Phase 3: Classify strategy

For each effective path:

- `copy_through` if there is a single contributor
- `structural_merge` if the normalized path falls in a supported structural class and all contributors parse successfully
- `last_writer_overlay` if the path is a non-structural class that can safely preserve last-writer semantics
- `manual_conflict` otherwise

A parse failure for any contributor in a structural class downgrades that path to `manual_conflict`.

### Phase 4: Materialize output

For each path in the plan:

- copy files for `copy_through`
- copy the winning file for `last_writer_overlay`
- generate files for `structural_merge`
- block or emit conflict placeholders for `manual_conflict`, depending on `--force`

### Phase 5: Write metadata

Always write:

- generated `descriptor.mod`
- `foch-merge-plan.json`
- `foch-merge-report.json`

### Phase 6: Revalidate

Always run a validation pass against the generated output tree:

- parse validation
- semantic validation over supported script roots
- unresolved references and localisation regressions
- residual manual conflicts

The final command status is derived from this validation step.

## Implementation Order For Milestone 4

The first implementation pass should be split into the same four slices used by the roadmap so the coordinator can assign them independently:

- Slice A: contract freeze for plan/report shape and output layout
- Slice B: merge IR for supported structural roots
- Slice C: materialization and artifact emission
- Slice D: post-merge revalidation and exit-status gating

Recommended dependency order:

- complete Slice A before any worker starts Slice B or Slice C
- allow Slice B and Slice C to run in parallel only after the shared contract is frozen
- keep Slice D last, because it depends on the emitted tree and metadata files

## Output Artifact Layout

Given `foch merge playlist.json --out ./merged-mod`, the output directory layout is fixed as:

```text
merged-mod/
  descriptor.mod
  .foch/
    foch-merge-plan.json
    foch-merge-report.json
  <generated game-relative files...>
```

### `descriptor.mod`

The generated descriptor must:

- use a generated display name derived from the playset name plus ` (Merged)`
- omit Steam-specific publishing metadata
- point to the generated output directory only
- record the source playset path in a leading comment when the descriptor format allows comments

### `foch-merge-plan.json`

This file is the persisted version of the computed plan.

Required top-level fields:

- `game`
- `playset_name`
- `generated_at`
- `include_game_base`
- `strategies`
- `paths`

Each `paths` entry must contain:

- `path`
- `strategy`
- `contributors`
- `winner`
- `generated`
- `notes`

### `foch-merge-report.json`

This file records merge execution and validation outcome.

Required top-level fields:

- `status`
- `manual_conflict_count`
- `generated_file_count`
- `copied_file_count`
- `overlay_file_count`
- `validation`

`validation` must contain:

- `fatal_errors`
- `strict_findings`
- `advisory_findings`
- `parse_errors`
- `unresolved_references`
- `missing_localisation`

## Structural Merge Rules

V1 structural merging uses conservative, class-level rules:

### Events and decisions

- merge by top-level object key
- when a key exists in only one contributor, include it unchanged
- when the same top-level key exists in multiple contributors, choose the highest-precedence full definition and record the overridden contributors in metadata
- do not attempt deep field-wise reconciliation inside a shared top-level definition in v1

### Scripted effects, diplomatic actions, and triggered modifiers

- merge by top-level named block
- unique definitions are preserved
- duplicate top-level names resolve to highest precedence
- overridden definitions are recorded in provenance metadata

### Defines

- merge by assignment path when the parser can identify a deterministic assignment key
- when the same key is assigned by multiple contributors, highest precedence wins
- if an assignment cannot be normalized to a stable key, classify the file as `manual_conflict`

These rules make structural merge useful without pretending to solve semantic equivalence across arbitrary nested script.

## Conflict Handling Policy

### Blocking conflicts

The following conditions must produce `manual_conflict`:

- overlapping binary or unknown formats
- overlapping UI-related files in limited v1 roots
- parse failure in a would-be structural merge path
- non-normalizable content in `common/defines/**`
- any path class outside the allowed `last_writer_overlay` and `structural_merge` rules

### `--force` behavior

`--force` only changes materialization behavior. It does not downgrade a conflict internally.

With `--force`:

- generation continues for non-conflicting paths
- every text `manual_conflict` path emits a placeholder file containing contributor paths and a conflict marker
- binary `manual_conflict` paths are omitted from the output tree
- the merge report still records the run as conflict-bearing

`--force` never changes validation severity or hides conflicts from the report.

## Failure And Warning Policy

### Hard-stop failures

Return exit code `1` and stop immediately for:

- unreadable playset input
- malformed playset JSON
- missing configured roots required for the chosen command
- unwritable output directory
- metadata write failure

### Merge-blocking failures

Return exit code `2` for:

- any manual conflict during `merge-plan`
- any manual conflict during `merge` when `--force` is not set

### Post-generation validation failure

Return exit code `3` for:

- generated output with fatal validation errors

Localisation failures that are purely compatibility-related should be reported clearly, but they should not be conflated with the core merge classification or emission contract.

### Warnings only

Keep generation successful while reporting warnings for:

- advisory findings in the validation pass
- overridden contributors under `last_writer_overlay`
- structurally merged files that required precedence resolution but not manual conflict handling

## Acceptance Criteria

The first merge-capable implementation is acceptable only if all of the following are true:

- `merge-plan` and `merge` use the same precedence model
- every effective path is classified into exactly one strategy
- supported structural roots are limited to the v1 set in this document
- generated output always contains `.foch/foch-merge-plan.json`
- generated output always contains `.foch/foch-merge-report.json`
- `merge` always performs a post-generation validation pass
- manual conflicts are never silently converted into last-writer success

## Required Test Scenarios

At minimum, future implementation tests must cover:

- single-contributor file becomes `copy_through`
- unsupported overlapping text file becomes `last_writer_overlay` when allowed
- unsupported overlapping binary file becomes `manual_conflict`
- duplicate event or decision key resolves by precedence and is recorded in metadata
- duplicate scripted effect resolves by precedence and is recorded in metadata
- `common/defines/**` with normalizable assignments merges by key
- `common/defines/**` with non-normalizable content becomes `manual_conflict`
- parse failure inside a structural class becomes `manual_conflict`
- `--include-game-base` keeps base game at lower precedence than every mod
- `merge --force` continues generation while preserving conflict markers
- post-merge validation failure returns exit code `3`
- generated descriptor and metadata files are always present on successful generation
