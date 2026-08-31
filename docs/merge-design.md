# Merge Design

This document defines the current product merge contract. Foch is an EU4-aware
N-way semantic merger, not a generic three-way text merger and not a wrapper
around exact-path overwrite order.

## Goals

- preserve compatible contributions from an ordered EU4 mod playset;
- model verified loader semantics at the correct file, definition, or
  definition-module boundary;
- use the analyzed EU4 base snapshot as the semantic ancestor;
- make genuine ambiguity reviewable instead of silently choosing a winner;
- compute the complete result before confirmation; and
- atomically commit the exact reviewed bytes to a separate mod directory.

Localisation and unsupported content families may use narrower strategies. CWT
rules are evidence for shape and editor behavior, not proof of runtime load or
merge semantics.

## Product flow

```text
inspect input
    -> analyze semantic result
    -> review every unit
    -> confirm target/replacement
    -> commit frozen artifacts
```

`foch merge <input> --out <dir>` performs analysis and review without touching
the target. A TTY confirmation or `--confirm` authorizes commit. In
non-interactive use, analysis remains read-only unless `--confirm` is also
present.

There is no separate CLI merge-plan command in this contract. Path
classification is part of merge analysis. There is also no implemented
`MergeSession`; long-lived session design is deferred.

## Inputs and precedence

An input is either the Launcher's `dlc_load.json` plus sibling `.mod`
descriptors, or a `[project]` manifest in `foch.toml`. Resolution produces:

- the concrete EU4 game root and version;
- the ordered enabled mod contributors;
- declared dependencies and reviewed overrides;
- paired Workshop ACF installation identities when available; and
- the installed EU4 base-snapshot identity.

Playset order is semantic. Every path strategy, revision DAG, review
contributor, cache identity, and report must preserve it. Sorting contributors
merely to make a key deterministic is incorrect.

Source mods and the game installation are read-only. Normal analysis reads
installed Workshop content in place and does not copy or recursively hash whole
trees into an input CAS.

## Units and content families

A merge review unit is one of:

- **file** — a single output-relative path; or
- **definition module** — all files that jointly define one loader-level
  module for a concrete EU4 content family.

Exact-path overlap is neither necessary nor sufficient for a semantic conflict.
The EU4 content-family registry in `src/game/eu4/content` decides discovery,
module partition, key policy, ordering, base requirements, and supported merge
behavior.

The analyzed EU4 base snapshot is the ancestor for structural merge. Missing
vanilla input is never treated as an empty ancestor unless the content family
explicitly opts into a verified empty-base policy.

## Analysis strategies

The internal path plan assigns one strategy before materialization:

| Strategy | Contract |
| --- | --- |
| `copy_through` | One effective non-base contribution is copied unchanged. |
| `last_writer_overlay` | Verified loader semantics select the highest-precedence contributor. |
| `structural_merge` | Supported Clausewitz definitions are adapted to the semantic tree and N-way merged against the base ancestor. |
| `localisation_merge` | Keys are unioned; the highest-precedence contributor wins only for the same localisation key. |
| `manual_conflict` | The path cannot be safely analyzed automatically and needs review or withholding. |

Structural analysis builds revision DAGs and definition-module views, adapts
supported EU4 syntax into the semantic-tree kernel, materializes deterministic
Clausewitz bytes into a Rust-owned artifact tree, and re-parses/rechecks that
tree. None of this changes `--out`.

The stable production backend identity is `gumtree-pcs-nway`. The retained
`address-patch` backend is comparative evaluation history and is not the public
product path.

## Review contract

Each planned target resolves exactly once into a `MergeUnitOutcome` with:

- stable normalized ID (`file:<path>` or `module:<family>/<module>`);
- normalized path, content family, and unit kind;
- stable snake-case strategy;
- one disposition;
- concise summary, optional committed output path, and notes; and
- ordered, de-duplicated contributors with display name, precedence, source
  paths, and base-game flag.

Dispositions are:

| Disposition | Meaning |
| --- | --- |
| `safe` | Analysis produced a supported semantic result. |
| `copy` | The effective source can be copied without semantic synthesis. |
| `needs_user_choice` | Supported candidates remain ambiguous and need a reviewed decision. |
| `unsupported_input` | The input shape is outside verified behavior. |
| `engine_failure` | A bounded backend failure/panic was caught for this unit. |
| `deferred` | A configured explicit defer handler intentionally withheld the unit. |

The review summary counts every unit exactly once. Duplicate IDs or output
paths, double resolution, and pending units are invariant failures. If
cross-file pruning removes generated output, the unit remains in review with
`output_path = null`; its semantic disposition is not rewritten.

An implicit interactive/TUI defer remains `needs_user_choice`. `deferred` is
reserved for an explicit configured defer decision. `--force` must not emit an
explicitly deferred unit.

## Resolution policy

Reviewed static decisions live in `foch.toml` `[[resolutions]]`. Exact conflict
IDs and files outrank pattern rules. Built-in handlers are dispatched by the
root merge resolution registry. See
[foch-toml-resolutions.md](./foch-toml-resolutions.md).

Foch never applies a broad last-writer policy merely because it would avoid a
conflict. `--force` is limited to supported `needs_user_choice` fallbacks; it
does not reinterpret unsupported input, engine failure, or explicit defer as
safe.

## Commit contract

Analysis freezes:

- the generated artifact tree and its byte identity;
- the ordered product-input attestation;
- the installed base-snapshot identity;
- any existing output bytes consumed by `keep_existing`; and
- the fingerprint of a non-empty replacement target when the user asks to
  replace it.

`AnalyzedMerge::commit` revalidates those guards and atomically installs the
frozen tree. It does not parse, run a merge backend, resolve a new conflict, or
read a different external resolution file. Drift fails before target mutation.

`--confirm` authorizes the reviewed commit but not replacement of a non-empty
directory. Replacement needs a separate TTY confirmation/fingerprinted
authorization. Batch jobs must use a new or empty target.

## Partial results and statuses

Unsafe units are withheld at file or complete definition-module granularity;
unrelated safe units may still commit.

- `ready` — all required output is safe and validation passed.
- `partial_success` — safe output committed while one or more units were
  withheld or used an explicit supported fallback.
- `blocked` — an explicit non-conflict gate prevents activation/commit.
- `fatal` — input, validation, emission, I/O, cancellation, or invariant failure
  prevents a trustworthy analyzed result.

A partial result is not permission to activate the generated mod blindly. The
report must identify withheld units and activation safety.

## Output tree

A committed result contains only the separate generated mod tree. It never
normalizes or modifies source inputs.

```text
<out>/
  descriptor.mod
  common/...
  events/...
  localisation/...
  .foch/
    foch-merge-plan.json
    foch-merge-report.json
    foch-provenance.json      # only when requested
    foch-merge-trace.json     # only when requested
```

`foch-merge-plan.json` records the internal deterministic target
classification used by analysis; its name is an artifact compatibility
contract, not a separate command. `foch-merge-report.json` records status,
validation, backend/scope/base attestation, ordered product-input identity,
withheld units, and handler outcomes.

Provenance output is opt-in. When disabled, it must not perturb ordinary
emitted bytes.

## Determinism and safety invariants

- identical frozen inputs, policy, base snapshot, and Foch version produce the
  same review units and output bytes;
- paths in artifacts use normalized `/` separators and safe relative paths;
- contributors retain semantic precedence;
- source trees are never destinations;
- output is staged and installed atomically under a same-target lock;
- failure before commit leaves the target unchanged;
- replacement authorization is invalidated by target drift; and
- product acceptance re-parses and scores generated output but does not launch
  EU4 or prove in-game playability.

## Required tests

Merge changes should cover the smallest owning boundary first and then the CLI
integration path. The contract requires regressions for:

- ordered input and base ancestry;
- every analysis strategy and review disposition;
- definition-module aggregation and cross-file pruning;
- conflict-handler precedence and explicit defer;
- bounded backend failure/panic conversion;
- no writes during analysis;
- artifact, input, base, prior-output, and replacement-target drift;
- commit without semantic recomputation;
- partial success withholding only unsafe units; and
- deterministic artifacts across repeated runs.
